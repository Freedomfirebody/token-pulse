//! # tp-collector-antigravity-ide
//!
//! Antigravity IDE 版流式数据采集器 — 内部自洽架构。
//!
//! ## 与 CLI 版的区别
//!
//! - IDE 版 `GetAllCascadeTrajectories` 返回空 → 仅通过 brain/ 目录发现 session
//! - IDE 版无 `transcript.jsonl` → 无降级路径，RPC 不可用时返回空
//! - 数据获取完全依赖 `GetCascadeTrajectorySteps` 分段流式拉取

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use tracing::{info, warn};

use tp_protocol::{
    CollectionError, Datalog, DatasourceProvider, ReportClass, SourceName, TokenInfo,
};
use tp_antigravity_common::{
    ingest_store::IngestStore,
    model_aliases,
    process_locator::ProcessLocator,
    rpc_client::RpcClient,
    types::GeneratorMetadata,
};

const INGEST_INTERVAL_SECS: u64 = 30;
const BATCH_SIZE: u32 = 20;
const CONSUMER_FRAMEWORK: &str = "framework";

/// Antigravity IDE 流式数据采集器
pub struct AntigravityIdeCollector {
    session_root: PathBuf,
    source_name: SourceName,
    store: Arc<Mutex<IngestStore>>,
    rpc_timeout: Duration,
}

impl AntigravityIdeCollector {
    pub fn new(session_root: PathBuf, source_name: SourceName) -> Self {
        let folder_name = session_root.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("antigravity-ide")
            .to_string();

        let data_dir = dirs::home_dir()
            .map(|h| h.join(".token-pulse").join("token-monitor"))
            .unwrap_or_else(|| PathBuf::from(".token-pulse").join("token-monitor"))
            .join(&folder_name)
            .join("data");

        Self {
            session_root,
            source_name,
            store: Arc::new(Mutex::new(IngestStore::new(data_dir))),
            rpc_timeout: Duration::from_secs(120), // 针对超大 RPC 报文支持长超时 (120 秒)
        }
    }

    pub fn default_session_root() -> PathBuf {
        dirs::home_dir()
            .map(|h| h.join(".gemini").join("antigravity-ide"))
            .unwrap_or_else(|| PathBuf::from("."))
    }

    /// 启动后台采集任务
    pub fn start_background_ingest(&self) {
        let store = self.store.clone();
        let session_root = self.session_root.clone();
        let source_name = self.source_name;
        let rpc_timeout = self.rpc_timeout;

        tokio::spawn(async move {
            Self::ingest_loop(store, session_root, source_name, rpc_timeout).await;
        });

        info!("IDE 后台流式采集已启动 (interval={}s, batch={})",
            INGEST_INTERVAL_SECS, BATCH_SIZE);
    }

    /// Rebuild — 清空内部存储、游标和 session offset
    pub fn trigger_rebuild(&self) {
        self.store.lock().rebuild();
        info!("IDE 采集器 rebuild 完成 (store 已清空)");
    }

    // ===== 后台采集主循环 =====

    async fn ingest_loop(
        store: Arc<Mutex<IngestStore>>,
        session_root: PathBuf,
        source_name: SourceName,
        rpc_timeout: Duration,
    ) {
        let interval = Duration::from_secs(INGEST_INTERVAL_SECS);
        let target = session_root.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("antigravity-ide")
            .to_string();

        // 首次延迟 3 秒
        tokio::time::sleep(Duration::from_secs(3)).await;

        loop {
            let client = RpcClient::new(rpc_timeout);
            let connections = ProcessLocator::detect_connections(&client, &target).await;
            let store_len = store.lock().len();
            info!(
                connections = connections.len(),
                store_entries = store_len,
                "IDE ingest: 开始本轮采集"
            );

            if !connections.is_empty() {
                let conn = &connections[0];

                // IDE 版仅通过 brain/ 目录发现 session，读取本地最后修改时间
                let brain_sessions = scan_brain_sessions(&session_root);

                info!(
                    count = brain_sessions.len(),
                    "IDE ingest: session 发现完成"
                );

                for (session_id, rpc_mtime) in &brain_sessions {
                    let start_offset = store.lock().session_offset(session_id);
                    let stored_mtime = store.lock().session_modified_time(session_id);

                    // 如果本地最后修改时间不为 0，且不大于我们持久化记录的时间，说明没有更新，直接跳过！
                    if *rpc_mtime > 0 && stored_mtime > 0 && *rpc_mtime <= stored_mtime {
                        tracing::trace!(session_id, rpc_mtime, stored_mtime, "IDE 会话无更新，跳过数据拉取");
                        continue;
                    }

                    let step_hint = {
                        let transcript_path = session_root
                            .join("brain")
                            .join(session_id)
                            .join(".system_generated")
                            .join("logs")
                            .join("transcript.jsonl");
                        count_transcript_steps(&transcript_path).unwrap_or(20)
                    };

                    let fetched = stream_session_steps(
                        &client, conn, session_id, start_offset,
                        step_hint,
                        source_name, &store, &session_root,
                    ).await;

                    if fetched > 0 || *rpc_mtime > stored_mtime {
                        if *rpc_mtime > 0 {
                            store.lock().update_session_modified_time(session_id, *rpc_mtime);
                        }
                    }
                }

                store.lock().flush();
            } else {
                info!("IDE ingest: language_server RPC 不可用，跳过本轮采集");
            }

            tokio::time::sleep(interval).await;
        }
    }
}

// ===== DatasourceProvider 适配层 =====

#[async_trait]
impl DatasourceProvider for AntigravityIdeCollector {
    fn name(&self) -> SourceName { self.source_name }
    fn description(&self) -> &str { "Antigravity IDE (流式采集, 内部自洽)" }

    async fn collect(&self) -> Result<Vec<Datalog>, CollectionError> {
        let logs = self.store.lock().pull(CONSUMER_FRAMEWORK);
        Ok(logs)
    }

    async fn collect_since(&self, _since: DateTime<Utc>) -> Result<Vec<Datalog>, CollectionError> {
        self.collect().await
    }

    async fn health_check(&self) -> Result<bool, CollectionError> {
        Ok(self.session_root.join("brain").exists())
    }

    fn data_notifier(&self) -> Option<std::sync::Arc<tokio::sync::Notify>> {
        Some(self.store.lock().append_notifier())
    }
}

// ===== 共享辅助函数 =====

/// 对单个 session 分段流式拉取步骤
async fn stream_session_steps(
    client: &RpcClient,
    conn: &tp_antigravity_common::types::RpcConnection,
    session_id: &str,
    start_offset: u32,
    step_hint: u32,
    source_name: SourceName,
    store: &Arc<Mutex<IngestStore>>,
    session_root: &Path,
) -> usize {
    // 调用我们刚添加的 get_generator_metadata 函数进行全量拉取
    match client.get_generator_metadata(conn, session_id, step_hint).await {
        Ok(raw_list) => {
            if raw_list.is_empty() {
                return 0;
            }

            // 解析为 GeneratorMetadata 结构
            let mut all_metadata: Vec<GeneratorMetadata> = raw_list.iter()
                .filter_map(|val| GeneratorMetadata::from_rpc_json(val))
                .collect();

            // 按步骤索引排序，保证增量偏移顺序
            all_metadata.sort_by_key(|m| m.step_indices.first().copied().unwrap_or(0));

            // 增量过滤：如果全部步骤索引都小于 start_offset，则过滤掉
            let filtered_metadata: Vec<GeneratorMetadata> = all_metadata.iter()
                .filter(|m| {
                    m.step_indices.is_empty() || m.step_indices.iter().any(|&idx| idx >= start_offset)
                })
                .cloned()
                .collect();

            let fetched_count = filtered_metadata.len();

            if fetched_count > 0 {
                // 检测本地 transcript 是否有父会话映射，作为降级/备用 parent_id 覆盖
                let parent_id_override = {
                    let transcript_path = session_root
                        .join("brain")
                        .join(session_id)
                        .join(".system_generated")
                        .join("logs")
                        .join("transcript.jsonl");
                    find_parent_session_id_in_transcript(&transcript_path, session_id)
                };

                // 转换为 datalogs 并追加到本地 store
                let datalogs = metadata_to_datalogs(source_name, session_id, &filtered_metadata, parent_id_override);
                store.lock().append(datalogs);
            }

            // 根据所有已存在的最大 step index 推进 offset 游标
            let next_offset = all_metadata.iter()
                .flat_map(|m| &m.step_indices)
                .max()
                .copied()
                .map(|idx| idx + 1)
                .unwrap_or(start_offset);

            if next_offset > start_offset {
                store.lock().advance_session_offset(session_id, next_offset);
                info!(
                    session_id,
                    total_metadata = raw_list.len(),
                    new_metadata = fetched_count,
                    start_offset,
                    new_offset = next_offset,
                    "IDE ingest: 全量元数据采集完成 (自适应超时)"
                );
            }

            fetched_count
        }
        Err(e) => {
            warn!(
                session_id,
                start_offset,
                error = %e,
                "IDE 全量元数据采集失败"
            );
            0
        }
    }
}

fn metadata_to_datalogs(
    source_name: SourceName,
    session_id: &str,
    metadata: &[GeneratorMetadata],
    parent_id_override: Option<String>,
) -> Vec<Datalog> {
    metadata.iter().filter_map(|m| {
        let model = model_aliases::resolve_model_placeholder(&m.response_model)
            .or_else(|| model_aliases::resolve_model_placeholder(&m.model))
            .map(|s| s.to_string())
            .unwrap_or_else(|| m.response_model.clone());

        let timestamp = m.timestamp.unwrap_or_else(Utc::now);

        // 如果 GeneratorMetadata 中包含 parent_trajectory_id，或者有传入的 parent_id_override，优先作为 source_project 写入
        let project = m.parent_trajectory_id.clone()
            .or_else(|| parent_id_override.clone())
            .unwrap_or_else(|| session_id.to_string());

        Some(Datalog {
            source_name,
            collected_at: Utc::now(),
            source_api_key: None,
            source_project: project,
            source_model: model,
            source_datetime: timestamp,
            source_through_time: Duration::from_secs(0),
            source_parent_project: None,
            source_report_class: ReportClass::Official,
            token_info: TokenInfo {
                input: m.input_tokens,
                output: m.response_tokens,
                cache: m.cache_read_tokens,
                reasoning: m.thinking_tokens,
                resourcing: 0,
            },
        })
    }).collect()
}

fn scan_brain_sessions(session_root: &Path) -> Vec<(String, u64)> {
    let brain_dir = session_root.join("brain");
    if !brain_dir.exists() { return vec![]; }

    let mut sessions = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&brain_dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.len() >= 32 && name.contains('-') {
                    // BUG FIX: 使用 transcript.jsonl 的文件 mtime 作为变更检测依据
                    // 目录 mtime 不会因深层嵌套文件写入而更新
                    let transcript_path = brain_dir
                        .join(&name)
                        .join(".system_generated")
                        .join("logs")
                        .join("transcript.jsonl");

                    let mtime = file_mtime_ms(&transcript_path)
                        .or_else(|| {
                            entry.metadata().ok()
                                .and_then(|m| m.modified().ok())
                                .and_then(|t| t.duration_since(std::time::SystemTime::UNIX_EPOCH).ok())
                                .map(|d| d.as_millis() as u64)
                        })
                        .unwrap_or(0);

                    sessions.push((name, mtime));
                }
            }
        }
    }
    sessions
}

/// 获取文件的修改时间（毫秒级 Unix 时间戳）
fn file_mtime_ms(path: &Path) -> Option<u64> {
    std::fs::metadata(path).ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
}

/// 计算本地 transcript.jsonl 的行数作为实际步数
fn count_transcript_steps(path: &Path) -> Option<u32> {
    use std::io::BufRead;
    let file = std::fs::File::open(path).ok()?;
    let reader = std::io::BufReader::new(file);
    Some(reader.lines().count() as u32)
}

/// 从本地 transcript.jsonl 的前几行（主要是首行 USER_INPUT）中解析父会话 ID (UUID)
fn find_parent_session_id_in_transcript(path: &Path, exclude_session_id: &str) -> Option<String> {
    use std::io::BufRead;
    let file = std::fs::File::open(path).ok()?;
    let reader = std::io::BufReader::new(file);

    // 只需要检查前 5 行即可，主会话 ID 通常在首行 USER_INPUT 的 content 中
    for line_res in reader.lines().take(5) {
        if let Ok(line) = line_res {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) {
                if let Some(content) = val.get("content").and_then(|v| v.as_str()) {
                    if let Some(uuid) = find_uuid_in_text(content, exclude_session_id) {
                        return Some(uuid);
                    }
                }
            }
        }
    }
    None
}

/// 在文本中查找第一个符合 UUID 格式的 36 字符子串，且其前面包含 conversation/orchestrator/parent 关键字
fn find_uuid_in_text(text: &str, exclude: &str) -> Option<String> {
    let keywords = ["conversation id", "conversationid", "conversation", "orchestrator", "parent"];
    
    let text_lower = text.to_lowercase();
    
    for kw in &keywords {
        let mut search_from = 0;
        while search_from < text_lower.len() {
            let Some(pos) = text_lower[search_from..].find(kw) else { break };
            let abs_pos = search_from + pos;
            let kw_end = abs_pos + kw.len();
            
            let search_start = snap_to_char_boundary(text, kw_end, true);
            let raw_end = std::cmp::min(text.len(), search_start + 150);
            let search_end = snap_to_char_boundary(text, raw_end, false);
            
            if search_start < search_end {
                let chunk = &text[search_start..search_end];
                if let Some(uuid) = extract_uuid_with_exclude(chunk, exclude) {
                    return Some(uuid);
                }
            }
            search_from = kw_end;
        }
    }
    None
}

/// 将字节索引调整到最近的 UTF-8 字符边界
fn snap_to_char_boundary(s: &str, byte_idx: usize, forward: bool) -> usize {
    if byte_idx >= s.len() { return s.len(); }
    if byte_idx == 0 { return 0; }
    if s.is_char_boundary(byte_idx) { return byte_idx; }
    
    if forward {
        let mut i = byte_idx;
        while i < s.len() && !s.is_char_boundary(i) { i += 1; }
        i
    } else {
        let mut i = byte_idx;
        while i > 0 && !s.is_char_boundary(i) { i -= 1; }
        i
    }
}

fn extract_uuid_with_exclude(s: &str, exclude: &str) -> Option<String> {
    if s.len() < 36 {
        return None;
    }
    let bytes = s.as_bytes();
    for i in 0..=(bytes.len() - 36) {
        if bytes[i + 8] == b'-' && bytes[i + 13] == b'-' && bytes[i + 18] == b'-' && bytes[i + 23] == b'-' {
            let mut is_uuid = true;
            for j in 0..36 {
                if j == 8 || j == 13 || j == 18 || j == 23 {
                    continue;
                }
                let b = bytes[i + j];
                if !b.is_ascii_hexdigit() {
                    is_uuid = false;
                    break;
                }
            }
            if is_uuid {
                let candidate = s[i..i + 36].to_string();
                if candidate != exclude {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_uuid_in_text() {
        let text1 = "message the Project Orchestrator (conversation ID: bd6e0c16-c624-409c-bb66-d321a9e2ebdc) with a summary.";
        let res1 = find_uuid_in_text(text1, "80ba9cf0-0186-4588-95dd-0e8f617996bf");
        assert_eq!(res1, Some("bd6e0c16-c624-409c-bb66-d321a9e2ebdc".to_string()));

        let text2 = "Action: Report back to parent conversation ID f185943d-9b3c-4b67-ad7a-078324d96179 once complete.";
        let res2 = find_uuid_in_text(text2, "80ba9cf0-0186-4588-95dd-0e8f617996bf");
        assert_eq!(res2, Some("f185943d-9b3c-4b67-ad7a-078324d96179".to_string()));

        // Test exclusion
        let text3 = "conversation ID: 80ba9cf0-0186-4588-95dd-0e8f617996bf, parent: f185943d-9b3c-4b67-ad7a-078324d96179";
        let res3 = find_uuid_in_text(text3, "80ba9cf0-0186-4588-95dd-0e8f617996bf");
        assert_eq!(res3, Some("f185943d-9b3c-4b67-ad7a-078324d96179".to_string()));
    }
}


