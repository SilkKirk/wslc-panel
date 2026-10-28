//! 已部署应用起停（`start_deployment` / `stop_deployment`）。
//!
//! # 为什么两者共用一个实现
//!
//! `start` 和 `stop` 原先各写一份 60 行：守卫 → 置进度 → 后台跑一条
//! `deploy_cmd::*` → 回写（清进度 / 写错误横幅 / 发提示条 / 重问现状）。
//! 两份**逐行同形**，只有三处不同：
//!
//! | 差异 | 启动 | 停止 |
//! |---|---|---|
//! | 阶段 | `DeployPhase::Starting` | `DeployPhase::Stopping` |
//! | 命令 | `deploy_cmd::start` | `deploy_cmd::stop` |
//! | 成功文案 | 「已启动 X」 | 「已停止 X」 |
//!
//! 抄两遍的代价不是"多几行"，是**漏改**：v0.9.13 那次 `stop` 就漏了
//! `progress` 守卫（A 跑动作时点 B 的停止会覆盖 A 的进度），而 `Starting` /
//! `Stopping` 也是先抄错、后补的。收成一处之后，这两类差异只剩
//! [`DeployCommand`] 一个入参。

use super::super::super::prelude::*;
use super::super::Shell;
use super::super::state::DeployAction;

/// 起 / 停这一对动作的全部差异（见模块文档那张表）。
///
/// 收成枚举而不是三个参数：`stage` / `verb` / 函数指针三者必须**配套**
/// （写错一个就是"按了停止、界面写正在启动…"），枚举让它们不可能单独出现。
#[derive(Clone, Copy)]
enum DeployCommand {
    /// 跑 `deploy_cmd::start`。
    Start,
    /// 跑 `deploy_cmd::stop`。
    Stop,
}

impl DeployCommand {
    /// 界面上的阶段（那一行的「正在…」文案由它决定）。
    fn phase(self) -> DeployPhase {
        match self {
            Self::Start => DeployPhase::Starting,
            Self::Stop => DeployPhase::Stopping,
        }
    }

    /// 成功时提示条里的动词。
    fn verb(self) -> &'static str {
        match self {
            Self::Start => "启动",
            Self::Stop => "停止",
        }
    }

    /// 真正起进程的那一步。返回 `Result<(), String>` —— 后台闭包不能借 `self`，
    /// 错误要在这儿转成 owned 的文本（和 `deploy_cmd` 的其余调用点一致）。
    fn run(self, wslc: &Wslc, container: &str, action: &DeployAction) -> Result<(), String> {
        // 先把要用的两个字段拆出来：`deploy_cmd::start` 有四个参数，
        // 直接在两个臂里写 `&action.target` 会让第二臂超出行宽。
        let (target, command) = (&action.target, &action.command);
        match self {
            Self::Start => deploy_cmd::start(wslc, container, target, command),
            Self::Stop => deploy_cmd::stop(wslc, container, target),
        }
        .map_err(|e| e.to_string())
    }
}

impl Shell {
    /// 「启动」一条部署。
    pub fn start_deployment(&mut self, app: String, cx: &mut Context<Self>) {
        self.spawn_deploy_lifecycle(app, DeployCommand::Start, cx);
    }

    /// 「停止」一条部署（只发 `TERM`，不做 `kill -9`）。
    pub fn stop_deployment(&mut self, app: String, cx: &mut Context<Self>) {
        self.spawn_deploy_lifecycle(app, DeployCommand::Stop, cx);
    }

    /// 起 / 停的唯一实现。
    ///
    /// # 那两条守卫
    ///
    /// 1. `deploy_action` 认不出来 → 静默 return（界面上那些按钮也不会画）；
    /// 2. **已经有一个部署动作在跑** → `deploy_busy_notice` 说一句再 return。
    ///
    /// 第 2 条**不能**静默：别的行这时照样画着按钮（忙碌是按应用名算的，
    /// 见 `views::deploy_row_actions`），点了必须知道为什么没反应
    /// （`AGENTS.md` §7.1）。
    fn spawn_deploy_lifecycle(
        &mut self,
        app: String,
        command: DeployCommand,
        cx: &mut Context<Self>,
    ) {
        let Some(action) = self.deploy_action(&app) else {
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
        self.set_deploy_progress(
            Some(DeployProgress {
                app: app.clone(),
                phase: command.phase(),
            }),
            cx,
        );

        let wslc = self.state.wslc.clone();
        let container = action.container.clone();
        cx.spawn(async move |this, cx| {
            // ⚠️ `container` 要在外面**留一份**：下面那个 `async move` 只搬一份
            // **克隆**进去。写回状态前必须问一句"这一页还是那个容器吗"
            // —— 见 [`Shell::deploy_page_is`]。
            //
            // `action` 只被后台闭包**搬进去**（不参与回写），所以直接 move ——
            // 这里不需要再 clone 一次。
            let result = {
                let wslc = wslc.clone();
                let container = container.clone();
                cx.background_executor()
                    .spawn(async move { command.run(&wslc, &container, &action) })
                    .await
            };
            let _ = this.update(cx, |shell, cx| {
                if shell.deploy_page_is(&container) {
                    shell.clear_deploy_progress_for(&app, cx);
                }
                match result {
                    Ok(()) => shell
                        .state
                        .notify(Toast::info(format!("已{} {app}", command.verb()))),
                    Err(message) => {
                        if shell.deploy_page_is(&container) {
                            if let Some(deploy) = shell.state.deploy.as_mut() {
                                // `&String` 实现了 `Display`，不必 clone 一份再交出去。
                                deploy.fail(&message);
                            }
                        }
                        let text = format!("{}失败：{message}", command.verb());
                        shell.state.notify(Toast::error(text));
                    }
                }
                // 现状是**问容器**得来的，与"哪一页在显示"无关：
                // 这一句刷的永远是**当前**那一页，所以不用判容器。
                shell.refresh_deploy_status(cx);
            });
        })
        .detach();
    }
}
