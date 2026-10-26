//! `DialogKey` 的构造实现：每个键怎么变成 `DialogSpec`。
//!
//! 和父模块拆开只是因为 `open_dialog_for` 一个 match 就有 1000 行；
//! 层序 / 状态对齐 / 打开关闭的公共逻辑仍在 `dialogs.rs`。

use super::*;
use crate::views::modal_footer;

impl Shell {
    /// 记账：窗口里那一层已经关了，把 `dialog_stack` 里的记录也去掉。
    ///
    /// 三个关闭入口里**有两个不是我们关的**：
    ///
    /// | 入口 | 谁关的 | 谁记账 |
    /// |---|---|---|
    /// | 页脚按钮 | 我们（`close_top_dialog`） | 同一个函数 |
    /// | Esc / 点遮罩 / 标题栏 ✕ | **官方 `Dialog`** | `open_shell_dialog` 的 `on_cancel` |
    /// | `sync_dialog_stack` 自己的 pop | 我们 | 它末尾那句 `dialog_stack = desired` |
    ///
    /// 漏了就会"多关一层"：窗口少一层、账没少，下一帧 sync 按账去 pop，
    /// 把**下面那层**也关掉。v0.21.0 实机：详情 → 压缩磁盘 → 取消 → 详情也没了。
    pub(crate) fn forget_top_dialog(&mut self) {
        self.dialog_ui.dialog_stack.pop();
    }

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

        if desired == self.dialog_ui.dialog_stack {
            return;
        }

        // 1. 最长公共前缀
        let common = self
            .dialog_ui
            .dialog_stack
            .iter()
            .zip(desired.iter())
            .take_while(|(a, b)| a == b)
            .count();

        // 2. 非前缀的乱序 → 兜底全关（正常走不到）
        let is_prefix = common == self.dialog_ui.dialog_stack.len().min(desired.len());
        if !is_prefix {
            use gpui_kit::component::WindowExt as _;
            window.close_all_dialogs(cx);
            self.dialog_ui.dialog_stack.clear();
            for key in &desired {
                self.open_dialog_for(*key, window, cx);
            }
            self.dialog_ui.dialog_stack = desired;
            return;
        }

        // 3. 多的从栈顶 pop
        {
            use gpui_kit::component::WindowExt as _;
            for _ in common..self.dialog_ui.dialog_stack.len() {
                window.close_dialog(cx);
            }
        }
        // 4. 缺的依次追加
        for key in desired[common..].iter().copied() {
            self.open_dialog_for(key, window, cx);
        }
        self.dialog_ui.dialog_stack = desired;
    }

    /// 打开一个我们已经迁到官方 `Dialog` 的浮层。
    pub(crate) fn open_dialog_for(
        &mut self,
        key: DialogKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
                                    close_top_dialog(&entity, window, cx);
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
                                    close_top_dialog(&entity, window, cx);
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
                                    entity.update(cx, |shell, cx| shell.dismiss_db_backup(cx));
                                    close_top_dialog(&entity, window, cx);
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
                                    close_top_dialog(&entity, window, cx);
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
                                    close_top_dialog(&entity, window, cx);
                                })
                                .into_any_element(),
                        ]
                    },
                );
            }
            DialogKey::ContainerDetail => {
                let Some(name) = self.dialog_ui.detail.clone() else {
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
                                    close_top_dialog(&entity, window, cx);
                                })
                                .into_any_element(),
                        ]
                    },
                );
            }
            DialogKey::DistroDetail => {
                let Some(name) = self.dialog_ui.distro_detail.clone() else {
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
                                    close_top_dialog(&entity, window, cx);
                                })
                                .into_any_element(),
                        ]
                    },
                );
            }
            DialogKey::Exec => {
                let Some(exec) = self.state.exec.as_ref() else {
                    return;
                };
                let container = exec.container.clone();
                let entity = cx.entity();
                let body_entity = entity.clone();
                let content = shell_view(
                    &entity,
                    move |shell, cx| crate::views::exec_dialog_body(shell, &body_entity, cx),
                    cx,
                );
                let for_button = entity.clone();
                open_shell_dialog(
                    window,
                    cx,
                    DialogSpec {
                        title: format!("执行命令 —— {container}").into(),
                        width: 760.,
                        overlay_closable: true,
                        keyboard: true,
                        content,
                    },
                    Some(Shell::close_exec),
                    move || {
                        let entity = for_button.clone();
                        vec![
                            Button::new("exec-close")
                                .label("关闭")
                                .small()
                                .on_click(move |_, window, cx| {
                                    entity.update(cx, |shell, cx| shell.close_exec(cx));
                                    close_top_dialog(&entity, window, cx);
                                })
                                .into_any_element(),
                        ]
                    },
                );
            }
            DialogKey::Prompt => {
                let Some(prompt) = self.forms.prompt.as_ref() else {
                    return;
                };
                let kind = prompt.kind;
                let distro = prompt.distro.clone();
                let input = prompt.input.clone();
                let entity = cx.entity();
                let content = {
                    let entity = entity.clone();
                    let input = input.clone();
                    let distro = distro.clone();
                    cx.new(move |cx| {
                        crate::views::PromptContent::new(kind, distro, input, entity, cx)
                    })
                };
                let content = DialogContent::Prompt(content);

                // 打开前把焦点交给输入框；`Dialog` 的外壳可能会抢焦点，
                // 所以这里和 `open_prompt` 保持同一条顺序（先 focus、再 open）。
                let handle = input.read(cx).focus_handle(cx);
                window.focus(&handle, cx);

                let for_cancel = entity.clone();
                let for_confirm = entity.clone();
                open_shell_dialog(
                    window,
                    cx,
                    DialogSpec {
                        title: kind.title().into(),
                        width: 560.,
                        overlay_closable: true,
                        keyboard: true,
                        content,
                    },
                    Some(Shell::close_prompt),
                    move || {
                        let cancel = {
                            let entity = for_cancel.clone();
                            Button::new("prompt-cancel").label("取消").small().on_click(
                                move |_, window, cx| {
                                    entity.update(cx, |shell, cx| shell.close_prompt(cx));
                                    close_top_dialog(&entity, window, cx);
                                },
                            )
                        };
                        let confirm = {
                            let entity = for_confirm.clone();
                            Button::new("prompt-confirm")
                                .label(kind.confirm_label())
                                .small()
                                .primary()
                                .on_click(move |_, window, cx| {
                                    // 校验失败时 `submit_prompt` 会保留
                                    // `forms.prompt`（例如值为空）；这时**不能**
                                    // 关窗，否则下一帧 sync 看到状态还在，会把
                                    // 它重新打开。
                                    let closed = entity.update(cx, |shell, cx| {
                                        shell.submit_prompt(cx);
                                        shell.forms.prompt.is_none()
                                    });
                                    if closed {
                                        close_top_dialog(&entity, window, cx);
                                    }
                                })
                        };
                        vec![cancel.into_any_element(), confirm.into_any_element()]
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
                                    close_top_dialog(&entity, window, cx);
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
                                    close_top_dialog(&entity, window, cx);
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
                                    close_top_dialog(&entity, window, cx);
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
                                    close_top_dialog(&entity, window, cx);
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
                                    close_top_dialog(&entity, window, cx);
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
                                    close_top_dialog(&entity, window, cx);
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
                                    close_top_dialog(&entity, window, cx);
                                })
                        };
                        vec![cancel.into_any_element(), confirm.into_any_element()]
                    },
                );
            }
            DialogKey::DbAction => {
                let Some(kind) = self.forms.db_dialog.as_ref().map(|d| d.kind.clone()) else {
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
                                    close_top_dialog(&entity, window, cx);
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
                                    close_top_dialog(&entity, window, cx);
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
                    move |shell, cx| match shell.forms.create_dialog.as_ref() {
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
                                    close_top_dialog(&entity, window, cx);
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
                                    close_top_dialog(&entity, window, cx);
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
                    move |shell, cx| match shell.forms.image_form.as_ref() {
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
                                    close_top_dialog(&entity, window, cx);
                                })
                                .into_any_element(),
                        ]
                    },
                );
            }
            DialogKey::WslConf => {
                let title = match self.forms.wslconf_dialog.as_ref() {
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
                let Some(form) = self.forms.deploy_form.as_ref() else {
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
                    .forms
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
                                    close_top_dialog(&entity, window, cx);
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
                                    close_top_dialog(&entity, window, cx);
                                })
                        };
                        vec![cancel.into_any_element(), submit.into_any_element()]
                    },
                );
            }
        }
    }
}
