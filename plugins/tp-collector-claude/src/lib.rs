//! # tp-collector-claude
//!
//! Claude Code 数据采集组件 — 原生本地日志扫描与智能去重解析实现。

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use tracing::info;

use tp_protocol::{
    CollectionError, Datalog, DatasourceProvider, ReportClass, SourceName, TokenInfo,
};

// ==================== Claude Code 数据反序列化类型 ====================

#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
pub struct TokenUsageRaw {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    pub speed: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
struct DailyUsageMessage {
    usage: TokenUsageRaw,
    model: Option<String>,
    id: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, Clone)]
struct DailyUsageEntry {
    timestamp: String,
    message: DailyUsageMessage,
    version: Option<String>,
    session_id: Option<String>,
    #[serde(rename = "costUSD")]
    cost_usd: Option<f64>,
    request_id: Option<String>,
    is_sidechain: Option<bool>,
}

#[derive(Debug, Deserialize, Clone)]
struct DailyAgentProgressMessage {
    timestamp: String,
    message: DailyUsageMessage,
    #[serde(rename = "costUSD")]
    cost_usd: Option<f64>,
    request_id: Option<String>,
    is_sidechain: Option<bool>,
}

#[derive(Debug, Deserialize, Clone)]
struct DailyAgentProgressData {
    message: DailyAgentProgressMessage,
}

#[derive(Debug, Deserialize, Clone)]
struct DailyAgentProgressEntry {
    data: DailyAgentProgressData,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
enum DailyUsageLine {
    Direct(DailyUsageEntry),
    AgentProgress(DailyAgentProgressEntry),
}

impl DailyUsageLine {
    fn into_entry(self) -> DailyUsageEntry {
        match self {
            DailyUsageLine::Direct(entry) => entry,
            DailyUsageLine::AgentProgress(entry) => DailyUsageEntry {
                timestamp: entry.data.message.timestamp,
                message: entry.data.message.message,
                version: None,
                session_id: None,
                cost_usd: entry.data.message.cost_usd,
                request_id: entry.data.message.request_id,
                is_sidechain: entry.data.message.is_sidechain,
            },
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
struct DailyLoadedEntry {
    timestamp: DateTime<Utc>,
    project: String,
    session_id: String,
    project_path: String,
    usage: TokenUsageRaw,
    model: Option<String>,
    message_id: Option<String>,
    request_id: Option<String>,
    is_sidechain: Option<bool>,
}

// ==================== 采集器主体 ====================

/// Claude Code 数据采集器
pub struct ClaudeCollector {
    session_root: PathBuf,
}

impl ClaudeCollector {
    pub fn new() -> Self {
        let session_root = dirs::home_dir()
            .map(|h| h.join(".claude"))
            .unwrap_or_else(|| PathBuf::from("."));
        Self { session_root }
    }

    pub fn with_session_root(mut self, root: PathBuf) -> Self {
        self.session_root = root;
        self
    }
}

// ==================== 路径探测与扫描 ====================

fn expand_home_path(raw: &str) -> PathBuf {
    if raw == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(raw)
}

fn claude_paths() -> Result<Vec<PathBuf>, CollectionError> {
    let mut paths = Vec::new();
    let mut seen = std::collections::HashSet::new();

    if let Ok(env_paths) = std::env::var("CLAUDE_CONFIG_DIR") {
        for raw in env_paths.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let path = expand_home_path(raw);
            let normalized = if path.file_name().is_some_and(|name| name == "projects") && path.is_dir() {
                path.parent().map(Path::to_path_buf).unwrap_or(path)
            } else {
                path
            };
            if normalized.join("projects").is_dir() && seen.insert(normalized.clone()) {
                paths.push(normalized);
            }
        }
        if !paths.is_empty() {
            return Ok(paths);
        }
    }

    let home = dirs::home_dir().ok_or_else(|| {
        CollectionError::SourceUnavailable("Home directory is not set".to_string())
    })?;
    let xdg = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home.join(".config"));

    for path in [xdg.join("claude"), home.join(".claude")] {
        if path.join("projects").is_dir() && seen.insert(path.clone()) {
            paths.push(path);
        }
    }

    Ok(paths)
}

fn collect_files_with_extension(dir: &Path, extension: &str, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        if let Ok(file_type) = entry.file_type() {
            let path = entry.path();
            if file_type.is_file() && path.extension().is_some_and(|ext| ext == extension) {
                files.push(path);
            } else if file_type.is_dir() {
                collect_files_with_extension(&path, extension, files);
            }
        }
    }
}

// ==================== 提取项目和会话信息 ====================

fn extract_project(path: &Path) -> String {
    let mut saw_projects = false;
    for part in path.components().filter_map(|c| c.as_os_str().to_str()) {
        if saw_projects {
            return if part.trim().is_empty() { "unknown" } else { part }.to_string();
        }
        if part == "projects" {
            saw_projects = true;
        }
    }
    "unknown".to_string()
}

fn extract_session_parts(path: &Path) -> (String, String) {
    let parts = path
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect::<Vec<_>>();
    let projects_index = parts.iter().position(|part| *part == "projects");
    let relative = projects_index
        .map(|index| &parts[index + 1..])
        .unwrap_or(&parts);
    let file_session_id = relative
        .last()
        .and_then(|file_name| file_name.strip_suffix(".jsonl"))
        .filter(|session_id| !session_id.is_empty());
    if relative.len() == 2 {
        if let Some(session_id) = file_session_id {
            return (session_id.to_string(), relative[0].to_string());
        }
    }
    if relative.len() >= 4 && relative.get(relative.len() - 2) == Some(&"subagents") {
        let session_id = relative[relative.len() - 3].to_string();
        let project_path = relative[..relative.len() - 3].join(std::path::MAIN_SEPARATOR_STR);
        return (
            session_id,
            if project_path.is_empty() {
                "Unknown Project".to_string()
            } else {
                project_path
            },
        );
    }
    let session_id = relative
        .get(relative.len().saturating_sub(2))
        .copied()
        .unwrap_or("unknown")
        .to_string();
    let project_path = if relative.len() > 2 {
        relative[..relative.len() - 2].join(std::path::MAIN_SEPARATOR_STR)
    } else {
        "Unknown Project".to_string()
    };
    (session_id, project_path)
}

// ==================== 时间解析与去重辅助 ====================

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

fn parse_ts_timestamp(s: &str) -> Option<DateTime<Utc>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    if let Ok(dt) = DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.3fZ") {
        return Some(dt.with_timezone(&Utc));
    }
    None
}

fn usage_dedupe_hash(message_id: &str, request_id: Option<&str>) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    message_id.hash(&mut hasher);
    request_id.hash(&mut hasher);
    hasher.finish()
}

// ==================== 文件内容读取 ====================

fn read_usage_file(path: &Path, loaded: &mut Vec<DailyLoadedEntry>, since: Option<DateTime<Utc>>) {
    if let (Some(since_dt), Ok(meta)) = (since, fs::metadata(path)) {
        if let Ok(modified) = meta.modified() {
            if let Ok(dur) = modified.duration_since(std::time::SystemTime::UNIX_EPOCH) {
                if (dur.as_millis() as i64) < since_dt.timestamp_millis() {
                    return;
                }
            }
        }
    }

    let project = extract_project(path);
    let (session_id, project_path) = extract_session_parts(path);

    let Ok(file) = fs::File::open(path) else { return; };
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();

    let usage_marker = br#""usage":{"#;

    while let Ok(bytes_read) = reader.read_until(b'\n', &mut line) {
        if bytes_read == 0 { break; }
        
        // 过滤不含 usage: 的无效行
        if find_bytes(&line, usage_marker).is_none() {
            line.clear();
            continue;
        }

        if let Ok(entry_line) = serde_json::from_slice::<DailyUsageLine>(&line) {
            let data = entry_line.into_entry();
            if let Some(timestamp) = parse_ts_timestamp(&data.timestamp) {
                if let Some(since_dt) = since {
                    if timestamp < since_dt {
                        line.clear();
                        continue;
                    }
                }
                loaded.push(DailyLoadedEntry {
                    timestamp,
                    project: project.clone(),
                    session_id: session_id.clone(),
                    project_path: project_path.clone(),
                    usage: data.message.usage,
                    model: data.message.model,
                    message_id: data.message.id,
                    request_id: data.request_id,
                    is_sidechain: data.is_sidechain,
                });
            }
        }
        line.clear();
    }
}

// ==================== 去重融合逻辑 ====================

fn should_replace(candidate: &DailyLoadedEntry, existing: &DailyLoadedEntry) -> bool {
    let cand_side = candidate.is_sidechain == Some(true);
    let exist_side = existing.is_sidechain == Some(true);
    if cand_side != exist_side {
        return exist_side; // 如果一方是 sidechain 一方不是，保留非 sidechain
    }

    let cand_total = candidate.usage.input_tokens + candidate.usage.output_tokens;
    let exist_total = existing.usage.input_tokens + existing.usage.output_tokens;
    if cand_total != exist_total {
        return cand_total > exist_total;
    }
    candidate.usage.speed.is_some() && existing.usage.speed.is_none()
}

// ==================== DatasourceProvider 接口实现 ====================

#[async_trait]
impl DatasourceProvider for ClaudeCollector {
    fn name(&self) -> SourceName {
        SourceName::CloudeCode
    }

    fn description(&self) -> &str {
        "Claude Code — 本地会话日志主动扫描与智能去重"
    }

    async fn collect(&self) -> Result<Vec<Datalog>, CollectionError> {
        let paths = claude_paths()?;
        let mut files = Vec::new();
        for path in &paths {
            let projects_dir = path.join("projects");
            collect_files_with_extension(&projects_dir, "jsonl", &mut files);
        }
        files.sort_by_cached_key(|path| path.to_string_lossy().into_owned());

        let mut loaded_entries = Vec::new();
        for file in &files {
            read_usage_file(file, &mut loaded_entries, None);
        }

        // 基于 Hash 的去重逻辑
        let mut deduped_map: HashMap<u64, DailyLoadedEntry> = HashMap::new();
        for entry in loaded_entries {
            if let Some(ref msg_id) = entry.message_id {
                let request_id = entry.request_id.as_deref();
                let exact_hash = usage_dedupe_hash(msg_id, request_id);

                if let Some(existing) = deduped_map.get(&exact_hash) {
                    if should_replace(&entry, existing) {
                        deduped_map.insert(exact_hash, entry);
                    }
                } else {
                    deduped_map.insert(exact_hash, entry);
                }
            } else {
                // 如果没有 message_id，直接保留以防丢失
                let fallback_hash = usage_dedupe_hash(&entry.timestamp.timestamp_millis().to_string(), entry.request_id.as_deref());
                deduped_map.insert(fallback_hash, entry);
            }
        }

        // 映射为统一的 Datalog
        let mut datalogs = Vec::new();
        for (_, entry) in deduped_map {
            let model = entry.model.unwrap_or_else(|| "claude-sonnet".to_string());
            let cache = entry.usage.cache_creation_input_tokens + entry.usage.cache_read_input_tokens;
            datalogs.push(Datalog {
                source_name: SourceName::CloudeCode,
                collected_at: Utc::now(),
                source_api_key: None,
                source_project: entry.project,
                source_model: model,
                source_datetime: entry.timestamp,
                source_through_time: Duration::from_secs(0),
                source_parent_project: Some(entry.session_id), // 把 session_id 作为父级关联
                source_report_class: ReportClass::Official,
                token_info: TokenInfo {
                    input: entry.usage.input_tokens.saturating_sub(cache),
                    output: entry.usage.output_tokens,
                    cache,
                    resourcing: 0,
                    reasoning: 0,
                },
            });
        }

        info!(count = datalogs.len(), "原生 Claude Code 日志主动扫描及去重成功");
        Ok(datalogs)
    }

    async fn collect_since(&self, since: DateTime<Utc>) -> Result<Vec<Datalog>, CollectionError> {
        let paths = claude_paths()?;
        let mut files = Vec::new();
        for path in &paths {
            let projects_dir = path.join("projects");
            collect_files_with_extension(&projects_dir, "jsonl", &mut files);
        }
        files.sort_by_cached_key(|path| path.to_string_lossy().into_owned());

        let mut loaded_entries = Vec::new();
        for file in &files {
            read_usage_file(file, &mut loaded_entries, Some(since));
        }

        // 基于 Hash 的去重逻辑
        let mut deduped_map: HashMap<u64, DailyLoadedEntry> = HashMap::new();
        for entry in loaded_entries {
            if let Some(ref msg_id) = entry.message_id {
                let request_id = entry.request_id.as_deref();
                let exact_hash = usage_dedupe_hash(msg_id, request_id);

                if let Some(existing) = deduped_map.get(&exact_hash) {
                    if should_replace(&entry, existing) {
                        deduped_map.insert(exact_hash, entry);
                    }
                } else {
                    deduped_map.insert(exact_hash, entry);
                }
            } else {
                let fallback_hash = usage_dedupe_hash(&entry.timestamp.timestamp_millis().to_string(), entry.request_id.as_deref());
                deduped_map.insert(fallback_hash, entry);
            }
        }

        let mut datalogs = Vec::new();
        for (_, entry) in deduped_map {
            let model = entry.model.unwrap_or_else(|| "claude-sonnet".to_string());
            let cache = entry.usage.cache_creation_input_tokens + entry.usage.cache_read_input_tokens;
            datalogs.push(Datalog {
                source_name: SourceName::CloudeCode,
                collected_at: Utc::now(),
                source_api_key: None,
                source_project: entry.project,
                source_model: model,
                source_datetime: entry.timestamp,
                source_through_time: Duration::from_secs(0),
                source_parent_project: Some(entry.session_id),
                source_report_class: ReportClass::Official,
                token_info: TokenInfo {
                    input: entry.usage.input_tokens.saturating_sub(cache),
                    output: entry.usage.output_tokens,
                    cache,
                    resourcing: 0,
                    reasoning: 0,
                },
            });
        }

        info!(count = datalogs.len(), "增量 Claude Code 日志扫描及去重成功");
        Ok(datalogs)
    }

    async fn health_check(&self) -> Result<bool, CollectionError> {
        if let Ok(paths) = claude_paths() {
            Ok(paths.iter().any(|p| p.join("projects").is_dir()))
        } else {
            Ok(false)
        }
    }
}
