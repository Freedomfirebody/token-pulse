//! 热力图组件 — 按日期显示 token 使用密度。

use xilem::Color;
use xilem::view::{flex_row, flex_col, label, sized_box, FlexSpacer};
use xilem::masonry::properties::types::{AsUnit, CrossAxisAlignment};
use xilem::WidgetView;
use xilem::style::Style;

use crate::theme;
use crate::widgets::hoverable;

/// 热力图单日统计结构 (专用于 Tooltip 显示)
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

/// 热力图组件数据包
#[derive(Clone, Default)]
pub struct HeatmapData {
    pub weeks: Vec<Vec<Color>>,
    pub stats: Vec<Vec<Option<HeatmapDayStats>>>,
}

/// 热力图交互状态
#[derive(Clone, Default, PartialEq, Eq)]
pub struct HeatmapUIState {
    pub hovered_cell: Option<(usize, usize)>,
}

fn format_m_k_tokens(tokens: u64) -> String {
    if tokens == 0 {
        "0".to_string()
    } else if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}K", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

/// 创建自定义悬浮 Tooltip 视图 (完全贴合图二的 premium 像素级美学设计)
pub fn build_custom_tooltip<State: 'static>(stats: HeatmapDayStats) -> impl WidgetView<State> {
    let row = |label_str: &str, val_str: &str| {
        flex_row((
            label(label_str.to_string()).text_size(theme::FONT_SIZE_BODY).color(theme::TEXT_SECONDARY),
            FlexSpacer::Flex(1.0),
            label(val_str.to_string()).text_size(theme::FONT_SIZE_BODY).color(theme::TEXT_PRIMARY),
        ))
    };

    let divider = || {
        sized_box(label("".to_string()))
            .height(1.0_f32.px())
            .background_color(theme::BORDER_SUBTLE)
    };

    sized_box(
        flex_col((
            // 块 1：头部与总 Token 汇总
            flex_col((
                // 1. 日期头部 (Centered Date)
                sized_box(
                    flex_row((
                        FlexSpacer::Flex(1.0),
                        label(stats.date_str).text_size(theme::FONT_SIZE_BODY).color(theme::TEXT_PRIMARY),
                        FlexSpacer::Flex(1.0),
                    ))
                )
                .expand_width()
                .padding(xilem::style::Padding::bottom(6.0)),
                
                divider(),
                FlexSpacer::Fixed(8.0_f32.px()),
                
                // 2. 总 Token 处理量 (Tokens Processed)
                flex_row((
                    label("Tokens Processed".to_string()).text_size(theme::FONT_SIZE_BODY).color(theme::TEXT_SECONDARY),
                    FlexSpacer::Flex(1.0),
                    label(format_m_k_tokens(stats.tokens_processed))
                        .text_size(16.0_f32)
                        .color(theme::TEXT_CYAN),
                )),
                FlexSpacer::Fixed(8.0_f32.px()),
                
                divider(),
                FlexSpacer::Fixed(8.0_f32.px()),
            ))
            .cross_axis_alignment(CrossAxisAlignment::Fill),

            // 块 2：详细分类 Token 量
            flex_col((
                row("Input", &format_m_k_tokens(stats.input_tokens)),
                FlexSpacer::Fixed(4.0_f32.px()),
                row("Output", &format_m_k_tokens(stats.output_tokens)),
                FlexSpacer::Fixed(4.0_f32.px()),
                row("Cache Read", &format_m_k_tokens(stats.cache_tokens)),
                FlexSpacer::Fixed(4.0_f32.px()),
                row("Cache Write", "0"),
                FlexSpacer::Fixed(4.0_f32.px()),
                row("Reasoning", &format_m_k_tokens(stats.reasoning_tokens)),
                FlexSpacer::Fixed(8.0_f32.px()),
            ))
            .cross_axis_alignment(CrossAxisAlignment::Fill),

            // 块 3：成本与消息统计
            flex_col((
                divider(),
                FlexSpacer::Fixed(8.0_f32.px()),
                
                // 4. 成本与消息统计 (Cost / Messages)
                flex_row((
                    label("Cost".to_string()).text_size(theme::FONT_SIZE_BODY).color(theme::TEXT_SECONDARY),
                    FlexSpacer::Flex(1.0),
                    label(format!("${:.2}", stats.cost)).text_size(theme::FONT_SIZE_BODY).color(theme::TEXT_PRIMARY),
                )),
                FlexSpacer::Fixed(4.0_f32.px()),
                flex_row((
                    label("Messages".to_string()).text_size(theme::FONT_SIZE_BODY).color(theme::TEXT_SECONDARY),
                    FlexSpacer::Flex(1.0),
                    label(theme::format_with_commas(stats.message_count)).text_size(theme::FONT_SIZE_BODY).color(theme::TEXT_PRIMARY),
                )),
            ))
            .cross_axis_alignment(CrossAxisAlignment::Fill),
        ))
        .cross_axis_alignment(CrossAxisAlignment::Fill)
    )
    .width(220.0_f32.px())
    .background_color(theme::BG_PANEL)
    .corner_radius(theme::CARD_CORNER_RADIUS)
    .padding(12.0)
}

/// 渲染 7x28 带月份标题的悬浮日历热力图
pub fn heatmap_view<State: 'static>(
    ui_state: HeatmapUIState,
    data: HeatmapData,
    on_hover: impl Fn(&mut State, (usize, usize), bool) + Clone + Send + Sync + 'static,
) -> impl WidgetView<State> {
    // 1. 月份标题栏
    let months_header = flex_row((
        sized_box(label("")).width(35.0_f32.px()),
        sized_box(label("Nov")).width(36.0_f32.px()),
        sized_box(label("Dec")).width(36.0_f32.px()),
        sized_box(label("Jan")).width(36.0_f32.px()),
        sized_box(label("Feb")).width(36.0_f32.px()),
        sized_box(label("Mar")).width(36.0_f32.px()),
        sized_box(label("Apr")).width(36.0_f32.px()),
        sized_box(label("May")).width(36.0_f32.px()),
    ));

    // 2. 热力图主网格 (28周 × 7天)
    let grid_columns: Vec<_> = data.weeks.iter().enumerate().map(|(c_idx, week_colors)| {
        let on_hover = on_hover.clone();
        flex_col(
            week_colors.iter().enumerate().map(move |(r_idx, &cell_color)| {
                let cell_view = sized_box(label(""))
                    .width(10.0_f32.px())
                    .height(10.0_f32.px())
                    .background_color(cell_color)
                    .padding(1.0);
                
                let on_hover = on_hover.clone();
                hoverable(cell_view, move |state: &mut State, hovered| {
                    on_hover(state, (c_idx, r_idx), hovered);
                })
            }).collect::<Vec<_>>()
        )
    }).collect();

    let heatmap_grid = flex_row(grid_columns);

    // 3. 构建 Tooltip 部分
    let mut tooltip_view = None;
    let mut tooltip_spacer = None;

    if let Some((c_idx, r_idx)) = ui_state.hovered_cell {
        if let Some(week_stats) = data.stats.get(c_idx) {
            if let Some(Some(stats)) = week_stats.get(r_idx) {
                tooltip_view = Some(build_custom_tooltip(stats.clone()));
                tooltip_spacer = Some(FlexSpacer::Fixed(15.0_f32.px()));
            }
        }
    }

    flex_row((
        // 星期标签
        sized_box(
            flex_col((
                sized_box(label("Mon")).height(12.0_f32.px()),
                FlexSpacer::Fixed(12.0_f32.px()),
                sized_box(label("Wed")).height(12.0_f32.px()),
                FlexSpacer::Fixed(12.0_f32.px()),
                sized_box(label("Fri")).height(12.0_f32.px()),
            ))
        ).width(35.0_f32.px()),

        // 热力图区域
        flex_col((
            months_header,
            FlexSpacer::Fixed(6.0_f32.px()),
            heatmap_grid,
        )),

        tooltip_spacer,
        tooltip_view,
    ))
}
