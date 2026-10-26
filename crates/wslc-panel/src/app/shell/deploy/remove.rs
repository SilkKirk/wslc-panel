//! 删除部署：请求 → 二次确认 → 真删（文件和存档）。

use super::super::super::prelude::*;
use super::super::Shell;

impl Shell {
    /// 「删除」：先摆出二次确认（**不可撤销**，而且目标是卷挂载点时会删掉卷里
    /// 的数据 —— 所以要把完整路径显示出来）。
    pub fn request_remove_deployment(&mut self, app: String, cx: &mut Context<Self>) {
        if let Some(deploy) = self.state.deploy.as_mut() {
            deploy.confirm_remove = Some(app);
        }
        cx.notify();
    }

    /// 取消删除确认。
    pub fn cancel_remove_deployment(&mut self, cx: &mut Context<Self>) {
        if let Some(deploy) = self.state.deploy.as_mut() {
            deploy.confirm_remove = None;
        }
        cx.notify();
    }

    /// **只**删掉本地那条记录，容器里一个字都不动。
    ///
    /// 给"存档里的运行时键认不出来"那一行用：那种记录面板不知道该拿什么脚本
    /// 去操作它（见 `DeployEntry::runtime` 的说明），但用户总得能把它清掉，
    /// 否则那一行会永远赖在列表里。
    ///
    /// ⚠️ 界面上要把这件事说清（"容器里的文件**不会**被删"），
    /// 否则用户会以为它和「删除」是一回事。
    pub fn forget_deployment_record(&mut self, app: String, cx: &mut Context<Self>) {
        let Some(container) = self.state.deploy.as_ref().map(|d| d.container.clone()) else {
            return;
        };
        self.state.deploy_store.remove(&container, &app);
        if let Some(error) = self.state.save_deploy_store() {
            self.state.notify(Toast::error(error));
        }
        self.state.refresh_deploy_entries(&container);
        self.state.notify(Toast::info(format!(
            "已删掉 {app} 的记录（容器里的文件没动）"
        )));
        cx.notify();
    }

    /// 确认删除：停进程 → 删掉面板铺进目标目录的那些文件 → 删掉本地记录。
    pub fn confirm_remove_deployment(&mut self, app: String, cx: &mut Context<Self>) {
        let Some(action) = self.deploy_action(&app) else {
            return;
        };
        if self
            .state
            .deploy
            .as_ref()
            .is_some_and(|d| d.progress.is_some())
        {
            // 同 `start_deployment`：别的行的按钮还在，点了必须有一句反馈。
            // 确认框先关掉（用户已经表达过意图了，留着它只会挡住别的操作）。
            if let Some(deploy) = self.state.deploy.as_mut() {
                deploy.confirm_remove = None;
            }
            self.deploy_busy_notice(cx);
            return;
        }
        if let Some(deploy) = self.state.deploy.as_mut() {
            deploy.confirm_remove = None;
        }
        self.set_deploy_progress(
            Some(DeployProgress {
                app: app.clone(),
                phase: DeployPhase::Removing,
            }),
            cx,
        );

        let wslc = self.state.wslc.clone();
        let container = action.container.clone();
        let is_process = action.is_process;
        let site = action.site.clone();
        cx.spawn(async move |this, cx| {
            let result = {
                let wslc = wslc.clone();
                let container = container.clone();
                let app = app.clone();
                let site = site.clone();
                cx.background_executor()
                    .spawn(async move {
                        if is_process {
                            // 先停：进程还活着的话，删掉文件它照样在跑（而且 pidfile
                            // 没了就再也停不掉）
                            let _ = deploy_cmd::stop(&wslc, &container, &action.target);
                        }
                        deploy_cmd::remove_target(
                            &wslc,
                            &container,
                            &action.target,
                            &action.members,
                        )
                        .map_err(|e| e.to_string())?;
                        // 文件删掉之后再删 conf：conf 的 root 指的就是这个目录。
                        // 删不掉**不阻断**记录删除（文件已经没了；把记录留下只会
                        // 让「删除」永远失败），但必须说出来。
                        let mut warnings = Vec::new();
                        if let Some(site) = site.as_ref() {
                            match deploy_cmd::remove_site(&wslc, &container, &app, site) {
                                Ok(Some(warning)) => warnings.push(warning),
                                Ok(None) => {}
                                Err(error) => {
                                    warnings.push(format!("站点配置没删掉：{error}"));
                                }
                            }
                        }
                        Ok::<Vec<String>, String>(warnings)
                    })
                    .await
            };

            let _ = this.update(cx, |shell, cx| {
                if shell.deploy_page_is(&container) {
                    shell.clear_deploy_progress_for(&app, cx);
                }
                match result {
                    Ok(warnings) => {
                        // 只有容器那边真的清掉了才删本地记录 —— 否则用户会
                        // "记录没了、东西还在"
                        shell.state.deploy_store.remove(&container, &app);
                        if let Some(error) = shell.state.save_deploy_store() {
                            shell.state.notify(Toast::error(error));
                        }
                        shell.state.refresh_deploy_entries(&container);
                        let mut message = format!("已删除 {app} 的部署（只删了面板铺进去的文件）");
                        if !warnings.is_empty() {
                            message.push_str(&format!("；{}", warnings.join("；")));
                        }
                        shell.state.notify(Toast::info(message));
                    }
                    Err(message) => {
                        if shell.deploy_page_is(&container) {
                            if let Some(deploy) = shell.state.deploy.as_mut() {
                                deploy.fail(message.clone());
                            }
                        }
                        shell
                            .state
                            .notify(Toast::error(format!("删除失败：{message}")));
                    }
                }
                shell.refresh_deploy_status(cx);
            });
        })
        .detach();
    }
}
