//! 应用状态与主视图逻辑。
//!
//! 消费 `DashboardView` → 构建 Xilem 渲染树。

use xilem::masonry::properties::types::{AsUnit, CrossAxisAlignment, MainAxisAlignment};
use xilem::view::{flex_col, flex_row, label, sized_box, worker_raw, text_button, button, FlexSpacer, FlexExt as _};
use xilem::core::fork;
use xilem::core::one_of::Either;
use xilem::{WidgetView, AnyWidgetView};
use xilem::style::Style;

use tp_protocol::view::{DashboardView, DailyStats, RecentRecord, DimensionEntry};
use tp_protocol::datalog::TokenInfo;
use std::collections::BTreeMap;
use chrono::{Utc, Datelike};

use crate::theme;
use crate::views::metric_card::metric_card;
use crate::views::panel::panel_container;
use crate::widgets::portal::vertical_portal;

// 导入高解耦的封装组件状态与数据结构
// use crate::views::heatmap::HeatmapComponent;
use crate::views::breakdown::BreakdownComponent;
use crate::views::session_table::SessionTableComponent;
use crate::views::collector_card::{CollectorCardData, collector_card};
use crate::views::by_model::{PrecalculatedModelUsage, by_model_view};
use crate::widgets::responsive_layout;
use crate::widgets::{hoverable, popover_stack, PopoverConfig, AnchorPoint, PopoverAlign};

/// 仪表盘标签页
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DashTab {
    All,
    Antigravity,
    Codex,
    ClaudeCode,
}

/// 数据管道操作指令
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipelineCommand {
    Refresh,
    Upsert,
    Rebuild,
}

/// Message sent from main UI thread to background worker
#[derive(Debug, Clone)]
pub enum WorkerMessage {
    ClosePopupDelay,
}

/// Event sent from background worker to main UI thread
#[derive(Debug, Clone)]
pub enum AppEvent {
    ViewUpdate(DashboardView),
    ClosePopup,
}

/// 应用状态 — Xilem 的 root state
pub struct AppState {
    /// 当前显示的仪表盘数据
    pub view: DashboardView,
    /// 当前活跃标签页
    pub active_tab: DashTab,
    /// 数据刷新请求通道
    pub refresh_tx: Option<tokio::sync::mpsc::Sender<()>>,
    /// 视图更新 of watch channel
    pub view_rx: Option<tokio::sync::watch::Receiver<DashboardView>>,

    // ===== 高内聚封装组件挂载 =====
    // pub heatmap: HeatmapComponent,
    pub breakdown: BreakdownComponent,
    pub session_table: SessionTableComponent,
    
    pub collectors_data: Vec<CollectorCardData>,
    
    // 简易 Model 状态
    pub model_usages: Vec<PrecalculatedModelUsage>,

    // ===== UI Actions Dropdown =====
    pub dropdown_open: bool,
    pub command_tx: Option<tokio::sync::mpsc::Sender<PipelineCommand>>,
    
    /// Background worker message sender
    pub worker_tx: Option<tokio::sync::mpsc::UnboundedSender<WorkerMessage>>,

    /// Single column layout mode toggle
    pub single_column: bool,

    // ===== UI Actions Hover states =====
    pub hovered_refresh: bool,
    pub hovered_dropdown_btn: bool,
    pub hovered_upsert: bool,
    pub hovered_rebuild: bool,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            view: DashboardView::default(),
            active_tab: DashTab::All,
            refresh_tx: None,
            view_rx: None,
            // heatmap: HeatmapComponent::new(),
            breakdown: BreakdownComponent::new(),
            session_table: SessionTableComponent::new(),
            collectors_data: Vec::new(),
            model_usages: Vec::new(),
            dropdown_open: false,
            command_tx: None,
            worker_tx: None,
            single_column: false,
            hovered_refresh: false,
            hovered_dropdown_btn: false,
            hovered_upsert: false,
            hovered_rebuild: false,
        }
    }

    pub fn with_command_tx(mut self, tx: tokio::sync::mpsc::Sender<PipelineCommand>) -> Self {
        self.command_tx = Some(tx);
        self
    }

    pub fn with_refresh_tx(mut self, tx: tokio::sync::mpsc::Sender<()>) -> Self {
        self.refresh_tx = Some(tx);
        self
    }

    pub fn with_view_rx(mut self, rx: tokio::sync::watch::Receiver<DashboardView>) -> Self {
        self.view_rx = Some(rx);
        self
    }

    pub fn update_view(&mut self, view: DashboardView) {
        self.view = view;
        precalculate(self);
    }
}

/// 根据当前活跃标签页对 DashboardView 进行多维数据源投影过滤
fn filter_view(view: &DashboardView, tab: DashTab) -> DashboardView {
    match tab {
        DashTab::All => view.clone(),
        _ => {
            // 确定要过滤的源标识符列表
            let target_sources: Vec<&str> = match tab {
                DashTab::Antigravity => vec!["Antigravity", "Antigravity IDE", "antigravity", "antigravity_ide"],
                DashTab::Codex => vec!["Codex", "codex"],
                DashTab::ClaudeCode => vec!["Claude Code", "CloudeCode", "claude_code"],
                DashTab::All => unreachable!(),
            };

            // 1. 过滤 recent records
            let recent_records: Vec<RecentRecord> = view.recent_records
                .iter()
                .filter(|r| {
                    let name_str = r.source_name.to_string();
                    target_sources.iter().any(|&s| name_str.eq_ignore_ascii_case(s))
                })
                .cloned()
                .collect();

            // 2. 过滤 by_source
            let by_source: Vec<DimensionEntry> = view.by_source
                .iter()
                .filter(|e| {
                    target_sources.iter().any(|&s| e.key.eq_ignore_ascii_case(s))
                })
                .cloned()
                .collect();

            // 3. 依靠最近记录反向动态聚合计算 by_model 与 by_project，以防维度越界
            let mut model_map: std::collections::HashMap<String, (TokenInfo, u64, f64)> = std::collections::HashMap::new();
            let mut project_map: std::collections::HashMap<String, (TokenInfo, u64, f64)> = std::collections::HashMap::new();
            
            for r in &recent_records {
                let m_entry = model_map.entry(r.source_model.clone()).or_insert((TokenInfo::default(), 0, 0.0));
                m_entry.0.accumulate(&r.token_info);
                m_entry.1 += 1;
                m_entry.2 += r.cost_usd;

                let p_entry = project_map.entry(r.source_project.clone()).or_insert((TokenInfo::default(), 0, 0.0));
                p_entry.0.accumulate(&r.token_info);
                p_entry.1 += 1;
                p_entry.2 += r.cost_usd;
            }

            let mut by_model: Vec<DimensionEntry> = model_map.into_iter().map(|(key, (token_info, record_count, cost_usd))| {
                DimensionEntry { key, token_info, record_count, cost_usd }
            }).collect();
            by_model.sort_by(|a, b| b.token_info.total().cmp(&a.token_info.total()));

            let mut by_project: Vec<DimensionEntry> = project_map.into_iter().map(|(key, (token_info, record_count, cost_usd))| {
                DimensionEntry { key, token_info, record_count, cost_usd }
            }).collect();
            by_project.sort_by(|a, b| b.token_info.total().cmp(&a.token_info.total()));

            // 4. 重算总 KPI 与费用统计
            let mut total_tokens = TokenInfo::default();
            let mut total_cost = 0.0;
            let mut record_count = 0;
            
            for r in &recent_records {
                total_tokens.accumulate(&r.token_info);
                total_cost += r.cost_usd;
                record_count += 1;
            }

            // 5. 过滤并重新生成 daily_series 热力网格数据
            let mut daily_series: BTreeMap<String, DailyStats> = BTreeMap::new();
            for r in &recent_records {
                let day_key = r.source_datetime.format("%Y-%m-%d").to_string();
                let stats = daily_series.entry(day_key).or_insert(DailyStats::default());
                stats.token_info.accumulate(&r.token_info);
                stats.record_count += 1;
                stats.cost_usd += r.cost_usd;
                stats.message_count += 1;
            }

            // 6. 重新生成今日按小时序列数据
            let mut hourly_today: BTreeMap<String, TokenInfo> = BTreeMap::new();
            let today_str = Utc::now().format("%Y-%m-%d").to_string();
            for r in &recent_records {
                if r.source_datetime.format("%Y-%m-%d").to_string() == today_str {
                    let hh = r.source_datetime.format("%H").to_string();
                    hourly_today.entry(hh).or_default().accumulate(&r.token_info);
                }
            }

            // 7. 重算今日/本周/本月指标窗口
            let mut today_tokens = TokenInfo::default();
            let mut today_cost = 0.0;
            let mut week_tokens = TokenInfo::default();
            let mut week_cost = 0.0;
            let mut month_tokens = TokenInfo::default();
            let mut month_cost = 0.0;

            let now = Utc::now();
            let today_prefix = now.format("%Y-%m-%d").to_string();
            
            let weekday_offset = now.weekday().num_days_from_monday() as i64;
            let week_start = (now.date_naive() - chrono::Duration::days(weekday_offset)).format("%Y-%m-%d").to_string();
            let month_start = now.format("%Y-%m-01").to_string();

            for r in &recent_records {
                let day_key = r.source_datetime.format("%Y-%m-%d").to_string();
                if day_key == today_prefix {
                    today_tokens.accumulate(&r.token_info);
                    today_cost += r.cost_usd;
                }
                if day_key >= week_start {
                    week_tokens.accumulate(&r.token_info);
                    week_cost += r.cost_usd;
                }
                if day_key >= month_start {
                    month_tokens.accumulate(&r.token_info);
                    month_cost += r.cost_usd;
                }
            }

            DashboardView {
                total_tokens,
                today_tokens,
                week_tokens,
                month_tokens,
                total_cost,
                today_cost,
                week_cost,
                month_cost,
                record_count: record_count as u64,
                by_source,
                by_model,
                by_project,
                daily_series,
                hourly_today,
                recent_records,
                last_updated: view.last_updated,
                source_status: view.source_status.clone(),
                cache_termination_key: view.cache_termination_key.clone(),
            }
        }
    }
}

/// 核心数据向组件级轻量化数据结构的投影与构建
fn precalculate(state: &mut AppState) {
    let raw_view = state.view.clone();
    let filtered_view = filter_view(&raw_view, state.active_tab);
    let view = &filtered_view;

    // 1. 投影构建 Model Usage 数据
    let max_model_tokens = view.by_model.iter().map(|m| m.token_info.total()).max().unwrap_or(1).max(1);

    state.model_usages = view.by_model.iter().take(5).map(|entry| {
        let tokens = entry.token_info.total();
        let ratio = (tokens as f64 / max_model_tokens as f64).clamp(0.0, 1.0);
        let fill_flex = ratio.max(0.00001);
        let empty_flex = (1.0 - ratio).max(0.00001);
        let cost_str = theme::format_cost(entry.cost_usd);
        let subtitle_str = format!("{} tokens • {} sessions • {}", theme::format_with_commas(tokens), entry.record_count, cost_str);
        PrecalculatedModelUsage {
            name: entry.key.clone(),
            tokens,
            sessions: entry.record_count,
            cost_str,
            subtitle_str,
            fill_flex,
            empty_flex,
        }
    }).collect();

    // 2. 调度各封装组件更新其内部私有资产
    // state.heatmap.update(view);
    state.breakdown.update(view);
    state.session_table.update(view);

    // 3. 投影构建 active collectors 数据
    let mut antigravity_tokens = 0;
    let mut antigravity_records = 0;
    let mut antigravity_cost = 0.0;

    let mut claude_tokens = 0;
    let mut claude_records = 0;
    let mut claude_cost = 0.0;

    let mut codex_tokens = 0;
    let mut codex_records = 0;
    let mut codex_cost = 0.0;

    for entry in &view.by_source {
        match entry.key.as_str() {
            "Antigravity" => {
                antigravity_tokens = entry.token_info.total();
                antigravity_records = entry.record_count;
                antigravity_cost = entry.cost_usd;
            }
            "CloudeCode" => {
                claude_tokens = entry.token_info.total();
                claude_records = entry.record_count;
                claude_cost = entry.cost_usd;
            }
            "Codex" => {
                codex_tokens = entry.token_info.total();
                codex_records = entry.record_count;
                codex_cost = entry.cost_usd;
            }
            _ => {}
        }
    }

    state.collectors_data = vec![
        CollectorCardData {
            name: "Antigravity".to_string(),
            desc: "VS Code extension telemetry".to_string(),
            status: "ACTIVE".to_string(),
            status_color: theme::COLOR_SUCCESS,
            path: "~/.gemini/antigravity".to_string(),
            total_tokens: antigravity_tokens,
            records: antigravity_records,
            cost: antigravity_cost,
        },
        CollectorCardData {
            name: "Claude Code".to_string(),
            desc: "CLI shell token tracker".to_string(),
            status: "ACTIVE".to_string(),
            status_color: theme::COLOR_SUCCESS,
            path: "~/.claude.code/".to_string(),
            total_tokens: claude_tokens,
            records: claude_records,
            cost: claude_cost,
        },
        CollectorCardData {
            name: "Codex".to_string(),
            desc: "Auto-completion agent logs".to_string(),
            status: "ACTIVE".to_string(),
            status_color: theme::COLOR_SUCCESS,
            path: "~/.codex/".to_string(),
            total_tokens: codex_tokens,
            records: codex_records,
            cost: codex_cost,
        },
        CollectorCardData {
            name: "Pending Node".to_string(),
            desc: "Awaiting custom telemetry source".to_string(),
            status: "WAITING".to_string(),
            status_color: theme::TEXT_MUTED,
            path: "Configure in settings".to_string(),
            total_tokens: 0,
            records: 0,
            cost: 0.0,
        },
    ];
}

/// 构建行业最高品质、仿 JS (如 shadcn/ui) 高级悬浮特效的操作按钮与下拉浮动面板
fn build_actions_dropdown(
    dropdown_open: bool,
    hovered_refresh: bool,
    hovered_dropdown_btn: bool,
    hovered_upsert: bool,
    hovered_rebuild: bool,
) -> Box<AnyWidgetView<AppState>> {
    let refresh_btn = hoverable(
        sized_box(
            button(
                label("Refresh").text_size(theme::FONT_SIZE_BODY).color(theme::TEXT_PRIMARY),
                |state: &mut AppState| {
                    state.dropdown_open = false;
                    if let Some(ref tx) = state.command_tx {
                        let _ = tx.try_send(PipelineCommand::Refresh);
                    }
                }
            )
            .background_color(if hovered_refresh { theme::BG_HOVER } else { theme::BG_INPUT })
            .corner_radius(4.0)
            .padding(xilem::style::Padding::from_vh(5.0, 0.0))
        )
        .height(28.0_f32.px())
        .width(93.0_f32.px()),
        |state: &mut AppState, hovered| {
            state.hovered_refresh = hovered;
        }
    );

    let arrow_btn = hoverable(
        sized_box(
            button(
                sized_box(
                    label(if dropdown_open { "▲" } else { "▼" }).text_size(10.0).color(theme::TEXT_PRIMARY)
                )
                .width(12.0_f32.px())
                .height(12.0_f32.px()),
                |state: &mut AppState| {
                    state.dropdown_open = !state.dropdown_open;
                }
            )
            .background_color(if hovered_dropdown_btn { theme::BG_HOVER } else { theme::BG_INPUT })
            .corner_radius(4.0)
            .padding(xilem::style::Padding::from_vh(5.0, 0.0))
        )
        .height(28.0_f32.px())
        .width(24.0_f32.px()),
        |state: &mut AppState, hovered| {
            state.hovered_dropdown_btn = hovered;
        }
    );

    let split_button = sized_box(
        sized_box(
            flex_row((
                refresh_btn,
                sized_box(label(""))
                    .width(1.0_f32.px())
                    .height(18.0_f32.px())
                    .background_color(theme::BORDER_SUBTLE),
                arrow_btn,
            ))
            .cross_axis_alignment(CrossAxisAlignment::Center)
        )
        .background_color(theme::BG_INPUT)
        .corner_radius(4.0)
    )
    .width(120.0_f32.px())
    .background_color(if dropdown_open { theme::BORDER_ACCENT } else { theme::BORDER_SUBTLE })
    .corner_radius(5.0)
    .padding(1.0);

    let dropdown_panel = if dropdown_open {
        let upsert_item = hoverable(
            button(
                flex_row((
                    label("Upsert").text_size(theme::FONT_SIZE_BODY).color(theme::TEXT_PRIMARY),
                    FlexSpacer::Flex(1.0),
                ))
                .cross_axis_alignment(CrossAxisAlignment::Center),
                |state: &mut AppState| {
                    state.dropdown_open = false;
                    if let Some(ref tx) = state.command_tx {
                        let _ = tx.try_send(PipelineCommand::Upsert);
                    }
                }
            )
            .background_color(if hovered_upsert { theme::BG_HOVER } else { theme::BG_PANEL })
            .corner_radius(4.0)
            .padding(xilem::style::Padding::from_vh(6.0, 12.0)),
            |state: &mut AppState, hovered| {
                state.hovered_upsert = hovered;
            }
        );

        let rebuild_item = hoverable(
            button(
                flex_row((
                    label("Rebuild").text_size(theme::FONT_SIZE_BODY).color(theme::TEXT_PRIMARY),
                    FlexSpacer::Flex(1.0),
                ))
                .cross_axis_alignment(CrossAxisAlignment::Center),
                |state: &mut AppState| {
                    state.dropdown_open = false;
                    if let Some(ref tx) = state.command_tx {
                        let _ = tx.try_send(PipelineCommand::Rebuild);
                    }
                }
            )
            .background_color(if hovered_rebuild { theme::BG_HOVER } else { theme::BG_PANEL })
            .corner_radius(4.0)
            .padding(xilem::style::Padding::from_vh(6.0, 12.0)),
            |state: &mut AppState, hovered| {
                state.hovered_rebuild = hovered;
            }
        );

        Either::A(
            sized_box(
                sized_box(
                    flex_col((
                        upsert_item,
                        FlexSpacer::Fixed(4.0_f32.px()),
                        rebuild_item,
                    ))
                    .cross_axis_alignment(CrossAxisAlignment::Fill)
                )
                .background_color(theme::BG_PANEL)
                .corner_radius(5.0)
                .padding(4.0)
            )
            .width(120.0_f32.px())
            .background_color(theme::BORDER_SUBTLE)
            .corner_radius(6.0)
            .padding(1.0)
        )
    } else {
        Either::B(
            sized_box(label("")).width(0.0_f32.px()).height(0.0_f32.px())
        )
    };

    popover_stack(
        split_button,
        dropdown_panel,
        PopoverConfig {
            anchor_point: AnchorPoint::BottomLeft,
            popover_align: PopoverAlign::TopLeft,
            offset_x: 0.0,
            offset_y: 2.0,
        }
    )
    .boxed()
}

/// Xilem 应用主入口 — 根据 AppState 构建视图树
pub fn app_logic(state: &mut AppState) -> impl WidgetView<AppState> + use<> {
    let view = &state.view;

    // ===== Tab Buttons for Single =====
    let tab_all_single = text_button(if state.active_tab == DashTab::All { "[ 全部 ]" } else { "全部" }, |state: &mut AppState| {
        state.active_tab = DashTab::All;
        precalculate(state);
        if let Some(ref tx) = state.command_tx {
            let _ = tx.try_send(PipelineCommand::Refresh);
        }
    });

    let tab_antigravity_single = text_button(if state.active_tab == DashTab::Antigravity { "[ Antigravity ]" } else { "Antigravity" }, |state: &mut AppState| {
        state.active_tab = DashTab::Antigravity;
        precalculate(state);
        if let Some(ref tx) = state.command_tx {
            let _ = tx.try_send(PipelineCommand::Refresh);
        }
    });

    let tab_codex_single = text_button(if state.active_tab == DashTab::Codex { "[ Codex ]" } else { "Codex" }, |state: &mut AppState| {
        state.active_tab = DashTab::Codex;
        precalculate(state);
        if let Some(ref tx) = state.command_tx {
            let _ = tx.try_send(PipelineCommand::Refresh);
        }
    });

    let tab_claude_single = text_button(if state.active_tab == DashTab::ClaudeCode { "[ Claude Code ]" } else { "Claude Code" }, |state: &mut AppState| {
        state.active_tab = DashTab::ClaudeCode;
        precalculate(state);
        if let Some(ref tx) = state.command_tx {
            let _ = tx.try_send(PipelineCommand::Refresh);
        }
    });

    let tab_bar_single = sized_box(flex_row((
        tab_all_single,
        FlexSpacer::Fixed(15.0_f32.px()),
        tab_antigravity_single,
        FlexSpacer::Fixed(15.0_f32.px()),
        tab_codex_single,
        FlexSpacer::Fixed(15.0_f32.px()),
        tab_claude_single,
        FlexSpacer::Flex(1.0),
    ))).height(36.0_f32.px());

    // ===== Tab Buttons for Dual =====
    let tab_all_dual = text_button(if state.active_tab == DashTab::All { "[ 全部 ]" } else { "全部" }, |state: &mut AppState| {
        state.active_tab = DashTab::All;
        precalculate(state);
        if let Some(ref tx) = state.command_tx {
            let _ = tx.try_send(PipelineCommand::Refresh);
        }
    });
    
    let tab_antigravity_dual = text_button(if state.active_tab == DashTab::Antigravity { "[ Antigravity ]" } else { "Antigravity" }, |state: &mut AppState| {
        state.active_tab = DashTab::Antigravity;
        precalculate(state);
        if let Some(ref tx) = state.command_tx {
            let _ = tx.try_send(PipelineCommand::Refresh);
        }
    });
    
    let tab_codex_dual = text_button(if state.active_tab == DashTab::Codex { "[ Codex ]" } else { "Codex" }, |state: &mut AppState| {
        state.active_tab = DashTab::Codex;
        precalculate(state);
        if let Some(ref tx) = state.command_tx {
            let _ = tx.try_send(PipelineCommand::Refresh);
        }
    });

    let tab_claude_dual = text_button(if state.active_tab == DashTab::ClaudeCode { "[ Claude Code ]" } else { "Claude Code" }, |state: &mut AppState| {
        state.active_tab = DashTab::ClaudeCode;
        precalculate(state);
        if let Some(ref tx) = state.command_tx {
            let _ = tx.try_send(PipelineCommand::Refresh);
        }
    });

    let tab_bar_dual = sized_box(flex_row((
        tab_all_dual,
        FlexSpacer::Fixed(15.0_f32.px()),
        tab_antigravity_dual,
        FlexSpacer::Fixed(15.0_f32.px()),
        tab_codex_dual,
        FlexSpacer::Fixed(15.0_f32.px()),
        tab_claude_dual,
        FlexSpacer::Flex(1.0),
    ))).height(36.0_f32.px());

    // ===== KPI Row for Single =====
    let kpi_row_single = flex_row((
        metric_card("TOTAL TOKENS", view.total_tokens.total(), view.total_cost, theme::TEXT_CYAN).flex(1.0),
        FlexSpacer::Fixed((theme::SECTION_GAP as f32).px()),
        metric_card("TOTAL SESSIONS", view.by_project.len() as u64, 0.0, theme::COLOR_SUCCESS).flex(1.0),
        FlexSpacer::Fixed((theme::SECTION_GAP as f32).px()),
        metric_card("TOTAL MESSAGES", view.record_count, 0.0, theme::COLOR_INPUT).flex(1.0),
        FlexSpacer::Fixed((theme::SECTION_GAP as f32).px()),
        metric_card("TOTAL COST", 0, view.total_cost, theme::COLOR_OUTPUT).flex(1.0),
    ));

    // ===== KPI Row for Dual =====
    let kpi_row_dual = flex_row((
        metric_card("TOTAL TOKENS", view.total_tokens.total(), view.total_cost, theme::TEXT_CYAN).flex(1.0),
        FlexSpacer::Fixed((theme::SECTION_GAP as f32).px()),
        metric_card("TOTAL SESSIONS", view.by_project.len() as u64, 0.0, theme::COLOR_SUCCESS).flex(1.0),
        FlexSpacer::Fixed((theme::SECTION_GAP as f32).px()),
        metric_card("TOTAL MESSAGES", view.record_count, 0.0, theme::COLOR_INPUT).flex(1.0),
        FlexSpacer::Fixed((theme::SECTION_GAP as f32).px()),
        metric_card("TOTAL COST", 0, view.total_cost, theme::COLOR_OUTPUT).flex(1.0),
    ));

    // ===== Model Usage view constructions =====
    let by_model_single = by_model_view(state.model_usages.clone(), theme::TEXT_CYAN, theme::TEXT_MUTED);
    // let by_model_dual = by_model_view(state.model_usages.clone(), theme::TEXT_CYAN, theme::TEXT_MUTED);

    // ===== 6. Shared Bottom and Header parts =====
    let collector_cards_single: Vec<_> = state.collectors_data.iter().cloned().map(|c| {
        collector_card(c)
    }).collect();

    let collector_row_single = flex_row(collector_cards_single).gap(12.0_f32.px());

    let collectors_panel_single = panel_container(
        "ACTIVE TELEMETRY COLLECTORS",
        "Integrated nodes (scroll horizontally)",
        sized_box(crate::widgets::portal::horizontal_portal(collector_row_single))
            .height(180.0_f32.px())
            .expand_width(),
        theme::TEXT_CYAN,
        theme::TEXT_MUTED,
    );

    let collector_cards_dual: Vec<_> = state.collectors_data.iter().cloned().map(|c| {
        collector_card(c)
    }).collect();

    let collector_row_dual = flex_row(collector_cards_dual).gap(12.0_f32.px());

    let collectors_panel_dual = panel_container(
        "ACTIVE TELEMETRY COLLECTORS",
        "Integrated nodes (scroll horizontally)",
        sized_box(crate::widgets::portal::horizontal_portal(collector_row_dual))
            .height(180.0_f32.px())
            .expand_width(),
        theme::TEXT_CYAN,
        theme::TEXT_MUTED,
    );

    let termination_str = if let Some(ref key) = view.cache_termination_key {
        format!(" • Cache boundary: {}", key)
    } else {
        "".to_string()
    };

    let footer_text = format!(
        "Last updated: {} • {} total records{}",
        view.last_updated.format("%Y-%m-%d %H:%M:%S UTC"),
        theme::format_with_commas(view.record_count),
        termination_str
    );

    // Common Header Bar View for Single
    let header_bar_single = flex_row((
        flex_row((
            label("ANTIGRAVITY TOKEN MONITOR")
                .text_size(theme::FONT_SIZE_TITLE)
                .color(theme::TEXT_CYAN),
            FlexSpacer::Fixed(10.0_f32.px()),
            sized_box(label(""))
                .width(8.0_f32.px())
                .height(8.0_f32.px())
                .background_color(theme::COLOR_SUCCESS)
                .corner_radius(4.0),
        ))
        .cross_axis_alignment(CrossAxisAlignment::Center),
        FlexSpacer::Flex(1.0),
        flex_col((
            label(footer_text.clone())
                .text_size(theme::FONT_SIZE_SMALL)
                .color(theme::TEXT_MUTED),
            FlexSpacer::Fixed(6.0_f32.px()),
            build_actions_dropdown(
                state.dropdown_open,
                state.hovered_refresh,
                state.hovered_dropdown_btn,
                state.hovered_upsert,
                state.hovered_rebuild,
            )
        )).cross_axis_alignment(CrossAxisAlignment::End),
    ))
    .cross_axis_alignment(CrossAxisAlignment::Start);

    // Common Header Bar View for Dual
    let header_bar_dual = flex_row((
        flex_row((
            label("ANTIGRAVITY TOKEN MONITOR")
                .text_size(theme::FONT_SIZE_TITLE)
                .color(theme::TEXT_CYAN),
            FlexSpacer::Fixed(10.0_f32.px()),
            sized_box(label(""))
                .width(8.0_f32.px())
                .height(8.0_f32.px())
                .background_color(theme::COLOR_SUCCESS)
                .corner_radius(4.0),
        ))
        .cross_axis_alignment(CrossAxisAlignment::Center),
        FlexSpacer::Flex(1.0),
        flex_col((
            label(footer_text.clone())
                .text_size(theme::FONT_SIZE_SMALL)
                .color(theme::TEXT_MUTED),
            FlexSpacer::Fixed(6.0_f32.px()),
            build_actions_dropdown(
                state.dropdown_open,
                state.hovered_refresh,
                state.hovered_dropdown_btn,
                state.hovered_upsert,
                state.hovered_rebuild,
            )
        )).cross_axis_alignment(CrossAxisAlignment::End),
    ))
    .cross_axis_alignment(CrossAxisAlignment::Start);

    // ===== 7. Responsive Layout Composition =====
    let main_content_without_header_single = flex_col((
        FlexSpacer::Fixed(60.0_f32.px()), // 预留 60px 高度给绝对定位悬浮的 Header Bar
        tab_bar_single,
        FlexSpacer::Fixed(16.0_f32.px()),
        kpi_row_single,
        FlexSpacer::Fixed((theme::SECTION_GAP as f32).px()),
        
        // Heatmap Component (map_state)
        // xilem::core::map_state(
        //     state.heatmap.view(&state.worker_tx),
        //     |state: &mut AppState| &mut state.heatmap,
        // ),
        
        FlexSpacer::Fixed((theme::SECTION_GAP as f32).px()),
        flex_row((
            FlexSpacer::Flex(1.0),
            // Breakdown Component (map_state - Horizontal view)
            sized_box(xilem::core::map_state(
                state.breakdown.view_horizontal(),
                |state: &mut AppState| &mut state.breakdown,
            ))
            .width(560.0_f32.px()),
            FlexSpacer::Flex(1.0),
        ))
        .main_axis_alignment(MainAxisAlignment::Center),
        
        FlexSpacer::Fixed((theme::SECTION_GAP as f32).px()),
        by_model_single,
        FlexSpacer::Fixed((theme::SECTION_GAP as f32).px()),
        collectors_panel_single,
        FlexSpacer::Fixed((theme::SECTION_GAP as f32).px()),
        
        // Session Table Component (map_state)
        xilem::core::map_state(
            state.session_table.view(),
            |state: &mut AppState| &mut state.session_table,
        ),
    ))
    .cross_axis_alignment(CrossAxisAlignment::Fill);

    let main_content_single = popover_stack(
        main_content_without_header_single,
        sized_box(header_bar_single).expand_width(),
        PopoverConfig {
            anchor_point: AnchorPoint::TopLeft,
            popover_align: PopoverAlign::TopLeft,
            offset_x: 0.0,
            offset_y: 0.0,
        }
    )
    .boxed();

    let main_content_without_header_dual = flex_col((
        FlexSpacer::Fixed(60.0_f32.px()), // 预留 60px 头部空间
        tab_bar_dual,
        FlexSpacer::Fixed(16.0_f32.px()),
        kpi_row_dual,
        FlexSpacer::Fixed((theme::SECTION_GAP as f32).px()),
        flex_row((
            // Left column: Heatmap (Adapt) + Model Usage
            // flex_col((
            //     xilem::core::map_state(
            //         state.heatmap.view(&state.worker_tx),
            //         |state: &mut AppState| &mut state.heatmap,
            //     ),
            //     FlexSpacer::Fixed((theme::SECTION_GAP as f32).px()),
            //     by_model_dual,
            // ))
            // .cross_axis_alignment(CrossAxisAlignment::Fill)
            // .flex(1.0),
            //
            // FlexSpacer::Fixed((theme::SECTION_GAP as f32).px()),
            
            // Right column: Token Breakdown (Adapt - vertical view)
            sized_box(xilem::core::map_state(
                state.breakdown.view_vertical(),
                |state: &mut AppState| &mut state.breakdown,
            ))
            .width(360.0_f32.px()),
        )),
        FlexSpacer::Fixed((theme::SECTION_GAP as f32).px()),
        collectors_panel_dual,
        FlexSpacer::Fixed((theme::SECTION_GAP as f32).px()),
        
        // Session Table Component (map_state)
        xilem::core::map_state(
            state.session_table.view(),
            |state: &mut AppState| &mut state.session_table,
        ),
    ))
    .cross_axis_alignment(CrossAxisAlignment::Fill);

    let main_content_dual = popover_stack(
        main_content_without_header_dual,
        sized_box(header_bar_dual).expand_width(),
        PopoverConfig {
            anchor_point: AnchorPoint::TopLeft,
            popover_align: PopoverAlign::TopLeft,
            offset_x: 0.0,
            offset_y: 0.0,
        }
    )
    .boxed();

    let main_view = vertical_portal(
        responsive_layout(
            sized_box(main_content_dual)
                .expand_width()
                .background_color(theme::BG_MAIN)
                .padding(20.0),
            sized_box(main_content_single)
                .expand_width()
                .background_color(theme::BG_MAIN)
                .padding(20.0),
            960.0,
        )
    );

    let rx_opt = state.view_rx.clone();

    fork(
        main_view,
        worker_raw(
            move |proxy, mut rx_worker: tokio::sync::mpsc::UnboundedReceiver<WorkerMessage>| {
                let mut rx_view = rx_opt.clone();
                async move {
                    loop {
                        tokio::select! {
                            view_changed = async {
                                if let Some(ref mut rx) = rx_view {
                                    rx.changed().await.is_ok()
                                } else {
                                    std::future::pending::<bool>().await
                                }
                            } => {
                                if view_changed {
                                    if let Some(ref rx) = rx_view {
                                        let view = rx.borrow().clone();
                                        if proxy.message(AppEvent::ViewUpdate(view)).is_err() {
                                            break;
                                        }
                                    }
                                } else {
                                    break;
                                }
                            }
                            msg = rx_worker.recv() => {
                                if let Some(WorkerMessage::ClosePopupDelay) = msg {
                                    let proxy = proxy.clone();
                                    tokio::spawn(async move {
                                        tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;
                                        let _ = proxy.message(AppEvent::ClosePopup);
                                    });
                                } else {
                                    break;
                                }
                            }
                        }
                    }
                }
            },
            |state: &mut AppState, tx| {
                state.worker_tx = Some(tx);
            },
            |state: &mut AppState, event| {
                match event {
                    AppEvent::ViewUpdate(view) => {
                        state.update_view(view);
                    }
                    AppEvent::ClosePopup => {
                        // 局部 UI 弹出层彻底重置
                        // if !state.heatmap.ui.cell_hovered && !state.heatmap.ui.popup_hovered {
                        //     state.heatmap.ui.hovered_cell = None;
                        //     state.heatmap.ui.popup_hovered = false;
                        //     state.heatmap.ui.cell_hovered = false;
                        // }
                    }
                }
            }
        )
    )
}
