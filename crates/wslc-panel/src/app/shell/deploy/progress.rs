//! 进度与互斥：阶段、进度写回、清理、别的按钮该怎么提示。

use super::super::super::prelude::*;
use super::super::Shell;
use super::super::state::DeployAction;

impl Shell {
    /// 一次部署动作要用的素材（从当前页面状态 + 存档里取）。
    ///
    /// 类型认不出来 / 找不到记录 → `None`（界面上那些按钮也不会画）。
    pub(crate) fn deploy_action(&self, app: &str) -> Option<DeployAction> {
        let deploy = self.state.deploy.as_ref()?;
        let index = deploy.index_of(app)?;
        let entry = &deploy.entries[index];
        Some(DeployAction {
            container: deploy.container.clone(),
            target: entry.record.target.clone(),
            port: entry.record.port,
            command: entry.record.command.clone(),
            is_process: entry
                .runtime
                .is_some_and(|runtime| runtime.kind() == DeployKind::Process),
            members: entry.record.members.clone(),
            site: entry.record.site.clone(),
        })
    }

    /// 置 / 清"正在跑的那一次部署"（统一的入口 —— 界面靠它把按钮换成"正在…"）。
    pub(crate) fn set_deploy_progress(
        &mut self,
        progress: Option<DeployProgress>,
        cx: &mut Context<Self>,
    ) {
        if let Some(deploy) = self.state.deploy.as_mut() {
            deploy.progress = progress;
        }
        cx.notify();
    }

    /// 清掉"正在跑的那一次部署"——**只有当它还属于这个应用时**。
    ///
    /// # 为什么不能直接 `set_deploy_progress(None)`
    ///
    /// `progress` 是**一个槽**（一次只跑一个动作），而界面上的忙碌标记是
    /// **按应用名**算的（`views::deploy_row_actions` 比的是 `p.app == app`）。
    /// 于是 A 还在跑的时候，B 那一行照样有按钮 —— 用户点了 B（守卫会拦下来，
    /// 见 [`Shell::deploy_busy_notice`]，但万一 B 先跑起来）时，
    /// A 的回调回来一句无条件的 `None` 就会把 **B 的**「正在…」清掉：
    /// B 的按钮提前复活、可以再点一次，而 B 的动作其实还在跑。
    ///
    /// 判应用名之后就不会串台：谁的动作，谁才能清自己那一格。
    pub(crate) fn clear_deploy_progress_for(&mut self, app: &str, cx: &mut Context<Self>) {
        if let Some(deploy) = self.state.deploy.as_mut() {
            if deploy.progress.as_ref().is_some_and(|p| p.app == app) {
                deploy.progress = None;
            }
        }
        cx.notify();
    }

    /// 「已经有一个部署动作在跑」时统一给的那句话。
    ///
    /// 界面上**别的行**这时仍然画着按钮（忙碌是按应用名算的），所以点了必须有
    /// 反馈 —— 静默 `return` 会被当成"按钮坏了"（`AGENTS.md` §7.1）。
    pub(crate) fn deploy_busy_notice(&mut self, cx: &mut Context<Self>) {
        self.state.notify(Toast::info(
            "已经有一个部署动作在跑 —— 等它结束再点（进度显示在那一行上）",
        ));
        cx.notify();
    }

    /// 只换阶段（同一个部署动作的下一步）。
    pub(crate) fn set_deploy_phase(&mut self, phase: DeployPhase, cx: &mut Context<Self>) {
        if let Some(deploy) = self.state.deploy.as_mut() {
            if let Some(progress) = deploy.progress.as_mut() {
                progress.phase = phase;
            }
        }
        cx.notify();
    }
}
