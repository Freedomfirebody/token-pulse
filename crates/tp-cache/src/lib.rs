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
use chrono::{Datelike, Utc};
use parking_lot::RwLock;
use tokio::sync::{broadcast, watch};
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

        // 修剪过期小时级数据 — 保留最近 48 小时的 by_hour 明细
        {
            let pruned = self.aggregator.write().prune_hourly(48);
            if pruned > 0 {
                info!(pruned, "hourly data pruned after build");
            }
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

        // 收集 digest 可用的 date_key 集合（归档分区有有效 digest）
        let mut digest_dates: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut non_digest_keys: Vec<String> = Vec::new();

        for hour_key in &all_keys {
            if let Some(idx) = metadata.indices.get(hour_key) {
                if idx.has_valid_digest() {
                    // 归档且有有效 digest — 按 date 分组
                    let date_key = hour_key[..10].to_string();
                    digest_dates.insert(date_key);
                } else {
                    non_digest_keys.push(hour_key.clone());
                }
            } else {
                non_digest_keys.push(hour_key.clone());
            }
        }

        debug!(
            digest_dates = digest_dates.len(),
            non_digest = non_digest_keys.len(),
            "build: digest fast-path分析完成"
        );

        // 2a. Digest 快路径 — 按 date 加载预计算快照块
        for date_key in &digest_dates {
            // 尝试从 metadata 中找到该 date 任意一个 hour_key 的 digest_path
            let digest_path = all_keys.iter()
                .filter(|k| k.starts_with(date_key.as_str()))
                .find_map(|k| {
                    metadata.indices.get(k)
                        .and_then(|idx| idx.digest_path.as_ref())
                });

            if let Some(path) = digest_path {
                match tp_protocol::digest::ArchiveDigest::load(path) {
                    Ok(digest) => {
                        self.aggregator.write().merge_digest(&digest);

                        // 标记该 date 下所有 hour_key 为已处理
                        let mut processed = self.processed_keys.write();
                        for k in all_keys.iter().filter(|k| k.starts_with(date_key.as_str())) {
                            processed.insert(k.clone());
                        }

                        let date_key_count = all_keys.iter()
                            .filter(|k| k.starts_with(date_key.as_str()))
                            .count();
                        let mut progress = self.progress.write();
                        progress.processed_count += date_key_count;

                        debug!(date_key, keys = date_key_count, "digest merged (fast-path)");
                        continue;
                    }
                    Err(e) => {
                        // Digest 加载失败 — 降级为逐条模式
                        warn!(date_key, error = %e, "digest load failed, falling back to raw records");
                        for k in all_keys.iter().filter(|k| k.starts_with(date_key.as_str())) {
                            non_digest_keys.push(k.clone());
                        }
                    }
                }
            } else {
                // 没找到 digest path — 降级
                for k in all_keys.iter().filter(|k| k.starts_with(date_key.as_str())) {
                    non_digest_keys.push(k.clone());
                }
            }
        }

        // 2b. 逐个 hour_key 读取数据并 ingest (无 digest 的分区)
        for hour_key in &non_digest_keys {
            // 跳过已通过 digest 处理的 key
            if self.processed_keys.read().contains(hour_key) {
                continue;
            }

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

    /// 增量更新 — 仅读取并累加受影响的 hour_key 数据。
    ///
    /// 对应 Rule 4: 新数据到达 → 只 ingest 新增的 hour_key 记录。
    /// 这对 `DataPushed` 通知是安全的，因为推送的数据尚未被 aggregator 处理。
    ///
    /// 对于 `MetadataChanged`（归档等结构性变更），调用方应使用 `build()` 全量重建。
    #[instrument(skip(self), fields(affected_count = affected_keys.len()))]
    pub async fn incremental_update(&self, affected_keys: &[String]) -> Result<(), CacheError> {
        if affected_keys.is_empty() {
            return Ok(());
        }

        info!(
            affected_keys = ?affected_keys,
            "starting true incremental update"
        );

        let mut ingested_count = 0usize;

        for hour_key in affected_keys {
            // 读取该 hour_key 的全部数据
            let logs = self
                .pool
                .query_by_hour_key(hour_key)
                .await
                .map_err(|e| {
                    CacheError::PoolCommunicationError(format!(
                        "incremental query_by_hour_key({hour_key}) failed: {e}"
                    ))
                })?;

            if !logs.is_empty() {
                // 重建该 key 的策略:
                // 如果该 key 已被处理过，需要全量重建以避免重复计数
                if self.processed_keys.read().contains(hour_key) {
                    debug!(hour_key, "key already processed, triggering full rebuild");
                    self.build().await?;
                    // 广播增量更新完成信号
                    let _ = self
                        .update_tx
                        .send(CacheUpdateSignal::IncrementalUpdateComplete {
                            affected_keys: affected_keys.to_vec(),
                        });
                    return Ok(());
                }

                self.aggregator.write().ingest(&logs);
                self.processed_keys.write().insert(hour_key.clone());
                ingested_count += logs.len();
            }
        }

        info!(
            ingested = ingested_count,
            keys = affected_keys.len(),
            "incremental update complete"
        );

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

    /// 启动自治订阅 — 后台任务自动接收 Pool broadcast 并处理通知。
    ///
    /// 调用此方法后，Cache 不再需要 main.rs 手动调用 `on_pool_notification()`。
    /// Pool 内部的 `push_datalogs()` 完成时自动 broadcast，
    /// 此任务自动接收并处理通知。
    pub fn start_auto_subscribe(
        self: &Arc<Self>,
        pool: Arc<dyn PoolStorage>,
        shutdown: watch::Receiver<bool>,
    ) {
        let cache = Arc::clone(self);
        let mut shutdown = shutdown;

        tokio::spawn(async move {
            let mut rx = match pool.subscribe().await {
                Ok(rx) => rx,
                Err(e) => {
                    warn!("Cache 无法订阅 Pool broadcast: {e}，将依赖手动通知");
                    return;
                }
            };

            info!("Cache 自治订阅 Pool broadcast 已启动");

            loop {
                tokio::select! {
                    notification = rx.recv() => {
                        match notification {
                            Ok(n) => {
                                tracing::debug!(?n, "Cache 收到 Pool 通知");
                                cache.on_pool_notification(n).await;
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                warn!(lagged = n, "Cache 丢失 Pool 通知，触发全量重建");
                                if let Err(e) = cache.build().await {
                                    warn!(error = %e, "Cache 全量重建失败");
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                info!("Pool broadcast 通道关闭，Cache 自治订阅退出");
                                break;
                            }
                        }
                    }
                    res = shutdown.changed() => {
                        match res {
                            Ok(()) if *shutdown.borrow() => {
                                info!("Cache 自治订阅收到退出信号");
                                break;
                            }
                            Ok(()) => continue,
                            Err(_) => {
                                warn!("Cache shutdown channel 关闭");
                                break;
                            }
                        }
                    }
                }
            }
        });
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

        // 计算时间窗口费用 — 从 by_day_stats 中按日期范围求和
        let today_key = now.format("%Y-%m-%d").to_string();
        let today_cost = snap.by_day_stats.get(&today_key)
            .map(|s| s.cost_usd)
            .unwrap_or(0.0);

        let week_start = (now - chrono::Duration::days(now.weekday().num_days_from_monday() as i64))
            .format("%Y-%m-%d")
            .to_string();
        let week_cost: f64 = snap.by_day_stats.range(week_start..)
            .map(|(_, s)| s.cost_usd)
            .sum();

        let month_start = format!("{}-01", now.format("%Y-%m"));
        let month_cost: f64 = snap.by_day_stats.range(month_start..)
            .map(|(_, s)| s.cost_usd)
            .sum();

        DashboardView {
            // KPI 指标
            total_tokens: agg.window(TimeWindow::All, now),
            today_tokens: agg.window(TimeWindow::Today, now),
            week_tokens: agg.window(TimeWindow::ThisWeek, now),
            month_tokens: agg.window(TimeWindow::ThisMonth, now),
            total_cost: snap.total_cost,
            today_cost,
            week_cost,
            month_cost,
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
            memory_warning: None,

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
