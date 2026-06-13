//! # tp-aggregator
//!
//! data-show 层 — 合并冷数据 (cache) 与热数据 (pool) 为统一的 `DashboardView`。
//!
//! 这是面向 UI 的最终数据层，订阅 cache 更新信号并定期刷新，
//! 通过 `tokio::sync::watch` 推送最新视图给 Dash 渲染层。

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{Datelike, Utc};
use parking_lot::RwLock;
use tokio::sync::watch;
use tracing::{debug, error, info, warn};

use tp_compute::IncrementalAggregator;
use tp_protocol::view::{DailyStats, DashboardView, RecentRecord};
use tp_protocol::{
    AggregatorError, CacheProvider, DataShowProvider, PricingTable, PoolStorage,
};
use tp_protocol::dimension::{Dimension, TimeWindow};

// ---------------------------------------------------------------------------
// DataShow — 核心聚合展示器
// ---------------------------------------------------------------------------

/// data-show 层核心结构。
///
/// 持有 data-pool (热数据) 和 data-cache (冷数据) 的引用，
/// 通过 `IncrementalAggregator` 合并计算并产出 `DashboardView`。
pub struct DataShow {
    pool: Arc<dyn PoolStorage>,
    cache: Arc<dyn CacheProvider>,
    aggregator: RwLock<IncrementalAggregator>,
    view_tx: watch::Sender<DashboardView>,
    view_rx: watch::Receiver<DashboardView>,
    pricing: PricingTable,
}

impl DataShow {
    /// 创建新的 DataShow 实例。
    ///
    /// 初始视图为空的 `DashboardView::default()`。
    pub fn new(pool: Arc<dyn PoolStorage>, cache: Arc<dyn CacheProvider>) -> Self {
        let (view_tx, view_rx) = watch::channel(DashboardView::default());
        Self {
            pool,
            cache,
            aggregator: RwLock::new(IncrementalAggregator::new()),
            view_tx,
            view_rx,
            pricing: PricingTable::builtin(),
        }
    }

    /// 执行一次完整的数据刷新。
    ///
    /// 1. 拉取 cache 冷数据快照
    /// 2. 拉取 pool 热数据 (当天活跃)
    /// 3. 合并到新的 `IncrementalAggregator`
    /// 4. 构建 `DashboardView`
    /// 5. 推送到 watch 频道
    pub async fn refresh(&self) -> Result<(), AggregatorError> {
        // ---- Step 1: Pull cold-data from cache ----
        let cached_view = self.cache.get_snapshot().await.map_err(|e| {
            AggregatorError::CacheError(format!("cache get_snapshot failed: {e}"))
        })?;

        // ---- Step 2: Pull hot-data from pool ----
        let hot_data = self.pool.query_active().await.map_err(|e| {
            AggregatorError::PoolError(format!("pool query_active failed: {e}"))
        })?;

        // Filter hot data to keep only records strictly newer than cache boundary
        // to prevent double counting since the cache snap already covers all data up to the termination key.
        let filtered_hot_data = if let Some(ref term_key) = cached_view.cache_termination_key {
            hot_data
                .into_iter()
                .filter(|log| log.hour_key() > *term_key)
                .collect::<Vec<_>>()
        } else {
            hot_data
        };

        debug!(
            hot_records = filtered_hot_data.len(),
            "refresh: pulled cold-data snapshot and filtered hot-data"
        );

        // ---- Step 3: Merge into a fresh aggregator ----
        let mut agg = IncrementalAggregator::new();

        // Feed filtered hot-data records into the aggregator for fresh computation.
        agg.ingest(&filtered_hot_data);

        // Store updated aggregator
        {
            let mut guard = self.aggregator.write();
            *guard = agg;
        }

        // ---- Step 4: Build DashboardView ----
        let view = self.build_view(&cached_view, &filtered_hot_data)?;

        // ---- Step 5: Push to watch channel ----
        // Ignore send error (no receivers is fine)
        let _ = self.view_tx.send(view);

        debug!("refresh: view updated and pushed to watch channel");
        Ok(())
    }

    /// 构建完整的 `DashboardView`，合并 cache (冷) + aggregator (热)。
    fn build_view(
        &self,
        cached_view: &DashboardView,
        hot_data: &[tp_protocol::Datalog],
    ) -> Result<DashboardView, AggregatorError> {
        let now = Utc::now();
        let guard = self.aggregator.read();
        let snap = guard.snapshot();

        // ----- KPI: token totals by time window -----
        // Total = cache totals + hot aggregator totals
        let mut total_tokens = cached_view.total_tokens;
        total_tokens.accumulate(&snap.total_tokens);

        // Today/Week/Month: use hot aggregator's window() for precision,
        // then add cache's respective windows.
        let today_tokens = {
            let mut t = cached_view.today_tokens;
            t.accumulate(&guard.window(TimeWindow::Today, now));
            t
        };
        let week_tokens = {
            let mut t = cached_view.week_tokens;
            t.accumulate(&guard.window(TimeWindow::ThisWeek, now));
            t
        };
        let month_tokens = {
            let mut t = cached_view.month_tokens;
            t.accumulate(&guard.window(TimeWindow::ThisMonth, now));
            t
        };

        // ----- KPI: costs -----
        let total_cost = cached_view.total_cost + snap.total_cost;

        // Compute hot-data windowed costs by filtering records
        let today_str = now.format("%Y-%m-%d").to_string();

        let weekday_offset = now.weekday().num_days_from_monday() as i64;
        let week_start = (now.date_naive() - chrono::Duration::days(weekday_offset))
            .format("%Y-%m-%d")
            .to_string();

        let month_start = now
            .date_naive()
            .with_day(1)
            .expect("day 1 always valid")
            .format("%Y-%m-%d")
            .to_string();

        let mut hot_today_cost = 0.0_f64;
        let mut hot_week_cost = 0.0_f64;
        let mut hot_month_cost = 0.0_f64;
        for log in hot_data {
            let day_key = log.date_key();
            let cost = self.pricing.calculate_cost(&log.source_model, &log.token_info);
            if day_key == today_str {
                hot_today_cost += cost;
            }
            if day_key >= week_start {
                hot_week_cost += cost;
            }
            if day_key >= month_start {
                hot_month_cost += cost;
            }
        }

        let today_cost = cached_view.today_cost + hot_today_cost;
        let week_cost = cached_view.week_cost + hot_week_cost;
        let month_cost = cached_view.month_cost + hot_month_cost;

        let record_count = cached_view.record_count + snap.record_count;

        // ----- Dimension breakdowns -----
        let by_source = merge_dimension_entries(
            &cached_view.by_source,
            &guard.project(&Dimension::BySource),
        );
        let by_model = merge_dimension_entries(
            &cached_view.by_model,
            &guard.project(&Dimension::ByModel),
        );
        let registry = tp_protocol::ConversationRegistry::load_default();

        let raw_by_project = merge_dimension_entries(
            &cached_view.by_project,
            &guard.project(&Dimension::ByProject),
        );
        let by_project = resolve_and_merge_by_project(&raw_by_project, &registry);

        // ----- Daily series -----
        let mut daily_series = cached_view.daily_series.clone();
        for (day_key, token_info) in &snap.by_day {
            let entry = daily_series
                .entry(day_key.clone())
                .or_insert_with(DailyStats::default);
            entry.token_info.accumulate(token_info);
            // Count hot records for this day
            let day_records = hot_data
                .iter()
                .filter(|l| l.date_key() == *day_key)
                .count() as u64;
            entry.record_count += day_records;
            // Sum costs for this day
            let day_cost: f64 = hot_data
                .iter()
                .filter(|l| l.date_key() == *day_key)
                .map(|l| self.pricing.calculate_cost(&l.source_model, &l.token_info))
                .sum();
            entry.cost_usd += day_cost;
        }

        // ----- Hourly today -----
        let mut hourly_today: BTreeMap<String, tp_protocol::TokenInfo> = BTreeMap::new();
        // From cache
        for (hh, info) in &cached_view.hourly_today {
            hourly_today
                .entry(hh.clone())
                .or_default()
                .accumulate(info);
        }
        // From hot aggregator — filter by_hour to today only
        let today_prefix = now.format("%Y-%m-%dT").to_string();
        for (hour_key, info) in &snap.by_hour {
            if hour_key.starts_with(&today_prefix) {
                // Extract "HH" from "YYYY-MM-DDTHH"
                let hh = &hour_key[11..]; // after "YYYY-MM-DDT"
                hourly_today
                    .entry(hh.to_string())
                    .or_default()
                    .accumulate(info);
            }
        }

        let mut daily_by_source = cached_view.daily_by_source.clone();
        for (day_key, hot_source_map) in &snap.by_day_source {
            let day_map = daily_by_source.entry(day_key.clone()).or_default();
            for (source, hot_stats) in hot_source_map {
                let entry = day_map.entry(*source).or_default();
                entry.token_info.accumulate(&hot_stats.token_info);
                entry.record_count += hot_stats.record_count;
                entry.cost_usd += hot_stats.cost_usd;
                entry.message_count += hot_stats.message_count;
            }
        }

        let mut hourly_today_by_source = std::collections::BTreeMap::new();
        for (hh, source_map) in &cached_view.hourly_today_by_source {
            hourly_today_by_source.insert(hh.clone(), source_map.clone());
        }
        let today_prefix = now.format("%Y-%m-%dT").to_string();
        for (hour_key, hot_source_map) in &snap.by_hour_source {
            if hour_key.starts_with(&today_prefix) {
                let hh = &hour_key[11..];
                let hour_map = hourly_today_by_source.entry(hh.to_string()).or_default();
                for (source, info) in hot_source_map {
                    hour_map.entry(*source).or_default().accumulate(info);
                }
            }
        }

        // ----- Recent records (last 50, sorted by datetime desc) -----
        let mut recent: Vec<RecentRecord> = hot_data
            .iter()
            .map(|log| {
                let cost = self.pricing.calculate_cost(&log.source_model, &log.token_info);
                RecentRecord::from_datalog(log, cost)
            })
            .collect();
        recent.sort_by(|a, b| b.source_datetime.cmp(&a.source_datetime));
        recent.truncate(50);

        for r in &mut recent {
            let is_uuid = r.source_project.len() == 36 && r.source_project.chars().filter(|&c| c == '-').count() == 4;
            if is_uuid {
                r.source_project = registry.get_title(&r.source_project);
            }
        }

        // ----- Merge project_sources & resolve UUID keys to registry titles -----
        let mut project_sources: std::collections::HashMap<String, std::collections::HashSet<tp_protocol::SourceName>> = std::collections::HashMap::new();

        let mut merge_project_sources = |key: &String, sources: &std::collections::HashSet<tp_protocol::SourceName>| {
            let is_uuid = key.len() == 36 && key.chars().filter(|&c| c == '-').count() == 4;
            let resolved_key = if is_uuid {
                registry.get_title(key)
            } else {
                key.clone()
            };
            project_sources
                .entry(resolved_key)
                .or_default()
                .extend(sources.clone());
        };

        for (key, sources) in &cached_view.project_sources {
            merge_project_sources(key, sources);
        }
        for (key, sources) in &snap.project_sources {
            merge_project_sources(key, sources);
        }

        // ----- Merge model_sources -----
        let mut model_sources: std::collections::HashMap<String, std::collections::HashSet<tp_protocol::SourceName>> = std::collections::HashMap::new();
        for (key, sources) in &cached_view.model_sources {
            model_sources.entry(key.clone()).or_default().extend(sources.clone());
        }
        for (key, sources) in &snap.model_sources {
            model_sources.entry(key.clone()).or_default().extend(sources.clone());
        }

        Ok(DashboardView {
            total_tokens,
            today_tokens,
            week_tokens,
            month_tokens,
            total_cost,
            today_cost,
            week_cost,
            month_cost,
            record_count,
            by_source,
            by_model,
            by_project,
            daily_series,
            hourly_today,
            recent_records: recent,
            last_updated: now,
            source_status: cached_view.source_status.clone(),
            cache_termination_key: cached_view.cache_termination_key.clone(),
            daily_by_source,
            hourly_today_by_source,
            project_sources,
            model_sources,
            memory_warning: cached_view.memory_warning.clone(),
        })
    }

    /// 启动后台刷新循环。
    ///
    /// - 订阅 cache 更新信号，每次信号触发刷新
    /// - 定时每 30 秒兜底刷新
    /// - 收到 shutdown 信号后停止
    pub async fn start_background(
        self: Arc<Self>,
        shutdown: watch::Receiver<bool>,
    ) {
        info!("DataShow background loop started");

        // Subscribe to cache update signals
        let cache_rx = match self.cache.subscribe().await {
            Ok(rx) => rx,
            Err(e) => {
                error!("failed to subscribe to cache updates: {e}, running timer-only mode");
                // Fall back to timer-only mode
                self.run_timer_only_loop(shutdown).await;
                return;
            }
        };

        self.run_full_loop(cache_rx, shutdown).await;
    }

    /// 完整循环：cache 信号 + shutdown（纯响应式，无定时器兜底）
    async fn run_full_loop(
        self: &Arc<Self>,
        mut cache_rx: tokio::sync::broadcast::Receiver<tp_protocol::CacheUpdateSignal>,
        mut shutdown: watch::Receiver<bool>,
    ) {
        loop {
            tokio::select! {
                // Shutdown signal
                result = shutdown.changed() => {
                    match result {
                        Ok(()) if *shutdown.borrow() => {
                            info!("DataShow background loop: shutdown signal received");
                            break;
                        }
                        Ok(()) => continue,
                        Err(_) => {
                            warn!("DataShow background loop: shutdown channel closed");
                            break;
                        }
                    }
                }

                // Cache update signal — 纯响应式刷新
                signal = cache_rx.recv() => {
                    match signal {
                        Ok(sig) => {
                            debug!(?sig, "DataShow: cache update signal received, refreshing");
                            if let Err(e) = self.refresh().await {
                                error!("DataShow refresh failed on cache signal: {e}");
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            warn!(lagged = n, "DataShow: missed cache signals, refreshing anyway");
                            if let Err(e) = self.refresh().await {
                                error!("DataShow refresh failed after lag: {e}");
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            warn!("DataShow: cache broadcast channel closed, switching to timer-only");
                            break;
                        }
                    }
                }
            }
        }

        info!("DataShow background loop ended");
    }

    /// 仅定时器模式 (cache 订阅失败时的降级方案)
    async fn run_timer_only_loop(
        self: &Arc<Self>,
        mut shutdown: watch::Receiver<bool>,
    ) {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(30));
        interval.tick().await;

        loop {
            tokio::select! {
                result = shutdown.changed() => {
                    match result {
                        Ok(()) if *shutdown.borrow() => {
                            info!("DataShow timer-only loop: shutdown signal received");
                            break;
                        }
                        Ok(()) => continue,
                        Err(_) => {
                            warn!("DataShow timer-only loop: shutdown channel closed");
                            break;
                        }
                    }
                }
                _ = interval.tick() => {
                    debug!("DataShow: timer-only periodic refresh");
                    if let Err(e) = self.refresh().await {
                        error!("DataShow timer-only refresh failed: {e}");
                    }
                }
            }
        }

        info!("DataShow timer-only loop ended");
    }
}

// ---------------------------------------------------------------------------
// DataShowProvider trait 实现
// ---------------------------------------------------------------------------

#[async_trait]
impl DataShowProvider for DataShow {
    /// 获取当前 DashboardView 快照。
    async fn get_view(&self) -> Result<DashboardView, AggregatorError> {
        Ok(self.view_rx.borrow().clone())
    }

    /// 请求立即刷新 — 阻塞到刷新完成。
    async fn request_refresh(&self) -> Result<(), AggregatorError> {
        self.refresh().await
    }

    /// 订阅视图更新 — 返回 watch receiver 的克隆。
    fn subscribe_view(&self) -> watch::Receiver<DashboardView> {
        self.view_rx.clone()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// 合并两组 `DimensionEntry` — 相同 key 的条目合并 token 和 cost。
fn merge_dimension_entries(
    base: &[tp_protocol::DimensionEntry],
    overlay: &[tp_protocol::DimensionEntry],
) -> Vec<tp_protocol::DimensionEntry> {
    use std::collections::HashMap;

    let mut map: HashMap<String, tp_protocol::DimensionEntry> = HashMap::new();

    for entry in base {
        map.entry(entry.key.clone())
            .and_modify(|e| {
                e.token_info.accumulate(&entry.token_info);
                e.record_count += entry.record_count;
                e.cost_usd += entry.cost_usd;
                if entry.display_name.is_some() {
                    e.display_name = entry.display_name.clone();
                }
            })
            .or_insert_with(|| entry.clone());
    }

    for entry in overlay {
        map.entry(entry.key.clone())
            .and_modify(|e| {
                e.token_info.accumulate(&entry.token_info);
                e.record_count += entry.record_count;
                e.cost_usd += entry.cost_usd;
                if entry.display_name.is_some() {
                    e.display_name = entry.display_name.clone();
                }
            })
            .or_insert_with(|| entry.clone());
    }

    let mut result: Vec<_> = map.into_values().collect();
    // Sort by total tokens descending (consistent with IncrementalAggregator.project)
    result.sort_by(|a, b| b.token_info.total().cmp(&a.token_info.total()));
    result
}

/// 解析 UUID 会话 ID 为友好的名称映射，并且在内存中合并重复及 Unknown 类别。
fn resolve_and_merge_by_project(
    entries: &[tp_protocol::DimensionEntry],
    registry: &tp_protocol::ConversationRegistry,
) -> Vec<tp_protocol::DimensionEntry> {
    use std::collections::HashMap;

    let mut map: HashMap<String, tp_protocol::DimensionEntry> = HashMap::new();

    for entry in entries {
        let is_uuid = entry.key.len() == 36 && entry.key.chars().filter(|&c| c == '-').count() == 4;
        let title = if is_uuid {
            registry.get_title(&entry.key)
        } else {
            entry.key.clone()
        };

        map.entry(title.clone())
            .and_modify(|e| {
                e.token_info.accumulate(&entry.token_info);
                e.record_count += entry.record_count;
                e.cost_usd += entry.cost_usd;
            })
            .or_insert_with(|| tp_protocol::DimensionEntry {
                key: title.clone(),
                token_info: entry.token_info,
                record_count: entry.record_count,
                cost_usd: entry.cost_usd,
                display_name: Some(title),
            });
    }

    let mut result: Vec<_> = map.into_values().collect();
    result.sort_by(|a, b| b.token_info.total().cmp(&a.token_info.total()));
    result
}
