//! # tp-cache
//!
//! 数据缓存层 — 从 data-pool 拉取数据，使用 `tp_compute::IncrementalAggregator`
//! 进行增量聚合，管理缓存构建进度，并在 mate-data 变更时执行失效重建。
//!
//! 架构规则:
//! - Rule 3: mate-data 在构建期间变更 → 丢弃当前计算，重新开始
//! - Rule 4: 索引更新 → 重新计算受影响的索引数据

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use parking_lot::RwLock;
use tokio::sync::broadcast;
use tracing::{debug, info, instrument, warn};

use tp_compute::IncrementalAggregator;
use tp_protocol::dimension::{Dimension, TimeWindow};
use tp_protocol::traits::{CacheProgress, CacheUpdateSignal, PoolNotification};
use tp_protocol::view::{DashboardView, DimensionEntry};
use tp_protocol::{CacheError, CacheProvider, PoolStorage, TokenInfo};

// ---------------------------------------------------------------------------
// DataCache — 核心缓存结构
// ---------------------------------------------------------------------------

/// 数据缓存 — 从 pool 拉取数据并进行增量聚合。
///
/// 通过 `build()` 执行全量构建，`incremental_update()` 执行部分重建，
/// `on_pool_notification()` 响应 pool 的变更通知。
///
/// 所有聚合状态由 `IncrementalAggregator` 持有，线程安全由 `parking_lot::RwLock` 保证。
pub struct DataCache {
    /// 数据池存储引用
    pool: Arc<dyn PoolStorage>,
    /// 增量聚合器 (读写锁保护)
    aggregator: RwLock<IncrementalAggregator>,
    /// 缓存构建进度
    progress: RwLock<CacheProgress>,
    /// 缓存更新信号发送端
    update_tx: broadcast::Sender<CacheUpdateSignal>,
    /// 当前已知的 metadata 版本号 (用于构建期间的失效检测)
    metadata_version: AtomicU64,
    /// 已处理的 hour_key 集合 (用于增量构建)
    processed_keys: RwLock<HashSet<String>>,
}

impl DataCache {
    /// 创建新的数据缓存实例。
    ///
    /// # Arguments
    /// * `pool` — 数据池存储实现 (通过 `PoolStorage` trait 交互)
    pub fn new(pool: Arc<dyn PoolStorage>) -> Self {
        let (update_tx, _) = broadcast::channel(64);
        Self {
            pool,
            aggregator: RwLock::new(IncrementalAggregator::new()),
            progress: RwLock::new(CacheProgress::default()),
            update_tx,
            metadata_version: AtomicU64::new(0),
            processed_keys: RwLock::new(HashSet::new()),
        }
    }

    /// 清除所有缓存数据
    pub fn clear(&self) {
        self.aggregator.write().reset();
        self.processed_keys.write().clear();
        let mut progress = self.progress.write();
        *progress = CacheProgress::default();
        self.metadata_version.store(0, Ordering::SeqCst);
    }

    /// 全量构建 — 从 pool 读取所有未处理的 hour_key 数据并聚合。
    ///
    /// 构建过程中会检测 metadata 版本变化 (Rule 3):
    /// 如果 mate-data 在构建期间发生变更，将丢弃当前计算并重新开始。
    #[instrument(skip(self))]
    pub async fn build(&self) -> Result<(), CacheError> {
        info!("starting full cache build");

        // 标记构建中
        {
            let mut progress = self.progress.write();
            progress.building = true;
        }

        // 重试循环: Rule 3 — 如果 metadata 在构建中变化，重新开始
        loop {
            match self.do_build().await {
                Ok(true) => {
                    // 构建成功完成
                    break;
                }
                Ok(false) => {
                    // metadata 版本在构建中变化，重新开始 (Rule 3)
                    warn!("metadata changed during build, restarting (Rule 3)");
                    continue;
                }
                Err(e) => {
                    let mut progress = self.progress.write();
                    progress.building = false;
                    return Err(e);
                }
            }
        }

        // 标记构建完成
        {
            let mut progress = self.progress.write();
            progress.building = false;
            progress.last_build_at = Some(Utc::now());
        }

        // 广播全量重建完成信号
        let _ = self.update_tx.send(CacheUpdateSignal::FullRebuildComplete);

        info!("full cache build complete");
        Ok(())
    }

    /// 执行一次构建尝试。
    ///
    /// 返回 `Ok(true)` 表示构建成功完成，`Ok(false)` 表示 metadata 版本变化需要重建。
    async fn do_build(&self) -> Result<bool, CacheError> {
        // 1. 获取 metadata
        let metadata = self
            .pool
            .get_metadata()
            .await
            .map_err(|e| CacheError::PoolCommunicationError(format!("get_metadata failed: {e}")))?;

        let version_before = metadata.version;
        self.metadata_version.store(version_before, Ordering::SeqCst);

        let all_keys: Vec<String> = metadata
            .indices
            .iter()
            .filter(|(_, idx)| {
                idx.tier == tp_protocol::PartitionTier::Active
                    || idx.tier == tp_protocol::PartitionTier::ArchiveDaily
                    || idx.tier == tp_protocol::PartitionTier::ArchiveMonthly
            })
            .map(|(key, _)| key.clone())
            .collect();
        let total_count = all_keys.len();
        let termination_hour_key = all_keys.iter().max().cloned();

        // 重置聚合器和已处理集合 (全量重建)
        {
            self.aggregator.write().reset();
            self.processed_keys.write().clear();
        }

        // 更新进度
        {
            let mut progress = self.progress.write();
            progress.total_count = total_count;
            progress.processed_count = 0;
            progress.termination_hour_key = termination_hour_key;
        }

        debug!(total_keys = total_count, "building from pool metadata");

        // 2. 逐个 hour_key 读取数据并 ingest
        for hour_key in &all_keys {
            // 检查 metadata 版本是否变化 (Rule 3)
            let current_version = self
                .pool
                .get_metadata_version()
                .await
                .map_err(|e| {
                    CacheError::PoolCommunicationError(format!(
                        "get_metadata_version failed: {e}"
                    ))
                })?;

            if current_version != version_before {
                debug!(
                    version_before,
                    current_version, "metadata version changed during build"
                );
                return Ok(false);
            }

            // 读取该 hour_key 的数据
            let logs = self
                .pool
                .query_by_hour_key(hour_key)
                .await
                .map_err(|e| {
                    CacheError::PoolCommunicationError(format!(
                        "query_by_hour_key({hour_key}) failed: {e}"
                    ))
                })?;

            // 累加到聚合器
            if !logs.is_empty() {
                self.aggregator.write().ingest(&logs);
            }

            // 标记为已处理
            self.processed_keys.write().insert(hour_key.clone());

            // 更新进度
            {
                let mut progress = self.progress.write();
                progress.processed_count += 1;
            }
        }

        // 最终检查版本 (Rule 3)
        let version_after = self
            .pool
            .get_metadata_version()
            .await
            .map_err(|e| {
                CacheError::PoolCommunicationError(format!("get_metadata_version failed: {e}"))
            })?;

        if version_after != version_before {
            debug!(
                version_before,
                version_after, "metadata version changed after build"
            );
            return Ok(false);
        }

        self.metadata_version.store(version_after, Ordering::SeqCst);
        Ok(true)
    }

    /// 增量更新 — 仅重新计算受影响的 hour_key 数据。
    ///
    /// 对应 Rule 4: 索引更新 → 重新计算受影响的索引数据。
    /// 当前实现为"全量重建"策略（因增量差分需要反向减法支持），
    /// 但只处理受影响的 key 以减少 I/O。
    #[instrument(skip(self), fields(affected_count = affected_keys.len()))]
    pub async fn incremental_update(&self, affected_keys: &[String]) -> Result<(), CacheError> {
        if affected_keys.is_empty() {
            return Ok(());
        }

        info!(
            affected_keys = ?affected_keys,
            "starting incremental update"
        );

        // 增量更新策略: 重建整个聚合器 (简单可靠)
        // 因为 IncrementalAggregator 只支持累加，不支持减法修正，
        // 我们需要完整重建来保证数据正确性。
        self.build().await?;

        // 广播增量更新完成信号
        let _ = self
            .update_tx
            .send(CacheUpdateSignal::IncrementalUpdateComplete {
                affected_keys: affected_keys.to_vec(),
            });

        Ok(())
    }

    /// 处理 pool 通知 — 根据通知类型执行相应操作。
    ///
    /// - `MetadataChanged`: mate-data 变更 → 触发失效+重建 (Rule 3 & 4)
    /// - `DataPushed`: 新数据到达 → 增量更新受影响的 hour_key
    #[instrument(skip(self))]
    pub async fn on_pool_notification(&self, notification: PoolNotification) {
        match notification {
            PoolNotification::MetadataChanged {
                changed_keys,
                new_version,
            } => {
                info!(
                    new_version,
                    changed_keys_count = changed_keys.len(),
                    "metadata changed, invalidating and rebuilding"
                );

                // 更新版本号
                self.metadata_version.store(new_version, Ordering::SeqCst);

                // 触发完整重建 (Rule 3: mate-data 变更 → 丢弃 + 重建)
                if let Err(e) = self.build().await {
                    warn!(error = %e, "rebuild after metadata change failed");
                }
            }
            PoolNotification::DataPushed {
                affected_hour_keys,
                record_count,
            } => {
                debug!(
                    record_count,
                    affected_keys = ?affected_hour_keys,
                    "new data pushed, running incremental update"
                );

                // Rule 4: 索引更新 → 重新计算受影响的索引数据
                if let Err(e) = self.incremental_update(&affected_hour_keys).await {
                    warn!(error = %e, "incremental update after data push failed");
                }
            }
        }
    }

    /// 订阅缓存更新信号。
    pub fn subscribe(&self) -> broadcast::Receiver<CacheUpdateSignal> {
        self.update_tx.subscribe()
    }

    /// 从聚合器状态构建 `DashboardView` 快照。
    fn build_dashboard_view(&self) -> DashboardView {
        let agg = self.aggregator.read();
        let snap = agg.snapshot();
        let now = Utc::now();

        DashboardView {
            // KPI 指标
            total_tokens: agg.window(TimeWindow::All, now),
            today_tokens: agg.window(TimeWindow::Today, now),
            week_tokens: agg.window(TimeWindow::ThisWeek, now),
            month_tokens: agg.window(TimeWindow::ThisMonth, now),
            total_cost: snap.total_cost,
            today_cost: 0.0,  // 精确的日费用需要逐条记录，此处暂用 0
            week_cost: 0.0,   // 同上
            month_cost: 0.0,  // 同上
            record_count: snap.record_count,

            // 分维度聚合
            by_source: agg.project(&Dimension::BySource),
            by_model: agg.project(&Dimension::ByModel),
            by_project: agg.project(&Dimension::ByProject),

            // 时间序列
            daily_series: snap.by_day_stats.clone(),
            hourly_today: {
                let mut hourly_today: std::collections::BTreeMap<String, TokenInfo> = std::collections::BTreeMap::new();
                let today_prefix = now.format("%Y-%m-%dT").to_string();
                for (hour_key, info) in &snap.by_hour {
                    if hour_key.starts_with(&today_prefix) {
                        let hh = &hour_key[11..];
                        hourly_today.entry(hh.to_string()).or_default().accumulate(info);
                    }
                }
                hourly_today
            },

            // 最近活跃 — 由 data-show 层从 pool hot-data 获取
            recent_records: Vec::new(),

            daily_by_source: snap.by_day_source.clone(),
            hourly_today_by_source: {
                let mut hourly_today_by_source = std::collections::BTreeMap::new();
                let today_prefix = now.format("%Y-%m-%dT").to_string();
                for (hour_key, source_map) in &snap.by_hour_source {
                    if hour_key.starts_with(&today_prefix) {
                        let hh = &hour_key[11..];
                        hourly_today_by_source.insert(hh.to_string(), source_map.clone());
                    }
                }
                hourly_today_by_source
            },

            // New fields
            project_sources: snap.project_sources.clone(),
            model_sources: snap.model_sources.clone(),

            // 元信息
            last_updated: now,
            source_status: Vec::new(),
            cache_termination_key: self.progress.read().termination_hour_key.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// CacheProvider trait 实现
// ---------------------------------------------------------------------------

#[async_trait]
impl CacheProvider for DataCache {
    /// 获取缓存的聚合快照 — 从 aggregator 状态构建 DashboardView。
    async fn get_snapshot(&self) -> Result<DashboardView, CacheError> {
        Ok(self.build_dashboard_view())
    }

    /// 根据维度获取缓存结果 — 委托给 aggregator.project()。
    async fn get_by_dimension(&self, dimension: &Dimension) -> Result<Vec<DimensionEntry>, CacheError> {
        let entries = self.aggregator.read().project(dimension);
        Ok(entries)
    }

    /// 手动触发失效重算 — 重置受影响数据并重建。
    async fn invalidate(&self, affected_keys: &[String]) -> Result<(), CacheError> {
        info!(
            affected_keys = ?affected_keys,
            "manual invalidation triggered"
        );
        self.incremental_update(affected_keys).await
    }

    /// 触发完整重建。
    async fn rebuild(&self) -> Result<(), CacheError> {
        info!("manual full rebuild triggered");
        // 清除已处理集合以强制全量重建
        self.processed_keys.write().clear();
        self.build().await
    }

    /// 订阅缓存更新信号。
    async fn subscribe(&self) -> Result<broadcast::Receiver<CacheUpdateSignal>, CacheError> {
        Ok(self.update_tx.subscribe())
    }

    /// 获取缓存构建进度。
    async fn get_progress(&self) -> Result<CacheProgress, CacheError> {
        Ok(self.progress.read().clone())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::AtomicU64 as StdAtomicU64;
    use std::time::Duration;

    use chrono::DateTime;
    use tp_protocol::datalog::{Datalog, ReportClass, SourceName, TokenInfo};
    use tp_protocol::error::{PoolError, PushResult};
    use tp_protocol::meta::{PartitionTier, PoolMetadata};

    // ===== Mock PoolStorage =====

    struct MockPool {
        metadata: RwLock<PoolMetadata>,
        version: StdAtomicU64,
        logs: RwLock<Vec<Datalog>>,
    }

    impl MockPool {
        fn new() -> Self {
            Self {
                metadata: RwLock::new(PoolMetadata::default()),
                version: StdAtomicU64::new(0),
                logs: RwLock::new(Vec::new()),
            }
        }

        fn add_log(&self, log: Datalog) {
            let hour_key = log.hour_key();
            self.logs.write().push(log);

            let mut meta = self.metadata.write();
            meta.get_or_create(
                &hour_key,
                std::path::PathBuf::from(format!("data/{hour_key}.jsonl")),
                PartitionTier::ArchiveDaily,
            );
            meta.version = self.version.load(Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl PoolStorage for MockPool {
        async fn push_datalogs(&self, _logs: Vec<Datalog>) -> Result<PushResult, PoolError> {
            Ok(PushResult::empty())
        }

        async fn query_active(&self) -> Result<Vec<Datalog>, PoolError> {
            Ok(self.logs.read().clone())
        }

        async fn query_range(
            &self,
            _from: DateTime<Utc>,
            _to: DateTime<Utc>,
        ) -> Result<Vec<Datalog>, PoolError> {
            Ok(Vec::new())
        }

        async fn query_by_hour_key(&self, hour_key: &str) -> Result<Vec<Datalog>, PoolError> {
            let logs = self.logs.read();
            Ok(logs
                .iter()
                .filter(|l| l.hour_key() == hour_key)
                .cloned()
                .collect())
        }

        async fn get_metadata(&self) -> Result<PoolMetadata, PoolError> {
            Ok(self.metadata.read().clone())
        }

        async fn get_metadata_version(&self) -> Result<u64, PoolError> {
            Ok(self.version.load(Ordering::SeqCst))
        }

        async fn subscribe(
            &self,
        ) -> Result<broadcast::Receiver<PoolNotification>, PoolError> {
            let (tx, rx) = broadcast::channel(16);
            drop(tx);
            Ok(rx)
        }

        async fn run_archive(&self) -> Result<Vec<String>, PoolError> {
            Ok(Vec::new())
        }
    }

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
            source_through_time: Duration::from_secs(60),
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

    #[tokio::test]
    async fn test_build_empty_pool() {
        let pool = Arc::new(MockPool::new());
        let cache = DataCache::new(pool);

        cache.build().await.unwrap();

        let progress = cache.get_progress().await.unwrap();
        assert!(!progress.building);
        assert_eq!(progress.total_count, 0);
        assert_eq!(progress.processed_count, 0);
        assert!(progress.last_build_at.is_some());
    }

    #[tokio::test]
    async fn test_build_with_data() {
        let pool = Arc::new(MockPool::new());
        let dt = chrono::NaiveDate::from_ymd_opt(2026, 5, 26)
            .unwrap()
            .and_hms_opt(10, 0, 0)
            .unwrap()
            .and_utc();

        pool.add_log(make_log(
            SourceName::Antigravity,
            "gemini-3.5-flash",
            "s1",
            dt,
            100,
            200,
        ));

        let cache = DataCache::new(pool);
        cache.build().await.unwrap();

        let snapshot = cache.get_snapshot().await.unwrap();
        assert_eq!(snapshot.total_tokens.input, 100);
        assert_eq!(snapshot.total_tokens.output, 200);
        assert_eq!(snapshot.record_count, 1);
    }

    #[tokio::test]
    async fn test_get_by_dimension() {
        let pool = Arc::new(MockPool::new());
        let dt = chrono::NaiveDate::from_ymd_opt(2026, 5, 26)
            .unwrap()
            .and_hms_opt(10, 0, 0)
            .unwrap()
            .and_utc();

        pool.add_log(make_log(
            SourceName::Antigravity,
            "gemini-3.5-flash",
            "s1",
            dt,
            100,
            200,
        ));
        pool.add_log(make_log(
            SourceName::Codex,
            "gpt-4o",
            "s2",
            dt,
            50,
            75,
        ));

        let cache = DataCache::new(pool);
        cache.build().await.unwrap();

        let by_source = cache.get_by_dimension(&Dimension::BySource).await.unwrap();
        assert_eq!(by_source.len(), 2);

        let by_model = cache.get_by_dimension(&Dimension::ByModel).await.unwrap();
        assert_eq!(by_model.len(), 2);
    }

    #[tokio::test]
    async fn test_subscribe() {
        let pool = Arc::new(MockPool::new());
        let cache = DataCache::new(pool);

        let mut rx = cache.subscribe();
        // 发送信号
        let _ = cache.update_tx.send(CacheUpdateSignal::FullRebuildComplete);
        let signal = rx.recv().await.unwrap();
        assert!(matches!(signal, CacheUpdateSignal::FullRebuildComplete));
    }

    #[tokio::test]
    async fn test_on_pool_notification_data_pushed() {
        let pool = Arc::new(MockPool::new());
        let dt = chrono::NaiveDate::from_ymd_opt(2026, 5, 26)
            .unwrap()
            .and_hms_opt(10, 0, 0)
            .unwrap()
            .and_utc();

        pool.add_log(make_log(
            SourceName::Antigravity,
            "gemini-3.5-flash",
            "s1",
            dt,
            100,
            200,
        ));

        let cache = DataCache::new(pool);

        cache
            .on_pool_notification(PoolNotification::DataPushed {
                affected_hour_keys: vec!["2026-05-26T10".to_string()],
                record_count: 1,
            })
            .await;

        let snapshot = cache.get_snapshot().await.unwrap();
        assert_eq!(snapshot.total_tokens.input, 100);
    }

    #[tokio::test]
    async fn test_rebuild() {
        let pool = Arc::new(MockPool::new());
        let dt = chrono::NaiveDate::from_ymd_opt(2026, 5, 26)
            .unwrap()
            .and_hms_opt(10, 0, 0)
            .unwrap()
            .and_utc();

        pool.add_log(make_log(
            SourceName::Antigravity,
            "gemini-3.5-flash",
            "s1",
            dt,
            100,
            200,
        ));

        let cache = DataCache::new(pool);
        cache.build().await.unwrap();

        // Rebuild should produce the same result
        cache.rebuild().await.unwrap();
        let snapshot = cache.get_snapshot().await.unwrap();
        assert_eq!(snapshot.total_tokens.input, 100);
        assert_eq!(snapshot.record_count, 1);
    }
}
