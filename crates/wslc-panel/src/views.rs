//! 页面渲染。
//!
//! 全部页面都是纯函数：入参是 [`AppState`] 的只读引用 + `Entity<Shell>`，
//! 出参是元素。所有交互通过 `entity.update(...)` 回写到状态，
//! 因此这里没有可变状态，也没有异步逻辑 —— 渲染与数据彻底解耦。

// 注意：`primary()` / `danger()` 这些样式方法来自 trait `ButtonVariants`，
// 光导入 `Button` 是不够的 —— 这里用 glob 把 button 模块全带上。
use gpui_kit::component::button::*;
// `StyledExt` 提供 `font_bold` / `font_semibold` 等字重方法（由宏生成）。
use gpui_kit::component::input::{Input, InputState};
// 「微软商店」与「在线发行版」两张清单都是**下拉框**（参考实现也是
// `CustomComboBox`，不是我们原先那种按钮列表）。
use gpui_kit::component::select::Select;
// 列表行那颗「更多」用的下拉菜单（v0.9.14）。
//
// ⚠️ 它来自 gpui-kit 的**扩展 trait**：`Button::dropdown_menu(…)`（和
// `ButtonVariants` 一样，光导入 `Button` 是不够的），菜单项是 `PopupMenuItem`。
// 走的是组件的 popover 层（`deferred` + `POPUP_PRIORITY`），所以**不会被表格的
// `overflow_hidden` 裁掉** —— 那是这一类弹出能用的前提（见 `AGENTS.md` §7.5）。
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
// 基本信息页那四只圆环（v0.9.17）。
//
// 为什么用它而不是自己拿 `div` 拼：圆环要画弧（`canvas` + `PathBuilder`），
// 那条路本仓库一次都没走过；组件库里现成有 `ProgressCircle`（已核对 0.7.1 的
// 源码：`progress/progress_circle.rs`），直接用它就不必去赌没验证过的 API。
// ⚠️ 它的大小只能用 `Sizable::with_size()` 设，别用 Styled 的 `.size()`
// —— 见 `views::gauge` 里那段说明。
use gpui_kit::component::progress::ProgressCircle;
// 注意：这里**不导入** `Disableable` —— 全项目不再使用 `.disabled()`：
// gpui-component 的禁用态文字几乎看不清，改用"守卫 + 文案"表达不可用状态。
use gpui_kit::component::{Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::*;
// 弹窗的入场动画要一个时长（`Animation::new(Duration)`）。
use std::time::Duration;

use wslc_core::cmd::container::{PullPolicy, RunSpec};
// 安装的**计划**是纯逻辑（在 `wslc-core` 里）：页面直接把它的步骤列表画出来，
// 和执行时读的是同一份数据 —— 预览不会和执行走偏。
use wslc_core::mirrors;
use wslc_core::model::install as install_model;
// MySQL 容器的识别是**纯逻辑**（镜像名 / 命令 / 3306 端口），
// 所以列表行每帧都能白拿这个判断 —— 见 `container_row_actions`。
use wslc_core::model::mysql as mysql_model;
// 「部署应用」的识别也是纯逻辑（镜像名 / 命令），同一个理由 ——
// 列表行每帧都要判断"这一行该不该画「部署管理」按钮"。
use wslc_core::model::deploy as deploy_model;
use wslc_core::model::{
    ContainerListItem, ContainerState, ContainerSummary, Distro, DistroState, GrantKind,
};
// 镜像仓库（Registry V2）的纯逻辑：常用镜像清单、引用/主机名校验、体积累加。
use wslc_core::registry;
// `/etc/wsl.conf` 的字段表与保序文档模型（纯逻辑，在 `wslc-core` 里）。
use wslc_core::model::wslconf;
use wslc_core::settings::{SETTING_KEYS, SettingKey, SettingKind};

// 显式导入而不靠 `gpui_kit::*`：勾选框是这一页独有的组件，
// 写明来源比"碰巧 glob 里有"可靠。
use gpui_kit::component::checkbox::Checkbox;

// `FactsState` 是「创建容器」弹窗里那条"从镜像读来的信息"的状态机
// （`ImageForm` 则是「拉取镜像」弹窗的表单）—— 两者的纯数据部分都在
// `wslc-core` / `wslc-panel-core`，这里只是画出来。
use crate::app::{
    CreateDialog, DbDialogKind, DbPageForm, FactsState, ImageForm, InstallForm, Shell,
    WslConfDialog,
};
// 列宽定义与配置项预设值都是纯数据，住在不依赖 GPUI 的 `wslc-panel-core` 里
// —— 这样它们的单测不必链接 GPUI（见那个 crate 的顶层说明）。
use crate::columns::{
    ALL_COLUMNS, BACKUP_COLUMNS, DB_COLUMNS, DB_USER_COLUMNS, DEPLOY_COLUMNS, DISTRO_COLUMNS,
    IMAGE_COLUMNS, NETWORK_COLUMNS, VOLUME_COLUMNS,
};
use crate::presets::presets_for;
use crate::state::{
    AppState, DashboardStats, DbFailure, DbState, DbTab, DeployEntry, DeployLogs, DeploySiteConf,
    DeployState, DistroAction, ImmediateAction, InstallOutcome, InstallProgress, InstallSourceKind,
    LogsState, OnlineListSource, Page, PendingAction, PromptKind, PullProgress, SettingsTab,
    WslConfState, format_bytes,
};
use crate::theme;
// 基本信息页那排圆环用的纯逻辑：用量分档（≥60% 黄、≥85% 红）。
// 它在 `wslc-panel-core` 里（带单测），因为这个文件里的断言 CI 不会跑。
use crate::util::{LoadLevel, load_level};

// ---------------------------------------------------------------------------
// 通用小组件
// ---------------------------------------------------------------------------

/// 卡片：标题 + 内容。
fn card(title: &'static str, body: impl IntoElement) -> impl IntoElement {
    v_flex()
        .w_full()
        .gap_3()
        .p_4()
        .rounded_lg()
        .bg(theme::bg_card())
        .border_1()
        .border_color(theme::border())
        .child(
            div()
                .text_sm()
                .font_semibold()
                .text_color(theme::text_muted())
                .child(title),
        )
        .child(body)
}

/// 模态浮层的**外壳**：满屏遮罩 + 居中的卡片槽。
///
/// # 为什么只抽"外壳"这一层
///
/// 里面那张卡片的宽度 / 内边距 / 标题 / 是否带 `id` / 要不要 `max_h` ——
/// 8 个弹窗各不相同，把它们都做成参数只会让每个调用点更长
/// （同一个判断见本文件里那四个按钮包装的注释）。
///
/// # 但这一层必须抽
///
/// 遮罩的**行为**（`.occlude()`）以前在 8 处各写一份，于是漏了 ——
/// 结果 8 个弹窗开着的时候都能点到背后的页面（实测：打开「创建容器」
/// 还能点它背后那一列的按钮）。行为只有一份，才不会下次又漏一处。
///
/// # `id` 是给**入场动画**用的
///
/// v0.6.5 起这里给整个遮罩播一次 ~140ms 的淡入（`with_animation`）：
/// 之前弹窗是"啪"地一下出现，实机反馈是"不顺滑"。
///
/// ⚠️ `id` 必须在**同一个弹窗**的多次重绘之间保持稳定、**不同弹窗之间不同**：
/// 稳定才不会每帧重播（GPUI 按这个 id 记住播放进度），不同才不会让两个弹窗
/// 共用一条进度（弹窗叠着的时候会互相干扰）。所以每个调用点都传自己的字面量。
pub fn modal_scrim(id: &'static str, card: impl IntoElement) -> AnyElement {
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(theme::scrim())
        // 见 `theme::scrim()`：不带 `occlude()` 的遮罩吃不到点击，
        // 点下去会穿到背后的页面上。
        .occlude()
        .child(card)
        // 一次性入场淡入：GPUI 在这 140ms 里逐帧请求重绘，结束后**保持终值**
        // （不再请求帧）。系统开了"减弱动态效果"时 GPUI 直接给终值 ——
        // 这是 `AnimationExt` 自己遵守的偏好，我们不用另外判断。
        .with_animation(id, Animation::new(Duration::from_millis(140)), |el, delta| el.opacity(delta))
        .into_any_element()
}

/// 键值对行。
fn kv(label: &'static str, value: impl Into<SharedString>) -> impl IntoElement {
    h_flex()
        .w_full()
        .justify_between()
        .gap_4()
        .child(
            div()
                .flex_none()
                .text_sm()
                .text_color(theme::text_dim())
                .child(label),
        )
        .child(
            div()
                .text_sm()
                .text_color(theme::text())
                .overflow_hidden()
                .truncate()
                .child(value.into()),
        )
}

/// 块状键值对行：标签一行，值**换行占满整行**。
///
/// 用于路径这类长值 —— [`kv`] 的右半边带 `truncate()`，
/// 长路径会被截成 `C:\...\wslc-cli-76...`，用户看不出到底是哪个文件。
/// 这里改用等宽字体 + 自动换行，一行放不下就折行，绝不截断。
fn kv_block(label: &'static str, value: impl Into<SharedString>) -> impl IntoElement {
    v_flex()
        .w_full()
        .gap_1()
        .child(
            div()
                .flex_none()
                .text_xs()
                .text_color(theme::text_dim())
                .child(label),
        )
        .child(
            div()
                .w_full()
                .text_sm()
                .text_color(theme::text())
                .font_family("Consolas")
                .child(value.into()),
        )
}

/// 顶部统计数字块。
fn stat_tile(label: &'static str, value: String, color: Rgba) -> impl IntoElement {
    v_flex()
        .flex_1()
        .gap_1()
        .p_3()
        .rounded_md()
        .bg(theme::bg_card())
        .border_1()
        .border_color(theme::border())
        .child(div().text_xs().text_color(theme::text_dim()).child(label))
        .child(div().text_xl().font_bold().text_color(color).child(value))
}

/// 状态徽标。
///
/// 底色由 [`theme::state_colors`] 统一给出（不透明的预置色），
/// 不用 alpha 叠加 —— 叠出来的观感会随背后的容器背景变化。
fn badge(text: impl Into<SharedString>, fg: Rgba, bg: Rgba) -> impl IntoElement {
    div()
        .flex_none()
        .px_2()
        .py_1()
        .rounded_md()
        .bg(bg)
        .text_xs()
        .font_semibold()
        .text_color(fg)
        .child(text.into())
}

/// 空状态占位。
fn empty_state(text: &'static str) -> impl IntoElement {
    v_flex()
        .w_full()
        .py_8()
        .items_center()
        .justify_center()
        .child(div().text_sm().text_color(theme::text_dim()).child(text))
}

/// 表格表头。
///
/// 列宽走 [`crate::columns::column_fractions`] 换成比例、再由 `flex_grow` 分配 ——
/// 于是表格**永远占满整行**并随窗口伸缩。以前是固定像素：宽度之和小于窗口时，
/// 右边留一大片空白，在宽窗口上看起来就是"前几列挤着、最后一列拖出去一截"。
///
/// 列之间的间隔用**单元格内边距**（`px_2`）而不是 `gap_2`：比例之和正好 100%，
/// 不留缝隙给 flex 去收缩。
fn table_header(columns: &'static [(&'static str, f32)]) -> impl IntoElement {
    let fractions = crate::columns::column_fractions(columns);
    h_flex()
        .w_full()
        .px_3()
        .py_2()
        .border_b_1()
        .border_color(theme::border())
        .children(columns.iter().zip(fractions).map(|((name, _), fraction)| {
            div()
                .flex_grow(fraction)
                .flex_basis(px(0.))
                // ⚠️ 必须允许收缩：flex 项默认 `min-width: auto`，
                // 一个长路径会把那一列撑破、把别的列挤出去
                .min_w_0()
                .px_2()
                .text_xs()
                .font_semibold()
                .text_color(theme::text_dim())
                .child(*name)
        }))
}

/// 表格行容器（列宽与 [`table_header`] 同一套比例，两边必须一致）。
fn table_row(columns: &'static [(&'static str, f32)], cells: Vec<AnyElement>) -> impl IntoElement {
    let fractions = crate::columns::column_fractions(columns);
    // 行里多出来的 cell（列定义没写）按平均宽度兜底，别让它挤成一条线
    let fallback = 1.0 / columns.len().max(1) as f32;
    h_flex()
        .w_full()
        .px_3()
        .py_2()
        .border_b_1()
        .border_color(theme::border())
        .justify_start()
        .children(cells.into_iter().enumerate().map(|(i, cell)| {
            div()
                .flex_grow(fractions.get(i).copied().unwrap_or(fallback))
                .flex_basis(px(0.))
                .min_w_0()
                // ⚠️ 必须裁剪：`min_w_0` 只允许**这一格**被压缩，
                // 格子里的内容（徽标、按钮都是 `flex_none`，不会被压）比格子宽时
                // 会照原样画出去 —— 于是越过列边界、盖在隔壁单元格上。
                // 实机踩到过一次：实例列表状态列里第二个徽标「本面板保持」
                // 正好糊在隔壁「版本」列的数字上，看起来像乱码（见 `distro_row`）。
                // 裁掉之后这类毛病的表现变成"被截断"，一眼能看出来。
                .overflow_hidden()
                .px_2()
                .child(cell)
        }))
}

/// 单元格文字。
fn cell_text(text: impl Into<SharedString>) -> AnyElement {
    div()
        .text_sm()
        .text_color(theme::text())
        .overflow_hidden()
        .truncate()
        .child(text.into())
        .into_any_element()
}

/// 次要单元格文字。
fn cell_muted(text: impl Into<SharedString>) -> AnyElement {
    div()
        .text_sm()
        .text_color(theme::text_muted())
        .overflow_hidden()
        .truncate()
        .child(text.into())
        .into_any_element()
}

/// 正常色的「可悬停看全文」单元格（表格里的主要标识，比如镜像引用）。
fn cell_text_full(key: impl Into<SharedString>, text: impl Into<SharedString>) -> AnyElement {
    cell_full(key, text, false)
}

/// 次要色的「可悬停看全文」单元格（镜像、安装位置、挂载点这类长文本）。
fn cell_muted_full(key: impl Into<SharedString>, text: impl Into<SharedString>) -> AnyElement {
    cell_full(key, text, true)
}

/// 会截断的单元格：省略号 + **悬停显示全文**。
///
/// # 为什么借组件库的 `Button`（`text` 变体）来画一行字
///
/// tooltip 那一套（`ManagedTooltipExt` / `ComponentTooltip`）在 gpui-kit 里是
/// `pub(crate)` 的 —— **只有它自己的控件**（`Button` / `Switch` / …）能挂 tooltip，
/// 我们自己画的 `div` 挂不上（`gpui::InteractiveElement` 本身没有 tooltip 方法，
/// 只有一个 `tooltip_show_delay` 设置）。所以这里用 `Button` 当"承载 tooltip 的壳"：
/// `text` 变体没有内边距 / 底色 / 边框（`no_padding()`），看起来就是一行字。
///
/// 内容是**自己画的**而不是用 `.label()`：`.label()` 的字号由按钮尺寸决定
/// （`button_text_size(size)`），和隔壁单元格的 `text_sm()` 未必一致；
/// 自己画就能保证同一张表里字号 / 省略号**完全一样**，看不出是两种东西。
///
/// # `key` 必须当帧唯一
///
/// GPUI 的 hover 状态按元素 id 记，id 重复会让两个单元格互相干扰。
/// 调用点传的是容器名 / 镜像引用 / 卷名 / 发行版名这类**本来就唯一**的标识。
fn cell_full(
    key: impl Into<SharedString>,
    text: impl Into<SharedString>,
    muted: bool,
) -> AnyElement {
    let key: SharedString = key.into();
    let text: SharedString = text.into();
    let color = if muted {
        theme::text_muted()
    } else {
        theme::text()
    };

    Button::new(SharedString::from(format!("cell-tip-{key}")))
        .text()
        .tooltip(text.clone())
        // 纯展示（"文字 + tooltip"），不是动作，别进 Tab 顺序
        .tab_stop(false)
        .w_full()
        .child(
            div()
                .w_full()
                .text_sm()
                .text_color(color)
                .overflow_hidden()
                .truncate()
                .child(text),
        )
        .into_any_element()
}

/// 状态徽标单元格。
fn cell_badge(state: ContainerState) -> AnyElement {
    let (fg, bg) = theme::state_colors(&state);
    badge(state.label().to_owned(), fg, bg).into_any_element()
}

/// 危险操作按钮（点击后弹出二次确认）。
fn danger_button(
    id: &str,
    label: &'static str,
    action: PendingAction,
    entity: &Entity<Shell>,
) -> impl IntoElement {
    let entity = entity.clone();
    Button::new(SharedString::from(id.to_owned()))
        .label(label)
        .small()
        .on_click(move |_, _, cx| {
            entity.update(cx, |shell, cx| {
                shell.request(action.clone(), cx);
            });
        })
}

/// 危险操作按钮（**发行版域**，点击后弹出二次确认）。
///
/// 和 [`danger_button`] 分开写，而不是合并成一个泛型函数：
/// 两个动作类型（[`PendingAction`] / [`DistroAction`]）没有任何共同方法，
/// 合并就得引入 trait 或枚举包装，调用点反而更长。
fn danger_button_distro(
    id: &str,
    label: &'static str,
    action: DistroAction,
    entity: &Entity<Shell>,
) -> impl IntoElement {
    let entity = entity.clone();
    Button::new(SharedString::from(id.to_owned()))
        .label(label)
        .small()
        .on_click(move |_, _, cx| {
            entity.update(cx, |shell, cx| {
                shell.request_distro(action.clone(), cx);
            });
        })
}

/// 打开「单输入框提示弹窗」的按钮（移动位置 / 调整大小 / 设置默认用户）。
///
/// 和 [`danger_button_distro`] 的区别：那个弹**确认**，这个弹**输入框**。
fn prompt_button(
    id: &str,
    label: &'static str,
    kind: PromptKind,
    distro: &str,
    entity: &Entity<Shell>,
) -> AnyElement {
    let entity = entity.clone();
    let distro = distro.to_owned();
    Button::new(SharedString::from(id.to_owned()))
        .label(label)
        .small()
        .on_click(move |_, window, cx| {
            // `window` 是必需的：`InputState::new` 要 `&mut Window`。
            let distro = distro.clone();
            entity.update(cx, |shell, cx| {
                shell.open_prompt(kind, distro, window, cx);
            });
        })
        .into_any_element()
}

/// 即时操作按钮（启动 / 重启）。
///
/// 与 [`danger_button`] 的区别：不弹二次确认，点了就跑。
fn immediate_button(
    id: &str,
    label: &'static str,
    action: ImmediateAction,
    entity: &Entity<Shell>,
) -> impl IntoElement {
    let entity = entity.clone();
    Button::new(SharedString::from(id.to_owned()))
        .label(label)
        .small()
        .on_click(move |_, _, cx| {
            entity.update(cx, |shell, cx| {
                shell.run_immediate(action.clone(), cx);
            });
        })
}

/// 打开一个**面板内浮层**的按钮（日志 / exec）。
///
/// 和 [`immediate_button`] / [`danger_button`] 并列的第三种入口：
/// 那两个分别是"直接起进程"和"弹二次确认"，这一类只是把某个浮层打开
/// （浮层自己再去后台取数据）。
///
/// 需要 `&mut Window`：`exec` 浮层要建 `InputState`，而 `InputState::new`
/// 要 `&mut Window` —— 所以 window 必须**从点击回调里取**，不能提前存
/// （提前存的话 `Shell::new` 拿不到 window，见 `AGENTS.md` §7.5 的懒创建）。
fn overlay_button(
    id: &str,
    label: &'static str,
    open: impl Fn(&mut Shell, &mut Window, &mut Context<Shell>) + 'static,
    entity: &Entity<Shell>,
) -> AnyElement {
    let entity = entity.clone();
    Button::new(SharedString::from(id.to_owned()))
        .label(label)
        .small()
        .on_click(move |_, window, cx| {
            entity.update(cx, |shell, cx| open(shell, window, cx));
        })
        .into_any_element()
}

// ---------------------------------------------------------------------------
// 页面分发
// ---------------------------------------------------------------------------

/// 渲染当前页面。
///
/// 收的是 `&Shell` 而不是 `&AppState`：从 P3 起有了**带输入框的页面**
/// （「添加实例」），而 `InputState` 住在 `Shell` 里 ——
/// `AppState` 刻意完全不碰 GPUI，这是那条分层约束的代价，也是它的价值。
///
/// `cx` 只有「添加实例」页用得上（要从输入框里实时读值做命令预览），
/// 但签名统一收着更省事 —— 免得每加一个需要它的页面就改一次分发。
pub fn page(shell: &Shell, cx: &App, entity: &Entity<Shell>) -> AnyElement {
    let state = &shell.state;
    match state.page {
        // 基本信息页只读快照（不起进程、不需要输入框），所以不用 entity。
        Page::Dashboard => dashboard(state).into_any_element(),
        // 这几个直接返回 `AnyElement`，不再多套一层转换。
        Page::Instances => instances(state, entity),
        Page::AddInstance => add_instance(shell, cx, entity),
        Page::Containers => containers(state, entity).into_any_element(),
        // 「镜像」页只要 `&AppState`：v0.6.3 起拉取表单在弹窗里（那个要 `&Shell`，
        // 见 `pull_dialog_overlay`），页面本身只剩一个按钮和一张表。
        Page::Images => images(state, entity).into_any_element(),
        Page::Networks => networks(state, entity).into_any_element(),
        Page::Volumes => volumes(state, entity).into_any_element(),
        // 「数据库管理」要 `&Shell`（连接卡那三个输入框住在 `Shell` 里）
        // **和** `cx`（渲染时要读输入框的值）—— 和「添加实例」同一类页面。
        Page::Database => database(shell, cx, entity),
        // 「部署管理」也要 `&Shell` + `cx`：「添加部署」弹窗那五格输入框
        // 住在 `Shell` 里（页面本身没有输入框）。
        Page::Deploy => deploy_page(shell, cx, entity),
        Page::AppSettings => app_settings(shell, entity),
        Page::About => about(state, entity),
    }
}

// ---------------------------------------------------------------------------
// ① 基本信息 / 总览
// ---------------------------------------------------------------------------

/// 基本信息页：**这台机器上的 WSL 环境** + 一排总览圆环。
///
/// # 这一页回答什么
///
/// 1. **这套 WSL 是什么版本**（`wslc info`）—— 报问题时第一步要抄的；
/// 2. **容器这一摊现在占了多少**（CPU / 内存 / 存储 / 数量）—— 四只圆环。
///
/// # 为什么是一排圆环而不是一张每容器的表（v0.9.17）
///
/// v0.9.16 这里还是一张"运行中容器（实时）"的表，每行一个容器的
/// CPU / 内存 / 网络 / 进程数。实机反馈是**不需要看到每个容器** ——
/// 想知道的是"总共占了多少"，而且要 1Panel 那种一眼的总览。
///
/// 于是：表整个去掉（要看单个容器去「容器」页），换成四只环。
/// 这也顺手解掉了一个老问题：那一列 `docker.1panel.live/library...`
/// 之类的长镜像名在窄表里必然被截断，而环不需要那么宽。
///
/// # 四个环的口径
///
/// 数字全部来自 [`DashboardStats`]（**纯算术**，这一页自己不起任何进程），
/// 每一环下面那行小字就是口径本身，卡片底部还有一句总说明 ——
/// 口径写清楚是刻意的：`wslc stats` 的 CPU% 是 **100% ≈ 1 个核**，
/// 不折算的话 32 核机器上那几个小数会把人看糊涂。
pub fn dashboard(state: &AppState) -> impl IntoElement {
    let stats = state.snapshot.dashboard_stats();

    v_flex()
        .w_full()
        .gap_4()
        .child(environment_card(state))
        .child(status_card(&stats))
        .child(overview_tiles(state))
}

// ---------------------------------------------------------------------------
// 基本信息页的三块
// ---------------------------------------------------------------------------

/// 运行环境：`wslc info` 里**排障真正要用到**的那三项。
///
/// 横着排三格而不是竖着列三行：这一页的高度要留给下面那排圆环，
/// 而三项都很短（版本号），铺满一行正好。
///
/// 刻意**不显示** `Direct3D` / `DXCore`：它们是给 GPU 直通 / 计算负载看的，
/// 和"容器跑不跑得起来"没关系 —— 以前它们和版本号并排摆着，
/// 用户得自己判断哪个重要。
fn environment_card(state: &AppState) -> AnyElement {
    let Some(info) = state.snapshot.info.as_ref() else {
        // 首屏（或 `wslc info` 失败）时给一句话，而不是三个空框。
        // 失败原因由 `app.rs` 的 `error_banner` 统一说，这里只解释这块为什么空着。
        return card(
            "运行环境",
            div()
                .text_sm()
                .text_color(theme::text_dim())
                .child("还没读到 wslc 的环境信息。"),
        )
        .into_any_element();
    };

    card(
        "运行环境",
        h_flex()
            .w_full()
            .gap_4()
            .items_start()
            .child(env_cell("WSL 版本", info.client.version.clone()))
            .child(env_cell("内核版本", info.client.kernel_version.clone()))
            .child(env_cell("Windows", info.client.windows_version.clone())),
    )
    .into_any_element()
}

/// 「运行环境」里的一格：小标签 + 值。
///
/// 值比正文大一档（`text_lg` + 半粗）：这三项是**要抄下来贴给别人**的
/// （报问题的第一步就是报 WSL / 内核版本），大一点好读。
///
/// 仍然加 `truncate`：Windows 版本在别的机器上可能是
/// `10.0.26100.1742 (Win11 24H2)` 这种带括号的，不指望它一定放得下。
fn env_cell(label: &'static str, value: String) -> impl IntoElement {
    v_flex()
        .flex_1()
        .min_w_0()
        .gap_1()
        .child(div().text_xs().text_color(theme::text_dim()).child(label))
        .child(
            div()
                .text_lg()
                .font_semibold()
                .text_color(theme::text())
                .overflow_hidden()
                .truncate()
                .child(value),
        )
}

/// 圆环的**传参**直径。
///
/// ⚠️ `ProgressCircle` 实际画出来的是这个数的 **0.75 倍**
/// （组件库 `Size::Size(s) => this.size(s * 0.75)`）—— 写 170，屏幕上量到的是
/// 127.5px。这个数没在实机上校过，是照 1Panel 那排环的比例估的：
/// `AGENTS.md` §8 那条"实机看一眼"照旧成立（尤其是窗口窄到 880px 时）。
const GAUGE_DIAMETER: f32 = 170.0;

/// 一个圆环：环里是"大数字 + 标签"，环下面一行小字写口径。
///
/// `value` 是 0–100 的百分比；`None` 表示**这一项读不到** —— 那时候环画成空的、
/// 大数字写 `—`。不能把"读不到"画成 0%：两者在环上长得一模一样，
/// 而"占用 0"和"没读到"对用户是两件事。
fn gauge(
    id: &'static str,
    value: Option<f64>,
    color: Rgba,
    big: String,
    label: &'static str,
    caption: String,
) -> AnyElement {
    // `Hsla` 要显式标注：`Rgba -> Hsla` 是 `From` 实现出来的，
    // 而参数类型是 `impl Into<Hsla>`，`rgba.into()` 在那儿推不出目标类型
    // （theme.rs 里那几行也是这么写的）。
    let color: Hsla = color.into();

    v_flex()
        .flex_1()
        .min_w_0()
        .items_center()
        .gap_2()
        .child(
            ProgressCircle::new(id)
                // `.value()` 自己会 clamp 到 0–100，这里先夹一次是为了
                // 让"读不到"和"超出"在代码里就是同一个 0。
                .value(value.unwrap_or(0.0).clamp(0.0, 100.0) as f32)
                .color(color)
                // ⚠️ 必须用 `.with_size()`（`Sizable`），**不能**用 Styled 的
                // `.size()`：组件库 render 里是先 `refine_style`（应用 Styled）
                // 再按 `Size` 覆盖尺寸的，`.size()` 会被它盖掉。
                .with_size(px(GAUGE_DIAMETER))
                .child(
                    v_flex()
                        .items_center()
                        .gap_1()
                        .child(
                            div()
                                .text_lg()
                                .font_bold()
                                .text_color(theme::text())
                                .child(big),
                        )
                        .child(div().text_xs().text_color(theme::text_dim()).child(label)),
                ),
        )
        .child(
            // ⚠️ 这行小字要**居中**，不能只写个 `w_full` 就完事：这一格是
            // `flex_1` 的一整列，文本默认左对齐 —— 环在格子正中、字却贴着
            // 格子左边，两行对不上（实机反馈："下面的字有点歪了"）。
            // 用 `justify_center` 而不是 `text_center()`：前者本仓库到处在用，
            // 后者一次都没用过 —— 本机没工具链，不赌没验证过的 API（AGENTS.md §1）。
            // `overflow_hidden` 兜住"比格子还长"的那种小字（居中裁掉两头，不越界）。
            h_flex()
                .w_full()
                .justify_center()
                .overflow_hidden()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child(caption),
                ),
        )
        .into_any_element()
}

/// 用量 → 环的颜色。
///
/// 阈值判定（≥60% 黄、≥85% 红）在 `wslc-panel-core` 里带单测 ——
/// 这个文件里的断言 CI 不会跑。拿不到值时按"轻载"上色（不吓人）。
fn gauge_color(percent: Option<f64>) -> Rgba {
    match load_level(percent.unwrap_or(0.0)) {
        LoadLevel::Low => theme::primary(),
        LoadLevel::Medium => theme::warning(),
        LoadLevel::High => theme::danger(),
    }
}

/// 环里那个大数字：百分比两位小数；读不到就是破折号。
fn gauge_big(percent: Option<f64>) -> String {
    match percent {
        Some(value) => format!("{value:.2} %"),
        None => "—".to_owned(),
    }
}

/// 「状态」卡：四只圆环 —— 容器 CPU / 容器内存 / 会话盘 / 容器数量。
///
/// 顺序：CPU → 内存 → 容器 → 存储。前三只都是"容器这一摊"的（CPU / 内存 / 数量），
/// 存储单独摆在最后 —— 实机反馈就是把存储和容器调了个个儿。
fn status_card(stats: &DashboardStats) -> AnyElement {
    let cpu_percent = stats.cpu_whole_machine_percent();
    let mem_percent = stats.mem_percent();
    let disk_percent = stats.vhd_percent_of_disk();
    let has_running = stats.running > 0;

    // 数量那一环：全部为 0 时没有"比例"可言，`None` 让环空着。
    let count_percent = (stats.total > 0)
        .then(|| stats.running as f64 / stats.total as f64 * 100.0);

    // CPU 环：拿不到核数时**照样报原始合计**（那一行小字会说明"拿不到核数"），
    // 但绝不把它当"整机占用率"—— 多核机器上 400% 是四个核跑满。
    let cpu_big = match (cpu_percent, has_running) {
        (Some(percent), _) => gauge_big(Some(percent)),
        (None, true) => format!("{:.2} %", stats.cpu_percent),
        (None, false) => gauge_big(None),
    };
    let cpu_caption = match (stats.cpu_cores, has_running) {
        (Some(cores), true) => format!("{:.2} / {cores} 核", stats.cpu_cores_used()),
        (None, true) => "各容器之和（拿不到核数）".to_owned(),
        _ => "没有正在运行的容器".to_owned(),
    };

    let mem_caption = match (has_running, stats.mem_limit) {
        (true, Some(limit)) => {
            format!("{} / {}", format_bytes(stats.mem_used), format_bytes(limit))
        }
        _ => "没有正在运行的容器".to_owned(),
    };

    // 存储那一环：分子是**会话 VHD** 的实际占用，分母是所在盘总量
    // （"容器这一摊占了这块盘多少"）。缺哪一半都不写数字 —— 一个比例只有
    // 分子或只有分母都是编的，不如写清楚缺什么。
    let disk_caption = match (stats.vhd_bytes, stats.disk) {
        (Some(vhd), Some(disk)) => {
            format!("{} / {}", format_bytes(vhd), format_bytes(disk.total))
        }
        (None, Some(disk)) => format!("VHD 还没建出来 / {}", format_bytes(disk.total)),
        (Some(vhd), None) => format!("{}（拿不到盘总量）", format_bytes(vhd)),
        (None, None) => "拿不到会话存储信息".to_owned(),
    };

    let (count_big, count_caption) = if stats.total > 0 {
        (
            format!("{} / {}", stats.running, stats.total),
            "运行中 / 全部".to_owned(),
        )
    } else {
        ("0 / 0".to_owned(), "还没有容器".to_owned())
    };

    // 底部那句总口径。**刻意写在这里而不是每一环里**：四条小字已经
    // 各说各的数字了，再各自加一句"这是什么"会把环淹掉。
    let cores_note = match stats.cpu_cores {
        Some(cores) => format!("CPU 合计 ÷ {cores} 核"),
        None => "CPU 是各容器之和（未折算）".to_owned(),
    };
    let note = crate::util::strip_md(&format!(
        "口径：只算容器 —— CPU 与内存的分母分别是核数与 WSL 的内存上限（{cores_note}）；\
         存储是**会话 VHD**（镜像、容器可写层、卷都在它里面）占所在盘总量的比例，\
         不是整块盘的占用率。"
    ));

    card(
        "状态",
        v_flex()
            .w_full()
            .gap_4()
            .child(
                h_flex()
                    .w_full()
                    .gap_3()
                    .items_start()
                    .child(gauge(
                        "gauge-container-cpu",
                        cpu_percent,
                        gauge_color(cpu_percent),
                        cpu_big,
                        "CPU",
                        cpu_caption,
                    ))
                    .child(gauge(
                        "gauge-container-memory",
                        mem_percent,
                        gauge_color(mem_percent),
                        gauge_big(mem_percent),
                        "内存",
                        mem_caption,
                    ))
                    .child(gauge(
                        "gauge-container-count",
                        count_percent,
                        // 数量不是"用量"，别让它在 85% 时变红 ——
                        // 「4 个容器全在跑」是好事，不是告警。
                        theme::success(),
                        count_big,
                        "容器",
                        count_caption,
                    ))
                    .child(gauge(
                        "gauge-session-disk",
                        disk_percent,
                        gauge_color(disk_percent),
                        gauge_big(disk_percent),
                        "存储",
                        disk_caption,
                    )),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme::text_dim())
                    .child(note),
            ),
    )
    .into_any_element()
}

/// 底部那排数字：镜像 / 网络 / 卷 / WSL 实例。
///
/// 容器的数量**不在这里** —— 它已经是上面那一环了，摆两遍只会让人
/// 犹豫该看哪个（`AGENTS.md` §5 同一条道理）。这一排回答的是"我这儿
/// 有多少东西"，想看清单一页一个，所以刻意**不可点**。
fn overview_tiles(state: &AppState) -> impl IntoElement {
    let snap = &state.snapshot;

    h_flex()
        .w_full()
        .gap_3()
        .child(stat_tile(
            "WSL 实例",
            snap.distros.len().to_string(),
            theme::text(),
        ))
        .child(stat_tile("镜像", snap.images.len().to_string(), theme::text()))
        .child(stat_tile("网络", snap.networks.len().to_string(), theme::text()))
        .child(stat_tile("卷", snap.volumes.len().to_string(), theme::text()))
}

// ---------------------------------------------------------------------------
// ② 全部 container
// ---------------------------------------------------------------------------

/// 资源使用率：CPU 与内存**上下两行**（照 1Panel 的排版）。
///
/// 已停止的容器没有 stats，显示 `—`。
fn cell_resources(summary: &ContainerSummary) -> AnyElement {
    match summary.stats.as_ref() {
        None => cell_muted("—"),
        Some(stats) => v_flex()
            .gap_1()
            .child(
                div()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(format!("CPU {}", stats.cpu_perc)),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(stats.mem_usage.clone()),
            )
            .into_any_element(),
    }
}

/// 容器名（蓝色链接，点开看详情）。
///
/// 照 1Panel 的做法：名字就是入口，表格里不再挤一列操作按钮。
/// 挂载这类占地方的信息全部放进详情弹窗。
///
/// 悬停变亮、按下回到主色（比悬停暗一档）—— 组件库的 `Button` 自带
/// hover/active，而我们这种自己画的 `div` 得手写，否则鼠标移上去毫无反应
/// （v0.6.5 之前就是这样，实机反馈"没有点击的感觉"）。
fn container_name_link(name: &str, entity: &Entity<Shell>) -> AnyElement {
    let entity = entity.clone();
    let for_click = name.to_owned();
    div()
        .id(SharedString::from(format!("detail-{name}")))
        .cursor_pointer()
        .text_sm()
        .text_color(theme::primary())
        .hover(|this| this.text_color(theme::link_hover()))
        .active(|this| this.text_color(theme::primary()))
        .child(name.to_owned())
        .on_click(move |_, _, cx| {
            let target = for_click.clone();
            entity.update(cx, |shell, cx| shell.open_detail(target, cx));
        })
        .into_any_element()
}

/// 行内操作：**只留最高频的那几个**，其余收进「更多」下拉菜单。
///
/// # 这一格现在是 `[数据库] [部署管理] [启动/停止] [更多]`（v0.9.14 改）
///
/// 之前它会把 7 个按钮全铺出来（终端 / 数据库 / 部署管理 / 重启 / 停止 /
/// 日志 / 删除），于是这一列在 1707 逻辑宽下被挤成两行、还要靠权重硬撑。
/// 现在行内只留：**这一类容器专属的入口**（数据库 / 部署管理）+ **最高频的
/// 状态切换**（启动 / 停止），其余（终端 / 重启 / 日志 / 删除）进「更多」。
///
/// 取舍的理由：
/// - 「数据库 / 部署管理」是**只能从这一行进**的两个页面（都不在侧边栏里）——
///   藏进菜单等于把唯一入口埋起来，所以它们必须在行内；
/// - 「启动 / 停止」是这一页最常点的动作，也是唯一一个"看着状态点"的动作；
/// - 「终端 / 重启 / 日志 / 删除」都是**低频或危险**的：删除有二次确认、
///   日志是排查时才看、终端只在运行中才有意义 —— 它们放菜单里正好。
///
/// ⚠️ 菜单里的项**只画当前有意义的**（停止的容器不画「终端」：`wslc exec`
/// 在它上面必然失败，那种"点了必然报错"的项不如不画）。
///
/// MySQL / MariaDB 容器多一个「数据库」按钮（v0.7.0）：它是那一类容器上
/// 最高频的动作，而且**只能从这里进**（数据库管理页不在侧边栏里）。
fn container_row_actions(item: &ContainerListItem, entity: &Entity<Shell>) -> AnyElement {
    let name = item.display_name();
    let running = item.is_running();
    let mut actions: Vec<AnyElement> = Vec::new();

    if running {
        // 「数据库」：只有**认得出是 MySQL 家族**的容器才有。
        //
        // 判定是**纯字符串**（镜像名 / 命令 / 3306 端口，见
        // `mysql_container_kind`）—— 渲染每帧都跑，绝不能在这里起进程。
        // 认不出来就什么都不画（和 `image_preset` 同一条原则：
        // 宁可少一个按钮，也不要给一个点进去全是报错的按钮）。
        //
        // 同样的理由省略了「停止的容器」那一支：页面里每一格都要
        // `wslc exec`，容器没跑就只有 `WSLC_E_CONTAINER_NOT_RUNNING`。
        // 先在上面那行点「启动」，按钮自然会出来。
        if let Some(_candidate) = mysql_model::mysql_container_kind(item) {
            let target = name.to_owned();
            let image = item.image.clone();
            actions.push(overlay_button(
                &format!("db-{name}"),
                // 依据（镜像名 / 命令 / 3306 端口）不进按钮文案 ——
                // 数据库页上写着，这里是清单，只放得下动作。
                "数据库",
                move |shell, window, cx| {
                    shell.open_database_page(target.clone(), image.clone(), window, cx)
                },
                entity,
            ));
        }

        // 「部署管理」（v0.9）：认得出是 nginx / Java / Node.js / Go 的容器才有。
        //
        // 判定同样是**纯字符串**（镜像名 / 启动命令，见 `container_app_kind`）
        // —— 渲染每帧都跑，绝不能在这里起进程。认不出来就什么都不画。
        //
        // ⚠️ 它和「数据库」会**同时**出现在一个既像 MySQL 又像这几类的容器上
        // （比如一个镜像名认不出来、命令里同时有 `mysqld` 和 `node` 的怪东西）。
        // 那两个按钮各自进去都会给出权威判定并说明"看起来不是" —— 这个代价
        //（多一个按钮）比"漏掉一个"小，和 v0.7 接受 3306 误判是同一条取舍。
        if let Some(candidate) = deploy_model::container_app_kind(item) {
            let target = name.to_owned();
            let image = item.image.clone();
            let runtime = candidate.runtime;
            let signal = candidate.signal;
            actions.push(overlay_button(
                &format!("deploy-{name}"),
                // 运行时名也不进按钮文案 —— 页面顶上写着"Nginx · 按镜像名判断"。
                "部署管理",
                move |shell, _, cx| {
                    shell.open_deploy_page(
                        target.clone(),
                        image.clone(),
                        runtime,
                        signal,
                        cx,
                    )
                },
                entity,
            ));
        }
    }

    // 「启动 / 停止」：行内**唯一**的通用动作（不分容器类型，只看状态）。
    if running {
        actions.push(
            danger_button(
                &format!("stop-{name}"),
                "停止",
                PendingAction::StopContainer(name.to_owned()),
                entity,
            )
            .into_any_element(),
        );
    } else {
        actions.push(
            immediate_button(
                &format!("start-{name}"),
                "启动",
                ImmediateAction::StartContainer(name.to_owned()),
                entity,
            )
            .into_any_element(),
        );
    }

    // 「更多」放在**最后**：它收的是低频 / 危险动作，顺序上也让"主要动作在左"
    // 这条读起来成立（和参考实现的 `终端 / 日志 / 更多` 同一个形状）。
    actions.push(container_more_menu(item, entity));

    h_flex()
        .gap_2()
        .flex_wrap()
        .children(actions)
        .into_any_element()
}

/// 行末那颗「更多」：低频 / 危险动作都收在这里。
///
/// # 菜单里放什么（只画当前有意义的）
///
/// | 什么时候 | 菜单里 |
/// |---|---|
/// | 运行中 | 终端 / 重启 / 日志 / 删除 |
/// | 已停止 | 日志 / 删除 |
///
/// - 「终端」只有运行中才画：`wslc exec` 在停止的容器上必然失败；
/// - 「日志」**不分运行状态**：`wslc logs` 读的是容器留下的日志，
///   停掉的容器照样有东西可看（排查"它为什么退了"正是看这个）；
/// - 「删除」放最后，前面加一条分隔线 —— 它是唯一不可撤销的那个。
///
/// ⚠️ 菜单项**不用** `.disabled()`（全项目不用，见文件头的说明），
/// 也不做红色高亮：0.7.1 的 `PopupMenuItem` 没有危险态样式，
/// 而这本来就该由二次确认（`PendingAction`）来兜底，而不是靠颜色。
///
/// ⚠️ 回调是 `'static` 的（菜单可能比这一帧活得久）：所以名字 / `Entity`
/// 都要 `move` 进去，不能借 `item`。
fn container_more_menu(item: &ContainerListItem, entity: &Entity<Shell>) -> AnyElement {
    let name = item.display_name().to_owned();
    let running = item.is_running();
    let entity = entity.clone();

    Button::new(SharedString::from(format!("more-{name}")))
        .label("更多")
        .small()
        .dropdown_menu(move |mut menu, _window, _cx| {
            // ⚠️ 每一次都从**捕获进来的** `entity` / `name` 克隆一份，绑定名不要
            // 叫 `entity`（CI 实测报过 E0382）：内层 `move` 闭包会把那个新绑定
            // 搬走，而"遮蔽"只在同一个块里生效 —— 下一句再写
            // `let entity = entity.clone()` 克隆的就是**已经被搬走的那个**。
            if running {
                let shell_entity = entity.clone();
                let app = name.clone();
                menu = menu.item(PopupMenuItem::new("终端").on_click(move |_, _, cx| {
                    shell_entity.update(cx, |shell, cx| {
                        shell.run_immediate(ImmediateAction::OpenContainerShell(app.clone()), cx);
                    });
                }));

                let shell_entity = entity.clone();
                let app = name.clone();
                menu = menu.item(PopupMenuItem::new("重启").on_click(move |_, _, cx| {
                    shell_entity.update(cx, |shell, cx| {
                        shell.run_immediate(ImmediateAction::RestartContainer(app.clone()), cx);
                    });
                }));
            }

            let shell_entity = entity.clone();
            let app = name.clone();
            menu = menu.item(PopupMenuItem::new("日志").on_click(move |_, _, cx| {
                shell_entity.update(cx, |shell, cx| shell.open_logs(app.clone(), cx));
            }));

            // 分隔线 + 「删除」：不可撤销的那个单独放在最后
            let shell_entity = entity.clone();
            let app = name.clone();
            menu.item(PopupMenuItem::separator())
                .item(PopupMenuItem::new("删除").on_click(move |_, _, cx| {
                    shell_entity.update(cx, |shell, cx| {
                        shell.request(PendingAction::RemoveContainer(app.clone()), cx);
                    });
                }))
        })
        .into_any_element()
}

/// 全部容器页（含已退出）。
pub fn containers(state: &AppState, entity: &Entity<Shell>) -> impl IntoElement {
    let items = &state.snapshot.all;

    // 「在运行的有几个」直接用快照里那份**只列运行中**的清单的长度。
    //
    // 为什么不数 `items.iter().filter(|s| s.item.is_running())`：那等于**换一个判据**。
    // 「停止全部」执行时问的是 `wslc list`（不带 `-a`，见
    // `cmd::container::list_running`），页面上这个数字必须和它**是同一批** ——
    // 否则"确认框里写着 3 个、实际停了 4 个"就会发生。`is_running()` 是
    // `state == "running"` 的严格判断，而 `wslc list` 还会列出哪些其它状态
    // **没有逐个实测过**（`wslc` 连 `pause` 这个子命令都没有），所以这里不去猜，
    // 直接用它的输出。
    let running = state.snapshot.running.len();

    let rows: Vec<AnyElement> = items
        .iter()
        .map(|summary| {
            table_row(
                ALL_COLUMNS,
                vec![
                    container_name_link(summary.item.display_name(), entity),
                    // 镜像引用通常比这一列宽（`docker.1panel.live/library/nginx:latest`），
                    // 截断之后悬停能看全文 —— 这一格用户经常要点/要认。
                    cell_muted_full(
                        summary.item.display_name().to_owned(),
                        summary.item.image.clone(),
                    ),
                    cell_badge(summary.item.state_kind()),
                    cell_resources(summary),
                    cell_text(crate::util::ports_summary(summary)),
                    container_row_actions(&summary.item, entity),
                ],
            )
            .into_any_element()
        })
        .collect();

    let prune = {
        let entity = entity.clone();
        Button::new("prune")
            .label("清理已停止")
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| {
                    shell.request(PendingAction::PruneContainers, cx);
                });
            })
    };

    // 「停止全部」和发行版页的「关停全部」是同一类东西：**页面级**动作
    // （一次影响所有容器），所以放工具栏，不放进任何一行 —— 放进行里
    // 会让人以为只影响那一行（`AGENTS.md` §5）。
    //
    // 一个都没在跑时干脆不显示：这时点它什么也做不了（执行侧会直接
    // 回一句"没有正在运行的容器"），让用户去确认一个没有效果的动作
    // 只会让人怀疑自己是不是看错了。
    //
    // ⚠️ 用 `then(..)` + `.children(..)`，**不**像实例页那样拿一个空 `div()`
    // 占位：那边它两边没有兄弟，而这里左边是那句计数、右边是「清理已停止」
    // —— 空元素夹在中间会让 `gap_2` 多出一份间距（8px 变 16px），
    // "一个都没在跑"的时候那两颗按钮之间的缝一眼就不对。
    let stop_all = (running > 0).then(|| {
        let entity = entity.clone();
        Button::new("stop-all")
            .label("停止全部")
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| {
                    shell.request(PendingAction::StopAllContainers { running }, cx);
                });
            })
            .into_any_element()
    });

    v_flex()
        .w_full()
        .gap_3()
        .child(
            h_flex()
                .w_full()
                .justify_between()
                .child({
                    let entity = entity.clone();
                    Button::new("create-container")
                        .label("创建容器")
                        .small()
                        .primary()
                        .on_click(move |_, window, cx| {
                            // `InputState` 只能在有 window 的地方创建，
                            // 所以弹窗是懒创建的。
                            entity.update(cx, |shell, cx| shell.open_create_dialog(window, cx));
                        })
                })
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .text_color(theme::text_muted())
                                .child(format!(
                                    "共 {} 个容器，{} 个在运行",
                                    items.len(),
                                    running
                                )),
                        )
                        .children(stop_all)
                        .child(prune),
                ),
        )
        .child(
            v_flex()
                .w_full()
                .rounded_lg()
                .bg(theme::bg_card())
                .border_1()
                .border_color(theme::border())
                .overflow_hidden()
                .child(table_header(ALL_COLUMNS))
                .child(if rows.is_empty() {
                    empty_state("没有任何容器").into_any_element()
                } else {
                    v_flex().w_full().children(rows).into_any_element()
                }),
        )
}
// ---------------------------------------------------------------------------
// ②b 容器里的数据库管理（MySQL / MariaDB，v0.7.0）
// ---------------------------------------------------------------------------

/// 「数据库管理」页。
///
/// # 它为什么不在侧边栏
///
/// 它服务**一个**容器（`AppState::db` 里记着是哪个），所以顶部有
/// 「← 返回容器」—— 和「添加实例」是同一类页面（只能从列表上的按钮进）。
///
/// # 三种形态
///
/// 1. **探测就失败**（不是 MySQL / 容器没运行 / 镜像里没客户端）：
///    只画那张说明卡片。这时候再摆一排空表和连接卡只会干扰判断。
/// 2. **正常**：连接卡 + tab（数据库 / 用户 / SQL 控制台）。
/// 3. **还没读到数据**：表格的位置放一句"正在读取…"。
///
/// # 密码
///
/// 连接卡上的密码是**明文**的（`DbPageForm` 的注释里写了为什么这一版不做掩码），
/// 而它只存在内存里：不写进 `prefs.json`、不进日志（`MysqlCreds` 的 `Debug`
/// 是手写脱敏的）、也不进错误消息（出口是 `cmd::mysql::sanitize`）。
pub fn database(shell: &Shell, cx: &App, entity: &Entity<Shell>) -> AnyElement {
    // 分发（`page()`）为了统一签名给每个页面都传 `cx`（那边有注释说明为什么）。
    // v0.9.2 起这一页的输入框**全在浮层里**（连接卡 → 「基础信息」、
    // 控制台 → 每行的「SQL」），正文一个都不用 —— 所以这里显式绑一下消警告，
    // 而不是把参数删掉：删了就得为这一页破例改分发，下一个人还得再改回来。
    let _ = cx;

    let Some(db) = shell.state.db.as_ref() else {
        // 理论上来不到（只可能从容器行上的按钮进，那里已经建好状态了），
        // 但真到了这里就说一句人话，别画一片空白。
        return empty_state("还没有选中容器 —— 请到「容器」页点某一行的「数据库」")
            .into_any_element();
    };
    // 表单没建好就没法渲染（输入框是懒创建的）。
    //
    // 这里**不再绑一个 `form`**：连接卡搬进浮层、控制台也搬进浮层之后，
    // 页面正文一个输入框都不用（`form` 只在 `db_info_overlay` /
    // `db_console_overlay` 里用），绑了就是一个 unused 警告。
    if shell.db_form.is_none() {
        return empty_state("连接表单还没建好 —— 请重新从容器列表进入这一页").into_any_element();
    }

    // 页头（含「基础信息」按钮）。
    let header = db_header(db, entity);

    // ⚠️ 连接卡**不再常驻页面**：实机反馈"不用进来就在上面占用空间"。
    // 它现在在 `db_info_overlay` 里（点页头的「基础信息」打开），
    // 所以这里一个 `connecting` 都不画 —— 两处都画就等于同一份信息两个入口。
    if let Some(blocked) = db.blocked.as_ref() {
        return v_flex()
            .w_full()
            .gap_3()
            .child(header)
            .child(db_blocked_card(blocked))
            .into_any_element();
    }

    let body: AnyElement = if db.loaded {
        match db.tab {
            // ⚠️ 凭据取**单一数据源**（`AppState.db_credentials`），不要从
            // `DbState` 上再挂一份克隆 —— v0.8 就是这么错的：`DbState.credentials`
            // 从没被同步过（构造函数里给的是空的），于是"建库同时建用户"
            // 明明成功、列表里那两列却永远是 `—`（实机截图抓到的）。
            DbTab::Databases => db_databases_card(db, &shell.state.db_credentials, entity),
            DbTab::Users => db_users_card(db, entity),
        }
    } else {
        db_loading(db.busy)
    };

    // 存档读坏了要说出来 —— 否则那两列全是 `—`，用户只会以为
    // "面板没记住我建的账号"，而真实原因是存档解析失败。
    let creds_warning: Option<AnyElement> = shell
        .state
        .db_credentials_error
        .as_ref()
        .map(|error| db_store_warning(&format!("数据库账号存档读不出来：{error}")));

    v_flex()
        .w_full()
        .gap_3()
        .child(header)
        .children(db.error.as_ref().map(db_error_banner))
        .children(creds_warning)
        .child(db_tabs(db.tab, entity))
        .child(body)
        .into_any_element()
}

/// 「基础信息」浮层：连接卡（用户名 / 密码 + 那两段说明）。
///
/// # 为什么从页面上搬进浮层
///
/// 原来连接卡常驻页面顶部（`AGENTS.md` §5 的"同一动作只给一个入口"当时是
/// 按"它描述这一页现在连的是谁、扫一眼就该知道"来定的）。实机反馈
/// **"不用进来就在上面占用空间"** —— 一页里最有价值的是那两张表，
/// 而占掉顶部 ~250px 的是一份**很少改**的连接信息。
///
/// 取舍：现在要"确认自己在用哪个账号"得点一下按钮。为了不让这一步变成
/// 盲区，页头的按钮就写着「基础信息」，而**连接失败时**页面上的错误条
/// 与「重新读取」仍然直接把注意力引到它上面。
pub fn db_info_overlay(
    db: &DbState,
    form: &DbPageForm,
    entity: &Entity<Shell>,
    cx: &App,
) -> AnyElement {
    let close = {
        let entity = entity.clone();
        Button::new("db-info-close")
            .label("关闭")
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.close_db_info(cx));
            })
    };

    modal_scrim(
        "db-info",
        v_flex()
            .id("db-info-card")
            .w(px(680.))
            .max_h(px(560.))
            .gap_4()
            .p_5()
            .rounded_lg()
            .bg(theme::bg_card())
            .border_1()
            .border_color(theme::border())
            .overflow_y_scroll()
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_lg()
                            .font_bold()
                            .text_color(theme::text())
                            .child("基础信息"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::text_dim())
                            .child(format!(
                                "容器 {} · {} · {} · 客户端 {}",
                                db.container,
                                db.family.label(),
                                db.image,
                                db.client_version
                                    .as_deref()
                                    .unwrap_or(db.client.as_str()),
                            )),
                    ),
            )
            // 连接卡本体照旧用它自己那份渲染（表单 + 说明都在里面）
            .child(db_connection_card(db, form, cx))
            .child(h_flex().w_full().justify_end().child(close)),
    )
    .into_any_element()
}

/// SQL 控制台浮层（**针对某个库**）。
///
/// # 为什么控制台不是 tab
///
/// 实机反馈："上面不需要导入 sql 和 sql 控制台，都是在具体数据库 database
/// 操作的"。SQL 本来就是"对哪个库说话"的事 —— 一个不带库的控制台入口，
/// 用户进去还得先想 `USE` 哪一条（不指定库时 `SELECT * FROM t` 直接报
/// `No database selected`，实测过）。
///
/// 所以入口只有一处：数据库列表那一行的「SQL」按钮（`open_db_console_for`，
/// 它顺手把这个库的账号写进连接卡）。
pub fn db_console_overlay(
    db: &DbState,
    form: &DbPageForm,
    entity: &Entity<Shell>,
    cx: &App,
) -> AnyElement {
    let close = {
        let entity = entity.clone();
        Button::new("db-console-close")
            .label("关闭")
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.close_db_console(cx));
            })
    };

    // 标题里带上库名：浮层是盖在列表上的，底下那行看不见了 ——
    // "我现在在对哪个库执行"必须写在脸上（`db_console_card` 里那行
    // 「当前库」是第二道保险）。
    let title = match db.sql_database.as_deref() {
        Some(name) => format!("SQL 控制台 · {name}"),
        None => "SQL 控制台".to_owned(),
    };

    modal_scrim(
        "db-console",
        v_flex()
            .w(px(940.))
            .max_h(px(640.))
            .gap_4()
            .p_5()
            .rounded_lg()
            .bg(theme::bg_card())
            .border_1()
            .border_color(theme::border())
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .items_center()
                    .child(
                        div()
                            .text_lg()
                            .font_bold()
                            .text_color(theme::text())
                            .child(title),
                    )
                    .child(close),
            )
            // 控制台本体（输入框 + 输出）—— 输出可能很长，`db_console_card`
            // 里的输出区自己就是滚动的（`log_text`，带 id）。
            .child(
                v_flex()
                    .id("db-console-body")
                    .max_h(px(520.))
                    .overflow_y_scroll()
                    .child(db_console_card(db, form, entity, cx)),
            ),
    )
    .into_any_element()
}

/// 页头：容器 / 镜像 / 家族 / 客户端 + 「← 返回容器」「基础信息」「重新读取」。
fn db_header(db: &DbState, entity: &Entity<Shell>) -> AnyElement {
    let back = {
        let entity = entity.clone();
        Button::new("db-back")
            .label("← 返回容器")
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.close_database_page(cx));
            })
    };

    // 连接卡（用户名 / 密码 + 说明）现在住在这个按钮后面 —— 它以前常驻页面顶部，
    // 实机反馈"不用进来就在上面占用空间"。按钮文案就叫「基础信息」，
    // 和浮层标题一致，用户点下去就知道会看到什么。
    let info = {
        let entity = entity.clone();
        Button::new("db-info")
            .label("基础信息")
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.open_db_info(cx));
            })
    };

    // 读取期间把按钮**换掉**（项目里不用 `.disabled()`）：置灰的按钮会让人
    // 反复去点，而"读取中…"说清了现在在等什么。
    let reload: AnyElement = if db.busy {
        div()
            .text_sm()
            .text_color(theme::text_muted())
            .child("读取中…")
            .into_any_element()
    } else {
        let entity = entity.clone();
        Button::new("db-reload")
            .label("重新读取")
            .small()
            .primary()
            .on_click(move |_, window, cx| {
                entity.update(cx, |shell, cx| shell.load_db(window, cx));
            })
            .into_any_element()
    };

    h_flex()
        .w_full()
        .justify_between()
        .items_center()
        .child(
            v_flex()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .font_semibold()
                        .child(format!("容器 {}", db.container)),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_dim())
                        .child(format!(
                            "{} · {} · 客户端 {}",
                            db.family.label(),
                            db.image,
                            db.client_version.as_deref().unwrap_or(db.client.as_str()),
                        )),
                ),
        )
        .child(h_flex().gap_2().child(back).child(reload))
        .into_any_element()
}

/// 连接卡：用户名 / 密码 + 一句说明。
///
/// ⚠️ 它**现在住在一个浮层里**（`db_info_overlay`，点页头的「基础信息」打开），
/// 不再常驻页面顶部 —— 实机反馈"不用进来就在上面占用空间"。原先这里的注释
/// 写的是"挂在**页面**上而不是弹窗里：用户扫一眼就该知道这一页连的是谁"，
/// 那条取舍被这条反馈推翻了：那一页最有价值的是两张表，而顶部 ~250px 花在
/// 一份很少改的连接信息上。
fn db_connection_card(db: &DbState, form: &DbPageForm, cx: &App) -> AnyElement {
    let mut body = v_flex()
        .w_full()
        .gap_3()
        .child(
            h_flex()
                .w_full()
                .gap_3()
                .child(form_field("db-user", "用户名", &form.user, cx, false))
                .child(form_field(
                    "db-password",
                    "密码（只存在内存里）",
                    &form.password,
                    cx,
                    false,
                )),
        );

    if let Some(note) = db.cred_note.clone() {
        body = body.child(
            div()
                .text_xs()
                .text_color(theme::text_dim())
                .child(crate::util::strip_md(&note)),
        );
    }

    body = body.child(
        div()
            .text_xs()
            .text_color(theme::text_muted())
            .child(
                // Markdown 文案 → 渲染处统一清标记（`util::strip_md`）
                crate::util::strip_md(
                    "面板靠**容器自带的** mysql / mariadb 客户端执行 SQL —— 所以容器要在运行、\
                     镜像里得有客户端（不要求把端口发布到主机）。密码走 MYSQL_PWD 环境变量，\
                     不进命令行；改完点右上角「重新读取」生效。",
                ),
            ),
    );

    card("连接", body).into_any_element()
}

/// 探测就失败时的说明卡（不是 MySQL / 没客户端 / 容器没运行）。
fn db_blocked_card(failure: &DbFailure) -> AnyElement {
    v_flex()
        .w_full()
        .gap_3()
        .p_4()
        .rounded_lg()
        .bg(theme::danger_soft())
        .border_1()
        .border_color(theme::danger_edge())
        .child(
            div()
                .text_sm()
                .font_semibold()
                .text_color(theme::danger())
                .child("这个容器现在管不了数据库"),
        )
        .child(
            div()
                .text_sm()
                .text_color(theme::text())
                .child(crate::util::strip_md(&failure.summary())),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child(
                    "面板不自己连 3306，而是驱动容器里的客户端 —— 所以它不依赖端口有没有\
                     发布出来，但要求那个客户端存在。想自己进去看，用「容器」页那一行\
                     「更多」菜单里的「终端」。",
                ),
        )
        .child(log_text(Some(&failure.detail), "db-blocked-detail"))
        .into_any_element()
}

/// 上一次失败：一句"该怎么办" +（可滚动的）原文。
fn db_error_banner(failure: &DbFailure) -> AnyElement {
    v_flex()
        .w_full()
        .gap_2()
        .p_3()
        .rounded_md()
        .bg(theme::danger_soft())
        .border_1()
        .border_color(theme::danger_edge())
        .child(
            div()
                .text_sm()
                .text_color(theme::danger())
                .child(crate::util::strip_md(&failure.summary())),
        )
        .child(log_text(Some(&failure.detail), "db-error-detail"))
        .into_any_element()
}

/// 「账号存档读不出来」这类**纯文字**警告。
///
/// 和 [`db_error_banner`] 分开：那个吃 `DbFailure`（会再给一条"该怎么办"的
/// 提示，是针对 SQL 报错分类的）；存档读坏不是 SQL 失败，没有可分类的东西，
/// 硬塞成 `DbFailure` 只会让分类器瞎猜一条不相干的建议。
fn db_store_warning(text: &str) -> AnyElement {
    div()
        .w_full()
        .p_3()
        .rounded_md()
        .bg(theme::danger_soft())
        .border_1()
        .border_color(theme::danger_edge())
        .child(div().text_sm().text_color(theme::danger()).child(text.to_owned()))
        .into_any_element()
}

/// 还没读到数据时占位。
fn db_loading(busy: bool) -> AnyElement {
    let text = if busy {
        "正在读取…"
    } else {
        "还没有读到数据 —— 点右上角「重新读取」"
    };
    v_flex()
        .w_full()
        .py_8()
        .items_center()
        .justify_center()
        .child(div().text_sm().text_color(theme::text_dim()).child(text))
        .into_any_element()
}

/// 三个 tab。
fn db_tabs(current: DbTab, entity: &Entity<Shell>) -> AnyElement {
    let buttons: Vec<AnyElement> = DbTab::ALL
        .iter()
        .map(|tab| {
            let tab = *tab;
            let entity = entity.clone();
            // id 用 `{:?}`：稳定、且与显示语言无关（和设置页那几个 tab 一致）
            let mut button = Button::new(SharedString::from(format!("db-tab-{tab:?}")))
                .label(tab.label())
                .small()
                .on_click(move |_, _, cx| {
                    entity.update(cx, |shell, cx| shell.set_db_tab(tab, cx));
                });
            if current == tab {
                button = button.primary();
            }
            button.into_any_element()
        })
        .collect();

    h_flex()
        .w_full()
        .gap_2()
        .children(buttons)
        .into_any_element()
}

/// 库名那一格：名字 + （系统库时）一个「系统」标记。
///
/// 系统库现在**不出现在列表里**（[`db_databases_card`] 里过滤掉了），
/// 所以这个标记实际只在别处（比如将来"显示系统库"的开关）还有用 ——
/// 留着是因为"这一行是不是系统库"这件事本身仍然要能一眼看出来。
fn db_name_cell(entry: &wslc_core::model::DbEntry) -> AnyElement {
    h_flex()
        .gap_2()
        .min_w_0()
        .child(
            div()
                .text_sm()
                .overflow_hidden()
                .truncate()
                .child(entry.name.clone()),
        )
        .children(entry.is_system().then(|| {
            badge("系统", theme::text_dim(), theme::neutral_soft()).into_any_element()
        }))
        .into_any_element()
}

/// 「数据库」tab。
///
/// `credentials` 是 `AppState` 上那份**唯一**的账号存档 —— 别从 `DbState`
/// 里取（那里曾经有一份从没同步过的克隆，见 [`database`] 里的注释）。
fn db_databases_card(
    db: &DbState,
    credentials: &wslc_panel_core::dbcreds::DbCredentialStore,
    entity: &Entity<Shell>,
) -> AnyElement {
    let new_database = {
        let entity = entity.clone();
        Button::new("db-new-database")
            .label("新建数据库…")
            .small()
            .primary()
            .on_click(move |_, window, cx| {
                entity.update(cx, |shell, cx| {
                    shell.open_db_dialog(DbDialogKind::NewDatabase, window, cx)
                });
            })
    };

    // ⚠️ 页头**没有**「导入 SQL」按钮（v0.9.2 拿掉的）：导入是"往**某个库**灌
    // 数据"的事，入口在每一行的「导入备份」上。实机反馈：
    // "数据库列表那里要一个导入备份，点击导入这个库的数据，上面不需要导入 sql"。

    // ⚠️ **系统库不显示**（用户要求）。它们仍然真实存在、也仍然能被 SQL 控制台
    // 访问 —— 只是不该混在"我的库"里（`sys` / `performance_schema` 还占着
    // 表数和占用那两列，看着像自己的数据）。数量在下面那行里报出来，
    // 免得用户以为读漏了。
    let visible: Vec<&wslc_core::model::DbEntry> =
        db.databases.iter().filter(|e| !e.is_system()).collect();
    let hidden_system = db.databases.len() - visible.len();

    let rows: Vec<AnyElement> = visible
        .iter()
        // `.copied()` 把 `&&DbEntry` 变回 `&DbEntry`：`iter()` 给的是双重引用，
        // 而 `db_name_cell(entry)` 要的是 `&DbEntry` —— 函数参数不做自动解引用
        // （方法调用才会），少这一步就是 E0308。
        .copied()
        .map(|entry| {
            let action = PendingAction::DropDatabase {
                container: db.container.clone(),
                database: entry.name.clone(),
                client: db.client.clone(),
                creds: db.creds.clone(),
            };
            // 「用户名 / 密码」只对**面板建的库**有记录（建库时同时建的用户，
            // 记在本地存档里）。别处建的库显示 `—` —— 不猜、不编。
            let credential = credentials.get(&db.container, &entry.name);
            let backup_button = overlay_button(
                &format!("db-backup-{}", entry.name),
                "备份",
                {
                    let name = entry.name.clone();
                    move |shell, window, cx| {
                        shell.open_db_backup_dialog(name.clone(), window, cx)
                    }
                },
                entity,
            );
            // 「SQL」：开控制台并**用这一行的库 + 这一行的账号**（面板建的库
            // 才有账号记录，别的库沿用连接卡上那个）。
            let sql_button = overlay_button(
                &format!("db-sql-{}", entry.name),
                "SQL",
                {
                    let name = entry.name.clone();
                    move |shell, window, cx| {
                        shell.open_db_console_for(name.clone(), window, cx)
                    }
                },
                entity,
            );
            // 「导入备份」：把这个库的数据从一个本地 `.sql` 灌进去。
            // 目标库**就是这一行**（写进 kind 里），弹窗里没有可改的库名。
            let import_button = overlay_button(
                &format!("db-import-{}", entry.name),
                "导入备份",
                {
                    let name = entry.name.clone();
                    move |shell, window, cx| {
                        shell.open_db_dialog(
                            DbDialogKind::ImportSql { database: name.clone() },
                            window,
                            cx,
                        )
                    }
                },
                entity,
            );
            table_row(
                DB_COLUMNS,
                vec![
                    db_name_cell(entry),
                    match credential {
                        Some(cred) => cell_text(cred.user.clone()),
                        None => cell_muted("—"),
                    },
                    match credential {
                        Some(cred) => cell_text(cred.password.clone()),
                        None => cell_muted("—"),
                    },
                    cell_muted(entry.charset.clone()),
                    cell_muted(entry.collation.clone()),
                    cell_muted(entry.tables.to_string()),
                    cell_muted(format_bytes(entry.bytes)),
                    h_flex()
                        .gap_2()
                        .child(backup_button)
                        .child(import_button)
                        .child(sql_button)
                        .child(
                            danger_button(
                                &format!("db-drop-{}", entry.name),
                                "删除",
                                action,
                                entity,
                            )
                            .into_any_element(),
                        )
                        .into_any_element(),
                ],
            )
            .into_any_element()
        })
        .collect();

    let count_text = if hidden_system > 0 {
        format!(
            "共 {} 个数据库（另有 {hidden_system} 个系统库未显示）",
            visible.len()
        )
    } else {
        format!("共 {} 个数据库", visible.len())
    };

    v_flex()
        .w_full()
        .gap_3()
        .child(
            h_flex()
                .w_full()
                .justify_between()
                .items_center()
                // 页头只剩「新建数据库…」：库级动作（导入备份 / SQL / 备份 /
                // 删除）全在下面每一行上。
                .child(h_flex().gap_2().child(new_database))
                .child(
                    div()
                        .text_sm()
                        .text_color(theme::text_muted())
                        .child(count_text),
                ),
        )
        .child(
            v_flex()
                .w_full()
                .rounded_lg()
                .bg(theme::bg_card())
                .border_1()
                .border_color(theme::border())
                .overflow_hidden()
                .child(table_header(DB_COLUMNS))
                .child(if rows.is_empty() {
                    empty_state("没有读到任何数据库").into_any_element()
                } else {
                    v_flex().w_full().children(rows).into_any_element()
                }),
        )
        .into_any_element()
}

/// 「用户」tab。
fn db_users_card(db: &DbState, entity: &Entity<Shell>) -> AnyElement {
    let new_user = {
        let entity = entity.clone();
        Button::new("db-new-user")
            .label("新建用户…")
            .small()
            .primary()
            .on_click(move |_, window, cx| {
                entity.update(cx, |shell, cx| {
                    shell.open_db_dialog(DbDialogKind::NewUser, window, cx)
                });
            })
    };

    let rows: Vec<AnyElement> = db
        .users
        .iter()
        .map(|user| {
            let user_name = user.user.clone();
            let host = user.host.clone();

            let alter = overlay_button(
                &format!("db-pass-{user_name}-{host}"),
                "改密码",
                {
                    let (user_name, host) = (user_name.clone(), host.clone());
                    move |shell, window, cx| {
                        shell.open_db_dialog(
                            DbDialogKind::AlterPassword {
                                user: user_name.clone(),
                                host: host.clone(),
                            },
                            window,
                            cx,
                        )
                    }
                },
                entity,
            );
            let grant = overlay_button(
                &format!("db-grant-{user_name}-{host}"),
                "授权",
                {
                    let (user_name, host) = (user_name.clone(), host.clone());
                    move |shell, window, cx| {
                        shell.open_db_dialog(
                            DbDialogKind::Grant {
                                user: user_name.clone(),
                                host: host.clone(),
                            },
                            window,
                            cx,
                        )
                    }
                },
                entity,
            );
            let drop = danger_button(
                &format!("db-drop-user-{user_name}-{host}"),
                "删除",
                PendingAction::DropUser {
                    container: db.container.clone(),
                    user: user_name.clone(),
                    host: host.clone(),
                    client: db.client.clone(),
                    creds: db.creds.clone(),
                },
                entity,
            )
            .into_any_element();

            table_row(
                DB_USER_COLUMNS,
                vec![
                    h_flex()
                        .gap_2()
                        .min_w_0()
                        .child(
                            div()
                                .text_sm()
                                .overflow_hidden()
                                .truncate()
                                .child(user.user.clone()),
                        )
                        .children(user.is_system().then(|| {
                            badge("内部", theme::text_dim(), theme::neutral_soft())
                                .into_any_element()
                        }))
                        .into_any_element(),
                    cell_muted(user.host.clone()),
                    cell_muted(user.plugin.clone()),
                    if user.super_priv {
                        badge("Y", theme::success(), theme::success_soft()).into_any_element()
                    } else {
                        cell_muted("N")
                    },
                    h_flex()
                        .gap_2()
                        .flex_wrap()
                        .child(alter)
                        .child(grant)
                        .child(drop)
                        .into_any_element(),
                ],
            )
            .into_any_element()
        })
        .collect();

    v_flex()
        .w_full()
        .gap_3()
        .child(
            h_flex()
                .w_full()
                .justify_between()
                .items_center()
                .child(new_user)
                .child(
                    div()
                        .text_sm()
                        .text_color(theme::text_muted())
                        .child(format!(
                            "共 {} 条账号记录（同一个用户名可以有多个主机）",
                            db.users.len()
                        )),
                ),
        )
        .child(
            v_flex()
                .w_full()
                .rounded_lg()
                .bg(theme::bg_card())
                .border_1()
                .border_color(theme::border())
                .overflow_hidden()
                .child(table_header(DB_USER_COLUMNS))
                .child(if rows.is_empty() {
                    empty_state("没有读到任何用户（当前账号可能没有读 mysql.user 的权限）")
                        .into_any_element()
                } else {
                    v_flex().w_full().children(rows).into_any_element()
                }),
        )
        .into_any_element()
}

/// 「SQL 控制台」tab。
///
/// 它是"上面两张表没做的功能"的**逃生口**：面板不解析、不改写用户写的 SQL，
/// 原样交给容器里的客户端。⚠️ 输出用 `--table`（stdout 不是 tty 时 mysql
/// 默认是 batch，不显式要表格的话控制台里会是一堆 tab —— 实测）。
fn db_console_card(
    db: &DbState,
    form: &DbPageForm,
    entity: &Entity<Shell>,
    cx: &App,
) -> AnyElement {
    let run: AnyElement = if db.busy {
        div()
            .text_sm()
            .text_color(theme::text_muted())
            .child("执行中…")
            .into_any_element()
    } else {
        let entity = entity.clone();
        Button::new("db-run-sql")
            .label("执行")
            .small()
            .primary()
            .on_click(move |_, window, cx| {
                entity.update(cx, |shell, cx| shell.run_db_console(window, cx));
            })
            .into_any_element()
    };

    // 回显这次输出是哪条语句的 —— 连着跑两条之后没有这一行就分不清了
    // （和 exec 浮层里那条"上一次执行"同一个理由）。
    let echo: AnyElement = if db.last_sql.is_empty() {
        div().into_any_element()
    } else {
        h_flex()
            .w_full()
            .gap_2()
            .items_center()
            .child(
                div()
                    .text_xs()
                    .text_color(theme::text_dim())
                    .child("上一次执行"),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .font_family("Consolas")
                    .text_xs()
                    .text_color(theme::text_muted())
                    .overflow_hidden()
                    .truncate()
                    .child(db.last_sql.clone()),
            )
            .into_any_element()
    };

    // 三种状态分开：没跑过 / 跑完了没输出 / 有输出。
    // （`log_text(None, ..)` 会显示"正在加载…"，那在这儿是错的。）
    let output: AnyElement = if db.sql_ran || db.sql_output.is_some() {
        log_text(db.sql_output.as_deref(), "db-sql-output")
    } else {
        div()
            .text_sm()
            .text_color(theme::text_dim())
            .child("还没有执行任何语句")
            .into_any_element()
    };

    // 这一条是**这次执行会落在哪个库**。从数据库列表行上的「SQL」按钮进来时
    // 就是那一行；不指定的话 `SELECT * FROM t` 会报 "No database selected"
    // （实测过），所以这件事必须显式画出来，而不是让用户从报错里猜。
    let target_db: AnyElement = match db.sql_database.as_deref() {
        Some(name) => {
            let clear = {
                let entity = entity.clone();
                Button::new("db-sql-clear-db")
                    .label("改为不指定")
                    .small()
                    .on_click(move |_, _, cx| {
                        entity.update(cx, |shell, cx| shell.clear_sql_database(cx));
                    })
            };
            h_flex()
                .w_full()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_dim())
                        .child("当前库"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text())
                        .child(name.to_owned()),
                )
                .child(clear)
                .into_any_element()
        }
        None => div()
            .text_xs()
            .text_color(theme::text_dim())
            .child("当前库：不指定（语句里要自己 USE <库>）")
            .into_any_element(),
    };

    v_flex()
        .w_full()
        .gap_3()
        .child(
            v_flex()
                .w_full()
                .gap_3()
                .p_4()
                .rounded_lg()
                .bg(theme::bg_card())
                .border_1()
                .border_color(theme::border())
                .child(target_db)
                .child(form_field(
                    "db-sql",
                    "语句（原样执行，面板不改写、也不解析）",
                    &form.sql,
                    cx,
                    true,
                ))
                .child(
                    h_flex()
                        .w_full()
                        .justify_between()
                        .items_center()
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::text_muted())
                                .child("SHOW / SELECT / DDL 都能跑；输出是 mysql 客户端的表格形态。"),
                        )
                        .child(run),
                ),
        )
        .child(echo)
        .child(output)
        .into_any_element()
}

/// 「数据库管理」页的动作弹窗（新建库 / 新建用户 / 改密码 / 授权）。
///
/// 四种动作共用一个弹窗：同时只可能开一个，而且它们共用"用户名 / 主机 / 密码 /
/// 库名"那几格。哪种动作决定**画哪几格**，也决定提交时拼哪条 SQL
/// （拼装在 `app.rs::submit_db_dialog` 里，用的是 `wslc-core` 的纯函数）。
///
/// ⚠️ 提交前的校验（标识符白名单）在 `submit_db_dialog` 里：不合法就**不关弹窗**，
/// 只给一条提示 —— 用户就在那一格上，改一个字比重打一遍强。
pub fn db_dialog_overlay(shell: &Shell, entity: &Entity<Shell>, cx: &App) -> AnyElement {
    let Some(dialog) = shell.db_dialog.as_ref() else {
        return div().into_any_element();
    };

    let kind = dialog.kind.clone();
    let locked_user = matches!(kind, DbDialogKind::AlterPassword { .. } | DbDialogKind::Grant { .. });

    // 按动作画不同的字段
    let mut fields: Vec<AnyElement> = Vec::new();
    match &kind {
        DbDialogKind::NewDatabase => {
            fields.push(form_field("db-dialog-name", "库名", &dialog.database, cx, true));
            fields.push(
                h_flex()
                    .w_full()
                    .gap_3()
                    .child(form_field("db-dialog-charset", "字符集（可留空）", &dialog.charset, cx, false))
                    .child(form_field(
                        "db-dialog-collation",
                        "排序规则（可留空）",
                        &dialog.collation,
                        cx,
                        false,
                    ))
                    .into_any_element(),
            );

            // 「同时创建用户」是 v0.8 的主路径：建库的人几乎总要一个连它的账号。
            // ⚠️ v0.9.3 起**用户名由用户手填**（以前是按库名推的 `<库名>_user`，
            // 弹窗里只画一行"跟库名走"）—— 实机反馈"创建数据库的创建用户名
            // 这里让手填，不要自动生成"。密码仍然预填随机值（那个是真猜不出来）。
            if dialog.create_user {
                fields.push(
                    h_flex()
                        .w_full()
                        .gap_3()
                        .items_center()
                        .child(
                            Checkbox::new("db-dialog-create-user")
                                .label("同时创建用户并授权")
                                .checked(dialog.create_user)
                                // `Checkbox` 是**受控**组件：`on_change` 给的是
                                // "请求的新值"，存回 Shell 再 notify（和 wslconf 同一写法）
                                .on_change({
                                    let entity = entity.clone();
                                    move |checked: &bool, _, cx| {
                                        let create_user = *checked;
                                        entity.update(cx, |shell, cx| {
                                            shell.set_db_create_user(create_user, cx)
                                        });
                                    }
                                }),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::text_dim())
                                .child("主机 %（允许远程连接）"),
                        )
                        .into_any_element(),
                );
                // 用户名输入框 + 一个「用建议值」。
                // 按钮**只填一次**、不联动：用户点了之后自己改库名也不会被覆盖
                //（"不要自动生成"那条反馈要的就是这个）。
                fields.push(form_field(
                    "db-dialog-db-user",
                    "数据库用户名",
                    &dialog.db_user,
                    cx,
                    false,
                ));
                let suggest = {
                    let entity = entity.clone();
                    Button::new("db-dialog-suggest-user")
                        .label("用建议值")
                        .small()
                        .on_click(move |_, window, cx| {
                            entity.update(cx, |shell, cx| shell.fill_db_username(window, cx));
                        })
                };
                // 建议值本身显示在提示里，让用户知道点下去会填什么
                //（按钮标签是静态的 —— `.label()` 在本仓库一律收 `&'static str`）。
                let suggested =
                    mysql_model::default_user_for(dialog.database.read(cx).value().trim());
                let hint = if suggested == "_user" {
                    // 库名还没填：算出来是 `_user`，别把这种半成品显示给用户
                    "要手填。填好库名后点「用建议值」会自动算成「库名_user」".to_owned()
                } else {
                    format!("要手填。「用建议值」会填：{suggested}")
                };
                fields.push(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .items_center()
                        .child(suggest)
                        .child(div().text_xs().text_color(theme::text_dim()).child(hint))
                        .into_any_element(),
                );
                fields.push(form_field(
                    "db-dialog-password",
                    "该用户的密码（已生成随机值，可改）",
                    &dialog.password,
                    cx,
                    true,
                ));
            }
        }
        DbDialogKind::NewUser => {
            fields.push(
                h_flex()
                    .w_full()
                    .gap_3()
                    .child(form_field("db-dialog-user", "用户名", &dialog.user, cx, false))
                    .child(form_field("db-dialog-host", "主机", &dialog.host, cx, false))
                    .into_any_element(),
            );
            fields.push(form_field("db-dialog-password", "密码", &dialog.password, cx, true));
            fields.push(form_field(
                "db-dialog-grant-db",
                "授权到哪个库（可留空）",
                &dialog.grant_db,
                cx,
                true,
            ));
            fields.push(grant_kind_row(dialog.grant, entity));
        }
        DbDialogKind::AlterPassword { user, host } => {
            fields.push(
                div()
                    .text_xs()
                    .text_color(theme::text_dim())
                    .child(format!("账号：'{user}'@'{host}'（用户名和主机不能在这里改）"))
                    .into_any_element(),
            );
            fields.push(form_field("db-dialog-password", "新密码", &dialog.password, cx, true));
        }
        DbDialogKind::Grant { user, host } => {
            fields.push(
                div()
                    .text_xs()
                    .text_color(theme::text_dim())
                    .child(format!("账号：'{user}'@'{host}'"))
                    .into_any_element(),
            );
            fields.push(form_field(
                "db-dialog-grant-db",
                "授权到哪个库",
                &dialog.grant_db,
                cx,
                true,
            ));
            fields.push(grant_kind_row(dialog.grant, entity));
        }
        DbDialogKind::ImportSql { database } => {
            // 目标库**写死**（就是数据库列表上被点的那一行）：不给输入框，
            // 也就没有"填错库"这件事。要导到别的库，去点那一行的按钮。
            fields.push(
                div()
                    .text_xs()
                    .text_color(theme::text_dim())
                    .child(format!("导入到：{database}"))
                    .into_any_element(),
            );
            // 文件那一格 + 「选择文件…」按钮并排：路径可以直接粘（用户常从
            // 别处拷过来），也可以点按钮挑 —— 两条路都通。
            let pick = {
                let entity = entity.clone();
                Button::new("db-import-pick")
                    .label("选择文件…")
                    .small()
                    .on_click(move |_, window, cx| {
                        entity.update(cx, |shell, cx| shell.pick_import_file(window, cx));
                    })
            };
            fields.push(
                h_flex()
                    .w_full()
                    .gap_2()
                    // ⚠️ 用 `items_center` 而不是 `items_end`：后者在本仓库
                    // 没有先例（`items_center` 到处都是），而本机**没有 Rust
                    // 工具链**、只能靠 CI 验证 —— 不值得为一点对齐赌一轮 15 分钟。
                    .items_center()
                    .child(div().flex_1().child(form_field(
                        "db-import-path",
                        "本机的 .sql 文件",
                        &dialog.import_path,
                        cx,
                        false,
                    )))
                    .child(pick)
                    .into_any_element(),
            );
            fields.push(
                div()
                    .text_xs()
                    .text_color(theme::danger())
                    .child(
                        "文件里的语句会被原样执行 —— 它可能建表、改数据，\
                         甚至删掉同名的表。导入前请确认这份文件是你想要的那份。",
                    )
                    .into_any_element(),
            );
        }
    }

    // 用户名 / 主机在"改密码 / 授权"里是**锁定**的：那样画出来才看得出
    // 这条动作作用在谁身上（而不是让用户以为还能改）。
    if locked_user {
        fields.insert(
            0,
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child("下面这条语句作用在上面那个账号上。")
                .into_any_element(),
        );
    }

    let cancel = {
        let entity = entity.clone();
        Button::new("db-dialog-cancel")
            .label("取消")
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.close_db_dialog(cx));
            })
    };
    let confirm = {
        let entity = entity.clone();
        // 导入不是一条 SQL（它是"送个文件进去让 mysql 读"），走自己的提交路径；
        // 其余四种都是"拼一条 SQL 然后执行"。
        let is_import = kind.is_import();
        Button::new("db-dialog-confirm")
            .label(kind.confirm_label())
            .small()
            .primary()
            .on_click(move |_, window, cx| {
                entity.update(cx, |shell, cx| {
                    if is_import {
                        shell.start_db_import(window, cx);
                    } else {
                        shell.submit_db_dialog(window, cx);
                    }
                });
            })
    };

    modal_scrim(
        "db-dialog",
        v_flex()
            .w(px(680.))
            .gap_4()
            .p_5()
            .rounded_lg()
            .bg(theme::bg_card())
            .border_1()
            .border_color(theme::border())
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_lg()
                            .font_bold()
                            .text_color(theme::text())
                            .child(kind.title()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::text_dim())
                            .child(format!(
                                "在容器 {} 上执行 —— 用的是数据库页连接卡里那个账号。",
                                shell
                                    .state
                                    .db
                                    .as_ref()
                                    .map(|db| db.container.clone())
                                    .unwrap_or_default()
                            )),
                    ),
            )
            .children(fields)
            .child(
                h_flex()
                    .w_full()
                    .justify_end()
                    .gap_2()
                    .child(cancel)
                    .child(confirm),
            ),
    )
    .into_any_element()
}

/// 授权级别那一排按钮（选中态用 `primary` 配色，和别处一致）。
fn grant_kind_row(current: GrantKind, entity: &Entity<Shell>) -> AnyElement {
    let buttons: Vec<AnyElement> = GrantKind::ALL
        .iter()
        .map(|kind| {
            let kind = *kind;
            let entity = entity.clone();
            let mut button = Button::new(SharedString::from(format!("db-grant-{kind:?}")))
                .label(kind.label())
                .small()
                .on_click(move |_, _, cx| {
                    entity.update(cx, |shell, cx| shell.set_db_grant(kind, cx));
                });
            if current == kind {
                button = button.primary();
            }
            button.into_any_element()
        })
        .collect();

    h_flex()
        .w_full()
        .gap_2()
        .items_center()
        .child(
            div()
                .text_xs()
                .text_color(theme::text_dim())
                .child("权限"),
        )
        .children(buttons)
        .into_any_element()
}

/// 备份进度浮层（备份跑着的时候盖在页面上）。
///
/// 没在备份时返回空 —— 进度不是常驻的（跑完就关）。
/// ⚠️ 这一版**没有实时进度**（dump 输出直接落在容器里，见 `cmd::mysql`
/// 的模块注释），所以只有一句"备份中…"和目标文件路径。
pub fn db_backup_progress(db: &DbState, entity: &Entity<Shell>) -> AnyElement {
    let Some(state) = db.backup.as_ref() else {
        return div().into_any_element();
    };

    let cancel = {
        let entity = entity.clone();
        Button::new("db-backup-cancel")
            .label("知道了")
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.cancel_db_backup(cx));
            })
    };

    modal_scrim(
        "db-backup-progress",
        v_flex()
            .w(px(560.))
            .gap_4()
            .p_5()
            .rounded_lg()
            .bg(theme::bg_card())
            .border_1()
            .border_color(theme::border())
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_lg()
                            .font_bold()
                            .text_color(theme::text())
                            .child(format!("正在备份 {}", state.database)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::text_dim())
                            .child(format!("目标文件：{}", state.windows_path.display())),
                    ),
            )
            .child(
                div().text_sm().text_color(theme::text_muted()).child(
                    "在容器里执行 mysqldump，一般几十秒内结束（大库会久一些）。\
                     完成后会在 Windows 侧存一份档案，容器内 /tmp 也留一份用于恢复。",
                ),
            )
            .child(
                h_flex()
                    .w_full()
                    .justify_end()
                    .gap_2()
                    .child(div().text_sm().text_color(theme::text_muted()).child("备份中…"))
                    .child(cancel),
            ),
    )
    .into_any_element()
}

/// 某个库的**备份列表**浮层（v0.8）。
///
/// 列表是 **Windows 侧**目录扫出来的（`AppState::rescan_backups`）——
/// 这也正是**恢复的输入**：点「恢复」时会把这份文件 cp 进容器再重定向进
/// `mysql`（v0.8.2 起不再依赖容器内 `/tmp` 里那份旧的，所以容器重建过、
/// `/tmp` 被清过都照样能恢复）。因此这里不需要（也无法）预先画
/// "容器里那份还在不在"—— 只认列表里这个文件在不在。
/// 导入进度浮层（导入跑着的时候盖在页面上）。
///
/// 没在导入时返回空。和备份那个一样：**没有实时百分比**（`container cp` 和
/// `mysql` 都不吐进度），但"点下去之后界面上有变化"这条必须有
/// （`AGENTS.md` §7.1）—— 所以起码要显示"在导哪个文件、进哪个库"。
pub fn db_import_progress(db: &DbState, _entity: &Entity<Shell>) -> AnyElement {
    let Some(state) = db.import.as_ref() else {
        return div().into_any_element();
    };

    let target = state
        .database
        .as_deref()
        .map(|d| format!("导入到 {d}"))
        .unwrap_or_else(|| "导入到文件里指定的库".to_owned());

    modal_scrim(
        "db-import-progress",
        v_flex()
            .w(px(560.))
            .gap_4()
            .p_5()
            .rounded_lg()
            .bg(theme::bg_card())
            .border_1()
            .border_color(theme::border())
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_lg()
                            .font_bold()
                            .text_color(theme::text())
                            .child(format!("正在导入 {}", state.file_name)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::text_dim())
                            .child(target),
                    ),
            )
            .child(
                div().text_sm().text_color(theme::text_muted()).child(
                    "先把文件送进容器，再让容器里的 mysql 读它。\
                     大文件会慢一些 —— 完成后这里会自动关掉，列表也会重读一次。",
                ),
            )
            .child(
                h_flex()
                    .w_full()
                    .justify_end()
                    .child(div().text_sm().text_color(theme::text_muted()).child("导入中…")),
            ),
    )
    .into_any_element()
}

pub fn db_backup_dialog_overlay(
    db: &DbState,
    form: &DbPageForm,
    entity: &Entity<Shell>,
    cx: &App,
) -> AnyElement {
    let Some(database) = db.backup_dialog.as_ref() else {
        return div().into_any_element();
    };
    let database = database.clone();

    // 「立即备份」——**这个浮层唯一的创建入口**。
    //
    // ⚠️ 它曾经不存在：`Shell::start_db_backup` 从 v0.8 写出来就**没有任何调用点**
    // （全仓库只此一份定义），而列表行上那个「备份」按钮打开的是**本浮层**，
    // 浮层里只有恢复 / 打开位置 / 删除。于是"备份"这个功能整个是死的：
    // 弹窗永远写着"还没有备份"，而且永远不会有第一份 —— 实机报过来就是这个
    // （"怎么没有一个按钮点击后可以把这个库直接整个备份导出来"）。
    // 入口只此一处：行上的「备份」是进这个浮层，创建在这里（同一动作不给两个入口）。
    let start: AnyElement = if db.backup.is_some() {
        // 跑着的时候把按钮换成这一句（项目里不用 `.disabled()`）
        div()
            .text_sm()
            .text_color(theme::text_muted())
            .child("备份中…")
            .into_any_element()
    } else {
        overlay_button(
            "db-backup-start",
            "立即备份",
            {
                let database = database.clone();
                move |shell, window, cx| shell.start_db_backup(database.clone(), window, cx)
            },
            entity,
        )
    };

    // 备份用的是**连接卡**上的账号（`start_db_backup` → `read_db_form`），
    // 而这张卡藏在页头的「基础信息」后面 —— 不写出来，用户拿到
    // "备份失败：Access denied for user 'root'@'localhost'" 时无从下手
    //（面板建的库有独立账号，root 往往不是它）。所以这里把**当前会用的账号**
    // 显示出来：和 `open_db_console_for` 一样，卡上是什么就说什么。
    let account: AnyElement = {
        let user = form.user.read(cx).value().trim().to_owned();
        let user = if user.is_empty() {
            "root".to_owned()
        } else {
            user
        };
        let text = if form.password.read(cx).value().trim().is_empty() {
            format!(
                "备份用账号 {user}（密码那一格现在是空的 —— 先去页头「基础信息」\
                 补上，否则 mysqldump 会报 Access denied）。"
            )
        } else {
            format!("备份用账号 {user}（要换账号去页头「基础信息」）。")
        };
        div()
            .text_xs()
            .text_color(theme::text_dim())
            .child(text)
            .into_any_element()
    };

    let rows: Vec<AnyElement> = db
        .backups
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.database == database)
        .map(|(index, entry)| {
            let restore = Button::new(SharedString::from(format!("db-restore-{index}")))
                .label("恢复")
                .small()
                .primary()
                .on_click({
                    let entity = entity.clone();
                    move |_, _, cx| {
                        entity.update(cx, |shell, cx| shell.restore_db_backup(index, cx));
                    }
                })
                .into_any_element();
            let open_dir = overlay_button(
                &format!("db-open-backup-{index}"),
                "打开位置",
                {
                    let dir = entry.path.parent().map(|p| p.to_path_buf());
                    move |shell, _, cx| shell.reveal_backup_dir(dir.clone(), cx)
                },
                entity,
            );
            let delete = {
                let entity = entity.clone();
                Button::new(SharedString::from(format!("db-delete-backup-{index}")))
                    .label("删除")
                    .small()
                    .on_click(move |_, _, cx| {
                        entity.update(cx, |shell, cx| shell.delete_db_backup(index, cx));
                    })
                    .into_any_element()
            };
            table_row(
                BACKUP_COLUMNS,
                vec![
                    cell_text_full(entry.file_name.clone(), entry.file_name.clone()),
                    cell_muted(format_bytes(entry.bytes)),
                    cell_muted(entry.stamp.clone()),
                    h_flex()
                        .gap_2()
                        .child(restore)
                        .child(open_dir)
                        .child(delete)
                        .into_any_element(),
                ],
            )
            .into_any_element()
        })
        .collect();

    let close = {
        let entity = entity.clone();
        Button::new("db-backup-close")
            .label("关闭")
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.close_db_backup_dialog(cx));
            })
    };

    modal_scrim(
        "db-backup-dialog",
        v_flex()
            .id("db-backup-dialog-card")
            .w(px(780.))
            .max_h(px(560.))
            .gap_4()
            .p_5()
            .rounded_lg()
            .bg(theme::bg_card())
            .border_1()
            .border_color(theme::border())
            .overflow_y_scroll()
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        h_flex()
                            .w_full()
                            .justify_between()
                            .items_center()
                            .child(
                                div()
                                    .text_lg()
                                    .font_bold()
                                    .text_color(theme::text())
                                    .child(format!("备份 —— {database}")),
                            )
                            .child(start),
                    )
                    .child(account)
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::text_dim())
                            .child(
                                crate::util::strip_md(
                                    "列表是 Windows 侧的存档（容器删了它们还在），\
                                     也是**恢复的输入**：恢复时会把这份文件送进容器再导入，\
                                     所以容器重建过也照样能恢复。\
                                     恢复**覆盖**这个库现有的全部数据。",
                                ),
                            ),
                    ),
            )
            .child(if rows.is_empty() {
                empty_state("还没有备份 —— 点右上角的「立即备份」生成第一份")
                    .into_any_element()
            } else {
                v_flex()
                    .w_full()
                    .rounded_lg()
                    .bg(theme::bg())
                    .border_1()
                    .border_color(theme::border())
                    .overflow_hidden()
                    .child(table_header(BACKUP_COLUMNS))
                    .children(rows)
                    .into_any_element()
            })
            .child(h_flex().w_full().justify_end().child(close)),
    )
    .into_any_element()
}

// ---------------------------------------------------------------------------
// ②c 容器里的应用部署管理（nginx / Java / Node.js / Go，v0.9）
// ---------------------------------------------------------------------------

/// 「部署管理」页。
///
/// # 它为什么不在侧边栏
///
/// 和「数据库管理」同一个取舍：它服务**一个**容器（`AppState::deploy` 里记着是
/// 哪个），只能从容器列表行上的「部署管理」按钮进，所以顶部有「← 返回容器」。
///
/// # 三种形态（和 `database` 一一对应）
///
/// 1. **探测就失败**（不是这几类容器 / 容器没运行）：只画那张说明卡片。
///    这时候再摆一张空表只会干扰判断；
/// 2. **正常**：页头 + 「添加部署」+ 已部署列表；
/// 3. **还没读到数据**：表格的位置放一句"正在读取…"。
///
/// # 和数据库页最大的不同
///
/// 这一页的数据有**两个来源**，界面要如实区分：
/// 「部署过什么」来自**本地面板**的存档（所以容器停着也看得到），
/// 「现在跑没跑 / 目录里还有没有」**问容器**（那是事实）。
/// 所以每一行的状态列可能是"读取中…"——那不是说没有，是说还没问回来。
///
/// ⚠️ 这一页**不需要 `cx`**（所有动作都交给 `entity` 上的 `Shell` 方法去做），
/// 但签名和隔壁 `database` / `containers` 保持同一个形状，`views::page()` 那边
/// 就不用为它单写一个分支。用 `_cx` 把"这里故意不用"写出来。
pub fn deploy_page(shell: &Shell, _cx: &App, entity: &Entity<Shell>) -> AnyElement {
    let Some(deploy) = shell.state.deploy.as_ref() else {
        // 理论上来不到（只可能从容器行上的按钮进，那里已经建好状态了），
        // 但真到了这里就说一句人话，别画一片空白。
        return empty_state("还没有选中容器 —— 请到「容器」页点某一行的「部署管理」")
            .into_any_element();
    };

    let header = deploy_header(deploy, entity);

    if let Some(blocked) = deploy.blocked.as_ref() {
        return v_flex()
            .w_full()
            .gap_3()
            .child(header)
            .child(deploy_blocked_card(deploy, blocked))
            .into_any_element();
    }

    let body: AnyElement = if deploy.loaded {
        deploy_table(deploy, entity)
    } else {
        deploy_loading(deploy.busy)
    };

    v_flex()
        .w_full()
        .gap_3()
        .child(header)
        // 存档读坏了要说出来 —— 不然用户会以为"我部署过的东西全没了"。
        // ⚠️ `AppState::deploy_store_error` 存在的唯一目的就是这一行；
        // 对比 `db_credentials_error`：那个算出来了却全仓库没人读。
        .children(
            shell
                .state
                .deploy_store_error
                .as_ref()
                .map(|error| deploy_error_banner(&format!("部署存档读不出来：{error}"))),
        )
        .children(
            deploy
                .error
                .as_ref()
                .map(|failure| deploy_failure_banner(failure)),
        )
        .child(body)
        .into_any_element()
}

/// 页头：容器 / 镜像 / 运行时 + 依据 + 「← 返回容器」「重新读取」「添加部署…」。
fn deploy_header(deploy: &DeployState, entity: &Entity<Shell>) -> AnyElement {
    let back = {
        let entity = entity.clone();
        Button::new("deploy-back")
            .label("← 返回容器")
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.close_deploy_page(cx));
            })
    };

    // 读取期间把按钮**换掉**（项目里不用 `.disabled()`）：置灰的按钮会让人
    // 反复去点，而"读取中…"说清了现在在等什么。
    let reload: AnyElement = if deploy.busy {
        div()
            .text_sm()
            .text_color(theme::text_muted())
            .child("读取中…")
            .into_any_element()
    } else {
        let entity = entity.clone();
        Button::new("deploy-reload")
            .label("重新读取")
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.load_deploy(cx));
            })
            .into_any_element()
    };

    // 「添加部署…」——弹出一次性的表单（`InputState::new` 要 `&mut Window`，
    // 所以只能在点击这一刻建）。
    let add: AnyElement = if deploy.progress.is_some() {
        let label = deploy
            .progress
            .as_ref()
            .map(|progress| progress.phase.label())
            .unwrap_or("正在…");
        div()
            .text_sm()
            .text_color(theme::text_muted())
            .child(label)
            .into_any_element()
    } else {
        let entity = entity.clone();
        Button::new("deploy-add")
            .label("添加部署…")
            .small()
            .primary()
            .on_click(move |_, window, cx| {
                entity.update(cx, |shell, cx| shell.open_deploy_dialog(None, window, cx));
            })
            .into_any_element()
    };

    // nginx 的「重载」放在页头而不是行内：它作用于**容器里那个 nginx 进程**，
    // 不属于任何一条部署（清单里可能有多个站点，重载一次全生效）。
    let reload_nginx: AnyElement = if deploy.runtime == wslc_core::model::AppRuntime::Nginx {
        let entity = entity.clone();
        Button::new("deploy-reload-nginx")
            .label("重载 nginx")
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.reload_nginx_in_container(cx));
            })
            .into_any_element()
    } else {
        div().into_any_element()
    };

    h_flex()
        .w_full()
        .justify_between()
        .items_center()
        .child(
            v_flex()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .font_semibold()
                        .child(format!("容器 {}", deploy.container)),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_dim())
                        .child(format!(
                            "{} · {} · {}",
                            deploy.runtime.label(),
                            deploy.image,
                            deploy.signal.label(),
                        )),
                ),
        )
        .child(
            h_flex()
                .gap_2()
                .child(back)
                .child(reload_nginx)
                .child(reload)
                .child(add),
        )
        .into_any_element()
}

/// 探测就失败时的说明卡（不是这几类容器 / 容器没运行）。
fn deploy_blocked_card(deploy: &DeployState, failure: &crate::state::DeployFailure) -> AnyElement {
    let note = if failure.kind == wslc_core::model::DeployErrorKind::ContainerNotRunning {
        "这一页的每一个动作都要进容器里执行（准备目录 → 传文件 → 起进程），\
         所以容器得先跑起来。「启动」就在容器列表那一行上。"
    } else {
        // ⚠️ 「终端」v0.9.14 起在列表行的「更多」菜单里 —— 这句话要跟着改，
        // 否则用户去行内找那个按钮会找不到（`AGENTS.md` §5：过期注释/文案
        // 会让后来的人照着它继续错）。
        "「部署应用」服务的是 nginx / Java / Node.js / Go 这几类容器 —— \
         面板按镜像引用、环境变量、启动命令三样认人，一个线索都没有时不会去猜。\
         想自己进去看，用「容器」页那一行「更多」菜单里的「终端」。"
    };

    v_flex()
        .w_full()
        .gap_3()
        .p_4()
        .rounded_lg()
        .bg(theme::danger_soft())
        .border_1()
        .border_color(theme::danger_edge())
        .child(
            div()
                .text_sm()
                .font_semibold()
                .text_color(theme::danger())
                .child(format!("这个容器现在部署不了（{}）", failure.kind.label())),
        )
        .child(
            div()
                .text_sm()
                .text_color(theme::text())
                .child(crate::util::strip_md(&failure.summary())),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child(note),
        )
        .child(log_text(Some(&failure.detail), "deploy-blocked-detail"))
        .child(
            div()
                .text_xs()
                .text_color(theme::text_dim())
                .child(format!("刚才是在读容器 {} 的时候失败的。", deploy.container)),
        )
        .into_any_element()
}

/// 上一次失败：一句"该怎么办" +（可滚动的）原文。
fn deploy_failure_banner(failure: &crate::state::DeployFailure) -> AnyElement {
    v_flex()
        .w_full()
        .gap_2()
        .p_3()
        .rounded_lg()
        .bg(theme::danger_soft())
        .border_1()
        .border_color(theme::danger_edge())
        .child(
            div()
                .text_sm()
                .font_semibold()
                .text_color(theme::danger())
                .child(crate::util::strip_md(&failure.summary())),
        )
        .child(log_text(Some(&failure.detail), "deploy-error-detail"))
        .into_any_element()
}

/// 存档读坏了的提示条（一行字，不折叠）。
fn deploy_error_banner(text: &str) -> AnyElement {
    div()
        .w_full()
        .p_3()
        .rounded_lg()
        .bg(theme::danger_soft())
        .border_1()
        .border_color(theme::danger_edge())
        .child(
            div()
                .text_sm()
                .text_color(theme::danger())
                .child(text.to_owned()),
        )
        .into_any_element()
}

/// 「正在读取…」占位（和 `db_loading` 同一形状）。
fn deploy_loading(busy: bool) -> AnyElement {
    div()
        .w_full()
        .p_6()
        .rounded_lg()
        .bg(theme::bg_card())
        .border_1()
        .border_color(theme::border())
        .child(
            div()
                .text_sm()
                .text_color(theme::text_muted())
                .child(if busy {
                    "正在读取…（先看容器认不认得出，再问每一条现在跑没跑）"
                } else {
                    "还没读到数据 —— 点右上角「重新读取」"
                }),
        )
        .into_any_element()
}

/// 已部署列表。
fn deploy_table(deploy: &DeployState, entity: &Entity<Shell>) -> AnyElement {
    let rows: Vec<AnyElement> = deploy
        .entries
        .iter()
        .map(|entry| {
            table_row(
                DEPLOY_COLUMNS,
                vec![
                    cell_text(entry.record.app.clone()),
                    cell_text(
                        entry
                            .runtime
                            .map(|runtime| runtime.kind().label())
                            .unwrap_or("—"),
                    ),
                    // 目标路径是这一行最要紧的一格（部署铺到了哪儿、
                    // 删除会删掉什么），而且容器路径经常很长 ——
                    // 截断了也要能悬停看全文。
                    cell_muted_full(entry.record.app.clone(), entry.record.target.clone()),
                    deploy_status_cell(entry, deploy),
                    deploy_port_cell(entry, deploy),
                    deploy_row_actions(entry, deploy, entity),
                ],
            )
            .into_any_element()
        })
        .collect();

    let body: AnyElement = if deploy.entries.is_empty() {
        v_flex()
            .w_full()
            .gap_2()
            .p_6()
            .items_center()
            .child(
                div()
                    .text_sm()
                    .text_color(theme::text_muted())
                    .child("这个容器上还没有部署过任何应用"),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme::text_dim())
                    // `note()` 的文案里写着 Markdown（`**静态文件**` / 反引号），
                    // 而这里是纯文本 → 必须过 `strip_md`（见 `util::strip_md`：
                    // "与其在每条文案里手工删标记，不如在渲染处统一清掉"）。
                    .child(crate::util::strip_md(deploy.runtime.note())),
            )
            .into_any_element()
    } else {
        v_flex().w_full().children(rows).into_any_element()
    };

    v_flex()
        .w_full()
        .gap_2()
        .child(
            div()
                .text_xs()
                .text_color(theme::text_dim())
                .child(crate::util::strip_md(&format!(
                    "共 {} 条部署。「部署过什么」来自面板自己的存档（容器停着也看得到）；\
                     「现在跑没跑」是**问容器**得来的。",
                    deploy.entries.len()
                ))),
        )
        .child(
            v_flex()
                .w_full()
                .rounded_lg()
                .bg(theme::bg_card())
                .border_1()
                .border_color(theme::border())
                .overflow_hidden()
                .child(table_header(DEPLOY_COLUMNS))
                .child(body),
        )
        .into_any_element()
}

/// 状态那一格。
///
/// 「读不到」和「目标不存在」是**两件事**，所以分开画：前者是"问容器的时候
/// 出错了"（可能是容器刚停、网络抖动），后者是"真的没有了"。
/// 把前者显示成后者会让人以为文件丢了。
fn deploy_status_cell(entry: &DeployEntry, deploy: &DeployState) -> AnyElement {
    if let Some(error) = entry.probe_error.as_ref() {
        // 用 `cell_muted_full` 承载 tooltip：它拿组件库的 `Button` 当壳，
        // 所以截断的文字能悬停看全文（`AGENTS.md` §7.5）。
        return cell_muted_full(
            format!("deploy-status-{}-{}", deploy.container, entry.record.app),
            format!("读不到状态（{}）", crate::util::strip_md(error)),
        );
    }
    let mut extra = String::new();
    if entry.probed {
        match entry.files {
            Some(count) => extra = format!(" · {count} 个文件"),
            None => extra = " · 目标目录不在".to_owned(),
        }
    }
    let label = entry.status_label();
    div()
        .text_sm()
        .text_color(if entry.process == Some(wslc_core::model::ProcessStatus::Running) {
            theme::text()
        } else {
            theme::text_muted()
        })
        .child(format!("{label}{extra}"))
        .into_any_element()
}

/// 端口那一格：只**报告**已经发布出去的映射。
///
/// ⚠️ 已存在的容器改不了端口发布（`-p` 只在 `run` / `create` 时给），
/// 所以这里绝不画一个点了没用的「发布」按钮 —— 而是把下一步说清楚。
fn deploy_port_cell(entry: &DeployEntry, deploy: &DeployState) -> AnyElement {
    let port = entry.record.port;
    if port == 0 {
        return cell_text("—");
    }
    // 判据只有一份：`model::deploy::port_is_published`（纯函数、有单测）。
    // 早先这里是**内联复制**的一段，而真正被测的那份（`DeployProbe::publishes`）
    // 全仓库没有一个调用点 —— "测的不是跑的那份"（v0.9.13 改）。
    let published = deploy_model::port_is_published(&deploy.ports, port);
    if published {
        let key = format!("deploy-port-{}-{}", deploy.container, entry.record.app);
        let is_nginx = entry.runtime == Some(wslc_core::model::AppRuntime::Nginx);
        // 站点关的 nginx 由**默认站点**提供，路径不是 `/`（`/<app>/` 或用户自己
        // 填的相对目录）。这一格不猜路径，只把"已发布 + 站点关"说清楚；
        // 站点开 / 进程型才有独立 URL。
        //
        // ⚠️ 不拼 URL 的具体理由：默认站点的 path 是 `<target 相对 Web 根>`，
        // 不一定等于 `<app>`；老记录还可能直接铺在 Web 根（path 就是 `/`）。
        if is_nginx && entry.record.site.is_none() {
            return cell_muted_full(
                key,
                format!("{port}（已发布；站点关，由默认站点提供）"),
            );
        }
        if let Some(url) = deploy_model::app_url(&deploy.ports, port) {
            return cell_text_full(key, format!("{url}（容器 {port}）"));
        }
        cell_text(format!("{port}（已发布）"))
    } else {
        cell_muted_full(
            format!("deploy-port-{}-{}", deploy.container, entry.record.app),
            format!(
                "{port}（没发布到主机 —— 端口只能在建容器时给，已建好的容器改不了，\
                 外部访问不了）"
            ),
        )
    }
}

/// 行内操作。
///
/// 按形态分岔 —— 这不是"多给一个入口"，而是**两类东西的动作本来就不同**：
/// 内容形态（nginx）没有属于我们的进程，画「启动 / 停止」只会点了报错。
/// 只画**当前有意义**的那几个（停止时画「启动」，运行时画「停止」）。
fn deploy_row_actions(
    entry: &DeployEntry,
    deploy: &DeployState,
    entity: &Entity<Shell>,
) -> AnyElement {
    let app = entry.record.app.clone();
    let running = entry.process == Some(wslc_core::model::ProcessStatus::Running);
    let busy = deploy.progress.as_ref().is_some_and(|p| p.app == app);
    let mut actions: Vec<AnyElement> = Vec::new();

    if busy {
        // 这一条正在跑一个动作 —— 把按钮换掉而不是留着（`.disabled()` 全项目不用）
        let label = deploy
            .progress
            .as_ref()
            .map(|p| p.phase.label())
            .unwrap_or("正在…");
        return div()
            .text_xs()
            .text_color(theme::text_muted())
            .child(label)
            .into_any_element();
    }

    // 类型认不出来：**不猜**该用哪套动作（见 `DeployEntry::runtime` 的说明），
    // 只给「删除记录」并说明为什么别的按钮没有。
    if entry.runtime.is_none() {
        actions.push(
            deploy_danger_button(
                &format!("deploy-forget-{app}"),
                "删除记录",
                {
                    let app = app.clone();
                    move |shell, cx| shell.forget_deployment_record(app.clone(), cx)
                },
                entity,
            ),
        );
        actions.push(
            div()
                .text_xs()
                .text_color(theme::text_dim())
                .child("存档里的类型认不出来，只能删掉这条记录")
                .into_any_element(),
        );
        return h_flex()
            .gap_2()
            .flex_wrap()
            .children(actions)
            .into_any_element();
    }

    let is_process = entry.is_process();

    // 启动 / 停止（只有进程形态有）
    if is_process {
        if running {
            actions.push(
                immediate_button(
                    &format!("deploy-stop-{app}"),
                    "停止",
                    ImmediateAction::StopDeployment(app.clone()),
                    entity,
                )
                .into_any_element(),
            );
        } else {
            actions.push(
                immediate_button(
                    &format!("deploy-start-{app}"),
                    "启动",
                    ImmediateAction::StartDeployment(app.clone()),
                    entity,
                )
                .into_any_element(),
            );
        }
    } else {
        // 内容形态：铺进去就生效，这里给的是"我改完本地文件想再传一次"
        actions.push(
            overlay_button(
                &format!("deploy-redeploy-{app}"),
                "重新上传",
                {
                    let app = app.clone();
                    move |shell, window, cx| {
                        shell.open_deploy_dialog(Some(app.clone()), window, cx)
                    }
                },
                entity,
            ),
        );
    }

    // 日志（只有进程形态有 —— 内容形态的产物是文件，没有我们起的进程）
    if is_process {
        actions.push(
            overlay_button(
                &format!("deploy-logs-{app}"),
                "日志",
                {
                    let app = app.clone();
                    move |shell, _, cx| shell.open_deploy_logs(app.clone(), cx)
                },
                entity,
            ),
        );
    }

    // 站点配置（只有 nginx 站点开的记录有 —— 站点关没有面板生成的 conf）
    if !is_process && entry.record.site.is_some() {
        actions.push(
            overlay_button(
                &format!("deploy-site-{app}"),
                "站点配置",
                {
                    let app = app.clone();
                    move |shell, _, cx| shell.open_deploy_site_conf(app.clone(), cx)
                },
                entity,
            ),
        );
    }

    // 进程形态才需要"重新部署"（内容形态上面那个就是它）
    if is_process {
        actions.push(
            overlay_button(
                &format!("deploy-redeploy-{app}"),
                "重新部署",
                {
                    let app = app.clone();
                    move |shell, window, cx| {
                        shell.open_deploy_dialog(Some(app.clone()), window, cx)
                    }
                },
                entity,
            ),
        );
    }

    actions.push(
        deploy_danger_button(
            &format!("deploy-remove-{app}"),
            "删除",
            {
                let app = app.clone();
                move |shell, cx| shell.request_remove_deployment(app.clone(), cx)
            },
            entity,
        ),
    );

    h_flex()
        .gap_2()
        .flex_wrap()
        .children(actions)
        .into_any_element()
}

/// 部署页的红色按钮：**直接**调一个 `Shell` 方法，不走 `PendingAction`。
///
/// 为什么不复用 [`danger_button`]：那一个把动作交给 `ConfirmAction::execute`，
/// 而它的形状是"跑一条 `wslc` 命令 → 返回字符串"（同步、单个子进程）。
/// 删一条部署要做三件事 —— 停进程、删掉面板铺进去的那些文件、**改本地存档并重读列表** ——
/// 最后那件它表达不了。所以删除确认是部署页自己的浮层
///（[`deploy_remove_confirm_overlay`]），按钮直接把动作交给 `Shell`。
fn deploy_danger_button(
    id: &str,
    label: &'static str,
    run: impl Fn(&mut Shell, &mut Context<Shell>) + 'static,
    entity: &Entity<Shell>,
) -> AnyElement {
    let entity = entity.clone();
    Button::new(SharedString::from(id.to_owned()))
        .label(label)
        .small()
        .danger()
        .on_click(move |_, _, cx| {
            entity.update(cx, |shell, cx| run(shell, cx));
        })
        .into_any_element()
}

/// 「添加部署 / 重新部署」弹窗。
///
/// # 为什么是弹窗而不是页面上的表单
///
/// 五个输入框，而 `InputState::new` 要 `&mut Window` —— 只能懒创建
/// （渲染时建会每帧重建输入框，**字都打不进去**，见 `AGENTS.md` §7.5）。
/// 和「拉取镜像」「新建数据库」同一条。
///
/// # 表单里的说明文字都是**必须的**
///
/// - 「目标目录里会被替换的是哪几个文件」：部署是"删掉我们铺过的名字再铺"，
///   用户有权在点之前知道（v0.9.10 起不再整目录清空 —— 见
///   `model::deploy::removals_script`）；
/// - 「启动命令会原样交给 /bin/sh -c」：它**不做**引号转义（刻意的，
///   见 `model::deploy::start_script`），不说清楚用户会以为它是参数。
pub fn deploy_dialog_overlay(
    shell: &Shell,
    entity: &Entity<Shell>,
    cx: &App,
) -> AnyElement {
    let Some(form) = shell.deploy_form.as_ref() else {
        return div().into_any_element();
    };
    let Some(deploy) = shell.state.deploy.as_ref() else {
        return div().into_any_element();
    };
    let runtime = deploy.runtime;
    let re_deploy = form.editing.is_some();

    let card = v_flex()
        .w(px(560.0))
        .gap_3()
        .p_4()
        // 站点块出现后弹窗会高出一截；带 id 的滚动容器（AGENTS §7.5）。
        .id("deploy-dialog-scroll")
        .max_h(px(760.0))
        .overflow_y_scroll()
        .rounded_lg()
        .bg(theme::bg_card())
        .border_1()
        .border_color(theme::border())
        .child(
            v_flex()
                .gap_1()
                .child(
                    div()
                        .text_lg()
                        .font_semibold()
                        .child(if re_deploy {
                            format!(
                                "重新部署 {}",
                                form.editing.clone().unwrap_or_default()
                            )
                        } else {
                            format!("添加部署 · {}", runtime.label())
                        }),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_dim())
                        .child(format!("目标容器 {}", deploy.container)),
                ),
        );

    // 重新部署时**不给**应用名的输入框，只把名字写出来 ——
    // 和「数据库管理」的「改密码 / 授权」锁定用户名 / 主机是同一个手法
    //（改了名字就是另一个部署了，那应该走「添加部署」）。
    let card = if re_deploy {
        card.child(kv_block(
            "应用名",
            form.editing.clone().unwrap_or_default(),
        ))
    } else {
        card.child(form_field("deploy-app", "应用名", &form.app, cx, true))
    };

    let card = card
        .child(
            h_flex()
                .w_full()
                .gap_2()
                .items_end()
                .child(form_field(
                    "deploy-source",
                    "本地来源",
                    &form.source,
                    cx,
                    false,
                ))
                .child({
                    let entity = entity.clone();
                    Button::new("deploy-browse")
                        .label("浏览…")
                        .small()
                        .on_click(move |_, window, cx| {
                            entity.update(cx, |shell, cx| {
                                shell.browse_deploy_source(window, cx)
                            });
                        })
                }),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme::text_dim())
                // ⚠️ 这一整块（以及下面几处说明）都要过 `strip_md`：
                // 文案是 Markdown 风格写的，而 `div` 画的是纯文本 ——
                // 不过一遍，屏幕上就真的显示 `**` 和反引号（v0.9.13 修）。
                .child(crate::util::strip_md(&format!(
                    "{} 部署的是一个**{}**。",
                    runtime.label(),
                    match runtime.source_kind() {
                        deploy_model::SourceKind::Directory => "目录（只打目录的**内容**）",
                        deploy_model::SourceKind::File => "文件",
                    }
                ))),
        )
        .child(form_field(
            "deploy-target",
            "容器内目标目录",
            &form.target,
            cx,
            true,
        ))
        .child(
            div()
                .text_xs()
                .text_color(theme::danger())
                .child(crate::util::strip_md(
                    "「部署」会**替换掉面板上次铺进去的那几个文件**（不多删别的：\
                     镜像自带的文件、你自己放的东西都留着），所以要填一个专门放这个应用的\
                     目录。目标目录只允许填在 /opt、/srv、/app、/var/www、\
                     /usr/share/nginx 这些地方，不能是系统目录。",
                )),
        );

    // 进程形态才有「启动命令」这一格；内容形态（nginx）铺完就生效。
    let card = if runtime.kind() == wslc_core::model::DeployKind::Process {
        card.child(form_field(
            "deploy-command",
            "启动命令",
            &form.command,
            cx,
            true,
        ))
        .child(
            div()
                .text_xs()
                .text_color(theme::text_dim())
                .child(crate::util::strip_md(
                    "这一行会**原样**交给容器里的 `/bin/sh -c`（不做转义）—— \
                     所以可以写管道、`&&`、环境变量。日志落在目标目录的 app.log。",
                )),
        )
    } else {
        card.child(
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child(
                    "内容形态没有「启动命令」—— nginx 每个请求都从磁盘读，铺完就生效。",
                ),
        )
    };

    // nginx 的站点块：勾上才会写 conf。放在端口**前面** —— 端口在站点开时
    // 就是 `listen`，先解释"要不要站点"再让用户填端口。
    let site_block: Option<AnyElement> = if runtime == wslc_core::model::AppRuntime::Nginx {
        Some(
            v_flex()
                .w_full()
                .gap_2()
                .p_3()
                .rounded_md()
                .bg(theme::bg())
                .border_1()
                .border_color(theme::border())
                .child(
                    h_flex()
                        .w_full()
                        .gap_3()
                        .items_center()
                        .child({
                            let entity = entity.clone();
                            Checkbox::new("deploy-site-enabled")
                                .label("生成站点配置（SPA / 反代）")
                                .checked(form.site_enabled)
                                .on_change(move |checked: &bool, _, cx| {
                                    let enabled = *checked;
                                    entity.update(cx, |shell, cx| {
                                        shell.set_deploy_site_enabled(enabled, cx)
                                    });
                                })
                        }),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_dim())
                        .child(crate::util::strip_md(
                            "站点开 = 面板写一份 `/etc/nginx/conf.d/00-wslc-panel-<应用名>.conf`，\
                             让这个应用 `listen <端口>`；端口 80 会接管默认站点。\
                             不勾 = 只铺文件，由默认站点在 `/<应用名>/` 提供。",
                        )),
                )
                .children(form.site_enabled.then(|| {
                    v_flex()
                        .w_full()
                        .gap_2()
                        .child({
                            let entity = entity.clone();
                            Checkbox::new("deploy-site-spa")
                                .label("单页应用回退（找不到的路径交给 index.html）")
                                .checked(form.site_spa)
                                .on_change(move |checked: &bool, _, cx| {
                                    let enabled = *checked;
                                    entity.update(cx, |shell, cx| {
                                        shell.set_deploy_site_spa(enabled, cx)
                                    });
                                })
                        })
                        .child(form_field(
                            "deploy-proxy-prefix",
                            "反代路径前缀（可选）",
                            &form.proxy_prefix,
                            cx,
                            true,
                        ))
                        .child(form_field(
                            "deploy-proxy-upstream",
                            "反代上游（可选）",
                            &form.proxy_upstream,
                            cx,
                            true,
                        ))
                        .child({
                            let entity = entity.clone();
                            Checkbox::new("deploy-proxy-strip")
                                .label("转发时去掉前缀（后端收到 /foo 而不是 /api/foo）")
                                .checked(form.proxy_strip)
                                .on_change(move |checked: &bool, _, cx| {
                                    let enabled = *checked;
                                    entity.update(cx, |shell, cx| {
                                        shell.set_deploy_proxy_strip(enabled, cx)
                                    });
                                })
                        })
                        .child({
                            let entity = entity.clone();
                            Checkbox::new("deploy-auto-connect")
                                .label("自动把两个容器接到同一个用户网络")
                                .checked(form.auto_connect)
                                .on_change(move |checked: &bool, _, cx| {
                                    let enabled = *checked;
                                    entity.update(cx, |shell, cx| {
                                        shell.set_deploy_auto_connect(enabled, cx)
                                    });
                                })
                        })
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::text_dim())
                                .child(crate::util::strip_md(
                                    "上游是另一个容器时，面板会先找两个容器的**共同用户网络**；\
                                     没有就创建/接到 `wslc-panel`（勾选「自动接通」时）。\
                                     取消勾选就只能手动 `wslc network connect <网络> <容器>`。\
                                     默认 bridge 解析不了名字（实测 `getent` rc=2）。\
                                     上游停着也能保存配置（不会让 `nginx -t` 失败），\
                                     请求会在它起来后恢复。",
                                )),
                        )
                        .into_any_element()
                }))
                .into_any_element(),
        )
    } else {
        None
    };

    let card = card
        .children(site_block)
        .child(form_field(
            "deploy-port",
            "端口（应用监听；站点开时是 listen）",
            &form.port,
            cx,
            true,
        ))
        .child(
            div()
                .text_xs()
                .text_color(theme::text_dim())
                .child(crate::util::strip_md(
                    "端口只用来**提示有没有发布到主机**：已建好的容器改不了端口发布\
                     （`-p` 只能在建容器时给）。",
                )),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme::text_dim())
                // 多应用的规矩要在**填的时候**说，不要等提交报错才说。
                .child(crate::util::strip_md(
                    "多应用：一个应用 = 一个名字 + 一个目录 + 一个端口。\
                     nginx 的默认目标已经改成 Web 根下的子目录（`<Web 根>/<应用名>`）；\
                     进程型应用要自己绑这一格填的端口（Java 用 `--server.port=…`，\
                     Node 读 `PORT`），面板不会替你改启动命令。",
                )),
        )
        .child(
            h_flex()
                .w_full()
                .justify_end()
                .gap_2()
                .child({
                    let entity = entity.clone();
                    Button::new("deploy-cancel")
                        .label("取消")
                        .small()
                        .on_click(move |_, _, cx| {
                            entity.update(cx, |shell, cx| shell.close_deploy_dialog(cx));
                        })
                })
                .child({
                    let entity = entity.clone();
                    Button::new("deploy-submit")
                        .label(if re_deploy { "重新部署" } else { "部署" })
                        .small()
                        .primary()
                        .on_click(move |_, window, cx| {
                            entity.update(cx, |shell, cx| shell.submit_deploy(window, cx));
                        })
                }),
        );

    modal_scrim("deploy-dialog-scrim", card).into_any_element()
}

/// 应用日志浮层（只读：一个输入框都没有，所以状态是纯数据）。
pub fn deploy_logs_overlay(logs: &DeployLogs, entity: &Entity<Shell>) -> AnyElement {
    let body: AnyElement = if logs.busy {
        div()
            .text_sm()
            .text_color(theme::text_muted())
            .child("正在读取…")
            .into_any_element()
    } else {
        log_text(logs.text.as_deref(), "deploy-logs-text")
    };

    let card = v_flex()
        .w(px(680.0))
        .max_h(px(520.0))
        .gap_3()
        .p_4()
        .rounded_lg()
        .bg(theme::bg_card())
        .border_1()
        .border_color(theme::border())
        .child(
            v_flex()
                .gap_1()
                .child(
                    div()
                        .text_lg()
                        .font_semibold()
                        .child(format!("日志 · {}", logs.app)),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_dim())
                        .child(crate::util::strip_md(
                            "是这个应用**自己**的输出（启动时把它重定向到了目标目录的 \
                             app.log）—— 不是容器主进程的日志，那个在「日志」按钮里。",
                        )),
                ),
        )
        .child(body)
        .child(h_flex().w_full().justify_end().child({
            let entity = entity.clone();
            Button::new("deploy-logs-close")
                .label("关闭")
                .small()
                .on_click(move |_, _, cx| {
                    entity.update(cx, |shell, cx| shell.close_deploy_logs(cx));
                })
        }));

    modal_scrim("deploy-logs-scrim", card).into_any_element()
}

/// 「站点配置」只读浮层。
///
/// 文本**不**过 `strip_md`、不逐行重排：conf 里的缩进 / `#` / 空行都是有意义的，
/// 逐行处理只会让用户没法对照 `nginx -t` 的错误行。
pub fn deploy_site_conf_overlay(conf: &DeploySiteConf, entity: &Entity<Shell>) -> AnyElement {
    let body: AnyElement = if conf.busy {
        div()
            .text_sm()
            .text_color(theme::text_muted())
            .child("正在读取…")
            .into_any_element()
    } else {
        match conf.text.as_deref() {
            Some(text) if !text.trim().is_empty() => v_flex()
                .id("deploy-site-conf-text")
                .w_full()
                .max_h(px(420.))
                .overflow_y_scroll()
                .rounded_md()
                .bg(theme::bg())
                .p_3()
                .child(
                    div()
                        .font_family("Consolas")
                        .text_xs()
                        .text_color(theme::text())
                        .child(text.to_owned()),
                )
                .into_any_element(),
            Some(_) => empty_state("（配置是空的）").into_any_element(),
            None => div()
                .text_sm()
                .text_color(theme::text_dim())
                .child("正在加载…")
                .into_any_element(),
        }
    };

    let card = v_flex()
        .w(px(720.0))
        .max_h(px(560.0))
        .gap_3()
        .p_4()
        .rounded_lg()
        .bg(theme::bg_card())
        .border_1()
        .border_color(theme::border())
        .child(
            v_flex()
                .gap_1()
                .child(
                    div()
                        .text_lg()
                        .font_semibold()
                        .child(format!("站点配置 · {}", conf.app)),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_dim())
                        .child(crate::util::strip_md(
                            "容器里的 `/etc/nginx/conf.d/00-wslc-panel-<应用名>.conf`（只读）。\
                             重新部署会整份覆盖；要改就改部署弹窗里的站点区，别直接改这个文件。",
                        )),
                ),
        )
        .child(body)
        .child(h_flex().w_full().justify_end().child({
            let entity = entity.clone();
            Button::new("deploy-site-conf-close")
                .label("关闭")
                .small()
                .on_click(move |_, _, cx| {
                    entity.update(cx, |shell, cx| shell.close_deploy_site_conf(cx));
                })
        }));

    modal_scrim("deploy-site-conf-scrim", card).into_any_element()
}

/// 删除二次确认。
///
/// ⚠️ 必须把**完整路径**摆出来：删的虽然只有面板自己铺过的那几个名字，
/// 但目标可能是**卷挂载点**（本机那个 nginx 容器的 Web 根就挂在 `/dev/sdc` 上）
/// —— 那意味着删掉的是卷里的数据，而且不可撤销。
pub fn deploy_remove_confirm_overlay(
    app: &str,
    deploy: Option<&DeployState>,
    entity: &Entity<Shell>,
) -> AnyElement {
    let container = deploy.map(|d| d.container.clone()).unwrap_or_default();
    let target = deploy
        .and_then(|d| d.index_of(app).map(|i| d.entries[i].record.target.clone()))
        .unwrap_or_else(|| "（找不到目标目录）".to_owned());
    let is_process = deploy
        .and_then(|d| d.index_of(app).map(|i| d.entries[i].is_process()))
        .unwrap_or(false);

    let card = v_flex()
        .w(px(520.0))
        .gap_3()
        .p_4()
        .rounded_lg()
        .bg(theme::bg_card())
        .border_1()
        .border_color(theme::danger_edge())
        .child(
            div()
                .text_lg()
                .font_semibold()
                .text_color(theme::danger())
                .child(format!("删除部署「{app}」？")),
        )
        .child(
            div()
                .text_sm()
                .text_color(theme::text())
                .child(crate::util::strip_md(if is_process {
                    "面板会：① 停掉这个应用的进程；② 删掉面板铺进目标目录的那几个文件\
                     （`app.log` / `app.pid` 与上次部署的文件，**别人的东西不动**）；\
                     ③ 删掉本地的那条记录。"
                } else {
                    "面板会：删掉面板铺进目标目录的那几个文件（内容形态没有我们起的进程；\
                     镜像自带的文件、你自己放的东西都不动），然后删掉本地的那条记录。"
                })),
        )
        .child(kv_block("容器", container))
        .child(kv_block("目标目录", target))
        .child(
            div()
                .text_xs()
                .text_color(theme::danger())
                .child(crate::util::strip_md(
                    // ⚠️ 别写成"被清掉的是卷里的数据"：v0.9.10 之前那句是对的
                    //（那时确实是整目录 `find -delete`）。现在删的**只有面板自己
                    // 铺过的那几个名字**，别人的文件一个都不碰 —— 这句要说的是
                    // "删掉的那几个文件在**卷**上、删了就不会回来"，不是"清空目录"。
                    "⚠️ 这一步不可撤销。如果那个目录挂在卷上（比如 nginx 的站点目录），\
                     删掉的就是**卷里**的那几个文件 —— 卷还在，文件不会回来。",
                )),
        )
        .child(
            h_flex()
                .w_full()
                .justify_end()
                .gap_2()
                .child({
                    let entity = entity.clone();
                    Button::new("deploy-remove-cancel")
                        .label("取消")
                        .small()
                        .on_click(move |_, _, cx| {
                            entity.update(cx, |shell, cx| {
                                shell.cancel_remove_deployment(cx)
                            });
                        })
                })
                .child({
                    let entity = entity.clone();
                    let app = app.to_owned();
                    Button::new("deploy-remove-confirm")
                        .label("确认删除")
                        .small()
                        .danger()
                        .on_click(move |_, _, cx| {
                            let app = app.clone();
                            entity.update(cx, |shell, cx| {
                                shell.confirm_remove_deployment(app, cx)
                            });
                        })
                }),
        );

    modal_scrim("deploy-remove-scrim", card).into_any_element()
}

// ---------------------------------------------------------------------------
// ③ 镜像 / 网络 / 卷
// ---------------------------------------------------------------------------

/// 镜像页（v0.6.2 合并成一页，v0.6.3 把拉取表单收进弹窗）。
///
/// 页面现在只剩两样东西：一个「拉取镜像…」按钮 + **本地镜像**表。
/// 拉取表单（registry / 仓库名 / tag 下拉框 / 体积 / 拉取）在弹窗里，
/// 见 [`pull_dialog_overlay`] —— 它和这里**是同一个入口**：点这个按钮才会
/// 建表单、才可能拉取（`AGENTS.md` §5「同一个动作只给一个入口」）。
///
/// # 为什么表单不铺在页面上（v0.6.3 改回来）
///
/// v0.6.2 合并两页时把表单铺在了页面上，结果这一页被表单占满，
/// 而"本地有什么镜像"反倒成了次要内容 —— 后者才是经常要看的。
pub fn images(state: &AppState, entity: &Entity<Shell>) -> AnyElement {
    let rows: Vec<AnyElement> = state
        .snapshot
        .images
        .iter()
        .enumerate()
        .map(|(ix, img)| {
            let reference = img.reference();
            table_row(
                IMAGE_COLUMNS,
                vec![
                    cell_text_full(reference.clone(), reference.clone()),
                    cell_muted(img.short_id().to_owned()),
                    cell_muted(img.size.clone()),
                    cell_muted(img.created_since.clone()),
                    danger_button(
                        &format!("rmi-{ix}"),
                        "删除",
                        PendingAction::RemoveImage(reference),
                        entity,
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element()
        })
        .collect();

    let open_pull = {
        let entity = entity.clone();
        // 刻意**不**用 `.disabled()`：全项目都不导入 `Disableable`
        // （禁用态的文字几乎看不清，实机截图确认过）。重复点击由
        // `open_pull_dialog` 里的守卫 + 提示条处理。
        Button::new("open-pull-dialog")
            .label("拉取镜像…")
            .primary()
            .small()
            .on_click(move |_, window, cx| {
                // 表单里的 `InputState` / `SelectState` 只能在有 window 的地方创建，
                // 这里正好有 —— 所以表单是**点开弹窗那一刻**才建的。
                entity.update(cx, |shell, cx| shell.open_pull_dialog(window, cx));
            })
    };

    // 拉取中时这一行说明"在忙什么"：进度本身在弹窗里（而且那个弹窗关不掉），
    // 这一行只是让页面本身也看得出它在忙。
    let status = match &state.pulling {
        Some(progress) => format!("正在拉取 {} …", progress.image),
        None => format!(
            "共 {} 条镜像记录（同一镜像可能对应多个仓库引用）",
            state.snapshot.images.len()
        ),
    };

    v_flex()
        .w_full()
        .gap_3()
        .child(
            h_flex()
                .w_full()
                .justify_between()
                .items_center()
                .child(open_pull)
                .child(div().text_sm().text_color(theme::text_muted()).child(status)),
        )
        .child(
            v_flex()
                .w_full()
                .rounded_lg()
                .bg(theme::bg_card())
                .border_1()
                .border_color(theme::border())
                .overflow_hidden()
                .child(table_header(IMAGE_COLUMNS))
                .child(if rows.is_empty() {
                    empty_state("没有本地镜像").into_any_element()
                } else {
                    v_flex().w_full().children(rows).into_any_element()
                }),
        )
        .into_any_element()
}

/// 「拉取镜像」弹窗（覆盖层）。
///
/// # 两种形态
///
/// 没在拉取时是**表单**（[`pull_dialog_form`]）；拉取中是**进度**
/// （[`pull_progress_body`]，里面自带「取消拉取」）。
/// ⚠️ 拉取进行中这个弹窗**关不掉**（`Shell::close_pull_dialog` 会拒绝并给提示），
/// 所以进度形态下也不画「关闭」—— 一个按下去只会弹提示的按钮不如没有。
///
/// # 表单里的三段
///
/// 1. registry（**可编辑**，默认内置加速站）+ 仓库名 + 「查询」；
/// 2. **tag 下拉框**（可搜索）—— 选中即去量体积，而且**只量选中的那一个**：
///    一次要两个请求（index → 本机架构那份 manifest，实测内容协商省不掉），
///    实测 `node` 有 9170 个 tag，全量一遍就是 9170 次请求，直接撞限流；
/// 3. 选中之后：完整引用 + 「下载约 N MB」+「拉取」，
///    以及镜像已经在本地时的「**用这个镜像创建容器**」。
pub fn pull_dialog_overlay(
    form: &ImageForm,
    state: &AppState,
    entity: &Entity<Shell>,
) -> AnyElement {
    let pulling = state.pulling.is_some();
    let body: AnyElement = match &state.pulling {
        Some(progress) => pull_progress_body(progress, entity),
        None => pull_dialog_form(form, state, entity),
    };

    let mut card = v_flex()
        // tag 下拉框展开时会很高，允许滚动，别把按钮挤没。
        // ⚠️ 滚动容器**必须**带 `.id(...)`（`overflow_y_scroll` 来自
        // `StatefulInteractiveElement`，只对带 id 的元素可用 —— 漏了是编译错误）。
        .id("pull-dialog-card")
        .w(px(720.))
        .max_h(px(820.))
        .overflow_y_scroll()
        .gap_4()
        .p_5()
        .rounded_lg()
        .bg(theme::bg_card())
        .border_1()
        .border_color(theme::border())
        .child(
            div()
                .text_lg()
                .font_bold()
                .text_color(theme::text())
                .child("拉取镜像"),
        )
        .child(body);

    if !pulling {
        let close = {
            let entity = entity.clone();
            Button::new("pull-dialog-close")
                .label("关闭")
                .small()
                .on_click(move |_, _, cx| {
                    entity.update(cx, |shell, cx| shell.close_pull_dialog(cx));
                })
        };
        card = card.child(h_flex().w_full().justify_end().child(close));
    }

    modal_scrim("pull-dialog", card).into_any_element()
}

/// 弹窗里那张**表单**（没在拉取时）。
///
/// 拆成独立函数只是为了让 [`pull_dialog_overlay`] 的两种形态一眼看得清。
fn pull_dialog_form(form: &ImageForm, state: &AppState, entity: &Entity<Shell>) -> AnyElement {
    let page = &state.registry;

    let presets: Vec<AnyElement> = registry::COMMON_IMAGES
        .iter()
        .map(|name| {
            // `COMMON_IMAGES` 是 `&[&str]`，`iter()` 给的是 `&&str` ——
            // 解一层引用拿到 `&'static str`（它 `Copy`，`move` 闭包直接拷进去）。
            //
            // ⚠️ 写成 `let name = *name;` 而**不是** `let name: &'static str = *name;`：
            // 后者 clippy 会报 `explicit_auto_deref`（CI 的 lint 任务实测报过它）。
            let name = *name;
            let entity = entity.clone();
            Button::new(SharedString::from(format!("image-preset-{name}")))
                .label(name)
                .small()
                .on_click(move |_, window, cx| {
                    entity.update(cx, |shell, cx| {
                        shell.apply_registry_preset(name, window, cx);
                    });
                })
                .into_any_element()
        })
        .collect();

    let query_row = h_flex()
        .w_full()
        .gap_2()
        .items_center()
        .child(
            v_flex()
                .gap_1()
                .w(px(240.))
                .flex_none()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_dim())
                        .child("registry（默认内置加速站）"),
                )
                .child(Input::new(&form.host).id("image-host").w_full()),
        )
        .child(
            v_flex()
                .gap_1()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_dim())
                        .child("仓库名（可带 `:tag`，也可以粘完整引用或整条 URL）"),
                )
                .child(
                    Input::new(&form.repository)
                        .id("image-repository")
                        .w_full(),
                ),
        )
        .child({
            let entity = entity.clone();
            // 点下去按钮先变成「查询中…」—— 从点击到结果出来之间，
            // 界面上必须有变化（`AGENTS.md` §7.1：没有它，用户会以为按钮坏了
            // 然后去点第二次）。
            Button::new("image-query")
                .label(if page.loading { "查询中…" } else { "查询" })
                .primary()
                .small()
                .on_click(move |_, window, cx| {
                    entity.update(cx, |shell, cx| shell.query_registry(window, cx));
                })
        });

    let tag_picker = v_flex()
        .w_full()
        .gap_1()
        .child(
            div()
                .text_xs()
                .text_color(theme::text_dim())
                .child("tag（可以打字搜索）"),
        )
        .child(
            Select::new(&form.tags_select)
                .placeholder("先查一个仓库；查不到也能按上面的完整引用直接拉")
                .w_full()
                .menu_max_h(px(360.)),
        );

    // 状态行：说清"现在是什么情况"
    let status: AnyElement = if page.loading {
        div()
            .text_sm()
            .text_color(theme::text_muted())
            .child("正在拉取 tag 列表…")
            .into_any_element()
    } else if let Some(error) = page.error.as_ref() {
        v_flex()
            .w_full()
            .gap_1()
            .child(
                div()
                    .text_sm()
                    .text_color(theme::danger())
                    .child(error.clone()),
            )
            .child(div().text_xs().text_color(theme::text_dim()).child(
                // 文案是 Markdown 风格 → 过一遍 `strip_md`（否则屏幕上真的显示
                // `**` 和反引号）。这一处 v0.9.13 之前一直漏着。
                crate::util::strip_md(
                    "换一个仓库名再试；只有支持 Registry V2 的 `tags/list` 才列得出来\
                     （私有的多半要认证）。**列不出来也能拉**：把 tag 写在仓库名后面\
                     （`nginx:1.27`），或者直接粘完整引用。",
                ),
            ))
            .into_any_element()
    } else if let Some(loaded) = page.loaded.as_ref() {
        let note = page
            .normalization_note()
            .map(|note| format!("（{note}）"))
            .unwrap_or_default();
        let mut text = format!("{loaded}：{} 个 tag{note}", page.tags.len());
        // 用户打的 tag 不在列表里 —— 这件事**必须说出来**，否则用户会以为
        // 自己要的那个 tag 不存在（实机反馈："我想拉取 openjdk:8-jre-slim，
        // 结果啥东东都出来了"）。实测这个镜像源的 tags/list 确实可能不全：
        // `library/openjdk` 回 17282 个，里面没有 `8-jre-slim` 也没有 `latest`，
        // 但两者的 manifest 都是 200（能拉）。
        if let Some(tag) = page
            .effective_tag()
            .filter(|tag| !page.tags.iter().any(|t| t.as_str() == *tag))
        {
            text.push_str(&format!(
                " —— 你要的 {tag} 不在这个列表里。这个镜像源的列表可能不全\
                 （它的 manifest 存在时照样能拉，只是体积可能量不出来）"
            ));
        }
        div()
            .text_sm()
            .text_color(theme::text_muted())
            .child(text)
            .into_any_element()
    } else {
        div()
            .text_sm()
            .text_color(theme::text_dim())
            .child("填一个仓库名，或者点上面的常用镜像。tag 列表按「latest 优先、版本倒序」排过。")
            .into_any_element()
    };

    // 选中了什么、能不能拉
    let action: AnyElement = match page.selected_reference() {
        // 一个 tag 都没有时给一句说明，别让用户以为按钮没画出来。
        // 这里也是"别的 registry 列不出 tag"时的出口。
        None => div()
            .text_xs()
            .text_color(theme::text_dim())
            .child("选一个 tag（或者在仓库名后面写上 `:tag`）之后，这里会出现「拉取」。")
            .into_any_element(),
        Some(reference) => {
            // 已经在本地了就顺手给一个「创建容器」：镜像名预填，用户不用再手打一遍。
            //
            // 没拉下来时**不放**这个按钮（只给一句说明）：`wslc run` 的拉取策略是
            // `Never`，点了必然失败 —— 那是"界面上点了没反应"里最差的一种。
            // 拉取完成后 `start_pull` 会刷新快照，按钮自己就出现了。
            let pulled = state
                .snapshot
                .images
                .iter()
                .any(|img| img.reference() == reference);

            let size_line: AnyElement = if page.sizing {
                div()
                    .text_xs()
                    .text_color(theme::text_dim())
                    .child("正在量体积…")
                    .into_any_element()
            } else if let Some(label) = page.size_label() {
                // ⚠️ 措辞是「**下载约**」：这是 manifest 累加的**压缩后**大小，
                // 不是落盘占用。同机 nginx 实测 60.9 MB，而「本地镜像」表里
                // `wslc images` 的 `Size` 是 163 MB（差 2.7 倍）——
                // 写成"大小"用户拿它一对就会当 bug 报上来。
                div()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(label)
                    .into_any_element()
            } else if let Some(error) = page.size_error.as_ref() {
                // 量不出来**不挡住拉取**：说清原因，拉不拉由用户决定。
                div()
                    .text_xs()
                    .text_color(theme::warning())
                    .child(format!("体积量不出来：{error}"))
                    .into_any_element()
            } else {
                div().into_any_element()
            };

            let pull = {
                let entity = entity.clone();
                Button::new("image-pull")
                    .label("拉取")
                    .primary()
                    .small()
                    .on_click(move |_, _, cx| {
                        entity.update(cx, |shell, cx| shell.pull_registry_selection(cx));
                    })
            };

            let create: AnyElement = if pulled {
                let entity = entity.clone();
                let image = reference.clone();
                Button::new("image-create")
                    .label("用这个镜像创建容器")
                    .small()
                    .on_click(move |_, window, cx| {
                        // ⚠️ 走 `use_pulled_image` 而不是直接开创建弹窗：
                        // 它会**先关掉这个弹窗** —— 两层遮罩叠在一起没有任何好处。
                        let image = image.clone();
                        entity.update(cx, |shell, cx| {
                            shell.use_pulled_image(image, window, cx);
                        });
                    })
                    .into_any_element()
            } else {
                div()
                    .text_xs()
                    .text_color(theme::text_dim())
                    .child("拉下来之后，这里会出现「用这个镜像创建容器」。")
                    .into_any_element()
            };

            v_flex()
                .w_full()
                .gap_2()
                .child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .items_center()
                        .min_w_0()
                        .child(
                            div()
                                .text_sm()
                                .text_color(theme::text())
                                .child("将拉取"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::text_muted())
                                .child(reference),
                        ),
                )
                .child(
                    h_flex()
                        .w_full()
                        .gap_3()
                        .items_center()
                        .child(size_line)
                        .child(pull)
                        .child(create),
                )
                .into_any_element()
        }
    };

    v_flex()
        .w_full()
        .gap_3()
        .child(div().text_xs().text_color(theme::text_dim()).child(
            "实测这台机器上 Docker Hub 直连不通（registry-1.docker.io 超时），\
             所以默认用上面那个加速站（它有完整的 Registry V2 接口）。\
             换别的 registry 就直接改上面那格地址 —— 列不出 tag 也能拉：\
             把 tag 写在仓库名后面（`nginx:1.27`），或者粘一整条完整引用。",
        ))
        .child(h_flex().w_full().gap_2().flex_wrap().children(presets))
        .child(query_row)
        .child(tag_picker)
        .child(status)
        .child(action)
        .into_any_element()
}

/// 网络页。
pub fn networks(state: &AppState, entity: &Entity<Shell>) -> impl IntoElement {
    let rows: Vec<AnyElement> = state
        .snapshot
        .networks
        .iter()
        .enumerate()
        .map(|(ix, net)| {
            let op: AnyElement = if net.is_builtin() {
                div()
                    .text_xs()
                    .text_color(theme::text_dim())
                    .child("内置网络，不可删除")
                    .into_any_element()
            } else {
                danger_button(
                    &format!("rmnet-{ix}"),
                    "删除",
                    PendingAction::RemoveNetwork(net.name.clone()),
                    entity,
                )
                .into_any_element()
            };

            table_row(
                NETWORK_COLUMNS,
                vec![
                    cell_text(net.name.clone()),
                    cell_muted(net.short_id().to_owned()),
                    cell_muted(net.driver.clone()),
                    cell_muted(net.scope.clone()),
                    cell_muted(if net.ipv6_enabled() {
                        "启用"
                    } else {
                        "关闭"
                    }),
                    op,
                ],
            )
            .into_any_element()
        })
        .collect();

    v_flex()
        .w_full()
        .gap_3()
        .child(
            div()
                .text_sm()
                .text_color(theme::text_muted())
                .child(format!("共 {} 个网络", state.snapshot.networks.len())),
        )
        .child(
            v_flex()
                .w_full()
                .rounded_lg()
                .bg(theme::bg_card())
                .border_1()
                .border_color(theme::border())
                .overflow_hidden()
                .child(table_header(NETWORK_COLUMNS))
                .child(if rows.is_empty() {
                    empty_state("没有网络").into_any_element()
                } else {
                    v_flex().w_full().children(rows).into_any_element()
                }),
        )
}

/// 卷页。
pub fn volumes(state: &AppState, entity: &Entity<Shell>) -> impl IntoElement {
    let rows: Vec<AnyElement> = state
        .snapshot
        .volumes
        .iter()
        .enumerate()
        .map(|(ix, vol)| {
            table_row(
                VOLUME_COLUMNS,
                vec![
                    cell_text(vol.display_name().to_owned()),
                    cell_muted(vol.driver.clone()),
                    cell_muted(vol.scope.clone()),
                    cell_muted_full(vol.name.clone(), vol.mountpoint.clone()),
                    danger_button(
                        &format!("rmvol-{ix}"),
                        "删除",
                        PendingAction::RemoveVolume(vol.name.clone()),
                        entity,
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element()
        })
        .collect();

    // 页头：左边计数，右边两个动作。
    //
    // 「创建卷」复用那个共用的单输入框弹窗（`PromptKind::CreateVolume`）——
    // 注意它**不属于任何发行版**，所以主体传空串（弹窗据此不画那一行）。
    let header = h_flex()
        .w_full()
        .justify_between()
        .items_center()
        .child(
            div()
                .text_sm()
                .text_color(theme::text_muted())
                .child(format!("共 {} 个卷", state.snapshot.volumes.len())),
        )
        .child(
            h_flex()
                .gap_2()
                .child(prompt_button(
                    "volume-create",
                    "创建卷",
                    PromptKind::CreateVolume,
                    "",
                    entity,
                ))
                .child(
                    // 「清理」在界面上的名字要带上"未使用"三个字：
                    // 只写"清理"会让人担心它把正在用的卷也删了。
                    danger_button(
                        "volume-prune",
                        "清理未使用卷",
                        PendingAction::PruneVolumes,
                        entity,
                    )
                    .into_any_element(),
                ),
        );

    v_flex().w_full().gap_3().child(header).child(
        v_flex()
            .w_full()
            .rounded_lg()
            .bg(theme::bg_card())
            .border_1()
            .border_color(theme::border())
            .overflow_hidden()
            .child(table_header(VOLUME_COLUMNS))
            .child(if rows.is_empty() {
                empty_state("没有卷（无卷时 wslc 输出为空）").into_any_element()
            } else {
                v_flex().w_full().children(rows).into_any_element()
            }),
    )
}

// ---------------------------------------------------------------------------
// ④ WSL 实例（发行版）
// ---------------------------------------------------------------------------

/// WSL 实例（发行版）列表。
///
/// # 能干什么
///
/// 行内放**高频**动作（打开终端 / 终止 / 设为默认 / 删除），
/// 低频但重要的（改版本 / 压缩 / 打开安装位置）收进详情弹窗 ——
/// 和容器页同一套取舍。
///
/// ⚠️ 「启动」是**真的启动**：它从 Windows 这边吊住一个 `wsl.exe` 不放，
/// WSL 就认为有活动会话，发行版会一直运行下去（机制和实测见
/// [`wslc_core::cmd::distro::start`]）。
///
/// 代价是那个进程属于**本面板**：面板关掉它也不会退，发行版继续跑。
/// 这件事写在**详情弹窗**里 —— 用户有权知道是谁在维持它、
/// 以及关掉面板之后它会怎样（只对少数几行成立，塞进列表行会把那一格挤破，
/// 见 [`distro_row`]）。
///
/// # 为什么"没有实例"不是错误
///
/// 一台机器上没装发行版是完全正常的状态，所以这里给的是引导文案
/// （怎么让 `wsl.exe` 被找到），而不是一条红色错误。
pub fn instances(state: &AppState, entity: &Entity<Shell>) -> AnyElement {
    let distros = &state.snapshot.distros;

    if distros.is_empty() {
        return v_flex()
            .w_full()
            .gap_4()
            .child(card(
                "WSL 实例",
                v_flex()
                    .w_full()
                    .gap_3()
                    .child(empty_state("没有检测到任何 WSL 发行版"))
                    .child(div().text_xs().text_color(theme::text_dim()).child(
                        "若确实安装过，请确认 wsl.exe 可用：程序会自动在 \
                         C:\\Program Files\\WSL\\ 下查找，也可以用环境变量 WSL_PATH 指定。",
                    ))
                    // 一个实例都没有的时候，最该看到的就是"怎么装一个"
                    .child(h_flex().child(add_instance_button(entity))),
            ))
            .into_any_element();
    }

    let default_name = state.default_distro().map(|name| name.to_owned());
    let rows: Vec<AnyElement> = distros
        .iter()
        .map(|distro| {
            // 本面板正在吊着它吗？（决定行内还给不给「启动」按钮 ——
            // 再吊一个没有意义。这件事本身写在详情弹窗里，列表里不显示标记）
            let kept = state.kept_alive.iter().any(|n| n == &distro.name);
            distro_row(distro, default_name.as_deref(), kept, entity)
        })
        .collect();

    let running = distros.iter().filter(|d| d.state.is_running()).count();

    // 「关停全部」是**页面级**动作（影响所有发行版），所以放工具栏，
    // 不放进任何一行 —— 放行里会让人以为只影响那一行。
    //
    // 一个都没在跑时干脆不显示：`--shutdown` 虽然不会失败，
    // 但让用户确认一个没有效果的动作是没意义的。
    let shutdown_all: AnyElement = if running == 0 {
        div().into_any_element()
    } else {
        let entity = entity.clone();
        Button::new("shutdown-all")
            .label("关停全部")
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| {
                    shell.request_distro(DistroAction::ShutdownAll, cx);
                });
            })
            .into_any_element()
    };

    v_flex()
        .w_full()
        .gap_3()
        .child(distro_summary_card(state, distros))
        .child(
            h_flex()
                .w_full()
                .justify_between()
                .child(
                    h_flex().gap_3().child(add_instance_button(entity)).child(
                        div()
                            .text_sm()
                            .text_color(theme::text_muted())
                            .child(format!(
                                "共 {} 个发行版，{} 个在运行",
                                distros.len(),
                                running
                            )),
                    ),
                )
                .child(shutdown_all),
        )
        .child(
            v_flex()
                .w_full()
                .rounded_lg()
                .bg(theme::bg_card())
                .border_1()
                .border_color(theme::border())
                .overflow_hidden()
                .child(table_header(DISTRO_COLUMNS))
                .child(v_flex().w_full().children(rows)),
        )
        .into_any_element()
}

/// 实例概览卡：运行中 / 全部 / 磁盘合计 / 默认发行版。
fn distro_summary_card(state: &AppState, distros: &[Distro]) -> AnyElement {
    let running = distros.iter().filter(|d| d.state.is_running()).count();

    // 只把**读到了大小**的那些加起来。读不到的（注册表被挡、磁盘文件不在）
    // 按 0 算，但要在文案里如实说明有几个 —— 不能让用户以为合计是准的。
    let known = distros.iter().filter(|d| d.vhdx_bytes.is_some()).count();
    let total_bytes: u64 = distros.iter().filter_map(|d| d.vhdx_bytes).sum();

    let default = state
        .default_distro()
        .map(|name| name.to_owned())
        .unwrap_or_else(|| "（未设置）".to_owned());

    let disk_note = if known == distros.len() {
        "磁盘合计是 VHDX 的虚拟大小，不是磁盘实际占用。".to_owned()
    } else {
        format!(
            "磁盘合计是 VHDX 的虚拟大小；{} / {} 个实例没读到大小\
             （注册表读不到，或磁盘文件不在）。",
            distros.len() - known,
            distros.len()
        )
    };

    card(
        "实例概览",
        v_flex()
            .w_full()
            .gap_3()
            .child(
                h_flex()
                    .w_full()
                    .gap_3()
                    .child(stat_tile("运行中", running.to_string(), theme::success()))
                    .child(stat_tile(
                        "全部",
                        distros.len().to_string(),
                        theme::primary(),
                    ))
                    .child(stat_tile(
                        "磁盘合计",
                        format_bytes(total_bytes),
                        theme::warning(),
                    )),
            )
            .child(kv("默认发行版", default))
            .child(
                div()
                    .text_xs()
                    .text_color(theme::text_dim())
                    .child(disk_note),
            ),
    )
    .into_any_element()
}

/// 一行实例。
///
/// `kept` = **本面板正吊着它**（见 [`KeepAlive`](crate::app)）。
/// 列表里它只影响「启动」按钮给不给（已经吊着了，再吊一个没意义）；
/// "谁在维持这个发行版"那句话在详情弹窗里，不在这一行 —— 原因见下面的注释。
fn distro_row(
    distro: &Distro,
    default_name: Option<&str>,
    kept: bool,
    entity: &Entity<Shell>,
) -> AnyElement {
    let is_default = default_name == Some(distro.name.as_str());

    let location = distro
        .base_path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "—".to_owned());

    let disk = match distro.vhdx_bytes {
        Some(bytes) => format_bytes(bytes),
        None => "—".to_owned(),
    };

    // 状态列**只放状态徽标本身**，不在这里再标"由本面板保持运行"。
    //
    // ⚠️ 原先运行中的行会在状态右边再挂一个「本面板保持」徽标，实机截图里
    // 它**盖到了隔壁「版本」列的数字上**（两个字叠在一起，成了一段乱码）。
    // 原因是两个徽标都是 `flex_none`（不压缩），加起来比状态列宽
    // （权重 85 / 总和 1060），而 `table_row` 的单元格**不裁剪**内容 ——
    // 超出的部分就这么画进了下一列。单独加宽状态列只是把阈值往后推，
    // 换一种字体 / 缩放到 125% 又会溢出来。
    // 「谁在维持它」是低频信息，改写在详情弹窗里（`distro_detail_overlay`）。
    let status = cell_distro_badge(distro.state);

    table_row(
        DISTRO_COLUMNS,
        vec![
            distro_name_link(&distro.name, entity),
            status,
            cell_muted(distro.version_label()),
            if is_default {
                badge("是", theme::primary(), theme::primary_soft()).into_any_element()
            } else {
                cell_muted("—")
            },
            cell_muted_full(distro.name.clone(), location),
            cell_muted(disk),
            distro_row_actions(distro, is_default, kept, entity),
        ],
    )
    .into_any_element()
}

/// 发行版名字 —— 点击打开详情弹窗。
///
/// 和容器页一样：名字就是入口，低频动作都收在详情里。
/// 悬停/按下同 [`container_name_link`]。
fn distro_name_link(name: &str, entity: &Entity<Shell>) -> AnyElement {
    let entity = entity.clone();
    let for_click = name.to_owned();
    div()
        .id(SharedString::from(format!("distro-detail-{name}")))
        .cursor_pointer()
        .text_sm()
        .text_color(theme::primary())
        .hover(|this| this.text_color(theme::link_hover()))
        .active(|this| this.text_color(theme::primary()))
        .overflow_hidden()
        .truncate()
        .child(name.to_owned())
        .on_click(move |_, _, cx| {
            let target = for_click.clone();
            entity.update(cx, |shell, cx| shell.open_distro_detail(target, cx));
        })
        .into_any_element()
}

/// 「添加实例」入口按钮 —— 跳到「添加实例」页。
///
/// 跳转走 `set_page`（需要 `window`）：那一页的输入框必须在点击时创建。
fn add_instance_button(entity: &Entity<Shell>) -> AnyElement {
    let entity = entity.clone();
    Button::new("goto-add-instance")
        .label("添加实例")
        .small()
        .primary()
        .on_click(move |_, window, cx| {
            entity.update(cx, |shell, cx| {
                shell.set_page(Page::AddInstance, window, cx);
            });
        })
        .into_any_element()
}

/// 发行版行内操作：**只放高频的**。
///
/// 低频但重要的（改版本 / 压缩 / 打开安装位置）都在详情弹窗里 ——
/// 列表里塞满按钮，常用的那个反而找不到。
///
/// `kept` = 本面板正吊着它（见 [`distro_row`]）。
fn distro_row_actions(
    distro: &Distro,
    is_default: bool,
    kept: bool,
    entity: &Entity<Shell>,
) -> AnyElement {
    let name = distro.name.as_str();
    let mut actions: Vec<AnyElement> = Vec::new();

    // 打开终端同时就把发行版启动了，所以它既是"启动"也是"进去干活"。
    actions.push(
        immediate_button(
            &format!("term-{name}"),
            "打开终端",
            ImmediateAction::OpenDistroTerminal(name.to_owned()),
            entity,
        )
        .into_any_element(),
    );

    // 「启动」是**真的**启动：本面板会吊住一个 `wsl.exe` 让发行版一直运行
    // （见 `wslc_core::cmd::distro::start`）。
    // 已经由本面板吊着的就不再给这个按钮 —— 再吊一个没有意义，只会多一个进程。
    if !distro.state.is_running() && !distro.state.is_transitional() && !kept {
        actions.push(
            immediate_button(
                &format!("start-{name}"),
                "启动",
                ImmediateAction::StartDistro(name.to_owned()),
                entity,
            )
            .into_any_element(),
        );
    }

    // 只有运行中才谈得上"终止"。
    if distro.state.is_running() {
        actions.push(
            danger_button_distro(
                &format!("terminate-{name}"),
                "终止",
                DistroAction::Terminate(name.to_owned()),
                entity,
            )
            .into_any_element(),
        );
    }

    // 已经是默认了就不必再显示这个按钮（详情里会写明"当前是默认"）。
    // 同上：过渡态下一律不给写操作（这段代码上方的「启动」按钮
    // 也是用 `distro.state.is_transitional()` 挡的）。
    if !is_default && !distro.state.is_transitional() {
        actions.push(
            immediate_button(
                &format!("default-{name}"),
                "设为默认",
                ImmediateAction::SetDefaultDistro(name.to_owned()),
                entity,
            )
            .into_any_element(),
        );
    }

    // 「删除」**不在这里** —— 它挪去了详情。列表行是横向排布的，
    // 一个红色的「删除」和「打开终端」挨着，误点的代价太大；
    // 详情里地方宽，能把"要删掉多少"写清楚（见 `distro_detail_overlay`）。

    h_flex()
        .gap_2()
        .flex_wrap()
        .children(actions)
        .into_any_element()
}

/// 发行版状态徽标单元格。
fn cell_distro_badge(state: DistroState) -> AnyElement {
    let (fg, bg) = theme::distro_state_colors(&state);
    badge(state.label().to_owned(), fg, bg).into_any_element()
}

// ---------------------------------------------------------------------------
// ④b 添加实例
// ---------------------------------------------------------------------------

/// 「添加实例」页。
///
/// # 五条来源共用一套表单
///
/// 靠 [`InstallSourceKind`] 切换 —— 它们只是参数不同，没必要做成五个页面。
/// 这里**保留**了参考实现没有的「从 .wsl / 文件安装」（`.wsl` 是新格式，`--import` 吃不了）。
///
/// # 表单是懒创建的
///
/// `InputState::new` 需要 `&mut Window`，所以表单在**点击实例列表上那个
/// 「添加实例」按钮时**才建（见 `Shell::ensure_install_form`）。
/// 正常路径下进得来就一定有表单；万一没有（比如程序内部跳过来），
/// 给一句提示而不是 panic。
///
/// # 校验与预览都走纯函数
///
/// 红字（[`AppState::preflight`]）和步骤预览（`install_model::plan`）都是
/// **渲染时现算**的纯逻辑 —— 提交时用的是同两个函数，所以"看到的"和
/// "挡住的/要跑的"不会走偏。
///
/// ⚠️ 渲染时**不查**"安装目录非空"（那要碰文件系统，每帧查一次会在网络盘上卡住）；
/// 那条检查在提交那一刻做，见 `Shell::confirm_install`。
pub fn add_instance(shell: &Shell, cx: &App, entity: &Entity<Shell>) -> AnyElement {
    let state = &shell.state;
    let Some(form) = shell.install_form.as_ref() else {
        return card(
            "添加实例",
            v_flex()
                .w_full()
                .gap_2()
                .child(empty_state("表单还没准备好"))
                .child(div().text_xs().text_color(theme::text_dim()).child(
                    "回到「实例列表」，再点一次上面的「添加实例」按钮即可 —— 输入框必须在点击时创建。",
                )),
        )
        .into_any_element();
    };

    let source = form.source;
    let spec = form.to_spec(
        cx,
        &state.mirrors,
        &state.online,
        // 已经装过的发行版名：镜像源的"裸名字换兄弟"要拿它排除掉
        // （见 `OnlineDistroState::sibling_id`）
        &state.distro_names(),
        state.prefs.install_dir.as_deref(),
    );
    let check = state.preflight(&spec, false);
    let plan = install_model::plan(&spec, &state.plan_context());
    let running = state
        .installing
        .as_ref()
        .map(InstallProgress::is_running)
        .unwrap_or(false);

    // -- 来源五选一 --
    let source_buttons: Vec<AnyElement> = InstallSourceKind::ALL
        .iter()
        .map(|kind| {
            let kind = *kind;
            let entity = entity.clone();
            // id 用 `{:?}`（ASCII）而不是 label（中文）：元素 id 要稳定、
            // 且不该随显示文案变。
            let mut button = Button::new(SharedString::from(format!("src-{kind:?}")))
                .label(kind.label())
                .small()
                .on_click(move |_, _, cx| {
                    entity.update(cx, |shell, cx| shell.set_install_source(kind, cx));
                });
            if source == kind {
                button = button.primary();
            }
            button.into_any_element()
        })
        .collect();

    // -- 输入框 --
    let mut fields: Vec<AnyElement> = vec![form_field(
        "install-name",
        source.name_field_label(),
        &form.name,
        cx,
        true,
    )];

    if let Some((label, _)) = source.file_filter() {
        // 「浏览…」和输入框并排。`flex_1 + min_w_0` 让输入框吃掉剩余宽度，
        // 又在窗口很窄时允许它收缩（不写 `min_w_0` 会把它顶出去）。
        let browse_file = {
            let entity = entity.clone();
            Button::new("install-browse-file")
                .label("浏览…")
                .small()
                .on_click(move |_, window, cx| {
                    entity.update(cx, |shell, cx| shell.browse_install_path(window, cx));
                })
        };

        fields.push(
            h_flex()
                .w_full()
                .gap_2()
                .items_end()
                .child(div().flex_1().min_w_0().child(form_field(
                    "install-path",
                    label,
                    &form.source_path,
                    cx,
                    true,
                )))
                .child(browse_file)
                .into_any_element(),
        );

        // 「识别为 …」：本地来源**只有一个入口**，装法由后缀决定 ——
        // 用户必须能看出我们猜成了什么（猜错了他会看到一条 wsl 的报错，
        // 而那条报错不会告诉他"是你文件后缀不对"）。
        //
        // 纯函数、不碰磁盘：渲染时现算（读文件头要碰磁盘，那条检查只能放在
        // 提交那一刻，见 `add_instance` 的说明）。
        let path = form.source_path.read(cx).value().trim().to_owned();
        let (hint, color) = match install_model::classify_local_file(&path) {
            Some(kind) => (format!("识别为：{}", kind.label()), theme::text_dim()),
            None if path.is_empty() => (
                "按**后缀**自动判断装法：tar / tar.gz / tar.xz → `--import`；\
                 .vhdx → `--import --vhd`；.wsl → WSL 安装器。"
                    .to_owned(),
                theme::text_dim(),
            ),
            // 认不出来不硬猜，但也不能什么都不说 —— 按 tar 处理并讲清楚
            None => (
                "没认出这个后缀，会按 **tar 归档**处理（`wsl --import`）。\
                 如果它其实是 `.vhdx` 或 `.wsl`，改一下文件名后缀即可。"
                    .to_owned(),
                theme::warning(),
            ),
        };
        fields.push(
            div()
                .text_xs()
                .text_color(color)
                .child(crate::util::strip_md(&hint))
                .into_any_element(),
        );
    }

    // 安装目录：+ 浏览 + 存为默认
    let browse_dir = {
        let entity = entity.clone();
        Button::new("install-browse-dir")
            .label("浏览…")
            .small()
            .on_click(move |_, window, cx| {
                entity.update(cx, |shell, cx| shell.browse_install_dir(window, cx));
            })
    };
    let remember_dir = {
        let entity = entity.clone();
        Button::new("install-remember-dir")
            .label("存为默认")
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.remember_install_dir(cx));
            })
    };

    fields.push(
        v_flex()
            .w_full()
            .gap_1()
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_end()
                    .child(div().flex_1().min_w_0().child(form_field(
                        "install-dir",
                        if source.requires_install_dir() {
                            "安装目录（必填）"
                        } else {
                            "安装目录（可留空，交给 WSL 决定）"
                        },
                        &form.install_dir,
                        cx,
                        true,
                    )))
                    .child(browse_dir)
                    .child(remember_dir),
            )
            .child(
                div().text_xs().text_color(theme::text_dim()).child(
                    "留空时会用「默认安装目录 + 发行版名」；点「存为默认」把当前这个路径记住。",
                ),
            )
            .into_any_element(),
    );

    // -- 来源专属区块 --
    let source_block: AnyElement = if source.is_online() {
        online_block(state, form, entity)
    } else if source.is_mirror() {
        mirror_block(state, form, entity, cx)
    } else {
        div().into_any_element()
    };

    // -- 选项 --
    let mut options: Vec<AnyElement> = Vec::new();

    // 这里**没有** WSL 版本选择器：本项目只支持 WSL 2，
    // 装出来的固定是 WSL 2（`wslc_core::model::install::WSL_VERSION`）。
    // 给一个只有一个选项的下拉框不如不给。

    // 开关用**按钮**而不是复选框：全项目都是这个路子
    // （复选框样式在深色主题下对比度很差）。
    if source.supports_launch() {
        // ⚠️ 不要用 `let entity = entity.clone()` 这种同名遮蔽：这个块里要用两次，
        // 第二次会因为"已经从第一次的遮蔽变量里移走了"而编译不过（真的踩到了）。
        let launch_entity = entity.clone();
        let mut button = Button::new("toggle-launch")
            .label(if form.launch {
                "装完立即启动：是"
            } else {
                "装完立即启动：否"
            })
            .small()
            .on_click(move |_, _, cx| {
                launch_entity.update(cx, |shell, cx| shell.toggle_install_launch(cx));
            });
        if form.launch {
            button = button.primary();
        }
        options.push(button.into_any_element());

        let web_entity = entity.clone();
        let mut button = Button::new("toggle-web-download")
            .label(if form.web_download {
                "下载源：GitHub（--web-download）"
            } else {
                "下载源：微软商店（Microsoft Store）"
            })
            .small()
            .on_click(move |_, _, cx| {
                web_entity.update(cx, |shell, cx| shell.toggle_web_download(cx));
            });
        if form.web_download {
            button = button.primary();
        }
        options.push(button.into_any_element());
        // 这条对"装不装得上"影响最大，所以把两条通道的差别说清：
        // 微软商店走的是商店/清单那条通道，`--web-download` 是 WSL 自己去
        // GitHub 下整包 —— 实测本机上前者能连上、后者直连不通（要代理）。
        // 真失败了执行器还会自动换一次（见 `PlannedStep::retry_other_source`）。
        options.push(
            div()
                .text_xs()
                .text_color(theme::text_dim())
                .child(if form.web_download {
                    "GitHub 那条要能连上 raw.githubusercontent.com（被墙时要走代理）；\
                     连不上会自动切回微软商店再试一次。"
                } else {
                    "微软商店是 WSL 官方那条路（本机实测能连上）；\
                     商店也拉不动时，改用「在线发行版（镜像源）」——那条完全不依赖 WSL 的网络。"
                })
                .into_any_element(),
        );
    }

    {
        let entity = entity.clone();
        let mut button = Button::new("toggle-default")
            .label(if form.set_default {
                "装完设为默认发行版：是"
            } else {
                "装完设为默认发行版：否"
            })
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.toggle_install_default(cx));
            });
        if form.set_default {
            button = button.primary();
        }
        options.push(button.into_any_element());
    }

    // -- 校验（红字）--
    let mut problems: Vec<AnyElement> = Vec::new();
    for error in &check.errors {
        problems.push(
            div()
                .text_xs()
                .text_color(theme::danger())
                .child(format!("✘ {error}"))
                .into_any_element(),
        );
    }
    for warning in &check.warnings {
        problems.push(
            div()
                .text_xs()
                .text_color(theme::warning())
                .child(format!("! {warning}"))
                .into_any_element(),
        );
    }

    // -- 步骤预览（实时；与执行读的是同一份计划）--
    let preview_body: AnyElement = match &plan {
        Ok(plan) => v_flex()
            .w_full()
            .gap_1()
            .children(plan.preview_lines().into_iter().map(|line| {
                div()
                    .font_family("Consolas")
                    .text_xs()
                    .text_color(theme::text())
                    .child(line)
            }))
            .children(plan.notes.iter().map(|note| {
                div()
                    .text_xs()
                    .text_color(theme::text_dim())
                    .child(note.clone())
            }))
            .into_any_element(),
        Err(e) => div()
            .text_xs()
            .text_color(theme::danger())
            .child(format!("参数还不对：{e}"))
            .into_any_element(),
    };

    // -- 提交 / 取消 --
    //
    // ⚠️ 重定位那一步（导出 → 注销 → 导入）**不给**取消按钮：
    // 它的 `cancellable` 是 false，中途停下等于把刚装好的发行版删掉。
    // 让用户点了之后才发现没用，比"这里没有按钮 + 一句说明"糟得多。
    let cancellable = state
        .installing
        .as_ref()
        .map(|progress| progress.cancellable)
        .unwrap_or(true);

    let action: AnyElement =
        if running && cancellable {
            let entity_cancel = entity.clone();
            h_flex()
                .w_full()
                .items_center()
                .gap_3()
                .child(
                    Button::new("cancel-install")
                        .label("取消安装")
                        .small()
                        .danger()
                        .on_click(move |_, _, cx| {
                            entity_cancel.update(cx, |shell, cx| shell.cancel_install(cx));
                        }),
                )
                .child(elapsed_label(state))
                .into_any_element()
        } else if running {
            h_flex()
                .w_full()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::warning())
                        .child("正在改名/挪位置 —— 这一步不能取消（中途停下会把刚装好的东西删掉）"),
                )
                .child(elapsed_label(state))
                .into_any_element()
        } else {
            // 取消过 / 装完之后按钮要能再点一次：文案跟着状态走。
            let label = if state.installing.is_some() {
                "再装一个"
            } else {
                "开始安装"
            };
            let entity = entity.clone();
            h_flex()
                .w_full()
                .justify_end()
                .child(Button::new("do-install").label(label).primary().on_click(
                    move |_, _, cx| {
                        entity.update(cx, |shell, cx| shell.confirm_install(cx));
                    },
                ))
                .into_any_element()
        };

    let mut page = v_flex()
        .w_full()
        .gap_4()
        .child(card(
            "安装来源",
            v_flex()
                .w_full()
                .gap_3()
                .child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .flex_wrap()
                        .children(source_buttons),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_dim())
                        .child(crate::util::strip_md(source.hint())),
                ),
        ))
        .child(card(
            "参数",
            v_flex()
                .w_full()
                .gap_3()
                .children(fields)
                .child(source_block),
        ))
        .child(card("选项", v_flex().w_full().gap_3().children(options)));

    if !problems.is_empty() {
        page = page.child(card(
            "装前检查",
            v_flex().w_full().gap_2().children(problems),
        ));
    }

    page = page.child(card("要执行的步骤", preview_body)).child(action);

    // 装过 / 正在装 → 显示日志（装完之后留在页面上，用户要能读它）
    if state.installing.is_some() {
        page = page.child(install_log_card(state, source, entity));
    }

    page.into_any_element()
}

/// 「微软商店」那一块：刷新 + **下拉框** + 来源说明。
///
/// 参考实现也是一个下拉框（`CustomComboBox`）—— 原先我们那种"几十个按钮
/// 铺满一屏 + 外面再挂一个搜索框"既占地方、又要用户自己找。
/// 搜索收进下拉框里（`SelectState::searchable`）。
///
/// 不需要 `cx`：下拉框的选中由 `Shell` 那边订阅，这里只负责画
/// （不像镜像源那块还要读自定义 URL 输入框）。
fn online_block(state: &AppState, form: &InstallForm, entity: &Entity<Shell>) -> AnyElement {
    let refresh = {
        let entity = entity.clone();
        Button::new("online-refresh")
            .label(if state.online.loading {
                "正在拉取…"
            } else {
                "刷新商店清单"
            })
            .small()
            .on_click(move |_, window, cx| {
                entity.update(cx, |shell, cx| shell.refresh_online_list(window, cx));
            })
    };

    // 下拉框：显示 `Ubuntu 24.04 LTS (Ubuntu-24.04)`，取到的是 id。
    // 选中由 `Shell` 那边订阅（`SelectEvent::Confirm`），这里只负责画。
    let picker = Select::new(&form.store_select)
        .placeholder("选一个发行版（可以打字搜索）")
        .w_full()
        .menu_max_h(px(320.));

    // 状态：正在拉 / 失败原因（原文）/ 来源说明
    let status: AnyElement =
        if state.online.loading {
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child("正在拉取在线清单…")
                .into_any_element()
        } else if !state.online.error.trim().is_empty() {
            v_flex()
                .w_full()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::warning())
                        .child(format!("清单有问题：{}", state.online.error)),
                )
                .child(div().text_xs().text_color(theme::text_dim()).child(
                    "拉不到清单也能装：直接在「发行版名」里填在线 id（例如 Ubuntu-24.04）。",
                ))
                .into_any_element()
        } else if !state.online.items.is_empty() {
            let mut lines: Vec<AnyElement> = vec![
                div()
                    .text_xs()
                    .text_color(theme::text_dim())
                    .child(format!(
                        "共 {} 个可装。{}",
                        state.online.items.len(),
                        state.online.source.label()
                    ))
                    .into_any_element(),
            ];
            // ⚠️ 清单是**我们**用 curl 从 CDN 兜底拉到的（不是 wsl 自己拉的）时，
            // 说明 `wsl.exe` 连不上它要的那份 manifest —— 而安装那一步正是走它。
            // 实测（2026-10-10）这种状态下商店与 `--web-download` 会**双双**报
            // `Wsl/InstallDistro/WININET_E_CANNOT_CONNECT`。与其让用户装到一半才失败，
            // 不如现在就说清"这条来源现在多半装不上、该换哪条"。
            //
            // 注意这句话**会过期**（实测同一小时内 `wsl -l -o` 从成功变成 500 又变回来），
            // 所以它必须带上"怎么刷新"的出口，而不是一句判决。
            match state.online.source {
                OnlineListSource::FallbackJson => lines.push(
                    div()
                        .text_xs()
                        .text_color(theme::warning())
                        .child(
                            "注意：这份清单是面板自己从 CDN 拉的，说明 wsl 自己拉不到它要的那份 \
                         manifest —— 现在用「微软商店」装多半会连不上。\
                         先试「在线发行版（镜像源）」（那条不经过 wsl 的网络）；\
                         如果你刚把代理修好，点一下上面的「刷新商店清单」再看这行还在不在。",
                        )
                        .into_any_element(),
                ),
                OnlineListSource::Wsl => lines.push(
                    div()
                        .text_xs()
                        .text_color(theme::success())
                        .child("wsl 自己拉到了清单 —— 这条来源现在是通的。")
                        .into_any_element(),
                ),
                OnlineListSource::Unknown => {}
            }
            // 选中了"不带版本号的家族名"（`Ubuntu`）时提醒一句：实测这类条目在这台机器上
            // **装不动**（`wsl --install -d Ubuntu` 两秒就 `WININET_E_CANNOT_CONNECT`），
            // 装的时候会自动换成带版本的那条（见 `OnlineDistroState::sibling_id`）。
            //
            // ⚠️ 兄弟**已经装过**时不能提这句：那时 `sibling_id` 会给 `None`
            // （换了也只会撞 `ERROR_ALREADY_EXISTS`），说"会自动改用 X"就是假话。
            if let Some(picked) = state.online.selected.as_deref() {
                if let Some(sibling) = state.online.sibling_id(picked, &state.distro_names()) {
                    lines.push(
                        div()
                            .text_xs()
                            .text_color(theme::warning())
                            .child(format!(
                                "「{picked}」不带版本号 —— 实测这类条目在本机装不动\
                             （2 秒就报连接失败），装的时候会自动改用「{sibling}」再试一次。"
                            ))
                            .into_any_element(),
                    );
                }
            }
            v_flex().w_full().gap_1().children(lines).into_any_element()
        } else {
            div()
                .text_xs()
                .text_color(theme::text_dim())
                .child("还没拉过清单 —— 点上面的按钮，或者直接在「发行版名」里手输 id。")
                .into_any_element()
        };

    v_flex()
        .w_full()
        .gap_2()
        .child(
            h_flex()
                .w_full()
                .gap_2()
                .items_center()
                .child(refresh)
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child("选择发行版（微软商店 / Microsoft Store）"),
                ),
        )
        .child(picker)
        .child(status)
        .into_any_element()
}

/// 「在线发行版（镜像源）」那一块：清单（动态拉）+ **下拉框** + 自动测速状态 + 自定义 URL。
///
/// 清单来自 `wslc_core::cmd::catalog`（wslui 的公开接口，见 `mirrors.rs` 的说明），
/// 顺序照参考实现排（Ubuntu / Debian 在最前，见 `sort_offers_like_reference`）。
///
/// # 为什么没有"镜像站列表"了
///
/// 参考实现是**自动测速择优**：用户只选发行版，面板自己并发测一遍、挑最快的那个。
/// 原先我们让用户先点「探测最快镜像」、再对着十几行"清华 TUNA 0.16 秒 / 南大 …"
/// 自己挑 —— 那是把**面板该做的事**推给了用户（用户原话："不需要那些什么清华源
/// 之类的"）。现在测速在**选中发行版时自动开始**，界面上只留一行状态；
/// 镜像站名字进安装日志（出问题时那才是排查的地方）。
fn mirror_block(
    state: &AppState,
    form: &InstallForm,
    entity: &Entity<Shell>,
    cx: &App,
) -> AnyElement {
    let refresh = {
        let entity = entity.clone();
        Button::new("mirror-refresh")
            .label(if state.mirrors.loading {
                "正在拉取…"
            } else {
                "刷新清单"
            })
            .small()
            .on_click(move |_, window, cx| {
                entity.update(cx, |shell, cx| shell.refresh_mirror_catalog(window, cx));
            })
    };

    // 下拉框：显示 `Ubuntu 24.04 (12 镜像源)`，取到的是清单里的 id。
    let picker = Select::new(&form.mirror_select)
        .placeholder("选一个发行版（自动测速择优）")
        .w_full()
        .menu_max_h(px(320.));

    // 状态行：正在拉 / 拉失败（原话）/ 清单概览
    let status: AnyElement = if state.mirrors.loading {
        div()
            .text_xs()
            .text_color(theme::text_muted())
            .child("正在拉取发行版清单…")
            .into_any_element()
    } else if let Some(error) = state.mirrors.load_error.as_ref() {
        div()
            .text_xs()
            .text_color(theme::warning())
            .child(format!("清单没拉到：{error}"))
            .into_any_element()
    } else if !state.mirrors.offers.is_empty() {
        div()
            .text_xs()
            .text_color(theme::text_dim())
            .child(format!(
                "共 {} 个可装版本。{}{}",
                state.mirrors.offers.len(),
                state.mirrors.origin,
                state
                    .mirrors
                    .updated
                    .as_ref()
                    .map(|time| format!("（清单更新于 {time}）"))
                    .unwrap_or_default()
            ))
            .into_any_element()
    } else {
        div()
            .text_xs()
            .text_color(theme::text_dim())
            .child("还没拉到清单 —— 点「刷新清单」，或者直接在下面填一个 rootfs URL。")
            .into_any_element()
    };

    // 测速状态：正在测 / 已选好 / 全都拿不到。
    //
    // ⚠️ **不写镜像站名字**（用户要求"不需要那些什么清华源之类的"）：
    // 只给"选好了 + 多大"，名字在安装日志里（`从 <site> 下载 …` 那一步）。
    let probe_line: AnyElement = if state.mirrors.probing {
        div()
            .text_xs()
            .text_color(theme::text_muted())
            .child("正在自动测速，挑最快的镜像…")
            .into_any_element()
    } else if let Some(error) = state.mirrors.error.as_ref() {
        div()
            .text_xs()
            .text_color(theme::warning())
            .child(error.clone())
            .into_any_element()
    } else if let Some(choice) = state.mirrors.chosen.as_ref() {
        div()
            .text_xs()
            .text_color(theme::success())
            .child(format!(
                "已自动测速选好镜像（{} 个来源里最快的那个{}）",
                state.mirrors.results.len(),
                choice
                    .bytes
                    .map(|bytes| format!("，{}", mirrors::human_bytes(bytes)))
                    .unwrap_or_default()
            ))
            .into_any_element()
    } else {
        div()
            .text_xs()
            .text_color(theme::text_dim())
            .child("选中发行版后会自动测速挑最快的镜像。也可以在下面直接填 URL。")
            .into_any_element()
    };

    v_flex()
        .w_full()
        .gap_2()
        .child(
            h_flex()
                .w_full()
                .gap_2()
                .items_center()
                .child(refresh)
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child("选择发行版（在线发行版 · 镜像源）"),
                ),
        )
        .child(status)
        .child(picker)
        .child(probe_line)
        .child(form_field(
            "install-mirror-url",
            "自定义下载地址（留空 = 用自动测速挑出来的；.wsl 包和 tar 都支持）",
            &form.mirror_url,
            cx,
            true,
        ))
        .into_any_element()
}

/// 一句"已经跑了多久"。
fn elapsed_label(state: &AppState) -> AnyElement {
    let text = state
        .installing
        .as_ref()
        .map(|progress| {
            let mut parts = vec![format!(
                "已用 {}",
                crate::util::human_secs(progress.elapsed_secs)
            )];
            if let Some(percent) = progress.percent() {
                parts.push(format!("{percent:.1}%"));
            }
            if let Some(speed) = progress.speed() {
                parts.push(speed);
            }
            parts.join(" · ")
        })
        .unwrap_or_default();

    div()
        .text_xs()
        .text_color(theme::text_muted())
        .child(text)
        .into_any_element()
}

/// 安装日志卡片：步骤 + 进度 + 输出 + 结局。
///
/// 装完之后**留在页面上**（不自动清空）：失败了用户要读日志，
/// 成功了也能对一下"到底跑了哪些步骤"。下次点「再装一个」时被替换掉。
fn install_log_card(
    state: &AppState,
    source: InstallSourceKind,
    entity: &Entity<Shell>,
) -> AnyElement {
    let Some(progress) = state.installing.as_ref() else {
        return div().into_any_element();
    };

    let head = h_flex()
        .w_full()
        .gap_2()
        .items_center()
        .justify_between()
        .child(
            div()
                .text_sm()
                .text_color(theme::text())
                .child(if progress.total == 0 {
                    format!("正在准备安装 {}…", progress.name)
                } else {
                    format!(
                        "第 {} / {} 步：{}",
                        progress.index, progress.total, progress.step_label
                    )
                }),
        )
        .child(elapsed_label(state));

    // 已经走完的步骤：一眼能看出卡在哪一步
    let finished: Vec<AnyElement> = progress
        .finished
        .iter()
        .map(|step| {
            let (mark, color) = if step.ok {
                ("✔", theme::success())
            } else {
                ("✘", theme::danger())
            };
            div()
                .text_xs()
                .text_color(color)
                .child(format!("{mark} {}", step.label))
                .into_any_element()
        })
        .collect();

    let outcome: AnyElement = match &progress.outcome {
        None => div().into_any_element(),
        Some(InstallOutcome::Success(message)) => div()
            .text_sm()
            .text_color(theme::success())
            .child(format!("✔ {message}"))
            .into_any_element(),
        Some(InstallOutcome::Failed { detail, step }) => div()
            .text_sm()
            .text_color(theme::danger())
            .child(format!(
                "✘ 安装失败{}{detail}",
                step.as_ref()
                    .map(|step| format!("（{step}）"))
                    .unwrap_or_default()
            ))
            .into_any_element(),
        Some(InstallOutcome::Cancelled) => div()
            .text_sm()
            .text_color(theme::warning())
            .child("已取消 —— 已经装到一半的东西不会自动回收，去实例列表看一眼")
            .into_any_element(),
    };

    // 微软商店那条路失败时的**一键出口**：换成镜像源，并尽量选中同一个发行版。
    //
    // 为什么值得单独做个按钮：那条路依赖 `wsl.exe` 自己的网络（实测：它的清单
    // 拉不到时商店与 `--web-download` 会双双报 `WININET_E_CANNOT_CONNECT`），
    // 而镜像源是**我们自己去国内镜像站下载**、不经过 wsl 的网络 —— 同一个发行版
    // 往往在那边也有（Ubuntu/Debian/Alpine/Arch/Fedora/…）。让用户自己去
    // 清单里翻一遍、再手动对齐版本，是没必要付出的代价。
    let fallback: AnyElement = if source.is_online()
        && matches!(progress.outcome, Some(InstallOutcome::Failed { .. }))
    {
        let entity = entity.clone();
        h_flex()
            .w_full()
            .items_center()
            .gap_3()
            .child(
                Button::new("install-switch-to-mirror")
                    .label("改用「在线发行版（镜像源）」再试")
                    .small()
                    .primary()
                    .on_click(move |_, window, cx| {
                        entity.update(cx, |shell, cx| shell.switch_to_mirror_source(window, cx));
                    }),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme::text_dim())
                    .child("那条是面板自己从国内镜像站下载，不依赖 wsl 的网络。"),
            )
            .into_any_element()
    } else {
        div().into_any_element()
    };

    // ⚠️ 滚动容器必须带 `.id(...)`（见 online_block 的注释）。
    //
    // 只画**最后** 400 行：日志上限是 2000 行，一帧往界面里塞 2000 个元素
    // 是白白的开销，而用户真正要看的就是尾部（拉取那边的日志上限也是 200 行）。
    const SHOWN: usize = 400;
    let shown_from = progress.log.len().saturating_sub(SHOWN);
    let tail = &progress.log[shown_from..];

    let log: AnyElement = if tail.is_empty() {
        div().into_any_element()
    } else {
        v_flex()
            .id("install-log")
            .w_full()
            .max_h(px(260.))
            .overflow_y_scroll()
            .gap_1()
            .children(tail.iter().map(|line| {
                div()
                    .font_family("Consolas")
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(line.clone())
            }))
            .into_any_element()
    };

    let log_note = if shown_from > 0 {
        format!(
            "已收到 {} 行输出（只显示最后 {SHOWN} 行）",
            progress.log.len()
        )
    } else {
        format!("已收到 {} 行输出", progress.log.len())
    };

    card(
        "安装过程",
        v_flex()
            .w_full()
            .gap_2()
            .child(head)
            .children(finished)
            .child(outcome)
            .child(fallback)
            .child(log)
            .child(
                div()
                    .text_xs()
                    .text_color(theme::text_dim())
                    .child(log_note),
            ),
    )
    .into_any_element()
}

// ---------------------------------------------------------------------------
// ⑤ 应用设置
// ---------------------------------------------------------------------------

/// 应用设置页。
///
/// 这里管的是**本程序自己的偏好**
/// （`%LOCALAPPDATA%\wslc-panel\prefs.json`），
/// 和「wlsc 配置」页管的 `wslc` 的 `settings.yaml` 是两回事 ——
/// 分开放是刻意的，见 `prefs.rs` 的模块说明。
///
/// v0.3 只做两项（都是"简单且立刻见效"的）：自动刷新间隔、界面主题。
/// 后来加了「终端」卡 —— 它不是纯数据：自定义路径那个输入框住在
/// `Shell` 里（`InputState::new` 要 `&mut Window`），所以这一页现在收
/// `&Shell` 而不是 `&AppState`（和「添加实例」页同一个理由）。
pub fn app_settings(shell: &Shell, entity: &Entity<Shell>) -> AnyElement {
    let state = &shell.state;
    let content: AnyElement = match state.settings_tab {
        SettingsTab::General => settings_general(shell, entity),
        // 「高级」和「WSL」就是把原先那两个独立页面搬进来当 tab ——
        // 内容一行没改，只是不再各占一个侧边栏入口。
        SettingsTab::Advanced => config(state, entity),
        SettingsTab::Wsl => wsl_config(state, entity),
    };

    v_flex()
        .w_full()
        .gap_4()
        .child(settings_tabs(state, entity))
        .child(content)
        .into_any_element()
}

/// 设置页的 tab 行。
///
/// 用 Button 而不是 gpui-kit 的 `TabBar`：项目现有的"选中态"就是这么做的
/// （见 [`theme_card`]），风格一致，而且是**已经编译通过**的形式。
/// 想换成 `TabBar` 的话，替换点只有这一个函数。
fn settings_tabs(state: &AppState, entity: &Entity<Shell>) -> AnyElement {
    let buttons: Vec<AnyElement> = SettingsTab::ALL
        .iter()
        .map(|tab| {
            let tab = *tab;
            let entity = entity.clone();
            // id 用 `{:?}`：稳定且与显示语言无关
            let mut button = Button::new(SharedString::from(format!("settings-tab-{tab:?}")))
                .label(tab.label())
                .small()
                .on_click(move |_, _, cx| {
                    entity.update(cx, |shell, cx| shell.set_settings_tab(tab, cx));
                });
            if state.settings_tab == tab {
                button = button.primary();
            }
            button.into_any_element()
        })
        .collect();

    h_flex()
        .w_full()
        .gap_2()
        .children(buttons)
        .into_any_element()
}

/// 「常规」tab：界面 + 主题 + 终端。
///
/// 「关于」卡片从这里**搬走了** —— 它现在是一个独立的侧边栏页面
/// （见 [`about`]），因为那一页只有只读信息，混在设置里容易被当成开关。
fn settings_general(shell: &Shell, entity: &Entity<Shell>) -> AnyElement {
    let state = &shell.state;
    v_flex()
        .w_full()
        .gap_4()
        .child(interface_card(state, entity))
        .child(terminal_card(shell, entity))
        .child(theme_card(state, entity))
        .into_any_element()
}

/// 「关于」页：应用介绍、版本、构建、地址。
///
/// 单独一页而不是设置里的一个 tab：这里**没有任何开关**，
/// 放进设置会让人以为里面能改东西。
pub fn about(_state: &AppState, _entity: &Entity<Shell>) -> AnyElement {
    const LINKS: &[(&str, &str)] = &[
        ("项目主页", "https://github.com/SilkKirk/wslc-panel"),
        ("问题反馈", "https://github.com/SilkKirk/wslc-panel/issues"),
        (
            "参考项目 owu/wsl-dashboard",
            "https://github.com/owu/wsl-dashboard",
        ),
        ("界面库 gpui-kit", "https://github.com/longbridge/gpui-kit"),
    ];

    let rows: Vec<AnyElement> = LINKS
        .iter()
        .map(|(label, url)| {
            v_flex()
                .w_full()
                .gap_1()
                .child(div().text_xs().text_color(theme::text_dim()).child(*label))
                .child(
                    div()
                        .font_family("Consolas")
                        .text_xs()
                        .text_color(theme::primary())
                        .child(*url),
                )
                .into_any_element()
        })
        .collect();

    v_flex()
        .w_full()
        .gap_4()
        .child(card(
            "wslc-panel",
            v_flex()
                .w_full()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .text_color(theme::text())
                        .child("WSL 容器（wslc）和发行版（wsl.exe）的图形管理面板。"),
                )
                .child(kv("版本", env!("CARGO_PKG_VERSION")))
                .child(kv("构建", crate::short_build_sha()))
                .child(kv("界面库", "gpui-kit 0.7.1（GPUI）"))
                .child(div().text_xs().text_color(theme::text_dim()).child(
                    "许可证 Apache-2.0。参考项目 owu/wsl-dashboard 是 GPL-3.0-only，\
                     只用来理解机制，没有代码进入本仓库。",
                )),
        ))
        .child(card("地址", v_flex().w_full().gap_3().children(rows)))
        .into_any_element()
}

/// 主题选择卡（跟随系统 / 深色 / 浅色）。
///
/// 切换本身在 [`Shell::set_theme`] 里做完（持久化 + 两套配色一起换）——
/// 这里只负责画按钮。以前那一步必须写在点击回调里（`Theme::change` 要
/// `&mut App`），现在 `Shell` 的方法里也能拿到，所以收成一处，
/// 免得"回调里切了组件库、状态里忘了切自绘配色"这类两处不同步的 bug。
fn theme_card(state: &AppState, entity: &Entity<Shell>) -> AnyElement {
    let current = state.prefs.theme;
    // 「跟随系统」要显示系统当前是什么主题 —— 用的是启动时读到的缓存值
    // （点按钮时会在后台再读一次，见 `Shell::set_theme`）。
    let system_label = match state.system_is_dark {
        Some(true) => "系统当前：深色",
        Some(false) => "系统当前：浅色",
        None => "系统当前：没读到（按深色算）",
    };

    let buttons: Vec<AnyElement> = crate::prefs::ThemePref::ALL
        .iter()
        .map(|theme| {
            let theme = *theme;
            let entity = entity.clone();
            // 用 `{:?}` 而不是 `label()` 拼 id：id 要稳定且与显示语言无关。
            let mut button = Button::new(SharedString::from(format!("theme-{theme:?}")))
                .label(theme.label())
                .small()
                .on_click(move |_, _, cx| {
                    entity.update(cx, |shell, cx| shell.set_theme(theme, cx));
                });
            if current == theme {
                button = button.primary();
            }
            button.into_any_element()
        })
        .collect();

    card(
        "界面",
        v_flex()
            .w_full()
            .gap_3()
            .child(kv("主题", current.label()))
            .child(kv("当前配色", theme::mode().label()))
            .child(h_flex().w_full().gap_2().flex_wrap().children(buttons))
            .child(div().text_xs().text_color(theme::text_dim()).child(format!(
                "主题同时作用于自绘的界面和组件库的控件（输入框、按钮……）。{system_label}。"
            )))
            .child(div().text_xs().text_color(theme::text_dim()).child(
                "「跟随系统」读的是 Windows 的应用主题（AppsUseLightTheme 那个键，\
                 不是管任务栏的 SystemUsesLightTheme）；在系统里改完主题之后，\
                 再点一下「跟随系统」（或重启面板）就会跟上。",
            )),
    )
    .into_any_element()
}

// ---------------------------------------------------------------------------
// ⑥ wlsc 配置
// ---------------------------------------------------------------------------

/// 配置页。
///
/// 当前能力：逐项展示 + 预设值快捷写入 + 恢复默认 + 原始 YAML 查看 +
/// 带时间戳备份的保存 + 用系统编辑器打开。
///
/// 任意文本输入的完整表单需要 GPUI 的 `InputState` / `TextInput`，
/// 属于下一步（见 `docs/PLAN.md` M6）。
pub fn config(state: &AppState, entity: &Entity<Shell>) -> AnyElement {
    let Some(doc) = &state.settings else {
        let message = state
            .settings_error
            .clone()
            .unwrap_or_else(|| "尚未加载配置文件".to_owned());
        return v_flex()
            .w_full()
            .gap_4()
            .child(card(
                "wlsc 配置",
                v_flex()
                    .w_full()
                    .gap_3()
                    .child(div().text_sm().text_color(theme::danger()).child(message))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(open_in_editor_button(entity))
                            .child(reload_button(entity)),
                    ),
            ))
            .into_any_element();
    };

    let values = doc.values();
    let dirty = doc.is_dirty();

    let rows: Vec<AnyElement> = SETTING_KEYS
        .iter()
        .map(|key| setting_row(key, values.get(key), entity))
        .collect();

    let path = doc.path().display().to_string();
    let raw = doc.raw().to_owned();

    v_flex()
        .w_full()
        .gap_4()
        .child(card(
            "配置文件",
            v_flex()
                .w_full()
                .gap_3()
                .child(kv("路径", path))
                .child(kv(
                    "状态",
                    if dirty {
                        "有未保存的修改"
                    } else {
                        "已同步"
                    },
                ))
                .child(div().text_xs().text_color(theme::text_dim()).child(
                    "所有配置项的默认值在文件里都是注释状态，表示使用 wslc 内置默认值。\
                             保存前会自动做一次时间戳备份。",
                ))
                .child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .child(open_in_editor_button(entity))
                        .child(save_button(entity, dirty))
                        .child(reload_button(entity)),
                ),
        ))
        .child(card("配置项", v_flex().w_full().gap_2().children(rows)))
        .child(card(
            "原始 YAML",
            // 同 app.rs：滚动容器必须先有 id。
            v_flex()
                .id("settings-raw-yaml")
                .w_full()
                .max_h(px(420.))
                .overflow_y_scroll()
                .rounded_md()
                .bg(theme::bg())
                .p_3()
                .child(
                    div()
                        .font_family("Consolas")
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child(raw),
                ),
        ))
        .into_any_element()
}

// ---------------------------------------------------------------------------
// ⑥b WSL 配置（.wslconfig）
// ---------------------------------------------------------------------------

/// 「WSL 配置」页 —— `%USERPROFILE%\.wslconfig`。
///
/// # 这一页是**只读**的
///
/// 不做编辑，也不提供"一键校验"。原因是"校验"在这里做不到轻量：
/// 实测 WSL 只在 **VM 启动时**读一次 `.wslconfig` 并报出认不出的键，
/// 想主动触发就得先 `wsl --shutdown` —— 那会打断所有正在跑的发行版。
/// 详见 `wslc_core::wslconfig` 的模块说明。
///
/// 所以这一页只做**不需要跑 WSL 就能做**的部分：把文件摊开、
/// 把实测已知放错文件的键指出来、真要改就交给系统编辑器。
pub fn wsl_config(state: &AppState, entity: &Entity<Shell>) -> AnyElement {
    let info = &state.snapshot.wslconfig;

    let path_text = info
        .path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "（拿不到：USERPROFILE 没有定义）".to_owned());

    let open = {
        let entity = entity.clone();
        Button::new("open-wslconfig")
            .label("用系统默认程序打开")
            .small()
            .primary()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.open_wslconfig(cx));
            })
    };

    // 三种状态说的话完全不同：读到了 / 文件不存在 / 读失败
    let body: AnyElement = if let Some(error) = &info.error {
        v_flex()
            .w_full()
            .gap_2()
            .child(empty_state("读不到这个文件"))
            .child(
                div()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(error.clone()),
            )
            .into_any_element()
    } else if let Some(text) = &info.text {
        v_flex()
            // 滚动容器必须先有 id（同 config 页的说明）
            .id("wslconfig-raw")
            .w_full()
            .max_h(px(420.))
            .overflow_y_scroll()
            .rounded_md()
            .bg(theme::bg())
            .p_3()
            .child(
                div()
                    .font_family("Consolas")
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(text.clone()),
            )
            .into_any_element()
    } else {
        v_flex()
            .w_full()
            .gap_2()
            .child(empty_state("还没有这个文件"))
            .child(div().text_xs().text_color(theme::text_dim()).child(
                "没配过 .wslconfig 是完全正常的状态 —— WSL 会用全部默认值。\
                 想从头配一份就点下面的按钮（会先建一个空文件再打开）。",
            ))
            .into_any_element()
    };

    let misplaced: AnyElement = if info.misplaced.is_empty() {
        v_flex()
            .w_full()
            .gap_2()
            .child(
                div()
                    .text_xs()
                    .text_color(theme::text_dim())
                    .child("没有发现实测已知的「键放错文件」写法。"),
            )
            .child(div().text_xs().text_color(theme::text_dim()).child(
                "⚠️ 这并不等于配置没问题 —— 这里只比对实测确认过的那几条，\
                 不是 WSL 的完整键表。宁可少报，不要错报：错报会让人改坏本来正常的配置。",
            ))
            .into_any_element()
    } else {
        let rows: Vec<AnyElement> = info
            .misplaced
            .iter()
            .map(|m| {
                v_flex()
                    .w_full()
                    .gap_1()
                    .p_3()
                    .rounded_md()
                    .bg(theme::bg())
                    .child(
                        div()
                            .font_family("Consolas")
                            .text_xs()
                            .text_color(theme::text())
                            .child(format!("第 {} 行：{}", m.line, m.text)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::text_muted())
                            .child(format!("应该放到 {} —— {}", m.belongs_to, m.why)),
                    )
                    .into_any_element()
            })
            .collect();

        v_flex()
            .w_full()
            .gap_2()
            .child(
                div()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child("这几行 WSL 明确报过「未知键」，会被忽略 —— 配置看着生效、其实没有。"),
            )
            .children(rows)
            .into_any_element()
    };

    v_flex()
        .w_full()
        .gap_4()
        .child(card(
            "文件",
            v_flex()
                .w_full()
                .gap_2()
                .child(kv_block("位置", path_text).into_any_element())
                .child(div().text_xs().text_color(theme::text_dim()).child(
                    "这是 WSL 本身的全局配置（管 WSL2 虚拟机：内存、网络模式、内核命令行……）。\
                     发行版自己的配置是各发行版里的 /etc/wsl.conf，两者互不相干、键也不能混用。",
                )),
        ))
        .child(card("内容", body))
        .child(card("放错文件的键", misplaced))
        .child(card(
            "为什么这里没有「一键校验」",
            v_flex()
                .w_full()
                .gap_2()
                .child(div().text_xs().text_color(theme::text_muted()).child(
                    "实测（WSL 3.0.1.0）：wsl --status / --version / -l -v / --terminate \
                         都不报配置告警；只有 wsl --shutdown 之后第一条进发行版的命令会报，\
                         紧接着的第二、三条就静默了。",
                ))
                .child(div().text_xs().text_color(theme::text_muted()).child(
                    "也就是说这些告警来自 WSL2 虚拟机启动时读一次 .wslconfig。\
                         想主动触发就得先 wsl --shutdown —— 那会打断所有正在跑的发行版。\
                         为了校验一个配置文件付这个代价不值得，所以这里只做不依赖 WSL 的静态检查。",
                )),
        ))
        .child(h_flex().w_full().justify_end().child(open))
        .into_any_element()
}

/// 单个配置项的一行。
fn setting_row(
    key: &'static SettingKey,
    effective: Option<String>,
    entity: &Entity<Shell>,
) -> AnyElement {
    let is_default = effective.is_none();
    let shown = effective.unwrap_or_else(|| format!("默认（{}）", key.default));

    let mut actions = h_flex().flex_none().gap_2();

    // 枚举型：每个候选值一个按钮。
    if key.kind == SettingKind::Enum {
        for choice in key.choices.iter().copied() {
            let entity = entity.clone();
            actions = actions.child(
                Button::new(SharedString::from(format!("set-{}-{}", key.key, choice)))
                    .label(choice)
                    .small()
                    .on_click(move |_, _, cx| {
                        entity.update(cx, |shell, cx| {
                            shell.set_setting(key, Some(choice), cx);
                        });
                    }),
            );
        }
    }

    // 常用预设值。
    for preset in presets_for(key).iter().copied() {
        let entity = entity.clone();
        actions = actions.child(
            Button::new(SharedString::from(format!("preset-{}-{}", key.key, preset)))
                .label(preset)
                .small()
                .on_click(move |_, _, cx| {
                    entity.update(cx, |shell, cx| {
                        shell.set_setting(key, Some(preset), cx);
                    });
                }),
        );
    }

    // 非默认状态才显示"用默认"。
    if !is_default {
        let entity = entity.clone();
        actions = actions.child(
            Button::new(SharedString::from(format!("reset-{}", key.key)))
                .label("用默认")
                .small()
                .on_click(move |_, _, cx| {
                    entity.update(cx, |shell, cx| {
                        shell.set_setting(key, None, cx);
                    });
                }),
        );
    }

    let warning: AnyElement = if key.needs_warning {
        div()
            .text_xs()
            .text_color(theme::warning())
            .child("⚠ 修改此项不会迁移已有的容器与镜像，会在新路径下创建一个空会话。")
            .into_any_element()
    } else {
        div().into_any_element()
    };

    v_flex()
        .w_full()
        .gap_1()
        .py_2()
        .border_b_1()
        .border_color(theme::border())
        .child(
            h_flex()
                .w_full()
                .gap_3()
                .child(
                    div()
                        .w(px(150.))
                        .flex_none()
                        .text_sm()
                        .font_semibold()
                        .text_color(theme::text())
                        .child(key.label),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .text_color(if is_default {
                            theme::text_dim()
                        } else {
                            theme::primary()
                        })
                        .overflow_hidden()
                        .truncate()
                        .child(shown),
                )
                .child(actions),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme::text_dim())
                .child(format!("{}（{}）", key.help, key.key)),
        )
        .child(warning)
        .into_any_element()
}

fn open_in_editor_button(entity: &Entity<Shell>) -> impl IntoElement {
    let entity = entity.clone();
    Button::new("open-settings")
        .label("用系统编辑器打开")
        .primary()
        .on_click(move |_, _, cx| {
            entity.update(cx, |shell, cx| shell.open_settings_in_editor(cx));
        })
}

/// 保存按钮。
///
/// 没有改动时**不返回禁用按钮**，而是一行绿色文字。
/// 原因：gpui-component 的禁用态文字几乎看不清（实机截图确认过），
/// 而"已保存"本身是个**状态**，用文字表达比用灰按钮更准确。
fn save_button(entity: &Entity<Shell>, dirty: bool) -> AnyElement {
    if !dirty {
        return h_flex()
            .items_center()
            .child(
                div()
                    .text_sm()
                    .text_color(theme::success())
                    .child("✓ 已保存"),
            )
            .into_any_element();
    }

    let entity = entity.clone();
    Button::new("save-settings")
        .label("备份并保存")
        .primary()
        .on_click(move |_, _, cx| {
            entity.update(cx, |shell, cx| shell.save_settings(cx));
        })
        .into_any_element()
}

fn reload_button(entity: &Entity<Shell>) -> impl IntoElement {
    let entity = entity.clone();
    Button::new("reload-settings")
        .label("重新加载")
        .on_click(move |_, _, cx| {
            entity.update(cx, |shell, cx| {
                shell.reload_settings(cx);
            });
        })
}

/// "刷新"卡片：**应用自己的偏好**，与 `wslc` 的配置无关。
///
/// 界面上不再到处显示刷新间隔 —— 只在这一处设置。
///
/// 卡片标题刻意叫「刷新」而不是「界面」：主题卡也叫「界面」，
/// 两张同名卡片挨在一起会让人分不清哪个是哪个。
fn interface_card(state: &AppState, entity: &Entity<Shell>) -> AnyElement {
    let path_text = crate::prefs::Prefs::path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "（无法确定偏好文件位置）".to_owned());

    card(
        "刷新",
        v_flex()
            .w_full()
            .gap_3()
            .child(kv(
                "自动刷新间隔",
                format!("{} 秒", state.prefs.refresh_secs),
            ))
            .child(refresh_secs_picker(state, entity))
            .child(
                div()
                    .text_xs()
                    .text_color(theme::text_dim())
                    .child(format!("立即生效，并写入 {path_text}")),
            ),
    )
    .into_any_element()
}

/// 自动刷新间隔选择器（只出现在「应用设置」页）。
///
/// 这是**应用自己的偏好**，不是 `wslc` 的配置 —— 所以它和上面那些
/// `settings.yaml` 的条目在视觉上分开，用的是本地 `prefs` 而不是 `SettingsDoc`。
fn refresh_secs_picker(state: &AppState, entity: &Entity<Shell>) -> impl IntoElement {
    let current = state.prefs.refresh_secs;

    let buttons: Vec<AnyElement> = crate::prefs::REFRESH_PRESETS
        .iter()
        .map(|secs| {
            let secs = *secs;
            let entity = entity.clone();
            let mut button = Button::new(SharedString::from(format!("refresh-{secs}")))
                .label(format!("{secs} 秒"))
                .small()
                .on_click(move |_, _, cx| {
                    entity.update(cx, |shell, cx| {
                        shell.set_refresh_secs(secs, cx);
                    });
                });
            if current == secs {
                button = button.primary();
            }
            button.into_any_element()
        })
        .collect();

    // 手动改过 prefs.json、值不在预设里时，补一个只读提示，
    // 免得用户看到"一个都没选中"而困惑。
    let extra = if crate::prefs::REFRESH_PRESETS.contains(&current) {
        None
    } else {
        Some(
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child(format!("当前：{current} 秒（不在预设中）"))
                .into_any_element(),
        )
    };

    h_flex()
        .w_full()
        .gap_2()
        .flex_wrap()
        .children(buttons)
        .children(extra)
}

/// 「终端」卡：新终端的起始目录。
///
/// # 为什么有这一项
///
/// 不给 `--cd` 时 `wsl.exe` 会继承**调用它的那个进程**的工作目录，WSL 再把它
/// 翻译成 `/mnt/<盘>/...` —— 于是"打开终端"落在哪儿，取决于用户是从哪个文件夹
/// 启动的 `wslc-panel.exe`（本机实测：从 `C:\Users\76434\Downloads` 启动，
/// 终端就落在 `/mnt/c/Users/76434/Downloads`；路径翻译不了时回落 `~`）。
/// 默认因此改成显式的 `--cd ~`，这一卡让用户改。
///
/// # 输入框是懒创建的
///
/// 点过「自定义路径…」才会建（`InputState::new` 要 `&mut Window`，
/// 见 [`Shell::edit_terminal_dir`]）—— 渲染时现建会让用户一个字都打不进去。
fn terminal_card(shell: &Shell, entity: &Entity<Shell>) -> AnyElement {
    let current = &shell.state.prefs.terminal_dir;

    let mut buttons: Vec<AnyElement> = crate::prefs::TerminalStartDir::FIXED
        .iter()
        .map(|dir| {
            let dir = dir.clone();
            // 闭包是 `Fn`（GPUI 会反复调用它），所以里面必须**再 clone 一次** ——
            // 直接把捕获的那个 move 进去会编译不过（`borrow of moved value`），
            // 而且这里 `dir` 后面还要用来判断选中态。
            let for_click = dir.clone();
            let entity = entity.clone();
            // id 用 `{:?}`（ASCII）而不是 label：元素 id 要稳定、
            // 且不该随显示文案变（和主题卡同一个写法）。
            let mut button = Button::new(SharedString::from(format!("terminal-dir-{dir:?}")))
                .label(dir.label())
                .small()
                .on_click(move |_, _, cx| {
                    let dir = for_click.clone();
                    entity.update(cx, |shell, cx| shell.set_terminal_dir(dir, cx));
                });
            if current == &dir {
                button = button.primary();
            }
            button.into_any_element()
        })
        .collect();

    // 「自定义路径…」单独一个按钮：它除了选档位还要**把输入框显示出来**
    // （并在有条件时把焦点给过去）。选中的自定义档位在按钮上高亮 ——
    // 和上面两个固定档位同一套"选中态"表达。
    {
        let entity = entity.clone();
        let mut button = Button::new("terminal-dir-custom")
            .label("自定义路径…")
            .small()
            .on_click(move |_, window, cx| {
                // 和「添加实例」那个按钮一样：`window` 是必需的，
                // 因为要现场创建 `InputState`。
                entity.update(cx, |shell, cx| shell.edit_terminal_dir(window, cx));
            });
        if matches!(current, crate::prefs::TerminalStartDir::Custom(_)) {
            button = button.primary();
        }
        buttons.push(button.into_any_element());
    }

    let mut body = v_flex()
        .w_full()
        .gap_3()
        .child(kv("新终端的起始目录", current.describe()))
        .child(h_flex().w_full().gap_2().flex_wrap().children(buttons))
        .child(div().text_xs().text_color(theme::text_dim()).child(
            "「打开终端」（实例列表）用 `wsl --cd` 指定起点；不给就是继承面板自己的工作目录，\
             也就是从哪个文件夹启动的面板，终端就从哪儿开始。",
        ));

    // 输入框只在点过「自定义路径…」之后存在。它一直留着（不随页面离开销毁），
    // 所以这里只要 `Some` 就画。
    if let Some(input) = shell.terminal_dir_input.as_ref() {
        let apply = {
            let entity = entity.clone();
            Button::new("terminal-dir-apply")
                .label("应用")
                .small()
                .primary()
                .on_click(move |_, _, cx| {
                    entity.update(cx, |shell, cx| shell.apply_terminal_dir(cx));
                })
                .into_any_element()
        };

        body = body
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .child(Input::new(input).id("terminal-dir").w(px(360.)))
                    .child(apply),
            )
            .child(div().text_xs().text_color(theme::text_dim()).child(
                crate::util::strip_md(
                    "填发行版里的路径（`/home/wt/project` 或 `~`）—— Windows 路径不认，\
                     那是「面板所在目录」那一档的事。写错的路径 `wsl.exe` 不报错，只会落在 `/`。",
                ),
            ));
    }

    body = body.child(div().text_xs().text_color(theme::text_dim()).child(
        crate::util::strip_md(
            "容器的「打开终端」走 `wslc exec -w`，只对**绝对路径**生效：容器里没有\
             「Linux home」这个概念，`~` 交给镜像自己的工作目录决定。改动只影响之后新开的终端。",
        ),
    ));

    card("终端", body).into_any_element()
}

/// 拉取进度（v0.6.2 之后内联在「镜像」页的「正在拉取」卡片里）。
///
/// 只渲染最后 [`PROGRESS_TAIL_LINES`] 行、且**不滚动** ——
/// 拉取的输出动辄上千行，滚动容器会把用户带到最老的那几行，
/// 而这里永远显示"刚刚发生了什么"。
///
/// 它**不是浮层**：页面本身就是"正在拉"的上下文，再盖一层没有额外信息
/// （v0.6.2 之前「拉取镜像」弹窗的进度形态也复用了它，那个弹窗已经删了）。
fn pull_progress_body(progress: &PullProgress, entity: &Entity<Shell>) -> AnyElement {
    /// 进度区显示多少行。
    const PROGRESS_TAIL_LINES: usize = 12;

    let tail: Vec<AnyElement> = progress
        .lines
        .iter()
        .skip(progress.lines.len().saturating_sub(PROGRESS_TAIL_LINES))
        .map(|line| {
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child(line.clone())
                .into_any_element()
        })
        .collect();

    let body = if tail.is_empty() {
        vec![
            div()
                .text_xs()
                .text_color(theme::text_dim())
                .child("等待输出…")
                .into_any_element(),
        ]
    } else {
        tail
    };

    let abort = {
        let entity = entity.clone();
        Button::new("pull-abort")
            .label("取消拉取")
            .small()
            .danger()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.cancel_pull(cx));
            })
    };

    v_flex()
        .w_full()
        .gap_3()
        .child(
            div()
                .text_sm()
                .text_color(theme::text())
                .child(format!("正在拉取 {} ……", progress.image)),
        )
        .child(
            v_flex()
                .w_full()
                .gap_1()
                .rounded_md()
                .bg(theme::bg())
                .p_3()
                .children(body),
        )
        .child(
            h_flex()
                .w_full()
                .justify_between()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_dim())
                        .child(format!("已收到 {} 行输出", progress.lines.len())),
                )
                .child(abort),
        )
        .into_any_element()
}

/// 「创建容器」弹窗里那条**从镜像读来的**信息（声明端口 / 启动命令）。
///
/// 它是**锦上添花**：读不到照样能建容器（端口自己填），所以失败时只给一句
/// 说明，不用红字 —— 红字是留给"要出事了"的（`AGENTS.md` 的界面约定）。
///
/// 数据来自 `wslc image inspect --format json`（`Shell::probe_image_facts`
/// 在后台读、读到了顺手把端口预填），解析在 `wslc-core`
/// （`model::parse_image_inspect`，配两个真机 fixture 单测）。
fn image_facts_hint(dialog: &CreateDialog) -> AnyElement {
    match &dialog.facts {
        // 用户自己敲的镜像引用：我们没去读，就什么都不说。
        FactsState::Idle => div().into_any_element(),
        FactsState::Loading => div()
            .text_xs()
            .text_color(theme::text_dim())
            .child("正在读镜像信息…")
            .into_any_element(),
        FactsState::Failed(error) => div()
            .text_xs()
            .text_color(theme::text_dim())
            .child(format!(
                "读不到镜像信息：{error}（镜像还不在本地时就是这样 —— 端口请自己填）"
            ))
            .into_any_element(),
        FactsState::Ready(facts) => {
            let ports = if facts.exposed_ports.is_empty() {
                "这个镜像没有声明端口；要访问它得自己写端口映射（例如 8080:80）。".to_owned()
            } else {
                let list = facts
                    .exposed_ports
                    .iter()
                    .map(|port| format!("{port}/tcp"))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "镜像声明监听 {list} —— 建议 {}（上面端口那格为空时已替你填好，可改）。",
                    wslc_core::model::suggest_port_mappings(&facts.exposed_ports)
                )
            };

            let mut body = v_flex()
                .w_full()
                .gap_1()
                .child(div().text_xs().text_color(theme::text_dim()).child(ports));
            if !facts.command.trim().is_empty() {
                // 说清"这是**镜像自带**的"：下面有一格同名的输入框
                // （`create-command`），两处都写"启动命令"用户会分不清哪个生效。
                //
                // ⚠️ 这一行**不能**读成"这个镜像跑得起来" —— 它拼的是
                // `Entrypoint + Cmd`。有 CMD 的（nginx / mysql）留空就是用它；
                // **只有 ENTRYPOINT 的**（`eclipse-temurin:*-jre-alpine` 就是，
                // 这里显示的是 `/__cacert_entrypoint.sh`）留空**起来就退**。
                // 所以两种情况给两句不同的话（v0.9.10 起 `ImageFacts::has_cmd`
                // 能把它们分开；`confirm_create` 也会拦住后者留空提交）。
                let text = if facts.has_cmd {
                    format!(
                        "镜像自带的启动命令：{}（「启动命令」那格留空时用它）",
                        facts.command
                    )
                } else {
                    format!(
                        "⚠️ 这个镜像**没有默认命令**（只有 ENTRYPOINT：{}）——\
                         「启动命令」那格**必须自己填**，留空的话容器起来就会立刻退出。\
                         想让它先待着等部署就填 `tail -f /dev/null`，\
                         想直接跑应用就填你的启动命令。",
                        facts.command
                    )
                };
                body = body
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::text_dim())
                            // 这两句里都有 Markdown（`**没有默认命令**` /
                            // `**必须自己填**` / 反引号）→ 渲染处统一清掉
                            .child(crate::util::strip_md(&text)),
                    );
            }
            body.into_any_element()
        }
    }
}

/// 「容器名」那格下面的提示：**留空时它会叫什么**。
///
/// 名字是按镜像推出来的（[`wslc_core::model::suggest_container_name`]）——
/// 不显示出来的话，用户得等建完才在列表里看到名字，而那时已经晚了
/// （实机反馈：随机名看不出是哪个镜像起的）。
///
/// 用的是和 `to_spec` 完全一样的输入，所以提示里的名字、下面等效命令里的
/// `--name`、以及真正执行的名字三者不会不一致。
/// 用户自己填了名字就不显示（那时候这行只会是噪音）。
fn auto_name_hint(dialog: &CreateDialog, state: &AppState, cx: &App) -> AnyElement {
    if !dialog.name.read(cx).value().trim().is_empty() {
        return div().into_any_element();
    }
    let image = dialog.image.read(cx).value().trim().to_owned();
    if image.is_empty() {
        return div().into_any_element();
    }
    let name = wslc_core::model::suggest_container_name(&image, &state.container_names());
    if name.is_empty() {
        return div().into_any_element();
    }

    div()
        .text_xs()
        .text_color(theme::text_dim())
        .child(format!("留空就按镜像命名：{name}"))
        .into_any_element()
}

/// 「环境变量」那行下面的提示：**这个镜像第一次启动必须有**哪个变量。
///
/// 只对预设表里认识的镜像显示（mysql / mariadb / postgres）—— 这类信息
/// `wslc image inspect` 不会告诉你，而少给一个变量的后果是**容器起来就退出**、
/// 原因只写在日志里（实机就是这么踩的：mysql 一直退，翻日志才看到缺
/// `MYSQL_ROOT_PASSWORD`）。
///
/// 这一行是**照着输入框实时算**的（渲染时读值），所以：
///
/// - 从下拉框选 / 从「镜像」页进来 → 变量已经被预填，这里说"把值补上"；
/// - **自己手打**镜像引用 → 我们不会预填（预填只挂在"知道镜像了"那两个钩子上，
///   见 `Shell::probe_image_facts`），这里就改说"请加上 `KEY=你的值`"。
///
/// 判断"预填在不在"用的是最朴素的字符串包含：这里只是一句提示，
/// 值不值得为它把 `env` 的解析规则再抄一遍。
fn image_env_hint(dialog: &CreateDialog, cx: &App) -> AnyElement {
    let image = dialog.image.read(cx).value().trim().to_owned();
    let Some(preset) = wslc_core::model::image_preset(&image) else {
        return div().into_any_element();
    };
    let Some(prefill) = preset.prefill_env else {
        return div().into_any_element();
    };

    let alternatives: Vec<&str> = preset
        .required_any_env
        .iter()
        .copied()
        .filter(|key| *key != prefill)
        .collect();
    let or_else = if alternatives.is_empty() {
        String::new()
    } else {
        format!("（也可以改用 {}）", alternatives.join(" / "))
    };

    let raw = dialog.env.read(cx).value().to_owned();
    let text = if raw.contains(&format!("{prefill}=")) {
        format!("这个镜像第一次启动必须给 {prefill}：变量名已经预填，把值补上{or_else}")
    } else {
        format!("这个镜像第一次启动必须给 {prefill}，请在环境变量里加上 `{prefill}=你的值`{or_else}")
    };

    div()
        .text_xs()
        .text_color(theme::text_dim())
        .child(text)
        .into_any_element()
}

/// 「创建容器」弹窗的**表单字段**：标签 + 输入框。
///
/// `full` 决定宽度：直接放进 `v_flex` 的用 `w_full`，
/// 放进 `h_flex` 成对排列的用 `flex_1`。
/// （在 `v_flex` 里写 `flex_1` 会把字段**竖向**拉长，是个容易踩的坑。）
fn form_field(
    id: &'static str,
    label: &'static str,
    input: &Entity<InputState>,
    cx: &App,
    full: bool,
) -> AnyElement {
    let field = if full {
        v_flex().w_full()
    } else {
        v_flex().flex_1().min_w_0()
    };

    // 取值只为在为空时把标签调暗一点，让"必填未填"看得见。
    let empty = input.read(cx).value().trim().is_empty();

    field
        .gap_1()
        .child(
            div()
                .text_xs()
                .text_color(if empty {
                    theme::text_muted()
                } else {
                    theme::text_dim()
                })
                .child(label),
        )
        .child(Input::new(input).id(id).w_full())
        .into_any_element()
}

/// 拉取策略选择器。
fn pull_policy_row(current: &PullPolicy, entity: &Entity<Shell>) -> AnyElement {
    let options: [(&str, &str, PullPolicy); 3] = [
        ("never", "只用本地镜像", PullPolicy::Never),
        ("missing", "缺了就拉", PullPolicy::Missing),
        ("always", "总是拉取", PullPolicy::Always),
    ];

    let buttons: Vec<AnyElement> = options
        .into_iter()
        .map(|(id, label, policy)| {
            let entity = entity.clone();
            // `PullPolicy` 是 `Copy`，直接按值捕获即可
            // （写 `.clone()` 会被 clippy 的 `clone_on_copy` 抓）。
            let selected = *current == policy;

            let mut button = Button::new(SharedString::from(format!("create-pull-{id}")))
                .label(label)
                .small()
                .on_click(move |_, _, cx| {
                    entity.update(cx, |shell, cx| {
                        shell.set_create_pull(policy, cx);
                    });
                });
            if selected {
                button = button.primary();
            }
            button.into_any_element()
        })
        .collect();

    h_flex()
        .w_full()
        .gap_2()
        .child(
            div()
                .text_xs()
                .text_color(theme::text_dim())
                .child("镜像拉取策略"),
        )
        .children(buttons)
        .into_any_element()
}

/// 等效命令预览。
///
/// 直接调 `RunSpec::to_args()` 生成 —— 和真正执行时用的是**同一段代码**，
/// 不会出现"预览和执行不一致"。用户也可以直接复制到终端复现。
fn command_preview(spec: &RunSpec) -> AnyElement {
    let command = format!("wslc {}", spec.to_args().join(" "));

    v_flex()
        .w_full()
        .gap_1()
        .child(
            div()
                .text_xs()
                .text_color(theme::text_dim())
                .child("等效命令（与实际执行完全一致）"),
        )
        .child(
            div()
                .w_full()
                .rounded_md()
                .bg(theme::bg())
                .p_3()
                .font_family("Consolas")
                .text_xs()
                .text_color(theme::text_muted())
                .child(command),
        )
        .into_any_element()
}

/// 「创建容器」弹窗。
///
/// 表单字段直接对应 [`RunSpec`]，底部用 `to_args()` 实时预览等效命令。
///
/// **强制后台运行**（`-d`）：不带 `-d` 时 `wslc run` 会前台阻塞，
/// 而我们的子进程有超时，超时后会把刚建好的容器连带杀掉。
/// 需要前台交互的场景请用 `wslc` 自己的命令。
pub fn create_dialog_overlay(
    dialog: &CreateDialog,
    state: &AppState,
    entity: &Entity<Shell>,
    cx: &App,
) -> AnyElement {
    // ⚠️ 这里传了现有容器名：容器名留空时 `to_spec` 会按镜像推一个
    // （`nginx` / `nginx-2`），推出来的名字**同时出现在下面的等效命令里** ——
    // 预览和实际执行读同一个函数，所以不会不一致。
    let spec = dialog.to_spec(&state.container_names(), cx);
    let image_missing = spec.image.trim().is_empty();

    let cancel = {
        let entity = entity.clone();
        Button::new("create-cancel")
            .label("取消")
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.close_create_dialog(cx));
            })
    };

    let confirm = {
        let entity = entity.clone();
        Button::new("create-ok")
            .label("创建并启动")
            .primary()
            .on_click(move |_, window, cx| {
                // `window` 是必需的：提交时要检查"预填了但没填值"的必需变量，
                // 并把焦点送到那一格（`confirm_create` 里有说明）。
                entity.update(cx, |shell, cx| shell.confirm_create(window, cx));
            })
    };

    modal_scrim("create-dialog",
        v_flex()
            // 字段多，窗口矮时允许滚动，别把底部按钮挤没
            .id("create-dialog-card")
            .w(px(780.))
            .max_h(px(880.))
            .overflow_y_scroll()
            .gap_4()
            .p_5()
            .rounded_lg()
            .bg(theme::bg_card())
            .border_1()
            .border_color(theme::border())
            .child(
                div()
                    .text_lg()
                    .font_bold()
                    .text_color(theme::text())
                    .child("创建容器"),
            )
            .child(
                // 左边是唯一的事实来源（可以手打、可以粘完整引用），
                // 右边那个下拉框只是**从本地已有的镜像里挑一个**的快捷方式 ——
                // 数据来自每 3 秒刷新的快照，不用手打一长串 registry 前缀
                // （手打时最容易漏的就是它，漏了 `wslc run` 会去 Docker Hub 找，
                // 而那台机器上不可达）。
                h_flex()
                    .w_full()
                    .gap_2()
                    .child(form_field(
                        "create-image",
                        "镜像引用（必填，例如 nginx:latest）",
                        &dialog.image,
                        cx,
                        false,
                    ))
                    .child(
                        v_flex()
                            .gap_1()
                            .w(px(300.))
                            .flex_none()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme::text_dim())
                                    .child("或从本地镜像里选（可搜索）"),
                            )
                            .child(
                                Select::new(&dialog.image_select)
                                    // 占位符说的是"还没选"，不是"没有镜像"。
                                    // v0.6.5 把**空状态**文案写成了占位符，于是
                                    // 展开明明有 3 个镜像、触发器却写着"本地还没有镜像"。
                                    .placeholder("选一个本地镜像…")
                                    // ⚠️ 菜单**默认和触发器一样宽**（`menu_width`
                                    // 默认 `Length::Auto`），而镜像引用比这 300px
                                    // 长得多 —— 不加宽就是每个候选都带省略号。
                                    .menu_width(px(460.))
                                    .empty(|_, _| {
                                        div()
                                            .px_3()
                                            .py_2()
                                            .text_sm()
                                            .text_color(theme::text_muted())
                                            .child("本地还没有镜像 —— 先去「镜像」页拉一个")
                                    })
                                    .w_full()
                                    .menu_max_h(px(320.)),
                            ),
                    ),
            )
            .child(image_facts_hint(dialog))
            .child(form_field(
                "create-name",
                "容器名（留空按镜像命名）",
                &dialog.name,
                cx,
                true,
            ))
            .child(auto_name_hint(dialog, state, cx))
            .child(form_field(
                "create-command",
                "启动命令（留空用镜像自己的，支持管道与 &&）",
                &dialog.command,
                cx,
                true,
            ))
            .child(
                h_flex()
                    .w_full()
                    .gap_3()
                    .child(form_field(
                        "create-ports",
                        "端口映射（逗号或换行分隔，如 8080:80）",
                        &dialog.ports,
                        cx,
                        false,
                    ))
                    .child(form_field(
                        "create-env",
                        "环境变量 KEY=VALUE（逗号或换行分隔）",
                        &dialog.env,
                        cx,
                        false,
                    )),
            )
            .child(image_env_hint(dialog, cx))
            .child(
                h_flex()
                    .w_full()
                    .gap_3()
                    .child(form_field(
                        "create-volumes",
                        "卷挂载 卷名:/容器内路径",
                        &dialog.volumes,
                        cx,
                        false,
                    ))
                    .child(form_field(
                        "create-network",
                        "网络（留空用 bridge）",
                        &dialog.network,
                        cx,
                        false,
                    )),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap_3()
                    .child(form_field(
                        "create-memory",
                        "内存上限（如 512M）",
                        &dialog.memory,
                        cx,
                        false,
                    ))
                    .child(form_field(
                        "create-cpus",
                        "CPU 数（如 0.5）",
                        &dialog.cpus,
                        cx,
                        false,
                    )),
            )
            .child(pull_policy_row(&dialog.pull, entity))
            .child(
                v_flex()
                    .w_full()
                    .gap_1()
                    .rounded_md()
                    .bg(theme::bg())
                    .p_3()
                    .child(div().text_xs().text_color(theme::text_muted()).child(
                        "容器以「后台方式」(-d) 启动。前台模式会一直占着子进程，\
                                       超时后连容器一起被杀，所以这里不提供。",
                    ))
                    .child(div().text_xs().text_color(theme::warning()).child(
                        "实测本机连不上 Docker Hub：请先在「镜像」页用加速地址\
                                       拉好镜像，再把这里的策略选成「只用本地镜像」。",
                    )),
            )
            .child(command_preview(&spec))
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .text_color(if image_missing {
                                theme::warning()
                            } else {
                                theme::text_dim()
                            })
                            .child(if image_missing {
                                "还差一个镜像引用"
                            } else {
                                ""
                            }),
                    )
                    .child(h_flex().gap_2().child(cancel).child(confirm)),
            ),
    )
    .into_any_element()
}

/// 容器详情弹窗。
///
/// 挂载、端口这些占地方的信息在这儿看全；动作只放**列表行里没有**的那几个
/// （强杀 / 执行命令 / attach 主进程），高频动作留在列表行。
///
/// ⚠️ 这里原先写着"列表里不放操作按钮，操作按钮集中在这儿" —— 那是照 1Panel
/// 写的，但本项目的列表行一直有操作按钮（启动 / 重启 / 停止 / 日志 / 删除），
/// 于是详情里又摆了一份一模一样的，实机上看起来就是"同一排按钮出现两次"。
/// 现在按"不重复"来分：列表放高频的，详情放低频或危险的。
///
/// ⚠️ v0.9.14 起列表行自己又分了两层（行内 4 个 + 「更多」菜单），
/// 这份"不重复"的账要按 `container_row_actions` / `container_more_menu`
/// 那两个函数来算 —— 终端也在那一行的「更多」里，所以详情里照样没有它。
///
/// 挂载用 `kv_block`（换行不截断）：`E:\code → /etc/nginx/conf.d/`
/// 这种长路径用 `kv` 会被截成 `E:\...\con...`，等于没显示。
pub fn container_detail_overlay(
    name: &str,
    state: &AppState,
    entity: &Entity<Shell>,
) -> AnyElement {
    let Some(summary) = state
        .snapshot
        .all
        .iter()
        .find(|c| c.item.display_name() == name)
    else {
        // 容器刚被删掉 —— 弹窗自己消失，别留一个空壳
        return div().into_any_element();
    };

    let item = &summary.item;
    let running = item.is_running();

    // -- 动作 --------------------------------------------------------------
    //
    // ⚠️ **列表行里已经有的动作，这里一个都不重复。**
    //
    // 容器列表那一行给的是高频动作：**启动 / 停止**在行内，
    // **终端 / 重启 / 日志 / 删除** 在那一行的「更多」菜单里（v0.9.14 起）。
    // 详情里再摆一遍没有意义 —— 同一个动作两个入口，用户还得判断该点哪个，
    // 而两个入口背后是同一个 `ImmediateAction` / `PendingAction`，行为完全一样。
    // （这一点是**实机看出来的**：发布版里停止的容器在列表行和详情里各有一份
    // 一模一样的「启动 / 日志 / 删除」。）
    //
    // ⚠️ v0.6.3：「打开终端」从详情**挪到了列表行**。理由是实测的使用路径 ——
    // 建完容器想进去干活时，得"点名字开详情 → 找按钮"两次点击，而它其实是
    // 运行中的容器最高频的动作。挪上去之后这里就**不再重复**它了
    // （`AGENTS.md` §5 那条规矩）。
    //
    // 详情留给**列表里没有、而且低频或危险**的那几个：强杀（列表里只有优雅停止）、
    // 执行命令（非交互）、attach 主进程。
    //
    // 所以**停止的容器在详情里一个动作都没有** —— 那是对的，它能做的几件事
    // 列表行里全有。下面给一句话说明，免得看起来像按钮坏了。
    //
    // 发行版详情是同一条规矩（那边不重复 打开终端 / 启动 / 终止 / 设为默认）。
    let mut actions: Vec<AnyElement> = Vec::new();
    if running {
        actions.push(
            danger_button(
                "detail-kill",
                "强杀",
                PendingAction::KillContainer(name.to_owned()),
                entity,
            )
            .into_any_element(),
        );
        actions.push(overlay_button(
            "detail-exec",
            "执行命令",
            {
                let target = name.to_owned();
                move |shell, window, cx| shell.open_exec(target.clone(), window, cx)
            },
            entity,
        ));
        // `attach` 是"开一个新终端窗口"——同步调用，不等它关闭
        // （用户可能在里面待几个小时，见 `AGENTS.md` §7.1）。
        actions.push(
            immediate_button(
                "detail-attach",
                "attach 主进程",
                ImmediateAction::AttachContainer(name.to_owned()),
                entity,
            )
            .into_any_element(),
        );
    }

    // 一行说明：其余动作为什么不在这儿。
    // 没有它，用户会以为详情里少了按钮（或者按钮没画出来）。
    //
    // ⚠️ v0.9.14 起列表行里多了一层「更多」菜单 —— 这句话必须**指到菜单**
    // （终端 / 重启 / 日志 / 删除 都在那儿），否则用户按老习惯去行内找，
    // 找了一圈只看到"启动/停止"，会以为功能被删了。
    let hint = if running {
        "终端 / 重启 / 日志 / 删除 在列表那一行的「更多」菜单里（停止在行内）"
    } else {
        "这个容器已经停止 —— 启动在行内，日志 / 删除 在那一行的「更多」菜单里"
    };

    let close = {
        let entity = entity.clone();
        Button::new("detail-close")
            .label("关闭")
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.close_detail(cx));
            })
    };

    // 端口在这里显示**完整形式**（含绑定地址），和列表里的精简形式区分开：
    // 列表看的是"映射了多少"，详情看的是"到底绑在哪"。
    let ports_text = {
        let ports = item.port_mappings();
        if ports.is_empty() {
            "—".to_owned()
        } else {
            ports
                .iter()
                .map(|p| p.display())
                .collect::<Vec<_>>()
                .join("  ")
        }
    };

    // -- inspect 那一份（挂载 / 启动命令 / 环境变量**只有它有**）----------------
    //
    // ⚠️ 这三样在列表那一份数据里都没有：`list` 的 `Command` 被 wslc **截到 20 字符**
    //（`"docker-entrypoint.s…"`）、挂载只有卷名（`mysqldata`，没有容器内路径），
    // 环境变量则完全没有。所以打开详情时后台拉一次 inspect
    //（`Shell::open_detail` → `DetailInspect`）。
    let detail = state
        .detail_inspect
        .as_ref()
        .filter(|d| d.container == name);
    let inspect = detail.and_then(|d| d.inspect.as_ref());
    let inspect_error = detail.and_then(|d| d.error.as_deref());

    // 一项"只有 inspect 才知道"的信息。**三种状态分开画**，别都塌成 `—`：
    //   还没拉回来 → "正在读取…"（塌成 `—` 的话用户会以为这容器就没有）
    //   拉失败     → 把原因说出来（inspect 失败多半是容器刚被删掉）
    //   拉到了但空 → `—`（这才真的是"没有"）
    let inspect_field = |label: &'static str, value: Option<String>| -> AnyElement {
        if let Some(text) = value {
            return kv_block(label, text).into_any_element();
        }
        if inspect.is_some() {
            return kv(label, "—").into_any_element();
        }
        match inspect_error {
            Some(error) => kv_block(label, format!("读不到：{error}")).into_any_element(),
            None => kv(label, "正在读取…").into_any_element(),
        }
    };

    // 启动命令：`Entrypoint` + `Cmd` 接起来 —— 运行时就是这么拼的。
    // 只有 `Entrypoint` 的镜像（`-jre-alpine` 那类）在这里一眼看得出来。
    let command_line = inspect.and_then(|i| i.command_line());
    // 挂载：**优先 inspect**（列表 / labels 那条路实测是空的，
    // 见 `ContainerInspect::mounts_summary` 的表格）；拿不到才回落到旧的摘要。
    let mounts = inspect
        .and_then(|i| i.mounts_summary())
        .or_else(|| item.mounts_summary());
    // 环境变量：一行一条（`kv_block` 换行显示，卡片自己有 `overflow_y_scroll`）。
    //
    // ⚠️ 值**原样显示**，包括 `MYSQL_ROOT_PASSWORD=…` 这类：和「数据库管理」页
    // 把密码画在列表里一致，也和 Docker Desktop 的 inspect 一致 —— 这是用户
    // 自己机器上的屏幕，来查 env 的人要的就是值本身。
    // 要改成"密码类键打码、点一下才显示"的话，只改这一处的取值即可。
    let env = inspect
        .map(|i| i.env())
        .filter(|lines| !lines.is_empty())
        .map(|lines| lines.join("\n"));

    let mut rows: Vec<AnyElement> = vec![
        kv("ID", item.id().to_owned()).into_any_element(),
        kv("镜像", item.image.clone()).into_any_element(),
        kv("状态", item.status.clone()).into_any_element(),
        // ⚠️ 这一格是 `RunningFor`，实测是**创建至今**（三个容器都对得上 `CreatedAt`），
        // 不是运行时长 —— 真正的 uptime 就在上一行 `Status` 里（`Up 2 hours`）。
        // 原先标成「运行时长」，于是"状态 Up 2 hours"和"运行时长 45 hours ago"
        // 并排出现、看着自相矛盾（v0.9.7 实机反馈）。
        kv("已创建", item.running_for.clone()).into_any_element(),
        kv_block("端口", ports_text).into_any_element(),
        inspect_field("挂载", mounts),
        inspect_field("启动命令", command_line),
        inspect_field("环境变量", env),
    ];

    if let Some(stats) = summary.stats.as_ref() {
        rows.push(kv("CPU", stats.cpu_perc.clone()).into_any_element());
        rows.push(kv("内存", stats.mem_usage.clone()).into_any_element());
        rows.push(kv("网络 I/O", stats.net_io.clone()).into_any_element());
        rows.push(kv("PID", stats.pids.to_string()).into_any_element());
    }

    modal_scrim("container-detail",
        v_flex()
            .id("detail-card")
            .w(px(700.))
            .max_h(px(860.))
            .overflow_y_scroll()
            .gap_4()
            .p_5()
            .rounded_lg()
            .bg(theme::bg_card())
            .border_1()
            .border_color(theme::border())
            .child(
                div()
                    .text_lg()
                    .font_bold()
                    .text_color(theme::text())
                    .child(name.to_owned()),
            )
            .child(v_flex().w_full().gap_2().children(rows))
            .child(
                v_flex()
                    .w_full()
                    .gap_2()
                    // 先说明"其余动作在列表里"，再是这一页独有的那几个。
                    // 顺序反过来的话，停止的容器那一条会只剩右下角一个「关闭」，
                    // 看起来像界面坏了。
                    .child(div().text_xs().text_color(theme::text_dim()).child(hint))
                    .child(
                        h_flex()
                            .w_full()
                            .justify_between()
                            .items_center()
                            .child(h_flex().gap_2().flex_wrap().children(actions))
                            .child(close),
                    ),
            ),
    )
    .into_any_element()
}

/// 发行版详情弹窗。
///
/// **低频或危险**的动作在这儿：压缩 / 移动 / 调整大小 / 导出 / 删除，
/// 外加只读的「打开安装位置」「编辑配置」。高频动作（打开终端 / 启动 /
/// 终止 / 设为默认）只在列表行里 —— 同一件事给两个入口，用户还得判断点哪个。
/// 和容器详情同一套取舍。
///
/// 「安装位置」「虚拟磁盘」用 `kv_block`（换行不截断）：
/// `D:\linux\Ubuntu-26.04` 这种路径在列表里被截成 `D:\linux\Ubu...`，
/// 等于没显示，而这里正是要看清它。
///
/// 另外这里**替列表行承担了一件事**：如果这个发行版正被本面板吊着
/// （`state.kept_alive`），在「状态」下面加一句说明 —— 那个标记原先画在
/// 列表行的状态列里，会溢到隔壁「版本」列上（见 [`distro_row`]）。
pub fn distro_detail_overlay(name: &str, state: &AppState, entity: &Entity<Shell>) -> AnyElement {
    let Some(distro) = state.snapshot.distros.iter().find(|d| d.name == name) else {
        // 发行版刚被删掉 —— 弹窗自己消失，别留一个空壳
        return div().into_any_element();
    };

    let is_default = state.default_distro() == Some(distro.name.as_str());
    let running = distro.state.is_running();
    // 过渡态（安装 / 卸载 / 转换中）下 `wsl` 会拒绝写操作，
    // 所以那几种状态下干脆不显示写操作按钮。
    let transitional = distro.state.is_transitional();
    // 本项目**只支持 WSL 2**。正常的机器上这永远是 false；
    // 但如果真有个 WSL 1 发行版，它的磁盘操作（VHDX 那套）全都不成立，
    // 与其让用户点了看报错，不如不给按钮 + 明说原因。
    let is_wsl1 = distro.version == Some(1);
    // 「它现在是不是被**本面板**吊着的」（[`KeepAlive`](crate::app)）。
    //
    // 这件事原先标在列表行的状态列里，实机发现那个徽标会盖到隔壁「版本」列上
    // （见 `distro_row`）。它是低频信息 —— 只在点了「启动」之后才成立 ——
    // 所以搬到这儿来，列表行只留状态徽标。
    let kept = state.kept_alive.iter().any(|n| n == name);

    // -- 动作 --------------------------------------------------------------

    // ⚠️ **列表行里已经有的动作，这里一个都不重复。**
    //
    // 发行版列表那一行给的是：打开终端 / 启动 / 终止 / 设为默认。
    // 详情留给"低频、危险、需要看清代价"的操作：压缩 / 移动 / 调整大小 /
    // 导出 / 删除，外加只读的「打开安装位置」和「编辑配置」。
    //
    // 「设为默认」原先**两边都有** —— 而这一行注释当时只列了
    // 「打开终端 / 启动 / 终止」，把它漏了。实机看出来之后删掉：
    // 它的条件（`!is_default && !transitional`）和列表行那一份完全一样，
    // 删掉不丢任何能力。
    let mut actions: Vec<AnyElement> = Vec::new();

    // `--compact` 要求发行版处于**已停止**状态；运行中给出这个按钮是误导。
    // WSL 1 根本没有 VHDX，压缩无从谈起。
    if !running && !transitional && !is_wsl1 {
        actions.push(
            danger_button_distro(
                "distro-detail-compact",
                "压缩磁盘",
                DistroAction::Compact(name.to_owned()),
                entity,
            )
            .into_any_element(),
        );
    }

    // -- `wsl --manage` 的另外几项（P4）--
    //
    // 低频操作，所以只出现在详情里，不塞进列表行。
    // （「打开终端 / 启动 / 终止」反过来 —— 高频，只在列表行里。）
    if !transitional {
        actions.push(prompt_button(
            "distro-detail-move",
            "移动位置",
            PromptKind::MoveDistro,
            name,
            entity,
        ));

        // 调整大小是**VHDX 专属**的，WSL 1 上没有意义
        if !is_wsl1 {
            actions.push(prompt_button(
                "distro-detail-resize",
                "调整大小",
                PromptKind::ResizeDistro,
                name,
                entity,
            ));
        }

        // 「默认用户」搬去 `/etc/wsl.conf` 的编辑弹窗了（`[user] default`）。
        // 两个入口会让人不知道该改哪个，所以这里只留那一个。

        // 导出对 WSL 1 / 2 都成立（它只是把根文件系统打成 tar），
        // 所以**不**受 `is_wsl1` 限制。
        actions.push(prompt_button(
            "distro-detail-export",
            "导出",
            PromptKind::ExportDistro,
            name,
            entity,
        ));

        // 稀疏开关按需求去掉了。日常要开它走 `.wslconfig` 的
        // `[experimental] sparseVhd=true`（见「应用设置 → WSL」）；
        // 单个发行版想临时改，手动跑 `wsl --manage <名字> --set-sparse` 也能做。
    }

    // 这里**没有**「转为 WSL 1/2」：本项目只支持 WSL 2，
    // 不做版本转换（也就没有那条要搬整个根文件系统、几十分钟的操作）。
    //
    // 但如果机器上真有个 WSL 1 发行版，得让用户明白**为什么**它的
    // 磁盘操作是灰的 —— 那句说明在下面的「信息」区里。

    if !transitional {
        actions.push(
            danger_button_distro(
                "distro-detail-unregister",
                "删除",
                DistroAction::Unregister {
                    name: name.to_owned(),
                    vhdx_bytes: distro.vhdx_bytes,
                },
                entity,
            )
            .into_any_element(),
        );
    }

    // 「打开安装位置」是**只读**操作，所以和其他动作分开摆到左边。
    let reveal = {
        let entity = entity.clone();
        let target = name.to_owned();
        Button::new("distro-detail-reveal")
            .label("打开安装位置")
            .small()
            .on_click(move |_, _, cx| {
                let target = target.clone();
                entity.update(cx, |shell, cx| shell.reveal_distro_path(target, cx));
            })
    };

    // 「编辑配置」也是这一组：它读/写的是发行版**里面**的 `/etc/wsl.conf`，
    // 不改发行版本身的注册信息。
    //
    // 注意：这个文件在 `.wslconfig`（`%USERPROFILE%`，整机一份）里**不存在**
    // —— 见 `wslc_core::wslconfig` 的说明。所以入口摆在发行版详情里，
    // 而不是「WSL 配置」页（那一页管的是前者）。
    let edit_conf = {
        let entity = entity.clone();
        let target = name.to_owned();
        Button::new("distro-detail-wslconf")
            .label("编辑配置（/etc/wsl.conf）")
            .small()
            .on_click(move |_, window, cx| {
                let target = target.clone();
                entity.update(cx, |shell, cx| shell.open_wslconf(target, window, cx));
            })
    };

    let close = {
        let entity = entity.clone();
        Button::new("distro-detail-close")
            .label("关闭")
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.close_distro_detail(cx));
            })
    };

    // -- 信息 --------------------------------------------------------------

    let location = distro
        .base_path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "—".to_owned());
    let vhdx = distro
        .vhdx_path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "—".to_owned());
    let disk = distro
        .vhdx_bytes
        .map(format_bytes)
        .unwrap_or_else(|| "—".to_owned());
    let uid = distro
        .default_uid
        .map(|u| u.to_string())
        .unwrap_or_else(|| "—".to_owned());

    let mut rows: Vec<AnyElement> = vec![
        kv("状态", distro.state.label()).into_any_element(),
        kv("WSL 版本", distro.version_label()).into_any_element(),
        kv("默认发行版", if is_default { "是" } else { "否" }).into_any_element(),
        kv("默认用户 UID", uid).into_any_element(),
        kv("磁盘（虚拟）", disk).into_any_element(),
        kv_block("安装位置", location).into_any_element(),
        kv_block("虚拟磁盘", vhdx).into_any_element(),
    ];

    if is_wsl1 {
        rows.push(
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child(
                    "⚠️ 这是 WSL 1 发行版。本程序只支持 WSL 2，\
                     所以压缩 / 稀疏 / 调整大小这些磁盘操作对它不适用，已经隐藏。",
                )
                .into_any_element(),
        );
    }

    if kept {
        // 插在「状态」下面而不是追到末尾：它解释的正是上面那个「运行中」——
        // 为什么一个没人用的发行版会一直跑着。文案里要写清**关掉面板之后**
        // 会怎样，那是用户拿不到答案、只能猜的那一半。
        rows.insert(
            1,
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child(
                    "它现在由本面板吊住（点了「启动」）。关掉本面板它不会跟着退出，\
                     发行版会继续跑；要让它停下来，用「终止」。",
                )
                .into_any_element(),
        );
    }

    let mut buttons: Vec<AnyElement> =
        vec![reveal.into_any_element(), edit_conf.into_any_element()];
    buttons.extend(actions);

    modal_scrim("distro-detail",
        v_flex()
            .id("distro-detail-card")
            .w(px(760.))
            .max_h(px(860.))
            .overflow_y_scroll()
            .gap_4()
            .p_5()
            .rounded_lg()
            .bg(theme::bg_card())
            .border_1()
            .border_color(theme::border())
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_lg()
                            .font_bold()
                            .text_color(theme::text())
                            .child(name.to_owned()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::text_dim())
                            .child("低频动作都收在这里；列表行里只留了最常用的几个。"),
                    ),
            )
            .child(v_flex().w_full().gap_2().children(rows))
            .child(
                v_flex()
                    .w_full()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .font_semibold()
                            .text_color(theme::text_dim())
                            .child("操作"),
                    )
                    .child(h_flex().w_full().gap_2().flex_wrap().children(buttons)),
            )
            .child(h_flex().w_full().justify_end().child(close)),
    )
    .into_any_element()
}

/// 单输入框提示弹窗（移动位置 / 调整大小 / 设置默认用户）。
///
/// 弹窗**本身就是确认**：里面带着这个动作的代价说明和**实时**等效命令，
/// 提交后直接执行 —— 所以没有再叠一层二次确认。
/// 连续弹两个窗比一个信息充分的窗更烦人。
///
/// 要 `cx` 才能从输入框里读数：底部那行命令是随打字变化的。
pub fn prompt_overlay(shell: &Shell, entity: &Entity<Shell>, cx: &App) -> AnyElement {
    let Some(prompt) = shell.prompt.as_ref() else {
        return div().into_any_element();
    };

    let kind = prompt.kind;
    let typed = prompt.input.read(cx).value().trim().to_owned();
    // 没填时给个占位符，别让预览行变成一条断掉的命令
    let shown = if typed.is_empty() {
        "<待填>".to_owned()
    } else {
        typed
    };
    // 等效命令**逐种拼**，不抽一个"flag"出来：导出的 `--export` 不是
    // `--manage` 的子命令、创建卷走的甚至是另一个可执行文件（`wslc`），
    // 硬塞进同一个模板反而要写一堆特例。
    let preview = match kind {
        PromptKind::MoveDistro => format!("wsl --manage {} --move {shown}", prompt.distro),
        PromptKind::ResizeDistro => format!("wsl --manage {} --resize {shown}", prompt.distro),
        PromptKind::SetDefaultUser => {
            format!("wsl --manage {} --set-default-user {shown}", prompt.distro)
        }
        PromptKind::ExportDistro => format!("wsl --export {} {shown}", prompt.distro),
        PromptKind::CreateVolume => format!("wslc volume create {shown}"),
    };

    // 「创建卷」不属于任何发行版（卷是全局的），所以那一行只在有主体时画。
    // 画一行空的"发行版："比不画更让人困惑。
    let subject: Option<AnyElement> = (!prompt.distro.is_empty()).then(|| {
        div()
            .text_xs()
            .text_color(theme::text_dim())
            .child(format!("发行版：{}", prompt.distro))
            .into_any_element()
    });

    let cancel = {
        let entity = entity.clone();
        Button::new("prompt-cancel")
            .label("取消")
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.close_prompt(cx));
            })
    };

    let confirm = {
        let entity = entity.clone();
        Button::new("prompt-confirm")
            .label(kind.confirm_label())
            .small()
            .primary()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.submit_prompt(cx));
            })
    };

    // 只有"要填路径"的那两种才给「浏览…」——
    // 「调整大小」填的是 `50GB`，「设置默认用户」填的是用户名，
    // 给它们这个按钮只会让人以为该去选一个文件。
    let browse = kind.pick_target().map(|_| {
        let entity = entity.clone();
        Button::new("prompt-browse")
            .label("浏览…")
            .small()
            .on_click(move |_, window, cx| {
                entity.update(cx, |shell, cx| shell.browse_prompt_path(window, cx));
            })
            .into_any_element()
    });

    modal_scrim("prompt",
        v_flex()
            .w(px(620.))
            .gap_4()
            .p_5()
            .rounded_lg()
            .bg(theme::bg_card())
            .border_1()
            .border_color(theme::border())
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_lg()
                            .font_bold()
                            .text_color(theme::text())
                            .child(kind.title()),
                    )
                    .children(subject),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_end()
                    // `flex_1 + min_w_0`：让输入框吃掉剩余宽度，
                    // 又在很窄的时候允许它收缩（不写 min_w_0 会顶出去）
                    .child(div().flex_1().min_w_0().child(form_field(
                        "prompt-input",
                        kind.label(),
                        &prompt.input,
                        cx,
                        true,
                    )))
                    .children(browse),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(crate::util::strip_md(kind.note())),
            )
            .child(
                v_flex()
                    .w_full()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::text_dim())
                            .child("等效命令"),
                    )
                    .child(
                        div()
                            .font_family("Consolas")
                            .text_xs()
                            .text_color(theme::text())
                            .child(preview),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .justify_end()
                    .gap_2()
                    .child(cancel)
                    .child(confirm),
            ),
    )
    .into_any_element()
}

/// 等宽文本块（日志 / 命令输出共用）。
///
/// 两处的形状完全一样：可滚动、等宽、深色底，差异只有内容和 id。
///
/// `id` 必须由调用方给，而且**两个浮层不能重名** —— 它们可能同时开着
/// （日志和 exec 都从容器详情里进）。滚动容器**必须先有 `.id(...)`**：
/// `overflow_y_scroll` 来自 `StatefulInteractiveElement`，只对带 id 的
/// 元素可用，漏了是**编译错误**（`AGENTS.md` §7.5）。
///
/// 三种状态分得很清楚，因为它们对用户的意思完全不同：
/// `None` = 还没拉过（正在加载）、`Some("")` = 拉到了但是空的、
/// `Some(内容)` = 有内容。
fn log_text(text: Option<&str>, id: &'static str) -> AnyElement {
    match text {
        Some(t) if !t.trim().is_empty() => v_flex()
            .id(id)
            .w_full()
            .max_h(px(420.))
            .overflow_y_scroll()
            .rounded_md()
            .bg(theme::bg())
            .p_3()
            .child(
                div()
                    .font_family("Consolas")
                    .text_xs()
                    .text_color(theme::text())
                    // 日志里的空行和缩进是有意义的，所以**不** strip、
                    // 也**不**逐行拆开重排 —— 整段交给一个 `div`。
                    .child(t.to_owned()),
            )
            .into_any_element(),
        Some(_) => empty_state("（没有输出）").into_any_element(),
        None => div()
            .text_sm()
            .text_color(theme::text_dim())
            .child("正在加载…")
            .into_any_element(),
    }
}

/// 「容器日志」浮层。
///
/// # 为什么这里没有「跟随」
///
/// `wslc logs --follow` **永不返回**。把它内嵌进来就等于一条永远不结束的
/// 后台任务，还得自己实现"停止跟随"（去杀子进程），而输出照样只能攒在
/// 一个文本块里。开一个真正的终端窗口既简单又更好用 —— 用户可以滚、
/// 可以 Ctrl+C 停。所以这里只做**快照**，跟随交给「在终端里跟随」，
/// 那个按钮用的是同一份行数 / 时间戳选项。
pub fn logs_overlay(state: &AppState, entity: &Entity<Shell>) -> AnyElement {
    let Some(logs) = state.logs.as_ref() else {
        return div().into_any_element();
    };

    // -- 行数档位 --
    //
    // 选中态用 `primary` 配色表示（项目里不用 `.disabled()`）——
    // 和「添加实例」页选来源那一排是同一套写法。
    let tail_buttons: Vec<AnyElement> = LogsState::TAILS
        .iter()
        .enumerate()
        .map(|(ix, tail)| {
            let tail = *tail;
            let entity = entity.clone();
            let mut button = Button::new(SharedString::from(format!("logs-tail-{ix}")))
                .label(LogsState::tail_label(tail))
                .small()
                .on_click(move |_, _, cx| {
                    entity.update(cx, |shell, cx| shell.set_logs_tail(tail, cx));
                });
            if logs.tail == tail {
                button = button.primary();
            }
            button.into_any_element()
        })
        .collect();

    let timestamps = {
        let entity = entity.clone();
        let mut button = Button::new("logs-timestamps")
            .label("显示时间戳")
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.toggle_logs_timestamps(cx));
            });
        if logs.timestamps {
            button = button.primary();
        }
        button.into_any_element()
    };

    // 「刷新」在加载期间改文案而不是置灰：置灰需要一个我们没导入的 trait，
    // 而且"点了没反应"比"正在加载"更让人困惑。
    let refresh = {
        let entity = entity.clone();
        Button::new("logs-refresh")
            .label(if logs.loading {
                "加载中…"
            } else {
                "刷新"
            })
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.reload_logs(cx));
            })
            .into_any_element()
    };

    let follow = {
        let entity = entity.clone();
        let name = logs.container.clone();
        Button::new("logs-follow")
            .label("在终端里跟随")
            .small()
            .on_click(move |_, _, cx| {
                let name = name.clone();
                entity.update(cx, |shell, cx| {
                    shell.run_immediate(ImmediateAction::FollowContainerLogs(name), cx);
                });
            })
            .into_any_element()
    };

    let close = {
        let entity = entity.clone();
        Button::new("logs-close")
            .label("关闭")
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.close_logs(cx));
            })
            .into_any_element()
    };

    // -- 正文 --
    //
    // 失败时**保留**上一次的内容（`log_text` 拿到的还是旧的那份）：
    // 用户宁可看着旧日志，也不想面对一片空白。错误单独占一行。
    let body: AnyElement = match logs.error.as_ref() {
        Some(err) => v_flex()
            .w_full()
            .gap_2()
            .child(
                div()
                    .text_sm()
                    .text_color(theme::danger())
                    .child(format!("读取失败：{err}")),
            )
            .child(log_text(logs.text.as_deref(), "logs-body"))
            .into_any_element(),
        None => log_text(logs.text.as_deref(), "logs-body"),
    };

    // 日志为空时**解释一句** —— 空不等于"坏了"。
    //
    // ⚠️ 这条是 v0.9.8 实机反馈加的（"java 容器日志…看不到"）：那个容器的主进程是
    // 面板为「吊住」它而起的 `tail -f /dev/null`，它本身**不打印任何东西**，
    // 所以这里是空的、而且永远会是空的。而应用自己的输出**不在这里**：
    // 进程形态的部署把它重定向到了目标目录的 `app.log`（「部署管理」里那一行的
    // 「日志」）。不写这两句，用户只会以为日志功能坏了。
    let empty_hint: AnyElement = if logs.error.is_none()
        && logs.text.as_deref().is_none_or(|t| t.trim().is_empty())
    {
        div()
            .w_full()
            .text_sm()
            .text_color(theme::text_dim())
            .child(
                "容器主进程没有往 stdout / stderr 写东西时就是这个样子 —— 比如面板为了\
                 「吊住」容器而起的 `tail -f /dev/null`，它本身不打印任何东西。\
                 部署的应用日志不在这一份里：那是应用自己的输出，在「部署管理」\
                 那一行的「日志」里看。",
            )
            .into_any_element()
    } else {
        div().into_any_element()
    };

    modal_scrim("logs",
        v_flex()
            .id("logs-card")
            .w(px(820.))
            .max_h(px(760.))
            .gap_4()
            .p_5()
            .rounded_lg()
            .bg(theme::bg_card())
            .border_1()
            .border_color(theme::border())
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_lg()
                            .font_bold()
                            .text_color(theme::text())
                            .child(format!("日志 —— {}", logs.container)),
                    )
                    .child(div().text_xs().text_color(theme::text_dim()).child(
                        "这是一份快照，不会自动更新（点「刷新」重新拉）。\
                                 要实时跟着看就点「在终端里跟随」，它会开一个独立窗口。",
                    )),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .flex_wrap()
                    .items_center()
                    .children(tail_buttons)
                    .child(timestamps)
                    .child(refresh)
                    .child(follow),
            )
            .child(body)
            .child(empty_hint)
            .child(h_flex().w_full().justify_end().child(close)),
    )
    .into_any_element()
}

/// 「容器 exec」浮层：跑一条命令，把输出显示出来。
///
/// # 这不是终端
///
/// `wslc exec` **不带 `-it`**，所以没有 TTY：`vi` / `top` / `less` 这类
/// 需要终端的程序在这里用不了（要么直接报错、要么立刻退出）。
/// 那是「打开终端」那个按钮的事 —— 两边的说明里都写清楚了，
/// 免得用户以为这里能顶替终端。
///
/// 命令走 `/bin/sh -c`，所以管道、重定向、`&&` 都能用（见
/// [`wslc_panel_core::state::exec_argv`] 的说明）。
pub fn exec_overlay(shell: &Shell, entity: &Entity<Shell>, cx: &App) -> AnyElement {
    let Some(exec) = shell.state.exec.as_ref() else {
        return div().into_any_element();
    };
    // 输入框是懒创建的：两个都在才算浮层是"开着的"。
    let Some(input) = shell.exec_input.as_ref() else {
        return div().into_any_element();
    };

    // 执行期间把「运行」**换掉**而不是置灰（项目里不用 `.disabled()`）——
    // 置灰的按钮会让人反复去点，一行"执行中…"说清了正在发生什么。
    let run_area: AnyElement = if exec.running {
        div()
            .text_sm()
            .text_color(theme::text_muted())
            .child("执行中…")
            .into_any_element()
    } else {
        let entity = entity.clone();
        Button::new("exec-run")
            .label("运行")
            .small()
            .primary()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.run_exec(cx));
            })
            .into_any_element()
    };

    let open_shell = {
        let entity = entity.clone();
        let name = exec.container.clone();
        Button::new("exec-open-shell")
            .label("打开终端")
            .small()
            .on_click(move |_, _, cx| {
                let name = name.clone();
                entity.update(cx, |shell, cx| {
                    shell.run_immediate(ImmediateAction::OpenContainerShell(name), cx);
                });
            })
            .into_any_element()
    };

    let attach = {
        let entity = entity.clone();
        let name = exec.container.clone();
        Button::new("exec-attach")
            .label("attach 主进程")
            .small()
            .on_click(move |_, _, cx| {
                let name = name.clone();
                entity.update(cx, |shell, cx| {
                    shell.run_immediate(ImmediateAction::AttachContainer(name), cx);
                });
            })
            .into_any_element()
    };

    let close = {
        let entity = entity.clone();
        Button::new("exec-close")
            .label("关闭")
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.close_exec(cx));
            })
            .into_any_element()
    };

    // -- 输出 --
    let output: AnyElement = match exec.error.as_ref() {
        Some(err) => v_flex()
            .w_full()
            .gap_2()
            .child(
                div()
                    .text_sm()
                    .text_color(theme::danger())
                    .child(format!("执行失败：{err}")),
            )
            .child(log_text(exec.output.as_deref(), "exec-output"))
            .into_any_element(),
        None => log_text(exec.output.as_deref(), "exec-output"),
    };

    // 回显这次输出是哪条命令的 —— 连着跑两条之后，没有这一行就分不清
    // 下面那块输出属于哪一次。
    let echo: Option<AnyElement> = (!exec.last_command.is_empty()).then(|| {
        h_flex()
            .w_full()
            .gap_2()
            .items_center()
            .child(
                div()
                    .text_xs()
                    .text_color(theme::text_dim())
                    .child("上一次执行"),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .font_family("Consolas")
                    .text_xs()
                    .text_color(theme::text_muted())
                    .overflow_hidden()
                    .truncate()
                    .child(exec.last_command.clone()),
            )
            .into_any_element()
    });

    modal_scrim("exec",
        v_flex()
            .id("exec-card")
            .w(px(760.))
            .max_h(px(720.))
            .gap_4()
            .p_5()
            .rounded_lg()
            .bg(theme::bg_card())
            .border_1()
            .border_color(theme::border())
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_lg()
                            .font_bold()
                            .text_color(theme::text())
                            .child(format!("执行命令 —— {}", exec.container)),
                    )
                    .child(div().text_xs().text_color(theme::text_dim()).child(
                        "命令交给容器里的 /bin/sh 执行，所以管道、重定向、&& 都能用。\
                         没有 TTY —— vi、top、less 这类要用终端的程序请点「打开终端」。",
                    )),
            )
            .child(form_field("exec-command", "命令", input, cx, true))
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .flex_wrap()
                    .items_center()
                    .child(run_area)
                    .child(open_shell)
                    .child(attach),
            )
            .children(echo)
            .child(output)
            .child(h_flex().w_full().justify_end().child(close)),
    )
    .into_any_element()
}

/// 导出进度浮层。
///
/// # 为什么它和拉取镜像的浮层长得不一样
///
/// 拉取那边显示的是 **docker 的输出行**（每层一行，信息量在行里）。
/// 导出这边 `wsl --export` 几乎不打进度，但产物是个文件 ——
/// **文件大小是真实且连续增长的**，直接量它比解析输出有用得多，
/// 用户看到的也是"还要写多少"。
pub fn export_overlay(state: &AppState, entity: &Entity<Shell>) -> AnyElement {
    let Some(progress) = state.exporting.as_ref() else {
        return div().into_any_element();
    };

    let written = progress
        .written
        .map(format_bytes)
        .unwrap_or_else(|| "还没开始写入".to_owned());

    let last = if progress.last_line.trim().is_empty() {
        "（暂无输出）"
    } else {
        progress.last_line.trim()
    };

    let cancel = {
        let entity = entity.clone();
        Button::new("export-cancel")
            .label("取消导出")
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.cancel_export(cx));
            })
    };

    modal_scrim("export",
        v_flex()
            .w(px(680.))
            .gap_4()
            .p_5()
            .rounded_lg()
            .bg(theme::bg_card())
            .border_1()
            .border_color(theme::border())
            .child(
                div()
                    .text_lg()
                    .font_bold()
                    .text_color(theme::text())
                    .child(format!("正在导出 {}", progress.name)),
            )
            .child(
                v_flex()
                    .w_full()
                    .gap_2()
                    .child(kv_block("目标文件", progress.path.clone()).into_any_element())
                    .child(kv("已写入", written).into_any_element())
                    .child(
                        kv("已用时", format!("{} 秒", progress.elapsed_secs)).into_any_element(),
                    ),
            )
            .child(div().text_xs().text_color(theme::text_dim()).child(
                "wsl 不报告导出百分比，所以「已写入」是目标文件当前的大小 —— \
                     它是连续增长的，可以据此估还要多久。",
            ))
            .child(
                div()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(format!(
                        "导出期间请勿关机。取消会留下一个不完整的 tar，需要你自己删。\
                             最后一行输出：{last}"
                    )),
            )
            .child(h_flex().w_full().justify_end().child(cancel)),
    )
    .into_any_element()
}

// ---------------------------------------------------------------------------
// ⑧ 发行版配置（/etc/wsl.conf）
// ---------------------------------------------------------------------------

/// 「这一项显示的是 WSL 的默认值」的小标记。
///
/// 参考项目没有这个 —— 它的 UI 把每个字段都 `unwrap_or(默认值)` 再写回，
/// 用户根本分不清"我看到的是默认"还是"明确设过"。
fn default_tag(explicit: bool) -> Option<AnyElement> {
    if explicit {
        return None;
    }
    Some(badge("默认", theme::text_dim(), theme::bg()).into_any_element())
}

/// 表单里的一行。
///
/// 不需要 `cx`：`Input::new(input)` / `Checkbox::new(id)` 都只吃 `Entity`，
/// 不用 `App`。（原先它多收一个 `cx` 却在函数里写着 `let _ = cx;` ——
/// 那是"我其实不需要它"的标记，一并去掉了。）
fn wslconf_row(
    state: &WslConfState,
    dialog: &WslConfDialog,
    field: &'static wslconf::Field,
    entity: &Entity<Shell>,
) -> AnyElement {
    let explicit = state.doc.is_explicit(field.section, field.key);

    // 只读字段（目前只有 `[boot] systemd`）：**不画勾选框**。
    //
    // 项目约定是不导入 `Disableable`、全项目不用 `.disabled()`；而且一个
    // 灰掉的勾选框会让人以为"能点，只是暂时不能"。直接显示当前值 + 说明
    // 更诚实 —— 这一项我们只负责**原样写回**，不负责改。
    if field.read_only {
        return v_flex()
            .w_full()
            .gap_1()
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme::text_muted())
                            .child(field.label),
                    )
                    .child(badge("只读", theme::text_dim(), theme::bg())),
            )
            .child(
                div()
                    .font_family("Consolas")
                    .text_xs()
                    .text_color(theme::text())
                    .child(format!(
                        "{} = {}",
                        field.key,
                        state.doc.effective(field.section, field.key)
                    )),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme::text_dim())
                    .child(field.hint),
            )
            .into_any_element();
    }

    match field.kind {
        wslconf::FieldKind::Bool => {
            let entity = entity.clone();
            Checkbox::new(crate::state::wslconf_field_id(field.section, field.key))
                .label(field.label)
                .checked(state.doc.effective_bool(field.section, field.key))
                // `Checkbox` 是**受控**组件：`on_change` 给的是"请求的新值"，
                // 由我们存下来再 notify（见 gpui-kit 的 checkbox 文档）。
                .on_change(move |checked, _, cx| {
                    let value = *checked;
                    entity.update(cx, |shell, cx| {
                        shell.set_wslconf_bool(field.section, field.key, value, cx);
                    });
                })
                .into_any_element()
        }
        wslconf::FieldKind::Text => {
            let mut row = v_flex().w_full().gap_1().child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::text_dim())
                            .child(field.label),
                    )
                    .children(default_tag(explicit)),
            );

            if let Some(input) = dialog.inputs.get(&(field.section, field.key)) {
                row = row.child(
                    Input::new(input)
                        .id(crate::state::wslconf_field_id(field.section, field.key))
                        .w_full(),
                );
            }

            if !field.hint.is_empty() {
                row = row.child(
                    div()
                        .text_xs()
                        .text_color(theme::text_dim())
                        .child(field.hint),
                );
            }
            row.into_any_element()
        }
    }
}

/// 「发行版配置（`/etc/wsl.conf`）」弹窗。
///
/// 表单是**表驱动**的：遍历 [`wslconf::SECTIONS`] / [`wslconf::FIELDS`] 画出来，
/// 所以加字段不用改这里 —— 改 `wslc-core` 的那两张表就够了。
pub fn wslconf_overlay(shell: &Shell, entity: &Entity<Shell>) -> AnyElement {
    let (Some(state), Some(dialog)) = (shell.state.wslconf.as_ref(), shell.wslconf_dialog.as_ref())
    else {
        return div().into_any_element();
    };

    // -- 表单正文：按节分组 --
    let mut sections: Vec<AnyElement> = Vec::new();
    for section in wslconf::SECTIONS {
        // 版本门控：这一节要求的 WSL 版本够不够。
        // 版本读不出来时 `section_supported` 返回 true —— 和参考项目相反，
        // 理由见 `wslc_core::model::wslconf::section_supported`。
        if !wslconf::section_supported(section.name, &state.wsl_version) {
            continue;
        }

        // 注意这里**不能**加 `.copied()`：`FIELDS.iter()` 已经是 `&Field`，
        // 再 `copied()` 会得到 `Field`（值），和 `Vec<&Field>` 对不上。
        // 下面那两处 `fields.iter().copied()` 才对 —— 那里的 Item 是 `&&Field`。
        let fields: Vec<&'static wslconf::Field> = wslconf::FIELDS
            .iter()
            .filter(|f| f.section == section.name)
            .collect();
        if fields.is_empty() {
            continue;
        }

        // 布尔项**两两一行**。16 个字段里一半是开关，一列排下来弹窗太长
        // （用户反馈"上下太长"）—— 勾选框本身很窄，并排一点也不挤。
        let bool_fields: Vec<&'static wslconf::Field> = fields
            .iter()
            .copied()
            .filter(|f| f.kind == wslconf::FieldKind::Bool && !f.read_only)
            .collect();
        let other_fields: Vec<&'static wslconf::Field> = fields
            .iter()
            .copied()
            .filter(|f| f.kind != wslconf::FieldKind::Bool || f.read_only)
            .collect();

        let bool_rows: Vec<AnyElement> = bool_fields
            .chunks(2)
            .map(|pair| {
                let cells: Vec<AnyElement> = pair
                    .iter()
                    .map(|f| {
                        div()
                            .w(px(350.))
                            .child(wslconf_row(state, dialog, f, entity))
                            .into_any_element()
                    })
                    .collect();
                h_flex().w_full().gap_4().children(cells).into_any_element()
            })
            .collect();

        sections.push(
            v_flex()
                .w_full()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .font_semibold()
                        .text_color(theme::text())
                        .child(section.label),
                )
                .children(bool_rows)
                .children(
                    other_fields
                        .iter()
                        .map(|f| wslconf_row(state, dialog, f, entity))
                        .collect::<Vec<_>>(),
                )
                .into_any_element(),
        );
    }

    // -- 校验错误（用户不存在 / 启动命令不存在）--
    let errors: Vec<AnyElement> = state
        .errors
        .iter()
        .map(|e| {
            div()
                .text_xs()
                .text_color(theme::danger())
                .child(format!("⚠️ {e}"))
                .into_any_element()
        })
        .collect();

    // -- 预览：实际会写进去的内容 --
    let preview: AnyElement = if state.show_preview {
        v_flex()
            // 滚动容器必须先有 id
            .id("wslconf-preview")
            .w_full()
            .max_h(px(220.))
            .overflow_y_scroll()
            .rounded_md()
            .bg(theme::bg())
            .p_3()
            .child(
                div()
                    .font_family("Consolas")
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(state.doc.render()),
            )
            .into_any_element()
    } else {
        div().into_any_element()
    };

    let preview_btn = {
        let entity = entity.clone();
        Button::new("wslconf-preview")
            .label(if state.show_preview {
                "隐藏预览"
            } else {
                "预览内容"
            })
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.toggle_wslconf_preview(cx));
            })
    };
    let cancel = {
        let entity = entity.clone();
        Button::new("wslconf-cancel")
            .label("取消")
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.close_wslconf(cx));
            })
    };
    let save = {
        let entity = entity.clone();
        Button::new("wslconf-save")
            .label("保存")
            .small()
            .primary()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.save_wslconf(false, cx));
            })
    };
    let save_restart = {
        let entity = entity.clone();
        Button::new("wslconf-save-restart")
            .label("保存并重启发行版")
            .small()
            .primary()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.save_wslconf(true, cx));
            })
    };

    // 保存中就把两个保存按钮**换成一行字**。
    //
    // `save_wslconf` 要串起 `user_exists` + `path_exists`（两条 `wsl.exe`）
    // + 写 `/etc/wsl.conf` + 可选的 terminate，秒级到十几秒。原先这段时间
    // 弹窗一点变化都没有，两个按钮也照样能点 —— 连点会并发出两轮 `wsl.exe`，
    // 重复写一遍配置、再 terminate 一次发行版。
    //
    // 项目约定**不用** `.disabled()`（禁用态的文字几乎看不清，见 §7.5），
    // 所以这里是换掉元素、不是置灰。
    let save_actions: Vec<AnyElement> = if state.busy {
        vec![
            div()
                .text_sm()
                .text_color(theme::text_muted())
                .child("正在保存…（要读发行版状态并写文件，可能要十几秒）")
                .into_any_element(),
        ]
    } else {
        vec![save.into_any_element(), save_restart.into_any_element()]
    };

    modal_scrim("wslconf",
        v_flex()
            .id("wslconf-card")
            // 高度上限卡在 620：16 个字段全展开会顶满整屏
            // （用户反馈"上下太长"）。超了就滚 —— 与其把窗口撑到
            // 遮住上下文，不如让用户滚动。
            .w(px(780.))
            .max_h(px(620.))
            .overflow_y_scroll()
            .gap_4()
            .p_4()
            .rounded_lg()
            .bg(theme::bg_card())
            .border_1()
            .border_color(theme::border())
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_lg()
                            .font_bold()
                            .text_color(theme::text())
                            .child(format!("{} 配置（/etc/wsl.conf）", state.distro)),
                    )
                    .child(div().text_xs().text_color(theme::text_dim()).child(format!(
                        "{}　·　保存前会先备份到 /etc/wsl.conf.bak；\
                             注释和本程序不认识的键都会原样保留。",
                        if state.wsl_version.is_empty() {
                            "WSL 版本未知".to_owned()
                        } else {
                            format!("WSL {}", state.wsl_version)
                        }
                    ))),
            )
            .child(v_flex().w_full().gap_4().children(sections))
            .child(v_flex().w_full().gap_1().children(errors))
            .child(preview)
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .justify_end()
                    .child(preview_btn)
                    .child(cancel)
                    // 保存中这两个按钮会消失、换成"正在保存…"（见 `save_actions`）
                    .children(save_actions),
            ),
    )
    .into_any_element()
}
