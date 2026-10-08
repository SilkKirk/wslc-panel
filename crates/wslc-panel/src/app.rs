//! 应用外壳：窗口内容、左侧导航、异步刷新调度、危险操作确认。
//!
//! # 关于异步
//!
//! 所有 `wslc` 调用都是阻塞的（子进程 + 管道读取），**绝不能**跑在 UI 线程上。
//! 这里用 GPUI 的标准模式：
//!
//! ```text
//! cx.spawn(async move |this, cx| {
//!     let data = cx.background_executor().spawn(async move { 阻塞采集() }).await;
//!     this.update(cx, |state, cx| { 写回状态; cx.notify(); });
//! })
//! ```
//!
//! 这是整个项目里**唯一**接触 GPUI 异步 API 的地方，
//! 因此如果上游 API 有变动，只需要改这一个文件。

use std::sync::{Arc, Mutex};
use std::time::Duration;

// 注意：`primary()` / `danger()` 这些样式方法来自 trait `ButtonVariants`，
// 光导入 `Button` 是不够的 —— 这里用 glob 把 button 模块全带上。
use gpui_kit::component::button::*;
// `StyledExt` 提供 `font_bold` / `font_semibold` 等字重方法（由宏生成），
// 不导入 trait 就会报 "no method named font_bold"。
use gpui_kit::component::input::InputState;
// 同 views.rs：不导入 `Disableable`，全项目不再用 `.disabled()`。
use gpui_kit::component::{Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::*;

use wslc_core::Wslc;
use wslc_core::cmd::container::{PullPolicy, RunSpec};
use wslc_core::settings::SettingKey;

use crate::state::{self, AppState, ImmediateAction, Page, PendingAction, Toast, ToastKind};
use crate::theme;
use crate::views;

/// 「创建容器」弹窗的全部输入框。
///
/// 每个字段一个独立的 `InputState` —— 这是 GPUI 的标准做法。
/// 拉取策略用预设按钮（不是输入框），所以这里只存一个枚举值。
///
/// `pub(crate)` + 公开字段：渲染在 `views.rs` 里，需要逐个读出来画。
pub(crate) struct CreateDialog {
    pub(crate) image: Entity<InputState>,
    pub(crate) name: Entity<InputState>,
    pub(crate) ports: Entity<InputState>,
    pub(crate) env: Entity<InputState>,
    pub(crate) volumes: Entity<InputState>,
    pub(crate) network: Entity<InputState>,
    pub(crate) memory: Entity<InputState>,
    pub(crate) cpus: Entity<InputState>,
    pub(crate) pull: PullPolicy,
}

impl CreateDialog {
    /// 把表单读成一个 [`RunSpec`]。
    ///
    /// 需要 `cx` 才能从 `InputState` 里取值，所以它不是纯函数 ——
    /// 这也是 `views::page` 要多收一个 `cx` 的原因
    /// （弹窗底部要**实时**预览等效命令）。
    /// `pub(crate)`：`views.rs` 要用它来做**实时**等效命令预览。
    pub(crate) fn to_spec(&self, cx: &App) -> RunSpec {
        let text = |input: &Entity<InputState>| input.read(cx).value().trim().to_owned();

        let mut spec = RunSpec::new(text(&self.image));
        // 强制后台运行，理由见 `Shell::confirm_create` 的文档注释。
        spec.detach = true;
        spec.pull = self.pull;

        let name = text(&self.name);
        if !name.is_empty() {
            spec.name = Some(name);
        }

        spec.ports = split_list(&text(&self.ports));
        spec.volumes = split_list(&text(&self.volumes));

        // 环境变量按 `KEY=VALUE` 解析；没有等号的**直接丢掉**，
        // 不去猜用户想表达什么（猜错了反而更难查）。
        spec.env = split_list(&text(&self.env))
            .iter()
            .filter_map(|pair| pair.split_once('='))
            .map(|(key, value)| (key.trim().to_owned(), value.trim().to_owned()))
            .filter(|(key, _)| !key.is_empty())
            .collect();

        let network = text(&self.network);
        if !network.is_empty() {
            spec.network = Some(network);
        }
        let memory = text(&self.memory);
        if !memory.is_empty() {
            spec.memory = Some(memory);
        }
        let cpus = text(&self.cpus);
        if !cpus.is_empty() {
            spec.cpus = Some(cpus);
        }

        spec
    }
}

/// 按逗号（中英文）或换行切分，去掉空白项。
///
/// 刻意**不按空格切**：环境变量的值里完全可能有空格
/// （`MESSAGE=hello world`），按空格切会把它切成两条。
fn split_list(text: &str) -> Vec<String> {
    text.split([',', '，', '\n', '\r'])
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}

/// 应用外壳。
pub struct Shell {
    /// 全部状态。
    pub state: AppState,
    /// 「拉取镜像」弹窗的输入框；弹窗关闭时为 `None`。
    ///
    /// 放在 `Shell` 而**不是** `AppState`：`AppState` 刻意完全不碰 GPUI
    /// （换渲染层时数据层能原样复用），而 `InputState` 是 GPUI 的类型。
    ///
    /// 另外 `InputState::new` 需要一个 `&mut Window`，而 `Shell::new` 拿不到
    /// window —— 所以只能**懒创建**：用户点按钮时（事件回调里有 window）才建。
    pull_input: Option<Entity<InputState>>,
    /// 正在进行的拉取任务的取消令牌。
    ///
    /// 和 `pull_input` 一样放 `Shell`：这是**能力**（能 kill 子进程），
    /// 不是"状态"。纯数据部分（拉了哪个镜像、输出了什么）在
    /// `AppState::pulling` 里，因为 `views.rs` 只拿得到 `&AppState`。
    // 拉取策略。
    pull_cancel: Option<wslc_core::CancelToken>,
    /// 「创建容器」弹窗；关闭时为 `None`。
    create_dialog: Option<CreateDialog>,
    /// 正在查看详情的容器名；关闭时为 None。
    detail: Option<String>,
}

impl Shell {
    /// 创建外壳：立刻触发一次采集、加载配置、启动自动刷新。
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut shell = Self {
            state: AppState::new(Wslc::new()),
            pull_input: None,
            pull_cancel: None,
            create_dialog: None,
            detail: None,
        };
        shell.refresh(cx);
        shell.start_auto_refresh(cx);
        shell
    }

    // -- 拉取镜像弹窗 --------------------------------------------------------

    /// 打开「拉取镜像」弹窗，并把焦点交给输入框。
    pub fn open_pull_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(progress) = &self.state.pulling {
            let image = progress.image.clone();
            self.state
                .notify(Toast::error(format!("{image} 正在拉取中，请等它结束")));
            cx.notify();
            return;
        }

        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("docker.1ms.run/library/nginx:latest")
        });

        // 打开就把焦点给输入框，用户可以直接开始打字。
        let handle = input.read(cx).focus_handle(cx);
        window.focus(&handle, cx);

        self.pull_input = Some(input);
        cx.notify();
    }

    /// 关闭弹窗。
    ///
    /// 拉取进行中时**不允许关闭** —— 那会让人以为任务被取消了。
    /// 想停请用「取消拉取」。
    pub fn close_pull_dialog(&mut self, cx: &mut Context<Self>) {
        if self.state.pulling.is_some() {
            return;
        }
        self.pull_input = None;
        cx.notify();
    }

    /// 读取输入框内容，发起**流式** `wslc pull`。
    ///
    /// 弹窗保持打开并切到"进度模式"：实时显示输出最后若干行 + 「取消拉取」。
    /// 结束后自动关闭、弹提示、刷新列表。
    ///
    /// 为什么不用阻塞的 `cmd::image::pull`：那个要等进程结束才返回，
    /// 用户只能干看着转圈，也没法取消。
    pub fn confirm_pull(&mut self, cx: &mut Context<Self>) {
        let Some(input) = self.pull_input.clone() else {
            return;
        };
        // 注意：`InputState::value` **不带参数**（`pub fn value(&self) -> SharedString`，
        // gpui-base/src/input/base/state.rs:1257）。
        // gpui-component 里另有一个 `value(&self, cx)`，那是别的类型，别混。
        let reference = input.read(cx).value().trim().to_owned();

        if reference.is_empty() {
            self.state.notify(Toast::error("请先填写镜像引用"));
            cx.notify();
            return;
        }
        if self.state.pulling.is_some() {
            return;
        }

        // 读取线程不能直接碰 `AppState`，所以先把输出行塞进这个共享缓冲，
        // 由下面的异步任务定期搬进状态。
        //
        // 用 `Arc<Mutex<Vec<String>>>` 而不是 `mpsc::Sender`：
        // 回调要求 `Send + Sync`，而 `Sender` 的 `Sync` 实现随版本变化，
        // 共享缓冲没有这个不确定性。
        let buffer: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&buffer);

        let wslc = self.state.wslc.clone();
        let handle = match wslc_core::cmd::image::pull_streaming(&wslc, &reference, move |line| {
            if let Ok(mut pending) = sink.lock() {
                // 兜底防爆：读取线程可能远快于界面轮询。
                if pending.len() < 2000 {
                    pending.push(line.to_owned());
                }
            }
        }) {
            Ok(handle) => handle,
            Err(e) => {
                self.state
                    .notify(Toast::error(format!("无法启动拉取：{e}")));
                cx.notify();
                return;
            }
        };

        self.pull_cancel = Some(handle.cancel_token());
        self.state.pulling = Some(state::PullProgress::new(reference.clone()));
        self.state
            .notify(Toast::info(format!("开始拉取 {reference}")));
        cx.notify();

        cx.spawn(async move |this, cx| {
            let code = loop {
                // 1) 把这一轮攒下的输出搬进状态
                let batch: Vec<String> = {
                    let mut pending = buffer.lock().unwrap_or_else(|e| e.into_inner());
                    std::mem::take(&mut *pending)
                };
                if !batch.is_empty() {
                    let _ = this.update(cx, |shell, cx| {
                        if let Some(progress) = shell.state.pulling.as_mut() {
                            // 没有新增就不重绘
                            if progress.push_lines(batch) {
                                cx.notify();
                            }
                        }
                    });
                }

                // 2) 子进程结束了吗（非阻塞）
                match handle.try_wait() {
                    Ok(Some(code)) => break code,
                    Ok(None) => {}
                    Err(e) => {
                        tracing::warn!("检查拉取进程失败：{e}");
                        break -1;
                    }
                }

                cx.background_executor()
                    .timer(Duration::from_millis(200))
                    .await;
            };

            let cancelled = handle.was_cancelled();
            // 进程已退出，这里只是等读取线程把剩余输出读完。
            let _ = handle.finish();

            let _ = this.update(cx, |shell, cx| {
                // 最后一行留作失败时的原因说明
                let last_line = shell
                    .state
                    .pulling
                    .as_ref()
                    .and_then(|p| p.last_line())
                    .unwrap_or_default()
                    .to_owned();

                shell.state.pulling = None;
                shell.pull_cancel = None;

                if cancelled {
                    shell
                        .state
                        .notify(Toast::info(format!("{reference} 已取消")));
                } else if code == 0 {
                    shell
                        .state
                        .notify(Toast::success(format!("{reference} 拉取完成")));
                } else {
                    let detail = if last_line.is_empty() {
                        format!("退出码 {code}")
                    } else {
                        last_line
                    };
                    shell
                        .state
                        .notify(Toast::error(format!("{reference} 拉取失败：{detail}")));
                }

                // 取消不刷新（用户明确不想继续）；失败也刷一下，
                // 因为可能已经拉下来一部分层。
                if !cancelled {
                    shell.refresh(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 取消正在进行的拉取（kill 子进程）。
    pub fn cancel_pull(&mut self, cx: &mut Context<Self>) {
        match &self.pull_cancel {
            Some(token) => {
                token.cancel();
                tracing::info!("已请求取消拉取");
                self.state.notify(Toast::info("正在取消…"));
            }
            None => self.state.notify(Toast::error("当前没有正在进行的拉取")),
        }
        cx.notify();
    }

    // -- 容器：启动 / 重启（不需要二次确认）--------------------------------

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
        }
    }

    /// 启动/重启的公共实现：后台跑一条 `wslc <verb> <name>`，完了刷新。
    ///
    /// 用函数指针而不是闭包泛型：`start` 和 `restart` 签名一致，
    /// 函数指针省掉一层泛型参数，编译器也更容易推断。
    fn spawn_container_action(
        &mut self,
        verb: &'static str,
        name: String,
        action: fn(&Wslc, &[String]) -> wslc_core::Result<Vec<String>>,
        cx: &mut Context<Self>,
    ) {
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

    /// 打开「创建容器」弹窗。
    ///
    /// 和拉取弹窗一样是**懒创建**：`InputState::new` 要 `&mut Window`，
    /// 而 `Shell::new` 拿不到 window。
    pub fn open_create_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dialog = CreateDialog {
            image: cx.new(|cx| {
                InputState::new(window, cx).placeholder("docker.1ms.run/library/nginx:latest")
            }),
            name: cx.new(|cx| InputState::new(window, cx).placeholder("留空则自动命名")),
            ports: cx.new(|cx| InputState::new(window, cx).placeholder("8080:80, 9090:90")),
            env: cx.new(|cx| InputState::new(window, cx).placeholder("TZ=Asia/Shanghai")),
            volumes: cx
                .new(|cx| InputState::new(window, cx).placeholder("webdata:/usr/share/nginx/html")),
            network: cx.new(|cx| InputState::new(window, cx).placeholder("留空则用 bridge")),
            memory: cx.new(|cx| InputState::new(window, cx).placeholder("512M")),
            cpus: cx.new(|cx| InputState::new(window, cx).placeholder("0.5")),
            pull: PullPolicy::Never,
        };

        // 焦点给第一个必填字段
        let handle = dialog.image.read(cx).focus_handle(cx);
        window.focus(&handle, cx);

        self.create_dialog = Some(dialog);
        cx.notify();
    }

    /// 关闭「创建容器」弹窗。
    pub fn close_create_dialog(&mut self, cx: &mut Context<Self>) {
        self.create_dialog = None;
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
    pub fn confirm_create(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.create_dialog.as_ref() else {
            return;
        };
        let spec = dialog.to_spec(cx);

        if spec.image.trim().is_empty() {
            self.state.notify(Toast::error("请先填写镜像引用"));
            cx.notify();
            return;
        }
        if let Err(e) = spec.validate() {
            self.state.notify(Toast::error(format!("参数有误：{e}")));
            cx.notify();
            return;
        }

        let command = format!("wslc {}", spec.to_args().join(" "));
        tracing::info!("创建容器：{command}");

        self.create_dialog = None;
        self.state
            .notify(Toast::info("正在创建容器…（若需拉取镜像可能要几分钟）"));
        cx.notify();

        let wslc = self.state.wslc.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { wslc_core::cmd::container::run(&wslc, &spec) })
                .await;

            let _ = this.update(cx, |shell, cx| {
                match result {
                    Ok(id) => {
                        let id = id.trim().to_owned();
                        let short = if id.len() > 12 { &id[..12] } else { &id };
                        shell
                            .state
                            .notify(Toast::success(format!("容器已创建：{short}")));
                        // 建完直接跳到「当前运行」，让用户看到结果
                        shell.set_page(Page::Containers, cx);
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
    /// 照 1Panel：列表里不放操作按钮，点名字开这里。
    pub fn open_detail(&mut self, name: String, cx: &mut Context<Self>) {
        self.detail = Some(name);
        cx.notify();
    }

    /// 关闭容器详情弹窗。
    pub fn close_detail(&mut self, cx: &mut Context<Self>) {
        self.detail = None;
        cx.notify();
    }

    // -- 数据刷新 ----------------------------------------------------------

    /// 后台采集一次完整快照。
    ///
    /// 重复调用会被忽略（`busy` 保护），避免自动刷新和手动刷新叠加。
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.state.busy {
            return;
        }
        self.state.busy = true;
        cx.notify();

        let wslc = self.state.wslc.clone();
        cx.spawn(async move |this, cx| {
            let snapshot = cx
                .background_executor()
                .spawn(async move { state::load_snapshot(&wslc) })
                .await;

            let _ = this.update(cx, |shell, cx| {
                shell.state.busy = false;
                // 耗时只写日志，不在界面上显示。
                tracing::debug!(
                    "采集完成：{} ms，{} 个容器，{} 处错误",
                    snapshot.elapsed_ms,
                    snapshot.all.len(),
                    snapshot.errors.len()
                );
                shell.state.snapshot = snapshot;
                // 首次拿到 `wslc info` 之后才能确定 settings.yaml 的真实位置。
                // 只加载一次，避免把用户没保存的编辑覆盖掉。
                if shell.state.settings.is_none() {
                    shell.load_settings(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 按固定间隔自动刷新。
    ///
    /// 间隔由偏好（`prefs.refresh_secs`，默认 3 秒）决定，用户可以在"设置"页改；
    /// 每轮都重新读一次，所以改完立即生效，不需要重启。
    fn start_auto_refresh(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                let interval = match this.update(cx, |shell, _| shell.state.refresh_interval()) {
                    Ok(interval) => interval,
                    // 实体已销毁 → 退出循环，避免泄漏。
                    Err(_) => break,
                };

                cx.background_executor().timer(interval).await;

                if this.update(cx, |shell, cx| shell.refresh(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    // -- 配置 --------------------------------------------------------------

    /// 加载 `settings.yaml`（优先使用 `wslc info` 报告的路径）。
    pub fn load_settings(&mut self, cx: &mut Context<Self>) {
        let info = self.state.snapshot.info.clone();
        match state::load_settings(info.as_ref()) {
            Ok(doc) => {
                self.state.settings = Some(doc);
                self.state.settings_error = None;
            }
            Err(e) => {
                self.state.settings = None;
                self.state.settings_error = Some(e);
            }
        }
        cx.notify();
    }

    /// 用户主动重新加载（会丢弃未保存的修改）。
    pub fn reload_settings(&mut self, cx: &mut Context<Self>) {
        self.load_settings(cx);
        self.state.notify(Toast::info("已重新加载 settings.yaml"));
        cx.notify();
    }

    /// 修改一项配置（只改内存中的文档，需要保存才落盘）。
    pub fn set_setting(
        &mut self,
        key: &'static SettingKey,
        value: Option<&'static str>,
        cx: &mut Context<Self>,
    ) {
        let Some(doc) = self.state.settings.as_mut() else {
            self.state.notify(Toast::error("配置文件尚未加载"));
            cx.notify();
            return;
        };

        if doc.set(key.section, key.key, value) {
            let shown = value.unwrap_or("默认值");
            self.state
                .notify(Toast::info(format!("{} → {shown}（记得保存）", key.label)));
        }
        cx.notify();
    }

    /// 备份并保存 `settings.yaml`。
    pub fn save_settings(&mut self, cx: &mut Context<Self>) {
        let result = match self.state.settings.as_mut() {
            Some(doc) => doc.save_with_backup().map_err(|e| e.to_string()),
            None => {
                self.state.notify(Toast::error("配置文件尚未加载"));
                cx.notify();
                return;
            }
        };

        let toast = match result {
            Ok(Some(backup)) => Toast::success(format!(
                "已保存，备份：{}",
                backup
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            )),
            Ok(None) => Toast::success("已保存"),
            Err(e) => Toast::error(format!("保存失败：{e}")),
        };
        self.state.notify(toast);
        cx.notify();
    }

    /// 用系统默认编辑器打开配置文件（`wslc settings`）。
    pub fn open_settings_in_editor(&mut self, cx: &mut Context<Self>) {
        let wslc = self.state.wslc.clone();
        let toast = match wslc_core::cmd::system::open_settings_in_editor(&wslc) {
            Ok(()) => Toast::success("已用系统默认编辑器打开 settings.yaml"),
            Err(e) => Toast::error(format!("打开失败：{e}")),
        };
        self.state.notify(toast);
        cx.notify();
    }

    // -- 危险操作确认 ------------------------------------------------------

    /// 请求执行一个危险操作（先弹确认框）。
    pub fn request(&mut self, action: PendingAction, cx: &mut Context<Self>) {
        self.state.request_confirm(action);
        cx.notify();
    }

    /// 取消确认。
    pub fn cancel_pending(&mut self, cx: &mut Context<Self>) {
        self.state.cancel_confirm();
        cx.notify();
    }

    /// 确认并执行。
    pub fn confirm_pending(&mut self, cx: &mut Context<Self>) {
        let Some(action) = self.state.confirm.take() else {
            return;
        };

        let wslc = self.state.wslc.clone();
        let toast = match action.execute(&wslc) {
            Ok(message) => Toast::success(message),
            Err(e) => Toast::error(format!("{}失败：{e}", action.title())),
        };
        self.state.notify(toast);
        cx.notify();
        // 立即刷新，让列表反映最新状态。
        self.refresh(cx);
    }

    // -- 界面状态 ----------------------------------------------------------

    /// 切换页面。
    pub fn set_page(&mut self, page: Page, cx: &mut Context<Self>) {
        if self.state.page != page {
            self.state.page = page;
            cx.notify();
        }
    }

    /// 设置自动刷新间隔（秒），并立即写入偏好文件。
    ///
    /// 自动刷新循环每轮都会重新读 `state.prefs`，所以改完下一轮就生效，
    /// 不需要重启，也不需要通知循环。
    pub fn set_refresh_secs(&mut self, secs: u64, cx: &mut Context<Self>) {
        let prefs = crate::prefs::Prefs { refresh_secs: secs }.normalized();
        if self.state.prefs == prefs {
            return;
        }
        let secs = prefs.refresh_secs;
        self.state.prefs = prefs;

        match self.state.prefs.save() {
            Ok(()) => {
                tracing::info!("自动刷新间隔已改为 {secs} 秒");
                self.state
                    .notify(Toast::success(format!("刷新间隔已改为 {secs} 秒")));
            }
            Err(e) => {
                tracing::warn!("保存偏好失败：{e}");
                // 内存里的值仍然生效，只是重启后会丢。
                self.state
                    .notify(Toast::error(format!("已改为 {secs} 秒，但保存失败：{e}")));
            }
        }
        cx.notify();
    }

    /// 在资源管理器里定位会话存储。
    ///
    /// **只读操作** —— v0.2 刻意不允许修改 `storagePath`：
    /// 改了不会迁移已有容器/镜像，还会在原地留下旧数据并新建一个空会话。
    pub fn reveal_storage(&mut self, cx: &mut Context<Self>) {
        let Some(storage) = self.state.snapshot.storage.clone() else {
            self.state.notify(Toast::error("还没有解析出存储位置"));
            cx.notify();
            return;
        };

        // 优先选中 VHD 文件本身；它还不存在就退到目录。
        let target = storage
            .vhd
            .clone()
            .filter(|p| p.is_file())
            .or_else(|| {
                storage
                    .sessions_dir
                    .is_dir()
                    .then(|| storage.sessions_dir.clone())
            })
            .or_else(|| storage.base.is_dir().then(|| storage.base.clone()));

        let Some(target) = target else {
            self.state.notify(Toast::error(format!(
                "路径还不存在：{}",
                storage.base.display()
            )));
            cx.notify();
            return;
        };

        let is_file = target.is_file();
        let mut command = std::process::Command::new("explorer.exe");
        if is_file {
            // 注意：`/select,` 后面**不能有空格**，否则资源管理器会把整串当路径。
            command.arg(format!("/select,{}", target.display()));
        } else {
            command.arg(&target);
        }

        // `explorer.exe` 即使成功也常返回非 0，所以这里只看能否启动成功。
        match command.spawn() {
            Ok(_) => {
                tracing::info!("已在资源管理器中打开 {}", target.display());
                self.state
                    .notify(Toast::info(format!("已打开 {}", target.display())));
            }
            Err(e) => {
                tracing::warn!("打开资源管理器失败：{e}");
                self.state.notify(Toast::error(format!("打开失败：{e}")));
            }
        }
        cx.notify();
    }

    /// 关闭提示条。
    pub fn dismiss_toast(&mut self, cx: &mut Context<Self>) {
        self.state.toast = None;
        cx.notify();
    }
}

// ---------------------------------------------------------------------------
// 渲染
// ---------------------------------------------------------------------------

impl Render for Shell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let state = &self.state;

        // 导航按 `Page::group()` 分组，组名变化时插一条小标题。
        let mut nav: Vec<AnyElement> = Vec::new();
        let mut last_group = "";
        for page in Page::ALL {
            let group = page.group();
            if group != last_group {
                if !last_group.is_empty() {
                    nav.push(div().h(px(6.)).into_any_element());
                }
                nav.push(
                    div()
                        .px_3()
                        .pt_2()
                        .pb_1()
                        .text_xs()
                        .font_semibold()
                        .text_color(theme::text_dim())
                        .child(group)
                        .into_any_element(),
                );
                last_group = group;
            }
            nav.push(nav_item(page, state.page, &entity));
        }

        // 副标题只显示当前会话。刷新耗时/间隔属于实现细节，
        // 不进界面（耗时写日志，间隔在"设置"页里改）。
        let subtitle = format!("会话 {}", state.session_label());

        let refresh_button = {
            let entity = entity.clone();
            let busy = state.busy;
            let label = if busy { "刷新中…" } else { "刷新" };
            // 刻意**不**用 `.disabled(busy)`：禁用态的文字几乎看不清
            // （实机截图确认过）。`refresh()` 内部本来就有 `busy` 守卫，
            // 重复点击是无害的，文案也会变成"刷新中…"。
            Button::new("refresh")
                .label(label)
                .primary()
                .on_click(move |_, _, cx| {
                    entity.update(cx, |shell, cx| shell.refresh(cx));
                })
        };

        let error_banner: AnyElement = if !state.snapshot.errors.is_empty() {
            v_flex()
                .w_full()
                .gap_1()
                .p_3()
                .rounded_md()
                .bg(theme::danger_soft())
                .border_1()
                .border_color(theme::danger_edge())
                .children(
                    state
                        .snapshot
                        .errors
                        .iter()
                        .map(|e| div().text_xs().text_color(theme::danger()).child(e.clone())),
                )
                .into_any_element()
        } else if !state.snapshot.has_data() {
            // 首屏 / 连不上 wslc 时的提示。比一片空白有用得多。
            let hint = if state.busy {
                "正在读取 wslc 数据…"
            } else {
                "没有读到任何数据。请确认已安装 WSL 3.0 以上版本；\
                 若 wslc.exe 不在默认路径，请设置环境变量 WSLC_PATH 指向它。"
            };
            div()
                .w_full()
                .text_sm()
                .text_color(theme::text_dim())
                .child(hint)
                .into_any_element()
        } else {
            div().into_any_element()
        };

        let toast: AnyElement = match &state.toast {
            None => div().into_any_element(),
            Some(t) => {
                let entity = entity.clone();
                let color = match t.kind {
                    ToastKind::Info => theme::primary(),
                    ToastKind::Success => theme::success(),
                    ToastKind::Error => theme::danger(),
                };
                div()
                    .absolute()
                    .bottom(px(20.))
                    .right(px(20.))
                    .child(
                        h_flex()
                            .max_w(px(560.))
                            .gap_3()
                            .px_4()
                            .py_3()
                            .rounded_md()
                            .bg(theme::bg_card())
                            .border_1()
                            .border_color(color)
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(color)
                                    .overflow_hidden()
                                    .child(t.text.clone()),
                            )
                            .child(Button::new("toast-dismiss").label("关闭").small().on_click(
                                move |_, _, cx| {
                                    entity.update(cx, |shell, cx| shell.dismiss_toast(cx));
                                },
                            )),
                    )
                    .into_any_element()
            }
        };

        let confirm: AnyElement = match &state.confirm {
            None => div().into_any_element(),
            Some(action) => confirm_overlay(action, &entity),
        };

        // 拉取镜像弹窗。`pull_input` 是 `Shell` 的字段，不在 `state` 里。
        let pull_dialog: AnyElement = match &self.pull_input {
            None => div().into_any_element(),
            Some(input) => views::pull_dialog_overlay(input, state, &entity),
        };

        // 创建容器弹窗。要读 8 个 InputState 的值来做等效命令预览，
        // 所以得把 `cx` 传下去。
        let create_dialog: AnyElement = match &self.create_dialog {
            None => div().into_any_element(),
            Some(dialog) => views::create_dialog_overlay(dialog, &entity, cx),
        };

        // 容器详情弹窗。
        let detail_dialog: AnyElement = match &self.detail {
            None => div().into_any_element(),
            Some(name) => views::container_detail_overlay(name, state, &entity),
        };

        let page_body = views::page(state, &entity);

        div()
            .relative()
            .size_full()
            .bg(theme::bg())
            .text_color(theme::text())
            .child(
                div()
                    .flex()
                    .flex_row()
                    .size_full()
                    // 侧边栏
                    .child(
                        v_flex()
                            .w(px(216.))
                            .h_full()
                            .flex_none()
                            .gap_1()
                            .p_3()
                            .bg(theme::bg_sidebar())
                            .border_r_1()
                            .border_color(theme::border())
                            .child(
                                h_flex()
                                    .w_full()
                                    .gap_2()
                                    .px_3()
                                    .py_3()
                                    .child(
                                        div()
                                            .text_lg()
                                            .font_bold()
                                            .text_color(theme::primary())
                                            .child("wslc"),
                                    )
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(theme::text_dim())
                                            .child("panel"),
                                    ),
                            )
                            .children(nav),
                    )
                    // 主区域
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .overflow_hidden()
                            .child(
                                h_flex()
                                    .w_full()
                                    .flex_none()
                                    .justify_between()
                                    .px_6()
                                    .py_4()
                                    .border_b_1()
                                    .border_color(theme::border())
                                    .child(
                                        v_flex()
                                            .gap_1()
                                            .child(
                                                div()
                                                    .text_lg()
                                                    .font_bold()
                                                    .child(state.page.label()),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(theme::text_dim())
                                                    .child(subtitle),
                                            ),
                                    )
                                    .child(refresh_button),
                            )
                            .child(
                                // `overflow_y_scroll` 来自 `StatefulInteractiveElement`，
                                // 只对**带 id 的**元素可用（这正是它叫 "stateful" 的原因）。
                                // 不带 `.id()` 会报 "no method named overflow_y_scroll"。
                                v_flex()
                                    .id("app-body-scroll")
                                    .w_full()
                                    .flex_1()
                                    .min_h_0()
                                    .gap_4()
                                    .p_5()
                                    .overflow_y_scroll()
                                    .child(error_banner)
                                    .child(page_body),
                            ),
                    ),
            )
            .child(toast)
            .child(confirm)
            .child(pull_dialog)
            .child(create_dialog)
            .child(detail_dialog)
    }
}

/// 左侧导航项。
fn nav_item(page: Page, current: Page, entity: &Entity<Shell>) -> AnyElement {
    let entity = entity.clone();
    let active = page == current;

    let mut item = h_flex()
        .w_full()
        .id(nav_id(page))
        .gap_2()
        .px_3()
        .py_2()
        .rounded_md()
        .cursor_pointer()
        .child(div().text_sm().child(page.label()))
        .on_click(move |_, _, cx| {
            entity.update(cx, |shell, cx| shell.set_page(page, cx));
        });

    item = if active {
        item.bg(theme::bg_selected()).text_color(theme::primary())
    } else {
        item.text_color(theme::text_muted())
    };

    item.into_any_element()
}

/// 导航项的稳定 ID。
///
/// 用 `&'static str` 而不是格式化出来的 `String`：
/// GPUI 的交互元素要求 ID 稳定，静态字符串最不容易出错。
fn nav_id(page: Page) -> &'static str {
    match page {
        Page::Dashboard => "nav-dashboard",
        Page::Containers => "nav-containers",
        Page::Images => "nav-images",
        Page::Networks => "nav-networks",
        Page::Volumes => "nav-volumes",
        Page::Config => "nav-config",
    }
}

/// 危险操作的确认浮层。
fn confirm_overlay(action: &PendingAction, entity: &Entity<Shell>) -> AnyElement {
    let cancel = {
        let entity = entity.clone();
        Button::new("confirm-cancel")
            .label("取消")
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.cancel_pending(cx));
            })
    };

    let confirm = {
        let entity = entity.clone();
        Button::new("confirm-ok")
            .label(action.confirm_label())
            .primary()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.confirm_pending(cx));
            })
    };

    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(theme::scrim())
        .child(
            v_flex()
                .w(px(460.))
                .gap_4()
                .p_5()
                .rounded_lg()
                .bg(theme::bg_card())
                .border_1()
                .border_color(theme::danger().opacity(0.5))
                .child(
                    div()
                        .text_lg()
                        .font_bold()
                        .text_color(theme::danger())
                        .child(action.title()),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(theme::text_muted())
                        .child(action.body()),
                )
                .child(
                    h_flex()
                        .w_full()
                        .justify_end()
                        .gap_2()
                        .child(cancel)
                        .child(confirm),
                ),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    // ⚠️ 同 views.rs：这里**不能**写 `use super::*;`。
    //
    // `app.rs` 里有 `use gpui_kit::*;`，而 gpui 在 gpui.rs 里无条件
    // 再导出了 gpui_macros 的 `test` **属性宏** —— 一旦 `use super::*`，
    // 本模块的 `#[test]` 会解析到那个宏而不是内建的，展开时自我递归，
    // 报 `recursion limit reached while expanding #[test]`。
    use super::split_list;

    #[test]
    fn split_list_handles_commas_and_newlines() {
        assert_eq!(split_list("8080:80, 9090:90"), vec!["8080:80", "9090:90"]);
        assert_eq!(split_list("a\nb\r\nc"), vec!["a", "b", "c"]);
        // 中文逗号也认 —— 用户从中文文档里复制粘贴很常见
        assert_eq!(split_list("a，b"), vec!["a", "b"]);
    }

    #[test]
    fn split_list_drops_empty_items() {
        assert!(split_list("").is_empty());
        assert!(split_list("  ,  , \n ").is_empty());
        assert_eq!(split_list(" , a , "), vec!["a"]);
    }

    #[test]
    fn split_list_does_not_split_on_spaces() {
        // 环境变量的值里完全可能有空格，按空格切会把它切成两条
        assert_eq!(
            split_list("MESSAGE=hello world"),
            vec!["MESSAGE=hello world"]
        );
    }
}
