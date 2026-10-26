//! 更新检查（只检查、不下载，见 `docs/PLAN-install-update.md`）。

use super::super::prelude::*;
use super::Shell;
use super::core::current_version;

impl Shell {
    /// 启动 8 秒后自动查一次。
    ///
    /// 为什么是 8 秒而不是"立刻"：实测一轮采集要**串行**跑 7 条 `wslc`
    /// 命令再加 `wsl -l -v` 和 `reg query`，启动头几秒本来就很忙；
    /// 这时候再插一个 2 秒的 HTTP 请求，用户看到的第一次卡顿就说不清是谁的锅。
    /// 8 秒之后窗口已经画出来了，多这一件事没人会发现。
    pub(crate) fn start_auto_check(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(8)).await;
            // 实体已销毁（用户 8 秒内就关了窗口）→ `update` 返回 Err，忽略即可。
            let _ = this.update(cx, |shell, cx| shell.maybe_auto_check(cx));
        })
        .detach();
    }

    /// 按偏好与 24 小时节流决定要不要查。
    pub(crate) fn maybe_auto_check(&mut self, cx: &mut Context<Self>) {
        let now = wslc_panel_core::util::now_secs();
        // 本地构建的版本号和发布版一样，提示更新只会干扰开发。
        let dev_build = crate::build_sha() == "dev";
        if !wslc_panel_core::update::should_auto_check(&self.state.prefs, now, dev_build) {
            tracing::debug!("跳过自动检查更新（开关、开发构建或不到 24 小时）");
            return;
        }
        self.check_update(false, cx);
    }

    /// 查一次更新。
    ///
    /// `manual` = 用户点的：「正在检查…」「检查失败」这类话只说给手点的人听。
    /// 自动检查失败**必须静默** —— 否则每天开面板都弹一句"检查更新失败"，
    /// 而这个工具本身根本不依赖网络。
    pub fn check_update(&mut self, manual: bool, cx: &mut Context<Self>) {
        // 正在查就不要再发一条（用户可能连点）。
        if matches!(self.state.update, wslc_panel_core::update::Phase::Checking) {
            // ⚠️ 别静默返回：`AGENTS.md` §7.1 那条"点了没反应会被当成按钮坏了"
            // 对这里同样成立。界面上那颗按钮虽然已经换成「正在检查…」、点不到，
            // 但自动检查（启动 8 秒后）和手动点用的是同一条路，将来多一个入口
            // 就会踩到这条 —— 给一句话的成本是零。
            if manual {
                self.state.notify(Toast::info("正在检查更新，等它结束"));
                cx.notify();
            }
            return;
        }

        self.state.update = wslc_panel_core::update::Phase::Checking;
        if manual {
            self.state.notify(Toast::info("正在检查更新…"));
        }
        cx.notify();

        cx.spawn(async move |this, cx| {
            // `cx.spawn` 的 future 在**界面线程**上被 poll，所以真正的阻塞调用
            // 必须再套一层 `background_executor()`（`AGENTS.md` §7.1）。
            let outcome = cx
                .background_executor()
                .spawn(async { wslc_core::cmd::update::check() })
                .await;
            let now = wslc_panel_core::util::now_secs();
            let _ = this.update(cx, |shell, cx| {
                shell.apply_update_check(outcome, now, manual, cx);
            });
        })
        .detach();
    }

    /// 把一次检查的结果落到状态上。
    pub(crate) fn apply_update_check(
        &mut self,
        outcome: Result<wslc_core::update::Release, String>,
        now: u64,
        manual: bool,
        cx: &mut Context<Self>,
    ) {
        let current = current_version();

        // ⚠️ **这一条必须在 `match` 外面**：不管成功、失败，这一次都算"查过了"。
        //
        // 原先它写在 `Ok(release)` 分支里，而 `Err` 分支一个字段都没动 ——
        // 于是 `should_auto_check` 下一次仍然看到"没查过 / 太久没查"，**每次启动
        // 都再发一次** `curl`（最长 25 秒）。而注释和 README 承诺的恰恰相反：
        // 「查失败不会让下一次启动又立刻再查一次」「24 小时最多一次」。
        //
        // 触发条件不是边角情况：这台机器上 `api.github.com` 时好时坏，
        // 共享出口（公司 NAT / 校园网）还会把 60 次/小时用光 —— 那正是最容易
        // 一直失败、也最容易被这条 bug 反复烧配额的机器。
        self.state.prefs.last_update_check = Some(now);
        if let Err(e) = self.state.prefs.save() {
            tracing::warn!("记录更新检查时间失败：{e}");
        }

        match outcome {
            Ok(release) => {
                let newer = wslc_core::update::is_newer(&release.version, &current);
                let skipped = wslc_panel_core::update::is_skipped(
                    &release,
                    self.state.prefs.skipped_version.as_deref(),
                );

                tracing::info!(
                    "检查更新：当前 {}，远端 {}（{}），{}",
                    current,
                    release.raw_version,
                    release.tag,
                    if newer {
                        "有新版本"
                    } else {
                        "已是最新"
                    }
                );

                if newer {
                    let version = release.raw_version.clone();
                    self.state.update = wslc_panel_core::update::Phase::Available { release, skipped };
                    if !skipped {
                        self.state.notify(Toast::info(format!(
                            "有新版本 {version} —— 在「设置 → 关于」里打开发布页"
                        )));
                    }
                } else {
                    self.state.update = wslc_panel_core::update::Phase::UpToDate { checked_at: now };
                    if manual {
                        self.state
                            .notify(Toast::info(format!("已是最新（{}）", release.raw_version)));
                    }
                }
            }
            Err(message) => {
                tracing::warn!("检查更新失败：{message}");
                if manual {
                    self.state
                        .notify(Toast::error(format!("检查更新失败：{message}")));
                    self.state.update = wslc_panel_core::update::Phase::Failed { message };
                } else {
                    // 自动检查失败：界面**一点变化都不该有**，回到空闲。
                    self.state.update = wslc_panel_core::update::Phase::Idle;
                }
            }
        }

        cx.notify();
    }

    /// 打开发布页（默认浏览器）。
    ///
    /// 没查到具体版本时退回 Releases 列表页 —— 用户点这颗按钮的目的
    /// 永远是"我要去下载"，不该因为还没查过就什么都不做。
    pub fn open_release_page(&mut self, cx: &mut Context<Self>) {
        let url = match &self.state.update {
            wslc_panel_core::update::Phase::Available { release, .. } => release.page_url.clone(),
            _ => format!("https://github.com/{}/releases", wslc_core::update::REPO),
        };
        self.open_url(url, cx);
    }

    /// 「跳过这个版本」：记住它，不再提示。
    pub fn skip_update_version(&mut self, cx: &mut Context<Self>) {
        let Some(version) = (match &self.state.update {
            wslc_panel_core::update::Phase::Available { release, .. } => Some(release.raw_version.clone()),
            _ => None,
        }) else {
            return;
        };

        self.state.prefs.skipped_version = Some(version.clone());
        if let Err(e) = self.state.prefs.save() {
            tracing::warn!("保存「跳过版本」失败：{e}");
        }
        if let wslc_panel_core::update::Phase::Available { skipped, .. } = &mut self.state.update {
            *skipped = true;
        }

        tracing::info!("已跳过版本 {version}");
        self.state
            .notify(Toast::info(format!("{version} 不再提示")));
        cx.notify();
    }

    /// 开关「启动时自动检查更新」。
    pub fn set_auto_check_update(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.state.prefs.auto_check_update == on {
            return;
        }
        self.state.prefs.auto_check_update = on;

        match self.state.prefs.save() {
            Ok(()) => {
                tracing::info!("启动时自动检查更新：{}", if on { "开" } else { "关" });
                self.state.notify(Toast::success(if on {
                    "启动时会自动检查更新"
                } else {
                    "不再自动检查更新（仍可手动检查）"
                }));
            }
            Err(e) => {
                tracing::warn!("保存偏好失败：{e}");
                // 内存里的值仍然生效，只是重启后会丢。
                self.state
                    .notify(Toast::error(format!("已改，但保存失败：{e}")));
            }
        }
        cx.notify();
    }
}
