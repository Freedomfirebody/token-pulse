//! 热力图组件 — 按日期显示 token 使用 density。

use xilem::{Color, WidgetView};
use xilem::view::{flex_row, flex_col, label, sized_box, FlexSpacer};
use xilem::masonry::properties::types::{AsUnit, CrossAxisAlignment, MainAxisAlignment};
use xilem::style::Style;
use chrono::Datelike;

use crate::theme;
use crate::widgets::hoverable;

/// 热力图单日统计结构 (专用于 Tooltip 显示)
#[derive(Clone, Default)]
pub struct HeatmapDayStats {
    pub date_str: String,
    pub tokens_processed: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_tokens: u64,
    pub reasoning_tokens: u64,
    pub cost: f64,
    pub message_count: u64,
}

/// 热力图组件数据包
#[derive(Clone, Default)]
pub struct HeatmapData {
    pub weeks: Vec<Vec<Color>>,
    pub stats: Vec<Vec<Option<HeatmapDayStats>>>,
}

/// 热力图交互状态
#[derive(Clone, Default, PartialEq, Eq)]
pub struct HeatmapUIState {
    pub hovered_cell: Option<(usize, usize)>,
    pub cell_hovered: bool,
    pub popup_hovered: bool,
}

fn format_m_k_tokens(tokens: u64) -> String {
    if tokens == 0 {
        "0".to_string()
    } else if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}K", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

/// 创建自定义悬浮 Tooltip 视图 (完全贴合图二的 premium 像素级美学设计)
pub fn build_custom_tooltip<State: 'static>(stats_opt: Option<HeatmapDayStats>) -> impl WidgetView<State> {
    let row = |label_str: &str, val_str: &str| {
        flex_row((
            label(label_str.to_string()).text_size(theme::FONT_SIZE_BODY).color(theme::TEXT_SECONDARY),
            FlexSpacer::Flex(1.0),
            label(val_str.to_string()).text_size(theme::FONT_SIZE_BODY).color(theme::TEXT_PRIMARY),
        ))
    };

    let divider = || {
        sized_box(label("".to_string()))
            .height(1.0_f32.px())
            .background_color(theme::BORDER_SUBTLE)
    };

    flex_col(stats_opt.map(|stats| {
        let content_box = sized_box(
            flex_col((
                // 块 1：头部与总 Token 汇总
                flex_col((
                    // 1. 日期头部 (Centered Date)
                    sized_box(
                        flex_row((
                            FlexSpacer::Flex(1.0),
                            label(stats.date_str).text_size(theme::FONT_SIZE_BODY).color(theme::TEXT_PRIMARY),
                            FlexSpacer::Flex(1.0),
                        ))
                    )
                    .expand_width()
                    .padding(xilem::style::Padding::bottom(6.0)),
                    
                    divider(),
                    FlexSpacer::Fixed(8.0_f32.px()),
                    
                    // 2. 总 Token 处理量 (Tokens Processed)
                    flex_row((
                        label("Tokens Processed".to_string()).text_size(theme::FONT_SIZE_BODY).color(theme::TEXT_SECONDARY),
                        FlexSpacer::Flex(1.0),
                        label(format_m_k_tokens(stats.tokens_processed))
                            .text_size(16.0_f32)
                            .color(theme::TEXT_CYAN),
                    )),
                    FlexSpacer::Fixed(8.0_f32.px()),
                    
                    divider(),
                    FlexSpacer::Fixed(8.0_f32.px()),
                ))
                .cross_axis_alignment(CrossAxisAlignment::Fill),

                // 块 2：详细分类 Token 量
                flex_col((
                    row("Input", &format_m_k_tokens(stats.input_tokens)),
                    FlexSpacer::Fixed(4.0_f32.px()),
                    row("Output", &format_m_k_tokens(stats.output_tokens)),
                    FlexSpacer::Fixed(4.0_f32.px()),
                    row("Cache Read", &format_m_k_tokens(stats.cache_tokens)),
                    FlexSpacer::Fixed(4.0_f32.px()),
                    row("Cache Write", "0"),
                    FlexSpacer::Fixed(4.0_f32.px()),
                    row("Reasoning", &format_m_k_tokens(stats.reasoning_tokens)),
                    FlexSpacer::Fixed(8.0_f32.px()),
                ))
                .cross_axis_alignment(CrossAxisAlignment::Fill),

                // 块 3：成本与消息统计
                flex_col((
                    divider(),
                    FlexSpacer::Fixed(8.0_f32.px()),
                    
                    // 4. 成本与消息统计 (Cost / Messages)
                    flex_row((
                        label("Cost".to_string()).text_size(theme::FONT_SIZE_BODY).color(theme::TEXT_SECONDARY),
                        FlexSpacer::Flex(1.0),
                        label(format!("${:.2}", stats.cost)).text_size(theme::FONT_SIZE_BODY).color(theme::TEXT_PRIMARY),
                    )),
                    FlexSpacer::Fixed(4.0_f32.px()),
                    flex_row((
                        label("Messages".to_string()).text_size(theme::FONT_SIZE_BODY).color(theme::TEXT_SECONDARY),
                        FlexSpacer::Flex(1.0),
                        label(theme::format_with_commas(stats.message_count)).text_size(theme::FONT_SIZE_BODY).color(theme::TEXT_PRIMARY),
                    )),
                ))
                .cross_axis_alignment(CrossAxisAlignment::Fill),
            ))
            .cross_axis_alignment(CrossAxisAlignment::Fill)
        )
        .width(220.0_f32.px())
        .height(190.0_f32.px())
        .background_color(theme::BG_CARD)
        .corner_radius(theme::CARD_CORNER_RADIUS)
        .padding(12.0);

        sized_box(content_box)
            .background_color(theme::BORDER_SUBTLE)
            .corner_radius(theme::CARD_CORNER_RADIUS + 1.0)
            .padding(1.0)
    }))
}

/// 渲染 7x13 带月份标题与图例的 3 个月日历热力图
pub fn heatmap_view<State: 'static>(
    ui_state: HeatmapUIState,
    data: HeatmapData,
    on_hover: impl Fn(&mut State, (usize, usize), bool) + Clone + Send + Sync + 'static,
    on_grid_hover: impl Fn(&mut State, bool) + Clone + Send + Sync + 'static,
) -> impl WidgetView<State> {
    // 1. 定义大格子的精细布局参数 (适合短周期的 24px 大格子，极其 premium)
    let cell_box_size = 24.0_f32;
    let cell_gap = 4.0_f32;
    let col_step = cell_box_size + cell_gap;

    // 2. 动态生成月份标题栏 (适配 13 列，3 个月周期)
    let now_dt = chrono::Local::now();
    let today = now_dt.date_naive();
    let weekday = today.weekday();
    let days_from_monday = weekday.num_days_from_monday() as i64;
    let current_week_monday = today - chrono::Duration::days(days_from_monday);
    let grid_start_monday = current_week_monday - chrono::Duration::weeks(12); // 12周前 (共13周，3个月)

    let mut last_month = 0;
    let mut last_label_col = None;
    let mut header_cells = Vec::new();
    
    // 星期标签占位符 (50px 宽度)
    header_cells.push(
        sized_box(label("".to_string()).text_size(theme::FONT_SIZE_SMALL).color(theme::TEXT_MUTED))
            .width(50.0_f32.px())
    );

    for c in 0..13 {
        let cell_date = grid_start_monday + chrono::Duration::weeks(c as i64);
        let month = cell_date.month();
        let show_label = if last_month != month {
            if let Some(last_c) = last_label_col {
                c - last_c >= 3 // 间隔至少 3 周防重叠
            } else {
                true
            }
        } else {
            false
        };

        if show_label {
            let month_name = match month {
                1 => "Jan", 2 => "Feb", 3 => "Mar", 4 => "Apr",
                5 => "May", 6 => "Jun", 7 => "Jul", 8 => "Aug",
                9 => "Sep", 10 => "Oct", 11 => "Nov", 12 => "Dec",
                _ => "",
            };
            header_cells.push(
                sized_box(label(month_name.to_string()).text_size(theme::FONT_SIZE_SMALL).color(theme::TEXT_MUTED))
                    .width(col_step.px())
            );
            last_month = month;
            last_label_col = Some(c);
        } else {
            header_cells.push(
                sized_box(label("".to_string()).text_size(theme::FONT_SIZE_SMALL).color(theme::TEXT_MUTED))
                    .width(col_step.px())
            );
        }
    }

    let months_header = sized_box(
        flex_row(header_cells)
            .main_axis_alignment(MainAxisAlignment::Start)
            .gap(0.0_f32.px())
    )
    .height(22.0_f32.px());

    // 3. 热力图主网格 (13周 × 7天)
    let grid_columns: Vec<_> = data.weeks.iter().enumerate().map(|(c_idx, week_colors)| {
        let on_hover = on_hover.clone();
        let ui_state = ui_state.clone();
        let cells: Vec<_> = week_colors.iter().enumerate().map(move |(r_idx, &cell_color)| {
            let is_hovered = ui_state.hovered_cell == Some((c_idx, r_idx));
            let normal_padding = 3.0_f64; // 让普通格在 24px 大小内具有 3px 内边距 (实际大小 18px)
            let size = if is_hovered { cell_box_size } else { cell_box_size - (2.0 * normal_padding) as f32 };
            let padding = if is_hovered { 0.0_f64 } else { normal_padding };

            let cell_inner = sized_box(label("".to_string()))
                .width(size.px())
                .height(size.px())
                .background_color(cell_color);

            let cell_view = sized_box(cell_inner)
                .width(cell_box_size.px())
                .height(cell_box_size.px())
                .padding(padding);
            
            let on_hover = on_hover.clone();
            hoverable(cell_view, move |state: &mut State, hovered| {
                on_hover(state, (c_idx, r_idx), hovered);
            })
        }).collect();

        flex_col(cells)
            .main_axis_alignment(MainAxisAlignment::Start)
            .gap(cell_gap.px())
    }).collect();

    let heatmap_grid = flex_row(grid_columns)
        .main_axis_alignment(MainAxisAlignment::Start)
        .gap(cell_gap.px());

    // 包装 hoverable
    let grid_hover = on_grid_hover.clone();
    let hoverable_grid = hoverable(heatmap_grid, move |state: &mut State, hovered| {
        grid_hover(state, hovered);
    });

    // 4. GitHub 风格的 Legend 行 ("Less ■ ■ ■ ■ ■ More" 对齐右侧)
    let legend = flex_row((
        label("Less".to_string()).text_size(theme::FONT_SIZE_SMALL).color(theme::TEXT_MUTED),
        FlexSpacer::Fixed(6.0_f32.px()),
        sized_box(label("".to_string())).width(12.0_f32.px()).height(12.0_f32.px()).background_color(theme::HEATMAP_EMPTY),
        FlexSpacer::Fixed(3.0_f32.px()),
        sized_box(label("".to_string())).width(12.0_f32.px()).height(12.0_f32.px()).background_color(theme::HEATMAP_LOW),
        FlexSpacer::Fixed(3.0_f32.px()),
        sized_box(label("".to_string())).width(12.0_f32.px()).height(12.0_f32.px()).background_color(theme::HEATMAP_MED),
        FlexSpacer::Fixed(3.0_f32.px()),
        sized_box(label("".to_string())).width(12.0_f32.px()).height(12.0_f32.px()).background_color(theme::HEATMAP_HIGH),
        FlexSpacer::Fixed(3.0_f32.px()),
        sized_box(label("".to_string())).width(12.0_f32.px()).height(12.0_f32.px()).background_color(theme::HEATMAP_MAX),
        FlexSpacer::Fixed(6.0_f32.px()),
        label("More".to_string()).text_size(theme::FONT_SIZE_SMALL).color(theme::TEXT_MUTED),
    ))
    .main_axis_alignment(MainAxisAlignment::End);

    // 5. 星期标签 (Mon 对应 Row 0, Wed 对应 Row 2, Fri 对应 Row 4) — 精准贴合
    let weekday_spacer = cell_box_size + 2.0 * cell_gap;
    let weekday_labels = sized_box(
        flex_col((
            sized_box(label("Mon".to_string()).text_size(theme::FONT_SIZE_SMALL).color(theme::TEXT_MUTED))
                .height(cell_box_size.px()),
            FlexSpacer::Fixed(weekday_spacer.px()),
            sized_box(label("Wed".to_string()).text_size(theme::FONT_SIZE_SMALL).color(theme::TEXT_MUTED))
                .height(cell_box_size.px()),
            FlexSpacer::Fixed(weekday_spacer.px()),
            sized_box(label("Fri".to_string()).text_size(theme::FONT_SIZE_SMALL).color(theme::TEXT_MUTED))
                .height(cell_box_size.px()),
        ))
        .main_axis_alignment(MainAxisAlignment::Start)
        .gap(0.0_f32.px())
    ).width(50.0_f32.px());

    // 6. 构建主热力图布局
    flex_row((
        weekday_labels,
        // 热力图与 Legend 区域
        flex_col((
            months_header,
            FlexSpacer::Fixed(8.0_f32.px()),
            hoverable_grid,
            FlexSpacer::Fixed(8.0_f32.px()),
            legend,
        ))
        .main_axis_alignment(MainAxisAlignment::Start)
        .gap(0.0_f32.px()),
    ))
    .main_axis_alignment(MainAxisAlignment::Start)
    .gap(0.0_f32.px())
}
