//! 配色（**深色 / 浅色两套**，跟着「应用设置 → 界面主题」走）。
//!
//! 刻意**不依赖** `gpui-component` 的 `Theme`/`ActiveTheme`：
//! 那套主题的字段名会随版本变动，而项目早期最需要的是"一定能编译、能看清"。
//! 这里只用 GPUI 自己的 `rgb()` / `hsla()`，颜色集中在这一个文件里，
//! 之后再换成 `gpui_component::Theme` 只需改这里。
//!
//! 底色一律用**预先算好的不透明色**而不是 `rgba(...).opacity()`：
//! 半透明叠在什么背景上会随布局变化，预先定色更可控，也少一层 API 依赖。
//!
//! # 当前模式存在哪
//!
//! 存一个**进程内的全局**（[`set_mode`] / [`mode`]），而不是往每个渲染函数里
//! 传 `cx`：全项目有一百多处 `theme::text()` / `theme::bg_card()` 这样的调用，
//! 为了取一个枚举给每个函数加参数，改动能把整份 `views.rs` 翻一遍，
//! 而收益只是"同一个值换了个来源"。
//!
//! 代价是**必须在切主题时同时调用 [`set_mode`]**（`main.rs` 启动时一次、
//! `Shell::apply_theme` 每次切换时一次）。漏掉的症状很明确：界面还是旧配色。
//! 这也正是 `ThemePref` 的注释里说的"两套配色要一起切"。

use std::sync::atomic::{AtomicU8, Ordering};

use gpui_kit::{Hsla, Rgba, rgb};

/// 当前配色模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// 深色。
    Dark,
    /// 浅色。
    Light,
}

impl Mode {
    /// 给日志/提示用的一句话。
    pub fn label(self) -> &'static str {
        match self {
            Self::Dark => "深色",
            Self::Light => "浅色",
        }
    }
}

/// 0 = 深色（默认），1 = 浅色。用 `Relaxed`：这是一个"值本身"，
/// 不靠它同步别的内存，最坏情况是这一帧用了上一帧的模式。
static MODE: AtomicU8 = AtomicU8::new(0);

/// 切换配色模式（**必须**和 `gpui_kit::component::Theme::change` 成对调用）。
pub fn set_mode(mode: Mode) {
    MODE.store(
        match mode {
            Mode::Dark => 0,
            Mode::Light => 1,
        },
        Ordering::Relaxed,
    );
}

/// 当前配色模式。
pub fn mode() -> Mode {
    if MODE.load(Ordering::Relaxed) == 1 {
        Mode::Light
    } else {
        Mode::Dark
    }
}

/// 把"是不是深色"落到模式上，并**两套配色一起切**。
///
/// 两套：
///
/// 1. 我们自己画的那些 `div` —— [`set_mode`] 那个全局；
/// 2. gpui-component 的控件（`Input` / `Button` / `Select`…）—— `Theme::change`，
///    之后还要把它的强调色换成我们自己的（[`tune_component_theme`]）。
///
/// ⚠️ 顺序不能反：`Theme::change` 会**重新加载**那套主题、把我们改过的颜色盖回去，
/// 所以换色必须在它之后（组件库自己的文档也这么说）。
pub fn apply(is_dark: bool, cx: &mut gpui_kit::App) {
    let mode = if is_dark { Mode::Dark } else { Mode::Light };
    set_mode(mode);
    gpui_kit::component::Theme::change(
        match mode {
            Mode::Dark => gpui_kit::component::ThemeMode::Dark,
            Mode::Light => gpui_kit::component::ThemeMode::Light,
        },
        None,
        cx,
    );
    tune_component_theme(cx);
}

/// 把组件库的**按钮配色**（主按钮 + 默认/次要按钮 + 边框）换成我们自己的。
///
/// # 为什么必须换
///
/// 组件库的强调色是 shadcn 那套**中性色**：深色模式下 `button_primary` 接近
/// 白底黑字、浅色模式下接近黑底白字。而本面板自己的强调色是蓝的
/// （链接、徽标、选中项都用 [`primary`]）—— 于是同一页里
/// 「创建容器」（组件库的主按钮，一个白块）和蓝色链接并排放着，看着就像
/// **两个来源的按钮**（用户就是拿这个来问的）。
///
/// # 为什么两组都要写
///
/// `button_primary*` 在主题加载时是"从 `primary*` 落下来"的（组件库
/// `ThemeColor` 的字段注释写着 `fallback to primary`），**加载之后**再改
/// `primary` 不会回头改它 —— 所以两组一起写。
/// # 为什么次要按钮也要管
///
/// 用户接着反馈"『清理已停止』这种按钮在深色模式下有点看不清"。读了组件库
/// `button.rs` 才明白：**默认变体**的边框取的是 `input` 那个 token
/// （`fn border_color` 里 `Self::Default => cx.theme().input`），而底色取
/// `tokens.button` —— 在深色主题里这两者都接近背景色，于是次要按钮看起来
/// 就是"一行字加一个几乎看不见的框"。这里一并换掉：
///
/// - `button*`（默认变体的面）换成比卡片**亮一档**的 [`Palette::button_face`]；
/// - `input` 与 `border` 换成我们自己的边框色（顺带让输入框的描边也更清楚）。
fn tune_component_theme(cx: &mut gpui_kit::App) {
    let palette = palette();
    let base: Hsla = rgb(palette.accent).into();
    let hover: Hsla = rgb(palette.accent_hover).into();
    let active: Hsla = rgb(palette.accent_active).into();
    let foreground: Hsla = rgb(palette.accent_fg).into();

    let face: Hsla = rgb(palette.button_face).into();
    let face_hover: Hsla = rgb(palette.button_face_hover).into();
    let face_active: Hsla = rgb(palette.button_face_active).into();
    let face_text: Hsla = rgb(palette.text).into();
    let line: Hsla = rgb(palette.border).into();

    // `Theme::update` 是组件库给的"不会写歪"的写入口：它把 `colors` 与 `tokens`
    // 两份颜色同步好、并刷新所有窗口（直接改 `Theme::global_mut` 会让两份颜色
    // 对不上 —— 组件库文档专门警告过这件事）。
    gpui_kit::component::Theme::update(cx, |theme| {
        theme.colors.primary = base;
        theme.colors.primary_hover = hover;
        theme.colors.primary_active = active;
        theme.colors.primary_foreground = foreground;

        theme.colors.button_primary = base;
        theme.colors.button_primary_hover = hover;
        theme.colors.button_primary_active = active;
        theme.colors.button_primary_foreground = foreground;

        // 默认变体（页面上"次要操作"都走这个）
        theme.colors.button = face;
        theme.colors.button_hover = face_hover;
        theme.colors.button_active = face_active;
        theme.colors.button_foreground = face_text;

        // `.secondary()` 目前没人用，但换个主题文件时别让它掉队
        theme.colors.button_secondary = face;
        theme.colors.button_secondary_hover = face_hover;
        theme.colors.button_secondary_active = face_active;
        theme.colors.button_secondary_foreground = face_text;

        // 默认变体的**边框**取的是 `input`
        theme.colors.input = line;
        theme.colors.border = line;
    });
}

/// 一套配色。
///
/// 字段和下面那些取色函数一一对应；加一个颜色要两套都加 ——
/// 编译器会盯着（结构体字面量少字段就编译不过）。
#[derive(Debug, Clone, Copy)]
struct Palette {
    bg: u32,
    bg_card: u32,

    text: u32,
    text_muted: u32,
    text_dim: u32,

    border: u32,
    primary: u32,
    success: u32,
    warning: u32,
    danger: u32,
    neutral: u32,

    primary_soft: u32,
    success_soft: u32,
    warning_soft: u32,
    danger_soft: u32,
    neutral_soft: u32,
    dim_soft: u32,
    danger_edge: u32,

    /// 给组件库用的强调色（主按钮底色 / 悬停 / 按下 / 字色）。
    ///
    /// 和 [`Palette::primary`] 是同一个色系但单独列出来：主按钮的**按下态**
    /// 需要比 `primary` 更深一档，而那个颜色在我们自己的界面里用不到。
    accent: u32,
    accent_hover: u32,
    accent_active: u32,
    accent_fg: u32,

    /// 次要按钮（组件库的**默认变体**）的"面"：常态 / 悬停 / 按下。
    ///
    /// 单独列一组而不是复用 `bg_card()`：次要按钮要比它所在的卡片**亮一档**
    /// 才看得出是个按钮 —— 深色下原来几乎和背景同色，用户反馈"看不清"。
    button_face: u32,
    /// 悬停 / 按下时的面。
    ///
    /// ⚠️ 这两个值和 `button_face` 的**差值要够大**：v0.6.5 之前深色下只差
    /// ~9/255（约 3.5%），实机反馈是"点了没感觉"。现在深色差 ~18、浅色差 ~20。
    button_face_hover: u32,
    button_face_active: u32,

    /// 蓝色链接（容器名 / 发行版名）**悬停**时的文字色。
    link_hover: u32,
}

/// 深色（v0.2 起的原配色，保持不动）。
const DARK: Palette = Palette {
    bg: 0x0d_11_17,
    bg_card: 0x16_1b_23,

    text: 0xe6_ed_f3,
    text_muted: 0x8b_96_a5,
    text_dim: 0x5c_67_74,

    border: 0x26_2d_38,
    primary: 0x3b_82_f6,
    success: 0x3f_b9_50,
    warning: 0xd2_99_22,
    danger: 0xf8_51_49,
    neutral: 0x6e_76_81,

    primary_soft: 0x15_23_38,
    success_soft: 0x12_2b_1a,
    warning_soft: 0x2b_23_11,
    danger_soft: 0x2c_16_18,
    neutral_soft: 0x1b_20_27,
    dim_soft: 0x18_1d_24,
    danger_edge: 0x5a_24_28,

    accent: 0x3b_82_f6,
    accent_hover: 0x5a_95_f8,
    accent_active: 0x2f_6f_dc,
    accent_fg: 0xff_ff_ff,

    button_face: 0x21_28_33,
    button_face_hover: 0x33_3e_4d,
    button_face_active: 0x15_1a_21,

    link_hover: 0x7a_ac_ff,
};

/// 浅色。
///
/// 语义色的取值刻意**比深色那套深一档**：浅底上的文字要压得更暗才够对比
/// （深色主题里 `#f85149` 这种亮红放在白底上会发飘、看不清）。
/// 柔和底同理，用非常浅的同色系（不是把前景色加透明度 —— 见文件头的说明）。
const LIGHT: Palette = Palette {
    bg: 0xf4_f5_f7,
    bg_card: 0xff_ff_ff,

    text: 0x1f_23_28,
    text_muted: 0x54_5d_68,
    text_dim: 0x86_90_9b,

    border: 0xd5_db_e2,
    primary: 0x1f_6f_eb,
    success: 0x1a_7f_37,
    warning: 0x9a_67_00,
    danger: 0xcf_22_2e,
    neutral: 0x6b_74_7e,

    primary_soft: 0xe8_f0_fe,
    success_soft: 0xe4_f5_e9,
    warning_soft: 0xfd_f3_e0,
    danger_soft: 0xfd_eb_ed,
    neutral_soft: 0xf0_f1_f4,
    dim_soft: 0xf2_f3_f6,
    danger_edge: 0xf0_b4_b8,

    accent: 0x1f_6f_eb,
    accent_hover: 0x3a_84_f0,
    accent_active: 0x18_5a_c2,
    accent_fg: 0xff_ff_ff,

    button_face: 0xed_f0_f4,
    button_face_hover: 0xdf_e5_ec,
    button_face_active: 0xcb_d3_dd,

    // 浅色下链接"悬停变深"更符合直觉（深色主题里则是变亮）。
    link_hover: 0x18_5a_c2,
};

/// 当前模式对应的那一套。
fn palette() -> &'static Palette {
    match mode() {
        Mode::Dark => &DARK,
        Mode::Light => &LIGHT,
    }
}

// ---------------------------------------------------------------------------
// 取色函数
// ---------------------------------------------------------------------------

/// 窗口底色。
pub fn bg() -> Rgba {
    rgb(palette().bg)
}
/// 卡片底色。
pub fn bg_card() -> Rgba {
    rgb(palette().bg_card)
}
/// 主要文字色。
pub fn text() -> Rgba {
    rgb(palette().text)
}
/// 次要文字色。
pub fn text_muted() -> Rgba {
    rgb(palette().text_muted)
}
/// 提示文字色。
pub fn text_dim() -> Rgba {
    rgb(palette().text_dim)
}
/// 边框色。
pub fn border() -> Rgba {
    rgb(palette().border)
}
/// 主色。
pub fn primary() -> Rgba {
    rgb(palette().primary)
}
/// 成功色。
pub fn success() -> Rgba {
    rgb(palette().success)
}
/// 警告色。
pub fn warning() -> Rgba {
    rgb(palette().warning)
}
/// 危险色。
pub fn danger() -> Rgba {
    rgb(palette().danger)
}
/// 中性色。
pub fn neutral() -> Rgba {
    rgb(palette().neutral)
}

/// 主色柔和底。
pub fn primary_soft() -> Rgba {
    rgb(palette().primary_soft)
}

/// 蓝色链接（容器名 / 发行版名）**悬停**文字色。
pub fn link_hover() -> Rgba {
    rgb(palette().link_hover)
}
/// 成功色柔和底。
pub fn success_soft() -> Rgba {
    rgb(palette().success_soft)
}
/// 警告色柔和底。
pub fn warning_soft() -> Rgba {
    rgb(palette().warning_soft)
}
/// 危险色柔和底。
pub fn danger_soft() -> Rgba {
    rgb(palette().danger_soft)
}
/// 中性色柔和底。
pub fn neutral_soft() -> Rgba {
    rgb(palette().neutral_soft)
}
/// 弱化柔和底。
pub fn dim_soft() -> Rgba {
    rgb(palette().dim_soft)
}
/// 错误横幅描边。
pub fn danger_edge() -> Rgba {
    rgb(palette().danger_edge)
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

/// WSL **发行版**状态徽标的 `(前景, 底色)` 配色。
///
/// 刻意与 [`state_colors`] 分开：`ContainerState`（来自 `wslc`）和
/// `DistroState`（来自 `wsl`）是两套独立枚举，
/// 硬合成一个函数只会让两边都变难改。
pub fn distro_state_colors(state: &wslc_core::model::DistroState) -> (Rgba, Rgba) {
    use wslc_core::model::DistroState as S;
    match state {
        S::Running => (success(), success_soft()),
        S::Stopped => (neutral(), neutral_soft()),
        // 过渡态不是错误，只是"正在忙" —— 用主色
        S::Installing | S::Converting => (primary(), primary_soft()),
        // 卸载中确实是破坏性动作，用危险色
        S::Uninstalling => (danger(), danger_soft()),
    }
}
