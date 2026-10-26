//! `Shell`（GPUI 实体）：结构体定义、以及按功能域分出去的 `impl Shell`。
//!
//! # 布局
//!
//! ```text
//! shell/mod.rs     struct Shell（**就在这里**）、各子模块声明
//! shell/core.rs    构造 / 刷新 / 导航 / 确认 / 提示条
//! shell/dialogs.rs 官方 `Dialog` 的窗口级层栈（DialogKey / 状态对齐 / 各浮层构造）
//! shell/<域>.rs    该域的全部 impl Shell { … }
//! shell/render.rs  impl Render for Shell
//! shell/state.rs   挂在 Shell 上的纯数据
//! ```
//!
//! # 为什么结构体必须留在这一层
//!
//! 下面那些是**子模块**，于是它们是 `Shell` 的后代模块，按 Rust 的私有可见性
//! 规则（「定义模块 + 其后代」）可以直接读写 `Shell` 的**私有**字段
//! （订阅句柄、取消令牌、刷新节奏、详情弹窗那个名字……）。
//! 只有 `forms` 那一组是 `pub(crate)`：`views/` 画输入框要读它们。
//!
//! ⚠️ 反过来说：如果把这个结构体挪进 `core.rs`，那 `db.rs` 就只是它的**兄弟**
//! 模块，连 `self.state` 都碰不到 —— 除非把每个字段都放开成 `pub(crate)`，
//! 而那等于把内部状态暴露给 `views`。**别再挪回去。**
//!
//! （这一条是踩出来的：第一版把结构体放在 `core.rs`，`cargo check` 全线报错。）
//!
//! # 顶层字段为什么要分组
//!
//! 曾经是 42 个**平铺**字段，于是"这个动作会碰哪些状态"得读完整页。现在按
//! **能力**收成几组（`tasks` / `refresh` / `forms` …，每个组的结构体定义在
//! `state.rs`）。搬的时候**一次只搬一组、一组一轮 CI** —— 一次动 42 个字段的
//! 编译面，和"错了不知道错在哪"的代价都不值（见
//! `docs/AUDIT-2026-10-26.md` §4.1 与 §11 的进度表）。

use super::prelude::*;

mod containers;
mod core;
mod db;
mod deploy;
mod dialogs;
mod distros;
mod images;
mod install;
mod render;
mod settings;
mod state;
mod update;

// 对话框栈的标识与开合逻辑住在 `dialogs.rs`（它跨所有域）；这里只再导出标识，
// 因为 `Shell::dialog_stack` 这个字段的类型要它。
pub(crate) use dialogs::DialogKey;
pub(crate) use state::*;

/// 应用外壳。
pub struct Shell {
    /// 全部状态。
    pub state: AppState,
    /// 长任务与"正在进行的动作"的句柄（取消令牌 / 选择器状态 / 吊着的发行版）。
    ///
    /// 收成一组而不是平铺：它们是同一类东西 —— **能力**，不是界面状态
    /// （对应的纯数据在 `AppState::pulling` / `exporting` / `installing`）。
    tasks: Tasks,
    /// 刷新节奏与"按钮现在该显示什么"。
    ///
    /// 同样是界面状态（不进快照）：快照每轮整体重建，放进去反而要每轮重新赋值。
    refresh: Refresh,
    /// 全部输入框 / 表单（12 个，见 [`Forms`]）。
    ///
    /// 这一组**必须** `pub(crate)`：`views/` 画输入框要读它们（`Input::new` 收
    /// `&Entity<InputState>`，而页面渲染只拿得到 `&Shell`）。
    pub(crate) forms: Forms,
    /// 输入框 / 下拉框的订阅句柄（5 个，见 [`Subscriptions`]）。
    ///
    /// 丢了不会报错，只会"选了什么都没反应" —— 所以整组放在一起，见结构体的文档。
    subs: Subscriptions,
    /// 错误横幅是不是展开着（多条错误时才用得上）。
    errors_expanded: bool,
    /// 正在查看详情的容器名；关闭时为 None。
    detail: Option<String>,
    /// 正在查看详情的**发行版名**；关闭时为 None。
    ///
    /// 和容器的 `detail` 分成两个字段（而不是共用一个）：
    /// 两者的详情弹窗内容完全不同，共用一个字符串还得额外判断
    /// "这个名字是容器还是发行版"。
    distro_detail: Option<String>,
    /// 「拉取镜像」弹窗开着吗。
    ///
    /// 和 `forms.image_form` 分开记：**表单建好之后一直留着**（用户第二次打开时
    /// registry / 仓库名 / 上一批 tag 都还在），而这个布尔只管可见性。
    /// 拉取进行中**不许关**（见 [`Shell::close_pull_dialog`]）。
    pull_open: bool,
    /// SQL 控制台这一页的界面状态（6 个，见 [`SqlUi`]）。
    ///
    /// 两个 GPUI 实体（结果表 / 表名树）加四个页内开关 —— 纯数据在 `AppState`，
    /// 实体在这里（`wslc-panel-core` 装不下 GPUI 实体）。
    pub(crate) sql_ui: SqlUi,
    /// 我们已经压进**窗口级对话框栈**的那几层（官方 `Dialog`），按打开顺序。
    ///
    /// # 为什么需要自己记一份
    ///
    /// 官方 `ActiveDialog` **没有 id 字段**（`root.rs` 里只有 focus_handle /
    /// builder 等），所以**没法"按 id 关某一层"**。`WindowExt` 只给三个口子：
    ///
    /// - `close_dialog(cx)` → `active_dialogs.pop()`，**只关栈顶**；
    /// - `close_all_dialogs(cx)` → 全关；
    /// - `has_active_dialog(cx)` → 有没有开着。
    ///
    /// 于是"状态要求哪些浮层开着"和"实际开着哪些"之间只能靠**最长公共前缀**
    /// 来对齐：多出来的从栈顶 pop（安全），缺的依次追加（不动下层）。
    /// 这份记录就是那个 diff 的左边。见 `Shell::sync_dialog_stack`。
    pub(crate) dialog_stack: Vec<DialogKey>,
    /// 左侧**应用导航栏**收起来了没有（v0.12.2）。
    ///
    /// 用的是组件库的 `Sidebar` + `SidebarCollapsible::Offcanvas`：
    /// 收起时它**完全退出布局**（不只是变窄），主区域能拿到全部宽度。
    ///
    /// 为什么不用 `Icon` 模式（收成一条图标轨）：那个模式要求每个菜单项
    /// 都有图标才看得懂，而我们这 7 项一直是纯文字。给它们硬配图标
    /// （`IconName::Server` / `Network` …）得先确认每个名字在 Lucide 里
    /// 都存在 —— 猜错了是编译错误，而我们本地编不了。所以先做"整体收起"。
    pub(crate) sidebar_collapsed: bool,
    /// 「数据库管理」页的「基础信息」浮层开着没有。
    ///
    /// 连接卡（用户名 / 密码 + 那两段说明）原来**常驻页面顶部**，实机反馈
    /// "不用进来就在上面占用空间" —— 现在收进这个浮层，页面顶上是页头 +
    /// 一个「基础信息」按钮。
    pub(crate) db_info_open: bool,
}
