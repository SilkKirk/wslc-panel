//! 配色。
//!
//! 刻意**不依赖** `gpui-component` 的 `Theme`/`ActiveTheme`：
//! 那套主题的字段名会随版本变动，而项目早期最需要的是"一定能编译、能看清"。
//! 这里只用 GPUI 自己的 `rgb()` / `hsla()`，颜色集中在这一个文件里，
//! 之后再换成 `gpui_component::Theme` 只需改这里。
//!
//! 底色一律用**预先算好的不透明色**而不是 `rgba(...).opacity()`：
//! 半透明叠在什么背景上会随布局变化，预先定色更可控，也少一层 API 依赖。

use gpui_kit::{Hsla, Rgba, hsla, rgb};

// ---------------------------------------------------------------------------
// 背景层次：越靠前越"远"
// ---------------------------------------------------------------------------

/// 窗口底色。
pub const BG: u32 = 0x0d_11_17;
/// 侧边栏底色。
pub const BG_SIDEBAR: u32 = 0x0a_0e_14;
/// 卡片/面板底色。
pub const BG_CARD: u32 = 0x16_1b_23;
/// 选中态底色。
pub const BG_SELECTED: u32 = 0x1d_2c_45;

// ---------------------------------------------------------------------------
// 文本层次
// ---------------------------------------------------------------------------

/// 主要文字。
pub const TEXT: u32 = 0xe6_ed_f3;
/// 次要文字。
pub const TEXT_MUTED: u32 = 0x8b_96_a5;
/// 更弱的提示文字。
pub const TEXT_DIM: u32 = 0x5c_67_74;

// ---------------------------------------------------------------------------
// 语义色（前景）与对应的柔和底色
// ---------------------------------------------------------------------------

/// 边框。
pub const BORDER: u32 = 0x26_2d_38;
/// 主色（强调、按钮）。
pub const PRIMARY: u32 = 0x3b_82_f6;
/// 成功 / 运行中。
pub const SUCCESS: u32 = 0x3f_b9_50;
/// 警告。
pub const WARNING: u32 = 0xd2_99_22;
/// 危险 / 已停止。
pub const DANGER: u32 = 0xf8_51_49;
/// 中性（已退出）。
pub const NEUTRAL: u32 = 0x6e_76_81;

/// 主色的柔和底（徽标、选中块）。
pub const PRIMARY_SOFT: u32 = 0x15_23_38;
/// 成功色的柔和底。
pub const SUCCESS_SOFT: u32 = 0x12_2b_1a;
/// 警告色的柔和底。
pub const WARNING_SOFT: u32 = 0x2b_23_11;
/// 危险色的柔和底（错误横幅）。
pub const DANGER_SOFT: u32 = 0x2c_16_18;
/// 中性色的柔和底。
pub const NEUTRAL_SOFT: u32 = 0x1b_20_27;
/// 弱化的柔和底（未知状态）。
pub const DIM_SOFT: u32 = 0x18_1d_24;
/// 错误横幅的描边。
pub const DANGER_EDGE: u32 = 0x5a_24_28;

// ---------------------------------------------------------------------------
// 取色函数
// ---------------------------------------------------------------------------

/// 窗口底色。
pub fn bg() -> Rgba {
    rgb(BG)
}
/// 侧边栏底色。
pub fn bg_sidebar() -> Rgba {
    rgb(BG_SIDEBAR)
}
/// 卡片底色。
pub fn bg_card() -> Rgba {
    rgb(BG_CARD)
}
/// 选中底色。
pub fn bg_selected() -> Rgba {
    rgb(BG_SELECTED)
}
/// 主要文字色。
pub fn text() -> Rgba {
    rgb(TEXT)
}
/// 次要文字色。
pub fn text_muted() -> Rgba {
    rgb(TEXT_MUTED)
}
/// 提示文字色。
pub fn text_dim() -> Rgba {
    rgb(TEXT_DIM)
}
/// 边框色。
pub fn border() -> Rgba {
    rgb(BORDER)
}
/// 主色。
pub fn primary() -> Rgba {
    rgb(PRIMARY)
}
/// 成功色。
pub fn success() -> Rgba {
    rgb(SUCCESS)
}
/// 警告色。
pub fn warning() -> Rgba {
    rgb(WARNING)
}
/// 危险色。
pub fn danger() -> Rgba {
    rgb(DANGER)
}
/// 中性色。
pub fn neutral() -> Rgba {
    rgb(NEUTRAL)
}

/// 主色柔和底。
pub fn primary_soft() -> Rgba {
    rgb(PRIMARY_SOFT)
}
/// 成功色柔和底。
pub fn success_soft() -> Rgba {
    rgb(SUCCESS_SOFT)
}
/// 警告色柔和底。
pub fn warning_soft() -> Rgba {
    rgb(WARNING_SOFT)
}
/// 危险色柔和底。
pub fn danger_soft() -> Rgba {
    rgb(DANGER_SOFT)
}
/// 中性色柔和底。
pub fn neutral_soft() -> Rgba {
    rgb(NEUTRAL_SOFT)
}
/// 弱化柔和底。
pub fn dim_soft() -> Rgba {
    rgb(DIM_SOFT)
}
/// 错误横幅描边。
pub fn danger_edge() -> Rgba {
    rgb(DANGER_EDGE)
}

/// 模态遮罩（半透明黑）。
///
/// 用 `hsla` 而不是 `rgb(...).opacity(...)`：前者是 GPUI 里
/// 与 `rgb` 同级的基础构造器，`gpui-base` 自己也在用（见 `styled.rs`）。
pub fn scrim() -> Hsla {
    hsla(0., 0., 0., 0.55)
}

/// 状态徽标的 `(前景, 底色)` 配色。
///
/// 集中在一处，UI 侧不用自己拼颜色。
pub fn state_colors(state: &wslc_core::model::ContainerState) -> (Rgba, Rgba) {
    use wslc_core::model::ContainerState as S;
    match state {
        S::Running => (success(), success_soft()),
        S::Paused | S::Restarting => (warning(), warning_soft()),
        S::Exited | S::Dead => (neutral(), neutral_soft()),
        S::Created | S::Removing => (primary(), primary_soft()),
        S::Unknown(_) => (text_dim(), dim_soft()),
    }
}
