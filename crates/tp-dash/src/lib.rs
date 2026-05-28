//! # tp-dash
//!
//! 渲染端 — Xilem GPU 加速 Dashboard UI。
//!
//! 消费 `tp_aggregator::DataShow` 提供的 `DashboardView`，
//! 实现高性能、响应式的 Token 使用可视化面板。

pub mod theme;
pub mod app_state;
pub mod widgets;
pub mod views;

pub use app_state::AppState;
pub use app_state::app_logic;
pub use app_state::PipelineCommand;




