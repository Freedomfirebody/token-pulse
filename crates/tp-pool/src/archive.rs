//! 归档数据存储 — 日归档与月归档管理。
//!
//! 文件布局:
//! ```text
//! archive/
//!   daily/
//!     YYYY-MM-DD.jsonl        ← 按天合并的归档
//!   monthly/
//!     YYYY-MM/
//!       YYYY-MM-DD.jsonl      ← 按月分组、按天文件的归档
//! ```

use std::path::PathBuf;

use tp_protocol::{Datalog, PoolError};
use tp_protocol::digest::ArchiveDigest;

use crate::active;

/// 归档数据存储
///
/// 管理 daily 和 monthly 两层归档文件。
pub struct ArchiveStore {
    base_path: PathBuf,
}

impl ArchiveStore {
    /// 创建新的归档数据存储
    ///
    /// `base_path` 应指向 `<pool_root>/archive` 目录。
    pub fn new(base_path: PathBuf) -> Self {
        Self { base_path }
    }

    /// 写入日归档 — 覆盖写入指定日期的 JSONL 文件
    ///
    /// 路径: `archive/daily/YYYY-MM-DD.jsonl`
    pub fn write_daily(&self, date_key: &str, logs: &[Datalog]) -> Result<(), PoolError> {
        let path = self.daily_path(date_key);
        active::write_jsonl_file(&path, logs)?;
        tracing::debug!(
            "日归档已写入: {} 条 -> {}",
            logs.len(),
            path.display()
        );
        Ok(())
    }

    /// 写入月归档 — 在月份目录下按天写入 JSONL 文件
    ///
    /// 路径: `archive/monthly/YYYY-MM/YYYY-MM-DD.jsonl`
    pub fn write_monthly(
        &self,
        month_key: &str,
        date_key: &str,
        logs: &[Datalog],
    ) -> Result<(), PoolError> {
        let path = self.monthly_path(month_key, date_key);
        active::write_jsonl_file(&path, logs)?;
        tracing::debug!(
            "月归档已写入: {} 条 -> {}",
            logs.len(),
            path.display()
        );
        Ok(())
    }

    /// 读取指定日期的日归档数据
    pub fn read_daily(&self, date_key: &str) -> Result<Vec<Datalog>, PoolError> {
        let path = self.daily_path(date_key);
        if !path.exists() {
            return Ok(Vec::new());
        }
        active::read_jsonl_file(&path)
    }

    /// 读取月归档中指定天的数据
    pub fn read_monthly_day(
        &self,
        month_key: &str,
        date_key: &str,
    ) -> Result<Vec<Datalog>, PoolError> {
        let path = self.monthly_path(month_key, date_key);
        if !path.exists() {
            return Ok(Vec::new());
        }
        active::read_jsonl_file(&path)
    }

    /// 范围查询 — 读取 [from, to] 日期范围内的所有数据
    ///
    /// 同时搜索 daily 和 monthly 归档。
    /// `from` 和 `to` 格式: "YYYY-MM-DD"
    pub fn read_range(&self, from: &str, to: &str) -> Result<Vec<Datalog>, PoolError> {
        let mut all_logs = Vec::new();

        // 1. 搜索 daily 归档
        let daily_dir = self.base_path.join("daily");
        if daily_dir.exists() {
            let daily_logs = self.read_range_from_dir(&daily_dir, from, to, false)?;
            all_logs.extend(daily_logs);
        }

        // 2. 搜索 monthly 归档
        let monthly_dir = self.base_path.join("monthly");
        if monthly_dir.exists() {
            let monthly_logs = self.read_range_from_monthly(&monthly_dir, from, to)?;
            all_logs.extend(monthly_logs);
        }

        // 按 source_datetime 排序
        all_logs.sort_by_key(|log| log.source_datetime);

        tracing::debug!(
            "范围查询完成: {} 条, 范围=[{}, {}]",
            all_logs.len(),
            from,
            to
        );
        Ok(all_logs)
    }

    // ─── Digest I/O ───

    /// 写入日归档 Digest — 预计算的聚合结论
    ///
    /// 路径: `archive/daily/YYYY-MM-DD.digest.json`
    pub fn write_daily_digest(&self, date_key: &str, digest: &ArchiveDigest) -> Result<PathBuf, PoolError> {
        let path = self.daily_digest_path(date_key);
        digest.save(&path).map_err(|e| {
            PoolError::SerializationError(format!("Digest 写入失败 {}: {e}", path.display()))
        })?;
        tracing::debug!(
            "日归档 Digest 已写入: {} -> {}",
            date_key,
            path.display()
        );
        Ok(path)
    }

    /// 读取日归档 Digest
    pub fn read_daily_digest(&self, date_key: &str) -> Result<Option<ArchiveDigest>, PoolError> {
        let path = self.daily_digest_path(date_key);
        if !path.exists() {
            return Ok(None);
        }
        let digest = ArchiveDigest::load(&path).map_err(|e| {
            PoolError::SerializationError(format!("Digest 读取失败 {}: {e}", path.display()))
        })?;
        Ok(Some(digest))
    }

    /// Digest 文件路径: `archive/daily/YYYY-MM-DD.digest.json`
    pub fn daily_digest_path(&self, date_key: &str) -> PathBuf {
        self.base_path
            .join("daily")
            .join(format!("{date_key}.digest.json"))
    }

    // ─── 内部辅助方法 ───

    /// 日归档文件路径: `archive/daily/YYYY-MM-DD.jsonl`
    fn daily_path(&self, date_key: &str) -> PathBuf {
        self.base_path
            .join("daily")
            .join(format!("{date_key}.jsonl"))
    }

    /// 月归档文件路径: `archive/monthly/YYYY-MM/YYYY-MM-DD.jsonl`
    fn monthly_path(&self, month_key: &str, date_key: &str) -> PathBuf {
        self.base_path
            .join("monthly")
            .join(month_key)
            .join(format!("{date_key}.jsonl"))
    }

    /// 从日归档目录读取指定日期范围内的所有文件
    fn read_range_from_dir(
        &self,
        dir: &std::path::Path,
        from: &str,
        to: &str,
        _is_monthly: bool,
    ) -> Result<Vec<Datalog>, PoolError> {
        let mut logs = Vec::new();

        if !dir.exists() {
            return Ok(logs);
        }

        let mut entries: Vec<_> = std::fs::read_dir(dir)?
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .extension()
                    .map_or(false, |ext| ext == "jsonl")
            })
            .collect();

        entries.sort_by_key(|e| e.file_name());

        for entry in entries {
            let date_key = entry
                .path()
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();

            // 日期范围过滤
            if date_key.as_str() >= from && date_key.as_str() <= to {
                let file_logs = active::read_jsonl_file(&entry.path())?;
                logs.extend(file_logs);
            }
        }

        Ok(logs)
    }

    /// 从月归档目录读取指定日期范围内的数据
    fn read_range_from_monthly(
        &self,
        monthly_dir: &std::path::Path,
        from: &str,
        to: &str,
    ) -> Result<Vec<Datalog>, PoolError> {
        let mut logs = Vec::new();

        // 确定需要扫描的月份范围
        let from_month = &from[..7]; // "YYYY-MM"
        let to_month = &to[..7]; // "YYYY-MM"

        let mut month_dirs: Vec<_> = std::fs::read_dir(monthly_dir)?
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map_or(false, |ft| ft.is_dir()))
            .collect();

        month_dirs.sort_by_key(|e| e.file_name());

        for month_dir in month_dirs {
            let month_key = month_dir.file_name().to_string_lossy().to_string();

            // 月份范围过滤
            if month_key.as_str() >= from_month && month_key.as_str() <= to_month {
                let month_logs =
                    self.read_range_from_dir(&month_dir.path(), from, to, true)?;
                logs.extend(month_logs);
            }
        }

        Ok(logs)
    }
}
