//! Model Usage 渲染组件。

use xilem::{Color, WidgetView, AnyWidgetView};
use xilem::view::{flex_row, flex_col, label, sized_box, FlexSpacer, FlexExt};
use xilem::masonry::properties::types::{AsUnit, CrossAxisAlignment};
use xilem::style::Style;
use crate::theme;

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

pub fn by_model_view<State: 'static>(
    model_usages: Vec<PrecalculatedModelUsage>,
    title_color: Color,
    subtitle_color: Color,
) -> Box<AnyWidgetView<State>> {
    let model_rows: Vec<_> = model_usages.iter().map(|usage| {
        let left_view = sized_box(
            label(usage.name.clone()).text_size(theme::FONT_SIZE_BODY).color(theme::TEXT_PRIMARY)
        );
        let right_view = label(usage.subtitle_str.clone()).text_size(theme::FONT_SIZE_BODY).color(theme::TEXT_SECONDARY);
        render_horizontal_bar_row(left_view, right_view, usage.fill_flex, usage.empty_flex)
    }).collect();

    crate::views::panel::panel_container(
        "MODEL USAGE",
        "Ranked by total tokens",
        flex_col(model_rows),
        title_color,
        subtitle_color,
    )
    .boxed()
}
