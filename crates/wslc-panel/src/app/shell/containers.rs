//! 容器域：启动 / 停止 / 创建 / 详情 / 日志 / exec / 终端。

use super::super::prelude::*;
use super::Shell;
use super::state::{CreateDialog, DropdownItem, FactsState, TextPrompt};
// 弹窗底部那行按钮的唯一实现（和 18 个浮层共用同一份，见 `views::common`）。
use crate::views::modal_footer;

impl Shell {
    /// 打开提示对话框，并把焦点交给输入框。
    ///
    /// # 这是**第一个**迁到官方 `Dialog` 的浮层
    ///
    /// 其余 17 个浮层还是"状态驱动的纯函数 + `views::modal_scrim`"，作为
    /// `Shell::render` 的孩子画出来 —— 它们的层序靠 `render.rs` 末尾那条
    /// `.child()` 链子手工维持（`AGENTS.md` §7.5 记着这条链子踩过两次坑）。
    ///
    /// 官方 `Dialog` 走的是**窗口级层栈**（`WindowState::active_dialogs`，由
    /// `Root` 渲染），所以：层序 = 栈序，遮罩自带 `.occlude()`、Esc 与点遮罩
    /// 关闭自带（`keyboard` / `overlay_closable`），不用我们维护。
    ///
    /// # 三个要注意的地方（都是 v0.19.0 实机踩出来的）
    ///
    /// 1. builder 闭包**可能被重跑**（gpui-component 的原话："it may run again
    ///    when the dialog layer is rebuilt. Keep it cheap and create persistent
    ///    entities before opening, not inside the builder"）—— 所以 `InputState`
    ///    是在**打开之前**建好、存进 `self.prompt` 的，闭包里只用它的 `Entity`；
    /// 2. ⚠️ **别在闭包里现算"随打字变化"的内容**：闭包只在层被重建时重跑，
    ///    **打字不触发它**。v0.19.0 实机就是这个 bug —— 输入框里已经填了
    ///    `D:\wsl\MyDistro`，下面那行等效命令还停在 `--move <待填>`。
    ///    正确做法是内容用**子实体**（[`crate::views::PromptContent`]）并
    ///    `observe` 输入框，打字时只重画那一块；
    /// 3. ⚠️ **底部按钮必须自己给**（`.footer(..)`）：普通 `Dialog` 不会自动画
    ///    OK / Cancel —— 那是 `AlertDialog` 的行为。`DialogButtonProps` 的
    ///    `render_ok` / `render_cancel` 只有 `AlertDialog` 调用，`Dialog::render`
    ///    只看 `self.footer`，而 `show_cancel` 默认 `false`。v0.19.0 实机就是
    ///    因为这条少了一整排按钮。
    pub fn open_prompt(
        &mut self,
        kind: PromptKind,
        distro: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_kit::component::WindowExt as _;

        // 已经在开着就别再压一层（连点两下会弹出两个一模一样的对话框）。
        if self.prompt.is_some() {
            return;
        }

        let input = cx.new(|cx| InputState::new(window, cx).placeholder(kind.placeholder()));

        // 打开就把焦点给输入框，用户可以直接开始打字。
        let handle = input.read(cx).focus_handle(cx);
        window.focus(&handle, cx);

        let entity = cx.entity();
        // 内容做成**子实体**并订阅输入框：打字时只重画它（见 `PromptContent` 的注释）。
        let content = cx.new({
            let input = input.clone();
            let entity = entity.clone();
            let distro = distro.clone();
            move |cx| crate::views::PromptContent::new(kind, distro, input, entity, cx)
        });

        self.prompt = Some(TextPrompt {
            kind,
            distro,
            input,
        });
        cx.notify();

        let on_ok = {
            let entity = entity.clone();
            move |_: &ClickEvent, _: &mut Window, cx: &mut App| -> bool {
                entity.update(cx, |shell, cx| shell.submit_prompt(cx));
                true
            }
        };
        let on_cancel = {
            let entity = entity.clone();
            move |_: &ClickEvent, _: &mut Window, cx: &mut App| -> bool {
                entity.update(cx, |shell, cx| shell.close_prompt(cx));
                true
            }
        };
        let on_close = {
            let entity = entity.clone();
            move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                entity.update(cx, |shell, cx| shell.close_prompt(cx));
            }
        };

        window.open_dialog(cx, move |dialog, _window, _cx| {
            // 内容是一块**子实体**（`PromptContent`），它自己 `observe` 输入框：
            // 打字时只重画那一块，不依赖 dialog 层重建。
            //
            // ⚠️ 别把它改成"在闭包里现算"：闭包只在层被重建时重跑，打字不触发 ——
            // v0.19.0 实机就是这个 bug（输入框填了路径，等效命令还停在 `<待填>`）。
            let body = content.clone();

            // ⚠️ 底部按钮必须自己给：普通 `Dialog` **不会**自动画 OK / Cancel
            // （那是 `AlertDialog` 的行为；`Dialog::render` 只看 `self.footer`，
            // `show_cancel` 默认 `false`）。v0.19.0 实机就是因为这条少了一整排按钮。
            let cancel = {
                let entity = entity.clone();
                Button::new("prompt-cancel")
                    .label("取消")
                    .small()
                    .on_click(move |_, window, cx| {
                        entity.update(cx, |shell, cx| shell.close_prompt(cx));
                        window.close_dialog(cx);
                    })
            };
            let confirm = {
                let entity = entity.clone();
                Button::new("prompt-confirm")
                    .label(kind.confirm_label())
                    .small()
                    .primary()
                    .on_click(move |_, window, cx| {
                        entity.update(cx, |shell, cx| shell.submit_prompt(cx));
                        window.close_dialog(cx);
                    })
            };

            dialog
                .title(kind.title())
                .w(px(560.))
                // 遮罩 / Esc / 点遮罩关闭都由官方负责（Esc 与遮罩走 `on_cancel`）。
                .overlay(true)
                .overlay_closable(true)
                .keyboard(true)
                .on_ok(on_ok.clone())
                .on_cancel(on_cancel.clone())
                .on_close(on_close.clone())
                .child(body)
                .footer(modal_footer([
                    cancel.into_any_element(),
                    confirm.into_any_element(),
                ]))
        });
    }

    /// 关闭提示弹窗。
    pub fn close_prompt(&mut self, cx: &mut Context<Self>) {
        self.prompt = None;
        cx.notify();
    }

    /// 提交提示弹窗：按 `kind` 调对应的 `wsl --manage`。
    ///
    /// 移动和调整大小都可能是**分钟级**操作，所以走后台执行器。
    pub fn submit_prompt(&mut self, cx: &mut Context<Self>) {
        // 一次把需要的值全取出来，**结束对 `self` 的借用** ——
        // 下面要 `&mut self` 去发提示条。
        let (kind, distro, value) = match self.prompt.as_ref() {
            Some(prompt) => (
                prompt.kind,
                prompt.distro.clone(),
                prompt.input.read(cx).value().trim().to_owned(),
            ),
            None => return,
        };

        if value.is_empty() {
            self.state.notify(Toast::error("请先填写内容"));
            cx.notify();
            return;
        }

        // 本地能挡的先挡掉，不白起一个进程。
        if kind == PromptKind::ResizeDistro {
            if let Err(e) = wslc_core::cmd::distro::normalize_size(&value) {
                self.state.notify(Toast::error(format!("大小不合法：{e}")));
                cx.notify();
                return;
            }
        }

        // 移动和调整大小都要动这个发行版的虚拟磁盘，和正在进行的
        // 安装 / 导出抢同一个文件。弹窗可能是几分钟前打开的，
        // 那期间安装可能已经开始 —— 所以判在**提交这一刻**。
        let touches_disk = matches!(kind, PromptKind::MoveDistro | PromptKind::ResizeDistro);
        let busy = if touches_disk {
            self.state.busy_with(&distro)
        } else {
            None
        };
        if let Some(what) = busy {
            self.state
                .notify(Toast::error(format!("{distro} 正在{what}，等它结束再操作")));
            cx.notify();
            return;
        }

        // 导出是**长任务**（要进度条、要能取消），走的是完全不同的一条路 ——
        // 它和上面三个只是共用了"让用户填一个文本参数"这个外形。
        if kind == PromptKind::ExportDistro {
            self.prompt = None;
            self.start_export(distro, value, cx);
            return;
        }

        // 创建卷也先分流：它走 `wslc.exe` 而不是 `wsl.exe`，而且**不属于
        // 任何发行版**（`distro` 是空串）。放进下面那个 match 的话，
        // 那一整块都得同时拿着两个调用器。
        if kind == PromptKind::CreateVolume {
            self.prompt = None;
            self.create_volume(value, cx);
            return;
        }

        let title = kind.title();
        self.prompt = None;
        self.state
            .notify(Toast::info(format!("正在{title}…（大磁盘可能要几分钟）")));
        cx.notify();

        let wsl = self.state.wsl.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    match kind {
                        PromptKind::MoveDistro => {
                            wslc_core::cmd::distro::move_distro(&wsl, &distro, &value)
                                .map(|()| format!("{distro} 已移动到 {value}"))
                        }
                        PromptKind::ResizeDistro => {
                            wslc_core::cmd::distro::resize(&wsl, &distro, &value)
                                .map(|()| format!("{distro} 的磁盘已调整为 {value}"))
                        }
                        PromptKind::SetDefaultUser => {
                            wslc_core::cmd::distro::set_default_user(&wsl, &distro, &value)
                                .map(|()| format!("{distro} 的默认用户已设为 {value}"))
                        }
                        // 导出在上面就分流到 `start_export` 了，到不了这里。
                        // 不用 `unreachable!()`：真要漏进来，宁可给一条能读的错误，
                        // 也不要在用户的机器上 panic。
                        PromptKind::ExportDistro => Err(wslc_core::Error::InvalidArgument(
                            "导出不走这条同步路径".to_owned(),
                        )),
                        // 创建卷也已经在上面分流（它走 `wslc`，不在这里）。
                        PromptKind::CreateVolume => Err(wslc_core::Error::InvalidArgument(
                            "创建卷不走这条发行版路径".to_owned(),
                        )),
                    }
                })
                .await;

            let _ = this.update(cx, |shell, cx| {
                match result {
                    Ok(message) => shell.state.notify(Toast::success(message)),
                    Err(e) => shell
                        .state
                        .notify(Toast::error(format!("{title}失败：{e}"))),
                }
                shell.refresh(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// 创建卷（`wslc volume create <名字>`）。
    ///
    /// 名字先在本地挡一道（[`state::validate_volume_name`]）：以 `-` 开头的
    /// 名字会被 `wslc` 当成**选项**而不是卷名 —— 用户会以为建成功了，
    /// 实际什么都没建。这类"看着成功其实没做"必须先挡。
    ///
    /// 别的合法性（字符集 / 长度）交给 `wslc` 自己判：它的规则我们没实测过，
    /// 自己编一套只会把合法名字挡在外面。
    pub(crate) fn create_volume(&mut self, name: String, cx: &mut Context<Self>) {
        if let Err(reason) = state::validate_volume_name(&name) {
            self.state.notify(Toast::error(reason));
            cx.notify();
            return;
        }

        self.state
            .notify(Toast::info(format!("正在创建卷 {name}…")));
        cx.notify();

        let wslc = self.state.wslc.clone();
        cx.spawn(async move |this, cx| {
            let target = name.clone();
            let result = cx
                .background_executor()
                .spawn(async move { wslc_core::cmd::volume::create(&wslc, &target) })
                .await;

            let _ = this.update(cx, |shell, cx| {
                match result {
                    Ok(out) => {
                        // `wslc volume create` 成功时会回显一行（卷名或 ID）。
                        // 有就带上，没有也不编。
                        let out = out.trim().to_owned();
                        shell.state.notify(Toast::success(if out.is_empty() {
                            format!("已创建卷 {name}")
                        } else {
                            format!("已创建卷 {name}：{out}")
                        }));
                    }
                    Err(e) => shell
                        .state
                        .notify(Toast::error(format!("创建卷 {name} 失败：{e}"))),
                }
                // 列表要立刻反映出新卷
                shell.refresh(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// 启动容器。
    ///
    /// 启动/重启**不加二次确认**：它们不破坏数据，而且是运维里最高频的
    /// 动作，每次都弹窗反而碍事。破坏性的停止/强杀/删除仍然走确认。
    pub fn start_container(&mut self, name: String, cx: &mut Context<Self>) {
        self.spawn_container_action("启动", name, wslc_core::cmd::container::start, cx);
    }

    /// 重启容器。
    pub fn restart_container(&mut self, name: String, cx: &mut Context<Self>) {
        self.spawn_container_action("重启", name, wslc_core::cmd::container::restart, cx);
    }

    /// 分派一个即时操作（界面统一走这个入口）。
    pub fn run_immediate(&mut self, action: ImmediateAction, cx: &mut Context<Self>) {
        match action {
            ImmediateAction::StartContainer(name) => self.start_container(name, cx),
            ImmediateAction::RestartContainer(name) => self.restart_container(name, cx),
            ImmediateAction::StartDistro(name) => self.start_distro(name, cx),
            ImmediateAction::OpenDistroTerminal(name) => self.open_distro_terminal(name, cx),
            ImmediateAction::SetDefaultDistro(name) => self.set_default_distro(name, cx),
            ImmediateAction::OpenContainerShell(name) => self.open_container_shell(name, cx),
            ImmediateAction::AttachContainer(name) => self.attach_container(name, cx),
            ImmediateAction::FollowContainerLogs(name) => self.follow_container_logs(name, cx),
            // 「启动 / 停止一条部署」（v0.9）—— 它们不做容器级的事，
            // 只操作"那个应用自己的进程"，所以走部署页那套方法。
            ImmediateAction::StartDeployment(app) => self.start_deployment(app, cx),
            ImmediateAction::StopDeployment(app) => self.stop_deployment(app, cx),
        }
    }

    // -- WSL 发行版（实例）动作 --------------------------------------------

    /// 启动/重启的公共实现：后台跑一条 `wslc <verb> <name>`，完了刷新。
    ///
    /// 用函数指针而不是闭包泛型：`start` 和 `restart` 签名一致，
    /// 函数指针省掉一层泛型参数，编译器也更容易推断。
    pub(crate) fn spawn_container_action(
        &mut self,
        verb: &'static str,
        name: String,
        action: fn(&Wslc, &[String]) -> wslc_core::Result<Vec<String>>,
        cx: &mut Context<Self>,
    ) {
        // 点下去先给个回应 —— 和发行版那条（[`Shell::spawn_distro_action`]）
        // 保持一致。
        //
        // 没有这一条的话，从点击到结果出来这段时间界面上什么都没变。
        // `wslc start/restart` 通常是秒级，但容器多、镜像要拉、或者 WSL
        // 刚被唤醒时会明显更久 —— 那期间用户只会觉得按钮坏了然后去点第二次
        // （`AGENTS.md` §7.1 第二条）。
        self.state
            .notify(Toast::info(format!("正在{verb} {name}…")));
        cx.notify();

        let wslc = self.state.wslc.clone();
        let target = name.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { action(&wslc, std::slice::from_ref(&target)) })
                .await;

            let _ = this.update(cx, |shell, cx| {
                match result {
                    Ok(_) => shell
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

    // -- 创建容器弹窗 --------------------------------------------------------

    /// 打开「创建容器」弹窗（不带预填）。
    ///
    /// 和拉取弹窗一样是**懒创建**：`InputState::new` 要 `&mut Window`，
    /// 而 `Shell::new` 拿不到 window。
    pub fn open_create_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_create_dialog_prefilled(None, window, cx);
    }

    /// 打开「创建容器」弹窗，可**预填镜像引用**。
    ///
    /// 「镜像」页拉完镜像直接建容器走这条：镜像名是我们刚拉下来的**完整引用**
    /// （`docker.1panel.live/library/alpine:latest`），让用户再手打一遍没有意义 ——
    /// 而且手打时最容易漏掉 registry 前缀，那样 `wslc run` 会去 Docker Hub 找，
    /// 而它在这台机器上不可达（实测 000）。
    ///
    /// 拉取策略仍然是 [`PullPolicy::Never`]（弹窗里可改）：镜像刚拉到本地，
    /// 再拉一次纯属白等。
    pub fn open_create_dialog_prefilled(
        &mut self,
        image: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state
            .set_section_open(UiSection::CreateAdvanced, false);
        self.state
            .set_section_open(UiSection::CreateCommandPreview, false);
        let image_select = cx.new(|cx| {
            SelectState::new(Vec::<DropdownItem>::new(), None, window, cx).searchable(true)
        });

        // ⚠️ 这个订阅是**必须**的：不订阅的话"从下拉框里选了一个镜像"什么都不会发生
        // （输入框不变、也不会去读它的端口）。订阅本身要存进 `Shell` ——
        // `Subscription` **一 drop 就退订**（和 `select_subs` 同一个坑）。
        let pick_sub = cx.subscribe_in(
            &image_select,
            window,
            |shell, _emitter, event: &SelectEvent<Vec<DropdownItem>>, window, cx| {
                if let SelectEvent::Confirm(Some(reference)) = event {
                    shell.pick_local_image(reference.clone(), window, cx);
                }
            },
        );
        self.create_subs = vec![pick_sub];

        let dialog = CreateDialog {
            image: cx.new(|cx| {
                InputState::new(window, cx).placeholder("docker.1panel.live/library/nginx:latest")
            }),
            image_select,
            name: cx.new(|cx| InputState::new(window, cx).placeholder("留空就按镜像命名")),
            command: cx.new(|cx| {
                InputState::new(window, cx).placeholder("留空用镜像自己的，如 tail -f /dev/null")
            }),
            ports: cx.new(|cx| InputState::new(window, cx).placeholder("8080:80, 9090:90")),
            env: cx.new(|cx| InputState::new(window, cx).placeholder("TZ=Asia/Shanghai")),
            volumes: cx
                .new(|cx| InputState::new(window, cx).placeholder("webdata:/usr/share/nginx/html")),
            network: cx.new(|cx| InputState::new(window, cx).placeholder("留空则用 bridge")),
            memory: cx.new(|cx| InputState::new(window, cx).placeholder("512M")),
            cpus: cx.new(|cx| InputState::new(window, cx).placeholder("0.5")),
            pull: PullPolicy::Never,
            facts: FactsState::default(),
        };

        // 预填（`set_value` 要 `&mut Window`，所以只能在有 window 的地方做）
        if let Some(image) = image {
            dialog.image.update(cx, |state, cx| {
                state.set_value(image.to_owned(), window, cx)
            });
        }

        // 焦点给第一个必填字段
        let handle = dialog.image.read(cx).focus_handle(cx);
        window.focus(&handle, cx);

        // 输入实体自己重绘不会通知 Shell；摘要和展开后的预览需要同一帧更新。
        for input in [
            &dialog.image,
            &dialog.name,
            &dialog.command,
            &dialog.ports,
            &dialog.env,
            &dialog.volumes,
            &dialog.network,
            &dialog.memory,
            &dialog.cpus,
        ] {
            self.create_subs
                .push(cx.subscribe(input, |_, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        cx.notify();
                    }
                }));
        }
        self.create_dialog = Some(dialog);
        // 本地镜像候选：直接来自快照（每 3 秒采一次），**不起新进程**。
        self.fill_image_select(window, cx);
        // 带着镜像进来时顺手读一次"它声明了哪些端口、会跑什么"——
        // 读到了就把端口预填好（见 `probe_image_facts`）。
        if let Some(image) = image {
            self.probe_image_facts(image.to_owned(), window, cx);
        }
        cx.notify();
    }

    /// 把本地已有的镜像填进创建表单那个「从本地镜像里选」下拉框。
    ///
    /// 数据直接用 [`AppState::snapshot`] 里的 `images`（`wslc images` 每 3 秒
    /// 采一次）—— **不额外起一个 `wslc`**，而且它就是页面上看到的同一份。
    /// 筛选 / 去重 / 排序在 `wslc_core::model::local_references` 里（纯函数）。
    ///
    /// 标签写 `引用（体积）`（体积帮用户认人），**取到的值只有引用** ——
    /// 那才是 `wslc run` 要的东西。
    pub(crate) fn fill_image_select(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self.create_dialog.as_ref() else {
            return;
        };
        let select = dialog.image_select.clone();

        let items: Vec<DropdownItem> =
            wslc_core::model::local_references(&self.state.snapshot.images)
                .into_iter()
                .map(|reference| {
                    let size = self
                        .state
                        .snapshot
                        .images
                        .iter()
                        .find(|img| img.reference() == reference)
                        .map(|img| img.size.trim().to_owned())
                        .unwrap_or_default();
                    let label = if size.is_empty() {
                        reference.clone()
                    } else {
                        format!("{reference}（{size}）")
                    };
                    DropdownItem::new(label, reference)
                })
                .collect();

        select.update(cx, |state, cx| state.set_items(items, window, cx));
    }

    /// 「创建容器」表单里从本地镜像下拉框选了一个：填进「镜像引用」并去读它。
    ///
    /// 顺带 [`Shell::probe_image_facts`]：选完镜像就该看到它的端口与启动命令
    /// （和从「镜像」页带着引用进来时同一条路）。
    pub fn pick_local_image(
        &mut self,
        reference: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(dialog) = self.create_dialog.as_ref() else {
            return;
        };
        let input = dialog.image.clone();
        input.update(cx, |state, cx| {
            state.set_value(reference.clone(), window, cx)
        });
        self.probe_image_facts(reference, window, cx);
        cx.notify();
    }

    /// 后台读一次镜像声明的端口 / 启动命令，填进创建表单。
    ///
    /// ⚠️ 这是**阻塞**调用（起 `wslc image inspect`，实测几十毫秒），所以走
    /// 后台执行器；填输入框要 `&mut Window`，所以走 `spawn_in` + `update_in`
    /// （`AGENTS.md` §7.1 / §7.2 同一条路，和 `refresh_online_list` 一样）。
    ///
    /// 失败**不打断**创建流程：只是没有预填，用户照旧能手打端口。
    pub(crate) fn probe_image_facts(
        &mut self,
        image: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 静态预设（環境变量 / 数据卷）**先**套上：它们不需要 inspect，
        // 而且用户一看到表单就该已经填好（见 `prefill_from_image_preset`）。
        self.prefill_from_image_preset(&image, window, cx);

        if let Some(dialog) = self.create_dialog.as_mut() {
            dialog.facts = FactsState::Loading;
        }
        let wslc = self.state.wslc.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { wslc_core::cmd::image::inspect_facts(&wslc, &image) })
                .await;
            let _ = this.update_in(cx, |shell, window, cx| {
                // 用户可能已经把弹窗关了（或者又开了一个）—— 那就丢掉这次结果。
                let Some(dialog) = shell.create_dialog.as_mut() else {
                    return;
                };
                match result {
                    Ok(facts) => {
                        // 端口**只在用户还没填**的时候预填 —— 不覆盖用户手输的东西
                        // （和「添加实例」那个 `last_derived_dir` 是同一条原则）。
                        let empty = dialog.ports.read(cx).value().trim().is_empty();
                        let suggested =
                            wslc_core::model::suggest_port_mappings(&facts.exposed_ports);
                        if empty && !suggested.is_empty() {
                            dialog.ports.update(cx, |state, cx| {
                                state.set_value(suggested.clone(), window, cx)
                            });
                        }
                        dialog.facts = FactsState::Ready(facts);
                    }
                    Err(e) => dialog.facts = FactsState::Failed(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 按镜像套用**静态预设**：必需的環境变量（只填 `KEY=`）+ 建议的数据卷。
    ///
    /// # 和端口预填同一条原则
    ///
    /// **只在那一格是空的时候填**，绝不覆盖用户手输的东西。
    ///
    /// # 为什么環境变量只填 `KEY=`、不填值
    ///
    /// 这些变量的值多半是**密码**（`MYSQL_ROOT_PASSWORD`），我们猜不出来；
    /// 随便填一个反而是害用户（他以为已经配好了）。所以只把变量名摆在那儿，
    /// **提交时**再检查一次它有没有被填上 —— 见
    /// [`wslc_core::model::prefilled_env_left_empty`] 与 `confirm_create`。
    ///
    /// 卷用**命名卷**（`mysqldata:/var/lib/mysql`）而不是绑定挂载：
    /// 命名卷不需要用户先想好一个主机路径，而且容器删了数据还在。
    pub(crate) fn prefill_from_image_preset(
        &mut self,
        image: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(preset) = wslc_core::model::image_preset(image) else {
            return;
        };
        let Some(dialog) = self.create_dialog.as_ref() else {
            return;
        };

        if let Some(key) = preset.prefill_env {
            let env = dialog.env.clone();
            if env.read(cx).value().trim().is_empty() {
                env.update(cx, |state, cx| {
                    state.set_value(format!("{key}="), window, cx)
                });
            }
        }

        if let Some((volume, path)) = preset.data_volume {
            let volumes = dialog.volumes.clone();
            if volumes.read(cx).value().trim().is_empty() {
                volumes.update(cx, |state, cx| {
                    state.set_value(format!("{volume}:{path}"), window, cx)
                });
            }
        }
    }

    /// 关闭「创建容器」弹窗。
    pub fn close_create_dialog(&mut self, cx: &mut Context<Self>) {
        self.create_dialog = None;
        // 订阅跟着弹窗一起丢：下拉框已经没了，留着它只是白占一个 `Subscription`
        // （下次开弹窗会重新建一份）。
        self.create_subs.clear();
        cx.notify();
    }

    /// 切换拉取策略。
    pub fn set_create_pull(&mut self, pull: PullPolicy, cx: &mut Context<Self>) {
        if let Some(dialog) = self.create_dialog.as_mut() {
            dialog.pull = pull;
            cx.notify();
        }
    }

    /// 读取表单，执行 `wslc run`。
    ///
    /// **强制后台运行**（`-d`）：不带 `-d` 时 `wslc run` 会前台阻塞，
    /// 而我们的子进程有超时，超时后会把刚建好的容器连带杀掉。
    ///
    /// 需要 `window`：表单里那几个"预填了但没填值"的必需变量要**拦住**
    /// 并把焦点送到那一格（见下面的检查）。
    pub fn confirm_create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self.create_dialog.as_ref() else {
            return;
        };
        // 查重名要用现有容器名 —— 和预览读的是同一个函数，所以
        // "预览里写着 `--name nginx-2`" 和实际执行的名字**必然一致**。
        let existing = self.state.container_names();
        let spec = dialog.to_spec(&existing, cx);

        if spec.image.trim().is_empty() {
            self.state.notify(Toast::error("请先填写镜像引用"));
            cx.notify();
            return;
        }

        // 预填的必需变量（`MYSQL_ROOT_PASSWORD=` 这种）**还留着一个空值**时先拦住：
        // 它看着像填了，其实等于没填 —— 提交上去容器照样起不来（mysql 报的就是
        // "Database is uninitialized and password option is not specified"），
        // 用户还得再去翻一遍日志才知道。**空值等于没填**，就在这里点破。
        //
        // 只拦"我们预填的那个原样留着"这一种情况：用户把变量删掉、或换了同一组
        // 里另一个合法选项（`MYSQL_ALLOW_EMPTY_PASSWORD=yes`）都放行 —— 他有可能是
        // 故意复用一份已经初始化好的数据卷（那种情况下本来就不需要这些变量）。
        if let Some(key) = wslc_core::model::prefilled_env_left_empty(&spec.image, &spec.env) {
            let alternatives: Vec<&str> = wslc_core::model::image_preset(&spec.image)
                .map(|preset| {
                    preset
                        .required_any_env
                        .iter()
                        .copied()
                        .filter(|k| *k != key)
                        .collect()
                })
                .unwrap_or_default();
            let hint = if alternatives.is_empty() {
                String::new()
            } else {
                format!("（也可以改用同一组里的：{}）", alternatives.join(" / "))
            };
            self.state.notify(Toast::error(format!(
                "{key} 还没填值 —— 这个镜像第一次启动必须有它。请在「环境变量」那格里补上{hint}"
            )));

            // 焦点直接送到环境变量那格，省得用户自己找
            if let Some(dialog) = self.create_dialog.as_ref() {
                let handle = dialog.env.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
            }
            cx.notify();
            return;
        }

        if let Err(e) = spec.validate() {
            // 错误可能来自资源或网络参数，先展开再让用户修改，避免提示指向隐藏字段。
            self.state.set_section_open(UiSection::CreateAdvanced, true);
            self.state.notify(Toast::error(format!("参数有误：{e}")));
            cx.notify();
            return;
        }

        // ⚠️ 镜像**自己不带命令**（`Cmd` 是空的）、而「启动命令」也留空 ——
        // 建出来的容器会"起来就跑完"。实测 `eclipse-temurin:8-jre-alpine`：
        // `wslc start` 提示成功，但 `StartedAt` 与 `FinishedAt` 相隔 **1.1 ms**、
        // 退出码 0、logs 为空（那个镜像只有 `ENTRYPOINT=/__cacert_entrypoint.sh`，
        // 末行 `exec "$@"`，不带参数等于什么都不做）—— 界面上只看到
        // "点了启动还是已退出"，反复点都一样（`docs/PLAN-v0.9.md` §11 / §16）。
        //
        // 和上面那个"预填了变量名却没填值"同一类：**空值等于没配**，在这里点破。
        // 只有**确定**读到了"这个镜像没有 Cmd"才拦 —— 还没读到 / 读失败时放行
        // （不能因为一次 inspect 失败就不让人建容器）。
        //
        // ⚠️ `spec.command` 是**拼好的 argv**（`state::create_command`）：留空时是
        // 空表（那正是"不给命令、用镜像自己的"），所以判空用 `is_empty()`。
        if spec.command.is_empty() {
            let has_cmd = match self.create_dialog.as_ref().map(|dialog| &dialog.facts) {
                Some(FactsState::Ready(facts)) => Some(facts.has_cmd),
                _ => None,
            };
            if has_cmd == Some(false) {
                self.state.notify(Toast::error(
                    "这个镜像自己没有默认命令（只有 ENTRYPOINT）——「启动命令」留空的话，\
                     容器起来就会立刻退出。填一条：想让它先待着等部署，填 \
                     `tail -f /dev/null`；想直接跑应用，填你的启动命令。",
                ));
                // 焦点送到那一格，省得用户自己找
                if let Some(dialog) = self.create_dialog.as_ref() {
                    let handle = dialog.command.read(cx).focus_handle(cx);
                    window.focus(&handle, cx);
                }
                cx.notify();
                return;
            }
        }

        let command = format!("wslc {}", spec.to_args().join(" "));
        tracing::info!("创建容器：{command}");

        self.create_dialog = None;
        self.state
            .notify(Toast::info("正在创建容器…（若需拉取镜像可能要几分钟）"));
        cx.notify();

        let wslc = self.state.wslc.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { wslc_core::cmd::container::run(&wslc, &spec) })
                .await;

            let _ = this.update_in(cx, |shell, window, cx| {
                match result {
                    Ok(id) => {
                        let id = id.trim().to_owned();
                        // ⚠️ `get(..12)` 而不是 `&id[..12]`：`len()` 是**字节**数，
                        // 而 `id` 是子进程的原始输出（被包装脚本改成中文、或混进
                        // 一行畸形文本都会踩到），第 12 个字节落在多字节字符中间
                        // 就是一次 panic —— 而且发生在 `update_in` 的回调里，
                        // 直接把界面拖掉。`model/container.rs` 的 `short_id`
                        // 为同一个理由就是这么写的，那边有一整段说明。
                        let short = id.get(..12).unwrap_or(&id);
                        shell
                            .state
                            .notify(Toast::success(format!("容器已创建：{short}")));
                        // 建完直接跳到「容器」页，让用户看到结果。
                        // 用 `set_page`（而不是 `goto_page`）：这一页有搜索框，
                        // 必须在有 window 的地方建；`update_in` 正好把 window 带回来了。
                        shell.set_page(Page::Containers, window, cx);
                    }
                    Err(e) => shell.state.notify(Toast::error(format!("创建失败：{e}"))),
                }
                shell.refresh(cx);
                cx.notify();
            });
        })
        .detach();
    }

    // -- 容器详情弹窗 --------------------------------------------------------

    /// 打开容器详情弹窗。
    ///
    /// 点列表里的名字进来。详情里放的是**列表行没有**的动作
    /// （强杀 / 执行命令 / 打开终端 / attach 主进程）+ 完整信息
    /// （端口含绑定地址、挂载、CPU / 内存 / 网络 I/O / PID）。
    ///
    /// ⚠️ 别把列表行已有的动作（启动 / 重启 / 停止 / 日志 / 删除）再搬进来 ——
    /// 同一件事两个入口，用户还得判断点哪个。见 `container_detail_overlay`。
    ///
    /// # 打开时顺手拉一次 `wslc inspect`
    ///
    /// 详情里要显示的**挂载 / 启动命令 / 环境变量**，列表那一份数据里
    /// 一样都没有（挂载只有卷名、命令被截断到 20 字符、环境变量完全没有），
    /// 见 [`DetailInspect`]。inspect 是一次几百毫秒的子进程，所以照
    /// `AGENTS.md` §7.1 走后台，先画"正在读取…"。
    pub fn open_detail(&mut self, name: String, cx: &mut Context<Self>) {
        self.detail = Some(name.clone());
        self.state.detail_inspect = Some(DetailInspect::loading(name.clone()));
        cx.notify();
        self.load_detail_inspect(name, cx);
    }

    /// 后台拉一次详情用的 `inspect`。
    ///
    /// 它**不是**"详情自己的数据源"：端口 / CPU / 内存 / PID 仍然来自列表快照
    /// （那些值每轮刷新都在动），inspect 只补三样静态信息。
    pub(crate) fn load_detail_inspect(&mut self, name: String, cx: &mut Context<Self>) {
        let wslc = self.state.wslc.clone();
        cx.spawn(async move |this, cx| {
            let result = {
                let wslc = wslc.clone();
                let container = name.clone();
                cx.background_executor()
                    .spawn(async move {
                        wslc_core::cmd::container::inspect(&wslc, &container, false)
                            .map_err(|e| e.to_string())
                    })
                    .await
            };
            let _ = this.update(cx, |shell, cx| {
                // 用户可能点了 A、没等回来又点 B，或者干脆关掉了详情 ——
                // 这时候**不能**把 A 的数据写进去（会画到 B 的详情里）。
                if shell.detail.as_deref() != Some(name.as_str()) {
                    return;
                }
                if let Some(detail) = shell.state.detail_inspect.as_mut() {
                    if detail.container == name {
                        match result {
                            Ok(inspect) => detail.inspect = Some(inspect),
                            Err(error) => detail.error = Some(error),
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 关闭容器详情弹窗。
    pub fn close_detail(&mut self, cx: &mut Context<Self>) {
        self.detail = None;
        // 那份 inspect 跟着一起扔：留着的话下次打开**别的**容器会先闪一下旧数据
        //（写回时的名字比对能挡住"写错对象"，但挡不住"先把旧的画出来"）。
        self.state.detail_inspect = None;
        cx.notify();
    }

    // -- 容器日志浮层 --------------------------------------------------------

    /// 打开容器日志浮层，并立刻拉一次。
    ///
    /// 停止的容器也能看日志（`wslc logs` 读的是它留下的日志），
    /// 所以列表里那个按钮不做"运行中才显示"的限制。
    pub fn open_logs(&mut self, name: String, cx: &mut Context<Self>) {
        self.state.logs = Some(LogsState::new(name));
        cx.notify();
        self.reload_logs(cx);
    }

    /// 关闭日志浮层。
    ///
    /// 在飞的请求不用管：回来时 `AppState::logs` 已经是 `None`，
    /// 那一支直接丢掉结果（见 [`Shell::reload_logs`]）。
    pub fn close_logs(&mut self, cx: &mut Context<Self>) {
        self.state.logs = None;
        cx.notify();
    }

    /// 换行数档位并重拉。
    pub fn set_logs_tail(&mut self, tail: Option<u32>, cx: &mut Context<Self>) {
        if let Some(logs) = self.state.logs.as_mut() {
            logs.tail = tail;
        }
        cx.notify();
        self.reload_logs(cx);
    }

    /// 设置是否显示时间戳，并重拉。
    ///
    /// 收目标值而不是自己取反：界面上那个是受控 `Switch`，
    /// 它回调给的就是用户请求的新状态。
    pub fn set_logs_timestamps(&mut self, on: bool, cx: &mut Context<Self>) {
        if let Some(logs) = self.state.logs.as_mut() {
            logs.timestamps = on;
        }
        cx.notify();
        self.reload_logs(cx);
    }

    /// 后台拉一次日志。
    ///
    /// `wslc logs` 是**同步等子进程**的，所以必须走后台执行器
    /// （`AGENTS.md` §7.1）—— 大容器的日志可能要读好几秒。
    ///
    /// 结果用**票据**认领（[`LogsState::begin_load`]）：连点两个档位时
    /// 两个请求都在飞，先发的可能后回，不认票据就会显示上一次选择的内容。
    pub fn reload_logs(&mut self, cx: &mut Context<Self>) {
        let Some(logs) = self.state.logs.as_mut() else {
            return;
        };
        let container = logs.container.clone();
        let options = logs.options();
        let ticket = logs.begin_load();
        cx.notify();

        let wslc = self.state.wslc.clone();
        cx.spawn(async move |this, cx| {
            let target = container.clone();
            // 给异步任务一份克隆：`options` 本身在下面还要用来比对
            // （"回来时档位还是不是当初那一档"），不能移进去。
            let for_call = options.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    wslc_core::cmd::container::logs(&wslc, &target, &for_call)
                        .map_err(|e| e.to_string())
                })
                .await;

            let _ = this.update(cx, |shell, cx| {
                // 浮层可能已经被关掉、或者换成了别的容器 / 别的档位 ——
                // 那就丢掉这份结果（`finish_load` 自己还会再认一次票据，
                // 这里多判一次是为了盖住"关了又开、`load_seq` 从 1 重新数"
                // 那种票据撞车的情况）。
                if let Some(logs) = shell.state.logs.as_mut() {
                    if logs.container == container && logs.options() == options {
                        logs.finish_load(ticket, result);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    // -- 容器 exec 浮层 ------------------------------------------------------

    /// 打开 exec 浮层（要 `&mut Window` 才能建输入框）。
    pub fn open_exec(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("ls -la /  （可以用管道、重定向、&&）")
        });
        let handle = input.read(cx).focus_handle(cx);
        window.focus(&handle, cx);

        self.state.exec = Some(ExecState::new(name));
        self.exec_input = Some(input);
        cx.notify();
    }

    /// 关闭 exec 浮层，输入框一起丢掉。
    ///
    /// 刻意**不保留**上次的命令：这个浮层的用途是"临时跑一条看看"，
    /// 留着上一条会让下次打开时手一抖又跑一遍。
    pub fn close_exec(&mut self, cx: &mut Context<Self>) {
        self.state.exec = None;
        self.exec_input = None;
        cx.notify();
    }

    /// 执行输入框里那条命令，结果填回浮层。
    ///
    /// 命令走 `/bin/sh -c`（见 [`exec_argv`] 的说明：管道 / 重定向 / `&&`
    /// 都要能用，而且 `/bin/sh` 是 POSIX 保证存在的）。
    ///
    /// **非交互**：`wslc exec` 不带 `-it`，所以 `vi`、`top` 这类要终端的
    /// 程序在这里没法用 —— 那是「打开终端」那个按钮的事。
    pub fn run_exec(&mut self, cx: &mut Context<Self>) {
        let Some(exec) = self.state.exec.as_ref() else {
            return;
        };
        // 正在跑就不再起一条：按钮那边也已经换成"执行中…"了，
        // 但连点两下仍然可能挤进两次事件。
        if exec.running {
            return;
        }
        let Some(input) = self.exec_input.as_ref() else {
            return;
        };

        let command = input.read(cx).value().trim().to_owned();
        if command.is_empty() {
            self.state.notify(Toast::error("请先填写要执行的命令"));
            cx.notify();
            return;
        }

        let container = exec.container.clone();
        let argv = exec_argv(&command);

        if let Some(exec) = self.state.exec.as_mut() {
            exec.running = true;
            exec.error = None;
            exec.last_command = command;
        }
        cx.notify();

        let wslc = self.state.wslc.clone();
        cx.spawn(async move |this, cx| {
            let target = container.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    wslc_core::cmd::container::exec(&wslc, &target, &argv)
                        .map_err(|e| e.to_string())
                })
                .await;

            let _ = this.update(cx, |shell, cx| {
                if let Some(exec) = shell.state.exec.as_mut() {
                    if exec.container == container {
                        exec.running = false;
                        match result {
                            Ok(out) => {
                                exec.output = Some(out);
                                exec.error = None;
                            }
                            Err(e) => exec.error = Some(e),
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    // -- 容器里的数据库管理（MySQL / MariaDB，v0.7.0）------------------------
    //
    // 这一整块都遵守同一条规矩：**起进程的东西一律在后台执行器上**
    // （`AGENTS.md` §7.1）。`wslc exec` 一次几百毫秒，放在界面线程上点两下
    // 窗口就会「未响应」。

    /// 在新终端窗口里打开容器的交互式 shell。
    ///
    /// **同步**：`spawn_in_new_console` 只把窗口拉起来就返回，不等它关闭
    /// （用户可能在里面待几个小时）—— `AGENTS.md` §7.1 允许这类同步调用。
    ///
    /// 用 `/bin/sh` 而不是 `/bin/bash`：POSIX 保证有 `sh`，而 alpine
    /// 这类镜像里**没有** bash —— 用 bash 会让一部分容器点下去就报错。
    /// 需要 bash 的话进去再敲（提示条里也这么说）。
    pub fn open_container_shell(&mut self, name: String, cx: &mut Context<Self>) {
        let wslc = self.state.wslc.clone();
        // 容器里的起点：只有"自定义路径"且是绝对路径时才有值 ——
        // 容器没有"Linux home"，`~` 交给镜像自己的 WORKDIR
        // （见 `TerminalStartDir::container_workdir`）。
        let workdir = self
            .state
            .prefs
            .terminal_dir
            .container_workdir()
            .map(str::to_owned);
        let toast = match wslc_core::cmd::container::open_interactive_shell(
            &wslc,
            &name,
            "/bin/sh",
            workdir.as_deref(),
        ) {
            Ok(()) => Toast::success(format!(
                "已打开 {name} 的终端（/bin/sh —— 镜像里有 bash 的话进去敲 bash 就行）"
            )),
            Err(e) => Toast::error(format!("打开终端失败：{e}")),
        };
        self.state.notify(toast);
        cx.notify();
    }

    /// 在新终端窗口里 attach 到容器主进程。
    ///
    /// ⚠️ 和「打开终端」的区别：`attach` 接的是容器**主进程**的
    /// stdin/stdout。主进程不是交互式的（比如 `nginx`）时，接上去什么
    /// 也敲不了 —— 那个窗口只用来**看**它的输出。这一点写进按钮的说明里。
    pub fn attach_container(&mut self, name: String, cx: &mut Context<Self>) {
        let wslc = self.state.wslc.clone();
        let toast = match wslc_core::cmd::container::attach_in_new_console(&wslc, &name) {
            Ok(()) => Toast::success(format!(
                "已在新窗口里 attach 到 {name}（接的是它的主进程，通常只能看输出）"
            )),
            Err(e) => Toast::error(format!("attach 失败：{e}")),
        };
        self.state.notify(toast);
        cx.notify();
    }

    /// 在新终端窗口里跟随容器日志。
    ///
    /// 行数 / 时间戳沿用日志浮层上刚选的那两个开关 —— 用户点这个按钮时
    /// 看到的就是他要的那份视图。浮层不在（理论上到不了）就用默认值。
    pub fn follow_container_logs(&mut self, name: String, cx: &mut Context<Self>) {
        let options = self
            .state
            .logs
            .as_ref()
            .filter(|l| l.container == name)
            .map(LogsState::options)
            .unwrap_or_default();

        let wslc = self.state.wslc.clone();
        let toast =
            match wslc_core::cmd::container::logs_follow_in_new_console(&wslc, &name, &options) {
                Ok(()) => Toast::info(format!(
                    "已在新窗口里跟随 {name} 的日志（关掉那个窗口就停止）"
                )),
                Err(e) => Toast::error(format!("打开跟随窗口失败：{e}")),
            };
        self.state.notify(toast);
        cx.notify();
    }
}
