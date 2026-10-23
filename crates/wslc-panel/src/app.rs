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

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// 注意：`primary()` / `danger()` 这些样式方法来自 trait `ButtonVariants`，
// 光导入 `Button` 是不够的 —— 这里用 glob 把 button 模块全带上。
use gpui_kit::component::button::*;
// `StyledExt` 提供 `font_bold` / `font_semibold` 等字重方法（由宏生成），
// 不导入 trait 就会报 "no method named font_bold"。
// 输入框与它的变化事件。
//
// `InputEvent::Change` 用来把「容器」页搜索框的输入**订阅回 `Shell`**：
// 渲染整页的是 `Shell`，输入框改的只是它自己的值 —— 不订阅父级就不会重绘，
// 过滤结果要等下一次自动刷新（默认 3 秒）才变，打字时像"搜索框坏了"。
// 这套写法在 v0.6 的 tag 过滤框上验证过（`cx.subscribe` + `matches!(event, InputEvent::Change)`）。
use gpui_kit::component::input::{EditorState, InputEvent, InputState, TabSize};
// 「微软商店」与「在线发行版」两张清单都走组件库的 `Select`（下拉框）——
// 参考实现也是一个下拉框（`CustomComboBox`），而不是我们原先那种
// "几十个按钮铺满一屏"的写法。
use gpui_kit::component::searchable_list::SearchableListItem;
use gpui_kit::component::select::{SelectEvent, SelectState};
// 同 views.rs：不导入 `Disableable`，全项目不再用 `.disabled()`。
use gpui_kit::component::{Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::*;

use wslc_core::cmd::container::{PullPolicy, RunSpec, inspect as inspect_container};
use wslc_core::cmd::network as network_cmd;
// 部署管理页（v0.9）：真正的活都在 `cmd::deploy` 里（打包 / 上传 / 起停判活），
// 识别 / 脚本生成 / 输出解析是 `model::deploy` 的纯逻辑。
// ⚠️ `deploy` 这个名字两边都有（`cmd::deploy` 和 `model::deploy`），
// 所以一个用全路径、一个起别名 —— 否则读代码的人分不清在调哪一层。
use wslc_core::cmd::deploy as deploy_cmd;
use wslc_core::model::deploy as deploy_model;
use wslc_core::model::{AppRuntime, DeployKind, DeploySignal, DeploySite};
// 数据库管理页（v0.7.0）：真正的活都在 `cmd::mysql` 里（起进程 + 脱敏），
// 识别 / 拼参数 / SQL 生成是 `model::mysql` 的纯逻辑。
use wslc_core::cmd::mysql::{MysqlProbe, run_sql};
use wslc_core::model::{
    GrantKind, MysqlCreds, MysqlFamily, RootCredHint, SqlMode, mysql as mysql_model,
};
// 安装现在是**多步计划**（见 `wslc_core::model::install` 的模块说明）：
// `InstallSpec` 是用户填的东西，`plan()` 把它变成步骤，`run_plan` 把它做出来。
use wslc_core::cmd::distro::InstallSpec;
use wslc_core::cmd::install::{self, InstallCancel, InstallEvent, InstallSummary, RunOptions};
use wslc_core::mirrors;
use wslc_core::model::install::{
    self as install_model, InstallFieldErrors, InstallSource, derive_install_dir,
    suggest_name_from_file,
};
// 镜像仓库（Registry V2）的纯逻辑：引用规范化、URL 拼装、tag 排序、错误翻译。
use wslc_core::registry;
use wslc_core::settings::SettingKey;
// `Wsl` 是发行版（实例）的调用器，和容器的 `Wslc` 并列。
use wslc_core::{Wsl, Wslc};

use crate::state::{
    self, AppState, BackupState, ConfirmAction, DbFailure, DbState, DbTab, DeployFailure,
    DeployLogs, DeployPhase, DeployProgress, DeploySiteConf, DeployState, DetailInspect,
    DistroAction, ExecState, ExportProgress, ImmediateAction, ImportState, InstallOutcome,
    InstallProgress, InstallSourceKind, LogsState, MirrorChoice, MirrorProbeResult, Page,
    PendingAction, PromptKind, Toast, ToastKind, UiSection, create_command, exec_argv,
};
use crate::theme;
// `split_list` / `split_env_list` / `refresh_label` 是纯函数，
// 住在不依赖 GPUI 的 `wslc-panel-core` 里
// —— 这样它们的单测不必链接 GPUI（见那个 crate 的顶层说明）。
use crate::util::{refresh_label, split_env_list, split_list};
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
    /// 是否已经点过「开始安装」且校验没过。
    ///
    /// 点之前**不**在页面上常驻红字 / 错误卡：那会一直占着地方，还会和
    /// 步骤预览里的同一句错误重复。点之后才把 [`Self::field_errors`] 对应的
    /// 输入框标红，并把焦点送过去。
    pub(crate) show_errors: bool,
    /// 上一次提交失败时，哪几块输入有问题（名字 / 来源 / 安装目录）。
    ///
    /// 为什么不在渲染时重新算：`preflight` 里"安装目录非空"要碰文件系统
    /// （见 `add_instance` 的说明），渲染时不能查；把提交那一刻的结论留下来，
    /// 才能让这一帧把目录也标红。
    pub(crate) field_errors: InstallFieldErrors,
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
    // 让折叠摘要与预览随草稿更新，退出弹窗后退订。
    _input_subs: Vec<Subscription>,
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
    /// 启动命令。**留空 = 用镜像自己的 `ENTRYPOINT` / `CMD`**（见
    /// [`create_command`] —— 留空必须真的"不给命令"：给一条空命令会把
    /// 镜像的 `CMD` 顶掉，mysql / nginx 那类镜像就起不来了）。
    ///
    /// ⚠️ 原样交给容器里的 `/bin/sh -c`（不引号、不转义），和「添加部署」
    /// 那一格同一条规矩：用户在这一格写的就是他要执行的命令行。
    ///
    /// 这一格是补上的：之前**没有**它，于是"只有 `ENTRYPOINT`、没有 `CMD`"的
    /// 镜像从面板建出来必然**起来就退**。实测 `eclipse-temurin:8-jre-alpine`：
    /// `wslc start` 成功，但 `StartedAt` 与 `FinishedAt` 相隔 **1.1 ms**、
    /// 退出码 0、logs 为空（那个镜像的 entrypoint 末行是 `exec "$@"`，
    /// 不带参数时就等于什么都不做）—— 界面上只看到"点了启动还是已退出"。
    pub(crate) command: Entity<InputState>,
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

        // 留空就**一条命令都不给** —— 镜像自己的 ENTRYPOINT / CMD 才有机会生效
        // （为什么不能给一条空命令，见 `create_command` 的说明）。
        //
        // 位置在 `--name` 之后、`spec.ports` 之前只是可读性；真正要紧的是
        // `RunSpec::to_args` 把它拼在**镜像引用之后** —— 那是 `wslc run` 认
        // "覆盖 CMD" 的位置。
        spec.command = create_command(&text(&self.command));

        spec.ports = split_list(&text(&self.ports));
        spec.volumes = split_list(&text(&self.volumes));

        // 环境变量按 `KEY=VALUE` 解析；没有等号的**直接丢掉**，
        // 不去猜用户想表达什么（猜错了反而更难查）。
        //
        // ⚠️ 用 `split_env_list` 而不是 `split_list`：值里的逗号是合法的
        // （`SPRING_PROFILES_ACTIVE=dev,local`），按逗号一刀切会把后半段
        // 变成没有 `=` 的段然后静默丢掉。见 `util::split_env_list`。
        spec.env = split_env_list(&text(&self.env))
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
    /// SQL 控制台里那段脚本（v0.12 起是**多行编辑器**）。
    ///
    /// # 为什么从 `InputState` 换成 `EditorState`
    ///
    /// 之前是单行 `Input` —— 稍微像样的查询都写不下（没有换行、没有缩进），
    /// 而且关键字和字符串一个颜色，看不出哪里写错。
    ///
    /// `EditorState` 带来：多行、行号、缩进参考线、括号自动配对，
    /// 以及 **SQL 语法高亮**（靠 `gpui-kit` 的 `tree-sitter-sql` feature，
    /// 见根 `Cargo.toml` 里那段说明）。
    ///
    /// ⚠️ 两者 API 很像但不完全一样：
    /// - 取值：`EditorState::text()`，不是 `InputState::value()`；
    /// - 赋值：两者都有 `set_value(text, window, cx)`。
    /// 别猜，看 `gpui-kit` 的 `examples/editor/src/main.rs`，两种用法都在那儿。
    pub(crate) sql: Entity<EditorState>,
}

/// 「添加部署 / 重新部署」弹窗里的输入框（v0.9）。
///
/// 懒创建（`InputState::new` 要 `&mut Window`），和 `DbDialog` 同一个理由。
/// ⚠️ 和「数据库管理」的连接卡正相反：这些框**只在弹窗开着时存在**，
/// 关掉就丢 —— 每次打开都要重新预填（预填值见 `Shell::open_deploy_dialog`）。
pub(crate) struct DeployForm {
    /// `Some(应用名)` = 在**重新部署**已有那一条。
    ///
    /// 这时应用名锁死（改了就是另一个部署了，应该走「添加部署」），
    /// 其余几格预填成记录里的值 —— 让"部署到别的目录"这种事有路可走，
    /// 而不用先删掉（**删掉会把容器里的目录删了**）。
    pub(crate) editing: Option<String>,
    /// 应用名（也是目标目录名、本地存档的键）。
    pub(crate) app: Entity<InputState>,
    /// 本地来源路径（文件 / 目录，由「浏览…」填）。
    pub(crate) source: Entity<InputState>,
    /// 容器内的目标目录。
    pub(crate) target: Entity<InputState>,
    /// 启动命令。
    ///
    /// ⚠️ 它**原样**交给容器里的 `/bin/sh -c`（不引号、不转义）——
    /// 用户在这一格写的就是他要执行的命令行。界面上必须把这件事说出来。
    pub(crate) command: Entity<InputState>,
    /// 应用监听的端口。站点关时是**信息性**的（只用来提示有没有发布到主机）；
    /// 站点开时它就是 `server { listen … }`。
    pub(crate) port: Entity<InputState>,
    /// 「生成站点配置」勾上没有（只有 nginx 有这一块）。
    pub(crate) site_enabled: bool,
    /// 单页应用回退。
    pub(crate) site_spa: bool,
    /// `server_name`（域名 / localhost）。
    pub(crate) server_name: Entity<InputState>,
    /// 反代路径前缀（`/api/`）。
    pub(crate) proxy_prefix: Entity<InputState>,
    /// 反代上游（`http://api:8080`）。
    pub(crate) proxy_upstream: Entity<InputState>,
    /// 反代时去掉前缀。
    pub(crate) proxy_strip: bool,
    /// 更多反代（`/auth/ => http://auth:9000; ...`）。
    pub(crate) proxy_more: Entity<InputState>,
    /// 上游是另一个容器时，自动把两个容器接到同一个用户网络。
    pub(crate) auto_connect: bool,
    // 让折叠摘要随输入更新；表单关闭时自动退订。
    _input_subs: Vec<Subscription>,
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
    /// **导入一份 `.sql` 到某个库**（从数据库列表那一行的「导入备份」进来）。
    ///
    /// 目标库**由那一行定死**（`database`），不给"导入到别的库"的选项 ——
    /// 实机反馈："数据库列表那里要一个导入备份，点击导入这个库的数据"。
    /// 顺带这也消掉了"目标库填错"这类错误：它没有可填错的地方。
    ImportSql {
        /// 导入到哪个库（那一行的库名）。
        database: String,
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
            Self::ImportSql { .. } => "导入 SQL 文件",
        }
    }

    /// 提交按钮文案。
    pub(crate) fn confirm_label(&self) -> &'static str {
        match self {
            Self::NewDatabase => "创建",
            Self::NewUser => "创建",
            Self::AlterPassword { .. } => "改密码",
            Self::Grant { .. } => "授权",
            Self::ImportSql { .. } => "开始导入",
        }
    }

    /// 这个动作**不是**一条 SQL —— 它有自己的提交路径。
    ///
    /// 分开的理由：其余四种都是"拼一条 SQL 然后执行"，而导入是"把一个文件
    /// 送进容器再让 mysql 读它"。`submit_db_dialog` 是按 SQL 拼装的，
    /// 硬塞进去只会让那个函数里多一层和 SQL 无关的分支。
    pub(crate) fn is_import(&self) -> bool {
        matches!(self, Self::ImportSql { .. })
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
    /// 新建用户 / 授权：**同上面那一格的候选下拉框**（库列表里已有的业务库）。
    ///
    /// # 为什么是"输入框 + 下拉框"而不是把输入框换成下拉框
    ///
    /// 下拉框的候选来自已经读到的库列表（`DbState::grant_database_options`），
    /// 而那个列表可能**还没读出来**（页面刚打开 / 读失败），也可能**不全**
    /// （普通账号读不到别人的库）。那样一来"只能选"就等于把授权这件事
    /// 堵死在一个可能为空的清单上。
    ///
    /// 所以两者是**同一格的两个入口**：下拉框选完写进输入框（
    /// [`Shell::pick_grant_database`]），提交时**只读输入框**
    /// （`submit_db_dialog` 里那一处 `text(&dialog.grant_db)`）——
    /// 也就是说输入框是唯一的事实来源，下拉框只是快一点的录入方式。
    /// 这和「创建容器」里"镜像引用输入框 + 本地镜像下拉框"是同一个形状
    /// （那个也把下拉框定义成"从本地已有的镜像里挑一个的快捷方式"）。
    pub(crate) grant_db_select: Entity<SelectState<Vec<DropdownItem>>>,
    /// 授权级别。
    pub(crate) grant: GrantKind,
    /// （仅新建数据库）**同时创建用户** —— v0.8 的主路径：建库的人几乎总是
    /// 需要一个连它的账号，单独再走一遍「新建用户」纯属多余。
    pub(crate) create_user: bool,
    /// （仅新建数据库）**数据库用户名**（用户手填）。
    ///
    /// 以前它是按库名推出来的（`<库名>_user`），弹窗里只画一行"跟库名走"的
    /// 说明。实机反馈："创建数据库的创建用户名这里让手填，不要自动生成" ——
    /// 所以现在它是真输入框：**留空就拦下来**（`AGENTS.md` §7.5 的
    /// "空值等于没填"），旁边那个「用建议值」按钮把 `<库名>_user` 填进去，
    /// 但**不自动写**（用户不点就不动）。
    pub(crate) db_user: Entity<InputState>,
    /// （仅导入）本机那份 `.sql` 的路径。
    ///
    /// 目标库**没有输入框**：它由数据库列表那一行定死，写在
    /// [`DbDialogKind::ImportSql`] 的 `database` 里（用户要的是"导入这个库的
    /// 数据"，给一个能改的框只会多一个填错的机会）。
    pub(crate) import_path: Entity<InputState>,
    // 保留订阅，让编码摘要及用户名建议随输入更新。
    _input_subs: Vec<Subscription>,
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

/// 本程序自己的版本号（`Cargo.toml` 的 `version`），解析成可比较的形式。
///
/// 解析不出来时退回 `0.0.0`：那会让"远端比 0.0.0 新"永远成立 ——
/// 也就是"有新版就提示"，这是**故意选的方向**。反过来（退回一个很大的版本）
/// 会让更新提示彻底静默，而那种 bug 没人会发现。
fn current_version() -> wslc_core::update::Version {
    wslc_core::update::Version::parse(env!("CARGO_PKG_VERSION")).unwrap_or(
        wslc_core::update::Version {
            major: 0,
            minor: 0,
            patch: 0,
            pre: None,
        },
    )
}

/// 应用外壳。
pub struct Shell {
    /// 全部状态。
    pub state: AppState,
    /// 最近一次采集完成的时间（自动刷新与手动刷新都算）。
    ///
    /// 只用于页头那句「最后刷新：N 秒前」—— 它是**界面状态**，不进快照：
    /// 快照每轮整体重建，放进去反而要每轮重新赋值。
    last_refresh: Option<Instant>,
    /// 错误横幅是不是展开着（多条错误时才用得上）。
    errors_expanded: bool,
    /// 「容器」页的搜索框；进过那一页之后一直是 `Some`。
    ///
    /// `InputState::new` 要 `&mut Window`，所以只能在进入页面时建一次
    /// （见 [`Shell::ensure_container_search`]）。`pub(crate)` 是因为
    /// `views.rs` 要读它来画 `Input`。
    pub(crate) container_search: Option<Entity<InputState>>,
    /// 搜索框内容变化的订阅。
    ///
    /// ⚠️ **必须存着**：`Subscription` 一 drop 就退订，表现是"打字时列表
    /// 不跟着变"，不会报错 —— 这是本仓库踩过的坑（见 `select_subs`）。
    container_search_sub: Option<Subscription>,
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
    /// 已经跑过多少轮刷新（给**部署页状态**用的那一份计数）。
    ///
    /// 和 `status_tick` 分开：部署页那一列要问的是"每一条部署现在跑没跑"，
    /// 一条一次 `exec` —— 跟着 3 秒一次太快（见 [`Shell::deploy_status_every_n_ticks`]）。
    deploy_tick: u32,
    /// 「数据库管理」页的连接卡 / SQL 输入框；没进过那一页时为 `None`。
    ///
    /// `pub(crate)`：页面在 `views.rs` 里渲染。
    pub(crate) db_form: Option<DbPageForm>,
    /// SQL 结果表格的 `TableState`（v0.11）。
    ///
    /// # 为什么在 `Shell` 而不是 `AppState`
    ///
    /// `TableState` 是 GPUI 的实体（`Entity<TableState<D>>`），`wslc-panel-core`
    /// 不依赖 GPUI，装不下它。这和 `container_search` 的处境一样：
    /// **界面状态**留在 `Shell`，纯数据（`DbState::sql_results`）在 `AppState`。
    ///
    /// # 什么时候重建
    ///
    /// 每次跑完 SQL 就**整个换掉**（见 [`Shell::apply_db_console_result`]）：
    /// 结果集变了，列定义、行、排序全都要重新来。缓存着旧的不如重建 ——
    /// 重建的代价只是按列名估一次宽度（前 100 行），微不足道。
    pub(crate) sql_table: Option<
        Entity<gpui_kit::component::table::TableState<crate::sql_table::SqlResultDelegate>>,
    >,
    /// 当前在看的**第几个**结果（多语句时按 tab 分开）。
    pub(crate) sql_result_tab: usize,
    /// 「看原始输出」是不是展开着。
    ///
    /// 原始输出（`mysql --table` 的 ASCII 表格）不常看 —— 只在排查
    /// 列宽异常 / 字符集问题时才有用，所以默认收起。
    pub(crate) sql_raw_open: bool,
    /// 库 / 表树的**树控件状态**（v0.12.1）。
    ///
    /// # 为什么换成组件的 `Tree`
    ///
    /// v0.12 里这棵树是**自己用 `Button` 一行行画的**，实机一看就丑：
    /// `Button` 会把文字**居中**、给每一行套一个整行宽度的底色和内边距，
    /// 于是 110 行看起来像 110 个色块，完全不像树（缩进也读不出来）。
    ///
    /// `gpui-kit` 自带 `component::tree::Tree`（虚拟滚动 + 逐级展开 + 选中态 +
    /// 右键菜单），官方 `examples/editor` 就是用它做文件树的。
    /// 它自己管展开/折叠和选中，我们只管把 `TreeItem` 喂进去。
    ///
    /// ⚠️ `TreeState` 是 GPUI 实体，`wslc-panel-core` 装不下 —— 和
    /// [`Shell::sql_table`] 同一个理由：纯数据（`DbState::schema_tree`）在
    /// core，**界面状态**在 `Shell`。
    pub(crate) schema_tree_state: Option<Entity<gpui_kit::component::tree::TreeState>>,
    /// 树是**按哪次查询**建出来的（用于判断要不要重建）。
    ///
    /// 存 `(搜索关键字, 表总数)`：两者都没变就不重建 —— `TreeState` 重建会
    /// 丢掉用户已经展开的层级，所以这件事必须**只在真的变了**时做。
    pub(crate) schema_tree_built_for: Option<(String, usize)>,
    /// 库 / 表树的搜索框（`InputState::new` 要 `&mut Window`，所以懒创建）。
    pub(crate) schema_search: Option<Entity<InputState>>,
    /// 搜索框内容变化的订阅。
    ///
    /// ⚠️ **必须存着**：`Subscription` 一 drop 就退订，表现是"打字时树不跟着过滤"，
    /// 不会报错 —— 这是本仓库踩过的坑（见 `container_search_sub`）。
    pub(crate) schema_search_sub: Option<Subscription>,
    /// 「数据库管理」页的一次性动作弹窗；关闭时为 `None`。
    ///
    /// 和 `create_dialog` 一样懒创建（`InputState::new` 要 `&mut Window`）。
    pub(crate) db_dialog: Option<DbDialog>,
    /// 「数据库管理」页的「基础信息」浮层开着没有。
    ///
    /// 连接卡（用户名 / 密码 + 那两段说明）原来**常驻页面顶部**，实机反馈
    /// "不用进来就在上面占用空间" —— 现在收进这个浮层，页面顶上是页头 +
    /// 一个「基础信息」按钮。
    pub(crate) db_info_open: bool,
    /// 「部署管理」页的「添加部署 / 重新部署」弹窗；关闭时为 `None`。
    ///
    /// 只有这一个要 window（里面全是输入框）。这一页另外两个浮层
    /// （**日志**、**删除确认**）都是纯数据，住在 `AppState::deploy` 里 ——
    /// 它们只有按钮和文字，没有 `InputState`。
    pub(crate) deploy_form: Option<DeployForm>,
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
    /// 「添加部署」选了来源：按新来源重算**启动命令**
    /// （见 [`Shell::resync_deploy_command`]）。
    DeploySource,
}

/// 保存对话框关掉之后要做什么（CSV 导出用，v0.11.3）。
///
/// 单独一个类型而不是 `Option<Result<…>>` 嵌套：那三层
/// （取消 / 失败 / 拿到路径）在调用处读不出来，而且 `Option<Result<Option<T>>>`
/// 有四个状态、其中两个无意义。
#[derive(Debug, Clone, PartialEq, Eq)]
enum SaveDecision {
    /// 拿到路径了，去写文件。
    Write(std::path::PathBuf),
    /// 用户点了取消 —— 不算错误，也不用提示。
    Cancelled,
    /// 对话框本身打不开（powershell 起不来之类）。
    Failed(String),
}

/// 「添加部署」弹窗里需要单独聚焦的输入框。
///
/// 单独一个枚举而不是传字符串：调用点在四处校验失败分支上，
/// 传错了表现为"焦点跑到别的格子"，那是最难看出来的一类错。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeployField {
    /// 应用名。
    App,
    /// 本地来源。
    Source,
    /// 容器内的目标目录。
    Target,
    /// 启动命令。
    Command,
    /// 应用监听的端口（多应用下它要唯一）。
    ///
    /// v0.9.19 补的：端口校验失败时得把焦点送回这一格 —— 早先枚举里没有它，
    /// 端口填错只能靠用户自己找。
    Port,
    /// 反代路径前缀（站点开的校验焦点）。
    ProxyPrefix,
    /// 反代上游。
    ProxyUpstream,
    /// `server_name`。
    ServerName,
    /// 更多反代（`;` 分隔）。
    ProxyMore,
}

/// 从「添加部署」弹窗里读出来的值。
struct DeployFormValues {
    /// 应用名。
    app: String,
    /// 本地来源路径。
    source: String,
    /// 容器内的目标目录。
    target: String,
    /// 启动命令。
    command: String,
    /// 端口。空 / 解析不出来时是 `None` —— 调用方**必须**报错：
    /// 多应用下静默回落到运行时默认端口会撞（v0.9.19 改）。
    port: Option<u16>,
    /// 是否生成 nginx 站点配置（只有 nginx 有意义）。
    site_enabled: bool,
    /// 单页应用回退。
    site_spa: bool,
    /// `server_name`。
    server_name: String,
    /// 反代路径前缀。
    proxy_prefix: String,
    /// 反代上游。
    proxy_upstream: String,
    /// 反代去掉前缀。
    proxy_strip: bool,
    /// 更多反代（`;` 分隔）。
    proxy_more: String,
    /// 自动接通两个容器的网络。
    auto_connect: bool,
}

/// 一次部署动作要用的素材（从当前页面状态 + 存档里取出来的一份快照）。
///
/// 单独一个结构体是因为这几个字段要在 `spawn` 的异步闭包里用 ——
/// 闭包不能借 `self`，所以必须**先取出来**。取的时候顺手把"这一条还有没有
/// 意义"判掉（找不到了就返回 `None`，界面上那些按钮也不会画）。
struct DeployAction {
    /// 目标容器名。
    container: String,
    /// 容器内的目标目录。
    target: String,
    /// 应用端口（站点配置重写时 `listen` 要用）。
    port: u16,
    /// 启动命令。
    command: String,
    /// 是不是进程形态（决定要不要先停 / 能不能启动）。
    is_process: bool,
    /// 上一次真的铺进去的名字 —— 「删除」只删这些（外加 `app.log` / `app.pid`），
    /// **不整目录清空**（见 `model::deploy::removals_script`）。
    /// 老存档里没有这一项时是空表：那就只删日志 / pidfile。
    members: Vec<String>,
    /// 站点配置（删除部署时要连 conf 一起删）。
    site: Option<DeploySite>,
}

/// 「写 / 删站点配置」这一步的结果。
///
/// 和 [`DeployAction`] 一样是**先取出来**的快照：后台闭包不能借 `self`。
struct SiteStep {
    /// 应该写进存档的 site（失败时是**上一次**的，不能把没生效的写进去）。
    site: Option<DeploySite>,
    /// 非致命警告（reload 失败 / 有噪音）。
    warning: Option<String>,
    /// 成功时附带的说明（自动接通了哪个网络）。
    note: Option<String>,
    /// 致命错误（`nginx -t` 没过、conf 删不掉）；`Some` 时 `site` 保留旧值。
    error: Option<String>,
}

impl Shell {
    /// 创建外壳：立刻触发一次采集、加载配置、启动自动刷新。
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut shell = Self {
            state: AppState::new(Wslc::new(), Wsl::new()),
            last_refresh: None,
            errors_expanded: false,
            container_search: None,
            container_search_sub: None,
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
            deploy_tick: 0,
            db_form: None,
            sql_table: None,
            sql_result_tab: 0,
            sql_raw_open: false,
            schema_tree_state: None,
            schema_tree_built_for: None,
            schema_search: None,
            schema_search_sub: None,
            db_dialog: None,
            db_info_open: false,
            deploy_form: None,
        };
        shell.refresh(cx);
        shell.start_auto_refresh(cx);
        shell.start_toast_reaper(cx);
        // 更新检查单独排一条时间线：延迟 8 秒，只查一次（见那个方法）。
        shell.start_auto_check(cx);
        shell
    }

    /// 建「容器」页的搜索框（幂等）。
    ///
    /// **必须**在有 `&mut Window` 的地方调用（`InputState::new` 要它），
    /// 所以入口是 `set_page`，不是 render。
    ///
    /// 订阅 `InputEvent::Change` 不是优化，是**必须**：渲染整页的是 `Shell`，
    /// 输入框改的是 `InputState` 自己的值 —— 不订阅父级不会重绘，过滤结果
    /// 要等到下一次自动刷新才变，打字时看起来就像"搜索框坏了"。
    /// （v0.6 的 tag 过滤框就是这套写法，见 `InputEvent` 的导入注释。）
    fn ensure_container_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.container_search.is_some() {
            return;
        }

        let input =
            cx.new(|cx| InputState::new(window, cx).placeholder("搜索名称 / 镜像 / ID / 端口"));
        let sub = cx.subscribe(&input, |_shell, _input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });

        self.container_search = Some(input);
        self.container_search_sub = Some(sub);
        cx.notify();
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
            show_errors: false,
            field_errors: InstallFieldErrors::default(),
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
        let host_sub = cx.subscribe(&host, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        let repository_sub = cx.subscribe(&repository, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        self.image_subs = vec![tags_sub, host_sub, repository_sub];

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
        self.state.set_section_open(UiSection::PullSource, false);
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
    pub fn use_pulled_image(&mut self, image: String, window: &mut Window, cx: &mut Context<Self>) {
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

        // ⚠️ 候选项由 `RegistryState::tag_items()` 给（纯逻辑，那边有测试）——
        // 它会把**列表里没有、但用户手打的**那个 tag 补在最前面。
        //
        // 为什么这条必须有：实机反馈"我想拉取 openjdk:8-jre-slim，结果啥东东都
        // 出来了" —— 用户打的 tag 不在这个 registry 返回的列表里（实测它的
        // `tags/list` 对 `library/openjdk` 只回 17282 个，既没有 `8-jre-slim`
        // 也没有 `latest`，而两者的 manifest 都是 200/能拉），于是
        // `set_selected_value` 找不到对应项 → 组件**自动选中第一项** `28-ea`
        // → "将拉取"跟着变成 `:28-ea`，**拉错镜像**。
        let items: Vec<DropdownItem> = self
            .state
            .registry
            .tag_items()
            .into_iter()
            .map(|tag| DropdownItem::new(tag.clone(), tag))
            .collect();
        let keep = self.state.registry.effective_tag().map(str::to_owned);

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
            self.state.set_section_open(UiSection::PullSource, true);
            let handle = host_input.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
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
        // 已经选中的 tag 再选一次 = 什么都不做。
        //
        // 两个理由：① [`Shell::fill_tag_select`] 换清单后会**重新选一次**，而组件
        // 也会为"选中变了"发事件 —— 不判重就会多量一次体积（两次请求，还可能撞
        // 限流）；② 那条路径下重选同一个 tag 会把刚量到的体积**清掉**
        //（`select_tag` 里 `size = None`），于是"量出来了又变回量不出来"。
        if self.state.registry.selected.as_deref() == Some(tag.as_str()) {
            return;
        }
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
    /// 引用由 [`RegistryState::selected_reference`] 从
    /// registry / 仓库名 / tag 三件东西拼出来。
    pub fn pull_registry_selection(&mut self, cx: &mut Context<Self>) {
        if self.state.pulling.is_some() {
            self.state
                .notify(Toast::info("已经有一个拉取在跑了，等它结束"));
            cx.notify();
            return;
        }
        // 输入可能刚在这一帧改变；提交时再次核对，不能拉取上次查询的旧目标。
        let inputs_match = self.image_form.as_ref().is_some_and(|form| {
            self.state.registry.matches_inputs(
                form.host.read(cx).value().trim(),
                form.repository.read(cx).value().trim(),
            )
        });
        if !inputs_match {
            self.state.notify(Toast::error("输入已修改，请先查询镜像"));
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
            // 「启动 / 停止一条部署」（v0.9）—— 它们不做容器级的事，
            // 只操作"那个应用自己的进程"，所以走部署页那套方法。
            ImmediateAction::StartDeployment(app) => self.start_deployment(app, cx),
            ImmediateAction::StopDeployment(app) => self.stop_deployment(app, cx),
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

    /// 用系统默认浏览器打开一个 URL。
    ///
    /// # 为什么是 `explorer.exe`
    ///
    /// 它把 URL 交给 Windows 的默认协议处理器（也就是默认浏览器），
    /// 本身不等待、不捕获输出 —— `AGENTS.md` §7.1 允许这种"只把进程拉起来"
    /// 的同步调用（和打开资源管理器同一个理由）。
    ///
    /// ⚠️ 只接受 `http://` / `https://`：这个入参来自容器 inspect 的
    /// `Ports`，理论上是我们自己拼的，但把任意字符串交给
    /// `explorer.exe` 等于多开一个协议处理器入口，没必要。
    pub fn open_url(&mut self, url: String, cx: &mut Context<Self>) {
        let url = url.trim().to_owned();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            self.state
                .notify(Toast::error(format!("不是可打开的 URL：{url}")));
            cx.notify();
            return;
        }
        match std::process::Command::new("explorer.exe").arg(&url).spawn() {
            Ok(_) => {
                tracing::info!("已用默认浏览器打开 {url}");
                self.state
                    .notify(Toast::info(format!("已在浏览器打开 {url}")));
            }
            Err(e) => {
                tracing::warn!("打开浏览器失败：{e}");
                self.state
                    .notify(Toast::error(format!("打开浏览器失败：{e}")));
            }
        }
        cx.notify();
    }

    // -- 更新提示 ----------------------------------------------------------
    //
    // 这一节只做两件事：**查**（一次 HTTP GET）和**显示**（卡片 + 一条提示）。
    // 刻意不做下载/校验/替换 exe —— 用户自己在发布页取文件，理由见
    // `docs/PLAN-install-update.md` §9。

    /// 启动 8 秒后自动查一次。
    ///
    /// 为什么是 8 秒而不是"立刻"：实测一轮采集要**串行**跑 7 条 `wslc`
    /// 命令再加 `wsl -l -v` 和 `reg query`，启动头几秒本来就很忙；
    /// 这时候再插一个 2 秒的 HTTP 请求，用户看到的第一次卡顿就说不清是谁的锅。
    /// 8 秒之后窗口已经画出来了，多这一件事没人会发现。
    fn start_auto_check(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_secs(8))
                .await;
            // 实体已销毁（用户 8 秒内就关了窗口）→ `update` 返回 Err，忽略即可。
            let _ = this.update(cx, |shell, cx| shell.maybe_auto_check(cx));
        })
        .detach();
    }

    /// 按偏好与 24 小时节流决定要不要查。
    fn maybe_auto_check(&mut self, cx: &mut Context<Self>) {
        let now = crate::util::now_secs();
        // 本地构建的版本号和发布版一样，提示更新只会干扰开发。
        let dev_build = crate::build_sha() == "dev";
        if !crate::update::should_auto_check(&self.state.prefs, now, dev_build) {
            tracing::debug!("跳过自动检查更新（开关、开发构建或不到 24 小时）");
            return;
        }
        self.check_update(false, cx);
    }

    /// 查一次更新。
    ///
    /// `manual` = 用户点的：「正在检查…」「检查失败」这类话只说给手点的人听。
    /// 自动检查失败**必须静默** —— 否则每天开面板都弹一句"检查更新失败"，
    /// 而这个工具本身根本不依赖网络。
    pub fn check_update(&mut self, manual: bool, cx: &mut Context<Self>) {
        // 正在查就不要再发一条（用户可能连点）。
        if matches!(self.state.update, crate::update::Phase::Checking) {
            // ⚠️ 别静默返回：`AGENTS.md` §7.1 那条"点了没反应会被当成按钮坏了"
            // 对这里同样成立。界面上那颗按钮虽然已经换成「正在检查…」、点不到，
            // 但自动检查（启动 8 秒后）和手动点用的是同一条路，将来多一个入口
            // 就会踩到这条 —— 给一句话的成本是零。
            if manual {
                self.state.notify(Toast::info("正在检查更新，等它结束"));
                cx.notify();
            }
            return;
        }

        self.state.update = crate::update::Phase::Checking;
        if manual {
            self.state.notify(Toast::info("正在检查更新…"));
        }
        cx.notify();

        cx.spawn(async move |this, cx| {
            // `cx.spawn` 的 future 在**界面线程**上被 poll，所以真正的阻塞调用
            // 必须再套一层 `background_executor()`（`AGENTS.md` §7.1）。
            let outcome = cx
                .background_executor()
                .spawn(async { wslc_core::cmd::update::check() })
                .await;
            let now = crate::util::now_secs();
            let _ = this.update(cx, |shell, cx| {
                shell.apply_update_check(outcome, now, manual, cx);
            });
        })
        .detach();
    }

    /// 把一次检查的结果落到状态上。
    fn apply_update_check(
        &mut self,
        outcome: Result<wslc_core::update::Release, String>,
        now: u64,
        manual: bool,
        cx: &mut Context<Self>,
    ) {
        let current = current_version();

        // ⚠️ **这一条必须在 `match` 外面**：不管成功、失败，这一次都算"查过了"。
        //
        // 原先它写在 `Ok(release)` 分支里，而 `Err` 分支一个字段都没动 ——
        // 于是 `should_auto_check` 下一次仍然看到"没查过 / 太久没查"，**每次启动
        // 都再发一次** `curl`（最长 25 秒）。而注释和 README 承诺的恰恰相反：
        // 「查失败不会让下一次启动又立刻再查一次」「24 小时最多一次」。
        //
        // 触发条件不是边角情况：这台机器上 `api.github.com` 时好时坏，
        // 共享出口（公司 NAT / 校园网）还会把 60 次/小时用光 —— 那正是最容易
        // 一直失败、也最容易被这条 bug 反复烧配额的机器。
        self.state.prefs.last_update_check = Some(now);
        if let Err(e) = self.state.prefs.save() {
            tracing::warn!("记录更新检查时间失败：{e}");
        }

        match outcome {
            Ok(release) => {
                let newer = wslc_core::update::is_newer(&release.version, &current);
                let skipped =
                    crate::update::is_skipped(&release, self.state.prefs.skipped_version.as_deref());

                tracing::info!(
                    "检查更新：当前 {}，远端 {}（{}），{}",
                    current,
                    release.raw_version,
                    release.tag,
                    if newer { "有新版本" } else { "已是最新" }
                );

                if newer {
                    let version = release.raw_version.clone();
                    self.state.update = crate::update::Phase::Available {
                        release,
                        skipped,
                    };
                    if !skipped {
                        self.state.notify(Toast::info(format!(
                            "有新版本 {version} —— 在「设置 → 关于」里打开发布页"
                        )));
                    }
                } else {
                    self.state.update = crate::update::Phase::UpToDate { checked_at: now };
                    if manual {
                        self.state
                            .notify(Toast::info(format!("已是最新（{}）", release.raw_version)));
                    }
                }
            }
            Err(message) => {
                tracing::warn!("检查更新失败：{message}");
                if manual {
                    self.state
                        .notify(Toast::error(format!("检查更新失败：{message}")));
                    self.state.update = crate::update::Phase::Failed { message };
                } else {
                    // 自动检查失败：界面**一点变化都不该有**，回到空闲。
                    self.state.update = crate::update::Phase::Idle;
                }
            }
        }

        cx.notify();
    }

    /// 打开发布页（默认浏览器）。
    ///
    /// 没查到具体版本时退回 Releases 列表页 —— 用户点这颗按钮的目的
    /// 永远是"我要去下载"，不该因为还没查过就什么都不做。
    pub fn open_release_page(&mut self, cx: &mut Context<Self>) {
        let url = match &self.state.update {
            crate::update::Phase::Available { release, .. } => release.page_url.clone(),
            _ => format!("https://github.com/{}/releases", wslc_core::update::REPO),
        };
        self.open_url(url, cx);
    }

    /// 「跳过这个版本」：记住它，不再提示。
    pub fn skip_update_version(&mut self, cx: &mut Context<Self>) {
        let Some(version) = (match &self.state.update {
            crate::update::Phase::Available { release, .. } => Some(release.raw_version.clone()),
            _ => None,
        }) else {
            return;
        };

        self.state.prefs.skipped_version = Some(version.clone());
        if let Err(e) = self.state.prefs.save() {
            tracing::warn!("保存「跳过版本」失败：{e}");
        }
        if let crate::update::Phase::Available { skipped, .. } = &mut self.state.update {
            *skipped = true;
        }

        tracing::info!("已跳过版本 {version}");
        self.state
            .notify(Toast::info(format!("{version} 不再提示")));
        cx.notify();
    }

    /// 开关「启动时自动检查更新」。
    pub fn set_auto_check_update(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.state.prefs.auto_check_update == on {
            return;
        }
        self.state.prefs.auto_check_update = on;

        match self.state.prefs.save() {
            Ok(()) => {
                tracing::info!("启动时自动检查更新：{}", if on { "开" } else { "关" });
                self.state.notify(Toast::success(if on {
                    "启动时会自动检查更新"
                } else {
                    "不再自动检查更新（仍可手动检查）"
                }));
            }
            Err(e) => {
                tracing::warn!("保存偏好失败：{e}");
                // 内存里的值仍然生效，只是重启后会丢。
                self.state
                    .notify(Toast::error(format!("已改，但保存失败：{e}")));
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
    fn load_detail_inspect(&mut self, name: String, cx: &mut Context<Self>) {
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
        // SQL 编辑器（v0.12）：多行 + 行号 + SQL 语法高亮。
        //
        // 每一项都是有理由的，不是"顺手都打开"：
        // - `language("sql")`：靠 `tree-sitter-sql` 认关键字 / 字符串 / 注释，
        //   这是用户要的那个"像 dbx 一样的编辑器"的核心；
        // - `line_number(true)`：SQL 报错只说 "line 3"，没有行号就对不上；
        // - `indent_guides(true)`：多行语句缩进容易乱，参考线能看出层级；
        // - `soft_wrap(false)`：SQL 按行组织，自动折行会把 `;` 断到看不见的地方；
        // - `tab_size(2, soft)`：插空格不插制表符 —— 粘进别的工具时对齐不会散。
        let sql = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("sql")
                .line_number(true)
                .indent_guides(true)
                .soft_wrap(false)
                .tab_size(TabSize {
                    tab_size: 2,
                    hard_tabs: false,
                })
                .placeholder("SHOW DATABASES;")
        });

        // 用户名预填 root：绝大多数人只填密码那一格。
        user.update(cx, |state, cx| {
            state.set_value("root".to_owned(), window, cx)
        });
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
        // ⚠️ `value()` 返回的是 `SharedString`（gpui 的引用计数字符串），
        // 要自己转成 `String` —— `.trim()` 之后才是 `&str`，那种写法不用转。
        (user, form.password.read(cx).value().trim().to_owned())
    }

    /// 这一次动作要用的账号 —— **唯一入口**。
    ///
    /// # 为什么不能从 `DbState::creds` 取
    ///
    /// `DbState::creds` 全仓库只在 [`Shell::load_db`] 里赋值，而数据库页的
    /// 入口里有一半会**先把行上的账号写进连接卡**
    /// （[`Shell::open_db_backup_dialog`] / [`Shell::open_db_console_for`]）。
    /// 于是同一页上会出现两套账号，而且**看不出来**：
    ///
    /// | 动作 | 原来取的 |
    /// |---|---|
    /// | 备份 / 导入 / SQL 控制台 / 建库建用户 | 连接卡（用户看到的那个） |
    /// | 恢复备份 / 删库 / 删用户 | `DbState::creds`（进页面那一刻那个） |
    ///
    /// 复现路径是日常操作：进数据库页（`creds` = root）→ 点某一行的「备份」
    /// （连接卡被换成该库的 `shop_user`）→ 在浮层里点「恢复」→ 恢复用的是
    /// **root**，MySQL 回一句 `Access denied`，而浮层上明明写着
    /// 「备份用账号 shop_user」。
    ///
    /// 所以执行路径只认连接卡；`DbState::creds` 退回成"上一次连成功的那份"
    /// （只在诊断 / 显示里用）。
    ///
    /// `pub(crate)`：`views.rs` 渲染时构造 `PendingAction`（删库 / 删用户）
    /// 也要用它 —— 那两个按钮的账号必须和别处是同一个来源。
    pub(crate) fn db_creds(&self, cx: &App) -> MysqlCreds {
        let (user, password) = self.read_db_form(cx);
        MysqlCreds {
            user,
            password,
            host: None,
            port: None,
        }
    }

    /// 把「面板记的账号」写进连接卡之后，同步镜像一份到 `DbState::creds`。
    ///
    /// 那个字段已经不是执行入口（见 [`Shell::db_creds`]），但它仍然被文档
    /// 描述成"上一次连成功的那份" —— 从列表行进来时卡被换掉了，镜像不跟上
    /// 就会留下一个**永远比卡旧**的值。两份不一致最容易在下一个人
    /// "顺手从 `db.creds` 取一下"的时候咬人。
    fn mirror_card_into_db_creds(&mut self, cred: &wslc_panel_core::dbcreds::DbCredential) {
        if let Some(db) = self.state.db.as_mut() {
            db.creds = MysqlCreds {
                user: cred.user.clone(),
                password: cred.password.clone(),
                host: None,
                port: None,
            };
        }
    }

    /// **数据库列表那一行**该用哪个账号 —— 行内四个按钮 + 恢复共用这一个。
    ///
    /// # 规则（只有一条，按顺序取第一个有的）
    ///
    /// 1. 面板给 `(容器, 库)` 记过账号（`db-credentials.json`，建库时同时建的
    ///    那个用户）→ 用它。那是这个库**自己的**账号，备份 / 导入 / 恢复 / SQL
    ///    本来就该以它的身份做 —— 实测 `GRANT ALL ON <库>.*` 连 `DROP` 都给了，
    ///    所以连「删除这个库」它也干得了；
    /// 2. 没记过（不是面板建的库，或者存档被清过）→ 退回连接卡上那个
    ///    （用户看得见、能改的那份）。
    ///
    /// # 为什么不能用连接卡"看情况"
    ///
    /// 连接卡是**一个**值，而列表里有**多行**。用户在 `djxm` 那行点过「备份」
    /// 之后卡被换成 `djxm_user`，此时再去点 `wslc_probe` 那行的「删除」——
    /// 用卡就会拿 `djxm_user` 去删 `wslc_probe`，报一句别人看不懂的
    /// Access denied。所以**行内动作一律按行走**，卡只在"行上没有记录"时兜底。
    ///
    /// ⚠️ 唯一的例外是「用户」tab 的「删除」：删一个**账号**需要
    /// `DROP USER` 权限，而面板给某个库记的那个账号只有那个库的权限
    /// （`GRANT ALL ON <库>.*` 不含 `DROP USER`），所以那里仍然用连接卡上
    /// 的账号 —— 那一行的"对象"是账号本身，不是某个库。
    pub(crate) fn creds_for_row(&self, container: &str, database: &str, cx: &App) -> MysqlCreds {
        if let Some(cred) = self.state.db_credentials.get(container, database) {
            return MysqlCreds {
                user: cred.user.clone(),
                password: cred.password.clone(),
                host: None,
                port: None,
            };
        }
        self.db_creds(cx)
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
            let family = mysql_model::family_from_image(&image).unwrap_or(MysqlFamily::Mysql);
            self.state.db = Some(DbState::provisional(name, image, family));
            // 换了容器就要清密码：上一个容器的密码留在这儿会**连错库**
            // （两个容器恰好都有个 root 账号时，这个错误会非常难发现）。
            if let Some(form) = self.db_form.as_ref() {
                form.password
                    .update(cx, |state, cx| state.set_value(String::new(), window, cx));
                form.user.update(cx, |state, cx| {
                    state.set_value("root".to_owned(), window, cx)
                });
            }
            self.goto_page(Page::Database, cx);
            self.load_db(window, cx);
            return;
        }

        self.goto_page(Page::Database, cx);
        // 同一个容器：**不重建状态**（连接卡上填的账号要留着），
        // 但**没读到过**就再读一次 —— 否则上一轮读失败（密码错、容器刚起来
        // 还没就绪）之后，切走再回来只会看到同一张空页面：
        // 「重新读取」按钮在，但用户没理由知道要去点它。
        // 正在读的时候 `load_db` 自己会挡住（它第一句就是那条守卫）。
        if !self.state.db.as_ref().is_some_and(|db| db.loaded) {
            self.load_db(window, cx);
            return;
        }
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
        // 备份目录**先算出来**（纯字符串拼接），扫描本身放到后台执行器上 ——
        // `read_dir` + 每条一次 `metadata()`，在机械盘 / 网络盘上会明显卡一下，
        // 而下面那个 `update_in` 回调跑在**界面线程**上。
        let backup_dir = self.state.container_backup_dir(&container);

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
                let backup_dir = backup_dir.clone();
                cx.background_executor()
                    .spawn(async move {
                        let databases =
                            wslc_core::cmd::mysql::list_databases(&wslc, &target, &bin, &creds);
                        // 用户列表**单独**取：普通账号读不到 `mysql.user`
                        // （ERROR 1142），那不该把数据库列表一起判成失败。
                        let users = wslc_core::cmd::mysql::list_users(&wslc, &target, &bin, &creds);
                        // 顺手重扫备份目录（进页面时目录可能被手动动过）。
                        // 放在这个块里是**故意的**：扫描碰文件系统，而
                        // `update_in` 的回调在界面线程上 —— 见
                        // `AppState::rescan_backups` 的文档。
                        let backups = backup_dir
                            .as_deref()
                            .and_then(|dir| state::scan_backup_dir(dir).ok());
                        (databases, users, backups)
                    })
                    .await
            };

            let _ = this.update_in(cx, |shell, window, cx| {
                // 把这次实际用的账号同步到连接卡上：用户看得见"用的是哪个密码"
                // （从环境变量读出来的那个也要显示出来，否则他会以为没生效）
                if let Some(form) = shell.db_form.as_ref() {
                    form.user
                        .update(cx, |state, cx| state.set_value(user.clone(), window, cx));
                    form.password.update(cx, |state, cx| {
                        state.set_value(password.clone(), window, cx)
                    });
                }

                let databases_ok = lists.0.is_ok();
                // ⚠️ 这一整块借着 `shell.state.db`，而下面的提示条要
                // `&mut shell.state` —— 借用没结束就调 `notify` 是编译错误，
                // 所以先把要说的那句话算成 owned 的，出了块再发。
                // 备份列表的重扫同理：等 `db` 借用结束、出了块再做。
                //
                // ⚠️ 不要写成 `let mut rescan = false;` 然后在块里置 true ——
                // 那个初值永远不会被读到（块里必定先赋值），编译器会报
                // `unused_assignments`。下面直接让块**返回**它。
                let (failure_summary, rescan): (Option<String>, bool) = {
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
                        // ⚠️ 这里拿到的是 `wslc_core::Error`，`fail()` 收
                        // `impl Display`，会自己把它变成字符串并分类。
                        Err(message) => {
                            if databases_ok {
                                db.fail(message);
                            }
                        }
                    }
                    // 块返回 `(提示条文案, 要不要重扫备份列表)` ——
                    // `rescan` 恒为 true（走到这里说明 probe 成功、列表已读），
                    // 但保持返回它的形状，将来若某个分支不该重扫就改一处。
                    (db.error.as_ref().map(DbFailure::summary), true)
                };
                // 备份列表：把后台算好的那份填进去（**不**在这里扫盘）。
                if rescan {
                    if let Some(entries) = lists.2 {
                        shell.state.apply_backups(&container, entries);
                    }
                }
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
        // 也走连接卡：用户可能刚在「基础信息」里改过密码再建库，这时
        // 重读列表必须用**新**的那份 —— 用 `db.creds`（上一次 load_db 的）
        // 会让"建库成功、列表却报 Access denied"。
        let creds = self.db_creds(cx);
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
                                db.fail(message);
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
        self.state.set_section_open(UiSection::DbEncoding, false);
        let database = cx.new(|cx| InputState::new(window, cx).placeholder("shop"));
        let charset = cx.new(|cx| {
            InputState::new(window, cx).placeholder("留空 = 服务器默认（一般为 utf8mb4）")
        });
        let collation = cx.new(|cx| InputState::new(window, cx).placeholder("留空 = 跟着字符集"));
        let user = cx.new(|cx| InputState::new(window, cx).placeholder("app"));
        let host = cx.new(|cx| InputState::new(window, cx).placeholder("%（任意主机）"));
        let password = cx.new(|cx| InputState::new(window, cx).placeholder("密码"));
        let grant_db = cx.new(|cx| InputState::new(window, cx).placeholder("留空 = 先不授权"));
        // 候选下拉框：带搜索（库多的机器上一屏放不下），**空列表**在
        // `fill_grant_db_select` 里补上 —— 这里先给空 `Vec`。
        let grant_db_select = cx.new(|cx| {
            SelectState::new(Vec::<DropdownItem>::new(), None, window, cx).searchable(true)
        });
        let db_user = cx.new(|cx| InputState::new(window, cx).placeholder("例如 shop_user"));
        let import_path = cx.new(|cx| {
            InputState::new(window, cx).placeholder(r"点右边的「选择文件…」，或直接粘一个路径")
        });

        // 预填：把行上带过来的用户名 / 主机填好（改密码 / 授权那两种）
        if let DbDialogKind::AlterPassword { user: u, host: h }
        | DbDialogKind::Grant { user: u, host: h } = &kind
        {
            user.update(cx, |state, cx| state.set_value(u.clone(), window, cx));
            host.update(cx, |state, cx| state.set_value(h.clone(), window, cx));
        } else {
            host.update(cx, |state, cx| state.set_value("%".to_owned(), window, cx));
        }
        // 「新建数据库」默认**同时建用户**（v0.8 的主路径），密码先给一个
        // 随机值（用户可见可改）。用户名保持手填，只有点击「用建议值」才填写，
        // 后续修改库名也不覆盖已经填写的用户名。
        let mut create_user = false;
        if kind == DbDialogKind::NewDatabase {
            create_user = true;
            password.update(cx, |state, cx| {
                state.set_value(mysql_model::generate_password(), window, cx)
            });
        }
        // 焦点给"最该填的那一格"
        let first = match &kind {
            DbDialogKind::NewDatabase => &database,
            DbDialogKind::NewUser => &user,
            DbDialogKind::AlterPassword { .. } => &password,
            // 授权：给**输入框**而不是上面那个下拉框。
            // 输入框在两种情况下都可用（下拉框的候选可能是空的），而且
            // 光标落进去就能直接打字；想用列表就点一下上面那格。
            DbDialogKind::Grant { .. } => &grant_db,
            // 导入：最该填的是**文件**（目标库已经由那一行定了）
            DbDialogKind::ImportSql { .. } => &import_path,
        };
        let handle = first.read(cx).focus_handle(cx);
        window.focus(&handle, cx);

        let mut input_subs: Vec<Subscription> = [&database, &charset, &collation]
            .into_iter()
            .map(|input| {
                cx.subscribe(input, |_, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        cx.notify();
                    }
                })
            })
            .collect();
        // 从下拉框里选了一个库 → 写进输入框。
        //
        // ⚠️ 这个订阅**不是可选的**：不订阅的话"从下拉框里选了一个库"什么都不会发生
        // （输入框不变，提交时读的还是空 —— 界面上看起来像"选了但没生效"）。
        // 和「创建容器」里 `image_select` 那条订阅同一个坑：`Subscription` 一 drop
        // 就退订，所以必须存进 `DbDialog`。
        //
        // 用 `subscribe_in` 而不是 `subscribe`：回调里要把值写回 `InputState`，
        // 而它的 `set_value` 要 `&mut Window`（`AGENTS.md` §7.2）。
        input_subs.push(cx.subscribe_in(
            &grant_db_select,
            window,
            |shell, _emitter, event: &SelectEvent<Vec<DropdownItem>>, window, cx| {
                // 和本项目另外四处 `Select` 订阅同一个写法：只有"选了一项"才动作，
                // 清空（`Confirm(None)`，下拉框开了 `cleanable` 才会有）不改输入框
                // —— 那会把用户手打的内容一起抹掉，而"清空"在下拉框上的语义本来
                // 就只是"我不从这儿选了"。
                if let SelectEvent::Confirm(Some(database)) = event {
                    shell.pick_grant_database(database.clone(), window, cx);
                }
            },
        ));
        // 反方向（输入框 → 下拉框）**刻意不做**，理由是一次重入：
        //
        // `pick_grant_database` 用 `InputState::set_value` 写值，而它会**同步**
        // 发 `InputEvent::Change`；如果这里再订阅 `grant_db` 的 Change 去
        // `grant_db_select.update(…)`，那条回调就在 `SelectState` 自己的
        // `Confirm` 事件栈里跑 —— 等于在 `select.update(…)` 内部再
        // `select.update(…)`，GPUI 会直接 panic（"already being updated"）。
        //
        // 代价是：用户手改了输入框之后，下拉框的显示会停在最后选中的那个库上。
        // 这是**能看见的**不一致，所以在界面上用文案把"以哪格为准"说清楚
        // （`views::grant_db_field` 把输入框标成最终生效的那一格），
        // 而不是让两格去追一个会崩的双向同步。

        self.db_dialog = Some(DbDialog {
            _input_subs: input_subs,
            kind,
            database,
            charset,
            collation,
            user,
            host,
            password,
            grant_db,
            grant_db_select,
            grant: GrantKind::All,
            create_user,
            db_user,
            import_path,
        });

        // 候选库（下拉框）要等 `db_dialog` 建好才填得进去。
        self.fill_grant_db_select(window, cx);
        cx.notify();
    }

    /// 把当前读到的**业务库**填进「授权到哪个库」的下拉框。
    ///
    /// 候选由 `DbState::grant_database_options()` 给（纯逻辑，带单测：
    /// 排除系统库 + 按名字排序）。一候选都没有时下拉框就是空的，展开由
    /// `Select::empty(…)` 说一句"还没读到库列表"（见 `views::grant_db_field`）
    /// —— 这一格**不会**因此不可用：旁边那个输入框照样能手打。
    ///
    /// `set_items` 会把下拉框的选中项清掉，所以填完要把输入框里**已经写着**的
    /// 那个库重新选上（用户可能是"先手打了库名，再想用下拉框换一个"）。
    fn fill_grant_db_select(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self.db_dialog.as_ref() else {
            return;
        };
        // 只有这两个动作画那一格（`views::db_dialog_overlay` 里两处
        // `grant_db_field`）。其余三种动作填了也没人看得见。
        //
        // 匹配**引用**（`&dialog.kind`）而不是 `dialog.kind`：`DbDialogKind`
        // 不是 `Copy`（里面带着 `String`），按值匹配在共享引用后面借用不过。
        if !matches!(
            &dialog.kind,
            DbDialogKind::NewUser | DbDialogKind::Grant { .. }
        ) {
            return;
        }
        let select = dialog.grant_db_select.clone();
        let current = dialog.grant_db.read(cx).value().trim().to_owned();
        let items: Vec<DropdownItem> = self
            .state
            .db
            .as_ref()
            .map(|db| db.grant_database_options())
            .unwrap_or_default()
            .into_iter()
            .map(|name| DropdownItem::new(name.clone(), name))
            .collect();

        select.update(cx, |state, cx| {
            state.set_items(items, window, cx);
            if !current.is_empty() {
                state.set_selected_value(&current, window, cx);
            }
        });
    }

    /// 「新建用户 / 授权」弹窗里从下拉框选了一个库：写进输入框。
    ///
    /// # 为什么只往这一个方向同步
    ///
    /// 输入框是**唯一的事实来源**（`submit_db_dialog` 只读它），所以这里只需
    /// 把值写进去。反方向（输入框 → 下拉框）**刻意不做**：`set_value` 会
    /// **同步**发 `InputEvent::Change`，那条回调是在 `SelectState` 自己的
    /// `Confirm` 事件栈里跑的 —— 再 `select.update(…)` 就是 GPUI 的
    /// "already being updated" panic。
    ///
    /// 代价要认：用户手改输入框之后，下拉框可能还显示着上一次选中的库。
    /// 所以 `views::grant_db_field` 在输入框下面写着"**以这里为准**"，
    /// 让"哪一格生效"在界面上是看得出来的，而不是靠用户猜。
    pub fn pick_grant_database(
        &mut self,
        database: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(input) = self.db_dialog.as_ref().map(|d| d.grant_db.clone()) else {
            return;
        };
        input.update(cx, |state, cx| state.set_value(database, window, cx));
        cx.notify();
    }

    /// 「导入 SQL」弹窗里点「选择文件…」。
    ///
    /// 复用面板唯一那个选择器（`cmd::picker` 起独立进程弹 Windows 原生对话框，
    /// 理由见 `AGENTS.md` §7.4）：`sql` 作为扩展名过滤，`AfterPick::Nothing`
    /// —— 选完只填输入框，导入要等用户点「开始导入」。
    pub fn pick_import_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(input) = self.db_dialog.as_ref().map(|d| d.import_path.clone()) else {
            return;
        };
        self.spawn_picker(
            &input,
            false,
            "SQL 文件",
            &["sql"],
            "选择要导入的 SQL 文件",
            AfterPick::Nothing,
            window,
            cx,
        );
    }

    /// 开始导入一份 `.sql`（长任务；状态在 `AppState::db.import`）。
    ///
    /// 目标库**来自弹窗的 kind**（`ImportSql { database }` —— 就是数据库列表
    /// 上被点的那一行），不给用户改的机会：这一条动作的语义就是"导入这个库的
    /// 数据"。它同时消掉了"库名填错"这类错误。
    ///
    /// # 校验都在**起进程之前**，而且失败**不关弹窗**
    ///
    /// 用户就在那一格上（`AGENTS.md` §7.5 的"空值等于没填"同理）：
    /// 文件必须真的在（选了之后被删 / 手打的路径写错，都要当场说）。
    pub fn start_db_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 先把要用的取成 owned（下面要改 self，不能再借着 db_dialog）
        let (path_text, database) = {
            let Some(dialog) = self.db_dialog.as_ref() else {
                return;
            };
            let DbDialogKind::ImportSql { database } = &dialog.kind else {
                return;
            };
            (
                dialog.import_path.read(cx).value().trim().to_owned(),
                database.clone(),
            )
        };

        if path_text.is_empty() {
            self.state.notify(Toast::error("先选一个 .sql 文件"));
            cx.notify();
            return;
        }
        let path = std::path::PathBuf::from(&path_text);
        if !path.is_file() {
            self.state
                .notify(Toast::error(format!("找不到这个文件：{path_text}")));
            cx.notify();
            return;
        }
        // 库名要进 `mysql -D …`（过一次 shell，`load_script` 会引起来），
        // 但先把"这个名字根本不是合法标识符"的情况拦在本地。
        // 它来自我们自己的库列表，正常永远过得去 —— 这一条是防御。
        if let Err(e) = mysql_model::validate_ident("库名", &database) {
            self.state.notify(Toast::error(e.to_string()));
            cx.notify();
            return;
        }

        let Some(db) = self.state.db.as_ref() else {
            return;
        };
        if db.busy || db.import.is_some() || db.backup.is_some() {
            // 有别的长任务在跑：让它先结束（按钮那边也会换成"…中"）
            return;
        }
        let container = db.container.clone();
        let bin = db.client.clone();
        // 按**行**取账号：这一行的库是 `database`，导入就该用它的账号
        //（见 [`Shell::creds_for_row`]）。「导入备份」是在同一个库上灌数据，
        // 和「备份」必须是同一个身份 —— 否则"备份能成、导入 Access denied"
        // 会变成一个只看浮层文案解释不了的现象。
        //
        // ⚠️ 必须在 `database` 被包成 `Option` **之前**取：下面那一步是为了
        // `ImportState` 的字段类型，而这里要的是裸库名。
        let creds = self.creds_for_row(&container, &database, cx);
        let database = Some(database);

        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("(名字读不出来)")
            .to_owned();
        if let Some(db) = self.state.db.as_mut() {
            db.import = Some(ImportState {
                file_name: file_name.clone(),
                database: database.clone(),
                dismissed: false,
            });
        }
        self.db_dialog = None;
        self.state.notify(Toast::info(format!(
            "正在导入 {file_name}{}…",
            database
                .as_deref()
                .map(|d| format!("到 {d}"))
                .unwrap_or_default()
        )));
        cx.notify();

        let wslc = self.state.wslc.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = {
                let wslc = wslc.clone();
                let container = container.clone();
                let bin = bin.clone();
                let creds = creds.clone();
                let path = path.clone();
                let database = database.clone();
                cx.background_executor()
                    .spawn(async move {
                        // ① 送进容器（目录目标 + 先 mkdir，实测见 cmd::mysql）
                        let container_path = wslc_core::cmd::mysql::push_dump_to_container(
                            &wslc, &container, &path,
                        )?;
                        // ② 让容器里的 mysql 读它
                        wslc_core::cmd::mysql::load_sql_file(
                            &wslc,
                            &container,
                            &bin,
                            &creds,
                            &container_path,
                            database.as_deref(),
                        )
                    })
                    .await
            };

            let _ = this.update_in(cx, |shell, _window, cx| {
                if let Some(db) = shell.state.db.as_mut() {
                    if db.container == container {
                        db.import = None;
                    }
                }
                match &result {
                    Ok(()) => {
                        shell.state.notify(Toast::success(format!(
                            "已导入 {file_name}{}",
                            database
                                .as_deref()
                                .map(|d| format!("到 {d}"))
                                .unwrap_or_default()
                        )));
                        // 表数 / 占用会变，重读一次让列表反映出来
                        shell.reload_db_lists(cx);
                    }
                    Err(e) => {
                        let failure = DbFailure::from_text(e.to_string());
                        shell
                            .state
                            .notify(Toast::error(format!("导入失败：{}", failure.summary())));
                        // 原文留在页面上（toast 只给一句"该怎么办"）
                        if let Some(db) = shell.state.db.as_mut() {
                            if db.container == container {
                                db.fail(e.to_string());
                            }
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 从数据库列表某一行开 SQL 控制台：**用这个库 + 这一行的账号**。
    ///
    /// 做法是把那一行的用户名 / 密码**写进连接卡**（而不是藏一份"控制台专用
    /// 凭据"）—— 连接卡本来就是"这次连数据库用哪个账号"的唯一入口，
    /// 写进去用户能看见自己在用谁、也能随时改。库名则记在 `sql_database` 上，
    /// 执行时作为 `mysql -D <库>` 传下去。
    ///
    /// 那一行没有账号记录（不是面板建的库）时**只设库名**，账号保持用户当前
    /// 填的那个 —— 不猜、不编（凭据只对面板建的库有）。
    pub fn open_db_console_for(
        &mut self,
        database: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(db) = self.state.db.as_ref() else {
            return;
        };
        let container = db.container.clone();
        let credential = self
            .state
            .db_credentials
            .get(&container, &database)
            .cloned();

        // ⚠️ 先把两个输入框的句柄 clone 出来，再写值 —— `self.db_form.as_ref()`
        // 那一借必须在这里结束：下面 `mirror_card_into_db_creds` 要 `&mut self`
        // （E0502）。句柄本身是 `Entity`，clone 很便宜。
        let form_inputs = self
            .db_form
            .as_ref()
            .map(|form| (form.user.clone(), form.password.clone()));
        if let (Some(cred), Some((user_input, password_input))) =
            (credential.as_ref(), form_inputs)
        {
            user_input.update(cx, |state, cx| {
                state.set_value(cred.user.clone(), window, cx)
            });
            password_input.update(cx, |state, cx| {
                state.set_value(cred.password.clone(), window, cx)
            });
            // 卡上变了，`db.creds` 这份对照也跟上（它不是执行入口，
            // 见 [`Shell::db_creds`]——不同步只会让那份对照失真）。
            self.mirror_card_into_db_creds(cred);
        }
        if let Some(db) = self.state.db.as_mut() {
            db.sql_database = Some(database.clone());
            db.clear_error();
        }
        // 顺序很重要：**先切页、再给焦点**。
        //
        // `set_page(Page::Sql)` 会调 `ensure_db_form` 把编辑器建出来，而它
        // **首次**创建时会把焦点抢到「密码」那格（见 `ensure_db_form` 末尾）。
        // 所以焦点必须在这之后给，否则用户一进来敲字会打到密码框里去。
        self.set_page(Page::Sql, window, cx);
        if let Some(form) = self.db_form.as_ref() {
            let handle = form.sql.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }
        let toast = match credential {
            Some(cred) => Toast::info(format!(
                "控制台已指向 {database}，账号用 {}（来自面板记的账号）",
                cred.user
            )),
            None => Toast::info(format!(
                "控制台已指向 {database}（这个库不是面板建的，账号仍用连接卡上那个）"
            )),
        };
        self.state.notify(toast);
        cx.notify();
    }

    /// 把控制台的"当前库"清掉（回到不指定 —— 由语句自己 `USE`）。
    pub fn clear_sql_database(&mut self, cx: &mut Context<Self>) {
        if let Some(db) = self.state.db.as_mut() {
            db.sql_database = None;
        }
        cx.notify();
    }

    /// 「新建数据库」弹窗里开关「同时创建用户」。
    pub fn set_db_create_user(&mut self, create_user: bool, cx: &mut Context<Self>) {
        if let Some(dialog) = self.db_dialog.as_mut() {
            dialog.create_user = create_user;
        }
        cx.notify();
    }

    /// 把「数据库用户名」填成建议值 `<库名>_user`（弹窗里那个「用建议值」按钮）。
    ///
    /// ⚠️ 刻意**不**做成"输入库名时自动联动" —— 实机反馈是"让手填，不要自动
    /// 生成"。这里只在用户**主动点**的时候写一次，而且写完就不再联动
    ///（后面改库名不会把它改掉）。
    ///
    /// 建议值取自**当前**库名，所以先填库名再点，值才符合直觉；库名还空着时
    /// 算出来是 `_user`（没有意义），这时只给一句提示。
    pub fn fill_db_username(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self.db_dialog.as_ref() else {
            return;
        };
        let database = dialog.database.read(cx).value().trim().to_owned();
        if database.is_empty() {
            self.state.notify(Toast::error(
                "先填库名，再点「用建议值」（建议值就是库名 + _user）",
            ));
            cx.notify();
            return;
        }
        let suggested = mysql_model::default_user_for(&database);
        let input = dialog.db_user.clone();
        input.update(cx, |state, cx| state.set_value(suggested, window, cx));
        cx.notify();
    }

    /// 关掉动作弹窗（不提交）。
    pub fn close_db_dialog(&mut self, cx: &mut Context<Self>) {
        self.db_dialog = None;
        cx.notify();
    }

    /// 把「导入中」浮层收起来（**不**取消导入）。
    ///
    /// 导入是分钟级的，而那个浮层带遮罩 —— 没有这个出口时用户会被锁在
    /// 屏幕前。收起来之后导入照跑，完成回调里照样发提示、照样重读列表。
    ///
    /// 为什么用 `dismissed` 而不是把 `db.import` 置 `None`：那个 `Option`
    /// 同时是**互斥锁**（`start_db_import` 第一句就查它）。置 `None` 会放开
    /// "再来一份导入"的口子，两份 `container cp` 抢同一个目录。
    pub fn dismiss_db_import(&mut self, cx: &mut Context<Self>) {
        if let Some(db) = self.state.db.as_mut() {
            if let Some(import) = db.import.as_mut() {
                import.dismissed = true;
            }
        }
        self.state.notify(Toast::info(
            "导入还在后台跑 —— 完成后会提示你，列表也会自动重读",
        ));
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
    ///
    /// 「建库同时建用户」（v0.8）返回第三样 `to_record`：账号要等 SQL
    /// **成功之后**才写进本地存档 —— 列表上不能出现一个 MySQL 里
    /// 根本不存在的账号。所有段**先都拼出来、都校验过**再合并成一条
    /// `-e`（`;` 分隔，实测能跑）—— 半途失败时一条都不会发出去，
    /// 不会留下"建了库没建用户"的半成品。
    pub fn submit_db_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 先把要用的东西取成 owned 的（下面要改 `self`，不能再借着它）
        // 闭包**返回 `Result<_, wslc_core::Error>`**：里面的 `?` 全是
        // `Error -> Error`，不需要 From 转换。`String` 的转换在**外面**做
        //（上一版把 `?` 放进 `Result<_, String>` 的闭包里，
        // `String: From<Error>` 不存在 —— CI 抓的）。
        // ⚠️ 类型要写**全路径** `wslc_core::Result`：app.rs 作用域里的裸
        // `Result` 来自 `gpui_kit::*`，它的默认错误类型是 anyhow —— 两个
        // "Result" 长得一样，是不同的类型（CI 抓的）。
        let prepared: wslc_core::Result<
            Option<(String, &'static str, Option<(String, String, String)>)>,
        > = (|| {
            let Some(dialog) = self.db_dialog.as_ref() else {
                return Ok(None);
            };
            let text = |input: &Entity<InputState>| input.read(cx).value().trim().to_owned();

            match &dialog.kind {
                DbDialogKind::NewDatabase => {
                    let charset = text(&dialog.charset);
                    let collation = text(&dialog.collation);
                    let database = text(&dialog.database);
                    let mut parts: Vec<String> = Vec::new();
                    parts.push(mysql_model::create_database_sql(
                        &database,
                        (!charset.is_empty()).then_some(charset.as_str()),
                        (!collation.is_empty()).then_some(collation.as_str()),
                    )?);
                    // 用户名**由用户手填**（v0.9.3 起）。
                    // host 固定 `%`（要远程连的账号，默认锁 localhost 反而添堵）。
                    let mut to_record = None;
                    if dialog.create_user {
                        let user = text(&dialog.db_user);
                        // 留空就拦下来：这条路径下"没填用户名"= 建了一个连不上
                        // 的库，属于 §7.5 那种"看着填了、其实空着"的陷阱。
                        // 报错**不关弹窗**（外面的 Err 分支只发提示），
                        // 用户改一个字就能重试。
                        if user.is_empty() {
                            return Err(wslc_core::Error::InvalidArgument(
                                "请填数据库用户名（不知道填什么就点「用建议值」）".to_owned(),
                            ));
                        }
                        mysql_model::validate_ident("用户名", &user)?;
                        let password = text(&dialog.password);
                        parts.push(mysql_model::create_user_sql(&user, "%", &password)?);
                        parts.push(mysql_model::grant_sql(
                            &database,
                            &user,
                            "%",
                            GrantKind::All,
                        )?);
                        to_record = Some((database, user, password));
                    }
                    Ok(Some((parts.join("; "), dialog.kind.title(), to_record)))
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
                    .map(|sql| Some((sql, dialog.kind.title(), None)))
                }
                DbDialogKind::AlterPassword { user, host } => {
                    mysql_model::alter_user_password_sql(user, host, &text(&dialog.password))
                        .map(|sql| Some((sql, dialog.kind.title(), None)))
                }
                DbDialogKind::Grant { user, host } => {
                    let grant_db = text(&dialog.grant_db);
                    if grant_db.is_empty() {
                        Err(wslc_core::Error::InvalidArgument(
                            "请填要授权给哪个数据库".to_owned(),
                        ))
                    } else {
                        mysql_model::grant_sql(&grant_db, user, host, dialog.grant)
                            .map(|sql| Some((sql, dialog.kind.title(), None)))
                    }
                }
                // 导入**不是**一条 SQL（它是"把文件送进容器让 mysql 读它"），
                // 所以确认按钮那边直接调 `start_db_import`，走不到这里。
                // 这一臂只是为了让 match 完备。
                DbDialogKind::ImportSql { .. } => Ok(None),
            }
        })();

        // 弹窗不在（理论上来不到）→ 什么都不做
        let (sql, title, to_record) = match prepared {
            Ok(None) => return,
            Ok(Some(prepared)) => prepared,
            Err(e) => {
                // 编码字段折叠后，校验错误仍应把用户带到可见的输入框。
                let message = e.to_string();
                let encoding_input = self.db_dialog.as_ref().and_then(|dialog| {
                    if message.contains("字符集") {
                        Some(dialog.charset.clone())
                    } else if message.contains("排序规则") {
                        Some(dialog.collation.clone())
                    } else {
                        None
                    }
                });
                if let Some(input) = encoding_input {
                    self.state.set_section_open(UiSection::DbEncoding, true);
                    let handle = input.read(cx).focus_handle(cx);
                    window.focus(&handle, cx);
                }
                // 不关弹窗，保留输入供用户直接修正。
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
        // 执行用的是**连接卡**上的账号（弹窗里的 user/host 是"被操作的对象"）。
        // 走 [`Shell::db_creds`] 而不是自己拼：那是全页唯一的取账号入口。
        let creds = self.db_creds(cx);

        self.db_dialog = None;
        self.run_db_statement(container, bin, creds, sql, title, to_record, window, cx);
    }

    /// SQL 控制台：执行输入框里那段脚本（**切分后逐条执行**）。
    ///
    /// # v0.11 起结果不再是"一段文本"
    ///
    /// 之前是 `--table` 的 ASCII 表格整段塞进 `log_text` —— 用户看不懂
    /// （实机反馈原话："现有的这个查询界面和数据展示很难看懂"）。现在：
    ///
    /// - 客户端切分（`model::sqlstmt`）→ 逐条执行（`cmd::mysql::run_statements`）；
    /// - 每条一个 `SqlResult`，界面上画成真正的表格（`views::db_console_card`）；
    /// - 同时留一份 `--table` 的原文进 `sql_raw`，作"看原始输出"的兜底。
    pub fn run_db_console(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = self.db_form.as_ref() else {
            return;
        };
        // ⚠️ `EditorState` 用 `text()`，不是 `InputState::value()`
        // （`text()` 返回的还不是 `String`，要 `.to_string()` 一次）。
        let sql = form.sql.read(cx).text().to_string();
        let sql = sql.trim().to_owned();
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
        // 控制台**针对哪个库**：从数据库列表行上的「SQL」按钮进来时是那一行，
        // 否则是 `None`（由语句自己 `USE`）。不指定时 `SELECT * FROM t`
        // 会报 "No database selected"（实测过）。
        let database = db.sql_database.clone();
        // 账号也要跟着**那个库**走：从 `djxm` 行的「SQL」进来就该用 `djxm_user`，
        // 而不是"连接卡上碰巧是谁"（见 [`Shell::creds_for_row`]）。
        // `sql_database` 为 `None`（手动清掉了当前库）时按行的兜底规则取——
        // 那时用连接卡，因为没有一个具体的库可以查存档。
        let creds = match database.as_deref() {
            Some(name) => self.creds_for_row(&container, name, cx),
            None => self.db_creds(cx),
        };

        if let Some(db) = self.state.db.as_mut() {
            db.busy = true;
            db.clear_error();
            db.last_sql = sql.clone();
            db.sql_results.clear();
            db.sql_raw = None;
            db.sql_error = None;
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
                let database = database.clone();
                cx.background_executor()
                    .spawn(async move {
                        let started = std::time::Instant::now();
                        // 结构化结果（界面画表格）+ 原始文本（兜底）。
                        //
                        // `run_statements` 逐条跑；`run_console_sql` 那一次
                        // `--table` 只跑**整段**，专门用来留原文 ——
                        // 多花一次 exec 换"能看到 mysql 原样打了什么"，
                        // 排障时这点开销值得。
                        let structured = wslc_core::cmd::mysql::run_statements(
                            &wslc,
                            &target,
                            &bin,
                            &creds,
                            &sql,
                            database.as_deref(),
                        );
                        let elapsed = started.elapsed().as_millis() as u64;
                        let raw = wslc_core::cmd::mysql::run_console_sql(
                            &wslc,
                            &target,
                            &bin,
                            &creds,
                            &sql,
                            database.as_deref(),
                        )
                        .ok();
                        (structured.map_err(|e| e.to_string()), raw, elapsed)
                    })
                    .await
            };

            let _ = this.update_in(cx, |shell, window, cx| {
                shell.apply_db_console_result(container, result, window, cx);
            });
        })
        .detach();
    }

    /// 把一次 SQL 执行的结果敲进状态。
    ///
    /// `result` 是 `(结构化结果, 原始输出, 耗时毫秒)`。拆成独立函数是为了
    /// 让 `spawn_in` 那个闭包只做"搬运"，判断都集中在有注释的这里。
    ///
    /// ⚠️ 要 `&mut Window`：结果集变了就得**重建** `TableState`
    /// （`TableState::new` 要 window，和 `InputState::new` 同一个道理）。
    fn apply_db_console_result(
        &mut self,
        container: String,
        result: (
            std::result::Result<Vec<wslc_core::model::sqlresult::SqlResult>, String>,
            Option<String>,
            u64,
        ),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (structured, raw, elapsed_ms) = result;
        let mut reload = false;
        // 重建表格要用的东西先算好（`sql_table` 是 `self` 的字段，
        // 而 `db` 借的是 `self.state`，两者同时可变借用会撞车）。
        let mut new_table: Option<wslc_core::model::sqlresult::SqlResult> = None;
        if let Some(db) = self.state.db.as_mut() {
            if db.container != container {
                return;
            }
            db.busy = false;
            db.sql_ran = true;
            db.sql_elapsed_ms = elapsed_ms;
            db.sql_raw = raw;
            match structured {
                Ok(results) => {
                    db.sql_results = results;
                    db.sql_error = None;
                    db.clear_error();
                    // 第一个**有表格**的结果作为默认 tab —— 全是 DDL 时停在 0。
                    let first_grid = db
                        .sql_results
                        .iter()
                        .position(|r| !r.columns.is_empty())
                        .unwrap_or(0);
                    new_table = db.sql_results.get(first_grid).cloned();
                    self.sql_result_tab = first_grid;
                    // 控制台可能刚改了库 / 表 —— 顺手把列表刷新一次。
                    // 失败时列表不会变，就不白跑一趟了。
                    reload = true;
                }
                Err(message) => {
                    // SQL 报错**不算**页面级错误：它就发生在控制台里，
                    // 显示在控制台下面比顶到页头更贴合上下文。
                    //
                    // ⚠️ 必须存进 `sql_error`（而不是像 v0.10 那样塞回输出）：
                    // 渲染那边靠它区分"红字错误"和"结果表格"。v0.10 曾经只置
                    // `sql_ran = true` 而让输出是 `None`，界面显示
                    // **"正在加载…"** 而真正的报错去了页头（实机截图抓到的：
                    // `show database` 拼错那次）。
                    db.sql_results.clear();
                    db.sql_error = Some(message);
                    // 页面级那条留给"读列表 / 连接"这类失败，
                    // 清了它免得同一条错误在屏幕上出现两遍。
                    db.clear_error();
                }
            }
        }
        // 表格整个换掉。`None`（报错 / 全是 DDL / 没有结果）时也清，
        // 免得旧结果还留在屏幕上。
        self.sql_table = new_table.map(|r| {
            cx.new(|cx| {
                gpui_kit::component::table::TableState::new(
                    crate::sql_table::SqlResultDelegate::new(r),
                    window,
                    cx,
                )
                .cell_selectable(true)
            })
        });
        if reload {
            self.reload_db_lists(cx);
        }
        cx.notify();
    }

    /// 切换结果 tab（多语句时）。
    ///
    /// 换 tab 要重建 `TableState` —— 每个结果的列和行都不一样，没法复用。
    pub fn switch_sql_result_tab(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(db) = self.state.db.as_ref() else {
            return;
        };
        let Some(result) = db.sql_results.get(index) else {
            return;
        };
        if result.columns.is_empty() {
            // 没有表格的结果（DDL / 无输出）不建表格 —— 界面画一句说明就够了
            self.sql_table = None;
        } else {
            let result = result.clone();
            self.sql_table = Some(cx.new(|cx| {
                gpui_kit::component::table::TableState::new(
                    crate::sql_table::SqlResultDelegate::new(result),
                    window,
                    cx,
                )
                .cell_selectable(true)
            }));
        }
        self.sql_result_tab = index;
        cx.notify();
    }

    /// 切换「看原始输出」的展开状态。
    pub fn toggle_sql_raw(&mut self, cx: &mut Context<Self>) {
        self.sql_raw_open = !self.sql_raw_open;
        cx.notify();
    }

    /// 把当前结果导出成 CSV（v0.11.3）。
    ///
    /// # 流程
    ///
    /// 弹保存对话框 → 用户在后台等 → 写文件（**原子写**，见 `fileio`）。
    ///
    /// # 为什么写盘也在后台
    ///
    /// 几万行的 CSV 是几 MB，写到机械盘 / 网络盘上要几百毫秒。
    /// `AGENTS.md` §7.1：界面线程上的任何阻塞都会让窗口"未响应"。
    /// 这里虽然不是起进程，但 `std::fs::write` 一样会卡。
    ///
    /// # 为什么复用 `picking`
    ///
    /// 那个布尔的作用是"已经有一个选择框开着"（见 `spawn_picker`）。
    /// 导出也弹选择框，两边共用同一个守卫才不会一次弹出两个对话框
    /// —— 任务栏里堆一排对话框是最糟的。
    pub fn export_sql_result_csv(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.picking {
            self.state
                .notify(Toast::info("已经有一个选择框开着了，先处理它"));
            cx.notify();
            return;
        }

        // ⚠️ 这一段**整体**放在一个块里，让 `db`（对 `self.state` 的不可变借用）
        // 在块结束时立刻归还。外面还要 `self.state.notify(..)`（那要 `&mut`）
        // 和 `self.picking = true`（要 `&mut self`）—— 借用没结束就写，是 E0502。
        //
        // 这个形状在 `open_db_backup_dialog` / `run_db_console` 里都用过：
        // 先把要用的东西算成 owned 的，出了块再用。
        let prepared: Option<(String, usize, String)> = {
            let Some(db) = self.state.db.as_ref() else {
                return;
            };
            let result = db.sql_results.get(self.sql_result_tab);
            // 没有结果、或者结果没有表格（DDL / 无输出）→ 不给导出。
            // 一个只有表头的 CSV 没有意义，只会让用户以为导出坏了。
            match result {
                Some(r) if !r.columns.is_empty() => {
                    // 建议文件名：`库-时间戳.csv`。表名我们不知道（结果可能是
                    // JOIN 出来的），所以用当前库名兜底。
                    let stamp = wslc_core::model::mysql::now_secs();
                    let base = db.sql_database.as_deref().unwrap_or("result").to_owned();
                    // ⚠️ 行数**现在**记下来，不要去数 CSV 字符串里有多少个换行 ——
                    // 单元格里的换行是被 `"` 包着的（RFC 4180），
                    // `lines().count()` 会把一个跨两行的单元格数成两行。
                    Some((
                        format!("{base}-{stamp}.csv"),
                        r.row_count(),
                        crate::sqlview::to_csv(r),
                    ))
                }
                _ => None,
            }
        };

        let Some((suggested, row_count, csv)) = prepared else {
            self.state
                .notify(Toast::error("这个结果没有表格可以导出"));
            cx.notify();
            return;
        };

        self.picking = true;
        cx.notify();

        let title = "导出 CSV".to_owned();
        cx.spawn_in(window, async move |this, cx| {
            // 第一步：选路径（后台起 powershell 等对话框）。
            let picked = cx
                .background_executor()
                .spawn(async move {
                    wslc_core::cmd::picker::pick_save_file(&title, &suggested, &["csv"])
                })
                .await;

            // ⚠️ `update_in` 的回调是**同步**的，里面不能再 await。
            // 所以"收下路径 → 交回一个待写的任务"这两步要分开：
            // 回调只负责把 `picking` 复位、把决定带出来；
            // 真正的写盘在**外面**（还在这个 async 块里）做。
            //
            // 项目里另一个 `background_executor` 也只出现在 `cx.spawn` 的
            // 顶层（见 `start_toast_reaper`），没有"在 update 回调里 await"的先例。
            //
            // 返回值：`Ok(决定)` = 回调跑过了；`Err(_)` = 实体已销毁（窗口关了）。
            let decision = this.update_in(cx, |shell, _window, _cx| {
                shell.picking = false;
                match picked {
                    Ok(Some(path)) => SaveDecision::Write(std::path::PathBuf::from(path)),
                    Ok(None) => SaveDecision::Cancelled,
                    Err(e) => SaveDecision::Failed(e.to_string()),
                }
            });

            let target = match decision {
                // 实体已销毁 / 用户取消 —— 都不用说话
                Err(_) => return,
                Ok(SaveDecision::Cancelled) => return,
                Ok(SaveDecision::Failed(e)) => {
                    let _ = this.update(cx, |shell, cx| {
                        shell
                            .state
                            .notify(Toast::error(format!("打开保存对话框失败：{e}")));
                        cx.notify();
                    });
                    return;
                }
                Ok(SaveDecision::Write(p)) => p,
            };

            // 第二步：写盘（后台 —— 几 MB 的 CSV 落到机械盘上要几百毫秒）。
            let body = csv;
            let write_to = target.clone();
            let write = cx
                .background_executor()
                .spawn(async move {
                    wslc_panel_core::fileio::write_atomic(&write_to, &body)
                        .map_err(|e| e.to_string())
                })
                .await;

            let _ = this.update(cx, |shell, cx| {
                match write {
                    Ok(()) => shell.state.notify(Toast::success(format!(
                        "已导出 {row_count} 行到 {}",
                        target.display()
                    ))),
                    Err(e) => shell
                        .state
                        .notify(Toast::error(format!("写文件失败：{e}"))),
                }
                cx.notify();
            });
        })
        .detach();
    }

    // -- SQL 控制台的库 / 表树（v0.11.2，v0.12.1 换成组件的 `Tree`）-----------

    /// 确保搜索框存在（`InputState::new` 要 `&mut Window`，所以懒创建）。
    pub(crate) fn ensure_schema_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.schema_search.is_some() {
            return;
        }
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("过滤库 / 表名")
        });
        // 订阅：打字时**重建树**（不是"只重绘"）。
        //
        // 过滤不能用"渲染时临时筛"那套了：`TreeState` 的 items 是**喂进去**的，
        // 组件自己不知道原始数据长什么样。所以关键字一变就得重新灌一份 items。
        //
        // ⚠️ 订阅**必须存进字段**，不能 `let _ = ...` —— drop 即退订，
        // 表现是"打字没反应"，而不会报错（`container_search_sub` 那个坑）。
        let sub = cx.subscribe(&input, |shell, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let query = input.read(cx).value().to_string();
                shell.rebuild_schema_tree(&query, cx);
                cx.notify();
            }
        });
        self.schema_search = Some(input);
        self.schema_search_sub = Some(sub);
    }

    /// 按当前数据 + 关键字**重建** `TreeState` 的 items。
    ///
    /// # 什么时候会重建
    ///
    /// - 读到新的表结构之后；
    /// - 搜索关键字变了；
    /// - 第一次进这一页。
    ///
    /// # 为什么关键字不变就跳过
    ///
    /// `TreeState` 重建会**丢掉用户已经展开的层级**（展开状态住在组件里）。
    /// 所以只有 `(关键字, 表总数)` 真的变了才重灌 —— 这个指纹在
    /// [`Shell::schema_tree_built_for`] 里。
    ///
    /// # 两种形态
    ///
    /// - **没搜索**：库是父节点、表是子节点（默认收起，点库名展开）；
    /// - **有搜索**：**拍平**成一层「库.表」。理由：树的父节点默认是收起的，
    ///   搜索时如果还挂着父节点，用户会看到一个收起的库名、得再点一下才见到
    ///   他刚搜的那张表 —— 那就不叫搜索了。拍平之后命中的表直接在眼前。
    pub(crate) fn rebuild_schema_tree(&mut self, query: &str, cx: &mut Context<Self>) {
        use gpui_kit::component::tree::TreeItem;

        let Some(db) = self.state.db.as_ref() else {
            return;
        };
        let total = db.schema_tree.table_count();
        let fingerprint = (query.trim().to_owned(), total);
        // 指纹没变就别动 —— 见函数文档（重建会丢展开状态）。
        if self.schema_tree_built_for.as_ref() == Some(&fingerprint)
            && self.schema_tree_state.is_some()
        {
            return;
        }

        let filtered = db.schema_tree.filter(query);
        let searching = !query.trim().is_empty();
        let mut items: Vec<TreeItem> = Vec::new();

        for node in &filtered.databases {
            if searching {
                // 拍平：每条都是叶子，id = `t\u{1}库\u{1}表`。
                for t in &node.tables {
                    items.push(TreeItem::new(
                        leaf_id(&node.name, &t.name),
                        format!("{}.{}", node.name, t.name),
                    ));
                }
            } else {
                let children: Vec<TreeItem> = node
                    .tables
                    .iter()
                    .map(|t| {
                        let label = match t.kind.tag() {
                            "" => t.name.clone(),
                            tag => format!("{}  {tag}", t.name),
                        };
                        TreeItem::new(leaf_id(&node.name, &t.name), label)
                    })
                    .collect();
                let label = if node.is_system() {
                    format!("{}（系统）", node.name)
                } else {
                    node.name.clone()
                };
                items.push(TreeItem::new(folder_id(&node.name), label).children(children));
            }
        }

        let state = cx.new(|cx| gpui_kit::component::tree::TreeState::new(cx));
        state.update(cx, |s, cx| s.set_items(items, cx));
        self.schema_tree_state = Some(state);
        self.schema_tree_built_for = Some(fingerprint);
    }

    /// 点树里的一行。
    ///
    /// `id` 的格式见 [`leaf_id`] / [`folder_id`]：`t\u{1}库\u{1}表` 是表，
    /// `d\u{1}库` 是库（点库由组件自己展开/收起，这里什么都不做）。
    pub fn on_schema_tree_click(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut parts = id.split('\u{1}');
        let Some(kind) = parts.next() else {
            return;
        };
        if kind != "t" {
            // 库节点：展开 / 收起由 `Tree` 自己做，不用我们管。
            return;
        }
        let (Some(database), Some(table)) = (parts.next(), parts.next()) else {
            return;
        };
        self.pick_schema_table(database.to_owned(), table.to_owned(), window, cx);
    }

    /// 读整棵库 / 表树（进控制台时调一次）。
    ///
    /// 失败时**不清空**已有的树：用户可能已经有树了，一次读失败不该让它消失
    /// （比如网络抖了一下）。只在**从未读到过**时才发提示。
    pub fn load_schema_tree(&mut self, cx: &mut Context<Self>) {
        let Some(db) = self.state.db.as_ref() else {
            return;
        };
        let container = db.container.clone();
        let bin = db.client.clone();
        // 树是全库可见的（`information_schema`），但账号得有权限 ——
        // 按行的兜底规则取（有具体库时用那个库的账号，否则用连接卡）。
        let creds = match db.sql_database.as_deref() {
            Some(name) => self.creds_for_row(&container, name, cx),
            None => self.db_creds(cx),
        };
        let wslc = self.state.wslc.clone();
        // ⚠️ `container` 在后面 `update` 里还要用（判断"是不是还停在同一个容器"），
        // 而内层的 `async move` 会把它搬走 —— 所以备一份 clone。
        // 这个坑在 `delete_db_backup_confirmed` 里踩过一次（CI 的 E0382）。
        let exec_container = container.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    wslc_core::cmd::mysql::list_schema_tree(&wslc, &exec_container, &bin, &creds)
                        .map_err(|e| e.to_string())
                })
                .await;
            let _ = this.update(cx, |shell, cx| {
                let mut ok = false;
                if let Some(db) = shell.state.db.as_mut() {
                    if db.container != container {
                        return;
                    }
                    match result {
                        Ok(tree) => {
                            db.schema_tree = tree;
                            ok = true;
                        }
                        Err(message) => {
                            // 之前读到过就不打扰（见函数文档）
                            if db.schema_tree.is_empty() {
                                shell
                                    .state
                                    .notify(Toast::info(format!("读不到表结构：{message}")));
                            }
                        }
                    }
                }
                if ok {
                    // 换了数据就得重灌 items。指纹先清掉，否则"表总数没变、
                    // 但表名变了"（改表名场景）会被误判成不变。
                    shell.schema_tree_built_for = None;
                    let query = shell
                        .schema_search
                        .as_ref()
                        .map(|i| i.read(cx).value().to_string())
                        .unwrap_or_default();
                    shell.rebuild_schema_tree(&query, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 点树里的表名 → 把 `SELECT * FROM \`库\`.\`表\` LIMIT 200` 填进编辑区。
    ///
    /// # 为什么是"填进去"而不是"直接执行"
    ///
    /// 用户点表是想看数据，但**先看一眼 SQL** 是更安全的默认：他可能想加
    /// `WHERE`，也可能只是点错了表。填进去之后按一下「执行」就行，
    /// 而"点了就立刻跑一个全表扫描"是不可撤销的（尤其是生产库）。
    pub fn pick_schema_table(
        &mut self,
        database: String,
        table: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let sql = wslc_core::model::schematree::select_from_table_sql(&database, &table);
        // 顺便把「当前库」设成这个表所在的库 —— 否则 `SELECT * FROM \`库\`.\`表\``
        // 虽然能跑，但下面再手写 `SELECT * FROM 别的表` 时会报
        // "No database selected"（实测过）。
        if let Some(db) = self.state.db.as_mut() {
            db.sql_database = Some(database);
        }
        if let Some(form) = self.db_form.as_ref() {
            form.sql
                .update(cx, |state, cx| state.set_value(sql, window, cx));
        }
        cx.notify();
    }

    /// 从数据库管理页回到容器列表。
    ///
    /// **不**清 `state.db`：下次再进来时连接信息与刚读到的两张表都还在
    /// （省掉一次探测 + 两次查询）。
    pub fn close_database_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_page(Page::Containers, window, cx);
        cx.notify();
    }

    // -- 部署管理（nginx / Java / Node.js / Go，v0.9）------------------------

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
        // 这比先显示一张空表诚实得多（`crate::deploys` 的模块注释讲了为什么
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
    fn refresh_deploy_status(&mut self, cx: &mut Context<Self>) {
        self.refresh_deploy_status_inner(true, cx);
    }

    /// 慢档轮询用的那一条（[`Shell::tick_deploy_status`]）：**保留旧值**。
    ///
    /// 不清 `probed` / `process` / `files`，所以界面在重探期间继续显示上一次
    /// 问到的结果；结果回来才覆盖。理由：9 秒一轮的轮询如果每轮都把整列刷成
    /// 「读取中…」，用户看到的是**一直在闪** —— 那比"数据晚几秒"难受得多。
    fn refresh_deploy_status_quiet(&mut self, cx: &mut Context<Self>) {
        self.refresh_deploy_status_inner(false, cx);
    }

    /// 公共实现。`clear_first = true` → 先把每一行的现状清掉（画「读取中…」）。
    fn refresh_deploy_status_inner(&mut self, clear_first: bool, cx: &mut Context<Self>) {
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
    fn resync_deploy_command(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
    fn focus_deploy_field(
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
    fn read_deploy_form(&self, cx: &App) -> Option<DeployFormValues> {
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
        //（`crate::deploys::merge_members`，纯函数、有单测）。
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
        let removals = crate::deploys::merge_members(&previous_members, &member_names);

        // 记下要写进存档的东西（弹窗马上要关，那之后就读不到了）。
        //
        // ⚠️ 存档里的 `members` 记的是**这一次真的铺了什么**，不是删除列表 ——
        // 下次重新部署时"上一次"取的就是它。存删除列表的话，那些"上一次有、
        // 这一次没有"的名字会一代代累积下去，记录越读越假。
        let record = crate::deploys::DeployRecord {
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
                        crate::deploys::DeployRecord {
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
                            crate::deploys::DeployRecord {
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
                            crate::deploys::DeployRecord {
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
    fn deploy_page_is(&self, container: &str) -> bool {
        self.state
            .deploy
            .as_ref()
            .is_some_and(|deploy| deploy.container == container)
    }

    /// 一次部署动作要用的素材（从当前页面状态 + 存档里取）。
    ///
    /// 类型认不出来 / 找不到记录 → `None`（界面上那些按钮也不会画）。
    fn deploy_action(&self, app: &str) -> Option<DeployAction> {
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
    fn set_deploy_progress(&mut self, progress: Option<DeployProgress>, cx: &mut Context<Self>) {
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
    fn clear_deploy_progress_for(&mut self, app: &str, cx: &mut Context<Self>) {
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
    fn deploy_busy_notice(&mut self, cx: &mut Context<Self>) {
        self.state.notify(Toast::info(
            "已经有一个部署动作在跑 —— 等它结束再点（进度显示在那一行上）",
        ));
        cx.notify();
    }

    /// 只换阶段（同一个部署动作的下一步）。
    fn set_deploy_phase(&mut self, phase: DeployPhase, cx: &mut Context<Self>) {
        if let Some(deploy) = self.state.deploy.as_mut() {
            if let Some(progress) = deploy.progress.as_mut() {
                progress.phase = phase;
            }
        }
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
        // 「建库同时建用户」的账号 —— SQL **成功之后**才写进本地存档
        //（列表里展示的就是它）。放在执行**前**传进来，是因为弹窗已经关了，
        // 输入框的值拿不到了。
        to_record: Option<(String, String, String)>,
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
                    Ok(()) => {
                        // 成功才写档：列表上不能出现一个 MySQL 里不存在的账号。
                        // 写档失败只提示（最坏是重启后要重新录入），不回滚。
                        if let Some((database, user, password)) = to_record {
                            shell.state.db_credentials.put(
                                &container,
                                &database,
                                wslc_panel_core::dbcreds::DbCredential {
                                    user,
                                    password,
                                    host: "%".to_owned(),
                                },
                            );
                            if let Some(e) = shell.state.save_db_credentials() {
                                shell.state.notify(Toast::error(e));
                            }
                        }
                        shell.state.notify(Toast::success(format!("{title}成功")));
                    }
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

    // -- 数据库备份 / 恢复（v0.8）--------------------------------------------
    //
    // 实测过的路：
    // 备份 = 容器内 `mysqldump … > /tmp/wslc-backups/…` + `container cp` 拉回 Windows；
    // 恢复 = 把 **Windows 侧那份** `container cp` 进
    //        `/tmp/wslc-restore/`（目标写成**目录**）+ 容器内 `mysql … < …`。
    //        容器重建 / `/tmp` 被清都不再是障碍（v0.8.2 改，
    //        完整实测矩阵见 `wslc_core::model::mysql::restore_container_path`）。

    /// 备份一个数据库（长任务；状态在 `AppState::db.backup`）。
    pub fn start_db_backup(
        &mut self,
        database: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(db) = self.state.db.as_ref() else {
            return;
        };
        if db.backup.is_some() {
            // 已经有一个在跑 —— 按钮那边也换成了「备份中…」
            return;
        }
        if db.busy || db.import.is_some() {
            // 读列表 / 导入没结束时不开新的长任务（进度浮层也挡着点击，
            // 这一条是防"从别的路径调进来"）
            //
            // ⚠️ 别静默返回：点了没反应会被当成"按钮坏了"（`AGENTS.md` §7.1）。
            // 这个分支原先一声不响 —— 而现在它是「立即备份」唯一可能的失败方式。
            self.state.notify(Toast::info(
                "正在读列表 / 导入中 —— 等它结束再点「立即备份」",
            ));
            cx.notify();
            return;
        }
        let Some(backup_dir) = self.state.backup_dir.clone() else {
            self.state
                .notify(Toast::error("LOCALAPPDATA 未设置，备份没地方放"));
            cx.notify();
            return;
        };
        let container = db.container.clone();
        let bin = db.client.clone();
        // 按**行**取账号：面板给这个库记过就用它自己的账号，没记过才用连接卡
        //（见 [`Shell::creds_for_row`]）。这一行是 `djxm`，备份就必须是
        // `djxm_user` 在 dump `djxm` —— 而不是"连接卡上碰巧是什么"。
        let creds = self.creds_for_row(&container, &database, cx);
        let paths = match wslc_core::cmd::mysql::backup_paths(
            &backup_dir,
            &container,
            &database,
            wslc_core::model::mysql::now_secs(),
        ) {
            Ok(paths) => paths,
            Err(e) => {
                self.state.notify(Toast::error(e.to_string()));
                cx.notify();
                return;
            }
        };

        if let Some(db) = self.state.db.as_mut() {
            db.backup = Some(BackupState {
                database: database.clone(),
                container_path: paths.container_path.clone(),
                windows_path: paths.windows_path.clone(),
                last_message: None,
                dismissed: false,
            });
        }
        self.state
            .notify(Toast::info(format!("正在备份数据库 {database}…")));
        cx.notify();

        // 备份目录先算出来：下面完成回调里要在**后台**重扫它。
        let backup_scan_dir = self.state.container_backup_dir(&container);
        let wslc = self.state.wslc.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = {
                let wslc = wslc.clone();
                let container = container.clone();
                let bin = bin.clone();
                let creds = creds.clone();
                let database = database.clone();
                let paths = paths.clone();
                // ⚠️ `BackgroundExecutor::spawn` 只吃 **future**（不是普通闭包）
                // —— `async move { 阻塞活 }` 把同步调用包进去，跟其它命令一个写法
                cx.background_executor()
                    .spawn(async move {
                        wslc_core::cmd::mysql::backup_database(
                            &wslc, &container, &bin, &creds, &database, &paths,
                        )
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                    })
                    .await
            };

            // 重扫备份目录也要在**后台**做（`read_dir` + 每条 metadata）：
            // 下面那个 `update_in` 跑在界面线程上。失败不致命 —— 列表保持原样。
            let rescanned = match backup_scan_dir {
                Some(dir) => cx
                    .background_executor()
                    .spawn(async move { state::scan_backup_dir(&dir).ok() })
                    .await,
                None => None,
            };

            let _ = this.update_in(cx, |shell, _window, cx| {
                let finished = shell
                    .state
                    .db
                    .as_mut()
                    .filter(|db| db.container == container)
                    .map(|db| db.backup.take());
                match (&result, finished.flatten()) {
                    (Ok(()), Some(state)) => {
                        if let Some(entries) = rescanned {
                            shell.state.apply_backups(&container, entries);
                        }
                        shell.state.notify(Toast::success(format!(
                            "备份完成：{}（容器内留了一份用于恢复）",
                            state.windows_path.display()
                        )));
                    }
                    (Ok(()), None) => {
                        // 备份完成但页面已切走：文件是好的，一句话说明
                        shell
                            .state
                            .notify(Toast::info("备份完成（数据库页已切走）"));
                    }
                    (Err(message), _) => {
                        if let Some(db) = shell.state.db.as_mut() {
                            if db.container == container {
                                db.fail(message.clone());
                            }
                        }
                        let failure = DbFailure::from_text(message.clone());
                        shell
                            .state
                            .notify(Toast::error(format!("备份失败：{}", failure.summary())));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 关掉备份列表浮层。
    pub fn close_db_backup_dialog(&mut self, cx: &mut Context<Self>) {
        if let Some(db) = self.state.db.as_mut() {
            db.backup_dialog = None;
        }
        cx.notify();
    }

    /// 打开某个库的备份列表（列表是 Windows 侧目录扫出来的）。
    ///
    /// 顺手把面板记的账号**写进连接卡**：这一页所有动作都按行取账号
    ///（`Shell::creds_for_row`），而卡是没记账号时的兜底。从列表行进来的用户
    /// 根本没见过那张卡（它藏在页头的「基础信息」后面）。不写的话，
    /// **面板建的库**（有自己的账号，root 往往不对）会拿 root 去 dump，
    /// 报一句 Access denied，而用户没地方知道该填谁 —— 浮层里那行
    /// "备份用账号 X" 也因此显示了正确的值。
    /// 理由和 `open_db_console_for` 完全一样：卡上显示什么，执行就用什么。
    ///
    /// ⚠️ 写完卡还要顺手写 `db.creds`：那个字段不再是执行入口（见
    /// [`Shell::db_creds`]），但它仍然是"上一次连成功的那份"，留着是为了
    /// 将来做诊断时能和卡上的值对照 —— 不同步会让那份对照失真。
    pub fn open_db_backup_dialog(
        &mut self,
        database: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(db) = self.state.db.as_ref() else {
            return;
        };
        let container = db.container.clone();
        let credential = self
            .state
            .db_credentials
            .get(&container, &database)
            .cloned();

        // 同 `open_db_console_for`：句柄先 clone，别让 `db_form` 的借
        // 跨到 `mirror_card_into_db_creds` 的 `&mut self` 上去（E0502）。
        let form_inputs = self
            .db_form
            .as_ref()
            .map(|form| (form.user.clone(), form.password.clone()));
        if let (Some(cred), Some((user_input, password_input))) =
            (credential.as_ref(), form_inputs)
        {
            user_input.update(cx, |state, cx| {
                state.set_value(cred.user.clone(), window, cx)
            });
            password_input.update(cx, |state, cx| {
                state.set_value(cred.password.clone(), window, cx)
            });
            // 卡上变了，`db.creds` 这份对照也跟上（见 [`Shell::db_creds`]）。
            self.mirror_card_into_db_creds(cred);
        }
        if let Some(db) = self.state.db.as_mut() {
            db.backup_dialog = Some(database);
        }
        // 进浮层前重扫一次（可能在文件管理器里手动删过 / 拷进来过）
        self.state.rescan_backups(&container);
        cx.notify();
    }

    /// 恢复一份备份（**覆盖**该库现有数据 —— 走二次确认）。
    pub fn restore_db_backup(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(db) = self.state.db.as_ref() else {
            return;
        };
        if db.backup.is_some() || db.busy {
            return;
        }
        let Some(entry) = db.backups.get(index) else {
            return;
        };
        // 账号按**这条备份所属的库**取（`Shell::creds_for_row`）：
        // 备份浮层点开时已经把自己钉在某个库上（`backup_dialog`），
        // 「恢复」就是"把这条备份灌回它的原库"，所以身份必须和「备份」
        // 完全一致 —— 备份用 `djxm_user`、恢复也必须用它。
        //
        // 这一行原先取的是 `db.creds`（只在 `load_db` 里赋一次值），
        // 于是"点某一行的「备份」把卡换成 shop_user，再在同一个浮层里点
        // 「恢复」"会用 **root** 去恢复，报 Access denied 而浮层上明明写着
        // 「备份用账号 shop_user」。现在两条路径都按行走，不再看卡。
        let creds = self.creds_for_row(&db.container, &entry.database, cx);
        let action = PendingAction::RestoreDatabase {
            container: db.container.clone(),
            database: entry.database.clone(),
            client: db.client.clone(),
            // 恢复的输入是 **Windows 侧那份**：执行时会把它 cp 进容器
            //（所以容器重建过也照样能恢复，见 `PendingAction::RestoreDatabase`）
            windows_path: entry.path.clone(),
            creds,
        };
        self.request(action, cx);
    }

    /// 请求删除一份备份文件（**先弹二次确认**）。
    ///
    /// 删除是不可撤销的：Windows 侧那份文件删了就是删了，容器内那份也一起
    /// `rm -f`，面板里没有回收站。它的兄弟动作「恢复」一直有确认（覆盖数据
    /// 至少还能再备份回来），而删除原先**没有** —— 这一条是补那个反着的配置。
    ///
    /// 真正的执行在 [`Shell::delete_db_backup_confirmed`]：那个函数只拿
    /// `PendingAction::DeleteDatabaseBackup` 里那几个字段，不再自己查索引
    /// （确认框开着的期间列表可能已经变了，按索引取会删错一条）。
    pub fn delete_db_backup(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(db) = self.state.db.as_ref() else {
            return;
        };
        let Some(entry) = db.backups.get(index) else {
            return;
        };
        let action = PendingAction::DeleteDatabaseBackup {
            container: db.container.clone(),
            database: entry.database.clone(),
            windows_path: entry.path.clone(),
            container_path: entry.container_path.clone(),
        };
        self.request(action, cx);
    }

    /// 执行一份备份的删除（已经从二次确认里点过「永久删除」）。
    ///
    /// 参数是**确认时定下来的**路径，不再按索引回查：确认框开着的几秒里
    /// 用户可能在文件管理器里删过、或者又备份了一份（列表重排），
    /// 按索引取会删掉另一条。
    pub fn delete_db_backup_confirmed(
        &mut self,
        container: String,
        database: String,
        windows_path: std::path::PathBuf,
        container_path: String,
        cx: &mut Context<Self>,
    ) {
        let wslc = self.state.wslc.clone();
        // ⚠️ 这两件事都是**阻塞**的（删 Windows 侧文件 + 进容器 `rm -f`），
        // 所以整段丢进后台执行器。`cx.spawn(…)` 的 future 是在**界面线程**上被
        // poll 的，直接在它里面调 `wslc.run(…)` 就是 `AGENTS.md` §7.1 那个形状
        //（点一下窗口变灰，直到子进程结束）。仓库里其余后台任务都是这个写法：
        // `cx.background_executor().spawn(async move { … }).await`。
        //
        // 重扫备份目录也放在同一个后台块里：`read_dir` + 每条 metadata 会卡
        // 一下，而这个块下面的 `update` 回调在界面线程上。
        //
        // ⚠️ `container` / `database` 后面在 `update` 里还要用（刷列表、拼提示），
        // 而内层那两个 `async move` 会把它们搬走 —— 所以这里备好两份 clone。
        let exec_container = container.clone();
        let target_container = container.clone();
        let target_database = database.clone();
        let scan_dir = self.state.container_backup_dir(&container);
        cx.spawn(async move |this, cx| {
            let (file_result, container_result, rescanned) = cx
                .background_executor()
                .spawn(async move {
                    // Windows 侧那份是**主体**，删不掉要报错；容器内那份尽力删
                    //（容器停了 / 文件没了都不影响这次删除的结果）
                    let file_result =
                        std::fs::remove_file(&windows_path).map_err(|e| e.to_string());
                    let container_result = wslc
                        .run(&["exec", &exec_container, "rm", "-f", &container_path])
                        .map(|_| ())
                        .map_err(|e| e.to_string());
                    let rescanned = scan_dir
                        .as_deref()
                        .and_then(|dir| state::scan_backup_dir(dir).ok());
                    (file_result, container_result, rescanned)
                })
                .await;

            let _ = this.update(cx, |shell, cx| {
                match &file_result {
                    Ok(()) => {
                        let _ = container_result;
                        if let Some(entries) = rescanned {
                            shell.state.apply_backups(&target_container, entries);
                        }
                        shell.state.notify(Toast::success(format!(
                            "已删除 {target_database} 的备份（容器内那份尽力清理）"
                        )));
                    }
                    Err(e) => shell
                        .state
                        .notify(Toast::error(format!("删不掉备份文件：{e}"))),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 把「备份中」浮层收起来（**不**取消备份）。
    ///
    /// 这一版**没有**接取消令牌：`backup_database` 里是两条**顺序**的短命令
    /// （dump 一般几十秒内自己结束），接令牌要把它改成 `spawn_to_file` +
    /// 轮询那条路 —— 等"边 dump 边传"一起做。所以这里**不**假装能打断，
    /// 只把遮罩收掉（它挡住整页）。
    ///
    /// 和[`Shell::dismiss_db_import`] 同一个理由；区别是这个浮层底下没有
    /// 互斥锁要维护（`db.backup` 照样是 `Some`，所以「立即备份」仍然是
    /// "备份中…"、不会重复起一份）。
    pub fn cancel_db_backup(&mut self, cx: &mut Context<Self>) {
        if let Some(db) = self.state.db.as_mut() {
            if let Some(backup) = db.backup.as_mut() {
                backup.dismissed = true;
            }
        }
        self.state.notify(Toast::info(
            "备份还在跑（这一版不能中途打断）—— 完成后会提示你，列表也会自动重读",
        ));
        cx.notify();
    }

    /// 在文件管理器里打开备份目录（「打开位置」）。
    ///
    /// 同步 `explorer.exe`：它不等待、不捕获输出 —— `AGENTS.md` §7.1 允许。
    pub fn reveal_backup_dir(&mut self, dir: Option<std::path::PathBuf>, cx: &mut Context<Self>) {
        let Some(dir) = dir.filter(|d| d.is_dir()) else {
            self.state
                .notify(Toast::error("备份目录不存在（可能已被清空）"));
            cx.notify();
            return;
        };
        let toast = match std::process::Command::new("explorer.exe").arg(&dir).spawn() {
            Ok(_) => Toast::info(format!("已打开 {}", dir.display())),
            Err(e) => Toast::error(format!("打不开文件管理器：{e}")),
        };
        self.state.notify(toast);
        cx.notify();
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
                // 页头「最后刷新」用：自动 / 手动刷新都算一次。
                shell.last_refresh = Some(Instant::now());
                // 没有错误了就收起横幅；否则上一次点开的「查看」会让下一次
                // 新错误**自动展开**，看起来像突然弹了一屏红字。
                if shell.state.snapshot.errors.is_empty() {
                    shell.errors_expanded = false;
                }

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
        // 部署页那一列也要自己刷新 —— 见 [`Shell::tick_deploy_status`]。
        self.tick_deploy_status(cx);
        if self.state.busy {
            return;
        }
        self.refresh_quiet(cx);
    }

    /// 「部署管理」页的状态列**慢档**轮询（这一页不参与快照那套刷新）。
    ///
    /// # 为什么需要它（v0.9.13 补）
    ///
    /// 那一列的状态是**问容器**得来的（`probe_script`：pidfile + `/proc` 判活、
    /// 数文件），而这套快照刷新（`refresh_quiet` → `load_snapshot`）**不碰它**。
    /// 于是：坐着不动的时候，应用自己崩了、或者在「终端」里被 `kill` 掉了，
    /// 界面上会**一直显示「运行中」** —— 而页面顶部那句说明恰恰写着
    /// "「现在跑没跑」是**问容器**得来的"。这就是"两个地方对同一件事有两种说法"。
    ///
    /// # 为什么比快照慢一档
    ///
    /// 这个轮询**每一条部署一次 `wslc exec`**（8 条 = 8 个子进程），
    /// 而快照那一套是几条命令拿全量。跟着 3 秒一轮太重，所以走
    /// [`Self::deploy_status_every_n_ticks`]（默认约 9 秒）。
    ///
    /// ⚠️ 重探**不清旧值**（[`Shell::refresh_deploy_status_quiet`]）：
    /// 否则每 9 秒整列都会闪一下「读取中…」—— 那比"数据晚 9 秒"更难受。
    fn tick_deploy_status(&mut self, cx: &mut Context<Self>) {
        // 只有站在这一页上才值得问：别的页面根本看不到这一列。
        if self.state.page != Page::Deploy {
            return;
        }
        // 一条部署动作正在跑 / 正在读 → 别插一脚（进度文案和这一列会互相踩）。
        // 先把三个旗标取出来，`self.state.deploy` 的借用就在这里结束 ——
        // 下面要 `&mut self`（借用检查器看得见这个顺序）。
        let (action_running, busy, loaded) = match self.state.deploy.as_ref() {
            Some(deploy) => (deploy.progress.is_some(), deploy.busy, deploy.loaded),
            None => return,
        };
        if action_running || busy || !loaded {
            return;
        }
        self.deploy_tick = self.deploy_tick.wrapping_add(1);
        if self.deploy_tick % Self::deploy_status_every_n_ticks() != 0 {
            return;
        }
        self.refresh_deploy_status_quiet(cx);
    }

    /// 部署页状态列隔几轮问一次（一轮 = `prefs.refresh_secs`，默认 3 秒）。
    fn deploy_status_every_n_ticks() -> u32 {
        3
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

        // 「删除备份」**分流**到它自己的执行函数。
        //
        // 它要做三件事（删本地文件 → 进容器 rm → 重扫目录写回 `AppState`），
        // 而 `PendingAction::execute` 的形状是"跑一条 `wslc` 命令 → 返回字符串"
        // —— 最后那件它表达不了（`&self`）。所以在这里把字段拆出来、交给
        // `delete_db_backup_confirmed`，别让它掉进下面那个通用分支。
        //
        // 确认框已经 `take()` 走了（上面），所以这里 return 不会留下残影。
        if let ConfirmAction::Container(PendingAction::DeleteDatabaseBackup {
            container,
            database,
            windows_path,
            container_path,
        }) = &action
        {
            let (container, database) = (container.clone(), database.clone());
            let (windows_path, container_path) = (windows_path.clone(), container_path.clone());
            self.delete_db_backup_confirmed(
                container,
                database,
                windows_path,
                container_path,
                cx,
            );
            return;
        }

        // 删库 / 删用户之后要重读数据库页那两张表 —— 容器的列表刷新管不到它们。
        // 这里先算好（`action` 马上要移进异步块）。
        // 删库还要**同时清掉本地存档里的账号**：库都没了，列表上不能再出现
        // 一个连不上的用户名 / 密码（就算哪天重建同名库，密码也对不上了）。
        let dropped_database = match &action {
            ConfirmAction::Container(PendingAction::DropDatabase {
                container,
                database,
                ..
            }) => Some((container.clone(), database.clone())),
            _ => None,
        };
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
                let ok = matches!(&result, Ok(_));
                match result {
                    Ok(message) => shell.state.notify(Toast::success(message)),
                    Err(e) => shell
                        .state
                        .notify(Toast::error(format!("{title}失败：{e}"))),
                }

                // 删库成功：本地存档里的账号一起清（见 `dropped_database` 的注释）
                if ok {
                    if let Some((container, database)) = dropped_database {
                        shell.state.db_credentials.remove(&container, &database);
                        if let Some(e) = shell.state.save_db_credentials() {
                            shell.state.notify(Toast::error(e));
                        }
                    }
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
    /// ⚠️ 「镜像」页**不需要** window：v0.6.3 把拉取表单挪进了弹窗，
    /// 它是在**点「拉取镜像…」那一刻**建的（`SelectState` 同理）。
    ///
    /// v0.9.22 起「容器」页也需要 window：列表上方的搜索框是 `InputState`，
    /// 只能在进入页面时建一次（见 [`Shell::ensure_container_search`]）。
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
            // 「SQL 控制台」（v0.12）：整页的工作区。它复用 `DbPageForm`
            // （编辑器那一格住在里面），外加树自己的搜索框。
            if page == Page::Sql {
                self.ensure_db_form(window, cx);
                self.ensure_schema_search(window, cx);
                // 树只在**还没读过**时才自动读一次。
                //
                // 每次进页面都读一遍会白花一次 `wslc exec`（约 70ms），
                // 而用户大部分时候只是在几条查询之间来回。要刷新表结构
                // 就点页面上的「刷新表结构」——那是明确的动作。
                let need_tree = self
                    .state
                    .db
                    .as_ref()
                    .is_some_and(|db| db.schema_tree.is_empty());
                if need_tree {
                    self.load_schema_tree(cx);
                } else {
                    // 数据已经有了，但树控件的 items 可能还没灌过（第一次进这一页，
                    // 或者上次是在"有搜索关键字"的状态下离开的）—— 补一次。
                    let query = self
                        .schema_search
                        .as_ref()
                        .map(|i| i.read(cx).value().to_string())
                        .unwrap_or_default();
                    self.rebuild_schema_tree(&query, cx);
                }
            }
            // 「容器」：搜索框是 `InputState`，只能在进入页面这一刻建。
            if page == Page::Containers {
                self.ensure_container_search(window, cx);
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
                                if let Some(form) = shell.install_form.as_mut() {
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
                        shell.wslconf_dialog = Some(WslConfDialog {
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
        // 换了来源，上一次的标红已经不对应新的输入了；下一次提交再重新算。
        form.show_errors = false;
        form.field_errors = InstallFieldErrors::default();
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
    pub fn confirm_install(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
            // 不弹提示条、也不在页面上列错误清单：把"哪几块有问题"记下来，
            // 让 `add_instance` 把对应的输入框标红；焦点送到第一块，
            // 长表单里用户不用自己找。
            let fields = check.field_errors();
            let focus = self.install_form.as_ref().and_then(|form| {
                if fields.name {
                    Some(form.name.clone())
                } else if fields.source {
                    match form.source {
                        InstallSourceKind::Local => Some(form.source_path.clone()),
                        // 在线清单没有可聚焦的输入框（下拉框），红框会画在
                        // 来源区块上；镜像源则把焦点送到"自定义地址"。
                        InstallSourceKind::Online => None,
                        InstallSourceKind::Mirror => Some(form.mirror_url.clone()),
                    }
                } else if fields.install_dir {
                    Some(form.install_dir.clone())
                } else {
                    None
                }
            });
            if let Some(form) = self.install_form.as_mut() {
                form.show_errors = true;
                form.field_errors = fields;
            }
            if let Some(input) = focus {
                let handle = input.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
            }
            tracing::info!(
                "安装参数未通过：{}",
                check.error_text().unwrap_or_else(|| "参数有误".to_owned())
            );
            cx.notify();
            return;
        }
        if let Some(form) = self.install_form.as_mut() {
            form.show_errors = false;
            form.field_errors = InstallFieldErrors::default();
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
            let input =
                cx.new(|cx| InputState::new(window, cx).placeholder("/home/wt/project 或 ~"));
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

    /// 让提示条**自己消失**的小循环（每 500ms 看一眼）。
    ///
    /// # 为什么是一个常驻循环，而不是"弹提示时起一个定时器"
    ///
    /// `AppState::notify` 住在 `wslc-panel-core` 里，那一层**没有执行器**、
    /// 起不了定时器。换成"在每个 notify 的调用点各起一个"的话：全仓库 60 多处
    /// 都得改，而且旧定时器还得有办法认出"现在这条提示已经不是我那条了"
    /// （提示条是单条的 `Option<Toast>`，后弹的会覆盖先弹的）。
    ///
    /// 一个循环只在一个地方，而且天然覆盖"以后新加的 notify"。
    ///
    /// # 代价
    ///
    /// 每 500ms 醒一次、读一个 `Option<Toast>`（外加一次 `Instant::elapsed`）。
    /// **只有真的收掉时才 `cx.notify()`** —— 否则界面会每半秒重绘一遍，
    /// 而 GPUI 每一帧都要跑布局 + 光栅化（`Cargo.toml` 里那段
    /// `profile.dev` 的注释说的就是这件事）。
    ///
    /// 500ms 这个粒度：提示条本来就活 4~8 秒，晚半秒收掉没人看得出来；
    /// 反过来做成一帧一查没有意义。
    fn start_toast_reaper(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(500))
                    .await;
                let alive = this.update(cx, |shell, cx| {
                    if shell.state.clear_expired_toast() {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    // 实体已销毁 → 退出循环，避免泄漏（和 `start_auto_refresh`
                    // 同一个写法）。
                    break;
                }
            }
        })
        .detach();
    }

    /// 打开「基础信息」浮层（数据库页的连接信息）。
    pub fn open_db_info(&mut self, cx: &mut Context<Self>) {
        self.db_info_open = true;
        cx.notify();
    }

    /// 关掉「基础信息」浮层。
    pub fn close_db_info(&mut self, cx: &mut Context<Self>) {
        self.db_info_open = false;
        cx.notify();
    }

    /// 从 SQL 控制台回到「数据库管理」页。
    ///
    /// `sql_database` 留着 —— 下次点同一行的「SQL」还是那个库，省得重选。
    pub fn close_db_console(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_page(Page::Database, window, cx);
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

        // 副标题回答「数据有多新」，不再放会话 / 构建号 —— 那两样是低频
        // 诊断信息，已经挪到「应用设置 → 关于」。
        let refreshed = match self.last_refresh {
            Some(at) => crate::util::refresh_age_label(at.elapsed().as_secs()),
            None => "还没刷新".to_owned(),
        };
        let subtitle = format!(
            "最后刷新：{refreshed} · 自动刷新 {} 秒",
            state.prefs.refresh_secs
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

        // 错误横幅：1 条直显；多条默认折叠成「第一条 + 还有 N 项」。
        //
        // 之前把**每个区段**的错误逐条铺出来，`wslc` 一时读不到时能红七行，
        // 把整页内容推下去 —— 用户第一眼看到的是七行报错，而不是"哪些数据还在"。
        let errors = &state.snapshot.errors;
        let error_banner: AnyElement = if errors.is_empty() {
            if !state.snapshot.has_data() {
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
            }
        } else {
            let count = errors.len();
            let expanded = self.errors_expanded;
            let toggle = |label: &'static str| {
                let entity = entity.clone();
                Button::new("errors-toggle")
                    .label(label)
                    .small()
                    .on_click(move |_, _, cx| {
                        entity.update(cx, |shell, cx| {
                            shell.errors_expanded = !shell.errors_expanded;
                            cx.notify();
                        });
                    })
            };

            let mut body = v_flex().w_full().gap_2();
            if count == 1 {
                body = body.child(
                    div()
                        .text_xs()
                        .text_color(theme::danger())
                        .child(errors[0].clone()),
                );
            } else if expanded {
                // 滚动容器必须先有 id（`overflow_y_scroll` 只对带 id 的元素可用）。
                body =
                    body.child(
                        v_flex()
                            .id("error-list")
                            .w_full()
                            .max_h(px(160.))
                            .overflow_y_scroll()
                            .gap_1()
                            .children(errors.iter().map(|e| {
                                div().text_xs().text_color(theme::danger()).child(e.clone())
                            })),
                    );
                body = body.child(h_flex().w_full().justify_end().child(toggle("收起")));
            } else {
                body = body.child(
                    h_flex()
                        .w_full()
                        .justify_between()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .min_w_0()
                                .text_xs()
                                .text_color(theme::danger())
                                .overflow_hidden()
                                .truncate()
                                .child(errors[0].clone()),
                        )
                        .child(
                            h_flex()
                                .flex_none()
                                .gap_2()
                                .items_center()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme::text_dim())
                                        .child(format!("还有 {} 项", count - 1)),
                                )
                                .child(toggle("查看")),
                        ),
                );
            }

            v_flex()
                .w_full()
                .p_2()
                .rounded_md()
                .bg(theme::danger_soft())
                .border_1()
                .border_color(theme::danger_edge())
                .child(body)
                .into_any_element()
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
            (true, Some(form)) => views::pull_dialog_overlay(form, state, &entity, cx),
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
        let wslconf_dialog: AnyElement = views::wslconf_overlay(self, &entity, cx);

        // 容器日志浮层（状态全在 `AppState::logs` 里，不需要 `cx`）。
        let logs_dialog: AnyElement = views::logs_overlay(state, &entity);

        // 容器 exec 浮层。要 `cx` 才能读命令输入框的值。
        let exec_dialog: AnyElement = views::exec_overlay(self, &entity, cx);

        // 数据库页的动作弹窗（新建库 / 新建用户 / 改密码 / 授权）。
        // 要 `cx` 才能读表单里那几格的值。
        let db_dialog: AnyElement = views::db_dialog_overlay(self, &entity, cx);

        // 「基础信息」（连接卡）—— 只有浮层开着、且确实在数据库页时才画。
        let db_info: AnyElement = match &state.db {
            Some(db) if self.db_info_open => match self.db_form.as_ref() {
                Some(form) => views::db_info_overlay(db, form, &entity, cx),
                None => div().into_any_element(),
            },
            _ => div().into_any_element(),
        };

        // ⚠️ v0.12 起 SQL 控制台是**整页**（`Page::Sql`），不再是浮层 ——
        // 所以这里没有它的位置了。浮层那一版（`db_console_overlay`）已删：
        // 它盖住页面、看着像弹窗，而且塞不下"树 + 编辑器 + 表格"三块。

        // 备份进度（跑着才有内容）与备份列表（点「备份」打开）。
        let db_backup_progress: AnyElement = match &state.db {
            Some(db) => views::db_backup_progress(db, &entity),
            None => div().into_any_element(),
        };
        let db_backup_dialog: AnyElement = match &state.db {
            Some(db) => match self.db_form.as_ref() {
                // `db_form` 只用来判断"页面进得来没有"（浮层自己一个输入框都不读）。
                Some(_) => {
                    views::db_backup_dialog_overlay(db, &state.db_credentials, self, &entity, cx)
                }
                None => div().into_any_element(),
            },
            None => div().into_any_element(),
        };
        // 导入进度（跑着才有内容）。
        let db_import_progress: AnyElement = match &state.db {
            Some(db) => views::db_import_progress(db, &entity),
            None => div().into_any_element(),
        };

        // 部署页（v0.9）的三个浮层：
        // - 「添加部署 / 重新部署」要 `cx`（读那五格输入框的值）；
        // - 日志与删除确认都是**纯数据**（没有输入框），所以只要 `&Shell`。
        let deploy_dialog: AnyElement = views::deploy_dialog_overlay(self, &entity, cx);
        let deploy_logs: AnyElement = match state.deploy.as_ref().and_then(|d| d.logs.as_ref()) {
            Some(logs) => views::deploy_logs_overlay(logs, &entity),
            None => div().into_any_element(),
        };
        // 「站点配置」也是**纯数据**（只读），和日志并列。
        let deploy_site_conf: AnyElement =
            match state.deploy.as_ref().and_then(|d| d.site_conf.as_ref()) {
                Some(conf) => views::deploy_site_conf_overlay(conf, &entity),
                None => div().into_any_element(),
            };
        let deploy_confirm: AnyElement = match state
            .deploy
            .as_ref()
            .and_then(|d| d.confirm_remove.as_ref())
        {
            Some(app) => views::deploy_remove_confirm_overlay(app, state.deploy.as_ref(), &entity),
            None => div().into_any_element(),
        };

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
        // ⚠️ 「SQL 控制台」那一页要**撑满高度**（它自己分上下两块、各自滚动），
        // 其余页面是"内容多高就多高，外层滚动"。所以这两支不能合并：
        // 给普通页面加 `h_full` 会让短页面被拉满、长页面被压扁。
        let page_body: AnyElement = if state.page == Page::Sql {
            v_flex()
                .w_full()
                .h_full()
                .min_h_0()
                .child(
                    div()
                        .w_full()
                        .h_full()
                        .min_h_0()
                        .child(views::page(self, cx, &entity))
                        .with_animation(
                            state.page.label(),
                            Animation::new(Duration::from_millis(120)),
                            |el, delta| el.opacity(delta),
                        ),
                )
                .into_any_element()
        } else {
            v_flex()
                .w_full()
                .child(
                    div()
                        .w_full()
                        .child(views::page(self, cx, &entity))
                        .with_animation(
                            state.page.label(),
                            Animation::new(Duration::from_millis(120)),
                            |el, delta| el.opacity(delta),
                        ),
                )
                .into_any_element()
        };

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
                                // ⚠️ 「SQL 控制台」是**整页工作区**，外层不能给它
                                // `p_5` 和 `overflow_y_scroll`：
                                //
                                // - `p_5` 会把左侧那棵树从内容区左边框推开 20px，
                                //   而用户明确要"左边的列表树左对齐"；
                                // - 外层滚动 + 内层滚动（树自己滚、表格自己虚拟滚）
                                //   会互相打架，而且 `DataTable` 拿不到确定高度时
                                //   算错可见行范围（表现为滚动条乱跳 / 底部空白）。
                                //
                                // 所以这一页自己管布局和滚动，外层只做容器。
                                //
                                // `overflow_y_scroll` 来自 `StatefulInteractiveElement`，
                                // 只对**带 id 的**元素可用（这正是它叫 "stateful" 的原因）。
                                // 不带 `.id()` 会报 "no method named overflow_y_scroll"。
                                if state.page == Page::Sql {
                                    v_flex()
                                        .id("app-body-sql")
                                        .w_full()
                                        .flex_1()
                                        .min_h_0()
                                        .overflow_hidden()
                                        .child(error_banner)
                                        .child(page_body)
                                        .into_any_element()
                                } else {
                                    v_flex()
                                        .id("app-body-scroll")
                                        .w_full()
                                        .flex_1()
                                        .min_h_0()
                                        .gap_4()
                                        .p_5()
                                        .overflow_y_scroll()
                                        .child(error_banner)
                                        .child(page_body)
                                        .into_any_element()
                                },
                            )
                            // 提示条放在主内容列**最后**：它是一条贴底的布局项，
                            // 出现时把上面那个 `flex_1` 的滚动区挤矮一点，
                            // 而不是盖在它上面（见 `toast` 的说明）。
                            .child(toast),
                    ),
            )
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
            // 部署页的三个浮层：它们只在「部署管理」页上出现，
            // 不需要盖住数据库 / 备份那几层（那两组不会同时开着），
            // 所以插在数据库弹窗**之前**。
            .child(deploy_dialog)
            .child(deploy_logs)
            .child(deploy_site_conf)
            // 删除确认排在日志之后：它是最后点的那个动作，应该在最上面
            //（日志浮层与它可能同时开着 —— 用户看着日志觉得该删了）。
            .child(deploy_confirm)
            // 数据库页的动作弹窗排在最后：它可能盖在任一页（含容器详情）上。
            .child(db_dialog)
            // 「基础信息」排在动作弹窗**之后**（同时开着时它在上面 —— 但两者
            // 互斥：动作弹窗要按底下的按钮才会开，而浮层挡着那些按钮）。
            .child(db_info)
            // ⚠️ v0.12：SQL 控制台从浮层改成了**整页**（`Page::Sql`），
            // 所以这条链子上没有它的位置了。
            //
            // 留一条**踩过的坑**记在这里，因为这个坑很容易再踩：
            // v0.9.2 把它做成浮层时，只写了 `let db_console = …` 却没把它
            // `.child(…)` 进这棵树 —— 算出来的 `AnyElement` 当场被丢掉，
            // 于是点行上的「SQL」只有一条 toast、**界面永远不出现**
            //（实机原话："这个 sql 按钮点击也没弹出控制台输入 sql 的界面来"）。
            //
            // 教训：**这棵树里每算一个 `AnyElement`，都必须确认它真的被挂上去了。**
            // 漏挂不会编译失败、也不会运行时报错，只是"点了没反应"。
            // 备份进度要盖住它打开时所处的一切；备份列表在进度之后
            //（进度跑完自动关，那时列表还在）。
            .child(db_backup_progress)
            .child(db_import_progress)
            .child(db_backup_dialog)
            // ⚠️ **通用二次确认必须挂在最后一个。**
            //
            // 层序就是这个 `.child()` 链的先后：**后画的压在上面**（同一个文件里
            // 上面那几段注释都是按这条写的）。而 `confirm` 是"最后点的那个动作"
            // 的确认框，它可以从**任一**浮层里点出来：
            //
            // - 发行版详情 → 「压缩磁盘」「删除」
            // - 容器详情   → 「强杀」
            // - 备份列表   → 「恢复」
            //
            //（⚠️ 备份列表里的「删除」**不**走这里 —— 它直接删、没有确认框，
            // 见 `Shell::delete_db_backup`。别照这句话去推断它。）
            //
            // v0.9.14 及以前它挂在整条链子的**最前面**（`pull_dialog` 之前），
            // 于是这些确认框全被自己下面那层浮层盖住了 —— 实机报过来的就是这一条：
            // "实例详情这里点击压缩磁盘之后没有隐藏详情，压缩磁盘的对话确认在详情之下"
            //（确认框连着它的遮罩一起被盖住：既看不到「确认」，也挡不住背后的按钮）。
            //
            // ⚠️ 加新浮层时问一句：**它能不能从另一个浮层里被打开？** 能的话，
            // 就必须排在那个浮层之后。`views::modal_scrim` 那套遮罩只保证"点不到
            // 背后"，管不了层序 —— 层序全靠这条链子。见 `AGENTS.md` §7.5。
            .child(confirm)
    }
}

/// 左侧导航项。
fn nav_item(page: Page, current: Page, entity: &Entity<Shell>) -> AnyElement {
    let entity = entity.clone();
    // 子页面（添加实例 / 数据库 / 部署）也高亮它所属的父项 ——
    // 否则进这三页时侧边栏一个都不选中，用户会失去"我在哪"。
    let active = current.highlights_nav(page);

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
/// ⚠️ 这个 match 覆盖的是**全部 `Page` 变体**（`Page` 没有 `#[non_exhaustive]`，
/// 少一支就是编译错误），而侧边栏只画 `Page::ALL` 里那 8 页。
/// 另外四页 —— `AddInstance` / `Database` / `Deploy`（v0.9）/ `Sql`（v0.12）
/// —— **不是导航项**
/// （只能从别的页面上的按钮进），它们那三支给的是不会被用到的 id。
///
/// ⚠️ 这段注释早先写的是"这个 match 只覆盖 `Page::ALL` 里那 8 页"，
/// 从 `AddInstance` 起就不准了 —— 照着一句过期的注释去判断，会得出
/// "这些页面在侧边栏里"的错误结论。
// ---------------------------------------------------------------------------
// 库 / 表树：节点 id 的编码与解码
// ---------------------------------------------------------------------------

/// `TreeItem` 的 id 分隔符。
///
/// # 为什么不是 `.` 或 `::`
///
/// MySQL 的标识符几乎什么字符都能含（反引号里连空格和点都可以），
/// 所以拿 `.` 拼 `库.表` 再拆开会**拆错**：库名 `a.b` + 表名 `c` 和
/// 库名 `a` + 表名 `b.c` 拼出来一样。
///
/// `U+0001`（Start of Heading）是控制字符，不可能出现在标识符里 ——
/// 用它当分隔符，拆解就是无歧义的。
const TREE_ID_SEP: char = '\u{1}';

/// 表节点（叶子）的 id：`t\u{1}库\u{1}表`。
fn leaf_id(database: &str, table: &str) -> String {
    format!("t{TREE_ID_SEP}{database}{TREE_ID_SEP}{table}")
}

/// 库节点（父节点）的 id：`d\u{1}库`。
///
/// 只需要唯一 —— 点库节点时组件自己展开/收起，我们不解析它。
fn folder_id(database: &str) -> String {
    format!("d{TREE_ID_SEP}{database}")
}

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
        // 同上（v0.9 的部署管理页）
        Page::Deploy => "nav-deploy",
        // 同上（v0.12 的 SQL 控制台整页）。
        //
        // 它在侧边栏里**不出现**，但高亮要落在「数据库管理」上 ——
        // 那不是靠这个 id，而是 `Page::nav_parent()`（见 `highlights_nav`）。
        Page::Sql => "nav-sql",
        Page::AppSettings => "nav-app-settings",
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

    views::modal_scrim(
        "confirm",
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
