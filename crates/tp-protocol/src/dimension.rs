//! 数据维度与时间粒度定义。

use serde::{Deserialize, Serialize};

/// 数据聚合维度
///
/// 用于 data-cache 和 data-show 的维度投影与分片计算。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dimension {
    /// 按数据来源聚合 (Antigravity / Codex / CloudeCode)
    BySource,
    /// 按模型聚合
    ByModel,
    /// 按项目/会话聚合
    ByProject,
    /// 按 API Key 聚合
    ByApiKey,
    /// 按时间聚合 (需指定粒度)
    ByTime(TimeGranularity),
    /// 按父级项目聚合
    ByParentProject,
    /// 按报告类型聚合 (Official / Calculate)
    ByReportClass,
}

/// 时间聚合粒度
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeGranularity {
    /// 小时级
    Hourly,
    /// 天级
    Daily,
    /// 周级
    Weekly,
    /// 月级
    Monthly,
}

impl TimeGranularity {
    /// 获取用于格式化时间 key 的 chrono format 字符串
    pub fn format_str(&self) -> &'static str {
        match self {
            TimeGranularity::Hourly => "%Y-%m-%dT%H",
            TimeGranularity::Daily => "%Y-%m-%d",
            TimeGranularity::Weekly => "%Y-W%W",
            TimeGranularity::Monthly => "%Y-%m",
        }
    }
}

/// 时间窗口 — 用于聚合查询
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeWindow {
    /// 今天
    Today,
    /// 本周
    ThisWeek,
    /// 本月
    ThisMonth,
    /// 全部
    All,
    /// 最近 N 天
    LastNDays(u32),
}
