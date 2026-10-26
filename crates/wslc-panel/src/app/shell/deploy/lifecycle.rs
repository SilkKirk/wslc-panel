//! 已部署应用起停（`start_deployment` / `stop_deployment`）。

use super::super::super::prelude::*;
use super::super::Shell;

impl Shell {
    /// 「启动」一条部署。
    pub fn start_deployment(&mut self, app: String, cx: &mut Context<Self>) {
        let Some(action) = self.deploy_action(&app) else {
            return;
        };
        if self
            .state
            .deploy
            .as_ref()
            .is_some_and(|d| d.progress.is_some())
        {
            // ⚠️ 别静默 return：**别的行**这时照样画着按钮（忙碌是按应用名算的，
            // 见 `views::deploy_row_actions`），用户点了必须知道为什么没反应。
            self.deploy_busy_notice(cx);
            return;
        }
        self.set_deploy_progress(
            Some(DeployProgress {
                app: app.clone(),
                phase: DeployPhase::Starting,
            }),
            cx,
        );
        let wslc = self.state.wslc.clone();
        let container = action.container.clone();
        cx.spawn(async move |this, cx| {
            // ⚠️ `container` 要在外面**留一份**：下面那个 `async move` 只搬一份
            // **克隆**进去。写回状态前必须问一句"这一页还是那个容器吗" ——
            // 见 `Shell::deploy_page_is`。
            let result = {
                let wslc = wslc.clone();
                let container = container.clone();
                cx.background_executor()
                    .spawn(async move {
                        deploy_cmd::start(&wslc, &container, &action.target, &action.command)
                            .map_err(|e| e.to_string())
                    })
                    .await
            };
            let _ = this.update(cx, |shell, cx| {
                if shell.deploy_page_is(&container) {
                    shell.clear_deploy_progress_for(&app, cx);
                }
                match result {
                    Ok(()) => shell.state.notify(Toast::info(format!("已启动 {app}"))),
                    Err(message) => {
                        if shell.deploy_page_is(&container) {
                            if let Some(deploy) = shell.state.deploy.as_mut() {
                                deploy.fail(message.clone());
                            }
                        }
                        shell
                            .state
                            .notify(Toast::error(format!("启动失败：{message}")));
                    }
                }
                // 现状是**问容器**得来的，与"哪一页在显示"无关：
                // 这一句刷的永远是**当前**那一页，所以不用判容器。
                shell.refresh_deploy_status(cx);
            });
        })
        .detach();
    }

    /// 「停止」一条部署（只发 `TERM`，不做 `kill -9`）。
    pub fn stop_deployment(&mut self, app: String, cx: &mut Context<Self>) {
        let Some(action) = self.deploy_action(&app) else {
            return;
        };
        // ⚠️ 这条守卫是 v0.9.13 补的：早先它**没有**，于是 A 正在跑动作时点 B 的
        // 「停止」会直接覆盖掉 A 的进度（A 的回调回来时又把 B 的「正在…」清掉，
        // B 的按钮提前复活、能再点一次）。同 `start_deployment` 的理由。
        if self
            .state
            .deploy
            .as_ref()
            .is_some_and(|d| d.progress.is_some())
        {
            self.deploy_busy_notice(cx);
            return;
        }
        self.set_deploy_progress(
            Some(DeployProgress {
                app: app.clone(),
                // ⚠️ 这里早先写的是 `Starting` —— 用户按的是「停止」，
                // 那一行却写着"正在启动…"。v0.9.13 才有 `Stopping` 这一档。
                phase: DeployPhase::Stopping,
            }),
            cx,
        );
        let wslc = self.state.wslc.clone();
        let container = action.container.clone();
        cx.spawn(async move |this, cx| {
            let result = {
                let wslc = wslc.clone();
                let container = container.clone();
                cx.background_executor()
                    .spawn(async move {
                        deploy_cmd::stop(&wslc, &container, &action.target)
                            .map_err(|e| e.to_string())
                    })
                    .await
            };
            let _ = this.update(cx, |shell, cx| {
                if shell.deploy_page_is(&container) {
                    shell.clear_deploy_progress_for(&app, cx);
                }
                match result {
                    Ok(()) => shell.state.notify(Toast::info(format!("已停止 {app}"))),
                    Err(message) => {
                        if shell.deploy_page_is(&container) {
                            if let Some(deploy) = shell.state.deploy.as_mut() {
                                deploy.fail(message.clone());
                            }
                        }
                        shell
                            .state
                            .notify(Toast::error(format!("停止失败：{message}")));
                    }
                }
                shell.refresh_deploy_status(cx);
            });
        })
        .detach();
    }
}
