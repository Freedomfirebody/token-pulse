use std::path::PathBuf;
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorConfig {
    pub session_root: String,
    pub debug: bool,
    pub poll_interval_ms: u64,
    pub history_limit: usize,
    pub max_file_bytes: u64,
    pub use_rpc_export: bool,
    pub export_steps_jsonl: bool,
    pub rpc_export_interval_ms: u64,
    pub rpc_timeout_ms: u64,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
            session_root: get_default_session_root().to_string_lossy().to_string(),
            debug: false,
            poll_interval_ms: 60_000,
            history_limit: 120,
            max_file_bytes: 100 * 1024 * 1024, // 100MB
            use_rpc_export: true,
            export_steps_jsonl: false,
            rpc_export_interval_ms: 300_000,
            rpc_timeout_ms: 5_000,
        }
    }
}

pub fn get_default_session_root() -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        home.join(".gemini").join("antigravity")
    } else {
        PathBuf::from(".")
    }
}
