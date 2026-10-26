//! 跨页面共用的**小组件**：卡片、徽标、表格壳、单元格、按钮族、表单格、日志块。
//!
//! 这里的每一样都被两个以上的页面用到 —— 只被一处用到的助手留在那个页面自己的模块里。

use super::prelude::*;
// `ShellView` 的渲染闭包要共享所有权；`views` 的 prelude 里没有 `Rc`。
use std::rc::Rc;

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
/// # 直接用官方 `Accordion`，不再自己拼折叠头
///
/// 早先这里是 `Collapsible` + 手画的 trigger（`Button` + `Icon` + `rotate`），
/// 注释里写着"和 `Accordion` 的 trigger 完全一致（读 `accordion.rs` 确认过）"——
/// **既然在模仿它，就该直接用它**。官方 `AccordionItem` 自带：
///
/// - `ChevronDown` + `rotate(0.5)`（展开转 180°，和我们手画的一模一样）；
/// - 面板用**弹簧动画**展开（`spring` + `MotionReveal`），并且**收起时卸载内容**
///   （"a settled closed panel unmounts, so its content costs no layout or paint"）；
/// - 触发的键盘可达性与焦点框。
///
/// # 三处必须显式盖掉官方默认（都是读源码得出的）
///
/// 1. `.bordered(false)`：官方 `Accordion` 默认**有**边框 + 圆角 + 裁切
///    （把整组画成一张卡片）。我们的折叠区**本来就在卡片里**，再套一圈就是两层；
/// 2. `.h_auto()`：`Accordion::render` 里写死了 `.size_full()`（`w_full` + `h_full`），
///    在 `v_flex` 卡片里 `h_full` 会把折叠区撑到整卡高度。
///    能盖掉是因为 `refine_style(&self.style)` 在 render 末尾；
/// 3. `.bg(theme::bg_card())`：官方 `AccordionItem` 会铺 `tokens.accordion` 底色，
///    我们在卡片里，得用卡片底色（`AGENTS.md` §5：颜色归我们，形态归组件）。
///
/// # 为什么回调要拿 `&[usize]` 做幂等判断，而不是无脑 toggle
///
/// 官方 `Accordion::on_toggle_click` 挂在**容器**上（`BaseAccordion.on_click`），
/// 而单项的 `on_toggle_click` 会被 `Accordion::render` 覆盖成它内部的记账。
/// 容器级回调在"点内容区"时也可能被触发 —— 所以这里**不能**无脑 `toggle_section`，
/// 而是先比对官方报的展开状态和我们的状态，**只在真的不一致时才翻**
/// （`open.is_empty()` = 收起了）。这样即使回调多触发一次，界面也不会自己乱折叠。
pub(crate) fn disclosure(
    section: UiSection,
    title: impl Into<String>,
    state: &AppState,
    entity: &Entity<Shell>,
    body: impl IntoElement,
) -> AnyElement {
    let open = state.section_open(section);
    let title = title.into();
    let entity = entity.clone();
    Accordion::new(SharedString::from(format!("section-{section:?}")))
        .bordered(false)
        .h_auto()
        .item(move |item| {
            item.title(title)
                .open(open)
                .bg(theme::bg_card())
                .child(body)
        })
        .on_toggle_click(move |open_ix, _, cx| {
            let now_open = !open_ix.is_empty();
            entity.update(cx, |shell, cx| {
                if shell.state.section_open(section) != now_open {
                    shell.state.toggle_section(section);
                    cx.notify();
                }
            });
        })
        .into_any_element()
}

// 弹窗的**遮罩与卡片外壳**不在这里：19 个浮层全部走官方 `Dialog` 的窗口级
// 层栈（`app::shell::dialogs`），遮罩、尺寸、标题、入场动画都由它负责。
// 本文件只保留"内容里会用到"的助手 —— 包括弹窗底部那行按钮 [`modal_footer`]
// （它属于内容，不属于外壳：官方 `Dialog` 不会自动画 OK / Cancel）。

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
/// 同一句话原先在三个文件里各写一份（`database_page()` / `sql_console_page()` /
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

/// 表格里的「名字链接」：显示主色的名字，点开详情。
///
/// `container_name_link`（容器页）与 `distro_name_link`（实例页）原先各写一份
/// **逐行相同**的实现（id / 光标 / 主色 / hover / active / on_click），
/// 只有 id 前缀和"要不要 truncate"不同。合成一处之后，两页的链接**不可能**再漂移。
///
/// # 这里有两处必须是手写的
///
/// - `hover` / `active`：这是**自己画的** `div`，组件库的 `Button` 自带悬停效果
///   而 `div` 没有（`AGENTS.md` §7.5：自己画的可点元素必须手写，否则鼠标移上去
///   毫无反应）；
/// - `id`：`active` 来自 `StatefulInteractiveElement`，**只对带 id 的元素可用**
///   （漏了是编译错误，不是运行时 panic）。
///
/// `id_prefix` 由调用页给（`detail` / `distro-detail`），保证同一帧里两页的
/// 元素 id 不撞。名字统一 `truncate()`：外层单元格本来就有 `overflow_hidden`，
/// 不 truncate 的话长名字是"硬切"（没有省略号），truncate 之后是"…"，严格更好。
pub(crate) fn name_link(
    id_prefix: &'static str,
    name: &str,
    entity: &Entity<Shell>,
    open: impl Fn(&mut Shell, String, &mut Context<Shell>) + 'static,
) -> AnyElement {
    let entity = entity.clone();
    // 闭包是 `Fn`（GPUI 会反复调用它），所以里面必须**再 clone 一次** ——
    // 直接把手里的 `String` move 进去会编译不过（`borrow of moved value`）。
    let for_click = name.to_owned();
    div()
        .id(SharedString::from(format!("{id_prefix}-{name}")))
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
            entity.update(cx, |shell, cx| open(shell, target, cx));
        })
        .into_any_element()
}

/// 一组**互斥选项** —— 官方 `RadioGroup`（横向），选中项由 `selected` 指出。
///
/// # 为什么不再用"一排按钮 + 选中那颗改成 `.primary()`"
///
/// 那是本项目原先的写法（9 处）。问题是**语义错了**：一排按钮对读屏软件来说是
/// 一堆各自独立的"动作"，而不是"这组里选了哪个"；键盘上也拿不到
/// "方向键在组内移动、空格选中"那套官方行为。官方 `Radio` 自带这些。
///
/// # 从 `Radio` 源码（0.8.0）核出来的两条
///
/// 1. `RadioGroup::new` 的默认 style 带 **`.flex_1()`** —— 放进 `v_flex` 会被
///    竖向拉长（和 `Field` 是同一个坑），所以这里显式 `.flex_none()` 盖掉
///    （`refine_style` 在 render 末尾，能盖住）；
/// 2. 横向布局在 render 里是 `h_flex().w_full().flex_wrap()` + **固定 `gap_3()`**，
///    改不了。所以它适合"独占一行的一组选项"，**不适合塞进紧凑工具栏**
///    （日志浮层那排"行数 / 时间戳 / 刷新"就只能继续用按钮）。
///
/// 每项的 id 由官方按**位置**给（`radio.id = ix.into()`），组 id 是我们传的这个 ——
/// GPUI 的元素 id 是**路径**，所以同页多组之间不会撞（已按源码确认）。
///
/// `pick` 收的是"选中的下标"，由调用页映射回自己的值：
/// 组件的回调本来就只给 `&usize`，硬要泛型化成一个 `T` 反而要在调用点写更多。
pub(crate) fn radio_row(
    id: &'static str,
    labels: Vec<SharedString>,
    selected: Option<usize>,
    entity: &Entity<Shell>,
    pick: impl Fn(&mut Shell, usize, &mut Window, &mut Context<Shell>) + 'static,
) -> AnyElement {
    let entity = entity.clone();
    RadioGroup::horizontal(id)
        .flex_none()
        .children(labels.into_iter().map(Radio::from))
        .selected_index(selected)
        .on_change(move |ix, window, cx| {
            let ix = *ix;
            entity.update(cx, |shell, cx| pick(shell, ix, window, cx));
        })
        .into_any_element()
}

/// 一排**互斥的 tab** —— 官方 `TabBar` + `Tab`，选中项由 `selected` 指出。
///
/// # 为什么收成一处
///
/// 「应用设置」的四个 tab、「数据库管理」的两个 tab、SQL 的多结果 tab 原先
/// 各写一份接线：建 `Vec<Tab>` → 算选中下标 → `TabBar::new(id).pill()…on_click`。
/// 三份逐字相同，只有"哪个枚举"和"选完写回哪"不同 —— 而**同一类控件在一个
/// 应用里必须长得一样、行为一样**。
///
/// # 为什么不再用"一排按钮 + 选中那颗 `.primary()`"
///
/// 那是本项目的旧写法（v0.18 前 9 处）。它把**状态**画成了**动作**：一排按钮
/// 对读屏软件是一堆各自独立的动作，键盘上也拿不到 tab 该有的左右箭头移动。
/// 组件库本来就有 `TabBar`（下划线 / 胶囊等形态由 `TabVariant` 管）。
///
/// ⚠️ `on_click` 回调给的是**下标**（`&usize`），由调用页映射回自己的值 ——
/// 组件的 API 就是这样，硬泛型化成一个 `T` 反而让调用点更长。
///
/// ⚠️ 它**不适合塞进紧凑工具栏**：横向布局在 `RadioGroup` 那边有同样的限制，
/// 而这排 tab 自带间距与滚动，塞进一行按钮中间会把那一行撑坏。工具栏里的
/// 行数档位（日志浮层）因此仍是按钮。
pub(crate) fn tab_bar(
    id: &'static str,
    labels: Vec<SharedString>,
    selected: usize,
    entity: &Entity<Shell>,
    pick: impl Fn(&mut Shell, usize, &mut Window, &mut Context<Shell>) + 'static,
) -> AnyElement {
    use gpui_kit::component::tab::{Tab, TabBar};

    let entity = entity.clone();
    TabBar::new(id)
        .pill()
        .selected_index(selected)
        .children(labels.into_iter().map(|label| Tab::new().label(label)))
        .on_click(move |ix, window, cx| {
            let ix = *ix;
            entity.update(cx, |shell, cx| pick(shell, ix, window, cx));
        })
        .into_any_element()
}

/// 把"读 `Shell` 状态画一块内容"包成**子实体**，让它能随状态变化自动重画。
///
/// # 为什么需要它（这是 v0.19.0 那个 bug 的一般形式）
///
/// 官方 `Dialog` 的 builder 闭包**只在"对话框层被重建"时重跑**，
/// 普通的状态变化（打字、后台任务推进、`cx.notify()`）**不会**触发它。
///
/// v0.19.0 实机就是这条：输入框里填了路径，等效命令还停在 `<待填>`。
/// 修法是把内容做成子实体 —— 而"日志 / exec 输出 / 进度"这些浮层是同一个问题：
/// 它们的内容**一直在变**（后台任务每几帧 notify 一次）。
///
/// 所以这里把这件事做成通用件：`ShellView` 持有 `Entity<Shell>` 并 `observe` 它，
/// Shell 一 notify 它就 `cx.notify()` 自己，于是**只重画这块内容**。
///
/// # 用法
///
/// ```ignore
/// let body = cx.new({
///     let entity = entity.clone();
///     move |cx| ShellView::new(entity, |shell, cx| views::exec_dialog_body(shell, cx))
/// });
/// // 然后 dialog.child(body)
/// ```
///
/// `render` 闭包收 `(&Shell, &App)`，返回 `AnyElement` —— 保持"纯函数渲染"那套写法，
/// 只是外面多了一层会重画的壳。
/// `ShellView` 的渲染闭包类型。
///
/// 抽成别名不只是为了少打字 —— 直接写在字段上会被 clippy 的
/// `type_complexity` 判为"类型太复杂"（`lint` 是阻塞的）。
type ShellRenderFn = Rc<dyn Fn(&Shell, &App) -> AnyElement>;

pub(crate) struct ShellView {
    entity: Entity<Shell>,
    render: ShellRenderFn,
}

impl ShellView {
    /// 建实体并**订阅 `Shell`**（`observe` 的返回值丢掉就取消订阅，所以必须 `detach`）。
    pub(crate) fn new(
        entity: Entity<Shell>,
        render: impl Fn(&Shell, &App) -> AnyElement + 'static,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&entity, |_, _, cx| cx.notify()).detach();
        Self {
            entity,
            render: Rc::new(render),
        }
    }
}

impl Render for ShellView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let render = self.render.clone();
        let shell = self.entity.read(cx);
        // `read` 的借用要在 `cx` 再被借之前结束 —— 所以先把元素算出来。
        render(shell, cx)
    }
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

/// 「创建容器」弹窗的**表单字段**：标签 + 输入框。
///
/// `full` 决定宽度：直接放进 `v_flex` 的用 `w_full`，
/// 放进 `h_flex` 成对排列的用 `flex_1`。
/// （在 `v_flex` 里写 `flex_1` 会把字段**竖向**拉长，是个容易踩的坑 ——
/// 官方 `Field::render` 第一行就是 `.flex_1()`，所以**两种宽度都必须显式覆盖**，
/// 见 [`form_field_with_error`]。）
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
///
/// # 为什么外壳改用官方 `Field`
///
/// 原先这里是手写的 `v_flex().gap_1().child(标签 div).child(输入框)` —— 52 处调用点
/// 拼出来的形状完全一样。官方 `form` 模块给的正是这个壳（`Form` / `Field`），
/// 所以壳交给它，我们只保留"颜色"和"错误态"这两件它管不了的事
/// （`AGENTS.md` §5：颜色归我们，形态归组件）。
///
/// 读 `form/field.rs`（0.8.0）确认过的四件事：
///
/// 1. `Field::render` **第一行是 `.flex_1()`** —— 直接放进 `v_flex` 会把字段竖向
///    拉长（就是上面那条注释说的坑）。所以下面两种宽度都**显式覆盖**：
///    `full` → `.flex_none().w_full()`，否则 → `.flex_1().min_w_0()`。
///    `refine_style(&self.style)` 在 render 的最后才执行，我们的能盖掉它的默认值；
/// 2. 标签默认是 `text_sm + font_medium` + 主题色。我们这一套设计是
///    **`text_xs` + 变暗表示"必填未填"**，所以走 `label_fn` 自己画 ——
///    外面那层 `wrap_label` 的字号继承会被内层的 `.text_xs()` 覆盖；
/// 3. `description(..)` 渲染成 `text_xs + muted_foreground`，正好是我们要的"提示"
///    档位，将来要加说明不必再手写；
/// 4. **`Field` 没有 error 态**（只有 `required` 的红色星号）—— 所以"标签变红"
///    仍然由我们通过 `label_fn` 的颜色表达，"边框变红"仍然打在 `Input` 上。
///
/// ⚠️ 一处已知瑕疵：`Field` 内部会给自己的包裹层 `.id(self.id)`，而 `self.id`
/// 只有在**经由 `Form` 的 enumerate** 构建时才被喂进 `ix`（`Field::props`）。
/// 我们这里是**单独**建 `Field`（为了保留每个弹窗自己的布局），所以 id 恒为 0 ——
/// 多个字段共用一个元素 id。这些包裹层不挂任何 hover / 点击 / 滚动处理，
/// 共享的只是"没有状态"的那份状态，所以现在看不出问题；
/// 哪天有字段需要在 `Field` 外壳上做悬停效果，就得改成由 `Form` 统一建。
pub(crate) fn form_field_with_error(
    id: &'static str,
    label: &'static str,
    input: &Entity<InputState>,
    cx: &App,
    full: bool,
    invalid: bool,
) -> AnyElement {
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

    let field = Field::new()
        // 标签自己画：颜色要说"必填未填"，字号要压到 `text_xs`。
        .label_fn(move |_, _| div().text_xs().text_color(label_color).child(label))
        .child(input_el);

    if full {
        field.flex_none().w_full().into_any_element()
    } else {
        field.flex_1().min_w_0().into_any_element()
    }
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
