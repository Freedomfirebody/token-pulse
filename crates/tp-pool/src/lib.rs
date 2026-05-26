//! # tp-pool
//!
//! 分层归档数据存储 (data-pool) — Token-Pulse 的持久化核心。
//!
//! 实现三层存储分片策略:
//! - **Active**: 当天数据, 以小时为分片 (20分钟阈值)
//! - **Archive Daily**: 昨天往前, 以天为分片 (2小时阈值)
//! - **Archive Monthly**: 上月及以前, 以月为文件夹/天为文件 (5天阈值)
//!
//! 通过 replace-or-push 规则引擎保证数据去重与优先级覆盖。

pub mod active;
pub mod archive;
pub mod metadata;
pub mod replace;

mod storage;

pub use storage::DataPool;
