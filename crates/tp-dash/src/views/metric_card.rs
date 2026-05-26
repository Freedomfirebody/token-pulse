//! KPI 指标卡片组件。

use xilem::masonry::properties::types::{AsUnit, CrossAxisAlignment};
use xilem::view::{flex_col, label, sized_box, FlexSpacer};
use xilem::style::Style;
use xilem::{Color, WidgetView};

use crate::theme;

/// 创建 KPI 指标卡片
///
/// ```text
/// ┌─────────────────────────┐
/// │  TITLE                  │
/// │  1,234,567 tokens       │
/// │  $12.34                 │
/// └─────────────────────────┘
/// ```
pub fn metric_card<State: 'static>(
    title: &str,
    value: u64,
    cost: f64,
    accent_color: Color,
) -> impl WidgetView<State> {
    let title_str = title.to_string();
    let value_str = theme::format_with_commas(value);
    let cost_str = theme::format_cost(cost);

    sized_box(
        flex_col((
            label(title_str)
                .text_size(theme::FONT_SIZE_SMALL)
                .color(theme::TEXT_SECONDARY),
            FlexSpacer::Fixed(4.0_f32.px()),
            label(value_str)
                .text_size(theme::FONT_SIZE_KPI)
                .color(theme::TEXT_PRIMARY),
            FlexSpacer::Fixed(2.0_f32.px()),
            label(cost_str)
                .text_size(theme::FONT_SIZE_BODY)
                .color(accent_color),
        ))
        .cross_axis_alignment(CrossAxisAlignment::Start)
    )
    .expand_width()
    .height((theme::CARD_HEIGHT as f32).px())
    .background_color(theme::BG_CARD)
    .padding(theme::CARD_PADDING)
    .corner_radius(theme::CARD_CORNER_RADIUS)
}

/// 创建小型指标卡片 (仅显示值)
pub fn mini_metric_card<State: 'static>(
    title: &str,
    value_text: String,
    accent_color: Color,
) -> impl WidgetView<State> {
    let title_str = title.to_string();

    sized_box(
        flex_col((
            label(title_str)
                .text_size(theme::FONT_SIZE_SMALL)
                .color(theme::TEXT_MUTED),
            FlexSpacer::Fixed(2.0_f32.px()),
            label(value_text)
                .text_size(theme::FONT_SIZE_HEADING)
                .color(accent_color),
        ))
        .cross_axis_alignment(CrossAxisAlignment::Start)
    )
    .expand_width()
    .background_color(theme::BG_CARD)
    .padding(10.0)
    .corner_radius(theme::CARD_CORNER_RADIUS)
}
