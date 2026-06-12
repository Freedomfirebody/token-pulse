//! 面向 Dash 渲染层的数据视图类型。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::datalog::{Datalog, SourceName, TokenInfo};

/// 数据源状态
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceStatus {
    /// 来源名称
    pub source: SourceName,
    /// 是否在线/可达
    pub online: bool,
    /// 最后采集时间
    pub last_collected_at: Option<DateTime<Utc>>,
    /// 最后采集记录数
    pub last_collected_count: usize,
    /// 错误信息 (如有)
    pub last_error: Option<String>,
}

/// 维度聚合条目
///
/// 用于表示按某个维度聚合后的单行数据。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DimensionEntry {
    /// 维度 key (模型名、来源名、项目名、日期等)
    pub key: String,
    /// 聚合后的 token 数据
    pub token_info: TokenInfo,
    /// 记录数
    pub record_count: u64,
    /// 费用 (USD)
    pub cost_usd: f64,
    /// 显示名称 (可选，如对话的友好名称)
    #[serde(default)]
    pub display_name: Option<String>,
}

/// 仪表盘完整数据视图
///
/// 这是 data-show 输出的最终数据结构，
/// 由 Dash 渲染层直接消费。
///
/// 合并了 cold-data (来自 cache) 和 hot-data (来自 pool) 的增量计算结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardView {
    // ===== KPI 指标 =====

    /// 全量 token 汇总
    pub total_tokens: TokenInfo,
    /// 今日 token 汇总
    pub today_tokens: TokenInfo,
    /// 本周 token 汇总
    pub week_tokens: TokenInfo,
    /// 本月 token 汇总
    pub month_tokens: TokenInfo,
    /// 总费用 (USD)
    pub total_cost: f64,
    /// 今日费用
    pub today_cost: f64,
    /// 本周费用
    pub week_cost: f64,
    /// 本月费用
    pub month_cost: f64,
    /// 总记录数
    pub record_count: u64,

    // ===== 分维度聚合 =====

    /// 按来源聚合
    pub by_source: Vec<DimensionEntry>,
    /// 按模型聚合
    pub by_model: Vec<DimensionEntry>,
    /// 按项目聚合
    pub by_project: Vec<DimensionEntry>,

    // ===== 时间序列 =====

    /// 按天的 token 时间序列 (热力图数据)
    /// key: "YYYY-MM-DD", value: 该天的 token 汇总
    pub daily_series: BTreeMap<String, DailyStats>,

    /// 按小时的今日时间序列
    /// key: "HH", value: 该小时的 token 汇总
    pub hourly_today: BTreeMap<String, TokenInfo>,

    // ===== 最近活跃 =====

    /// 最近的数据记录 (用于日志表格)
    pub recent_records: Vec<RecentRecord>,

    // ===== 元信息 =====

    /// 视图生成时间
    pub last_updated: DateTime<Utc>,
    /// 各数据源状态
    pub source_status: Vec<SourceStatus>,
    /// 缓存的中止节点 (截止的小时 key)
    pub cache_termination_key: Option<String>,
    #[serde(default)]
    pub daily_by_source: BTreeMap<String, std::collections::HashMap<SourceName, DailyStats>>,
    #[serde(default)]
    pub hourly_today_by_source: BTreeMap<String, std::collections::HashMap<SourceName, TokenInfo>>,

    // ===== Precise Source Lookup Maps =====
    #[serde(default)]
    pub project_sources: std::collections::HashMap<String, std::collections::HashSet<SourceName>>,
    #[serde(default)]
    pub model_sources: std::collections::HashMap<String, std::collections::HashSet<SourceName>>,

    /// 内存超限警告信息
    #[serde(default)]
    pub memory_warning: Option<String>,
}

/// 每日统计
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DailyStats {
    /// 该日的 token 汇总
    pub token_info: TokenInfo,
    /// 记录数
    pub record_count: u64,
    /// 费用
    pub cost_usd: f64,
    /// 消息/请求数
    pub message_count: u64,
}

/// 最近记录 (简化版 Datalog，用于展示)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentRecord {
    pub source_name: SourceName,
    pub source_project: String,
    pub source_model: String,
    pub source_datetime: DateTime<Utc>,
    pub source_report_class: crate::datalog::ReportClass,
    pub token_info: TokenInfo,
    pub cost_usd: f64,
}

impl RecentRecord {
    pub fn from_datalog(log: &Datalog, cost: f64) -> Self {
        Self {
            source_name: log.source_name,
            source_project: log.source_project.clone(),
            source_model: log.source_model.clone(),
            source_datetime: log.source_datetime,
            source_report_class: log.source_report_class,
            token_info: log.token_info,
            cost_usd: cost,
        }
    }
}

impl Default for DashboardView {
    fn default() -> Self {
        Self {
            total_tokens: TokenInfo::default(),
            today_tokens: TokenInfo::default(),
            week_tokens: TokenInfo::default(),
            month_tokens: TokenInfo::default(),
            total_cost: 0.0,
            today_cost: 0.0,
            week_cost: 0.0,
            month_cost: 0.0,
            record_count: 0,
            by_source: Vec::new(),
            by_model: Vec::new(),
            by_project: Vec::new(),
            daily_series: BTreeMap::new(),
            hourly_today: BTreeMap::new(),
            recent_records: Vec::new(),
            last_updated: Utc::now(),
            source_status: Vec::new(),
            cache_termination_key: None,
            daily_by_source: BTreeMap::new(),
            hourly_today_by_source: BTreeMap::new(),
            project_sources: std::collections::HashMap::new(),
            model_sources: std::collections::HashMap::new(),
            memory_warning: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dashboard_view_memory_warning_serialization() {
        let mut view = DashboardView::default();
        view.memory_warning = Some("⚠️ languageService 内存占用过大 (3.4 GB)".to_string());

        let serialized = serde_json::to_string(&view).unwrap();
        let deserialized: DashboardView = serde_json::from_str(&serialized).unwrap();

        assert_eq!(
            deserialized.memory_warning,
            Some("⚠️ languageService 内存占用过大 (3.4 GB)".to_string())
        );
    }

    #[test]
    fn test_dashboard_view_memory_warning_backwards_compatibility() {
        // A json string representing old view data without the memory_warning field
        let json_data = r#"{
            "total_tokens": {"input": 0, "output": 0, "cache": 0, "resourcing": 0, "reasoning": 0},
            "today_tokens": {"input": 0, "output": 0, "cache": 0, "resourcing": 0, "reasoning": 0},
            "week_tokens": {"input": 0, "output": 0, "cache": 0, "resourcing": 0, "reasoning": 0},
            "month_tokens": {"input": 0, "output": 0, "cache": 0, "resourcing": 0, "reasoning": 0},
            "total_cost": 0.0,
            "today_cost": 0.0,
            "week_cost": 0.0,
            "month_cost": 0.0,
            "record_count": 0,
            "by_source": [],
            "by_model": [],
            "by_project": [],
            "daily_series": {},
            "hourly_today": {},
            "recent_records": [],
            "last_updated": "2026-06-11T14:27:00Z",
            "source_status": [],
            "daily_by_source": {},
            "hourly_today_by_source": {}
        }"#;

        let deserialized: DashboardView = serde_json::from_str(json_data).unwrap();
        assert!(deserialized.memory_warning.is_none());
    }
}
