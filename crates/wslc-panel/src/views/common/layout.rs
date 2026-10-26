//! 布局族：卡片、键值对行、空状态、计数行、弹窗底栏、存档警告条。
//!
//! `common.rs` 与 `common/` 目录并存，本模块由 `common.rs` 用
//! `pub(crate) use layout::*;` 再导出 —— 调用点仍然写 `common::…`。
//!
//! 判断一个助手该不该进这里只有一句话：**它画的是"块怎么摆"吗？**
//! 画的是表格 / 表单 / 按钮的，去隔壁那几个文件。

use super::super::prelude::*;
// `error_banner` / `blocked_card` 里那段「可滚动的原文」共用 `common.rs` 根上的 `log_text`。
use super::log_text;

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

/// 列表页页头：**动作在左、计数在右**。
///
/// # 只收这两页
///
/// 「镜像」与「卷」原先逐字同形（`h_flex().w_full().justify_between().items_center()`），
/// 而「卷」那一页早先是**反过来**放的（计数在左）—— 用户每换一页都要重新找一眼
/// "这页有几个"。收成一处之后两页不可能再漂。
///
/// ⚠️ 另外两页**刻意不用它**，别硬套：
/// 「容器」页那条是**两行筛选条**（搜索框 + 范围 + 排序），左边不是"动作"而是控件；
/// 「部署」页左边是一整块说明（容器 / 镜像 / 运行时 + 依据），比一行按钮高得多。
pub(crate) fn page_header(actions: impl IntoElement, count: impl IntoElement) -> AnyElement {
    h_flex()
        .w_full()
        .justify_between()
        .items_center()
        .child(actions)
        .child(count)
        .into_any_element()
}

/// 「忙的时候把按钮换成一行灰字」—— 项目里**不用** `.disabled()`（见 `AGENTS.md` §7.5）：
/// 置灰的按钮会让人反复去点，而"执行中…"说清了现在在等什么。
///
/// # 为什么收成一处
///
/// 这段 `div().text_sm().text_color(text_muted()).child("…")` 原先在 9 个地方手写
/// （页头的「重新读取」、SQL 的「执行」、数据库 / 部署两页的读取态、备份 / 导入、
/// 日志浮层、站点配置……），每处的样式一模一样、只有文案不同。
/// 收起来之后"等待态长什么样"只有一个定义 —— 这本来就是
/// `AGENTS.md` §7.1「点下去先发一条『正在…』」的落地件。
pub(crate) fn busy_text(label: impl Into<SharedString>) -> AnyElement {
    div()
        .text_sm()
        .text_color(theme::text_muted())
        .child(label.into())
        .into_any_element()
}

/// 同 [`busy_text`]，但用 `text_xs` —— 塞在小块 / 清单状态行 / 树占位里时用这一档。
///
/// 两档分开而不是加个参数：`text_sm` 和 `text_xs` 在这里不是"同一件事的两种写法"，
/// 而是**两种处境**（替换一颗按钮 / 一行状态脚注），混成一个参数只会让两个调用点
/// 各自去查该传哪个。
pub(crate) fn busy_text_xs(label: impl Into<SharedString>) -> AnyElement {
    div()
        .text_xs()
        .text_color(theme::text_muted())
        .child(label.into())
        .into_any_element()
}

/// 上一次失败的**两段式**横幅：一句摘要 +（可滚动的）原文。
///
/// # 为什么这里不用官方 `Alert`
///
/// 读 `alert.rs` 确认过：`Alert` 的 `message` 是 `impl Into<Text>`，而 `Text` 只实现了
/// `From<SharedString | String | &str | TextView>` —— 接不了任意 `Element`。
/// 而这个横幅是**两段**（摘要 + 可滚动的原文），压成一句就滚动不了，
/// 也丢掉了"原文保留换行"这个排查价值。所以外壳手写，配色仍走 `theme::danger_*`。
///
/// # 两页原先漂了
///
/// 「数据库」那份是 `rounded_md` + 摘要**不加粗**，「部署」那份是 `rounded_lg` +
/// 加粗 —— 同一类信息（"上一次动作失败了"）在两张页面上长得不一样。
/// 现在统一成 `rounded_md` + 加粗（横幅是窄条，半径该小一档；
/// 摘要加粗是为了和下面的原文拉开层次）。
pub(crate) fn error_banner(summary: &str, detail: &str, id: &'static str) -> AnyElement {
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
                .font_semibold()
                .text_color(theme::danger())
                .child(wslc_panel_core::util::strip_md(summary)),
        )
        .child(log_text(Some(detail), id))
        .into_any_element()
}

/// 探测就失败时的说明卡（不是这一页认识的容器 / 容器没运行）。
///
/// 「数据库管理」与「部署管理」两页原先各写一份，逐行同形，只有文案不同 ——
/// 连"面板不自己连 3306 / 不自己起进程"那段解释都在两处各写了一遍。
///
/// ⚠️ 标题里的**具体原因**留在调用侧拼：部署页要把 `failure.kind.label()` 塞进
/// 括号（"这个容器现在部署不了（不是 Node / Java / Go…）"），数据库页是一句固定的
/// "这个容器现在管不了数据库" —— 那是两页各自的领域知识，不该进公共外壳。
///
/// `footnote` 是给"只有那一页需要多说一句"的地方留的（部署页会说
/// "刚才是在读容器 X 的时候失败的"）。用 `Option` 而不是让调用侧 `.child()`：
/// 这个函数返回 `AnyElement`（装箱的），调用侧没法再往上挂东西。
pub(crate) fn blocked_card(
    title: impl Into<SharedString>,
    summary: &str,
    note: &'static str,
    detail: &str,
    id: &'static str,
    footnote: Option<AnyElement>,
) -> AnyElement {
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
                .child(title.into()),
        )
        .child(
            div()
                .text_sm()
                .text_color(theme::text())
                .child(wslc_panel_core::util::strip_md(summary)),
        )
        .child(div().text_xs().text_color(theme::text_muted()).child(note))
        .child(log_text(Some(detail), id))
        .children(footnote)
        .into_any_element()
}
