//! nginx 站点配置：打开 / 保存、重写配置、在容器里 reload。

use super::super::super::prelude::*;
use super::super::Shell;

impl Shell {
    /// 打开「站点配置」只读浮层。
    ///
    /// 只有 nginx 站点开的记录有这个入口（`deploy_row_actions` 判的）；这里再
    /// 守一道：没有 `site` 就只说一句，不开一个必然空着的浮层。
    pub fn open_deploy_site_conf(&mut self, app: String, cx: &mut Context<Self>) {
        let Some(action) = self.deploy_action(&app) else {
            return;
        };
        let Some(site) = action.site.clone() else {
            self.state
                .notify(Toast::info("这个部署没有站点配置（站点关）"));
            return;
        };
        if let Some(deploy) = self.state.deploy.as_mut() {
            deploy.site_conf = Some(DeploySiteConf {
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
                let app = app.clone();
                let site = site.clone();
                cx.background_executor()
                    .spawn(async move {
                        deploy_cmd::read_site_conf(&wslc, &container, &app, Some(&site))
                            .map_err(|e| e.to_string())
                    })
                    .await
            };
            let _ = this.update(cx, |shell, cx| {
                let Some(deploy) = shell.state.deploy.as_mut() else {
                    return;
                };
                // 同日志浮层：容器 + 应用名两道守卫，否则会把结果写进别人的浮层。
                if deploy.container != container {
                    return;
                }
                let Some(conf) = deploy.site_conf.as_mut() else {
                    return;
                };
                if conf.app != app {
                    return;
                }
                conf.busy = false;
                match result {
                    Ok(text) => conf.text = Some(text),
                    Err(message) => {
                        conf.text = Some(format!("读配置失败：{message}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 关掉「站点配置」浮层。
    pub fn close_deploy_site_conf(&mut self, cx: &mut Context<Self>) {
        if let Some(deploy) = self.state.deploy.as_mut() {
            deploy.site_conf = None;
        }
        cx.notify();
    }

    /// 重写当前应用的站点配置（容器重建后 conf 丢了时用）。
    ///
    /// 只写 conf、**不重新铺文件**；也不重新接网络（容器重建后网络连接也会丢，
    /// 需要重新部署或手动 `wslc network connect` —— 浮层说明里写了）。
    ///
    /// # 为什么不做"自动补写"
    ///
    /// 容器重建后网络连接也丢了；自动重写 conf 会让人以为"已经好了"，其实反代
    /// 还是 502。手动点一次，失败 / 警告能停在浮层里。
    pub fn reapply_deploy_site(&mut self, app: String, cx: &mut Context<Self>) {
        let Some(action) = self.deploy_action(&app) else {
            return;
        };
        let Some(site) = action.site.clone() else {
            self.state
                .notify(Toast::info("这个部署没有站点配置（站点关）"));
            return;
        };
        if self
            .state
            .deploy
            .as_ref()
            .is_some_and(|d| d.progress.is_some())
        {
            self.deploy_busy_notice(cx);
            return;
        }
        if let Some(deploy) = self.state.deploy.as_mut() {
            if let Some(conf) = deploy.site_conf.as_mut() {
                conf.busy = true;
            } else {
                deploy.site_conf = Some(DeploySiteConf {
                    app: app.clone(),
                    text: None,
                    busy: true,
                });
            }
        }
        cx.notify();

        let wslc = self.state.wslc.clone();
        let container = action.container.clone();
        let target = action.target.clone();
        let port = action.port;
        cx.spawn(async move |this, cx| {
            let result = {
                let wslc = wslc.clone();
                let container = container.clone();
                let app = app.clone();
                let site = site.clone();
                let target = target.clone();
                cx.background_executor()
                    .spawn(async move {
                        let resolv =
                            deploy_cmd::read_resolv_conf(&wslc, &container).unwrap_or_default();
                        let resolver = deploy_model::first_nameserver(&resolv)
                            .unwrap_or_else(|| "127.0.0.11".to_owned());
                        let applied = deploy_cmd::apply_site(
                            &wslc,
                            &container,
                            &app,
                            &site,
                            &target,
                            port,
                            Some(&resolver),
                        )
                        .map_err(|e| e.to_string())?;
                        let text = deploy_cmd::read_site_conf(&wslc, &container, &app, Some(&site))
                            .map_err(|e| e.to_string())?;
                        Ok::<(Option<String>, String), String>((applied.warning, text))
                    })
                    .await
            };
            let _ = this.update(cx, |shell, cx| {
                let Some(deploy) = shell.state.deploy.as_mut() else {
                    return;
                };
                if deploy.container != container {
                    return;
                }
                let Some(conf) = deploy.site_conf.as_mut() else {
                    return;
                };
                if conf.app != app {
                    return;
                }
                conf.busy = false;
                match result {
                    Ok((warning, text)) => {
                        conf.text = Some(text);
                        let mut message = "站点配置已重写".to_owned();
                        if let Some(warning) = warning {
                            message.push_str(&format!("；{warning}"));
                        }
                        shell.state.notify(Toast::info(message));
                    }
                    Err(message) => {
                        conf.text = Some(format!("重写失败：{message}"));
                        shell
                            .state
                            .notify(Toast::error(format!("重写站点配置失败：{message}")));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 让容器里的 nginx 重载配置（内容形态的「重载 nginx」）。
    pub fn reload_nginx_in_container(&mut self, cx: &mut Context<Self>) {
        let Some(deploy) = self.state.deploy.as_ref() else {
            return;
        };
        let container = deploy.container.clone();
        let wslc = self.state.wslc.clone();
        self.state.notify(Toast::info("正在让 nginx 重载…"));
        cx.spawn(async move |this, cx| {
            let result = {
                let wslc = wslc.clone();
                let container = container.clone();
                cx.background_executor()
                    .spawn(async move {
                        deploy_cmd::reload_nginx(&wslc, &container).map_err(|e| e.to_string())
                    })
                    .await
            };
            let _ = this.update(cx, |shell, cx| {
                match result {
                    Ok(message) => {
                        let message = message.trim();
                        if message.is_empty() {
                            shell.state.notify(Toast::info("nginx 已重载"));
                        } else {
                            // nginx 把 `signal process started` 写在 stderr，
                            // 那句话对用户是有信息量的（说明信号真的发出去了）
                            shell
                                .state
                                .notify(Toast::info(format!("nginx 已重载：{message}")));
                        }
                    }
                    Err(message) => {
                        if shell.deploy_page_is(&container) {
                            if let Some(deploy) = shell.state.deploy.as_mut() {
                                deploy.fail(message.clone());
                            }
                        }
                        shell
                            .state
                            .notify(Toast::error(format!("重载失败：{message}")));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}
