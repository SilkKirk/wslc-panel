//! 提交表单：校验 → 打包上传 → 铺文件 → 接网络 → 起进程 → 逐阶段报告。

use super::super::super::prelude::*;
use super::super::Shell;
use super::super::state::{DeployField, SiteStep};

impl Shell {
    /// 提交「添加部署 / 重新部署」。
    ///
    /// 全程在后台执行器上（一个目录里几千个文件要**一个一个** `wslc container cp`
    /// 过去，可能几十秒），每一步都在界面上有痕迹
    ///（按钮换成「正在上传… / 正在启动…」），见 `AGENTS.md` §7.1。
    ///
    /// # 顺序（每一条都有理由）
    ///
    /// 1. **停旧进程**（进程形态、而且是重新部署）—— 必须在删掉那些文件**之前**。
    ///    ⚠️ 少了这一步：pidfile 就在目标目录里，它一被删，**旧进程就再也停不掉**
    ///    （没有 pid 了），而新的启动会再起一个 —— 端口冲突；
    /// 2. **准备目标目录**（`mkdir -p` + 删掉**我们铺过的那些名字**，见
    ///    `model::deploy::removals_script`）—— 两条理由：
    ///    ① `wslc container cp` 要求目标目录**已存在**（实测不存在的路径报
    ///    `ERROR_PATH_NOT_FOUND`）；② 这一步走 `exec`，它会**报出"容器没在运行"**
    ///    —— 而 `cp` 在那种情况下**静默成功**（实测 exit 0、不输出、什么都不做）；
    /// 3. **逐个顶层条目复制**（见 `model::deploy::copy_members`：`cp` 复制目录会
    ///    把目录名一起带过去，目标里会平白多一层）；
    /// 4. **核对**（数目标目录的顶层条目）—— 给第 2 条里那个"静默成功"兜底。
    ///    只信 `cp` 的退出码会得到"成功、容器里空的"；
    /// 5. **补可执行位**（只有 Go 这类要执行的文件需要；`cp` 给的是 `666`）；
    /// 6. **启动**（进程形态）。
    pub fn submit_deploy(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(form), Some(deploy)) = (self.deploy_form.as_ref(), self.state.deploy.as_ref())
        else {
            return;
        };
        // ⚠️ 先把旗标取出来：下面要 `&mut self` 发一条提示，
        // `deploy` 那个不可变借用必须在那之前结束。
        let action_running = deploy.progress.is_some();
        let runtime = deploy.runtime;
        let container = deploy.container.clone();
        let editing = form.editing.clone();
        // 站点目录要和 `container` 一起先拿出来：下面那些 `self.state.notify(…)`
        // 要 `&mut self`，`deploy` 那个借用在最后用到它的地方就得结束。
        let web_root = deploy.web_root.clone();
        let ports = deploy.ports.clone();
        let container_networks = deploy.networks.clone();
        let Some(values) = self.read_deploy_form(cx) else {
            return;
        };
        if action_running {
            // 已经在跑一次部署（页头那颗按钮那时也已经换成「正在…」）——
            // 这一条只是兜底。⚠️ 别静默 return：弹窗还开着、用户点了「部署」
            // 却什么都不变，只会以为按钮坏了（`AGENTS.md` §7.1）。
            self.deploy_busy_notice(cx);
            return;
        }

        // ---- 校验：不合法**不关弹窗**，把焦点送回那一格（和 v0.7 同一条） ----
        if !deploy_model::valid_app_name(&values.app) {
            self.state.notify(Toast::error(
                "应用名只能是字母 / 数字 / 下划线 / 连字符，长度 1~64",
            ));
            self.focus_deploy_field(DeployField::App, window, cx);
            cx.notify();
            return;
        }
        // 多应用：新建时应用名不能和已有的撞 —— 撞了 `DeployStore::put` 会
        // **覆盖**那一条（同一个 `<容器>/<应用>` 键），看起来像"部署成功，
        // 但原来那个不见了"。想覆盖必须走那一行的「重新部署」。
        if editing.is_none()
            && self
                .state
                .deploy_store
                .get(&container, &values.app)
                .is_some()
        {
            self.state.notify(Toast::error(format!(
                "已经有叫「{}」的部署 —— 想覆盖请用那一行的「重新部署」，或者换个名字",
                values.app
            )));
            self.focus_deploy_field(DeployField::App, window, cx);
            cx.notify();
            return;
        }
        if values.source.is_empty() {
            self.state.notify(Toast::error("请先选一个要部署的来源"));
            self.focus_deploy_field(DeployField::Source, window, cx);
            cx.notify();
            return;
        }
        if values.target.is_empty() {
            self.state.notify(Toast::error("请填容器里的目标目录"));
            self.focus_deploy_field(DeployField::Target, window, cx);
            cx.notify();
            return;
        }
        // ⚠️ 安全闸：部署和删除都会在目标目录里**删东西**（只删我们自己铺的，
        // 见 `model::deploy::removals_script`），所以路径要先过关 ——
        // 目标是用户填的，填成 `/usr` 就等于往系统目录里塞文件、删的时候
        // 还可能撞上系统文件（白名单的理由写在那条函数上）。
        //
        // 用 `safe_target_with_site` 而不是 `safe_target`：nginx 形态的默认值
        // 是**从 `inspect` 的挂载点推出来的**，用户把站点挂在白名单之外时
        // （`-v webdata:/site`），只用白名单会**拒掉面板自己预填的那一格** ——
        // 用户看着"只允许填在 … 底下"，而正确的路径就在他眼前的输入框里。
        if !deploy_model::safe_target_with_site(&values.target, web_root.as_deref()) {
            self.state
                .notify(Toast::error(deploy_model::safe_target_hint()));
            self.focus_deploy_field(DeployField::Target, window, cx);
            cx.notify();
            return;
        }
        // 多应用：目标目录不能和别的应用重叠到会互删的程度。
        //
        // 方向很重要：**老记录铺在 Web 根、新应用铺到根下的子目录**是迁移路径，
        // 要放行；只有"新目标是旧目标的父目录 / 同一个目录"，或者子目录名正好
        // 是旧应用铺过的名字（会被它的逐名删除带走）时才拦。
        // 见 `docs/PLAN-multi-app.md` §4.3。
        let previous_target = editing
            .as_ref()
            .and_then(|app| self.state.deploy_store.get(&container, app.as_str()))
            .map(|record| record.target.clone());
        let target_changed = previous_target
            .as_deref()
            .map(|old| old.trim_end_matches('/') != values.target.trim_end_matches('/'))
            .unwrap_or(true);
        let mut target_warning: Option<String> = None;
        if let Some(block) =
            self.state
                .deploy_store
                .target_blocker(&container, &values.app, &values.target)
        {
            if target_changed {
                self.state
                    .notify(Toast::error(block.message(&values.target)));
                self.focus_deploy_field(DeployField::Target, window, cx);
                cx.notify();
                return;
            }
            // 目标没变、但和别的应用共用一个目录（老记录）：只警告不拦 ——
            // 拦住的话老记录就永远改不了了。**先记下来**，等端口那条警告说完
            // 再弹（`Toast` 只有一个槽，后弹的会盖掉先弹的；这条更重要）。
            target_warning = Some(block.message(&values.target));
        }
        let process_shape = runtime.kind() == DeployKind::Process;
        if process_shape && values.command.is_empty() {
            self.state.notify(Toast::error(
                "进程形态要有一条启动命令 —— 面板靠它把应用跑起来",
            ));
            self.focus_deploy_field(DeployField::Command, window, cx);
            cx.notify();
            return;
        }

        // 来源必须真的存在，而且**类型要对**（目录 / 文件）—— 用一个同步的
        // 元数据查询判：它不进文件内容，微秒级，不需要丢到后台。
        let source_path = std::path::PathBuf::from(&values.source);
        let want_directory = runtime.source_kind() == deploy_model::SourceKind::Directory;
        let is_dir = source_path.is_dir();
        let is_file = source_path.is_file();
        if !is_dir && !is_file {
            self.state
                .notify(Toast::error(format!("本地路径不存在：{}", values.source)));
            self.focus_deploy_field(DeployField::Source, window, cx);
            cx.notify();
            return;
        }
        if want_directory && !is_dir {
            self.state.notify(Toast::error(format!(
                "{} 部署的是一个**目录**，请选目录",
                runtime.label()
            )));
            self.focus_deploy_field(DeployField::Source, window, cx);
            cx.notify();
            return;
        }
        if !want_directory && !is_file {
            self.state.notify(Toast::error(format!(
                "{} 部署的是一个**文件**，请选文件",
                runtime.label()
            )));
            self.focus_deploy_field(DeployField::Source, window, cx);
            cx.notify();
            return;
        }

        // 多应用：端口必须是用户明确填的。以前空着会静默回落到运行时默认，
        // 多应用下那必然和第一条撞；进程型 / 站点开的 nginx 都会真的 bind。
        let Some(port) = values.port else {
            self.state.notify(Toast::error("请填一个端口（1~65535）"));
            self.focus_deploy_field(DeployField::Port, window, cx);
            cx.notify();
            return;
        };
        if port == 0 {
            self.state.notify(Toast::error("端口不能是 0"));
            self.focus_deploy_field(DeployField::Port, window, cx);
            cx.notify();
            return;
        }

        // ---- nginx 站点配置（只有勾了「生成站点配置」才会有） ----
        //
        // 这里只**校验并组装**；真正写文件 / `nginx -t` / 回滚在后台的
        // `apply_site`（见 `cmd::deploy`）。上游规范化在提交前做，免得文件都
        // 铺完了才告诉用户"上游写错了"。
        let site: Option<DeploySite> = if runtime == AppRuntime::Nginx && values.site_enabled {
            let server_name = match deploy_model::valid_server_name(&values.server_name) {
                Ok(name) => name,
                Err(message) => {
                    self.state.notify(Toast::error(message));
                    self.focus_deploy_field(DeployField::ServerName, window, cx);
                    cx.notify();
                    return;
                }
            };
            let prefix = values.proxy_prefix.trim();
            let upstream = values.proxy_upstream.trim();
            let mut proxies = Vec::new();
            if !prefix.is_empty() || !upstream.is_empty() {
                let prefix = match deploy_model::valid_proxy_prefix(prefix) {
                    Ok(prefix) => prefix,
                    Err(message) => {
                        self.state.notify(Toast::error(message));
                        self.focus_deploy_field(DeployField::ProxyPrefix, window, cx);
                        cx.notify();
                        return;
                    }
                };
                let upstream = match deploy_model::normalize_upstream(upstream) {
                    Ok(upstream) => upstream,
                    Err(message) => {
                        self.state.notify(Toast::error(message));
                        self.focus_deploy_field(DeployField::ProxyUpstream, window, cx);
                        cx.notify();
                        return;
                    }
                };
                proxies.push(deploy_model::ProxyRule {
                    prefix,
                    upstream,
                    strip_prefix: values.proxy_strip,
                });
            }
            let extra = match deploy_model::parse_proxy_rules(&values.proxy_more) {
                Ok(rules) => rules,
                Err(message) => {
                    self.state.notify(Toast::error(message));
                    self.focus_deploy_field(DeployField::ProxyMore, window, cx);
                    cx.notify();
                    return;
                }
            };
            proxies.extend(extra);
            // nginx 的 `duplicate location` 是**配置校验**错误，不是运行时错误；
            // 在这里拦比等文件都铺完、`nginx -t` 失败再回滚更好（还能把焦点送回
            // 「更多反代」那一格）。
            let mut seen = BTreeSet::new();
            for rule in &proxies {
                if !seen.insert(rule.prefix.clone()) {
                    self.state.notify(Toast::error(format!(
                        "反代前缀 {} 重复了 —— 同一个前缀只能有一条规则",
                        rule.prefix
                    )));
                    self.focus_deploy_field(DeployField::ProxyMore, window, cx);
                    cx.notify();
                    return;
                }
            }
            Some(DeploySite {
                server_name,
                spa: values.site_spa,
                proxies,
                conf: deploy_model::conf_path(&values.app),
            })
        } else {
            None
        };
        // 上一次真的写进容器的那份（站点开→关时要删、失败了要保留它）。
        let previous_site = editing
            .as_ref()
            .and_then(|app| self.state.deploy_store.get(&container, app.as_str()))
            .and_then(|record| record.site.clone());

        if process_shape || site.is_some() {
            if let Some(owner) =
                self.state
                    .deploy_store
                    .port_conflict(&container, &values.app, port)
            {
                self.state.notify(Toast::error(format!(
                    "端口 {port} 已经被「{owner}」用了 —— 同一个容器里的应用不能共用一个端口"
                )));
                self.focus_deploy_field(DeployField::Port, window, cx);
                cx.notify();
                return;
            }
            if !deploy_model::port_is_published(&ports, port) {
                self.state.notify(Toast::info(format!(
                    "{port} 没发布到主机 —— 应用只能在容器/其它容器里访问；\
                     要外部访问得重建容器并加 -p {port}:{port}"
                )));
            }
        }
        // 共用目录的警告放在最后弹：它比"端口没发布"更该被看见。
        if let Some(message) = target_warning {
            self.state.notify(Toast::info(format!("注意：{message}")));
        }

        // 要 `chmod +x` 的文件名：只有"要被执行的文件"才需要（见
        // `AppRuntime::needs_exec_bit`）—— 实测 `wslc container cp` 过去的文件
        // 权限是 **666**（目录倒是 777），没有可执行位。
        let artifact = if runtime.needs_exec_bit() {
            match deploy_model::artifact_name(&source_path) {
                Some(name) => name,
                None => {
                    self.state
                        .notify(Toast::error("这个路径取不出文件名，没法部署"));
                    cx.notify();
                    return;
                }
            }
        } else {
            String::new()
        };
        // 要补 +x 的**容器内绝对路径**（`None` = 这一族不需要）
        let chmod_path = if artifact.is_empty() {
            None
        } else {
            Some(deploy_model::container_join(&values.target, &artifact))
        };

        // 重新部署时先把旧进程停掉。
        //
        // ⚠️ 必须在**清空目标目录之前**：pidfile 就在那个目录里，
        // 目录一清，旧进程就再也停不掉了（没有 pid 了），而新的启动会再起一个
        // —— 端口冲突。实测那套脚本跑得通，但顺序错了就没救。
        let stop_first = editing.is_some() && process_shape;
        let source_kind = runtime.source_kind();
        let wslc = self.state.wslc.clone();

        // 这次要铺进去的**名字**（文件来源是文件名，目录来源是顶层条目名）。
        //
        // `prepare_target` 靠它决定删哪些 —— 只删**我们自己铺的**那些名字，
        // 不再整目录清空（见 `model::deploy::removals_script`：v0.9.8 那个
        // `find -delete` 把 `eclipse-temurin` 的 JDK 删掉过一次）。
        // 读一次本地目录名是微秒级，不用丢后台。
        let member_names = match deploy_cmd::member_names(&source_path, source_kind) {
            Ok(names) => names,
            Err(e) => {
                self.state
                    .notify(Toast::error(format!("读不出要部署的文件名：{e}")));
                cx.notify();
                return;
            }
        };

        // **删除列表** = 这一次要铺的 ∪ 存档里**上一次铺过的**
        //（`wslc_panel_core::deploys::merge_members`，纯函数、有单测）。
        //
        // ⚠️ "上一次"这一半是 v0.9.13 补上的，它只能来自**存档里那条记录**
        //（`DeployRecord::members`）—— `model::deploy::removals_script` 的文档
        // 写着删除列表有三个来源，而早先这里只给了"这一次"的：源目录里
        // **改名 / 删掉**的文件于是永远留在目标目录里（"我明明改了，页面上还是
        // 旧的"正是 PLAN-v0.9 §2.3 当初要避免的那一类），记录随后又被覆盖，
        // 连「删除」也再收拾不到它。
        let previous_members = editing
            .as_ref()
            .and_then(|app| self.state.deploy_store.get(&container, app.as_str()))
            .map(|record| record.members.clone())
            .unwrap_or_default();
        let removals = wslc_panel_core::deploys::merge_members(&previous_members, &member_names);

        // 记下要写进存档的东西（弹窗马上要关，那之后就读不到了）。
        //
        // ⚠️ 存档里的 `members` 记的是**这一次真的铺了什么**，不是删除列表 ——
        // 下次重新部署时"上一次"取的就是它。存删除列表的话，那些"上一次有、
        // 这一次没有"的名字会一代代累积下去，记录越读越假。
        let record = wslc_panel_core::deploys::DeployRecord {
            app: values.app.clone(),
            runtime: runtime.key().to_owned(),
            source: values.source.clone(),
            target: values.target.clone(),
            command: values.command.clone(),
            artifact: artifact.clone(),
            port,
            members: member_names,
            site: previous_site.clone(),
        };
        let app = values.app.clone();
        let target = values.target.clone();
        let command = values.command.clone();
        let auto_connect = values.auto_connect;
        let port = record.port;
        // 改目标目录时，旧进程的 pidfile 在**旧**目录里 —— 必须用旧目标停，
        // 否则新目录里找不到 pid，旧进程会一直活着（多应用/迁移时很容易踩）。
        let stop_target = previous_target.clone().unwrap_or_else(|| target.clone());

        // 目标目录是**我们自己铺的**吗？
        //
        // 「重新部署」显然算（`editing` 就是那条记录）；另外同一个目录可能被
        // **另一个应用名**用过（面板存档里有记录）—— 那也是我们铺的。
        //
        // 它**只决定"要不要提醒一句"**，不决定拦不拦（v0.9.11 起）：不是我们铺的
        // 目录如果非空，`prepare_target` 返回 `true`，部署照常进行，最后给用户
        // 说一句"那里面本来有别的东西，面板没动它们"。
        //
        // ⚠️ 别再把它当成安全闸 —— 安全由 `model::deploy::removals_script` 的
        // **逐名删除**保证（v0.9.10 之前是整目录 `find -delete`，那才需要拦）。
        // 判断失手（比如记录被删掉）只是少说一句话，不会删错东西。
        let ours_target = editing.is_some()
            || self
                .state
                .deploy
                .as_ref()
                .is_some_and(|d| d.entries.iter().any(|e| e.record.target == target));

        self.deploy_form = None;
        self.set_deploy_progress(
            Some(DeployProgress {
                app: app.clone(),
                // 没有"打包"这一步了：`wslc container cp` 直接搬文件。
                // 「上传」这一档同时盖住"准备目录 + 复制 + 核对 + chmod"。
                phase: DeployPhase::Uploading,
            }),
            cx,
        );

        cx.spawn_in(window, async move |this, cx| {
            // ---- ① 停旧进程（重新部署）+ ② 准备目录 + ③ 复制 + ④ 核对 + ⑤ chmod ----
            //
            // 这一整段是**串行**的，而且每一步都有独立的失败原因，所以放同一个
            // 后台任务里（每换一次执行器就多一次跨线程往返，没有收益）。
            let uploaded = {
                let wslc = wslc.clone();
                let container = container.clone();
                let target = target.clone();
                let stop_target = stop_target.clone();
                let source = source_path.clone();
                let chmod_path = chmod_path.clone();
                // 删除列表要借给后台闭包用，而后台闭包是 `async move` ——
                // 直接借 `removals` 会把它整个移进去，后面（失败分支写存档）
                // 就用不了了。先克隆一份。
                let removals = removals.clone();
                cx.background_executor()
                    .spawn(async move {
                        if stop_first {
                            // 失败**不算数**：可能本来就没在跑（pidfile 不在）
                            let _ = deploy_cmd::stop(&wslc, &container, &stop_target);
                        }
                        // 准备目录 —— 它顺带把"容器没在运行"报出来
                        //（`cp` 在那种情况下**静默成功**，见 `cmd::deploy`）。
                        //
                        // 返回的 `foreign` = 这个目录里本来就有**不是我们铺的**东西
                        //（比如镜像自带的 JDK）—— ⚠️ **不阻断部署**，只在下面对着
                        // 用户说一句（v0.9.11 起；早先是直接拒绝，实测那次拒绝挡住的是
                        // 用户自己的正常部署，见 `cmd::deploy::prepare_target`）。
                        //
                        // ⚠️ 删的**只有 `removals` 里那些名字**（这次要铺的 ∪
                        // 上一次铺过的，外加 app.log / app.pid）——
                        // 目录里别人的东西不会被删。
                        let foreign = deploy_cmd::prepare_target(
                            &wslc,
                            &container,
                            &target,
                            ours_target,
                            &removals,
                        )
                        .map_err(|e| e.to_string())?;
                        let members = deploy_cmd::copy_sources(
                            &wslc,
                            &container,
                            &source,
                            source_kind,
                            &target,
                        )
                        .map_err(|e| e.to_string())?;
                        // 核对：光信 `cp` 的退出码会得到"成功但容器里空的"
                        deploy_cmd::verify_copy(&wslc, &container, &target, members.len())
                            .map_err(|e| e.to_string())?;
                        if let Some(path) = chmod_path.as_deref() {
                            deploy_cmd::chmod_exec(&wslc, &container, path)
                                .map_err(|e| e.to_string())?;
                        }
                        Ok::<bool, String>(foreign)
                    })
                    .await
            };
            // 目录里本来有别的东西（不是我们铺的）—— 部署照常继续，最后说一句
            let foreign_target = match uploaded {
                Ok(foreign) => foreign,
                Err(message) => {
                    let _ = this.update_in(cx, |shell, _window, cx| {
                        // ⚠️ 后台这几秒里用户完全可能切到别的容器的部署页，
                        // 而 `AppState::deploy` **只有一个槽**（换容器即整体替换）——
                        // 不判的话会把 A 的错误横幅挂到 B 的页面上、还会把 B 的
                        // "正在…"清掉（那会让 B 的按钮提前复活）。
                        // 存档与列表那些调用是**按容器**的，不受影响。
                        if shell.deploy_page_is(&container) {
                            shell.clear_deploy_progress_for(&app, cx);
                            if let Some(deploy) = shell.state.deploy.as_mut() {
                                deploy.fail(message.clone());
                            }
                        }
                        shell
                            .state
                            .notify(Toast::error(format!("部署失败：{message}")));
                        // 铺到一半失败也要重问现状 —— 界面上不能留着一个假的"运行中"
                        shell.refresh_deploy_status(cx);
                    });
                    return;
                }
            };

            // ---- ⑥ 站点配置（nginx，站点开 / 关）----
            //
            // 放在复制**之后**：conf 的 `root` 指向的就是刚铺好的目录；`-t`
            // 不过时文件还在，回滚的只是 conf，不会把站点文件删掉。
            let site_step = {
                let wslc = wslc.clone();
                let container = container.clone();
                let app = app.clone();
                let target = target.clone();
                let desired = site.clone();
                let previous = previous_site.clone();
                let container_networks = container_networks.clone();
                if runtime != AppRuntime::Nginx || (desired.is_none() && previous.is_none()) {
                    SiteStep {
                        site: None,
                        warning: None,
                        note: None,
                        error: None,
                    }
                } else {
                    let _ = this.update_in(cx, |shell, _window, cx| {
                        if shell.deploy_page_is(&container) {
                            shell.set_deploy_phase(DeployPhase::Configuring, cx);
                        }
                    });
                    cx.background_executor()
                        .spawn(async move {
                            let mut step = SiteStep {
                                site: previous.clone(),
                                warning: None,
                                note: None,
                                error: None,
                            };
                            if let Some(desired) = desired.as_ref() {
                                // 反代预检：上游是**容器名**时，先把两个容器接到
                                // 同一个用户网络 —— 默认 bridge 解析不了名字
                                // （实测 `getent` rc=2），否则 conf 写得再对也是 502。
                                if auto_connect {
                                    let mut left = if container_networks.is_empty() {
                                        inspect_container(&wslc, &container, false)
                                            .ok()
                                            .map(|inspect| {
                                                inspect
                                                    .networks()
                                                    .into_iter()
                                                    .map(|(name, _, _, _)| name)
                                                    .collect::<Vec<_>>()
                                            })
                                            .unwrap_or_default()
                                    } else {
                                        container_networks.clone()
                                    };
                                    let mut seen_hosts: Vec<String> = Vec::new();
                                    let mut notes: Vec<String> = Vec::new();
                                    let mut warnings: Vec<String> = Vec::new();
                                    for rule in &desired.proxies {
                                        let host =
                                            deploy_model::upstream_host(&rule.upstream);
                                        let is_ip = host.parse::<std::net::IpAddr>().is_ok();
                                        if host == container.as_str()
                                            || host == "localhost"
                                            || is_ip
                                            || seen_hosts.iter().any(|item| item.as_str() == host)
                                        {
                                            continue;
                                        }
                                        seen_hosts.push(host.to_owned());
                                        // 找不到同名容器 → 按"外部主机"处理，
                                        // 不吓用户（上游可能是外部域名）。
                                        let Ok(peer) = inspect_container(&wslc, host, false) else {
                                            continue;
                                        };
                                        let peer_networks: Vec<String> = peer
                                            .networks()
                                            .into_iter()
                                            .map(|(name, _, _, _)| name)
                                            .collect();
                                        match network_cmd::ensure_shared_network(
                                            &wslc,
                                            &container,
                                            &left,
                                            host,
                                            &peer_networks,
                                        ) {
                                            Ok(network) => {
                                                // ⚠️ 接上之后要更新 `left`：下一条规则
                                                // 再 ensure 时如果 `left` 还是旧的，会
                                                // 对同一个网络重复 `connect`，第二个
                                                // peer 就会报"already connected"。
                                                if !left.iter().any(|item| item == &network) {
                                                    left.push(network.clone());
                                                }
                                                notes.push(format!(
                                                    "已确认 {container} 和 {host} 在同一个网络 {network}"
                                                ));
                                            }
                                            Err(error) => {
                                                // 只警告、不阻断：conf 本身有效，网络没接上
                                                // 只是 502；阻断的话用户连"先部署、后接网络"
                                                // 都做不到。
                                                warnings.push(format!(
                                                    "自动接通 {host} 的网络失败：{error} — 反代可能 502；\
                                                     手动 `wslc network connect {} {host}`",
                                                    deploy_model::PANEL_NETWORK
                                                ));
                                            }
                                        }
                                    }
                                    if !notes.is_empty() {
                                        step.note = Some(notes.join("；"));
                                    }
                                    if !warnings.is_empty() {
                                        step.warning = Some(warnings.join("；"));
                                    }
                                }
                                let resolv = deploy_cmd::read_resolv_conf(&wslc, &container)
                                    .unwrap_or_default();
                                let resolver = deploy_model::first_nameserver(&resolv)
                                    .unwrap_or_else(|| "127.0.0.11".to_owned());
                                match deploy_cmd::apply_site(
                                    &wslc,
                                    &container,
                                    &app,
                                    desired,
                                    &target,
                                    port,
                                    Some(&resolver),
                                ) {
                                    Ok(result) => {
                                        step.site = Some(desired.clone());
                                        if result.warning.is_some() {
                                            step.warning = result.warning;
                                        }
                                    }
                                    Err(error) => {
                                        // `-t` 没过：apply_site 已经把文件回滚了，
                                        // 记录必须保留**上一次**的 site。
                                        step.site = previous.clone();
                                        step.error = Some(error.to_string());
                                    }
                                }
                            } else if let Some(previous) = previous.as_ref() {
                                match deploy_cmd::remove_site(&wslc, &container, &app, previous) {
                                    Ok(warning) => {
                                        step.site = None;
                                        step.warning = warning;
                                    }
                                    Err(error) => {
                                        step.site = Some(previous.clone());
                                        step.error = Some(error.to_string());
                                    }
                                }
                            }
                            step
                        })
                        .await
                }
            };

            // ---- ⑦ 启动（只有进程形态要）----
            if process_shape {
                let _ = this.update_in(cx, |shell, _window, cx| {
                    if shell.deploy_page_is(&container) {
                        shell.set_deploy_phase(DeployPhase::Starting, cx);
                    }
                });
            }
            let started = if process_shape {
                let wslc = wslc.clone();
                let container = container.clone();
                let target = target.clone();
                let command = command.clone();
                cx.background_executor()
                    .spawn(async move {
                        deploy_cmd::start(&wslc, &container, &target, &command)
                            .map_err(|e| e.to_string())
                    })
                    .await
            } else {
                Ok(())
            };

            let _ = this.update_in(cx, |shell, _window, cx| {
                if shell.deploy_page_is(&container) {
                    shell.clear_deploy_progress_for(&app, cx);
                }

                // 站点配置失败：文件铺好了，但 conf 没生效（`apply_site` 已经
                // 把文件回滚成写之前的样子）。记录写**上一次**的 site ——
                // `site_step.site` 此时就是旧值，不能把没生效的写进去
                // （否则下次删除会去删一个不存在的 conf、站点开也会骗人）。
                if let Some(message) = site_step.error.clone() {
                    shell.state.deploy_store.put(
                        &container,
                        wslc_panel_core::deploys::DeployRecord {
                            app: app.clone(),
                            runtime: runtime.key().to_owned(),
                            source: record.source.clone(),
                            target: record.target.clone(),
                            command: record.command.clone(),
                            artifact: record.artifact.clone(),
                            port,
                            members: record.members.clone(),
                            site: site_step.site.clone(),
                        },
                    );
                    let _ = shell.state.save_deploy_store();
                    shell.state.refresh_deploy_entries(&container);
                    if shell.deploy_page_is(&container) {
                        if let Some(deploy) = shell.state.deploy.as_mut() {
                            deploy.fail(message.clone());
                        }
                    }
                    shell.state.notify(Toast::error(format!(
                        "文件铺好了，但站点配置没生效：{message}"
                    )));
                    shell.refresh_deploy_status(cx);
                    return;
                }

                match started {
                    Ok(()) => {
                        // 写本地存档（"我们部署过什么"由面板记）。
                        // ⚠️ 这一段**不判容器**：它写的是**按容器索引**的存档，
                        // 部署确实发生了，跟"用户现在在看哪一页"无关。
                        shell.state.deploy_store.put(
                            &container,
                            wslc_panel_core::deploys::DeployRecord {
                                app: app.clone(),
                                runtime: runtime.key().to_owned(),
                                source: record.source.clone(),
                                target: record.target.clone(),
                                command: record.command.clone(),
                                artifact: record.artifact.clone(),
                                port,
                                members: record.members.clone(),
                                site: site_step.site.clone(),
                            },
                        );
                        if let Some(error) = shell.state.save_deploy_store() {
                            // 写不进去**不算部署失败**（文件已经在容器里了），
                            // 但要说出来 —— 否则重启后这条记录凭空消失
                            shell.state.notify(Toast::error(error));
                        }
                        shell.state.refresh_deploy_entries(&container);
                        // 目录里本来有别的东西时说一句（**不是**失败 —— 部署是成功的）。
                        // 说清"面板没动它们"，否则用户看到目录里多出来的 `openjdk/`
                        // 会以为是我们塞的、或者以为我们删过什么。
                        if foreign_target {
                            shell
                                .state
                                .notify(Toast::info(deploy_model::target_not_empty_hint(
                                    &target,
                                )));
                        }
                        let mut message = format!("已部署 {app} → {target}");
                        if let Some(note) = site_step.note.clone() {
                            message.push_str(&format!("；{note}"));
                        }
                        if let Some(warning) = site_step.warning.clone() {
                            message.push_str(&format!("；站点配置：{warning}"));
                        }
                        shell.state.notify(Toast::info(message));
                    }
                    Err(message) => {
                        // 文件铺好了、只是没起来 —— 记录照样写，
                        // 否则用户连"停止 / 看日志"都点不到，而那正是排障要的
                        //
                        // ⚠️ 失败时存的是**删除列表**（这次 ∪ 上一次），不是"这一次的"：
                        // 复制可能只做了一半，而我们已经把上一次的名字删掉了 ——
                        // 存并集才能让之后的「删除」把这些名字都收拾干净。
                        shell.state.deploy_store.put(
                            &container,
                            wslc_panel_core::deploys::DeployRecord {
                                app: app.clone(),
                                runtime: runtime.key().to_owned(),
                                source: record.source.clone(),
                                target: record.target.clone(),
                                command: record.command.clone(),
                                artifact: record.artifact.clone(),
                                port,
                                members: removals.clone(),
                                site: site_step.site.clone(),
                            },
                        );
                        let _ = shell.state.save_deploy_store();
                        shell.state.refresh_deploy_entries(&container);
                        if shell.deploy_page_is(&container) {
                            if let Some(deploy) = shell.state.deploy.as_mut() {
                                deploy.fail(message.clone());
                            }
                        }
                        shell.state.notify(Toast::error(format!(
                            "文件铺好了，但启动失败：{message}"
                        )));
                    }
                }
                shell.refresh_deploy_status(cx);
            });
        })
        .detach();
    }
}
