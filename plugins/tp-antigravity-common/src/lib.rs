//! # tp-antigravity-common
//!
//! Antigravity 采集器共享基础设施 — 提供 RPC 客户端、进程检测、
//! 模型映射、流式存储和 token 估算等跨 CLI/IDE 通用功能。

pub mod model_aliases;
pub mod types;
pub mod process_locator;
pub mod rpc_client;
pub mod estimator;
pub mod ingest_store;
