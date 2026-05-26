//! Sparkline 迷你折线图 (占位实现)。

use xilem::view::label;
use xilem::style::Style;
use xilem::WidgetView;

use crate::theme;

/// 创建简单的迷你 sparkline 指标
///
/// 由于 Xilem 目前没有原生的折线图绘制，
/// 这里先用文本指标条代替。未来可用 Vello 自定义绘制。
pub fn sparkline<State: 'static>(
    values: &[u64],
    label_text: &str,
) -> impl WidgetView<State> {
    let total: u64 = values.iter().sum();
    let avg = if values.is_empty() { 0 } else { total / values.len() as u64 };
    let max = values.iter().max().copied().unwrap_or(0);

    let text = format!(
        "{} — avg: {} max: {}",
        label_text,
        theme::format_with_commas(avg),
        theme::format_with_commas(max)
    );

    label(text)
        .text_size(theme::FONT_SIZE_BODY)
        .color(theme::TEXT_SECONDARY)
}
