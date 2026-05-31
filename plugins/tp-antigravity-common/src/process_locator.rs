//! 进程检测与端口扫描 — 发现运行中的 language_server 实例
//!
//! 提供 `ProcessLocator`（进程发现）和 `PortLocator`（端口扫描），
//! 支持 Windows WMI + sysinfo 双路径检测，以及跨平台端口枚举。

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use sysinfo::System;
use tracing::{debug, trace};

use crate::rpc_client::RpcClient;
use crate::types::{ProcessCandidate, RpcConnection};

// ==========================================
// 1. ProcessLocator
// ==========================================

/// Antigravity language_server 进程定位器
pub struct ProcessLocator {
    _debug: bool,
}

impl ProcessLocator {
    /// 创建新的进程定位器
    pub fn new(debug: bool) -> Self {
        Self { _debug: debug }
    }

    /// 检测匹配指定 app_data_dir 的 language_server 进程
    ///
    /// Windows 下优先使用 WMI (Get-CimInstance) 获取完整命令行，
    /// 失败时回退到 sysinfo crate。
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
                                trace!("[DEBUG ProcessLocator WMI] Found matching process name: '{}', PID: {}, Args: {:?}", name, pid, args);
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

        // sysinfo 回退路径（所有平台通用）
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
                trace!("[DEBUG ProcessLocator] Found matching process name: '{}', PID: {}, Args: {:?}", name_str, pid_u32, args_vec);
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

        // 去重并按 PID 降序排列
        let mut by_pid = HashMap::new();
        for c in candidates {
            by_pid.insert(c.pid, c);
        }
        let mut result: Vec<ProcessCandidate> = by_pid.into_values().collect();
        result.sort_by(|a, b| b.pid.cmp(&a.pid));
        result
    }

    /// 检测并验证 RPC 连接 — 组合进程发现 + 端口扫描 + 心跳验证
    ///
    /// 返回所有通过心跳验证的活跃连接。
    /// 内部的进程检测 (PowerShell WMI) 和端口扫描 (netstat) 是同步阻塞操作，
    /// 已包装在 `spawn_blocking` 中避免阻塞 tokio 线程。
    /// 整体操作有 30 秒超时保护。
    pub async fn detect_connections(
        client: &RpcClient,
        target_app_data_dir: &str,
    ) -> Vec<RpcConnection> {
        use tokio::time::timeout;
        use std::time::Duration;

        // 30 秒总超时，避免 WMI/netstat 卡住时无限阻塞
        match timeout(Duration::from_secs(30), Self::detect_connections_inner(client, target_app_data_dir)).await {
            Ok(connections) => connections,
            Err(_) => {
                tracing::warn!(
                    target_app_data_dir,
                    "进程检测超时 (30s)，跳过 RPC 连接"
                );
                Vec::new()
            }
        }
    }

    /// detect_connections 的实际实现（无超时包装）
    async fn detect_connections_inner(
        client: &RpcClient,
        target_app_data_dir: &str,
    ) -> Vec<RpcConnection> {
        // 将同步阻塞的进程检测放到 spawn_blocking 中
        let target = target_app_data_dir.to_string();
        let process_candidates = match tokio::task::spawn_blocking(move || {
            let locator = ProcessLocator::new(false);
            locator.detect_processes(&target)
        }).await {
            Ok(candidates) => candidates,
            Err(e) => {
                tracing::warn!(error = %e, "进程检测 spawn_blocking 失败");
                return Vec::new();
            }
        };

        debug!(
            count = process_candidates.len(),
            target_app_data_dir,
            "进程检测完成"
        );

        let mut connections = Vec::new();

        for candidate in &process_candidates {
            // 将同步阻塞的端口扫描放到 spawn_blocking 中
            let pid = candidate.pid;
            let scanned_ports = match tokio::task::spawn_blocking(move || {
                PortLocator::get_listening_ports(pid)
            }).await {
                Ok(ports) => ports,
                Err(_) => Vec::new(),
            };

            let mut candidate_ports = Vec::new();
            if candidate.extension_port > 0 {
                candidate_ports.push(candidate.extension_port);
            }
            for p in &scanned_ports {
                if !candidate_ports.contains(p) {
                    candidate_ports.push(*p);
                }
            }

            debug!(
                pid = candidate.pid,
                ports = ?candidate_ports,
                "候选进程端口列表"
            );

            // 逐端口尝试心跳验证（已经是 async HTTP 请求，无需 spawn_blocking）
            for port in &candidate_ports {
                let conn = RpcConnection {
                    pid: candidate.pid,
                    port: *port,
                    csrf_token: candidate.csrf_token.clone(),
                    app_data_dir: target_app_data_dir.to_string(),
                };

                if client.heartbeat(&conn).await {
                    debug!(pid = candidate.pid, port, "心跳验证成功");
                    connections.push(conn);
                    break; // 每个进程只需一个有效端口
                }
            }
        }

        connections
    }
}

// ==========================================
// 2. PortLocator
// ==========================================

/// 进程端口扫描器 — 获取指定 PID 的 TCP 监听端口
pub struct PortLocator;

impl PortLocator {
    /// 获取指定 PID 的所有 TCP 监听端口
    ///
    /// Windows: 使用 `netstat -ano`
    /// Unix: 依次尝试 lsof / ss / netstat
    pub fn get_listening_ports(pid: u32) -> Vec<u16> {
        #[cfg(target_os = "windows")]
        {
            let mut ports = std::collections::BTreeSet::new();
            if let Ok(output) = Command::new("netstat").arg("-ano").output() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                for line in stdout.lines() {
                    let tokens: Vec<&str> = line.split_whitespace().collect();
                    if tokens.len() >= 5 {
                        // 确认是 TCP 连接且处于 LISTENING 状态
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

// ==========================================
// 辅助函数
// ==========================================

/// 解析 Windows 命令行字符串为参数列表（处理引号）
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

/// 判断进程是否为目标 antigravity language_server
fn is_antigravity_process(args: &[String], name: &str, target_app_data_dir: &str) -> bool {
    let name_lower = name.to_lowercase();

    // language_server 始终带 --csrf_token 参数；GUI 包装器 "Antigravity.exe" 则没有
    let has_csrf = args.iter().any(|arg| arg.contains("--csrf_token"));

    if name_lower.contains("language_server") || (name_lower.contains("antigravity") && has_csrf) {
        // 检查 --app_data_dir 参数是否匹配目标目录
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

        // 未显式设置 --app_data_dir 时，回退到检查进程名是否包含目标目录名
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

/// 从地址字符串中解析端口号（如 "127.0.0.1:42150" → 42150）
fn parse_port_from_address(addr: &str) -> Option<u16> {
    if addr.contains(']') {
        addr.split("]:").nth(1)?.parse::<u16>().ok()
    } else {
        addr.split(':').last()?.parse::<u16>().ok()
    }
}

/// 从 Unix 命令输出中解析监听端口列表
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
