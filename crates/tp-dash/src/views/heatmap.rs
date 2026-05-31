//! 热力图组件 — 按日期显示 token 使用 density。

use chrono::Datelike;
use xilem::masonry::properties::types::{AsUnit, CrossAxisAlignment, MainAxisAlignment};
use xilem::style::Style;
use xilem::view::{flex_col, flex_row, label, sized_box, FlexSpacer};
use xilem::{Color, WidgetView};

use crate::theme;

use xilem::masonry::accesskit::{Node, Role};
use xilem::masonry::core::{
    AccessCtx, BoxConstraints, ChildrenIds, EventCtx, LayoutCtx, PaintCtx,
    PointerEvent, PropertiesMut, PropertiesRef, RegisterCtx,
    Widget,
};
use xilem::masonry::kurbo::{Rect, Size};
use xilem::masonry::vello::Scene;

use xilem::core::{MessageContext, MessageResult, Mut, View, ViewMarker};
use xilem::core::one_of::Either;
use xilem::{Pod, ViewCtx};


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

/// Heatmap 封装组件
#[derive(Clone, Default)]
pub struct HeatmapComponent {
    pub data: HeatmapData,
    pub ui: HeatmapUIState,
    pub last_record_count: u64,
}

impl HeatmapComponent {
    pub fn new() -> Self {
        Self {
            data: HeatmapData::default(),
            ui: HeatmapUIState::default(),
            last_record_count: u64::MAX,
        }
    }

    /// 根据全局数据树投影出子组件日历热力图网格的颜色色阶与详情列表
    pub fn update(&mut self, summary: &tp_protocol::view::DashboardView) {

        let now_dt = chrono::Local::now();
        let today = now_dt.date_naive();
        let weekday = today.weekday();
        let days_from_monday = weekday.num_days_from_monday() as i64;
        let current_week_monday = today - chrono::Duration::days(days_from_monday);
        let grid_start_monday = current_week_monday - chrono::Duration::weeks(12); // 12周前 (共13周，3个月)

        let mut stats_by_date: std::collections::HashMap<chrono::NaiveDate, tp_protocol::view::DailyStats> = std::collections::HashMap::new();
        for (date_str, stats) in &summary.daily_series {
            if let Ok(date) = chrono::NaiveDate::parse_from_str(date_str, "%Y-%m-%d") {
                stats_by_date.insert(date, stats.clone());
            }
        }

        // 计算自适应分位数色阶阈值 (25%, 50%, 75% 分位数)
        let mut non_zero_tokens: Vec<u64> = summary.daily_series.values()
            .map(|s| s.token_info.total())
            .filter(|&t| t > 0)
            .collect();
        non_zero_tokens.sort_unstable();

        let (q25, q50, q75) = if non_zero_tokens.is_empty() {
            (1_000, 10_000, 100_000) // 默认兜底
        } else {
            let len = non_zero_tokens.len();
            let q25 = non_zero_tokens[len * 25 / 100];
            let q50 = non_zero_tokens[len * 50 / 100];
            let q75 = non_zero_tokens[len * 75 / 100];

            let q25 = q25.max(1);
            let q50 = q50.max(q25 + 1);
            let q75 = q75.max(q50 + 1);
            (q25, q50, q75)
        };

        self.data.weeks = (0..13).map(|c| {
            (0..7).map(|r| {
                let cell_date = grid_start_monday + chrono::Duration::weeks(c as i64) + chrono::Duration::days(r as i64);
                let tokens = stats_by_date.get(&cell_date).map(|s| s.token_info.total()).unwrap_or(0);
                theme::heatmap_color_dynamic(tokens, q25, q50, q75)
            }).collect()
        }).collect();

        self.data.stats = (0..13).map(|c| {
            (0..7).map(|r| {
                let cell_date = grid_start_monday + chrono::Duration::weeks(c as i64) + chrono::Duration::days(r as i64);
                let stats_opt = stats_by_date.get(&cell_date);
                Some(match stats_opt {
                    Some(s) => HeatmapDayStats {
                        date_str: cell_date.format("%B %d, %Y").to_string(),
                        tokens_processed: s.token_info.total(),
                        input_tokens: s.token_info.input,
                        output_tokens: s.token_info.output,
                        cache_tokens: s.token_info.cache,
                        reasoning_tokens: s.token_info.reasoning,
                        cost: s.cost_usd,
                        message_count: s.message_count,
                    },
                    None => HeatmapDayStats {
                        date_str: cell_date.format("%B %d, %Y").to_string(),
                        tokens_processed: 0,
                        input_tokens: 0,
                        output_tokens: 0,
                        cache_tokens: 0,
                        reasoning_tokens: 0,
                        cost: 0.0,
                        message_count: 0,
                    },
                })
            }).collect()
        }).collect();

        self.last_record_count = summary.record_count;
    }

    /// 渲染日历热力图及对应的 Tooltip 气泡弹窗
    pub fn view(
        &mut self,
        worker_tx: &Option<tokio::sync::mpsc::UnboundedSender<crate::app_state::WorkerMessage>>,
    ) -> impl WidgetView<Self> {
        let ui = self.ui.clone();
        let data = self.data.clone();
        let worker_tx_clone = worker_tx.clone();
        let worker_tx_clone2 = worker_tx.clone();

        // 1. 渲染热力图网格
        let heatmap_grid = heatmap_view(
            ui.clone(),
            data.clone(),
            move |comp: &mut Self, cell, hovered| {
                comp.ui.cell_hovered = hovered;
                if hovered {
                    comp.ui.popup_hovered = false;
                    let (c, r) = cell;
                    let has_stats = comp.data.stats.get(c)
                        .and_then(|w| w.get(r))
                        .and_then(|s| s.as_ref())
                        .is_some();
                    if has_stats {
                        comp.ui.hovered_cell = Some(cell);
                    } else {
                        comp.ui.hovered_cell = None;
                        comp.ui.popup_hovered = false;
                    }
                } else {
                    if !comp.ui.popup_hovered {
                        if let Some(ref tx) = worker_tx_clone {
                            let _ = tx.send(crate::app_state::WorkerMessage::ClosePopupDelay);
                        }
                    }
                }
            },
            move |comp: &mut Self, grid_hovered| {
                if !grid_hovered {
                    comp.ui.cell_hovered = false;
                    if !comp.ui.popup_hovered {
                        if let Some(ref tx) = worker_tx_clone2 {
                            let _ = tx.send(crate::app_state::WorkerMessage::ClosePopupDelay);
                        }
                    }
                }
            }
        );

        // 3. 计算悬浮 Tooltip 气泡参数 (完全使用相对锚定，免去屏幕绝对坐标依赖)
        let mut stats_opt = None;
        let mut anchor_point = crate::widgets::AnchorPoint::TopLeft;

        if let Some((c_idx, r_idx)) = self.ui.hovered_cell {
            if let Some(week_stats) = self.data.stats.get(c_idx) {
                if let Some(Some(stats)) = week_stats.get(r_idx) {
                    let cell_box_size = 24.0;
                    let cell_gap = 4.0;
                    let col_step = cell_box_size + cell_gap;

                    // months_header 高度 = 22.0，与网格间距 8.0
                    let y_offset = 22.0 + 8.0 + (r_idx as f64) * (col_step as f64);

                    // 使用 CustomCenterRelative 相对中间轴偏移
                    // 50px Mon/Wed/Fri 标签宽度，360px 网格宽度。总宽 410px，中点左移 205px，网格左沿在中点 -155px。
                    let relative_x = -155.0 + (c_idx as f64) * (col_step as f64) + (cell_box_size as f64);

                    anchor_point = crate::widgets::AnchorPoint::CustomCenterRelative(
                        relative_x,
                        y_offset + (cell_box_size as f64 / 2.0),
                    );
                    stats_opt = Some(stats.clone());
                }
            }
        }

        let tooltip = build_custom_tooltip(stats_opt);
        let worker_tx_clone3 = worker_tx.clone();

        let hoverable_tooltip = crate::widgets::hoverable(tooltip, move |comp: &mut Self, hovered| {
            comp.ui.popup_hovered = hovered;
            if !hovered && !comp.ui.cell_hovered {
                if let Some(ref tx) = worker_tx_clone3 {
                    let _ = tx.send(crate::app_state::WorkerMessage::ClosePopupDelay);
                }
            }
        });

        // 4. 使用 popover_stack 实现完美的悬浮对齐层级表现 (包裹 heatmap_grid)
        let grid_with_popover = crate::widgets::popover_stack(
            heatmap_grid,
            hoverable_tooltip,
            crate::widgets::PopoverConfig {
                anchor_point,
                popover_align: crate::widgets::PopoverAlign::TopLeft,
                offset_x: 8.0,
                offset_y: -50.0, // 往上偏移一段，实现居中效果
            }
        );

        // 2. 表演面板包装
        crate::views::panel::panel_container(
            "TOKEN RETENTION HEATMAP",
            "Current tokens distributed by last modified date",
            grid_with_popover,
            theme::TEXT_CYAN,
            theme::TEXT_MUTED,
        )
    }
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
fn tooltip_row<State: 'static>(label_str: String, val_str: String) -> impl WidgetView<State> {
    flex_row((
        label(label_str).text_size(theme::FONT_SIZE_BODY).color(theme::TEXT_SECONDARY),
        FlexSpacer::Flex(1.0),
        label(val_str).text_size(theme::FONT_SIZE_BODY).color(theme::TEXT_PRIMARY),
    ))
}

fn tooltip_divider<State: 'static>() -> impl WidgetView<State> {
    sized_box(label("".to_string()))
        .height(1.0_f32.px())
        .background_color(theme::BORDER_SUBTLE)
}

fn tooltip_block_1<State: 'static>(stats: HeatmapDayStats) -> impl WidgetView<State> {
    flex_col((
        // 1. 日期头部 (Centered Date)
        sized_box(
            flex_row((
                FlexSpacer::Flex(1.0),
                label(stats.date_str.clone()).text_size(theme::FONT_SIZE_BODY).color(theme::TEXT_PRIMARY),
                FlexSpacer::Flex(1.0),
            ))
        )
        .expand_width()
        .padding(xilem::style::Padding::bottom(6.0)),

        tooltip_divider(),
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

        tooltip_divider(),
        FlexSpacer::Fixed(8.0_f32.px()),
    ))
    .cross_axis_alignment(CrossAxisAlignment::Fill)
}

fn tooltip_block_2<State: 'static>(stats: HeatmapDayStats) -> impl WidgetView<State> {
    flex_col((
        tooltip_row("Input".to_string(), format_m_k_tokens(stats.input_tokens)),
        FlexSpacer::Fixed(4.0_f32.px()),
        tooltip_row("Output".to_string(), format_m_k_tokens(stats.output_tokens)),
        FlexSpacer::Fixed(4.0_f32.px()),
        tooltip_row("Cache Read".to_string(), format_m_k_tokens(stats.cache_tokens)),
        FlexSpacer::Fixed(4.0_f32.px()),
        tooltip_row("Cache Write".to_string(), "0".to_string()),
        FlexSpacer::Fixed(4.0_f32.px()),
        tooltip_row("Reasoning".to_string(), format_m_k_tokens(stats.reasoning_tokens)),
        FlexSpacer::Fixed(8.0_f32.px()),
    ))
    .cross_axis_alignment(CrossAxisAlignment::Fill)
}

fn tooltip_block_3<State: 'static>(stats: HeatmapDayStats) -> impl WidgetView<State> {
    flex_col((
        tooltip_divider(),
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
    .cross_axis_alignment(CrossAxisAlignment::Fill)
}

/// 创建自定义悬浮 Tooltip 视图 (完全贴合图二的 premium 像素级美学设计)
pub fn build_custom_tooltip<State: 'static>(stats_opt: Option<HeatmapDayStats>) -> impl WidgetView<State> {
    if let Some(stats) = stats_opt {
        let p1 = tooltip_block_1(stats.clone());
        let p2 = tooltip_block_2(stats.clone());
        let p3 = tooltip_block_3(stats);

        let content_box = sized_box(
            flex_col((
                p1,
                p2,
                p3,
            ))
            .cross_axis_alignment(CrossAxisAlignment::Fill)
        )
        .width(220.0_f32.px())
        .height(190.0_f32.px())
        .background_color(theme::BG_CARD)
        .corner_radius(theme::CARD_CORNER_RADIUS)
        .padding(12.0);

        Either::A(
            sized_box(content_box)
                .background_color(theme::BORDER_SUBTLE)
                .corner_radius(theme::CARD_CORNER_RADIUS + 1.0)
                .padding(1.0)
        )
    } else {
        Either::B(
            sized_box(label("".to_string()))
                .width(0.0_f32.px())
                .height(0.0_f32.px())
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum HeatmapGridAction {
    CellHover(Option<(usize, usize)>),
    GridHover(bool),
}

pub struct HeatmapGridWidget {
    weeks: Vec<Vec<Color>>,
    hovered_cell: Option<(usize, usize)>,
    grid_hovered: bool,
}

impl HeatmapGridWidget {
    pub fn new(weeks: Vec<Vec<Color>>) -> Self {
        Self {
            weeks,
            hovered_cell: None,
            grid_hovered: false,
        }
    }
}

impl Widget for HeatmapGridWidget {
    type Action = HeatmapGridAction;

    fn accepts_pointer_interaction(&self) -> bool {
        true
    }

    fn register_children(&mut self, _ctx: &mut RegisterCtx<'_>) {}

    fn layout(
        &mut self,
        _ctx: &mut LayoutCtx<'_>,
        _props: &mut PropertiesMut<'_>,
        _bc: &BoxConstraints,
    ) -> Size {
        Size::new(360.0, 192.0)
    }

    fn paint(&mut self, _ctx: &mut PaintCtx<'_>, _props: &PropertiesRef<'_>, scene: &mut Scene) {
        let cell_box_size = 24.0;
        let cell_gap = 4.0;
        let col_step = cell_box_size + cell_gap;
        let normal_padding = 3.0;

        for c in 0..self.weeks.len() {
            let col_colors = &self.weeks[c];
            for r in 0..col_colors.len() {
                let cell_color = col_colors[r];
                let is_hovered = self.hovered_cell == Some((c, r));
                
                let (x_offset, y_offset, size) = if is_hovered {
                    (c as f64 * col_step, r as f64 * col_step, 24.0)
                } else {
                    (c as f64 * col_step + normal_padding, r as f64 * col_step + normal_padding, 18.0)
                };

                let rect = Rect::new(x_offset, y_offset, x_offset + size, y_offset + size);
                
                scene.fill(
                    xilem::masonry::vello::peniko::Fill::NonZero,
                    xilem::masonry::kurbo::Affine::IDENTITY,
                    cell_color,
                    None,
                    &rect,
                );
            }
        }
    }

    fn accessibility_role(&self) -> Role {
        Role::GenericContainer
    }

    fn accessibility(&mut self, _ctx: &mut AccessCtx<'_>, _props: &PropertiesRef<'_>, _node: &mut Node) {}

    fn children_ids(&self) -> ChildrenIds {
        ChildrenIds::new()
    }

    fn on_pointer_event(
        &mut self,
        ctx: &mut EventCtx<'_>,
        _props: &mut PropertiesMut<'_>,
        event: &PointerEvent,
    ) {
        let is_hovered = ctx.is_hovered();

        if is_hovered && !self.grid_hovered {
            self.grid_hovered = true;
            ctx.submit_action::<HeatmapGridAction>(HeatmapGridAction::GridHover(true));
        }

        let mut new_hovered_cell = None;
        if is_hovered {
            let mut pos_opt = None;
            match event {
                PointerEvent::Move(update) => {
                    pos_opt = Some(update.current.logical_point());
                }
                _ => {}
            }

            if let Some(pos) = pos_opt {
                let col_step = 28.0;
                let cell_box_size = 24.0;
                let c = (pos.x / col_step).floor() as i32;
                let r = (pos.y / col_step).floor() as i32;

                if c >= 0 && c < 13 && r >= 0 && r < 7 {
                    let x_in_cell = pos.x - (c as f64 * col_step);
                    let y_in_cell = pos.y - (r as f64 * col_step);
                    if x_in_cell >= 0.0 && x_in_cell <= cell_box_size && y_in_cell >= 0.0 && y_in_cell <= cell_box_size {
                        new_hovered_cell = Some((c as usize, r as usize));
                    }
                }
            }
        }

        if new_hovered_cell != self.hovered_cell {
            self.hovered_cell = new_hovered_cell;
            ctx.submit_action::<HeatmapGridAction>(HeatmapGridAction::CellHover(new_hovered_cell));
            ctx.request_paint_only();
        }

        match event {
            PointerEvent::Leave(_) | PointerEvent::Cancel(_) => {
                if self.grid_hovered {
                    self.grid_hovered = false;
                    ctx.submit_action::<HeatmapGridAction>(HeatmapGridAction::GridHover(false));
                }
                if self.hovered_cell.is_some() {
                    self.hovered_cell = None;
                    ctx.submit_action::<HeatmapGridAction>(HeatmapGridAction::CellHover(None));
                    ctx.request_paint_only();
                }
            }
            _ => {
                if !is_hovered && self.grid_hovered {
                    self.grid_hovered = false;
                    ctx.submit_action::<HeatmapGridAction>(HeatmapGridAction::GridHover(false));
                    if self.hovered_cell.is_some() {
                        self.hovered_cell = None;
                        ctx.submit_action::<HeatmapGridAction>(HeatmapGridAction::CellHover(None));
                        ctx.request_paint_only();
                    }
                }
            }
        }
    }
}

pub struct HeatmapGridStaticView<F1, F2> {
    weeks: Vec<Vec<Color>>,
    on_cell_hover: F1,
    on_grid_hover: F2,
}

pub fn heatmap_grid_static_view<State, Action, F1, F2>(
    weeks: Vec<Vec<Color>>,
    on_cell_hover: F1,
    on_grid_hover: F2,
) -> HeatmapGridStaticView<F1, F2>
where
    State: 'static,
    Action: 'static,
    F1: Fn(&mut State, (usize, usize), bool) -> Action + Send + Sync + 'static,
    F2: Fn(&mut State, bool) -> Action + Send + Sync + 'static,
{
    HeatmapGridStaticView {
        weeks,
        on_cell_hover,
        on_grid_hover,
    }
}

impl<F1, F2> ViewMarker for HeatmapGridStaticView<F1, F2> {}

impl<F1, F2, State, Action> View<State, Action, ViewCtx> for HeatmapGridStaticView<F1, F2>
where
    State: 'static,
    Action: 'static,
    F1: Fn(&mut State, (usize, usize), bool) -> Action + Send + Sync + 'static,
    F2: Fn(&mut State, bool) -> Action + Send + Sync + 'static,
{
    type Element = Pod<HeatmapGridWidget>;
    type ViewState = ();

    fn build(&self, ctx: &mut ViewCtx, _app_state: &mut State) -> (Self::Element, Self::ViewState) {
        (
            ctx.with_action_widget(|ctx| {
                ctx.create_pod(HeatmapGridWidget::new(self.weeks.clone()))
            }),
            (),
        )
    }

    fn rebuild(
        &self,
        prev: &Self,
        _state: &mut Self::ViewState,
        _ctx: &mut ViewCtx,
        element: Mut<'_, Self::Element>,
        _app_state: &mut State,
    ) {
        if self.weeks != prev.weeks {
            element.widget.weeks = self.weeks.clone();
        }
    }

    fn teardown(
        &self,
        _view_state: &mut Self::ViewState,
        ctx: &mut ViewCtx,
        element: Mut<'_, Self::Element>,
    ) {
        ctx.teardown_leaf(element);
    }

    fn message(
        &self,
        _view_state: &mut Self::ViewState,
        message: &mut MessageContext,
        _element: Mut<'_, Self::Element>,
        app_state: &mut State,
    ) -> MessageResult<Action> {
        match message.take_message::<HeatmapGridAction>() {
            Some(action) => {
                match *action {
                    HeatmapGridAction::CellHover(Some(cell)) => {
                        MessageResult::Action((self.on_cell_hover)(app_state, cell, true))
                    }
                    HeatmapGridAction::CellHover(None) => {
                        MessageResult::Action((self.on_cell_hover)(app_state, (0, 0), false))
                    }
                    HeatmapGridAction::GridHover(hovered) => {
                        MessageResult::Action((self.on_grid_hover)(app_state, hovered))
                    }
                }
            }
            None => MessageResult::Stale,
        }
    }
}

/// 渲染 7x13 带月份标题与图例的 3 个月日历热力图

pub fn heatmap_view<State: 'static>(
    _ui_state: HeatmapUIState,
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
    let hoverable_grid = heatmap_grid_static_view(
        data.weeks.clone(),
        on_hover,
        on_grid_hover,
    );

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
    sized_box(
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
        .main_axis_alignment(MainAxisAlignment::Center)
        .gap(0.0_f32.px())
    )
    .expand_width()
}
