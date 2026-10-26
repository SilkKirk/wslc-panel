//! `Shell`（GPUI 实体）：结构体定义、以及按功能域分出去的 `impl Shell`。
//!
//! # 布局
//!
//! ```text
//! shell/mod.rs     struct Shell（**就在这里**）、各子模块声明
//! shell/core.rs    构造 / 刷新 / 导航 / 确认 / 提示条
//! shell/<域>.rs    该域的全部 impl Shell { … }
//! shell/render.rs  impl Render for Shell
//! shell/state.rs   挂在 Shell 上的纯数据
//! ```
//!
//! # 为什么结构体必须留在这一层
//!
//! 下面那些是**子模块**，于是它们是 `Shell` 的后代模块，按 Rust 的私有可见性
//! 规则（「定义模块 + 其后代」）可以直接读写 `Shell` 的 20 个**私有**字段
//! （订阅句柄、取消令牌、进行中的任务……）。
//!
//! ⚠️ 反过来说：如果把这个结构体挪进 `core.rs`，那 `db.rs` 就只是它的**兄弟**
//! 模块，连 `self.state` 都碰不到 —— 除非把每个字段都放开成 `pub(crate)`，
//! 而那等于把内部状态暴露给 `views`。**别再挪回去。**
//!
//! （这一条是踩出来的：第一版把结构体放在 `core.rs`，`cargo check` 全线报错。）

use super::prelude::*;

mod containers;
mod core;
mod db;
mod deploy;
mod distros;
mod images;
mod install;
mod render;
mod settings;
mod state;
mod update;

pub(crate) use state::*;

/// 已经压进**窗口级对话框栈**的浮层标识（官方 `Dialog`）。
///
/// # 为什么是一个枚举而不是"直接比状态"
///
/// 状态里那几样东西（`exporting` / `db_backup` …）各自独立，而对话框栈是**有序**的。
/// 用一个 `Vec<DialogKey>` 把"这一帧应该开着哪些、按什么顺序"表达出来，
/// 才能和实际栈做最长公共前缀的比对（官方 `ActiveDialog` 没有 id，见
/// `Shell::dialog_stack` 的说明）。
///
/// ⚠️ 迁移进度：目前只有「导出进度」走这条路。其余浮层还在
/// `views::modal_scrim` 的自绘路径上，**不在这个栈里** ——
/// 所以 `Shell::sync_dialog_stack` 现在只认这一种。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DialogKey {
    /// 「正在导出」进度对话框。
    Export,
    /// 「正在备份数据库」进度对话框。
    DbBackupProgress,
    /// 「正在导入 SQL」进度对话框。
    DbImportProgress,
    /// 「容器日志」对话框。
    ContainerLogs,
    /// 「容器详情」对话框。
    ContainerDetail,
    /// 「发行版详情」对话框。
    DistroDetail,
    /// 「应用日志」对话框（部署管理页）。
    DeployLogs,
    /// 「站点配置」对话框（部署管理页）。
    DeploySiteConf,
    /// 「删除部署」确认框。
    DeployRemoveConfirm,
}

/// 应用外壳。
pub struct Shell {
    /// 全部状态。
    pub state: AppState,
    /// 最近一次采集完成的时间（自动刷新与手动刷新都算）。
    ///
    /// 只用于页头那句「最后刷新：N 秒前」—— 它是**界面状态**，不进快照：
    /// 快照每轮整体重建，放进去反而要每轮重新赋值。
    last_refresh: Option<Instant>,
    /// 错误横幅是不是展开着（多条错误时才用得上）。
    errors_expanded: bool,
    /// 「容器」页的搜索框；进过那一页之后一直是 `Some`。
    ///
    /// `InputState::new` 要 `&mut Window`，所以只能在进入页面时建一次
    /// （见 [`Shell::ensure_container_search`]）。`pub(crate)` 是因为
    /// `views.rs` 要读它来画 `Input`。
    pub(crate) container_search: Option<Entity<InputState>>,
    /// 搜索框内容变化的订阅。
    ///
    /// ⚠️ **必须存着**：`Subscription` 一 drop 就退订，表现是"打字时列表
    /// 不跟着变"，不会报错 —— 这是本仓库踩过的坑（见 `select_subs`）。
    container_search_sub: Option<Subscription>,
    /// 正在进行的拉取任务的取消令牌。
    ///
    /// 放 `Shell`：这是**能力**（能 kill 子进程），不是"状态"。
    /// 纯数据部分（拉了哪个镜像、输出了什么）在 `AppState::pulling` 里，
    /// 因为 `views.rs` 只拿得到 `&AppState`。
    pull_cancel: Option<wslc_core::CancelToken>,
    /// 正在进行的**导出**任务的取消令牌。
    ///
    /// 和 `pull_cancel` 并列：两者都是长任务，都能取消，但一个走 `wslc.exe`、
    /// 一个走 `wsl.exe`，句柄类型也不同，合并只会让这个字段变成 `enum`。
    export_cancel: Option<wslc_core::CancelToken>,
    /// 是否正在等一个文件 / 目录选择器的结果。
    ///
    /// 用来**防止重复弹窗**：选择器没有超时（用户不点完它就一直开着），
    /// 连点两下「浏览…」会堆出两个对话框。见 `cmd::picker` 的说明。
    picking: bool,
    /// 本面板正在吊着的发行版（见 [`KeepAlive`]）。
    ///
    /// 界面只看到 `AppState::kept_alive` 里的名字；真正的进程句柄留在这里 ——
    /// 那是"能力"，不是界面状态。
    keep_alive: Vec<KeepAlive>,
    /// 「发行版配置（`/etc/wsl.conf`）」弹窗的输入框；关闭时为 `None`。
    ///
    /// `views::wslconf_overlay` 要读它来画 `Input`，所以是 `pub(crate)`。
    pub(crate) wslconf_dialog: Option<WslConfDialog>,
    /// 「新终端的起始目录」那个自定义路径输入框。
    ///
    /// 点过设置页的「自定义路径…」之后才是 `Some`，建完就**一直留着**
    /// （和 `install_form` 同一个理由：`InputState::new` 要 `&mut Window`，
    /// 而渲染时每帧重建输入框会让用户**一个字都打不进去**）。
    ///
    /// `pub(crate)`：`views.rs` 要读它来画 `Input`。
    pub(crate) terminal_dir_input: Option<Entity<InputState>>,
    /// 「创建容器」弹窗；关闭时为 `None`。
    create_dialog: Option<CreateDialog>,
    /// 「容器 exec」浮层的命令输入框；浮层关闭时为 `None`。
    ///
    /// 和 `create_dialog` 一样懒创建（`InputState::new` 要 `&mut Window`）。
    /// 纯数据部分（跑过什么、输出是什么）在 `AppState::exec` 里。
    pub(crate) exec_input: Option<Entity<InputState>>,
    /// 正在查看详情的容器名；关闭时为 None。
    detail: Option<String>,
    /// 正在查看详情的**发行版名**；关闭时为 None。
    ///
    /// 和容器的 `detail` 分成两个字段（而不是共用一个）：
    /// 两者的详情弹窗内容完全不同，共用一个字符串还得额外判断
    /// "这个名字是容器还是发行版"。
    distro_detail: Option<String>,
    /// 「添加实例」页的表单。
    ///
    /// 和 `create_dialog` 一样**懒创建**（`InputState::new` 要 `&mut Window`），
    /// 但**不随页面离开而销毁** —— 见 [`InstallForm`]。
    ///
    /// `pub(crate)`：`views.rs` 要读它来渲染页面（`Shell` 的其余字段
    /// 只有 `app.rs` 自己用，所以是私有的）。
    pub(crate) install_form: Option<InstallForm>,
    /// 两个下拉框（商店清单 / 镜像源清单）的**订阅**。
    ///
    /// ⚠️ 必须存着：`Context::subscribe_in` 返回的 `Subscription`
    /// **一 drop 就退订**（组件库自己也是 `vec![cx.subscribe_in(..)]` 存起来的）。
    /// 丢了它不会报错，只是"选了下拉框什么都不会发生" —— 那种 bug 很难查。
    select_subs: Vec<Subscription>,
    /// 「镜像」页的表单（registry / 仓库名两个输入框 + tag 下拉框）；
    /// 进过那一页之后一直是 `Some`。
    ///
    /// `pub(crate)`：`views.rs` 要读它来渲染页面。
    pub(crate) image_form: Option<ImageForm>,
    /// 「镜像」页 tag 下拉框的**订阅**。
    ///
    /// ⚠️ 和 `select_subs` 同一个坑：`Subscription` **一 drop 就退订**，
    /// 表现是"在下拉框里选了一个 tag，什么都不发生"—— 不会报错，所以很难查。
    image_subs: Vec<Subscription>,
    /// 「拉取镜像」弹窗开着吗。
    ///
    /// 和 `image_form` 分开记：**表单建好之后一直留着**（用户第二次打开时
    /// registry / 仓库名 / 上一批 tag 都还在），而这个布尔只管可见性。
    /// 拉取进行中**不许关**（见 [`Shell::close_pull_dialog`]）。
    pull_open: bool,
    /// 「创建容器」弹窗里那个「从本地镜像里选」下拉框的**订阅**。
    ///
    /// ⚠️ 和 `select_subs` / `image_subs` 同一个坑：`Subscription`
    /// **一 drop 就退订**，表现是"选了一个镜像，输入框没变"。
    ///
    /// 它跟着弹窗的**创建**走（每次开弹窗重建一份），所以这里不用
    /// `Option` —— 弹窗关掉时订阅也跟着丢，正好。
    create_subs: Vec<Subscription>,
    /// 单输入框提示弹窗（移动位置 / 调整大小 / 设置默认用户）；关闭时为 `None`。
    ///
    /// 同样是 `pub(crate)`：浮层在 `views.rs` 里渲染。
    pub(crate) prompt: Option<TextPrompt>,
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
    /// 正在跑的安装（事件队列 + 结果）；空闲时为 `None`。
    ///
    /// 纯数据部分（走到第几步、日志、进度）在 `AppState::installing` 里 ——
    /// `views.rs` 只拿得到 `&AppState`。
    install_run: Option<InstallRun>,
    /// 正在跑的安装的取消令牌。
    ///
    /// 和 `export_cancel` 并列：都是长任务、都能取消，但一个走 `wsl.exe`
    /// （外加 `curl.exe`），另一个只有 `wsl --export`。
    install_cancel: Option<InstallCancel>,
    /// 采集期间又有刷新请求进来了吗（本轮跑完要立刻补一次）。
    ///
    /// 丢不得：操作（启动 / 停止 / 创建 / 删除）完成后都会补刷，撞上 3 秒一次的
    /// 自动刷新时若把这次请求丢掉，界面要等到下一个周期才变 ——
    /// 用户看到的就是"点了停止，状态还是运行中"。
    refresh_again: bool,
    /// 当前这轮采集是否**由用户发起**（点按钮 / 操作完成后补刷）。
    ///
    /// 决定顶部按钮要不要显示「刷新中…」。3 秒一次的自动刷新是**后台**行为：
    /// 它每轮都会把 `state.busy` 置起来（并发守卫要用），但按钮不该跟着闪 ——
    /// 否则按钮几乎永远停在「刷新中…」上（用户看到的"按钮一直是刷新中"）。
    refresh_visible: bool,
    /// 已经跑过多少轮刷新。
    ///
    /// 用来把 `wsl --status` 降到 30 秒一次（见 [`Shell::status_every_n_ticks`]）。
    /// 放在 `Shell` 而不是 `AppState`：这是**调度细节**，不是界面要展示的状态。
    status_tick: u32,
    /// 已经跑过多少轮刷新（给**部署页状态**用的那一份计数）。
    ///
    /// 和 `status_tick` 分开：部署页那一列要问的是"每一条部署现在跑没跑"，
    /// 一条一次 `exec` —— 跟着 3 秒一次太快（见 [`Shell::deploy_status_every_n_ticks`]）。
    deploy_tick: u32,
    /// 「数据库管理」页的连接卡 / SQL 输入框；没进过那一页时为 `None`。
    ///
    /// `pub(crate)`：页面在 `views.rs` 里渲染。
    pub(crate) db_form: Option<DbPageForm>,
    /// SQL 结果表格的 `TableState`（v0.11）。
    ///
    /// # 为什么在 `Shell` 而不是 `AppState`
    ///
    /// `TableState` 是 GPUI 的实体（`Entity<TableState<D>>`），`wslc-panel-core`
    /// 不依赖 GPUI，装不下它。这和 `container_search` 的处境一样：
    /// **界面状态**留在 `Shell`，纯数据（`DbState::sql_results`）在 `AppState`。
    ///
    /// # 什么时候重建
    ///
    /// 每次跑完 SQL 就**整个换掉**（见 [`Shell::apply_db_console_result`]）：
    /// 结果集变了，列定义、行、排序全都要重新来。缓存着旧的不如重建 ——
    /// 重建的代价只是按列名估一次宽度（前 100 行），微不足道。
    pub(crate) sql_table:
        Option<Entity<gpui_kit::component::table::TableState<crate::sql_table::SqlResultDelegate>>>,
    /// 当前在看的**第几个**结果（多语句时按 tab 分开）。
    pub(crate) sql_result_tab: usize,
    /// 「看原始输出」是不是展开着。
    ///
    /// 原始输出（`mysql --table` 的 ASCII 表格）不常看 —— 只在排查
    /// 列宽异常 / 字符集问题时才有用，所以默认收起。
    pub(crate) sql_raw_open: bool,
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
    /// SQL 页左侧**表名树**收起来了没有（v0.12.2）。
    ///
    /// 和上面分开记：一个是应用级导航，一个是这一页的工作区侧栏，
    /// 两者该各自开关（看代码时不想看到导航栏，不代表也不想看表结构）。
    pub(crate) tree_collapsed: bool,
    /// 库 / 表树的**树控件状态**（v0.12.1）。
    ///
    /// # 为什么换成组件的 `Tree`
    ///
    /// v0.12 里这棵树是**自己用 `Button` 一行行画的**，实机一看就丑：
    /// `Button` 会把文字**居中**、给每一行套一个整行宽度的底色和内边距，
    /// 于是 110 行看起来像 110 个色块，完全不像树（缩进也读不出来）。
    ///
    /// `gpui-kit` 自带 `component::tree::Tree`（虚拟滚动 + 逐级展开 + 选中态 +
    /// 右键菜单），官方 `examples/editor` 就是用它做文件树的。
    /// 它自己管展开/折叠和选中，我们只管把 `TreeItem` 喂进去。
    ///
    /// ⚠️ `TreeState` 是 GPUI 实体，`wslc-panel-core` 装不下 —— 和
    /// [`Shell::sql_table`] 同一个理由：纯数据（`DbState::schema_tree`）在
    /// core，**界面状态**在 `Shell`。
    pub(crate) schema_tree_state: Option<Entity<gpui_kit::component::tree::TreeState>>,
    /// 树是**按哪次查询**建出来的（用于判断要不要重建）。
    ///
    /// 存 `(搜索关键字, 表总数)`：两者都没变就不重建 —— `TreeState` 重建会
    /// 丢掉用户已经展开的层级，所以这件事必须**只在真的变了**时做。
    pub(crate) schema_tree_built_for: Option<(String, usize)>,
    /// 库 / 表树的搜索框（`InputState::new` 要 `&mut Window`，所以懒创建）。
    pub(crate) schema_search: Option<Entity<InputState>>,
    /// 搜索框内容变化的订阅。
    ///
    /// ⚠️ **必须存着**：`Subscription` 一 drop 就退订，表现是"打字时树不跟着过滤"，
    /// 不会报错 —— 这是本仓库踩过的坑（见 `container_search_sub`）。
    pub(crate) schema_search_sub: Option<Subscription>,
    /// 「数据库管理」页的一次性动作弹窗；关闭时为 `None`。
    ///
    /// 和 `create_dialog` 一样懒创建（`InputState::new` 要 `&mut Window`）。
    pub(crate) db_dialog: Option<DbDialog>,
    /// 「数据库管理」页的「基础信息」浮层开着没有。
    ///
    /// 连接卡（用户名 / 密码 + 那两段说明）原来**常驻页面顶部**，实机反馈
    /// "不用进来就在上面占用空间" —— 现在收进这个浮层，页面顶上是页头 +
    /// 一个「基础信息」按钮。
    pub(crate) db_info_open: bool,
    /// 「部署管理」页的「添加部署 / 重新部署」弹窗；关闭时为 `None`。
    ///
    /// 只有这一个要 window（里面全是输入框）。这一页另外两个浮层
    /// （**日志**、**删除确认**）都是纯数据，住在 `AppState::deploy` 里 ——
    /// 它们只有按钮和文字，没有 `InputState`。
    pub(crate) deploy_form: Option<DeployForm>,
}
