//! Token Breakdown 属性配比面板组件。
//! 支持两种呈现模式：
//! 1. 垂直模式 (Vertical) - 用于紧凑侧边栏 (300px 宽度)。
//! 2. 横向模式 (Horizontal) - 用于宽屏自适应容器 (560px 宽度，带 2x2 网格卡片和圆角进度条)。

use xilem::view::{flex_row, flex_col, label, sized_box, FlexSpacer};
use xilem::masonry::properties::types::{AsUnit, CrossAxisAlignment};
use xilem::{WidgetView, AnyWidgetView};
use xilem::style::Style;

use crate::theme;

/// Token Breakdown 维度数据包
#[derive(Clone, Default)]
pub struct TokenBreakdownData {
    pub total_tokens: u64,
    pub total_classified: u64,
    pub classified_percent: f64,
    pub total_input: u64,
    pub p_input: f64,
    pub w_input: f32,
    pub total_output: u64,
    pub p_output: f64,
    pub w_output: f32,
    pub total_cache: u64,
    pub p_cache: f64,
    pub w_cache: f32,
    pub total_reasoning: u64,
    pub p_reasoning: f64,
    pub w_reasoning: f32,
}

/// 渲染比例色条中的单个分段
fn render_bar_segment<State: 'static>(width: f32, height: f32, color: xilem::Color) -> impl WidgetView<State> {
    sized_box(label(""))
        .width(width.px())
        .height(height.px())
        .background_color(color)
}

/// 渲染详细指标条目行 (垂直列表模式)
fn render_details_row<State: 'static>(
    name: &str,
    count: u64,
    percent: f64,
    color: xilem::Color,
) -> impl WidgetView<State> {
    sized_box(
        flex_row((
            sized_box(label(""))
                .width(10.0_f32.px())
                .height(10.0_f32.px())
                .background_color(color)
                .corner_radius(2.0),
            FlexSpacer::Fixed(10.0_f32.px()),
            label(name.to_string()).text_size(theme::FONT_SIZE_BODY).color(theme::TEXT_PRIMARY),
            FlexSpacer::Flex(1.0),
            label(format!("{} ({:.1}%)", theme::format_with_commas(count), percent))
                .text_size(theme::FONT_SIZE_BODY)
                .color(theme::TEXT_SECONDARY),
        ))
        .cross_axis_alignment(CrossAxisAlignment::Center)
        .padding(8.0)
    )
    .width(300.0_f32.px())
    .background_color(theme::BG_INPUT)
    .padding(4.0)
    .corner_radius(theme::CARD_CORNER_RADIUS)
}

/// 渲染 2x2 网格卡片中的单个指标卡片 (横向自适应模式)
fn render_horizontal_grid_card<State: 'static>(
    name: &str,
    count: u64,
    percent: f64,
    color: xilem::Color,
) -> impl WidgetView<State> {
    sized_box(
        flex_col((
            // 第一行：指示色块 + 维度名称
            flex_row((
                sized_box(label(""))
                    .width(10.0_f32.px())
                    .height(10.0_f32.px())
                    .background_color(color)
                    .corner_radius(2.0),
                FlexSpacer::Fixed(8.0_f32.px()),
                label(name.to_string())
                    .text_size(theme::FONT_SIZE_SMALL)
                    .color(theme::TEXT_SECONDARY),
            ))
            .cross_axis_alignment(CrossAxisAlignment::Center),
            
            FlexSpacer::Fixed(6.0_f32.px()),
            
            // 第二行：指标数值 + 占比
            flex_row((
                label(theme::format_with_commas(count))
                    .text_size(theme::FONT_SIZE_BODY)
                    .color(theme::TEXT_PRIMARY),
                FlexSpacer::Fixed(6.0_f32.px()),
                label(format!("({:.1}%)", percent))
                    .text_size(theme::FONT_SIZE_BODY)
                    .color(theme::TEXT_MUTED),
            ))
            .cross_axis_alignment(CrossAxisAlignment::Center),
        ))
        .cross_axis_alignment(CrossAxisAlignment::Start)
        .padding(12.0)
    )
    .width(270.0_f32.px()) // 2 列卡片： 270px + 20px gap + 270px = 560px 宽度
    .background_color(theme::BG_INPUT)
    .corner_radius(theme::CARD_CORNER_RADIUS)
}

/// 渲染垂直紧凑模式 (Vertical Mode) - 用于侧边栏 (300px)
pub fn breakdown_view_vertical<State: 'static>(data: TokenBreakdownData) -> Box<AnyWidgetView<State>> {
    let mut bar_segments = Vec::new();
    if data.w_input > 0.0 {
        bar_segments.push(render_bar_segment(data.w_input, 14.0, theme::COLOR_INPUT));
    }
    if data.w_output > 0.0 {
        bar_segments.push(render_bar_segment(data.w_output, 14.0, theme::COLOR_OUTPUT));
    }
    if data.w_cache > 0.0 {
        bar_segments.push(render_bar_segment(data.w_cache, 14.0, theme::COLOR_CACHE));
    }
    if data.w_reasoning > 0.0 {
        bar_segments.push(render_bar_segment(data.w_reasoning, 14.0, theme::COLOR_REASONING));
    }

    let segmented_bar = sized_box(
        flex_row(bar_segments).gap(1.5_f32.px())
    )
    .width(300.0_f32.px())
    .height(14.0_f32.px())
    .background_color(theme::BG_INPUT)
    .corner_radius(7.0);

    flex_col((
        label(theme::format_with_commas(data.total_tokens))
            .text_size(theme::FONT_SIZE_TITLE)
            .color(theme::TEXT_PRIMARY),
        label("TOTAL TOKENS").text_size(theme::FONT_SIZE_SMALL).color(theme::TEXT_MUTED),
        FlexSpacer::Fixed(4.0_f32.px()),
        label(format!(
            "{} categorized • {:.1}% of total classified",
            theme::format_with_commas(data.total_classified),
            data.classified_percent
        ))
        .text_size(theme::FONT_SIZE_SMALL)
        .color(theme::TEXT_MUTED),
        FlexSpacer::Fixed(12.0_f32.px()),
        segmented_bar,
        FlexSpacer::Fixed(16.0_f32.px()),
        render_details_row("INPUT", data.total_input, data.p_input, theme::COLOR_INPUT),
        FlexSpacer::Fixed(6.0_f32.px()),
        render_details_row("OUTPUT", data.total_output, data.p_output, theme::COLOR_OUTPUT),
        FlexSpacer::Fixed(6.0_f32.px()),
        render_details_row("CACHE", data.total_cache, data.p_cache, theme::COLOR_CACHE),
        FlexSpacer::Fixed(6.0_f32.px()),
        render_details_row("REASONING", data.total_reasoning, data.p_reasoning, theme::COLOR_REASONING),
    ))
    .boxed()
}

/// 渲染横向自适应模式 (Horizontal Mode) - 用于居中布局 (560px 宽度，带 2x2 网格)
pub fn breakdown_view_horizontal<State: 'static>(data: TokenBreakdownData) -> Box<AnyWidgetView<State>> {
    // 进度条在横向模式下较宽，按 560 / 300 比例无损缩放
    let scale = 560.0_f32 / 300.0_f32;
    let height = 18.0_f32; // 更厚实、精致的进度条

    let mut bar_segments = Vec::new();
    if data.w_input > 0.0 {
        bar_segments.push(render_bar_segment(data.w_input * scale, height, theme::COLOR_INPUT));
    }
    if data.w_output > 0.0 {
        bar_segments.push(render_bar_segment(data.w_output * scale, height, theme::COLOR_OUTPUT));
    }
    if data.w_cache > 0.0 {
        bar_segments.push(render_bar_segment(data.w_cache * scale, height, theme::COLOR_CACHE));
    }
    if data.w_reasoning > 0.0 {
        bar_segments.push(render_bar_segment(data.w_reasoning * scale, height, theme::COLOR_REASONING));
    }

    let segmented_bar = sized_box(
        flex_row(bar_segments).gap(1.5_f32.px())
    )
    .width(560.0_f32.px())
    .height(height.px())
    .background_color(theme::BG_INPUT)
    .corner_radius(9.0);

    // 2x2 指标卡片网格
    let grid_row_1 = flex_row((
        render_horizontal_grid_card("INPUT", data.total_input, data.p_input, theme::COLOR_INPUT),
        FlexSpacer::Fixed(20.0_f32.px()),
        render_horizontal_grid_card("OUTPUT", data.total_output, data.p_output, theme::COLOR_OUTPUT),
    ));

    let grid_row_2 = flex_row((
        render_horizontal_grid_card("CACHE", data.total_cache, data.p_cache, theme::COLOR_CACHE),
        FlexSpacer::Fixed(20.0_f32.px()),
        render_horizontal_grid_card("REASONING", data.total_reasoning, data.p_reasoning, theme::COLOR_REASONING),
    ));

    let grid = flex_col((
        grid_row_1,
        FlexSpacer::Fixed(12.0_f32.px()),
        grid_row_2,
    ))
    .cross_axis_alignment(CrossAxisAlignment::Fill);

    flex_col((
        label(theme::format_with_commas(data.total_tokens))
            .text_size(theme::FONT_SIZE_TITLE)
            .color(theme::TEXT_PRIMARY),
        label("TOTAL TOKENS").text_size(theme::FONT_SIZE_SMALL).color(theme::TEXT_MUTED),
        FlexSpacer::Fixed(4.0_f32.px()),
        label(format!(
            "{} categorized • {:.1}% of total classified",
            theme::format_with_commas(data.total_classified),
            data.classified_percent
        ))
        .text_size(theme::FONT_SIZE_SMALL)
        .color(theme::TEXT_MUTED),
        FlexSpacer::Fixed(16.0_f32.px()),
        segmented_bar,
        FlexSpacer::Fixed(20.0_f32.px()),
        grid,
    ))
    .cross_axis_alignment(CrossAxisAlignment::Start)
    .boxed()
}
