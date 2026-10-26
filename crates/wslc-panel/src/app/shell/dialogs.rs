//! 官方 `Dialog` 的窗口级层栈：`DialogKey`、状态对齐、以及每个浮层的构造。
//!
//! # 为什么单独一个文件
//!
//! 这些浮层分属容器 / 发行版 / 数据库 / 部署四个域，但它们的**开合**是同一件事：
//! `Root` 只有一个窗口级栈（`WindowState::active_dialogs`），**层序 = 栈序**。
//! 由一处统一压栈，才不会出现"从详情里打开的确认框被详情盖住"那类层序 bug
//! （`AGENTS.md` §7.5）。
//!
//! 原先这堆代码住在 `distros.rs`（发行版域）：它是最早迁到 `Dialog` 的域，
//! 后来 15 个浮层陆续迁进来也顺手加在那儿，于是那个文件一半篇幅是**别的域**的
//! 东西。挪出来之后：
//!
//! - `distros.rs` 只剩发行版域自己的方法；
//! - 加一个新浮层只动**本文件**（`DialogKey` 一个变体 + `sync_dialog_stack`
//!   的一个 `if` + `open_dialog_for` 的一个臂）；
//! - `Shell` 的结构体仍在 `app/shell/mod.rs` —— 本模块是它的后代，所以能读写
//!   那些私有字段（见 `mod.rs` 里"结构体必须留在这一层"的说明）。

use super::super::prelude::*;
use super::Shell;
use crate::views::{ShellView, modal_footer};

/// 已经压进**窗口级对话框栈**的浮层标识（官方 `Dialog`）。
///
/// # 为什么是一个枚举而不是"直接比状态"
///
/// 状态里那几样东西（`exporting` / `db_backup` …）各自独立，而对话框栈是**有序**的。
/// 用一个 `Vec<DialogKey>` 把"这一帧应该开着哪些、按什么顺序"表达出来，
/// 才能和实际栈做最长公共前缀的比对（官方 `ActiveDialog` 没有 id，见
/// [`Shell::sync_dialog_stack`]）。
///
/// 枚举里的顺序**就是** `DialogKey::ALL` 里的顺序（那个才是层序），
/// 枚举自身的排列只是为了好读 —— 改层序去改 `ALL`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DialogKey {
    /// 「数据库基础信息」对话框（连接卡）。
    DbInfo,
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
    /// 「备份列表」对话框（数据库管理页）。
    DbBackupList,
    /// 通用二次确认（危险操作）。
    Confirm,
    /// 「数据库动作」弹窗（新建库 / 建用户 / 改密 / 授权 / 导入）。
    DbAction,
    /// 「创建容器」弹窗。
    CreateContainer,
    /// 「拉取镜像」弹窗。
    PullImage,
    /// 「发行版配置」（/etc/wsl.conf）弹窗。
    WslConf,
    /// 「添加部署 / 重新部署」弹窗。
    DeployForm,
}

impl DialogKey {
    /// 全部浮层，**顺序 = 层序**（`Root` 的栈按 push 顺序叠，后 push 的在上面）。
    ///
    /// 这张表就是"z 序"的唯一来源。原先顺序藏在 `sync_dialog_stack` 的 17 个
    /// `if` 书写顺序里 —— 加浮层时插在哪、为什么插在那，读的人只能猜。
    ///
    /// ⚠️ **`Confirm` 必须在最后**：它是"从任何浮层里弹出来的二次确认"，
    /// 压在表单弹窗**下面**就是 v0.9.14 那个"压缩磁盘的确认框在详情之下"的同型 bug。
    pub(crate) const ALL: [DialogKey; 17] = [
        // 只读说明卡压在最底下：用户从页头点开后，还会接着打开备份列表 /
        // 数据库动作，那些必须盖在它上面。
        DialogKey::DbInfo,
        DialogKey::Export,
        DialogKey::DbBackupProgress,
        DialogKey::DbImportProgress,
        DialogKey::ContainerLogs,
        DialogKey::ContainerDetail,
        DialogKey::DistroDetail,
        DialogKey::DeployLogs,
        DialogKey::DeploySiteConf,
        DialogKey::DeployRemoveConfirm,
        DialogKey::DbBackupList,
        DialogKey::DeployForm,
        DialogKey::WslConf,
        DialogKey::PullImage,
        DialogKey::CreateContainer,
        DialogKey::DbAction,
        DialogKey::Confirm,
    ];

    /// 这一帧这个浮层该开着吗 —— **唯一判据**（原先散在 17 个 `if` 里）。
    ///
    /// 每个臂都必须能回答"哪些状态同时成立才开"。尤其注意带"页面进得来"
    /// 门槛的那几个（`db_form.is_some()`）：少了前半句，没进数据库页时
    /// 也会弹出一个内容空空的浮层。
    pub(crate) fn is_open(self, shell: &Shell) -> bool {
        match self {
            DialogKey::DbInfo => {
                shell.db_info_open && shell.db_form.is_some() && shell.state.db.is_some()
            }
            DialogKey::Export => shell.state.exporting.is_some(),
            // 备份 / 导入进度：`dismissed` 时不进栈（用户点过「放到后台」），
            // 但任务照跑 —— 它**只影响浮层**，不影响任务本身。
            DialogKey::DbBackupProgress => shell
                .state
                .db
                .as_ref()
                .is_some_and(|db| db.backup.as_ref().is_some_and(|b| !b.dismissed)),
            DialogKey::DbImportProgress => shell
                .state
                .db
                .as_ref()
                .is_some_and(|db| db.import.as_ref().is_some_and(|i| !i.dismissed)),
            DialogKey::ContainerLogs => shell.state.logs.is_some(),
            DialogKey::ContainerDetail => shell.detail.is_some(),
            DialogKey::DistroDetail => shell.distro_detail.is_some(),
            DialogKey::DeployLogs => shell
                .state
                .deploy
                .as_ref()
                .is_some_and(|d| d.logs.is_some()),
            DialogKey::DeploySiteConf => shell
                .state
                .deploy
                .as_ref()
                .is_some_and(|d| d.site_conf.is_some()),
            DialogKey::DeployRemoveConfirm => shell
                .state
                .deploy
                .as_ref()
                .is_some_and(|d| d.confirm_remove.is_some()),
            DialogKey::DbBackupList => {
                shell.db_form.is_some()
                    && shell
                        .state
                        .db
                        .as_ref()
                        .is_some_and(|db| db.backup_dialog.is_some())
            }
            DialogKey::DeployForm => shell.deploy_form.is_some(),
            DialogKey::WslConf => shell.wslconf_dialog.is_some(),
            DialogKey::PullImage => shell.pull_open && shell.image_form.is_some(),
            DialogKey::CreateContainer => shell.create_dialog.is_some(),
            DialogKey::DbAction => shell.db_form.is_some() && shell.db_dialog.is_some(),
            DialogKey::Confirm => shell.state.confirm.is_some(),
        }
    }
}

/// 一个浮层外壳的"差异部分"。
///
/// 其余东西（遮罩、宽度单位、Esc、`content` 的接线、`footer` 的重建）都由
/// [`open_shell_dialog`] 统一处理 —— 17 个臂原先各写一遍，只有这五样不同。
struct DialogSpec {
    title: SharedString,
    width: f32,
    /// 点遮罩关不关（有提交动作的浮层要 `false`，见 `AGENTS.md` §7.5）。
    overlay_closable: bool,
    /// Esc 关不关。
    keyboard: bool,
    content: Entity<ShellView>,
}

/// 把「读 `Shell` 状态画一块」包成会随 `Shell` 重画的子实体。
///
/// ⚠️ 必须包：`Dialog` 的 builder 闭包只在"浮层被重建"时重跑，打字 / 后台任务
/// 推进都**不**触发它 —— 不包就会僵在打开那一刻（v0.19.0 的 bug）。
fn shell_view(
    entity: &Entity<Shell>,
    body: impl Fn(&Shell, &App) -> AnyElement + 'static,
    cx: &mut Context<Shell>,
) -> Entity<ShellView> {
    let view = entity.clone();
    cx.new(move |cx| ShellView::new(view, body, cx))
}

/// 统一打开一个官方 `Dialog`，并把**三条退出路径**（按钮 / Esc / 点遮罩）的
/// "清状态"接好。
///
/// `close` 传方法引用（`Shell::close_logs` 这类）—— 它的签名统一是
/// `fn(&mut Shell, &mut Context<Shell>)`。进度类浮层不可关，传 `None`。
///
/// ⚠️ `footer` 是**闭包**而不是 `Vec`：`Dialog` 的 builder 可能重跑，而
/// `AnyElement` 是一次性的，不能复用。
///
/// ⚠️ 清状态必须写在回调里：`sync_dialog_stack` 是**按状态对齐**的，
/// 只关窗口不清状态，下一帧会把它重新打开（`AGENTS.md` §7.5）。
fn open_shell_dialog<F>(
    window: &mut Window,
    cx: &mut Context<Shell>,
    spec: DialogSpec,
    close: Option<fn(&mut Shell, &mut Context<Shell>)>,
    footer: F,
) where
    F: Fn() -> Vec<AnyElement> + 'static,
{
    use gpui_kit::component::WindowExt as _;

    let entity = cx.entity();
    let on_cancel = close.map(|close| {
        let entity = entity.clone();
        move |_: &ClickEvent, _: &mut Window, cx: &mut App| -> bool {
            entity.update(cx, close);
            true
        }
    });

    window.open_dialog(cx, move |dialog, _window, _cx| {
        let mut dialog = dialog
            .title(spec.title.clone())
            .w(px(spec.width))
            .overlay(true)
            .overlay_closable(spec.overlay_closable)
            .keyboard(spec.keyboard)
            .child(spec.content.clone());
        if let Some(on_cancel) = on_cancel.clone() {
            dialog = dialog.on_cancel(on_cancel);
        }
        let items = footer();
        if !items.is_empty() {
            dialog = dialog.footer(modal_footer(items));
        }
        dialog
    });
}

impl Shell {
    /// 把「窗口级对话框栈」对齐到当前状态（官方 `Dialog`）。
    ///
    /// # 算法：最长公共前缀
    ///
    /// 官方 `ActiveDialog` **没有 id 字段**，`WindowExt` 只给
    /// `close_dialog`（关栈顶）/ `close_all_dialogs` / `has_active_dialog`。
    /// 所以"该开哪些"和"已经开着哪些"只能按**顺序**对齐：
    ///
    /// 1. 求 `dialog_stack` 与 `desired` 的最长公共前缀 `L`；
    /// 2. `dialog_stack.len() > L` → 从栈顶 pop 掉多出来的（**删的都是最上面几层，
    ///    正是 `close_dialog` 能关的，所以不可能关错窗**）；
    /// 3. `desired.len() > L` → 依次把缺的追加进去（**不动下面已有的层**，
    ///    所以下层不会被重建、焦点不会丢）。
    ///
    /// 例：详情里点「压缩磁盘」→ `[Detail]` 变成 `[Detail, Confirm]`，
    /// 前缀相同 → **只 open Confirm**；确认框点「取消」→ 反过来 pop 一次。
    ///
    /// 只有"非前缀的乱序"（例如 `[A, B]` 变成 `[B]`）才需要 `close_all_dialogs`
    /// 兜底 —— 按现在的状态机走不到（上层挡着下层），但正确性优先。
    ///
    /// 每帧都会调一次，但**只在真的要变时才动窗口**（第一行那个 return）。
    pub(crate) fn sync_dialog_stack(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 「这一帧该开哪些、按什么顺序」只有一张表：`DialogKey::ALL` + `is_open`。
        //
        // 原先这里是 17 个顺序 `if`（约 90 行），层序**只**体现在书写顺序里，
        // 而且"要不要开"的判据和"怎么开"（`open_dialog_for`）分居两处 ——
        // 加一个浮层要动三个地方，漏一个的表现是"浮层自己弹出来"或"点了没反应"。
        let desired: Vec<DialogKey> = DialogKey::ALL
            .iter()
            .copied()
            .filter(|key| key.is_open(self))
            .collect();
        if desired == self.dialog_stack {
            return;
        }

        // 1. 最长公共前缀
        let common = self
            .dialog_stack
            .iter()
            .zip(desired.iter())
            .take_while(|(a, b)| a == b)
            .count();

        // 2. 非前缀的乱序 → 兜底全关（正常走不到）
        let is_prefix = common == self.dialog_stack.len().min(desired.len());
        if !is_prefix {
            use gpui_kit::component::WindowExt as _;
            window.close_all_dialogs(cx);
            self.dialog_stack.clear();
            for key in &desired {
                self.open_dialog_for(*key, window, cx);
            }
            self.dialog_stack = desired;
            return;
        }

        // 3. 多的从栈顶 pop
        {
            use gpui_kit::component::WindowExt as _;
            for _ in common..self.dialog_stack.len() {
                window.close_dialog(cx);
            }
        }
        // 4. 缺的依次追加
        for key in desired[common..].iter().copied() {
            self.open_dialog_for(key, window, cx);
        }
        self.dialog_stack = desired;
    }

    /// 打开一个我们已经迁到官方 `Dialog` 的浮层。
    pub(crate) fn open_dialog_for(
        &mut self,
        key: DialogKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_kit::component::WindowExt as _;

        match key {
            DialogKey::DbInfo => {
                let entity = cx.entity();
                let body_entity = entity.clone();
                let content = shell_view(
                    &entity,
                    move |shell, cx| crate::views::db_info_body(shell, &body_entity, cx),
                    cx,
                );
                let for_button = entity.clone();
                open_shell_dialog(
                    window,
                    cx,
                    DialogSpec {
                        title: "基础信息".into(),
                        width: 700.,
                        overlay_closable: true,
                        keyboard: true,
                        content,
                    },
                    // 三条退出路径都要清 `db_info_open`，否则下一帧会重开。
                    Some(Shell::close_db_info),
                    move || {
                        let entity = for_button.clone();
                        vec![
                            Button::new("db-info-close")
                                .label("关闭")
                                .small()
                                .on_click(move |_, window, cx| {
                                    entity.update(cx, |shell, cx| shell.close_db_info(cx));
                                    window.close_dialog(cx);
                                })
                                .into_any_element(),
                        ]
                    },
                );
            }
            DialogKey::Export => {
                let Some(progress) = self.state.exporting.as_ref() else {
                    return;
                };
                let entity = cx.entity();
                let content = shell_view(
                    &entity,
                    move |shell, _cx| crate::views::export_dialog_body(&shell.state),
                    cx,
                );
                let for_button = entity.clone();
                open_shell_dialog(
                    window,
                    cx,
                    DialogSpec {
                        title: format!("正在导出 {}", progress.name).into(),
                        width: 620.,
                        overlay_closable: false,
                        keyboard: false,
                        content,
                    },
                    Some(Shell::cancel_export),
                    move || {
                        let entity = for_button.clone();
                        vec![
                            Button::new("export-cancel")
                                .label("取消导出")
                                .small()
                                .on_click(move |_, window, cx| {
                                    entity.update(cx, |shell, cx| shell.cancel_export(cx));
                                    window.close_dialog(cx);
                                })
                                .into_any_element(),
                        ]
                    },
                );
            }
            DialogKey::DbBackupProgress => {
                let Some(db) = self.state.db.as_ref() else {
                    return;
                };
                let Some(b) = db.backup.as_ref() else {
                    return;
                };
                let entity = cx.entity();
                let content = shell_view(
                    &entity,
                    move |shell, _cx| match shell.state.db.as_ref() {
                        Some(db) => crate::views::db_backup_progress_body(db),
                        None => div().into_any_element(),
                    },
                    cx,
                );
                let for_button = entity.clone();
                open_shell_dialog(
                    window,
                    cx,
                    DialogSpec {
                        title: format!("正在备份 {}", b.database).into(),
                        width: 560.,
                        overlay_closable: false,
                        keyboard: false,
                        content,
                    },
                    // 进度类不可关（Esc / 点遮罩都不行），所以没有 on_cancel。
                    None,
                    move || {
                        let entity = for_button.clone();
                        vec![
                            Button::new("db-backup-dismiss")
                                .label("放到后台")
                                .small()
                                .on_click(move |_, window, cx| {
                                    entity.update(cx, |shell, cx| shell.cancel_db_backup(cx));
                                    window.close_dialog(cx);
                                })
                                .into_any_element(),
                        ]
                    },
                );
            }
            DialogKey::DbImportProgress => {
                let Some(db) = self.state.db.as_ref() else {
                    return;
                };
                let Some(i) = db.import.as_ref() else {
                    return;
                };
                let entity = cx.entity();
                let content = shell_view(
                    &entity,
                    move |shell, _cx| match shell.state.db.as_ref() {
                        Some(db) => crate::views::db_import_progress_body(db),
                        None => div().into_any_element(),
                    },
                    cx,
                );
                let for_button = entity.clone();
                open_shell_dialog(
                    window,
                    cx,
                    DialogSpec {
                        title: format!("正在导入 {}", i.file_name).into(),
                        width: 560.,
                        overlay_closable: false,
                        keyboard: false,
                        content,
                    },
                    None,
                    move || {
                        let entity = for_button.clone();
                        vec![
                            Button::new("db-import-dismiss")
                                .label("放到后台")
                                .small()
                                .on_click(move |_, window, cx| {
                                    entity.update(cx, |shell, cx| shell.dismiss_db_import(cx));
                                    window.close_dialog(cx);
                                })
                                .into_any_element(),
                        ]
                    },
                );
            }
            DialogKey::ContainerLogs => {
                let Some(container) = self.state.logs.as_ref().map(|l| l.container.clone()) else {
                    return;
                };
                let entity = cx.entity();
                let body_entity = entity.clone();
                let content = shell_view(
                    &entity,
                    move |shell, _cx| crate::views::logs_dialog_body(&shell.state, &body_entity),
                    cx,
                );
                let for_button = entity.clone();
                open_shell_dialog(
                    window,
                    cx,
                    DialogSpec {
                        title: format!("日志 —— {container}").into(),
                        width: 760.,
                        overlay_closable: true,
                        keyboard: true,
                        content,
                    },
                    Some(Shell::close_logs),
                    move || {
                        let entity = for_button.clone();
                        vec![
                            Button::new("logs-close")
                                .label("关闭")
                                .small()
                                .on_click(move |_, window, cx| {
                                    entity.update(cx, |shell, cx| shell.close_logs(cx));
                                    window.close_dialog(cx);
                                })
                                .into_any_element(),
                        ]
                    },
                );
            }
            DialogKey::ContainerDetail => {
                let Some(name) = self.detail.clone() else {
                    return;
                };
                let entity = cx.entity();
                let body_entity = entity.clone();
                let body_name = name.clone();
                let content = shell_view(
                    &entity,
                    move |shell, _cx| {
                        crate::views::container_detail_body(&body_name, &shell.state, &body_entity)
                    },
                    cx,
                );
                let for_button = entity.clone();
                open_shell_dialog(
                    window,
                    cx,
                    DialogSpec {
                        title: name.into(),
                        width: 700.,
                        overlay_closable: true,
                        keyboard: true,
                        content,
                    },
                    Some(Shell::close_detail),
                    move || {
                        let entity = for_button.clone();
                        vec![
                            Button::new("detail-close")
                                .label("关闭")
                                .small()
                                .on_click(move |_, window, cx| {
                                    entity.update(cx, |shell, cx| shell.close_detail(cx));
                                    window.close_dialog(cx);
                                })
                                .into_any_element(),
                        ]
                    },
                );
            }
            DialogKey::DistroDetail => {
                let Some(name) = self.distro_detail.clone() else {
                    return;
                };
                let entity = cx.entity();
                let body_entity = entity.clone();
                let body_name = name.clone();
                let content = shell_view(
                    &entity,
                    move |shell, _cx| {
                        crate::views::distro_detail_body(&body_name, &shell.state, &body_entity)
                    },
                    cx,
                );
                let for_button = entity.clone();
                open_shell_dialog(
                    window,
                    cx,
                    DialogSpec {
                        title: name.into(),
                        width: 800.,
                        overlay_closable: true,
                        keyboard: true,
                        content,
                    },
                    Some(Shell::close_distro_detail),
                    move || {
                        let entity = for_button.clone();
                        vec![
                            Button::new("distro-detail-close")
                                .label("关闭")
                                .small()
                                .on_click(move |_, window, cx| {
                                    entity.update(cx, |shell, cx| shell.close_distro_detail(cx));
                                    window.close_dialog(cx);
                                })
                                .into_any_element(),
                        ]
                    },
                );
            }
            DialogKey::DeployLogs => {
                let Some(deploy) = self.state.deploy.as_ref() else {
                    return;
                };
                let Some(logs) = deploy.logs.as_ref() else {
                    return;
                };
                let title = format!("日志 · {}", logs.app);
                let entity = cx.entity();
                let content = shell_view(
                    &entity,
                    move |shell, _cx| match shell
                        .state
                        .deploy
                        .as_ref()
                        .and_then(|d| d.logs.as_ref())
                    {
                        Some(logs) => crate::views::deploy_logs_body(logs),
                        None => div().into_any_element(),
                    },
                    cx,
                );
                let for_button = entity.clone();
                open_shell_dialog(
                    window,
                    cx,
                    DialogSpec {
                        title: title.into(),
                        width: 680.,
                        overlay_closable: true,
                        keyboard: true,
                        content,
                    },
                    Some(Shell::close_deploy_logs),
                    move || {
                        let entity = for_button.clone();
                        vec![
                            Button::new("deploy-logs-close")
                                .label("关闭")
                                .small()
                                .on_click(move |_, window, cx| {
                                    entity.update(cx, |shell, cx| shell.close_deploy_logs(cx));
                                    window.close_dialog(cx);
                                })
                                .into_any_element(),
                        ]
                    },
                );
            }
            DialogKey::DeploySiteConf => {
                let Some(conf) = self
                    .state
                    .deploy
                    .as_ref()
                    .and_then(|d| d.site_conf.as_ref())
                else {
                    return;
                };
                let title = format!("站点配置 · {}", conf.app);
                let entity = cx.entity();
                let content = shell_view(
                    &entity,
                    move |shell, _cx| match shell
                        .state
                        .deploy
                        .as_ref()
                        .and_then(|d| d.site_conf.as_ref())
                    {
                        Some(conf) => crate::views::deploy_site_conf_body(conf),
                        None => div().into_any_element(),
                    },
                    cx,
                );
                let for_reapply = entity.clone();
                let for_close = entity.clone();
                open_shell_dialog(
                    window,
                    cx,
                    DialogSpec {
                        title: title.into(),
                        width: 700.,
                        overlay_closable: true,
                        keyboard: true,
                        content,
                    },
                    Some(Shell::close_deploy_site_conf),
                    move || {
                        let reapply = {
                            let entity = for_reapply.clone();
                            Button::new("deploy-site-conf-reapply")
                                .label("重写配置")
                                .small()
                                .on_click(move |_, _, cx| {
                                    entity.update(cx, |shell, cx| {
                                        let app = shell
                                            .state
                                            .deploy
                                            .as_ref()
                                            .and_then(|d| d.site_conf.as_ref())
                                            .map(|c| c.app.clone());
                                        if let Some(app) = app {
                                            shell.reapply_deploy_site(app, cx);
                                        }
                                    });
                                })
                        };
                        let close = {
                            let entity = for_close.clone();
                            Button::new("deploy-site-conf-close")
                                .label("关闭")
                                .small()
                                .on_click(move |_, window, cx| {
                                    entity.update(cx, |shell, cx| shell.close_deploy_site_conf(cx));
                                    window.close_dialog(cx);
                                })
                        };
                        vec![reapply.into_any_element(), close.into_any_element()]
                    },
                );
            }
            DialogKey::DeployRemoveConfirm => {
                let Some(app) = self
                    .state
                    .deploy
                    .as_ref()
                    .and_then(|d| d.confirm_remove.clone())
                else {
                    return;
                };
                let title = format!("删除部署「{app}」？");
                let entity = cx.entity();
                let body_app = app.clone();
                let content = shell_view(
                    &entity,
                    move |shell, _cx| {
                        crate::views::deploy_remove_confirm_body(
                            &body_app,
                            shell.state.deploy.as_ref(),
                        )
                    },
                    cx,
                );
                let for_cancel = entity.clone();
                let for_confirm = entity.clone();
                open_shell_dialog(
                    window,
                    cx,
                    DialogSpec {
                        title: title.into(),
                        width: 560.,
                        overlay_closable: true,
                        keyboard: true,
                        content,
                    },
                    // `overlay_closable(true)` 就必须配 close —— 否则点遮罩 Dialog
                    // 关了、`confirm_remove` 还留着，下一帧又把它弹回来
                    // （v0.20.0 报的那个"点其他位置也会弹出"）。
                    Some(Shell::cancel_remove_deployment),
                    move || {
                        let cancel = {
                            let entity = for_cancel.clone();
                            Button::new("deploy-remove-cancel")
                                .label("取消")
                                .small()
                                .on_click(move |_, window, cx| {
                                    entity
                                        .update(cx, |shell, cx| shell.cancel_remove_deployment(cx));
                                    window.close_dialog(cx);
                                })
                        };
                        let confirm = {
                            let entity = for_confirm.clone();
                            let app = app.clone();
                            Button::new("deploy-remove-confirm")
                                .label("确认删除")
                                .small()
                                .danger()
                                .on_click(move |_, window, cx| {
                                    let app = app.clone();
                                    entity.update(cx, |shell, cx| {
                                        shell.confirm_remove_deployment(app, cx)
                                    });
                                    window.close_dialog(cx);
                                })
                        };
                        vec![cancel.into_any_element(), confirm.into_any_element()]
                    },
                );
            }
            DialogKey::DbBackupList => {
                let Some(database) = self
                    .state
                    .db
                    .as_ref()
                    .and_then(|db| db.backup_dialog.clone())
                else {
                    return;
                };
                let entity = cx.entity();
                let body_entity = entity.clone();
                let content = shell_view(
                    &entity,
                    move |shell, cx| match shell.state.db.as_ref() {
                        Some(db) => crate::views::db_backup_dialog_body(
                            db,
                            &shell.state.db_credentials,
                            shell,
                            &body_entity,
                            cx,
                        ),
                        None => div().into_any_element(),
                    },
                    cx,
                );
                let for_button = entity.clone();
                open_shell_dialog(
                    window,
                    cx,
                    DialogSpec {
                        title: format!("备份 —— {database}").into(),
                        width: 800.,
                        overlay_closable: true,
                        keyboard: true,
                        content,
                    },
                    Some(Shell::close_db_backup_dialog),
                    move || {
                        let entity = for_button.clone();
                        vec![
                            Button::new("db-backup-close")
                                .label("关闭")
                                .small()
                                .on_click(move |_, window, cx| {
                                    entity.update(cx, |shell, cx| shell.close_db_backup_dialog(cx));
                                    window.close_dialog(cx);
                                })
                                .into_any_element(),
                        ]
                    },
                );
            }
            DialogKey::Confirm => {
                let Some(action) = self.state.confirm.clone() else {
                    return;
                };
                let title = action.title().to_owned();
                let ok_label = action.confirm_label().to_owned();
                let entity = cx.entity();
                let body_action = action.clone();
                let content = shell_view(
                    &entity,
                    move |_shell, _cx| super::core::confirm_body(&body_action),
                    cx,
                );
                let for_cancel = entity.clone();
                let for_confirm = entity.clone();
                open_shell_dialog(
                    window,
                    cx,
                    DialogSpec {
                        title: title.into(),
                        width: 460.,
                        overlay_closable: false,
                        keyboard: true,
                        content,
                    },
                    Some(Shell::cancel_pending),
                    move || {
                        let cancel = {
                            let entity = for_cancel.clone();
                            Button::new("confirm-cancel")
                                .label("取消")
                                .small()
                                .on_click(move |_, window, cx| {
                                    entity.update(cx, |shell, cx| shell.cancel_pending(cx));
                                    window.close_dialog(cx);
                                })
                        };
                        let confirm = {
                            let entity = for_confirm.clone();
                            let ok_label = ok_label.clone();
                            Button::new("confirm-ok")
                                .label(ok_label)
                                .small()
                                .primary()
                                .on_click(move |_, window, cx| {
                                    entity.update(cx, |shell, cx| shell.confirm_pending(cx));
                                    window.close_dialog(cx);
                                })
                        };
                        vec![cancel.into_any_element(), confirm.into_any_element()]
                    },
                );
            }
            DialogKey::DbAction => {
                let Some(kind) = self.db_dialog.as_ref().map(|d| d.kind.clone()) else {
                    return;
                };
                let title = kind.title().to_owned();
                let ok_label = kind.confirm_label().to_owned();
                let is_import = kind.is_import();
                let entity = cx.entity();
                let body_entity = entity.clone();
                let content = shell_view(
                    &entity,
                    move |shell, cx| crate::views::db_dialog_body(shell, &body_entity, cx),
                    cx,
                );
                let for_cancel = entity.clone();
                let for_confirm = entity.clone();
                open_shell_dialog(
                    window,
                    cx,
                    DialogSpec {
                        title: title.into(),
                        width: 700.,
                        overlay_closable: false,
                        keyboard: true,
                        content,
                    },
                    Some(Shell::close_db_dialog),
                    move || {
                        let cancel = {
                            let entity = for_cancel.clone();
                            Button::new("db-dialog-cancel")
                                .label("取消")
                                .small()
                                .on_click(move |_, window, cx| {
                                    entity.update(cx, |shell, cx| shell.close_db_dialog(cx));
                                    window.close_dialog(cx);
                                })
                        };
                        let confirm = {
                            let entity = for_confirm.clone();
                            let ok_label = ok_label.clone();
                            Button::new("db-dialog-confirm")
                                .label(ok_label)
                                .small()
                                .primary()
                                .on_click(move |_, window, cx| {
                                    // 导入不是一条 SQL（送文件进去让 mysql 读），
                                    // 走自己的提交路径。
                                    entity.update(cx, |shell, cx| {
                                        if is_import {
                                            shell.start_db_import(window, cx);
                                        } else {
                                            shell.submit_db_dialog(window, cx);
                                        }
                                    });
                                    window.close_dialog(cx);
                                })
                        };
                        vec![cancel.into_any_element(), confirm.into_any_element()]
                    },
                );
            }
            DialogKey::CreateContainer => {
                let entity = cx.entity();
                let body_entity = entity.clone();
                let content = shell_view(
                    &entity,
                    move |shell, cx| match shell.create_dialog.as_ref() {
                        Some(dialog) => {
                            crate::views::create_dialog_body(dialog, &shell.state, &body_entity, cx)
                        }
                        None => div().into_any_element(),
                    },
                    cx,
                );
                let for_cancel = entity.clone();
                let for_confirm = entity.clone();
                open_shell_dialog(
                    window,
                    cx,
                    DialogSpec {
                        title: "创建容器".into(),
                        width: 800.,
                        overlay_closable: false,
                        keyboard: true,
                        content,
                    },
                    Some(Shell::close_create_dialog),
                    move || {
                        let cancel = {
                            let entity = for_cancel.clone();
                            Button::new("create-cancel").label("取消").small().on_click(
                                move |_, window, cx| {
                                    entity.update(cx, |shell, cx| shell.close_create_dialog(cx));
                                    window.close_dialog(cx);
                                },
                            )
                        };
                        let confirm = {
                            let entity = for_confirm.clone();
                            Button::new("create-ok")
                                .label("创建并启动")
                                .small()
                                .primary()
                                .on_click(move |_, window, cx| {
                                    // `window` 是必需的：提交时要检查"预填了但没填值"
                                    // 的必需变量，并把焦点送到那一格。
                                    entity.update(cx, |shell, cx| shell.confirm_create(window, cx));
                                    window.close_dialog(cx);
                                })
                        };
                        vec![cancel.into_any_element(), confirm.into_any_element()]
                    },
                );
            }
            DialogKey::PullImage => {
                // 在闭包**外面**算：self 的借用不能逃进 'static 闭包（E0521）。
                let pulling = self.state.pulling.is_some();
                let entity = cx.entity();
                let body_entity = entity.clone();
                let content = shell_view(
                    &entity,
                    move |shell, cx| match shell.image_form.as_ref() {
                        Some(form) => {
                            crate::views::pull_dialog_body(form, &shell.state, &body_entity, cx)
                        }
                        None => div().into_any_element(),
                    },
                    cx,
                );
                let for_button = entity.clone();
                open_shell_dialog(
                    window,
                    cx,
                    DialogSpec {
                        title: "拉取镜像".into(),
                        width: 760.,
                        overlay_closable: false,
                        // 拉取进行中不许用 Esc 关，避免"关了任务还在跑"。
                        keyboard: !pulling,
                        content,
                    },
                    Some(Shell::close_pull_dialog),
                    move || {
                        // ⚠️ 拉取**进行中**时这个弹窗关不掉（`close_pull_dialog` 会拒绝
                        // 并给提示），所以那时不画「关闭」——一个按下去只弹提示的
                        // 按钮不如没有。
                        if pulling {
                            return Vec::new();
                        }
                        let entity = for_button.clone();
                        vec![
                            Button::new("pull-dialog-close")
                                .label("关闭")
                                .small()
                                .on_click(move |_, window, cx| {
                                    entity.update(cx, |shell, cx| shell.close_pull_dialog(cx));
                                    window.close_dialog(cx);
                                })
                                .into_any_element(),
                        ]
                    },
                );
            }
            DialogKey::WslConf => {
                let title = match self.wslconf_dialog.as_ref() {
                    Some(_) => match self.state.wslconf.as_ref() {
                        Some(w) => format!("{} 配置（/etc/wsl.conf）", w.distro),
                        None => "配置（/etc/wsl.conf）".to_owned(),
                    },
                    None => return,
                };
                let entity = cx.entity();
                let body_entity = entity.clone();
                let content = shell_view(
                    &entity,
                    move |shell, cx| crate::views::wslconf_dialog_body(shell, &body_entity, cx),
                    cx,
                );
                open_shell_dialog(
                    window,
                    cx,
                    DialogSpec {
                        title: title.into(),
                        width: 820.,
                        // 配置表单填一半不该被误点丢掉。
                        overlay_closable: false,
                        keyboard: true,
                        content,
                    },
                    // ⚠️ 必须给 close：否则 Esc 也关不掉，这个弹窗就没有任何
                    // "不用按钮"的退出路径；而且不清状态的话下一帧会重开。
                    Some(Shell::close_wslconf),
                    // 底部按钮在内容里（预览 / 取消 / 保存 ×2），不走 footer。
                    // `Vec::new` 本身就是 `Fn() -> Vec<AnyElement>`（clippy 会拦
                    // `|| Vec::new()` 这种冗余闭包）。
                    Vec::new,
                );
            }
            DialogKey::DeployForm => {
                let Some(form) = self.deploy_form.as_ref() else {
                    return;
                };
                let title = match form.editing.clone() {
                    Some(app) => format!("重新部署 {app}"),
                    None => match self.state.deploy.as_ref() {
                        Some(d) => format!("添加部署 · {}", d.runtime.label()),
                        None => "添加部署".to_owned(),
                    },
                };
                let re_deploy = self
                    .deploy_form
                    .as_ref()
                    .is_some_and(|f| f.editing.is_some());
                let entity = cx.entity();
                let body_entity = entity.clone();
                let content = shell_view(
                    &entity,
                    move |shell, cx| crate::views::deploy_dialog_body(shell, &body_entity, cx),
                    cx,
                );
                let for_cancel = entity.clone();
                let for_submit = entity.clone();
                open_shell_dialog(
                    window,
                    cx,
                    DialogSpec {
                        title: title.into(),
                        width: 800.,
                        overlay_closable: false,
                        keyboard: true,
                        content,
                    },
                    Some(Shell::close_deploy_dialog),
                    move || {
                        let cancel = {
                            let entity = for_cancel.clone();
                            Button::new("deploy-cancel").label("取消").small().on_click(
                                move |_, window, cx| {
                                    entity.update(cx, |shell, cx| shell.close_deploy_dialog(cx));
                                    window.close_dialog(cx);
                                },
                            )
                        };
                        let submit = {
                            let entity = for_submit.clone();
                            Button::new("deploy-submit")
                                .label(if re_deploy { "重新部署" } else { "部署" })
                                .small()
                                .primary()
                                .on_click(move |_, window, cx| {
                                    entity.update(cx, |shell, cx| shell.submit_deploy(window, cx));
                                    window.close_dialog(cx);
                                })
                        };
                        vec![cancel.into_any_element(), submit.into_any_element()]
                    },
                );
            }
        }
    }
}
