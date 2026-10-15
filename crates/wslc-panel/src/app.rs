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
// 输入框。⚠️ v0.6.2 之前这里还导入 `InputEvent`（订阅 tag 过滤框的按键）——
// 过滤框被可搜索下拉框取代之后就没有订阅输入框的场合了，导入也要跟着去掉，
// 否则 `ui` job 会留一条 unused import 警告。
use gpui_kit::component::input::InputState;
// 「微软商店」与「在线发行版」两张清单都走组件库的 `Select`（下拉框）——
// 参考实现也是一个下拉框（`CustomComboBox`），而不是我们原先那种
// "几十个按钮铺满一屏"的写法。
use gpui_kit::component::searchable_list::SearchableListItem;
use gpui_kit::component::select::{SelectEvent, SelectState};
// 同 views.rs：不导入 `Disableable`，全项目不再用 `.disabled()`。
use gpui_kit::component::{Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::*;

use wslc_core::cmd::container::{PullPolicy, RunSpec};
// 数据库管理页（v0.7.0）：真正的活都在 `cmd::mysql` 里（起进程 + 脱敏），
// 识别 / 拼参数 / SQL 生成是 `model::mysql` 的纯逻辑。
use wslc_core::cmd::mysql::{MysqlProbe, run_console_sql, run_sql};
use wslc_core::model::{GrantKind, MysqlCreds, MysqlFamily, RootCredHint, SqlMode, mysql as mysql_model};
// 安装现在是**多步计划**（见 `wslc_core::model::install` 的模块说明）：
// `InstallSpec` 是用户填的东西，`plan()` 把它变成步骤，`run_plan` 把它做出来。
use wslc_core::cmd::distro::InstallSpec;
use wslc_core::cmd::install::{self, InstallCancel, InstallEvent, InstallSummary, RunOptions};
use wslc_core::mirrors;
use wslc_core::model::install::{
    self as install_model, InstallSource, derive_install_dir, suggest_name_from_file,
};
// 镜像仓库（Registry V2）的纯逻辑：引用规范化、URL 拼装、tag 排序、错误翻译。
use wslc_core::registry;
use wslc_core::settings::SettingKey;
// `Wsl` 是发行版（实例）的调用器，和容器的 `Wslc` 并列。
use wslc_core::{Wsl, Wslc};

use crate::state::{
    self, AppState, ConfirmAction, DbFailure, DbState, DbTab, DistroAction, ExecState,
    ExportProgress, ImmediateAction, InstallOutcome, InstallProgress, InstallSourceKind, LogsState,
    MirrorChoice, MirrorProbeResult, Page, PendingAction, PromptKind, RegistryState, Toast,
    ToastKind, exec_argv,
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
    /// 本地文件路径（tar / VHDX / `.wsl`）。联网的两条来源用不到。
    pub(crate) source_path: Entity<InputState>,
    /// 镜像站那块"自定义 rootfs URL"（留空 = 用自动测速挑出来的那个）。
    pub(crate) mirror_url: Entity<InputState>,
    /// 「微软商店」那张清单的**下拉框**。
    ///
    /// 和输入框一样是懒创建的（`SelectState::new` 也要 `&mut Window`），
    /// 所以它跟着表单一起建、一起留着。
    pub(crate) store_select: Entity<SelectState<Vec<DropdownItem>>>,
    /// 「在线发行版（镜像源）」那张清单的下拉框。
    pub(crate) mirror_select: Entity<SelectState<Vec<DropdownItem>>>,
    /// 在线清单里选中的发行版 id（`wsl --install -d` 要的就是它）。
    ///
    /// 和下拉框里的选中项是**同一件事**，但这里存一份：`to_spec` 要能用它，
    /// 而"用户手输名字"那条路（清单拉不到时）没有下拉框可读。
    pub(crate) online_id: Option<String>,
    /// 选中的来源。
    pub(crate) source: InstallSourceKind,
    /// 装完是否启动（只有在线安装支持）。
    pub(crate) launch: bool,
    /// 装完是否设为默认。
    pub(crate) set_default: bool,
    /// 在线安装是否走 `--web-download`（从网络下，而不是微软商店）。
    ///
    /// 默认**固定是微软商店**（本机实测这条路能连上）。曾经有个"先探 GitHub 通不通
    /// 再决定默认值"的后台探测，v0.5.1 删掉了：它探的是**我们自己的 curl**
    /// （不读 Windows 系统代理），而真正决定成败的是 `wsl.exe` 自己的网络通道 ——
    /// 两者在这台机器上的结论正好相反。现在交给"失败自动换一次"
    /// （`PlannedStep::retry_other_source`）去兜，按钮在界面上，用户也能改。
    pub(crate) web_download: bool,
    /// **上一次由我们推导出来的**安装目录。
    ///
    /// 用来判断"用户是不是自己改过目录"：值等于它（或为空）说明这个框还是我们填的，
    /// 名字一变就可以跟着更新；否则绝不覆盖用户手输的东西。
    ///
    /// 这么绕是因为本仓库没有"输入框内容变化"的订阅先例（本机编译不了，
    /// 猜 API 要白烧一轮 CI）—— 用"比较上次推导值"能达到同样效果。
    pub(crate) last_derived_dir: Option<String>,
}

/// 「镜像」页的表单（v0.6 建，v0.6.2 并进「镜像」页）。
///
/// 和 [`InstallForm`] 同一个分工：GPUI 的 `Entity<InputState>` /
/// `Entity<SelectState<_>>` 留在 `Shell` 里，纯数据（tag 列表 / 选中项 / 体积）
/// 在 `AppState::registry` 里。进页面时**懒创建**（都要 `&mut Window`），
/// 离开**不销毁**。
pub(crate) struct ImageForm {
    /// registry 地址。默认 [`wslc_core::registry::DEFAULT_REGISTRY`]。
    ///
    /// ⚠️ 它是**可编辑**的，这不是顺便给的：v0.6.2 合并「镜像 / 镜像仓库」两页、
    /// 去掉「按引用拉取…」弹窗时，正是靠它才没丢掉"从别的 registry 拉"这个能力
    /// （`docs/PLAN-v0.6.md` §3.6 记着当初就是因为这个能力才留了两个入口）。
    /// 用户输入会先过 [`wslc_core::registry::normalize_host`]，
    /// 再由 [`wslc_core::registry::is_valid_host`] 把关 —— 它会拼进 URL
    /// 再作为裸 argv 交给 `curl.exe`。
    pub(crate) host: Entity<InputState>,
    /// 仓库名。`alpine` / `library/nginx` / `grafana/grafana` / `nginx:1.27`，
    /// 甚至粘一整条浏览器 URL 进来都认（规范化在
    /// [`wslc_core::registry::split_input`] 里，有单测）。
    pub(crate) repository: Entity<InputState>,
    /// tag 下拉框（可搜索）。
    ///
    /// ⚠️ 它的 `SelectEvent::Confirm` **必须订阅**（见 [`Shell::ensure_image_form`]）：
    /// 不订阅的话选中了什么都不会发生 —— `Subscription` 一 drop 就退订。
    ///
    /// 列表整体喂进去（实测 `node` 有 9170 个 tag）：组件库的 `List` 是
    /// **虚拟化**的（`crates/component/src/list/list.rs` 用 `v_virtual_list`），
    /// 所以只有可见的几行会被建出来 —— 这正是它替代"本地过滤 + 只渲染 200 行"的原因。
    pub(crate) tags_select: Entity<SelectState<Vec<DropdownItem>>>,
}

/// 下拉框里的一项：**显示文本 ≠ 取值**。
///
/// 组件库的 `Select` 拿 [`SearchableListItem`] 当数据源，而它已经为
/// `String` / `SharedString` 实现了 —— 那种情况下"看到的就是取到的值"。
/// 我们要的是显示 `Ubuntu 24.04 LTS` 而取到 `Ubuntu-24.04`
/// （`wsl --install -d` 只认后者），所以自己定一个小类型。
///
/// 镜像源那张清单同理：显示 `Ubuntu 24.04 (12 镜像源)`，取到的是清单里的
/// `"<name> <version>"`（`Offer::id()`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DropdownItem {
    /// 列表里显示的那一行。
    pub(crate) label: SharedString,
    /// 选中时真正取到的值。
    pub(crate) value: String,
}

impl DropdownItem {
    pub(crate) fn new(label: impl Into<SharedString>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
        }
    }
}

impl SearchableListItem for DropdownItem {
    type Value = String;

    fn title(&self) -> SharedString {
        self.label.clone()
    }

    fn value(&self) -> &String {
        &self.value
    }
}

impl InstallForm {
    /// 把表单读成一个 [`InstallSpec`]。
    ///
    /// 需要 `cx` 才能从 `InputState` 里取值，所以它不是纯函数 ——
    /// 这也是 `views::page` 要多收一个 `&Shell` 的原因
    /// （页面要**实时**预览要跑哪些步骤）。
    ///
    /// `taken`（已经装过的发行版名）与 `default_dir`（「新实例默认安装目录」）
    /// 由调用方传进来，而不是在这里去读 `AppState`：渲染那一帧和提交那一刻
    /// 拿到的是同一份，预览才不会和真正执行的走偏。
    ///
    /// **没有版本这一项**：本项目只支持 WSL 2，装出来的固定是 WSL 2
    /// （见 [`wslc_core::model::install::WSL_VERSION`]）。
    ///
    /// # 镜像站那一条为什么以输入框为准
    ///
    /// 「自定义 URL」填了就用它，**忽略**探测出来的那个 ——
    /// 用户手输地址就是在表达"别管你探测到什么"。这也让"镜像站上没有我要的版本"
    /// 有个出口，而不是只能换来源。
    pub(crate) fn to_spec(
        &self,
        cx: &App,
        mirrors: &crate::state::MirrorState,
        online: &crate::state::OnlineDistroState,
        taken: &[String],
        default_dir: Option<&str>,
    ) -> InstallSpec {
        let text = |input: &Entity<InputState>| input.read(cx).value().trim().to_owned();
        let typed_name = text(&self.name);

        let source = match self.source {
            // 本地那一条按**后缀**分岔（界面只有一个入口，数据层仍是三条不同的路）。
            // 认不出来时按 tar 处理 —— 界面会如实写"没认出来，按 tar 处理"，
            // 而不是让用户对着一个空的下拉框猜。
            InstallSourceKind::Local => {
                let path = text(&self.source_path);
                match install_model::classify_local_file(&path) {
                    Some(install_model::LocalFileKind::Vhdx) => InstallSource::Vhdx { path },
                    Some(install_model::LocalFileKind::Bundle) => InstallSource::File { path },
                    Some(install_model::LocalFileKind::Tar) | None => InstallSource::Tar { path },
                }
            }
            InstallSourceKind::Mirror => {
                let custom = text(&self.mirror_url);
                // 选中的那条发行版给出 `release`（只用于显示 / 临时文件名）
                let release = mirrors
                    .selected_offer()
                    .map(|offer| offer.version.clone())
                    .unwrap_or_default();
                match (custom.is_empty(), mirrors.chosen.as_ref()) {
                    // 手填优先：格式按后缀猜（`.wsl` 要走安装器，不是 `--import`）
                    (false, _) => InstallSource::Mirror {
                        url: custom.clone(),
                        mirror: "自定义 URL".to_owned(),
                        release,
                        format: if custom.to_lowercase().ends_with(".wsl") {
                            "wsl".to_owned()
                        } else if custom.to_lowercase().ends_with(".tar.gz") {
                            "tar.gz".to_owned()
                        } else {
                            "tar.xz".to_owned()
                        },
                    },
                    // 否则用探测出来的那个；都还没有就给空 URL（校验会挡住并说明）
                    (true, Some(choice)) => InstallSource::Mirror {
                        url: choice.url.clone(),
                        mirror: choice.site.clone(),
                        release: choice.release.clone(),
                        format: choice.format.clone(),
                    },
                    (true, None) => InstallSource::Mirror {
                        url: String::new(),
                        mirror: String::new(),
                        release,
                        format: "tar.xz".to_owned(),
                    },
                }
            }
            InstallSourceKind::Online => {
                // 清单里选过就用它的 id（那才是 `wsl --install -d` 认的名字）；
                // 没选过（清单拉不到）就把用户手输的名字当 id ——
                // 这正是 v0.3 的那条老路：本机拉不到在线清单是常态。
                let id = self
                    .online_id
                    .clone()
                    .filter(|id| !id.trim().is_empty())
                    .unwrap_or_else(|| typed_name.clone());
                // 裸家族名（`Ubuntu`）在这台机器上装不动（实测 2 秒
                // `WININET_E_CANNOT_CONNECT`），换成同族带版本号的那条。
                // 见 `OnlineDistroState::sibling_id` 的说明。
                // ⚠️ 已经装过的兄弟要排除掉：换过去只会撞
                // `Wsl/InstallDistro/ERROR_ALREADY_EXISTS`（实测）。
                let fallback_id = online.sibling_id(&id, taken);
                InstallSource::Online {
                    id,
                    launch: self.launch,
                    web_download: self.web_download,
                    fallback_id,
                }
            }
        };

        let mut spec = InstallSpec::new(text(&self.name), source);
        // 微软商店那条路上，名字**可以留空**：留空就用清单里选中的那个 id
        // （参考实现的商店页干脆没有名字输入框）。这样默认走的就是
        // "名字 == id"的快路径，不会有那次多余的全量拷贝。
        if spec.name.trim().is_empty() && self.source.is_online() {
            spec.name = self
                .online_id
                .clone()
                .filter(|id| !id.trim().is_empty())
                .unwrap_or_default();
        }
        spec.install_dir = self.resolved_install_dir(default_dir, &spec.name, cx);
        spec.set_default = self.set_default;
        spec
    }

    /// 这次安装实际会用到的安装目录。
    ///
    /// # 为什么在**这里**还要再推一次（不只靠 `resync_install_dir`）
    ///
    /// `Shell::resync_install_dir` 只在两处被调用：从在线清单**点选**发行版、
    /// 以及用文件选择器挑 rootfs。也就是说 **手输名字**（镜像源那条路必然手输）
    /// 和"点选镜像源清单"都不经过它 —— 后果就是本机日志里那一幕：
    /// 「默认安装目录」`D:\wsl` 被当成安装目录**本身**用了
    /// （`wsl --import Ubuntu D:\wsl <tar>`），发行版直接装进父目录；
    /// 而下一次安装又会被"这个安装目录里已经有东西了"挡住 ——
    /// 用户看到的是两条互不相干的错，很难联想到根因。
    ///
    /// 放在 `to_spec` 里、而不是"提交那一刻"：预览和执行读的是同一个函数，
    /// 否则界面上显示的目录和真正用的又不是一个
    /// （见 `model::install` 的模块说明）。
    ///
    /// 只在**用户没自己改过**时才推 —— 判据与 `resync_install_dir` 完全一致：
    /// 空着，或者还等于我们上次推出来的那个。用户亲手填 / 选的路径一律不动。
    fn resolved_install_dir(&self, default_dir: Option<&str>, name: &str, cx: &App) -> String {
        let typed = self.install_dir.read(cx).value().trim().to_owned();
        let untouched =
            typed.is_empty() || Some(typed.as_str()) == self.last_derived_dir.as_deref();
        if !untouched {
            return typed;
        }
        derive_install_dir(default_dir, name.trim()).unwrap_or(typed)
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
    /// 「从本地镜像里选」下拉框（可搜索）。
    ///
    /// 它只是**快捷方式**：左边那个输入框仍然是唯一的事实来源
    /// （可以手打、可以粘完整引用）。候选来自快照里的 `wslc images`，
    /// 见 [`Shell::fill_image_select`]。
    ///
    /// ⚠️ 它的 `SelectEvent::Confirm` **必须订阅**（见
    /// [`Shell::open_create_dialog_prefilled`]）：`Subscription` 一 drop 就退订，
    /// 表现是"选了一个镜像，输入框没变"—— 不会报错，所以很难查。
    pub(crate) image_select: Entity<SelectState<Vec<DropdownItem>>>,
    pub(crate) name: Entity<InputState>,
    pub(crate) ports: Entity<InputState>,
    pub(crate) env: Entity<InputState>,
    pub(crate) volumes: Entity<InputState>,
    pub(crate) network: Entity<InputState>,
    pub(crate) memory: Entity<InputState>,
    pub(crate) cpus: Entity<InputState>,
    pub(crate) pull: PullPolicy,
    /// 从镜像里读来的那点信息（端口 / 启动命令），见 [`FactsState`]。
    pub(crate) facts: FactsState,
}

/// 「创建容器」弹窗里那条**从镜像读来的**信息的状态。
///
/// 放 `CreateDialog` 而**不是** `AppState`：它是这个弹窗的附带信息，
/// 弹窗一关就该跟着走。而**解析**本身是纯逻辑，住在 `wslc-core`
/// （`model::parse_image_inspect`，配两个真机 fixture 单测）。
///
/// 为什么要有这个状态机而不是一个 `Option`：`Idle`（用户手填的镜像，我们没去读）
/// 和 `Loading`（正在读）在界面上是两句不同的话，用一个 `None` 表示不了。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum FactsState {
    /// 还没开始读（例如镜像引用是用户自己敲的）。
    #[default]
    Idle,
    /// 正在读。
    Loading,
    /// 读到了。
    Ready(wslc_core::model::ImageFacts),
    /// 读不到（多半是镜像还不在本地）。原因直接给用户看。
    Failed(String),
}

impl CreateDialog {
    /// 把表单读成一个 [`RunSpec`]。
    ///
    /// 需要 `cx` 才能从 `InputState` 里取值，所以它不是纯函数 ——
    /// 这也是 `views::page` 要多收一个 `cx` 的原因
    /// （弹窗底部要**实时**预览等效命令）。
    /// `pub(crate)`：`views.rs` 要用它来做**实时**等效命令预览。
    ///
    /// `existing_names` 是**现有容器的名字**（`AppState::container_names()`）：
    /// 容器名留空时按镜像推一个（见 [`wslc_core::model::suggest_container_name`]），
    /// 撞名要能看出来，所以得知道已经有哪些名字。
    pub(crate) fn to_spec(&self, existing_names: &[String], cx: &App) -> RunSpec {
        let text = |input: &Entity<InputState>| input.read(cx).value().trim().to_owned();

        let mut spec = RunSpec::new(text(&self.image));
        // 强制后台运行，理由见 `Shell::confirm_create` 的文档注释。
        spec.detach = true;
        spec.pull = self.pull;

        let name = text(&self.name);
        if name.is_empty() {
            // 留空时**不让 `wslc` 随机起名**（那是 Docker 那套
            // `shimmering_karakoram`，列表里看不出是哪个镜像起的）——
            // 按镜像推一个：`nginx` / `nginx-2`。推不出来（镜像还没填）就
            // 保持 `None`，交给 `wslc` 自己起（那时提交也会被"请先填写镜像引用"拦住）。
            let suggested = wslc_core::model::suggest_container_name(&spec.image, existing_names);
            if !suggested.is_empty() {
                spec.name = Some(suggested);
            }
        } else {
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

/// 「数据库管理」页上那三个输入框（连接卡两个 + SQL 控制台一个）。
///
/// 为什么它们是**页面**的一部分而不是弹窗（和「拉取镜像」正相反）：
/// 连接卡描述的是"这一页现在连的是谁"，属于**页面状态** —— 用户扫一眼就知道
/// 现在用的是哪个账号，而不用点开一个弹窗去确认。
///
/// 它们同样懒创建（`InputState::new` 要 `&mut Window`），但**不随页面离开销毁**：
/// 切走再回来时用户刚填的密码与 SQL 还在（和 `install_form` 同一条：重建输入框
/// 会让用户一个字都打不进去，而且这里还会丢掉他要用的密码）。
pub(crate) struct DbPageForm {
    /// 用户名（默认 `root`）。
    pub(crate) user: Entity<InputState>,
    /// 密码。
    ///
    /// ⚠️ 明文显示。`MysqlCreds` 的 `Debug` 是脱敏的（密码不会进日志），
    /// 但这一格是画在屏幕上的 —— 换成掩码要先确认组件库支持
    /// （`AGENTS.md` 说"推测的结论要写明"），所以这一版照实显示。
    pub(crate) password: Entity<InputState>,
    /// SQL 控制台里那条语句。
    pub(crate) sql: Entity<InputState>,
}

/// 「数据库管理」页的一次性动作弹窗在做什么。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DbDialogKind {
    /// 新建数据库。
    NewDatabase,
    /// 新建用户（可顺带授权到某个库）。
    NewUser,
    /// 改密码。⚠️ 用户名 / 主机是**锁定**的：改了就是另一个账号了。
    AlterPassword {
        /// 目标用户。
        user: String,
        /// 目标主机。
        host: String,
    },
    /// 给已有用户授权。
    Grant {
        /// 目标用户。
        user: String,
        /// 目标主机。
        host: String,
    },
}

impl DbDialogKind {
    /// 弹窗标题。
    pub(crate) fn title(&self) -> &'static str {
        match self {
            Self::NewDatabase => "新建数据库",
            Self::NewUser => "新建 MySQL 用户",
            Self::AlterPassword { .. } => "修改密码",
            Self::Grant { .. } => "授权",
        }
    }

    /// 提交按钮文案。
    pub(crate) fn confirm_label(&self) -> &'static str {
        match self {
            Self::NewDatabase => "创建",
            Self::NewUser => "创建",
            Self::AlterPassword { .. } => "改密码",
            Self::Grant { .. } => "授权",
        }
    }
}

/// 「数据库管理」页的动作弹窗。
///
/// 四种动作共用一个结构体（而不是四个 `Option` 字段）：同时只可能开一个，
/// 而且它们共用"用户名 / 主机 / 密码 / 库名"这几个输入框 —— 分成四份就要
/// 把同一套输入框建四遍，还要各自记住"上一份填了什么"。
pub(crate) struct DbDialog {
    /// 在做什么。
    pub(crate) kind: DbDialogKind,
    /// 新建数据库：库名。
    pub(crate) database: Entity<InputState>,
    /// 新建数据库：字符集（留空 = 服务器的默认值）。
    pub(crate) charset: Entity<InputState>,
    /// 新建数据库：排序规则（留空 = 跟着字符集）。
    pub(crate) collation: Entity<InputState>,
    /// 新建用户 / 改密码：用户名。
    pub(crate) user: Entity<InputState>,
    /// 新建用户 / 改密码：主机。
    pub(crate) host: Entity<InputState>,
    /// 新建用户 / 改密码：密码。
    pub(crate) password: Entity<InputState>,
    /// 新建用户 / 授权：授权到哪个库（留空 = 不授权）。
    pub(crate) grant_db: Entity<InputState>,
    /// 授权级别。
    pub(crate) grant: GrantKind,
}

/// 这次连数据库要用哪个账号。
///
/// 规则只有一条：**用户填了就用他的**，没填才用探测到的那个
/// （容器环境变量里的 `MYSQL_ROOT_PASSWORD`）。
///
/// 为什么"空密码"要特殊对待：空密码是 MySQL 的**合法配置**
/// （`MYSQL_ALLOW_EMPTY_PASSWORD=yes`），而"还没填"和"就是要空密码"在界面上
/// 长得一模一样。这里选"环境变量优先" —— 那种容器里环境变量写的本来就是
/// 空密码（`RootCredHint::Empty`），两条路殊途同归。
fn effective_db_creds(
    probe: &MysqlProbe,
    typed_user: &str,
    typed_password: &str,
) -> (String, String) {
    let user = if typed_user.is_empty() {
        "root".to_owned()
    } else {
        typed_user.to_owned()
    };
    let password = if typed_password.is_empty() {
        match &probe.root_hint {
            RootCredHint::Password(password) => password.clone(),
            _ => String::new(),
        }
    } else {
        typed_password.to_owned()
    };
    (user, password)
}

/// 应用外壳。
pub struct Shell {
    /// 全部状态。
    pub state: AppState,
    /// 正在进行的拉取任务的取消令牌。
    ///
    /// 放 `Shell`：这是**能力**（能 kill 子进程），不是"状态"。
    /// 纯数据部分（拉了哪个镜像、输出了什么）在 `AppState::pulling` 里，
    /// 因为 `views.rs` 只拿得到 `&AppState`。
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
    /// 「新终端的起始目录」那个自定义路径输入框。
    ///
    /// 点过设置页的「自定义路径…」之后才是 `Some`，建完就**一直留着**
    /// （和 `install_form` 同一个理由：`InputState::new` 要 `&mut Window`，
    /// 而渲染时每帧重建输入框会让用户**一个字都打不进去**）。
    ///
    /// `pub(crate)`：`views.rs` 要读它来画 `Input`。
    pub(crate) terminal_dir_input: Option<Entity<InputState>>,
    /// 「创建容器」弹窗；关闭时为 `None`。
    create_dialog: Option<CreateDialog>,
    /// 「容器 exec」浮层的命令输入框；浮层关闭时为 `None`。
    ///
    /// 和 `create_dialog` 一样懒创建（`InputState::new` 要 `&mut Window`）。
    /// 纯数据部分（跑过什么、输出是什么）在 `AppState::exec` 里。
    pub(crate) exec_input: Option<Entity<InputState>>,
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
    /// 和 `create_dialog` 一样**懒创建**（`InputState::new` 要 `&mut Window`），
    /// 但**不随页面离开而销毁** —— 见 [`InstallForm`]。
    ///
    /// `pub(crate)`：`views.rs` 要读它来渲染页面（`Shell` 的其余字段
    /// 只有 `app.rs` 自己用，所以是私有的）。
    pub(crate) install_form: Option<InstallForm>,
    /// 两个下拉框（商店清单 / 镜像源清单）的**订阅**。
    ///
    /// ⚠️ 必须存着：`Context::subscribe_in` 返回的 `Subscription`
    /// **一 drop 就退订**（组件库自己也是 `vec![cx.subscribe_in(..)]` 存起来的）。
    /// 丢了它不会报错，只是"选了下拉框什么都不会发生" —— 那种 bug 很难查。
    select_subs: Vec<Subscription>,
    /// 「镜像」页的表单（registry / 仓库名两个输入框 + tag 下拉框）；
    /// 进过那一页之后一直是 `Some`。
    ///
    /// `pub(crate)`：`views.rs` 要读它来渲染页面。
    pub(crate) image_form: Option<ImageForm>,
    /// 「镜像」页 tag 下拉框的**订阅**。
    ///
    /// ⚠️ 和 `select_subs` 同一个坑：`Subscription` **一 drop 就退订**，
    /// 表现是"在下拉框里选了一个 tag，什么都不发生"—— 不会报错，所以很难查。
    image_subs: Vec<Subscription>,
    /// 「拉取镜像」弹窗开着吗。
    ///
    /// 和 `image_form` 分开记：**表单建好之后一直留着**（用户第二次打开时
    /// registry / 仓库名 / 上一批 tag 都还在），而这个布尔只管可见性。
    /// 拉取进行中**不许关**（见 [`Shell::close_pull_dialog`]）。
    pull_open: bool,
    /// 「创建容器」弹窗里那个「从本地镜像里选」下拉框的**订阅**。
    ///
    /// ⚠️ 和 `select_subs` / `image_subs` 同一个坑：`Subscription`
    /// **一 drop 就退订**，表现是"选了一个镜像，输入框没变"。
    ///
    /// 它跟着弹窗的**创建**走（每次开弹窗重建一份），所以这里不用
    /// `Option` —— 弹窗关掉时订阅也跟着丢，正好。
    create_subs: Vec<Subscription>,
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
    /// 采集期间又有刷新请求进来了吗（本轮跑完要立刻补一次）。
    ///
    /// 丢不得：操作（启动 / 停止 / 创建 / 删除）完成后都会补刷，撞上 3 秒一次的
    /// 自动刷新时若把这次请求丢掉，界面要等到下一个周期才变 ——
    /// 用户看到的就是"点了停止，状态还是运行中"。
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
    /// 「数据库管理」页的连接卡 / SQL 输入框；没进过那一页时为 `None`。
    ///
    /// `pub(crate)`：页面在 `views.rs` 里渲染。
    pub(crate) db_form: Option<DbPageForm>,
    /// 「数据库管理」页的一次性动作弹窗；关闭时为 `None`。
    ///
    /// 和 `create_dialog` 一样懒创建（`InputState::new` 要 `&mut Window`）。
    pub(crate) db_dialog: Option<DbDialog>,
}

/// `wsl --status` 的采集周期（秒）。
///
/// 它每轮都跑没必要：默认发行版 / 默认版本极少变，而且它的输出是**本地化**的
/// （中文系统是「默认分发:」），解析成本比列表高。
///
/// 列表（`wsl --list --verbose`）仍然跟随自动刷新 —— 状态变化要看得到。
const DISTRO_STATUS_SECS: u64 = 30;

/// 选到路径之后，除了填进输入框还要不要额外做一件事。
///
/// 单独一个类型而不是 `bool`：两种意图完全不同，`true` / `false`
/// 在调用处读不出来是哪个。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AfterPick {
    /// 只填输入框，别的什么都不做。
    Nothing,
    /// 把「安装目录」标成**用户亲手选的** —— 那之后名字联动就不能再改它
    /// （见 [`Shell::resync_install_dir`] 的 `last_derived_dir`）。
    InstallDirIsNowUserChosen,
}

impl Shell {
    /// 创建外壳：立刻触发一次采集、加载配置、启动自动刷新。
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut shell = Self {
            state: AppState::new(Wslc::new(), Wsl::new()),
            pull_cancel: None,
            export_cancel: None,
            picking: false,
            keep_alive: Vec::new(),
            wslconf_dialog: None,
            terminal_dir_input: None,
            create_dialog: None,
            exec_input: None,
            detail: None,
            distro_detail: None,
            install_form: None,
            select_subs: Vec::new(),
            image_form: None,
            image_subs: Vec::new(),
            pull_open: false,
            create_subs: Vec::new(),
            prompt: None,
            install_run: None,
            install_cancel: None,
            refresh_again: false,
            refresh_visible: false,
            status_tick: 0,
            db_form: None,
            db_dialog: None,
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

        // 两张清单都是**下拉框**（组件库的 `Select`）。它们和输入框一样要
        // `&mut Window`，所以在这里一起建；条数不多（商店 ~24、镜像源 24），
        // 一开始给空列表，拉回来再 `set_items` 填。
        //
        // 商店那张带搜索（`searchable`）：微软的清单里 24 个发行版的名字
        // 有好几对很像（Ubuntu 26.04 / 24.04 / 22.04 / 20.04），
        // 原先是列表外面单独一个搜索框，现在收进下拉框里（参考实现没有搜索，
        // 但它的清单是 `wsl -l -o` 的顺序、我们的更长）。
        let store_select = cx.new(|cx| {
            SelectState::new(Vec::<DropdownItem>::new(), None, window, cx).searchable(true)
        });
        let mirror_select =
            cx.new(|cx| SelectState::new(Vec::<DropdownItem>::new(), None, window, cx));

        // 选中即生效：商店那条把 id 记下来并填进名字框；
        // 镜像源那条**立刻开始自动测速**（参考实现就是"选中即测速择优"，
        // 没有"探测"按钮）。
        //
        // ⚠️ `subscribe_in` 返回的 `Subscription` **一丢就退订** ——
        // 必须存进 `Shell`（组件库自己也是这么做的：`vec![cx.subscribe_in(...)]`）。
        let store_sub = cx.subscribe_in(
            &store_select,
            window,
            |shell, _emitter, event: &SelectEvent<Vec<DropdownItem>>, window, cx| {
                if let SelectEvent::Confirm(Some(id)) = event {
                    shell.pick_online_distro(id.clone(), window, cx);
                }
            },
        );
        let mirror_sub = cx.subscribe_in(
            &mirror_select,
            window,
            |shell, _emitter, event: &SelectEvent<Vec<DropdownItem>>, _window, cx| {
                if let SelectEvent::Confirm(Some(id)) = event {
                    shell.select_mirror_distro(id.clone(), cx);
                }
            },
        );
        self.select_subs = vec![store_sub, mirror_sub];

        self.install_form = Some(InstallForm {
            name: cx.new(|cx| {
                InputState::new(window, cx).placeholder(default_source.name_placeholder())
            }),
            install_dir: cx.new(|cx| InputState::new(window, cx).placeholder(r"D:\wsl\MyDistro")),
            source_path: cx.new(|cx| {
                InputState::new(window, cx).placeholder(default_source.path_placeholder())
            }),
            mirror_url: cx.new(|cx| {
                InputState::new(window, cx).placeholder("留空 = 用自动测速挑出来的那个地址")
            }),
            store_select,
            mirror_select,
            online_id: None,
            source: default_source,
            launch: false,
            set_default: false,
            // 默认走**微软商店**（实测本机这条路能连上；两条通道都不带 `--location`，
            // 理由见 `model::install::plan` 里那张表）。开关在界面上，用户可以切；
            // 真失败了执行器还会自动换一次（见 `PlannedStep::retry_other_source`）。
            web_download: false,
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
    fn create_volume(&mut self, name: String, cx: &mut Context<Self>) {
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

    // -- 镜像：远端浏览 / 拉取（v0.6 建，v0.6.2 并进「镜像」页） ---------------

    /// 建「镜像」页的表单（幂等）：registry + 仓库名两个输入框 + tag 下拉框。
    ///
    /// **必须**在有 `&mut Window` 的地方调用 —— `InputState::new` 与
    /// `SelectState::new` 都要它。
    pub fn ensure_image_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.image_form.is_some() {
            return;
        }

        let host = cx.new(|cx| {
            InputState::new(window, cx).placeholder(wslc_core::registry::DEFAULT_REGISTRY)
        });
        // 预填内置那个：绝大多数用户根本不改它（`set_value` 也要 `&mut Window`，
        // 所以只能在这里做）。
        host.update(cx, |state, cx| {
            state.set_value(wslc_core::registry::DEFAULT_REGISTRY, window, cx)
        });
        let repository = cx.new(|cx| {
            InputState::new(window, cx).placeholder("nginx / library/nginx / grafana/grafana")
        });
        let tags_select = cx.new(|cx| {
            SelectState::new(Vec::<DropdownItem>::new(), None, window, cx).searchable(true)
        });

        // ⚠️ 这个订阅是**必须**的，不是优化：渲染整页的是 `Shell`，而下拉框的选中项
        // 改的是 `SelectState` 自己的状态 —— 不订阅父级就不会重绘，而且
        // "选了 tag 之后量体积 / 出现拉取按钮"整条链都断了。
        //
        // 订阅本身要存进 `Shell`：`Subscription` **一 drop 就退订**
        // （和 `select_subs` 同一个坑，见那个字段的说明）。
        let tags_sub = cx.subscribe(
            &tags_select,
            |shell, _emitter, event: &SelectEvent<Vec<DropdownItem>>, cx| {
                if let SelectEvent::Confirm(Some(tag)) = event {
                    shell.select_registry_tag(tag.clone(), cx);
                }
            },
        );
        self.image_subs = vec![tags_sub];

        self.image_form = Some(ImageForm {
            host,
            repository,
            tags_select,
        });
    }

    /// 打开「拉取镜像」弹窗。
    ///
    /// 表单**只在第一次打开时建**（`InputState::new` / `SelectState::new` 都要
    /// `&mut Window`），之后一直留着 —— 第二次打开时上回的 registry / 仓库名 /
    /// 那一批 tag 都还在（重建会把它们清掉，用户得重新查一遍）。
    pub fn open_pull_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(progress) = &self.state.pulling {
            let image = progress.image.clone();
            self.state
                .notify(Toast::error(format!("{image} 正在拉取中，请等它结束")));
            cx.notify();
            return;
        }
        self.ensure_image_form(window, cx);
        // 下拉框里得有上一次查到的 tag（表单是留着的，但列表来自状态）。
        self.fill_tag_select(window, cx);
        self.pull_open = true;
        // 焦点给仓库名框：打开就能直接打字（registry 一般不用改）。
        if let Some(form) = self.image_form.as_ref() {
            let handle = form.repository.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }
        cx.notify();
    }

    /// 关闭「拉取镜像」弹窗。
    ///
    /// ⚠️ 拉取进行中**不允许关闭** —— 关掉会让人以为任务被取消了。
    /// 关不掉时给一句提示，别让这一下点击石沉大海（`AGENTS.md` §7.1 那条
    /// "点下去必须有变化"的同一条道理）。
    pub fn close_pull_dialog(&mut self, cx: &mut Context<Self>) {
        if self.state.pulling.is_some() {
            self.state
                .notify(Toast::info("正在拉取：要停止请点「取消拉取」"));
            cx.notify();
            return;
        }
        self.pull_open = false;
        cx.notify();
    }

    /// 从「拉取镜像」弹窗直接去建容器：**先关弹窗**，再开创建表单。
    ///
    /// 两个浮层叠在一起没有任何好处（而且遮罩只有一层），关掉之后用户回头
    /// 还能在「本地镜像」表里看见刚拉下来的那一条。
    pub fn use_pulled_image(
        &mut self,
        image: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pull_open = false;
        self.open_create_dialog_prefilled(Some(image.as_str()), window, cx);
    }

    /// 把 tag 列表填进下拉框。
    ///
    /// ⚠️ `set_items` 会把下拉框自己的选中项清掉，而用户没做错什么 ——
    /// 所以换完清单要把"要拉的那个 tag"（[`RegistryState::effective_tag`]）
    /// **重新选上**（它还在新清单里的话）。
    ///
    /// ⚠️ 要 `&mut Window`，所以只能在 `update_in` 里调（见 [`Shell::query_registry`]）。
    fn fill_tag_select(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = self.image_form.as_ref() else {
            return;
        };
        let select = form.tags_select.clone();

        let items: Vec<DropdownItem> = self
            .state
            .registry
            .tags
            .iter()
            .map(|tag| DropdownItem::new(tag.clone(), tag.clone()))
            .collect();
        let keep = self
            .state
            .registry
            .effective_tag()
            .map(str::to_owned);

        select.update(cx, |state, cx| {
            state.set_items(items, window, cx);
            if let Some(tag) = keep {
                state.set_selected_value(&tag, window, cx);
            }
        });
    }

    /// 「镜像」页点「查询」：拉这个仓库的 tag 列表。
    ///
    /// ⚠️ 那个请求**必须在后台执行器上**（`AGENTS.md` §7.1）：实测 `node` 的
    /// tag 列表是 152 KB / 2.4 秒，放界面线程上就是"点了卡两秒"。
    ///
    /// ⚠️ 回来之后要**填下拉框**，而 `SelectState::set_items` 要 `&mut Window`
    /// —— 所以走 `spawn_in` + `update_in`（和 [`Shell::refresh_online_list`]
    /// 同一条路，见 `AGENTS.md` §7.2）。
    pub fn query_registry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 先把两个控件句柄取出来（`Entity` 克隆很便宜）：后面要 `&mut self`
        // 调别的方法，别让 `form` 的不可变借用一直挂着。
        let (host_input, repo_input) = {
            let Some(form) = self.image_form.as_ref() else {
                return;
            };
            (form.host.clone(), form.repository.clone())
        };

        let typed_host = host_input.read(cx).value().trim().to_owned();
        // 空着就当内置那个：用户清空 registry 框的意图多半是"用默认的"，
        // 而不是"给我一个非法地址的错误"。
        let host = {
            let normalized = wslc_core::registry::normalize_host(&typed_host);
            if normalized.is_empty() {
                wslc_core::registry::DEFAULT_REGISTRY.to_owned()
            } else {
                normalized
            }
        };
        // 收拾过就把结果写回框里（粘一整条 URL 是常态：`https://x/` 会变成 `x`）。
        if host != typed_host {
            let value = host.clone();
            host_input.update(cx, |state, cx| state.set_value(value, window, cx));
        }
        // ⚠️ 校验必须在**发请求之前**：`host` 会拼进 URL，再作为裸 argv 交给
        // `curl.exe`（判据只有 `registry::is_valid_host` 一处）。
        if !wslc_core::registry::is_valid_host(&host) {
            self.state
                .notify(Toast::error(format!("registry 地址不合法：{host}")));
            cx.notify();
            return;
        }

        let raw = repo_input.read(cx).value().trim().to_owned();
        if raw.is_empty() {
            self.state
                .notify(Toast::error("先填一个仓库名，例如 nginx"));
            cx.notify();
            return;
        }
        // 规范化（短名补 `library/`、拆开粘贴进来的 URL、拆出 `:tag`）。
        // **失败就不发请求** —— 判据只有 `registry::split_input` 那一处
        // （那里也是安全边界）。
        let (repository, input_tag) = match registry::split_input(&raw) {
            Ok(parts) => parts,
            Err(e) => {
                self.state.notify(Toast::error(e));
                cx.notify();
                return;
            }
        };
        if self.state.registry.loading {
            return;
        }
        self.state
            .registry
            .begin_query(&host, &raw, &repository, input_tag.clone());
        cx.notify();

        let fetch_host = host.clone();
        let fetch_repository = repository.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    wslc_core::cmd::registry::fetch_tags(&fetch_host, &fetch_repository)
                })
                .await;
            let _ = this.update_in(cx, |shell, window, cx| {
                match result {
                    Ok(tags) => shell.state.registry.set_tags(&repository, tags),
                    Err(e) => shell.state.registry.set_error(e),
                }
                // 输入里自带 tag（用户粘的是完整引用，例如
                // `docker.m.daocloud.io/library/nginx:1.27`）→ 直接选中它。
                // **成功和失败两条路都要做**：别的 registry 的 tags/list 可能
                // 要认证（实测 daocloud 401），那时列表是空的，
                // 而这个 tag 就是唯一能拉的东西。
                if let Some(tag) = input_tag.clone() {
                    shell.select_registry_tag(tag, cx);
                }
                shell.fill_tag_select(window, cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// 选中一个 tag → 后台量它的**下载体积**。
    ///
    /// 只给**选中的那一个**量：一次要两个请求（index → 本机架构那份 manifest，
    /// 实测内容协商省不掉），列表里 1339 个 tag 全量一遍就是 1339 次请求，
    /// 直接撞限流。
    pub fn select_registry_tag(&mut self, tag: String, cx: &mut Context<Self>) {
        self.state.registry.select_tag(&tag);
        cx.notify();

        let host = self.state.registry.host.clone();
        let repository = self
            .state
            .registry
            .loaded
            .clone()
            .unwrap_or_else(|| self.state.registry.repository.clone());
        // ⚠️ `tag` 要**两份**：一份移进后台任务（`fetch_size` 要它），
        // 另一份留给结果回来时的核对。不克隆的话 `tag` 会被移进内层协程，
        // 外层那个 `Some(tag.as_str())` 就成了"借用已移动的值"（CI 实测报 E0382）。
        let fetch_tag = tag.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    wslc_core::cmd::registry::fetch_size(&host, &repository, &fetch_tag)
                })
                .await;
            let _ = this.update(cx, |shell, cx| {
                // 结果回来时用户可能已经点了别的 tag —— 那这一次就**丢掉**。
                // 把 nginx 的体积挂到 alpine 上是比"暂时没有体积"更糟的事。
                if shell.state.registry.selected.as_deref() != Some(tag.as_str()) {
                    return;
                }
                match result {
                    Ok(size) => shell.state.registry.set_size(size.bytes, size.layers),
                    Err(e) => shell.state.registry.set_size_error(e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 常用镜像的快捷入口：填进输入框并**立刻查**。
    ///
    /// 一键到位 —— 用户点 `nginx` 的意图就是"看看 nginx 有哪些 tag"，
    /// 再让他点一次「查询」是多余的。
    pub fn apply_registry_preset(
        &mut self,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // ⚠️ 先挡掉"正在查"，**再**动输入框：反过来的话输入框已经变成新仓库名，
        // 而结果列表还是上一个仓库的 —— 那正是 §3.2 要避免的串台，
        // 只不过这次是我们自己造成的。
        if self.state.registry.loading {
            self.state.notify(Toast::info("上一个查询还没回来"));
            cx.notify();
            return;
        }
        let Some(form) = self.image_form.as_ref() else {
            return;
        };
        form.repository
            .update(cx, |state, cx| state.set_value(name.to_owned(), window, cx));
        self.query_registry(window, cx);
    }

    /// 「镜像」页点「拉取」：把将要拉的那个引用拉下来。
    ///
    /// v0.6.2 起这是**唯一**的拉取入口（原先还有「拉取镜像」弹窗）——
    /// 所以这里不再有"读输入框 + 校验"那一层，
    /// 引用由 [`RegistryState::selected_reference`] 从
    /// registry / 仓库名 / tag 三件东西拼出来。
    pub fn pull_registry_selection(&mut self, cx: &mut Context<Self>) {
        if self.state.pulling.is_some() {
            self.state
                .notify(Toast::info("已经有一个拉取在跑了，等它结束"));
            cx.notify();
            return;
        }
        let Some(reference) = self.state.registry.selected_reference() else {
            self.state
                .notify(Toast::error("先选一个 tag，或者在仓库名后面写上 `:tag`"));
            cx.notify();
            return;
        };
        self.start_pull(reference, cx);
    }

    /// 发起一次**流式** `wslc pull`（不碰输入框、不弹窗）。
    ///
    /// v0.6.2 之后拉取只有**一个**入口（「镜像」页那个「拉取」按钮），
    /// 所以这里收的是**拼好的引用**：[`RegistryState::selected_reference`]
    /// 把 registry / 仓库名 / tag 三件东西拼成 `host/repo:tag`。
    /// 流式进度 + 取消 + 收尾提示 + 刷新**只有这一份实现**。
    pub fn start_pull(&mut self, reference: String, cx: &mut Context<Self>) {
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
    ///
    /// 起始目录来自偏好（[`TerminalStartDir`](crate::prefs::TerminalStartDir)）：
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
    fn fill_image_select(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
    fn probe_image_facts(&mut self, image: String, window: &mut Window, cx: &mut Context<Self>) {
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
    fn prefill_from_image_preset(
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
                env.update(cx, |state, cx| state.set_value(format!("{key}="), window, cx));
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
            self.state
                .notify(Toast::error(format!("{key} 还没填值 —— 这个镜像第一次启动必须有它。请在「环境变量」那格里补上{hint}")));

            // 焦点直接送到环境变量那格，省得用户自己找
            if let Some(dialog) = self.create_dialog.as_ref() {
                let handle = dialog.env.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
            }
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
    /// 点列表里的名字进来。详情里放的是**列表行没有**的动作
    /// （强杀 / 执行命令 / 打开终端 / attach 主进程）+ 完整信息
    /// （端口含绑定地址、挂载、CPU / 内存 / 网络 I/O / PID）。
    ///
    /// ⚠️ 别把列表行已有的动作（启动 / 重启 / 停止 / 日志 / 删除）再搬进来 ——
    /// 同一件事两个入口，用户还得判断点哪个。见 `container_detail_overlay`。
    pub fn open_detail(&mut self, name: String, cx: &mut Context<Self>) {
        self.detail = Some(name);
        cx.notify();
    }

    /// 关闭容器详情弹窗。
    pub fn close_detail(&mut self, cx: &mut Context<Self>) {
        self.detail = None;
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

    /// 开关时间戳并重拉。
    pub fn toggle_logs_timestamps(&mut self, cx: &mut Context<Self>) {
        if let Some(logs) = self.state.logs.as_mut() {
            logs.timestamps = !logs.timestamps;
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

    /// 建数据库页的三个输入框（幂等）。
    ///
    /// 幂等很要紧：`open_database_page` 每次进来都会调它，而重建输入框会让
    /// 用户刚打的字和刚填的密码全丢。
    fn ensure_db_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.db_form.is_some() {
            return;
        }
        let user = cx.new(|cx| InputState::new(window, cx).placeholder("root"));
        let password = cx.new(|cx| InputState::new(window, cx).placeholder("root 的密码"));
        let sql = cx.new(|cx| InputState::new(window, cx).placeholder("SHOW DATABASES"));

        // 用户名预填 root：绝大多数人只填密码那一格。
        user.update(cx, |state, cx| state.set_value("root".to_owned(), window, cx));
        // 焦点给密码（最可能要填的就是它）。
        let handle = password.read(cx).focus_handle(cx);
        window.focus(&handle, cx);

        self.db_form = Some(DbPageForm {
            user,
            password,
            sql,
        });
    }

    /// 读连接卡上**此刻**的值。
    ///
    /// 「执行时用连接卡上的值」是刻意的：那份值就是用户眼睛看到的那个，
    /// 不存在"界面显示 A、实际用 B"的可能（`DbState::creds` 只是它的镜像）。
    fn read_db_form(&self, cx: &App) -> (String, String) {
        let Some(form) = self.db_form.as_ref() else {
            return ("root".to_owned(), String::new());
        };
        let user = form.user.read(cx).value().trim().to_owned();
        let user = if user.is_empty() {
            "root".to_owned()
        } else {
            user
        };
        (user, form.password.read(cx).value().to_owned())
    }

    /// 打开某个容器的数据库管理页（从容器列表行的「数据库」按钮进）。
    ///
    /// `image` 只用于显示和"用镜像名兜底判家族"；**权威判定**在
    /// [`Shell::load_db`] 里用 `inspect` 做。
    pub fn open_database_page(
        &mut self,
        name: String,
        image: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_db_form(window, cx);

        // 同一个容器就**不重建状态**：切走再回来时不该把连接信息清掉。
        let same_container = self
            .state
            .db
            .as_ref()
            .is_some_and(|db| db.container == name);

        if !same_container {
            let family =
                mysql_model::family_from_image(&image).unwrap_or(MysqlFamily::Mysql);
            self.state.db = Some(DbState::provisional(name, image, family));
            // 换了容器就要清密码：上一个容器的密码留在这儿会**连错库**
            // （两个容器恰好都有个 root 账号时，这个错误会非常难发现）。
            if let Some(form) = self.db_form.as_ref() {
                form.password
                    .update(cx, |state, cx| state.set_value(String::new(), window, cx));
                form.user
                    .update(cx, |state, cx| state.set_value("root".to_owned(), window, cx));
            }
            self.goto_page(Page::Database, cx);
            self.load_db(window, cx);
            return;
        }

        self.goto_page(Page::Database, cx);
        cx.notify();
    }

    /// 探测容器 + 读两张表（进页面、点「重新读取」都走这里）。
    ///
    /// 「重新读取」是**唯一**的刷新入口：连接卡上刚改的账号要靠它生效
    /// （不然用户改完密码还得猜"什么时候才算生效"）。
    pub fn load_db(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(db) = self.state.db.as_ref() else {
            return;
        };
        // 正在跑就不叠第二次（按钮那边也已经换成「读取中…」）。
        if db.busy {
            return;
        }
        let container = db.container.clone();
        // 用户此刻填在连接卡上的值。密码为空 = "还没填" → 用探测到的那个
        // （容器环境变量里的 `MYSQL_ROOT_PASSWORD` 就是给这种时候用的）。
        let (typed_user, typed_password) = self.read_db_form(cx);

        if let Some(db) = self.state.db.as_mut() {
            db.busy = true;
            db.blocked = None;
            db.clear_error();
        }
        cx.notify();

        let wslc = self.state.wslc.clone();
        cx.spawn_in(window, async move |this, cx| {
            let target = container.clone();
            let wslc_probe = wslc.clone();
            let probe = cx
                .background_executor()
                .spawn(async move {
                    wslc_core::cmd::mysql::detect(&wslc_probe, &target).map_err(|e| e.to_string())
                })
                .await;

            let probe = match probe {
                Ok(probe) => probe,
                Err(message) => {
                    // 探测就失败：页面只显示这一条（那些空表没有意义）
                    let _ = this.update_in(cx, |shell, _window, cx| {
                        let failure = DbFailure::from_text(message);
                        let summary = failure.summary();
                        if let Some(db) = shell.state.db.as_mut() {
                            if db.container == container {
                                db.busy = false;
                                db.loaded = false;
                                db.blocked = Some(failure);
                            }
                        }
                        // ⚠️ 提示条要 `&mut shell.state`，而上面那个 `db` 借用
                        // 必须**先结束**才能调它 —— 所以 summary 是 owned 的。
                        shell
                            .state
                            .notify(Toast::error(format!("读取失败：{summary}")));
                        cx.notify();
                    });
                    return;
                }
            };

            // 用哪个密码：**用户填了就用他的**，没填才用环境变量里那个
            let (user, password) = effective_db_creds(&probe, &typed_user, &typed_password);
            let creds = MysqlCreds {
                user: user.clone(),
                password: password.clone(),
                host: None,
                port: None,
            };
            let bin = probe.client.bin.clone();
            let lists = {
                let target = container.clone();
                let bin = bin.clone();
                let creds = creds.clone();
                cx.background_executor()
                    .spawn(async move {
                        let databases =
                            wslc_core::cmd::mysql::list_databases(&wslc, &target, &bin, &creds);
                        // 用户列表**单独**取：普通账号读不到 `mysql.user`
                        // （ERROR 1142），那不该把数据库列表一起判成失败。
                        let users = wslc_core::cmd::mysql::list_users(&wslc, &target, &bin, &creds);
                        (databases, users)
                    })
                    .await
            };

            let _ = this.update_in(cx, |shell, window, cx| {
                // 把这次实际用的账号同步到连接卡上：用户看得见"用的是哪个密码"
                // （从环境变量读出来的那个也要显示出来，否则他会以为没生效）
                if let Some(form) = shell.db_form.as_ref() {
                    form.user
                        .update(cx, |state, cx| state.set_value(user.clone(), window, cx));
                    form.password
                        .update(cx, |state, cx| state.set_value(password.clone(), window, cx));
                }

                let databases_ok = lists.0.is_ok();
                // ⚠️ 这一整块借着 `shell.state.db`，而下面的提示条要
                // `&mut shell.state` —— 借用没结束就调 `notify` 是编译错误，
                // 所以先把要说的那句话算成 owned 的，出了块再发。
                let failure_summary: Option<String> = {
                    let Some(db) = shell.state.db.as_mut() else {
                        cx.notify();
                        return;
                    };
                    if db.container != container {
                        // 已经切到别的容器了，这份结果丢掉
                        return;
                    }
                    db.apply_probe(&probe);
                    db.creds = creds.clone();
                    db.busy = false;
                    db.loaded = true;
                    match lists.0 {
                        Ok(databases) => db.databases = databases,
                        Err(message) => db.fail(message),
                    }
                    match lists.1 {
                        Ok(users) => db.users = users,
                        // 数据库列出来了、用户列表没列出来（多半是权限）：
                        // 照样把它说出来，但别覆盖掉数据库那边的成功。
                        Err(message) => {
                            if databases_ok {
                                db.error = Some(DbFailure::from_text(message));
                            }
                        }
                    }
                    db.error.as_ref().map(DbFailure::summary)
                };
                if let Some(summary) = failure_summary {
                    shell
                        .state
                        .notify(Toast::error(format!("读取失败：{summary}")));
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 只重读两张表（表结构变更之后调）。
    ///
    /// 和 [`Shell::load_db`] 分开：那个要重新跑 `inspect` + 客户端探测（两个进程），
    /// 而"刚建完一个库"只需要把列表拿回来。
    ///
    /// ⚠️ 它刻意**不收 `&mut Window`**：这一步不需要往输入框里写东西，
    /// 于是确认弹窗那条路（`confirm_pending` 的异步块里没有 window）也能直接调它。
    pub fn reload_db_lists(&mut self, cx: &mut Context<Self>) {
        let Some(db) = self.state.db.as_ref() else {
            return;
        };
        let container = db.container.clone();
        let bin = db.client.clone();
        let creds = db.creds.clone();
        if let Some(db) = self.state.db.as_mut() {
            db.busy = true;
        }
        cx.notify();

        let wslc = self.state.wslc.clone();
        cx.spawn(async move |this, cx| {
            let lists = {
                let target = container.clone();
                let bin = bin.clone();
                let creds = creds.clone();
                cx.background_executor()
                    .spawn(async move {
                        let databases =
                            wslc_core::cmd::mysql::list_databases(&wslc, &target, &bin, &creds);
                        let users = wslc_core::cmd::mysql::list_users(&wslc, &target, &bin, &creds);
                        (databases, users)
                    })
                    .await
            };

            let _ = this.update(cx, |shell, cx| {
                let databases_ok = lists.0.is_ok();
                if let Some(db) = shell.state.db.as_mut() {
                    if db.container != container {
                        return;
                    }
                    db.busy = false;
                    db.loaded = true;
                    match lists.0 {
                        Ok(databases) => db.databases = databases,
                        Err(message) => db.fail(message),
                    }
                    match lists.1 {
                        Ok(users) => db.users = users,
                        // 数据库列出来了、用户列表没列出来（多半是权限）：
                        // 照样说出来，但别覆盖掉数据库那边的成功。
                        Err(message) => {
                            if databases_ok {
                                db.error = Some(DbFailure::from_text(message));
                            }
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 切换数据库页的 tab。
    pub fn set_db_tab(&mut self, tab: DbTab, cx: &mut Context<Self>) {
        if let Some(db) = self.state.db.as_mut() {
            db.tab = tab;
        }
        cx.notify();
    }

    /// 打开一个动作弹窗（新建库 / 新建用户 / 改密码 / 授权）。
    ///
    /// 输入框只能在这里建：`InputState::new` 要 `&mut Window`，而这个函数
    /// 是从**点击回调**里调的（那里正好有 window）。
    pub fn open_db_dialog(
        &mut self,
        kind: DbDialogKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let database = cx.new(|cx| InputState::new(window, cx).placeholder("shop"));
        let charset = cx.new(|cx| InputState::new(window, cx).placeholder("留空 = 服务器默认（一般为 utf8mb4）"));
        let collation = cx.new(|cx| InputState::new(window, cx).placeholder("留空 = 跟着字符集"));
        let user = cx.new(|cx| InputState::new(window, cx).placeholder("app"));
        let host = cx.new(|cx| InputState::new(window, cx).placeholder("%（任意主机）"));
        let password = cx.new(|cx| InputState::new(window, cx).placeholder("密码"));
        let grant_db = cx.new(|cx| InputState::new(window, cx).placeholder("留空 = 先不授权"));

        // 预填：把行上带过来的用户名 / 主机填好（改密码 / 授权那两种）
        if let DbDialogKind::AlterPassword { user: u, host: h }
        | DbDialogKind::Grant { user: u, host: h } = &kind
        {
            user.update(cx, |state, cx| state.set_value(u.clone(), window, cx));
            host.update(cx, |state, cx| state.set_value(h.clone(), window, cx));
        } else {
            host.update(cx, |state, cx| state.set_value("%".to_owned(), window, cx));
        }
        // 焦点给"最该填的那一格"
        let first = match &kind {
            DbDialogKind::NewDatabase => &database,
            DbDialogKind::NewUser => &user,
            DbDialogKind::AlterPassword { .. } => &password,
            DbDialogKind::Grant { .. } => &grant_db,
        };
        let handle = first.read(cx).focus_handle(cx);
        window.focus(&handle, cx);

        self.db_dialog = Some(DbDialog {
            kind,
            database,
            charset,
            collation,
            user,
            host,
            password,
            grant_db,
            grant: GrantKind::All,
        });
        cx.notify();
    }

    /// 关掉动作弹窗（不提交）。
    pub fn close_db_dialog(&mut self, cx: &mut Context<Self>) {
        self.db_dialog = None;
        cx.notify();
    }

    /// 选授权级别。
    pub fn set_db_grant(&mut self, grant: GrantKind, cx: &mut Context<Self>) {
        if let Some(dialog) = self.db_dialog.as_mut() {
            dialog.grant = grant;
        }
        cx.notify();
    }

    /// 提交动作弹窗。
    ///
    /// 这里最重要的一条：**参数不合法就在本地拦下，不关弹窗** ——
    /// 用户就在那一格里，改一个字比重打一遍强（`AGENTS.md` §7.5 的
    /// 「空值等于没填」也是这个思路）。
    pub fn submit_db_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 先把要用的东西取成 owned 的（下面要改 `self`，不能再借着它）
        let prepared: Result<(String, &'static str), String> = {
            let Some(dialog) = self.db_dialog.as_ref() else {
                return;
            };
            let text = |input: &Entity<InputState>| input.read(cx).value().trim().to_owned();

            let sql = match &dialog.kind {
                DbDialogKind::NewDatabase => {
                    let charset = text(&dialog.charset);
                    let collation = text(&dialog.collation);
                    mysql_model::create_database_sql(
                        &text(&dialog.database),
                        (!charset.is_empty()).then_some(charset.as_str()),
                        (!collation.is_empty()).then_some(collation.as_str()),
                    )
                }
                DbDialogKind::NewUser => {
                    let grant_db = text(&dialog.grant_db);
                    let host = text(&dialog.host);
                    let grant = (!grant_db.is_empty()).then(|| (grant_db.as_str(), dialog.grant));
                    mysql_model::create_user_statements(
                        &text(&dialog.user),
                        &host,
                        &text(&dialog.password),
                        grant,
                    )
                }
                DbDialogKind::AlterPassword { user, host } => {
                    mysql_model::alter_user_password_sql(user, host, &text(&dialog.password))
                }
                DbDialogKind::Grant { user, host } => {
                    let grant_db = text(&dialog.grant_db);
                    if grant_db.is_empty() {
                        Err(wslc_core::Error::InvalidArgument(
                            "请填要授权给哪个数据库".to_owned(),
                        ))
                    } else {
                        mysql_model::grant_sql(&grant_db, user, host, dialog.grant)
                    }
                }
            };
            sql.map(|sql| (sql, dialog.kind.title()))
                .map_err(|e| e.to_string())
        };

        let (sql, title) = match prepared {
            Ok(prepared) => prepared,
            Err(message) => {
                // 不关弹窗：用户就在那一格上，改一个字比重打一遍强
                self.state.notify(Toast::error(message));
                cx.notify();
                return;
            }
        };

        let Some(db) = self.state.db.as_ref() else {
            return;
        };
        if db.busy {
            return;
        }
        let container = db.container.clone();
        let bin = db.client.clone();
        // 执行用的是**连接卡**上的账号（弹窗里的 user/host 是"被操作的对象"）
        let (user, password) = self.read_db_form(cx);
        let creds = MysqlCreds {
            user,
            password,
            host: None,
            port: None,
        };

        self.db_dialog = None;
        self.run_db_statement(container, bin, creds, sql, title, window, cx);
    }

    /// SQL 控制台：执行输入框里那条语句。
    ///
    /// 结果**原样**展示（`--table` 形态：stdout 不是 tty 时 mysql 默认是
    /// batch，不显式要表格的话控制台里会是一堆 tab）。
    pub fn run_db_console(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = self.db_form.as_ref() else {
            return;
        };
        let sql = form.sql.read(cx).value().trim().to_owned();
        if sql.is_empty() {
            self.state.notify(Toast::error("先写一条 SQL 再执行"));
            cx.notify();
            return;
        }
        let Some(db) = self.state.db.as_ref() else {
            return;
        };
        if db.busy {
            return;
        }
        let container = db.container.clone();
        let bin = db.client.clone();
        let (user, password) = self.read_db_form(cx);
        let creds = MysqlCreds {
            user,
            password,
            host: None,
            port: None,
        };

        if let Some(db) = self.state.db.as_mut() {
            db.busy = true;
            db.clear_error();
            db.last_sql = sql.clone();
            db.sql_output = None;
            db.sql_ran = false;
        }
        cx.notify();

        let wslc = self.state.wslc.clone();
        cx.spawn_in(window, async move |this, cx| {
            let target = container.clone();
            let result = {
                let bin = bin.clone();
                let creds = creds.clone();
                let sql = sql.clone();
                cx.background_executor()
                    .spawn(async move {
                        run_console_sql(&wslc, &target, &bin, &creds, &sql, None)
                            .map_err(|e| e.to_string())
                    })
                    .await
            };

            let _ = this.update_in(cx, |shell, _window, cx| {
                let mut reload = false;
                if let Some(db) = shell.state.db.as_mut() {
                    if db.container != container {
                        return;
                    }
                    db.busy = false;
                    db.sql_ran = true;
                    match result {
                        Ok(output) => {
                            db.sql_output = Some(output);
                            db.clear_error();
                            // 控制台可能刚改了库 / 表 —— 顺手把列表刷新一次。
                            // 失败时列表不会变，就不白跑一趟了。
                            reload = true;
                        }
                        Err(message) => {
                            // SQL 报错**不算**页面级错误：它就发生在控制台里，
                            // 显示在控制台下面比顶到页头更贴合上下文
                            db.sql_output = None;
                            db.fail(message);
                        }
                    }
                }
                if reload {
                    shell.reload_db_lists(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 从数据库管理页回到容器列表。
    ///
    /// **不**清 `state.db`：下次再进来时连接信息与刚读到的两张表都还在
    /// （省掉一次探测 + 两次查询）。
    pub fn close_database_page(&mut self, cx: &mut Context<Self>) {
        self.goto_page(Page::Containers, cx);
        cx.notify();
    }

    /// 在后台执行一条 DDL，成功后重读两张表。
    ///
    /// 和 [`Shell::run_db_console`] 的区别：那个把结果显示在控制台里，
    /// 这个只报一句成功 / 失败（表单提交的路径上没有"结果区"）。
    fn run_db_statement(
        &mut self,
        container: String,
        bin: String,
        creds: MysqlCreds,
        sql: String,
        title: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(db) = self.state.db.as_mut() {
            db.busy = true;
            db.clear_error();
        }
        self.state.notify(Toast::info(format!("正在{title}…")));
        cx.notify();

        let wslc = self.state.wslc.clone();
        cx.spawn_in(window, async move |this, cx| {
            let target = container.clone();
            let result = {
                let bin = bin.clone();
                let creds = creds.clone();
                let sql = sql.clone();
                cx.background_executor()
                    .spawn(async move {
                        run_sql(&wslc, &target, &bin, &creds, &sql, None, SqlMode::Batch)
                            .map(|_| ())
                            .map_err(|e| e.to_string())
                    })
                    .await
            };

            let _ = this.update_in(cx, |shell, _window, cx| {
                let mut ok = false;
                if let Some(db) = shell.state.db.as_mut() {
                    if db.container == container {
                        db.busy = false;
                        match &result {
                            Ok(()) => {
                                db.clear_error();
                                ok = true;
                            }
                            Err(message) => db.fail(message.clone()),
                        }
                    }
                }
                match &result {
                    Ok(()) => shell.state.notify(Toast::success(format!("{title}成功"))),
                    Err(message) => {
                        // 给用户看的是"该怎么办"（分类给的），原文留在页面上
                        let failure = DbFailure::from_text(message.clone());
                        shell
                            .state
                            .notify(Toast::error(format!("{title}失败：{}", failure.summary())));
                    }
                }
                if ok {
                    shell.reload_db_lists(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    // -- 在新终端窗口里打开的东西 --------------------------------------------

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
        // 放在最前面：`busy` 那条提前返回也不该让界面上的那份"谁在吊着它"
        // 的信息过期（发行版详情弹窗里那句说明读的就是 `state.kept_alive`）。
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

    /// 这个危险操作会不会和正在跑的长任务打架？会的话给出拒绝的理由。
    ///
    /// 放在 [`Shell::confirm_pending`] 里而不是渲染层：渲染层决定"按钮画不画"
    /// 会随刷新闪来闪去（安装开始/结束都在变），而**点下去的那一刻**才是
    /// 真正要判的地方 —— 弹窗可能是几分钟前打开的，那期间安装可能已经开始了。
    fn blocking_reason(&self, action: &ConfirmAction) -> Option<String> {
        match action {
            // 「关停全部」压的是 `wsl --shutdown`，作用在所有发行版上，
            // 会把安装 / 导出正在跑的 `wsl.exe` 一起掐掉。
            // 名义上"关停发行版"不该影响正在安装的那个（它还没注册完），
            // 但它掐的是整个 WSL 服务 —— 实测里重定位那一步被打断
            // 等于把刚装好的删掉（见 `cmd::install` 的模块说明）。
            ConfirmAction::Distro(DistroAction::ShutdownAll) => self
                .state
                .long_task()
                .map(|what| format!("{what} —— 这时「关停全部」会把它一起掐掉，等它结束再动")),
            // 动某一个发行版：只在这个动作碰的正是那个正在被安装/导出的
            // 发行版时才拦（别的一律放行 —— 安装要几十分钟，把整页操作
            // 全锁住比这个 bug 更烦人）。`distro_name()` 对 `ShutdownAll`
            // 返回 `None`，上面那条已经处理过它了。
            ConfirmAction::Distro(other) => other.distro_name().and_then(|name| {
                self.state.busy_with(name).map(|what| {
                    format!("{name} 正在{what} —— 这时对它做别的写操作会把那一步弄坏，等它结束再动")
                })
            }),
            // 容器域和发行版安装互不相干：一个走 `wslc.exe`、一个走 `wsl.exe`，
            // 各自的发行版/会话也不重叠。拦下来只会平白挡住正常的容器操作。
            //
            // 「终止会话」也归在这里，理由同上：它终止的是 **wslc 的容器会话**，
            // 而安装 / 导出动的是 `wsl.exe` 注册的**发行版**（新实例的 `--import`
            // 会新建一个自己的发行版），两者不共享进程也不共享文件。
            // 唯一的交集是它们都要占 CPU/IO —— 那不该成为"禁止操作"的理由。
            ConfirmAction::Container(_) => None,
        }
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

        // 确认框**无论如何都要关掉**（上面已经 take 走了）——
        // 拒绝执行时留一个"点了没反应又关不掉"的弹窗是最糟的结果。
        if let Some(reason) = self.blocking_reason(&action) {
            self.state.notify(Toast::error(reason));
            cx.notify();
            return;
        }

        // 发行版被删掉之后它的详情弹窗就没有对象了 —— 现在先判断好，
        // 待会儿 `action` 要移进异步块。
        let close_detail = match &action {
            ConfirmAction::Distro(DistroAction::Unregister { name, .. }) => {
                self.distro_detail.as_deref() == Some(name.as_str())
            }
            _ => false,
        };

        // 删库 / 删用户之后要重读数据库页那两张表 —— 容器的列表刷新管不到它们。
        // 这里先算好（`action` 马上要移进异步块）。
        let reload_db = matches!(
            action,
            ConfirmAction::Container(
                PendingAction::DropDatabase { .. } | PendingAction::DropUser { .. }
            )
        );

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

                // 删库 / 删用户：重读数据库页那两张表（成功失败都读一次 ——
                // 失败了也要让用户看到"它到底还在不在"）。
                if reload_db {
                    shell.reload_db_lists(cx);
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
    /// `window` 是给**带控件的页面**准备的：「添加实例」有输入框和两个下拉框，
    /// 而 `InputState::new` / `SelectState::new` 都要 `&mut Window` —— 所以必须
    /// **在点击时**把表单建好。在渲染时建会每帧重建一次控件，字都打不进去。
    ///
    /// ⚠️ 「镜像」页现在**不需要** window：v0.6.3 把拉取表单挪进了弹窗，
    /// 它是在**点「拉取镜像…」那一刻**建的（`SelectState` 同理）。
    pub fn set_page(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        if page.needs_window_to_enter() {
            // 「添加实例」那一套：表单 + 两份清单的预取。
            //
            // ⚠️ 必须**按页判断**，不能只看 `needs_window_to_enter()`：
            // 「数据库管理」也是要 window 的页（连接卡有输入框），
            // 但它不需要装发行版的那份表单，更不该顺手去拉商店 / 镜像源清单。
            if page == Page::AddInstance {
                self.ensure_install_form(window, cx);

                // 进页面就把两份清单在后台拉起来：用户切到「微软商店」或
                // 「在线发行版（镜像源）」时列表**已经在了**，不用先点一次「刷新」。
                // 两个请求都很小（商店清单十几 KB、镜像清单 50 KB），
                // 而且各自的函数里会挡住重复请求。**不在这里同步拉**：那会卡住界面。
                if self.state.online.items.is_empty() && !self.state.online.loading {
                    self.refresh_online_list(window, cx);
                }
                if self.state.mirrors.offers.is_empty() && !self.state.mirrors.loading {
                    self.refresh_mirror_catalog(window, cx);
                } else if self.state.mirrors.chosen.is_none() && !self.state.mirrors.probing {
                    // 清单已经在手（上次进来拉过）、但还没测速过 —— 补一次。
                    // 否则"换个发行版页"回来会发现 URL 还是空的（校验会挡住提交）。
                    self.probe_mirrors(cx);
                }
            }
            // 「数据库管理」：连接卡那三个输入框（懒创建，幂等）
            if page == Page::Database {
                self.ensure_db_form(window, cx);
            }
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
        after: AfterPick,
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
                        if after == AfterPick::InstallDirIsNowUserChosen {
                            // ⚠️ 这行**只能在真的选到东西时**执行。
                            //
                            // 原先它写在 `browse_install_dir` 里、`spawn_picker`
                            // 调用之后，于是"点了浏览又点了取消"也会把它清掉：
                            // 用户什么都没选，却永久失去了"安装目录跟着发行版名
                            // 联动"，之后只能自己手敲路径。
                            // （选择器还可能因为 `picking` 已经在跑而直接返回，
                            // 那种情况下更不该动。）
                            if let Some(form) = shell.install_form.as_mut() {
                                form.last_derived_dir = None;
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
        // 切到镜像源就顺手开始测速（清单已经在手、还没测过的话）——
        // 用户不用先点「刷新清单」，也不用等下一次进页面。
        if source.is_mirror()
            && self.state.mirrors.chosen.is_none()
            && !self.state.mirrors.probing
            && !self.state.mirrors.offers.is_empty()
        {
            self.probe_mirrors(cx);
        }
        cx.notify();
    }

    /// 「微软商店」装不上时的一键出口：换成镜像源，并尽量选中同一个发行版。
    ///
    /// # 为什么要有这个
    ///
    /// 商店那条路**依赖 `wsl.exe` 自己的网络**：实测这台机器上
    /// `wsl -l -o` 被 `raw.githubusercontent.com` 拦掉（500）时，商店与
    /// `--web-download` 会双双报 `Wsl/InstallDistro/WININET_E_CANNOT_CONNECT`，
    /// 而镜像源是**面板自己**用 `curl.exe` 从国内镜像站下载（同一时刻清华 200），
    /// 完全不经过 wsl 的网络。同一个发行版往往在镜像清单里也有
    /// （Ubuntu / Debian / Alpine / Arch / Fedora / Rocky / Kali / …），
    /// 所以直接帮他切过去、并选中对应的那一版，比让他自己去翻清单强。
    pub fn switch_to_mirror_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 在线清单里选过的那个 id（没选过就用输入框里的名字）
        let online_id = self
            .install_form
            .as_ref()
            .map(|form| {
                form.online_id
                    .clone()
                    .unwrap_or_else(|| form.name.read(cx).value().trim().to_owned())
            })
            .unwrap_or_default();

        let matched = self.state.mirrors.match_online_id(&online_id);
        if let Some(offer) = matched.clone() {
            self.state.mirrors.select_distro(offer);
        }
        self.set_install_source(InstallSourceKind::Mirror, cx);
        // 下拉框也要跟着选中（否则它显示的还是上一条），然后**自动测速** ——
        // 和用户自己在下拉框里选一遍是同一条路。
        self.fill_mirror_select(window, cx);
        self.probe_mirrors(cx);

        match matched {
            Some(offer) => self.state.notify(Toast::success(format!(
                "已切到镜像源，并选中了「{offer}」—— 正在自动测速挑最快的镜像"
            ))),
            None => self.state.notify(Toast::info(
                "已切到镜像源 —— 在清单里挑一个版本，或填自定义下载地址",
            )),
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
    pub fn toggle_web_download(&mut self, cx: &mut Context<Self>) {
        if let Some(form) = self.install_form.as_mut() {
            form.web_download = !form.web_download;
            cx.notify();
        }
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

        // ⚠️ `Prefs` 是**整体**写回磁盘的：这里必须带上其余字段，
        // 只填 install_dir 会把它们重置掉。用 `..` 而不是手抄字段 ——
        // 手抄的那份在加第 4 个字段时一定会漏（终端起始目录就是第 4 个）。
        let prefs = crate::prefs::Prefs {
            install_dir: Some(dir.clone()),
            ..self.state.prefs.clone()
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
    /// 选完之后要把 [`InstallForm::last_derived_dir`] 清掉：那之后这个名字联动
    /// 就不能再动它了 —— 用户亲手选的目录被我们改掉是最恼人的一类 bug。
    ///
    /// ⚠️ 但**只能真的选到东西时**才清，所以这件事交给 `spawn_picker`
    /// 在选中的分支里做（见 [`AfterPick::InstallDirIsNowUserChosen`]）。
    /// 写在这一行之后的话，"点了取消"也会清 —— 用户什么都没选，
    /// 却永久失去了名字联动。
    pub fn browse_install_dir(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(input) = self
            .install_form
            .as_ref()
            .map(|form| form.install_dir.clone())
        else {
            return;
        };
        self.spawn_picker(
            &input,
            true,
            "目录",
            &[],
            "选择安装目录",
            AfterPick::InstallDirIsNowUserChosen,
            window,
            cx,
        );
    }

    // -- 添加实例：在线清单与镜像站 ----------------------------------------

    /// 拉在线可安装发行版的清单（微软商店那条路）。
    ///
    /// 先问 `wsl --list --online`，拉不到就自己拉微软那份
    /// `DistributionInfo.json`（本机 `raw.githubusercontent.com` 不通，
    /// 所以兜底那条路是**常态**，见 `cmd::distro::online_distros`）。
    ///
    /// 拉回来要**填进下拉框**，而 `SelectState::set_items` 要 `&mut Window`
    /// —— 所以这里走 `spawn_in` + `update_in`（和文件选择器同一条路，
    /// 见 `AGENTS.md` §7.2）。
    pub fn refresh_online_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.online.loading {
            return;
        }
        self.state.online.loading = true;
        self.state.online.error.clear();
        cx.notify();

        let wsl = self.state.wsl.clone();
        cx.spawn_in(window, async move |this, cx| {
            let listing = cx
                .background_executor()
                .spawn(async move { wslc_core::cmd::distro::online_distros(&wsl) })
                .await;

            let _ = this.update_in(cx, |shell, window, cx| {
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
                shell.fill_store_select(window, cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// 把在线清单填进「微软商店」的下拉框。
    ///
    /// 显示**友好名 + id**：`Ubuntu 24.04 LTS (Ubuntu-24.04)`。两个都要写 ——
    /// 用户以为自己装的是 "Ubuntu 24.04 LTS"，而装完列表里出现的是
    /// `Ubuntu-24.04`（那才是 `wsl --install -d` 认的名字），
    /// 提前写在选项里比事后解释强。
    fn fill_store_select(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = self.install_form.as_ref() else {
            return;
        };
        let select = form.store_select.clone();
        // `set_items` 会把选中项清掉，而用户没做错什么 —— 换清单之后
        // 要把原来选的那个**重新选上**（它还在新清单里的话）。
        let keep = form.online_id.clone();

        let items: Vec<DropdownItem> = self
            .state
            .online
            .items
            .iter()
            .map(|item| {
                let label = if item.label == item.id {
                    item.id.clone()
                } else {
                    format!("{} ({})", item.label, item.id)
                };
                DropdownItem::new(label, item.id.clone())
            })
            .collect();

        select.update(cx, |state, cx| {
            state.set_items(items, window, cx);
            if let Some(id) = keep {
                state.set_selected_value(&id, window, cx);
            }
        });
    }

    /// 选中在线清单里的一项。
    ///
    /// # 名字默认填**清单里的 id**，而不是友好名
    ///
    /// 参考实现填的是友好名（`Ubuntu 24.04 LTS` → `Ubuntu-24-04-LTS`）。
    /// 那样每次在线安装都会走"导出 → 注销 → 导入"的重定位（多拷几个 GB），
    /// 因为它和 `wsl --install -d` 认的 id 对不上。
    /// 我们的默认值是 id（`Ubuntu-24.04`，本身就是合法名字），于是**不需要改名**；
    /// 位置由装完之后那一步 `--manage <id> --move <目录>` 补正
    /// （在线安装**永远不带 `--location`**，理由见 `model::install::plan` 里那张表）。
    /// 用户想改名随时能改，界面上也会提示那会多一次全量拷贝。
    pub fn pick_online_distro(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(form) = self.install_form.as_mut() {
            // 只在"名字还是我们填的"时候动它：空着、或者还是上一次选中的那个 id。
            // 用户自己敲过名字就不该被覆盖（和 `last_derived_dir` 是同一条原则）。
            let current = form.name.read(cx).value().trim().to_owned();
            let ours = current.is_empty() || Some(current.as_str()) == form.online_id.as_deref();
            form.online_id = Some(id.clone());
            if ours {
                form.name
                    .update(cx, |state, cx| state.set_value(id.clone(), window, cx));
            }
        }
        self.state.online.select(id);
        self.resync_install_dir(window, cx);
        cx.notify();
    }

    /// 拉「在线发行版（镜像源）」的清单。
    ///
    /// 两个 HTTP 请求（先问清单地址、再取清单）走 `curl.exe`，**必须在后台**：
    /// 最长 25 秒 × 2，放界面上就是"点进去卡半分钟"（`AGENTS.md` §7.1）。
    ///
    /// 清单到手之后**自动开始测速**（见 [`Shell::probe_mirrors`]）：
    /// 参考实现是"选中即测速择优"，没有"探测"按钮 —— 而我们要保证
    /// 用户点「创建安装」时 URL 已经定了（预览里的命令必须就是真正要跑的）。
    pub fn refresh_mirror_catalog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.mirrors.loading {
            return;
        }
        self.state.mirrors.loading = true;
        self.state.mirrors.load_error = None;
        cx.notify();

        cx.spawn_in(window, async move |this, cx| {
            let fetched = cx
                .background_executor()
                .spawn(async { wslc_core::cmd::catalog::fetch() })
                .await;

            let _ = this.update_in(cx, |shell, window, cx| {
                shell.state.mirrors.loading = false;
                match fetched {
                    Ok(catalog) => {
                        let count = catalog.offers.len();
                        shell.state.mirrors.set_offers(
                            catalog.offers,
                            catalog.origin.label().to_owned(),
                            catalog.updated,
                        );
                        shell.fill_mirror_select(window, cx);
                        shell
                            .state
                            .notify(Toast::success(format!("拿到 {count} 个可装的发行版")));
                        shell.probe_mirrors(cx);
                    }
                    Err(e) => {
                        // 清单拉不到时**留着旧清单**：旧的还能用，总比空的强
                        shell.state.mirrors.load_error = Some(e.clone());
                        shell.state.notify(Toast::error(e));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 把镜像清单填进「在线发行版」的下拉框。
    ///
    /// 显示的是参考实现的写法：`Ubuntu 24.04 (12 镜像源)` —— **只给数量**，
    /// 不列镜像站名（装的时候会自动测速挑最快的，用户不需要先看一遍）。
    fn fill_mirror_select(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = self.install_form.as_ref() else {
            return;
        };
        let select = form.mirror_select.clone();
        let keep = self.state.mirrors.selected_offer().map(|offer| offer.id());

        let items: Vec<DropdownItem> = self
            .state
            .mirrors
            .offers
            .iter()
            .map(|offer| DropdownItem::new(offer.menu_label(), offer.id()))
            .collect();

        select.update(cx, |state, cx| {
            state.set_items(items, window, cx);
            if let Some(id) = keep {
                state.set_selected_value(&id, window, cx);
            }
        });
    }

    /// 切换镜像源那块选中的发行版 —— 换完**立刻**重新测速。
    ///
    /// 参考实现就是"选中即测速择优"（它在下拉框旁边写着「自动测速择优」），
    /// 用户不用再点一次"探测"。测速是并发的（见 [`Shell::probe_mirrors`]），
    /// 最多也就几秒。
    pub fn select_mirror_distro(&mut self, id: String, cx: &mut Context<Self>) {
        self.state.mirrors.select_distro(id);
        self.probe_mirrors(cx);
        cx.notify();
    }

    /// 自动测速：**并发** HEAD 一遍候选，挑最快的那个。
    ///
    /// # 为什么并发
    ///
    /// 一条发行版最多十几个候选、每个 8 秒上限 —— 串行最坏是一分半，
    /// 而这是"选中发行版就自动开始"的动作（用户没得选，只能干等）。
    /// 参考实现也是并发（`futures_util::future::join_all`）。
    ///
    /// 这里用 `std::thread::scope`：探测本身是**阻塞**的 curl 调用，
    /// 线程是最自然的载体，而 scope 保证返回前全部 join 掉
    /// （不用管线程生命周期，也不需要线程池）。
    ///
    /// # 结果只用来挑一个
    ///
    /// 镜像站列表**不再显示**（用户要求"不需要那些什么清华源之类的"）：
    /// 界面只留一行状态，名字进安装日志 —— 出问题时那才是排查的地方。
    /// 成功也**不发提示条**：这个是自动触发的，换一次发行版弹一次太吵。
    pub fn probe_mirrors(&mut self, cx: &mut Context<Self>) {
        if self.state.mirrors.probing {
            return;
        }
        // 没有清单（还没拉 / 拉失败）时**安静地什么都不做**：
        // 这个函数现在是自动触发的，弹一句"先拉清单"只会莫名其妙。
        let Some(offer) = self.state.mirrors.selected_offer() else {
            return;
        };

        let candidates = offer.candidates();
        let release = offer.version.clone();

        self.state.mirrors.probing = true;
        self.state.mirrors.error = None;
        self.state.mirrors.results.clear();
        self.state.mirrors.chosen = None;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let probed = cx
                .background_executor()
                .spawn(async move {
                    let probes: Vec<Option<mirrors::Probe>> = std::thread::scope(|scope| {
                        let handles: Vec<_> = candidates
                            .iter()
                            .map(|candidate| {
                                scope.spawn(move || {
                                    // 纵深防御：`candidate.url` 来自远端清单，而它会作为
                                    // **裸 argv** 交给 curl（见 `mirrors::is_http_url`）。
                                    // `parse_catalog` 已经滤过一遍，界面这条探测路径是
                                    // 独立拼的，再挡一次。
                                    if !mirrors::is_http_url(&candidate.url) {
                                        return None;
                                    }
                                    let args = mirrors::probe_args(&candidate.url);
                                    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
                                    wslc_core::cli::run_helper_with_hint(
                                        "curl.exe",
                                        "curl.exe",
                                        wslc_core::cli::CURL_NOT_FOUND_HINT,
                                        &refs,
                                        Duration::from_secs(20),
                                    )
                                    .ok()
                                    .and_then(|out| mirrors::parse_probe(&out.stdout))
                                })
                            })
                            .collect();

                        handles
                            .into_iter()
                            .filter_map(|handle| handle.join().ok())
                            .collect()
                    });

                    candidates.into_iter().zip(probes).collect::<Vec<_>>()
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
                // 按耗时排（可用的在前）—— 探测是按清单顺序发起的，
                // 不排一下最快的那条会散落在中间
                shell.state.mirrors.sort_results_by_speed();

                match mirrors::pick_fastest(&probed) {
                    Some((candidate, probe)) => {
                        shell.state.mirrors.chosen = Some(MirrorChoice {
                            site: candidate.site.clone(),
                            url: candidate.url,
                            release: release.clone(),
                            format: candidate.format,
                            bytes: probe.bytes,
                        });
                    }
                    None => {
                        shell.state.mirrors.chosen = None;
                        shell.state.mirrors.error = Some(
                            "这个发行版在所有镜像上都拿不到（镜像站的文件会随版本改名）——\
                             换一个版本，或者用「自定义下载地址」"
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
        let spec = self.install_form.as_ref().map(|form| {
            form.to_spec(
                cx,
                &self.state.mirrors,
                &self.state.online,
                &self.state.distro_names(),
                self.state.prefs.install_dir.as_deref(),
            )
        });
        let Some(spec) = spec else {
            self.state.notify(Toast::error("表单尚未创建"));
            cx.notify();
            return;
        };

        let ctx = self.state.plan_context();
        let dir = spec.effective_install_dir(&ctx).unwrap_or_default();
        let check = self.state.preflight(&spec, dir_non_empty(&dir));
        if !check.ok() {
            self.state.notify(Toast::error(
                check.error_text().unwrap_or_else(|| "参数有误".to_owned()),
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
        tracing::info!("安装 {}：{}", plan.name, plan.preview_lines().join("  →  "));

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

        // ⚠️ 两个名字都要记：改名安装的中间态里 `wsl -l -v` 上是**源名**
        // （比如 `Ubuntu-26.04`），而重定位（导出 → 注销 → 导入）恰好在那段时间里跑。
        // 只记最终名字的话，那一行不受任何互斥保护（见 `AppState::busy_with`）。
        self.state.installing = Some(InstallProgress::new(plan.name.clone(), plan.source_names()));
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
            None => self.state.notify(Toast::error("当前没有正在进行的安装")),
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
            // 用 `?` 而不是 `let ... else { return None }`：两者等价，
            // 而这一个函数返回的就是 `Option`，`?` 更短也更难写错
            // （clippy 的 `question_mark` 报的就是这条）。
            let run = self.install_run.as_ref()?;
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
        // ⚠️ `Prefs` 是要整体写回磁盘的：用 `..` 带上其余字段
        // （以前是手抄 refresh_secs/theme/install_dir —— 加第 4 个字段
        // 「终端起始目录」时那三行就会漏掉它，用户设过的终端起点被一次
        // 刷新间隔改动悄悄抹掉）。
        let prefs = crate::prefs::Prefs {
            refresh_secs: secs,
            ..self.state.prefs.clone()
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

    // -- 新终端的起始目录 ----------------------------------------------------

    /// 设置「新终端的起始目录」，并立即写入偏好文件。
    ///
    /// 只影响**之后打开**的终端：已经在跑的那些窗口的 cwd 是它们自己的事，
    /// 这里改不了（界面上也是这么写的）。
    pub fn set_terminal_dir(
        &mut self,
        dir: crate::prefs::TerminalStartDir,
        cx: &mut Context<Self>,
    ) {
        let prefs = crate::prefs::Prefs {
            terminal_dir: dir,
            ..self.state.prefs.clone()
        };
        if self.state.prefs == prefs {
            return;
        }
        let described = prefs.terminal_dir.describe();
        self.state.prefs = prefs;

        match self.state.prefs.save() {
            Ok(()) => {
                tracing::info!("新终端的起始目录已改为 {described}");
                self.state
                    .notify(Toast::success(format!("新终端将从「{described}」开始")));
            }
            Err(e) => {
                tracing::warn!("保存偏好失败：{e}");
                // 内存里的值仍然生效，只是重启后会丢。
                self.state.notify(Toast::error(format!(
                    "已改为「{described}」，但保存失败：{e}"
                )));
            }
        }
        cx.notify();
    }

    /// 打开「自定义起始目录」的输入框（幂等）。
    ///
    /// **必须**在有 `&mut Window` 的地方调用 —— `InputState::new` 要它
    /// （`AGENTS.md` §7.5：渲染时现建输入框会让用户一个字都打不进去）。
    ///
    /// 建完之后**一直留着**，再把焦点给过去：用户点完按钮就能直接敲。
    pub fn edit_terminal_dir(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.terminal_dir_input.is_none() {
            // 预填当前的自定义路径（不是自定义档位时留空，走占位符提示格式）。
            let current = match &self.state.prefs.terminal_dir {
                crate::prefs::TerminalStartDir::Custom(dir) => dir.clone(),
                _ => String::new(),
            };
            let input = cx.new(|cx| {
                InputState::new(window, cx).placeholder("/home/wt/project 或 ~")
            });
            // `set_value` 也要 `&mut Window`，所以只能在这儿做。
            input.update(cx, |state, cx| state.set_value(current, window, cx));
            self.terminal_dir_input = Some(input);
        }

        // `InputState::focus` 是 `pub(crate)`，外部只能走 `focus_handle`。
        if let Some(input) = self.terminal_dir_input.clone() {
            let handle = input.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }
        cx.notify();
    }

    /// 把输入框里的路径应用成「自定义起始目录」。
    ///
    /// 校验是纯函数（[`crate::prefs::parse_custom_dir`]），所以"报错的红字"
    /// 和"真的存下去的值"用的是同一套规则。校验不过就**只提示、不改偏好** ——
    /// 把非法值存进去，用户下次点「打开终端」会得到一个落点随机的窗口
    /// （`wsl.exe` 对不存在的 `--cd` 不报错，只落在 `/`）。
    pub fn apply_terminal_dir(&mut self, cx: &mut Context<Self>) {
        let Some(input) = self.terminal_dir_input.clone() else {
            return;
        };
        let typed = input.read(cx).value().trim().to_owned();
        match crate::prefs::parse_custom_dir(&typed) {
            Ok(dir) => self.set_terminal_dir(crate::prefs::TerminalStartDir::Custom(dir), cx),
            Err(e) => {
                self.state.notify(Toast::error(e));
                cx.notify();
            }
        }
    }

    /// 切主题：**持久化 + 两套配色一起换**。
    ///
    /// 两套配色指：
    ///
    /// 1. gpui-component 自己的主题 —— `Input` / `Button` / `Select` 这些控件
    ///    只认它（不切的话输入框还是白底浅灰字，实机看就是"字打了但读不出来"）；
    /// 2. 我们自己画的那些 `div` —— 走 [`crate::theme::set_mode`] 那个全局。
    ///
    /// 「跟随系统」用**缓存**的系统主题立刻切一下（点下去立刻有反应），
    /// 同时在后台再读一次：用户很可能刚在系统设置里改过主题才来点这个按钮。
    /// **不在点击回调里同步读**（起 `reg.exe`，几十毫秒，但那是界面线程 ——
    /// `AGENTS.md` §7.1）。
    pub fn set_theme(&mut self, theme: crate::prefs::ThemePref, cx: &mut Context<Self>) {
        let changed = self.state.prefs.theme != theme;
        self.state.prefs.theme = theme;

        if let Some(is_dark) = theme.fixed_is_dark() {
            self.apply_theme(is_dark, cx);
            if changed {
                self.persist_theme(theme, cx);
            }
            return;
        }

        // 跟随系统
        let cached = self.state.system_is_dark.unwrap_or(true);
        self.apply_theme(theme.resolve_is_dark(cached), cx);
        if changed {
            self.persist_theme(theme, cx);
        }

        cx.spawn(async move |this, cx| {
            let probed = cx
                .background_executor()
                .spawn(async { wslc_core::cmd::win_theme::system_is_dark() })
                .await;
            let _ = this.update(cx, |shell, cx| {
                shell.state.system_is_dark = probed;
                // 探到的和缓存不一致 → 现在就跟上（用户多半刚改过系统主题）
                if let Some(is_dark) = probed {
                    if shell.state.prefs.theme.fixed_is_dark().is_none() {
                        shell.apply_theme(is_dark, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 真正落地一次主题切换（两套配色）。
    fn apply_theme(&mut self, is_dark: bool, cx: &mut Context<Self>) {
        // `theme::apply` 既设我们自己的全局模式，又给出组件库要的那个模式 ——
        // 两套配色只能一起切，写在一处才不会漏。
        crate::theme::apply(is_dark, cx);
        cx.notify();
    }

    /// 写回偏好文件（+ 提示）。
    fn persist_theme(&mut self, theme: crate::prefs::ThemePref, cx: &mut Context<Self>) {
        match self.state.prefs.save() {
            Ok(()) => {
                tracing::info!("主题已改为{}", theme.label());
                self.state
                    .notify(Toast::success(format!("主题已改为{}", theme.label())));
            }
            Err(e) => {
                tracing::warn!("保存偏好失败：{e}");
                // 内存里的值仍然生效，只是重启后会丢。
                self.state.notify(Toast::error(format!(
                    "已改为{}，但保存失败：{e}",
                    theme.label()
                )));
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

        // 提示条：主内容列里的一个**布局项**，不是浮层。
        //
        // 原先它是挂在根元素上的 `.absolute().bottom(20).left(0).right(0)`，
        // 横跨整个窗口宽度。v0.5.10 把它从右下角挪到底部居中，理由是右下角
        // 正好压着「开始安装」；但居中之后它变成压住**底部中间**的东西了 ——
        // 本机实测过，底部那一带的按钮会被它盖住。
        //
        // 浮层方案无论放哪儿都必然压住**某个**控件（这正是 v0.5.10 那次
        // 修复的成因），所以这里改成布局项：提示条出现时主内容区让出一条
        // 高度，谁都不会被盖住，也不需要再挑位置。
        // 代价是它出现 / 消失时内容会上下动一下 —— 比"有按钮点不到"好。
        let toast: AnyElement = match &state.toast {
            None => div().into_any_element(),
            Some(t) => {
                let entity = entity.clone();
                let color = match t.kind {
                    ToastKind::Info => theme::primary(),
                    ToastKind::Success => theme::success(),
                    ToastKind::Error => theme::danger(),
                };
                h_flex()
                    .w_full()
                    // `flex_none`：别让它被上面那个 `flex_1` 的滚动区挤扁。
                    .flex_none()
                    .justify_center()
                    .px_5()
                    .pb_4()
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
                                    // 见 `util::strip_md`：提示条里也有写
                                    // `**不完整**` 这类标记的文案。
                                    .child(crate::util::strip_md(&t.text)),
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

        // 「拉取镜像」弹窗。表单住在 `Shell::image_form` 里（不在 `state` 里），
        // 所以只有这里能画它。
        let pull_dialog: AnyElement = match (self.pull_open, self.image_form.as_ref()) {
            (true, Some(form)) => views::pull_dialog_overlay(form, state, &entity),
            // `pull_open` 为真时表单一定在（`open_pull_dialog` 先建再置位）——
            // 真走到这里也只是什么都不画，不 panic。
            _ => div().into_any_element(),
        };

        // 创建容器弹窗。要读 8 个 InputState 的值来做等效命令预览，
        // 所以得把 `cx` 传下去。
        let create_dialog: AnyElement = match &self.create_dialog {
            None => div().into_any_element(),
            Some(dialog) => views::create_dialog_overlay(dialog, &self.state, &entity, cx),
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
        let wslconf_dialog: AnyElement = views::wslconf_overlay(self, &entity);

        // 容器日志浮层（状态全在 `AppState::logs` 里，不需要 `cx`）。
        let logs_dialog: AnyElement = views::logs_overlay(state, &entity);

        // 容器 exec 浮层。要 `cx` 才能读命令输入框的值。
        let exec_dialog: AnyElement = views::exec_overlay(self, &entity, cx);

        // 数据库页的动作弹窗（新建库 / 新建用户 / 改密码 / 授权）。
        // 要 `cx` 才能读表单里那几格的值。
        let db_dialog: AnyElement = views::db_dialog_overlay(self, &entity, cx);

        // 页面渲染要 `&Shell`（不只是 `&AppState`）——「添加实例」页有输入框，
        // 而输入框的 `InputState` 住在 `Shell` 里。
        // `cx` 也只有那一页用得上（要实时读输入框的值做命令预览）。
        //
        // 切页时淡入一下（120ms）：`with_animation` 的 id 用**页面标签** ——
        // 换页就换 id，于是重新播一次；同一页的后续重绘 id 不变，不会重播
        // （GPUI 按 id 记住播放进度，一次性动画结束后保持终值）。
        //
        // ⚠️ 动画得挂在 `div()` 上而不是 `AnyElement` 上：`AnyElement` 不实现
        // `Styled`（它是类型擦除后的元素），`.opacity()` 在那儿不存在。
        // 外面那层 `v_flex().w_full()` 负责把"整页宽"传下去 —— 动画包装层
        // 自己不带宽度，而页面内容大量用 `w_full`。
        let page_body: AnyElement = v_flex()
            .w_full()
            .child(
                div().w_full().child(views::page(self, cx, &entity)).with_animation(
                    state.page.label(),
                    Animation::new(Duration::from_millis(120)),
                    |el, delta| el.opacity(delta),
                ),
            )
            .into_any_element();

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
                            )
                            // 提示条放在主内容列**最后**：它是一条贴底的布局项，
                            // 出现时把上面那个 `flex_1` 的滚动区挤矮一点，
                            // 而不是盖在它上面（见 `toast` 的说明）。
                            .child(toast),
                    ),
            )
            .child(confirm)
            // 拉取弹窗排在创建弹窗**之前**：两者不会同时开着
            // （`use_pulled_image` 会先关掉拉取弹窗），但万一同时开着，
            // 后画的创建表单应该压在上面 —— 用户刚点的是它。
            .child(pull_dialog)
            .child(create_dialog)
            .child(detail_dialog)
            .child(distro_detail_dialog)
            // 提示弹窗放最后：它可能是从发行版详情里打开的，
            // 后画的压在详情上面。
            .child(prompt_dialog)
            .child(export_overlay)
            .child(wslconf_dialog)
            // 日志和 exec 都可能是从容器详情里打开的，所以排在它之后。
            .child(logs_dialog)
            .child(exec_dialog)
            // 数据库页的动作弹窗排在最后：它可能盖在任一页（含容器详情）上。
            .child(db_dialog)
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

    // ⚠️ 悬停/按下**必须**自己写：这是我们自己画的 `div`（要"选中态 + 分组标题"，
    // 用不了组件库的 `Button`），而 `Button` 自带的那套它一点都没有 ——
    // v0.6.5 之前鼠标移上去毫无反应，实机反馈"没有点击的感觉"。
    //
    // 未选中的项：悬停给一层浅底 + 文字提亮（`nav_hover` 是侧边栏底色与
    // "已选中"之间的那一档）；按下再深一点。已选中的项不再叠悬停 ——
    // 它已经在最亮的那一档了，再变只会让人以为选中态丢了。
    item = if active {
        item.bg(theme::bg_selected()).text_color(theme::primary())
    } else {
        item.text_color(theme::text_muted())
            .hover(|this| this.bg(theme::nav_hover()).text_color(theme::text()))
            .active(|this| this.bg(theme::nav_hover()))
    };

    item.into_any_element()
}

/// 导航项的稳定 ID。
///
/// 用 `&'static str` 而不是格式化出来的 `String`：
/// GPUI 的交互元素要求 ID 稳定，静态字符串最不容易出错。
///
/// ⚠️ 这个 match 只覆盖 `Page::ALL` 里那 8 页 —— `AddInstance` / `Database`
/// 都不是导航项（它们从别的页面的按钮进），所以这里也不需要它们的 id
/// （真走到那两页也只是给一个不会被用到的 id）。
fn nav_id(page: Page) -> &'static str {
    match page {
        Page::Dashboard => "nav-dashboard",
        Page::Instances => "nav-instances",
        // 侧边栏不列它，但 `Page` 是穷尽的，这一支必须有；
        // 真走到这儿也只是给一个不会被用到的 id。
        Page::AddInstance => "nav-add-instance",
        Page::Containers => "nav-containers",
        Page::Images => "nav-images",
        Page::Networks => "nav-networks",
        Page::Volumes => "nav-volumes",
        // 同 `AddInstance`：侧边栏不列它（只能从容器行上的按钮进），
        // 但 `Page` 是穷尽的，这一支必须有。
        Page::Database => "nav-database",
        Page::AppSettings => "nav-app-settings",
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

    views::modal_scrim("confirm",
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
                    // 文案里写的是 Markdown（`**已停止**`），而这里是纯文本
                    // 渲染 —— 不清掉的话用户看到的是字面星号，
                    // 而这恰好是"不可撤销"那类确认框。见 `util::strip_md`。
                    .child(crate::util::strip_md(&action.body())),
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
