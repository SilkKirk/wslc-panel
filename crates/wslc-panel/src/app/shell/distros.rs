//! 发行版域：启动、默认发行版、终端、`/etc/wsl.conf`、导出。

use super::super::prelude::*;
use super::Shell;
use super::state::{AfterPick, KeepAlive, WslConfDialog};

impl Shell {
    /// 取消正在进行的导出（kill 子进程）。
    ///
    /// ⚠️ 取消会留下一个**不完整**的 tar —— 提示里要说明，
    /// 免得用户把它当成一个能用的备份。
    pub fn cancel_export(&mut self, cx: &mut Context<Self>) {
        match &self.tasks.export_cancel {
            Some(token) => {
                token.cancel();
                tracing::info!("已请求取消导出");
                self.state.notify(Toast::info("正在取消导出…"));
            }
            None => self.state.notify(Toast::error("当前没有正在进行的导出")),
        }
        cx.notify();
    }

    /// 开始导出发行版（长任务：流式 + 轮询目标文件大小 + 可取消）。
    ///
    /// # 为什么进度不用 `wsl --export` 的输出
    ///
    /// 它**不打百分比**，只偶尔打几行状态。但导出的产物是一个文件，
    /// 而**文件大小是真实且连续增长的** —— 直接量它比解析输出靠谱得多，
    /// 用户看到的也是"还要写多少"这种能估算的信息。
    ///
    /// 输出回调只用来在失败时留一句原因。
    pub fn start_export(&mut self, name: String, path: String, cx: &mut Context<Self>) {
        if self.state.exporting.is_some() {
            self.state.notify(Toast::error("已经有一个导出在跑了"));
            cx.notify();
            return;
        }
        // 拉取和导出在界面上占的是同一个位置（都是居中的长任务浮层），
        // 不让它们同时跑，否则浮层会打架。
        if self.state.pulling.is_some() {
            self.state
                .notify(Toast::error("正在拉取镜像，等它结束再导出"));
            cx.notify();
            return;
        }
        // 别导出正在被安装的那个发行版：安装的重定位阶段（导出 → 注销 → 导入）
        // 本来就在导出它，两条流式任务会抢同一个 VHD，而且它随时可能被
        // `--unregister` 掉 —— 用户拿到的会是一个半截的 tar。
        if let Some(what) = self.state.busy_with(&name) {
            self.state
                .notify(Toast::error(format!("{name} 正在{what}，等它结束再导出")));
            cx.notify();
            return;
        }

        // 输出只保留最后一行：失败时它就是原因。
        let last: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let sink = last.clone();

        let handle = match wslc_core::cmd::distro::export_streaming(
            &self.state.wsl,
            &name,
            &path,
            move |line| {
                if let Ok(mut slot) = sink.lock() {
                    *slot = line.to_owned();
                }
            },
        ) {
            Ok(handle) => handle,
            Err(e) => {
                self.state.notify(Toast::error(format!("导出失败：{e}")));
                cx.notify();
                return;
            }
        };

        self.state.exporting = Some(ExportProgress::new(name.clone(), path.clone()));
        self.tasks.export_cancel = Some(handle.cancel_token());
        cx.notify();

        let probe = std::path::PathBuf::from(&path);
        let target = path;
        let started = std::time::Instant::now();

        cx.spawn(async move |this, cx| {
            let code = loop {
                // 1) 目标文件长到多大了（`metadata` 是微秒级，不会卡界面）
                let written = std::fs::metadata(&probe).ok().map(|m| m.len());
                let secs = started.elapsed().as_secs();
                let line = last.lock().ok().map(|s| s.clone()).unwrap_or_default();

                let _ = this.update(cx, |shell, cx| {
                    if let Some(progress) = shell.state.exporting.as_mut() {
                        // 只在这两个数字真的变了才重绘 ——
                        // 每 500 ms 无脑重绘一遍是浪费。
                        let changed = progress.written != written || progress.elapsed_secs != secs;
                        progress.written = written;
                        progress.elapsed_secs = secs;
                        progress.last_line = line;
                        if changed {
                            cx.notify();
                        }
                    }
                });

                // 2) 进程结束了吗（非阻塞）
                match handle.try_wait() {
                    Ok(Some(code)) => break code,
                    Ok(None) => {}
                    Err(e) => {
                        tracing::warn!("检查导出进程失败：{e}");
                        break -1;
                    }
                }

                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
            };

            let cancelled = handle.was_cancelled();
            // 进程已退出，这里只是等读取线程把剩余输出读完。
            let _ = handle.finish();

            let _ = this.update(cx, |shell, cx| {
                let detail = shell
                    .state
                    .exporting
                    .take()
                    .map(|p| p.last_line)
                    .unwrap_or_default();
                shell.tasks.export_cancel = None;

                if cancelled {
                    shell.state.notify(Toast::info(format!(
                        "已取消导出 —— {target} 是个**不完整**的文件，需要自己删掉"
                    )));
                } else if code == 0 {
                    let size = std::fs::metadata(&target)
                        .ok()
                        .map(|m| wslc_panel_core::util::format_bytes(m.len()))
                        .unwrap_or_else(|| "大小未知".to_owned());
                    shell.state.notify(Toast::success(format!(
                        "{name} 已导出到 {target}（{size}）"
                    )));
                } else {
                    let detail = if detail.trim().is_empty() {
                        format!("退出码 {code}")
                    } else {
                        detail
                    };
                    shell
                        .state
                        .notify(Toast::error(format!("导出 {name} 失败：{detail}")));
                }

                cx.notify();
            });
        })
        .detach();
    }

    // -- 容器：启动 / 重启（不需要二次确认）--------------------------------

    /// 后台跑一个发行版动作，完了刷新。
    ///
    /// # 为什么所有动作都必须走这里
    ///
    /// `wsl.exe` 是**同步**等的（`Command::output()` 那种），
    /// 直接在界面线程上调用会让整个窗口卡住 ——
    /// Windows 大约 5 秒后就给它挂上"**未响应**"。
    ///
    /// 实测「启动」一个发行版要一两秒，`--shutdown` 更久，
    /// 压缩/移动是分钟级。这些**全都**不能在界面线程上跑。
    ///
    /// 另外：点下去先发一条"正在…"，否则从点击到结果出来这段时间
    /// 界面上什么都没发生，用起来像是按钮没反应。
    pub(crate) fn spawn_distro_action(
        &mut self,
        verb: &'static str,
        name: String,
        action: fn(&Wsl, &str) -> wslc_core::Result<()>,
        cx: &mut Context<Self>,
    ) {
        let wsl = self.state.wsl.clone();
        let target = name.clone();

        self.state
            .notify(Toast::info(format!("正在{verb} {name}…")));
        cx.notify();

        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { action(&wsl, &target) })
                .await;

            let _ = this.update(cx, |shell, cx| {
                match result {
                    Ok(()) => shell
                        .state
                        .notify(Toast::success(format!("{name} 已{verb}"))),
                    Err(e) => shell
                        .state
                        .notify(Toast::error(format!("{name} {verb}失败：{e}"))),
                }
                shell.refresh(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// 启动发行版，并**让它一直保持运行**。
    ///
    /// 做法是从 Windows 这边吊住一个 `wsl.exe -d <name> -- sleep infinity` 不放：
    /// 那个进程活着，WSL 就认为有活动会话，发行版不会被回收。
    /// 机制和实测数据见 [`wslc_core::cmd::distro::start`]。
    ///
    /// ⚠️ 和 [`Shell::spawn_distro_action`] 那条路不一样：这个**要留下句柄**，
    /// 所以不能复用那个"跑完就完"的辅助函数。
    pub fn start_distro(&mut self, name: String, cx: &mut Context<Self>) {
        // 先清一遍死掉的哨兵，免得"已经吊着了"的判断基于过期信息
        self.sync_keep_alive(cx);

        if self.state.kept_alive.iter().any(|n| n == &name) {
            self.state
                .notify(Toast::info(format!("{name} 已经由本面板保持运行中")));
            cx.notify();
            return;
        }

        self.state.notify(Toast::info(format!("正在启动 {name}…")));
        cx.notify();

        let wsl = self.state.wsl.clone();
        let target = name.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { wslc_core::cmd::distro::start(&wsl, &target) })
                .await;

            let _ = this.update(cx, |shell, cx| {
                match result {
                    Ok(child) => {
                        shell.tasks.keep_alive.push(KeepAlive {
                            name: name.clone(),
                            child,
                        });
                        shell.sync_keep_alive(cx);
                        shell.state.notify(Toast::success(format!(
                            "{name} 已启动并保持运行 —— 由本面板吊着，\
                             关掉面板它也会继续跑；想停就点「终止」"
                        )));
                    }
                    Err(e) => shell
                        .state
                        .notify(Toast::error(format!("启动 {name} 失败：{e}"))),
                }
                shell.refresh(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// 把已经死掉的哨兵清出去，并把名单同步给界面。
    ///
    /// 什么时候会死：用户点了「终止」/「关停全部」（`wsl --terminate` 会连
    /// `sleep infinity` 一起杀，哨兵进程随之退出），或者在别处跑了 `wsl --shutdown`。
    ///
    /// 放在 `refresh` 里每轮跑一次：`try_wait` 是**非阻塞**的，几乎不要钱；
    /// 而且它顺手回收了退出的子进程，不留僵尸。
    pub(crate) fn sync_keep_alive(&mut self, cx: &mut Context<Self>) {
        self.tasks.keep_alive.retain_mut(|k| k.is_alive());

        let names: Vec<String> = self
            .tasks
            .keep_alive
            .iter()
            .map(|k| k.name.clone())
            .collect();
        if self.state.kept_alive != names {
            self.state.kept_alive = names;
            cx.notify();
        }
    }

    /// 打开发行版的终端（新控制台窗口）。
    ///
    /// 这是**同步**的：`spawn_in_new_console` 只负责把窗口拉起来就返回，
    /// 不会等终端关闭（也不该等 —— 用户可能在里面待几个小时）。
    ///
    /// 起始目录来自偏好（[`TerminalStartDir`](wslc_panel_core::prefs::TerminalStartDir)）：
    /// **不是**面板自己的 cwd —— 那取决于用户从哪个文件夹启动的面板，
    /// 见 `wslc_core::cmd::distro::open_terminal`。
    pub fn open_distro_terminal(&mut self, name: String, cx: &mut Context<Self>) {
        let wsl = self.state.wsl.clone();
        // 先 clone 出来：`open_terminal` 借的是 `start`，而下面还要动 `self.state`。
        let start = self.state.prefs.terminal_dir.clone();
        let toast = match wslc_core::cmd::distro::open_terminal(&wsl, &name, start.cd_arg()) {
            Ok(()) => Toast::success(format!("已打开 {name} 的终端")),
            Err(e) => Toast::error(format!("打开终端失败：{e}")),
        };
        self.state.notify(toast);
        cx.notify();
        // 终端一起来发行版就变成运行中，刷新让状态跟上。
        self.refresh(cx);
    }

    /// 设为默认发行版（不破坏数据，所以不弹确认）。
    pub fn set_default_distro(&mut self, name: String, cx: &mut Context<Self>) {
        self.spawn_distro_action("设为默认", name, wslc_core::cmd::distro::set_default, cx);
    }

    /// 在资源管理器里定位发行版的安装目录。
    ///
    /// **只读**操作：只打开窗口，不动文件。
    pub fn reveal_distro_path(&mut self, name: String, cx: &mut Context<Self>) {
        let base = self
            .state
            .snapshot
            .distros
            .iter()
            .find(|d| d.name == name)
            .and_then(|d| d.base_path.clone());

        let Some(base) = base else {
            self.state.notify(Toast::error(format!(
                "读不到 {name} 的安装位置（注册表里没有 BasePath）"
            )));
            cx.notify();
            return;
        };

        if !base.is_dir() {
            self.state
                .notify(Toast::error(format!("目录不存在：{}", base.display())));
            cx.notify();
            return;
        }

        // `explorer.exe` 即使成功也常返回非 0，所以只看能否启动。
        match std::process::Command::new("explorer.exe")
            .arg(&base)
            .spawn()
        {
            Ok(_) => {
                tracing::info!("已在资源管理器中打开 {}", base.display());
                self.state
                    .notify(Toast::info(format!("已打开 {}", base.display())));
            }
            Err(e) => {
                tracing::warn!("打开资源管理器失败：{e}");
                self.state.notify(Toast::error(format!("打开失败：{e}")));
            }
        }
        cx.notify();
    }

    /// 打开**发行版**详情弹窗。
    ///
    /// 低频但重要的动作（改版本 / 压缩 / 打开安装位置）都收在这里，
    /// 列表行里只留高频的那几个 —— 和容器页同一套取舍。
    pub fn open_distro_detail(&mut self, name: String, cx: &mut Context<Self>) {
        self.dialog_ui.distro_detail = Some(name);
        cx.notify();
    }

    /// 关闭发行版详情弹窗。
    pub fn close_distro_detail(&mut self, cx: &mut Context<Self>) {
        self.dialog_ui.distro_detail = None;
        cx.notify();
    }

    // -- 文件 / 目录选择器 --------------------------------------------------

    /// 弹一个选择器，选完把路径写进 `input`。
    ///
    /// # 为什么能安心在这里 await
    ///
    /// 选择器跑在一个**独立进程**里（`cmd::picker` 的说明里讲了为什么不用
    /// 原生 crate）。所以哪怕用户开着对话框去泡杯茶，界面线程也一点没被占住 ——
    /// 这一点是**结构上**成立的，不依赖我对某个库的线程模型的判断。
    ///
    /// # 为什么用 `spawn_in` 而不是 `spawn`
    ///
    /// 写输入框要 `InputState::set_value(value, window, cx)`，**它要一个
    /// `&mut Window`**。`cx.spawn` 给的回调里没有 window；
    /// `cx.spawn_in(window, ...)` 才有 —— 配合 `update_in` 就能把 window 拿回来。
    // 参数确实多（输入框 / 目录还是文件 / 标题 / 扩展名 / 窗口 / 上下文……），
    // 但它们各自独立、没有天然的聚合体，硬凑个结构体只是把复杂度搬个地方。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn spawn_picker(
        &mut self,
        input: &Entity<InputState>,
        folders: bool,
        label: &'static str,
        extensions: &'static [&'static str],
        title: &'static str,
        after: AfterPick,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.tasks.picking {
            self.state
                .notify(Toast::info("已经有一个选择框开着了，先处理它"));
            cx.notify();
            return;
        }
        self.tasks.picking = true;
        let input = input.clone();
        cx.notify();

        cx.spawn_in(window, async move |this, cx| {
            // 起进程等对话框 —— 这段在后台执行器上，不占界面线程。
            let picked = cx
                .background_executor()
                .spawn(async move {
                    if folders {
                        wslc_core::cmd::picker::pick_directory(title)
                    } else {
                        wslc_core::cmd::picker::pick_file(label, extensions)
                    }
                })
                .await;

            let _ = this.update_in(cx, |shell, window, cx| {
                shell.tasks.picking = false;

                match picked {
                    Ok(Some(path)) => {
                        input.update(cx, |state, cx| state.set_value(path, window, cx));
                        match after {
                            AfterPick::Nothing => {}
                            AfterPick::InstallDirIsNowUserChosen => {
                                // ⚠️ 这行**只能在真的选到东西时**执行。
                                //
                                // 原先它写在 `browse_install_dir` 里、`spawn_picker`
                                // 调用之后，于是"点了浏览又点了取消"也会把它清掉：
                                // 用户什么都没选，却永久失去了"安装目录跟着发行版名
                                // 联动"，之后只能自己手敲路径。
                                // （选择器还可能因为 `picking` 已经在跑而直接返回，
                                // 那种情况下更不该动。）
                                if let Some(form) = shell.forms.install_form.as_mut() {
                                    form.last_derived_dir = None;
                                }
                            }
                            AfterPick::DeploySource => {
                                // 同一个理由：只在**真的选到**之后重算启动命令。
                                // 取消的话用户那一格原样留着。
                                shell.resync_deploy_command(window, cx);
                            }
                        }
                    }
                    // 用户点了取消 —— 这不是错误，什么都不做
                    Ok(None) => {}
                    Err(e) => shell
                        .state
                        .notify(Toast::error(format!("打开选择框失败：{e}"))),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 给提示弹窗的输入框弹选择器（移动位置选目录 / 导出选文件）。
    pub fn browse_prompt_path(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = self
            .forms
            .prompt
            .as_ref()
            .and_then(|p| p.kind.pick_target().map(|t| (p.input.clone(), t)));

        let Some((input, target)) = picked else {
            return;
        };

        self.spawn_picker(
            &input,
            target.folders,
            target.label,
            target.extensions,
            target.title,
            // 移动位置 / 导出只填输入框，选完不需要额外做什么
            //（安装目录那一份才要清 `last_derived_dir`）。
            AfterPick::Nothing,
            window,
            cx,
        );
    }

    /// 打开「发行版配置」弹窗：后台读 `/etc/wsl.conf` + `wsl --version`。
    ///
    /// 两件事都要起 `wsl.exe`（秒级），所以走后台执行器；
    /// 回来时用 `spawn_in` + `update_in` 拿回 `&mut Window` 建输入框
    /// （`InputState::new` 要它）。
    pub fn open_wslconf(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.wslconf.is_some() {
            return;
        }
        self.state
            .notify(Toast::info(format!("正在读取 {name} 的 /etc/wsl.conf…")));
        cx.notify();

        let wsl = self.state.wsl.clone();
        let target = name.clone();
        cx.spawn_in(window, async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move {
                    let text = wslc_core::cmd::distro::read_wsl_conf(&wsl, &target)?;
                    let version = wslc_core::cmd::distro::wsl_version(&wsl);
                    Ok::<_, wslc_core::Error>((text, version))
                })
                .await;

            let _ = this.update_in(cx, |shell, window, cx| {
                match loaded {
                    Ok((text, version)) => {
                        let doc = wslc_core::model::wslconf::WslConfDoc::parse(&text);

                        // 给每个可编辑的文本字段建输入框，并**预填有效值**
                        // （文件里没写时就是默认值）—— 界面上不该是空白。
                        let mut inputs = std::collections::HashMap::new();
                        for field in wslc_core::model::wslconf::FIELDS {
                            if field.kind != wslc_core::model::wslconf::FieldKind::Text
                                || field.read_only
                            {
                                continue;
                            }
                            let value = doc.effective(field.section, field.key);
                            let input = cx.new(|cx| {
                                InputState::new(window, cx).placeholder(field.placeholder)
                            });
                            input.update(cx, |state, cx| state.set_value(value, window, cx));
                            inputs.insert((field.section, field.key), input);
                        }

                        let input_subs = inputs
                            .values()
                            .map(|input| {
                                cx.subscribe(input, |_, _, event: &InputEvent, cx| {
                                    if matches!(event, InputEvent::Change) {
                                        cx.notify();
                                    }
                                })
                            })
                            .collect();
                        shell.forms.wslconf_dialog = Some(WslConfDialog {
                            inputs,
                            _input_subs: input_subs,
                        });
                        shell.state.wslconf = Some(state::WslConfState::new(name, doc, version));
                    }
                    Err(e) => shell
                        .state
                        .notify(Toast::error(format!("读取 {name} 的配置失败：{e}"))),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 关闭弹窗（不保存）。
    pub fn close_wslconf(&mut self, cx: &mut Context<Self>) {
        self.forms.wslconf_dialog = None;
        self.state.wslconf = None;
        cx.notify();
    }

    /// 切换一个布尔字段。
    ///
    /// `Checkbox` 是**受控**组件：`on_change` 给的是"请求的新值"，
    /// 由我们存下来再 `cx.notify()`（见 gpui-kit 的 checkbox 文档）。
    pub fn set_wslconf_bool(
        &mut self,
        section: &'static str,
        key: &'static str,
        value: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = self.state.wslconf.as_mut() {
            state.doc.set_bool(section, key, value);
            cx.notify();
        }
    }

    /// 显示 / 隐藏"实际会写进去的内容"。
    pub fn toggle_wslconf_preview(&mut self, cx: &mut Context<Self>) {
        if let Some(state) = self.state.wslconf.as_mut() {
            state.show_preview = !state.show_preview;
            cx.notify();
        }
    }

    /// 保存；`restart` = 顺便重启发行版让改动生效。
    ///
    /// `wsl.conf` 要**发行版重启之后**才被读，所以改完不重启等于没生效 ——
    /// 参考项目也是这么给的（「保存」/「保存并重启发行版」两个按钮）。
    pub fn save_wslconf(&mut self, restart: bool, cx: &mut Context<Self>) {
        // 1) 把输入框的值读回文档。
        //    先收集成拥有所有权的数据，**结束对 `self` 的借用** ——
        //    下面要 `&mut self.state`。
        let Some(dialog) = self.forms.wslconf_dialog.as_ref() else {
            return;
        };
        let updates: Vec<(&'static str, &'static str, String)> = wslc_core::model::wslconf::FIELDS
            .iter()
            .filter(|f| f.kind == wslc_core::model::wslconf::FieldKind::Text && !f.read_only)
            .filter_map(|f| {
                dialog
                    .inputs
                    .get(&(f.section, f.key))
                    .map(|input| (f.section, f.key, input.read(cx).value().trim().to_owned()))
            })
            .collect();

        let Some(state) = self.state.wslconf.as_mut() else {
            return;
        };
        for (section, key, value) in updates {
            // 与预览共用，未改动的预填默认值不写成显式配置。
            state.apply_text_value(section, key, &value);
        }

        if !state.doc.is_dirty() {
            self.state.notify(Toast::info("没有改动"));
            cx.notify();
            return;
        }

        let doc = state.doc.clone();
        let text = doc.render();
        let distro = state.distro.clone();
        state.busy = true;
        state.errors.clear();
        cx.notify();

        // 记下"它本来是不是我们吊着的" —— 重启后要恢复
        let was_kept = self.state.kept_alive.iter().any(|n| n == &distro);

        let wsl = self.state.wsl.clone();
        let target = distro.clone();
        // 内层 `async move` 会把 `task_target` 吃掉；外层 `update` 还要用
        // `target` 拼提示、恢复 keep-alive，所以这里分成两个名字。
        let task_target = target.clone();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    // 2) 校验。写错 `[user] default` 会让发行版**下次启动直接失败**，
                    //    写错 `[boot] command` 会让它起不来 —— 都必须在写之前挡下。
                    let mut errors = Vec::new();
                    if let Some(user) = doc.get("user", "default") {
                        if !user.trim().is_empty()
                            && !wslc_core::cmd::distro::user_exists(&wsl, &task_target, user)?
                        {
                            errors.push(format!("发行版里没有用户「{user}」"));
                        }
                    }
                    if let Some(cmd) = doc.get("boot", "command") {
                        if !cmd.trim().is_empty()
                            && !wslc_core::cmd::distro::path_exists(&wsl, &task_target, cmd)?
                        {
                            errors.push(format!("找不到启动命令「{cmd}」"));
                        }
                    }
                    if !errors.is_empty() {
                        return Ok::<_, wslc_core::Error>((errors, false));
                    }

                    // 3) 写（`write_wsl_conf` 内部会先备份到 .bak）
                    wslc_core::cmd::distro::write_wsl_conf(&wsl, &task_target, &text)?;

                    // 4) 重启：`wsl.conf` 要重启才被读
                    if restart {
                        wslc_core::cmd::distro::terminate(&wsl, &task_target)?;
                    }
                    Ok((errors, true))
                })
                .await;

            let _ = this.update(cx, |shell, cx| {
                let saved = match outcome {
                    Ok((errors, saved)) => {
                        if let Some(state) = shell.state.wslconf.as_mut() {
                            state.busy = false;
                            state.errors = errors;
                            if !state.errors.is_empty() {
                                // 当前后台校验涉及默认用户和启动命令，报错时展开相关组。
                                state.set_section_open("user", true);
                                state.set_section_open("boot", true);
                            }
                        }
                        saved
                    }
                    Err(e) => {
                        if let Some(state) = shell.state.wslconf.as_mut() {
                            state.busy = false;
                        }
                        shell.state.notify(Toast::error(format!("保存失败：{e}")));
                        false
                    }
                };

                if saved {
                    shell.forms.wslconf_dialog = None;
                    shell.state.wslconf = None;
                    shell.state.notify(Toast::success(format!(
                        "{target} 的 /etc/wsl.conf 已保存{}",
                        if restart {
                            "，发行版已重启（改动已生效）"
                        } else {
                            "（重启发行版后才生效）"
                        }
                    )));

                    // 重启会把我们吊着的哨兵一起带走，清掉那个标记；
                    // 如果它本来是被我们保持运行的，就重新吊起来。
                    shell.sync_keep_alive(cx);
                    if restart && was_kept {
                        shell.start_distro(target.clone(), cx);
                    }
                    shell.refresh(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }
}
