//! 归档快照块 (Archive Digest) — 预计算的时间窗口聚合结论。
//!
//! 快照块在归档时一次性生成，后续查询直接使用结论，
//! 不需要重新读取和解析原始 JSONL 记录。
//!
//! ## 粒度
//!
//! - **Daily**: 每天一个，包含小时级明细 (`by_hour`)
//! - **Monthly**: 每月一个，由多个 Daily digest 合并而来，不含小时级明细

use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::Hasher;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::datalog::{Datalog, ReportClass, SourceName, TokenInfo};
use crate::view::DailyStats;

/// 快照块粒度
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DigestGranularity {
    /// 日快照块 — 包含小时级明细
    Daily,
    /// 月快照块 — 由日快照块合并，不含小时级明细
    Monthly,
}

/// 归档快照块 — 一个固化的时间窗口的预计算聚合结论
///
/// 包含该时段内所有 11 个分析维度的聚合数据，
/// 在归档时一次性计算，后续 Cache 重建时直接合并使用。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveDigest {
    // ===== 元信息 =====

    /// 快照块粒度
    pub granularity: DigestGranularity,
    /// 时间 key — "YYYY-MM-DD" (Daily) 或 "YYYY-MM" (Monthly)
    pub time_key: String,
    /// 构建时间
    pub built_at: DateTime<Utc>,
    /// 原始记录的 checksum（用于验证快照是否过期）
    pub source_checksum: u64,

    // ===== KPI 汇总 =====

    /// 该时段的 token 汇总
    pub total_tokens: TokenInfo,
    /// 该时段的费用 (USD)
    pub total_cost: f64,
    /// 该时段的记录数
    pub record_count: u64,

    // ===== 按来源维度 =====

    /// 按来源的 token 分布
    pub by_source: HashMap<SourceName, TokenInfo>,
    /// 按来源的费用
    pub source_costs: HashMap<SourceName, f64>,
    /// 按来源的记录数
    pub source_records: HashMap<SourceName, u64>,

    // ===== 按模型维度 =====

    /// 按模型的 token 分布
    pub by_model: HashMap<String, TokenInfo>,
    /// 按模型的费用
    pub model_costs: HashMap<String, f64>,
    /// 按模型的记录数
    pub model_records: HashMap<String, u64>,

    // ===== 按项目维度 =====

    /// 按项目的 token 分布
    pub by_project: HashMap<String, TokenInfo>,
    /// 按项目的费用
    pub project_costs: HashMap<String, f64>,
    /// 按项目的记录数
    pub project_records: HashMap<String, u64>,
    /// 项目名称映射 (UUID → 友好名称)
    pub project_names: HashMap<String, String>,

    // ===== 来源映射 =====

    /// 项目 → 来源集合
    pub project_sources: HashMap<String, HashSet<SourceName>>,
    /// 模型 → 来源集合
    pub model_sources: HashMap<String, HashSet<SourceName>>,

    // ===== 时间序列 =====

    /// 按天统计（Daily digest 只有 1 条，Monthly 有多条）
    pub by_day_stats: BTreeMap<String, DailyStats>,
    /// 按天×来源统计
    pub by_day_source: BTreeMap<String, HashMap<SourceName, DailyStats>>,

    /// 按小时的 token 分布（仅 Daily digest 有，Monthly 为 None）
    #[serde(default)]
    pub by_hour: Option<BTreeMap<String, TokenInfo>>,
    /// 按小时×来源（仅 Daily digest 有，Monthly 为 None）
    #[serde(default)]
    pub by_hour_source: Option<BTreeMap<String, HashMap<SourceName, TokenInfo>>>,

    // ===== 按报告类型 =====

    /// 按报告类型 (Official / Calculate) 的 token 分布
    pub by_report_class: HashMap<ReportClass, TokenInfo>,
}

impl ArchiveDigest {
    /// 创建空的快照块
    pub fn empty(granularity: DigestGranularity, time_key: &str) -> Self {
        Self {
            granularity,
            time_key: time_key.to_string(),
            built_at: Utc::now(),
            source_checksum: 0,
            total_tokens: TokenInfo::default(),
            total_cost: 0.0,
            record_count: 0,
            by_source: HashMap::new(),
            source_costs: HashMap::new(),
            source_records: HashMap::new(),
            by_model: HashMap::new(),
            model_costs: HashMap::new(),
            model_records: HashMap::new(),
            by_project: HashMap::new(),
            project_costs: HashMap::new(),
            project_records: HashMap::new(),
            project_names: HashMap::new(),
            project_sources: HashMap::new(),
            model_sources: HashMap::new(),
            by_day_stats: BTreeMap::new(),
            by_day_source: BTreeMap::new(),
            by_hour: None,
            by_hour_source: None,
            by_report_class: HashMap::new(),
        }
    }

    /// 从原始 Datalog 记录构建日快照块
    ///
    /// 使用提供的 `cost_fn` 为每条记录计算费用。
    /// `cost_fn` 签名: `fn(model: &str, token_info: &TokenInfo) -> f64`
    pub fn build_daily<F>(date_key: &str, logs: &[Datalog], cost_fn: F) -> Self
    where
        F: Fn(&str, &TokenInfo) -> f64,
    {
        let mut digest = Self::empty(DigestGranularity::Daily, date_key);
        digest.by_hour = Some(BTreeMap::new());
        digest.by_hour_source = Some(BTreeMap::new());
        digest.source_checksum = compute_checksum(logs);

        for log in logs {
            let cost = cost_fn(&log.source_model, &log.token_info);

            // KPI
            digest.total_tokens.accumulate(&log.token_info);
            digest.total_cost += cost;
            digest.record_count += 1;

            // 按来源
            digest
                .by_source
                .entry(log.source_name)
                .or_default()
                .accumulate(&log.token_info);
            *digest
                .source_costs
                .entry(log.source_name)
                .or_insert(0.0) += cost;
            *digest
                .source_records
                .entry(log.source_name)
                .or_insert(0) += 1;

            // 按模型
            digest
                .by_model
                .entry(log.source_model.clone())
                .or_default()
                .accumulate(&log.token_info);
            *digest
                .model_costs
                .entry(log.source_model.clone())
                .or_insert(0.0) += cost;
            *digest
                .model_records
                .entry(log.source_model.clone())
                .or_insert(0) += 1;

            // 按项目
            digest
                .by_project
                .entry(log.source_project.clone())
                .or_default()
                .accumulate(&log.token_info);
            *digest
                .project_costs
                .entry(log.source_project.clone())
                .or_insert(0.0) += cost;
            *digest
                .project_records
                .entry(log.source_project.clone())
                .or_insert(0) += 1;

            // 来源映射
            digest
                .project_sources
                .entry(log.source_project.clone())
                .or_default()
                .insert(log.source_name);
            digest
                .model_sources
                .entry(log.source_model.clone())
                .or_default()
                .insert(log.source_name);

            // 按天统计 (Daily digest 只有一天)
            let day_key = log.date_key();
            let day_stats = digest
                .by_day_stats
                .entry(day_key.clone())
                .or_default();
            day_stats.token_info.accumulate(&log.token_info);
            day_stats.record_count += 1;
            day_stats.cost_usd += cost;
            day_stats.message_count += 1;

            // 按天×来源
            let day_source_stats = digest
                .by_day_source
                .entry(day_key)
                .or_default()
                .entry(log.source_name)
                .or_default();
            day_source_stats.token_info.accumulate(&log.token_info);
            day_source_stats.record_count += 1;
            day_source_stats.cost_usd += cost;
            day_source_stats.message_count += 1;

            // 按小时
            let hour_key = log.hour_key();
            if let Some(ref mut by_hour) = digest.by_hour {
                by_hour
                    .entry(hour_key.clone())
                    .or_default()
                    .accumulate(&log.token_info);
            }
            if let Some(ref mut by_hour_source) = digest.by_hour_source {
                by_hour_source
                    .entry(hour_key)
                    .or_default()
                    .entry(log.source_name)
                    .or_default()
                    .accumulate(&log.token_info);
            }

            // 按报告类型
            digest
                .by_report_class
                .entry(log.source_report_class)
                .or_default()
                .accumulate(&log.token_info);
        }

        digest.built_at = Utc::now();
        digest
    }

    /// 合并多个日快照块为月快照块
    ///
    /// 月快照块不保留小时级明细 (`by_hour` = None)。
    pub fn merge_to_monthly(month_key: &str, daily_digests: &[ArchiveDigest]) -> Self {
        let mut merged = Self::empty(DigestGranularity::Monthly, month_key);

        for digest in daily_digests {
            merged.accumulate(digest);
        }

        // 月度快照不需要小时粒度
        merged.by_hour = None;
        merged.by_hour_source = None;
        merged.built_at = Utc::now();
        merged
    }

    /// 累加另一个 Digest 的数据到自身
    ///
    /// 所有标量字段相加，HashMap/BTreeMap 按 key 合并。
    pub fn accumulate(&mut self, other: &ArchiveDigest) {
        // KPI
        self.total_tokens.accumulate(&other.total_tokens);
        self.total_cost += other.total_cost;
        self.record_count += other.record_count;

        // 按来源
        for (k, v) in &other.by_source {
            self.by_source.entry(*k).or_default().accumulate(v);
        }
        for (k, v) in &other.source_costs {
            *self.source_costs.entry(*k).or_insert(0.0) += v;
        }
        for (k, v) in &other.source_records {
            *self.source_records.entry(*k).or_insert(0) += v;
        }

        // 按模型
        for (k, v) in &other.by_model {
            self.by_model.entry(k.clone()).or_default().accumulate(v);
        }
        for (k, v) in &other.model_costs {
            *self.model_costs.entry(k.clone()).or_insert(0.0) += v;
        }
        for (k, v) in &other.model_records {
            *self.model_records.entry(k.clone()).or_insert(0) += v;
        }

        // 按项目
        for (k, v) in &other.by_project {
            self.by_project.entry(k.clone()).or_default().accumulate(v);
        }
        for (k, v) in &other.project_costs {
            *self.project_costs.entry(k.clone()).or_insert(0.0) += v;
        }
        for (k, v) in &other.project_records {
            *self.project_records.entry(k.clone()).or_insert(0) += v;
        }
        for (k, v) in &other.project_names {
            self.project_names.entry(k.clone()).or_insert_with(|| v.clone());
        }

        // 来源映射
        for (k, v) in &other.project_sources {
            self.project_sources
                .entry(k.clone())
                .or_default()
                .extend(v.iter().copied());
        }
        for (k, v) in &other.model_sources {
            self.model_sources
                .entry(k.clone())
                .or_default()
                .extend(v.iter().copied());
        }

        // 按天统计
        for (k, v) in &other.by_day_stats {
            let entry = self.by_day_stats.entry(k.clone()).or_default();
            entry.token_info.accumulate(&v.token_info);
            entry.record_count += v.record_count;
            entry.cost_usd += v.cost_usd;
            entry.message_count += v.message_count;
        }

        // 按天×来源
        for (day_key, source_map) in &other.by_day_source {
            let day_map = self.by_day_source.entry(day_key.clone()).or_default();
            for (source, stats) in source_map {
                let entry = day_map.entry(*source).or_default();
                entry.token_info.accumulate(&stats.token_info);
                entry.record_count += stats.record_count;
                entry.cost_usd += stats.cost_usd;
                entry.message_count += stats.message_count;
            }
        }

        // 按小时 (仅当 self 和 other 都有时合并)
        if let (Some(self_by_hour), Some(other_by_hour)) =
            (&mut self.by_hour, &other.by_hour)
        {
            for (k, v) in other_by_hour {
                self_by_hour.entry(k.clone()).or_default().accumulate(v);
            }
        }
        if let (Some(self_by_hour_source), Some(other_by_hour_source)) =
            (&mut self.by_hour_source, &other.by_hour_source)
        {
            for (hour_key, source_map) in other_by_hour_source {
                let hour_map = self_by_hour_source.entry(hour_key.clone()).or_default();
                for (source, info) in source_map {
                    hour_map.entry(*source).or_default().accumulate(info);
                }
            }
        }

        // 按报告类型
        for (k, v) in &other.by_report_class {
            self.by_report_class.entry(*k).or_default().accumulate(v);
        }
    }

    /// 将快照块序列化并保存到文件
    pub fn save(&self, path: &Path) -> Result<(), std::io::Error> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("序列化失败: {e}"))
        })?;
        std::fs::write(path, json)
    }

    /// 从文件加载快照块
    pub fn load(path: &Path) -> Result<Self, std::io::Error> {
        let content = std::fs::read_to_string(path)?;
        serde_json::from_str(&content).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("反序列化失败: {e}"),
            )
        })
    }
}

/// 计算一组 Datalog 记录的 checksum
///
/// 用于验证快照块是否与原始数据一致。
/// 使用简单的 hash 策略：累加所有记录的关键字段哈希值。
pub fn compute_checksum(logs: &[Datalog]) -> u64 {
    let mut hasher = std::hash::DefaultHasher::new();
    hasher.write_usize(logs.len());
    for log in logs {
        hasher.write(log.source_project.as_bytes());
        hasher.write(log.source_model.as_bytes());
        hasher.write_u64(log.token_info.input);
        hasher.write_u64(log.token_info.output);
        hasher.write_u64(log.token_info.cache);
        hasher.write_u64(log.token_info.reasoning);
        hasher.write_i64(log.source_datetime.timestamp());
    }
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datalog::{ReportClass, SourceName};
    use std::time::Duration;

    fn make_log_at(source: SourceName, model: &str, project: &str, input: u64, output: u64, dt: DateTime<Utc>) -> Datalog {
        Datalog {
            source_name: source,
            collected_at: dt,
            source_api_key: None,
            source_project: project.to_string(),
            source_model: model.to_string(),
            source_datetime: dt,
            source_through_time: Duration::from_secs(0),
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

    fn make_log(source: SourceName, model: &str, project: &str, input: u64, output: u64) -> Datalog {
        make_log_at(source, model, project, input, output, Utc::now())
    }

    fn parse_dt(s: &str) -> DateTime<Utc> {
        chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")
            .unwrap()
            .and_utc()
    }

    #[test]
    fn test_build_daily_basic() {
        let logs = vec![
            make_log(SourceName::Antigravity, "gemini-2.5-pro", "proj-a", 100, 50),
            make_log(SourceName::Codex, "o3-pro", "proj-b", 200, 100),
        ];

        let digest = ArchiveDigest::build_daily("2026-06-10", &logs, |_model, info| {
            (info.input + info.output) as f64 * 0.001
        });

        assert_eq!(digest.granularity, DigestGranularity::Daily);
        assert_eq!(digest.time_key, "2026-06-10");
        assert_eq!(digest.record_count, 2);
        assert_eq!(digest.total_tokens.input, 300);
        assert_eq!(digest.total_tokens.output, 150);
        assert!((digest.total_cost - 0.45).abs() < 1e-10);

        // 按来源
        assert_eq!(digest.by_source.len(), 2);
        assert_eq!(digest.by_source[&SourceName::Antigravity].input, 100);
        assert_eq!(digest.by_source[&SourceName::Codex].input, 200);

        // 按模型
        assert_eq!(digest.by_model.len(), 2);
        assert_eq!(digest.by_model["gemini-2.5-pro"].input, 100);

        // 按项目
        assert_eq!(digest.by_project.len(), 2);

        // 小时数据应存在 (Daily digest)
        assert!(digest.by_hour.is_some());
        assert!(digest.by_hour_source.is_some());
    }

    #[test]
    fn test_merge_to_monthly() {
        let dt1 = parse_dt("2026-06-01T10:00:00");
        let dt2 = parse_dt("2026-06-02T14:00:00");
        let dt3 = parse_dt("2026-06-02T16:00:00");

        let logs_day1 = vec![
            make_log_at(SourceName::Antigravity, "gemini-2.5-pro", "proj-a", 100, 50, dt1),
        ];
        let logs_day2 = vec![
            make_log_at(SourceName::Codex, "o3-pro", "proj-b", 200, 100, dt2),
            make_log_at(SourceName::Antigravity, "gemini-2.5-pro", "proj-a", 50, 25, dt3),
        ];

        let cost_fn = |_model: &str, info: &TokenInfo| (info.input + info.output) as f64 * 0.001;
        let digest1 = ArchiveDigest::build_daily("2026-06-01", &logs_day1, cost_fn);
        let digest2 = ArchiveDigest::build_daily("2026-06-02", &logs_day2, cost_fn);

        let monthly = ArchiveDigest::merge_to_monthly("2026-06", &[digest1, digest2]);

        assert_eq!(monthly.granularity, DigestGranularity::Monthly);
        assert_eq!(monthly.time_key, "2026-06");
        assert_eq!(monthly.record_count, 3);
        assert_eq!(monthly.total_tokens.input, 350);
        assert_eq!(monthly.total_tokens.output, 175);

        // 月度快照不含小时明细
        assert!(monthly.by_hour.is_none());
        assert!(monthly.by_hour_source.is_none());

        // 按天统计应有两天
        assert_eq!(monthly.by_day_stats.len(), 2);

        // 来源合并
        assert_eq!(monthly.by_source.len(), 2);
        assert_eq!(monthly.by_source[&SourceName::Antigravity].input, 150);
    }

    #[test]
    fn test_accumulate_idempotent() {
        let logs = vec![
            make_log(SourceName::Antigravity, "model-a", "proj", 100, 50),
        ];
        let cost_fn = |_: &str, _: &TokenInfo| 1.0;
        let digest = ArchiveDigest::build_daily("2026-01-01", &logs, cost_fn);

        let mut merged = ArchiveDigest::empty(DigestGranularity::Monthly, "2026-01");
        merged.accumulate(&digest);
        merged.accumulate(&digest);

        assert_eq!(merged.record_count, 2);
        assert_eq!(merged.total_tokens.input, 200);
        assert!((merged.total_cost - 2.0).abs() < 1e-10);
    }

    #[test]
    fn test_serialization_roundtrip() {
        let logs = vec![
            make_log(SourceName::Antigravity, "gemini-2.5-pro", "proj-a", 100, 50),
        ];
        let digest = ArchiveDigest::build_daily("2026-06-10", &logs, |_, _| 0.5);

        let json = serde_json::to_string(&digest).unwrap();
        let restored: ArchiveDigest = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.time_key, "2026-06-10");
        assert_eq!(restored.record_count, 1);
        assert_eq!(restored.total_tokens.input, 100);
        assert!((restored.total_cost - 0.5).abs() < 1e-10);
    }

    #[test]
    fn test_save_and_load() {
        let dir = std::env::temp_dir().join("tp_digest_test");
        let _ = std::fs::remove_dir_all(&dir);

        let logs = vec![
            make_log(SourceName::Antigravity, "model-a", "proj", 100, 50),
        ];
        let digest = ArchiveDigest::build_daily("2026-06-10", &logs, |_, _| 1.0);

        let path = dir.join("2026-06-10.digest.json");
        digest.save(&path).unwrap();
        assert!(path.exists());

        let loaded = ArchiveDigest::load(&path).unwrap();
        assert_eq!(loaded.time_key, "2026-06-10");
        assert_eq!(loaded.record_count, 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_checksum_stability() {
        let logs = vec![
            make_log(SourceName::Antigravity, "model-a", "proj", 100, 50),
        ];
        let c1 = compute_checksum(&logs);
        let c2 = compute_checksum(&logs);
        assert_eq!(c1, c2, "checksum should be deterministic");
    }

    #[test]
    fn test_checksum_sensitivity() {
        let logs1 = vec![
            make_log(SourceName::Antigravity, "model-a", "proj", 100, 50),
        ];
        let logs2 = vec![
            make_log(SourceName::Antigravity, "model-a", "proj", 101, 50),
        ];
        let c1 = compute_checksum(&logs1);
        let c2 = compute_checksum(&logs2);
        assert_ne!(c1, c2, "different data should produce different checksums");
    }

    #[test]
    fn test_empty_digest() {
        let digest = ArchiveDigest::empty(DigestGranularity::Daily, "2026-01-01");
        assert_eq!(digest.record_count, 0);
        assert!(digest.total_tokens.is_zero());
        assert!(digest.by_source.is_empty());
    }

    #[test]
    fn test_build_daily_empty_logs() {
        let digest = ArchiveDigest::build_daily("2026-01-01", &[], |_, _| 0.0);
        assert_eq!(digest.record_count, 0);
        assert!(digest.total_tokens.is_zero());
        assert!(digest.by_hour.unwrap().is_empty());
    }
}
