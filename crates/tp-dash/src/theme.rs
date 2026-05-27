//! 统一色彩、字号、间距常量。

use xilem::Color;

// ===== 背景色 =====
pub const BG_MAIN: Color = Color::from_rgb8(10, 15, 20);
pub const BG_CARD: Color = Color::from_rgb8(15, 25, 35);
pub const BG_PANEL: Color = Color::from_rgb8(18, 30, 42);
pub const BG_HOVER: Color = Color::from_rgb8(25, 40, 55);
pub const BG_INPUT: Color = Color::from_rgb8(12, 20, 28);

// ===== 文字色 =====
pub const TEXT_PRIMARY: Color = Color::from_rgb8(240, 245, 250);
pub const TEXT_SECONDARY: Color = Color::from_rgb8(160, 175, 190);
pub const TEXT_MUTED: Color = Color::from_rgb8(100, 115, 130);
pub const TEXT_CYAN: Color = Color::from_rgb8(51, 224, 255);
pub const TEXT_ACCENT: Color = Color::from_rgb8(100, 200, 255);

// ===== 数据色 =====
pub const COLOR_INPUT: Color = Color::from_rgb8(59, 130, 246);   // 蓝色
pub const COLOR_OUTPUT: Color = Color::from_rgb8(16, 185, 129);  // 绿色
pub const COLOR_CACHE: Color = Color::from_rgb8(245, 158, 11);   // 橙色
pub const COLOR_REASONING: Color = Color::from_rgb8(139, 92, 246); // 紫色
pub const COLOR_RESOURCING: Color = Color::from_rgb8(236, 72, 153); // 粉色

// ===== 状态色 =====
pub const COLOR_SUCCESS: Color = Color::from_rgb8(34, 197, 94);
pub const COLOR_WARNING: Color = Color::from_rgb8(234, 179, 8);
pub const COLOR_ERROR: Color = Color::from_rgb8(239, 68, 68);
pub const COLOR_OFFICIAL: Color = Color::from_rgb8(34, 197, 94);
pub const COLOR_CALCULATE: Color = Color::from_rgb8(234, 179, 8);

// ===== 热力图色阶 (从冷到热) =====
pub const HEATMAP_EMPTY: Color = Color::from_rgb8(11, 38, 48);
pub const HEATMAP_LOW: Color = Color::from_rgb8(0, 68, 95);
pub const HEATMAP_MED: Color = Color::from_rgb8(0, 120, 140);
pub const HEATMAP_HIGH: Color = Color::from_rgb8(0, 180, 200);
pub const HEATMAP_MAX: Color = Color::from_rgb8(51, 224, 255);

// ===== 边框与间距 =====
pub const BORDER_SUBTLE: Color = Color::from_rgb8(30, 50, 65);
pub const BORDER_ACCENT: Color = Color::from_rgb8(51, 224, 255);

// ===== 尺寸常量 =====
pub const CARD_HEIGHT: f64 = 105.0;
pub const CARD_PADDING: f64 = 16.0;
pub const PANEL_PADDING: f64 = 16.0;
pub const SECTION_GAP: f64 = 12.0;
pub const CARD_CORNER_RADIUS: f64 = 8.0;

// ===== 字号 =====
pub const FONT_SIZE_TITLE: f32 = 28.0;
pub const FONT_SIZE_HEADING: f32 = 18.0;
pub const FONT_SIZE_BODY: f32 = 14.0;
pub const FONT_SIZE_SMALL: f32 = 12.0;
pub const FONT_SIZE_KPI: f32 = 32.0;

/// 根据 token 数量返回热力图颜色
pub fn heatmap_color(tokens: u64) -> Color {
    if tokens == 0 {
        HEATMAP_EMPTY
    } else if tokens < 1_000 {
        HEATMAP_LOW
    } else if tokens < 10_000 {
        HEATMAP_MED
    } else if tokens < 100_000 {
        HEATMAP_HIGH
    } else {
        HEATMAP_MAX
    }
}

/// 根据分位数动态计算热力图颜色 (实现自适应的极高品质视觉呈现)
pub fn heatmap_color_dynamic(tokens: u64, q25: u64, q50: u64, q75: u64) -> Color {
    if tokens == 0 {
        HEATMAP_EMPTY
    } else if tokens < q25 {
        HEATMAP_LOW
    } else if tokens < q50 {
        HEATMAP_MED
    } else if tokens < q75 {
        HEATMAP_HIGH
    } else {
        HEATMAP_MAX
    }
}

/// 格式化数字带千分位
pub fn format_with_commas(n: u64) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let len = bytes.len();
    if len <= 3 {
        return s;
    }
    let mut result = String::with_capacity(len + len / 3);
    for (i, &b) in bytes.iter().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            result.push(',');
        }
        result.push(b as char);
    }
    result
}

/// 格式化费用显示
pub fn format_cost(cost: f64) -> String {
    if cost < 0.01 {
        format!("${:.4}", cost)
    } else if cost < 1.0 {
        format!("${:.3}", cost)
    } else {
        format!("${:.2}", cost)
    }
}
