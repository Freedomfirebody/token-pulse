use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use tokio::sync::Mutex;
use serde::{Serialize, Deserialize};
use sysinfo::System;

use crate::config::MonitorConfig;
use crate::types::SessionScanCandidate;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcArtifactManifest {
    pub schema_version: u32,
    pub session_id: String,
    pub server_last_modified_ms: Option<u64>,
    pub step_count: Option<u32>,
    pub artifact_hash: String,
    pub exported_at: u64,
    pub failure_count: u32,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrajectorySummary {
    pub session_id: String,
    pub last_modified_ms: Option<u64>,
    pub step_count: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct RpcConnectionInfo {
    pub pid: u32,
    pub port: u16,
    pub csrf_token: String,
}

#[derive(Debug, Clone)]
pub struct ProcessCandidate {
    pub pid: u32,
    pub ppid: u32,
    pub extension_port: u16,
    pub csrf_token: String,
    pub extension_server_csrf_token: Option<String>,
}

// ==========================================
// 1. ProcessLocator
// ==========================================
pub struct ProcessLocator {
    _debug: bool,
}

impl ProcessLocator {
    pub fn new(debug: bool) -> Self {
        Self { _debug: debug }
    }

    pub fn detect_processes(&self) -> Vec<ProcessCandidate> {
        let mut sys = System::new_all();
        sys.refresh_all();

        let mut candidates = Vec::new();

        for (pid, process) in sys.processes() {
            let pid_u32 = pid.as_u32();
            let name_str = process.name().to_string_lossy().into_owned();
            let args_vec: Vec<String> = process.cmd().iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();

            if is_antigravity_process(&args_vec, &name_str) {
                let mut csrf_token = None;
                let mut extension_server_csrf_token = None;
                let mut port = None;
                let mut next_is_token = false;
                let mut next_is_ext_token = false;
                let mut next_is_port = false;

                for arg in &args_vec {
                    if next_is_token {
                        csrf_token = Some(arg.clone());
                        next_is_token = false;
                    } else if next_is_ext_token {
                        extension_server_csrf_token = Some(arg.clone());
                        next_is_ext_token = false;
                    } else if next_is_port {
                        if let Ok(p) = arg.parse::<u16>() {
                            port = Some(p);
                        }
                        next_is_port = false;
                    } else if arg.starts_with("--csrf_token=") {
                        csrf_token = arg.split_once('=').map(|(_, v)| v.to_string());
                    } else if arg == "--csrf_token" {
                        next_is_token = true;
                    } else if arg.starts_with("--extension_server_csrf_token=") {
                        extension_server_csrf_token = arg.split_once('=').map(|(_, v)| v.to_string());
                    } else if arg == "--extension_server_csrf_token" {
                        next_is_ext_token = true;
                    } else if arg.starts_with("--extension_server_port=") {
                        port = arg.split_once('=').and_then(|(_, v)| v.parse::<u16>().ok());
                    } else if arg == "--extension_server_port" {
                        next_is_port = true;
                    }
                }

                if let Some(token) = csrf_token {
                    let ppid = process.parent().map(|p| p.as_u32()).unwrap_or(0);
                    candidates.push(ProcessCandidate {
                        pid: pid_u32,
                        ppid,
                        extension_port: port.unwrap_or(0),
                        csrf_token: token,
                        extension_server_csrf_token,
                    });
                }
            }
        }

        // Deduplicate and sort desc by PID
        let mut by_pid = HashMap::new();
        for c in candidates {
            by_pid.insert(c.pid, c);
        }
        let mut result: Vec<ProcessCandidate> = by_pid.into_values().collect();
        result.sort_by(|a, b| b.pid.cmp(&a.pid));
        result
    }
}

fn is_antigravity_process(args: &[String], name: &str) -> bool {
    let name_lower = name.to_lowercase();
    if name_lower.contains("language_server") || name_lower.contains("antigravity") {
        return true;
    }
    
    let mut next_is_app_data = false;
    for arg in args {
        let arg_lower = arg.to_lowercase();
        if next_is_app_data {
            if arg_lower == "antigravity" {
                return true;
            }
            next_is_app_data = false;
        } else if arg_lower == "--app_data_dir" {
            next_is_app_data = true;
        } else if arg_lower.starts_with("--app_data_dir=") {
            if arg_lower.split_once('=').map(|(_, v)| v == "antigravity").unwrap_or(false) {
                return true;
            }
        } else if arg_lower.contains("/antigravity/") || arg_lower.contains("\\antigravity\\") {
            return true;
        }
    }
    false
}

// ==========================================
// 2. PortLocator
// ==========================================
pub struct PortLocator;

impl PortLocator {
    pub fn get_listening_ports(pid: u32) -> Vec<u16> {
        #[cfg(target_os = "windows")]
        {
            let mut ports = std::collections::BTreeSet::new();
            if let Ok(output) = Command::new("netstat").arg("-ano").output() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                for line in stdout.lines() {
                    let tokens: Vec<&str> = line.split_whitespace().collect();
                    if tokens.len() >= 5 {
                        // Ensure it is a TCP connection and currently in LISTENING state
                        if tokens[0].eq_ignore_ascii_case("TCP") 
                            && tokens.iter().any(|&t| t.eq_ignore_ascii_case("LISTENING")) 
                        {
                            if let Some(last_token) = tokens.last() {
                                if last_token.parse::<u32>().ok() == Some(pid) {
                                    let local_addr = tokens[1];
                                    if let Some(port) = parse_port_from_address(local_addr) {
                                        ports.insert(port);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            ports.into_iter().collect()
        }

        #[cfg(not(target_os = "windows"))]
        {
            let commands = vec![
                format!("lsof -Pan -p {} -iTCP -sTCP:LISTEN", pid),
                format!("lsof -Pan -p {} -i", pid),
                format!("ss -tlnp 2>/dev/null | grep \"pid={},\"", pid),
                format!("netstat -tulpn 2>/dev/null | grep {}", pid),
            ];

            let mut ports = std::collections::BTreeSet::new();
            for cmd_str in commands {
                if let Ok(output) = Command::new("sh").arg("-c").arg(&cmd_str).output() {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    for port in parse_ports_from_output(&stdout) {
                        ports.insert(port);
                    }
                    if !ports.is_empty() {
                        break;
                    }
                }
            }
            ports.into_iter().collect()
        }
    }
}

fn parse_port_from_address(addr: &str) -> Option<u16> {
    if addr.contains(']') {
        addr.split("]:").nth(1)?.parse::<u16>().ok()
    } else {
        addr.split(':').last()?.parse::<u16>().ok()
    }
}

#[cfg(not(target_os = "windows"))]
fn parse_ports_from_output(stdout: &str) -> Vec<u16> {
    let mut ports = Vec::new();
    for line in stdout.lines() {
        let line_lower = line.to_lowercase();
        let is_listen = line_lower.contains("listen");
        
        for (idx, _) in line_lower.match_indices(':') {
            let rest = &line_lower[idx + 1..];
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if !digits.is_empty() {
                if let Ok(port) = digits.parse::<u16>() {
                    if port > 0 {
                        let prefix = &line_lower[..idx];
                        if prefix.ends_with("127.0.0.1") 
                            || prefix.ends_with("localhost")
                            || prefix.ends_with("*")
                            || prefix.ends_with("::1")
                            || prefix.ends_with("::")
                        {
                            if is_listen || line_lower.contains("lsof") || line_lower.contains("ss") {
                                ports.push(port);
                            }
                        }
                    }
                }
            }
        }
    }
    ports
}

// ==========================================
// 3. AntigravityRpcClient
// ==========================================
pub struct AntigravityRpcClient {
    client: reqwest::Client,
    locator: ProcessLocator,
    timeout_ms: u64,
    connections: Arc<Mutex<Option<Vec<RpcConnectionInfo>>>>,
    session_connections: Arc<Mutex<HashMap<String, RpcConnectionInfo>>>,
}

impl AntigravityRpcClient {
    pub fn new(timeout_ms: u64, debug: bool) -> Self {
        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
            
        Self {
            client,
            locator: ProcessLocator::new(debug),
            timeout_ms,
            connections: Arc::new(Mutex::new(None)),
            session_connections: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn reset_connection(&self) {
        let mut conns = self.connections.lock().await;
        *conns = None;
        let mut sess_conns = self.session_connections.lock().await;
        sess_conns.clear();
    }

    pub async fn detect_connections(&self) -> Vec<RpcConnectionInfo> {
        let process_candidates = self.locator.detect_processes();
        let mut resolved = Vec::new();
        
        for candidate in process_candidates {
            let ports = PortLocator::get_listening_ports(candidate.pid);
            for port in ports {
                // Try extension_server_csrf_token first if present
                if let Some(ref ext_token) = candidate.extension_server_csrf_token {
                    if test_port(&self.client, port, ext_token, self.timeout_ms).await {
                        resolved.push(RpcConnectionInfo {
                            pid: candidate.pid,
                            port,
                            csrf_token: ext_token.clone(),
                        });
                        break;
                    }
                }
                
                // Fallback to standard csrf_token
                if test_port(&self.client, port, &candidate.csrf_token, self.timeout_ms).await {
                    resolved.push(RpcConnectionInfo {
                        pid: candidate.pid,
                        port,
                        csrf_token: candidate.csrf_token.clone(),
                    });
                    break;
                }
            }
        }
        resolved
    }

    pub async fn ensure_connections(&self) -> Vec<RpcConnectionInfo> {
        let mut conns = self.connections.lock().await;
        if conns.is_none() {
            let detected = self.detect_connections().await;
            *conns = Some(detected);
        }
        conns.clone().unwrap_or_default()
    }

    async fn get_connections_for_session(&self, session_id: &str) -> Vec<RpcConnectionInfo> {
        let connections = self.ensure_connections().await;
        let preferred = {
            let sess_conns = self.session_connections.lock().await;
            sess_conns.get(session_id).cloned()
        };
        
        match preferred {
            None => connections,
            Some(pref) => {
                let mut result = vec![pref.clone()];
                for conn in connections {
                    if conn.pid != pref.pid || conn.port != pref.port {
                        result.push(conn);
                    }
                }
                result
            }
        }
    }

    async fn request(&self, method: &str, body: serde_json::Value, connection: &RpcConnectionInfo) -> Result<serde_json::Value, String> {
        let url = format!(
            "https://127.0.0.1:{}/exa.language_server_pb.LanguageServerService/{}",
            connection.port, method
        );
        
        let res = self.client.post(&url)
            .header("Content-Type", "application/json")
            .header("Connect-Protocol-Version", "1")
            .header("X-Codeium-Csrf-Token", &connection.csrf_token)
            .json(&body)
            .timeout(std::time::Duration::from_millis(self.timeout_ms))
            .send()
            .await
            .map_err(|e| format!("Request failed: {}", e))?;
            
        if res.status() != reqwest::StatusCode::OK {
            let status = res.status();
            let text = res.text().await.unwrap_or_default();
            return Err(format!("RPC {} failed with status {}: {}", method, status, text));
        }
        
        let val = res.json::<serde_json::Value>()
            .await
            .map_err(|e| format!("Failed to parse JSON: {}", e))?;
            
        Ok(val)
    }

    pub async fn list_trajectories(&self) -> Result<Vec<TrajectorySummary>, String> {
        let connections = self.ensure_connections().await;
        let mut merged: HashMap<String, (TrajectorySummary, RpcConnectionInfo)> = HashMap::new();
        
        for connection in &connections {
            match self.request("GetAllCascadeTrajectories", serde_json::json!({}), connection).await {
                Ok(response) => {
                    let raw_summaries = response.get("trajectorySummaries")
                        .or_else(|| response.get("cascadeTrajectories"));
                        
                    let mut items = Vec::new();
                    if let Some(raw) = raw_summaries {
                        if let Some(arr) = raw.as_array() {
                            items.extend(arr.clone());
                        } else if let Some(obj) = raw.as_object() {
                            for (key, val) in obj {
                                let mut item = val.clone();
                                if let Some(item_obj) = item.as_object_mut() {
                                    item_obj.insert("cascadeId".to_string(), serde_json::Value::String(key.clone()));
                                }
                                items.push(item);
                            }
                        }
                    }
                    
                    let summaries: Vec<TrajectorySummary> = items.iter()
                        .filter_map(|item| normalize_summary(item))
                        .collect();
                        
                    for summary in summaries {
                        let session_id = summary.session_id.clone();
                        let existing = merged.get(&session_id);
                        let should_insert = match existing {
                            None => true,
                            Some((existing_summary, _)) => is_better_summary(&summary, existing_summary),
                        };
                        if should_insert {
                            merged.insert(session_id, (summary, connection.clone()));
                        }
                    }
                }
                Err(e) => {
                    println!("[RPC] list_trajectories error on pid={}: {}", connection.pid, e);
                }
            }
        }
        
        let mut session_conns = self.session_connections.lock().await;
        session_conns.clear();
        for (session_id, (_, conn)) in &merged {
            session_conns.insert(session_id.clone(), conn.clone());
        }
        
        let summaries: Vec<TrajectorySummary> = merged.into_values().map(|(s, _)| s).collect();
        Ok(summaries)
    }

    pub async fn get_trajectory_steps(&self, session_id: &str) -> Vec<serde_json::Value> {
        let connections = self.get_connections_for_session(session_id).await;
        for connection in connections {
            match self.request("GetCascadeTrajectory", serde_json::json!({ "cascadeId": session_id }), &connection).await {
                Ok(result) => {
                    if let Some(steps) = result.get("trajectory").and_then(|t| t.get("steps")).and_then(|s| s.as_array()) {
                        return steps.clone();
                    }
                }
                Err(e) => {
                    println!("[RPC] GetCascadeTrajectory failed for {} on pid={}: {}", session_id, connection.pid, e);
                }
            }
            
            match self.request("GetCascadeTrajectorySteps", serde_json::json!({
                "cascadeId": session_id,
                "startIndex": 0,
                "endIndex": 10000
            }), &connection).await {
                Ok(fallback) => {
                    if let Some(steps) = fallback.get("steps").or_else(|| fallback.get("step")).and_then(|s| s.as_array()) {
                        return steps.clone();
                    }
                }
                Err(e) => {
                    println!("[RPC] GetCascadeTrajectorySteps failed for {} on pid={}: {}", session_id, connection.pid, e);
                }
            }
        }
        Vec::new()
    }

    pub async fn get_trajectory_metadata(&self, session_id: &str) -> Vec<serde_json::Value> {
        let connections = self.get_connections_for_session(session_id).await;
        for connection in connections {
            match self.request("GetCascadeTrajectoryGeneratorMetadata", serde_json::json!({ "cascadeId": session_id }), &connection).await {
                Ok(result) => {
                    if let Some(meta) = result.get("generatorMetadata").and_then(|m| m.as_array()) {
                        return meta.clone();
                    }
                }
                Err(e) => {
                    println!("[RPC] GetCascadeTrajectoryGeneratorMetadata failed for {} on pid={}: {}", session_id, connection.pid, e);
                }
            }
        }
        Vec::new()
    }

    pub async fn flush(&self) {
        let connections = self.ensure_connections().await;
        for connection in connections {
            let _ = self.request("SendAllQueuedMessages", serde_json::json!({}), &connection).await;
        }
    }
}

async fn test_port(client: &reqwest::Client, port: u16, csrf_token: &str, timeout_ms: u64) -> bool {
    let url = format!("https://127.0.0.1:{}/exa.language_server_pb.LanguageServerService/Heartbeat", port);
    let body = serde_json::json!({ "uuid": "00000000-0000-0000-0000-000000000000" });
    
    let res = client.post(&url)
        .header("Content-Type", "application/json")
        .header("Connect-Protocol-Version", "1")
        .header("X-Codeium-Csrf-Token", csrf_token)
        .json(&body)
        .timeout(std::time::Duration::from_millis(timeout_ms))
        .send()
        .await;
        
    match res {
        Ok(response) => response.status() == reqwest::StatusCode::OK,
        Err(_) => false,
    }
}

fn normalize_summary(val: &serde_json::Value) -> Option<TrajectorySummary> {
    let obj = val.as_object()?;
    
    let session_id = obj.get("cascadeId")
        .or_else(|| obj.get("trajectoryId"))
        .or_else(|| obj.get("id"))
        .or_else(|| obj.get("sessionId"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())?;
        
    if session_id.trim().is_empty() {
        return None;
    }
    
    let last_modified_ms = obj.get("lastModifiedTime")
        .or_else(|| obj.get("lastModified"))
        .or_else(|| obj.get("updatedAt"))
        .or_else(|| obj.get("modifiedAt"))
        .and_then(|v| {
            if let Some(n) = v.as_u64() {
                Some(n)
            } else if let Some(s) = v.as_str() {
                chrono::DateTime::parse_from_rfc3339(s).ok()
                    .map(|dt| dt.timestamp_millis() as u64)
                    .or_else(|| s.parse::<u64>().ok())
            } else {
                None
            }
        });
        
    let step_count = obj.get("stepCount")
        .or_else(|| obj.get("numSteps"))
        .or_else(|| obj.get("totalSteps"))
        .and_then(|v| v.as_u64().map(|n| n as u32));
        
    Some(TrajectorySummary {
        session_id,
        last_modified_ms,
        step_count,
    })
}

fn is_better_summary(next: &TrajectorySummary, current: &TrajectorySummary) -> bool {
    let next_mod = next.last_modified_ms.unwrap_or(0);
    let curr_mod = current.last_modified_ms.unwrap_or(0);
    if next_mod != curr_mod {
        return next_mod > curr_mod;
    }
    next.step_count.unwrap_or(0) > current.step_count.unwrap_or(0)
}

// ==========================================
// 4. TrajectoryExporter
// ==========================================
pub struct TrajectoryExporter {
    config: MonitorConfig,
    client: AntigravityRpcClient,
    cache_root: PathBuf,
}

impl TrajectoryExporter {
    pub fn new(config: MonitorConfig) -> Self {
        let timeout_ms = config.rpc_timeout_ms;
        let debug = config.debug;
        let cache_root = PathBuf::from(&config.session_root)
            .join(".token-monitor")
            .join("rpc-cache")
            .join("v1");
            
        Self {
            client: AntigravityRpcClient::new(timeout_ms, debug),
            config,
            cache_root,
        }
    }

    fn get_session_dir(&self, session_id: &str) -> PathBuf {
        self.cache_root.join(session_id)
    }

    fn get_manifest_path(&self, session_id: &str) -> PathBuf {
        self.get_session_dir(session_id).join("manifest.json")
    }

    fn get_steps_path(&self, session_id: &str) -> PathBuf {
        self.get_session_dir(session_id).join("steps.jsonl")
    }

    fn get_usage_path(&self, session_id: &str) -> PathBuf {
        self.get_session_dir(session_id).join("usage.jsonl")
    }

    pub fn load_manifest(&self, session_id: &str) -> Result<RpcArtifactManifest, std::io::Error> {
        let path = self.get_manifest_path(session_id);
        let file = File::open(path)?;
        let manifest = serde_json::from_reader(file)?;
        Ok(manifest)
    }

    pub async fn export_changed_sessions(
        &self,
        candidates: &[SessionScanCandidate],
        force: bool,
        selective_force: bool,
    ) -> Result<u32, String> {
        if !self.config.use_rpc_export {
            return Ok(0);
        }
        
        let mut exported_count = 0;
        
        let summaries = match self.fetch_summaries_with_retry().await {
            Ok(s) => s,
            Err(e) => {
                println!("[RPC Exporter] Summary fetch failed: {}", e);
                return Err(e);
            }
        };
        
        let summary_by_id: HashMap<String, TrajectorySummary> = summaries.into_iter()
            .map(|s| (s.session_id.clone(), s))
            .collect();
            
        for candidate in candidates {
            let summary = summary_by_id.get(&candidate.session_id).cloned();
            let in_active_rpc = summary.is_some();
            
            if !in_active_rpc {
                continue;
            }
            
            let summary = summary.unwrap();
            let previous = self.load_manifest(&candidate.session_id).ok();
            
            let should_force_this_session = force || (
                selective_force && (
                    previous.is_none()
                    || previous.as_ref().map(|p| p.failure_count).unwrap_or(0) > 0
                )
            );
            
            let export_needed = should_export_session(&summary, previous.as_ref(), should_force_this_session);
            if !export_needed {
                continue;
            }
            
            match self.fetch_session_payload_with_retry(&candidate.session_id).await {
                Ok((steps, metadata)) => {
                    let serialized_steps = if self.config.export_steps_jsonl {
                        serialize_steps(&candidate.session_id, &steps)
                    } else {
                        serialize_steps_redacted(&candidate.session_id, &steps)
                    };
                    
                    let serialized_usage = serialize_usage(&candidate.session_id, &metadata);
                    
                    match self.write_session_artifacts(
                        &candidate.session_id,
                        summary.last_modified_ms,
                        summary.step_count,
                        serialized_steps,
                        serialized_usage,
                    ).await {
                        Ok(_) => {
                            exported_count += 1;
                        }
                        Err(e) => {
                            let _ = self.record_failure(&candidate.session_id, &e.to_string()).await;
                        }
                    }
                }
                Err(e) => {
                    let _ = self.record_failure(&candidate.session_id, &e).await;
                }
            }
        }
        
        Ok(exported_count)
    }

    async fn fetch_summaries_with_retry(&self) -> Result<Vec<TrajectorySummary>, String> {
        let max_attempts = 2;
        let mut last_err = String::new();
        
        for attempt in 1..=max_attempts {
            self.client.flush().await;
            match self.client.list_trajectories().await {
                Ok(summaries) => return Ok(summaries),
                Err(e) => {
                    last_err = e;
                    if attempt < max_attempts {
                        self.client.reset_connection().await;
                    }
                }
            }
        }
        Err(last_err)
    }

    async fn fetch_session_payload_with_retry(&self, session_id: &str) -> Result<(Vec<serde_json::Value>, Vec<serde_json::Value>), String> {
        let max_attempts = 2;
        let mut last_err = String::new();
        
        for attempt in 1..=max_attempts {
            let steps = self.client.get_trajectory_steps(session_id).await;
            let metadata = self.client.get_trajectory_metadata(session_id).await;
            
            if !steps.is_empty() || !metadata.is_empty() {
                return Ok((steps, metadata));
            } else {
                last_err = "Empty payload".to_string();
                if attempt < max_attempts {
                    self.client.reset_connection().await;
                }
            }
        }
        Err(last_err)
    }

    async fn write_session_artifacts(
        &self,
        session_id: &str,
        server_last_modified_ms: Option<u64>,
        step_count: Option<u32>,
        steps: Option<Vec<serde_json::Value>>,
        usage: Vec<serde_json::Value>,
    ) -> Result<RpcArtifactManifest, std::io::Error> {
        let session_dir = self.get_session_dir(session_id);
        std::fs::create_dir_all(&session_dir)?;
        
        let steps_content = match &steps {
            Some(s) => to_jsonl(s),
            None => String::new(),
        };
        let usage_content = to_jsonl(&usage);
        
        use sha1::{Sha1, Digest};
        let mut hasher = Sha1::new();
        hasher.update(session_id.as_bytes());
        hasher.update(b"\0");
        hasher.update(steps_content.as_bytes());
        hasher.update(b"\0");
        hasher.update(usage_content.as_bytes());
        let artifact_hash = hasher.finalize().iter().map(|b| format!("{:02x}", b)).collect::<String>();
        
        if steps.is_some() {
            write_atomic(&self.get_steps_path(session_id), &steps_content)?;
        } else {
            let _ = std::fs::remove_file(self.get_steps_path(session_id));
        }
        write_atomic(&self.get_usage_path(session_id), &usage_content)?;
        
        let manifest = RpcArtifactManifest {
            schema_version: 1,
            session_id: session_id.to_string(),
            server_last_modified_ms,
            step_count,
            artifact_hash,
            exported_at: chrono::Utc::now().timestamp_millis() as u64,
            failure_count: 0,
            last_error: None,
        };
        
        let manifest_content = serde_json::to_string_pretty(&manifest)?;
        write_atomic(&self.get_manifest_path(session_id), &format!("{}\n", manifest_content))?;
        
        Ok(manifest)
    }

    async fn record_failure(&self, session_id: &str, error: &str) -> Result<(), std::io::Error> {
        let previous = self.load_manifest(session_id).ok();
        let next = RpcArtifactManifest {
            schema_version: 1,
            session_id: session_id.to_string(),
            server_last_modified_ms: previous.as_ref().and_then(|p| p.server_last_modified_ms),
            step_count: previous.as_ref().and_then(|p| p.step_count),
            artifact_hash: previous.as_ref().map(|p| p.artifact_hash.clone()).unwrap_or_default(),
            exported_at: previous.as_ref().map(|p| p.exported_at).unwrap_or(0),
            failure_count: previous.as_ref().map(|p| p.failure_count).unwrap_or(0) + 1,
            last_error: Some(error.to_string()),
        };
        
        let session_dir = self.get_session_dir(session_id);
        std::fs::create_dir_all(&session_dir)?;
        
        let manifest_content = serde_json::to_string_pretty(&next)?;
        write_atomic(&self.get_manifest_path(session_id), &format!("{}\n", manifest_content))?;
        
        Ok(())
    }
}

fn should_export_session(
    summary: &TrajectorySummary,
    previous: Option<&RpcArtifactManifest>,
    force: bool,
) -> bool {
    if force {
        return true;
    }
    let prev = match previous {
        None => return true,
        Some(p) => p,
    };
    if prev.artifact_hash.is_empty() {
        return true;
    }
    prev.server_last_modified_ms != summary.last_modified_ms || prev.step_count != summary.step_count
}

fn to_jsonl(records: &[serde_json::Value]) -> String {
    if records.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for rec in records {
        if let Ok(line) = serde_json::to_string(rec) {
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

fn write_atomic(path: &Path, content: &str) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "Parent directory not found"))?;
    std::fs::create_dir_all(parent)?;
    let temp_name = format!("{}.tmp-{}-{}", path.file_name().unwrap().to_string_lossy(), std::process::id(), chrono::Utc::now().timestamp_millis());
    let temp_path = parent.join(temp_name);
    std::fs::write(&temp_path, content)?;
    std::fs::rename(&temp_path, path)?;
    Ok(())
}

// ==========================================
// 5. Internal Serialization Helpers
// ==========================================
#[derive(Debug, Clone, Default)]
struct ExtractedUsage {
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    reasoning_tokens: u64,
    total_tokens: u64,
}

fn extract_usage(value: &serde_json::Value) -> ExtractedUsage {
    let mut usage = ExtractedUsage::default();
    
    fn visit(val: &serde_json::Value, usage: &mut ExtractedUsage) {
        if let Some(arr) = val.as_array() {
            for item in arr {
                visit(item, usage);
            }
        } else if let Some(obj) = val.as_object() {
            for (key, child) in obj {
                let normalized_key = key.to_lowercase();
                if let Some(num) = to_finite_number(child) {
                    if normalized_key.starts_with("input") && normalized_key.contains("token")
                        || normalized_key.starts_with("prompt") && normalized_key.contains("token")
                    {
                        usage.input_tokens += num;
                    } else if normalized_key.starts_with("output") && normalized_key.contains("token")
                        || normalized_key.starts_with("completion") && normalized_key.contains("token")
                    {
                        usage.output_tokens += num;
                    } else if normalized_key.contains("cache") && normalized_key.contains("read") && normalized_key.contains("token") {
                        usage.cache_read_tokens += num;
                    } else if normalized_key.contains("cache") && normalized_key.contains("write") && normalized_key.contains("token") {
                        usage.cache_write_tokens += num;
                    } else if (normalized_key.contains("reasoning") || normalized_key.contains("thinking")) && normalized_key.contains("token") {
                        usage.reasoning_tokens += num;
                    } else if normalized_key == "totaltokens" || normalized_key == "total_tokens" {
                        usage.total_tokens += num;
                    }
                }
                visit(child, usage);
            }
        }
    }
    
    visit(value, &mut usage);
    
    let sum = usage.input_tokens + usage.output_tokens + usage.cache_read_tokens + usage.cache_write_tokens + usage.reasoning_tokens;
    usage.total_tokens = usage.total_tokens.max(sum);
    usage
}

fn to_finite_number(value: &serde_json::Value) -> Option<u64> {
    if let Some(n) = value.as_u64() {
        Some(n)
    } else if let Some(f) = value.as_f64() {
        Some(f as u64)
    } else if let Some(s) = value.as_str() {
        s.parse::<u64>().ok()
    } else {
        None
    }
}

fn extract_role(value: &serde_json::Value) -> String {
    if let Some(obj) = value.as_object() {
        if let Some(t) = obj.get("type").and_then(|v| v.as_str()) {
            if t == "CORTEX_STEP_TYPE_USER_INPUT" {
                return "user".to_string();
            }
            if t == "CORTEX_STEP_TYPE_MODEL_RESPONSE" || t == "CORTEX_STEP_TYPE_PLANNER_RESPONSE" {
                return "model".to_string();
            }
        }
        if let Some(header) = obj.get("header").and_then(|h| h.as_object()) {
            if let Some(sender) = header.get("sender").and_then(|s| s.as_str()) {
                return if sender == "USER" { "user".to_string() } else { "model".to_string() };
            }
        }
    }
    "unknown".to_string()
}

fn extract_timestamp(value: &serde_json::Value) -> Option<u64> {
    let obj = value.as_object()?;
    for key in &["timestamp", "createdAt", "lastModifiedTime"] {
        if let Some(v) = obj.get(*key) {
            if let Some(n) = v.as_u64() {
                return Some(n);
            } else if let Some(s) = v.as_str() {
                if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(s) {
                    return Some(parsed.timestamp_millis() as u64);
                }
                if let Ok(n) = s.parse::<u64>() {
                    return Some(n);
                }
            }
        }
    }
    None
}

fn extract_model(value: &serde_json::Value) -> Option<String> {
    let obj = value.as_object()?;
    obj.get("model")
        .or_else(|| obj.get("modelId"))
        .or_else(|| obj.get("modelName"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn extract_step_text(value: &serde_json::Value) -> String {
    let obj = match value.as_object() {
        None => return String::new(),
        Some(o) => o,
    };
    
    if let Some(user_input) = obj.get("userInput").and_then(|u| u.as_object()) {
        if let Some(user_resp) = user_input.get("userResponse").and_then(|r| r.as_str()) {
            if !user_resp.trim().is_empty() {
                return user_resp.to_string();
            }
        }
        if let Some(items) = user_input.get("items").and_then(|i| i.as_array()) {
            let mut parts = Vec::new();
            for item in items {
                if let Some(item_obj) = item.as_object() {
                    if let Some(text_val) = item_obj.get("text").and_then(|t| t.as_object()) {
                        if let Some(content) = text_val.get("content").and_then(|c| c.as_str()) {
                            parts.push(content.to_string());
                        }
                    }
                    if let Some(code_val) = item_obj.get("code").and_then(|c| c.as_object()) {
                        if let Some(val_str) = code_val.get("value").and_then(|v| v.as_str()) {
                            parts.push(val_str.to_string());
                        }
                    }
                }
            }
            if !parts.is_empty() {
                return parts.join("\n\n");
            }
        }
    }
    
    if let Some(model_resp) = obj.get("modelResponse").and_then(|m| m.as_object()) {
        if let Some(content) = model_resp.get("content").and_then(|c| c.as_array()) {
            let mut parts = Vec::new();
            for item in content {
                if let Some(part_obj) = item.as_object() {
                    if let Some(text_val) = part_obj.get("text").and_then(|t| t.as_object()) {
                        if let Some(content_str) = text_val.get("content").and_then(|c| c.as_str()) {
                            if !content_str.is_empty() {
                                parts.push(content_str.to_string());
                            }
                        }
                    }
                }
            }
            if !parts.is_empty() {
                return parts.join("\n");
            }
        }
        if let Some(text_str) = model_resp.get("text").and_then(|t| t.as_str()) {
            return text_str.to_string();
        }
    }
    
    if let Some(planner_resp) = obj.get("plannerResponse").and_then(|p| p.as_object()) {
        if let Some(thinking) = planner_resp.get("thinking").and_then(|t| t.as_str()) {
            return thinking.to_string();
        }
    }
    
    if let Some(text) = obj.get("text").and_then(|t| t.as_str()) {
        if !text.trim().is_empty() {
            return text.to_string();
        }
    }
    
    if let Some(message) = obj.get("message").and_then(|m| m.as_object()) {
        if let Some(text) = message.get("text").and_then(|t| t.as_str()) {
            if !text.trim().is_empty() {
                return text.to_string();
            }
        }
    }
    
    String::new()
}

fn extract_usage_model(value: &serde_json::Value) -> Option<String> {
    let obj = value.as_object()?;
    
    let candidates = vec![
        obj.get("responseModel").and_then(|v| v.as_str()),
        obj.get("model").and_then(|v| v.as_str()),
        obj.get("modelName").and_then(|v| v.as_str()),
        obj.get("modelId").and_then(|v| v.as_str()),
    ];
    
    if let Some(direct) = preferred_model_from_opts(&candidates, None) {
        return Some(direct);
    }
    
    if let Some(chat_model) = obj.get("chatModel").and_then(|c| c.as_object()) {
        let chat_candidates = vec![
            chat_model.get("responseModel").and_then(|v| v.as_str()),
            chat_model.get("model").and_then(|v| v.as_str()),
            chat_model.get("modelName").and_then(|v| v.as_str()),
            chat_model.get("modelId").and_then(|v| v.as_str()),
        ];
        if let Some(from_chat) = preferred_model_from_opts(&chat_candidates, None) {
            return Some(from_chat);
        }
        
        if let Some(usage) = chat_model.get("usage").and_then(|u| u.as_object()) {
            let usage_candidates = vec![
                usage.get("responseModel").and_then(|v| v.as_str()),
                usage.get("model").and_then(|v| v.as_str()),
                usage.get("modelName").and_then(|v| v.as_str()),
                usage.get("modelId").and_then(|v| v.as_str()),
            ];
            if let Some(from_usage) = preferred_model_from_opts(&usage_candidates, None) {
                return Some(from_usage);
            }
        }
    }
    
    None
}

fn preferred_model_from_opts(candidates: &[Option<&str>], inherited: Option<&str>) -> Option<String> {
    crate::model_aliases::preferred_model(candidates, inherited)
}

fn serialize_steps(session_id: &str, steps: &[serde_json::Value]) -> Option<Vec<serde_json::Value>> {
    let mut out = Vec::new();
    for (index, step) in steps.iter().enumerate() {
        let text = extract_step_text(step);
        out.push(serde_json::json!({
            "recordType": "step",
            "sessionId": session_id,
            "stepIndex": index,
            "role": extract_role(step),
            "timestamp": extract_timestamp(step),
            "model": extract_model(step),
            "text": text,
        }));
    }
    Some(out)
}

fn serialize_steps_redacted(session_id: &str, steps: &[serde_json::Value]) -> Option<Vec<serde_json::Value>> {
    let mut out = Vec::new();
    for (index, step) in steps.iter().enumerate() {
        let usage = extract_usage(step);
        let mut obj = serde_json::json!({
            "recordType": "step",
            "sessionId": session_id,
            "stepIndex": index,
            "role": extract_role(step),
            "timestamp": extract_timestamp(step),
            "model": extract_model(step),
        });
        
        if usage.total_tokens > 0 {
            if let Some(map) = obj.as_object_mut() {
                map.insert("inputTokens".to_string(), serde_json::Value::Number(usage.input_tokens.into()));
                map.insert("outputTokens".to_string(), serde_json::Value::Number(usage.output_tokens.into()));
                map.insert("cacheReadTokens".to_string(), serde_json::Value::Number(usage.cache_read_tokens.into()));
                map.insert("cacheWriteTokens".to_string(), serde_json::Value::Number(usage.cache_write_tokens.into()));
                map.insert("reasoningTokens".to_string(), serde_json::Value::Number(usage.reasoning_tokens.into()));
                map.insert("totalTokens".to_string(), serde_json::Value::Number(usage.total_tokens.into()));
            }
        }
        
        out.push(obj);
    }
    Some(out)
}

fn serialize_usage(session_id: &str, metadata: &[serde_json::Value]) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    for (index, entry) in metadata.iter().enumerate() {
        let usage = extract_usage(entry);
        out.push(serde_json::json!({
            "recordType": "usage",
            "sessionId": session_id,
            "sequence": index,
            "timestamp": extract_timestamp(entry),
            "model": extract_usage_model(entry),
            "inputTokens": usage.input_tokens,
            "outputTokens": usage.output_tokens,
            "cacheReadTokens": usage.cache_read_tokens,
            "cacheWriteTokens": usage.cache_write_tokens,
            "reasoningTokens": usage.reasoning_tokens,
            "totalTokens": usage.total_tokens,
            "raw": entry,
        }));
    }
    out
}
