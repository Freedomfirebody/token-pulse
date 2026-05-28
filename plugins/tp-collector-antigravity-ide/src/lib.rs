//! # tp-collector-antigravity-ide
//!
//! Antigravity IDE 数据采集组件 — 文件系统 transcript.jsonl 直接采集。
//! 扫描 `~/.gemini/antigravity/brain/` 目录下的 `.system_generated/logs/transcript.jsonl`，
//! 高性能解析离线会话遥测数据。

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

/// Antigravity IDE (文件系统 transcript.jsonl 直接采集)
pub struct AntigravityIDECollector {
    session_root: PathBuf,
}

impl AntigravityIDECollector {
    pub fn new(session_root: PathBuf) -> Self {
        Self { session_root }
    }

    fn scan_sessions_v2(&self, since: Option<DateTime<Utc>>) -> Result<Vec<Datalog>, CollectionError> {
        let brain_root = self.session_root.join("brain");
        if !brain_root.exists() {
            return Err(CollectionError::SourceUnavailable(format!(
                "Brain directory not found: {}", brain_root.display()
            )));
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

    fn parse_transcript_file(&self, path: &Path, session_id: &str, fallback_timestamp_ms: Option<i64>, since: Option<DateTime<Utc>>) -> Result<Vec<Datalog>, CollectionError> {
        let file = std::fs::File::open(path).map_err(CollectionError::Io)?;
        let reader = BufReader::new(file);
        let mut logs = Vec::new();
        let mut cumulative_chars = 0;

        for line in reader.lines() {
            let line = match line { Ok(l) => l, Err(_) => continue };
            let trimmed = line.trim();
            if trimmed.is_empty() { continue; }

            let value: Value = match serde_json::from_str(trimmed) {
                Ok(v) => v, Err(_) => continue,
            };

            let (datalog_opt, step_chars) = self.extract_datalog_from_transcript_step_with_accumulation(&value, session_id, fallback_timestamp_ms, cumulative_chars);
            cumulative_chars += step_chars;

            if let Some(datalog) = datalog_opt {
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

    fn extract_datalog_from_transcript_step_with_accumulation(
        &self,
        value: &Value,
        session_id: &str,
        fallback_timestamp_ms: Option<i64>,
        cumulative_chars: usize,
    ) -> (Option<Datalog>, usize) {
        let source = value.get("source").and_then(|v| v.as_str()).unwrap_or("");
        let content_str = value.get("content").and_then(|v| v.as_str()).unwrap_or("");
        let thinking_str = value.get("thinking").and_then(|v| v.as_str()).unwrap_or("");
        
        let content_len = content_str.len();
        let thinking_len = thinking_str.len();
        let step_chars = content_len + thinking_len;

        let mut input = 0;
        let mut output = 0;
        let mut cache = 0;
        let mut reasoning = 0;
        let mut model = "gemini-3.5-flash".to_string();
        let mut report_class = ReportClass::Calculate;

        let mut is_model_call = false;

        if let Some(usage) = value.get("usage_metadata").filter(|v| !v.is_null()) {
            input = usage.get("prompt_token_count")
                .and_then(|v| v.as_u64()).unwrap_or(0);
            output = usage.get("candidates_token_count")
                .and_then(|v| v.as_u64()).unwrap_or(0);
            cache = usage.get("cached_content_token_count")
                .and_then(|v| v.as_u64()).unwrap_or(0);
            reasoning = usage.get("thoughts_token_count")
                .and_then(|v| v.as_u64()).unwrap_or(0);

            let model_raw = value.get("model")
                .or_else(|| usage.get("model"))
                .and_then(|v| v.as_str())
                .unwrap_or("gemini-3.5-flash");
            model = resolve_model(model_raw);
            report_class = ReportClass::Official;
            is_model_call = true;
        } else if source == "MODEL" {
            // 如果是 MODEL 生成的步骤，但缺少显式的 usage_metadata，我们进行智能离线估算
            output = ((content_len as f64) * 0.35).round() as u64;
            reasoning = ((thinking_len as f64) * 0.35).round() as u64;
            
            // 估算系统提示词和可用的 tools 声明基础开销为 1500 tokens
            input = 1500 + ((cumulative_chars as f64) * 0.35).round() as u64;
            cache = if input > 3000 { input - 2000 } else { 0 };

            let model_raw = value.get("model")
                .and_then(|v| v.as_str())
                .unwrap_or("gemini-3.5-flash");
            model = resolve_model(model_raw);
            report_class = ReportClass::Calculate;
            is_model_call = true;
        }

        if !is_model_call || (input == 0 && output == 0 && cache == 0 && reasoning == 0) {
            return (None, step_chars);
        }

        let step_idx = value.get("step_index").and_then(|v| v.as_i64()).unwrap_or(0);
        let base_ts = fallback_timestamp_ms.unwrap_or_else(|| Utc::now().timestamp_millis());
        let timestamp_ms = base_ts + step_idx * 1000;
        let datetime = DateTime::from_timestamp_millis(timestamp_ms).unwrap_or_else(Utc::now);

        (Some(Datalog {
            source_name: SourceName::AntigravityIDE,
            collected_at: Utc::now(),
            source_api_key: None,
            source_project: session_id.to_string(),
            source_model: model,
            source_datetime: datetime,
            source_through_time: Duration::from_secs(0),
            source_parent_project: None,
            source_report_class: report_class,
            token_info: TokenInfo { input, output, cache, resourcing: 0, reasoning },
        }), step_chars)
    }
}

#[async_trait]
impl DatasourceProvider for AntigravityIDECollector {
    fn name(&self) -> SourceName { SourceName::AntigravityIDE }
    fn description(&self) -> &str { "Antigravity IDE (文件系统 transcript.jsonl 直接采集) — 高性能离线采集" }

    async fn collect(&self) -> Result<Vec<Datalog>, CollectionError> {
        let session_root = self.session_root.clone();
        let collector = AntigravityIDECollector::new(session_root);
        tokio::task::spawn_blocking(move || collector.scan_sessions_v2(None))
            .await
            .map_err(|e| CollectionError::Unknown(format!("Task join error: {}", e)))?
    }

    async fn collect_since(&self, since: DateTime<Utc>) -> Result<Vec<Datalog>, CollectionError> {
        let session_root = self.session_root.clone();
        let collector = AntigravityIDECollector::new(session_root);
        tokio::task::spawn_blocking(move || collector.scan_sessions_v2(Some(since)))
            .await
            .map_err(|e| CollectionError::Unknown(format!("Task join error: {}", e)))?
    }

    async fn health_check(&self) -> Result<bool, CollectionError> {
        Ok(self.session_root.join("brain").exists())
    }
}
