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
                        source_name, &store,
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
                // 转换为 datalogs 并追加到本地 store
                let datalogs = metadata_to_datalogs(source_name, session_id, &filtered_metadata);
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

