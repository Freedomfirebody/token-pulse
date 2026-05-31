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
            rpc_timeout: Duration::from_secs(15),
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
                let mut session_ids: Vec<String> = rpc_sessions.iter()
                    .map(|s| s.session_id.clone())
                    .collect();

                let brain_ids = scan_brain_session_ids(&session_root);
                for id in brain_ids {
                    if !session_ids.contains(&id) {
                        session_ids.push(id);
                    }
                }

                info!(
                    rpc_count = rpc_sessions.len(),
                    total = session_ids.len(),
                    "CLI ingest: session 发现完成"
                );

                // 对每个 session 增量分段拉取
                for (idx, session_id) in session_ids.iter().enumerate() {
                    let start_offset = store.lock().session_offset(session_id);
                    let step_hint = rpc_sessions.iter()
                        .find(|s| s.session_id == *session_id)
                        .and_then(|s| s.step_count)
                        .unwrap_or(5000);

                    if start_offset == 0 && step_hint > 0 {
                        info!(
                            progress = format!("{}/{}", idx + 1, session_ids.len()),
                            session_id,
                            step_hint,
                            "CLI ingest: 开始采集 session"
                        );
                    }

                    let fetched = Self::stream_session_steps(
                        &client, conn, session_id, start_offset, step_hint,
                        source_name, &store,
                    ).await;

                    if fetched > 0 {
                        rpc_session_ids.insert(session_id.clone());
                    }
                }

                // 刷盘 session offsets
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
    ) -> usize {
        let mut offset = start_offset;
        let mut total_fetched = 0usize;
        let mut batch_count = 0u32;
        let mut current_batch_size = BATCH_SIZE; // 安全起始大小，避免 200 导致 local server 超时挂起
        let mut consecutive_failures = 0;

        loop {
            // 已超过已知步数上限 → 停止
            if offset >= step_hint {
                break;
            }

            let end = offset + current_batch_size;
            match client.get_trajectory_steps_paged(conn, session_id, offset, end).await {
                Ok(steps) => {
                    consecutive_failures = 0; // 重置连续失败计数
                    if steps.is_empty() { break; }

                    // 如果成功且分片较小，逐步增大分片以提高速度（上限 100）
                    if current_batch_size < 100 {
                        current_batch_size = (current_batch_size + 10).min(100);
                    }

                    // 首批首个 step: 打印 JSON 结构用于诊断
                    if batch_count == 0 {
                        if let Some(first) = steps.first() {
                            let keys: Vec<&str> = first.as_object()
                                .map(|o| o.keys().map(|k| k.as_str()).collect())
                                .unwrap_or_default();
                            info!(
                                session_id,
                                keys = ?keys,
                                "CLI ingest: 首个 step JSON keys"
                            );
                        }
                    }
                    batch_count += 1;

                    let metadata: Vec<GeneratorMetadata> = steps.iter().enumerate()
                        .filter_map(|(i, step)| {
                            GeneratorMetadata::from_step_json(step, offset + i as u32)
                        })
                        .collect();

                    let batch_meta_count = metadata.len();
                    if !metadata.is_empty() {
                        let datalogs = metadata_to_datalogs(source_name, session_id, &metadata);
                        total_fetched += datalogs.len();
                        store.lock().append(datalogs);
                    }

                    let batch_len = steps.len() as u32;
                    offset += batch_len;
                    store.lock().advance_session_offset(session_id, offset);

                    // 仅前几批打印详细日志，后续静默
                    if batch_count <= 3 || batch_meta_count > 0 {
                        info!(
                            session_id,
                            batch_steps = batch_len,
                            batch_metadata = batch_meta_count,
                            offset,
                            current_batch_size,
                            "CLI ingest: batch 完成"
                        );
                    }
                }
                Err(e) => {
                    consecutive_failures += 1;
                    warn!(
                        session_id,
                        offset,
                        current_batch_size,
                        failures = consecutive_failures,
                        error = %e,
                        "CLI 分段步骤获取失败，尝试减小分片大小"
                    );

                    if current_batch_size > 5 {
                        // 动态减半并设置不小于 5 的重试分片
                        current_batch_size = (current_batch_size / 2).max(5);
                        info!(
                            session_id,
                            offset,
                            new_batch_size = current_batch_size,
                            "调整分片大小以重新尝试"
                        );
                        // 给本地 language server 0.5s 恢复喘息时间
                        tokio::time::sleep(Duration::from_millis(500)).await;
                    } else {
                        warn!(
                            session_id,
                            offset,
                            "最小分片下获取仍失败，停止当前 session"
                        );
                        break;
                    }
                }
            }
        }

        if total_fetched > 0 || offset > start_offset {
            info!(session_id, total_fetched, offset, "CLI session steps 流式采集完成");
        }

        total_fetched
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
) -> Vec<Datalog> {
    metadata.iter().filter_map(|m| {
        let model = model_aliases::resolve_model_placeholder(&m.response_model)
            .or_else(|| model_aliases::resolve_model_placeholder(&m.model))
            .map(|s| s.to_string())
            .unwrap_or_else(|| m.response_model.clone());

        let timestamp = m.timestamp.unwrap_or_else(Utc::now);

        Some(Datalog {
            source_name,
            collected_at: Utc::now(),
            source_api_key: None,
            source_project: session_id.to_string(),
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

/// 扫描 brain 目录获取所有 session_id
fn scan_brain_session_ids(session_root: &Path) -> Vec<String> {
    let brain_dir = session_root.join("brain");
    if !brain_dir.exists() { return vec![]; }

    let mut ids = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&brain_dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.len() >= 32 && name.contains('-') {
                    ids.push(name);
                }
            }
        }
    }
    ids
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
                    input = inp;
                    output = out;
                    cache = oc.unwrap_or(0);
                    reasoning = or_.unwrap_or(0);
                    report_class = ReportClass::Official;
                } else {
                    output = estimator.estimate_text(content_str);
                    reasoning = estimator.estimate_text(thinking_str);
                    input = 500 + cumulative_tokens;
                    cache = tokens_at_last_model;
                }
            } else {
                output = estimator.estimate_text(content_str);
                reasoning = estimator.estimate_text(thinking_str);
                input = 500 + cumulative_tokens;
                cache = tokens_at_last_model;
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

                logs.push(Datalog {
                    source_name,
                    collected_at: Utc::now(),
                    source_api_key: None,
                    source_project: session_id.to_string(),
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
