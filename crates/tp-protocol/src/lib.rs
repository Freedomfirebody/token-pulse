//! # tp-protocol
//!
//! 数据协议包 — Token-Pulse 系统的共享类型、trait 定义与错误类型。
//!
//! 所有 crate 的通信契约都定义在此处，确保零耦合的跨 crate 数据交换。
// Dummy comment to invalidate build cache and force rebuild of tp-protocol in workspace.

pub mod datalog;
pub mod dimension;
pub mod error;
pub mod meta;
pub mod traits;
pub mod view;
pub mod pricing;
pub mod conversation;

// ===== 顶级 re-export =====
pub use datalog::{Datalog, DatalogUid, ReportClass, SourceName, TokenInfo};
pub use dimension::{Dimension, TimeGranularity};
pub use error::*;
pub use meta::{MetaIndex, PartitionTier, PoolMetadata};
pub use traits::{CacheProvider, CacheUpdateSignal, DatasourceProvider, DataShowProvider, PoolNotification, PoolStorage};
pub use view::{DashboardView, DimensionEntry, SourceStatus};
pub use pricing::{ModelPrice, PricingTable};
pub use conversation::ConversationRegistry;

