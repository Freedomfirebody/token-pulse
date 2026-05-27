//! Token-Pulse — 多源 AI Token 使用量监控仪表盘。
//!
//! 架构:
//! ```text
//! ┌─ Collectors ──────────────────────────────────────┐
//! │  tp-collector-antigravity  (Gemini Antigravity)   │
//! │  tp-collector-codex        (OpenAI Codex/ccusage) │
//! │  tp-collector-claude       (Claude Code)          │
//! └──────────────────┬────────────────────────────────┘
//!                    │ Vec<Datalog>
//!                    ▼
//! ┌─ Data Pool ──────────────────────────────────────┐
//! │  tp-pool  (tiered storage: active/daily/monthly) │
//! │  mate-data index  +  replace-or-push rules       │
//! └──────────────────┬───────────────────────────────┘
//!                    │ PoolNotification
//!                    ▼
//! ┌─ Cache ──────────────────────────────────────────┐
//! │  tp-cache  (incremental aggregation + snapshots) │
//! └──────────────────┬───────────────────────────────┘
//!                    │ CacheUpdateSignal
//!                    ▼
//! ┌─ Aggregator ─────────────────────────────────────┐
//! │  tp-aggregator  (cold + hot merge → DashboardView│
//! └──────────────────┬───────────────────────────────┘
//!                    │ watch::Receiver<DashboardView>
//!                    ▼
//! ┌─ Dash ───────────────────────────────────────────┐
//! │  tp-dash  (Xilem UI rendering)                   │
//! └──────────────────────────────────────────────────┘
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use tracing::{info, warn, error};

use tp_protocol::{DataShowProvider, PoolStorage};


fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ===== 1. 初始化日志 =====
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
        )
        .init();

    info!("Token-Pulse v{} 启动", env!("CARGO_PKG_VERSION"));

    // ===== 2. 解析数据目录 =====
    let data_dir = resolve_data_dir();
    info!(data_dir = %data_dir.display(), "数据目录");
    std::fs::create_dir_all(&data_dir)?;

    // ===== 3. 构建 Tokio Runtime =====
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    // ===== 4. 在 Runtime 中初始化后台管道 =====
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (view_tx, view_rx) = watch::channel(tp_protocol::view::DashboardView::default());
    let (cmd_tx, cmd_rx) = mpsc::channel::<tp_dash::PipelineCommand>(32);
    let (collector_cmd_tx, _) = tokio::sync::broadcast::channel::<tp_collector::CollectorCommand>(16);

    let data_dir_clone = data_dir.clone();
    let shutdown_rx_clone = shutdown_rx.clone();
    let collector_cmd_tx_clone = collector_cmd_tx.clone();

    runtime.spawn(async move {
        if let Err(e) = run_pipeline(data_dir_clone, view_tx, cmd_rx, collector_cmd_tx_clone, shutdown_rx_clone).await {
            error!(error = %e, "管道启动失败");
        }
    });

    // ===== 5. 启动 Xilem UI (主线程) =====
    info!("启动 Xilem 渲染引擎");

    let app_state = tp_dash::AppState::new()
        .with_view_rx(view_rx)
        .with_command_tx(cmd_tx);

    // 启动诊断
    run_startup_diagnostics(&data_dir);

    let app = xilem::Xilem::new_simple(
        app_state,
        tp_dash::app_logic,
        xilem::WindowOptions::new("Token Pulse — AI Token Dashboard")
            .with_initial_inner_size(xilem::dpi::LogicalSize::new(1100.0, 800.0))
            .with_min_inner_size(xilem::dpi::LogicalSize::new(800.0, 600.0)),
    );

    let run_res = app.run_in(xilem::EventLoop::with_user_event());
    info!("UI 事件循环结束: {:?}", run_res);

    // ===== 6. 优雅关闭 =====
    let _ = shutdown_tx.send(true);
    runtime.shutdown_timeout(Duration::from_secs(5));
    info!("Token-Pulse 已关闭");
    std::process::exit(0);
}

/// 解析数据存储目录
fn resolve_data_dir() -> PathBuf {
    // 优先使用环境变量
    if let Ok(dir) = std::env::var("TOKEN_PULSE_DATA_DIR") {
        return PathBuf::from(dir);
    }
    // 默认: ~/.token-pulse/
    dirs::home_dir()
        .map(|h| h.join(".token-pulse"))
        .unwrap_or_else(|| PathBuf::from(".token-pulse"))
}

/// 后台数据管道
async fn run_pipeline(
    data_dir: PathBuf,
    view_tx: watch::Sender<tp_protocol::view::DashboardView>,
    mut cmd_rx: mpsc::Receiver<tp_dash::PipelineCommand>,
    collector_cmd_tx: tokio::sync::broadcast::Sender<tp_collector::CollectorCommand>,
    shutdown_rx: watch::Receiver<bool>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    info!("初始化数据管道...");

    // ===== 初始化 Data Pool =====
    let pool = Arc::new(
        tp_pool::DataPool::new(data_dir.join("pool"))
            .map_err(|e| format!("Pool 初始化失败: {}", e))?
    );
    info!("Data Pool 就绪");

    // ===== 初始化 Cache =====
    let cache = Arc::new(
        tp_cache::DataCache::new(pool.clone() as Arc<dyn tp_protocol::PoolStorage>)
    );

    // 首次构建缓存
    if let Err(e) = cache.build().await {
        warn!(error = %e, "缓存首次构建失败，将使用空数据");
    }
    info!("Data Cache 就绪");

    // ===== 初始化 Aggregator (Data Show) =====
    let aggregator = Arc::new(
        tp_aggregator::DataShow::new(
            pool.clone() as Arc<dyn tp_protocol::PoolStorage>,
            cache.clone() as Arc<dyn tp_protocol::CacheProvider>,
        )
    );

    // 初始刷新
    if let Err(e) = aggregator.refresh().await {
        warn!(error = %e, "聚合器初始刷新失败");
    }

    // 启动聚合器后台刷新
    let agg_clone = aggregator.clone();
    let shutdown_rx_agg = shutdown_rx.clone();
    tokio::spawn(async move {
        agg_clone.start_background(shutdown_rx_agg).await;
    });
    info!("Data Aggregator 就绪");

    // ===== 初始化 Collectors =====
    let (collect_tx, mut collect_rx) = mpsc::channel::<Vec<tp_protocol::Datalog>>(256);

    // 注册数据源
    let mut coordinator = tp_collector::CollectorCoordinator::new(collect_tx);

    // Antigravity Collector
    let antigravity_root = dirs::home_dir()
        .map(|h| h.join(".gemini").join("antigravity"))
        .unwrap_or_else(|| PathBuf::from("."));
    coordinator.register(Arc::new(
        tp_collector_antigravity::AntigravityCollector::new(antigravity_root.clone())
    ));


    // Codex Collector
    coordinator.register(Arc::new(tp_collector_codex::CodexCollector::new()));

    // Claude Collector
    coordinator.register(Arc::new(tp_collector_claude::ClaudeCollector::new()));

    info!(sources = coordinator.source_count(), "Collectors 注册完成");

    // ===== 数据摄入管道: collect_rx → pool =====
    let pool_for_ingest = pool.clone();
    let cache_for_ingest = cache.clone();
    tokio::spawn(async move {
        while let Some(logs) = collect_rx.recv().await {
            if logs.is_empty() { continue; }
            let count = logs.len();
            match pool_for_ingest.push_datalogs(logs).await {
                Ok(result) => {
                    info!(
                        pushed = result.pushed,
                        replaced = result.replaced,
                        skipped = result.skipped,
                        "数据已推送到 Pool"
                    );
                    // 通知缓存
                    if !result.affected_hour_keys.is_empty() {
                        let notification = tp_protocol::PoolNotification::DataPushed {
                            affected_hour_keys: result.affected_hour_keys,
                            record_count: count,
                        };
                        cache_for_ingest.on_pool_notification(notification).await;
                    }
                }
                Err(e) => {
                    error!(error = %e, "推送数据到 Pool 失败");
                }
            }
        }
    });

    // ===== 监听 UI 管道控制指令 =====
    let pool_for_cmd = pool.clone();
    let cache_for_cmd = cache.clone();
    let aggregator_for_cmd = aggregator.clone();
    let collector_cmd_tx_for_cmd = collector_cmd_tx.clone();
    tokio::spawn(async move {
        while let Some(cmd) = cmd_rx.recv().await {
            info!("收到管道控制指令: {:?}", cmd);
            match cmd {
                tp_dash::PipelineCommand::Refresh => {
                    info!("执行展示数据 Refresh (重新预计算并渲染)...");
                    if let Err(e) = aggregator_for_cmd.request_refresh().await {
                        error!("Refresh 失败: {:?}", e);
                    }
                }
                tp_dash::PipelineCommand::Upsert => {
                    info!("执行展示数据 Upsert (采集截止点重置为0，强制从头拉取)...");
                    let _ = collector_cmd_tx_for_cmd.send(tp_collector::CollectorCommand::TriggerUpsert);
                }
                tp_dash::PipelineCommand::Rebuild => {
                    info!("执行全系统 Rebuild (清空存储、清空缓存、采集点归零并重新拉取)...");
                    // 1. 清空 pool 物理存储
                    if let Err(e) = pool_for_cmd.clear_storage() {
                        error!("清空存储失败: {:?}", e);
                    }
                    // 2. 清空 cache 内存状态
                    cache_for_cmd.clear();
                    // 3. 广播重置采集器并从 0 强制拉取
                    let _ = collector_cmd_tx_for_cmd.send(tp_collector::CollectorCommand::TriggerRebuild);
                    // 4. 立即刷新展示 (将重置为全空状态)
                    if let Err(e) = aggregator_for_cmd.request_refresh().await {
                        error!("Refresh 失败: {:?}", e);
                    }
                }
            }
        }
    });

    // ===== 启动流式采集 (每个数据源独立并行) =====
    let collect_interval = Duration::from_secs(60); // 每分钟采集一次
    tokio::spawn(async move {
        coordinator.run_streaming(collect_interval, collector_cmd_tx, shutdown_rx).await;
    });

    info!("数据管道启动完成");

    // 主管道任务: 定期将 aggregator view 推送到 UI
    let mut interval = tokio::time::interval(Duration::from_secs(2));
    let view_subscriber = aggregator.subscribe_view();
    loop {
        tokio::select! {
            _ = interval.tick() => {
                let current_view = view_subscriber.borrow().clone();
                let _ = view_tx.send(current_view);
            }
        }
    }
}

/// 启动诊断
fn run_startup_diagnostics(data_dir: &PathBuf) {
    println!("\x1b[1;36m=== TOKEN PULSE STARTUP DIAGNOSTICS ===\x1b[0m");
    println!("  Data Directory: {:?}", data_dir);
    println!("  Data Dir Exists: {}", data_dir.exists());

    if let Some(home) = dirs::home_dir() {
        let brain_dir = home.join(".gemini").join("antigravity").join("brain");
        println!("  Antigravity Brain: {:?} (exists: {})", brain_dir, brain_dir.exists());
        if brain_dir.exists() {
            if let Ok(entries) = std::fs::read_dir(&brain_dir) {
                let count = entries.filter_map(|e| e.ok()).filter(|e| e.path().is_dir()).count();
                println!("  Session Count: \x1b[1;32m{}\x1b[0m", count);
            }
        }
    }

    println!("\x1b[1;36m========================================\x1b[0m");
}
