//! 会话列表分析 (Session Analysis) 表格组件。

use xilem::view::{flex_row, flex_col, label, sized_box, text_button, FlexSpacer, prose, FlexExt as _};
use xilem::masonry::properties::types::{AsUnit, CrossAxisAlignment};
use xilem::{WidgetView, AnyWidgetView};
use xilem::style::Style;

use crate::theme;

/// 表格的分页/滚动分页模式
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TableMode {
    /// 标准分页模式
    Pagination,
    /// 滚动分页模式 (Infinite Scroll / Load More)
    InfiniteScroll,
}

impl Default for TableMode {
    fn default() -> Self {
        TableMode::Pagination
    }
}

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
    pub is_calculated: bool,
}

/// 会话列表数据包
#[derive(Clone, Default)]
pub struct SessionTableData {
    pub rows: Vec<SessionRow>,
}

/// SessionTable 封装组件
#[derive(Clone)]
pub struct SessionTableComponent {
    pub data: SessionTableData,
    pub current_page: usize,
    pub page_size: usize,
    pub mode: TableMode,
    pub scroll_limit: usize,
}

impl Default for SessionTableComponent {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionTableComponent {
    pub fn new() -> Self {
        Self {
            data: SessionTableData::default(),
            current_page: 0,
            page_size: 5, // 每页显示5条，精美紧凑
            mode: TableMode::Pagination,
            scroll_limit: 5,
        }
    }

    /// 根据全局数据树投影出子组件的分析数据
    pub fn update(&mut self, summary: &tp_protocol::view::DashboardView) {
        use std::collections::HashMap;

        // 构建一个映射，用以检查该会话的 key 是否包含估算/计算出的 Datalog 记录
        let mut project_is_calculated = HashMap::new();
        for r in &summary.recent_records {
            if r.source_report_class == tp_protocol::ReportClass::Calculate {
                project_is_calculated.insert(r.source_project.clone(), true);
            }
        }

        let mut grouped: HashMap<String, SessionRow> = HashMap::new();

        for entry in &summary.by_project {
            let name = entry.display_name.clone()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| entry.key.clone());

            let total_tokens = entry.token_info.total();

            // 如果此会话中存在任何估计/计算类型的数据，则判定为计算结果
            let is_calc = project_is_calculated.get(&entry.key).copied().unwrap_or(false);

            grouped.entry(name.clone())
                .and_modify(|row| {
                    row.record_count += entry.record_count;
                    row.total_tokens += total_tokens;
                    if entry.token_info.cache > 0 {
                        row.sparkline_color = theme::TEXT_CYAN;
                    }
                    if is_calc {
                        row.is_calculated = true;
                        row.active_desc = "Active Session • Synced (Calculated)".to_string();
                    }
                })
                .or_insert_with(|| {
                    let sparkline_color = if entry.token_info.cache > 0 {
                        theme::TEXT_CYAN
                    } else {
                        theme::COLOR_CACHE
                    };

                    let active_desc = if is_calc {
                        "Active Session • Synced (Calculated)".to_string()
                    } else {
                        "Active Session • Synced (Precise)".to_string()
                    };

                    SessionRow {
                        key: name,
                        active_desc,
                        mode_text: "[ACTIVE]".to_string(),
                        is_active: true,
                        record_count: entry.record_count,
                        total_tokens,
                        sparkline_heights: Vec::new(),
                        sparkline_color,
                        is_calculated: is_calc,
                    }
                });
        }

        let mut rows: Vec<SessionRow> = grouped.into_values().collect();
        for row in &mut rows {
            row.sparkline_heights = calculate_sparkline_heights(row.total_tokens);
        }

        // 按总 Token 数降序排列，完成“有序排列”
        rows.sort_by(|a, b| b.total_tokens.cmp(&a.total_tokens));

        self.data.rows = rows;

        // 边界安全校验，防止页码溢出
        let rows_count = self.data.rows.len();
        let max_page = if rows_count == 0 {
            0
        } else {
            (rows_count - 1) / self.page_size
        };
        if self.current_page > max_page {
            self.current_page = max_page;
        }
    }

    /// 渲染 Session 分析表格视图
    pub fn view(&mut self) -> Box<AnyWidgetView<Self>> {
        let data_rows = self.data.rows.clone();
        let rows_count = data_rows.len();
        let current_page = self.current_page;
        let page_size = self.page_size;
        let data_mode = self.mode;
        let scroll_limit = self.scroll_limit;
        
        // 1. 标准表头 (与数据行精确对齐的列宽度)
        let table_header = flex_row((
            sized_box(label("#").text_size(theme::FONT_SIZE_SMALL).color(theme::TEXT_MUTED)).width(40.0_f32.px()),
            sized_box(label("SESSION / CONVERSATION").text_size(theme::FONT_SIZE_SMALL).color(theme::TEXT_MUTED)).flex(1.0),
            sized_box(label("STATUS").text_size(theme::FONT_SIZE_SMALL).color(theme::TEXT_MUTED)).width(100.0_f32.px()),
            sized_box(label("MESSAGES").text_size(theme::FONT_SIZE_SMALL).color(theme::TEXT_MUTED)).width(100.0_f32.px()),
            sized_box(label("TOTAL TOKENS").text_size(theme::FONT_SIZE_SMALL).color(theme::TEXT_MUTED)).width(140.0_f32.px()),
            sized_box(label("ACTIVITY PULSE").text_size(theme::FONT_SIZE_SMALL).color(theme::TEXT_MUTED)).width(100.0_f32.px()),
        ))
        .padding(10.0);

        // 2. 根据所选分页模式截取并渲染对应的数据行 (支持交替行背景色，体现标准表单有序质感)
        let (filtered_rows, start_index) = match data_mode {
            TableMode::Pagination => {
                let start = current_page * page_size;
                let end = (start + page_size).min(rows_count);
                if start < rows_count {
                    (&data_rows[start..end], start)
                } else {
                    (&[][..], 0)
                }
            }
            TableMode::InfiniteScroll => {
                let end = scroll_limit.min(rows_count);
                (&data_rows[0..end], 0)
            }
        };

        let table_rows: Vec<_> = filtered_rows
            .iter()
            .enumerate()
            .map(|(i, row)| {
                // 交替背景色：奇偶行颜色差异以提升网格辨识度
                let bg_color = if i % 2 == 0 { theme::BG_CARD } else { theme::BG_PANEL };
                render_table_row(start_index + i, row.clone(), bg_color)
            })
            .collect();

        // 3. 构建底部切换/分页控制面板
        let is_paginated = data_mode == TableMode::Pagination;
        let is_scroll = data_mode == TableMode::InfiniteScroll;

        // 底部左侧：模式切换按钮
        let mode_pagination_btn = text_button(
            if is_paginated { "● Pagination Mode" } else { "○ Pagination Mode" }.to_string(),
            |comp: &mut SessionTableComponent| {
                comp.mode = TableMode::Pagination;
                comp.current_page = 0;
            }
        );

        let mode_scroll_btn = text_button(
            if is_scroll { "● Infinite Scroll" } else { "○ Infinite Scroll" }.to_string(),
            |comp: &mut SessionTableComponent| {
                comp.mode = TableMode::InfiniteScroll;
                comp.scroll_limit = comp.page_size;
            }
        );

        let left_controls = flex_row((
            mode_pagination_btn,
            FlexSpacer::Fixed(16.0_f32.px()),
            mode_scroll_btn,
        ))
        .cross_axis_alignment(CrossAxisAlignment::Center);

        // 底部右侧：当前模式的专属分页动作组件
        let right_controls: Box<AnyWidgetView<Self>> = match data_mode {
            TableMode::Pagination => {
                let total_pages = if rows_count == 0 {
                    1
                } else {
                    (rows_count + page_size - 1) / page_size
                };

                let prev_btn = text_button(
                    "◀ Prev".to_string(),
                    move |comp: &mut SessionTableComponent| {
                        if comp.current_page > 0 {
                            comp.current_page -= 1;
                        }
                    }
                );

                let page_indicator = label(format!("Page {} of {}", current_page + 1, total_pages))
                    .text_size(theme::FONT_SIZE_BODY)
                    .color(theme::TEXT_CYAN);

                let next_btn = text_button(
                    "Next ▶".to_string(),
                    move |comp: &mut SessionTableComponent| {
                        if (comp.current_page + 1) * comp.page_size < comp.data.rows.len() {
                            comp.current_page += 1;
                        }
                    }
                );

                flex_row((
                    prev_btn,
                    FlexSpacer::Fixed(16.0_f32.px()),
                    page_indicator,
                    FlexSpacer::Fixed(16.0_f32.px()),
                    next_btn,
                ))
                .cross_axis_alignment(CrossAxisAlignment::Center)
                .boxed()
            }
            TableMode::InfiniteScroll => {
                if scroll_limit < rows_count {
                    let load_more_btn = text_button(
                        "▼ Load More".to_string(),
                        move |comp: &mut SessionTableComponent| {
                            comp.scroll_limit += comp.page_size;
                        }
                    );
                    flex_row((load_more_btn,))
                        .cross_axis_alignment(CrossAxisAlignment::Center)
                        .boxed()
                } else {
                    let loaded_label = label(format!("All {} sessions loaded", rows_count))
                        .text_size(theme::FONT_SIZE_SMALL)
                        .color(theme::TEXT_MUTED);
                    flex_row((loaded_label,))
                        .cross_axis_alignment(CrossAxisAlignment::Center)
                        .boxed()
                }
            }
        };

        let footer_controls = flex_row((
            left_controls,
            FlexSpacer::Flex(1.0),
            right_controls,
        ))
        .cross_axis_alignment(CrossAxisAlignment::Center)
        .padding(10.0);

        // 4. 带高品质标题和副标题的面板包装
        crate::views::panel::panel_container(
            "Session Analysis",
            "Sorted by recent activity",
            flex_col((
                table_header,
                FlexSpacer::Fixed(8.0_f32.px()),
                flex_col(table_rows).gap(6.0_f32.px()),
                FlexSpacer::Fixed(12.0_f32.px()),
                footer_controls,
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
fn render_table_row<State: 'static>(
    index: usize,
    row: SessionRow,
    bg_color: xilem::Color,
) -> impl WidgetView<State> {
    sized_box(
        flex_row((
            // 有序排列序号
            sized_box(
                label(format!("#{}", index + 1))
                    .text_size(theme::FONT_SIZE_BODY)
                    .color(theme::TEXT_MUTED)
            )
            .width(40.0_f32.px()),

            // 会话名称列 (Flex)
            flex_col((
                prose(row.key)
                    .text_size(theme::FONT_SIZE_HEADING)
                    .text_color(theme::TEXT_PRIMARY),
                FlexSpacer::Fixed(4.0_f32.px()),
                prose(row.active_desc)
                    .text_size(theme::FONT_SIZE_SMALL)
                    .text_color(theme::TEXT_MUTED),
            ))
            .flex(1.0),

            // 会话状态列
            sized_box(
                label(row.mode_text)
                    .text_size(theme::FONT_SIZE_BODY)
                    .color(if row.is_active { theme::COLOR_SUCCESS } else { theme::COLOR_WARNING })
            )
            .width(100.0_f32.px()),

            // 消息数量列
            sized_box(
                label(format!("{} msg", row.record_count))
                    .text_size(theme::FONT_SIZE_BODY)
                    .color(theme::TEXT_PRIMARY)
            )
            .width(100.0_f32.px()),

            // 总 Token 数量列
            sized_box(
                prose(theme::format_with_commas(row.total_tokens))
                    .text_size(theme::FONT_SIZE_BODY)
                    .text_color(if row.is_calculated { theme::COLOR_CALCULATE } else { theme::TEXT_CYAN })
            )
            .width(140.0_f32.px()),

            // 迷你脉冲 Sparkline 活跃度列
            sized_box(
                render_sparkline(row.sparkline_heights, row.sparkline_color)
            )
            .width(100.0_f32.px()),
        ))
        .cross_axis_alignment(CrossAxisAlignment::Center)
        .padding(10.0)
    )
    .background_color(bg_color)
    .corner_radius(theme::CARD_CORNER_RADIUS)
    .padding(4.0)
}
