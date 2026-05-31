//! # tp-collector-antigravity
//!
//! Antigravity 数据采集组件 — 扫描 `~/.gemini/antigravity/brain/` 目录，
//! 解析 JSONL/JSON 会话日志，生成 Datalog 记录。

mod config;
mod types;
mod store;
mod model_aliases;
mod scanner;
mod rpc;

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use tracing::{debug, info};

use tp_protocol::{
    CollectionError, Datalog, DatasourceProvider, ReportClass, SourceName, TokenInfo,
};


/// 模型占位符映射表
fn resolve_model_placeholder(value: &str) -> Option<&'static str> {
    match value {
        "MODEL_PLACEHOLDER_M16" => Some("gemini-3.1-pro-high"),
        "MODEL_PLACEHOLDER_M133" | "gemini-3-flash-b" | "gemini-3-flash-agent"
        | "MODEL_PLACEHOLDER_M187" | "MODEL_PLACEHOLDER_M20" => Some("gemini-3.5-flash"),
        "MODEL_PLACEHOLDER_M37" => Some("gemini-3.1-pro-high"),
        "MODEL_PLACEHOLDER_M36" => Some("gemini-3.1-pro-low"),
        "MODEL_PLACEHOLDER_M18" => Some("gemini-3-flash"),
        "MODEL_PLACEHOLDER_M8" => Some("gemini-3-pro-high"),
        "MODEL_PLACEHOLDER_M7" => Some("gemini-3-pro-low"),
        "MODEL_PLACEHOLDER_M26" | "claude-opus-4-6-thinking" => Some("claude-opus-4-6-thinking"),
        "MODEL_PLACEHOLDER_M35" | "claude-sonnet-4-6-thinking" => Some("claude-sonnet-4-6-thinking"),
        "MODEL_PLACEHOLDER_M12" => Some("claude-opus-4-5-thinking"),
        "MODEL_CLAUDE_4_5_SONNET" => Some("claude-sonnet-4-5"),
        "MODEL_CLAUDE_4_5_SONNET_THINKING" => Some("claude-sonnet-4-5-thinking"),
        "MODEL_GOOGLE_GEMINI_2_5_FLASH" => Some("gemini-2.5-flash"),
        "MODEL_GOOGLE_GEMINI_2_5_FLASH_LITE" => Some("gemini-2.5-flash-lite"),
        _ => None,
    }
}

fn resolve_model(raw: &str) -> String {
    resolve_model_placeholder(raw)
        .map(|s| s.to_string())
        .unwrap_or_else(|| raw.to_string())
}

#[derive(Clone, Copy, Default)]
struct FileOffsetState {
    offset: u64,
    cumulative_chars: usize,
    chars_at_last_model: usize,
}

/// Antigravity 数据采集器
#[derive(Clone)]
pub struct AntigravityCollector {
    session_root: PathBuf,
    source_name: SourceName,
    last_read_offsets: std::sync::Arc<parking_lot::Mutex<std::collections::HashMap<PathBuf, FileOffsetState>>>,
}

impl AntigravityCollector {
    pub fn new(session_root: PathBuf, source_name: SourceName) -> Self {
        Self {
            session_root,
            source_name,
            last_read_offsets: std::sync::Arc::new(parking_lot::Mutex::new(std::collections::HashMap::new())),
        }
    }

    pub fn default_session_root() -> PathBuf {
        dirs::home_dir()
            .map(|h| h.join(".gemini").join("antigravity"))
            .unwrap_or_else(|| PathBuf::from("."))
    }

    async fn sync_rpc(&self) -> Result<(), CollectionError> {
        let session_root = self.session_root.to_string_lossy().to_string();
        let config = store::SettingsStore::new(&session_root).load_config();
        info!("sync_rpc start, session_root {}, config: {:?}", &session_root, &config);

        let scan_res = {
            let session_root_clone = session_root.clone();
            tokio::task::spawn_blocking(move || {
                let scanner = scanner::SessionScanner::new();
                scanner.scan(&session_root_clone)
            })
            .await
            .map_err(|e| CollectionError::Unknown(format!("Task join error during scan: {}", e)))?
        };

        let candidates = scan_res
            .map_err(|e| CollectionError::Unknown(format!("Scan failed: {}", e)))?;

        let exporter = self::rpc::TrajectoryExporter::new(config);
        let _ = exporter.export_changed_sessions(&candidates, false, true)
            .await
            .map_err(|e| CollectionError::Unknown(format!("Export failed: {}", e)))?;
        Ok(())
    }

    fn scan_sessions(&self, since: Option<DateTime<Utc>>) -> Result<Vec<Datalog>, CollectionError> {
        let folder_name = self.session_root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("antigravity")
            .to_string();
        let cache_root = dirs::home_dir()
            .map(|h| h.join(".token-pulse").join("token-monitor"))
            .unwrap_or_else(|| PathBuf::from(".token-pulse").join("token-monitor"))
            .join(folder_name)
            .join("rpc-cache")
            .join("v1");
        if !cache_root.exists() {
            return Err(CollectionError::SourceUnavailable(format!(
                "RPC cache directory not found: {}", cache_root.display()
            )));
        }

        let mut all_logs = Vec::new();
        let entries = std::fs::read_dir(&cache_root).map_err(CollectionError::Io)?;

        for entry in entries.flatten() {
            if !entry.path().is_dir() { continue; }
            let session_id = entry.file_name().to_string_lossy().to_string();
            match self.parse_session(&entry.path(), &session_id, since) {
                Ok(logs) => {
                    if !logs.is_empty() {
                        info!(session_id = %session_id, count = logs.len(), "解析会话成功");
                        all_logs.extend(logs);
                    }
                }
                Err(e) => {
                    info!(session_id = %session_id, error = %e, "跳过会话");
                }
            }
        }
        Ok(all_logs)
    }

    fn parse_session(&self, session_dir: &Path, session_id: &str, since: Option<DateTime<Utc>>) -> Result<Vec<Datalog>, CollectionError> {
        let mut logs = Vec::new();
        let usage_path = session_dir.join("usage.jsonl");

        if usage_path.exists() {
            let manifest_path = session_dir.join("manifest.json");
            let mut session_last_modified: Option<i64> = None;
            if manifest_path.exists() {
                if let Ok(file) = std::fs::File::open(&manifest_path) {
                    if let Ok(val) = serde_json::from_reader::<_, serde_json::Value>(file) {
                        session_last_modified = val.get("server_last_modified_ms")
                            .or_else(|| val.get("serverLastModifiedMs"))
                            .and_then(|v| v.as_i64());
                    }
                }
            }

            if session_last_modified.is_none() {
                if let Ok(meta) = std::fs::metadata(&usage_path) {
                    let stable_time = meta.created().or_else(|_| meta.modified());
                    if let Ok(time) = stable_time {
                        if let Ok(dur) = time.duration_since(std::time::SystemTime::UNIX_EPOCH) {
                            session_last_modified = Some(dur.as_millis() as i64);
                        }
                    }
                }
            }

            // 如果设置了增量开始时间 since，且会话最终修改时间早于 since，则可以直接完全跳过此会话文件的解析！
            if let (Some(mtime_ms), Some(since_dt)) = (session_last_modified, since) {
                if mtime_ms < since_dt.timestamp_millis() {
                    return Ok(Vec::new());
                }
            }

            if let Ok(parsed) = self.parse_jsonl_file(&usage_path, session_id, session_last_modified, since) {
                logs.extend(parsed);
            }
        }
        Ok(logs)
    }

    fn parse_jsonl_file(&self, path: &Path, session_id: &str, fallback_timestamp_ms: Option<i64>, since: Option<DateTime<Utc>>) -> Result<Vec<Datalog>, CollectionError> {
        use std::io::{BufRead, BufReader, Seek, SeekFrom};

        let mut file = std::fs::File::open(path).map_err(CollectionError::Io)?;
        let meta = file.metadata().map_err(CollectionError::Io)?;
        let file_size = meta.len();

        let path_buf = path.to_path_buf();
        let mut offsets = self.last_read_offsets.lock();
        let state = offsets.get(&path_buf).cloned().unwrap_or_default();
        
        let mut start_offset = state.offset;

        // 如果文件缩水了（可能是Compaction或被截断重构），重置起始偏移为0
        if file_size < start_offset {
            start_offset = 0;
        }

        file.seek(SeekFrom::Start(start_offset)).map_err(CollectionError::Io)?;
        let mut reader = BufReader::new(file);
        let mut logs = Vec::new();
        let mut current_offset = start_offset;

        let mut line = String::new();
        loop {
            line.clear();
            let bytes_read = reader.read_line(&mut line).map_err(CollectionError::Io)?;
            if bytes_read == 0 {
                break;
            }
            
            let trimmed = line.trim();
            current_offset += bytes_read as u64;

            if trimmed.is_empty() { continue; }

            let value: Value = match serde_json::from_str(trimmed) {
                Ok(v) => v, Err(_) => continue,
            };

            if let Some(datalog) = self.extract_datalog_from_step(&value, session_id, fallback_timestamp_ms) {
                // 行级增量时间过滤
                if let Some(since_dt) = since {
                    if datalog.source_datetime < since_dt {
                        continue;
                    }
                }
                logs.push(datalog);
            }
        }

        offsets.insert(path_buf, FileOffsetState {
            offset: current_offset,
            cumulative_chars: 0,
            chars_at_last_model: 0,
        });

        Ok(logs)
    }

    fn extract_datalog_from_step(&self, value: &Value, session_id: &str, fallback_timestamp_ms: Option<i64>) -> Option<Datalog> {
        let usage = value.get("usage").or_else(|| value.get("token_usage")).unwrap_or(value);

        let input = usage.get("input_tokens")
            .or_else(|| usage.get("inputTokens"))
            .and_then(|v| v.as_u64()).unwrap_or(0);
        let output = usage.get("output_tokens")
            .or_else(|| usage.get("outputTokens"))
            .and_then(|v| v.as_u64()).unwrap_or(0);
        let cache_read = usage.get("cache_read_tokens")
            .or_else(|| usage.get("cacheReadTokens"))
            .and_then(|v| v.as_u64()).unwrap_or(0);
        let cache_write = usage.get("cache_creation_tokens")
            .or_else(|| usage.get("cacheCreationTokens"))
            .and_then(|v| v.as_u64()).unwrap_or(0);
        let reasoning = usage.get("reasoning_tokens")
            .or_else(|| usage.get("reasoningTokens"))
            .or_else(|| usage.get("thinkingOutputTokens"))
            .and_then(|v| v.as_u64()).unwrap_or(0);

        if input == 0 && output == 0 && cache_read == 0 && cache_write == 0 { return None; }

        let model_raw = usage.get("model").or_else(|| usage.get("modelId"))
            .or_else(|| usage.get("responseModel"))
            .and_then(|v| v.as_str()).unwrap_or("unknown");
        let model = resolve_model(model_raw);

        // 提取并解析真实的时间戳或使用 fallback，配合 sequence 产生唯一的毫秒级偏移以避免去重覆盖
        let mut ts = None;
        let ts_paths = [
            value.get("timestamp"),
            value.get("created_at"),
            value.get("createdAt"),
            usage.get("timestamp"),
            usage.get("created_at"),
            usage.get("createdAt"),
            value.get("raw").and_then(|r| r.get("chatModel")).and_then(|c| c.get("chatStartMetadata")).and_then(|m| m.get("createdAt")),
            value.get("raw").and_then(|r| r.get("chatModel")).and_then(|c| c.get("createdAt")),
            value.get("raw").and_then(|r| r.get("createdAt")),
        ];

        for val in ts_paths.into_iter().flatten() {
            if val.is_null() { continue; }
            if let Some(n) = val.as_i64() {
                ts = Some(n);
                break;
            }
            if let Some(s) = val.as_str() {
                if let Ok(n) = s.parse::<i64>() {
                    ts = Some(n);
                    break;
                }
                if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
                    ts = Some(dt.timestamp_millis());
                    break;
                }
                if let Ok(dt) = DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.3fZ") {
                    ts = Some(dt.timestamp_millis());
                    break;
                }
            }
        }

        let base_ts = ts.or(fallback_timestamp_ms).unwrap_or_else(|| Utc::now().timestamp_millis());
        // 优先使用 stepIndex (与 IDE 路径的 step_index 一致)，保证 UID 在两个采集路径间对齐
        let step_offset = value.get("stepIndex")
            .or_else(|| value.get("step_index"))
            .and_then(|v| v.as_i64())
            .or_else(|| value.get("sequence").and_then(|v| v.as_i64()))
            .unwrap_or(0);
        let timestamp_ms = base_ts + step_offset * 1000;

        let datetime = DateTime::from_timestamp_millis(timestamp_ms).unwrap_or_else(Utc::now);

        // RPC 缓存中的数据一定是官方精确值
        let report_class = ReportClass::Official;

        Some(Datalog {
            source_name: self.source_name,
            collected_at: Utc::now(),
            source_api_key: None,
            source_project: session_id.to_string(),
            source_model: model,
            source_datetime: datetime,
            source_through_time: Duration::from_secs(0),
            source_parent_project: None,
            source_report_class: report_class,
            token_info: TokenInfo { input, output, cache: cache_read + cache_write, resourcing: 0, reasoning },
        })
    }

    // ===== IDE 直接采集核心逻辑 =====

    fn scan_sessions_v2(&self, since: Option<DateTime<Utc>>) -> Result<Vec<Datalog>, CollectionError> {
        let brain_root = self.session_root.join("brain");
        if !brain_root.exists() {
            // 如果 brain 目录不存在，静默跳过
            return Ok(Vec::new());
        }

        let mut all_logs = Vec::new();
        let entries = std::fs::read_dir(&brain_root).map_err(CollectionError::Io)?;

        for entry in entries.flatten() {
            if !entry.path().is_dir() { continue; }
            let session_id = entry.file_name().to_string_lossy().to_string();
            let transcript_path = entry.path().join(".system_generated").join("logs").join("transcript.jsonl");

            if transcript_path.exists() {
                let mut file_last_modified: Option<i64> = None;
                if let Ok(meta) = std::fs::metadata(&transcript_path) {
                    if let Ok(modified) = meta.modified() {
                        if let Ok(dur) = modified.duration_since(std::time::SystemTime::UNIX_EPOCH) {
                            file_last_modified = Some(dur.as_millis() as i64);
                        }
                    }
                }

                if let (Some(mtime_ms), Some(since_dt)) = (file_last_modified, since) {
                    if mtime_ms < since_dt.timestamp_millis() {
                        continue;
                    }
                }

                match self.parse_transcript_file(&transcript_path, &session_id, file_last_modified, since) {
                    Ok(logs) => {
                        if !logs.is_empty() {
                            debug!(session_id = %session_id, count = logs.len(), "解析 IDE 会话成功");
                            all_logs.extend(logs);
                        }
                    }
                    Err(e) => {
                        debug!(session_id = %session_id, error = %e, "跳过 IDE 会话");
                    }
                }
            }
        }
        Ok(all_logs)
    }

    fn parse_transcript_file(
        &self,
        path: &Path,
        session_id: &str,
        fallback_timestamp_ms: Option<i64>,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<Datalog>, CollectionError> {
        use std::io::{BufRead, BufReader, Seek, SeekFrom};

        let mut file = std::fs::File::open(path).map_err(CollectionError::Io)?;
        let meta = file.metadata().map_err(CollectionError::Io)?;
        let file_size = meta.len();

        let path_buf = path.to_path_buf();
        let mut offsets = self.last_read_offsets.lock();
        let state = offsets.get(&path_buf).cloned().unwrap_or_default();

        let mut start_offset = state.offset;
        let mut cumulative_chars = state.cumulative_chars;
        let mut chars_at_last_model = state.chars_at_last_model;

        // 如果文件缩水了（可能是Compaction或被截断重构），重置所有状态为0
        if file_size < start_offset {
            start_offset = 0;
            cumulative_chars = 0;
            chars_at_last_model = 0;
        }

        file.seek(SeekFrom::Start(start_offset)).map_err(CollectionError::Io)?;
        let mut reader = BufReader::new(file);
        let mut current_offset = start_offset;

        // 计算基于创建时间（具有修改时间备份）的稳定基准时间戳
        let mut stable_ts = None;
        let stable_time = meta.created().or_else(|_| meta.modified());
        if let Ok(time) = stable_time {
            if let Ok(dur) = time.duration_since(std::time::SystemTime::UNIX_EPOCH) {
                stable_ts = Some(dur.as_millis() as i64);
            }
        }
        let stable_fallback_ms = stable_ts.or(fallback_timestamp_ms);

        let mut new_values = Vec::new();
        let mut line = String::new();
        loop {
            line.clear();
            let bytes_read = reader.read_line(&mut line).map_err(CollectionError::Io)?;
            if bytes_read == 0 {
                break;
            }

            let trimmed = line.trim();
            current_offset += bytes_read as u64;

            if trimmed.is_empty() { continue; }

            let value: Value = match serde_json::from_str(trimmed) {
                Ok(v) => v, Err(_) => continue,
            };
            new_values.push(value);
        }

        let mut logs = Vec::new();

        enum GroupedStep {
            ModelGroup(Vec<Value>),
            Other(Value),
        }

        let mut grouped_steps = Vec::new();
        let mut current_group = Vec::new();

        for val in new_values {
            let source = val.get("source").and_then(|v| v.as_str()).unwrap_or("");
            let has_usage = val.get("usage_metadata").filter(|v| !v.is_null()).is_some();
            if source == "MODEL" || has_usage {
                current_group.push(val);
            } else {
                if !current_group.is_empty() {
                    grouped_steps.push(GroupedStep::ModelGroup(std::mem::take(&mut current_group)));
                }
                grouped_steps.push(GroupedStep::Other(val));
            }
        }
        if !current_group.is_empty() {
            grouped_steps.push(GroupedStep::ModelGroup(current_group));
        }

        for step in grouped_steps {
            match step {
                GroupedStep::Other(val) => {
                    let content_str = val.get("content").and_then(|v| v.as_str()).unwrap_or("");
                    let thinking_str = val.get("thinking").and_then(|v| v.as_str()).unwrap_or("");
                    cumulative_chars += content_str.len() + thinking_str.len();
                }
                GroupedStep::ModelGroup(group) => {
                    let mut group_content_len = 0;
                    let mut group_thinking_len = 0;
                    let mut stable_ts = None;
                    let mut step_idx = 0;
                    let mut model_raw = "gemini-3.5-flash";
                    let mut report_class = ReportClass::Calculate;

                    let mut official_input = None;
                    let mut official_output = None;
                    let mut official_cache = None;
                    let mut official_reasoning = None;

                    for val in &group {
                        let content_str = val.get("content").and_then(|v| v.as_str()).unwrap_or("");
                        let thinking_str = val.get("thinking").and_then(|v| v.as_str()).unwrap_or("");
                        group_content_len += content_str.len();
                        group_thinking_len += thinking_str.len();

                        if step_idx == 0 {
                            step_idx = val.get("step_index").and_then(|v| v.as_i64()).unwrap_or(0);
                        }

                        if let Some(m) = val.get("model").and_then(|v| v.as_str()) {
                            model_raw = m;
                        }

                        if let Some(usage) = val.get("usage_metadata").filter(|v| !v.is_null()) {
                            official_input = usage.get("prompt_token_count").and_then(|v| v.as_u64());
                            official_output = usage.get("candidates_token_count").and_then(|v| v.as_u64());
                            official_cache = usage.get("cached_content_token_count").and_then(|v| v.as_u64());
                            official_reasoning = usage.get("thoughts_token_count").and_then(|v| v.as_u64());
                            report_class = ReportClass::Official;
                        }

                        if stable_ts.is_none() {
                            let ts_paths = [
                                val.get("timestamp"),
                                val.get("created_at"),
                                val.get("createdAt"),
                            ];
                            for val_ts in ts_paths.into_iter().flatten() {
                                if val_ts.is_null() { continue; }
                                if let Some(n) = val_ts.as_i64() {
                                    stable_ts = Some(n);
                                    break;
                                }
                                if let Some(s) = val_ts.as_str() {
                                    if let Ok(n) = s.parse::<i64>() {
                                        stable_ts = Some(n);
                                        break;
                                    }
                                    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
                                        stable_ts = Some(dt.timestamp_millis());
                                        break;
                                    }
                                }
                            }
                        }
                    }

                    let input;
                    let output;
                    let cache;
                    let reasoning;

                    if let (Some(inp), Some(out)) = (official_input, official_output) {
                        input = inp;
                        output = out;
                        cache = official_cache.unwrap_or(0);
                        reasoning = official_reasoning.unwrap_or(0);
                    } else {
                        output = ((group_content_len as f64) * 0.35).round() as u64;
                        reasoning = ((group_thinking_len as f64) * 0.35).round() as u64;

                        // Gemini API 每次调用发送完整上下文窗口作为 prompt
                        let full_context_tokens = 500 + ((cumulative_chars as f64) * 0.35).round() as u64;
                        input = full_context_tokens;
                        // 估算 cache: 之前已发送过的上下文部分可能命中缓存
                        let prev_context_tokens = ((chars_at_last_model as f64) * 0.35).round() as u64;
                        cache = prev_context_tokens;
                    }

                    if input > 0 || output > 0 || cache > 0 || reasoning > 0 {
                        let base_ts = stable_ts.or(stable_fallback_ms).unwrap_or_else(|| Utc::now().timestamp_millis());
                        let timestamp_ms = base_ts + step_idx * 1000;
                        let datetime = DateTime::from_timestamp_millis(timestamp_ms).unwrap_or_else(Utc::now);

                        let datalog = Datalog {
                            source_name: self.source_name,
                            collected_at: Utc::now(),
                            source_api_key: None,
                            source_project: session_id.to_string(),
                            source_model: resolve_model(model_raw),
                            source_datetime: datetime,
                            source_through_time: Duration::from_secs(0),
                            source_parent_project: None,
                            source_report_class: report_class,
                            token_info: TokenInfo { input, output, cache, resourcing: 0, reasoning },
                        };

                        if let Some(since_dt) = since {
                            if datalog.source_datetime < since_dt {
                                cumulative_chars += group_content_len + group_thinking_len;
                                chars_at_last_model = cumulative_chars;
                                continue;
                            }
                        }
                        logs.push(datalog);
                    }

                    cumulative_chars += group_content_len + group_thinking_len;
                    chars_at_last_model = cumulative_chars;
                }
            }
        }

        offsets.insert(path_buf, FileOffsetState {
            offset: current_offset,
            cumulative_chars,
            chars_at_last_model,
        });

        Ok(logs)
    }
}

#[async_trait]
impl DatasourceProvider for AntigravityCollector {
    fn name(&self) -> SourceName { self.source_name }
    fn description(&self) -> &str { "Antigravity (Gemini VS Code 插件 & IDE 离线直接采集) — 统一系统扫描采集" }

    async fn collect(&self) -> Result<Vec<Datalog>, CollectionError> {
        if let Err(e) = self.sync_rpc().await {
            debug!(error = %e, "Antigravity Telemetry RPC sync failed, falling back to cached files");
        }

        let collector = self.clone();

        // 1. 采集 RPC 缓存的会话数据
        let logs_rpc = tokio::task::spawn_blocking({
            let coll = collector.clone();
            move || coll.scan_sessions(None)
        })
        .await
        .map_err(|e| CollectionError::Unknown(format!("Task join error for RPC cache: {}", e)))?;

        let logs_rpc = match logs_rpc {
            Ok(logs) => logs,
            Err(CollectionError::SourceUnavailable(_)) => {
                debug!("RPC cache not available for this session root, skipping");
                Vec::new()
            }
            Err(e) => return Err(e),
        };

        // 获取已通过 RPC 采集的 session_id 集合
        let rpc_session_ids: std::collections::HashSet<String> = logs_rpc
            .iter()
            .map(|log| log.source_project.clone())
            .collect();

        // 2. 采集 IDE 直接写入 brain 目录的会话数据
        let logs_ide = tokio::task::spawn_blocking({
            let coll = collector.clone();
            move || coll.scan_sessions_v2(None)
        })
        .await
        .map_err(|e| CollectionError::Unknown(format!("Task join error for Brain logs: {}", e)))??;

        // 过滤掉已在 logs_rpc 中采集的会话，防止在 transcript.jsonl 中重复统计导致 token 翻倍或指数累加
        let filtered_ide: Vec<Datalog> = logs_ide
            .into_iter()
            .filter(|log| !rpc_session_ids.contains(&log.source_project))
            .collect();

        let mut combined = logs_rpc;
        combined.extend(filtered_ide);
        Ok(combined)
    }

    async fn collect_since(&self, since: DateTime<Utc>) -> Result<Vec<Datalog>, CollectionError> {
        if let Err(e) = self.sync_rpc().await {
            debug!(error = %e, "Antigravity Telemetry RPC sync failed, falling back to cached files");
        }

        let collector = self.clone();

        // 1. 增量采集 VS Code RPC 缓存的会话数据
        let logs_rpc = tokio::task::spawn_blocking({
            let coll = collector.clone();
            move || coll.scan_sessions(Some(since))
        })
        .await
        .map_err(|e| CollectionError::Unknown(format!("Task join error for RPC cache: {}", e)))?;

        let logs_rpc = match logs_rpc {
            Ok(logs) => logs,
            Err(CollectionError::SourceUnavailable(_)) => {
                debug!("RPC cache not available for this session root, skipping");
                Vec::new()
            }
            Err(e) => return Err(e),
        };

        // 获取已通过 RPC 采集的 session_id 集合
        let rpc_session_ids: std::collections::HashSet<String> = logs_rpc
            .iter()
            .map(|log| log.source_project.clone())
            .collect();

        // 2. 增量采集 IDE 直接写入 brain 目录的会话数据
        let logs_ide = tokio::task::spawn_blocking({
            let coll = collector.clone();
            move || coll.scan_sessions_v2(Some(since))
        })
        .await
        .map_err(|e| CollectionError::Unknown(format!("Task join error for Brain logs: {}", e)))??;

        // 过滤掉已在 logs_rpc 中采集的会话，防止在 transcript.jsonl 中重复统计导致 token 翻倍或指数累加
        let filtered_ide: Vec<Datalog> = logs_ide
            .into_iter()
            .filter(|log| !rpc_session_ids.contains(&log.source_project))
            .collect();

        let mut combined = logs_rpc;
        combined.extend(filtered_ide);
        Ok(combined)
    }

    async fn health_check(&self) -> Result<bool, CollectionError> {
        let folder_name = self.session_root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("antigravity")
            .to_string();
        let cache_root = dirs::home_dir()
            .map(|h| h.join(".token-pulse").join("token-monitor"))
            .unwrap_or_else(|| PathBuf::from(".token-pulse").join("token-monitor"))
            .join(folder_name);
        // 如果 brain 目录或者 RPC 缓存目录存在，即认为数据源可用
        Ok(self.session_root.join("brain").exists() || cache_root.exists())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[tokio::test]
    #[ignore]
    async fn test_generate_actual_cache() {
        let session_root = PathBuf::from("C:\\Users\\smzhf\\.gemini\\antigravity");
        if !session_root.exists() {
            println!("Skipping test: session_root not found");
            return;
        }
        
        let folder_name = session_root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("antigravity")
            .to_string();
            
        let cache_root = dirs::home_dir()
            .unwrap()
            .join(".token-pulse")
            .join("token-monitor")
            .join(folder_name)
            .join("rpc-cache")
            .join("v1");
            
        if cache_root.exists() {
            println!("Clearing existing cache directory: {}", cache_root.display());
            let _ = std::fs::remove_dir_all(&cache_root);
        }
        
        let collector = AntigravityCollector::new(session_root, tp_protocol::SourceName::Antigravity);
        println!("Starting sync_rpc...");
        collector.sync_rpc().await.unwrap();
        println!("sync_rpc completed successfully!");
    }
}
