//! 跨页面共用的**小组件**：卡片、徽标、表格壳、单元格、按钮族、表单格、日志块。
//!
//! 这里的每一样都被两个以上的页面用到 —— 只被一处用到的助手留在那个页面自己的模块里。

use super::prelude::*;

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

/// 展开状态只控制展示，折叠不会清空表单实体里的参数。
///
/// # 折叠头用官方 `Icon`，不用 `▾` / `▸` 字符
///
/// 早先标题是 `format!("{} {title}", if open { "▾" } else { "▸" })` —— 那两个
/// 是**字体里的字符**，不同字体下大小 / 基线不一样（中文界面里常偏上偏下），
/// 也不会跟着"减弱动态效果"之类的偏好走。
///
/// 现在用 Lucide 的 `ChevronDown` + `rotate(…)`：展开时转 180° ——
/// **和 `Accordion` 的 trigger 完全一致**（读 `accordion.rs` 确认过它的写法），
/// 同一类交互在同一个应用里应该有同一个样子。
pub(crate) fn disclosure(
    section: UiSection,
    title: impl Into<String>,
    state: &AppState,
    entity: &Entity<Shell>,
    body: impl IntoElement,
) -> AnyElement {
    use gpui_kit::component::{Icon, IconName};

    let open = state.section_open(section);
    let title = title.into();
    let entity = entity.clone();
    Collapsible::new()
        .open(open)
        .w_full()
        .child(
            Button::new(SharedString::from(format!("section-{section:?}")))
                .small()
                .on_click(move |_, _, cx| {
                    entity.update(cx, |shell, cx| {
                        shell.state.toggle_section(section);
                        cx.notify();
                    });
                })
                // 内容自己画：左边标题、右边箭头。`Button::label()` 只能放一段
                // 文字，放不下"标题 + 图标"这个组合。
                //
                // ⚠️ `rotate(percentage(...))` 是照 `accordion.rs` 的 trigger 抄的
                // —— `rotate` 吃 `impl Into<Radians>`，而 `Percentage` 有那个
                // `From`（`0.5` = 半圈 = 180°）。本仓库没有别的 `Radians` 用法，
                // 别自己造一个 `Radians(PI)` 那种写法。
                .child(
                    h_flex()
                        .w_full()
                        .justify_between()
                        .items_center()
                        .gap_2()
                        .child(div().text_sm().child(title))
                        .child(
                            Icon::new(IconName::ChevronDown)
                                .xsmall()
                                .flex_none()
                                .text_color(theme::text_dim())
                                .rotate(gpui_kit::percentage(if open { 0.5 } else { 0. })),
                        ),
                ),
        )
        .content(v_flex().w_full().gap_3().pt_2().child(body))
        .into_any_element()
}

/// 弹窗卡片的**统一尺寸档**。
///
/// # 为什么需要它
///
/// 原来 18 个弹窗各写各的 `.w(px(…))`，实测下来有 **6 种**宽度：
///
/// ```text
/// 560 / 620 / 680 / 700 / 760 / 780 / 820
/// ```
///
/// 但它们的实际分布是**三簇**：`560/620`（3 处，确认与单输入）、
/// `680/700`（4 处，表单）、`760/780/820`（6 处，详情 / 日志 / 表格）。
/// 簇内那 ±20~60px 的差别不是布局需要 —— 同样 680 宽的两个弹窗，
/// 一个 `max_h 560`、一个 `max_h 820`，纯属各写各的。
///
/// 后果用户能看见：点「创建容器」(780) 再点「执行命令」(760)，
/// **卡片边缘会挪动 20px**。正式软件里同一种模态只有一两档宽度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModalSize {
    /// 窄：确认框、单输入弹窗（原 560 / 620）。
    Narrow,
    /// 中：常规表单（原 680 / 700）。
    Medium,
    /// 宽：详情 / 日志 / 表格（原 760 / 780 / 820）。
    Wide,
}

impl ModalSize {
    /// 这一档的像素宽度。
    pub fn width(self) -> f32 {
        match self {
            Self::Narrow => 560.,
            // 680 → 700：簇内取整，和 `Medium` 的另一处（700）对齐。
            Self::Medium => 700.,
            // 760 / 780 / 820 → 800：同上，取簇内中位。
            Self::Wide => 800.,
        }
    }
}

/// 弹窗卡片的**视觉外壳**：宽度 / 内边距 / 圆角 / 底色 / 边框。
///
/// 只做这一件事 —— **`id`、`max_h`、滚动都不在这里**。原因：
///
/// - `id` 只有"需要滚动 / 需要 hover"的卡片才有（18 处里 9 处），
///   做成必填会逼着另外 9 处编一个假的；
/// - `max_h` 各弹窗差得多（确认框 3 行 vs 日志几百行），这里猜不了。
///
/// 所以调用方式是把本函数当成**起点**，返回 `Div` 可以继续挂：
///
/// ```ignore
/// modal_card(ModalSize::Medium)
///     .id("create-dialog-card")        // 需要滚动时才有
///     .max_h(px(880.))
///     .overflow_y_scroll()             // 只对带 id 的元素可用
///     .child(…)
/// ```
///
/// ⚠️ 返回 `Div` 而不是 `impl IntoElement`：调用方要能继续 `.id(…)`
/// （`StatefulInteractiveElement` 的方法）。
pub fn modal_card(size: ModalSize) -> gpui_kit::Div {
    v_flex()
        .w(px(size.width()))
        .gap_4()
        .p_5()
        .rounded_lg()
        .bg(theme::bg_card())
        .border_1()
        .border_color(theme::border())
}

/// 模态浮层的**外壳**：满屏遮罩 + 居中的卡片槽。
///
/// # 分工
///
/// - 这里只管**遮罩**（行为 + 入场动画）；
/// - 卡片本体的视觉走 [`modal_card`]；
/// - 具体内容由调用方给。
///
/// 早先这里写着"8 个弹窗各不相同，做成参数只会让调用点更长" ——
/// 那句话后来被实测推翻了：不是"各不相同"，是**没人统一**，
/// 于是攒出 6 种宽度（见 [`ModalSize`]）。
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
        .with_animation(
            id,
            Animation::new(Duration::from_millis(140)),
            |el, delta| el.opacity(delta),
        )
        .into_any_element()
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

/// 状态徽标。
///
/// 底色由 [`theme::state_colors`] 统一给出（不透明的预置色），
/// 不用 alpha 叠加 —— 叠出来的观感会随背后的容器背景变化。
///
/// # 为什么用 `Tag` 而不是自己画 div
///
/// 这条是"组件库有的就别自己写"的直接应用。自己画的那版只有
/// `px_2 py_1 rounded_md` 三行样式，但**没有边框、没有 hover、圆角也不跟
/// 组件库的 radius token** —— 于是同一个界面上，徽标是 8px、按钮是 6px、
/// 输入框是另一档，看起来像几个人拼的。
///
/// `Tag::custom(fg_bg, fg, border)` 的三个颜色**完全由我们给**
/// （读 `tag.rs` 的 `TagVariant::Custom` 确认过：它不碰 `cx.theme()`），
/// 所以配色仍是 `theme.rs` 那一套，只是形状/圆角/边框/hover 交给组件库 ——
/// **这正是"高内聚低耦合"想要的分工**：颜色归我们，形态归组件。
pub(crate) fn badge(text: impl Into<SharedString>, fg: Rgba, bg: Rgba) -> impl IntoElement {
    use gpui_kit::component::tag::Tag;

    // `Rgba -> Hsla` 要显式标注：`From` 实现推不出目标类型
    // （同文件 `gauge()` 里那条注释说的是同一件事）。
    let fg: Hsla = fg.into();
    // 边框：`Tag` 要求三色都给，而 `theme` 没有"徽标边框"这一档取色。
    // 用前景色的 30% 不透明度 —— 比"完全没有边框"有形状感，
    // 又不会像实色边框那样抢戏。
    let border = fg.opacity(0.3);
    Tag::custom(bg.into(), fg, border)
        .flex_none()
        .child(text.into())
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

/// 「这一页得先选一个容器」—— 整页空状态的文案。
///
/// # 为什么要收成一处
///
/// 同一句话原先在三个文件里各写一份（`database()` / `sql_console_page()` /
/// `deploy_page()`），只有结尾的入口名不一样。改一处忘一处的后果不是编译错误，
/// 而是**同一个处境在三个页面上被描述成三句不同的话** —— 用户会以为进了三个功能。
///
/// 这也顺带把「从哪个按钮进来」钉死了：`Database` 一定是从容器行的「数据库」，
/// `Deploy` 一定是从「部署管理」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NoContainer {
    /// 数据库管理页（容器行的「数据库」）。
    Database,
    /// 部署管理页（容器行的「部署管理」）。
    Deploy,
}

impl NoContainer {
    /// 整页空状态的文案（配 [`page_empty`] 用）。
    pub(crate) fn empty_hint(self) -> &'static str {
        match self {
            Self::Database => "还没有选中容器 —— 请到「容器」页点某一行的「数据库」",
            Self::Deploy => "还没有选中容器 —— 请到「容器」页点某一行的「部署管理」",
        }
    }
}

/// 防御分支的文案：进了「数据库 / SQL 控制台」页，但表单实体没建起来。
///
/// 理论上走不到（进页面前 `ensure_db_form` 已经建好了）。两处共用一份，
/// 是因为它们说的是同一件事；分开写迟早会漂成两句。
///
/// ⚠️ 不解释内部状态 —— "编辑器还没建好"是给开发看的，用户只需要知道**下一步点哪**。
pub(crate) const REOPEN_DB_PAGE: &str = "请从「容器」页的「数据库」按钮重新进入";

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

/// 表格表头 —— 官方 `TableHeader` 里那一行 `TableHead`。
///
/// 列宽走 [`crate::columns::column_fractions`] 换成比例、再由 `flex_grow` 分配 ——
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
    let fractions = crate::columns::column_fractions(columns);
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
    let fractions = crate::columns::column_fractions(columns);
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
                TableRow::new()
                    .px_3()
                    .child(TableCell::new().col_span(columns.len()).flex_1().child(node)),
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

    Button::new(SharedString::from(format!("cell-tip-{key}")))
        .text()
        .tooltip(tip.unwrap_or_else(|| text.clone()))
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

/// 可点击的 URL 单元格：主色 + 悬停变亮 + 点击用默认浏览器打开。
///
/// 和 [`cell_full`] 一样借组件库的 `Button::text()` 当壳，所以长 URL 仍然
/// 可以悬停看全文；区别只是多了一个 `on_click`，并且用主色让人看出能点。
pub(crate) fn cell_link(
    key: impl Into<SharedString>,
    text: impl Into<SharedString>,
    url: impl Into<String>,
    entity: &Entity<Shell>,
) -> AnyElement {
    let key: SharedString = key.into();
    let text: SharedString = text.into();
    let url: String = url.into();
    let entity = entity.clone();

    Button::new(SharedString::from(format!("cell-link-{key}")))
        .text()
        .tooltip(text.clone())
        .w_full()
        .on_click(move |_, _, cx| {
            entity.update(cx, |shell, cx| shell.open_url(url.clone(), cx));
        })
        .child(
            div()
                .w_full()
                .text_sm()
                .text_color(theme::primary())
                .overflow_hidden()
                .truncate()
                .child(text),
        )
        .into_any_element()
}

/// 状态徽标单元格。
pub(crate) fn cell_badge(state: ContainerState) -> AnyElement {
    let (fg, bg) = theme::state_colors(&state);
    badge(state.label().to_owned(), fg, bg).into_any_element()
}

/// **行内小按钮**的唯一实现：一行字、点下去跑一段动作。
///
/// # 为什么要有这一层
///
/// 下面那五个具名助手原先各写一份
/// `Button::new(id).label(label).small().on_click(…)` —— 五份一模一样的外壳，
/// 只有点击体不同。外壳一旦要改（换尺寸、加 tooltip、换成带图标的变体），
/// 就得记得改五处；漏一处的表现是"这一列的按钮和别处长得不一样"，
/// 而这种差异 CI 完全看不出来（`AGENTS.md` §8）。
///
/// 现在外壳只有这一份。五个具名助手保留，是因为**它们的名字说明语义**
/// （这个按钮会弹确认 / 会直接跑 / 只是开个浮层），调用点读起来比
/// `action_button(id, label, |shell, _, cx| …)` 清楚。
fn action_button(
    id: &str,
    label: &'static str,
    entity: &Entity<Shell>,
    run: impl Fn(&mut Shell, &mut Window, &mut Context<Shell>) + 'static,
) -> Button {
    let entity = entity.clone();
    Button::new(SharedString::from(id.to_owned()))
        .label(label)
        .small()
        .on_click(move |_, window, cx| {
            entity.update(cx, |shell, cx| run(shell, window, cx));
        })
}

/// 危险操作按钮（点击后弹出二次确认）。
pub(crate) fn danger_button(
    id: &str,
    label: &'static str,
    action: PendingAction,
    entity: &Entity<Shell>,
) -> impl IntoElement {
    action_button(id, label, entity, move |shell, _, cx| {
        shell.request(action.clone(), cx);
    })
}

/// 危险操作按钮（**发行版域**，点击后弹出二次确认）。
///
/// 和 [`danger_button`] 分开写，而不是合并成一个泛型函数：
/// 两个动作类型（[`PendingAction`] / [`DistroAction`]）没有任何共同方法，
/// 合并就得引入 trait 或枚举包装，调用点反而更长。
/// 重复的只是这一层薄包装 —— 按钮外壳已经收进 [`action_button`]。
pub(crate) fn danger_button_distro(
    id: &str,
    label: &'static str,
    action: DistroAction,
    entity: &Entity<Shell>,
) -> impl IntoElement {
    action_button(id, label, entity, move |shell, _, cx| {
        shell.request_distro(action.clone(), cx);
    })
}

/// 打开「单输入框提示弹窗」的按钮（移动位置 / 调整大小 / 设置默认用户）。
///
/// 和 [`danger_button_distro`] 的区别：那个弹**确认**，这个弹**输入框**。
pub(crate) fn prompt_button(
    id: &str,
    label: &'static str,
    kind: PromptKind,
    distro: &str,
    entity: &Entity<Shell>,
) -> AnyElement {
    let distro = distro.to_owned();
    action_button(id, label, entity, move |shell, window, cx| {
        // `window` 是必需的：`InputState::new` 要 `&mut Window`。
        shell.open_prompt(kind, distro.clone(), window, cx);
    })
    .into_any_element()
}

/// 即时操作按钮（启动 / 重启）。
///
/// 与 [`danger_button`] 的区别：不弹二次确认，点了就跑。
pub(crate) fn immediate_button(
    id: &str,
    label: &'static str,
    action: ImmediateAction,
    entity: &Entity<Shell>,
) -> impl IntoElement {
    action_button(id, label, entity, move |shell, _, cx| {
        shell.run_immediate(action.clone(), cx);
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
pub(crate) fn overlay_button(
    id: &str,
    label: &'static str,
    open: impl Fn(&mut Shell, &mut Window, &mut Context<Shell>) + 'static,
    entity: &Entity<Shell>,
) -> AnyElement {
    action_button(id, label, entity, open).into_any_element()
}

// ---------------------------------------------------------------------------
// 页面分发
// ---------------------------------------------------------------------------

/// 「创建容器」弹窗的**表单字段**：标签 + 输入框。
///
/// `full` 决定宽度：直接放进 `v_flex` 的用 `w_full`，
/// 放进 `h_flex` 成对排列的用 `flex_1`。
/// （在 `v_flex` 里写 `flex_1` 会把字段**竖向**拉长，是个容易踩的坑。）
pub(crate) fn form_field(
    id: &'static str,
    label: &'static str,
    input: &Entity<InputState>,
    cx: &App,
    full: bool,
) -> AnyElement {
    form_field_with_error(id, label, input, cx, full, false)
}

/// [`form_field`] 的错误态版本：`invalid` 时标签和输入框边框一起变红。
///
/// 只给「添加实例」用：那一页点「开始安装」校验失败后，用红框指出哪一格
/// 要改，而不是在页面底部再堆一段和步骤预览重复的错误清单。
pub(crate) fn form_field_with_error(
    id: &'static str,
    label: &'static str,
    input: &Entity<InputState>,
    cx: &App,
    full: bool,
    invalid: bool,
) -> AnyElement {
    let field = if full {
        v_flex().w_full()
    } else {
        v_flex().flex_1().min_w_0()
    };

    // 取值只为在为空时把标签调暗一点，让"必填未填"看得见。
    let empty = input.read(cx).value().trim().is_empty();
    let label_color = if invalid {
        theme::danger()
    } else if empty {
        theme::text_muted()
    } else {
        theme::text_dim()
    };

    let mut input_el = Input::new(input).id(id).w_full();
    if invalid {
        input_el = input_el.border_color(theme::danger());
    }

    field
        .gap_1()
        .child(div().text_xs().text_color(label_color).child(label))
        .child(input_el)
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
pub(crate) fn log_text(text: Option<&str>, id: &'static str) -> AnyElement {
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
