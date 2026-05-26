use serde::{Serialize, Deserialize};
use tokio::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CcusageModelBreakdown {
    pub model_name: String,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_creation_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
    #[serde(default)]
    pub cost: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CcusageMetadata {
    #[serde(default)]
    pub agents: Option<Vec<String>>,
    #[serde(default)]
    pub last_activity: Option<String>,
    #[serde(default)]
    pub reasoning_output_tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CcusageRow {
    #[serde(default)]
    pub agent: String,
    pub period: String,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
    #[serde(default)]
    pub total_cost: f64,
    #[serde(default)]
    pub models_used: Vec<String>,
    #[serde(default)]
    pub model_breakdowns: Vec<CcusageModelBreakdown>,
    #[serde(default)]
    pub metadata: CcusageMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CcusageTotals {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
    #[serde(default)]
    pub total_cost: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CcusageDailyResponse {
    pub daily: Vec<CcusageRow>,
    pub totals: CcusageTotals,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CcusageWeeklyResponse {
    pub weekly: Vec<CcusageRow>,
    pub totals: CcusageTotals,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CcusageMonthlyResponse {
    pub monthly: Vec<CcusageRow>,
    pub totals: CcusageTotals,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CcusageSessionResponse {
    pub session: Vec<CcusageRow>,
    pub totals: CcusageTotals,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CcusageSnapshot {
    pub daily: Vec<CcusageRow>,
    pub weekly: Vec<CcusageRow>,
    pub monthly: Vec<CcusageRow>,
    pub sessions: Vec<CcusageRow>,
    pub totals: CcusageTotals,
}

async fn run_ccusage_command(args: &[&str]) -> Result<serde_json::Value, String> {
    let timeout_dur = std::time::Duration::from_secs(3);
    
    // Try npx ccusage first
    let mut cmd = if cfg!(target_os = "windows") {
        let mut c = Command::new("cmd");
        c.args(&["/d", "/c", "npx", "ccusage"]);
        c.args(args);
        c
    } else {
        let mut c = Command::new("npx");
        c.arg("ccusage");
        c.args(args);
        c
    };

    let output = match tokio::time::timeout(timeout_dur, cmd.output()).await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => {
            // Fallback to direct ccusage
            let mut fallback_cmd = if cfg!(target_os = "windows") {
                let mut c = Command::new("cmd");
                c.args(&["/d", "/c", "ccusage"]);
                c.args(args);
                c
            } else {
                let mut c = Command::new("ccusage");
                c.args(args);
                c
            };
            match tokio::time::timeout(timeout_dur, fallback_cmd.output()).await {
                Ok(Ok(out)) => out,
                Ok(Err(fe)) => return Err(format!("ccusage CLI not found or failed to spawn: {} (fallback err: {})", e, fe)),
                Err(_) => return Err("ccusage CLI fallback execution timed out".to_string()),
            }
        }
        Err(_) => {
            // Timeout on npx ccusage, try fallback to direct ccusage
            let mut fallback_cmd = if cfg!(target_os = "windows") {
                let mut c = Command::new("cmd");
                c.args(&["/d", "/c", "ccusage"]);
                c.args(args);
                c
            } else {
                let mut c = Command::new("ccusage");
                c.args(args);
                c
            };
            match tokio::time::timeout(timeout_dur, fallback_cmd.output()).await {
                Ok(Ok(out)) => out,
                Ok(Err(fe)) => return Err(format!("ccusage CLI execution timed out, fallback failed to spawn: {}", fe)),
                Err(_) => return Err("ccusage CLI execution and fallback both timed out".to_string()),
            }
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        // Try fallback to direct ccusage if npx execution failed
        let mut fallback_cmd = if cfg!(target_os = "windows") {
            let mut c = Command::new("cmd");
            c.args(&["/d", "/c", "ccusage"]);
            c.args(args);
            c
        } else {
            let mut c = Command::new("ccusage");
            c.args(args);
            c
        };
        if let Ok(Ok(fallback_output)) = tokio::time::timeout(timeout_dur, fallback_cmd.output()).await {
            if fallback_output.status.success() {
                let stdout = String::from_utf8_lossy(&fallback_output.stdout);
                if let Ok(parsed) = serde_json::from_str(&stdout) {
                    return Ok(parsed);
                }
            }
        }
        return Err(format!("Command failed: {}", stderr));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&stdout)
        .map_err(|e| format!("Failed to parse JSON: {}", e))
}

pub async fn fetch_ccusage_snapshot(timezone: &str) -> Result<CcusageSnapshot, String> {
    // 1. Try to query the local ccusage web monitor service directly via HTTP first!
    let client = reqwest::Client::new();
    let url = "http://127.0.0.1:3977/api/snapshot";
    
    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct LocalSummary {
        totals: CcusageTotals,
    }
    
    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct LocalSnapshot {
        daily: Vec<CcusageRow>,
        weekly: Vec<CcusageRow>,
        monthly: Vec<CcusageRow>,
        sessions: Vec<CcusageRow>,
        summary: LocalSummary,
    }
    
    match client.get(url)
        .timeout(std::time::Duration::from_millis(1500))
        .send()
        .await 
    {
        Ok(res) => {
            if res.status() == reqwest::StatusCode::OK {
                if let Ok(local_snap) = res.json::<LocalSnapshot>().await {
                    println!("[ccusage] Successfully retrieved token snapshot directly from local monitor service!");
                    return Ok(CcusageSnapshot {
                        daily: local_snap.daily,
                        weekly: local_snap.weekly,
                        monthly: local_snap.monthly,
                        sessions: local_snap.sessions,
                        totals: local_snap.summary.totals,
                    });
                }
            }
        }
        Err(_) => {
            println!("[ccusage] Local monitor server not active on 3977, falling back to CLI execution.");
        }
    }

    // 2. Fallback to CLI commands if local server is not active
    let timezone_str = if timezone.is_empty() { "Asia/Shanghai" } else { timezone };

    let daily_json = run_ccusage_command(&["daily", "--json", "--timezone", timezone_str]).await?;
    let weekly_json = run_ccusage_command(&["weekly", "--json", "--timezone", timezone_str]).await?;
    let monthly_json = run_ccusage_command(&["monthly", "--json", "--timezone", timezone_str]).await?;
    let session_json = run_ccusage_command(&["session", "--json", "--timezone", timezone_str]).await?;

    let daily_resp: CcusageDailyResponse = serde_json::from_value(daily_json)
        .map_err(|e| format!("Failed to decode daily: {}", e))?;
    let weekly_resp: CcusageWeeklyResponse = serde_json::from_value(weekly_json)
        .map_err(|e| format!("Failed to decode weekly: {}", e))?;
    let monthly_resp: CcusageMonthlyResponse = serde_json::from_value(monthly_json)
        .map_err(|e| format!("Failed to decode monthly: {}", e))?;
    let session_resp: CcusageSessionResponse = serde_json::from_value(session_json)
        .map_err(|e| format!("Failed to decode session: {}", e))?;

    Ok(CcusageSnapshot {
        daily: daily_resp.daily,
        weekly: weekly_resp.weekly,
        monthly: monthly_resp.monthly,
        sessions: session_resp.session,
        totals: daily_resp.totals,
    })
}
