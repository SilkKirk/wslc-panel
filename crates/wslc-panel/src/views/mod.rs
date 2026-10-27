//! 页面渲染。
//!
//! 全部页面都是纯函数：入参是 [`AppState`] 的只读引用 + `Entity<Shell>`，
//! 出参是元素。所有交互通过 `entity.update(...)` 回写到状态，
//! 因此这里没有可变状态，也没有异步逻辑 —— 渲染与数据彻底解耦。
//!
//! # 为什么拆成目录
//!
//! 这个文件原先是一整份**一万行**的 `views.rs`（10 546 行 / 168 个顶层条目）。
//! 它按页面的顺序往下排，但页面之间没有任何边界 —— 想找「部署页那几个浮层」
//! 得先知道它排在第几段，改一处还要担心和别的页面共用着某个助手。
//!
//! 现在按**功能域**分文件（每个文件对应侧边栏的一个页面 / 一类浮层），
//! 共用的小组件沉到 [`common`]，共用导入沉到 [`prelude`]。判断一个函数该放哪：
//! **它被两个以上的页面用到吗？** 是 → `common`；否 → 那个页面自己的模块。
//!
//! 页面分发 [`page`] 留在本文件 —— 它是唯一需要认识所有页面的地方。

mod common;
mod config;
mod containers;
mod create;
mod dashboard;
mod database;
mod db_dialogs;
mod deploy;
mod details;
mod images;
mod install;
mod instances;
mod networks;
mod overlays;
mod prelude;
mod settings;
mod sql;
mod volumes;
mod wslconf;

// ⚠️ 这里**不用 `pub(crate) use xxx::*`**。
//
// glob 重导出会让"谁在外面被用到"变成一件查不清的事：`app/` 里
// `views::` 后面能接哪些名字，取决于每个子模块碰巧定义了什么。改成逐项
// 列出之后，`views` 对外的接口就是本文件这十几行 —— 加一个浮层时漏写
// 这一行会直接编译失败，而不是悄悄多出一个导出。
//
// 列在这里的每一项都是 `app/` 真的调用过、或需要跨子模块使用的。
// （`page` 是页面分发，见文件末尾。）
// 浮层的**外壳**不在这里：19 个浮层全部走官方 `Dialog`（窗口级层栈，见
// `app::shell::dialogs`），`views` 只产出"内容"（`*_body`）。
//
// ⚠️ 这一条**同样约束各子模块内部的可见性**（v0.22.3 起）：只在本文
// 文件里用、别处一次都不调的助手写成 `fn`（私有），别写成 `pub(crate)`。
// 审查时量过：154 个 `pub(crate)` 里有 75 个从没被别的文件调过 ——
// 那种声明的意思是"别处会用"，读代码的人据此判断"改它要小心谁"，
// 而实际上一个人都没用。**接口应当等于真实的使用面。**
// 判据：全仓**代码行**里搜一遍（注释里提到名字不算调用），搜不到就私有；
// 反过来 `page()` 要调的 `*_page` 必须留着 `pub(crate)`。
pub(crate) use common::{ShellView, modal_footer, sidebar_toggle};
pub(crate) use create::create_dialog_body;
pub(crate) use db_dialogs::{
    db_backup_dialog_body, db_backup_progress_body, db_dialog_body, db_import_progress_body,
    db_info_body,
};
pub(crate) use deploy::{
    deploy_dialog_body, deploy_logs_body, deploy_remove_confirm_body, deploy_site_conf_body,
};
pub(crate) use details::{container_detail_body, distro_detail_body};
pub(crate) use images::pull_dialog_body;
pub(crate) use overlays::{PromptContent, exec_dialog_body, export_dialog_body, logs_dialog_body};
pub(crate) use wslconf::wslconf_dialog_body;

use prelude::*;

// 页面分发要认识**每一个**页面，所以这里把各模块的入口逐项列出来 ——
// 这正是「唯一需要认识所有页面的地方」该有的样子（加一个页面就会在这里
// 编译失败，而不是渲染成一页空白）。
//
// 命名统一成 `<模块>::<模块>_page`：入口名**刻意不与模块同名** ——
// 同名时要靠 `use dashboard::dashboard;` 把函数引进命名空间，而"少了这一行
// 就只剩模块名、报 cannot find function"是本文件踩过的坑（v0.18 那条注释）。
// 带 `_page` 后缀之后，`use` 与调用点都不再有这层歧义。
use common::{REOPEN_DB_PAGE, page_empty};
use containers::containers_page;
use dashboard::dashboard_page;
use database::database_page;
use deploy::deploy_page;
use images::images_page;
use install::add_instance_page;
use instances::instances_page;
use networks::networks_page;
use settings::settings_page;
use sql::sql_console_page;
use volumes::volumes_page;

/// 渲染当前页面。
///
/// 收的是 `&Shell` 而不是 `&AppState`：从 P3 起有了**带输入框的页面**
/// （「添加实例」），而 `InputState` 住在 `Shell` 里 ——
/// `AppState` 刻意完全不碰 GPUI，这是那条分层约束的代价，也是它的价值。
///
/// `cx` 只有「添加实例」页用得上（要从输入框里实时读值做命令预览），
/// 但签名统一收着更省事 —— 免得每加一个需要它的页面就改一次分发。
pub(crate) fn page(shell: &Shell, cx: &App, entity: &Entity<Shell>) -> AnyElement {
    let state = &shell.state;
    match state.page {
        // 基本信息页只读快照（不起进程、不需要输入框），所以不用 entity。
        Page::Dashboard => dashboard_page(state).into_any_element(),
        // 这几个直接返回 `AnyElement`，不再多套一层转换。
        Page::Instances => instances_page(state, entity),
        Page::AddInstance => add_instance_page(shell, cx, entity),
        // 「容器」页要读搜索框（`InputState`）并触发筛选，所以收 `&Shell` + `&App`。
        Page::Containers => containers_page(shell, cx, entity).into_any_element(),
        // 「镜像」页只要 `&AppState`：v0.6.3 起拉取表单在弹窗里（那个要 `&Shell`，
        // 见 `pull_dialog_body`），页面本身只剩一个按钮和一张表。
        Page::Images => images_page(state, entity).into_any_element(),
        Page::Networks => networks_page(state, entity).into_any_element(),
        Page::Volumes => volumes_page(state, entity).into_any_element(),
        // 「数据库管理」要 `&Shell`（连接卡那三个输入框住在 `Shell` 里）
        // **和** `cx`（渲染时要读输入框的值）—— 和「添加实例」同一类页面。
        Page::Database => database_page(shell, cx, entity),
        // 「SQL 控制台」（v0.12）：**整页**工作区（左树 + 右上编辑器 + 右下结果）。
        // 要 `&Shell`（编辑器在 `db_form` 里、树的展开状态在 `Shell` 上）
        // 和 `cx`（读编辑器 / 搜索框的值）。
        Page::Sql => match shell.forms.db_form.as_ref() {
            Some(form) => sql_console_page(shell, form, cx, entity),
            // 防御分支（理论上走不到：进这一页时 `ensure_db_form` 已经建好了）。
            // ⚠️ 文案不解释内部状态 —— "编辑器还没建好"是给开发看的，
            // 用户只需要知道**下一步点哪**。
            None => page_empty(REOPEN_DB_PAGE),
        },
        // 「部署管理」也要 `&Shell` + `cx`：「添加部署」弹窗那五格输入框
        // 住在 `Shell` 里（页面本身没有输入框）。
        Page::Deploy => deploy_page(shell, cx, entity),
        Page::AppSettings => settings_page(shell, entity),
    }
}
