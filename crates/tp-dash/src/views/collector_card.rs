//! 采集器节点状态卡片组件 (Collector Status Card)。
//!
//! 针对每个接入的采集器 (Antigravity, Claude Code, Codex)，展示其配置与实时采集统计。

use xilem::masonry::properties::types::{AsUnit, CrossAxisAlignment};
use xilem::view::{flex_col, flex_row, label, sized_box, FlexSpacer, prose};
use xilem::style::Style;
use xilem::{Color, WidgetView};

use crate::theme;

/// 采集器节点数据模型
#[derive(Clone, Debug)]
pub struct CollectorCardData {
    /// 采集器名称 (如 "Antigravity")
    pub name: String,
    /// 采集器类型描述
    pub desc: String,
    /// 运行状态 (如 "ACTIVE")
    pub status: String,
    /// 状态展示颜色
    pub status_color: Color,
    /// 遥测数据源路径
    pub path: String,
    /// 累计采集的总 Token 数
    pub total_tokens: u64,
    /// 累计消息/ Trajectory 记录数
    pub records: u64,
    /// 产生费用的估计
    pub cost: f64,
}

/// 渲染高保真采集器节点卡片
pub fn collector_card<State: 'static>(
    data: CollectorCardData,
) -> impl WidgetView<State> {
    let name_str = data.name.clone();
    let desc_str = data.desc.clone();
    let status_str = format!("● {}", data.status);
    let path_str = data.path.clone();
    let stats_str = format!("{} tokens • {} msgs", theme::format_with_commas(data.total_tokens), data.records);
    let cost_str = theme::format_cost(data.cost);

    sized_box(
        flex_col((
            // 第一行: 运行状态标志 与 小标题
            flex_row((
                label(status_str)
                    .text_size(theme::FONT_SIZE_SMALL)
                    .color(data.status_color),
                FlexSpacer::Flex(1.0),
                label(name_str.to_uppercase())
                    .text_size(theme::FONT_SIZE_SMALL)
                    .color(theme::TEXT_MUTED),
            ))
            .cross_axis_alignment(CrossAxisAlignment::Center),
            
            FlexSpacer::Fixed(6.0_f32.px()),
            
            // 核心展示标题
            label(name_str)
                .text_size(theme::FONT_SIZE_HEADING)
                .color(theme::TEXT_PRIMARY),
            
            FlexSpacer::Fixed(2.0_f32.px()),
            
            // 采集节点类型描述
            label(desc_str)
                .text_size(theme::FONT_SIZE_SMALL)
                .color(theme::TEXT_SECONDARY),
            
            FlexSpacer::Fixed(14.0_f32.px()),
            
            // 遥测路径元数据
            flex_row((
                sized_box(label("PATH").text_size(theme::FONT_SIZE_SMALL).color(theme::TEXT_MUTED))
                    .width(45.0_f32.px()),
                FlexSpacer::Fixed(6.0_f32.px()),
                prose(path_str).text_size(theme::FONT_SIZE_SMALL).text_color(theme::TEXT_PRIMARY),
            ))
            .cross_axis_alignment(CrossAxisAlignment::Center),
            
            FlexSpacer::Fixed(6.0_f32.px()),
            
            // 实时采集统计
            flex_row((
                sized_box(label("STATS").text_size(theme::FONT_SIZE_SMALL).color(theme::TEXT_MUTED))
                    .width(45.0_f32.px()),
                FlexSpacer::Fixed(6.0_f32.px()),
                prose(stats_str).text_size(theme::FONT_SIZE_SMALL).text_color(theme::TEXT_PRIMARY),
            ))
            .cross_axis_alignment(CrossAxisAlignment::Center),
            
            FlexSpacer::Fixed(6.0_f32.px()),
            
            // 产生费用额
            flex_row((
                sized_box(label("COST").text_size(theme::FONT_SIZE_SMALL).color(theme::TEXT_MUTED))
                    .width(45.0_f32.px()),
                FlexSpacer::Fixed(6.0_f32.px()),
                prose(cost_str).text_size(theme::FONT_SIZE_SMALL).text_color(theme::TEXT_CYAN),
            ))
            .cross_axis_alignment(CrossAxisAlignment::Center),
        ))
        .cross_axis_alignment(CrossAxisAlignment::Start)
    )
    .width(260.0_f32.px())
    .height(180.0_f32.px())
    .background_color(theme::BG_CARD)
    .padding(14.0)
    .corner_radius(theme::CARD_CORNER_RADIUS)
}
