//! 「部署管理」页的生命周期：进来时读存档、按需探一次状态、离开时收尾。

use super::super::super::prelude::*;
use super::super::Shell;

impl Shell {
    /// 打开某个容器的部署管理页（从容器列表行的「部署管理」按钮进）。
    ///
    /// `runtime` / `signal` 来自**列表那条启发式**（纯字符串，渲染期就能算），
    /// 先让页面立刻画得出来；权威判定在 [`Shell::load_deploy`] 里用 `inspect` 做。
    ///
    /// 接收 `cx` 而**不要 window**：「添加部署」那五个输入框是**点按钮那一刻**
    /// 才建的（`open_deploy_dialog` 有 window），页面本身一个输入框都没有 ——
    /// 和 v0.6.3 的「拉取镜像」同一个形状（见 `Page::needs_window_to_enter`）。
    pub fn open_deploy_page(
        &mut self,
        name: String,
        image: String,
        runtime: AppRuntime,
        signal: DeploySignal,
        cx: &mut Context<Self>,
    ) {
        // 同一个容器就**不重建状态**：切走再回来时列表和刚读到的现状都还在。
        let same_container = self
            .state
            .deploy
            .as_ref()
            .is_some_and(|deploy| deploy.container == name);
        if !same_container {
            self.state.deploy = Some(DeployState::provisional(name, image, runtime, signal));
            // 弹窗一定关掉：它的预填值（运行时的来源类型、默认目标目录、启动命令）
            // 全是**上一个容器**的，留着会让用户在一张属于 A 的表单上部署到 B。
            self.forms.deploy_form = None;
        }
        self.goto_page(Page::Deploy, cx);
        // 每次进来都重读：容器可能刚被启动、目录可能刚被改过 ——
        // "现在怎么样"是**事实**，不能靠缓存。`load_deploy` 自己会挡重复
        //（第一句就是 busy 守卫）。
        self.load_deploy(cx);
    }

    /// 探测容器（一次 `inspect`），然后把本地存档读进来、逐条问现状。
    ///
    /// 进页面、点「重新读取」、部署完成后都走这里。
    pub fn load_deploy(&mut self, cx: &mut Context<Self>) {
        let Some(deploy) = self.state.deploy.as_ref() else {
            return;
        };
        // 正在跑就不叠第二次（按钮那边也已经换成「读取中…」）。
        if deploy.busy {
            return;
        }
        let container = deploy.container.clone();

        // 本地存档先读进来（纯本地、几毫秒）—— 这样"读取中"的时候列表里
        // **已经有行**了：用户看得到自己部署过什么，只是现状还没回来。
        // 这比先显示一张空表诚实得多（`wslc_panel_core::deploys` 的模块注释讲了为什么
        // 记录放本地）。
        self.state.refresh_deploy_entries(&container);

        if let Some(deploy) = self.state.deploy.as_mut() {
            deploy.busy = true;
            deploy.blocked = None;
            deploy.clear_error();
        }
        cx.notify();

        let wslc = self.state.wslc.clone();
        cx.spawn(async move |this, cx| {
            let target = container.clone();
            let wslc_probe = wslc.clone();
            let probe = cx
                .background_executor()
                .spawn(async move {
                    deploy_cmd::detect(&wslc_probe, &target).map_err(|e| e.to_string())
                })
                .await;

            let probe = match probe {
                Ok(probe) => probe,
                Err(message) => {
                    // 探测就失败（不是这几类容器 / 容器没运行）：
                    // 页面只显示这一条 —— 那些空行没有意义。
                    let _ = this.update(cx, |shell, cx| {
                        let failure = DeployFailure::from_text(message);
                        let summary = failure.summary();
                        if let Some(deploy) = shell.state.deploy.as_mut() {
                            if deploy.container == container {
                                deploy.busy = false;
                                deploy.loaded = false;
                                deploy.blocked = Some(failure);
                            }
                        }
                        // ⚠️ 提示条要 `&mut shell.state`，所以上面那个 `deploy`
                        // 借用必须**先结束** —— 因此 summary 是 owned 的。
                        shell
                            .state
                            .notify(Toast::error(format!("读取失败：{summary}")));
                        cx.notify();
                    });
                    return;
                }
            };

            let _ = this.update(cx, |shell, cx| {
                let Some(deploy) = shell.state.deploy.as_mut() else {
                    return;
                };
                if deploy.container != container {
                    // 用户在探测期间切到了别的容器 —— 这份结果不属于当前页面
                    return;
                }
                deploy.apply_probe(&probe);
                deploy.loaded = true;
                // 探测这一趟到此为止；每一行的现状由下面那个函数去问
                deploy.busy = false;
                cx.notify();
                shell.refresh_deploy_status(cx);
            });
        })
        .detach();
    }

    /// **只**重问每一条的现状（不再 `inspect`）。
    ///
    /// 启动 / 停止 / 删除 / 部署完成之后用它。单独一个函数而不是复用
    /// [`Shell::load_deploy`]：那一个会重新 `inspect`（一次进程 + 一次 JSON
    /// 解析），而"我按了启动"这件事**不会**改变容器的挂载点或运行时 ——
    /// 白问一次还慢一截。
    ///
    /// 一条一次 exec；每条问两件事（跑没跑 + 目录里几个文件），
    /// 合成一条脚本（`model::deploy::probe_script`）。
    ///
    /// 重探期间每一行会先变成「读取中…」（这一条是**动作之后**用的：
    /// 用户刚点过，看到"在重问"是好事）。慢档轮询走
    /// [`Shell::refresh_deploy_status_quiet`] —— 那一条**保留旧值**。
    pub(crate) fn refresh_deploy_status(&mut self, cx: &mut Context<Self>) {
        self.refresh_deploy_status_inner(true, cx);
    }

    /// 慢档轮询用的那一条（[`Shell::tick_deploy_status`]）：**保留旧值**。
    ///
    /// 不清 `probed` / `process` / `files`，所以界面在重探期间继续显示上一次
    /// 问到的结果；结果回来才覆盖。理由：9 秒一轮的轮询如果每轮都把整列刷成
    /// 「读取中…」，用户看到的是**一直在闪** —— 那比"数据晚几秒"难受得多。
    pub(crate) fn refresh_deploy_status_quiet(&mut self, cx: &mut Context<Self>) {
        self.refresh_deploy_status_inner(false, cx);
    }

    /// 公共实现。`clear_first = true` → 先把每一行的现状清掉（画「读取中…」）。
    pub(crate) fn refresh_deploy_status_inner(
        &mut self,
        clear_first: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(deploy) = self.state.deploy.as_ref() else {
            return;
        };
        // ⚠️ 已经在问就不叠第二次：慢档轮询和动作回调完全可能撞上，
        // 而两条 `probe_state` 并发跑同一批目标只会互相覆盖结果。
        // （`load_deploy` 也是同一条守卫，用的是同一个 `busy`。）
        if deploy.busy {
            return;
        }
        let container = deploy.container.clone();
        // 要问哪些行：类型认不出来的**不问**（不知道该用哪种脚本）
        let targets: Vec<(String, DeployKind, String)> = deploy
            .entries
            .iter()
            .filter_map(|entry| {
                entry.runtime.map(|runtime| {
                    (
                        entry.record.app.clone(),
                        runtime.kind(),
                        entry.record.target.clone(),
                    )
                })
            })
            .collect();

        if targets.is_empty() {
            // 没有可问的行：把忙清掉就行（否则按钮永远停在「读取中…」）
            if let Some(deploy) = self.state.deploy.as_mut() {
                deploy.busy = false;
            }
            cx.notify();
            return;
        }

        if let Some(deploy) = self.state.deploy.as_mut() {
            deploy.busy = true;
            if clear_first {
                for entry in deploy.entries.iter_mut() {
                    entry.probed = false;
                    entry.process = None;
                    entry.files = None;
                    entry.probe_error = None;
                }
            }
        }
        cx.notify();

        let wslc = self.state.wslc.clone();
        cx.spawn(async move |this, cx| {
            // ⚠️ `wslc` / `container` 要在外面**留一份**：下面那个 `async move`
            // 只搬一份**克隆**进去。回写时要拿 `container` 判"这一页还是那个
            // 容器吗" —— 直接搬进去就是 `borrow of moved value`（CI 实测报过）。
            let states = {
                let wslc = wslc.clone();
                let container = container.clone();
                cx.background_executor()
                    .spawn(async move {
                        targets
                            .into_iter()
                            .map(|(app, kind, target)| {
                                let result =
                                    deploy_cmd::probe_state(&wslc, &container, kind, &target);
                                (app, result.map_err(|e| e.to_string()))
                            })
                            .collect::<Vec<_>>()
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
                deploy.busy = false;
                for (app, result) in states {
                    let Some(index) = deploy.index_of(&app) else {
                        continue;
                    };
                    let entry = &mut deploy.entries[index];
                    entry.probed = true;
                    match result {
                        Ok(parsed) => {
                            entry.process = parsed.process;
                            entry.files = parsed.files;
                            entry.probe_error = None;
                        }
                        Err(message) => {
                            // ⚠️ 单条读不到**不该**把整页判成失败：别的行还看得见。
                            // 这一行会显示「读不到状态」并把原因放在悬停里 ——
                            // 比"显示成目标不存在"诚实（那会让人以为文件丢了）。
                            entry.probe_error = Some(message);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 从部署管理页回到容器列表。
    ///
    /// **不**清 `state.deploy`：和数据库页同一条理由 —— 下次进来省掉一次探测。
    pub fn close_deploy_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_page(Page::Containers, window, cx);
        cx.notify();
    }

    /// 当前的部署页是不是**还在看这个容器**。
    ///
    /// # 为什么每个异步回写都要先问这一句
    ///
    /// `AppState::deploy` 只有一个槽（换容器即整体替换，和 `AppState::db`
    /// 同一个取舍）。而动作跑在后台的这几秒里，用户完全可能切到另一个容器的
    /// 部署页 —— 那时 `state.deploy` 里装的是**别人**。
    ///
    /// 不判的后果不是崩溃，是**状态串台**：A 的错误横幅挂到 B 的页面上，
    /// 或者 A 的完成回调把 B 的"正在…"清掉（B 的按钮于是提前复活，
    /// 用户能再点一次同一个部署）。
    ///
    /// ⚠️ 注意**哪些**回写要判：只有"页面上的东西"（`progress` / `error` 横幅 /
    /// 日志浮层 / `confirm_remove`）要判；**存档（`deploy_store`）和列表刷新
    /// 不判** —— 它们按容器索引，"用户现在在看哪一页"和"这件事发生过没有"
    /// 是两回事。
    pub(crate) fn deploy_page_is(&self, container: &str) -> bool {
        self.state
            .deploy
            .as_ref()
            .is_some_and(|deploy| deploy.container == container)
    }
}
