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
            rpc_timeout: Duration::from_secs(15),
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

                // IDE 版仅通过 brain/ 目录发现 session
                let session_ids = scan_brain_session_ids(&session_root);

                info!(
                    count = session_ids.len(),
                    "IDE ingest: session 发现完成"
                );

                for session_id in &session_ids {
                    let start_offset = store.lock().session_offset(session_id);

                    stream_session_steps(
                        &client, conn, session_id, start_offset,
                        5000, // IDE 无 step_count hint，使用高上限
                        source_name, &store,
                    ).await;
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
    let mut offset = start_offset;
    let mut total_fetched = 0usize;
    let mut batch_count = 0u32;
    let mut current_batch_size = 20; // 安全起始大小，避免 200 导致 local server 超时挂起
    let mut consecutive_failures = 0;

    loop {
        if offset >= step_hint {
            break;
        }

        let end = offset + current_batch_size;
        match client.get_trajectory_steps_paged(conn, session_id, offset, end).await {
            Ok(steps) => {
                consecutive_failures = 0; // 重置连续失败计数
                if steps.is_empty() { break; }

                // 如果成功且分片较小，逐步增大分片以提高速度（上限 100）

                // 首批首个 step: 打印 JSON keys
                if batch_count == 0 {
                    if let Some(first) = steps.first() {
                        let keys: Vec<&str> = first.as_object()
                            .map(|o| o.keys().map(|k| k.as_str()).collect())
                            .unwrap_or_default();
                        info!(
                            session_id,
                            keys = ?keys,
                            "IDE ingest: 首个 step JSON keys"
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

                if batch_count <= 3 || batch_meta_count > 0 {
                    info!(
                        session_id,
                        batch_steps = batch_len,
                        batch_metadata = batch_meta_count,
                        offset,
                        current_batch_size,
                        "IDE ingest: batch 完成"
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
                    "IDE 分段步骤获取失败，尝试减小分片大小"
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
        info!(session_id, total_fetched, offset, "IDE session steps 流式采集完成");
    }

    total_fetched
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
