//! 流式采集器内部存储 — append-only 数据日志 + 消费者游标 + session 增量 offset
//!
//! ## 设计原则
//!
//! - 采集器内部自洽，数据采集与消费完全解耦
//! - 消费者通过 `pull(consumer_id)` 获取增量数据，游标自动推进
//! - session_offsets 持久化，避免重启后从 RPC 重新拉取全量步骤
//! - consumer_cursors 持久化，但启动时重置为 0（因为 entries 是纯内存的）
//! - `rebuild()` 清空所有状态（entries、cursors、session_offsets）

use std::collections::HashMap;
use std::path::PathBuf;

use tracing::{debug, warn};
use tp_protocol::Datalog;

/// 流式采集器内部存储
///
/// 数据流：
/// ```text
/// 后台采集 → append(datalogs) → entries (内存)
///                                    ↑
/// 消费者   → pull("pool")    → entries[cursor..] (增量)
/// ```
pub struct IngestStore {
    /// Append-only 数据日志（纯内存，重启后清空）
    entries: Vec<Datalog>,

    /// 每个消费者的读取游标（offset into entries）
    /// key = consumer_id (如 "framework", "export")
    cursors: HashMap<String, usize>,

    /// 每个 session 已采集到的最大 step_index
    /// 持久化到磁盘，重启后增量续采
    session_offsets: HashMap<String, u32>,

    /// 持久化目录 (~/.token-pulse/token-monitor/{folder}/data/)
    data_dir: PathBuf,

    /// 脏标记 — 减少不必要的磁盘写入
    offsets_dirty: bool,
}

impl IngestStore {
    /// 创建 IngestStore，加载持久化的 session_offsets
    ///
    /// consumer_cursors 启动时强制重置为 0，
    /// 因为 entries 是纯内存的，重启后需要重新消费。
    pub fn new(data_dir: PathBuf) -> Self {
        if let Err(e) = std::fs::create_dir_all(&data_dir) {
            warn!(error = %e, dir = %data_dir.display(), "创建数据目录失败");
        }

        // 加载持久化的 session offsets（用于 RPC 增量续采）
        let session_offsets: HashMap<String, u32> = Self::load_json(&data_dir.join("session_offsets.json"))
            .unwrap_or_default();

        debug!(
            sessions = session_offsets.len(),
            dir = %data_dir.display(),
            "IngestStore 初始化完成"
        );

        Self {
            entries: Vec::new(),
            cursors: HashMap::new(), // 启动时重置（entries 是空的）
            session_offsets,
            data_dir,
            offsets_dirty: false,
        }
    }

    // ===== 写入端（后台采集任务调用）=====

    /// 追加数据到内部存储
    pub fn append(&mut self, logs: Vec<Datalog>) {
        if logs.is_empty() { return; }
        self.entries.extend(logs);
    }

    /// 获取指定 session 的当前 step offset
    pub fn session_offset(&self, session_id: &str) -> u32 {
        self.session_offsets.get(session_id).copied().unwrap_or(0)
    }

    /// 更新指定 session 的 step offset（仅标记 dirty，不立即写盘）
    pub fn advance_session_offset(&mut self, session_id: &str, new_offset: u32) {
        let current = self.session_offsets.get(session_id).copied().unwrap_or(0);
        if new_offset > current {
            self.session_offsets.insert(session_id.to_string(), new_offset);
            self.offsets_dirty = true;
        }
    }

    /// 将脏数据刷盘（每轮采集结束后调用一次）
    pub fn flush(&mut self) {
        if self.offsets_dirty {
            Self::save_json(
                &self.data_dir.join("session_offsets.json"),
                &self.session_offsets,
            );
            self.offsets_dirty = false;
        }
    }

    // ===== 读取端（消费者调用）=====

    /// 增量拉取 — 返回该消费者自上次 pull 以来的新数据
    ///
    /// 游标自动推进到当前 entries 末尾。
    pub fn pull(&mut self, consumer_id: &str) -> Vec<Datalog> {
        let cursor = self.cursors.get(consumer_id).copied().unwrap_or(0);
        let total = self.entries.len();

        if cursor >= total {
            return Vec::new();
        }

        let new_data = self.entries[cursor..].to_vec();
        self.cursors.insert(consumer_id.to_string(), total);

        // 持久化游标
        Self::save_json(
            &self.data_dir.join("consumer_cursors.json"),
            &self.cursors,
        );

        new_data
    }

    /// 当前存储条目总数
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 存储是否为空
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    // ===== Rebuild =====

    /// 全量重建 — 清空 entries、游标、session offsets
    ///
    /// 后台采集任务下一轮会从 offset 0 重新拉取所有数据。
    pub fn rebuild(&mut self) {
        let entry_count = self.entries.len();
        let session_count = self.session_offsets.len();

        self.entries.clear();
        self.cursors.clear();
        self.session_offsets.clear();
        self.offsets_dirty = false;

        // 持久化清空状态
        Self::save_json(
            &self.data_dir.join("consumer_cursors.json"),
            &self.cursors,
        );
        Self::save_json(
            &self.data_dir.join("session_offsets.json"),
            &self.session_offsets,
        );

        debug!(
            cleared_entries = entry_count,
            cleared_sessions = session_count,
            "IngestStore rebuild 完成"
        );
    }

    // ===== 持久化辅助 =====

    fn save_json<T: serde::Serialize>(path: &PathBuf, data: &T) {
        match serde_json::to_string_pretty(data) {
            Ok(json) => {
                if let Err(e) = std::fs::write(path, json) {
                    warn!(error = %e, path = %path.display(), "持久化写入失败");
                }
            }
            Err(e) => {
                warn!(error = %e, "JSON 序列化失败");
            }
        }
    }

    fn load_json<T: serde::de::DeserializeOwned>(path: &PathBuf) -> Option<T> {
        let content = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&content).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tp_protocol::{ReportClass, SourceName, TokenInfo};
    use chrono::Utc;
    use std::time::Duration;

    fn make_datalog(project: &str, input: u64) -> Datalog {
        Datalog {
            source_name: SourceName::Antigravity,
            collected_at: Utc::now(),
            source_api_key: None,
            source_project: project.to_string(),
            source_model: "test".to_string(),
            source_datetime: Utc::now(),
            source_through_time: Duration::from_secs(0),
            source_parent_project: None,
            source_report_class: ReportClass::Official,
            token_info: TokenInfo { input, output: 0, cache: 0, reasoning: 0, resourcing: 0 },
        }
    }

    #[test]
    fn test_append_and_pull() {
        let dir = std::env::temp_dir().join("ingest_store_test_1");
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = IngestStore::new(dir.clone());

        // Append batch 1
        store.append(vec![make_datalog("s1", 100), make_datalog("s1", 200)]);
        assert_eq!(store.len(), 2);

        // Consumer A pulls → gets 2
        let pulled = store.pull("A");
        assert_eq!(pulled.len(), 2);

        // Append batch 2
        store.append(vec![make_datalog("s2", 300)]);

        // Consumer A pulls → gets only 1 (incremental)
        let pulled = store.pull("A");
        assert_eq!(pulled.len(), 1);

        // Consumer B pulls → gets all 3 (first pull)
        let pulled = store.pull("B");
        assert_eq!(pulled.len(), 3);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_rebuild_resets_everything() {
        let dir = std::env::temp_dir().join("ingest_store_test_2");
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = IngestStore::new(dir.clone());

        store.append(vec![make_datalog("s1", 100)]);
        store.advance_session_offset("s1", 50);
        let _ = store.pull("A");

        store.rebuild();

        assert_eq!(store.len(), 0);
        assert_eq!(store.session_offset("s1"), 0);
        assert!(store.pull("A").is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
