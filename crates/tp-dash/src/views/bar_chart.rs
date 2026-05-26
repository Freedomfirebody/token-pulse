//! 横向条形图组件。

use xilem::masonry::properties::types::{AsUnit, CrossAxisAlignment};
use xilem::view::{flex_col, flex_row, label, sized_box, FlexSpacer};
use xilem::style::Style;
use xilem::{Color, WidgetView};

use crate::theme;

/// 条形图数据
pub struct BarEntry {
    pub left_label: String,
    pub right_label: String,
    pub value: u64,
    pub color: Color,
}

/// 创建横向条形图
///
/// ```text
/// ┌──────────────────────────────────┐
/// │ LEFT_LABEL     ████████  RIGHT   │
/// │ LEFT_LABEL     ████      RIGHT   │
/// │ LEFT_LABEL     ██        RIGHT   │
/// └──────────────────────────────────┘
/// ```
pub fn bar_chart<State: 'static>(
    entries: &[BarEntry],
) -> impl WidgetView<State> {
    let max_value = entries.iter().map(|e| e.value).max().unwrap_or(1).max(1);

    let rows: Vec<_> = entries.iter().map(|entry| {
        let pct = entry.value as f64 / max_value as f64;
        let bar_width = (pct * 200.0).max(2.0) as f32;
        let left = entry.left_label.clone();
        let right = entry.right_label.clone();
        let color = entry.color;

        flex_row((
            sized_box(
                label(left).text_size(theme::FONT_SIZE_BODY).color(theme::TEXT_PRIMARY)
            ).width(120.0_f32.px()),
            FlexSpacer::Fixed(8.0_f32.px()),
            sized_box(
                sized_box(label("")).background_color(color).corner_radius(3.0)
            ).width(bar_width.px()).height(16.0_f32.px()),
            FlexSpacer::Flex(1.0),
            label(right).text_size(theme::FONT_SIZE_BODY).color(theme::TEXT_SECONDARY),
        ))
        .cross_axis_alignment(CrossAxisAlignment::Center)
    }).collect();

    flex_col(rows)
}
