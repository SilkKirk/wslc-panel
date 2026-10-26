//! 布局族：卡片、键值对行、空状态、计数行、弹窗底栏、存档警告条。
//!
//! `common.rs` 与 `common/` 目录并存，本模块由 `common.rs` 用
//! `pub(crate) use layout::*;` 再导出 —— 调用点仍然写 `common::…`。
//!
//! 判断一个助手该不该进这里只有一句话：**它画的是"块怎么摆"吗？**
//! 画的是表格 / 表单 / 按钮的，去隔壁那几个文件。

use super::super::prelude::*;

/// 卡片：标题 + 内容。
pub(crate) fn card(title: &'static str, body: impl IntoElement) -> impl IntoElement {
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

/// 键值对行。
pub(crate) fn kv(label: &'static str, value: impl Into<SharedString>) -> impl IntoElement {
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
pub(crate) fn kv_block(label: &'static str, value: impl Into<SharedString>) -> impl IntoElement {
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

/// 空状态占位 —— **表格内部**用的那种（一行灰字，不带边框）。
///
/// ⚠️ 整页 / 整卡片的空状态请用 [`page_empty`]（官方 `Empty`）。
/// 这两个不能合并：`Empty` 会画一圈**虚线边框**并 `flex_1` 撑满，
/// 放在表格里会把表头和数据行之间的分隔线盖掉、还会把行高拉到整屏。
pub(crate) fn empty_state(text: &'static str) -> impl IntoElement {
    v_flex()
        .w_full()
        .py_8()
        .items_center()
        .justify_center()
        .child(div().text_sm().text_color(theme::text_dim()).child(text))
}

/// 整页 / 整卡片的空状态 —— 官方 `Empty`。
///
/// 和 [`empty_state`] 的区别（选哪个看容器）：
///
/// | | `empty_state` | `page_empty` |
/// |---|---|---|
/// | 边框 | 无 | 虚线（组件库画） |
/// | 高度 | 自然高度 + `py_8` | `flex_1` 撑满父级 |
/// | 用在 | **表格内部**的空行 | 一整页 / 一整张卡片 |
///
/// 文案走 `EmptyTitle`（它实现 `Styled`），所以颜色仍是我们的
/// `theme::text_dim()`，不跟组件库主题走。
pub(crate) fn page_empty(text: &'static str) -> AnyElement {
    use gpui_kit::component::empty::{Empty, EmptyHeader, EmptyTitle};

    Empty::new()
        .header(
            EmptyHeader::new().title(
                EmptyTitle::new()
                    .text_sm()
                    .text_color(theme::text_dim())
                    .child(text),
            ),
        )
        .into_any_element()
}

/// 列表页的**计数行**：「共 N 个 X」这类一句话。
///
/// # 为什么要收
///
/// 8 个列表页原先各写一份 `div().text_sm().text_color(text_muted()).child(…)`，
/// 而且排布不一样：`images` / `database` 是"动作在左、计数在右"，
/// `volumes` 反过来（计数在左），`networks` 只有计数。
/// 同一类信息在不同页面上字号相同、颜色相同、**位置不同**，用户每换一页都要
/// 重新找一遍"这页有几个"。
///
/// 现在：计数统一走这里，页头统一成**动作在左、计数在右**
/// （`images` / `volumes` / 数据库两张表都是这个形状）。
///
/// ⚠️ 它只管**一句话**。带解释的长句（部署页那句"「部署过什么」来自面板自己的
/// 存档……"、实例页那行"共 N 个发行版 · M 个在运行 · 磁盘合计 …"）仍留在
/// 调用点自己拼 —— 收进来只会把 `text_xs` / `text_dim` 那几档也搅乱。
pub(crate) fn list_count(text: impl Into<SharedString>) -> AnyElement {
    div()
        .text_sm()
        .text_color(theme::text_muted())
        .child(text.into())
        .into_any_element()
}

/// 弹窗底部那一行**按钮**：右对齐、按钮之间 `gap_2`。
///
/// 收的是 `impl IntoIterator<Item = impl IntoElement>`，所以调用点写
/// `modal_footer([cancel, confirm])` 就行 —— **不用**先把每个按钮
/// `into_any_element()`（那样调用点反而比原来更长，抽这层就没意义了）。
///
/// ⚠️ 只管"一行纯按钮"。下面两种**刻意不用它**，别硬套：
///
/// - 行里混着状态文字的（`db_dialogs` 的"备份中… / 导入中…" + 关闭），
///   那些还用 `items_center + gap_3`；
/// - 行里有三四颗按钮、还要 `flex_wrap` 的（`.wslconf` 的预览 / 取消 / 保存 ×2）。
///
/// 至于按钮上的**用词**：「取消」只给"有提交动作、点了什么都不保存"的弹窗，
/// 「关闭」只给"只读浮层、本来就没有提交"的。这是**两件事**，
/// 不是同一个动作的两种叫法 —— 收进 helper 时不要顺手统一成一个词。
pub(crate) fn modal_footer(actions: impl IntoIterator<Item = impl IntoElement>) -> AnyElement {
    h_flex()
        .w_full()
        .justify_end()
        .gap_2()
        .children(actions)
        .into_any_element()
}

/// 「某个存档读不出来」这类**一行**警告条（官方 `Alert`，banner 形态）。
///
/// # 为什么收成一处
///
/// 两处存档警告原先各写一份，而且**观感不同**：数据库页用官方 `Alert`
/// （`db_store_warning`），部署页手写「底色 + 边框 + 文字色」三件套
/// （`deploy_error_banner`，圆角还比另一处大一号）。同一类信息（"面板自己那份
/// 存档读坏了，所以列表里有几列是空的"）在两个页面上长得不一样 ——
/// 用户会以为是两种不同严重程度的问题。
///
/// 现在统一走官方 `Alert`：底色 / 边框 / 图标 / 关掉按钮都由组件库给，而
/// `danger` 已经由 `theme::tune_component_theme` 换成我们自己的红。
/// `banner()` 是**整条铺满、无圆角**的形态（错误横幅），不是飘在内容里的圆角卡片。
///
/// ⚠️ `id` 必须唯一：两处存档警告可能同时出现（数据库页 + 部署页），
/// 它们的 id 不同，GPUI 才不会把两条当成同一个元素。
///
/// ⚠️ 带**可滚动原文**的那种错误（`db_error_banner`）**不走这里** ——
/// `Alert::message` 收的是 `Text`（`From<SharedString | String | &str | TextView>`），
/// 接不了任意 `Element`，两段式横幅只能手写外壳。
pub(crate) fn store_warning(id: &'static str, text: impl Into<SharedString>) -> AnyElement {
    use gpui_kit::component::alert::Alert;

    Alert::error(id, text.into()).banner().into_any_element()
}
