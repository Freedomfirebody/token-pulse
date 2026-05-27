//! 应用状态与主视图逻辑。
//!
//! 消费 `DashboardView` → 构建 Xilem 渲染树。

use xilem::masonry::properties::types::{AsUnit, CrossAxisAlignment, MainAxisAlignment};
use xilem::view::{flex_col, flex_row, label, sized_box, worker_raw, text_button, button, FlexExt as _, FlexSpacer};
use xilem::style::Style;
use xilem::core::fork;
use xilem::core::one_of::Either;
use xilem::WidgetView;
use chrono::{Datelike, NaiveDate};

use tp_protocol::view::DashboardView;
use crate::theme;
use crate::views::metric_card::metric_card;
use crate::views::panel::panel_container;
use crate::widgets::portal::vertical_portal;

// 导入解耦的组件模型与视图
use crate::views::heatmap::{HeatmapData, HeatmapUIState, HeatmapDayStats, heatmap_view};
use crate::views::breakdown::{TokenBreakdownData, breakdown_view_vertical, breakdown_view_horizontal};
use crate::views::session_table::{SessionTableData, SessionRow, session_table_view, calculate_sparkline_heights};
use crate::views::collector_card::{CollectorCardData, collector_card};
use crate::widgets::responsive_layout;
use crate::widgets::{hoverable, popover_stack, PopoverConfig, AnchorPoint, PopoverAlign};

/// 仪表盘标签页
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DashTab {
    Overview,
    ByModel,
    BySource,
    ByProject,
}

/// 数据管道操作指令
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipelineCommand {
    Refresh,
    Upsert,
    Rebuild,
}

#[derive(Clone)]
pub struct PrecalculatedModelUsage {
    pub name: String,
    pub tokens: u64,
    pub sessions: u64,
    pub cost_str: String,
    pub subtitle_str: String,
    pub fill_flex: f64,
    pub empty_flex: f64,
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

    // ===== 解耦工程化组件子状态与数据结构 =====
    pub heatmap_ui: HeatmapUIState,
    pub heatmap_data: HeatmapData,
    pub breakdown_data: TokenBreakdownData,
    pub sessions_data: SessionTableData,
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
            active_tab: DashTab::Overview,
            refresh_tx: None,
            view_rx: None,
            heatmap_ui: HeatmapUIState::default(),
            heatmap_data: HeatmapData::default(),
            breakdown_data: TokenBreakdownData::default(),
            sessions_data: SessionTableData::default(),
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

/// 核心数据向组件级轻量化数据结构的投影与构建
fn precalculate(state: &mut AppState) {
    let view = &state.view;

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

    // 2. 投影构建 Heatmap 核心组件数据 (13周 × 7天年度日历网格)
    let now_dt = chrono::Local::now();
    let today = now_dt.date_naive();
    let weekday = today.weekday();
    let days_from_monday = weekday.num_days_from_monday() as i64;
    let current_week_monday = today - chrono::Duration::days(days_from_monday);
    let grid_start_monday = current_week_monday - chrono::Duration::weeks(12); // 12周前 (共13周，3个月)

    let mut stats_by_date: std::collections::HashMap<NaiveDate, tp_protocol::view::DailyStats> = std::collections::HashMap::new();
    for (date_str, stats) in &view.daily_series {
        if let Ok(date) = NaiveDate::parse_from_str(date_str, "%Y-%m-%d") {
            stats_by_date.insert(date, stats.clone());
        }
    }

    // 计算自适应分位数色阶阈值 (25%, 50%, 75% 分位数)
    let mut non_zero_tokens: Vec<u64> = view.daily_series.values()
        .map(|s| s.token_info.total())
        .filter(|&t| t > 0)
        .collect();
    non_zero_tokens.sort_unstable();

    let (q25, q50, q75) = if non_zero_tokens.is_empty() {
        (1_000, 10_000, 100_000) // 默认兜底
    } else {
        let len = non_zero_tokens.len();
        let q25 = non_zero_tokens[len * 25 / 100];
        let q50 = non_zero_tokens[len * 50 / 100];
        let q75 = non_zero_tokens[len * 75 / 100];
        
        let q25 = q25.max(1);
        let q50 = q50.max(q25 + 1);
        let q75 = q75.max(q50 + 1);
        (q25, q50, q75)
    };

    state.heatmap_data.weeks = (0..13).map(|c| {
        (0..7).map(|r| {
            let cell_date = grid_start_monday + chrono::Duration::weeks(c as i64) + chrono::Duration::days(r as i64);
            let tokens = stats_by_date.get(&cell_date).map(|s| s.token_info.total()).unwrap_or(0);
            theme::heatmap_color_dynamic(tokens, q25, q50, q75)
        }).collect()
    }).collect();

    state.heatmap_data.stats = (0..13).map(|c| {
        (0..7).map(|r| {
            let cell_date = grid_start_monday + chrono::Duration::weeks(c as i64) + chrono::Duration::days(r as i64);
            let stats_opt = stats_by_date.get(&cell_date);
            stats_opt.map(|s| {
                HeatmapDayStats {
                    date_str: cell_date.format("%B %d, %Y").to_string(),
                    tokens_processed: s.token_info.total(),
                    input_tokens: s.token_info.input,
                    output_tokens: s.token_info.output,
                    cache_tokens: s.token_info.cache,
                    reasoning_tokens: s.token_info.reasoning,
                    cost: s.cost_usd,
                    message_count: s.message_count,
                }
            })
        }).collect()
    }).collect();

    // 3. 投影构建 Token Breakdown 比例分段组件数据
    let total_input = view.total_tokens.input;
    let total_output = view.total_tokens.output;
    let total_cache = view.total_tokens.cache;
    let total_reasoning = view.total_tokens.reasoning;
    
    let total_classified = total_input + total_output + total_cache + total_reasoning;
    let total_all = view.total_tokens.total();
    let classified_percent = if total_all > 0 {
        (total_classified as f64 / total_all as f64) * 100.0
    } else {
        0.0
    };

    let p_input = if total_classified > 0 { (total_input as f64 / total_classified as f64) * 100.0 } else { 0.0 };
    let p_output = if total_classified > 0 { (total_output as f64 / total_classified as f64) * 100.0 } else { 0.0 };
    let p_cache = if total_classified > 0 { (total_cache as f64 / total_classified as f64) * 100.0 } else { 0.0 };
    let p_reasoning = if total_classified > 0 { (total_reasoning as f64 / total_classified as f64) * 100.0 } else { 0.0 };

    let total_width = 300.0_f32;
    let gap_size = 1.5_f32;
    let min_width = 5.0_f32;
    
    let values = [total_input, total_output, total_cache, total_reasoning];
    let active_count = values.iter().filter(|&&v| v > 0).count();
    
    let widths = if active_count == 0 {
        vec![0.0; 4]
    } else {
        let total_gaps = if active_count > 1 { (active_count - 1) as f32 * gap_size } else { 0.0 };
        let usable_width = total_width - total_gaps;
        let total_min = active_count as f32 * min_width;
        if total_min >= usable_width {
            values.iter().map(|&v| if v > 0 { usable_width / active_count as f32 } else { 0.0 }).collect()
        } else {
            let remaining = usable_width - total_min;
            let sum_vals: u64 = values.iter().sum();
            values.iter().map(|&v| {
                if v == 0 {
                    0.0
                } else {
                    let prop = v as f32 / sum_vals as f32;
                    min_width + prop * remaining
                }
            }).collect()
        }
    };

    state.breakdown_data = TokenBreakdownData {
        total_tokens: total_all,
        total_classified,
        classified_percent,
        total_input,
        p_input,
        w_input: widths[0],
        total_output,
        p_output,
        w_output: widths[1],
        total_cache,
        p_cache,
        w_cache: widths[2],
        total_reasoning,
        p_reasoning,
        w_reasoning: widths[3],
    };

    // 3.5. 投影构建 active collectors 数据
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
            name: "Codex / ccusage".to_string(),
            desc: "Auto-completion agent logs".to_string(),
            status: "ACTIVE".to_string(),
            status_color: theme::COLOR_SUCCESS,
            path: "~/.ccusage/".to_string(),
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

    // 4. 投影构建 Session Table 核心数据
    state.sessions_data.rows = view.by_project.iter().map(|entry| {
        let total_tokens = entry.token_info.total();
        let heights = calculate_sparkline_heights(total_tokens);
        let sparkline_color = if entry.token_info.cache > 0 {
            theme::TEXT_CYAN
        } else {
            theme::COLOR_CACHE
        };

        SessionRow {
            key: entry.key.clone(),
            active_desc: "Active Session • Synced".to_string(),
            mode_text: "[ACTIVE]".to_string(),
            is_active: true,
            record_count: entry.record_count,
            total_tokens,
            sparkline_heights: heights,
            sparkline_color,
        }
    }).collect();
}

/// Renders horizontal model bar rows
fn render_horizontal_bar_row<L, R, State: 'static>(
    left_view: L,
    right_view: R,
    fill_flex: f64,
    empty_flex: f64,
) -> impl WidgetView<State>
where
    L: WidgetView<State> + 'static,
    R: WidgetView<State> + 'static,
{
    flex_col((
        flex_row((
            left_view,
            FlexSpacer::Flex(1.0),
            right_view,
        ))
        .cross_axis_alignment(CrossAxisAlignment::Center),
        FlexSpacer::Fixed(3.0_f32.px()),
        sized_box(
            flex_row((
                sized_box(label(""))
                    .height(4.0_f32.px())
                    .expand_width()
                    .background_color(theme::TEXT_CYAN)
                    .flex(fill_flex),
                sized_box(label(""))
                    .height(4.0_f32.px())
                    .expand_width()
                    .background_color(theme::BG_INPUT)
                    .flex(empty_flex),
            ))
            .gap(0.0_f32.px())
        )
        .expand_width()
        .corner_radius(2.0),
        FlexSpacer::Fixed(6.0_f32.px()),
    ))
    .cross_axis_alignment(CrossAxisAlignment::Fill)
}

/// 构建行业最高品质、仿 JS (如 shadcn/ui) 高级悬浮特效的操作按钮与下拉浮动面板
fn build_actions_dropdown(
    dropdown_open: bool,
    hovered_refresh: bool,
    hovered_dropdown_btn: bool,
    hovered_upsert: bool,
    hovered_rebuild: bool,
) -> impl WidgetView<AppState> {
    // 1. 左半部分：刷新按钮 (高度固定为 28px，宽度固定为 93px，与右半部拼接完美达到 120px 宽度)
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
            .padding(xilem::style::Padding::from_vh(5.0, 0.0)) // 固定宽度，无需侧边 padding
        )
        .height(28.0_f32.px())
        .width(93.0_f32.px()),
        |state: &mut AppState, hovered| {
            state.hovered_refresh = hovered;
        }
    );

    // 2. 右半部分：折叠指示箭头 (高度固定为 28px，宽度固定为 24px)
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

    // 3. 仿 JS 聚焦与交互效果 of 组合分割按钮 (高度 28px，总宽度精确固定为 120px：93 + 1 + 24 + 2)
    let split_button = sized_box(
        sized_box(
            flex_row((
                refresh_btn,
                sized_box(label(""))
                    .width(1.0_f32.px())
                    .height(18.0_f32.px()) // 居中 18px 划分线
                    .background_color(theme::BORDER_SUBTLE),
                arrow_btn,
            ))
            .cross_axis_alignment(CrossAxisAlignment::Center)
        )
        .background_color(theme::BG_INPUT)
        .corner_radius(4.0)
    )
    .width(120.0_f32.px()) // 物理像素精确宽度固定为 120px，与下拉面板宽度完全一致！
    .background_color(if dropdown_open { theme::BORDER_ACCENT } else { theme::BORDER_SUBTLE })
    .corner_radius(5.0)
    .padding(1.0);

    // 4. 下拉浮动面板 (极致精简：仅纯文本 Upsert 和 Rebuild，左对齐，120px 宽度，小巧精致)
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
            .width(120.0_f32.px()) // 面板宽度精确固定为 120px
            .background_color(theme::BORDER_SUBTLE)
            .corner_radius(6.0)
            .padding(1.0)
        )
    } else {
        Either::B(
            sized_box(label("")).width(0.0_f32.px()).height(0.0_f32.px())
        )
    };

    // 使用 popover_stack 高级组件进行对齐与层级管理，彻底摆脱写死的 x/y 绝对偏移量，实现 100% 动态等宽垂直对齐！
    popover_stack(
        split_button,
        dropdown_panel,
        PopoverConfig {
            anchor_point: AnchorPoint::BottomLeft,
            popover_align: PopoverAlign::TopLeft,
            offset_x: 0.0,
            offset_y: 2.0, // 2px 精美间距
        }
    )
}

/// Xilem 应用主入口 — 根据 AppState 构建视图树
pub fn app_logic(state: &mut AppState) -> impl WidgetView<AppState> + use<> {
    let view = &state.view;

    // ===== Tab Buttons for Single =====
    let tab_all_single = text_button(if state.active_tab == DashTab::Overview { "[ 全部 ]" } else { "全部" }, |state: &mut AppState| {
        state.active_tab = DashTab::Overview;
    });
    
    let tab_antigravity_single = text_button(if state.active_tab == DashTab::ByModel { "[ Antigravity ]" } else { "Antigravity" }, |state: &mut AppState| {
        state.active_tab = DashTab::ByModel;
    });
    
    let tab_codex_single = text_button(if state.active_tab == DashTab::BySource { "[ Codex ]" } else { "Codex" }, |state: &mut AppState| {
        state.active_tab = DashTab::BySource;
    });

    let tab_bar_single = sized_box(flex_row((
        tab_all_single,
        FlexSpacer::Fixed(15.0_f32.px()),
        tab_antigravity_single,
        FlexSpacer::Fixed(15.0_f32.px()),
        tab_codex_single,
        FlexSpacer::Flex(1.0),
    ))).height(36.0_f32.px());

    // ===== Tab Buttons for Dual =====
    let tab_all_dual = text_button(if state.active_tab == DashTab::Overview { "[ 全部 ]" } else { "全部" }, |state: &mut AppState| {
        state.active_tab = DashTab::Overview;
    });
    
    let tab_antigravity_dual = text_button(if state.active_tab == DashTab::ByModel { "[ Antigravity ]" } else { "Antigravity" }, |state: &mut AppState| {
        state.active_tab = DashTab::ByModel;
    });
    
    let tab_codex_dual = text_button(if state.active_tab == DashTab::BySource { "[ Codex ]" } else { "Codex" }, |state: &mut AppState| {
        state.active_tab = DashTab::BySource;
    });

    let tab_bar_dual = sized_box(flex_row((
        tab_all_dual,
        FlexSpacer::Fixed(15.0_f32.px()),
        tab_antigravity_dual,
        FlexSpacer::Fixed(15.0_f32.px()),
        tab_codex_dual,
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

    // ===== 1. Tooltip Coordinates and hoverable tooltip (Calculated early) =====
    let mut anchor_point = AnchorPoint::TopLeft;
    let mut stats_opt = None;

    if let Some((c_idx, r_idx)) = state.heatmap_ui.hovered_cell {
        if let Some(week_stats) = state.heatmap_data.stats.get(c_idx) {
            if let Some(Some(stats)) = week_stats.get(r_idx) {
                // Precise local base coordinates inside the fixed-width 480px panel
                // panel padding (16px) + weekday labels (50px) = 66px
                let x_base = 66.0;
                
                let cell_box_size = 24.0;
                let cell_gap = 4.0;

                // Inside panel, above first cell: padding (16) + title/subtitle block (40) + month header + spacer = 86px
                // Adding the vertical offset of the heatmap card relative to the body content:
                // Spacer(60) + TabBar(36) + Spacer(16) + KpiRow(105) + Spacer(12) = 229px.
                // So y_base becomes 229.0 + 86.0 = 315.0px.
                let y_base = 315.0;

                let x_offset = x_base + (c_idx as f64) * (cell_box_size + cell_gap);
                let y_offset = y_base + (r_idx as f64) * (cell_box_size + cell_gap);

                // ALWAYS place to the right of the cell (+ cell_box_size) so it attaches to the side like a context menu,
                // and avoids covering up the calendar grid!
                anchor_point = AnchorPoint::Custom(x_offset + cell_box_size, y_offset + cell_box_size / 2.0);
                stats_opt = Some(stats.clone());
            }
        }
    }

    // ===== 2. Single-column Heatmap view construction =====
    let tooltip_single = crate::views::heatmap::build_custom_tooltip(stats_opt.clone());
    let hoverable_tooltip_single = crate::widgets::hoverable(tooltip_single, |state: &mut AppState, hovered| {
        state.heatmap_ui.popup_hovered = hovered;
        if !hovered && !state.heatmap_ui.cell_hovered {
            if let Some(ref tx) = state.worker_tx {
                let _ = tx.send(WorkerMessage::ClosePopupDelay);
            }
        }
    });

    let heatmap_panel_single = panel_container(
        "TOKEN RETENTION HEATMAP",
        "Current tokens distributed by last modified date",
        heatmap_view(
            state.heatmap_ui.clone(),
            state.heatmap_data.clone(),
            |state: &mut AppState, cell, hovered| {
                state.heatmap_ui.cell_hovered = hovered;
                if hovered {
                    state.heatmap_ui.popup_hovered = false;
                    let (c, r) = cell;
                    let has_stats = state.heatmap_data.stats.get(c)
                        .and_then(|w| w.get(r))
                        .and_then(|s| s.as_ref())
                        .is_some();
                    if has_stats {
                        state.heatmap_ui.hovered_cell = Some(cell);
                    } else {
                        state.heatmap_ui.hovered_cell = None;
                        state.heatmap_ui.popup_hovered = false;
                    }
                } else {
                    if !state.heatmap_ui.popup_hovered {
                        if let Some(ref tx) = state.worker_tx {
                            let _ = tx.send(WorkerMessage::ClosePopupDelay);
                        }
                    }
                }
            },
            |state: &mut AppState, grid_hovered| {
                if !grid_hovered {
                    state.heatmap_ui.cell_hovered = false;
                    if !state.heatmap_ui.popup_hovered {
                        if let Some(ref tx) = state.worker_tx {
                            let _ = tx.send(WorkerMessage::ClosePopupDelay);
                        }
                    }
                }
            },
        ),
        theme::TEXT_CYAN,
        theme::TEXT_MUTED,
    );

    let heatmap_panel_fixed_single = sized_box(heatmap_panel_single).width(480.0_f32.px());

    let heatmap_single = flex_row((
        heatmap_panel_fixed_single,
        FlexSpacer::Flex(1.0),
    ))
    .main_axis_alignment(MainAxisAlignment::Start);

    // ===== 3. Dual-column Heatmap view construction =====
    let tooltip_dual = crate::views::heatmap::build_custom_tooltip(stats_opt.clone());
    let hoverable_tooltip_dual = crate::widgets::hoverable(tooltip_dual, |state: &mut AppState, hovered| {
        state.heatmap_ui.popup_hovered = hovered;
        if !hovered && !state.heatmap_ui.cell_hovered {
            if let Some(ref tx) = state.worker_tx {
                let _ = tx.send(WorkerMessage::ClosePopupDelay);
            }
        }
    });

    let heatmap_panel_dual = panel_container(
        "TOKEN RETENTION HEATMAP",
        "Current tokens distributed by last modified date",
        heatmap_view(
            state.heatmap_ui.clone(),
            state.heatmap_data.clone(),
            |state: &mut AppState, cell, hovered| {
                state.heatmap_ui.cell_hovered = hovered;
                if hovered {
                    state.heatmap_ui.popup_hovered = false;
                    let (c, r) = cell;
                    let has_stats = state.heatmap_data.stats.get(c)
                        .and_then(|w| w.get(r))
                        .and_then(|s| s.as_ref())
                        .is_some();
                    if has_stats {
                        state.heatmap_ui.hovered_cell = Some(cell);
                    } else {
                        state.heatmap_ui.hovered_cell = None;
                        state.heatmap_ui.popup_hovered = false;
                    }
                } else {
                    if !state.heatmap_ui.popup_hovered {
                        if let Some(ref tx) = state.worker_tx {
                            let _ = tx.send(WorkerMessage::ClosePopupDelay);
                        }
                    }
                }
            },
            |state: &mut AppState, grid_hovered| {
                if !grid_hovered {
                    state.heatmap_ui.cell_hovered = false;
                    if !state.heatmap_ui.popup_hovered {
                        if let Some(ref tx) = state.worker_tx {
                            let _ = tx.send(WorkerMessage::ClosePopupDelay);
                        }
                    }
                }
            },
        ),
        theme::TEXT_CYAN,
        theme::TEXT_MUTED,
    );

    let heatmap_panel_fixed_dual = sized_box(heatmap_panel_dual).width(480.0_f32.px());

    let heatmap_dual = flex_row((
        heatmap_panel_fixed_dual,
        FlexSpacer::Flex(1.0),
    ))
    .main_axis_alignment(MainAxisAlignment::Start);

    // ===== 4. Model Usage view constructions =====
    let model_rows_single: Vec<_> = state.model_usages.iter().map(|usage| {
        let left_view = sized_box(
            label(usage.name.clone()).text_size(theme::FONT_SIZE_BODY).color(theme::TEXT_PRIMARY)
        );
        let right_view = label(usage.subtitle_str.clone()).text_size(theme::FONT_SIZE_BODY).color(theme::TEXT_SECONDARY);
        render_horizontal_bar_row(left_view, right_view, usage.fill_flex, usage.empty_flex)
    }).collect();

    let by_model_single = panel_container(
        "MODEL USAGE",
        "Ranked by total tokens",
        flex_col(model_rows_single),
        theme::TEXT_CYAN,
        theme::TEXT_MUTED,
    );

    let model_rows_dual: Vec<_> = state.model_usages.iter().map(|usage| {
        let left_view = sized_box(
            label(usage.name.clone()).text_size(theme::FONT_SIZE_BODY).color(theme::TEXT_PRIMARY)
        );
        let right_view = label(usage.subtitle_str.clone()).text_size(theme::FONT_SIZE_BODY).color(theme::TEXT_SECONDARY);
        render_horizontal_bar_row(left_view, right_view, usage.fill_flex, usage.empty_flex)
    }).collect();

    let by_model_dual = panel_container(
        "MODEL USAGE",
        "Ranked by total tokens",
        flex_col(model_rows_dual),
        theme::TEXT_CYAN,
        theme::TEXT_MUTED,
    );

    // ===== 5. Token Breakdown view constructions =====
    let breakdown_single = panel_container(
        "Token Breakdown",
        "Proportional shares",
        breakdown_view_horizontal(state.breakdown_data.clone()), // Horizontal Grid Layout!
        theme::TEXT_CYAN,
        theme::TEXT_MUTED,
    );

    let breakdown_dual = panel_container(
        "Token Breakdown",
        "Proportional shares",
        breakdown_view_vertical(state.breakdown_data.clone()), // Vertical Stacked Layout!
        theme::TEXT_CYAN,
        theme::TEXT_MUTED,
    );

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

    let session_table_single = session_table_view(state.sessions_data.clone(), theme::TEXT_CYAN, theme::TEXT_MUTED);

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

    let session_table_dual = session_table_view(state.sessions_data.clone(), theme::TEXT_CYAN, theme::TEXT_MUTED);

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
        FlexSpacer::Fixed(60.0_f32.px()), // 预留 60px 高度给绝对定位悬浮的 Header Bar，完美避免重叠
        tab_bar_single,
        FlexSpacer::Fixed(16.0_f32.px()),
        kpi_row_single,
        FlexSpacer::Fixed((theme::SECTION_GAP as f32).px()),
        heatmap_single,
        FlexSpacer::Fixed((theme::SECTION_GAP as f32).px()),
        flex_row((
            FlexSpacer::Flex(1.0),
            sized_box(breakdown_single).width(560.0_f32.px()), // 560px for horizontal breakdown grid
            FlexSpacer::Flex(1.0),
        ))
        .main_axis_alignment(MainAxisAlignment::Center),
        FlexSpacer::Fixed((theme::SECTION_GAP as f32).px()),
        by_model_single,
        FlexSpacer::Fixed((theme::SECTION_GAP as f32).px()),
        collectors_panel_single,
        FlexSpacer::Fixed((theme::SECTION_GAP as f32).px()),
        session_table_single,
    ))
    .cross_axis_alignment(CrossAxisAlignment::Fill);

    // 将日历热力图弹窗与 Header Bar 悬浮图层进行层级递进（Tooltip在Level 1，Header Dropdown在Level 2），100% 解决层级遮挡问题
    let main_content_with_tooltip_single = popover_stack(
        main_content_without_header_single,
        hoverable_tooltip_single,
        PopoverConfig {
            anchor_point,
            popover_align: PopoverAlign::LeftCenter,
            offset_x: 4.0, // cell_gap
            offset_y: 0.0,
        }
    );

    let main_content_single = popover_stack(
        main_content_with_tooltip_single,
        sized_box(header_bar_single).expand_width(),
        PopoverConfig {
            anchor_point: AnchorPoint::TopLeft,
            popover_align: PopoverAlign::TopLeft,
            offset_x: 0.0,
            offset_y: 0.0,
        }
    );

    let main_content_without_header_dual = flex_col((
        FlexSpacer::Fixed(60.0_f32.px()), // 预留 60px 头部空间
        tab_bar_dual,
        FlexSpacer::Fixed(16.0_f32.px()),
        kpi_row_dual,
        FlexSpacer::Fixed((theme::SECTION_GAP as f32).px()),
        flex_row((
            // Left column: Heatmap + Model Usage
            flex_col((
                heatmap_dual,
                FlexSpacer::Fixed((theme::SECTION_GAP as f32).px()),
                by_model_dual,
            ))
            .cross_axis_alignment(CrossAxisAlignment::Fill)
            .flex(1.0),
            
            FlexSpacer::Fixed((theme::SECTION_GAP as f32).px()),
            
            // Right column: Token Breakdown (vertical sidebar)
            sized_box(breakdown_dual).width(360.0_f32.px()),
        )),
        FlexSpacer::Fixed((theme::SECTION_GAP as f32).px()),
        collectors_panel_dual,
        FlexSpacer::Fixed((theme::SECTION_GAP as f32).px()),
        session_table_dual,
    ))
    .cross_axis_alignment(CrossAxisAlignment::Fill);

    let main_content_with_tooltip_dual = popover_stack(
        main_content_without_header_dual,
        hoverable_tooltip_dual,
        PopoverConfig {
            anchor_point,
            popover_align: PopoverAlign::LeftCenter,
            offset_x: 4.0, // cell_gap
            offset_y: 0.0,
        }
    );

    let main_content_dual = popover_stack(
        main_content_with_tooltip_dual,
        sized_box(header_bar_dual).expand_width(),
        PopoverConfig {
            anchor_point: AnchorPoint::TopLeft,
            popover_align: PopoverAlign::TopLeft,
            offset_x: 0.0,
            offset_y: 0.0,
        }
    );

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
                        if !state.heatmap_ui.cell_hovered && !state.heatmap_ui.popup_hovered {
                            state.heatmap_ui.hovered_cell = None;
                            state.heatmap_ui.popup_hovered = false; // Prevent stuck states
                            state.heatmap_ui.cell_hovered = false;
                        }
                    }
                }
            }
        )
    )
}
