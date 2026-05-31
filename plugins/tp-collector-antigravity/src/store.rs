use std::fs::File;
use std::io::Read;
use std::path::PathBuf;

use crate::config::MonitorConfig;

pub struct SettingsStore {
    file_path: PathBuf,
}

impl SettingsStore {
    pub fn new(session_root: &str) -> Self {
        let folder_name = PathBuf::from(session_root)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("antigravity")
            .to_string();
        let file_path = dirs::home_dir()
            .map(|h| h.join(".token-pulse").join("token-monitor"))
            .unwrap_or_else(|| PathBuf::from(".token-pulse").join("token-monitor"))
            .join(folder_name)
            .join("settings.json");
        Self { file_path }
    }

    pub fn load_config(&self) -> MonitorConfig {
        if !self.file_path.exists() {
            return MonitorConfig::default();
        }

        let mut file = match File::open(&self.file_path) {
            Ok(f) => f,
            Err(_) => return MonitorConfig::default(),
        };

        let mut content = String::new();
        if file.read_to_string(&mut content).is_ok() {
            if let Ok(config) = serde_json::from_str::<MonitorConfig>(&content) {
                return config;
            }
        }

        MonitorConfig::default()
    }
}
