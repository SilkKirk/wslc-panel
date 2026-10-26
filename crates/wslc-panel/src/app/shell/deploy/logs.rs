//! 应用日志：打开时读一次 + 定时追，关掉时取消。

use super::super::super::prelude::*;
use super::super::Shell;

impl Shell {
    /// 打开某个应用的日志浮层。
    pub fn open_deploy_logs(&mut self, app: String, cx: &mut Context<Self>) {
        let Some(action) = self.deploy_action(&app) else {
            return;
        };
        if let Some(deploy) = self.state.deploy.as_mut() {
            deploy.logs = Some(DeployLogs {
                app: app.clone(),
                text: None,
                busy: true,
            });
        }
        cx.notify();

        let wslc = self.state.wslc.clone();
        let container = action.container.clone();
        cx.spawn(async move |this, cx| {
            let result = {
                let wslc = wslc.clone();
                let container = container.clone();
                cx.background_executor()
                    .spawn(async move {
                        deploy_cmd::read_log(&wslc, &container, &action.target)
                            .map_err(|e| e.to_string())
                    })
                    .await
            };
            let _ = this.update(cx, |shell, cx| {
                let Some(deploy) = shell.state.deploy.as_mut() else {
                    return;
                };
                // ⚠️ 两道守卫：**容器**（用户可能切走了）+ **应用名**
                //（用户可能关掉这个浮层又开了另一个应用的）。`AppState::deploy`
                // 只有一个槽，漏了任何一道都会把结果写进别人的浮层。
                if deploy.container != container {
                    return;
                }
                let Some(logs) = deploy.logs.as_mut() else {
                    return;
                };
                if logs.app != app {
                    return;
                }
                logs.busy = false;
                match result {
                    Ok(text) => logs.text = Some(text),
                    Err(message) => {
                        logs.text = Some(format!("读日志失败：{message}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 关掉日志浮层。
    pub fn close_deploy_logs(&mut self, cx: &mut Context<Self>) {
        if let Some(deploy) = self.state.deploy.as_mut() {
            deploy.logs = None;
        }
        cx.notify();
    }
}
