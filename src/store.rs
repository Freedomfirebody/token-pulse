use std::fs::File;
use std::io::{Write, Read};
use std::path::PathBuf;

use crate::config::MonitorConfig;

pub struct SettingsStore {
    file_path: PathBuf,
}

impl SettingsStore {
    pub fn new(session_root: &str) -> Self {
        let file_path = PathBuf::from(session_root)
            .join(".token-monitor")
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

    pub fn save_config(&self, config: &MonitorConfig) -> Result<(), std::io::Error> {
        if let Some(parent) = self.file_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let content = serde_json::to_string_pretty(config)?;
        let mut file = File::create(&self.file_path)?;
        file.write_all(content.as_bytes())?;
        Ok(())
    }
}
