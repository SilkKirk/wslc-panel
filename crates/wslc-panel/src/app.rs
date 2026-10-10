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
use std::time::{Duration, Instant};

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
// 安装现在是**多步计划**（见 `wslc_core::model::install` 的模块说明）：
// `InstallSpec` 是用户填的东西，`plan()` 把它变成步骤，`run_plan` 把它做出来。
use wslc_core::cmd::distro::InstallSpec;
use wslc_core::cmd::install::{self, InstallCancel, InstallEvent, InstallSummary, RunOptions};
use wslc_core::mirrors;
use wslc_core::model::install::{
    self as install_model, InstallSource, derive_install_dir, suggest_name_from_file,
};
use wslc_core::settings::SettingKey;
// `Wsl` 是发行版（实例）的调用器，和容器的 `Wslc` 并列。
use wslc_core::{Wsl, Wslc};

use crate::state::{
    self, AppState, ConfirmAction, DistroAction, ExportProgress, ImmediateAction,
    InstallOutcome, InstallProgress, InstallSourceKind, MirrorChoice, MirrorProbeResult, Page,
    PendingAction, PromptKind, Toast, ToastKind,
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
    /// 来源文件路径（tar / vhdx / `.wsl`）。网络来源用不到。
    pub(crate) source_path: Entity<InputState>,
    /// 镜像站那块"自定义 rootfs URL"（留空 = 用探测出来的那个）。
    pub(crate) mirror_url: Entity<InputState>,
    /// 在线清单的搜索框。
    pub(crate) online_filter: Entity<InputState>,
    /// 在线清单里选中的发行版 id（`wsl --install -d` 要的就是它）。
    pub(crate) online_id: Option<String>,
    /// 选中的来源。
    pub(crate) source: InstallSourceKind,
    /// 装完是否启动（只有在线安装支持）。
    pub(crate) launch: bool,
    /// 装完是否设为默认。
    pub(crate) set_default: bool,
    /// 在线安装是否走 `--web-download`（从网络下，而不是微软商店）。
    ///
    /// 默认由"GitHub 通不通"决定（参考实现也这么探），但按钮在界面上，用户能改。
    pub(crate) web_download: bool,
    /// 用户**自己动过**那个开关吗。
    ///
    /// 探测是后台跑的（几秒后才有结果），期间用户可能已经手动切过了 ——
    /// 那时候不能再用探测结果去覆盖他的选择。
    pub(crate) web_download_touched: bool,
    /// **上一次由我们推导出来的**安装目录。
    ///
    /// 用来判断"用户是不是自己改过目录"：值等于它（或为空）说明这个框还是我们填的，
    /// 名字一变就可以跟着更新；否则绝不覆盖用户手输的东西。
    ///
    /// 这么绕是因为本仓库没有"输入框内容变化"的订阅先例（本机编译不了，
    /// 猜 API 要白烧一轮 CI）—— 用"比较上次推导值"能达到同样效果。
    pub(crate) last_derived_dir: Option<String>,
}

impl InstallForm {
    /// 把表单读成一个 [`InstallSpec`]。
    ///
    /// 需要 `cx` 才能从 `InputState` 里取值，所以它不是纯函数 ——
    /// 这也是 `views::page` 要多收一个 `&Shell` 的原因
    /// （页面要**实时**预览要跑哪些步骤）。
    ///
    /// **没有版本这一项**：本项目只支持 WSL 2，装出来的固定是 WSL 2
    /// （见 [`wslc_core::model::install::WSL_VERSION`]）。
    ///
    /// # 镜像站那一条为什么以输入框为准
    ///
    /// 「自定义 URL」填了就用它，**忽略**探测出来的那个 ——
    /// 用户手输地址就是在表达"别管你探测到什么"。这也让"镜像站上没有我要的版本"
    /// 有个出口，而不是只能换来源。
    pub(crate) fn to_spec(&self, cx: &App, mirrors: &crate::state::MirrorState) -> InstallSpec {
        let text = |input: &Entity<InputState>| input.read(cx).value().trim().to_owned();
        let typed_name = text(&self.name);

        let source = match self.source {
            InstallSourceKind::Tar => InstallSource::Tar {
                path: text(&self.source_path),
            },
            InstallSourceKind::Vhdx => InstallSource::Vhdx {
                path: text(&self.source_path),
            },
            InstallSourceKind::File => InstallSource::File {
                path: text(&self.source_path),
            },
            InstallSourceKind::Mirror => {
                let custom = text(&self.mirror_url);
                match (custom.is_empty(), mirrors.chosen.as_ref()) {
                    // 手填优先
                    (false, _) => InstallSource::Mirror {
                        url: custom,
                        mirror: "自定义 URL".to_owned(),
                        release: mirrors
                            .selected_distro()
                            .map(|d| d.release.to_owned())
                            .unwrap_or_default(),
                    },
                    // 否则用探测出来的那个；都还没有就给空 URL（校验会挡住并说明）
                    (true, Some(choice)) => InstallSource::Mirror {
                        url: choice.url.clone(),
                        mirror: choice.site.clone(),
                        release: choice.release.clone(),
                    },
                    (true, None) => InstallSource::Mirror {
                        url: String::new(),
                        mirror: String::new(),
                        release: mirrors
                            .selected_distro()
                            .map(|d| d.release.to_owned())
                            .unwrap_or_default(),
                    },
                }
            }
            InstallSourceKind::Online => InstallSource::Online {
                // 清单里选过就用它的 id（那才是 `wsl --install -d` 认的名字）；
                // 没选过（清单拉不到）就把用户手输的名字当 id ——
                // 这正是 v0.3 的那条老路：本机拉不到在线清单是常态。
                id: self
                    .online_id
                    .clone()
                    .filter(|id| !id.trim().is_empty())
                    .unwrap_or_else(|| typed_name.clone()),
                launch: self.launch,
                web_download: self.web_download,
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

/// 「发行版配置（`/etc/wsl.conf`）」弹窗的输入框。
///
/// 每个**文本**字段一个 `InputState`，按 `(节, 键)` 索引。
/// 布尔字段用勾选框（`gpui_kit::component::checkbox::Checkbox`），不需要输入框。
///
/// 键用字段表里的 `&'static str` —— 它们是 [`wslc_core::model::wslconf::FIELDS`]
/// 里的常量，所以不用 `String`，也就不会有拼写不一致的问题。
pub(crate) struct WslConfDialog {
    pub(crate) inputs: std::collections::HashMap<(&'static str, &'static str), Entity<InputState>>,
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

/// 一次正在跑的安装。
///
/// # 为什么要有它
///
/// 安装跑在**后台执行器**上（几分钟到几十分钟），它没法直接改界面状态；
/// 而界面要**边跑边显示**日志和进度。这里的三个字段就是那条通路：
/// 后台线程往 `events` 里塞事件，界面每 250 ms 取一次，
/// `done` 一旦有值就说明跑完了（执行器是**阻塞**的，所以结果只能这样交回来）。
///
/// 取消令牌单独存在 `Shell::install_cancel` 里 —— 那是"能力"（能杀子进程），
/// 和这份"状态"不是一回事。
struct InstallRun {
    /// 后台线程塞进来的事件（界面每轮取走）。
    events: Arc<Mutex<Vec<InstallEvent>>>,
    /// 执行结果；`None` = 还在跑。
    done: Arc<Mutex<Option<InstallSummary>>>,
    /// 整个安装是什么时候开始的（界面上显示"已用时"）。
    started: Instant,
}

/// 取锁；中毒（别的线程 panic 过）时照常用里面的值。
///
/// 这几个锁保护的都只是"事件队列"，中毒不影响数据本身的正确性 ——
/// 在这里 `unwrap()` 只会把后台线程的一次 panic 变成界面的一次 crash。
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// GitHub 通不通 —— 在线安装默认走 `--web-download` 还是微软商店。
///
/// 参考实现也是这么探的：GitHub 通的时候直接从网络下比走商店快，
/// 商店在国内还经常拉不动。
///
/// ⚠️ **阻塞**（起 `curl.exe`，最长 5 秒），只能在后台执行器上调用 ——
/// 见 [`Shell::probe_web_download`]。
fn github_reachable() -> bool {
    // `-o NUL` 丢掉正文，只留状态码；`-m 5` 是"别为一个默认值等太久"
    let args = [
        "-s",
        "-I",
        "-L",
        "-m",
        "5",
        "-o",
        "NUL",
        "-w",
        "%{http_code}",
        "https://github.com",
    ];
    match wslc_core::cli::run_helper_with_hint(
        "curl.exe",
        "curl.exe",
        wslc_core::cli::CURL_NOT_FOUND_HINT,
        &args,
        Duration::from_secs(10),
    ) {
        Ok(out) => out.stdout.trim() == "200",
        Err(e) => {
            tracing::info!("探测 GitHub 失败（那就默认走微软商店）：{e}");
            false
        }
    }
}

/// 安装目录里已经有东西了吗。
///
/// ⚠️ 只在**提交那一刻**调用：渲染每帧都可能发生，而 `read_dir` 碰上网络盘
/// 或者掉线的移动硬盘能卡很久 —— 那种卡顿看起来就像界面死了。
fn dir_non_empty(dir: &str) -> bool {
    let dir = dir.trim();
    if dir.is_empty() {
        return false;
    }
    match std::fs::read_dir(dir) {
        Ok(mut entries) => entries.next().is_some(),
        // 目录不存在 / 没权限 → 当作"不是非空"。
        // 真有问题的话，后面的 `create_dir_all` / `wsl` 会给一句更准确的错。
        Err(_) => false,
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
    ///
    /// `views::wslconf_overlay` 要读它来画 `Input`，所以是 `pub(crate)`。
    pub(crate) wslconf_dialog: Option<WslConfDialog>,
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
    /// 正在跑的安装（事件队列 + 结果）；空闲时为 `None`。
    ///
    /// 纯数据部分（走到第几步、日志、进度）在 `AppState::installing` 里 ——
    /// `views.rs` 只拿得到 `&AppState`。
    install_run: Option<InstallRun>,
    /// 正在跑的安装的取消令牌。
    ///
    /// 和 `export_cancel` 并列：都是长任务、都能取消，但一个走 `wsl.exe`
    /// （外加 `curl.exe`），另一个只有 `wsl --export`。
    install_cancel: Option<InstallCancel>,
    /// 探过"GitHub 通不通"了吗（在线安装的默认下载路径）。
    ///
    /// 只探一次：它决定的是一个**默认值**，每次进页面都起一个 curl 不值得。
    web_download_probed: bool,
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
            install_run: None,
            install_cancel: None,
            web_download_probed: false,
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
    ///
    /// 安装目录预填「默认安装目录」偏好（见 `prefs.rs`）：它同时是"由名字推路径"
    /// 的基准，用户不用每装一个都重新敲一遍盘符。
    fn ensure_install_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.install_form.is_some() {
            return;
        }

        let default_source = InstallSourceKind::default();
        let default_dir = self.state.prefs.install_dir.clone().unwrap_or_default();

        self.install_form = Some(InstallForm {
            name: cx.new(|cx| {
                InputState::new(window, cx).placeholder(default_source.name_placeholder())
            }),
            install_dir: cx.new(|cx| {
                InputState::new(window, cx).placeholder(r"D:\wsl\MyDistro")
            }),
            source_path: cx.new(|cx| {
                InputState::new(window, cx).placeholder(default_source.path_placeholder())
            }),
            mirror_url: cx.new(|cx| {
                InputState::new(window, cx).placeholder("留空 = 用上面探测出来的那个地址")
            }),
            online_filter: cx.new(|cx| InputState::new(window, cx).placeholder("搜索发行版")),
            online_id: None,
            source: default_source,
            launch: false,
            set_default: false,
            // 默认走微软商店；后台探到 GitHub 通会把它翻过来（见 `probe_web_download`）
            web_download: false,
            web_download_touched: false,
            last_derived_dir: if default_dir.is_empty() {
                None
            } else {
                Some(default_dir.clone())
            },
        });

        // 预填默认安装目录本身（"名字还没填"时它就是基准）
        if !default_dir.is_empty() {
            if let Some(form) = self.install_form.as_ref() {
                form.install_dir
                    .update(cx, |state, cx| state.set_value(default_dir, window, cx));
            }
        }
    }

    /// 按名字刷新安装目录（**只在用户没自己改过时**）。
    ///
    /// 判断依据是 [`InstallForm::last_derived_dir`]：输入框的值还等于我们上次推的
    /// （或者干脆是空的），就说明它是我们填的，可以跟着名字走；
    /// 否则一律不动 —— 用户手输了路径却被我们改掉，是最恼人的一类 bug。
    fn resync_install_dir(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let default_dir = self.state.prefs.install_dir.clone();
        let Some(form) = self.install_form.as_mut() else {
            return;
        };

        let name = form.name.read(cx).value().trim().to_owned();
        let current = form.install_dir.read(cx).value().trim().to_owned();
        let untouched =
            current.is_empty() || Some(current.as_str()) == form.last_derived_dir.as_deref();
        if !untouched {
            return;
        }

        let derived = derive_install_dir(default_dir.as_deref(), &name)
            .or_else(|| default_dir.clone().filter(|_| name.is_empty()));
        if let Some(dir) = derived {
            form.install_dir
                .update(cx, |state, cx| state.set_value(dir.clone(), window, cx));
            form.last_derived_dir = Some(dir);
        }
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
            // 在线安装默认走商店还是 GitHub —— 后台探一次（几秒），
            // 结果回来了把开关拨过去。**不在这里同步探**：那会卡住界面。
            self.probe_web_download(cx);
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
    // 参数确实多（输入框 / 目录还是文件 / 标题 / 扩展名 / 窗口 / 上下文……），
    // 但它们各自独立、没有天然的聚合体，硬凑个结构体只是把复杂度搬个地方。
    #[allow(clippy::too_many_arguments)]
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
    ///
    /// 选完做三件事（而不只是填个路径）：
    ///
    /// 1. 填路径；
    /// 2. 从**文件名**猜一个发行版名（`ubuntu-rootfs-amd64.tar.gz` → `ubuntu`）——
    ///    用户不必自己敲一遍；
    /// 3. 让安装目录跟着新名字走（见 [`Shell::resync_install_dir`]）。
    ///
    /// 后缀表按来源给：选"从 VHDX 导入"时不该看见一堆 `.tar`。
    pub fn browse_install_path(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((label, extensions)) = self.install_form.as_ref().and_then(|form| {
            let (label, extensions) = form.source.file_filter()?;
            Some((label, extensions))
        }) else {
            return;
        };

        if self.picking {
            self.state
                .notify(Toast::info("已经有一个选择框开着了，先处理它"));
            cx.notify();
            return;
        }
        self.picking = true;
        cx.notify();

        cx.spawn_in(window, async move |this, cx| {
            let picked = cx
                .background_executor()
                .spawn(async move { wslc_core::cmd::picker::pick_file(label, extensions) })
                .await;

            let _ = this.update_in(cx, |shell, window, cx| {
                shell.picking = false;

                match picked {
                    Ok(Some(path)) => {
                        // 先只做"填输入框"这类需要 `&mut Window` 的事，
                        // 借用在下面那个块结束时自然放开，才能再 `&mut shell`。
                        let suggested = {
                            let Some(form) = shell.install_form.as_ref() else {
                                cx.notify();
                                return;
                            };
                            form.source_path
                                .update(cx, |state, cx| state.set_value(path.clone(), window, cx));

                            let suggested = suggest_name_from_file(&path);
                            if !suggested.is_empty() {
                                form.name.update(cx, |state, cx| {
                                    state.set_value(suggested.clone(), window, cx)
                                });
                            }
                            suggested
                        };

                        if !suggested.is_empty() {
                            shell.resync_install_dir(window, cx);
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

    /// 切换「应用设置」页里的 tab。
    pub fn set_settings_tab(&mut self, tab: state::SettingsTab, cx: &mut Context<Self>) {
        self.state.settings_tab = tab;
        cx.notify();
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

    /// 切换在线安装的下载路径（微软商店 / `--web-download`）。
    ///
    /// 记下"用户动过它"，免得几秒后才回来的探测结果把他的选择覆盖掉。
    pub fn toggle_web_download(&mut self, cx: &mut Context<Self>) {
        if let Some(form) = self.install_form.as_mut() {
            form.web_download = !form.web_download;
            form.web_download_touched = true;
            cx.notify();
        }
    }

    /// 后台探一次"GitHub 通不通"，据此决定在线安装的默认下载路径。
    ///
    /// **必须后台探**：`curl` 最长 5 秒，放在点击回调里就是"点了「添加实例」
    /// 界面卡 5 秒"（`AGENTS.md` §7.1 那类 bug）。它决定的是一个**默认值**，
    /// 晚几百毫秒回来完全没关系 —— 用户能看到开关自己变过去。
    ///
    /// 只探一次（`web_download_probed`）：每次进页面都起一个 curl 不值得。
    fn probe_web_download(&mut self, cx: &mut Context<Self>) {
        if self.web_download_probed {
            return;
        }
        self.web_download_probed = true;

        cx.spawn(async move |this, cx| {
            let reachable = cx
                .background_executor()
                .spawn(async { github_reachable() })
                .await;

            let _ = this.update(cx, |shell, cx| {
                if let Some(form) = shell.install_form.as_mut() {
                    // 只在用户没动过那个开关时改
                    if !form.web_download_touched {
                        form.web_download = reachable;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 把当前安装目录记成"新实例默认安装目录"。
    ///
    /// 刻意**不做成设置页里的输入框**：那需要给设置页也引入"懒创建输入框"
    /// 那一套机制（见 [`InstallForm`] 的说明）。用户在安装页填好一个路径、
    /// 顺手点一下"存为默认"，是更短的路径。
    pub fn remember_install_dir(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.install_form.as_ref() else {
            return;
        };
        let dir = form.install_dir.read(cx).value().trim().to_owned();

        if dir.is_empty() {
            self.state
                .notify(Toast::error("先在「安装目录」里填一个绝对路径"));
            cx.notify();
            return;
        }
        if !install_model::is_absolute_windows_path(&dir) {
            self.state.notify(Toast::error(format!(
                "要记也得是绝对路径（如 D:\\wsl）：{dir}"
            )));
            cx.notify();
            return;
        }

        // ⚠️ `Prefs` 是**整体**写回磁盘的：这里必须带上刷新间隔与主题，
        // 只填 install_dir 会把它们重置掉。
        let prefs = crate::prefs::Prefs {
            refresh_secs: self.state.prefs.refresh_secs,
            theme: self.state.prefs.theme,
            install_dir: Some(dir.clone()),
        };
        self.state.prefs = prefs;
        match self.state.prefs.save() {
            Ok(()) => {
                tracing::info!("默认安装目录已记为 {dir}");
                self.state
                    .notify(Toast::success(format!("已记住：新实例默认装到 {dir}")));
            }
            Err(e) => {
                // 内存里的值仍然生效，只是重启后会丢
                self.state
                    .notify(Toast::error(format!("已记住 {dir}，但写盘失败：{e}")));
            }
        }
        cx.notify();
    }

    /// 选一个目录填进「安装目录」。
    ///
    /// 选完要把 [`InstallForm::last_derived_dir`] 清掉：那之后这个名字联动
    /// 就不能再动它了 —— 用户亲手选的目录被我们改掉是最恼人的一类 bug。
    pub fn browse_install_dir(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(input) = self.install_form.as_ref().map(|form| form.install_dir.clone()) else {
            return;
        };
        self.spawn_picker(&input, true, "目录", &[], "选择安装目录", window, cx);
        if let Some(form) = self.install_form.as_mut() {
            form.last_derived_dir = None;
        }
    }

    // -- 添加实例：在线清单与镜像站 ----------------------------------------

    /// 拉在线可安装发行版的清单。
    ///
    /// 先问 `wsl --list --online`，拉不到就自己拉微软那份
    /// `DistributionInfo.json`（本机 `raw.githubusercontent.com` 不通，
    /// 所以兜底那条路是**常态**，见 `cmd::distro::online_distros`）。
    pub fn refresh_online_list(&mut self, cx: &mut Context<Self>) {
        if self.state.online.loading {
            return;
        }
        self.state.online.loading = true;
        self.state.online.error.clear();
        cx.notify();

        let wsl = self.state.wsl.clone();
        cx.spawn(async move |this, cx| {
            let listing = cx
                .background_executor()
                .spawn(async move { wslc_core::cmd::distro::online_distros(&wsl) })
                .await;

            let _ = this.update(cx, |shell, cx| {
                shell.state.online.loading = false;
                shell.state.online.source = listing.source;
                if listing.items.is_empty() {
                    shell.state.online.clear();
                    shell.state.online.error = if listing.error.trim().is_empty() {
                        "没有拿到任何在线发行版".to_owned()
                    } else {
                        listing.error
                    };
                } else {
                    shell.state.online.error.clear();
                    shell.state.online.items = listing.items;
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 选中在线清单里的一项。
    ///
    /// # 名字默认填**清单里的 id**，而不是友好名
    ///
    /// 参考实现填的是友好名（`Ubuntu 24.04 LTS` → `Ubuntu-24-04-LTS`）。
    /// 那样每次在线安装都会走"导出 → 注销 → 导入"的重定位（多拷几个 GB），
    /// 因为它和 `wsl --install -d` 认的 id 对不上。
    /// 我们的默认值是 id（`Ubuntu-24.04`，本身就是合法名字），于是走
    /// `--install -d Ubuntu-24.04 --location <目录>` 这条快路；
    /// 用户想改名随时能改，界面上也会提示那会多一次全量拷贝。
    pub fn pick_online_distro(
        &mut self,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(form) = self.install_form.as_mut() {
            form.online_id = Some(id.clone());
            form.name
                .update(cx, |state, cx| state.set_value(id.clone(), window, cx));
        }
        self.state.online.select(id);
        self.resync_install_dir(window, cx);
        cx.notify();
    }

    /// 切换镜像站那块选中的发行版。
    pub fn select_mirror_distro(&mut self, id: String, cx: &mut Context<Self>) {
        self.state.mirrors.select_distro(id);
        cx.notify();
    }

    /// 探测镜像站：逐条 HEAD 一遍，挑最快的那个。
    ///
    /// **串行**探测（内置表里每个发行版只有 2~3 个候选、每条 8 秒上限）：
    /// 并发要引入线程管理，而收益只是"省几秒"。
    pub fn probe_mirrors(&mut self, cx: &mut Context<Self>) {
        if self.state.mirrors.probing {
            return;
        }
        let Some(distro) = self.state.mirrors.selected_distro() else {
            self.state
                .notify(Toast::error("内置镜像表里没有可用的发行版"));
            cx.notify();
            return;
        };
        if !mirrors::available_on_this_arch() {
            self.state.notify(Toast::error(format!(
                "内置镜像表目前只有 amd64 的条目，这台机器是 {} —— 请用「自定义 URL」",
                mirrors::arch()
            )));
            cx.notify();
            return;
        }

        let candidates = mirrors::candidates(distro);
        let release = distro.release.to_owned();

        self.state.mirrors.probing = true;
        self.state.mirrors.error = None;
        self.state.mirrors.results.clear();
        self.state.mirrors.chosen = None;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let probed = cx
                .background_executor()
                .spawn(async move {
                    let mut out: Vec<(mirrors::Candidate, Option<mirrors::Probe>)> = Vec::new();
                    for candidate in candidates {
                        let args = mirrors::probe_args(&candidate.url);
                        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
                        let probe = wslc_core::cli::run_helper_with_hint(
                            "curl.exe",
                            "curl.exe",
                            wslc_core::cli::CURL_NOT_FOUND_HINT,
                            &refs,
                            Duration::from_secs(20),
                        )
                        .ok()
                        .and_then(|out| mirrors::parse_probe(&out.stdout));
                        out.push((candidate, probe));
                    }
                    out
                })
                .await;

            let _ = this.update(cx, |shell, cx| {
                shell.state.mirrors.probing = false;
                shell.state.mirrors.results = probed
                    .iter()
                    .map(|(candidate, probe)| MirrorProbeResult {
                        site: candidate.site.clone(),
                        url: candidate.url.clone(),
                        code: probe.as_ref().map(|p| p.code).unwrap_or(0),
                        secs: probe.as_ref().map(|p| p.secs).unwrap_or(0.0),
                        bytes: probe.as_ref().and_then(|p| p.bytes),
                    })
                    .collect();

                match mirrors::pick_fastest(&probed) {
                    Some((candidate, probe)) => {
                        let site = candidate.site.clone();
                        shell.state.mirrors.chosen = Some(MirrorChoice {
                            site: site.clone(),
                            url: candidate.url,
                            release: release.clone(),
                            bytes: probe.bytes,
                        });
                        shell.state.notify(Toast::success(format!(
                            "选中最快的镜像：{site}"
                        )));
                    }
                    None => {
                        shell.state.mirrors.chosen = None;
                        shell.state.mirrors.error = Some(
                            "这个发行版在所有内置镜像上都拿不到（可能那一版的文件改名了）——\
                             换一个版本，或者用「自定义 URL」"
                                .to_owned(),
                        );
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    // -- 添加实例：执行 ----------------------------------------------------

    /// 按表单执行安装。
    ///
    /// 安装可能跑十几分钟到几十分钟（在线下载 / 铺开文件系统），所以：
    ///
    /// 1. 装前检查在**提交这一刻**做（其中"目录非空"要碰文件系统，
    ///    不能放在渲染里）；
    /// 2. 真正干活的是 [`wslc_core::cmd::install::run_plan`]（**阻塞**），
    ///    跑在后台执行器上；
    /// 3. 界面每 250 ms 把事件搬进 `AppState::installing`，
    ///    所以日志和进度是**边跑边显示**的。
    pub fn confirm_install(&mut self, cx: &mut Context<Self>) {
        if self.install_run.is_some() {
            self.state
                .notify(Toast::error("已经有一个安装在进行 —— 等它结束，或者先取消"));
            cx.notify();
            return;
        }

        // 先把 spec 取出来（借用在这一句结束），下面才好 `&mut self` 发提示条。
        let spec = self
            .install_form
            .as_ref()
            .map(|form| form.to_spec(cx, &self.state.mirrors));
        let Some(spec) = spec else {
            self.state.notify(Toast::error("表单尚未创建"));
            cx.notify();
            return;
        };

        let ctx = self.state.plan_context();
        let dir = spec
            .effective_install_dir(&ctx)
            .unwrap_or_default();
        let check = self.state.preflight(&spec, dir_non_empty(&dir));
        if !check.ok() {
            self.state.notify(Toast::error(
                check
                    .error_text()
                    .unwrap_or_else(|| "参数有误".to_owned()),
            ));
            cx.notify();
            return;
        }
        for warning in &check.warnings {
            tracing::info!("安装提醒：{warning}");
        }

        let plan = match install_model::plan(&spec, &ctx) {
            Ok(plan) => plan,
            Err(e) => {
                self.state.notify(Toast::error(format!("参数有误：{e}")));
                cx.notify();
                return;
            }
        };

        // 要跑哪些步骤写进日志：出问题时能直接复制到终端复现
        // （界面上的预览和执行读的是**同一份计划**）。
        tracing::info!(
            "安装 {}：{}",
            plan.name,
            plan.preview_lines().join("  →  ")
        );

        let events: Arc<Mutex<Vec<InstallEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let done: Arc<Mutex<Option<InstallSummary>>> = Arc::new(Mutex::new(None));
        let cancel = InstallCancel::new();
        let expected_bytes = self.state.mirrors.chosen.as_ref().and_then(|c| c.bytes);
        let wsl = self.state.wsl.clone();

        // 事件队列与结果槽的**原始 Arc 交给 `install_run`**（界面每轮从那儿取），
        // 后台任务拿的是克隆 —— 两边看到的是同一份数据。
        let work_events = Arc::clone(&events);
        let work_done = Arc::clone(&done);
        let work_cancel = cancel.clone();

        self.state.installing = Some(InstallProgress::new(plan.name.clone()));
        self.install_run = Some(InstallRun {
            events: Arc::clone(&events),
            done: Arc::clone(&done),
            started: Instant::now(),
        });
        self.install_cancel = Some(cancel.clone());
        self.state.notify(Toast::info(format!(
            "正在安装 {}…（期间界面可以继续用，随时能取消）",
            plan.name
        )));
        cx.notify();

        cx.spawn(async move |this, cx| {
            // 阻塞活：后台执行器 + 流式事件。这是本仓库唯一允许起进程的地方。
            let work = cx.background_executor().spawn(async move {
                let mut opts = RunOptions::new(move |event| lock(&work_events).push(event));
                opts.cancel = work_cancel;
                opts.expected_bytes = expected_bytes;
                let summary = install::run_plan(&wsl, &plan, opts);
                *lock(&work_done) = Some(summary.clone());
                summary
            });

            // 边跑边搬事件。`absorb_install_events` 顺便看一眼执行结果有没有到 ——
            // `run_plan` 是**阻塞**函数，结果只能这样交回来。
            let summary = loop {
                let finished = this
                    .update(cx, |shell, cx| shell.absorb_install_events(cx))
                    .ok()
                    .flatten();
                if let Some(summary) = finished {
                    break summary;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
            };

            // 等它真正结束，免得 `Task` 在完成前被 drop 掉（那会取消任务）
            let _ = work.await;

            let _ = this.update(cx, |shell, cx| {
                shell.absorb_install_events(cx);
                shell.finish_install(summary, cx);
            });
        })
        .detach();
    }

    /// 取消正在进行的安装（kill 当前子进程）。
    ///
    /// ⚠️ 取消**不保证**什么都没发生：在线安装可能已经注册了一半，
    /// 重定位阶段（`--unregister` 之后）更是根本不给取消 ——
    /// 见 `wslc_core::cmd::install` 的模块说明。
    pub fn cancel_install(&mut self, cx: &mut Context<Self>) {
        match &self.install_cancel {
            Some(token) => {
                token.cancel();
                tracing::info!("已请求取消安装");
                self.state.notify(Toast::info("正在取消安装…"));
            }
            None => self
                .state
                .notify(Toast::error("当前没有正在进行的安装")),
        }
        cx.notify();
    }

    /// 把后台线程塞进来的事件搬进界面状态；返回**执行结果**（还在跑时是 `None`）。
    ///
    /// 只有**真的有变化**才 `cx.notify()`：进度事件每 250 ms 就来一个，
    /// 无脑重绘等于让界面一直空转（导出那边是同样的处理）。
    ///
    /// 结果也从这里回传（而不是另外留一个方法）：界面每轮反正要看一眼事件队列，
    /// 顺手看一眼结果槽不额外花什么，还能保证"最后一批事件"一定先被吃掉。
    fn absorb_install_events(&mut self, cx: &mut Context<Self>) -> Option<InstallSummary> {
        let (events, started, finished) = {
            let Some(run) = self.install_run.as_ref() else {
                return None;
            };
            let drained: Vec<InstallEvent> = std::mem::take(&mut *lock(&run.events));
            (drained, run.started, lock(&run.done).clone())
        };

        let Some(progress) = self.state.installing.as_mut() else {
            return finished;
        };

        let mut changed = false;
        for event in &events {
            changed |= progress.apply(event);
        }

        // 没有进度事件的步骤（导入 / 注销 / 安装本身）也要显示"跑了多久"
        let secs = started.elapsed().as_secs();
        if progress.elapsed_secs != secs {
            progress.elapsed_secs = secs;
            changed = true;
        }

        if changed {
            cx.notify();
        }
        finished
    }

    /// 安装收尾：写结局、发提示、刷新列表。
    ///
    /// 失败时**留在页面上** —— 那里有完整日志，用户要能读它；
    /// 成功才跳到实例列表，让他直接看到新装好的东西。
    fn finish_install(&mut self, summary: InstallSummary, cx: &mut Context<Self>) {
        self.install_run = None;
        self.install_cancel = None;

        let outcome = if summary.cancelled {
            InstallOutcome::Cancelled
        } else if summary.ok {
            InstallOutcome::Success(summary.detail.clone())
        } else {
            InstallOutcome::Failed {
                detail: summary.detail.clone(),
                step: summary.failed_step.clone(),
            }
        };
        if let Some(progress) = self.state.installing.as_mut() {
            progress.finish(outcome);
        }

        if summary.cancelled {
            self.state.notify(Toast::info(format!(
                "{} 的安装已取消。如果它其实已经装好了，去实例列表看一眼；不想要就删掉。",
                summary.name
            )));
        } else if summary.ok {
            self.state
                .notify(Toast::success(format!("{} 已安装", summary.name)));
            self.goto_page(Page::Instances, cx);
        } else {
            self.state
                .notify(Toast::error(format!("安装失败：{}", summary.detail)));
        }

        self.refresh(cx);
        cx.notify();
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
            // ⚠️ 同样要带上默认安装目录 —— 少写一个字段就是把用户设过的东西抹掉。
            install_dir: self.state.prefs.install_dir.clone(),
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
        Page::About => "nav-about",
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
