//! # tp-collector-antigravity
//!
//! Antigravity CLI/终端版流式数据采集器 — 内部自洽架构。
//!
//! ## 架构
//!
//! ```text
//! ┌─── 后台采集 (30s) ───────────────────┐
//! │  detect_connections()                 │
//! │  → 发现 sessions                      │
//! │  → 分段 stream steps (200/batch)      │
//! │  → parse → append(IngestStore)        │
//! │  → RPC 不可用时 → transcript 降级      │
//! └──────────────────────────────────────┘
//!           ↓ append
//!   ┌── IngestStore (内存) ──┐
//!   │  entries: Vec<Datalog> │
//!   │  cursors per consumer  │
//!   │  session_offsets       │
//!   └────────────────────────┘
//!           ↑ pull("framework")
//! ┌─── Framework collect() ──────────────┐
//! │  返回该消费者自上次以来的增量数据     │
//! └──────────────────────────────────────┘
//! ```
//!
//! ## 与 IDE 版的区别
//!
//! | 维度 | CLI 版 (本 crate) | IDE 版 |
//! |------|-------------------|--------|
//! | RPC `GetAllCascadeTrajectories` | ✅ 返回完整列表 | ❌ 返回空 |
//! | transcript.jsonl | ✅ 有内容 | ❌ 0 bytes |
//! | 降级策略 | transcript 字符估算 | 无 |

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde_json::Value;
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

/// 后台采集间隔
const INGEST_INTERVAL_SECS: u64 = 30;

/// 每批拉取的步骤数
const BATCH_SIZE: u32 = 20;

/// 消费者 ID（framework 拉取用）
const CONSUMER_FRAMEWORK: &str = "framework";

/// 累积偏移状态 — 用于 transcript.jsonl 增量读取和 Token 估算
#[derive(Clone, Copy, Default)]
struct FileOffsetState {
    offset: u64,
    cumulative_tokens: u64,
    tokens_at_last_model: u64,
}

/// Antigravity CLI 流式数据采集器
pub struct AntigravityCollector {
    /// brain 目录根路径 (~/.gemini/antigravity)
    session_root: PathBuf,
    /// 数据源标识
    source_name: SourceName,
    /// 内部 append-only 存储
    store: Arc<Mutex<IngestStore>>,
    /// transcript.jsonl 增量读取状态
    transcript_offsets: Arc<Mutex<HashMap<PathBuf, FileOffsetState>>>,
    /// RPC 请求超时
    rpc_timeout: Duration,
}

impl AntigravityCollector {
    /// 创建 CLI 流式采集器
    pub fn new(session_root: PathBuf, source_name: SourceName) -> Self {
        let folder_name = session_root.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("antigravity")
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
            transcript_offsets: Arc::new(Mutex::new(HashMap::new())),
            rpc_timeout: Duration::from_secs(120), // 针对超大 RPC 报文支持长超时 (120 秒)
        }
    }

    pub fn default_session_root() -> PathBuf {
        dirs::home_dir()
            .map(|h| h.join(".gemini").join("antigravity"))
            .unwrap_or_else(|| PathBuf::from("."))
    }

    /// 启动后台采集任务 — 独立于 framework 的定时轮询
    pub fn start_background_ingest(&self) {
        let store = self.store.clone();
        let session_root = self.session_root.clone();
        let source_name = self.source_name;
        let rpc_timeout = self.rpc_timeout;
        let transcript_offsets = self.transcript_offsets.clone();

        tokio::spawn(async move {
            Self::ingest_loop(
                store,
                session_root,
                source_name,
                rpc_timeout,
                transcript_offsets,
            ).await;
        });

        info!("CLI 后台流式采集已启动 (interval={}s, batch={})",
            INGEST_INTERVAL_SECS, BATCH_SIZE);
    }

    /// Rebuild — 清空内部存储、游标和 session offset
    pub fn trigger_rebuild(&self) {
        self.store.lock().rebuild();
        self.transcript_offsets.lock().clear();
        info!("CLI 采集器 rebuild 完成 (store + transcript offsets 已清空)");
    }

    // ===== 后台采集主循环 =====

    async fn ingest_loop(
        store: Arc<Mutex<IngestStore>>,
        session_root: PathBuf,
        source_name: SourceName,
        rpc_timeout: Duration,
        transcript_offsets: Arc<Mutex<HashMap<PathBuf, FileOffsetState>>>,
    ) {
        let interval = Duration::from_secs(INGEST_INTERVAL_SECS);
        let target = session_root.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("antigravity")
            .to_string();

        // 首次延迟 3 秒，让 UI 先完成渲染
        tokio::time::sleep(Duration::from_secs(3)).await;

        loop {
            let client = RpcClient::new(rpc_timeout);
            let connections = ProcessLocator::detect_connections(&client, &target).await;
            let store_len = store.lock().len();
            info!(
                connections = connections.len(),
                store_entries = store_len,
                "CLI ingest: 开始本轮采集"
            );

            let mut rpc_session_ids = HashSet::new();

            if !connections.is_empty() {
                // RPC 路径：分段 stream steps
                let conn = &connections[0];

                // 发现 sessions：RPC 列表 + brain 目录取并集
                let rpc_sessions = client.list_trajectories(conn).await;
                
                // 将所有 session 统一组织为：session_id -> mtime 映射
                let mut session_mtimes = std::collections::HashMap::new();
                for s in &rpc_sessions {
                    if let Some(ms) = s.last_modified_ms {
                        session_mtimes.insert(s.session_id.clone(), ms);
                    }
                }
                
                // 扫描 brain 并集，包含本地文件夹最后修改时间
                let brain_sessions = scan_brain_sessions(&session_root);
                for (id, mtime) in brain_sessions {
                    // 如果 RPC 已经提供了修改时间，以 RPC 为准，否则使用本地文件夹修改时间
                    session_mtimes.entry(id).or_insert(mtime);
                }

                // 排序，保证稳定性
                let mut session_ids: Vec<String> = session_mtimes.keys().cloned().collect();
                session_ids.sort();

                info!(
                    rpc_count = rpc_sessions.len(),
                    total = session_ids.len(),
                    "CLI ingest: session 发现完成"
                );

                // 对每个 session 增量拉取
                for (idx, session_id) in session_ids.iter().enumerate() {
                    let start_offset = store.lock().session_offset(session_id);
                    let step_hint = rpc_sessions.iter()
                        .find(|s| s.session_id == *session_id)
                        .and_then(|s| s.step_count)
                        .unwrap_or_else(|| {
                            // 从本地 transcript.jsonl 的行数中精准读取实际步数
                            let transcript_path = session_root
                                .join("brain")
                                .join(session_id)
                                .join(".system_generated")
                                .join("logs")
                                .join("transcript.jsonl");
                            count_transcript_steps(&transcript_path).unwrap_or(20)
                        });

                    // 获取本次最新的修改时间
                    let rpc_mtime = session_mtimes.get(session_id).copied().unwrap_or(0);
                    let stored_mtime = store.lock().session_modified_time(session_id);

                    // 如果最新修改时间不为 0，且不大于我们持久化记录的时间，说明没有更新，直接跳过！
                    if rpc_mtime > 0 && stored_mtime > 0 && rpc_mtime <= stored_mtime {
                        tracing::trace!(session_id, rpc_mtime, stored_mtime, "会话无更新，跳过数据拉取");
                        rpc_session_ids.insert(session_id.clone());
                        continue;
                    }

                    let est_secs = 0.24 + (step_hint as f64 * 0.0012) + (step_hint as f64).powi(2) * 0.0000003;
                    if start_offset == 0 && step_hint > 0 {
                        info!(
                            progress = format!("{}/{}", idx + 1, session_ids.len()),
                            session_id,
                            step_hint,
                            "CLI ingest: 开始采集 session (预计耗时: {:.1} 秒)",
                            est_secs
                        );
                    }

                    let fetched = Self::stream_session_steps(
                        &client, conn, session_id, start_offset, step_hint,
                        source_name, &store, &session_root,
                    ).await;

                    if fetched > 0 || rpc_mtime > stored_mtime {
                        if rpc_mtime > 0 {
                            store.lock().update_session_modified_time(session_id, rpc_mtime);
                        }
                    }
                    rpc_session_ids.insert(session_id.clone());
                }

                // 刷盘 session offsets 和 modified times
                store.lock().flush();
            }

            // Transcript 降级：对 RPC 未覆盖的 session 使用字符估算
            let sr = session_root.clone();
            let sn = source_name;
            let skip = rpc_session_ids;
            let offsets = transcript_offsets.clone();
            let store_clone = store.clone();

            let _ = tokio::task::spawn_blocking(move || {
                let logs = collect_via_transcript(&sr, sn, &skip, &offsets);
                if !logs.is_empty() {
                    info!(count = logs.len(), "CLI ingest: Transcript 降级补充完成");
                    store_clone.lock().append(logs);
                }
            }).await;

            tokio::time::sleep(interval).await;
        }
    }

    /// 对单个 session 分段流式拉取步骤 → 解析 → append
    ///
    /// 返回本次新获取的 metadata 条数。
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
                        "CLI ingest: 全量元数据采集完成 (自适应超时)"
                    );
                }

                fetched_count
            }
            Err(e) => {
                warn!(
                    session_id,
                    start_offset,
                    error = %e,
                    "CLI 全量元数据采集失败"
                );
                0
            }
        }
    }
}

// ===== DatasourceProvider 适配层 =====

#[async_trait]
impl DatasourceProvider for AntigravityCollector {
    fn name(&self) -> SourceName { self.source_name }
    fn description(&self) -> &str { "Antigravity CLI (流式采集, 内部自洽)" }

    /// 增量拉取 — 返回自上次 pull 以来的新数据
    async fn collect(&self) -> Result<Vec<Datalog>, CollectionError> {
        let logs = self.store.lock().pull(CONSUMER_FRAMEWORK);
        Ok(logs)
    }

    /// 增量由 cursor 保证，since 参数不再需要
    async fn collect_since(&self, _since: DateTime<Utc>) -> Result<Vec<Datalog>, CollectionError> {
        self.collect().await
    }

    async fn health_check(&self) -> Result<bool, CollectionError> {
        Ok(self.session_root.join("brain").exists())
    }
}

// ===== 共享辅助函数 =====

/// GeneratorMetadata → Datalog 转换
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

/// 扫描 brain 目录获取所有 session_id 以及上次修改时间（毫秒级时间戳）
fn scan_brain_sessions(session_root: &Path) -> Vec<(String, u64)> {
    let brain_dir = session_root.join("brain");
    if !brain_dir.exists() { return vec![]; }

    let mut sessions = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&brain_dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.len() >= 32 && name.contains('-') {
                    let mut mtime = 0;
                    if let Ok(metadata) = entry.metadata() {
                        if let Ok(modified) = metadata.modified() {
                            if let Ok(duration) = modified.duration_since(std::time::SystemTime::UNIX_EPOCH) {
                                mtime = duration.as_millis() as u64;
                            }
                        }
                    }
                    sessions.push((name, mtime));
                }
            }
        }
    }
    sessions
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
    let text_lower = text.to_lowercase();
    let keywords = ["conversation id", "conversationid", "conversation", "orchestrator", "parent"];
    
    for kw in &keywords {
        let mut start_idx = 0;
        while let Some(pos) = text_lower[start_idx..].find(kw) {
            let abs_pos = start_idx + pos;
            let search_start = abs_pos + kw.len();
            let search_end = std::cmp::min(text.len(), search_start + 150);
            if search_start < search_end {
                let chunk = &text[search_start..search_end];
                if let Some(uuid) = extract_uuid_with_exclude(chunk, exclude) {
                    return Some(uuid);
                }
            }
            start_idx = abs_pos + kw.len();
        }
    }
    None
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


// ===== Transcript 降级路径 =====

/// 扫描 transcript.jsonl 进行字符→token 估算（RPC 不可用时的降级路径）
fn collect_via_transcript(
    session_root: &Path,
    source_name: SourceName,
    skip_ids: &HashSet<String>,
    offsets: &Arc<Mutex<HashMap<PathBuf, FileOffsetState>>>,
) -> Vec<Datalog> {
    let brain_root = session_root.join("brain");
    if !brain_root.exists() { return vec![]; }

    let mut all_logs = Vec::new();
    let entries = match std::fs::read_dir(&brain_root) {
        Ok(e) => e,
        Err(_) => return vec![],
    };

    for entry in entries.flatten() {
        if !entry.path().is_dir() { continue; }
        let session_id = entry.file_name().to_string_lossy().to_string();
        if skip_ids.contains(&session_id) { continue; }

        let transcript_path = entry.path()
            .join(".system_generated")
            .join("logs")
            .join("transcript.jsonl");

        if !transcript_path.exists() { continue; }

        match parse_transcript_file(&transcript_path, &session_id, source_name, offsets) {
            Ok(logs) if !logs.is_empty() => {
                info!(
                    session_id,
                    log_count = logs.len(),
                    "CLI ingest: Transcript 降级解析成功"
                );
                all_logs.extend(logs);
            }
            Err(e) => warn!(session_id, error = %e, "CLI ingest: Transcript 降级解析失败"),
            _ => {}
        }
    }

    all_logs
}

/// 解析单个 transcript.jsonl → Vec<Datalog>
fn parse_transcript_file(
    path: &Path,
    session_id: &str,
    source_name: SourceName,
    offsets: &Arc<Mutex<HashMap<PathBuf, FileOffsetState>>>,
) -> Result<Vec<Datalog>, CollectionError> {
    use std::io::{BufRead, BufReader, Seek, SeekFrom};

    let mut file = std::fs::File::open(path).map_err(CollectionError::Io)?;
    let meta = file.metadata().map_err(CollectionError::Io)?;
    let file_size = meta.len();

    let path_buf = path.to_path_buf();
    let mut lock = offsets.lock();
    let state = lock.get(&path_buf).cloned().unwrap_or_default();

    let mut start_offset = state.offset;
    let mut cumulative_tokens = state.cumulative_tokens;
    let mut tokens_at_last_model = state.tokens_at_last_model;

    if file_size < start_offset {
        start_offset = 0;
        cumulative_tokens = 0;
        tokens_at_last_model = 0;
    }

    file.seek(SeekFrom::Start(start_offset)).map_err(CollectionError::Io)?;
    let mut reader = BufReader::new(file);
    let mut current_offset = start_offset;

    let stable_fallback_ms = meta.created().or_else(|_| meta.modified()).ok()
        .and_then(|t| t.duration_since(std::time::SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64);

    let mut logs = Vec::new();
    let mut line = String::new();
    let estimator = tp_antigravity_common::estimator::TokenEstimator::default();

    loop {
        line.clear();
        let bytes_read = reader.read_line(&mut line).map_err(CollectionError::Io)?;
        if bytes_read == 0 { break; }
        current_offset += bytes_read as u64;

        let trimmed = line.trim();
        if trimmed.is_empty() { continue; }

        let val: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let step_type = val.get("type").and_then(|v| v.as_str());
        let has_usage = val.get("usage_metadata").filter(|v| !v.is_null()).is_some();
        let content_str = val.get("content").and_then(|v| v.as_str()).unwrap_or("");
        let thinking_str = val.get("thinking").and_then(|v| v.as_str()).unwrap_or("");

        if step_type == Some("PLANNER_RESPONSE") || has_usage {
            let mut report_class = ReportClass::Calculate;
            let (input, output, cache, reasoning);

            if let Some(usage) = val.get("usage_metadata").filter(|v| !v.is_null()) {
                let oi = usage.get("prompt_token_count").and_then(|v| v.as_u64());
                let oo = usage.get("candidates_token_count").and_then(|v| v.as_u64());
                let oc = usage.get("cached_content_token_count").and_then(|v| v.as_u64());
                let or_ = usage.get("thoughts_token_count").and_then(|v| v.as_u64());

                if let (Some(inp), Some(out)) = (oi, oo) {
                    cache = oc.unwrap_or(0);
                    input = inp.saturating_sub(cache);
                    output = out;
                    reasoning = or_.unwrap_or(0);
                    report_class = ReportClass::Official;
                } else {
                    output = estimator.estimate_text(content_str);
                    reasoning = estimator.estimate_text(thinking_str);
                    cache = tokens_at_last_model;
                    input = (500 + cumulative_tokens).saturating_sub(cache);
                }
            } else {
                output = estimator.estimate_text(content_str);
                reasoning = estimator.estimate_text(thinking_str);
                cache = tokens_at_last_model;
                input = (500 + cumulative_tokens).saturating_sub(cache);
            }

            if input > 0 || output > 0 || cache > 0 || reasoning > 0 {
                let ts = [
                    val.get("timestamp"),
                    val.get("created_at"),
                ].into_iter().flatten().find_map(|v| {
                    if v.is_null() { return None; }
                    v.as_i64().or_else(|| v.as_str().and_then(|s| {
                        s.parse::<i64>().ok().or_else(||
                            DateTime::parse_from_rfc3339(s).ok().map(|dt| dt.timestamp_millis())
                        )
                    }))
                });

                let step_idx = val.get("step_index").and_then(|v| v.as_i64()).unwrap_or(0);
                let model_raw = val.get("model").and_then(|v| v.as_str()).unwrap_or("gemini-3.5-flash");
                let base_ts = ts.or(stable_fallback_ms).unwrap_or_else(|| Utc::now().timestamp_millis());
                let datetime = DateTime::from_timestamp_millis(base_ts + step_idx * 1000)
                    .unwrap_or_else(Utc::now);

                let model = model_aliases::resolve_model_placeholder(model_raw)
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| model_raw.to_string());

                info!(
                    session_id,
                    step_idx,
                    model = ?model,
                    input,
                    output,
                    cache,
                    reasoning,
                    cumulative_tokens,
                    "CLI ingest: Transcript 降级解析到一条 LLM 调用记录"
                );

                let parent_id_override = find_parent_session_id_in_transcript(path, session_id);
                let final_project = parent_id_override.unwrap_or_else(|| session_id.to_string());

                logs.push(Datalog {
                    source_name,
                    collected_at: Utc::now(),
                    source_api_key: None,
                    source_project: final_project,
                    source_model: model,
                    source_datetime: datetime,
                    source_through_time: Duration::from_secs(0),
                    source_parent_project: None,
                    source_report_class: report_class,
                    token_info: TokenInfo { input, output, cache, resourcing: 0, reasoning },
                });
            }

            cumulative_tokens += output + reasoning;
            tokens_at_last_model = cumulative_tokens;
        } else {
            let output = estimator.estimate_text(content_str);
            let reasoning = estimator.estimate_text(thinking_str);
            cumulative_tokens += output + reasoning;
        }
    }

    lock.insert(path_buf, FileOffsetState {
        offset: current_offset,
        cumulative_tokens,
        tokens_at_last_model,
    });

    Ok(logs)
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

