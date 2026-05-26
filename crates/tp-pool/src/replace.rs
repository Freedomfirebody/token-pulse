//! Replace-or-push 规则引擎。
//!
//! 实现 datalog 的去重与优先级覆盖策略:
//! - UID = source_project + source_datetime
//! - Rule 1: Official 替换 Official 或 Calculate
//! - Rule 2: Calculate 替换 Calculate
//! - Rule 3: Calculate **不**替换 Official

use std::collections::HashMap;
use std::path::Path;

use tp_protocol::{Datalog, DatalogUid, PoolError, ReportClass};

/// Replace-or-push 决策结果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplaceDecision {
    /// 新记录，直接追加
    Push,
    /// 替换已有记录
    Replace,
    /// 跳过 (calculate 不替换 official)
    Skip,
}

/// Replace-or-push 规则引擎
///
/// 维护一个 UID key_string → ReportClass 的注册表，
/// 用于判断新到达的 datalog 应该 push / replace / skip。
#[derive(Debug, Clone)]
pub struct ReplaceOrPush {
    /// key = DatalogUid::to_key_string(), value = ReportClass
    registry: HashMap<String, ReportClass>,
}

impl ReplaceOrPush {
    /// 创建空的规则引擎
    pub fn new() -> Self {
        Self {
            registry: HashMap::new(),
        }
    }

    /// 对 incoming datalog 应用 replace-or-push 规则
    ///
    /// 返回决策结果，但**不**自动注册。
    /// 调用者应在 Push/Replace 成功落盘后调用 `register`。
    pub fn apply(&mut self, incoming: &Datalog) -> ReplaceDecision {
        let uid = incoming.uid();
        let key = uid.to_key_string();
        let incoming_class = incoming.source_report_class;

        match self.registry.get(&key) {
            None => {
                // 未见过此 UID — 直接 push
                ReplaceDecision::Push
            }
            Some(existing_class) => {
                match (incoming_class, existing_class) {
                    // Rule 1: Official replaces Official or Calculate
                    (ReportClass::Official, ReportClass::Official) => ReplaceDecision::Replace,
                    (ReportClass::Official, ReportClass::Calculate) => ReplaceDecision::Replace,
                    // Rule 2: Calculate replaces Calculate
                    (ReportClass::Calculate, ReportClass::Calculate) => ReplaceDecision::Replace,
                    // Rule 3: Calculate does NOT replace Official
                    (ReportClass::Calculate, ReportClass::Official) => ReplaceDecision::Skip,
                }
            }
        }
    }

    /// 注册一条 UID → ReportClass 映射
    ///
    /// 在数据成功写入存储后调用。
    pub fn register(&mut self, uid: DatalogUid, class: ReportClass) {
        self.registry.insert(uid.to_key_string(), class);
    }

    /// 将注册表持久化到文件
    pub fn save(&self, path: &Path) -> Result<(), PoolError> {
        let json = serde_json::to_string_pretty(&self.registry).map_err(|e| {
            PoolError::SerializationError(format!("注册表序列化失败: {e}"))
        })?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, json)?;
        tracing::debug!("注册表已保存: {} 条目 -> {}", self.registry.len(), path.display());
        Ok(())
    }

    /// 从文件加载注册表
    pub fn load(path: &Path) -> Result<Self, PoolError> {
        if !path.exists() {
            tracing::info!("注册表文件不存在，创建空注册表: {}", path.display());
            return Ok(Self::new());
        }
        let content = std::fs::read_to_string(path)?;
        let registry: HashMap<String, ReportClass> =
            serde_json::from_str(&content).map_err(|e| {
                PoolError::SerializationError(format!("注册表反序列化失败: {e}"))
            })?;
        tracing::info!("注册表已加载: {} 条目 <- {}", registry.len(), path.display());
        Ok(Self { registry })
    }

    /// 获取当前注册表的条目数
    pub fn len(&self) -> usize {
        self.registry.len()
    }

    /// 检查注册表是否为空
    pub fn is_empty(&self) -> bool {
        self.registry.is_empty()
    }
}

impl Default for ReplaceOrPush {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::DateTime;
    use std::time::Duration;
    use tp_protocol::{SourceName, TokenInfo};

    fn make_datalog(project: &str, class: ReportClass) -> Datalog {
        Datalog {
            source_name: SourceName::Antigravity,
            collected_at: chrono::Utc::now(),
            source_api_key: None,
            source_project: project.to_string(),
            source_model: "gemini-3.5-flash".to_string(),
            source_datetime: DateTime::from_timestamp_millis(1717000000000).unwrap(),
            source_through_time: Duration::from_secs(60),
            source_parent_project: None,
            source_report_class: class,
            token_info: TokenInfo::default(),
        }
    }

    #[test]
    fn test_push_new_record() {
        let mut engine = ReplaceOrPush::new();
        let log = make_datalog("sess-1", ReportClass::Official);
        assert_eq!(engine.apply(&log), ReplaceDecision::Push);
    }

    #[test]
    fn test_official_replaces_official() {
        let mut engine = ReplaceOrPush::new();
        let log = make_datalog("sess-1", ReportClass::Official);
        engine.register(log.uid(), ReportClass::Official);

        let incoming = make_datalog("sess-1", ReportClass::Official);
        assert_eq!(engine.apply(&incoming), ReplaceDecision::Replace);
    }

    #[test]
    fn test_official_replaces_calculate() {
        let mut engine = ReplaceOrPush::new();
        let log = make_datalog("sess-1", ReportClass::Calculate);
        engine.register(log.uid(), ReportClass::Calculate);

        let incoming = make_datalog("sess-1", ReportClass::Official);
        assert_eq!(engine.apply(&incoming), ReplaceDecision::Replace);
    }

    #[test]
    fn test_calculate_replaces_calculate() {
        let mut engine = ReplaceOrPush::new();
        let log = make_datalog("sess-1", ReportClass::Calculate);
        engine.register(log.uid(), ReportClass::Calculate);

        let incoming = make_datalog("sess-1", ReportClass::Calculate);
        assert_eq!(engine.apply(&incoming), ReplaceDecision::Replace);
    }

    #[test]
    fn test_calculate_does_not_replace_official() {
        let mut engine = ReplaceOrPush::new();
        let log = make_datalog("sess-1", ReportClass::Official);
        engine.register(log.uid(), ReportClass::Official);

        let incoming = make_datalog("sess-1", ReportClass::Calculate);
        assert_eq!(engine.apply(&incoming), ReplaceDecision::Skip);
    }
}
