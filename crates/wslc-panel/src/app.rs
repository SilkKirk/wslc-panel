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

use wslc_core::cmd::container::{PullPolicy, RunSpec};
use wslc_core::cmd::distro::{InstallSource, InstallSpec};
use wslc_core::settings::SettingKey;
// `Wsl` 是发行版（实例）的调用器，和容器的 `Wslc` 并列。
use wslc_core::{Wsl, Wslc};

use crate::state::{
    self, AppState, ConfirmAction, DistroAction, ExportProgress, ImmediateAction,
    InstallSourceKind, Page, PendingAction, PromptKind, Toast, ToastKind,
};
use crate::theme;
// `split_list` / `refresh_label` 是纯函数，住在不依赖 GPUI 的 `wslc-panel-core` 里
// —— 这样它们的单测不必链接 GPUI（见那个 crate 的顶层说明）。
use crate::util::{refresh_label, split_list};
use crate::views;

/// 「添加实例」页的全部输入框与选项。
///
/// # 为什么它属于**页面**而不是弹窗
///
/// 这是本项目第一个"带输入框的页面"。和 [`CreateDialog`] 一样是
/// "每个字段一个 `InputState`"，但生命周期跟着页面走：
/// 进页面时**懒创建**（`InputState::new` 需要 `&mut Window`，
/// 只有点击导航那一刻才有），离开时**不销毁** ——
/// 用户很可能点错了又点回来，重建会把已经打进去的字清掉。
pub(crate) struct InstallForm {
    /// 发行版名。
    pub(crate) name: Entity<InputState>,
    /// 安装目录（在线安装可以留空）。
    pub(crate) install_dir: Entity<InputState>,
    /// 来源文件路径（tar / RootFS）。在线安装时用不到。
    pub(crate) source_path: Entity<InputState>,
    /// 选中的来源。
    pub(crate) source: InstallSourceKind,
    /// 装完是否启动（只有在线安装支持）。
    pub(crate) launch: bool,
    /// 装完是否设为默认。
    pub(crate) set_default: bool,
}

impl InstallForm {
    /// 把表单读成一个 [`InstallSpec`]。
    ///
    /// 需要 `cx` 才能从 `InputState` 里取值，所以它不是纯函数 ——
    /// 这也是 `views::page` 要多收一个 `&Shell` 的原因
    /// （页面底部要**实时**预览等效命令）。
    ///
    /// **没有版本这一项**：本项目只支持 WSL 2，装出来的固定是 WSL 2
    /// （见 `wslc_core::cmd::distro::WSL_VERSION`）。
    pub(crate) fn to_spec(&self, cx: &App) -> InstallSpec {
        let text = |input: &Entity<InputState>| input.read(cx).value().trim().to_owned();

        let source = match self.source {
            InstallSourceKind::Tar => InstallSource::Tar {
                path: text(&self.source_path),
            },
            InstallSourceKind::File => InstallSource::File {
                path: text(&self.source_path),
            },
            InstallSourceKind::Online => InstallSource::Online {
                launch: self.launch,
            },
        };

        let mut spec = InstallSpec::new(text(&self.name), source);
        spec.install_dir = text(&self.install_dir);
        spec.set_default = self.set_default;
        spec
    }
}

/// 「单输入框提示弹窗」—— 移动位置 / 调整大小 / 设置默认用户**共用**。
///
/// 这三个动作都是"给一个文本参数、跑一条 `wsl --manage`"，
/// 所以没必要做三个几乎一样的弹窗。
///
/// 和 [`CreateDialog`] 一样懒创建（`InputState::new` 要 `&mut Window`）。
/// 弹窗**本身就是确认**：里面带着说明和等效命令，提交后直接执行 ——
/// 连续弹两个窗比一个信息充分的窗更烦人。
pub(crate) struct TextPrompt {
    /// 要做什么。
    pub(crate) kind: PromptKind,
    /// 作用在哪个发行版上。
    pub(crate) distro: String,
    /// 唯一的输入框。
    pub(crate) input: Entity<InputState>,
}

/// 本面板正在"吊着"的一个发行版。
///
/// WSL 在最后一个活动会话结束约 20 秒后回收发行版。所谓"启动"，就是
/// **从 Windows 这边吊住一个 `wsl.exe` 不放** —— 见
/// [`wslc_core::cmd::distro::start`] 的说明和实测数据。
///
/// 句柄必须留着：它既是"这个发行版为什么还在跑"的凭据，
/// 也是判断哨兵还活不活着的唯一办法（`try_wait`）。
struct KeepAlive {
    name: String,
    child: std::process::Child,
}

impl KeepAlive {
    /// 哨兵还活着吗？
    ///
    /// `try_wait` 会**顺手回收**已经退出的子进程 —— 不调它的话，
    /// 退掉的子进程会以僵尸状态一直挂在系统里。
    fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

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
    /// 正在进行的**导出**任务的取消令牌。
    ///
    /// 和 `pull_cancel` 并列：两者都是长任务，都能取消，但一个走 `wslc.exe`、
    /// 一个走 `wsl.exe`，句柄类型也不同，合并只会让这个字段变成 `enum`。
    export_cancel: Option<wslc_core::CancelToken>,
    /// 是否正在等一个文件 / 目录选择器的结果。
    ///
    /// 用来**防止重复弹窗**：选择器没有超时（用户不点完它就一直开着），
    /// 连点两下「浏览…」会堆出两个对话框。见 `cmd::picker` 的说明。
    picking: bool,
    /// 本面板正在吊着的发行版（见 [`KeepAlive`]）。
    ///
    /// 界面只看到 `AppState::kept_alive` 里的名字；真正的进程句柄留在这里 ——
    /// 那是"能力"，不是界面状态。
    keep_alive: Vec<KeepAlive>,
    /// 「发行版配置（`/etc/wsl.conf`）」弹窗的输入框；关闭时为 `None`。
    wslconf_dialog: Option<WslConfDialog>,
    /// 「创建容器」弹窗；关闭时为 `None`。
    create_dialog: Option<CreateDialog>,
    /// 正在查看详情的容器名；关闭时为 None。
    detail: Option<String>,
    /// 正在查看详情的**发行版名**；关闭时为 None。
    ///
    /// 和容器的 `detail` 分成两个字段（而不是共用一个）：
    /// 两者的详情弹窗内容完全不同，共用一个字符串还得额外判断
    /// "这个名字是容器还是发行版"。
    distro_detail: Option<String>,
    /// 「添加实例」页的表单。
    ///
    /// 和 `pull_input` / `create_dialog` 一样**懒创建**（`InputState::new`
    /// 要 `&mut Window`），但**不随页面离开而销毁** —— 见 [`InstallForm`]。
    ///
    /// `pub(crate)`：`views.rs` 要读它来渲染页面（`Shell` 的其余字段
    /// 只有 `app.rs` 自己用，所以是私有的）。
    pub(crate) install_form: Option<InstallForm>,
    /// 单输入框提示弹窗（移动位置 / 调整大小 / 设置默认用户）；关闭时为 `None`。
    ///
    /// 同样是 `pub(crate)`：浮层在 `views.rs` 里渲染。
    pub(crate) prompt: Option<TextPrompt>,
    /// 采集期间又有刷新请求进来；跑完要补一次。
    refresh_again: bool,
    /// 当前这轮采集是否**由用户发起**（点按钮 / 操作完成后补刷）。
    ///
    /// 决定顶部按钮要不要显示「刷新中…」。3 秒一次的自动刷新是**后台**行为：
    /// 它每轮都会把 `state.busy` 置起来（并发守卫要用），但按钮不该跟着闪 ——
    /// 否则按钮几乎永远停在「刷新中…」上（用户看到的"按钮一直是刷新中"）。
    refresh_visible: bool,
    /// 已经跑过多少轮刷新。
    ///
    /// 用来把 `wsl --status` 降到 30 秒一次（见 [`Shell::status_every_n_ticks`]）。
    /// 放在 `Shell` 而不是 `AppState`：这是**调度细节**，不是界面要展示的状态。
    status_tick: u32,
}

/// `wsl --status` 的采集周期（秒）。
///
/// 它每轮都跑没必要：默认发行版 / 默认版本极少变，而且它的输出是**本地化**的
/// （中文系统是「默认分发:」），解析成本比列表高。
///
/// 列表（`wsl --list --verbose`）仍然跟随自动刷新 —— 状态变化要看得到。
const DISTRO_STATUS_SECS: u64 = 30;

impl Shell {
    /// 创建外壳：立刻触发一次采集、加载配置、启动自动刷新。
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut shell = Self {
            state: AppState::new(Wslc::new(), Wsl::new()),
            pull_input: None,
            pull_cancel: None,
            export_cancel: None,
            picking: false,
            keep_alive: Vec::new(),
            wslconf_dialog: None,
            create_dialog: None,
            detail: None,
            distro_detail: None,
            install_form: None,
            prompt: None,
            refresh_again: false,
            refresh_visible: false,
            status_tick: 0,
        };
        shell.refresh(cx);
        shell.start_auto_refresh(cx);
        shell
    }

    /// 建「添加实例」表单（幂等）。
    ///
    /// **必须**在有 `&mut Window` 的地方调用 —— `InputState::new` 要它。
    fn ensure_install_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.install_form.is_some() {
            return;
        }

        let default_source = InstallSourceKind::default();
        self.install_form = Some(InstallForm {
            name: cx.new(|cx| {
                InputState::new(window, cx).placeholder(default_source.name_placeholder())
            }),
            install_dir: cx.new(|cx| InputState::new(window, cx).placeholder(r"D:\wsl\MyDistro")),
            source_path: cx.new(|cx| {
                InputState::new(window, cx).placeholder(default_source.path_placeholder())
            }),
            source: default_source,
            launch: false,
            set_default: false,
        });
    }

    // -- 单输入框提示弹窗（移动位置 / 调整大小 / 设置默认用户）---------------

    /// 打开提示弹窗，并把焦点交给输入框。
    pub fn open_prompt(
        &mut self,
        kind: PromptKind,
        distro: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(kind.placeholder()));

        // 打开就把焦点给输入框，用户可以直接开始打字。
        let handle = input.read(cx).focus_handle(cx);
        window.focus(&handle, cx);

        self.prompt = Some(TextPrompt {
            kind,
            distro,
            input,
        });
        cx.notify();
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

        // 导出是**长任务**（要进度条、要能取消），走的是完全不同的一条路 ——
        // 它和上面三个只是共用了"让用户填一个文本参数"这个外形。
        if kind == PromptKind::ExportDistro {
            self.prompt = None;
            self.start_export(distro, value, cx);
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

    /// 隔多少轮刷新采一次 `wsl --status`。
    ///
    /// **按秒换算而不是写死轮数**：刷新间隔是用户可以改的（1~600 秒），
    /// 写死"每 10 轮"在间隔改成 30 秒时就变成 5 分钟一次了。
    fn status_every_n_ticks(&self) -> u32 {
        let secs = self.state.prefs.refresh_secs.max(1);
        u32::try_from(DISTRO_STATUS_SECS.div_ceil(secs))
            .unwrap_or(u32::MAX)
            .max(1)
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

    /// 取消正在进行的导出（kill 子进程）。
    ///
    /// ⚠️ 取消会留下一个**不完整**的 tar —— 提示里要说明，
    /// 免得用户把它当成一个能用的备份。
    pub fn cancel_export(&mut self, cx: &mut Context<Self>) {
        match &self.export_cancel {
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
            self.state.notify(Toast::error("正在拉取镜像，等它结束再导出"));
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
        self.export_cancel = Some(handle.cancel_token());
        cx.notify();

        let probe = std::path::PathBuf::from(&path);
        let target = path;
        let started = std::time::Instant::now();

        cx.spawn(async move |this, cx| {
            let mut handle = handle;
            let code = loop {
                // 1) 目标文件长到多大了（`metadata` 是微秒级，不会卡界面）
                let written = std::fs::metadata(&probe).ok().map(|m| m.len());
                let secs = started.elapsed().as_secs();
                let line = last.lock().ok().map(|s| s.clone()).unwrap_or_default();

                let _ = this.update(cx, |shell, cx| {
                    if let Some(progress) = shell.state.exporting.as_mut() {
                        // 只在这两个数字真的变了才重绘 ——
                        // 每 500 ms 无脑重绘一遍是浪费。
                        let changed =
                            progress.written != written || progress.elapsed_secs != secs;
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
                shell.export_cancel = None;

                if cancelled {
                    shell.state.notify(Toast::info(format!(
                        "已取消导出 —— {target} 是个**不完整**的文件，需要自己删掉"
                    )));
                } else if code == 0 {
                    let size = std::fs::metadata(&target)
                        .ok()
                        .map(|m| state::format_bytes(m.len()))
                        .unwrap_or_else(|| "大小未知".to_owned());
                    shell
                        .state
                        .notify(Toast::success(format!("{name} 已导出到 {target}（{size}）")));
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
        }
    }

    // -- WSL 发行版（实例）动作 --------------------------------------------

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
    fn spawn_distro_action(
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

        self.state
            .notify(Toast::info(format!("正在启动 {name}…")));
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
                        shell.keep_alive.push(KeepAlive {
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
    fn sync_keep_alive(&mut self, cx: &mut Context<Self>) {
        self.keep_alive.retain_mut(|k| k.is_alive());

        let names: Vec<String> = self.keep_alive.iter().map(|k| k.name.clone()).collect();
        if self.state.kept_alive != names {
            self.state.kept_alive = names;
            cx.notify();
        }
    }

    /// 打开发行版的终端（新控制台窗口）。
    ///
    /// 这是**同步**的：`spawn_in_new_console` 只负责把窗口拉起来就返回，
    /// 不会等终端关闭（也不该等 —— 用户可能在里面待几个小时）。
    pub fn open_distro_terminal(&mut self, name: String, cx: &mut Context<Self>) {
        let wsl = self.state.wsl.clone();
        let toast = match wslc_core::cmd::distro::open_terminal(&wsl, &name) {
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
        self.spawn_distro_action(
            "设为默认",
            name,
            wslc_core::cmd::distro::set_default,
            cx,
        );
    }

    /// 在资源管理器里定位发行版的安装目录。
    ///
    /// 和 `reveal_storage` 一样是**只读**操作：只打开窗口，不动文件。
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
            self.state.notify(Toast::error(format!(
                "目录不存在：{}",
                base.display()
            )));
            cx.notify();
            return;
        }

        // `explorer.exe` 即使成功也常返回非 0，所以只看能否启动。
        match std::process::Command::new("explorer.exe").arg(&base).spawn() {
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
                        // 建完直接跳到「容器」页，让用户看到结果。
                        // 用 `goto_page` 而不是 `set_page`：这里没有 window，
                        // 而「容器」页也不需要表单。
                        shell.goto_page(Page::Containers, cx);
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

    /// 打开**发行版**详情弹窗。
    ///
    /// 低频但重要的动作（改版本 / 压缩 / 打开安装位置）都收在这里，
    /// 列表行里只留高频的那几个 —— 和容器页同一套取舍。
    pub fn open_distro_detail(&mut self, name: String, cx: &mut Context<Self>) {
        self.distro_detail = Some(name);
        cx.notify();
    }

    /// 关闭发行版详情弹窗。
    pub fn close_distro_detail(&mut self, cx: &mut Context<Self>) {
        self.distro_detail = None;
        cx.notify();
    }

    // -- 数据刷新 ----------------------------------------------------------

    /// 后台采集一次完整快照，并让顶部按钮显示「刷新中…」。
    ///
    /// 点按钮、以及操作（启动/停止/创建/删除）完成后的补刷都走这里。
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.refresh_inner(cx, true);
    }

    /// 后台采集一次完整快照，但**不动**顶部按钮的文案（静默刷新）。
    ///
    /// 3 秒一次的自动刷新走这条：它是后台行为，按钮该一直写着「刷新」；
    /// 用户想立刻看到最新数据时自己点按钮（那条走 [`Shell::refresh`]）。
    fn refresh_quiet(&mut self, cx: &mut Context<Self>) {
        self.refresh_inner(cx, false);
    }

    /// 采集的公共实现。`visible` 表示这一轮要不要点亮顶部按钮。
    ///
    /// 已经在采的时候**不会并发再开一轮**，但也不会丢掉这次请求：
    /// 记在 `refresh_again` 上，本轮结束后立刻补跑。
    ///
    /// 这一点很关键 —— 操作（启动/停止/创建/删除）完成后都会调它，
    /// 如果正好撞上 3 秒的自动刷新就把这次请求丢掉，
    /// 界面要等到下一个周期才变，用户会以为操作没生效。
    fn refresh_inner(&mut self, cx: &mut Context<Self>, visible: bool) {
        // 先顺手清掉已经死掉的哨兵（用户点了「终止」、或在别处 `wsl --shutdown`）。
        // 放在最前面：`busy` 那条提前返回也不该让界面上的"保持运行中"标记过期。
        self.sync_keep_alive(cx);

        if self.state.busy {
            // **不能丢**：正在跑的那轮采到的是操作**之前**的数据。
            // 丢掉这次请求，界面就要等下一个周期才变 ——
            // 用户看到的就是"点了停止，状态还是运行中"。
            self.refresh_again = true;
            // 用户点的按钮正好撞上后台那轮：立刻点亮按钮，免得"点了没反应"。
            // 补跑的那轮会一直亮到采完（见下面处理 `refresh_again` 的顺序）。
            if visible && !self.refresh_visible {
                self.refresh_visible = true;
                cx.notify();
            }
            return;
        }
        self.refresh_again = false;
        self.state.busy = true;
        self.refresh_visible = visible;
        cx.notify();

        // `wsl --status` 降频到 30 秒一次；其余（含发行版列表）跟随本轮刷新。
        let with_status = self.status_tick % self.status_every_n_ticks() == 0;
        self.status_tick = self.status_tick.wrapping_add(1);

        let wslc = self.state.wslc.clone();
        let wsl = self.state.wsl.clone();
        cx.spawn(async move |this, cx| {
            let (snapshot, status) = cx
                .background_executor()
                .spawn(async move {
                    let snapshot = state::load_snapshot(&wslc, &wsl);
                    // 同一轮里串行跑，避免并发起两个 wsl.exe
                    let status = with_status.then(|| state::load_distro_status(&wsl));
                    (snapshot, status)
                })
                .await;

            let _ = this.update(cx, |shell, cx| {
                shell.state.busy = false;
                shell.refresh_visible = false;
                // 耗时只写日志，不在界面上显示。
                tracing::debug!(
                    "采集完成：{} ms，{} 个容器，{} 个发行版，{} 处错误",
                    snapshot.elapsed_ms,
                    snapshot.all.len(),
                    snapshot.distros.len(),
                    snapshot.errors.len()
                );
                shell.state.snapshot = snapshot;

                // 30 秒一次的那部分：失败时**保留旧值**。
                // 一次瞬时失败不该让界面变成空白（那看起来像"数据丢了"）。
                if let Some(result) = status {
                    match result {
                        Ok(s) => {
                            shell.state.distro_status = Some(s);
                            shell.state.distro_status_error = None;
                        }
                        Err(e) => {
                            tracing::warn!("wsl --status 失败：{e}");
                            shell.state.distro_status_error = Some(e.to_string());
                        }
                    }
                }

                // 首次拿到 `wslc info` 之后才能确定 settings.yaml 的真实位置。
                // 只加载一次，避免把用户没保存的编辑覆盖掉。
                if shell.state.settings.is_none() {
                    shell.load_settings(cx);
                }

                // 本轮采集期间被挡下的刷新请求 → 立刻补跑。
                //
                // 放在 `cx.notify()` **之前**：补跑会把 `busy` 重新置起来，
                // 先通知的话会闪一帧「刷新」再变回「刷新中…」。
                if std::mem::take(&mut shell.refresh_again) {
                    shell.refresh(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 定时刷新（自动刷新用）。
    ///
    /// 与 [`Shell::refresh`] 的区别有两点：
    /// 1. **忙的时候直接跳过、不排队** —— 下一个周期自然会再来一次，堆着没有意义；
    /// 2. **静默** —— 不动顶部按钮的文案。3 秒一次的后台刷新不该让按钮
    ///    一直停在「刷新中…」上；用户想立刻刷新就自己点按钮。
    pub fn tick(&mut self, cx: &mut Context<Self>) {
        if self.state.busy {
            return;
        }
        self.refresh_quiet(cx);
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

                if this.update(cx, |shell, cx| shell.tick(cx)).is_err() {
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

    /// 用系统默认程序打开 `%USERPROFILE%\.wslconfig`。
    ///
    /// # 为什么走 `explorer.exe`
    ///
    /// 它按**文件关联**打开，用户装了 VS Code / Notepad++ 就会用那个 ——
    /// 比我们写死 `notepad.exe` 尊重用户的习惯。项目里"在资源管理器里打开"
    /// 也是这个路子。
    ///
    /// **只 `spawn` 不等待**：等编辑器关掉会把界面挂住（这正是 v0.3.4 修的那类问题）。
    pub fn open_wslconfig(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.state.snapshot.wslconfig.path.clone() else {
            self.state.notify(Toast::error(
                "找不到 .wslconfig 的位置（USERPROFILE 没定义？）",
            ));
            cx.notify();
            return;
        };

        // 文件不存在时**先建一个空的**：不然 explorer 会弹一个系统级的
        // "找不到文件"框，看起来像我们的错。建个空文件正是"从头配一份"的起点。
        let created = !path.exists();
        if created {
            if let Err(e) = std::fs::write(&path, "") {
                self.state
                    .notify(Toast::error(format!("创建 .wslconfig 失败：{e}")));
                cx.notify();
                return;
            }
        }

        let toast = match std::process::Command::new("explorer.exe")
            .arg(&path)
            .spawn()
        {
            Ok(_) if created => Toast::success("已创建空的 .wslconfig 并用系统默认程序打开"),
            Ok(_) => Toast::success("已用系统默认程序打开 .wslconfig"),
            Err(e) => Toast::error(format!("打开失败：{e}")),
        };
        self.state.notify(toast);
        cx.notify();
    }

    // -- 危险操作确认 ------------------------------------------------------

    /// 请求执行一个**容器域**危险操作（先弹确认框）。
    pub fn request(&mut self, action: PendingAction, cx: &mut Context<Self>) {
        self.state.request_confirm(action);
        cx.notify();
    }

    /// 请求执行一个**发行版域**危险操作（先弹确认框）。
    pub fn request_distro(&mut self, action: DistroAction, cx: &mut Context<Self>) {
        self.state.request_distro_confirm(action);
        cx.notify();
    }

    /// 取消确认。
    pub fn cancel_pending(&mut self, cx: &mut Context<Self>) {
        self.state.cancel_confirm();
        cx.notify();
    }

    /// 确认并执行。
    ///
    /// 两个域共用这一个入口：[`ConfirmAction::execute`] 内部按域分派到
    /// 各自的调用器（`wslc` / `wsl`）。
    ///
    /// ⚠️ **必须异步**：`wslc` / `wsl` 的调用都是同步等子进程的，
    /// 放在界面线程上会把窗口卡成"未响应"。容器那边的停止/删除还只是秒级，
    /// 发行版的压缩、移动是**分钟级** —— 同步跑的话界面能挂十分钟。
    pub fn confirm_pending(&mut self, cx: &mut Context<Self>) {
        let Some(action) = self.state.confirm.take() else {
            return;
        };

        // 发行版被删掉之后它的详情弹窗就没有对象了 —— 现在先判断好，
        // 待会儿 `action` 要移进异步块。
        let close_detail = match &action {
            ConfirmAction::Distro(DistroAction::Unregister { name, .. }) => {
                self.distro_detail.as_deref() == Some(name.as_str())
            }
            _ => false,
        };

        // 点下去先给个回应。慢动作（压缩/移动）要等很久，
        // 没有这条的话用户只会觉得按钮坏了，然后去点第二次。
        let title = action.title();
        self.state.notify(Toast::info(format!("正在{title}…")));
        cx.notify();

        let wslc = self.state.wslc.clone();
        let wsl = self.state.wsl.clone();

        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { action.execute(&wslc, &wsl) })
                .await;

            let _ = this.update(cx, |shell, cx| {
                match result {
                    Ok(message) => shell.state.notify(Toast::success(message)),
                    Err(e) => shell
                        .state
                        .notify(Toast::error(format!("{title}失败：{e}"))),
                }

                if close_detail {
                    shell.distro_detail = None;
                }

                // 立即刷新，让列表反映最新状态。
                shell.refresh(cx);
                cx.notify();
            });
        })
        .detach();
    }

    // -- 界面状态 ----------------------------------------------------------

    /// 切换页面（导航点击走这里）。
    ///
    /// `window` 是给「添加实例」页准备的：那一页有输入框，而
    /// `InputState::new` 需要 `&mut Window` —— 所以必须**在点击时**
    /// 把表单建好。在渲染时建会每帧重建一次输入框，字都打不进去。
    pub fn set_page(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        if page.needs_window_to_enter() {
            self.ensure_install_form(window, cx);
        }
        self.goto_page(page, cx);
    }

    /// 不碰表单的页面切换 —— 给**没有 window** 的场合用。
    ///
    /// 目前只有"容器创建完成后跳到「容器」页"这一处。
    fn goto_page(&mut self, page: Page, cx: &mut Context<Self>) {
        if self.state.page != page {
            self.state.page = page;
            cx.notify();
        }
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
    fn spawn_picker(
        &mut self,
        input: &Entity<InputState>,
        folders: bool,
        label: &'static str,
        extensions: &'static [&'static str],
        title: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.picking {
            self.state
                .notify(Toast::info("已经有一个选择框开着了，先处理它"));
            cx.notify();
            return;
        }
        self.picking = true;
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
                shell.picking = false;

                match picked {
                    Ok(Some(path)) => {
                        input.update(cx, |state, cx| state.set_value(path, window, cx));
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
            window,
            cx,
        );
    }

    /// 给「添加实例」页的**来源文件**输入框弹选择器。
    pub fn browse_install_path(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 后缀表写成常量而不是 `&["tar"][..]`：临时数组靠 rvalue 提升也能拿到
        // `'static`，但显式写出来不必让人去推这件事。
        const TAR_ONLY: &[&str] = &["tar"];
        const INSTALL_FILES: &[&str] = &["tar", "gz", "vhdx"];

        let picked = self.install_form.as_ref().and_then(|form| {
            // 后缀按来源给 —— 用户看到的就是"只列 tar"或者"只列安装文件"
            let (label, extensions) = match form.source {
                InstallSourceKind::Tar => ("tar 文件", TAR_ONLY),
                InstallSourceKind::File => ("安装文件", INSTALL_FILES),
                // 在线安装没有文件路径这一项
                InstallSourceKind::Online => return None,
            };
            Some((form.source_path.clone(), label, extensions))
        });

        let Some((input, label, extensions)) = picked else {
            return;
        };

        self.spawn_picker(
            &input,
            false,
            label,
            extensions,
            "选择安装文件",
            window,
            cx,
        );
    }

    // -- 发行版配置（/etc/wsl.conf）-----------------------------------------

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

                        shell.wslconf_dialog = Some(WslConfDialog { inputs });
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
        self.wslconf_dialog = None;
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
        let Some(dialog) = self.wslconf_dialog.as_ref() else {
            return;
        };
        let updates: Vec<(&'static str, &'static str, String)> =
            wslc_core::model::wslconf::FIELDS
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
            // 清空 = 恢复默认（保存时把那一行删掉）
            if value.is_empty() {
                state.doc.clear(section, key);
            } else {
                state.doc.set(section, key, value);
            }
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
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    // 2) 校验。写错 `[user] default` 会让发行版**下次启动直接失败**，
                    //    写错 `[boot] command` 会让它起不来 —— 都必须在写之前挡下。
                    let mut errors = Vec::new();
                    if let Some(user) = doc.get("user", "default") {
                        if !user.trim().is_empty()
                            && !wslc_core::cmd::distro::user_exists(&wsl, &target, user)?
                        {
                            errors.push(format!("发行版里没有用户「{user}」"));
                        }
                    }
                    if let Some(cmd) = doc.get("boot", "command") {
                        if !cmd.trim().is_empty()
                            && !wslc_core::cmd::distro::path_exists(&wsl, &target, cmd)?
                        {
                            errors.push(format!("找不到启动命令「{cmd}」"));
                        }
                    }
                    if !errors.is_empty() {
                        return Ok::<_, wslc_core::Error>((errors, false));
                    }

                    // 3) 写（`write_wsl_conf` 内部会先备份到 .bak）
                    wslc_core::cmd::distro::write_wsl_conf(&wsl, &target, &text)?;

                    // 4) 重启：`wsl.conf` 要重启才被读
                    if restart {
                        wslc_core::cmd::distro::terminate(&wsl, &target)?;
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
                    shell.wslconf_dialog = None;
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

    // -- 添加实例 ----------------------------------------------------------

    /// 切换安装来源。
    ///
    /// 顺带把**新来源用不到的选项复位** —— 否则用户在"在线安装"里勾了
    /// "装完启动"，再切到"从 tar 导入"，那个勾还留着但界面上看不见，
    /// 一旦切回去又冒出来，很像 bug。
    pub fn set_install_source(&mut self, source: InstallSourceKind, cx: &mut Context<Self>) {
        let Some(form) = self.install_form.as_mut() else {
            return;
        };
        if form.source == source {
            return;
        }

        form.source = source;
        if !source.supports_launch() {
            form.launch = false;
        }
        cx.notify();
    }

    /// 切换"装完启动"。
    pub fn toggle_install_launch(&mut self, cx: &mut Context<Self>) {
        if let Some(form) = self.install_form.as_mut() {
            form.launch = !form.launch;
            cx.notify();
        }
    }

    /// 切换"装完设为默认"。
    pub fn toggle_install_default(&mut self, cx: &mut Context<Self>) {
        if let Some(form) = self.install_form.as_mut() {
            form.set_default = !form.set_default;
            cx.notify();
        }
    }

    /// 按表单执行安装。
    ///
    /// 安装可能跑十几分钟到几十分钟（在线下载 / 铺开文件系统），
    /// 所以走后台执行器 + 完成后再提示；期间界面照常可用。
    pub fn confirm_install(&mut self, cx: &mut Context<Self>) {
        // 先把 spec 取出来、**结束对 `self` 的借用** —— 下面要 `&mut self`
        // 去发提示条，借用还活着的话编译器会拦。
        let spec = match self.install_form.as_ref() {
            Some(form) => form.to_spec(cx),
            None => {
                self.state.notify(Toast::error("表单尚未创建"));
                cx.notify();
                return;
            }
        };

        if let Err(e) = spec.validate() {
            self.state.notify(Toast::error(format!("参数有误：{e}")));
            cx.notify();
            return;
        }

        // 等效命令写进日志：出问题时能直接复制到终端复现。
        tracing::info!("安装发行版：{}", spec.preview_lines().join("  &&  "));

        self.state.notify(Toast::info(format!(
            "正在安装 {}…（可能要十几分钟，期间界面可以继续用）",
            spec.name.trim()
        )));
        cx.notify();

        let wsl = self.state.wsl.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { wslc_core::cmd::distro::install(&wsl, &spec) })
                .await;

            let _ = this.update(cx, |shell, cx| {
                match result {
                    Ok(message) => {
                        shell.state.notify(Toast::success(message));
                        // 装完跳到列表，让用户直接看到新实例
                        shell.goto_page(Page::Instances, cx);
                    }
                    Err(e) => shell
                        .state
                        .notify(Toast::error(format!("安装失败：{e}"))),
                }
                shell.refresh(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// 设置自动刷新间隔（秒），并立即写入偏好文件。
    ///
    /// 自动刷新循环每轮都会重新读 `state.prefs`，所以改完下一轮就生效，
    /// 不需要重启，也不需要通知循环。
    pub fn set_refresh_secs(&mut self, secs: u64, cx: &mut Context<Self>) {
        // ⚠️ 必须**带上当前的 theme**：`Prefs` 是要整体写回磁盘的，
        // 这里要是只填 `refresh_secs`，改一次刷新间隔就会把主题重置掉。
        let prefs = crate::prefs::Prefs {
            refresh_secs: secs,
            theme: self.state.prefs.theme,
        }
        .normalized();
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

    /// 记住界面主题（并写回偏好文件）。
    ///
    /// ⚠️ **这里不切换组件库的主题** —— 那一步必须在**点击回调里**做，
    /// 因为 `gpui_kit::component::Theme::change` 需要一个 `&mut App`，
    /// 而 `Context<Self>` 给不出来。
    ///
    /// 所以调用顺序是：点击回调先 `Theme::change(...)`，再调本方法持久化。
    /// 见 `views.rs` 的 `theme_card`。
    pub fn set_theme(&mut self, theme: crate::prefs::ThemePref, cx: &mut Context<Self>) {
        if self.state.prefs.theme == theme {
            return;
        }

        self.state.prefs.theme = theme;
        match self.state.prefs.save() {
            Ok(()) => {
                tracing::info!("主题已改为{}", theme.label());
                self.state
                    .notify(Toast::success(format!("主题已改为{}", theme.label())));
            }
            Err(e) => {
                tracing::warn!("保存偏好失败：{e}");
                // 内存里的值仍然生效，只是重启后会丢。
                self.state
                    .notify(Toast::error(format!("已改为{}，但保存失败：{e}", theme.label())));
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
        let subtitle = format!(
            "会话 {} · 构建 {}",
            state.session_label(),
            crate::short_build_sha()
        );

        // 按钮只在**用户发起**的采集期间显示「刷新中…」。
        // 3 秒一次的自动刷新是后台行为：它照样跑，但按钮一直是「刷新」，
        // 用户随时可以点它手动刷新（见 `refresh_quiet` / `refresh_inner`）。
        let user_refreshing = state.busy && self.refresh_visible;

        let refresh_button = {
            let entity = entity.clone();
            let label = refresh_label(state.busy, self.refresh_visible);
            // 刻意**不**用 `.disabled(...)`：禁用态的文字几乎看不清
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
            //
            // 用 `user_refreshing` 而不是 `state.busy`：连不上 wslc 时
            // 后台每 3 秒就会空跑一轮，用 `busy` 的话这两句话会一直闪。
            let hint = if user_refreshing {
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

        // 发行版详情弹窗。
        let distro_detail_dialog: AnyElement = match &self.distro_detail {
            None => div().into_any_element(),
            Some(name) => views::distro_detail_overlay(name, state, &entity),
        };

        // 单输入框提示弹窗（移动位置 / 调整大小 / 设置默认用户 / 导出）。
        // 要 `cx` 才能实时读输入框的值做等效命令预览。
        let prompt_dialog: AnyElement = views::prompt_overlay(self, &entity, cx);

        // 导出进度浮层（长任务，可取消）。
        let export_overlay: AnyElement = views::export_overlay(state, &entity);

        // 发行版配置（/etc/wsl.conf）弹窗。要 `cx` 才能读输入框的值。
        let wslconf_dialog: AnyElement = views::wslconf_overlay(self, &entity, cx);

        // 页面渲染要 `&Shell`（不只是 `&AppState`）——「添加实例」页有输入框，
        // 而输入框的 `InputState` 住在 `Shell` 里。
        // `cx` 也只有那一页用得上（要实时读输入框的值做命令预览）。
        let page_body = views::page(self, cx, &entity);

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
            .child(distro_detail_dialog)
            // 提示弹窗放最后：它可能是从发行版详情里打开的，
            // 后画的压在详情上面。
            .child(prompt_dialog)
            .child(export_overlay)
            .child(wslconf_dialog)
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
        .on_click(move |_, window, cx| {
            // `window` 在这里是必需的：「添加实例」页要现场创建输入框，
            // 而 `InputState::new` 需要 `&mut Window`。
            entity.update(cx, |shell, cx| shell.set_page(page, window, cx));
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
        Page::Instances => "nav-instances",
        Page::AddInstance => "nav-add-instance",
        Page::Containers => "nav-containers",
        Page::Images => "nav-images",
        Page::Networks => "nav-networks",
        Page::Volumes => "nav-volumes",
        Page::AppSettings => "nav-app-settings",
        Page::Config => "nav-config",
        Page::WslConfig => "nav-wsl-config",
    }
}

/// 危险操作的确认浮层。
///
/// 容器域和发行版域共用这一个浮层 —— 文案由 [`ConfirmAction`] 自己给，
/// 所以这里不需要知道是哪个域。
fn confirm_overlay(action: &ConfirmAction, entity: &Entity<Shell>) -> AnyElement {
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
