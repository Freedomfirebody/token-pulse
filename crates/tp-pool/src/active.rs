//! 活跃数据存储 — 当天的小时级分片管理。
//!
//! 文件布局:
//! ```text
//! active/
//!   YYYY-MM-DD/
//!     HH.jsonl    ← 每小时一个 JSONL 文件
//! ```
//!
//! 所有写入操作为 append 模式，保证并发安全。

use std::io::{BufRead, Write};
use std::path::PathBuf;

use chrono::Utc;

use tp_protocol::{Datalog, PoolError};

/// 活跃数据存储
///
/// 管理当天数据，以小时为粒度分片存储为 JSONL 文件。
pub struct ActiveStore {
    base_path: PathBuf,
}

impl ActiveStore {
    /// 创建新的活跃数据存储
    ///
    /// `base_path` 应指向 `<pool_root>/active` 目录。
    pub fn new(base_path: PathBuf) -> Self {
        Self { base_path }
    }

    /// 追加写入指定小时分片的日志
    ///
    /// 数据以 JSONL 格式 (每行一条 JSON) 追加到文件末尾。
    /// 若目录或文件不存在则自动创建。
    pub fn write_logs(&self, hour_key: &str, logs: &[Datalog]) -> Result<(), PoolError> {
        if logs.is_empty() {
            return Ok(());
        }

        let path = self.hour_path(hour_key);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        let mut writer = std::io::BufWriter::new(file);

        for log in logs {
            let line = serde_json::to_string(log).map_err(|e| {
                PoolError::SerializationError(format!("Datalog 序列化失败: {e}"))
            })?;
            writeln!(writer, "{line}").map_err(|e| {
                PoolError::WriteError(format!("写入 JSONL 失败: {e}"))
            })?;
        }

        writer.flush().map_err(|e| {
            PoolError::WriteError(format!("flush 失败: {e}"))
        })?;

        tracing::debug!(
            "活跃数据已写入: {} 条 -> {}",
            logs.len(),
            path.display()
        );
        Ok(())
    }

    /// 读取指定小时分片的所有日志
    pub fn read_hour(&self, hour_key: &str) -> Result<Vec<Datalog>, PoolError> {
        let path = self.hour_path(hour_key);
        if !path.exists() {
            return Ok(Vec::new());
        }
        Self::read_jsonl_file(&path)
    }

    /// 读取当天所有小时分片的日志
    ///
    /// 自动根据当前日期确定目录，汇总所有小时文件。
    pub fn read_today(&self) -> Result<Vec<Datalog>, PoolError> {
        let today = Utc::now().format("%Y-%m-%d").to_string();
        let today_dir = self.base_path.join(&today);

        if !today_dir.exists() {
            return Ok(Vec::new());
        }

        let mut all_logs = Vec::new();
        let mut entries: Vec<_> = std::fs::read_dir(&today_dir)?
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .extension()
                    .map_or(false, |ext| ext == "jsonl")
            })
            .collect();

        // 按文件名排序确保按小时顺序
        entries.sort_by_key(|e| e.file_name());

        for entry in entries {
            let logs = Self::read_jsonl_file(&entry.path())?;
            all_logs.extend(logs);
        }

        tracing::debug!(
            "今日活跃数据已读取: {} 条, 日期={}",
            all_logs.len(),
            today
        );
        Ok(all_logs)
    }

    /// 列出所有存在的小时 key
    ///
    /// 扫描所有日期目录下的 JSONL 文件，
    /// 返回格式: `["YYYY-MM-DDTHH", ...]`
    pub fn list_hour_keys(&self) -> Result<Vec<String>, PoolError> {
        let mut keys = Vec::new();

        if !self.base_path.exists() {
            return Ok(keys);
        }

        let mut date_dirs: Vec<_> = std::fs::read_dir(&self.base_path)?
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map_or(false, |ft| ft.is_dir()))
            .collect();

        date_dirs.sort_by_key(|e| e.file_name());

        for date_dir in date_dirs {
            let date_key = date_dir.file_name().to_string_lossy().to_string();

            let mut hour_files: Vec<_> = std::fs::read_dir(date_dir.path())?
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.path()
                        .extension()
                        .map_or(false, |ext| ext == "jsonl")
                })
                .collect();

            hour_files.sort_by_key(|e| e.file_name());

            for hour_file in hour_files {
                let hour = hour_file
                    .path()
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                keys.push(format!("{date_key}T{hour}"));
            }
        }

        Ok(keys)
    }

    /// 计算指定小时 key 的文件路径
    ///
    /// hour_key 格式: "YYYY-MM-DDTHH"
    /// 返回: `<base_path>/YYYY-MM-DD/HH.jsonl`
    fn hour_path(&self, hour_key: &str) -> PathBuf {
        // "YYYY-MM-DDTHH" → date="YYYY-MM-DD", hour="HH"
        let date_key = &hour_key[..10];
        let hour = &hour_key[11..];
        self.base_path.join(date_key).join(format!("{hour}.jsonl"))
    }

    /// 读取一个 JSONL 文件的所有记录
    fn read_jsonl_file(path: &std::path::Path) -> Result<Vec<Datalog>, PoolError> {
        let file = std::fs::File::open(path)?;
        let reader = std::io::BufReader::new(file);
        let mut logs = Vec::new();

        for (line_num, line) in reader.lines().enumerate() {
            let line = line?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            match serde_json::from_str::<Datalog>(trimmed) {
                Ok(log) => logs.push(log),
                Err(e) => {
                    tracing::warn!(
                        "JSONL 解析失败: {}:{} — {e}",
                        path.display(),
                        line_num + 1
                    );
                    // 跳过损坏的行，继续读取
                }
            }
        }

        Ok(logs)
    }
}

/// 从 JSONL 文件读取 Datalog (公开给 archive 模块使用)
pub(crate) fn read_jsonl_file(path: &std::path::Path) -> Result<Vec<Datalog>, PoolError> {
    ActiveStore::read_jsonl_file(path)
}

/// 将 Datalog 列表以 JSONL 格式写入文件 (覆盖模式，供 archive 使用)
pub(crate) fn write_jsonl_file(
    path: &std::path::Path,
    logs: &[Datalog],
) -> Result<(), PoolError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let file = std::fs::File::create(path)?;
    let mut writer = std::io::BufWriter::new(file);

    for log in logs {
        let line = serde_json::to_string(log).map_err(|e| {
            PoolError::SerializationError(format!("Datalog 序列化失败: {e}"))
        })?;
        writeln!(writer, "{line}").map_err(|e| {
            PoolError::WriteError(format!("写入 JSONL 失败: {e}"))
        })?;
    }

    writer.flush().map_err(|e| {
        PoolError::WriteError(format!("flush 失败: {e}"))
    })?;

    Ok(())
}
