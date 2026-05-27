//! 面板容器组件。

use xilem::masonry::properties::types::{AsUnit, CrossAxisAlignment};
use xilem::view::{flex_row, flex_col, label, sized_box, text_button, FlexSpacer};
use xilem::style::Style;
use xilem::WidgetView;

use crate::theme;

/// 创建带标题和副标题的面板容器
pub fn panel_container<State: 'static, V: WidgetView<State> + 'static>(
    title: &str,
    subtitle: &str,
    content: V,
    title_color: xilem::Color,
    subtitle_color: xilem::Color,
) -> impl WidgetView<State> {
    let title_str = title.to_string();
    let subtitle_str = subtitle.to_string();

    sized_box(
        flex_col((
            label(title_str)
                .text_size(theme::FONT_SIZE_HEADING)
                .color(title_color),
            label(subtitle_str)
                .text_size(theme::FONT_SIZE_SMALL)
                .color(subtitle_color),
            FlexSpacer::Fixed(8.0_f32.px()),
            content,
        ))
        .cross_axis_alignment(CrossAxisAlignment::Start)
    )
    .expand_width()
    .background_color(theme::BG_PANEL)
    .padding(theme::PANEL_PADDING)
    .corner_radius(theme::CARD_CORNER_RADIUS)
}

/// 创建带标题、副标题以及布局调整控制按钮的面板容器 (支持自由移动)
pub fn panel_container_with_controls<State: 'static, V: WidgetView<State> + 'static>(
    title: &str,
    subtitle: &str,
    content: V,
    title_color: xilem::Color,
    subtitle_color: xilem::Color,
    on_move_up: impl Fn(&mut State) + Clone + Send + Sync + 'static,
    on_move_down: impl Fn(&mut State) + Clone + Send + Sync + 'static,
) -> impl WidgetView<State> {
    let title_str = title.to_string();
    let subtitle_str = subtitle.to_string();

    let header = flex_row((
        flex_col((
            label(title_str)
                .text_size(theme::FONT_SIZE_HEADING)
                .color(title_color),
            label(subtitle_str)
                .text_size(theme::FONT_SIZE_SMALL)
                .color(subtitle_color),
        )).cross_axis_alignment(CrossAxisAlignment::Start),
        FlexSpacer::Flex(1.0),
        text_button(" ▲ ".to_string(), on_move_up),
        FlexSpacer::Fixed(8.0_f32.px()),
        text_button(" ▼ ".to_string(), on_move_down),
    ))
    .cross_axis_alignment(CrossAxisAlignment::Center);

    sized_box(
        flex_col((
            header,
            FlexSpacer::Fixed(8.0_f32.px()),
            content,
        ))
        .cross_axis_alignment(CrossAxisAlignment::Start)
    )
    .expand_width()
    .background_color(theme::BG_PANEL)
    .padding(theme::PANEL_PADDING)
    .corner_radius(theme::CARD_CORNER_RADIUS)
}
