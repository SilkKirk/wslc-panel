//! 部署域：表单、提交、启停、日志、「站点配置」。

use super::super::prelude::*;
use super::Shell;
use super::state::{AfterPick, DeployAction, DeployField, DeployForm, DeployFormValues, SiteStep};

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
            self.deploy_form = None;
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

    /// 打开「添加部署」（`editing = None`）或「重新部署」（`Some(应用名)`）弹窗。
    ///
    /// # 为什么"重新部署"也走这个弹窗，而不是一键重传
    ///
    /// 一键重传只能覆盖"我在同一个路径重新构建了产物"这一种情况。
    /// 用户想改目标目录 / 启动命令时，一键重传帮不上忙 —— 而他能走的另一条路
    /// 是**先删掉再添加**，那个「删掉」会把容器里的目录清空，代价大得多。
    /// 所以"重新部署"= 把这一条**预填**进同一个弹窗：直接点确认就是重传，
    /// 想改哪格就改哪格。动作还是只有**一个入口**。
    ///
    /// 应用名在重新部署时**锁死**（改了就是另一个部署了，那应该走「添加部署」）。
    pub fn open_deploy_dialog(
        &mut self,
        editing: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(deploy) = self.state.deploy.as_ref() else {
            return;
        };
        let runtime = deploy.runtime;
        let web_root = deploy.web_root.clone();
        let container = deploy.container.clone();
        let ports = deploy.ports.clone();
        let record = editing
            .as_ref()
            .and_then(|app| self.state.deploy_store.get(&container, app).cloned());

        // 多应用：新应用的名字 / 端口不能和已有的撞。应用名撞了会**覆盖**那一条
        // （`DeployStore::put` 按 `<容器>/<应用>` 索引），端口撞了两个进程会
        // 在容器里抢同一个 bind —— 两件都是"部署成功但原来那个不见了"的来源。
        let existing_names: Vec<String> = self
            .state
            .deploy_store
            .for_container(&container)
            .into_iter()
            .map(|record| record.app.clone())
            .collect();
        let existing_ports = self.state.deploy_store.binding_ports(&container);
        let published_ports = deploy_model::published_container_ports(&ports);

        // ⚠️ 默认应用名**不是**运行时键（`java` → `/opt/java`，正是 §15 那次
        // 把镜像自带 JDK 删掉的路径）。理由写在 `DEFAULT_APP_NAME` 那儿。
        // 多应用时再点一次「添加部署」，`suggest_app_name` 会给出 `app-2`。
        let app_value = record
            .as_ref()
            .map(|r| r.app.clone())
            .unwrap_or_else(|| deploy_model::suggest_app_name(&existing_names));
        let source_value = record
            .as_ref()
            .map(|r| r.source.clone())
            .unwrap_or_default();
        let target_value = record
            .as_ref()
            .map(|r| r.target.clone())
            .unwrap_or_else(|| {
                // nginx 的目标必须**跟着挂载点走**，而且**每个应用一个子目录**：
                // 所有应用铺进同一个 Web 根时，`removals_script` 的逐名删除会把
                // 另一个应用的同名文件删掉（见 `docs/PLAN-multi-app.md` §1.3）。
                deploy_model::default_app_target(runtime, &app_value, web_root.as_deref())
            });
        // 存档里有就用存档的；没有就从来源路径推（Java / Go 的默认启动命令
        // 要的是**源文件的名字**，不是应用名）
        let artifact = record
            .as_ref()
            .map(|r| r.artifact.clone())
            .filter(|name| !name.is_empty())
            .or_else(|| {
                if source_value.is_empty() {
                    None
                } else {
                    deploy_model::artifact_name(std::path::Path::new(&source_value))
                }
            });
        let command_value = record
            .as_ref()
            .map(|r| r.command.clone())
            .unwrap_or_else(|| {
                runtime
                    .default_start_command(&app_value, artifact.as_deref())
                    .unwrap_or_default()
            });
        let port_value = record
            .as_ref()
            .map(|r| r.port.to_string())
            .unwrap_or_else(|| {
                deploy_model::suggest_app_port(
                    &existing_ports,
                    &published_ports,
                    runtime.default_port(),
                )
                .to_string()
            });
        // 站点开的记录才有这一块；新应用默认**关**（先铺文件，再决定要不要
        // 接管端口）。老记录里 `site` 是 `None`，读出来也就是关。
        let record_site = record.as_ref().and_then(|r| r.site.clone());
        let site_enabled = record_site.is_some();
        let site_spa = record_site.as_ref().map(|s| s.spa).unwrap_or(true);
        let first_proxy = record_site.as_ref().and_then(|site| site.proxies.first());
        let proxy_prefix_value = first_proxy
            .map(|rule| rule.prefix.clone())
            .unwrap_or_default();
        let proxy_upstream_value = first_proxy
            .map(|rule| rule.upstream.clone())
            .unwrap_or_default();
        let proxy_strip = first_proxy.is_some_and(|rule| rule.strip_prefix);
        let server_name_value = record_site
            .as_ref()
            .map(|site| site.server_name.clone())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "localhost".to_owned());
        // 第一条进上面两格；其余塞进「更多反代」一格（预填成它能解析的写法）。
        let proxy_more_value = record_site
            .as_ref()
            .map(|site| {
                site.proxies
                    .iter()
                    .skip(1)
                    .map(deploy_model::format_proxy_rule)
                    .collect::<Vec<_>>()
                    .join("; ")
            })
            .unwrap_or_default();

        self.state
            .set_section_open(UiSection::DeployProxy, first_proxy.is_some());
        self.state
            .set_section_open(UiSection::DeployProxyMore, !proxy_more_value.is_empty());

        // `InputState::new` 要 `&mut Window` → 只能在这里（有 window）建。
        let app = cx.new(|cx| InputState::new(window, cx).placeholder("site"));
        let source = cx.new(|cx| {
            InputState::new(window, cx).placeholder(
                if runtime.source_kind() == deploy_model::SourceKind::Directory {
                    r"E:\build\site"
                } else {
                    r"E:\build\app.jar"
                },
            )
        });
        let target = cx.new(|cx| InputState::new(window, cx).placeholder("/opt/site"));
        let command = cx.new(|cx| InputState::new(window, cx).placeholder("java -jar app.jar"));
        let port = cx.new(|cx| InputState::new(window, cx).placeholder("8080"));
        let server_name = cx.new(|cx| InputState::new(window, cx).placeholder("localhost"));
        let proxy_prefix = cx.new(|cx| InputState::new(window, cx).placeholder("/api/"));
        let proxy_upstream =
            cx.new(|cx| InputState::new(window, cx).placeholder("http://api:8080"));
        let proxy_more = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("/auth/ => http://auth:9000; /ws/ => http://ws:9001 strip")
        });

        app.update(cx, |state, cx| {
            state.set_value(app_value, window, cx);
        });
        source.update(cx, |state, cx| {
            state.set_value(source_value, window, cx);
        });
        target.update(cx, |state, cx| {
            state.set_value(target_value, window, cx);
        });
        command.update(cx, |state, cx| {
            state.set_value(command_value, window, cx);
        });
        port.update(cx, |state, cx| {
            state.set_value(port_value, window, cx);
        });
        server_name.update(cx, |state, cx| {
            state.set_value(server_name_value, window, cx);
        });
        proxy_prefix.update(cx, |state, cx| {
            state.set_value(proxy_prefix_value, window, cx);
        });
        proxy_upstream.update(cx, |state, cx| {
            state.set_value(proxy_upstream_value, window, cx);
        });
        proxy_more.update(cx, |state, cx| {
            state.set_value(proxy_more_value, window, cx);
        });

        // 焦点给**来源**那一格：新增时最可能要做的事就是点「浏览…」，
        // 重新部署时用户多半直接点确认 —— 焦点在哪都不影响。
        let handle = source.read(cx).focus_handle(cx);
        window.focus(&handle, cx);

        let input_subs = [&proxy_prefix, &proxy_upstream, &proxy_more]
            .into_iter()
            .map(|input| {
                cx.subscribe(input, |_, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        cx.notify();
                    }
                })
            })
            .collect();
        self.deploy_form = Some(DeployForm {
            _input_subs: input_subs,
            editing,
            app,
            source,
            target,
            command,
            port,
            site_enabled,
            site_spa,
            server_name,
            proxy_prefix,
            proxy_upstream,
            proxy_strip,
            proxy_more,
            auto_connect: true,
        });
        cx.notify();
    }

    /// 关掉「添加部署」弹窗（取消 / 提交完都走它）。
    pub fn close_deploy_dialog(&mut self, cx: &mut Context<Self>) {
        self.deploy_form = None;
        cx.notify();
    }

    /// 「生成站点配置」勾选变化。
    pub fn set_deploy_site_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if let Some(form) = self.deploy_form.as_mut() {
            form.site_enabled = enabled;
        }
        cx.notify();
    }

    /// 「单页应用回退」勾选变化。
    pub fn set_deploy_site_spa(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if let Some(form) = self.deploy_form.as_mut() {
            form.site_spa = enabled;
        }
        cx.notify();
    }

    /// 「去掉前缀」勾选变化。
    pub fn set_deploy_proxy_strip(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if let Some(form) = self.deploy_form.as_mut() {
            form.proxy_strip = enabled;
        }
        cx.notify();
    }

    /// 「自动接通网络」勾选变化。
    pub fn set_deploy_auto_connect(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if let Some(form) = self.deploy_form.as_mut() {
            form.auto_connect = enabled;
        }
        cx.notify();
    }

    /// 「浏览…」：按运行时的来源类型弹文件 / 目录对话框。
    ///
    /// 复用 [`Shell::spawn_picker`]（它已经带好了 `picking` 守卫 ——
    /// `cmd::picker` 的文档要求"同时只弹一个"）和正确的
    /// `spawn_in` + `background_executor` 形状，不再写一份。
    ///
    /// 选完之后由 `AfterPick::DeploySource` 触发
    /// [`Shell::resync_deploy_command`]（Java / Go 的启动命令里嵌的是**源文件的
    /// 名字**，来源一换那格就不对了）。
    pub fn browse_deploy_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(form), Some(deploy)) = (self.deploy_form.as_ref(), self.state.deploy.as_ref())
        else {
            return;
        };
        let runtime = deploy.runtime;
        let input = form.source.clone();
        let folders = runtime.source_kind() == deploy_model::SourceKind::Directory;
        let label = runtime.source_label();
        let extensions = runtime.source_extensions();

        self.spawn_picker(
            &input,
            folders,
            label,
            extensions,
            "选择要部署的来源",
            AfterPick::DeploySource,
            window,
            cx,
        );
    }

    /// 来源选好之后，把**启动命令**按新来源重算一遍。
    ///
    /// # 为什么可以覆盖用户已经改过的内容
    ///
    /// Java / Go 的默认启动命令里嵌的是**源文件的名字**
    /// （`java -jar myapp-1.2.0.jar`、`./api`）—— 来源一换它就不对了，
    /// 留着只会得到 `Error: Unable to access jarfile`。而这两族的正常操作顺序
    /// 就是"先选来源、再调命令"，所以覆盖是符合预期的。
    ///
    /// nginx / Node 不动这一格：它们的命令（`npm start`）与来源文件名无关。
    pub(crate) fn resync_deploy_command(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(form), Some(deploy)) = (self.deploy_form.as_ref(), self.state.deploy.as_ref())
        else {
            return;
        };
        let runtime = deploy.runtime;
        if !matches!(runtime, AppRuntime::Java | AppRuntime::Go) {
            return;
        }
        let source = form.source.read(cx).value().trim().to_owned();
        if source.is_empty() {
            return;
        }
        let app = form.app.read(cx).value().trim().to_owned();
        let artifact = deploy_model::artifact_name(std::path::Path::new(&source));
        let Some(command) = runtime.default_start_command(&app, artifact.as_deref()) else {
            return;
        };
        let input = form.command.clone();
        input.update(cx, |state, cx| state.set_value(command, window, cx));
        cx.notify();
    }

    /// 把焦点送到「添加部署」弹窗里某一格（校验没过时用）。
    ///
    /// 单独一个方法而不是在 `submit_deploy` 里写个闭包：闭包要同时捕获
    /// `&mut self` 和 `cx`，借用关系绕，而且四个校验分支会各写一遍。
    ///
    /// ⚠️ `cx` 必须是 `&mut Context<Self>`（不能是 `&App`）：
    /// `Window::focus(&self, handle, cx)` 要的是 `&mut App`，而
    /// `InputState::focus_handle(cx)` 要的是 `&App` —— 两者差一层可变借用。
    /// 第一版这里写成了 `&App`，CI 报 `E0308: types differ in mutability`。
    /// 拿 `Context<Self>` 而不是 `&mut App`，是因为调用点本来就握着
    /// `&mut Context<Self>`（省掉一次降低），而它降到 `&App` / `&mut App`
    /// 都是自动的 —— 仓库里 `window.focus(&handle, cx)` 那些地方就是这么写的。
    pub(crate) fn focus_deploy_field(
        &mut self,
        which: DeployField,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 校验报错不能把焦点送到折叠区内部却不展开。
        if matches!(
            &which,
            DeployField::ProxyPrefix | DeployField::ProxyUpstream | DeployField::ProxyMore
        ) {
            self.state.set_section_open(UiSection::DeployProxy, true);
        }
        if matches!(&which, DeployField::ProxyMore) {
            self.state
                .set_section_open(UiSection::DeployProxyMore, true);
        }
        cx.notify();
        let Some(form) = self.deploy_form.as_ref() else {
            return;
        };
        let input = match which {
            DeployField::App => &form.app,
            DeployField::Source => &form.source,
            DeployField::Target => &form.target,
            DeployField::Command => &form.command,
            DeployField::Port => &form.port,
            DeployField::ProxyPrefix => &form.proxy_prefix,
            DeployField::ProxyUpstream => &form.proxy_upstream,
            DeployField::ServerName => &form.server_name,
            DeployField::ProxyMore => &form.proxy_more,
        };
        let handle = input.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
    }

    /// 读「添加部署」弹窗里**此刻**的值。
    ///
    /// 和 `read_db_form` 同一条理由：执行时用的就是用户眼睛看到的那个，
    /// 不存在"界面显示 A、实际用 B"的可能。
    pub(crate) fn read_deploy_form(&self, cx: &App) -> Option<DeployFormValues> {
        let form = self.deploy_form.as_ref()?;
        Some(DeployFormValues {
            app: form.app.read(cx).value().trim().to_owned(),
            source: form.source.read(cx).value().trim().to_owned(),
            target: form.target.read(cx).value().trim().to_owned(),
            command: form.command.read(cx).value().trim().to_owned(),
            port: form.port.read(cx).value().trim().parse::<u16>().ok(),
            site_enabled: form.site_enabled,
            site_spa: form.site_spa,
            server_name: form.server_name.read(cx).value().trim().to_owned(),
            proxy_prefix: form.proxy_prefix.read(cx).value().trim().to_owned(),
            proxy_upstream: form.proxy_upstream.read(cx).value().trim().to_owned(),
            proxy_strip: form.proxy_strip,
            proxy_more: form.proxy_more.read(cx).value().trim().to_owned(),
            auto_connect: form.auto_connect,
        })
    }

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
