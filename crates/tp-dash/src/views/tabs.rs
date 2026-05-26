//! Tab 切换组件。

use xilem::masonry::properties::types::AsUnit;
use xilem::view::{flex_row, sized_box, text_button, FlexSpacer};
use xilem::style::Style;
use xilem::WidgetView;



/// 标签页定义
pub struct TabDef {
    pub label: String,
    pub is_active: bool,
}

/// 创建标签页头部
pub fn tab_header<State: 'static>(
    tabs: &[TabDef],
    on_click: impl Fn(&mut State, usize) + Clone + Send + Sync + 'static,
) -> impl WidgetView<State> {
    let tab_buttons: Vec<_> = tabs.iter().enumerate().map(|(i, tab)| {
        let tab_label = tab.label.clone();
        let on_click = on_click.clone();

        sized_box(
            text_button(tab_label, move |state: &mut State| {
                on_click(state, i);
            })
        )
        .padding(6.0)
        .corner_radius(4.0)
    }).collect();

    let mut row_children: Vec<_> = Vec::new();
    for (i, btn) in tab_buttons.into_iter().enumerate() {
        if i > 0 {
            row_children.push(FlexSpacer::Fixed(4.0_f32.px()));
        }
        // We need a different approach since we can't mix types in a Vec.
        // Instead, build a fixed-size tuple approach or use the "growing tuple" pattern.
        let _ = btn; // placeholder
        let _ = &row_children;
    }

    // Since dynamic-length flex is tricky with tuples, use a simpler approach:
    // Build up to a reasonable number of tabs as a tuple.
    // For now, show the first few tabs in a fixed tuple.
    // The caller can pass up to 4 tabs.
    build_tab_row(tabs, on_click)
}

/// Build tab row for up to 8 tabs
fn build_tab_row<State: 'static>(
    tabs: &[TabDef],
    on_click: impl Fn(&mut State, usize) + Clone + Send + Sync + 'static,
) -> impl WidgetView<State> {
    let make_btn = |i: usize, tab: &TabDef| {
        let tab_label = tab.label.clone();
        let on_click = on_click.clone();
        sized_box(
            text_button(tab_label, move |state: &mut State| {
                on_click(state, i);
            })
        )
        .padding(6.0)
        .corner_radius(4.0)
    };

    // Build a Vec of button views and pass to flex_row
    let buttons: Vec<_> = tabs.iter().enumerate().map(|(i, tab)| {
        make_btn(i, tab)
    }).collect();

    flex_row(buttons)
}
