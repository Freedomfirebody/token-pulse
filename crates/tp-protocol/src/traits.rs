//! 跨 crate 的 trait 定义。
//!
//! 所有组件之间的契约接口定义在此处，
//! 确保实现者和消费者不需要直接依赖。

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::sync::Arc;

use crate::datalog::{Datalog, SourceName};
use crate::dimension::Dimension;
use crate::error::*;
use crate::view::DashboardView;

// ===== 数据采集 Trait =====

/// 数据源提供者
///
/// 每个采集组件 (antigravity / codex / claude) 实现此 trait。
/// datasource 只采集结束的数据。
///
/// 支持两种数据获取模式：
/// - **拉取（默认）**: 定时调用 `collect()` / `collect_since()`
/// - **推送（可选）**: 实现 `data_notifier()` 返回 `Some(Notify)`，
///   框架会在 `select!` 中监听通知并立即触发 `collect()`
#[async_trait]
pub trait DatasourceProvider: Send + Sync {
    /// 获取此数据源的名称标识
    fn name(&self) -> SourceName;

    /// 获取此数据源的描述信息
    fn description(&self) -> &str;

    /// 执行一次完整采集 — 返回所有可用的已结束数据
    async fn collect(&self) -> Result<Vec<Datalog>, CollectionError>;

    /// 增量采集 — 仅返回指定时间点之后的新数据
    async fn collect_since(&self, since: DateTime<Utc>) -> Result<Vec<Datalog>, CollectionError>;

    /// 检查数据源是否可用
    async fn health_check(&self) -> Result<bool, CollectionError>;

    /// 获取数据到达通知器（可选）
    ///
    /// 如果数据源内部有 IngestStore 等缓冲区，
    /// 可返回 `Some(notify)` 让框架在新数据到达时立即触发 `collect()`，
    /// 而非等到下一个定时器周期。
    ///
    /// 默认返回 `None`，表示仅使用定时拉取模式。
    fn data_notifier(&self) -> Option<Arc<tokio::sync::Notify>> {
        None
    }
}

// ===== 数据池存储 Trait =====

/// 数据池通知信号
#[derive(Debug, Clone)]
pub enum PoolNotification {
    /// mate-data 索引变更 (新增或路径更新)
    MetadataChanged {
        changed_keys: Vec<String>,
        new_version: u64,
    },
    /// 新数据到达
    DataPushed {
        affected_hour_keys: Vec<String>,
        record_count: usize,
    },
}

/// 数据池存储接口
///
/// data-pool 的核心存储接口，供 data-cache 和 data-show 使用。
#[async_trait]
pub trait PoolStorage: Send + Sync {
    /// 推送数据记录 (应用 replace-or-push 规则)
    async fn push_datalogs(&self, logs: Vec<Datalog>) -> Result<PushResult, PoolError>;

    /// 查询活跃数据 (当天)
    async fn query_active(&self) -> Result<Vec<Datalog>, PoolError>;

    /// 查询指定时间范围的数据
    async fn query_range(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<Datalog>, PoolError>;

    /// 根据小时 key 查询分片数据
    async fn query_by_hour_key(&self, hour_key: &str) -> Result<Vec<Datalog>, PoolError>;

    /// 获取元数据快照
    async fn get_metadata(&self) -> Result<crate::meta::PoolMetadata, PoolError>;

    /// 获取元数据版本号 (用于快速变更检测)
    async fn get_metadata_version(&self) -> Result<u64, PoolError>;

    /// 订阅数据池通知 (返回一个 receiver)
    async fn subscribe(&self) -> Result<tokio::sync::broadcast::Receiver<PoolNotification>, PoolError>;

    /// 触发归档操作
    async fn run_archive(&self) -> Result<Vec<String>, PoolError>;
}

// ===== 缓存 Trait =====

/// 缓存更新信号
#[derive(Debug, Clone)]
pub enum CacheUpdateSignal {
    /// 指定维度的缓存已更新
    DimensionUpdated { dimension: Dimension },
    /// 全量缓存重建完成
    FullRebuildComplete,
    /// 增量更新完成
    IncrementalUpdateComplete {
        affected_keys: Vec<String>,
    },
}

/// 缓存提供者接口
///
/// data-cache 的核心接口，供 data-show 使用。
#[async_trait]
pub trait CacheProvider: Send + Sync {
    /// 获取缓存的聚合快照
    async fn get_snapshot(&self) -> Result<crate::view::DashboardView, CacheError>;

    /// 根据维度获取缓存结果
    async fn get_by_dimension(&self, dimension: &Dimension) -> Result<Vec<crate::view::DimensionEntry>, CacheError>;

    /// 手动触发失效重算
    async fn invalidate(&self, affected_keys: &[String]) -> Result<(), CacheError>;

    /// 触发完整重建
    async fn rebuild(&self) -> Result<(), CacheError>;

    /// 订阅缓存更新信号
    async fn subscribe(&self) -> Result<tokio::sync::broadcast::Receiver<CacheUpdateSignal>, CacheError>;

    /// 获取缓存构建进度
    async fn get_progress(&self) -> Result<CacheProgress, CacheError>;
}

/// 缓存构建进度
#[derive(Debug, Clone, Default)]
pub struct CacheProgress {
    /// 已处理的索引 key 数
    pub processed_count: usize,
    /// 总索引 key 数
    pub total_count: usize,
    /// 是否正在构建中
    pub building: bool,
    /// 上次完成构建的时间
    pub last_build_at: Option<DateTime<Utc>>,
    /// 中止节点 (截止的小时 key)
    pub termination_hour_key: Option<String>,
}

// ===== 数据展示 Trait =====

/// 数据展示接口 (data-show)
///
/// 合并 cache (冷数据) + pool (热数据) → 内存视图，
/// 提供给 Dash 渲染层消费。
#[async_trait]
pub trait DataShowProvider: Send + Sync {
    /// 获取当前的仪表盘视图
    async fn get_view(&self) -> Result<DashboardView, AggregatorError>;

    /// 请求刷新 (data-request/reflash)
    async fn request_refresh(&self) -> Result<(), AggregatorError>;

    /// 订阅视图更新 (reflash-rendering)
    fn subscribe_view(&self) -> tokio::sync::watch::Receiver<DashboardView>;
}
