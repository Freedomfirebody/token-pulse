use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;
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



#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ProcessCandidate {
    pub pid: u32,
    pub ppid: u32,
    pub extension_port: u16,
    pub csrf_token: String,
    pub extension_server_csrf_token: Option<String>,
    pub executable_path: Option<String>,
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

    pub fn detect_processes(&self, target_app_data_dir: &str) -> Vec<ProcessCandidate> {
        #[cfg(target_os = "windows")]
        {
            let mut wmi_candidates = Vec::new();
            let output = Command::new("powershell")
                .arg("-NoProfile")
                .arg("-Command")
                .arg("Get-CimInstance Win32_Process | Where-Object { $_.Name -like '*language_server*' -or $_.Name -like '*antigravity*' } | Select-Object ProcessId, Name, CommandLine, ExecutablePath | ConvertTo-Json -Compress")
                .output();
            
            if let Ok(out) = output {
                let stdout = String::from_utf8_lossy(&out.stdout);
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&stdout) {
                    let items = if let Some(arr) = val.as_array() {
                        arr.clone()
                    } else if val.is_object() {
                        vec![val]
                    } else {
                        Vec::new()
                    };

                    for item in items {
                        let pid = item.get("ProcessId").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                        let name = item.get("Name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        let cmdline = item.get("CommandLine").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        let exe_path = item.get("ExecutablePath").and_then(|v| v.as_str()).map(|s| s.to_string());
                        
                        let args = split_cmdline(&cmdline);
                        
                        let name_lower = name.to_lowercase();
                        if name_lower.contains("language_server") || name_lower.contains("antigravity") {
                            if self._debug {
                                println!("[DEBUG ProcessLocator WMI] Found matching process name: '{}', PID: {}, Args: {:?}", name, pid, args);
                            }
                        }

                        if is_antigravity_process(&args, &name, target_app_data_dir) {
                            let mut csrf_token = None;
                            let mut extension_server_csrf_token = None;
                            let mut port = None;
                            let mut next_is_token = false;
                            let mut next_is_ext_token = false;
                            let mut next_is_port = false;

                            for arg in &args {
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
                                } else if arg.starts_with("--extension_server_port=") || arg.starts_with("--https_server_port=") {
                                    port = arg.split_once('=').and_then(|(_, v)| v.parse::<u16>().ok());
                                } else if arg == "--extension_server_port" || arg == "--https_server_port" {
                                    next_is_port = true;
                                }
                            }

                            if let Some(token) = csrf_token {
                                wmi_candidates.push(ProcessCandidate {
                                    pid,
                                    ppid: 0,
                                    extension_port: port.unwrap_or(0),
                                    csrf_token: token,
                                    extension_server_csrf_token,
                                    executable_path: exe_path,
                                });
                            }
                        }
                    }
                }
            }
            if !wmi_candidates.is_empty() {
                let mut by_pid = HashMap::new();
                for c in wmi_candidates {
                    by_pid.insert(c.pid, c);
                }
                let mut result: Vec<ProcessCandidate> = by_pid.into_values().collect();
                result.sort_by(|a, b| b.pid.cmp(&a.pid));
                return result;
            }
        }

        let mut sys = System::new();
        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

        let mut candidates = Vec::new();

        for (pid, process) in sys.processes() {
            let pid_u32 = pid.as_u32();
            let name_str = process.name().to_string_lossy().into_owned();
            let args_vec: Vec<String> = process.cmd().iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();

            let name_lower = name_str.to_lowercase();
            if name_lower.contains("language_server") || name_lower.contains("antigravity") {
                println!("[DEBUG ProcessLocator] Found matching process name: '{}', PID: {}, Args: {:?}", name_str, pid_u32, args_vec);
            }

            if is_antigravity_process(&args_vec, &name_str, target_app_data_dir) {
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
                    } else if arg.starts_with("--extension_server_port=") || arg.starts_with("--https_server_port=") {
                        port = arg.split_once('=').and_then(|(_, v)| v.parse::<u16>().ok());
                    } else if arg == "--extension_server_port" || arg == "--https_server_port" {
                        next_is_port = true;
                    }
                }

                if let Some(token) = csrf_token {
                    let ppid = process.parent().map(|p| p.as_u32()).unwrap_or(0);
                    let exe_path = process.exe().and_then(|p| p.to_str()).map(|s| s.to_string());
                    candidates.push(ProcessCandidate {
                        pid: pid_u32,
                        ppid,
                        extension_port: port.unwrap_or(0),
                        csrf_token: token,
                        extension_server_csrf_token,
                        executable_path: exe_path,
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

fn split_cmdline(cmdline: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    for c in cmdline.chars() {
        if c == '"' {
            in_quotes = !in_quotes;
        } else if c == ' ' && !in_quotes {
            if !current.is_empty() {
                args.push(current.clone());
                current.clear();
            }
        } else {
            current.push(c);
        }
    }
    if !current.is_empty() {
        args.push(current);
    }
    args
}

fn is_antigravity_process(args: &[String], name: &str, target_app_data_dir: &str) -> bool {
    let name_lower = name.to_lowercase();
    
    // The language server always runs with --csrf_token and contains either "language_server" or "antigravity"
    // The GUI wrapper "Antigravity.exe" does not have --csrf_token in its command-line.
    let has_csrf = args.iter().any(|arg| arg.contains("--csrf_token"));
    
    if name_lower.contains("language_server") || (name_lower.contains("antigravity") && has_csrf) {
        // We must check if --app_data_dir argument matches the target_app_data_dir
        let mut app_data_dir = None;
        let mut next_is_app_data = false;
        for arg in args {
            let arg_lower = arg.to_lowercase();
            if next_is_app_data {
                app_data_dir = Some(arg_lower.clone());
                next_is_app_data = false;
            } else if arg_lower == "--app_data_dir" {
                next_is_app_data = true;
            } else if arg_lower.starts_with("--app_data_dir=") {
                app_data_dir = arg_lower.split_once('=').map(|(_, v)| v.to_string());
            }
        }
        
        if let Some(dir) = app_data_dir {
            let dir_path = Path::new(&dir);
            let dir_name = dir_path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_lowercase();
            return dir == target_app_data_dir.to_lowercase() || dir_name == target_app_data_dir.to_lowercase();
        }
        
        // If not explicitly set via --app_data_dir, fallback to checking name/args contains target_app_data_dir
        if name_lower.contains(&target_app_data_dir.to_lowercase()) {
            return true;
        }
    }
    
    let mut next_is_app_data = false;
    for arg in args {
        let arg_lower = arg.to_lowercase();
        if next_is_app_data {
            if arg_lower == target_app_data_dir.to_lowercase() {
                return true;
            }
            next_is_app_data = false;
        } else if arg_lower == "--app_data_dir" {
            next_is_app_data = true;
        } else if arg_lower.starts_with("--app_data_dir=") {
            if arg_lower.split_once('=').map(|(_, v)| v == target_app_data_dir.to_lowercase()).unwrap_or(false) {
                return true;
            }
        } else if arg_lower.contains(&format!("/{}/", target_app_data_dir.to_lowercase())) 
            || arg_lower.contains(&format!("\\{}\\", target_app_data_dir.to_lowercase())) 
        {
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
// 4. TrajectoryExporter
// ==========================================
pub struct TrajectoryExporter {
    config: MonitorConfig,
    locator: ProcessLocator,
    cache_root: PathBuf,
}

impl TrajectoryExporter {
    pub fn new(config: MonitorConfig) -> Self {
        let debug = config.debug;
        let target_app_data_dir = PathBuf::from(&config.session_root)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("antigravity")
            .to_string();
            
        let cache_root = dirs::home_dir()
            .map(|h| h.join(".token-pulse").join("token-monitor"))
            .unwrap_or_else(|| PathBuf::from(".token-pulse").join("token-monitor"))
            .join(&target_app_data_dir)
            .join("rpc-cache")
            .join("v1");
            
        Self {
            locator: ProcessLocator::new(debug),
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
        
        let target_app_data_dir = PathBuf::from(&self.config.session_root)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("antigravity")
            .to_string();

        let process_candidates = self.locator.detect_processes(&target_app_data_dir);
        let mut exe_path = None;
        let mut ls_address = None;

        if !process_candidates.is_empty() {
            let candidate = &process_candidates[0];
            if let Some(ref path) = candidate.executable_path {
                exe_path = Some(path.clone());
            }
            let ports = PortLocator::get_listening_ports(candidate.pid);
            if !ports.is_empty() {
                ls_address = Some(format!("127.0.0.1:{}", ports[0]));
            }
        }

        let fallback_exe = dirs::home_dir()
            .map(|h| h.join("AppData").join("Local").join("Programs").join("antigravity").join("resources").join("bin").join("language_server.exe"))
            .filter(|p| p.exists())
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "language_server.exe".to_string());

        let final_exe = exe_path.unwrap_or(fallback_exe);
        let final_address = ls_address.or_else(|| {
            std::env::var("ANTIGRAVITY_LS_ADDRESS").ok()
        });
        
        for candidate in candidates {
            let transcript_path = PathBuf::from(&candidate.session_dir)
                .join(".system_generated")
                .join("logs")
                .join("transcript.jsonl");

            if !transcript_path.exists() {
                continue;
            }

            let mtime_ms = std::fs::metadata(&transcript_path)
                .and_then(|m| m.modified())
                .and_then(|t| t.duration_since(std::time::SystemTime::UNIX_EPOCH).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e)))
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);

            let previous = self.load_manifest(&candidate.session_id).ok();
            
            let should_force_this_session = force || (
                selective_force && (
                    previous.is_none()
                    || previous.as_ref().map(|p| p.failure_count).unwrap_or(0) > 0
                )
            );
            
            let mut export_needed = should_force_this_session;
            if let Some(ref prev) = previous {
                if prev.server_last_modified_ms != Some(mtime_ms) {
                    export_needed = true;
                }
            } else {
                export_needed = true;
            }

            if !export_needed {
                continue;
            }
            
            let mut cmd = Command::new(&final_exe);
            cmd.arg("agentapi")
               .arg("get-conversation-metadata")
               .arg(&candidate.session_id);

            if let Some(ref addr) = final_address {
                cmd.env("ANTIGRAVITY_LS_ADDRESS", addr);
            }

            let output = cmd.output();
            let mut _created_at_str = None;
            let mut _project_id = None;

            match output {
                Ok(out) => {
                    if out.status.success() {
                        let stdout = String::from_utf8_lossy(&out.stdout);
                        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&stdout) {
                            if let Some(m) = val.get("response")
                                .and_then(|r| r.get("conversationMetadata"))
                                .and_then(|c| c.get("metadata"))
                            {
                                _created_at_str = m.get("createdAt").and_then(|v| v.as_str()).map(|s| s.to_string());
                                _project_id = m.get("projectId").and_then(|v| v.as_str()).map(|s| s.to_string());
                            }
                        }
                    } else {
                        let stdout = String::from_utf8_lossy(&out.stdout);
                        let stderr = String::from_utf8_lossy(&out.stderr);
                        tracing::debug!(
                            session_id = ?candidate.session_id,
                            exit_code = ?out.status.code(),
                            stdout = ?stdout.trim(),
                            stderr = ?stderr.trim(),
                            "agentapi metadata check returned non-zero (expected fallback if session is archived/offline)"
                        );
                    }
                }
                Err(e) => {
                    tracing::debug!(
                        session_id = ?candidate.session_id,
                        error = ?e,
                        "failed to execute agentapi subprocess CLI (expected fallback to local transcript estimation)"
                    );
                }
            }

            let mut cumulative_chars = 0;
            let mut chars_at_last_model = 0;

            let file = match std::fs::File::open(&transcript_path) {
                Ok(f) => f,
                Err(e) => {
                    let _ = self.record_failure(&candidate.session_id, &e.to_string()).await;
                    continue;
                }
            };
            
            use std::io::{BufRead, BufReader};
            let reader = BufReader::new(file);
            let mut new_values = Vec::new();
            for line_res in reader.lines() {
                if let Ok(line) = line_res {
                    let trimmed = line.trim();
                    if trimmed.is_empty() { continue; }
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
                        new_values.push(v);
                    }
                }
            }

            enum GroupedStep {
                ModelGroup(Vec<serde_json::Value>),
                Other(serde_json::Value),
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

            let mut exported_steps = Vec::new();
            let mut exported_usages = Vec::new();
            let mut sequence = 0;

            for step in grouped_steps {
                match step {
                    GroupedStep::Other(val) => {
                        let content_str = val.get("content").and_then(|v| v.as_str()).unwrap_or("");
                        let thinking_str = val.get("thinking").and_then(|v| v.as_str()).unwrap_or("");
                        cumulative_chars += content_str.len() + thinking_str.len();
                        
                        let role = if val.get("type").and_then(|t| t.as_str()) == Some("USER_INPUT") { "user" } else { "system" };
                        let timestamp = extract_timestamp_val(&val);
                        
                        let text = content_str.to_string();
                        let step_index = val.get("step_index").and_then(|v| v.as_i64()).unwrap_or(0) as usize;
                        
                        exported_steps.push(serde_json::json!({
                            "recordType": "step",
                            "sessionId": &candidate.session_id,
                            "stepIndex": step_index,
                            "role": role,
                            "timestamp": timestamp,
                            "model": serde_json::Value::Null,
                            "text": text,
                        }));
                    }
                    GroupedStep::ModelGroup(group) => {
                        let mut group_content_len = 0;
                        let mut group_thinking_len = 0;
                        let mut stable_ts = None;
                        let mut step_idx = 0;
                        let mut model_raw = "gemini-3.5-flash".to_string();
                        let mut is_official = false;

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
                                model_raw = m.to_string();
                            }

                            if let Some(usage) = val.get("usage_metadata").filter(|v| !v.is_null()) {
                                official_input = usage.get("prompt_token_count").and_then(|v| v.as_u64());
                                official_output = usage.get("candidates_token_count").and_then(|v| v.as_u64());
                                official_cache = usage.get("cached_content_token_count").and_then(|v| v.as_u64());
                                official_reasoning = usage.get("thoughts_token_count").and_then(|v| v.as_u64());
                                is_official = true;
                            }

                            if stable_ts.is_none() {
                                stable_ts = extract_timestamp_val(val);
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

                            // Gemini API 每次调用发送完整上下文窗口（所有历史消息）作为 prompt
                            // 因此 input ≈ 累计字符数 * token/char 比率 + 基础系统指令开销
                            let full_context_tokens = 500 + ((cumulative_chars as f64) * 0.35).round() as u64;
                            input = full_context_tokens;
                            // 估算 cache: 之前已发送过的上下文部分可能命中缓存
                            let prev_context_tokens = ((chars_at_last_model as f64) * 0.35).round() as u64;
                            cache = prev_context_tokens;
                        }

                        let timestamp = stable_ts.unwrap_or(0);

                        exported_steps.push(serde_json::json!({
                            "recordType": "step",
                            "sessionId": &candidate.session_id,
                            "stepIndex": step_idx as usize,
                            "role": "model",
                            "timestamp": timestamp,
                            "model": &model_raw,
                            "text": String::new(),
                        }));

                        let mut raw_val = serde_json::json!({
                            "chatModel": {
                                "model": &model_raw,
                                "usage": {
                                    "apiProvider": "API_PROVIDER_GOOGLE_GEMINI",
                                    "inputTokens": input.to_string(),
                                    "model": &model_raw,
                                    "outputTokens": output.to_string(),
                                    "thinkingOutputTokens": reasoning.to_string(),
                                }
                            }
                        });

                        if is_official {
                            if let Some(obj) = raw_val.get_mut("chatModel").and_then(|m| m.as_object_mut()) {
                                obj.insert("usage_metadata".to_string(), serde_json::json!({
                                    "prompt_token_count": input,
                                    "candidates_token_count": output,
                                    "cached_content_token_count": cache,
                                    "thoughts_token_count": reasoning,
                                }));
                            }
                        }

                        let resolved_model_name = resolve_model(&model_raw);

                        exported_usages.push(serde_json::json!({
                            "recordType": "usage",
                            "sessionId": &candidate.session_id,
                            "sequence": sequence,
                            "stepIndex": step_idx,
                            "timestamp": timestamp,
                            "model": resolved_model_name,
                            "inputTokens": input,
                            "outputTokens": output,
                            "cacheReadTokens": cache,
                            "cacheWriteTokens": 0,
                            "reasoningTokens": reasoning,
                            "totalTokens": input + output + reasoning,
                            "raw": raw_val,
                        }));

                        sequence += 1;
                        cumulative_chars += group_content_len + group_thinking_len;
                        chars_at_last_model = cumulative_chars;
                    }
                }
            }

            let exported_steps_len = exported_steps.len();
            let serialized_steps = if self.config.export_steps_jsonl {
                Some(exported_steps)
            } else {
                let mut redacted = Vec::new();
                for mut s in exported_steps {
                    if let Some(obj) = s.as_object_mut() {
                        obj.insert("text".to_string(), serde_json::Value::String(String::new()));
                    }
                    redacted.push(s);
                }
                Some(redacted)
            };

            match self.write_session_artifacts(
                &candidate.session_id,
                Some(mtime_ms),
                Some(exported_steps_len as u32),
                serialized_steps,
                exported_usages,
            ).await {
                Ok(_) => {
                    exported_count += 1;
                }
                Err(e) => {
                    let _ = self.record_failure(&candidate.session_id, &e.to_string()).await;
                }
            }
        }
        
        Ok(exported_count)
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
        
        use sha1::{Digest, Sha1};
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

fn extract_timestamp_val(val: &serde_json::Value) -> Option<i64> {
    let ts_paths = [
        val.get("timestamp"),
        val.get("created_at"),
        val.get("createdAt"),
    ];
    for val_ts in ts_paths.into_iter().flatten() {
        if val_ts.is_null() { continue; }
        if let Some(n) = val_ts.as_i64() {
            return Some(n);
        }
        if let Some(s) = val_ts.as_str() {
            if let Ok(n) = s.parse::<i64>() {
                return Some(n);
            }
            if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
                return Some(dt.timestamp_millis());
            }
        }
    }
    None
}

fn resolve_model(raw: &str) -> String {
    let raw = raw.trim();
    if raw.is_empty() {
        return "unknown".to_string();
    }
    if let Some(resolved) = crate::model_aliases::resolve_model_placeholder(raw) {
        return resolved.to_string();
    }
    if raw.starts_with("MODEL_PLACEHOLDER_") {
        return "unknown".to_string();
    }
    raw.to_string()
}
