//! 核心数据记录类型 — Datalog 及其组成部分。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// 数据来源标识
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceName {
    /// Google Gemini Antigravity (VS Code 插件)
    Antigravity,
    /// Antigravity IDE (文件系统日志直接采集)
    #[serde(rename = "antigravity_ide", alias = "antigravity_v2")]
    AntigravityIDE,
    /// OpenAI Codex CLI
    Codex,
    /// Anthropic Claude Code CLI
    CloudeCode,
}

impl std::fmt::Display for SourceName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SourceName::Antigravity => write!(f, "Antigravity"),
            SourceName::AntigravityIDE => write!(f, "Antigravity IDE"),
            SourceName::Codex => write!(f, "Codex"),
            SourceName::CloudeCode => write!(f, "CloudeCode"),
        }
    }
}

/// 数据报告类型
///
/// - `Official`: 工具官方报告的精确 token 数据
/// - `Calculate`: 系统估算/计算的 token 数据
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportClass {
    /// 官方数据 — 来自 API 响应的精确 token 计数
    Official,
    /// 计算数据 — 系统估算值
    Calculate,
}

impl std::fmt::Display for ReportClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReportClass::Official => write!(f, "official"),
            ReportClass::Calculate => write!(f, "calculate"),
        }
    }
}

/// 五维 Token 信息
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenInfo {
    /// 输入 token 数
    pub input: u64,
    /// 输出 token 数
    pub output: u64,
    /// 缓存 token 数 (cache_read + cache_creation)
    pub cache: u64,
    /// 资源/上下文 token 数
    pub resourcing: u64,
    /// 推理/思考 token 数
    pub reasoning: u64,
}
impl TokenInfo {
    pub fn total(&self) -> u64 {
        self.input
            .saturating_add(self.cache)
            .saturating_add(self.output)
            .saturating_add(self.reasoning)
    }

    /// 增量累加另一个 TokenInfo
    pub fn accumulate(&mut self, other: &TokenInfo) {
        self.input += other.input;
        self.output += other.output;
        self.cache += other.cache;
        self.resourcing += other.resourcing;
        self.reasoning += other.reasoning;
    }

    /// 减去另一个 TokenInfo（用于差分计算）
    pub fn subtract(&mut self, other: &TokenInfo) {
        self.input = self.input.saturating_sub(other.input);
        self.output = self.output.saturating_sub(other.output);
        self.cache = self.cache.saturating_sub(other.cache);
        self.resourcing = self.resourcing.saturating_sub(other.resourcing);
        self.reasoning = self.reasoning.saturating_sub(other.reasoning);
    }

    /// 检查是否为零值
    pub fn is_zero(&self) -> bool {
        self.input == 0
            && self.output == 0
            && self.cache == 0
            && self.resourcing == 0
            && self.reasoning == 0
    }
}

impl std::ops::Add for TokenInfo {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self {
            input: self.input + rhs.input,
            output: self.output + rhs.output,
            cache: self.cache + rhs.cache,
            resourcing: self.resourcing + rhs.resourcing,
            reasoning: self.reasoning + rhs.reasoning,
        }
    }
}

impl std::ops::AddAssign for TokenInfo {
    fn add_assign(&mut self, rhs: Self) {
        self.accumulate(&rhs);
    }
}

/// 获取默认的采集时间 (当前时间)
pub fn default_collected_at() -> DateTime<Utc> {
    Utc::now()
}

/// 统一数据记录 — 系统中所有 token 使用数据的标准表示
///
/// 从架构图:
/// - sourceName(Antigravity/CodeX/CloudeCode)
/// - source-api-key(None/Some(key-id))
/// - source-project — 会话/项目名称标签
/// - source-model — 模型名称
/// - source-datetime — 发起请求的时间点
/// - source-through-time — 会话总执行时长
/// - source-parent-project — 父级项目标签（理论上作为子Agent的）
/// - source-report-class — 报告数据模式 (calculate/official)
/// - tokenInfo: input/output/cache/resourcing/reasoning
/// - collected_at: 系统采集数据的时间点 (系统时间，供未来统计/审计使用)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Datalog {
    /// 数据来源标识 (Antigravity / Codex / CloudeCode)
    pub source_name: SourceName,

    /// 采集数据的时间点 (系统时间，默认为当前时间)
    #[serde(default = "default_collected_at", with = "chrono::serde::ts_milliseconds")]
    pub collected_at: DateTime<Utc>,

    /// API Key 级别的标签 (部分工具没有用该配置)
    #[serde(default)]
    pub source_api_key: Option<String>,

    /// 来源对话的名称标签 (只有id则显示id)
    pub source_project: String,

    /// 来源模型的名称标签
    pub source_model: String,

    /// 用户发起会话的发起请求的时间点标签
    #[serde(with = "chrono::serde::ts_milliseconds")]
    pub source_datetime: DateTime<Utc>,

    /// 单次用户会话的总执行时长
    #[serde(with = "duration_millis")]
    pub source_through_time: Duration,

    /// 来源对话的父级项目标签 (理论上作为子Agent的)
    #[serde(default)]
    pub source_parent_project: Option<String>,

    /// 报告数据模式 — calculate(计算) / official(官方)
    pub source_report_class: ReportClass,

    /// 五维 token 数据
    pub token_info: TokenInfo,
}

/// Datalog 的唯一标识
///
/// UID = source_project + source_datetime
/// 用于 replace-or-push 规则的去重判断
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DatalogUid {
    pub source_project: String,
    #[serde(with = "chrono::serde::ts_milliseconds")]
    pub source_datetime: DateTime<Utc>,
}

impl DatalogUid {
    pub fn from_datalog(log: &Datalog) -> Self {
        Self {
            source_project: log.source_project.clone(),
            source_datetime: log.source_datetime,
        }
    }

    /// 生成可用作文件名/key 的字符串表示
    pub fn to_key_string(&self) -> String {
        format!(
            "{}@{}",
            self.source_project,
            self.source_datetime.timestamp_millis()
        )
    }
}

impl Datalog {
    /// 获取此记录的唯一标识
    pub fn uid(&self) -> DatalogUid {
        DatalogUid::from_datalog(self)
    }

    /// 获取此记录所属的小时 key (用于分片存储)
    /// 格式: "YYYY-MM-DDTHH"
    pub fn hour_key(&self) -> String {
        self.source_datetime.format("%Y-%m-%dT%H").to_string()
    }

    /// 获取此记录所属的日期 key
    /// 格式: "YYYY-MM-DD"
    pub fn date_key(&self) -> String {
        self.source_datetime.format("%Y-%m-%d").to_string()
    }

    /// 获取此记录所属的月份 key
    /// 格式: "YYYY-MM"
    pub fn month_key(&self) -> String {
        self.source_datetime.format("%Y-%m").to_string()
    }
}

/// Duration 的毫秒序列化/反序列化
mod duration_millis {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u64(duration.as_millis() as u64)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        let millis = u64::deserialize(deserializer)?;
        Ok(Duration::from_millis(millis))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_info_accumulate() {
        let mut a = TokenInfo { input: 100, output: 200, cache: 50, resourcing: 10, reasoning: 30 };
        let b = TokenInfo { input: 50, output: 100, cache: 25, resourcing: 5, reasoning: 15 };
        a.accumulate(&b);
        assert_eq!(a.input, 150);
        assert_eq!(a.output, 300);
        assert_eq!(a.total(), 570);
    }

    #[test]
    fn test_datalog_uid() {
        let log = Datalog {
            source_name: SourceName::Antigravity,
            collected_at: Utc::now(),
            source_api_key: None,
            source_project: "session-001".to_string(),
            source_model: "gemini-3.5-flash".to_string(),
            source_datetime: DateTime::from_timestamp_millis(1717000000000).unwrap(),
            source_through_time: Duration::from_secs(120),
            source_parent_project: None,
            source_report_class: ReportClass::Official,
            token_info: TokenInfo::default(),
        };
        let uid = log.uid();
        assert_eq!(uid.source_project, "session-001");
        assert!(uid.to_key_string().contains("session-001"));
    }

    #[test]
    fn test_datalog_time_keys() {
        let log = Datalog {
            source_name: SourceName::Codex,
            collected_at: Utc::now(),
            source_api_key: None,
            source_project: "test".to_string(),
            source_model: "gpt-4o".to_string(),
            source_datetime: chrono::NaiveDate::from_ymd_opt(2026, 5, 26)
                .unwrap()
                .and_hms_opt(14, 30, 0)
                .unwrap()
                .and_utc(),
            source_through_time: Duration::from_secs(60),
            source_parent_project: None,
            source_report_class: ReportClass::Calculate,
            token_info: TokenInfo { input: 100, output: 200, cache: 0, resourcing: 0, reasoning: 0 },
        };
        assert_eq!(log.hour_key(), "2026-05-26T14");
        assert_eq!(log.date_key(), "2026-05-26");
        assert_eq!(log.month_key(), "2026-05");
    }
}
