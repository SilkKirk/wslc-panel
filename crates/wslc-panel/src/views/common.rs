//! 跨页面共用的**小组件**：卡片、徽标、表格壳、单元格、按钮族、表单格、日志块。
//!
//! 这里的每一样都被两个以上的页面用到 —— 只被一处用到的助手留在那个页面自己的模块里。
//!
//! # 模块划分（2026-10-26 拆）
//!
//! 33 个条目已经不是一类东西了，所以按"**这个助手画的是什么**"分成四个文件：
//!
//! | 子模块 | 里面是什么 |
//! |---|---|
//! | [`layout`] | 卡片 / 键值对行 / 空状态 / 计数行 / 页头 / 弹窗底栏 / 存档警告条 / 等待态灰字 / 失败横幅与说明卡 |
//! | [`table`] | 官方静态表格的壳 + 各类单元格 |
//! | [`form`] | 折叠区 / 单选组 / tab 排 / 表单字段 |
//! | [`button`] | 行内小按钮的唯一外壳 + 五个具名助手 + 侧边栏折叠按钮 |
//!
//! **判据**：新加一个助手之前先问"它画的是布局、表格、表单、还是按钮？"
//! —— 答不出来就说明它还不该进 `common`（那多半是某个页面自己的东西）。
//!
//! 本文件只留**四类都不是**的那几个：徽标（`badge`）、名字链接（`name_link`）、
//! 等宽文本块（`log_text`），以及两个"跨页面共用的**一句话**"——
//! [`NoContainer`] 的整页空态文案与 [`REOPEN_DB_PAGE`] 的防御分支文案。
//! 它们都是"同一句话在多个文件里各写一份就会漂"的那个问题，与画法无关。
//!
//! `common.rs` 与 `common/` 目录**并存**（`mod xxx;` 解析到 `common/xxx.rs`），
//! 子模块用 `pub(crate) use xxx::*;` 再导出 —— 30 多个调用点仍然写 `common::…`，
//! 一行都不用改。

use super::prelude::*;

// `ShellView` 的渲染闭包要共享所有权；`views` 的 prelude 里没有 `Rc`。
use std::rc::Rc;

mod button;
mod form;
mod layout;
mod table;
// 再导出，调用点仍然写 common::cell_text(..) / common::page_empty(..) / …。
pub(crate) use button::*;
pub(crate) use form::*;
pub(crate) use layout::*;
pub(crate) use table::*;

// 弹窗的**遮罩与卡片外壳**不在这里：19 个浮层全部走官方 `Dialog` 的窗口级
// 层栈（`app::shell::dialogs`），遮罩、尺寸、标题、入场动画都由它负责。
// `common` 只保留"内容里会用到"的助手 —— 包括弹窗底部那行按钮
// [`modal_footer`]（它属于内容，不属于外壳：官方 `Dialog` 不会自动画 OK / Cancel，
// 所以底栏仍在 `layout` 里）。

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
