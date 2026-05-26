//! # tp-collector-codex
//!
//! Codex 数据采集组件 — 原生本地日志扫描与差分解析实现（不再依赖外部 ccusage 进程/服务）。

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use serde::Deserialize;
use tracing::info;

use tp_protocol::{
    CollectionError, Datalog, DatasourceProvider, ReportClass, SourceName, TokenInfo,
};

// ==================== Codex 数据反序列化类型 ====================

#[derive(Debug, Clone, Copy, Deserialize, Default, PartialEq, Eq)]
pub struct CodexRawUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub cached_input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub reasoning_output_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
}

#[derive(Debug, Deserialize, Clone)]
struct CodexInfo {
    model: Option<String>,
    model_name: Option<String>,
    total_token_usage: Option<CodexRawUsage>,
    last_token_usage: Option<CodexRawUsage>,
}

#[derive(Debug, Deserialize, Clone)]
struct CodexPayload {
    #[serde(rename = "type")]
    payload_type: Option<String>,
    model: Option<String>,
    model_name: Option<String>,
    info: Option<CodexInfo>,
}

#[derive(Debug, Deserialize, Clone)]
struct CodexSessionLogEntry {
    #[serde(rename = "type")]
    entry_type: Option<String>,
    timestamp: Option<serde_json::Value>,
    payload: Option<CodexPayload>,
}

#[derive(Debug, Deserialize, Clone)]
struct CodexResultFields {
    timestamp: Option<serde_json::Value>,
    model: Option<String>,
    model_name: Option<String>,
    usage: Option<CodexRawUsage>,
}

#[derive(Debug, Deserialize, Clone)]
struct CodexLogEntry {
    timestamp: Option<serde_json::Value>,
    model: Option<String>,
    model_name: Option<String>,
    usage: Option<CodexRawUsage>,
    data: Option<CodexResultFields>,
    result: Option<CodexResultFields>,
    response: Option<CodexResultFields>,
}

#[derive(Debug, Clone)]
struct CodexTokenUsageEvent {
    session_id: String,
    timestamp: DateTime<Utc>,
    model: Option<String>,
    input_tokens: u64,
    cached_input_tokens: u64,
    output_tokens: u64,
    reasoning_output_tokens: u64,
}

// ==================== 采集器主体 ====================

/// Codex 数据采集器
pub struct CodexCollector {
    timezone: String,
}

impl CodexCollector {
    pub fn new() -> Self {
        Self {
            timezone: "Asia/Shanghai".to_string(),
        }
    }

    pub fn with_timezone(mut self, tz: String) -> Self {
        self.timezone = tz;
        self
    }
}

// ==================== 路径探测与扫描 ====================

fn codex_home_paths() -> Result<Vec<PathBuf>, CollectionError> {
    if let Ok(env_paths) = std::env::var("CODEX_HOME") {
        return Ok(env_paths
            .split(',')
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .collect());
    }

    let home = dirs::home_dir().ok_or_else(|| {
        CollectionError::SourceUnavailable("Home directory is not set".to_string())
    })?;
    Ok(vec![home.join(".codex")])
}

fn codex_usage_paths() -> Result<Vec<PathBuf>, CollectionError> {
    let mut paths = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for path in codex_home_paths()? {
        let sessions = path.join("sessions");
        if sessions.is_dir() {
            if seen.insert(sessions.clone()) {
                paths.push(sessions);
            }
        } else if seen.insert(path.clone()) {
            paths.push(path);
        }
    }
    Ok(paths)
}

fn collect_usage_files(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        if let Ok(file_type) = entry.file_type() {
            let path = entry.path();
            if file_type.is_file() && path.extension().is_some_and(|ext| ext == "jsonl") {
                files.push(path);
            } else if file_type.is_dir() {
                collect_usage_files(&path, files);
            }
        }
    }
}

// ==================== 时间解析与辅助匹配 ====================

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

fn parse_ts_value(val: &serde_json::Value) -> Option<DateTime<Utc>> {
    if let Some(s) = val.as_str() {
        if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
            return Some(dt.with_timezone(&Utc));
        }
        // 尝试解析不标准的其它 rfc3339 变体
        if let Ok(dt) = DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.3fZ") {
            return Some(dt.with_timezone(&Utc));
        }
    } else if let Some(n) = val.as_u64() {
        let millis = if n > 10_000_000_000 {
            n
        } else {
            n.checked_mul(1_000).unwrap_or(n)
        };
        return Utc.timestamp_opt((millis / 1000) as i64, ((millis % 1000) * 1_000_000) as u32).single();
    }
    None
}

fn file_modified_time(path: &Path) -> DateTime<Utc> {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .map(|t| DateTime::<Utc>::from(t))
        .unwrap_or_else(|_| Utc::now())
}

fn codex_session_id(sessions_dir: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(sessions_dir).unwrap_or(path);
    let mut session_id = relative
        .with_extension("")
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect::<Vec<_>>()
        .join("/");
    if session_id.is_empty() {
        session_id = "unknown".to_string();
    }
    session_id
}

fn subtract_codex_raw_usage(
    current: &CodexRawUsage,
    previous: Option<&CodexRawUsage>,
) -> CodexRawUsage {
    let prev = previous.cloned().unwrap_or_default();
    CodexRawUsage {
        input_tokens: current.input_tokens.saturating_sub(prev.input_tokens),
        cached_input_tokens: current.cached_input_tokens.saturating_sub(prev.cached_input_tokens),
        output_tokens: current.output_tokens.saturating_sub(prev.output_tokens),
        reasoning_output_tokens: current.reasoning_output_tokens.saturating_sub(prev.reasoning_output_tokens),
        total_tokens: current.total_tokens.saturating_sub(prev.total_tokens),
    }
}

// ==================== 逐行流式解析 ====================

fn visit_codex_session_file(
    sessions_dir: &Path,
    path: &Path,
    events: &mut Vec<CodexTokenUsageEvent>,
    since: Option<DateTime<Utc>>,
) -> Result<(), String> {
    if let (Some(since_dt), Ok(meta)) = (since, fs::metadata(path)) {
        if let Ok(modified) = meta.modified() {
            if let Ok(dur) = modified.duration_since(std::time::SystemTime::UNIX_EPOCH) {
                if (dur.as_millis() as i64) < since_dt.timestamp_millis() {
                    return Ok(());
                }
            }
        }
    }

    let file = fs::File::open(path).map_err(|e| format!("Open failed: {}", e))?;
    let mut reader = BufReader::with_capacity(128 * 1024, file);
    let mut line = Vec::new();

    let session_id = codex_session_id(sessions_dir, path);
    let mut previous_totals: Option<CodexRawUsage> = None;
    let mut current_model: Option<String> = None;
    let fallback_timestamp = file_modified_time(path);

    // 高频字节段的 Finder
    let event_msg_finder = br#""type":"event_msg""#;
    let turn_context_finder = br#""type":"turn_context""#;
    let token_count_finder = br#""type":"token_count""#;
    let usage_finder = br#""usage":"#;
    let input_tokens_finder = br#""input_tokens":"#;
    let prompt_tokens_finder = br#""prompt_tokens":"#;

    loop {
        line.clear();
        let bytes_read = reader.read_until(b'\n', &mut line).map_err(|e| e.to_string())?;
        if bytes_read == 0 {
            break;
        }

        // 判断当前行的类型种类以快速跳过无关的日志行，将开销缩减 100 倍
        let is_session = find_bytes(&line, turn_context_finder).is_some()
            || (find_bytes(&line, event_msg_finder).is_some() && find_bytes(&line, token_count_finder).is_some());
        let is_headless = find_bytes(&line, usage_finder).is_some()
            || find_bytes(&line, input_tokens_finder).is_some()
            || find_bytes(&line, prompt_tokens_finder).is_some();

        if !is_session && !is_headless {
            continue;
        }

        if is_session {
            if let Ok(entry) = serde_json::from_slice::<CodexSessionLogEntry>(&line) {
                let entry_type = entry.entry_type.as_deref();
                if entry_type == Some("turn_context") {
                    if let Some(payload) = entry.payload {
                        if let Some(m) = payload.model.or(payload.model_name) {
                            current_model = Some(m);
                        }
                    }
                    continue;
                }
                if entry_type != Some("event_msg") {
                    continue;
                }
                let Some(payload) = entry.payload else { continue; };
                if payload.payload_type.as_deref() != Some("token_count") {
                    continue;
                }
                let Some(info) = payload.info else { continue; };
                let total_usage = info.total_token_usage;
                let raw_usage = info.last_token_usage.or_else(|| {
                    total_usage.map(|total| subtract_codex_raw_usage(&total, previous_totals.as_ref()))
                });

                if let Some(total) = total_usage {
                    previous_totals = Some(total);
                }

                let Some(raw) = raw_usage else { continue; };
                if raw.input_tokens == 0 && raw.cached_input_tokens == 0 && raw.output_tokens == 0 && raw.reasoning_output_tokens == 0 {
                    continue;
                }

                let parsed_model = info.model.or(info.model_name).or(payload.model).or(payload.model_name);
                if let Some(ref m) = parsed_model {
                    current_model = Some(m.clone());
                }

                let timestamp = entry.timestamp.and_then(|v| parse_ts_value(&v)).unwrap_or(fallback_timestamp);

                if let Some(since_dt) = since {
                    if timestamp < since_dt {
                        continue;
                    }
                }

                events.push(CodexTokenUsageEvent {
                    session_id: session_id.clone(),
                    timestamp,
                    model: parsed_model.or_else(|| current_model.clone()),
                    input_tokens: raw.input_tokens,
                    cached_input_tokens: raw.cached_input_tokens.min(raw.input_tokens),
                    output_tokens: raw.output_tokens,
                    reasoning_output_tokens: raw.reasoning_output_tokens,
                });
            }
        } else {
            // Headless 执行日志行
            if let Ok(entry) = serde_json::from_slice::<CodexLogEntry>(&line) {
                let mut raw_usage = entry.usage;
                let mut parsed_model = entry.model.or(entry.model_name);
                let mut timestamp = entry.timestamp.and_then(|v| parse_ts_value(&v));

                // 尝试从嵌套字段 data / result / response 提取数据
                if raw_usage.is_none() {
                    if let Some(ref d) = entry.data {
                        raw_usage = d.usage;
                        parsed_model = parsed_model.or_else(|| d.model.clone()).or_else(|| d.model_name.clone());
                        timestamp = timestamp.or_else(|| d.timestamp.as_ref().and_then(|v| parse_ts_value(v)));
                    }
                }
                if raw_usage.is_none() {
                    if let Some(ref r) = entry.result {
                        raw_usage = r.usage;
                        parsed_model = parsed_model.or_else(|| r.model.clone()).or_else(|| r.model_name.clone());
                        timestamp = timestamp.or_else(|| r.timestamp.as_ref().and_then(|v| parse_ts_value(v)));
                    }
                }
                if raw_usage.is_none() {
                    if let Some(ref rp) = entry.response {
                        raw_usage = rp.usage;
                        parsed_model = parsed_model.or_else(|| rp.model.clone()).or_else(|| rp.model_name.clone());
                        timestamp = timestamp.or_else(|| rp.timestamp.as_ref().and_then(|v| parse_ts_value(v)));
                    }
                }

                let Some(raw) = raw_usage else { continue; };
                if raw.input_tokens == 0 && raw.cached_input_tokens == 0 && raw.output_tokens == 0 && raw.reasoning_output_tokens == 0 {
                    continue;
                }

                if let Some(ref m) = parsed_model {
                    current_model = Some(m.clone());
                }

                let event_ts = timestamp.unwrap_or(fallback_timestamp);
                if let Some(since_dt) = since {
                    if event_ts < since_dt {
                        continue;
                    }
                }

                events.push(CodexTokenUsageEvent {
                    session_id: session_id.clone(),
                    timestamp: event_ts,
                    model: parsed_model.or_else(|| current_model.clone()),
                    input_tokens: raw.input_tokens,
                    cached_input_tokens: raw.cached_input_tokens.min(raw.input_tokens),
                    output_tokens: raw.output_tokens,
                    reasoning_output_tokens: raw.reasoning_output_tokens,
                });
            }
        }
    }

    Ok(())
}

// ==================== DatasourceProvider 接口实现 ====================

#[async_trait]
impl DatasourceProvider for CodexCollector {
    fn name(&self) -> SourceName {
        SourceName::Codex
    }

    fn description(&self) -> &str {
        "Codex — 纯本地日志主动差分扫描"
    }

    async fn collect(&self) -> Result<Vec<Datalog>, CollectionError> {
        let paths = codex_usage_paths()?;
        let mut files = Vec::new();
        for path in &paths {
            collect_usage_files(path, &mut files);
        }
        files.sort_by_cached_key(|path| path.to_string_lossy().into_owned());

        let mut raw_events = Vec::new();
        // 单线程顺序遍历文件以保证状态差分 `previous_totals` 不错乱
        for file in &files {
            // 获取父目录的父目录，以计算正确的 session_id 相对前缀
            let parent_dir = file.parent().unwrap_or(file);
            let _ = visit_codex_session_file(parent_dir, file, &mut raw_events, None);
        }

        // 转换并转换为最终的 Datalog
        let mut datalogs = Vec::new();
        for ev in raw_events {
            let model = ev.model.unwrap_or_else(|| "gpt-5".to_string());
            datalogs.push(Datalog {
                source_name: SourceName::Codex,
                collected_at: Utc::now(),
                source_api_key: None,
                source_project: ev.session_id,
                source_model: model,
                source_datetime: ev.timestamp,
                source_through_time: Duration::from_secs(0),
                source_parent_project: None,
                source_report_class: ReportClass::Official,
                token_info: TokenInfo {
                    input: ev.input_tokens,
                    output: ev.output_tokens,
                    cache: ev.cached_input_tokens,
                    resourcing: 0,
                    reasoning: ev.reasoning_output_tokens,
                },
            });
        }

        info!(count = datalogs.len(), "原生 Codex 日志数据采集成功");
        Ok(datalogs)
    }

    async fn collect_since(&self, since: DateTime<Utc>) -> Result<Vec<Datalog>, CollectionError> {
        let paths = codex_usage_paths()?;
        let mut files = Vec::new();
        for path in &paths {
            collect_usage_files(path, &mut files);
        }
        files.sort_by_cached_key(|path| path.to_string_lossy().into_owned());

        let mut raw_events = Vec::new();
        for file in &files {
            if let Ok(metadata) = file.metadata() {
                if let Ok(modified) = metadata.modified() {
                    if DateTime::<Utc>::from(modified) < since {
                        continue;
                    }
                }
            }
            let parent_dir = file.parent().unwrap_or(file);
            let _ = visit_codex_session_file(parent_dir, file, &mut raw_events, Some(since));
        }

        let mut datalogs = Vec::new();
        for ev in raw_events {
            if ev.timestamp < since {
                continue;
            }
            let model = ev.model.unwrap_or_else(|| "gpt-5".to_string());
            datalogs.push(Datalog {
                source_name: SourceName::Codex,
                collected_at: Utc::now(),
                source_api_key: None,
                source_project: ev.session_id,
                source_model: model,
                source_datetime: ev.timestamp,
                source_through_time: Duration::from_secs(0),
                source_parent_project: None,
                source_report_class: ReportClass::Official,
                token_info: TokenInfo {
                    input: ev.input_tokens,
                    output: ev.output_tokens,
                    cache: ev.cached_input_tokens,
                    resourcing: 0,
                    reasoning: ev.reasoning_output_tokens,
                },
            });
        }

        info!(count = datalogs.len(), "增量 Codex 日志数据采集成功");
        Ok(datalogs)
    }

    async fn health_check(&self) -> Result<bool, CollectionError> {
        if let Ok(paths) = codex_usage_paths() {
            Ok(paths.iter().any(|p| p.exists()))
        } else {
            Ok(false)
        }
    }
}
