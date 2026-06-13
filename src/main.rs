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

#[cfg(target_os = "windows")]
use winit::platform::windows::WindowAttributesExtWindows;

use tp_protocol::{DataShowProvider, PoolStorage};
use anyhow::Result;



fn main() -> Result<()> {
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
        .with_view_rx(view_rx.clone())
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

    let event_loop = xilem::EventLoop::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    
    let (driver, windows) = app.into_driver_and_windows(|_proxy| {
        Ok(())
    });
    
    let main_window_id = windows[0].id;
    
    let tray_menu = Menu::new();
    let show_item = MenuItem::new("打开控制面板", true, None);
    let float_item = MenuItem::new("显示/隐藏悬浮窗", true, None);
    let exit_item = MenuItem::new("退出", true, None);
    
    let show_item_id = show_item.id().clone();
    let float_item_id = float_item.id().clone();
    let exit_item_id = exit_item.id().clone();
    
    tray_menu.append_items(&[
        &show_item,
        &float_item,
        &exit_item,
    ]).unwrap();

    let icon = create_dummy_icon();
    let tray_icon = match TrayIconBuilder::new()
        .with_menu(Box::new(tray_menu))
        .with_tooltip("Token Pulse Monitor")
        .with_icon(icon)
        .build()
    {
        Ok(t) => Some(t),
        Err(e) => {
            warn!("Failed to create tray icon (is explorer shell running?): {:?}", e);
            None
        }
    };

    let masonry_state = MasonryState::new(proxy.clone(), windows, Default::default());
    let app_driver = Box::new(TrayAppDriver {
        inner: driver,
        _main_window_id: main_window_id,
    });
    
    let proxy_clone = proxy.clone();
    let mut view_rx_clone = view_rx.clone();
    runtime.spawn(async move {
        // Send initial update event to ensure initial view is rendered
        let action: ErasedAction = Box::new(TelemetryUpdateEvent);
        let user_event = MasonryUserEvent::Action(main_window_id, action, WidgetId::reserved(0));
        let _ = proxy_clone.send_event(user_event);

        while view_rx_clone.changed().await.is_ok() {
            let action: ErasedAction = Box::new(TelemetryUpdateEvent);
            let user_event = MasonryUserEvent::Action(main_window_id, action, WidgetId::reserved(0));
            let _ = proxy_clone.send_event(user_event);
        }
    });

    let mut external_app = ExternalApp {
        masonry_state,
        app_driver,
        proxy,
        main_window_id,
        _tray_icon: tray_icon,
        show_item_id,
        float_item_id,
        exit_item_id,
        floating_window: None,
        floating_visible: false,
        view_rx: view_rx.clone(),
        widget_state: WidgetState::Badge,
        hover_start_time: None,
        hover_off_time: None,
        drag_active: false,
        last_cursor_pos: None,
        is_mouse_down: false,
        accumulated_drag_delta: (0.0, 0.0),
        previous_state: None,
        floating_resources: None,
        tooltip_window: None,
        resize_animation: None,
        float_needs_redraw: true,
    };

    info!("UI 事件循环开启");
    let run_res = event_loop.run_app(&mut external_app);
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
        tp_cache::DataCache::new(pool.clone() as Arc<dyn PoolStorage>)
    );

    // 首次构建缓存
    if let Err(e) = cache.build().await {
        warn!(error = %e, "缓存首次构建失败，将使用空数据");
    }

    // 启动 Cache 自治订阅 — 自动接收 Pool broadcast，无需 main.rs 手动中转
    cache.start_auto_subscribe(
        pool.clone() as Arc<dyn PoolStorage>,
        shutdown_rx.clone(),
    );

    info!("Data Cache 就绪");

    // ===== 初始化 Aggregator (Data Show) =====
    let aggregator = Arc::new(
        tp_aggregator::DataShow::new(
            pool.clone() as Arc<dyn PoolStorage>,
            cache.clone() as Arc<dyn tp_protocol::CacheProvider>,
        )
    );

    // 初始刷新
    if let Err(e) = aggregator.refresh().await {
        warn!(error = %e, "聚合器初始刷新失败");
    }
    // 立即推送初始历史数据给 UI，使页面秒开展现已缓存的历史数据，而不用等待后面的流式采集任务及循环 Tick
    let initial_view = aggregator.subscribe_view().borrow().clone();
    let _ = view_tx.send(initial_view);
    info!("已立即推送初始历史数据到 UI");

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

    // Antigravity VS Code Extension Collector
    let antigravity_root = dirs::home_dir()
        .map(|h| h.join(".gemini").join("antigravity"))
        .unwrap_or_else(|| PathBuf::from("."));
    let antigravity_collector = Arc::new(
        tp_collector_antigravity::AntigravityCollector::new(antigravity_root.clone(), tp_protocol::SourceName::Antigravity)
    );
    antigravity_collector.start_background_ingest();
    coordinator.register(antigravity_collector.clone());

    // Antigravity IDE Collector (纯 RPC 流式采集)
    let antigravity_ide_root = dirs::home_dir()
        .map(|h| h.join(".gemini").join("antigravity-ide"))
        .unwrap_or_else(|| PathBuf::from("."));
    let antigravity_ide_collector = Arc::new(
        tp_collector_antigravity_ide::AntigravityIdeCollector::new(antigravity_ide_root.clone(), tp_protocol::SourceName::AntigravityIDE)
    );
    antigravity_ide_collector.start_background_ingest();
    coordinator.register(antigravity_ide_collector.clone());

    // Codex Collector
    coordinator.register(Arc::new(tp_collector_codex::CodexCollector::new()));

    // Claude Collector
    coordinator.register(Arc::new(tp_collector_claude::ClaudeCollector::new()));

    info!(sources = coordinator.source_count(), "Collectors 注册完成");

    // ===== 数据摄入管道: collect_rx → pool =====
    let mut shutdown_rx_ingest = shutdown_rx.clone();
    let pool_for_ingest = pool.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                res = shutdown_rx_ingest.changed() => {
                    if res.is_ok() && *shutdown_rx_ingest.borrow() {
                        info!("数据摄入任务收到退出信号，正在退出...");
                        break;
                    }
                }
                opt = collect_rx.recv() => {
                    match opt {
                        Some(logs) => {
                            if logs.is_empty() { continue; }
                            match pool_for_ingest.push_datalogs(logs).await {
                                Ok(result) => {
                                    info!(
                                        pushed = result.pushed,
                                        replaced = result.replaced,
                                        skipped = result.skipped,
                                        "数据已推送到 Pool"
                                    );
                                    // Cache 已通过 start_auto_subscribe 自动接收 Pool broadcast，
                                    // 无需手动调用 cache.on_pool_notification()
                                }
                                Err(e) => {
                                    error!(error = %e, "推送数据到 Pool 失败");
                                }
                            }
                        }
                        None => break,
                    }
                }
            }
        }
    });

    // ===== 监听 UI 管道控制指令 =====
    let mut shutdown_rx_cmd = shutdown_rx.clone();
    let pool_for_cmd = pool.clone();
    let cache_for_cmd = cache.clone();
    let aggregator_for_cmd = aggregator.clone();
    let collector_cmd_tx_for_cmd = collector_cmd_tx.clone();
    let antigravity_for_cmd = antigravity_collector.clone();
    let antigravity_ide_for_cmd = antigravity_ide_collector.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                res = shutdown_rx_cmd.changed() => {
                    if res.is_ok() && *shutdown_rx_cmd.borrow() {
                        info!("指令监听任务收到退出信号，正在退出...");
                        break;
                    }
                }
                opt = cmd_rx.recv() => {
                    match opt {
                        Some(cmd) => {
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
                                    // 3. 重置流式采集器内部状态（store + cursors + session offsets）
                                    antigravity_for_cmd.trigger_rebuild();
                                    antigravity_ide_for_cmd.trigger_rebuild();
                                    // 4. 广播重置非流式采集器（Codex/Claude）并从 0 强制拉取
                                    let _ = collector_cmd_tx_for_cmd.send(tp_collector::CollectorCommand::TriggerRebuild);
                                    // 5. 立即刷新展示 (将重置为全空状态)
                                    if let Err(e) = aggregator_for_cmd.request_refresh().await {
                                        error!("Refresh 失败: {:?}", e);
                                    }
                                }
                            }
                        }
                        None => break,
                    }
                }
            }
        }
    });

    // ===== 启动流式采集 (每个数据源独立并行) =====
    let collect_interval = Duration::from_secs(60); // 每分钟采集一次
    let shutdown_rx_streaming = shutdown_rx.clone();
    tokio::spawn(async move {
        coordinator.run_streaming(collect_interval, collector_cmd_tx, shutdown_rx_streaming).await;
    });

    let (warning_tx, warning_rx) = watch::channel::<Option<String>>(None);

    // Spawn memory monitoring thread
    let shutdown_rx_mem = shutdown_rx.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        let mut shutdown = shutdown_rx_mem;
        let mut sys = sysinfo::System::new();
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let mock_warning = std::env::var("TOKEN_PULSE_MOCK_WARNING").ok();
                    let warning_opt = if let Some(_) = mock_warning {
                        Some("⚠️ languageService 内存占用过大 (3.4 GB)".to_string())
                    } else {
                        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, false);
                        let mut total_memory_bytes = 0u64;
                        for (_pid, process) in sys.processes() {
                            let name_opt = process.name().to_str();
                            let is_match = if let Some(name_str) = name_opt {
                                case_insensitive_contains(name_str, "language_server") || case_insensitive_contains(name_str, "antigravity")
                            } else {
                                let name_str = process.name().to_string_lossy();
                                case_insensitive_contains(&name_str, "language_server") || case_insensitive_contains(&name_str, "antigravity")
                            };
                            if is_match {
                                total_memory_bytes += process.memory();
                            }
                        }
                        let total_gb = total_memory_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
                        if total_gb > 3.0 {
                            Some(format!("⚠️ languageService 内存占用过大 ({:.1} GB)", total_gb))
                        } else {
                            None
                        }
                    };
                    let _ = warning_tx.send(warning_opt);
                }
                res = shutdown.changed() => {
                    if res.is_ok() && *shutdown.borrow() {
                        break;
                    }
                }
            }
        }
    });

    info!("数据管道启动完成");

    // 主管道任务: 响应式将 aggregator view 推送到 UI，支持优雅退出
    let mut view_subscriber = aggregator.subscribe_view();
    let mut warning_rx_loop = warning_rx.clone();
    let mut shutdown_rx_loop = shutdown_rx.clone();
    loop {
        tokio::select! {
            // Aggregator view 变更 — 立即推送
            _ = view_subscriber.changed() => {
                let mut current_view = view_subscriber.borrow_and_update().clone();
                current_view.memory_warning = warning_rx_loop.borrow().clone();
                let _ = view_tx.send(current_view);
            }
            // Memory warning 变更 — 混入最新 view 并推送
            _ = warning_rx_loop.changed() => {
                let mut current_view = view_subscriber.borrow().clone();
                current_view.memory_warning = warning_rx_loop.borrow_and_update().clone();
                let _ = view_tx.send(current_view);
            }
            res = shutdown_rx_loop.changed() => {
                if res.is_ok() && *shutdown_rx_loop.borrow() {
                    info!("UI 刷新推送任务收到退出信号，正在退出...");
                    break Ok(());
                }
            }
        }
    }
}

/// 启动诊断
fn run_startup_diagnostics(data_dir: &PathBuf) {
    println!("\x1b[1;36m=== TOKEN PULSE STARTUP DIAGNOSTICS ===\x1b[0m");
    println!("  Data Directory: {:?}", data_dir);
    println!("  Data Dir Exists: {}", data_dir.exists());
    println!("\x1b[1;36m========================================\x1b[0m");
}

use masonry_winit::app::{AppDriver, DriverCtx, MasonryState, WindowId as MasonryWindowId};
use masonry::core::{ErasedAction, WidgetId};
use tray_icon::{TrayIcon, TrayIconBuilder};
use tray_icon::menu::{Menu, MenuItem, MenuId};
use winit::application::ApplicationHandler;
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::window::WindowId as WinitWindowId;
use masonry_winit::app::MasonryUserEvent;

#[derive(Debug)]
struct ShowWindowEvent;

#[derive(Debug)]
struct ExitAppEvent;

struct TrayAppDriver<D> {
    inner: D,
    _main_window_id: MasonryWindowId,
}

impl<D: AppDriver> AppDriver for TrayAppDriver<D> {
    fn on_action(
        &mut self,
        window_id: MasonryWindowId,
        ctx: &mut DriverCtx<'_, '_>,
        widget_id: WidgetId,
        action: ErasedAction,
    ) {
        if action.is::<ShowWindowEvent>() {
            let window = ctx.window(window_id);
            window.handle().set_visible(true);
            window.handle().focus_window();
        } else if action.is::<ExitAppEvent>() {
            ctx.exit();
        } else if action.is::<TelemetryUpdateEvent>() {
            // No-op, used to wake up the event loop
        } else {
            self.inner.on_action(window_id, ctx, widget_id, action);
        }
    }

    fn on_start(&mut self, state: &mut MasonryState<'_>) {
        self.inner.on_start(state);
    }

    fn on_close_requested(&mut self, window_id: MasonryWindowId, ctx: &mut DriverCtx<'_, '_>) {
        let window = ctx.window(window_id);
        window.handle().set_visible(false);
    }
}

fn create_dummy_icon() -> tray_icon::Icon {
    let width = 32;
    let height = 32;
    let mut rgba = vec![0u8; (width * height * 4) as usize];
    for i in 0..rgba.len() / 4 {
        rgba[i * 4] = 0;       // R
        rgba[i * 4 + 1] = 255; // G
        rgba[i * 4 + 2] = 255; // B
        rgba[i * 4 + 3] = 255; // A
    }
    tray_icon::Icon::from_rgba(rgba, width, height).expect("Failed to create dummy icon")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WidgetState {
    SnappedLeft,
    SnappedRight,
    SnappedTop,
    Badge,
    Expanded,
}

fn clamp_position(x: i32, y: i32, width: u32, height: u32, window: &winit::window::Window) -> (i32, i32) {
    if let Some(monitor) = window.current_monitor().or_else(|| window.primary_monitor()) {
        let mon_pos = monitor.position();
        let mon_size = monitor.size();
        let min_x = mon_pos.x;
        let max_x = mon_pos.x + mon_size.width as i32 - width as i32;
        let min_y = mon_pos.y;
        let max_y = mon_pos.y + mon_size.height as i32 - height as i32;
        (x.clamp(min_x, max_x), y.clamp(min_y, max_y))
    } else {
        (x, y)
    }
}

fn case_insensitive_contains(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    haystack.as_bytes().windows(needle.len()).any(|window| {
        window.iter().zip(needle.bytes()).all(|(&h, n)| {
            h.to_ascii_lowercase() == n.to_ascii_lowercase()
        })
    })
}

fn rounded_rect_path(x: f32, y: f32, w: f32, h: f32, r: f32) -> tiny_skia::Path {
    let mut pb = tiny_skia::PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.quad_to(x + w, y, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.quad_to(x + w, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.quad_to(x, y + h, x, y + h - r);
    pb.line_to(x, y + r);
    pb.quad_to(x, y, x + r, y);
    pb.close();
    pb.finish().unwrap()
}

#[derive(Debug)]
struct TelemetryUpdateEvent;

const FONT_8X8: [[u8; 8]; 95] = [
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x18, 0x18, 0x18, 0x18, 0x00, 0x18, 0x00, 0x00],
    [0x66, 0x66, 0x66, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x24, 0x7e, 0x24, 0x7e, 0x24, 0x00, 0x00, 0x00],
    [0x1c, 0x3e, 0x1c, 0x18, 0x3e, 0x1c, 0x18, 0x00],
    [0x62, 0x64, 0x08, 0x10, 0x26, 0x46, 0x00, 0x00],
    [0x1c, 0x22, 0x22, 0x1c, 0x2a, 0x22, 0x1c, 0x00],
    [0x18, 0x18, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x0c, 0x18, 0x30, 0x30, 0x30, 0x18, 0x0c, 0x00],
    [0x30, 0x18, 0x0c, 0x0c, 0x0c, 0x18, 0x30, 0x00],
    [0x00, 0x24, 0x18, 0x7e, 0x18, 0x24, 0x00, 0x00],
    [0x00, 0x18, 0x18, 0x7e, 0x18, 0x18, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00, 0x18, 0x18, 0x08, 0x10],
    [0x00, 0x00, 0x00, 0x7e, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x18, 0x18, 0x00],
    [0x00, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x00],
    [0x3e, 0x66, 0x6e, 0x76, 0x66, 0x66, 0x3e, 0x00],
    [0x18, 0x1c, 0x18, 0x18, 0x18, 0x18, 0x7e, 0x00],
    [0x3e, 0x66, 0x06, 0x0c, 0x18, 0x30, 0x7e, 0x00],
    [0x7e, 0x06, 0x0c, 0x06, 0x06, 0x66, 0x3e, 0x00],
    [0x0c, 0x1c, 0x2c, 0x4c, 0x7e, 0x0c, 0x0c, 0x00],
    [0x7e, 0x60, 0x7c, 0x06, 0x06, 0x66, 0x3e, 0x00],
    [0x3e, 0x60, 0x7c, 0x66, 0x66, 0x66, 0x3e, 0x00],
    [0x7e, 0x66, 0x0c, 0x18, 0x18, 0x18, 0x18, 0x00],
    [0x3e, 0x66, 0x66, 0x3e, 0x66, 0x66, 0x3e, 0x00],
    [0x3e, 0x66, 0x66, 0x3e, 0x06, 0x06, 0x3e, 0x00],
    [0x00, 0x18, 0x18, 0x00, 0x18, 0x18, 0x00, 0x00],
    [0x00, 0x18, 0x18, 0x00, 0x18, 0x18, 0x08, 0x10],
    [0x0c, 0x18, 0x30, 0x60, 0x30, 0x18, 0x0c, 0x00],
    [0x00, 0x00, 0x7e, 0x00, 0x7e, 0x00, 0x00, 0x00],
    [0x30, 0x18, 0x0c, 0x06, 0x0c, 0x18, 0x30, 0x00],
    [0x3e, 0x66, 0x06, 0x0c, 0x18, 0x00, 0x18, 0x00],
    [0x3e, 0x66, 0x6f, 0x6b, 0x6e, 0x60, 0x3e, 0x00],
    [0x18, 0x3c, 0x66, 0x66, 0x7e, 0x66, 0x66, 0x00],
    [0x7c, 0x66, 0x66, 0x7c, 0x66, 0x66, 0x7c, 0x00],
    [0x3e, 0x66, 0x60, 0x60, 0x60, 0x66, 0x3e, 0x00],
    [0x78, 0x6c, 0x66, 0x66, 0x66, 0x6c, 0x78, 0x00],
    [0x7e, 0x60, 0x60, 0x7c, 0x60, 0x60, 0x7e, 0x00],
    [0x7e, 0x60, 0x60, 0x7c, 0x60, 0x60, 0x60, 0x00],
    [0x3e, 0x66, 0x60, 0x6e, 0x66, 0x66, 0x3e, 0x00],
    [0x66, 0x66, 0x66, 0x7e, 0x66, 0x66, 0x66, 0x00],
    [0x3e, 0x0c, 0x0c, 0x0c, 0x0c, 0x0c, 0x3e, 0x00],
    [0x1f, 0x06, 0x06, 0x06, 0x06, 0x66, 0x3c, 0x00],
    [0x66, 0x6c, 0x78, 0x70, 0x78, 0x6c, 0x66, 0x00],
    [0x60, 0x60, 0x60, 0x60, 0x60, 0x60, 0x7e, 0x00],
    [0x63, 0x77, 0x7f, 0x6b, 0x63, 0x63, 0x63, 0x00],
    [0x66, 0x76, 0x7e, 0x7e, 0x6e, 0x66, 0x66, 0x00],
    [0x3e, 0x66, 0x66, 0x66, 0x66, 0x66, 0x3e, 0x00],
    [0x7c, 0x66, 0x66, 0x7c, 0x60, 0x60, 0x60, 0x00],
    [0x3e, 0x66, 0x66, 0x66, 0x66, 0x6c, 0x3e, 0x0c],
    [0x7c, 0x66, 0x66, 0x7c, 0x78, 0x6c, 0x66, 0x00],
    [0x3e, 0x66, 0x60, 0x3e, 0x06, 0x66, 0x3e, 0x00],
    [0x7e, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x00],
    [0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x3e, 0x00],
    [0x66, 0x66, 0x66, 0x66, 0x66, 0x3c, 0x18, 0x00],
    [0x63, 0x63, 0x63, 0x6b, 0x7f, 0x77, 0x63, 0x00],
    [0x66, 0x66, 0x3c, 0x18, 0x3c, 0x66, 0x66, 0x00],
    [0x66, 0x66, 0x66, 0x3c, 0x18, 0x18, 0x18, 0x00],
    [0x7e, 0x06, 0x0c, 0x18, 0x30, 0x60, 0x7e, 0x00],
    [0x3c, 0x30, 0x30, 0x30, 0x30, 0x30, 0x3c, 0x00],
    [0x00, 0x40, 0x20, 0x10, 0x08, 0x04, 0x02, 0x00],
    [0x3c, 0x0c, 0x0c, 0x0c, 0x0c, 0x0c, 0x3c, 0x00],
    [0x18, 0x3c, 0x66, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0x00],
    [0x30, 0x30, 0x18, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x3c, 0x06, 0x3e, 0x66, 0x3e, 0x00],
    [0x60, 0x60, 0x7c, 0x66, 0x66, 0x66, 0x7c, 0x00],
    [0x00, 0x00, 0x3e, 0x60, 0x60, 0x66, 0x3e, 0x00],
    [0x06, 0x06, 0x3e, 0x66, 0x66, 0x66, 0x3e, 0x00],
    [0x00, 0x00, 0x3e, 0x66, 0x7e, 0x60, 0x3e, 0x00],
    [0x1c, 0x30, 0x7c, 0x30, 0x30, 0x30, 0x30, 0x00],
    [0x00, 0x00, 0x3e, 0x66, 0x66, 0x3e, 0x06, 0x3c],
    [0x60, 0x60, 0x7c, 0x66, 0x66, 0x66, 0x66, 0x00],
    [0x18, 0x00, 0x38, 0x18, 0x18, 0x18, 0x3c, 0x00],
    [0x0c, 0x00, 0x1c, 0x0c, 0x0c, 0x0c, 0x0c, 0x38],
    [0x60, 0x60, 0x66, 0x6c, 0x78, 0x6c, 0x66, 0x00],
    [0x38, 0x18, 0x18, 0x18, 0x18, 0x18, 0x3c, 0x00],
    [0x00, 0x00, 0x6c, 0x7e, 0x6b, 0x63, 0x63, 0x00],
    [0x00, 0x00, 0x7c, 0x66, 0x66, 0x66, 0x66, 0x00],
    [0x00, 0x00, 0x3e, 0x66, 0x66, 0x66, 0x3e, 0x00],
    [0x00, 0x00, 0x7c, 0x66, 0x66, 0x7c, 0x60, 0x60],
    [0x00, 0x00, 0x3e, 0x66, 0x66, 0x3e, 0x06, 0x07],
    [0x00, 0x00, 0x76, 0x7c, 0x60, 0x60, 0x60, 0x00],
    [0x00, 0x00, 0x3e, 0x60, 0x3e, 0x06, 0x3c, 0x00],
    [0x30, 0x30, 0xfc, 0x30, 0x30, 0x30, 0x1c, 0x00],
    [0x00, 0x00, 0x66, 0x66, 0x66, 0x66, 0x3e, 0x00],
    [0x00, 0x00, 0x66, 0x66, 0x66, 0x3c, 0x18, 0x00],
    [0x00, 0x00, 0x63, 0x6b, 0x7f, 0x3e, 0x22, 0x00],
    [0x00, 0x00, 0x66, 0x3c, 0x18, 0x3c, 0x66, 0x00],
    [0x00, 0x00, 0x66, 0x66, 0x66, 0x3e, 0x06, 0x3c],
    [0x00, 0x00, 0x7e, 0x0c, 0x18, 0x30, 0x7e, 0x00],
    [0x0e, 0x18, 0x18, 0x70, 0x18, 0x18, 0x0e, 0x00],
    [0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x00],
    [0x70, 0x18, 0x18, 0x0e, 0x18, 0x18, 0x70, 0x00],
    [0x76, 0x89, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
];

fn get_chinese_char_16x16(c: char) -> Option<&'static [u16; 16]> {
    match c {
        '内' => Some(&[
            0x0000, 0x7ffd, 0x4005, 0x4105,
            0x4105, 0x4205, 0x4405, 0x4405,
            0x4805, 0x5005, 0x4005, 0x4005,
            0x4005, 0x4005, 0x7ffd, 0x0000
        ]),
        '存' => Some(&[
            0x0400, 0x0e00, 0x7ffc, 0x0800,
            0x0800, 0x13c0, 0x1080, 0x3080,
            0x7ffd, 0x1080, 0x1080, 0x1080,
            0x1080, 0x10a0, 0x1040, 0x0000
        ]),
        '占' => Some(&[
            0x0100, 0x0100, 0x0100, 0x7ffc,
            0x0100, 0x0100, 0x1f00, 0x1100,
            0x1100, 0x1100, 0x1100, 0x1100,
            0x1100, 0x1f00, 0x0000, 0x0000
        ]),
        '用' => Some(&[
            0x0000, 0x7ffe, 0x4002, 0x4002,
            0x4002, 0x4002, 0x7ffe, 0x4002,
            0x4002, 0x4002, 0x4002, 0x7ffe,
            0x4002, 0x4002, 0x4002, 0x0000
        ]),
        '过' => Some(&[
            0x0100, 0x0100, 0x7ffe, 0x0100,
            0x1240, 0x1240, 0x127c, 0x13c0,
            0x1240, 0x1444, 0x1044, 0x1844,
            0x3c4c, 0x7ffd, 0x0010, 0x0000
        ]),
        '大' => Some(&[
            0x0100, 0x0100, 0x7ffe, 0x0100,
            0x0100, 0x0280, 0x0280, 0x0440,
            0x0440, 0x0820, 0x1010, 0x2008,
            0x4004, 0x8002, 0x0000, 0x0000
        ]),
        _ => None,
    }
}

fn draw_text(
    pixmap: &mut tiny_skia::Pixmap,
    text: &str,
    mut x: f32,
    y: f32,
    color: tiny_skia::Color,
    scale: f32,
    ts: tiny_skia::Transform,
) {
    let mut paint = tiny_skia::Paint::default();
    paint.set_color(color);
    paint.anti_alias = false;

    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == ' ' {
            x += 6.0 * scale;
            continue;
        }

        if let Some(bitmap) = get_chinese_char_16x16(c) {
            for row in 0..16 {
                let row_val = bitmap[row];
                for col in 0..16 {
                    if (row_val & (0x8000 >> col)) != 0 {
                        let px = x + col as f32 * scale;
                        let py = y + row as f32 * scale;
                        if let Some(rect) = tiny_skia::Rect::from_xywh(px, py, scale, scale) {
                            pixmap.fill_rect(rect, &paint, ts, None);
                        }
                    }
                }
            }
            x += 18.0 * scale;
        } else {
            let idx = (c as usize).saturating_sub(32);
            if idx < 96 {
                let bitmap = &FONT_8X8[idx];
                for row in 0..8 {
                    let row_val = bitmap[row];
                    for col in 0..8 {
                        if (row_val & (0x80 >> col)) != 0 {
                            let px = x + col as f32 * scale;
                            let py = y + (row as f32 + 4.0) * scale;
                            if let Some(rect) = tiny_skia::Rect::from_xywh(px, py, scale, scale) {
                                pixmap.fill_rect(rect, &paint, ts, None);
                            }
                        }
                    }
                }
            }
            x += 8.0 * scale;
        }
    }
}

struct FloatingWindowResources {
    _context: softbuffer::Context<&'static winit::window::Window>,
    surface: softbuffer::Surface<&'static winit::window::Window, &'static winit::window::Window>,
}

struct ResizeAnimation {
    start_time: std::time::Instant,
    duration: std::time::Duration,
    start_pos: winit::dpi::PhysicalPosition<i32>,
    target_pos: winit::dpi::PhysicalPosition<i32>,
    start_size: winit::dpi::LogicalSize<f64>,
    target_size: winit::dpi::LogicalSize<f64>,
}

struct ExternalApp {
    masonry_state: MasonryState<'static>,
    app_driver: Box<dyn AppDriver>,
    proxy: EventLoopProxy<MasonryUserEvent>,
    main_window_id: MasonryWindowId,
    _tray_icon: Option<TrayIcon>,
    show_item_id: MenuId,
    float_item_id: MenuId,
    exit_item_id: MenuId,
    floating_window: Option<winit::window::Window>,
    floating_visible: bool,
    view_rx: watch::Receiver<tp_protocol::view::DashboardView>,
    widget_state: WidgetState,
    hover_start_time: Option<std::time::Instant>,
    hover_off_time: Option<std::time::Instant>,
    drag_active: bool,
    last_cursor_pos: Option<(f64, f64)>,
    is_mouse_down: bool,
    accumulated_drag_delta: (f64, f64),
    previous_state: Option<WidgetState>,
    floating_resources: Option<FloatingWindowResources>,
    tooltip_window: Option<winit::window::Window>,
    resize_animation: Option<ResizeAnimation>,
    float_needs_redraw: bool,
}

fn get_snapped_layout(state: WidgetState, w: &winit::window::Window) -> Option<(winit::dpi::PhysicalPosition<i32>, winit::dpi::LogicalSize<f64>)> {
    let monitor = w.current_monitor().or_else(|| w.primary_monitor())?;
    let mon_pos = monitor.position();
    let mon_size = monitor.size();
    let win_pos = w.outer_position().unwrap_or_default();
    let scale_factor = w.scale_factor();
    
    match state {
        WidgetState::SnappedLeft => {
            let pos = winit::dpi::PhysicalPosition::new(mon_pos.x, win_pos.y);
            let size = winit::dpi::LogicalSize::new(12.0, 64.0);
            Some((pos, size))
        }
        WidgetState::SnappedRight => {
            let phys_handle = (12.0 * scale_factor) as i32;
            let pos = winit::dpi::PhysicalPosition::new(mon_pos.x + mon_size.width as i32 - phys_handle, win_pos.y);
            let size = winit::dpi::LogicalSize::new(12.0, 64.0);
            Some((pos, size))
        }
        WidgetState::SnappedTop => {
            let pos = winit::dpi::PhysicalPosition::new(win_pos.x, mon_pos.y);
            let size = winit::dpi::LogicalSize::new(64.0, 12.0);
            Some((pos, size))
        }
        WidgetState::Badge => {
            let size = winit::dpi::LogicalSize::new(64.0, 64.0);
            Some((win_pos, size))
        }
        WidgetState::Expanded => None,
    }
}

fn get_revealed_layout(state: WidgetState, w: &winit::window::Window) -> Option<(winit::dpi::PhysicalPosition<i32>, winit::dpi::LogicalSize<f64>)> {
    let monitor = w.current_monitor().or_else(|| w.primary_monitor())?;
    let mon_pos = monitor.position();
    let mon_size = monitor.size();
    let win_pos = w.outer_position().unwrap_or_default();
    let scale_factor = w.scale_factor();
    
    match state {
        WidgetState::SnappedLeft => {
            let pos = winit::dpi::PhysicalPosition::new(mon_pos.x, win_pos.y);
            let size = winit::dpi::LogicalSize::new(64.0, 64.0);
            Some((pos, size))
        }
        WidgetState::SnappedRight => {
            let phys_badge = (64.0 * scale_factor) as i32;
            let pos = winit::dpi::PhysicalPosition::new(mon_pos.x + mon_size.width as i32 - phys_badge, win_pos.y);
            let size = winit::dpi::LogicalSize::new(64.0, 64.0);
            Some((pos, size))
        }
        WidgetState::SnappedTop => {
            let pos = winit::dpi::PhysicalPosition::new(win_pos.x, mon_pos.y);
            let size = winit::dpi::LogicalSize::new(64.0, 64.0);
            Some((pos, size))
        }
        WidgetState::Badge => {
            let size = winit::dpi::LogicalSize::new(64.0, 64.0);
            Some((win_pos, size))
        }
        WidgetState::Expanded => {
            let size = winit::dpi::LogicalSize::new(320.0, 480.0);
            Some((win_pos, size))
        }
    }
}

impl ExternalApp {
    fn get_or_create_resources(&mut self) -> Option<&mut FloatingWindowResources> {
        if self.floating_resources.is_some() {
            return self.floating_resources.as_mut();
        }

        let window = self.floating_window.as_ref()?;
        let static_window: &'static winit::window::Window = unsafe { &*(window as *const winit::window::Window) };

        let context = match softbuffer::Context::new(static_window) {
            Ok(c) => c,
            Err(e) => {
                error!("Failed to create cached softbuffer context: {:?}", e);
                return None;
            }
        };
        let surface = match softbuffer::Surface::new(&context, static_window) {
            Ok(s) => s,
            Err(e) => {
                error!("Failed to create cached softbuffer surface: {:?}", e);
                return None;
            }
        };

        self.floating_resources = Some(FloatingWindowResources { _context: context, surface });
        self.floating_resources.as_mut()
    }

    fn show_tooltip(&mut self, event_loop: &ActiveEventLoop) {
        if self.tooltip_window.is_some() {
            return;
        }
        let (win_pos, scale_factor) = if let Some(ref w) = self.floating_window {
            (w.outer_position().unwrap_or_default(), w.scale_factor())
        } else {
            return;
        };

        let text = {
            let view = self.view_rx.borrow();
            if let Some(ref warn) = view.memory_warning {
                warn.trim_start_matches("⚠️").trim().to_string()
            } else {
                "Token Pulse: OK".to_string()
            }
        };

        let mut text_w = 0.0;
        for c in text.chars() {
            if get_chinese_char_16x16(c).is_some() {
                text_w += 18.0;
            } else {
                text_w += 8.0;
            }
        }
        let logical_w = if text_w + 30.0 > 120.0 { text_w + 30.0 } else { 120.0 } as f64;
        let logical_h = 32.0;

        let tooltip_attrs = winit::window::Window::default_attributes()
            .with_title("Token Pulse Tooltip")
            .with_decorations(false)
            .with_inner_size(winit::dpi::LogicalSize::new(logical_w, logical_h))
            .with_window_level(winit::window::WindowLevel::AlwaysOnTop)
            .with_visible(false);
        #[cfg(target_os = "windows")]
        let tooltip_attrs = tooltip_attrs.with_skip_taskbar(true);

        let tooltip = match event_loop.create_window(tooltip_attrs) {
            Ok(w) => w,
            Err(e) => {
                error!("Failed to create tooltip window: {:?}", e);
                return;
            }
        };

        let (cx, cy) = self.last_cursor_pos.unwrap_or((32.0 * scale_factor, 32.0 * scale_factor));
        let cursor_screen_x = win_pos.x + cx as i32;
        let cursor_screen_y = win_pos.y + cy as i32;
        tooltip.set_outer_position(winit::dpi::PhysicalPosition::new(
            cursor_screen_x + 12,
            cursor_screen_y + 12
        ));
        tooltip.set_visible(true);

        self.tooltip_window = Some(tooltip);
        self.draw_tooltip_window();
    }

    fn hide_tooltip(&mut self) {
        if self.tooltip_window.is_some() {
            self.tooltip_window = None;
        }
    }

    fn draw_tooltip_window(&mut self) {
        let window = match &self.tooltip_window {
            Some(w) => w,
            None => return,
        };
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            return;
        }

        let context = match softbuffer::Context::new(window) {
            Ok(c) => c,
            Err(e) => {
                error!("Failed to create tooltip softbuffer context: {:?}", e);
                return;
            }
        };
        let mut surface = match softbuffer::Surface::new(&context, window) {
            Ok(s) => s,
            Err(e) => {
                error!("Failed to create tooltip softbuffer surface: {:?}", e);
                return;
            }
        };

        if let Err(e) = surface.resize(
            std::num::NonZeroU32::new(size.width).unwrap(),
            std::num::NonZeroU32::new(size.height).unwrap(),
        ) {
            error!("Failed to resize tooltip softbuffer surface: {:?}", e);
            return;
        }

        let mut buffer = match surface.buffer_mut() {
            Ok(b) => b,
            Err(e) => {
                error!("Failed to get tooltip softbuffer surface buffer: {:?}", e);
                return;
            }
        };

        let mut pixmap = tiny_skia::Pixmap::new(size.width, size.height).unwrap();
        pixmap.fill(tiny_skia::Color::from_rgba8(31, 41, 55, 230));

        let scale_factor = window.scale_factor() as f32;
        let ts = tiny_skia::Transform::from_scale(scale_factor, scale_factor);

        let view = self.view_rx.borrow();
        let (text, is_warning) = if let Some(ref warn) = view.memory_warning {
            (warn.trim_start_matches("⚠️").trim().to_string(), true)
        } else {
            ("Token Pulse: OK".to_string(), false)
        };

        let border_color = if is_warning {
            tiny_skia::Color::from_rgba8(245, 158, 11, 255)
        } else {
            tiny_skia::Color::from_rgba8(59, 130, 246, 255)
        };

        let rect_path = rounded_rect_path(1.0, 1.0, (size.width as f32 / scale_factor) - 2.0, (size.height as f32 / scale_factor) - 2.0, 4.0);
        let mut border_paint = tiny_skia::Paint::default();
        border_paint.set_color(border_color);
        let mut stroke = tiny_skia::Stroke::default();
        stroke.width = 1.0;
        pixmap.stroke_path(&rect_path, &border_paint, &stroke, ts, None);

        let text_color = if is_warning {
            tiny_skia::Color::from_rgba8(254, 243, 199, 255)
        } else {
            tiny_skia::Color::from_rgba8(243, 244, 246, 255)
        };

        draw_text(&mut pixmap, &text, 10.0, 8.0, text_color, 1.0, ts);

        let pixels = pixmap.data();
        for (i, chunk) in pixels.chunks_exact(4).enumerate() {
            let r = chunk[0];
            let g = chunk[1];
            let b = chunk[2];
            let a = chunk[3];
            let val = ((a as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32);
            buffer[i] = val;
        }

        if let Err(e) = buffer.present() {
            error!("Failed to present tooltip softbuffer buffer: {:?}", e);
        }
    }

    fn start_resize_animation(&mut self, target_pos: winit::dpi::PhysicalPosition<i32>, target_size: winit::dpi::LogicalSize<f64>) {
        if let Some(ref w) = self.floating_window {
            let start_pos = w.outer_position().unwrap_or_default();
            let start_size = winit::dpi::LogicalSize::from_physical(w.inner_size(), w.scale_factor());
            self.resize_animation = Some(ResizeAnimation {
                start_time: std::time::Instant::now(),
                duration: std::time::Duration::from_millis(150),
                start_pos,
                target_pos,
                start_size,
                target_size,
            });
            w.request_redraw();
        }
    }

    fn step_resize_animation(&mut self) -> bool {
        let mut finished = false;
        let mut next_pos = None;
        let mut next_size = None;
        let was_active = self.resize_animation.is_some();

        if let Some(ref anim) = self.resize_animation {
            let elapsed = anim.start_time.elapsed();
            if elapsed >= anim.duration {
                finished = true;
                next_pos = Some(anim.target_pos);
                next_size = Some(anim.target_size);
            } else {
                let t = elapsed.as_secs_f64() / anim.duration.as_secs_f64();
                let t_smooth = t * t * (3.0 - 2.0 * t); // smoothstep
                
                let cur_x = anim.start_pos.x + ((anim.target_pos.x - anim.start_pos.x) as f64 * t_smooth) as i32;
                let cur_y = anim.start_pos.y + ((anim.target_pos.y - anim.start_pos.y) as f64 * t_smooth) as i32;
                
                let cur_w = anim.start_size.width + (anim.target_size.width - anim.start_size.width) * t_smooth;
                let cur_h = anim.start_size.height + (anim.target_size.height - anim.start_size.height) * t_smooth;
                
                next_pos = Some(winit::dpi::PhysicalPosition::new(cur_x, cur_y));
                next_size = Some(winit::dpi::LogicalSize::new(cur_w, cur_h));
            }
        }

        if let Some(pos) = next_pos {
            if let Some(ref w) = self.floating_window {
                w.set_outer_position(pos);
            }
        }
        if let Some(size) = next_size {
            if let Some(ref w) = self.floating_window {
                let _ = w.request_inner_size(size);
            }
        }

        if finished {
            self.resize_animation = None;
        }

        was_active
    }

    fn toggle_floating_window(&mut self, event_loop: &ActiveEventLoop) {
        if self.floating_window.is_none() {
            let monitor = event_loop.primary_monitor().or_else(|| event_loop.available_monitors().next());
            let initial_pos = if let Some(mon) = monitor {
                let size = mon.size();
                let pos = mon.position();
                winit::dpi::PhysicalPosition::new(pos.x + size.width as i32 - 164, pos.y + 100)
            } else {
                winit::dpi::PhysicalPosition::new(800, 100)
            };

            let attrs = winit::window::Window::default_attributes()
                .with_title("Token Pulse Widget")
                .with_decorations(false)
                .with_transparent(true)
                .with_window_level(winit::window::WindowLevel::AlwaysOnTop)
                .with_inner_size(winit::dpi::LogicalSize::new(64.0, 64.0))
                .with_position(initial_pos)
                .with_visible(true);
            #[cfg(target_os = "windows")]
            let attrs = attrs.with_skip_taskbar(true);
            match event_loop.create_window(attrs) {
                Ok(w) => {
                    self.floating_window = Some(w);
                    self.floating_visible = true;
                    self.widget_state = WidgetState::Badge;
                    info!("Floating window created and shown");
                }
                Err(e) => {
                    error!("Failed to create floating window: {:?}", e);
                }
            }
        } else {
            let visible = !self.floating_visible;
            if let Some(ref w) = self.floating_window {
                w.set_visible(visible);
            }
            self.floating_visible = visible;
            info!("Floating window visibility toggled to: {}", visible);
        }
    }

    fn draw_floating_window(&mut self) {
        let (size, scale_factor) = if let Some(ref w) = self.floating_window {
            let s = w.inner_size();
            if s.width == 0 || s.height == 0 {
                return;
            }
            (s, w.scale_factor() as f32)
        } else {
            return;
        };

        let mut pixmap = tiny_skia::Pixmap::new(size.width, size.height).unwrap();
        pixmap.fill(tiny_skia::Color::TRANSPARENT);
        self.render_widget(&mut pixmap, size.width, size.height, scale_factor);

        let resources = match self.get_or_create_resources() {
            Some(r) => r,
            None => return,
        };

        if let Err(e) = resources.surface.resize(
            std::num::NonZeroU32::new(size.width).unwrap(),
            std::num::NonZeroU32::new(size.height).unwrap(),
        ) {
            error!("Failed to resize softbuffer surface: {:?}", e);
            return;
        }

        let mut buffer = match resources.surface.buffer_mut() {
            Ok(b) => b,
            Err(e) => {
                error!("Failed to get softbuffer surface buffer: {:?}", e);
                return;
            }
        };

        let pixels = pixmap.data();
        for (i, chunk) in pixels.chunks_exact(4).enumerate() {
            let r = chunk[0];
            let g = chunk[1];
            let b = chunk[2];
            let a = chunk[3];
            let val = ((a as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32);
            buffer[i] = val;
        }

        if let Err(e) = buffer.present() {
            error!("Failed to present softbuffer buffer: {:?}", e);
        }
    }

    fn render_widget(&self, pixmap: &mut tiny_skia::Pixmap, width: u32, height: u32, scale_factor: f32) {
        let view = self.view_rx.borrow();
        let has_warning = view.memory_warning.is_some();
        let logical_w = width as f32 / scale_factor;
        let logical_h = height as f32 / scale_factor;
        let ts = tiny_skia::Transform::from_scale(scale_factor, scale_factor);
        
        match self.widget_state {
            WidgetState::SnappedLeft | WidgetState::SnappedRight => {
                let mut paint = tiny_skia::Paint::default();
                paint.anti_alias = true;
                if has_warning {
                    paint.set_color(tiny_skia::Color::from_rgba8(239, 68, 68, 255));
                } else {
                    paint.set_color(tiny_skia::Color::from_rgba8(75, 85, 99, 180));
                }
                
                let rect = tiny_skia::Rect::from_xywh(0.0, 0.0, logical_w, logical_h).unwrap();
                pixmap.fill_rect(rect, &paint, ts, None);
                
                let mut indicator = tiny_skia::Paint::default();
                indicator.anti_alias = true;
                if has_warning {
                    indicator.set_color(tiny_skia::Color::from_rgba8(245, 158, 11, 255));
                } else {
                    indicator.set_color(tiny_skia::Color::from_rgba8(16, 185, 129, 255));
                }
                let line_rect = tiny_skia::Rect::from_xywh(4.0, 12.0, 4.0, 40.0).unwrap();
                pixmap.fill_rect(line_rect, &indicator, ts, None);
            }
            WidgetState::SnappedTop => {
                let mut paint = tiny_skia::Paint::default();
                paint.anti_alias = true;
                if has_warning {
                    paint.set_color(tiny_skia::Color::from_rgba8(239, 68, 68, 255));
                } else {
                    paint.set_color(tiny_skia::Color::from_rgba8(75, 85, 99, 180));
                }
                let rect = tiny_skia::Rect::from_xywh(0.0, 0.0, logical_w, logical_h).unwrap();
                pixmap.fill_rect(rect, &paint, ts, None);
                
                let mut indicator = tiny_skia::Paint::default();
                indicator.anti_alias = true;
                if has_warning {
                    indicator.set_color(tiny_skia::Color::from_rgba8(245, 158, 11, 255));
                } else {
                    indicator.set_color(tiny_skia::Color::from_rgba8(16, 185, 129, 255));
                }
                let line_rect = tiny_skia::Rect::from_xywh(12.0, 4.0, 40.0, 4.0).unwrap();
                pixmap.fill_rect(line_rect, &indicator, ts, None);
            }
            WidgetState::Badge => {
                let border_color = if has_warning {
                    tiny_skia::Color::from_rgba8(239, 68, 68, 255)
                } else {
                    tiny_skia::Color::from_rgba8(59, 130, 246, 255)
                };
                
                let mut paint = tiny_skia::Paint::default();
                paint.anti_alias = true;
                paint.set_color(border_color);
                let outer_circle = tiny_skia::PathBuilder::from_circle(32.0, 32.0, 30.0).unwrap();
                pixmap.fill_path(&outer_circle, &paint, tiny_skia::FillRule::Winding, ts, None);
                
                let inner_color = tiny_skia::Color::from_rgba8(31, 41, 55, 255);
                paint.set_color(inner_color);
                let inner_circle = tiny_skia::PathBuilder::from_circle(32.0, 32.0, 26.0).unwrap();
                pixmap.fill_path(&inner_circle, &paint, tiny_skia::FillRule::Winding, ts, None);
                
                if has_warning {
                    let mut warning_paint = tiny_skia::Paint::default();
                    warning_paint.anti_alias = true;
                    warning_paint.set_color(tiny_skia::Color::from_rgba8(245, 158, 11, 255));
                    let mut path = tiny_skia::PathBuilder::new();
                    path.move_to(32.0, 18.0);
                    path.line_to(44.0, 42.0);
                    path.line_to(20.0, 42.0);
                    path.close();
                    let path = path.finish().unwrap();
                    pixmap.fill_path(&path, &warning_paint, tiny_skia::FillRule::Winding, ts, None);
                    
                    warning_paint.set_color(tiny_skia::Color::from_rgba8(31, 41, 55, 255));
                    let rect1 = tiny_skia::Rect::from_xywh(31.0, 26.0, 2.0, 8.0).unwrap();
                    pixmap.fill_rect(rect1, &warning_paint, ts, None);
                    let rect2 = tiny_skia::Rect::from_xywh(31.0, 37.0, 2.0, 2.0).unwrap();
                    pixmap.fill_rect(rect2, &warning_paint, ts, None);
                } else {
                    let mut token_paint = tiny_skia::Paint::default();
                    token_paint.anti_alias = true;
                    token_paint.set_color(tiny_skia::Color::from_rgba8(16, 185, 129, 255));
                    
                    let coin = tiny_skia::PathBuilder::from_circle(32.0, 32.0, 14.0).unwrap();
                    pixmap.fill_path(&coin, &token_paint, tiny_skia::FillRule::Winding, ts, None);
                    
                    token_paint.set_color(inner_color);
                    let hole = tiny_skia::PathBuilder::from_circle(32.0, 32.0, 6.0).unwrap();
                    pixmap.fill_path(&hole, &token_paint, tiny_skia::FillRule::Winding, ts, None);
                }
            }
            WidgetState::Expanded => {
                let mut paint = tiny_skia::Paint::default();
                paint.anti_alias = true;
                paint.set_color(tiny_skia::Color::from_rgba8(17, 24, 39, 245));
                
                let rect_path = rounded_rect_path(0.0, 0.0, logical_w, logical_h, 12.0);
                pixmap.fill_path(&rect_path, &paint, tiny_skia::FillRule::Winding, ts, None);
                
                let border_color = if has_warning {
                    tiny_skia::Color::from_rgba8(239, 68, 68, 200)
                } else {
                    tiny_skia::Color::from_rgba8(75, 85, 99, 150)
                };
                let mut stroke = tiny_skia::Stroke::default();
                stroke.width = 1.5;
                paint.set_color(border_color);
                pixmap.stroke_path(&rect_path, &paint, &stroke, ts, None);
                
                paint.set_color(tiny_skia::Color::from_rgba8(31, 41, 55, 255));
                let header_rect = rounded_rect_path(0.0, 0.0, logical_w, 50.0, 12.0);
                pixmap.fill_path(&header_rect, &paint, tiny_skia::FillRule::Winding, ts, None);
                let cover_rect = tiny_skia::Rect::from_xywh(0.0, 38.0, logical_w, 12.0).unwrap();
                pixmap.fill_rect(cover_rect, &paint, ts, None);
                
                let mut icon_paint = tiny_skia::Paint::default();
                icon_paint.anti_alias = true;
                icon_paint.set_color(tiny_skia::Color::from_rgba8(59, 130, 246, 255));
                let logo_path = tiny_skia::PathBuilder::from_circle(24.0, 25.0, 8.0).unwrap();
                pixmap.fill_path(&logo_path, &icon_paint, tiny_skia::FillRule::Winding, ts, None);
                
                // Render real title "Token Pulse" in header
                draw_text(pixmap, "Token Pulse", 40.0, 17.0, tiny_skia::Color::from_rgba8(243, 244, 246, 255), 1.0, ts);
                
                let btn_x = logical_w - 32.0;
                let btn_y = 17.0;
                let mut btn_paint = tiny_skia::Paint::default();
                btn_paint.anti_alias = true;
                btn_paint.set_color(tiny_skia::Color::from_rgba8(239, 68, 68, 255));
                let dot_path = tiny_skia::PathBuilder::from_circle(btn_x + 8.0, btn_y + 8.0, 8.0).unwrap();
                pixmap.fill_path(&dot_path, &btn_paint, tiny_skia::FillRule::Winding, ts, None);
                
                // Draw a small 'x' character inside the red close button
                draw_text(pixmap, "x", btn_x + 5.0, btn_y + 1.0, tiny_skia::Color::from_rgba8(255, 255, 255, 255), 1.0, ts);
                
                let mut y_offset = 60.0;
                let mut bar_paint = tiny_skia::Paint::default();
                bar_paint.anti_alias = true;

                if let Some(ref warn_msg) = view.memory_warning {
                    paint.set_color(tiny_skia::Color::from_rgba8(127, 29, 29, 230));
                    let warn_rect = rounded_rect_path(12.0, y_offset, logical_w - 24.0, 50.0, 6.0);
                    pixmap.fill_path(&warn_rect, &paint, tiny_skia::FillRule::Winding, ts, None);
                    
                    let mut warn_stroke = tiny_skia::Stroke::default();
                    warn_stroke.width = 1.0;
                    paint.set_color(tiny_skia::Color::from_rgba8(245, 158, 11, 200));
                    pixmap.stroke_path(&warn_rect, &paint, &warn_stroke, ts, None);
                    
                    let mut warning_icon_paint = tiny_skia::Paint::default();
                    warning_icon_paint.anti_alias = true;
                    warning_icon_paint.set_color(tiny_skia::Color::from_rgba8(245, 158, 11, 255));
                    let mut path = tiny_skia::PathBuilder::new();
                    path.move_to(28.0, y_offset + 15.0);
                    path.line_to(36.0, y_offset + 35.0);
                    path.line_to(20.0, y_offset + 35.0);
                    path.close();
                    let path = path.finish().unwrap();
                    pixmap.fill_path(&path, &warning_icon_paint, tiny_skia::FillRule::Winding, ts, None);
                    
                    let clean_warn = warn_msg.trim_start_matches("⚠️").trim();
                    draw_text(pixmap, "WARN:", 45.0, y_offset + 10.0, tiny_skia::Color::from_rgba8(252, 211, 77, 255), 1.0, ts);
                    draw_text(pixmap, clean_warn, 45.0, y_offset + 26.0, tiny_skia::Color::from_rgba8(254, 243, 199, 255), 0.8, ts);
                    
                    y_offset += 62.0;
                }
                
                paint.set_color(tiny_skia::Color::from_rgba8(31, 41, 55, 180));
                let stats_rect = rounded_rect_path(12.0, y_offset, logical_w - 24.0, 80.0, 8.0);
                pixmap.fill_path(&stats_rect, &paint, tiny_skia::FillRule::Winding, ts, None);
                
                let today_in = view.today_tokens.input;
                let today_out = view.today_tokens.output;
                let today_total = view.today_tokens.total();
                let today_cost = view.today_cost;
                
                // Draw "TODAY STATS" header
                draw_text(pixmap, "TODAY STATS", 24.0, y_offset + 8.0, tiny_skia::Color::from_rgba8(156, 163, 175, 255), 0.8, ts);
                
                // Draw total token count and cost
                let stats_str = format!("Tokens: {}  Cost: ${:.3}", today_total, today_cost);
                draw_text(pixmap, &stats_str, 24.0, y_offset + 22.0, tiny_skia::Color::from_rgba8(243, 244, 246, 255), 1.0, ts);
                
                let max_width = logical_w - 48.0;
                let input_ratio = if today_total > 0 { today_in as f32 / today_total as f32 } else { 0.5 };
                let input_width = max_width * input_ratio;
                
                bar_paint.set_color(tiny_skia::Color::from_rgba8(59, 130, 246, 255));
                let in_bar = tiny_skia::Rect::from_xywh(24.0, y_offset + 42.0, input_width, 8.0).unwrap();
                pixmap.fill_rect(in_bar, &bar_paint, ts, None);
                
                bar_paint.set_color(tiny_skia::Color::from_rgba8(16, 185, 129, 255));
                let out_bar = tiny_skia::Rect::from_xywh(24.0 + input_width, y_offset + 42.0, max_width - input_width, 8.0).unwrap();
                pixmap.fill_rect(out_bar, &bar_paint, ts, None);
                
                // Draw legend text instead of color bars
                let leg_in_str = format!("Input: {}", today_in);
                let leg_out_str = format!("Output: {}", today_out);
                draw_text(pixmap, &leg_in_str, 24.0, y_offset + 58.0, tiny_skia::Color::from_rgba8(59, 130, 246, 255), 0.8, ts);
                draw_text(pixmap, &leg_out_str, 150.0, y_offset + 58.0, tiny_skia::Color::from_rgba8(16, 185, 129, 255), 0.8, ts);
                
                y_offset += 92.0;
                
                paint.set_color(tiny_skia::Color::from_rgba8(31, 41, 55, 180));
                let breakdown_rect = rounded_rect_path(12.0, y_offset, logical_w - 24.0, 230.0, 8.0);
                pixmap.fill_path(&breakdown_rect, &paint, tiny_skia::FillRule::Winding, ts, None);
                
                draw_text(pixmap, "BY MODEL", 24.0, y_offset + 10.0, tiny_skia::Color::from_rgba8(156, 163, 175, 255), 0.8, ts);
                
                let mut item_y = y_offset + 28.0;
                let mut entries = view.by_model.clone();
                entries.sort_by(|a, b| b.token_info.total().cmp(&a.token_info.total()));
                
                if entries.is_empty() {
                    draw_text(pixmap, "No data available", 24.0, item_y + 10.0, tiny_skia::Color::from_rgba8(156, 163, 175, 255), 1.0, ts);
                } else {
                    for entry in entries.iter().take(5) {
                        let mut dot_paint = tiny_skia::Paint::default();
                        dot_paint.anti_alias = true;
                        dot_paint.set_color(tiny_skia::Color::from_rgba8(139, 92, 246, 255));
                        let dot = tiny_skia::PathBuilder::from_circle(30.0, item_y + 7.0, 3.0).unwrap();
                        pixmap.fill_path(&dot, &dot_paint, tiny_skia::FillRule::Winding, ts, None);
                        
                        // Clean model name for display
                        let display_name = if entry.key.len() > 15 {
                            format!("{}...", &entry.key[..12])
                        } else {
                            entry.key.clone()
                        };
                        draw_text(pixmap, &display_name, 40.0, item_y + 2.0, tiny_skia::Color::from_rgba8(209, 213, 219, 255), 0.8, ts);
                        
                        // Draw token count
                        let token_count = entry.token_info.total();
                        let token_str = if token_count >= 1_000_000 {
                            format!("{:.1}M", token_count as f32 / 1_000_000.0)
                        } else if token_count >= 1_000 {
                            format!("{:.1}k", token_count as f32 / 1000.0)
                        } else {
                            format!("{}", token_count)
                        };
                        
                        let val_ratio = if today_total > 0 { entry.token_info.total() as f32 / today_total as f32 } else { 0.5 };
                        let val_w = 40.0 * val_ratio.clamp(0.1, 1.0);
                        
                        bar_paint.set_color(tiny_skia::Color::from_rgba8(139, 92, 246, 180));
                        let val_bar = tiny_skia::Rect::from_xywh(logical_w - 90.0 - val_w, item_y + 6.0, val_w, 4.0).unwrap();
                        pixmap.fill_rect(val_bar, &bar_paint, ts, None);
                        
                        draw_text(pixmap, &token_str, logical_w - 45.0, item_y + 2.0, tiny_skia::Color::from_rgba8(243, 244, 246, 255), 0.8, ts);
                        
                        item_y += 24.0;
                    }
                }
            }
        }
    }

    fn collapse_widget(&mut self) {
        if self.widget_state == WidgetState::Expanded {
            let previous = self.previous_state.take().unwrap_or(WidgetState::Badge);
            self.widget_state = previous;
            if let Some(ref w) = self.floating_window {
                if let Some((pos, size)) = get_snapped_layout(self.widget_state, w) {
                    self.start_resize_animation(pos, size);
                } else {
                    w.request_redraw();
                }
            }
        }
    }
}

impl ApplicationHandler<MasonryUserEvent> for ExternalApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.masonry_state.handle_resumed(event_loop, &mut *self.app_driver);
        if self.floating_window.is_none() {
            self.toggle_floating_window(event_loop);
        }
    }

    fn suspended(&mut self, event_loop: &ActiveEventLoop) {
        self.masonry_state.handle_suspended(event_loop);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WinitWindowId,
        event: winit::event::WindowEvent,
    ) {
        let is_tooltip = self.tooltip_window.as_ref().map(|w| w.id() == window_id).unwrap_or(false);
        if is_tooltip {
            if matches!(event, winit::event::WindowEvent::RedrawRequested) {
                self.draw_tooltip_window();
            }
            return;
        }

        let is_floating = self.floating_window.as_ref().map(|w| w.id() == window_id).unwrap_or(false);
        if is_floating {
            match &event {
                winit::event::WindowEvent::RedrawRequested => {
                    self.draw_floating_window();
                }
                winit::event::WindowEvent::Resized(_) => {
                    if let Some(ref w) = self.floating_window {
                        w.request_redraw();
                    }
                }
                winit::event::WindowEvent::CursorEntered { .. } => {
                    self.hover_start_time = Some(std::time::Instant::now());
                    self.hover_off_time = None;
                    if self.widget_state != WidgetState::Expanded {
                        self.show_tooltip(event_loop);
                    }
                }
                winit::event::WindowEvent::CursorLeft { .. } => {
                    self.hover_off_time = Some(std::time::Instant::now());
                    self.hover_start_time = None;
                    self.hide_tooltip();
                }
                winit::event::WindowEvent::Focused(focused) => {
                    if !focused && self.widget_state == WidgetState::Expanded {
                        self.collapse_widget();
                    }
                }
                winit::event::WindowEvent::MouseInput { state, button, .. } => {
                    if *button == winit::event::MouseButton::Left {
                        if self.floating_window.is_some() {
                            if *state == winit::event::ElementState::Pressed {
                                let w = self.floating_window.as_ref().unwrap();
                                match self.widget_state {
                                    WidgetState::Badge | WidgetState::SnappedLeft | WidgetState::SnappedRight | WidgetState::SnappedTop => {
                                        self.is_mouse_down = true;
                                        self.drag_active = false;
                                        self.accumulated_drag_delta = (0.0, 0.0);
                                        self.hover_start_time = None;
                                        self.hover_off_time = None;
                                    }
                                    WidgetState::Expanded => {
                                        if let Some((cx, cy)) = self.last_cursor_pos {
                                            let scale_factor = w.scale_factor();
                                            let logical_x = cx / scale_factor;
                                            let logical_y = cy / scale_factor;
                                            if logical_x >= 288.0 && logical_x <= 304.0 && logical_y >= 17.0 && logical_y <= 33.0 {
                                                self.collapse_widget();
                                            }
                                        }
                                    }
                                }
                            } else if *state == winit::event::ElementState::Released {
                                if self.is_mouse_down {
                                    self.is_mouse_down = false;
                                    if self.drag_active {
                                        self.drag_active = false;
                                        let w = self.floating_window.as_ref().unwrap();
                                        let win_pos = w.outer_position().unwrap_or_default();
                                        let monitor = w.current_monitor().or_else(|| w.primary_monitor());
                                        if let Some(mon) = monitor {
                                            let mon_pos = mon.position();
                                            let mon_size = mon.size();
                                            let left = mon_pos.x;
                                            let right = mon_pos.x + mon_size.width as i32;
                                            let top = mon_pos.y;
                                            
                                            let scale_factor = w.scale_factor();
                                            let phys_badge = (64.0 * scale_factor) as i32;
                                            let snap_threshold = (20.0 * scale_factor) as i32;
                                            
                                            let dist_left = (win_pos.x - left).abs();
                                            let dist_right = ((win_pos.x + phys_badge) - right).abs();
                                            let dist_top = (win_pos.y - top).abs();
                                            
                                            if dist_left < snap_threshold {
                                                self.widget_state = WidgetState::SnappedLeft;
                                            } else if dist_right < snap_threshold {
                                                self.widget_state = WidgetState::SnappedRight;
                                            } else if dist_top < snap_threshold {
                                                self.widget_state = WidgetState::SnappedTop;
                                            } else {
                                                self.widget_state = WidgetState::Badge;
                                            }
                                            
                                            if let Some((pos, size)) = get_snapped_layout(self.widget_state, w) {
                                                self.start_resize_animation(pos, size);
                                            } else {
                                                w.request_redraw();
                                            }
                                        }
                                    } else {
                                        // Clicked!
                                        if self.widget_state != WidgetState::Expanded {
                                            self.hide_tooltip();
                                            let w = self.floating_window.as_ref().unwrap();
                                            self.previous_state = Some(self.widget_state);
                                            self.widget_state = WidgetState::Expanded;
                                            let pos = w.outer_position().unwrap_or_default();
                                            let scale_factor = w.scale_factor();
                                            let phys_w = (320.0 * scale_factor) as u32;
                                            let phys_h = (480.0 * scale_factor) as u32;
                                            let (clamped_x, clamped_y) = clamp_position(pos.x, pos.y, phys_w, phys_h, w);
                                            let target_pos = winit::dpi::PhysicalPosition::new(clamped_x, clamped_y);
                                            let target_size = winit::dpi::LogicalSize::new(320.0, 480.0);
                                            self.start_resize_animation(target_pos, target_size);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                winit::event::WindowEvent::CursorMoved { position, .. } => {
                    self.last_cursor_pos = Some((position.x, position.y));
                    if let Some(ref tooltip) = self.tooltip_window {
                        if let Some(ref w) = self.floating_window {
                            let win_pos = w.outer_position().unwrap_or_default();
                            let cursor_screen_x = win_pos.x + position.x as i32;
                            let cursor_screen_y = win_pos.y + position.y as i32;
                            tooltip.set_outer_position(winit::dpi::PhysicalPosition::new(
                                cursor_screen_x + 12,
                                cursor_screen_y + 12
                            ));
                        }
                    }
                }
                _ => {}
            }
            return;
        }

        self.masonry_state.handle_window_event(
            event_loop,
            window_id,
            event,
            self.app_driver.as_mut(),
        );
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: MasonryUserEvent) {
        if let MasonryUserEvent::Action(_, ref action, _) = event {
            if action.is::<TelemetryUpdateEvent>() {
                if let Some(ref w) = self.floating_window {
                    w.request_redraw();
                }
                if let Some(ref t) = self.tooltip_window {
                    t.request_redraw();
                }
            }
        }
        self.masonry_state.handle_user_event(event_loop, event, self.app_driver.as_mut());
    }

    fn device_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        device_id: winit::event::DeviceId,
        event: winit::event::DeviceEvent,
    ) {
        if let winit::event::DeviceEvent::MouseMotion { delta } = &event {
            if self.is_mouse_down {
                self.accumulated_drag_delta.0 += delta.0;
                self.accumulated_drag_delta.1 += delta.1;
                let dist = (self.accumulated_drag_delta.0.powi(2) + self.accumulated_drag_delta.1.powi(2)).sqrt();
                if dist > 5.0 {
                    self.drag_active = true;
                }
                if self.drag_active {
                    if let Some(ref w) = self.floating_window {
                        let pos = w.outer_position().unwrap_or_default();
                        let new_x = pos.x + delta.0 as i32;
                        let new_y = pos.y + delta.1 as i32;
                        w.set_outer_position(winit::dpi::PhysicalPosition::new(new_x, new_y));
                        w.request_redraw();
                    }
                }
            }
        }

        self.masonry_state.handle_device_event(
            event_loop,
            device_id,
            event,
            self.app_driver.as_mut(),
        );
    }

    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: winit::event::StartCause) {
        self.masonry_state.handle_new_events(event_loop, cause);
    }

    fn exiting(&mut self, event_loop: &ActiveEventLoop) {
        self.masonry_state.handle_exiting(event_loop);
    }

    fn memory_warning(&mut self, event_loop: &ActiveEventLoop) {
        self.masonry_state.handle_memory_warning(event_loop);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        while let Ok(event) = tray_icon::TrayIconEvent::receiver().try_recv() {
            if matches!(event, tray_icon::TrayIconEvent::Click { .. }) {
                let show_window_action: ErasedAction = Box::new(ShowWindowEvent);
                let user_event = MasonryUserEvent::Action(self.main_window_id, show_window_action, WidgetId::reserved(0));
                let _ = self.proxy.send_event(user_event);
            }
        }

        while let Ok(event) = tray_icon::menu::MenuEvent::receiver().try_recv() {
            if event.id == self.show_item_id {
                let show_window_action: ErasedAction = Box::new(ShowWindowEvent);
                let user_event = MasonryUserEvent::Action(self.main_window_id, show_window_action, WidgetId::reserved(0));
                let _ = self.proxy.send_event(user_event);
            } else if event.id == self.float_item_id {
                self.toggle_floating_window(event_loop);
            } else if event.id == self.exit_item_id {
                let exit_action: ErasedAction = Box::new(ExitAppEvent);
                let user_event = MasonryUserEvent::Action(self.main_window_id, exit_action, WidgetId::reserved(0));
                let _ = self.proxy.send_event(user_event);
            }
        }

        if self.floating_window.is_some() && self.floating_visible {
            let now = std::time::Instant::now();
            let hover_start = self.hover_start_time;
            let hover_off = self.hover_off_time;
            let state = self.widget_state;
            
            if (state == WidgetState::SnappedLeft
                || state == WidgetState::SnappedRight
                || state == WidgetState::SnappedTop)
                && hover_start.is_some()
            {
                if now.duration_since(hover_start.unwrap()) >= std::time::Duration::from_millis(200) {
                    self.hover_start_time = None;
                    let w = self.floating_window.as_ref().unwrap();
                    if let Some((pos, size)) = get_revealed_layout(state, w) {
                        self.start_resize_animation(pos, size);
                    }
                }
            }
            
            if hover_off.is_some() && state != WidgetState::Expanded {
                if now.duration_since(hover_off.unwrap()) >= std::time::Duration::from_millis(1500) {
                    self.hover_off_time = None;
                    let w = self.floating_window.as_ref().unwrap();
                    if let Some((pos, size)) = get_snapped_layout(state, w) {
                        self.start_resize_animation(pos, size);
                    }
                }
            }
            
            let animation_active = self.step_resize_animation();
            
            // 仅在动画进行中或内容变化时请求重绘，避免无限帧率渲染
            if animation_active || self.float_needs_redraw {
                self.float_needs_redraw = false;
                if let Some(ref w) = self.floating_window {
                    w.request_redraw();
                }
            }
        }

        // tooltip 不需要每帧重绘 — 内容仅在 show 时绘制一次

        self.masonry_state.handle_about_to_wait(event_loop);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_dummy_icon() {
        let _icon = create_dummy_icon();
    }

    #[test]
    fn test_draw_text_renders_pixels() {
        let mut pixmap = tiny_skia::Pixmap::new(32, 32).unwrap();
        pixmap.fill(tiny_skia::Color::TRANSPARENT);
        
        let ts = tiny_skia::Transform::identity();
        draw_text(&mut pixmap, "A", 0.0, 0.0, tiny_skia::Color::WHITE, 1.0, ts);
        
        // Count non-transparent pixels
        let non_transparent = pixmap.data().chunks_exact(4).filter(|c| c[3] > 0).count();
        assert!(non_transparent > 0, "draw_text should render at least some pixels for 'A'");
    }

    #[test]
    fn test_chinese_char_rendering() {
        assert!(get_chinese_char_16x16('内').is_some());
        assert!(get_chinese_char_16x16('存').is_some());
        
        let mut pixmap = tiny_skia::Pixmap::new(32, 32).unwrap();
        pixmap.fill(tiny_skia::Color::TRANSPARENT);
        
        let ts = tiny_skia::Transform::identity();
        draw_text(&mut pixmap, "内存", 0.0, 0.0, tiny_skia::Color::WHITE, 1.0, ts);
        let non_transparent = pixmap.data().chunks_exact(4).filter(|c| c[3] > 0).count();
        assert!(non_transparent > 0, "draw_text should render at least some pixels for Chinese chars");
    }
}
