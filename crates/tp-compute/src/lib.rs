//! # tp-compute
//!
//! 增量聚合引擎 — 被 tp-cache 和 tp-aggregator 共享的 token 数据聚合计算核心。
//!
//! 提供 `IncrementalAggregator`，支持：
//! - 逐条或批量 ingest `Datalog` 记录
//! - 多维度 (source / model / project / time / report_class) 的增量累加
//! - 基于 `PricingTable` 的实时费用计算
//! - 快照序列化 / 反序列化
//! - 聚合器间的 merge 操作
//! - 维度投影 (`project`) 和时间窗口 (`window`) 查询

pub mod aggregator {
    //! Re-export of the aggregator types from the crate root.
    pub use crate::{AggregationSnapshot, IncrementalAggregator};
}

pub mod projection {
    //! Re-export of projection helpers.
    pub use crate::IncrementalAggregator;
}

pub mod window {
    //! Re-export of window helpers.
    pub use crate::IncrementalAggregator;
}

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Datelike, Utc};
use serde::{Deserialize, Serialize};
use tracing::trace;

use tp_protocol::{
    Datalog, DimensionEntry, PricingTable, ReportClass, SourceName, TokenInfo,
    dimension::{Dimension, TimeGranularity, TimeWindow},
};

// ---------------------------------------------------------------------------
// AggregationSnapshot — 可序列化的公开快照
// ---------------------------------------------------------------------------

/// 聚合状态的可序列化快照。
///
/// 用于持久化/恢复 `IncrementalAggregator` 的完整状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregationSnapshot {
    /// 全量 token 汇总
    pub total_tokens: TokenInfo,
    /// 全量费用 (USD)
    pub total_cost: f64,
    /// 按数据来源聚合
    pub by_source: HashMap<SourceName, TokenInfo>,
    /// 按模型聚合
    pub by_model: HashMap<String, TokenInfo>,
    /// 按项目聚合
    pub by_project: HashMap<String, TokenInfo>,
    /// 按小时聚合 — key 格式 "YYYY-MM-DDTHH"
    pub by_hour: BTreeMap<String, TokenInfo>,
    /// 按天聚合 — key 格式 "YYYY-MM-DD"
    pub by_day: BTreeMap<String, TokenInfo>,
    /// 按月聚合 — key 格式 "YYYY-MM"
    pub by_month: BTreeMap<String, TokenInfo>,
    /// 已累计的记录总数
    pub record_count: u64,
    /// 按报告类型聚合 (Official / Calculate)
    pub by_report_class: HashMap<ReportClass, TokenInfo>,
    /// 按天聚合的更详细统计 (含 record_count, cost, message_count)
    #[serde(default)]
    pub by_day_stats: BTreeMap<String, tp_protocol::view::DailyStats>,
}

impl Default for AggregationSnapshot {
    fn default() -> Self {
        Self {
            total_tokens: TokenInfo::default(),
            total_cost: 0.0,
            by_source: HashMap::new(),
            by_model: HashMap::new(),
            by_project: HashMap::new(),
            by_hour: BTreeMap::new(),
            by_day: BTreeMap::new(),
            by_month: BTreeMap::new(),
            record_count: 0,
            by_report_class: HashMap::new(),
            by_day_stats: BTreeMap::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// AggregationState — 内部可变状态
// ---------------------------------------------------------------------------

/// 内部聚合状态 — 与 `AggregationSnapshot` 结构一致但不直接公开。
#[derive(Debug, Clone)]
struct AggregationState {
    total_tokens: TokenInfo,
    total_cost: f64,
    by_source: HashMap<SourceName, TokenInfo>,
    by_model: HashMap<String, TokenInfo>,
    by_project: HashMap<String, TokenInfo>,
    by_hour: BTreeMap<String, TokenInfo>,
    by_day: BTreeMap<String, TokenInfo>,
    by_month: BTreeMap<String, TokenInfo>,
    record_count: u64,
    by_report_class: HashMap<ReportClass, TokenInfo>,
    by_day_stats: BTreeMap<String, tp_protocol::view::DailyStats>,
}

impl Default for AggregationState {
    fn default() -> Self {
        Self {
            total_tokens: TokenInfo::default(),
            total_cost: 0.0,
            by_source: HashMap::new(),
            by_model: HashMap::new(),
            by_project: HashMap::new(),
            by_hour: BTreeMap::new(),
            by_day: BTreeMap::new(),
            by_month: BTreeMap::new(),
            record_count: 0,
            by_report_class: HashMap::new(),
            by_day_stats: BTreeMap::new(),
        }
    }
}

impl AggregationState {
    /// 从快照恢复
    fn from_snapshot(snap: &AggregationSnapshot) -> Self {
        Self {
            total_tokens: snap.total_tokens,
            total_cost: snap.total_cost,
            by_source: snap.by_source.clone(),
            by_model: snap.by_model.clone(),
            by_project: snap.by_project.clone(),
            by_hour: snap.by_hour.clone(),
            by_day: snap.by_day.clone(),
            by_month: snap.by_month.clone(),
            record_count: snap.record_count,
            by_report_class: snap.by_report_class.clone(),
            by_day_stats: snap.by_day_stats.clone(),
        }
    }

    /// 导出为快照
    fn to_snapshot(&self) -> AggregationSnapshot {
        AggregationSnapshot {
            total_tokens: self.total_tokens,
            total_cost: self.total_cost,
            by_source: self.by_source.clone(),
            by_model: self.by_model.clone(),
            by_project: self.by_project.clone(),
            by_hour: self.by_hour.clone(),
            by_day: self.by_day.clone(),
            by_month: self.by_month.clone(),
            record_count: self.record_count,
            by_report_class: self.by_report_class.clone(),
            by_day_stats: self.by_day_stats.clone(),
        }
    }

    /// 将另一个状态合并到自身
    fn merge_from(&mut self, other: &AggregationState) {
        self.total_tokens.accumulate(&other.total_tokens);
        self.total_cost += other.total_cost;
        self.record_count += other.record_count;

        for (source, info) in &other.by_source {
            self.by_source.entry(*source).or_default().accumulate(info);
        }
        for (model, info) in &other.by_model {
            self.by_model.entry(model.clone()).or_default().accumulate(info);
        }
        for (project, info) in &other.by_project {
            self.by_project.entry(project.clone()).or_default().accumulate(info);
        }
        for (key, info) in &other.by_hour {
            self.by_hour.entry(key.clone()).or_default().accumulate(info);
        }
        for (key, info) in &other.by_day {
            self.by_day.entry(key.clone()).or_default().accumulate(info);
        }
        for (key, info) in &other.by_month {
            self.by_month.entry(key.clone()).or_default().accumulate(info);
        }
        for (rc, info) in &other.by_report_class {
            self.by_report_class.entry(*rc).or_default().accumulate(info);
        }
        for (key, stats) in &other.by_day_stats {
            let entry = self.by_day_stats.entry(key.clone()).or_default();
            entry.token_info.accumulate(&stats.token_info);
            entry.record_count += stats.record_count;
            entry.cost_usd += stats.cost_usd;
            entry.message_count += stats.message_count;
        }
    }
}

// ---------------------------------------------------------------------------
// IncrementalAggregator
// ---------------------------------------------------------------------------

/// 增量聚合器 — 核心计算引擎。
///
/// 持有内部 `AggregationState`，支持逐条 / 批量 ingest，
/// 并通过 `PricingTable::builtin()` 自动计算费用。
#[derive(Debug, Clone)]
pub struct IncrementalAggregator {
    state: AggregationState,
    pricing: PricingTable,
}

impl IncrementalAggregator {
    /// 创建空的聚合器 (使用内置定价表)
    pub fn new() -> Self {
        Self {
            state: AggregationState::default(),
            pricing: PricingTable::builtin(),
        }
    }

    /// 批量 ingest — 将一组 `Datalog` 记录累加到聚合状态
    pub fn ingest(&mut self, logs: &[Datalog]) {
        for log in logs {
            self.ingest_one(log);
        }
    }

    /// 单条 ingest — 将一条 `Datalog` 记录累加到聚合状态
    pub fn ingest_one(&mut self, log: &Datalog) {
        let token = &log.token_info;

        // 1. 全量汇总
        self.state.total_tokens.accumulate(token);

        // 2. 费用计算
        let cost = self.pricing.calculate_cost(&log.source_model, token);
        self.state.total_cost += cost;

        // 3. 按来源
        self.state
            .by_source
            .entry(log.source_name)
            .or_default()
            .accumulate(token);

        // 4. 按模型
        self.state
            .by_model
            .entry(log.source_model.clone())
            .or_default()
            .accumulate(token);

        // 5. 按项目
        self.state
            .by_project
            .entry(log.source_project.clone())
            .or_default()
            .accumulate(token);

        // 6. 按小时
        let hour_key = log.hour_key();
        self.state
            .by_hour
            .entry(hour_key)
            .or_default()
            .accumulate(token);

        // 7. 按天
        let day_key = log.date_key();
        self.state
            .by_day
            .entry(day_key.clone())
            .or_default()
            .accumulate(token);

        // 7.5. 按天更详细的统计
        let day_stats = self.state.by_day_stats.entry(day_key).or_default();
        day_stats.token_info.accumulate(token);
        day_stats.record_count += 1;
        day_stats.cost_usd += cost;
        day_stats.message_count += 1;

        // 8. 按月
        let month_key = log.month_key();
        self.state
            .by_month
            .entry(month_key)
            .or_default()
            .accumulate(token);

        // 9. 按报告类型
        self.state
            .by_report_class
            .entry(log.source_report_class)
            .or_default()
            .accumulate(token);

        // 10. 记录数
        self.state.record_count += 1;

        trace!(
            source = %log.source_name,
            model = %log.source_model,
            tokens = token.total(),
            cost = cost,
            "ingested datalog"
        );
    }

    /// 导出当前状态的快照
    pub fn snapshot(&self) -> AggregationSnapshot {
        self.state.to_snapshot()
    }

    /// 从另一个聚合器合并状态
    pub fn merge(&mut self, other: &IncrementalAggregator) {
        self.state.merge_from(&other.state);
    }

    /// 清空所有聚合状态
    pub fn reset(&mut self) {
        self.state = AggregationState::default();
    }

    /// 从快照恢复聚合器
    pub fn from_snapshot(snap: &AggregationSnapshot) -> Self {
        Self {
            state: AggregationState::from_snapshot(snap),
            pricing: PricingTable::builtin(),
        }
    }

    // -----------------------------------------------------------------------
    // 维度投影
    // -----------------------------------------------------------------------

    /// 将聚合状态按指定维度投影为 `DimensionEntry` 列表。
    ///
    /// 返回的列表按 token 总量降序排列 (时间维度按 key 升序)。
    pub fn project(&self, dim: &Dimension) -> Vec<DimensionEntry> {
        match dim {
            Dimension::BySource => {
                let mut entries: Vec<DimensionEntry> = self
                    .state
                    .by_source
                    .iter()
                    .map(|(source, info)| DimensionEntry {
                        key: source.to_string(),
                        token_info: *info,
                        record_count: 0, // 维度级记录数暂不追踪
                        cost_usd: self.pricing.calculate_cost(&source.to_string(), info),
                    })
                    .collect();
                entries.sort_by(|a, b| b.token_info.total().cmp(&a.token_info.total()));
                entries
            }
            Dimension::ByModel => {
                let mut entries: Vec<DimensionEntry> = self
                    .state
                    .by_model
                    .iter()
                    .map(|(model, info)| DimensionEntry {
                        key: model.clone(),
                        token_info: *info,
                        record_count: 0,
                        cost_usd: self.pricing.calculate_cost(model, info),
                    })
                    .collect();
                entries.sort_by(|a, b| b.token_info.total().cmp(&a.token_info.total()));
                entries
            }
            Dimension::ByProject => {
                let mut entries: Vec<DimensionEntry> = self
                    .state
                    .by_project
                    .iter()
                    .map(|(project, info)| DimensionEntry {
                        key: project.clone(),
                        token_info: *info,
                        record_count: 0,
                        cost_usd: 0.0, // 项目级费用无法精确计算（需要逐条记录模型信息）
                    })
                    .collect();
                entries.sort_by(|a, b| b.token_info.total().cmp(&a.token_info.total()));
                entries
            }
            Dimension::ByTime(granularity) => {
                let map = match granularity {
                    TimeGranularity::Hourly => &self.state.by_hour,
                    TimeGranularity::Daily => &self.state.by_day,
                    TimeGranularity::Monthly => &self.state.by_month,
                    // Weekly 不直接存储，从 by_day 聚合
                    TimeGranularity::Weekly => {
                        return self.project_weekly();
                    }
                };
                // 时间维度按 key 升序 (自然时间顺序)
                map.iter()
                    .map(|(key, info)| DimensionEntry {
                        key: key.clone(),
                        token_info: *info,
                        record_count: 0,
                        cost_usd: 0.0,
                    })
                    .collect()
            }
            Dimension::ByReportClass => {
                let mut entries: Vec<DimensionEntry> = self
                    .state
                    .by_report_class
                    .iter()
                    .map(|(rc, info)| DimensionEntry {
                        key: rc.to_string(),
                        token_info: *info,
                        record_count: 0,
                        cost_usd: 0.0,
                    })
                    .collect();
                entries.sort_by(|a, b| b.token_info.total().cmp(&a.token_info.total()));
                entries
            }
            // ByApiKey / ByParentProject — 当前不追踪，返回空
            _ => Vec::new(),
        }
    }

    /// 从 `by_day` 数据聚合出周粒度。
    ///
    /// key 格式: "YYYY-Www" (ISO 周)
    fn project_weekly(&self) -> Vec<DimensionEntry> {
        let mut weekly: BTreeMap<String, TokenInfo> = BTreeMap::new();

        for (day_key, info) in &self.state.by_day {
            // 解析 "YYYY-MM-DD"
            if let Ok(date) = chrono::NaiveDate::parse_from_str(day_key, "%Y-%m-%d") {
                let iso_week = date.iso_week();
                let week_key = format!("{}-W{:02}", iso_week.year(), iso_week.week());
                weekly.entry(week_key).or_default().accumulate(info);
            }
        }

        weekly
            .into_iter()
            .map(|(key, info)| DimensionEntry {
                key,
                token_info: info,
                record_count: 0,
                cost_usd: 0.0,
            })
            .collect()
    }

    // -----------------------------------------------------------------------
    // 时间窗口
    // -----------------------------------------------------------------------

    /// 计算指定时间窗口内的 token 汇总。
    ///
    /// 使用 `by_day` / `by_hour` 数据进行范围过滤。
    pub fn window(&self, window: TimeWindow, now: DateTime<Utc>) -> TokenInfo {
        match window {
            TimeWindow::All => self.state.total_tokens,

            TimeWindow::Today => {
                let today_key = now.format("%Y-%m-%d").to_string();
                self.state
                    .by_day
                    .get(&today_key)
                    .copied()
                    .unwrap_or_default()
            }

            TimeWindow::ThisWeek => {
                // 本周一 00:00 UTC 到 now
                let weekday = now.weekday().num_days_from_monday(); // 0=Mon
                let monday = now.date_naive() - chrono::Duration::days(weekday as i64);
                self.sum_days_from(&monday, now)
            }

            TimeWindow::ThisMonth => {
                let first_of_month = now
                    .date_naive()
                    .with_day(1)
                    .expect("day 1 always valid");
                self.sum_days_from(&first_of_month, now)
            }

            TimeWindow::LastNDays(n) => {
                let start = now.date_naive() - chrono::Duration::days(n as i64 - 1);
                self.sum_days_from(&start, now)
            }
        }
    }

    /// 从 `by_day` 中累加从 `start_date` (含) 到 `now` (含当天) 的所有天数。
    fn sum_days_from(&self, start_date: &chrono::NaiveDate, now: DateTime<Utc>) -> TokenInfo {
        let end_date = now.date_naive();
        let start_key = start_date.format("%Y-%m-%d").to_string();
        let end_key = end_date.format("%Y-%m-%d").to_string();

        let mut result = TokenInfo::default();
        for (_key, info) in self.state.by_day.range(start_key..=end_key) {
            result.accumulate(info);
        }
        result
    }
}

impl Default for IncrementalAggregator {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration as StdDuration;

    fn make_log(
        source: SourceName,
        model: &str,
        project: &str,
        dt: DateTime<Utc>,
        input: u64,
        output: u64,
    ) -> Datalog {
        Datalog {
            source_name: source,
            collected_at: Utc::now(),
            source_api_key: None,
            source_project: project.to_string(),
            source_model: model.to_string(),
            source_datetime: dt,
            source_through_time: StdDuration::from_secs(60),
            source_parent_project: None,
            source_report_class: ReportClass::Official,
            token_info: TokenInfo {
                input,
                output,
                cache: 0,
                resourcing: 0,
                reasoning: 0,
            },
        }
    }

    #[test]
    fn test_ingest_and_snapshot() {
        let mut agg = IncrementalAggregator::new();
        let dt = chrono::NaiveDate::from_ymd_opt(2026, 5, 26)
            .unwrap()
            .and_hms_opt(10, 0, 0)
            .unwrap()
            .and_utc();

        let log = make_log(SourceName::Antigravity, "gemini-3.5-flash", "session-1", dt, 100, 200);
        agg.ingest_one(&log);

        let snap = agg.snapshot();
        assert_eq!(snap.record_count, 1);
        assert_eq!(snap.total_tokens.input, 100);
        assert_eq!(snap.total_tokens.output, 200);
        assert!(snap.by_source.contains_key(&SourceName::Antigravity));
        assert!(snap.by_model.contains_key("gemini-3.5-flash"));
        assert!(snap.by_hour.contains_key("2026-05-26T10"));
        assert!(snap.by_day.contains_key("2026-05-26"));
        assert!(snap.by_month.contains_key("2026-05"));
    }

    #[test]
    fn test_batch_ingest() {
        let mut agg = IncrementalAggregator::new();
        let dt1 = chrono::NaiveDate::from_ymd_opt(2026, 5, 26)
            .unwrap()
            .and_hms_opt(10, 0, 0)
            .unwrap()
            .and_utc();
        let dt2 = chrono::NaiveDate::from_ymd_opt(2026, 5, 26)
            .unwrap()
            .and_hms_opt(11, 0, 0)
            .unwrap()
            .and_utc();

        let logs = vec![
            make_log(SourceName::Antigravity, "gemini-3.5-flash", "s1", dt1, 100, 200),
            make_log(SourceName::Codex, "gpt-4o", "s2", dt2, 50, 75),
        ];
        agg.ingest(&logs);

        let snap = agg.snapshot();
        assert_eq!(snap.record_count, 2);
        assert_eq!(snap.total_tokens.input, 150);
        assert_eq!(snap.total_tokens.output, 275);
    }

    #[test]
    fn test_merge() {
        let dt = chrono::NaiveDate::from_ymd_opt(2026, 5, 26)
            .unwrap()
            .and_hms_opt(10, 0, 0)
            .unwrap()
            .and_utc();

        let mut agg1 = IncrementalAggregator::new();
        agg1.ingest_one(&make_log(SourceName::Antigravity, "gemini-3.5-flash", "s1", dt, 100, 200));

        let mut agg2 = IncrementalAggregator::new();
        agg2.ingest_one(&make_log(SourceName::Codex, "gpt-4o", "s2", dt, 50, 75));

        agg1.merge(&agg2);
        let snap = agg1.snapshot();
        assert_eq!(snap.record_count, 2);
        assert_eq!(snap.total_tokens.input, 150);
    }

    #[test]
    fn test_reset() {
        let mut agg = IncrementalAggregator::new();
        let dt = chrono::NaiveDate::from_ymd_opt(2026, 5, 26)
            .unwrap()
            .and_hms_opt(10, 0, 0)
            .unwrap()
            .and_utc();

        agg.ingest_one(&make_log(SourceName::Antigravity, "gemini-3.5-flash", "s1", dt, 100, 200));
        assert_eq!(agg.snapshot().record_count, 1);

        agg.reset();
        let snap = agg.snapshot();
        assert_eq!(snap.record_count, 0);
        assert!(snap.total_tokens.is_zero());
    }

    #[test]
    fn test_from_snapshot_roundtrip() {
        let mut agg = IncrementalAggregator::new();
        let dt = chrono::NaiveDate::from_ymd_opt(2026, 5, 26)
            .unwrap()
            .and_hms_opt(10, 0, 0)
            .unwrap()
            .and_utc();

        agg.ingest_one(&make_log(SourceName::Antigravity, "gemini-3.5-flash", "s1", dt, 100, 200));

        let snap = agg.snapshot();
        let restored = IncrementalAggregator::from_snapshot(&snap);
        let snap2 = restored.snapshot();

        assert_eq!(snap.record_count, snap2.record_count);
        assert_eq!(snap.total_tokens, snap2.total_tokens);
        assert_eq!(snap.by_hour.len(), snap2.by_hour.len());
    }

    #[test]
    fn test_project_by_model() {
        let mut agg = IncrementalAggregator::new();
        let dt = chrono::NaiveDate::from_ymd_opt(2026, 5, 26)
            .unwrap()
            .and_hms_opt(10, 0, 0)
            .unwrap()
            .and_utc();

        agg.ingest_one(&make_log(SourceName::Antigravity, "gemini-3.5-flash", "s1", dt, 100, 200));
        agg.ingest_one(&make_log(SourceName::Antigravity, "gemini-3.5-pro", "s2", dt, 500, 1000));

        let entries = agg.project(&Dimension::ByModel);
        assert_eq!(entries.len(), 2);
        // 降序：gemini-3.5-pro (1500 total) 应在前
        assert_eq!(entries[0].key, "gemini-3.5-pro");
    }

    #[test]
    fn test_project_by_time_daily() {
        let mut agg = IncrementalAggregator::new();
        let dt1 = chrono::NaiveDate::from_ymd_opt(2026, 5, 25)
            .unwrap()
            .and_hms_opt(10, 0, 0)
            .unwrap()
            .and_utc();
        let dt2 = chrono::NaiveDate::from_ymd_opt(2026, 5, 26)
            .unwrap()
            .and_hms_opt(10, 0, 0)
            .unwrap()
            .and_utc();

        agg.ingest_one(&make_log(SourceName::Antigravity, "gemini-3.5-flash", "s1", dt1, 100, 200));
        agg.ingest_one(&make_log(SourceName::Antigravity, "gemini-3.5-flash", "s1", dt2, 50, 75));

        let entries = agg.project(&Dimension::ByTime(TimeGranularity::Daily));
        assert_eq!(entries.len(), 2);
        // 升序: 2026-05-25 在前
        assert_eq!(entries[0].key, "2026-05-25");
        assert_eq!(entries[1].key, "2026-05-26");
    }

    #[test]
    fn test_window_today() {
        let mut agg = IncrementalAggregator::new();
        let now = chrono::NaiveDate::from_ymd_opt(2026, 5, 26)
            .unwrap()
            .and_hms_opt(15, 0, 0)
            .unwrap()
            .and_utc();

        // 今日数据
        agg.ingest_one(&make_log(SourceName::Antigravity, "gemini-3.5-flash", "s1", now, 100, 200));
        // 昨日数据
        let yesterday = now - chrono::Duration::days(1);
        agg.ingest_one(&make_log(SourceName::Antigravity, "gemini-3.5-flash", "s2", yesterday, 500, 1000));

        let today_tokens = agg.window(TimeWindow::Today, now);
        assert_eq!(today_tokens.input, 100);
        assert_eq!(today_tokens.output, 200);
    }

    #[test]
    fn test_window_all() {
        let mut agg = IncrementalAggregator::new();
        let now = chrono::NaiveDate::from_ymd_opt(2026, 5, 26)
            .unwrap()
            .and_hms_opt(15, 0, 0)
            .unwrap()
            .and_utc();

        agg.ingest_one(&make_log(SourceName::Antigravity, "gemini-3.5-flash", "s1", now, 100, 200));

        let all = agg.window(TimeWindow::All, now);
        assert_eq!(all.input, 100);
        assert_eq!(all.output, 200);
    }

    #[test]
    fn test_window_last_n_days() {
        let mut agg = IncrementalAggregator::new();
        let now = chrono::NaiveDate::from_ymd_opt(2026, 5, 26)
            .unwrap()
            .and_hms_opt(15, 0, 0)
            .unwrap()
            .and_utc();

        // 3 天的数据
        for i in 0..5 {
            let dt = now - chrono::Duration::days(i);
            agg.ingest_one(&make_log(SourceName::Antigravity, "gemini-3.5-flash", "s1", dt, 100, 0));
        }

        let last3 = agg.window(TimeWindow::LastNDays(3), now);
        assert_eq!(last3.input, 300); // 今天 + 昨天 + 前天
    }

    #[test]
    fn test_cost_calculation() {
        let mut agg = IncrementalAggregator::new();
        let dt = chrono::NaiveDate::from_ymd_opt(2026, 5, 26)
            .unwrap()
            .and_hms_opt(10, 0, 0)
            .unwrap()
            .and_utc();

        // gemini-3.5-flash: input=$0.075/M, output=$0.30/M
        agg.ingest_one(&make_log(
            SourceName::Antigravity,
            "gemini-3.5-flash",
            "s1",
            dt,
            1_000_000, // 1M input tokens
            1_000_000, // 1M output tokens
        ));

        let snap = agg.snapshot();
        // Expected: 0.075 + 0.30 = 0.375
        assert!((snap.total_cost - 0.375).abs() < 0.001);
    }

    #[test]
    fn test_default_impl() {
        let agg = IncrementalAggregator::default();
        let snap = agg.snapshot();
        assert_eq!(snap.record_count, 0);
        assert!(snap.total_tokens.is_zero());
    }
}
