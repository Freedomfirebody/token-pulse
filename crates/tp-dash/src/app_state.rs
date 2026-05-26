//! 应用状态与主视图逻辑。
//!
//! 消费 `DashboardView` → 构建 Xilem 渲染树。

use xilem::masonry::properties::types::{AsUnit, CrossAxisAlignment};
use xilem::view::{flex_col, flex_row, label, sized_box, worker_raw, text_button, FlexExt as _, FlexSpacer};
use xilem::style::Style;
use xilem::core::fork;
use xilem::WidgetView;
use chrono::{Datelike, NaiveDate};

use tp_protocol::view::DashboardView;
use crate::theme;
use crate::views::metric_card::metric_card;
use crate::views::panel::panel_container;
use crate::widgets::portal::vertical_portal;

// 导入解耦的组件模型与视图
use crate::views::heatmap::{HeatmapData, HeatmapUIState, HeatmapDayStats, heatmap_view};
use crate::views::breakdown::{TokenBreakdownData, breakdown_view};
use crate::views::session_table::{SessionTableData, SessionRow, session_table_view, calculate_sparkline_heights};
use crate::views::collector_card::{CollectorCardData, collector_card};

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

    // 2. 投影构建 Heatmap 核心组件数据 (28周 × 7天日历网格)
    let now_dt = chrono::Local::now();
    let today = now_dt.date_naive();
    let weekday = today.weekday();
    let days_from_monday = weekday.num_days_from_monday() as i64;
    let current_week_monday = today - chrono::Duration::days(days_from_monday);
    let grid_start_monday = current_week_monday - chrono::Duration::weeks(27);

    let mut stats_by_date: std::collections::HashMap<NaiveDate, tp_protocol::view::DailyStats> = std::collections::HashMap::new();
    for (date_str, stats) in &view.daily_series {
        if let Ok(date) = NaiveDate::parse_from_str(date_str, "%Y-%m-%d") {
            stats_by_date.insert(date, stats.clone());
        }
    }

    state.heatmap_data.weeks = (0..28).map(|c| {
        (0..7).map(|r| {
            let cell_date = grid_start_monday + chrono::Duration::weeks(c as i64) + chrono::Duration::days(r as i64);
            let tokens = stats_by_date.get(&cell_date).map(|s| s.token_info.total()).unwrap_or(0);
            theme::heatmap_color(tokens)
        }).collect()
    }).collect();

    state.heatmap_data.stats = (0..28).map(|c| {
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

    let total_width = 230.0_f32;
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

/// Xilem 应用主入口 — 根据 AppState 构建视图树
pub fn app_logic(state: &mut AppState) -> impl WidgetView<AppState> + use<> {
    let view = &state.view;

    // ===== Tab Buttons =====
    let tab_all = text_button(if state.active_tab == DashTab::Overview { "[ 全部 ]" } else { "全部" }, |state: &mut AppState| {
        state.active_tab = DashTab::Overview;
    });
    
    let tab_antigravity = text_button(if state.active_tab == DashTab::ByModel { "[ Antigravity ]" } else { "Antigravity" }, |state: &mut AppState| {
        state.active_tab = DashTab::ByModel;
    });
    
    let tab_codex = text_button(if state.active_tab == DashTab::BySource { "[ Codex ]" } else { "Codex" }, |state: &mut AppState| {
        state.active_tab = DashTab::BySource;
    });

    let tab_bar = flex_row((
        tab_all,
        FlexSpacer::Fixed(15.0_f32.px()),
        tab_antigravity,
        FlexSpacer::Fixed(15.0_f32.px()),
        tab_codex,
    ));

    // ===== KPI Row =====
    let kpi_row = flex_row((
        metric_card("TOTAL TOKENS", view.total_tokens.total(), view.total_cost, theme::TEXT_CYAN).flex(1.0),
        FlexSpacer::Fixed((theme::SECTION_GAP as f32).px()),
        metric_card("TOTAL SESSIONS", view.by_project.len() as u64, 0.0, theme::COLOR_SUCCESS).flex(1.0),
        FlexSpacer::Fixed((theme::SECTION_GAP as f32).px()),
        metric_card("TOTAL MESSAGES", view.record_count, 0.0, theme::COLOR_INPUT).flex(1.0),
        FlexSpacer::Fixed((theme::SECTION_GAP as f32).px()),
        metric_card("TOTAL COST", 0, view.total_cost, theme::COLOR_OUTPUT).flex(1.0),
    ));

    // ===== Decoupled Heatmap Module Component =====
    let heatmap = panel_container(
        "TOKEN RETENTION HEATMAP",
        "Current tokens distributed by last modified date",
        heatmap_view(state.heatmap_ui.clone(), state.heatmap_data.clone(), |state: &mut AppState, cell, hovered| {
            if hovered {
                state.heatmap_ui.hovered_cell = Some(cell);
            } else if state.heatmap_ui.hovered_cell == Some(cell) {
                state.heatmap_ui.hovered_cell = None;
            }
        }),
        theme::TEXT_CYAN,
        theme::TEXT_MUTED,
    );

    // ===== Model Usage (Left Column, simple inline bar) =====
    let model_rows: Vec<_> = state.model_usages.iter().map(|usage| {
        let left_view = sized_box(
            label(usage.name.clone()).text_size(theme::FONT_SIZE_BODY).color(theme::TEXT_PRIMARY)
        );

        let right_view = label(usage.subtitle_str.clone()).text_size(theme::FONT_SIZE_BODY).color(theme::TEXT_SECONDARY);

        render_horizontal_bar_row(left_view, right_view, usage.fill_flex, usage.empty_flex)
    }).collect();

    let by_model = panel_container(
        "MODEL USAGE",
        "Ranked by total tokens",
        flex_col(model_rows),
        theme::TEXT_CYAN,
        theme::TEXT_MUTED,
    );

    // ===== Decoupled Token Breakdown Module Component =====
    let breakdown = panel_container(
        "Token Breakdown",
        "Proportional shares",
        breakdown_view(state.breakdown_data.clone()),
        theme::TEXT_CYAN,
        theme::TEXT_MUTED,
    );

    // ===== Bottom Scrollable Collectors Row =====
    let collector_cards: Vec<_> = state.collectors_data.iter().cloned().map(|c| {
        collector_card(c)
    }).collect();

    let collector_row = flex_row(collector_cards).gap(12.0_f32.px());

    let collectors_panel = panel_container(
        "ACTIVE TELEMETRY COLLECTORS",
        "Integrated nodes (scroll horizontally)",
        sized_box(crate::widgets::portal::horizontal_portal(collector_row))
            .height(180.0_f32.px())
            .expand_width(),
        theme::TEXT_CYAN,
        theme::TEXT_MUTED,
    );

    // ===== Decoupled Session Table Module Component =====
    let session_table = session_table_view(state.sessions_data.clone(), theme::TEXT_CYAN, theme::TEXT_MUTED);

    let termination_str = if let Some(ref key) = view.cache_termination_key {
        format!(" • Cache boundary: {}", key)
    } else {
        "".to_string()
    };

    // ===== Footer =====
    let footer_text = format!(
        "Last updated: {} • {} total records{}",
        view.last_updated.format("%Y-%m-%d %H:%M:%S UTC"),
        theme::format_with_commas(view.record_count),
        termination_str
    );

    // ===== Assemble Full Layout (Grid composition) =====
    let main_content = flex_col((
        // Header Bar
        flex_row((
            flex_row((
                label("ANTIGRAVITY TOKEN MONITOR")
                    .text_size(theme::FONT_SIZE_TITLE)
                    .color(theme::TEXT_CYAN),
                FlexSpacer::Fixed(10.0_f32.px()),
                // Glowing status dot
                sized_box(label(""))
                    .width(8.0_f32.px())
                    .height(8.0_f32.px())
                    .background_color(theme::COLOR_SUCCESS)
                    .corner_radius(4.0),
            ))
            .cross_axis_alignment(CrossAxisAlignment::Center),
            FlexSpacer::Flex(1.0),
            flex_col((
                label(footer_text)
                    .text_size(theme::FONT_SIZE_SMALL)
                    .color(theme::TEXT_MUTED),
                FlexSpacer::Fixed(6.0_f32.px()),
                {
                    let refresh_btn = text_button("  Refresh  ", |state: &mut AppState| {
                        state.dropdown_open = false;
                        if let Some(ref tx) = state.command_tx {
                            let _ = tx.try_send(PipelineCommand::Refresh);
                        }
                    });

                    let arrow_btn = text_button(if state.dropdown_open { " ▲ " } else { " ▼ " }, |state: &mut AppState| {
                        state.dropdown_open = !state.dropdown_open;
                    });

                    let split_button = sized_box(
                        flex_row((
                            refresh_btn,
                            label("│").color(theme::TEXT_MUTED),
                            arrow_btn,
                        ))
                        .cross_axis_alignment(CrossAxisAlignment::Center)
                    )
                    .background_color(theme::BG_INPUT)
                    .corner_radius(4.0)
                    .padding(2.0);

                    let dropdown_panel = state.dropdown_open.then(|| {
                        let upsert_btn = text_button("[ Upsert ]", |state: &mut AppState| {
                            state.dropdown_open = false;
                            if let Some(ref tx) = state.command_tx {
                                let _ = tx.try_send(PipelineCommand::Upsert);
                            }
                        });

                        let rebuild_btn = text_button("[ Rebuild ]", |state: &mut AppState| {
                            state.dropdown_open = false;
                            if let Some(ref tx) = state.command_tx {
                                let _ = tx.try_send(PipelineCommand::Rebuild);
                            }
                        });

                        sized_box(
                            flex_col((
                                upsert_btn,
                                FlexSpacer::Fixed(6.0_f32.px()),
                                rebuild_btn,
                            ))
                            .cross_axis_alignment(CrossAxisAlignment::Fill)
                        )
                        .width(110.0_f32.px())
                        .background_color(theme::BG_CARD)
                        .padding(10.0)
                        .corner_radius(6.0)
                    });

                    flex_col((
                        split_button,
                        state.dropdown_open.then(|| FlexSpacer::Fixed(4.0_f32.px())),
                        dropdown_panel,
                    ))
                    .cross_axis_alignment(CrossAxisAlignment::End)
                }
            )).cross_axis_alignment(CrossAxisAlignment::End),
        ))
        .cross_axis_alignment(CrossAxisAlignment::Start),

        FlexSpacer::Fixed(16.0_f32.px()),

        tab_bar,

        FlexSpacer::Fixed(16.0_f32.px()),

        // KPI Cards Row
        kpi_row,

        FlexSpacer::Fixed((theme::SECTION_GAP as f32).px()),

        // Middle layout (Two-column layout composition)
        flex_row((
            // Left column: Heatmap + Model Usage
            flex_col((
                heatmap,
                FlexSpacer::Fixed((theme::SECTION_GAP as f32).px()),
                by_model,
            ))
            .cross_axis_alignment(CrossAxisAlignment::Fill)
            .flex(1.0),

            FlexSpacer::Fixed((theme::SECTION_GAP as f32).px()),

            // Right column: Token Breakdown
            sized_box(breakdown).width(280.0_f32.px()),
        )),

        FlexSpacer::Fixed((theme::SECTION_GAP as f32).px()),

        // Bottom panels row (Collector Nodes)
        collectors_panel,

        FlexSpacer::Fixed((theme::SECTION_GAP as f32).px()),

        // Session analysis list
        session_table,
    ))
    .cross_axis_alignment(CrossAxisAlignment::Fill);

    // Wrap in scrollable portal with background
    let main_view = vertical_portal(
        sized_box(main_content)
            .expand_width()
            .background_color(theme::BG_MAIN)
            .padding(20.0)
    );

    let rx_opt = state.view_rx.clone();

    fork(
        main_view,
        worker_raw(
            move |proxy, mut _rx: tokio::sync::mpsc::UnboundedReceiver<()>| {
                let mut rx = rx_opt.clone();
                async move {
                    if let Some(ref mut rx) = rx {
                        loop {
                            if rx.changed().await.is_err() {
                                break;
                            }
                            let view = rx.borrow().clone();
                            if proxy.message(view).is_err() {
                                break;
                            }
                        }
                    }
                }
            },
            |_state: &mut AppState, _tx| {},
            |state: &mut AppState, view| {
                state.update_view(view);
            }
        )
    )
}
