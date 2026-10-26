//! 挂在 `Shell` 上的**纯数据**：各表单、弹窗判据、中间结果。
//!
//! 它们都不碰 GPUI 的实体，只是被 `Shell` 的字段装着、被 `impl Shell` 里的
//! 方法读写。放在这里是为了让 `core.rs` 不必先翻过二十几个结构体才看到方法。

use super::super::prelude::*;

// `dialog_stack` 的类型定义在 `dialogs.rs`（对话框栈的那套判据都在那儿）。
use super::dialogs::DialogKey;

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
    /// （当初就是因为这个能力才留了两个入口）。
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
        mirrors: &wslc_panel_core::state::MirrorState,
        online: &wslc_panel_core::state::OnlineDistroState,
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
pub(crate) struct KeepAlive {
    pub(crate) name: String,
    pub(crate) child: std::process::Child,
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
    pub(crate) _input_subs: Vec<Subscription>,
}

impl KeepAlive {
    /// 哨兵还活着吗？
    ///
    /// `try_wait` 会**顺手回收**已经退出的子进程 —— 不调它的话，
    /// 退掉的子进程会以僵尸状态一直挂在系统里。
    pub(crate) fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

/// 「创建容器」弹窗的全部输入框。
///
/// 每个字段一个独立的 `InputState` —— 这是 GPUI 的标准做法。
/// 拉取策略用预设按钮（不是输入框），所以这里只存一个枚举值。
///
/// `pub(crate)` + 公开字段：渲染在 `views/` 里，需要逐个读出来画。
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
    /// 环境变量：**多行**（官方 `Textarea`）。值里可以有逗号，
    /// 一行一个是最不容易切错的写法。
    pub(crate) env: Entity<TextareaState>,
    /// 卷挂载：多行（一行一个）。
    pub(crate) volumes: Entity<TextareaState>,
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
    /// `pub(crate)`：`views/` 要用它来做**实时**等效命令预览。
    ///
    /// `existing_names` 是**现有容器的名字**（`AppState::container_names()`）：
    /// 容器名留空时按镜像推一个（见 [`wslc_core::model::suggest_container_name`]），
    /// 撞名要能看出来，所以得知道已经有哪些名字。
    pub(crate) fn to_spec(&self, existing_names: &[String], cx: &App) -> RunSpec {
        let text = |input: &Entity<InputState>| input.read(cx).value().trim().to_owned();
        let textarea = |input: &Entity<TextareaState>| input.read(cx).value().trim().to_owned();

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
        spec.volumes = split_list(&textarea(&self.volumes));

        // 环境变量按 `KEY=VALUE` 解析；没有等号的**直接丢掉**，
        // 不去猜用户想表达什么（猜错了反而更难查）。
        //
        // ⚠️ 用 `split_env_list` 而不是 `split_list`：值里的逗号是合法的
        // （`SPRING_PROFILES_ACTIVE=dev,local`），按逗号一刀切会把后半段
        // 变成没有 `=` 的段然后静默丢掉。见 `util::split_env_list`。
        spec.env = split_env_list(&textarea(&self.env))
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
pub(crate) struct InstallRun {
    /// 后台线程塞进来的事件（界面每轮取走）。
    pub(crate) events: Arc<Mutex<Vec<InstallEvent>>>,
    /// 执行结果；`None` = 还在跑。
    pub(crate) done: Arc<Mutex<Option<InstallSummary>>>,
    /// 整个安装是什么时候开始的（界面上显示"已用时"）。
    pub(crate) started: Instant,
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
    /// 连接串（v0.16，可选）。
    ///
    /// 填了就走**原生连接**（`wslc-db`），能连远程库；留空则回到原来的
    /// "在容器里 exec 客户端"那条路。
    ///
    /// ⚠️ 里面的口令**不落盘**：这个输入框只在内存里，`DbState.dsn` 也是。
    pub(crate) dsn: Entity<InputState>,
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
    ///
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
    /// 更多反代（`/auth/ => http://auth:9000; ...`，一行一条）。
    pub(crate) proxy_more: Entity<TextareaState>,
    /// 上游是另一个容器时，自动把两个容器接到同一个用户网络。
    pub(crate) auto_connect: bool,
    // 让折叠摘要随输入更新；表单关闭时自动退订。
    pub(crate) _input_subs: Vec<Subscription>,
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
    pub(crate) _input_subs: Vec<Subscription>,
}

/// 选到路径之后，除了填进输入框还要不要额外做一件事。
///
/// 单独一个类型而不是 `bool`：两种意图完全不同，`true` / `false`
/// 在调用处读不出来是哪个。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AfterPick {
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
pub(crate) enum SaveDecision {
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
pub(crate) enum DeployField {
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
pub(crate) struct DeployFormValues {
    /// 应用名。
    pub(crate) app: String,
    /// 本地来源路径。
    pub(crate) source: String,
    /// 容器内的目标目录。
    pub(crate) target: String,
    /// 启动命令。
    pub(crate) command: String,
    /// 端口。空 / 解析不出来时是 `None` —— 调用方**必须**报错：
    /// 多应用下静默回落到运行时默认端口会撞（v0.9.19 改）。
    pub(crate) port: Option<u16>,
    /// 是否生成 nginx 站点配置（只有 nginx 有意义）。
    pub(crate) site_enabled: bool,
    /// 单页应用回退。
    pub(crate) site_spa: bool,
    /// `server_name`。
    pub(crate) server_name: String,
    /// 反代路径前缀。
    pub(crate) proxy_prefix: String,
    /// 反代上游。
    pub(crate) proxy_upstream: String,
    /// 反代去掉前缀。
    pub(crate) proxy_strip: bool,
    /// 更多反代（`;` 分隔）。
    pub(crate) proxy_more: String,
    /// 自动接通两个容器的网络。
    pub(crate) auto_connect: bool,
}

/// 一次部署动作要用的素材（从当前页面状态 + 存档里取出来的一份快照）。
///
/// 单独一个结构体是因为这几个字段要在 `spawn` 的异步闭包里用 ——
/// 闭包不能借 `self`，所以必须**先取出来**。取的时候顺手把"这一条还有没有
/// 意义"判掉（找不到了就返回 `None`，界面上那些按钮也不会画）。
pub(crate) struct DeployAction {
    /// 目标容器名。
    pub(crate) container: String,
    /// 容器内的目标目录。
    pub(crate) target: String,
    /// 应用端口（站点配置重写时 `listen` 要用）。
    pub(crate) port: u16,
    /// 启动命令。
    pub(crate) command: String,
    /// 是不是进程形态（决定要不要先停 / 能不能启动）。
    pub(crate) is_process: bool,
    /// 上一次真的铺进去的名字 —— 「删除」只删这些（外加 `app.log` / `app.pid`），
    /// **不整目录清空**（见 `model::deploy::removals_script`）。
    /// 老存档里没有这一项时是空表：那就只删日志 / pidfile。
    pub(crate) members: Vec<String>,
    /// 站点配置（删除部署时要连 conf 一起删）。
    pub(crate) site: Option<DeploySite>,
}

/// 「写 / 删站点配置」这一步的结果。
///
/// 和 [`DeployAction`] 一样是**先取出来**的快照：后台闭包不能借 `self`。
pub(crate) struct SiteStep {
    /// 应该写进存档的 site（失败时是**上一次**的，不能把没生效的写进去）。
    pub(crate) site: Option<DeploySite>,
    /// 非致命警告（reload 失败 / 有噪音）。
    pub(crate) warning: Option<String>,
    /// 成功时附带的说明（自动接通了哪个网络）。
    pub(crate) note: Option<String>,
    /// 致命错误（`nginx -t` 没过、conf 删不掉）；`Some` 时 `site` 保留旧值。
    pub(crate) error: Option<String>,
}

/// 一次「在后台执行一条 DDL、成功后重读列表」的请求。
///
/// # 为什么打成结构体
///
/// `run_db_statement` 原先有 9 个参数（含 `&mut self` / `&mut Window` /
/// `&mut Context`），clippy 的 `too_many_arguments` 按 7 个算就超了。
/// `window` / `cx` 是 GPUI 的既定形状（不能并进业务字段），所以把**业务字段**
/// 收成一个结构体：签名降到 4 个，而调用点从
///
/// ```text
/// run_db_statement(container, bin, creds, sql, title, to_record, window, cx)
/// ```
///
/// 变成 `run_db_statement(DbStatement { container, bin, … }, window, cx)` ——
/// **每个值叫什么名字在调用点就能看见**（原来那 6 个同类型的 `String`
/// 参数顺序错了编译器也不报）。
pub(crate) struct DbStatement {
    /// 目标容器。
    pub(crate) container: String,
    /// 容器里的客户端二进制（探测得来的）。
    pub(crate) bin: String,
    /// 执行用的账号。
    pub(crate) creds: MysqlCreds,
    /// 拼好的 DDL / 多段 SQL。
    pub(crate) sql: String,
    /// 提示条里的动词（"新建数据库" / "改密码"……）。
    pub(crate) title: &'static str,
    /// 「建库同时建用户」的账号 —— SQL **成功之后**才写进本地存档
    ///（列表里展示的就是它）。放在执行**前**传进来，是因为弹窗已经关了，
    /// 输入框的值拿不到了。
    pub(crate) to_record: Option<(String, String, String)>,
}

/// 长任务与"正在进行的动作"的句柄。
///
/// 全是**能力**而不是界面状态：能 kill 一个子进程、能阻止重复弹窗、能判断
/// 我们吊着的发行版还在不在。对应的纯数据（拉了哪个镜像、走到第几步、日志）
/// 在 `AppState` 里 —— 因为 `views/` 只拿得到 `&AppState`。
///
/// 字段 `pub(super)`：定义模块是 `app::shell::state`，父模块是 `app::shell`，
/// 而各域模块（`db/` / `deploy/` …）都是它的后代 —— 于是它们读写得到，
/// `views/` 看不见。
pub(crate) struct Tasks {
    /// 正在进行的拉取任务的取消令牌。
    ///
    /// 放 `Shell`：这是**能力**（能 kill 子进程），不是"状态"。
    /// 纯数据部分（拉了哪个镜像、输出了什么）在 `AppState::pulling` 里，
    /// 因为 `views/` 只拿得到 `&AppState`。
    pub(super) pull_cancel: Option<wslc_core::CancelToken>,

    /// 正在进行的**导出**任务的取消令牌。
    ///
    /// 和 `pull_cancel` 并列：两者都是长任务，都能取消，但一个走 `wslc.exe`、
    /// 一个走 `wsl.exe`，句柄类型也不同，合并只会让这个字段变成 `enum`。
    pub(super) export_cancel: Option<wslc_core::CancelToken>,

    /// 正在跑的安装（事件队列 + 结果）；空闲时为 `None`。
    ///
    /// 纯数据部分（走到第几步、日志、进度）在 `AppState::installing` 里 ——
    /// `views/` 只拿得到 `&AppState`。
    pub(super) install_run: Option<InstallRun>,

    /// 正在跑的安装的取消令牌。
    ///
    /// 和 `export_cancel` 并列：都是长任务、都能取消，但一个走 `wsl.exe`
    /// （外加 `curl.exe`），另一个只有 `wsl --export`。
    pub(super) install_cancel: Option<InstallCancel>,

    /// 本面板正在吊着的发行版（见 [`KeepAlive`]）。
    ///
    /// 界面只看到 `AppState::kept_alive` 里的名字；真正的进程句柄留在这里 ——
    /// 那是"能力"，不是界面状态。
    pub(super) keep_alive: Vec<KeepAlive>,

    /// 是否正在等一个文件 / 目录选择器的结果。
    ///
    /// 用来**防止重复弹窗**：选择器没有超时（用户不点完它就一直开着），
    /// 连点两下「浏览…」会堆出两个对话框。见 `cmd::picker` 的说明。
    pub(super) picking: bool,
}

/// 刷新节奏与按钮状态。
///
/// 字段 pub(super)，理由同 [Tasks]：域模块是 pp::shell 的后代，iews/ 不是。
pub(crate) struct Refresh {
    /// 最近一次采集完成的时间（自动刷新与手动刷新都算）。
    ///
    /// 只用于页头那句「最后刷新：N 秒前」—— 它是**界面状态**，不进快照：
    /// 快照每轮整体重建，放进去反而要每轮重新赋值。
    pub(super) last_refresh: Option<Instant>,

    /// 采集期间又有刷新请求进来了吗（本轮跑完要立刻补一次）。
    ///
    /// 丢不得：操作（启动 / 停止 / 创建 / 删除）完成后都会补刷，撞上 3 秒一次的
    /// 自动刷新时若把这次请求丢掉，界面要等到下一个周期才变 ——
    /// 用户看到的就是"点了停止，状态还是运行中"。
    pub(super) refresh_again: bool,

    /// 当前这轮采集是否**由用户发起**（点按钮 / 操作完成后补刷）。
    ///
    /// 决定顶部按钮要不要显示「刷新中…」。3 秒一次的自动刷新是**后台**行为：
    /// 它每轮都会把 `state.busy` 置起来（并发守卫要用），但按钮不该跟着闪 ——
    /// 否则按钮几乎永远停在「刷新中…」上（用户看到的"按钮一直是刷新中"）。
    pub(super) refresh_visible: bool,

    /// 已经跑过多少轮刷新。
    ///
    /// 用来把 `wsl --status` 降到 30 秒一次（见 [`Shell::status_every_n_ticks`]）。
    /// 放在 `Shell` 而不是 `AppState`：这是**调度细节**，不是界面要展示的状态。
    pub(super) status_tick: u32,

    /// 已经跑过多少轮刷新（给**部署页状态**用的那一份计数）。
    ///
    /// 和 `status_tick` 分开：部署页那一列要问的是"每一条部署现在跑没跑"，
    /// 一条一次 `exec` —— 跟着 3 秒一次太快（见 [`Shell::deploy_status_every_n_ticks`]）。
    pub(super) deploy_tick: u32,
}

/// 全部**输入框 / 表单**（12 个），按"用户在哪里填"排列。
///
/// # 为什么收成一组
///
/// 这 12 个字段的类型各不相同（`CreateDialog` / `InstallForm` / `ImageForm` /
/// `DbPageForm` / `DbDialog` / `DeployForm` / `WslConfDialog` / `TextPrompt`，
/// 外加三个裸 `Entity<InputState>`），但**处境完全一样**：
///
/// - 里面都有 `InputState`，而 `InputState::new` 要 `&mut Window` ——
///   所以只能**懒创建**，建好之后一直留着（渲染时重建会让用户一个字都打不进去，
///   见 `AGENTS.md` §7.5）；
/// - 关闭时置 `None`。页面级的几个是例外：进过那一页之后一直 `Some`。
///
/// 平铺在 `Shell` 顶层时，这 12 个和"订阅句柄""取消令牌""刷新节奏"混在一起，
/// 想看"这个动作碰了哪些状态"得读完整页。收进 `forms` 之后，`self.forms.xxx`
/// 一眼就是"在改某个输入框"。
///
/// # ⚠️ 这里的字段是 `pub(crate)`，和 [`Tasks`] / [`Refresh`] 的 `pub(super)` 不同
///
/// 那两组是**能力**（能 kill 子进程、能阻止重复弹窗），`views/` 不该看见。
/// 这一组反过来：**必须**让 `views/` 读得到 —— 画一个输入框要
/// `Input::new(&Entity<InputState>)`，而页面渲染只拿得到 `&Shell`。
///
/// 放 `pub(super)` 是不行的：本结构体定义在 `app::shell::state`，
/// `pub(super)` 只到 `app::shell` 那一层，`views` 在它外面。
pub(crate) struct Forms {
    /// 「容器」页的搜索框；进过那一页之后一直是 `Some`。
    ///
    /// 见 [`Shell::ensure_container_search`]。
    pub(crate) container_search: Option<Entity<InputState>>,

    /// 「发行版配置（`/etc/wsl.conf`）」弹窗的输入框；关闭时为 `None`。
    pub(crate) wslconf_dialog: Option<WslConfDialog>,

    /// 「新终端的起始目录」那个自定义路径输入框。
    ///
    /// 点过设置页的「自定义路径…」之后才是 `Some`，建完就**一直留着**
    /// （和 `install_form` 同一个理由：`InputState::new` 要 `&mut Window`，
    /// 而渲染时每帧重建输入框会让用户**一个字都打不进去**）。
    pub(crate) terminal_dir_input: Option<Entity<InputState>>,

    /// 「创建容器」弹窗；关闭时为 `None`。
    pub(crate) create_dialog: Option<CreateDialog>,

    /// 「容器 exec」浮层的命令输入框；浮层关闭时为 `None`。
    ///
    /// 和 `create_dialog` 一样懒创建（`InputState::new` 要 `&mut Window`）。
    /// 纯数据部分（跑过什么、输出是什么）在 `AppState::exec` 里。
    pub(crate) exec_input: Option<Entity<InputState>>,

    /// 「添加实例」页的表单。
    ///
    /// 和 `create_dialog` 一样**懒创建**，但**不随页面离开而销毁** ——
    /// 用户很可能点错了又点回来，重建会把已经打进去的字清掉（见 [`InstallForm`]）。
    pub(crate) install_form: Option<InstallForm>,

    /// 「镜像」页的表单（registry / 仓库名两个输入框 + tag 下拉框）；
    /// 进过那一页之后一直是 `Some`。
    pub(crate) image_form: Option<ImageForm>,

    /// 单输入框提示弹窗（移动位置 / 调整大小 / 设置默认用户）；关闭时为 `None`。
    pub(crate) prompt: Option<TextPrompt>,

    /// 「数据库管理」页的连接卡 / SQL 输入框；没进过那一页时为 `None`。
    pub(crate) db_form: Option<DbPageForm>,

    /// 库 / 表树的搜索框（`InputState::new` 要 `&mut Window`，所以懒创建）。
    pub(crate) schema_search: Option<Entity<InputState>>,

    /// 「数据库管理」页的一次性动作弹窗；关闭时为 `None`。
    ///
    /// 和 `create_dialog` 一样懒创建（`InputState::new` 要 `&mut Window`）。
    pub(crate) db_dialog: Option<DbDialog>,

    /// 「部署管理」页的「添加部署 / 重新部署」弹窗；关闭时为 `None`。
    ///
    /// 只有这一个要 window（里面全是输入框）。这一页另外两个浮层
    /// （**日志**、**删除确认**）都是纯数据，住在 `AppState::deploy` 里 ——
    /// 它们只有按钮和文字，没有 `InputState`。
    pub(crate) deploy_form: Option<DeployForm>,
}

/// 输入框 / 下拉框的**订阅句柄**（5 个）。
///
/// ⚠️ 这五个必须**一直存着**：`Subscription` 一 drop 就退订，而退订**不会报错** ——
/// 表现只是"打字时列表不跟着变""在下拉框里选了一个 tag，什么都不发生"。
/// 这是本仓库踩过的坑（v0.6 的 tag 过滤框），所以每个字段的注释各记了一遍。
///
/// 收成一组是因为它们**是同一个坑的五次踩**：读的人只要记住一条规则
/// （订阅句柄不许丢），不必逐条去发现四个例外；写新代码时看一眼这个结构体
/// 就知道"要在 `Shell` 上持有订阅"。
///
/// 字段 `pub(super)`，理由同 [`Tasks`]：这是**生命周期管理**，不是界面状态，
/// `views/` 不该看见（也确实一处都没读）。
pub(crate) struct Subscriptions {
    /// 「容器」搜索框内容变化的订阅。
    pub(super) container_search_sub: Option<Subscription>,

    /// 两个下拉框（商店清单 / 镜像源清单）的订阅。
    ///
    /// `Context::subscribe_in` 返回的 `Subscription` **一 drop 就退订**
    /// （组件库自己也是 `vec![cx.subscribe_in(..)]` 存起来的）。
    pub(super) select_subs: Vec<Subscription>,

    /// 「镜像」页 tag 下拉框的订阅。
    ///
    /// 和 `select_subs` 同一个坑：表现是"在下拉框里选了一个 tag，什么都不发生"。
    pub(super) image_subs: Vec<Subscription>,

    /// 「创建容器」弹窗里那个「从本地镜像里选」下拉框的订阅。
    ///
    /// 它跟着弹窗的**创建**走（每次开弹窗重建一份），所以这里是 `Vec` 而不是
    /// `Option` —— 弹窗关掉时订阅也跟着丢，正好。
    pub(super) create_subs: Vec<Subscription>,

    /// 库 / 表树搜索框内容变化的订阅。
    ///
    /// 和 `container_search_sub` 同一个坑，只是它影响的是树。
    pub(super) schema_search_sub: Option<Subscription>,
}

/// SQL 控制台这一页的**界面状态**（6 个）：结果表格、结果 tab、原始输出、左侧表名树。
///
/// # 为什么在 `Shell` 而不是 `AppState`
///
/// 里面有两个 GPUI 实体（`TableState` / `TreeState`），而 `wslc-panel-core` 不依赖
/// GPUI —— 装不下它们。所以纯数据（`DbState::sql_results` / `DbState::schema_tree`）
/// 在 `AppState`，**实体与页内开关**在这里；和 [`Forms`] 里那几个输入框同一个理由。
///
/// # 什么时候重建
///
/// `sql_table` 每次跑完 SQL 就**整个换掉**（结果集变了，列定义、行、排序全要重来），
/// 缓存着旧的不如重建 —— 代价只是按列名估一次宽度（前 100 行）。
/// `schema_tree_state` 则**只在真的变了**时才重建：重建会丢掉用户已经展开的层级。
///
/// # 可见性
///
/// 除 `schema_tree_built_for` 之外都是 `pub(crate)`：这一页在 `views/sql.rs` 里渲染，
/// 那几个开关和实体它都要读。`schema_tree_built_for` 只有 `app` 自己算"要不要重建"
/// 时用，所以是 `pub(super)`。
pub(crate) struct SqlUi {
    /// SQL 结果表格的 `TableState`（v0.11）。
    pub(crate) sql_table:
        Option<Entity<gpui_kit::component::table::TableState<crate::sql_table::SqlResultDelegate>>>,

    /// 当前在看的**第几个**结果（多语句时按 tab 分开）。
    pub(crate) sql_result_tab: usize,

    /// 「看原始输出」是不是展开着。
    ///
    /// 原始输出（`mysql --table` 的 ASCII 表格）不常看 —— 只在排查列宽异常 /
    /// 字符集问题时才有用，所以默认收起。
    pub(crate) sql_raw_open: bool,

    /// SQL 页左侧**表名树**收起来了没有（v0.12.2）。
    ///
    /// 和 `sidebar_collapsed`（应用级导航）分开记：一个是全局面板，一个是这一页的
    /// 工作区侧栏，两者该各自开关 —— 看代码时不想看到导航栏，不代表也不想看表结构。
    pub(crate) tree_collapsed: bool,

    /// 库 / 表树的**树控件状态**（v0.12.1）。
    ///
    /// # 为什么换成组件的 `Tree`
    ///
    /// v0.12 里这棵树是**自己用 `Button` 一行行画的**，实机一看就丑：`Button` 会把
    /// 文字**居中**、给每一行套一个整行宽度的底色和内边距，于是 110 行看起来像
    /// 110 个色块，完全不像树（缩进也读不出来）。
    ///
    /// `gpui-kit` 自带 `component::tree::Tree`（虚拟滚动 + 逐级展开 + 选中态 +
    /// 右键菜单），官方 `examples/editor` 就是用它做文件树的。它自己管展开/折叠
    /// 和选中，我们只管把 `TreeItem` 喂进去。
    pub(crate) schema_tree_state: Option<Entity<gpui_kit::component::tree::TreeState>>,

    /// 树是**按哪次查询**建出来的（用于判断要不要重建）。
    ///
    /// 存 `(搜索关键字, 表总数)`：两者都没变就不重建 —— `TreeState` 重建会丢掉
    /// 用户已经展开的层级，所以这件事必须**只在真的变了**时做。
    pub(super) schema_tree_built_for: Option<(String, usize)>,
}

/// 「开着哪几个浮层 / 在看谁的详情」这五个字段。
///
/// # 为什么收成一组
///
/// 它们回答的是同一个问题：**此刻哪些浮层是开着的**。`dialog_stack` 记
/// "已经压进官方 `Dialog` 栈的那几层"（下面那段长注释），其余字段是各自的
/// 开 / 关判据 —— `DialogKey::is_open` 读的正是这些字段，外加 [`Forms`] /
/// `AppState` 里的 `is_some()`（`Exec` / `Prompt` 也已经在栈里了）。
///
/// 平铺的代价是：想知道"点这个按钮之后会弹什么"，得在几十行字段里找；
/// 而这几处**必须一起改**（加一个浮层，栈记录和它的开合判据要同时加）。
///
/// 字段 `pub(super)`：这些判据只有 `app/shell` 自己读，`views/` 拿不到也不需要
/// （它读的是已经算好的内容）。
pub(crate) struct DialogUi {
    /// 我们已经压进**窗口级对话框栈**的那几层（官方 `Dialog`），按打开顺序。
    ///
    /// # 为什么需要自己记一份
    ///
    /// 官方 `ActiveDialog` **没有 id 字段**（`root.rs` 里只有 focus_handle /
    /// builder 等），所以**没法"按 id 关某一层"**。`WindowExt` 只给三个口子：
    ///
    /// - `close_dialog(cx)` → `active_dialogs.pop()`，**只关栈顶**；
    /// - `close_all_dialogs(cx)` → 全关；
    /// - `has_active_dialog(cx)` → 有没有开着。
    ///
    /// 于是"状态要求哪些浮层开着"和"实际开着哪些"之间只能靠**最长公共前缀**
    /// 来对齐：多出来的从栈顶 pop（安全），缺的依次追加（不动下层）。
    /// 这份记录就是那个 diff 的左边。见 `Shell::sync_dialog_stack`。
    pub(super) dialog_stack: Vec<DialogKey>,

    /// 正在查看详情的容器名；关闭时为 None。
    pub(super) detail: Option<String>,

    /// 正在查看详情的**发行版名**；关闭时为 None。
    ///
    /// 和容器的 `detail` 分成两个字段（而不是共用一个）：两者的详情弹窗内容
    /// 完全不同，共用一个字符串还得额外判断"这个名字是容器还是发行版"。
    pub(super) distro_detail: Option<String>,

    /// 「数据库管理」页的「基础信息」浮层开着没有。
    ///
    /// 连接卡（用户名 / 密码 + 那两段说明）原来**常驻页面顶部**，实机反馈
    /// "不用进来就在上面占用空间" —— 现在收进这个浮层，页面顶上是页头 +
    /// 一个「基础信息」按钮。
    pub(super) db_info_open: bool,

    /// 「拉取镜像」弹窗开着吗。
    ///
    /// 和 `forms.image_form` 分开记：**表单建好之后一直留着**（用户第二次打开时
    /// registry / 仓库名 / 上一批 tag 都还在），而这个布尔只管可见性。
    /// 拉取进行中**不许关**（见 `Shell::close_pull_dialog`）。
    pub(super) pull_open: bool,
}
