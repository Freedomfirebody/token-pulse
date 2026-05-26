use serde::{Serialize, Deserialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenBreakdown {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub reasoning_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTotals {
    pub session_id: String,
    pub label: String,
    pub file_path: String,
    pub last_modified_ms: u64,
    pub mode: String, // "reported" | "estimated"
    pub source: String, // "filesystem" | "rpc-artifact"
    pub evidence_count: u64,
    pub message_count: u64,
    #[serde(default)]
    pub model_totals: HashMap<String, u64>,
    #[serde(default)]
    pub model_breakdowns: HashMap<String, TokenBreakdown>,
    #[serde(flatten)]
    pub breakdown: TokenBreakdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSnapshot {
    pub captured_at: u64,
    pub mode: String, // "reported" | "estimated"
    #[serde(flatten)]
    pub breakdown: TokenBreakdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionLifecycleStatus {
    Active,
    Archived,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionLifecycle {
    pub status: SessionLifecycleStatus,
    pub last_seen_at: u64,
    pub archived_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedSessionState {
    pub signature: String,
    pub latest: SessionTotals,
    pub snapshots: Vec<SessionSnapshot>,
    pub lifecycle: SessionLifecycle,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedState {
    pub last_poll_at: Option<u64>,
    pub sessions: HashMap<String, PersistedSessionState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardSession {
    pub session_id: String,
    pub label: String,
    pub file_path: String,
    pub last_modified_ms: u64,
    pub status: SessionLifecycleStatus,
    pub last_seen_at: u64,
    pub archived_at: Option<u64>,
    pub mode: String, // "reported" | "estimated"
    pub source: String, // "filesystem" | "rpc-artifact"
    pub message_count: u64,
    pub latest: TokenBreakdown,
    pub latest_delta: TokenBreakdown,
    pub recent_totals: Vec<u64>,
    pub snapshot_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityHeatmapBin {
    pub date: String, // "YYYY-MM-DD"
    pub total_tokens: u64,
    pub session_count: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub reasoning_tokens: u64,
    pub cost_usd: f64,
    pub message_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceBreakdown {
    pub source: String, // "filesystem" | "rpc-artifact"
    pub session_count: u64,
    pub changed_session_count: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModeBreakdown {
    pub mode: String, // "reported" | "estimated"
    pub session_count: u64,
    pub changed_session_count: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsageBreakdown {
    pub model: String,
    pub total_tokens: u64,
    pub session_count: u64,
    pub cost_usd: Option<f64>,
    pub pricing_status: String, // "priced" | "unpriced"
    pub pricing_note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardPricingSummary {
    pub status: String, // "ready" | "partial" | "unavailable" | "error"
    pub total_cost_usd: f64,
    pub priced_model_count: u64,
    pub unpriced_model_count: u64,
    pub missing_models: Vec<String>,
    pub last_updated_at: Option<u64>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcCoverageBreakdown {
    pub tracked_sessions: u64,
    pub exported_sessions: u64,
    pub skipped_sessions: u64,
    pub changed_sessions: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardAnalytics {
    pub activity_heatmap: Vec<ActivityHeatmapBin>,
    pub source_breakdown: Vec<SourceBreakdown>,
    pub mode_breakdown: Vec<ModeBreakdown>,
    pub model_usage: Vec<ModelUsageBreakdown>,
    pub rpc_coverage: RpcCoverageBreakdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardState {
    pub root_path: String,
    pub config: DashboardConfig,
    pub last_poll_at: Option<u64>,
    pub sync_status: String, // "idle" | "running" | "error"
    pub sync_message: String,
    pub export_status: ExportStatusSummary,
    pub sessions: Vec<DashboardSession>,
    pub summary: DashboardSummary,
    pub pricing: DashboardPricingSummary,
    pub analytics: DashboardAnalytics,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardConfig {
    pub use_rpc_export: bool,
    pub export_steps_jsonl: bool,
    pub poll_interval_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportStatusSummary {
    pub status: String, // "idle" | "running" | "error"
    pub message: String,
    pub last_export_at: Option<u64>,
    pub last_exported_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardSummary {
    pub session_count: u64,
    pub active_session_count: u64,
    pub archived_session_count: u64,
    pub message_count: u64,
    pub changed_session_count: u64,
    pub total_tokens: u64,
    pub estimated_session_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionScanCandidate {
    pub session_id: String,
    pub session_dir: String,
    pub pb_path: Option<String>,
    pub file_paths: Vec<String>,
    pub label_hint: String,
    pub last_modified_ms: u64,
    pub signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionParsePlan {
    pub session_id: String,
    pub session_dir: String,
    pub label_hint: String,
    pub last_modified_ms: u64,
    pub token_file_paths: Vec<String>,
    pub analysis_signature: String,
    pub source: String,
}
