//! 会话列表分析 (Session Analysis) 表格组件。

use xilem::view::{flex_row, flex_col, label, sized_box, FlexSpacer, FlexExt as _};
use xilem::masonry::properties::types::{AsUnit, CrossAxisAlignment};
use xilem::{WidgetView, AnyWidgetView};
use xilem::style::Style;

use crate::theme;

/// 会话行数据结构
#[derive(Clone)]
pub struct SessionRow {
    pub key: String,
    pub active_desc: String,
    pub mode_text: String,
    pub is_active: bool,
    pub record_count: u64,
    pub total_tokens: u64,
    pub sparkline_heights: Vec<f32>,
    pub sparkline_color: xilem::Color,
}

/// 会话列表数据包
#[derive(Clone, Default)]
pub struct SessionTableData {
    pub rows: Vec<SessionRow>,
}

/// SessionTable 封装组件
#[derive(Clone, Default)]
pub struct SessionTableComponent {
    pub data: SessionTableData,
}

impl SessionTableComponent {
    pub fn new() -> Self {
        Self {
            data: SessionTableData::default(),
        }
    }

    /// 根据全局数据树投影出子组件的分析数据
    pub fn update(&mut self, summary: &tp_protocol::view::DashboardView) {
        self.data.rows = summary.by_project.iter().map(|entry| {
            let total_tokens = entry.token_info.total();
            let heights = calculate_sparkline_heights(total_tokens);
            let sparkline_color = if entry.token_info.cache > 0 {
                theme::TEXT_CYAN
            } else {
                theme::COLOR_CACHE
            };

            SessionRow {
                key: entry.key.clone(),
                active_desc: "Active Session • Synced".to_string(),
                mode_text: "[ACTIVE]".to_string(),
                is_active: true,
                record_count: entry.record_count,
                total_tokens,
                sparkline_heights: heights,
                sparkline_color,
            }
        }).collect();
    }

    /// 渲染 Session 分析表格视图
    pub fn view(&mut self) -> Box<AnyWidgetView<Self>> {
        let data = self.data.clone();
        
        // 1. 表头
        let table_header = flex_row((
            sized_box(label("SESSION").text_size(theme::FONT_SIZE_SMALL).color(theme::TEXT_MUTED)).flex(1.0),
            sized_box(label("MODE").text_size(theme::FONT_SIZE_SMALL).color(theme::TEXT_MUTED)).width(110.0_f32.px()),
            sized_box(label("MESSAGES").text_size(theme::FONT_SIZE_SMALL).color(theme::TEXT_MUTED)).width(90.0_f32.px()),
            sized_box(label("TOTAL TOKENS").text_size(theme::FONT_SIZE_SMALL).color(theme::TEXT_MUTED)).width(140.0_f32.px()),
            sized_box(label("ACTIVITY PULSE").text_size(theme::FONT_SIZE_SMALL).color(theme::TEXT_MUTED)).width(80.0_f32.px()),
        ))
        .padding(10.0);

        // 2. 数据行 (使用 into_iter 传值映射)
        let table_rows: Vec<_> = data.rows.into_iter().map(|row| render_table_row(row)).collect();

        // 3. 带标题的面包容器包裹
        crate::views::panel::panel_container(
            "Session Analysis",
            "Sorted by recent activity",
            flex_col((
                table_header,
                FlexSpacer::Fixed(8.0_f32.px()),
                flex_col(table_rows).gap(8.0_f32.px()),
            )),
            theme::TEXT_CYAN,
            theme::TEXT_MUTED,
        )
        .boxed()
    }
}

/// 根据总 Token 数计算 Sparkline 迷你脉冲条高度 (高保真占位计算)
pub fn calculate_sparkline_heights(tokens: u64) -> Vec<f32> {
    if tokens == 0 {
        vec![2.0, 2.0, 2.0, 2.0, 2.0, 2.0, 2.0, 2.0]
    } else if tokens < 1000 {
        vec![2.0, 3.0, 4.0, 5.0, 4.0, 3.0, 2.0, 2.0]
    } else if tokens < 10000 {
        vec![2.0, 4.0, 8.0, 10.0, 9.0, 5.0, 3.0, 2.0]
    } else if tokens < 100000 {
        vec![3.0, 6.0, 12.0, 14.0, 12.0, 8.0, 4.0, 2.0]
    } else {
        vec![2.0, 5.0, 10.0, 16.0, 16.0, 10.0, 5.0, 2.0]
    }
}

/// 渲染迷你脉冲 Sparkline
fn render_sparkline<State: 'static>(heights: Vec<f32>, color: xilem::Color) -> impl WidgetView<State> {
    flex_row((
        sized_box(label(""))
            .width(0.0_f32.px())
            .height(16.0_f32.px()),
        sized_box(label(""))
            .width(3.0_f32.px())
            .height(heights[0].px())
            .background_color(color)
            .corner_radius(1.5),
        FlexSpacer::Fixed(1.5_f32.px()),
        sized_box(label(""))
            .width(3.0_f32.px())
            .height(heights[1].px())
            .background_color(color)
            .corner_radius(1.5),
        FlexSpacer::Fixed(1.5_f32.px()),
        sized_box(label(""))
            .width(3.0_f32.px())
            .height(heights[2].px())
            .background_color(color)
            .corner_radius(1.5),
        FlexSpacer::Fixed(1.5_f32.px()),
        sized_box(label(""))
            .width(3.0_f32.px())
            .height(heights[3].px())
            .background_color(color)
            .corner_radius(1.5),
        FlexSpacer::Fixed(1.5_f32.px()),
        sized_box(label(""))
            .width(3.0_f32.px())
            .height(heights[4].px())
            .background_color(color)
            .corner_radius(1.5),
        FlexSpacer::Fixed(1.5_f32.px()),
        sized_box(label(""))
            .width(3.0_f32.px())
            .height(heights[5].px())
            .background_color(color)
            .corner_radius(1.5),
        FlexSpacer::Fixed(1.5_f32.px()),
        sized_box(label(""))
            .width(3.0_f32.px())
            .height(heights[6].px())
            .background_color(color)
            .corner_radius(1.5),
        FlexSpacer::Fixed(1.5_f32.px()),
        sized_box(label(""))
            .width(3.0_f32.px())
            .height(heights[7].px())
            .background_color(color)
            .corner_radius(1.5),
    ))
    .cross_axis_alignment(CrossAxisAlignment::End)
}

/// 渲染同构表格每一行
fn render_table_row<State: 'static>(row: SessionRow) -> impl WidgetView<State> {
    sized_box(
        flex_row((
            flex_col((
                label(row.key).text_size(theme::FONT_SIZE_HEADING).color(theme::TEXT_PRIMARY),
                FlexSpacer::Fixed(4.0_f32.px()),
                label(row.active_desc).text_size(theme::FONT_SIZE_SMALL).color(theme::TEXT_MUTED),
            ))
            .flex(1.0),

            sized_box(
                label(row.mode_text).text_size(theme::FONT_SIZE_BODY).color(
                    if row.is_active { theme::COLOR_SUCCESS } else { theme::COLOR_WARNING }
                )
            )
            .width(110.0_f32.px()),

            sized_box(
                label(format!("{} msg", row.record_count)).text_size(theme::FONT_SIZE_BODY).color(theme::TEXT_PRIMARY)
            )
            .width(90.0_f32.px()),

            sized_box(
                label(theme::format_with_commas(row.total_tokens)).text_size(theme::FONT_SIZE_BODY).color(theme::TEXT_CYAN)
            )
            .width(140.0_f32.px()),

            sized_box(
                render_sparkline(row.sparkline_heights, row.sparkline_color)
            )
            .width(80.0_f32.px()),
        ))
        .cross_axis_alignment(CrossAxisAlignment::Center)
        .padding(10.0)
    )
    .background_color(theme::BG_CARD)
    .corner_radius(theme::CARD_CORNER_RADIUS)
    .padding(4.0)
}
