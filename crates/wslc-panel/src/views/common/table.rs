//! 表格族：官方静态表格的壳（`table_card` / `table_header` / `table_row`）
//! 与各类单元格（`cell_text` / `cell_full` / `cell_link` / `cell_badge` …）。
//!
//! `common.rs` 与 `common/` 目录并存，本模块由 `common.rs` 用
//! `pub(crate) use table::*;` 再导出 —— 调用点仍然写 `common::…`。

use super::super::prelude::*;
use super::badge;

/// 表格表头 —— 官方 `TableHeader` 里那一行 `TableHead`。
///
/// 列宽走 [`wslc_panel_core::columns::column_fractions`] 换成比例、再由 `flex_grow` 分配 ——
/// 于是表格**永远占满整行**并随窗口伸缩。以前是固定像素：宽度之和小于窗口时，
/// 右边留一大片空白，在宽窗口上看起来就是"前几列挤着、最后一列拖出去一截"。
///
/// # 为什么在官方组件之上还要自己算比例
///
/// `TableHead::render` 的默认宽度是 `flex_shrink_1() + flex_basis(relative(col_span))`
/// —— 也就是**所有列等分**（读 gpui-component 0.8.0 的 `src/table/table.rs` 确认）。
/// 我们的列不等宽（操作列要比「驱动」列宽），所以用算出来的比例把它覆盖掉。
///
/// 覆盖之所以成立：`TableHead::render` 最后一步才 `refine_style(&self.style)`，
/// 我们的 `flex_grow` / `flex_basis` 在它之后落地。
///
/// ⚠️ 必须同时给 `min_w_0()`：flex 项默认 `min-width: auto`，一个长路径会把那一列
/// 撑破、把别的列挤出去。官方还额外加了 `min_w(MIN_CELL_WIDTH = 100px * col_span)`，
/// 这一条被我们的 `min_w_0()` 覆盖掉了 —— 保持和改造前一致的收缩行为。
///
/// ⚠️ 边框色**覆盖不了**：`TableHeader` / `TableRow` 是在 `refine_style` **之后**才
/// `.border_color(cx.theme().table_row_border)` 的。好在那条 token 的 fallback 就是
/// `colors.border`（`theme/schema.rs` 里 `apply_color!(table_row_border, fallback = self.border)`），
/// 而 `theme.rs` 已经把 `colors.border` 设成了我们的 `line` —— 所以颜色仍然是我们那一套，
/// 只是**这件事不是我们控制的**，改配色时要记住它走的是组件主题。
pub(crate) fn table_header(columns: &'static [(&'static str, f32)]) -> TableRow {
    let fractions = wslc_panel_core::columns::column_fractions(columns);
    // 列之间的间隔用**单元格内边距**（`px_2`）而不是 `gap_2`：比例之和正好 100%，
    // 不留缝隙给 flex 去收缩。
    TableRow::new()
        .px_3()
        .children(columns.iter().zip(fractions).map(|((name, _), fraction)| {
            TableHead::new()
                .flex_grow(fraction)
                .flex_basis(px(0.))
                .min_w_0()
                .px_2()
                .text_xs()
                .font_semibold()
                .text_color(theme::text_dim())
                .child(*name)
        }))
}

/// 表格的一行（列宽与 [`table_header`] 同一套比例，两边必须一致）。
///
/// 返回官方 `TableRow` 而不是 `AnyElement`：官方的 `TableBody::children` 要求
/// `ChildElement`（trait 约束是 `Sizable + IntoElement`），而 `AnyElement` 不实现
/// `Sizable` —— 所以"行"必须保持**具体类型**，装箱之后就塞不回表格了。
pub(crate) fn table_row(
    columns: &'static [(&'static str, f32)],
    cells: Vec<AnyElement>,
) -> TableRow {
    let fractions = wslc_panel_core::columns::column_fractions(columns);
    // 行里多出来的 cell（列定义没写）按平均宽度兜底，别让它挤成一条线
    let fallback = 1.0 / columns.len().max(1) as f32;
    // ⚠️ 行的上下内边距挂在**单元格**上（`py_2`），不是挂在行上：
    // 官方 `TableCell` 自己会按 `Size` 给 `px/py`，而 `refine_style` 在最后，
    // 我们写的 `py_2` 能盖掉它 —— 挂行上则会被单元格的内边距叠成两层。
    TableRow::new()
        .px_3()
        .children(cells.into_iter().enumerate().map(|(i, cell)| {
            TableCell::new()
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
                .py_2()
                .child(cell)
        }))
}

/// 表格卡片：官方 `Table` + `TableHeader` + `TableBody`，配色按我们那一套收口。
///
/// # 为什么要有它
///
/// 8 个列表页原先各写一份**逐字相同**的外壳（自己拼 `div`：圆角 / 底色 / 边框 /
/// 裁切 / 表头 / 行）。外壳一旦要动（加阴影、换圆角、让表头吸顶），就得记得改 8 处；
/// 漏一处的结果是"这一页的表格和别页不像一套东西"，而这种差异 **CI 完全看不见**
/// （`AGENTS.md` §8）。
///
/// 现在用的是**官方静态表格**（`Table` / `TableHeader` / `TableBody` / `TableRow` /
/// `TableHead` / `TableCell`）。官方在 `table.rs` 的文档里写得很清楚：`Table` 是
/// "a simple, stateless, composable table without virtual scrolling or column
/// management" —— 正好对上我们的场景（几十行、行内带按钮、不需要排序）。
/// 虚拟滚动的 `DataTable` 只留给 SQL 结果表（那里是真的大结果集）。
///
/// # 为什么 `empty` 是 `Option<AnyElement>` 而不是 `Option<&'static str>`
///
/// 8 个页面的空状态并**不同构**：
///
/// - 多数是一句灰字（`empty_state("没有本地镜像")`）；
/// - 「容器」页有三种情况（整库为空 → 官方 [`page_empty`]；筛完为空 →
///   "没有匹配的容器" + 「清空筛选」按钮）；
/// - 「部署」页是两行说明（第二行还带 `strip_md`）；
/// - 「实例」列表**没有**空状态（空列表在进页面之前就被拦掉了）→ 传 `None`。
///
/// 所以这里收 `AnyElement`：一句话的用 [`empty_state`]，特殊的自己拼。
///
/// ⚠️ 「备份」列表（`db_dialogs`）**刻意不用它**：那张表画在弹窗卡（`bg_card`）里，
/// 底色得用 `theme::bg()` 才有对比；而且它整块为空时画的是卡片**外面**的官方 `Empty`。
pub(crate) fn table_card(
    columns: &'static [(&'static str, f32)],
    rows: Vec<TableRow>,
    empty: Option<AnyElement>,
) -> impl IntoElement {
    let body = if rows.is_empty() {
        match empty {
            // 空状态横跨整行：只有这一个 cell，`flex_1` 让它吃掉整行宽度，
            // `col_span` 顺便把官方那条 `min_w(100px * col_span)` 抬到整表宽度。
            Some(node) => TableBody::new().child(
                TableRow::new().px_3().child(
                    TableCell::new()
                        .col_span(columns.len())
                        .flex_1()
                        .child(node),
                ),
            ),
            None => TableBody::new(),
        }
    } else {
        TableBody::new().children(rows)
    };

    Table::new()
        .rounded_lg()
        .bg(theme::bg_card())
        .border_1()
        .border_color(theme::border())
        .child(
            // `.bg(...)` 要显式给：官方 `TableHeader` 默认铺 `tokens.table_head`
            // （fallback 是 `tokens.list_head`）—— 那是组件主题的一层浅色，
            // 和我们的卡片底色不是同一档（`AGENTS.md` §5：颜色归我们，形态归组件）。
            TableHeader::new()
                .bg(theme::bg_card())
                .child(table_header(columns)),
        )
        .child(body)
}

/// 单元格文字。
pub(crate) fn cell_text(text: impl Into<SharedString>) -> AnyElement {
    div()
        .text_sm()
        .text_color(theme::text())
        .overflow_hidden()
        .truncate()
        .child(text.into())
        .into_any_element()
}

/// 次要单元格文字。
pub(crate) fn cell_muted(text: impl Into<SharedString>) -> AnyElement {
    div()
        .text_sm()
        .text_color(theme::text_muted())
        .overflow_hidden()
        .truncate()
        .child(text.into())
        .into_any_element()
}

/// 正常色的「可悬停看全文」单元格（表格里的主要标识，比如镜像引用）。
pub(crate) fn cell_text_full(
    key: impl Into<SharedString>,
    text: impl Into<SharedString>,
) -> AnyElement {
    cell_full(key, text, false, None)
}

/// 次要色的「可悬停看全文」单元格（镜像、安装位置、挂载点这类长文本）。
pub(crate) fn cell_muted_full(
    key: impl Into<SharedString>,
    text: impl Into<SharedString>,
) -> AnyElement {
    cell_full(key, text, true, None)
}

/// 「一行会被截断的文字 + 悬停看全文」的唯一实现。
///
/// 用 gpui **原生**的 `.tooltip(..)`，不借 `Button`（理由见 [`cell_full`]）。
///
/// `.tooltip` 挂在 `Stateful` 那一层，所以 `.id()` 是**必需**的；
/// `id` 还必须当帧唯一（GPUI 的 hover 状态按元素 id 记）。
fn cell_text_with_tip(
    id: SharedString,
    text: SharedString,
    tip: SharedString,
    color: Rgba,
) -> impl IntoElement {
    use gpui_kit::component::tooltip::Tooltip;

    div()
        .id(id)
        .w_full()
        .text_sm()
        .text_color(color)
        .overflow_hidden()
        .truncate()
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        .child(text)
}

/// 会截断的单元格：省略号 + **悬停显示全文**。
///
/// # 为什么不借 `Button` 了（v0.20.2）
///
/// 原先这里拿 `Button`（`text` 变体）当"承载 tooltip 的壳"，理由是
/// "gpui 的 `InteractiveElement` 没有 tooltip 方法"。**那条在 gpui-kit 0.8.0
/// 已经不成立**：gpui **原生**就有 `.tooltip(..)`（`TooltipOverlay` 的文档还专门
/// 写了一句话声明"它不管这个原生 API"），普通 `div` 加个 `.id()` 就能挂。
/// 官方示例见 <https://gpui-kit.com/zh-CN/component/tooltip>。
///
/// 换回 `div` 的收益不只是少一层：
///
/// - `Button` 带着按钮的焦点 / 键盘语义（`tab_stop(false)` 只是不参与 Tab，
///   元素本身仍然是按钮），而这些单元格是**纯展示**；
/// - GPUI 的 hover 状态按元素 id 记 —— `Button` 和我们自己的 `div` 抢同一个
///   id 是没有必要的风险（`cell_link` 那边还要靠 hover 变色）。
///
/// # `key` 必须当帧唯一
///
/// 调用点传的是容器名 / 镜像引用 / 卷名 / 发行版名这类**本来就唯一**的标识。
pub(crate) fn cell_full(
    key: impl Into<SharedString>,
    text: impl Into<SharedString>,
    muted: bool,
    tip: Option<SharedString>,
) -> AnyElement {
    let key: SharedString = key.into();
    let text: SharedString = text.into();
    let color = if muted {
        theme::text_muted()
    } else {
        theme::text()
    };

    cell_text_with_tip(
        SharedString::from(format!("cell-tip-{key}")),
        text.clone(),
        // 没给 tip 就用全文（截断之后悬停能看全）
        tip.unwrap_or(text),
        color,
    )
    .into_any_element()
}

/// 可点击的 URL 单元格：主色 + 悬停变亮 + 点击用默认浏览器打开。
///
/// 和 [`cell_full`] 同一套"`div` + `.id()` + 原生 tooltip"，另外多三件事：
/// `cursor_pointer()`、手写的 hover / active 变色（`AGENTS.md` §7.5：
/// 自己画的可点元素必须手写，否则鼠标移上去毫无反应）、以及 `on_click`。
pub(crate) fn cell_link(
    key: impl Into<SharedString>,
    text: impl Into<SharedString>,
    url: impl Into<String>,
    entity: &Entity<Shell>,
) -> AnyElement {
    use gpui_kit::component::tooltip::Tooltip;

    let key: SharedString = key.into();
    let text: SharedString = text.into();
    let url: String = url.into();
    let entity = entity.clone();
    let tip = text.clone();

    div()
        .id(SharedString::from(format!("cell-link-{key}")))
        .w_full()
        .cursor_pointer()
        .text_sm()
        .text_color(theme::primary())
        .overflow_hidden()
        .truncate()
        .hover(|this| this.text_color(theme::link_hover()))
        .active(|this| this.text_color(theme::primary()))
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        .on_click(move |_, _, cx| {
            entity.update(cx, |shell, cx| shell.open_url(url.clone(), cx));
        })
        .child(text)
        .into_any_element()
}

/// 状态徽标单元格。
pub(crate) fn cell_badge(state: ContainerState) -> AnyElement {
    let (fg, bg) = theme::state_colors(&state);
    badge(state.label().to_owned(), fg, bg).into_any_element()
}
