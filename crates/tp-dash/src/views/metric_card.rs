//! KPI 指标卡片组件。

use xilem::masonry::properties::types::{AsUnit, CrossAxisAlignment};
use xilem::view::{flex_col, label, sized_box, FlexSpacer, prose};
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
    let full_str = theme::format_with_commas(value);
    let abbreviated_str = theme::format_kpi_value(value);
    let cost_str = theme::format_cost(cost);

    // 计算自适应阈值：如果完整数值本身就已符合小屏幕的渲染宽度，则无需缩写（阈值设为0，始终显示完整版）
    // 否则，阈值设为完整文本估计宽度加上 10px 的安全裕量
    let threshold = if abbreviated_str == full_str {
        0.0
    } else {
        theme::estimate_kpi_text_width(&full_str) as f64 + 10.0
    };

    sized_box(
        flex_col((
            label(title_str)
                .text_size(theme::FONT_SIZE_SMALL)
                .color(theme::TEXT_SECONDARY),
            FlexSpacer::Fixed(4.0_f32.px()),
            crate::widgets::responsive_layout::<State, (), _, _>(
                prose(full_str)
                    .text_size(theme::FONT_SIZE_KPI)
                    .text_color(theme::TEXT_PRIMARY),
                prose(abbreviated_str)
                    .text_size(theme::FONT_SIZE_KPI)
                    .text_color(theme::TEXT_PRIMARY),
                threshold,
            ),
            FlexSpacer::Fixed(2.0_f32.px()),
            prose(cost_str)
                .text_size(theme::FONT_SIZE_BODY)
                .text_color(accent_color),
        ))
        .cross_axis_alignment(CrossAxisAlignment::Fill)
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

/// 创建费用指标卡片 (大字显示金额，小字显示辅助信息，如总 Token)
pub fn cost_metric_card<State: 'static>(
    title: &str,
    cost: f64,
    aux_value: u64,
    accent_color: Color,
) -> impl WidgetView<State> {
    let title_str = title.to_string();
    let full_cost_str = theme::format_cost(cost);
    let aux_str = format!("{} tokens", theme::format_kpi_value(aux_value));

    sized_box(
        flex_col((
            label(title_str)
                .text_size(theme::FONT_SIZE_SMALL)
                .color(theme::TEXT_SECONDARY),
            FlexSpacer::Fixed(4.0_f32.px()),
            prose(full_cost_str)
                .text_size(theme::FONT_SIZE_KPI)
                .text_color(theme::TEXT_PRIMARY),
            FlexSpacer::Fixed(2.0_f32.px()),
            prose(aux_str)
                .text_size(theme::FONT_SIZE_BODY)
                .text_color(accent_color),
        ))
        .cross_axis_alignment(CrossAxisAlignment::Fill)
    )
    .expand_width()
    .height((theme::CARD_HEIGHT as f32).px())
    .background_color(theme::BG_CARD)
    .padding(theme::CARD_PADDING)
    .corner_radius(theme::CARD_CORNER_RADIUS)
}
