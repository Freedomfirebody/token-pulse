use xilem::masonry::properties::types::{AsUnit, CrossAxisAlignment, MainAxisAlignment};
use tokio::sync::mpsc::UnboundedSender;
use std::collections::HashMap;
use xilem::view::{
    flex_col, flex_row, label, sized_box, text_button, worker, FlexSpacer, FlexExt,
};
use xilem::style::Style;
use xilem::core::fork;
use xilem::{WidgetView, palette};
use chrono::Datelike;

use crate::types::SessionTotals;
use crate::pricing;

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

#[derive(Clone)]
pub struct PrecalculatedSession {
    pub session_id: String,
    pub label: String,
    pub last_modified: String,
    pub mode: String,
    pub mode_color: xilem::Color,
    pub message_count: u64,
    pub total_tokens: u64,
    pub formatted_tokens: String,
    pub sparkline_heights: Vec<f32>,
    pub sparkline_color: xilem::Color,
}

pub struct AppState {
    pub session_root: String,
    pub sessions: Vec<SessionTotals>,
    pub total_tokens: u64,
    pub session_count: u64,
    pub active_sessions: u64,
    pub refresh_sender: Option<UnboundedSender<()>>,

    // Pre-calculated view state data to completely eliminate UI main-thread rendering lag
    pub metrics_total_sessions: usize,
    pub metrics_active_sessions: usize,
    pub metrics_estimated_sessions: usize,
    pub metrics_total_messages: u64,
    pub metrics_total_cost: f64,
    pub metrics_priced_models_count: usize,
    pub metrics_unpriced_models_count: usize,

    pub model_usages: Vec<PrecalculatedModelUsage>,
    pub heatmap_weeks: Vec<Vec<xilem::Color>>,

    pub breakdown_total_input: u64,
    pub breakdown_total_output: u64,
    pub breakdown_total_cache: u64,
    pub breakdown_total_reasoning: u64,
    pub breakdown_total_classified: u64,
    pub breakdown_classified_percent: f64,
    pub breakdown_p_input: f64,
    pub breakdown_p_output: f64,
    pub breakdown_p_cache: f64,
    pub breakdown_p_reasoning: f64,
    pub breakdown_w_input: f32,
    pub breakdown_w_output: f32,
    pub breakdown_w_cache: f32,
    pub breakdown_w_reasoning: f32,

    pub precalculated_sessions: Vec<PrecalculatedSession>,
    pub active_tab: String,
    pub ccusage_snapshot: Option<crate::ccusage::CcusageSnapshot>,
    pub hovered_cell: Option<(usize, usize)>,
    pub heatmap_stats: Vec<Vec<HeatmapDayStats>>,
}

#[derive(Clone, Default)]
pub struct HeatmapDayStats {
    pub date_str: String,
    pub tokens_processed: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_tokens: u64,
    pub reasoning_tokens: u64,
    pub cost: f64,
    pub message_count: u64,
}

pub fn precalculate(state: &mut AppState) {
    // 1. Calculate Metrics
    let total_sessions = state.sessions.len();
    let active_sessions = state.sessions.iter().filter(|s| s.mode == "reported").count();
    let estimated_sessions = total_sessions - active_sessions;
    let total_messages: u64 = state.sessions.iter().map(|s| s.message_count).sum();

    let mut total_cost = 0.0;
    let mut priced_models = std::collections::HashSet::new();
    let mut unpriced_models = std::collections::HashSet::new();
    for session in &state.sessions {
        for (model_name, breakdown) in &session.model_breakdowns {
            if let Some(cost) = pricing::calculate_cost(model_name, breakdown) {
                total_cost += cost;
                priced_models.insert(model_name.clone());
            } else {
                unpriced_models.insert(model_name.clone());
            }
        }
    }
    let priced_models_count = priced_models.len();
    let unpriced_models_count = unpriced_models.len();

    state.metrics_total_sessions = total_sessions;
    state.metrics_active_sessions = active_sessions;
    state.metrics_estimated_sessions = estimated_sessions;
    state.metrics_total_messages = total_messages;
    state.metrics_total_cost = total_cost;
    state.metrics_priced_models_count = priced_models_count;
    state.metrics_unpriced_models_count = unpriced_models_count;

    // 2. Model Usage statistics
    let mut model_stats_map: HashMap<String, (u64, u64, Option<f64>)> = HashMap::new();
    for session in &state.sessions {
        for (model_name, breakdown) in &session.model_breakdowns {
            let stats = model_stats_map.entry(model_name.clone()).or_insert((0, 0, Some(0.0)));
            stats.0 += breakdown.total_tokens;
            stats.1 += 1;
            if let Some(cost) = pricing::calculate_cost(model_name, breakdown) {
                if let Some(ref mut c) = stats.2 {
                    *c += cost;
                }
            } else {
                stats.2 = None;
            }
        }
    }

    let mut model_stats: Vec<(String, u64, u64, Option<f64>)> = model_stats_map
        .into_iter()
        .map(|(name, (tokens, sessions, cost))| (name, tokens, sessions, cost))
        .collect();
    model_stats.sort_by(|a, b| b.1.cmp(&a.1));
    let max_model_tokens = model_stats.iter().map(|m| m.1).max().unwrap_or(1);

    state.model_usages = model_stats.iter().take(5).map(|(name, tokens, sessions, cost_opt)| {
        let ratio = (*tokens as f64 / max_model_tokens as f64).clamp(0.0, 1.0);
        let fill_flex = ratio.max(0.00001);
        let empty_flex = (1.0 - ratio).max(0.00001);
        let cost_str = if let Some(cost) = cost_opt {
            format!("${:.2}", cost)
        } else {
            "No LiteLLM pricing match".to_string()
        };
        let subtitle_str = format!("{} tokens • {} sessions • {}", format_with_commas(*tokens), sessions, cost_str);
        PrecalculatedModelUsage {
            name: name.clone(),
            tokens: *tokens,
            sessions: *sessions,
            cost_str,
            subtitle_str,
            fill_flex,
            empty_flex,
        }
    }).collect();

    // 3. Heatmap Grid
    let now_dt = chrono::Local::now();
    let today = now_dt.date_naive();
    let weekday = today.weekday();
    let days_from_monday = weekday.num_days_from_monday() as i64;
    let current_week_monday = today - chrono::Duration::days(days_from_monday);
    let grid_start_monday = current_week_monday - chrono::Duration::weeks(27);

    let mut stats_by_date: HashMap<chrono::NaiveDate, HeatmapDayStats> = HashMap::new();
    for session in &state.sessions {
        if let Some(dt) = chrono::DateTime::from_timestamp((session.last_modified_ms / 1000) as i64, 0) {
            let local_dt = dt.with_timezone(&chrono::Local);
            let date = local_dt.date_naive();
            
            let stats = stats_by_date.entry(date).or_insert_with(|| HeatmapDayStats {
                date_str: date.format("%B %d, %Y").to_string(),
                tokens_processed: 0,
                input_tokens: 0,
                output_tokens: 0,
                cache_tokens: 0,
                reasoning_tokens: 0,
                cost: 0.0,
                message_count: 0,
            });
            
            stats.tokens_processed += session.breakdown.total_tokens;
            stats.input_tokens += session.breakdown.input_tokens;
            stats.output_tokens += session.breakdown.output_tokens;
            stats.cache_tokens += session.breakdown.cache_read_tokens + session.breakdown.cache_write_tokens;
            stats.reasoning_tokens += session.breakdown.reasoning_tokens;
            stats.message_count += session.message_count;
            
            for (model_name, breakdown) in &session.model_breakdowns {
                if let Some(cost) = pricing::calculate_cost(model_name, breakdown) {
                    stats.cost += cost;
                }
            }
        }
    }

    state.heatmap_weeks = (0..28).map(|c| {
        (0..7).map(|r| {
            let cell_date = grid_start_monday + chrono::Duration::weeks(c as i64) + chrono::Duration::days(r as i64);
            let tokens = stats_by_date.get(&cell_date).map(|s| s.tokens_processed).unwrap_or(0);
            if tokens == 0 {
                xilem::Color::from_rgb8(11, 38, 48)
            } else if tokens < 1_000_000 {
                xilem::Color::from_rgb8(15, 74, 92)
            } else if tokens < 10_000_000 {
                xilem::Color::from_rgb8(19, 112, 138)
            } else if tokens < 100_000_000 {
                xilem::Color::from_rgb8(23, 156, 184)
            } else {
                xilem::Color::from_rgb8(51, 224, 255)
            }
        }).collect()
    }).collect();

    state.heatmap_stats = (0..28).map(|c| {
        (0..7).map(|r| {
            let cell_date = grid_start_monday + chrono::Duration::weeks(c as i64) + chrono::Duration::days(r as i64);
            stats_by_date.get(&cell_date).cloned().unwrap_or_else(|| HeatmapDayStats {
                date_str: cell_date.format("%B %d, %Y").to_string(),
                tokens_processed: 0,
                input_tokens: 0,
                output_tokens: 0,
                cache_tokens: 0,
                reasoning_tokens: 0,
                cost: 0.0,
                message_count: 0,
            })
        }).collect()
    }).collect();

    // 4. Token Breakdown
    let mut total_input = 0;
    let mut total_output = 0;
    let mut total_cache = 0;
    let mut total_reasoning = 0;
    for s in &state.sessions {
        total_input += s.breakdown.input_tokens;
        total_output += s.breakdown.output_tokens;
        total_cache += s.breakdown.cache_read_tokens + s.breakdown.cache_write_tokens;
        total_reasoning += s.breakdown.reasoning_tokens;
    }
    let total_classified = total_input + total_output + total_cache + total_reasoning;
    let total_all = state.total_tokens;
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

    let w_input = widths[0];
    let w_output = widths[1];
    let w_cache = widths[2];
    let w_reasoning = widths[3];

    state.breakdown_total_input = total_input;
    state.breakdown_total_output = total_output;
    state.breakdown_total_cache = total_cache;
    state.breakdown_total_reasoning = total_reasoning;
    state.breakdown_total_classified = total_classified;
    state.breakdown_classified_percent = classified_percent;
    state.breakdown_p_input = p_input;
    state.breakdown_p_output = p_output;
    state.breakdown_p_cache = p_cache;
    state.breakdown_p_reasoning = p_reasoning;
    state.breakdown_w_input = w_input;
    state.breakdown_w_output = w_output;
    state.breakdown_w_cache = w_cache;
    state.breakdown_w_reasoning = w_reasoning;

    // 5. Precalculated sessions list
    state.precalculated_sessions = state.sessions.iter().map(|session| {
        let last_modified = format_date(session.last_modified_ms);
        let mode_color = if session.mode == "reported" { palette::css::CYAN } else { palette::css::YELLOW };
        let sparkline_heights = get_sparkline_heights(session.breakdown.total_tokens);
        let sparkline_color = if session.mode == "reported" { xilem::Color::from_rgb8(0, 229, 255) } else { xilem::Color::from_rgb8(255, 215, 0) };
        PrecalculatedSession {
            session_id: session.session_id.clone(),
            label: session.label.clone(),
            last_modified,
            mode: session.mode.clone(),
            mode_color,
            message_count: session.message_count,
            total_tokens: session.breakdown.total_tokens,
            formatted_tokens: format_with_commas(session.breakdown.total_tokens),
            sparkline_heights,
            sparkline_color,
        }
    }).collect();
}

fn build_custom_tooltip(stats: &HeatmapDayStats) -> impl WidgetView<AppState> + use<> {
    let text_white = xilem::Color::from_rgb8(255, 255, 255);
    let text_gray = xilem::Color::from_rgb8(140, 160, 165);
    let text_cyan = xilem::Color::from_rgb8(51, 224, 255);
    let card_bg = xilem::Color::from_rgb8(15, 51, 62);
    let border_color = xilem::Color::from_rgb8(23, 156, 184);

    let row = |label_str: &str, val_str: &str| {
        flex_row((
            label(label_str).text_size(11.0).color(text_gray),
            FlexSpacer::Flex(1.0),
            label(val_str).text_size(11.0).color(text_white),
        ))
    };

    sized_box(
        flex_col((
            label(stats.date_str.as_str())
                .text_size(12.0)
                .color(text_white),
            FlexSpacer::Fixed(6.0_f32.px()),
            
            flex_row((
                label("Tokens").text_size(11.0).color(text_gray),
                FlexSpacer::Flex(1.0),
                label(format_with_commas(stats.tokens_processed).as_str())
                    .text_size(13.0)
                    .color(text_cyan),
            )),
            FlexSpacer::Fixed(6.0_f32.px()),
            
            row("Input", &format_with_commas(stats.input_tokens)),
            FlexSpacer::Fixed(3.0_f32.px()),
            row("Output", &format_with_commas(stats.output_tokens)),
            FlexSpacer::Fixed(3.0_f32.px()),
            row("Cache", &format_with_commas(stats.cache_tokens)),
            FlexSpacer::Fixed(3.0_f32.px()),
            row("Reasoning", &format_with_commas(stats.reasoning_tokens)),
            
            FlexSpacer::Fixed(6.0_f32.px()),
            row("Cost", &format!("${:.4}", stats.cost)),
            FlexSpacer::Fixed(3.0_f32.px()),
            row("Messages", &format_with_commas(stats.message_count)),
        ))
        .cross_axis_alignment(CrossAxisAlignment::Fill)
        .padding(10.0)
    )
    .width(220.0_f32.px())
    .background_color(card_bg)
    .border(border_color, 1.0)
    .corner_radius(4.0)
}

fn render_horizontal_bar_row<L, R>(
    left_view: L,
    right_view: R,
    fill_flex: f64,
    empty_flex: f64,
) -> impl WidgetView<AppState> + use<L, R>
where
    L: WidgetView<AppState> + 'static,
    R: WidgetView<AppState> + 'static,
{
    let fill_color = xilem::Color::from_rgb8(0, 229, 255);
    let bg_color = xilem::Color::from_rgb8(11, 38, 48);

    flex_col((
        flex_row((
            left_view,
            FlexSpacer::Flex(1.0),
            right_view,
        ))
        .cross_axis_alignment(CrossAxisAlignment::Center),
        FlexSpacer::Fixed(6.0_f32.px()),
        flex_row((
            sized_box(label(""))
                .height(6.0_f32.px())
                .background_color(fill_color)
                .flex(fill_flex),
            sized_box(label(""))
                .height(6.0_f32.px())
                .background_color(bg_color)
                .flex(empty_flex),
        )),
        FlexSpacer::Fixed(12.0_f32.px()),
    ))
    .cross_axis_alignment(CrossAxisAlignment::Fill)
}

fn metric_card(title: &str, value: &str, subtitle: &str, text_cyan: xilem::Color, text_white: xilem::Color, text_gray: xilem::Color) -> impl WidgetView<AppState> + use<> {
    let card_bg = xilem::Color::from_rgb8(15, 51, 62);
    sized_box(
        flex_col((
            label(title)
                .text_size(11.0)
                .color(text_cyan),
            FlexSpacer::Fixed(6.0_f32.px()),
            label(value)
                .text_size(24.0)
                .color(text_white),
            FlexSpacer::Fixed(6.0_f32.px()),
            label(subtitle)
                .text_size(11.0)
                .color(text_gray),
        ))
        .cross_axis_alignment(CrossAxisAlignment::Start)
    )
    .expand_width()
    .height(105.0_f32.px())
    .background_color(card_bg)
    .padding(15.0)
}

fn panel_container<V>(title: &str, subtitle: &str, content: V, text_cyan: xilem::Color, text_gray: xilem::Color) -> impl WidgetView<AppState> + use<V>
where
    V: WidgetView<AppState> + 'static,
{
    let card_bg = xilem::Color::from_rgb8(15, 51, 62);
    sized_box(
        flex_col((
            flex_row((
                label(title).text_size(11.0).color(text_cyan),
                FlexSpacer::Flex(1.0),
                label(subtitle).text_size(10.0).color(text_gray),
            )),
            FlexSpacer::Fixed(10.0_f32.px()),
            content,
        ))
        .cross_axis_alignment(CrossAxisAlignment::Fill)
    )
    .expand_width()
    .background_color(card_bg)
    .padding(15.0)
}

pub fn app_logic(state: &mut AppState) -> impl WidgetView<AppState> + use<> {
    let rebuild_start = std::time::Instant::now();
    let bg_color = xilem::Color::from_rgb8(6, 31, 39);
    let card_bg = xilem::Color::from_rgb8(15, 51, 62);
    let text_cyan = xilem::Color::from_rgb8(51, 224, 255);
    let text_white = xilem::Color::from_rgb8(255, 255, 255);
    let text_gray = xilem::Color::from_rgb8(140, 160, 165);

    // Tab buttons
    let tab_all = text_button(if state.active_tab == "all" { "[ 全部 ]" } else { "全部" }, |state: &mut AppState| {
        state.active_tab = "all".to_string();
    });
    
    let tab_antigravity = text_button(if state.active_tab == "antigravity" { "[ Antigravity ]" } else { "Antigravity" }, |state: &mut AppState| {
        state.active_tab = "antigravity".to_string();
    });
    
    let tab_codex = text_button(if state.active_tab == "codex" { "[ Codex ]" } else { "Codex" }, |state: &mut AppState| {
        state.active_tab = "codex".to_string();
    });

    let tab_bar = flex_row((
        tab_all,
        FlexSpacer::Fixed(15.0_f32.px()),
        tab_antigravity,
        FlexSpacer::Fixed(15.0_f32.px()),
        tab_codex,
    ));

    // Antigravity sessions view
    let antigravity_view = {
        // 1. Model Usage rows from pre-cached state
        let model_usage_rows: Vec<_> = state.model_usages.iter().map(|usage| {
            let left_view = sized_box(
                label(usage.name.as_str())
                    .text_size(11.0)
                    .color(text_white)
            )
            .background_color(xilem::Color::from_rgb8(11, 38, 48))
            .border(xilem::Color::from_rgb8(0, 229, 255), 1.0)
            .corner_radius(3.0)
            .padding(4.0);

            let right_view = flex_row((
                label(format_with_commas(usage.tokens).as_str())
                    .text_size(11.0)
                    .color(xilem::Color::from_rgb8(0, 229, 255)),
                FlexSpacer::Fixed(6.0_f32.px()),
                label(format!("{} sess", usage.sessions).as_str())
                    .text_size(11.0)
                    .color(xilem::Color::from_rgb8(255, 213, 79)),
                FlexSpacer::Fixed(6.0_f32.px()),
                label(usage.cost_str.as_str())
                    .text_size(11.0)
                    .color(text_gray),
            ));

            render_horizontal_bar_row(left_view, right_view, usage.fill_flex, usage.empty_flex)
        }).collect();

        let model_usage_list = flex_col(model_usage_rows);

        // 2. Heatmap grid from pre-cached colors (hoverable)
        let heatmap_grid = flex_row(
            state.heatmap_weeks.iter().enumerate().map(|(c_idx, week_colors)| {
                flex_col(
                    week_colors.iter().enumerate().map(|(r_idx, &cell_color)| {
                        let cell_view = sized_box(label(""))
                            .width(10.0_f32.px())
                            .height(10.0_f32.px())
                            .background_color(cell_color)
                            .padding(1.0);
                        
                        hoverable(cell_view, move |state: &mut AppState, hovered| {
                            if hovered {
                                state.hovered_cell = Some((c_idx, r_idx));
                            } else if state.hovered_cell == Some((c_idx, r_idx)) {
                                state.hovered_cell = None;
                            }
                        })
                    }).collect::<Vec<_>>()
                )
            }).collect::<Vec<_>>()
        );

        // 3. Proportional Segmented Bar and Breakdown details from pre-cached state
        let color_input = xilem::Color::from_rgb8(51, 176, 255);
        let color_output = xilem::Color::from_rgb8(51, 224, 255);
        let color_cache = xilem::Color::from_rgb8(128, 216, 255);
        let color_reasoning = xilem::Color::from_rgb8(255, 213, 79);

        let mut segments = Vec::new();
        if state.breakdown_w_input > 0.0 {
            segments.push(
                sized_box(label(""))
                    .width(state.breakdown_w_input.px())
                    .height(14.0_f32.px())
                    .background_color(color_input),
            );
        }
        if state.breakdown_w_output > 0.0 {
            segments.push(
                sized_box(label(""))
                    .width(state.breakdown_w_output.px())
                    .height(14.0_f32.px())
                    .background_color(color_output),
            );
        }
        if state.breakdown_w_cache > 0.0 {
            segments.push(
                sized_box(label(""))
                    .width(state.breakdown_w_cache.px())
                    .height(14.0_f32.px())
                    .background_color(color_cache),
            );
        }
        if state.breakdown_w_reasoning > 0.0 {
            segments.push(
                sized_box(label(""))
                    .width(state.breakdown_w_reasoning.px())
                    .height(14.0_f32.px())
                    .background_color(color_reasoning),
            );
        }

        let segmented_bar = sized_box(
            flex_row(segments).gap(1.5_f32.px())
        )
        .width(230.0_f32.px())
        .height(14.0_f32.px())
        .background_color(xilem::Color::from_rgb8(11, 38, 48));

        let details_row = |name: &str, count: u64, percent: f64, color: xilem::Color| {
            let item_bg = xilem::Color::from_rgb8(11, 38, 48);
            sized_box(
                flex_row((
                    sized_box(label(""))
                        .width(10.0_f32.px())
                        .height(10.0_f32.px())
                        .background_color(color),
                    FlexSpacer::Fixed(10.0_f32.px()),
                    label(name).text_size(12.0).color(text_white),
                    FlexSpacer::Flex(1.0),
                    label(format!("{} ({:.1}%)", format_with_commas(count), percent).as_str())
                        .text_size(12.0)
                        .color(text_gray),
                ))
                .cross_axis_alignment(CrossAxisAlignment::Center)
                .padding(8.0)
            )
            .width(230.0_f32.px())
            .background_color(item_bg)
            .padding(4.0)
        };

        let tooltip_view = if let Some((week_idx, day_idx)) = state.hovered_cell {
            if let Some(week_stats) = state.heatmap_stats.get(week_idx) {
                if let Some(stats) = week_stats.get(day_idx) {
                    Some(build_custom_tooltip(stats))
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        let tooltip_spacer = if state.hovered_cell.is_some() {
            Some(FlexSpacer::Fixed(15.0_f32.px()))
        } else {
            None
        };

        let token_breakdown_view = flex_col((
            label(format_with_commas(state.total_tokens).as_str())
                .text_size(24.0)
                .color(text_white),
            label("TOTAL TOKENS").text_size(11.0).color(text_gray),
            FlexSpacer::Fixed(4.0_f32.px()),
            label(format!("{} categorized • {:.1}% of total classified", format_with_commas(state.breakdown_total_classified), state.breakdown_classified_percent).as_str())
                .text_size(11.0)
                .color(text_gray),
            FlexSpacer::Fixed(12.0_f32.px()),
            segmented_bar,
            FlexSpacer::Fixed(16.0_f32.px()),
            details_row("INPUT", state.breakdown_total_input, state.breakdown_p_input, color_input),
            FlexSpacer::Fixed(6.0_f32.px()),
            details_row("OUTPUT", state.breakdown_total_output, state.breakdown_p_output, color_output),
            FlexSpacer::Fixed(6.0_f32.px()),
            details_row("CACHE", state.breakdown_total_cache, state.breakdown_p_cache, color_cache),
            FlexSpacer::Fixed(6.0_f32.px()),
            details_row("REASONING", state.breakdown_total_reasoning, state.breakdown_p_reasoning, color_reasoning),
        ));

        flex_col((
            // 1. Top Metrics cards row
            flex_row((
                metric_card("TOTAL TOKENS", &format_with_commas(state.total_tokens), "Aggregate token volume across exported sessions", text_cyan, text_white, text_gray).flex(1.0),
                FlexSpacer::Fixed(15.0_f32.px()),
                metric_card("TOTAL SESSIONS", &format!("{}", state.metrics_total_sessions), &format!("{} active, {} estimated", state.metrics_active_sessions, state.metrics_estimated_sessions), text_cyan, text_white, text_gray).flex(1.0),
                FlexSpacer::Fixed(15.0_f32.px()),
                metric_card("TOTAL MESSAGES", &format_with_commas(state.metrics_total_messages), "Total message count retrieved from each session", text_cyan, text_white, text_gray).flex(1.0),
                FlexSpacer::Fixed(15.0_f32.px()),
                metric_card("TOTAL COST", &format!("${:.2}", state.metrics_total_cost), &format!("Priced {} models; {} unmatched", state.metrics_priced_models_count, state.metrics_unpriced_models_count), text_cyan, text_white, text_gray).flex(1.0),
            )),

            FlexSpacer::Fixed(20.0_f32.px()),

            // 2. Middle Section: Two Columns (Left Column: Heatmap + Model Usage, Right Column: Token Breakdown)
            flex_row((
                // Left Column
                sized_box(
                    flex_col((
                        panel_container(
                            "TOKEN RETENTION HEATMAP",
                            "Current tokens distributed by last modified date",
                            flex_row((
                                sized_box(
                                    flex_col((
                                        sized_box(label("Mon").text_size(9.0).color(text_gray)).height(12.0_f32.px()),
                                        FlexSpacer::Fixed(12.0_f32.px()),
                                        sized_box(label("Wed").text_size(9.0).color(text_gray)).height(12.0_f32.px()),
                                        FlexSpacer::Fixed(12.0_f32.px()),
                                        sized_box(label("Fri").text_size(9.0).color(text_gray)).height(12.0_f32.px()),
                                    ))
                                )
                                .width(30.0_f32.px())
                                .padding(4.0),
                                heatmap_grid,
                                tooltip_spacer,
                                tooltip_view,
                            )),
                            text_cyan,
                            text_gray,
                        ),
                        FlexSpacer::Fixed(15.0_f32.px()),
                        panel_container(
                            "MODEL USAGE",
                            "Ranked by total tokens",
                            model_usage_list,
                            text_cyan,
                            text_gray,
                        ),
                    ))
                    .cross_axis_alignment(CrossAxisAlignment::Fill)
                )
                .expand_width()
                .flex(1.0),

                FlexSpacer::Fixed(20.0_f32.px()),

                // Right Column
                sized_box(
                    panel_container(
                        "Token Breakdown",
                        "Proportional shares",
                        token_breakdown_view,
                        text_cyan,
                        text_gray,
                    )
                )
                .width(280.0_f32.px()),
            )),

            FlexSpacer::Fixed(20.0_f32.px()),

            // 3. Section Header
            flex_row((
                label("Session Analysis").text_size(18.0).color(text_white),
                FlexSpacer::Fixed(10.0_f32.px()),
                label("Sorted by recent activity")
                    .text_size(11.0)
                    .color(text_gray),
            )),

            FlexSpacer::Fixed(10.0_f32.px()),

            // 4. Column Table Headers Row
            flex_row((
                sized_box(label("SESSION").text_size(11.0).color(text_gray))
                    .flex(1.0),
                sized_box(label("MODE").text_size(11.0).color(text_gray))
                    .width(110.0_f32.px()),
                sized_box(label("MESSAGES").text_size(11.0).color(text_gray))
                    .width(90.0_f32.px()),
                sized_box(label("TOTAL TOKENS").text_size(11.0).color(text_gray))
                    .width(140.0_f32.px()),
                sized_box(label("ACTIVITY PULSE").text_size(11.0).color(text_gray))
                    .width(80.0_f32.px()),
            ))
            .padding(10.0),

            // 5. Session Table
            flex_col(
                state.precalculated_sessions.iter().map(|pre_session| {
                    sized_box(
                        flex_row((
                            flex_col((
                                label(pre_session.label.as_str()).text_size(14.0).color(text_white),
                                FlexSpacer::Fixed(4.0_f32.px()),
                                label(pre_session.last_modified.as_str()).text_size(11.0).color(text_gray),
                            ))
                            .flex(1.0),

                            sized_box(
                                label(format!("[{}]", pre_session.mode.to_uppercase()))
                                    .text_size(12.0)
                                    .color(pre_session.mode_color)
                            )
                            .width(110.0_f32.px()),

                            sized_box(
                                label(format!("{} msg", pre_session.message_count))
                                    .text_size(13.0)
                                    .color(text_white)
                            )
                            .width(90.0_f32.px()),

                            sized_box(
                                label(format!("{} tkn", pre_session.formatted_tokens))
                                    .text_size(13.0)
                                    .color(text_cyan)
                            )
                            .width(140.0_f32.px()),

                            sized_box(
                                render_sparkline(pre_session.sparkline_heights.clone(), pre_session.sparkline_color)
                            )
                            .width(80.0_f32.px()),
                        ))
                        .cross_axis_alignment(CrossAxisAlignment::Center)
                        .padding(10.0)
                    )
                    .background_color(card_bg)
                    .padding(4.0)
                }).collect::<Vec<_>>()
            ),
        ))
        .cross_axis_alignment(CrossAxisAlignment::Fill)
    };

    // ccusage global statistics view
    let ccusage_view = {
        let filter_agent = if state.active_tab == "codex" { Some("codex".to_string()) } else { None };

        let matches_agent = |row: &crate::ccusage::CcusageRow| -> bool {
            if let Some(ref f) = filter_agent {
                if let Some(ref agents) = row.metadata.agents {
                    agents.iter().any(|a| &a.to_lowercase() == f)
                } else {
                    &row.agent.to_lowercase() == f
                }
            } else {
                true
            }
        };

        let today_date = chrono::Local::now().format("%Y-%m-%d").to_string();
        
        let mut today_tokens = 0; let mut today_cost = 0.0;
        let mut week_tokens = 0; let mut week_cost = 0.0;
        let mut month_tokens = 0; let mut month_cost = 0.0;
        let mut cum_tokens = 0; let mut cum_cost = 0.0;

        if let Some(snap) = &state.ccusage_snapshot {
            // Unfiltered totals fallback
            if filter_agent.is_none() {
                cum_tokens = snap.totals.total_tokens;
                cum_cost = snap.totals.total_cost;
            }

            // Find latest periods
            let latest_week = snap.weekly.last().map(|r| r.period.clone()).unwrap_or_default();
            let latest_month = snap.monthly.last().map(|r| r.period.clone()).unwrap_or_default();

            for row in &snap.daily {
                if !matches_agent(row) { continue; }
                if row.period == today_date { today_tokens += row.total_tokens; today_cost += row.total_cost; }
                if filter_agent.is_some() { cum_tokens += row.total_tokens; cum_cost += row.total_cost; }
            }
            for row in &snap.weekly {
                if !matches_agent(row) { continue; }
                if row.period == latest_week { week_tokens += row.total_tokens; week_cost += row.total_cost; }
            }
            for row in &snap.monthly {
                if !matches_agent(row) { continue; }
                if row.period == latest_month { month_tokens += row.total_tokens; month_cost += row.total_cost; }
            }
        }

        // Summarize agent distribution
        let mut agent_map: std::collections::HashMap<String, (u64, f64)> = std::collections::HashMap::new();
        if let Some(snap) = &state.ccusage_snapshot {
            for row in &snap.daily {
                if !matches_agent(row) { continue; }
                let agent = if let Some(ref agents) = row.metadata.agents {
                    agents.first().cloned().unwrap_or_else(|| "unknown".to_string())
                } else if row.agent.is_empty() {
                    "unknown".to_string()
                } else {
                    row.agent.clone()
                };
                let entry = agent_map.entry(agent).or_insert((0, 0.0));
                entry.0 += row.total_tokens;
                entry.1 += row.total_cost;
            }
        }
        let mut agent_stats: Vec<(String, u64, f64)> = agent_map.into_iter().map(|(k, (v, c))| (k, v, c)).collect();
        agent_stats.sort_by(|a, b| b.1.cmp(&a.1));

        // Summarize model distribution
        let mut model_map: std::collections::HashMap<String, (u64, f64)> = std::collections::HashMap::new();
        if let Some(snap) = &state.ccusage_snapshot {
            for row in &snap.daily {
                if !matches_agent(row) { continue; }
                for breakdown in &row.model_breakdowns {
                    let entry = model_map.entry(breakdown.model_name.clone()).or_insert((0, 0.0));
                    entry.0 += breakdown.total_tokens;
                    entry.1 += breakdown.cost;
                }
            }
        }
        let mut model_stats: Vec<(String, u64, f64)> = model_map.into_iter().map(|(k, (v, c))| (k, v, c)).collect();
        model_stats.sort_by(|a, b| b.1.cmp(&a.1));

        let agent_rows: Vec<_> = agent_stats.iter().take(5).map(|(agent, tokens, cost)| {
            flex_col((
                flex_row((
                    label(agent.as_str()).text_size(13.0).color(text_white),
                    FlexSpacer::Flex(1.0),
                    label(format!("{} tkn • ${:.4}", format_with_commas(*tokens), cost).as_str()).text_size(12.0).color(text_gray),
                )),
                FlexSpacer::Fixed(8.0_f32.px()),
            ))
        }).collect();

        let model_rows: Vec<_> = model_stats.iter().take(5).map(|(model, tokens, cost)| {
            flex_col((
                flex_row((
                    label(model.as_str()).text_size(13.0).color(text_white),
                    FlexSpacer::Flex(1.0),
                    label(format!("{} tkn • ${:.4}", format_with_commas(*tokens), cost).as_str()).text_size(12.0).color(text_gray),
                )),
                FlexSpacer::Fixed(8.0_f32.px()),
            ))
        }).collect();

        let session_rows: Vec<_> = state.ccusage_snapshot.as_ref()
            .map(|snap| snap.sessions.iter()
                .filter(|s| matches_agent(s))
                .take(6).map(|sess| {
                let label_str = if sess.period.len() > 22 { format!("...{}", &sess.period[sess.period.len()-20..]) } else { sess.period.clone() };
                let display_agent = if let Some(ref agents) = sess.metadata.agents {
                    agents.first().cloned().unwrap_or_else(|| "unknown".to_string())
                } else if sess.agent.is_empty() {
                    "unknown".to_string()
                } else {
                    sess.agent.clone()
                };
                flex_col((
                    flex_row((
                        label(label_str.as_str()).text_size(12.0).color(text_white),
                        FlexSpacer::Flex(1.0),
                        label(display_agent.as_str()).text_size(11.0).color(text_cyan),
                    )),
                    FlexSpacer::Fixed(2.0_f32.px()),
                    flex_row((
                        label(format_with_commas(sess.total_tokens).as_str()).text_size(11.0).color(text_gray),
                        FlexSpacer::Flex(1.0),
                        label(format!("${:.4}", sess.total_cost).as_str()).text_size(11.0).color(text_gray),
                    )),
                    FlexSpacer::Fixed(8.0_f32.px()),
                ))
            }).collect::<Vec<_>>())
            .unwrap_or_default();

        let history_rows: Vec<_> = state.ccusage_snapshot.as_ref()
            .map(|snap| snap.daily.iter().rev()
                .filter(|s| matches_agent(s))
                .take(10).map(|row| {
                let display_agent = if let Some(ref agents) = row.metadata.agents {
                    agents.first().cloned().unwrap_or_else(|| "unknown".to_string())
                } else if row.agent.is_empty() {
                    "unknown".to_string()
                } else {
                    row.agent.clone()
                };
                sized_box(
                    flex_row((
                        sized_box(label(row.period.as_str()).text_size(13.0).color(text_white)).width(140.0_f32.px()),
                        sized_box(label(display_agent.as_str()).text_size(13.0).color(text_cyan)).width(100.0_f32.px()),
                        sized_box(label(format_with_commas(row.total_tokens).as_str()).text_size(13.0).color(text_white)).flex(1.0),
                        sized_box(label(format!("${:.4}", row.total_cost).as_str()).text_size(13.0).color(text_cyan)).width(120.0_f32.px()),
                    ))
                    .cross_axis_alignment(CrossAxisAlignment::Center)
                    .padding(10.0)
                )
                .background_color(card_bg)
                .padding(4.0)
            }).collect::<Vec<_>>())
            .unwrap_or_default();

        flex_col((
            // KPI Cards row
            flex_row((
                metric_card("TODAY'S TOKENS", &format_with_commas(today_tokens), &format!("Cost: ${:.4}", today_cost), text_cyan, text_white, text_gray).flex(1.0),
                FlexSpacer::Fixed(15.0_f32.px()),
                metric_card("THIS WEEK", &format_with_commas(week_tokens), &format!("Cost: ${:.4}", week_cost), text_cyan, text_white, text_gray).flex(1.0),
                FlexSpacer::Fixed(15.0_f32.px()),
                metric_card("THIS MONTH", &format_with_commas(month_tokens), &format!("Cost: ${:.4}", month_cost), text_cyan, text_white, text_gray).flex(1.0),
                FlexSpacer::Fixed(15.0_f32.px()),
                metric_card("CUMULATIVE", &format_with_commas(cum_tokens), &format!("Cost: ${:.4}", cum_cost), text_cyan, text_white, text_gray).flex(1.0),
            )),
            FlexSpacer::Fixed(20.0_f32.px()),

            // Middle section
            flex_row((
                // Left side: Models and Agents
                sized_box(
                    flex_col((
                        panel_container(
                            "AGENT DISTRIBUTION",
                            "Ranked by total tokens",
                            flex_col(agent_rows),
                            text_cyan,
                            text_gray,
                        ),
                        FlexSpacer::Fixed(15.0_f32.px()),
                        panel_container(
                            "MODEL CONSTITUENTS",
                            "Ranked by total tokens",
                            flex_col(model_rows),
                            text_cyan,
                            text_gray,
                        ),
                    ))
                    .cross_axis_alignment(CrossAxisAlignment::Fill)
                )
                .expand_width()
                .flex(1.0),

                FlexSpacer::Fixed(20.0_f32.px()),

                // Right side: Table or recent sessions
                sized_box(
                    panel_container(
                        "RECENT ACTIVE SESSIONS",
                        "Sorted by activity",
                        flex_col(session_rows),
                        text_cyan,
                        text_gray,
                    )
                )
                .width(280.0_f32.px()),
            )),
            FlexSpacer::Fixed(20.0_f32.px()),

            // Details table
            flex_row((
                label("Usage History Log").text_size(18.0).color(text_white),
                FlexSpacer::Fixed(10.0_f32.px()),
                label("Historical data records").text_size(11.0).color(text_gray),
            )),
            FlexSpacer::Fixed(10.0_f32.px()),

            // Table headers
            flex_row((
                sized_box(label("PERIOD").text_size(11.0).color(text_gray)).width(140.0_f32.px()),
                sized_box(label("AGENT").text_size(11.0).color(text_gray)).width(100.0_f32.px()),
                sized_box(label("TOTAL TOKENS").text_size(11.0).color(text_gray)).flex(1.0),
                sized_box(label("COST").text_size(11.0).color(text_gray)).width(120.0_f32.px()),
            ))
            .padding(10.0),

            // Table rows
            flex_col(history_rows),
        ))
        .cross_axis_alignment(CrossAxisAlignment::Fill)
    };

    // Conditional tab content rendering using Option wrappers (which implement View)
    let ccusage_opt = if state.active_tab == "all" || state.active_tab == "codex" {
        Some(ccusage_view)
    } else {
        None
    };

    let antigravity_opt = if state.active_tab == "antigravity" {
        Some(antigravity_view)
    } else {
        None
    };

    let main_view = flex_col((
        // 1. Title bar
        flex_row((
            label(if state.active_tab == "all" { "TOKEN PULSE - ALL AGENTS" } else if state.active_tab == "codex" { "TOKEN PULSE - CODEX ONLY" } else { "ANTIGRAVITY SESSIONS" })
                .text_size(24.0)
                .color(text_white),
            FlexSpacer::Flex(1.0),
            text_button("Refresh", |state: &mut AppState| {
                if let Some(sender) = &state.refresh_sender {
                    let _ = sender.send(());
                }
            }),
        ))
        .cross_axis_alignment(CrossAxisAlignment::Center),

        FlexSpacer::Fixed(10.0_f32.px()),
        tab_bar,
        FlexSpacer::Fixed(15.0_f32.px()),

        ccusage_opt,
        antigravity_opt,
    ))
    .main_axis_alignment(MainAxisAlignment::Start)
    .cross_axis_alignment(CrossAxisAlignment::Fill)
    .padding(20.0);

    let main_container = sized_box(vertical_portal(main_view))
        .expand()
        .background_color(bg_color);

    println!("[UI] app_logic view tree built in {:?}", rebuild_start.elapsed());
    fork(
        main_container,
        worker(
            |proxy, mut rx| async move {
                let session_root = crate::config::get_default_session_root().to_string_lossy().to_string();
                let mut interval = tokio::time::interval(std::time::Duration::from_millis(30000));
                let mut last_signature = String::new();
                let mut telemetry_epoch: u64 = 0;

                println!("[Telemetry] Worker thread started. Session root: {}", session_root);

                loop {
                    tokio::select! {
                        _ = interval.tick() => {
                            println!("[Telemetry] Periodic tick: initiating background file scan...");
                        }
                        msg = rx.recv() => {
                            if msg.is_none() {
                                println!("[Telemetry] Worker channel closed. Exiting worker loop.");
                                break;
                            }
                            println!("[Telemetry] Manual refresh requested: initiating background file scan...");
                        }
                    }

                    // 1. Load the MonitorConfig
                    let config = crate::store::SettingsStore::new(&session_root).load_config();

                    // 2. Scan for candidates in a blocking task
                    let session_root_clone = session_root.clone();
                    let scan_start = std::time::Instant::now();
                    println!("[Telemetry] [Blocking Pool] Directory scan starting...");
                    
                    let candidates_result = tokio::task::spawn_blocking(move || {
                        let scanner = crate::scanner::SessionScanner::new();
                        scanner.scan(&session_root_clone)
                    }).await;

                    let candidates = match candidates_result {
                        Ok(Ok(c)) => c,
                        Ok(Err(e)) => {
                            println!("[Telemetry] Directory scan error: {}", e);
                            continue;
                        }
                        Err(e) => {
                            println!("[Telemetry] Directory scan panicked or failed to join: {:?}", e);
                            continue;
                        }
                    };
                    let scan_duration = scan_start.elapsed();

                    // Compute candidate signature
                    let mut signature_parts = Vec::new();
                    for c in &candidates {
                        signature_parts.push(c.signature.clone());
                    }
                    let candidate_signature = signature_parts.join("|");

                    // 3. Construct TrajectoryExporter and run export_changed_sessions
                    let exporter = crate::rpc::TrajectoryExporter::new(config);
                    println!("[Telemetry] Running trajectory exporter for {} candidates...", candidates.len());
                    let export_result = exporter.export_changed_sessions(&candidates, false, true).await;
                    let exported_count = match export_result {
                        Ok(count) => {
                            println!("[Telemetry] Telemetry export complete. Exported count: {}", count);
                            count
                        }
                        Err(e) => {
                            println!("[Telemetry] Telemetry export failed: {}", e);
                            0
                        }
                    };

                    if exported_count > 0 {
                        telemetry_epoch += 1;
                        println!("[Telemetry] Telemetry successfully exported! Incrementing telemetry epoch to {}.", telemetry_epoch);
                    }

                    // Calculate signature properly to trigger changes
                    let current_signature = format!("{}|epoch:{}", candidate_signature, telemetry_epoch);

                    if current_signature == last_signature {
                        println!(
                            "[Telemetry] Directory scan & export finished. Signature match (no changes). Found {} sessions.",
                            candidates.len()
                        );
                        continue;
                    }

                    println!(
                        "[Telemetry] Signature changed! Directory scan finished in {:?}. Initiating parallel parsing for {} sessions...",
                        scan_duration,
                        candidates.len()
                    );

                    let session_root_clone2 = session_root.clone();
                    let parse_start = std::time::Instant::now();
                    let parse_result = tokio::task::spawn_blocking(move || {
                        let scanner = crate::scanner::SessionScanner::new();
                        scanner.scan_and_parse_parallel(&session_root_clone2)
                    }).await;

                    let sessions = match parse_result {
                        Ok(Ok(s)) => s,
                        Ok(Err(e)) => {
                            println!("[Telemetry] Parallel parsing error: {}", e);
                            continue;
                        }
                        Err(e) => {
                            println!("[Telemetry] Parallel parsing panicked or failed to join: {:?}", e);
                            continue;
                        }
                    };
                    let parse_duration = parse_start.elapsed();

                    println!(
                        "[Telemetry] Parallel parsing completed: parsed {} sessions in {:?}",
                        sessions.len(),
                        parse_duration
                    );

                    // 4. Fetch ccusage snapshot in background
                    println!("[Telemetry] Initiating background ccusage snapshot fetch...");
                    let ccusage_snapshot = match crate::ccusage::fetch_ccusage_snapshot("Asia/Shanghai").await {
                        Ok(snap) => {
                            println!("[Telemetry] ccusage snapshot fetched successfully! Total ccusage tokens: {}", snap.totals.total_tokens);
                            Some(snap)
                        }
                        Err(e) => {
                            println!("[Telemetry] ccusage snapshot fetch failed: {}", e);
                            None
                        }
                    };

                    println!(
                        "[Telemetry] UI Update triggered! Setting new signature and dispatching sessions & ccusage to UI state."
                    );
                    last_signature = current_signature;
                    let _ = proxy.message((sessions, ccusage_snapshot));
                }
            },
            |state: &mut AppState, sender| {
                state.refresh_sender = Some(sender);
            },
            |state: &mut AppState, payload: (Vec<SessionTotals>, Option<crate::ccusage::CcusageSnapshot>)| {
                let (sessions, ccusage) = payload;
                state.session_count = sessions.len() as u64;
                state.active_sessions = sessions.iter().filter(|s| s.mode == "reported").count() as u64;
                state.total_tokens = sessions.iter().map(|s| s.breakdown.total_tokens).sum();
                state.sessions = sessions;
                state.ccusage_snapshot = ccusage;
                precalculate(state);
            }
        )
    )
}

fn format_with_commas(n: u64) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut result = String::new();
    let mut count = 0;
    for &b in bytes.iter().rev() {
        if count > 0 && count % 3 == 0 {
            result.push(',');
        }
        result.push(b as char);
        count += 1;
    }
    result.chars().rev().collect()
}

fn format_date(ms: u64) -> String {
    if let Some(dt) = chrono::DateTime::from_timestamp((ms / 1000) as i64, 0) {
        dt.format("%Y-%m-%d %H:%M:%S").to_string()
    } else {
        "Unknown Date".to_string()
    }
}

fn get_sparkline_heights(tokens: u64) -> Vec<f32> {
    if tokens == 0 {
        return vec![1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0];
    }
    let log_val = (tokens as f64).log10();
    if log_val < 3.0 {
        vec![1.0, 1.0, 2.0, 4.0, 4.0, 2.0, 1.0, 1.0]
    } else if log_val < 4.0 {
        vec![1.0, 2.0, 4.0, 8.0, 8.0, 4.0, 2.0, 1.0]
    } else if log_val < 5.0 {
        vec![1.0, 3.0, 6.0, 12.0, 12.0, 6.0, 3.0, 1.0]
    } else {
        vec![2.0, 5.0, 10.0, 16.0, 16.0, 10.0, 5.0, 2.0]
    }
}

fn render_sparkline(heights: Vec<f32>, color: xilem::Color) -> impl WidgetView<AppState> + use<> {

    flex_row((
        // Anchor height to 16px to ensure consistent vertical scaling
        sized_box(label(""))
            .width(0.0_f32.px())
            .height(16.0_f32.px()),
        sized_box(label(""))
            .width(3.0_f32.px())
            .height(heights[0].px())
            .background_color(color),
        FlexSpacer::Fixed(1.5_f32.px()),
        sized_box(label(""))
            .width(3.0_f32.px())
            .height(heights[1].px())
            .background_color(color),
        FlexSpacer::Fixed(1.5_f32.px()),
        sized_box(label(""))
            .width(3.0_f32.px())
            .height(heights[2].px())
            .background_color(color),
        FlexSpacer::Fixed(1.5_f32.px()),
        sized_box(label(""))
            .width(3.0_f32.px())
            .height(heights[3].px())
            .background_color(color),
        FlexSpacer::Fixed(1.5_f32.px()),
        sized_box(label(""))
            .width(3.0_f32.px())
            .height(heights[4].px())
            .background_color(color),
        FlexSpacer::Fixed(1.5_f32.px()),
        sized_box(label(""))
            .width(3.0_f32.px())
            .height(heights[5].px())
            .background_color(color),
        FlexSpacer::Fixed(1.5_f32.px()),
        sized_box(label(""))
            .width(3.0_f32.px())
            .height(heights[6].px())
            .background_color(color),
        FlexSpacer::Fixed(1.5_f32.px()),
        sized_box(label(""))
            .width(3.0_f32.px())
            .height(heights[7].px())
            .background_color(color),
    ))
    .cross_axis_alignment(CrossAxisAlignment::End)
}

fn truncate_path(path: &str, max_len: usize) -> String {
    if path.len() <= max_len {
        return path.to_string();
    }
    let parts: Vec<&str> = path.split('\\').collect();
    if parts.len() > 1 {
        let last = parts.last().unwrap_or(&"");
        if last.len() >= max_len - 5 {
            format!("...\\{}", &last[last.len() - (max_len - 5)..])
        } else {
            format!("...\\{}", last)
        }
    } else {
        format!("...{}", &path[path.len() - max_len + 3..])
    }
}

// === CUSTOM HOVER CONTAINER WIDGET & VIEW WRAPPER FOR PREMIUM HOT TOOLTIPS ===

use xilem::masonry::core::{
    BoxConstraints, EventCtx, LayoutCtx, PaintCtx, RegisterCtx, AccessCtx,
    Widget, WidgetPod, PointerEvent, PropertiesMut, PropertiesRef,
    ChildrenIds,
};
use xilem::masonry::kurbo::Size;
use xilem::masonry::vello::Scene;
use xilem::masonry::accesskit::{Node, Role};

use xilem::core::{MessageContext, Mut, View, ViewMarker, MessageResult, ViewId, ViewPathTracker};
use xilem::{Pod, ViewCtx};
use std::marker::PhantomData;

pub struct HoverWidget {
    child: WidgetPod<dyn Widget>,
}

impl HoverWidget {
    pub fn new(child: xilem::masonry::core::NewWidget<impl Widget + ?Sized>) -> Self {
        Self {
            child: child.erased().to_pod(),
        }
    }
    
    pub fn child_mut<'t>(this: &'t mut xilem::masonry::core::WidgetMut<'_, Self>) -> xilem::masonry::core::WidgetMut<'t, dyn Widget> {
        let child = &mut this.widget.child;
        this.ctx.get_mut(child)
    }
}

impl Widget for HoverWidget {
    type Action = bool; // true = hovered, false = left

    fn accepts_pointer_interaction(&self) -> bool {
        true
    }

    fn register_children(&mut self, ctx: &mut RegisterCtx<'_>) {
        ctx.register_child(&mut self.child);
    }

    fn layout(
        &mut self,
        ctx: &mut LayoutCtx<'_>,
        _props: &mut PropertiesMut<'_>,
        bc: &BoxConstraints,
    ) -> Size {
        let size = ctx.run_layout(&mut self.child, bc);
        ctx.place_child(&mut self.child, xilem::masonry::kurbo::Point::ORIGIN);
        size
    }

    fn paint(&mut self, _ctx: &mut PaintCtx<'_>, _props: &PropertiesRef<'_>, _scene: &mut Scene) {}

    fn accessibility_role(&self) -> Role {
        Role::GenericContainer
    }

    fn accessibility(&mut self, _ctx: &mut AccessCtx<'_>, _props: &PropertiesRef<'_>, _node: &mut Node) {}

    fn children_ids(&self) -> ChildrenIds {
        ChildrenIds::from_slice(&[self.child.id()])
    }

    fn on_pointer_event(
        &mut self,
        ctx: &mut EventCtx<'_>,
        _props: &mut PropertiesMut<'_>,
        event: &PointerEvent,
    ) {
        match event {
            PointerEvent::Enter(_) => {
                ctx.submit_action::<bool>(true);
            }
            PointerEvent::Leave(_) | PointerEvent::Cancel(_) => {
                ctx.submit_action::<bool>(false);
            }
            _ => {}
        }
    }
}

pub struct Hoverable<V, F, State, Action> {
    child: V,
    callback: F,
    phantom: PhantomData<fn() -> (State, Action)>,
}

pub fn hoverable<State, Action, V>(
    child: V,
    callback: impl Fn(&mut State, bool) -> Action + Send + Sync + 'static,
) -> Hoverable<V, impl for<'a> Fn(&'a mut State, bool) -> MessageResult<Action> + Send + Sync + 'static, State, Action>
where
    V: WidgetView<State, Action>,
{
    Hoverable {
        child,
        callback: move |state: &mut State, hovered| MessageResult::Action(callback(state, hovered)),
        phantom: PhantomData,
    }
}

const HOVER_CONTENT_VIEW_ID: ViewId = ViewId::new(0);

impl<F, V, State, Action> ViewMarker for Hoverable<V, F, State, Action> {}
impl<F, V, State, Action> View<State, Action, ViewCtx> for Hoverable<V, F, State, Action>
where
    V: WidgetView<State, Action>,
    F: Fn(&mut State, bool) -> MessageResult<Action> + Send + Sync + 'static,
    State: 'static,
    Action: 'static,
{
    type Element = Pod<HoverWidget>;
    type ViewState = V::ViewState;

    fn build(&self, ctx: &mut ViewCtx, app_state: &mut State) -> (Self::Element, Self::ViewState) {
        let (child, child_state) = ctx.with_id(HOVER_CONTENT_VIEW_ID, |ctx| {
            View::<State, Action, _>::build(&self.child, ctx, app_state)
        });
        (
            ctx.with_action_widget(|ctx| {
                ctx.create_pod(HoverWidget::new(child.new_widget))
            }),
            child_state,
        )
    }

    fn rebuild(
        &self,
        prev: &Self,
        state: &mut Self::ViewState,
        ctx: &mut ViewCtx,
        mut element: Mut<'_, Self::Element>,
        app_state: &mut State,
    ) {
        ctx.with_id(HOVER_CONTENT_VIEW_ID, |ctx| {
            let mut child = HoverWidget::child_mut(&mut element);
            View::<State, Action, _>::rebuild(
                &self.child,
                &prev.child,
                state,
                ctx,
                child.downcast(),
                app_state,
            );
        });
    }

    fn teardown(
        &self,
        view_state: &mut Self::ViewState,
        ctx: &mut ViewCtx,
        mut element: Mut<'_, Self::Element>,
    ) {
        ctx.with_id(HOVER_CONTENT_VIEW_ID, |ctx| {
            let mut child = HoverWidget::child_mut(&mut element);
            View::<State, Action, _>::teardown(
                &self.child,
                view_state,
                ctx,
                child.downcast(),
            );
        });
        ctx.teardown_leaf(element);
    }

    fn message(
        &self,
        view_state: &mut Self::ViewState,
        message: &mut MessageContext,
        mut element: Mut<'_, Self::Element>,
        app_state: &mut State,
    ) -> MessageResult<Action> {
        match message.take_first() {
            Some(HOVER_CONTENT_VIEW_ID) => {
                let mut child = HoverWidget::child_mut(&mut element);
                self.child.message(
                    view_state,
                    message,
                    child.downcast(),
                    app_state,
                )
            }
            None => match message.take_message::<bool>() {
                Some(hovered) => (self.callback)(app_state, *hovered),
                None => MessageResult::Stale,
            },
            _ => MessageResult::Stale,
        }
    }
}

// === CUSTOM VERTICAL PORTAL VIEW WRAPPER FOR HORIZONTAL STRETCHING ===

pub struct VerticalPortal<V, State, Action> {
    child: V,
    phantom: std::marker::PhantomData<(State, Action)>,
}

pub fn vertical_portal<State, Action, V>(child: V) -> VerticalPortal<V, State, Action>
where
    V: WidgetView<State, Action>,
{
    VerticalPortal {
        child,
        phantom: std::marker::PhantomData,
    }
}

impl<V, State, Action> ViewMarker for VerticalPortal<V, State, Action> {}

impl<Child, State, Action> View<State, Action, ViewCtx> for VerticalPortal<Child, State, Action>
where
    Child: WidgetView<State, Action>,
    State: 'static,
    Action: 'static,
{
    type Element = Pod<xilem::masonry::widgets::Portal<Child::Widget>>;
    type ViewState = Child::ViewState;

    fn build(&self, ctx: &mut ViewCtx, app_state: &mut State) -> (Self::Element, Self::ViewState) {
        let (child, child_state) = self.child.build(ctx, app_state);
        let widget_pod = ctx.create_pod(
            xilem::masonry::widgets::Portal::new(child.new_widget)
                .constrain_horizontal(true)
        );
        (widget_pod, child_state)
    }

    fn rebuild(
        &self,
        prev: &Self,
        view_state: &mut Self::ViewState,
        ctx: &mut ViewCtx,
        mut element: Mut<'_, Self::Element>,
        app_state: &mut State,
    ) {
        let child_element = xilem::masonry::widgets::Portal::child_mut(&mut element);
        self.child
            .rebuild(&prev.child, view_state, ctx, child_element, app_state);
    }

    fn teardown(
        &self,
        view_state: &mut Self::ViewState,
        ctx: &mut ViewCtx,
        mut element: Mut<'_, Self::Element>,
    ) {
        let child_element = xilem::masonry::widgets::Portal::child_mut(&mut element);
        self.child.teardown(view_state, ctx, child_element);
    }

    fn message(
        &self,
        view_state: &mut Self::ViewState,
        message: &mut MessageContext,
        mut element: Mut<'_, Self::Element>,
        app_state: &mut State,
    ) -> MessageResult<Action> {
        let child_element = xilem::masonry::widgets::Portal::child_mut(&mut element);
        self.child
            .message(view_state, message, child_element, app_state)
    }
}
