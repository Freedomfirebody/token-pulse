//! 周级别柱状图组件 — 按天显示最近 7 天的 token 使用量。
//! 采用纯函数式无状态视图渲染，彻底避免嵌套泛型导致的 Rust 编译开销。

use chrono::Datelike;
use xilem::masonry::properties::types::{AsUnit, CrossAxisAlignment, MainAxisAlignment};
use xilem::view::{flex_col, flex_row, label, sized_box, FlexSpacer};
use xilem::style::Style;
use xilem::{Color, WidgetView};

use crate::theme;

/// 将 Token 数量格式化为极其紧凑、美观的短形式 (如 1.2M, 450K, 0)
pub fn format_short_tokens(value: u64) -> String {
    if value == 0 {
        "0".to_string()
    } else if value >= 1_000_000_000 {
        format!("{:.1}B", value as f64 / 1_000_000_000.0)
    } else if value >= 1_000_000 {
        format!("{:.1}M", value as f64 / 1_000_000.0)
    } else if value >= 1_000 {
        format!("{:.1}K", value as f64 / 1_000.0)
    } else {
        value.to_string()
    }
}

/// 渲染最近 7 天 (含今日) 的 Token 使用量柱状图
pub fn weekly_chart_view<State: 'static>(
    summary: &tp_protocol::view::DashboardView,
) -> impl WidgetView<State> {
    let today = chrono::Local::now().date_naive();
    
    // 1. 获取最近 7 天 (含今日) 的日期与 Token 总使用量
    let mut daily_tokens = Vec::new();
    for i in (0..7).rev() {
        let day = today - chrono::Duration::days(i);
        let date_str = day.format("%Y-%m-%d").to_string();
        let tokens = summary.daily_series.get(&date_str)
            .map(|s| s.token_info.total())
            .unwrap_or(0);
        daily_tokens.push((day, tokens));
    }

    // 2. 找到这 7 天内的最大日使用量，作为柱体高度比例的基础底座
    let max_tokens = daily_tokens.iter().map(|(_, t)| *t).max().unwrap_or(0).max(1);

    // 3. 构建 7 根排列整齐的精美柱子
    let cols: Vec<_> = daily_tokens.into_iter().map(|(day, tokens)| {
        let is_today = day == today;
        let weekday_str = if is_today {
            "今日"
        } else {
            match day.weekday() {
                chrono::Weekday::Mon => "周一",
                chrono::Weekday::Tue => "周二",
                chrono::Weekday::Wed => "周三",
                chrono::Weekday::Thu => "周四",
                chrono::Weekday::Fri => "周五",
                chrono::Weekday::Sat => "周六",
                chrono::Weekday::Sun => "周日",
            }
        };

        // 基于使用强度比例，计算极具视觉表现力的青蓝渐变插值颜色
        let bar_color = if tokens == 0 {
            theme::HEATMAP_EMPTY
        } else {
            let ratio = tokens as f64 / max_tokens as f64;
            // 从暗靛蓝 (15, 52, 75) 渐变到亮青色 theme::TEXT_CYAN (51, 224, 255)
            let r = (15.0 + ratio * (51.0 - 15.0)) as u8;
            let g = (52.0 + ratio * (224.0 - 52.0)) as u8;
            let b = (75.0 + ratio * (255.0 - 75.0)) as u8;
            Color::from_rgb8(r, g, b)
        };

        // 柱体最大高度 120px，最小高度 4px 以保正即使有微弱数据也能点亮
        let bar_height = if tokens == 0 {
            4.0_f32
        } else {
            (tokens as f64 / max_tokens as f64 * 120.0).max(4.0) as f32
        };

        // 使用 FlexSpacer::Flex(1.0) 保证柱体完美在底部对齐
        let bar_container = sized_box(
            flex_col((
                FlexSpacer::Flex(1.0),
                sized_box(label(""))
                    .width(28.0_f32.px())
                    .height(bar_height.px())
                    .background_color(bar_color)
                    .corner_radius(4.0),
            ))
            .cross_axis_alignment(CrossAxisAlignment::Center)
        )
        .width(40.0_f32.px())
        .height(120.0_f32.px());

        // 组合单列的数值、柱体、星期与日期
        sized_box(
            flex_col((
                label(format_short_tokens(tokens))
                    .text_size(theme::FONT_SIZE_SMALL)
                    .color(if is_today { theme::TEXT_CYAN } else { theme::TEXT_SECONDARY }),
                FlexSpacer::Fixed(6.0_f32.px()),
                bar_container,
                FlexSpacer::Fixed(8.0_f32.px()),
                label(weekday_str.to_string())
                    .text_size(theme::FONT_SIZE_SMALL)
                    .color(if is_today { theme::TEXT_CYAN } else { theme::TEXT_PRIMARY }),
                FlexSpacer::Fixed(2.0_f32.px()),
                label(day.format("%m/%d").to_string())
                    .text_size(10.0)
                    .color(theme::TEXT_MUTED),
            ))
            .cross_axis_alignment(CrossAxisAlignment::Center)
        )
        .width(60.0_f32.px())
    }).collect();

    // 4. 水平居中并排展示，使用 gap 进行精致的柱体间距排版
    let chart_row = sized_box(
        flex_row(cols)
            .main_axis_alignment(MainAxisAlignment::Center)
            .cross_axis_alignment(CrossAxisAlignment::End)
            .gap(20.0_f32.px())
    )
    .height(180.0_f32.px())
    .expand_width();

    // 5. 包装进标准面板容器中返回
    crate::views::panel::panel_container(
        "WEEKLY TOKEN TRENDS",
        "Weekly token usage trend analysis over the last 7 days including today",
        chart_row,
        theme::TEXT_CYAN,
        theme::TEXT_MUTED,
    )
}
