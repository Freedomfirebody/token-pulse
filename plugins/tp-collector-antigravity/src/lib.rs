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

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use tracing::debug;

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

/// Antigravity 数据采集器
pub struct AntigravityCollector {
    session_root: PathBuf,
}

impl AntigravityCollector {
    pub fn new(session_root: PathBuf) -> Self {
        Self {
            session_root,
        }
    }

    pub fn default_session_root() -> PathBuf {
        dirs::home_dir()
            .map(|h| h.join(".gemini").join("antigravity"))
            .unwrap_or_else(|| PathBuf::from("."))
    }

    async fn sync_rpc(&self) -> Result<(), CollectionError> {
        let session_root = self.session_root.to_string_lossy().to_string();
        let config = self::store::SettingsStore::new(&session_root).load_config();
        
        let scan_res = {
            let session_root_clone = session_root.clone();
            tokio::task::spawn_blocking(move || {
                let scanner = self::scanner::SessionScanner::new();
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
        let cache_root = self.session_root.join(".token-monitor").join("rpc-cache").join("v1");
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
                        debug!(session_id = %session_id, count = logs.len(), "解析会话成功");
                        all_logs.extend(logs);
                    }
                }
                Err(e) => {
                    debug!(session_id = %session_id, error = %e, "跳过会话");
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
                    if let Ok(modified) = meta.modified() {
                        if let Ok(dur) = modified.duration_since(std::time::SystemTime::UNIX_EPOCH) {
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
        let file = std::fs::File::open(path).map_err(CollectionError::Io)?;
        let reader = BufReader::new(file);
        let mut logs = Vec::new();

        for line in reader.lines() {
            let line = match line { Ok(l) => l, Err(_) => continue };
            let trimmed = line.trim();
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
        let sequence = value.get("sequence").and_then(|v| v.as_i64()).unwrap_or(0);
        let timestamp_ms = base_ts + sequence * 1000;

        let datetime = DateTime::from_timestamp_millis(timestamp_ms).unwrap_or_else(Utc::now);

        let report_class = if input > 0 || cache_read > 0 {
            ReportClass::Official
        } else {
            ReportClass::Calculate
        };

        Some(Datalog {
            source_name: SourceName::Antigravity,
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
}

#[async_trait]
impl DatasourceProvider for AntigravityCollector {
    fn name(&self) -> SourceName { SourceName::Antigravity }
    fn description(&self) -> &str { "Antigravity (Gemini VS Code Extension) — 文件系统扫描采集" }

    async fn collect(&self) -> Result<Vec<Datalog>, CollectionError> {
        if let Err(e) = self.sync_rpc().await {
            debug!(error = %e, "Antigravity Telemetry RPC sync failed, falling back to cached files");
        }

        let session_root = self.session_root.clone();
        let collector = AntigravityCollector::new(session_root);
        tokio::task::spawn_blocking(move || collector.scan_sessions(None))
            .await
            .map_err(|e| CollectionError::Unknown(format!("Task join error: {}", e)))?
    }

    async fn collect_since(&self, since: DateTime<Utc>) -> Result<Vec<Datalog>, CollectionError> {
        if let Err(e) = self.sync_rpc().await {
            debug!(error = %e, "Antigravity Telemetry RPC sync failed, falling back to cached files");
        }

        let session_root = self.session_root.clone();
        let collector = AntigravityCollector::new(session_root);
        tokio::task::spawn_blocking(move || collector.scan_sessions(Some(since)))
            .await
            .map_err(|e| CollectionError::Unknown(format!("Task join error: {}", e)))?
    }

    async fn health_check(&self) -> Result<bool, CollectionError> {
        Ok(self.session_root.join("brain").exists())
    }
}


