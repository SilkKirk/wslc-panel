//! 应用状态：页面枚举、数据快照、待确认的危险操作。
//!
//! 这一层**完全不碰 GPUI**，只依赖 `wslc-core`，
//! 因此未来换渲染层（或加 CLI 模式）时这里可以原样复用。
//!
//! # 两个域
//!
//! 面板同时管两样东西，命令也不同：
//!
//! | 域 | 命令 | 对象 |
//! |---|---|---|
//! | 容器 | `wslc.exe` | container / image / network / volume |
//! | 实例 | `wsl.exe` | WSL 发行版（distro） |
//!
//! 两者的采集**频率不同**（见 [`load_distro_status`] 的说明）：
//! 列表跟随自动刷新（默认 3 秒），`wsl --status` 单独降到 30 秒。

use std::path::PathBuf;
use std::time::Duration;

use wslc_core::cmd::install::InstallEvent;
use wslc_core::mirrors::{self, Offer};
use wslc_core::model::install::{online_matches, InstallSpec, OnlineDistro, PlanContext, Preflight};
// 「在线清单来自哪儿」是**数据**（`wsl -l -o` 还是兜底 JSON），
// 定义在 `wslc-core` 里；这里转出去，界面按它显示那句说明。
pub use wslc_core::model::install::OnlineListSource;
use wslc_core::model::{
    ContainerSummary, Distro, ImageListItem, NetworkListItem, Session, SystemInfo, VolumeListItem,
    WslStatus,
};
use wslc_core::settings::SettingsDoc;
use wslc_core::storage::StorageInfo;
use wslc_core::wslconfig::MisplacedKey;
use wslc_core::{Result, Wsl, Wslc, cmd};

/// 左侧导航的页面。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    /// 总览 / 基本信息。
    Dashboard,
    /// WSL 实例（发行版）列表。
    ///
    /// 命名刻意用「WSL 实例」而不是裸「实例」：`wslc` 的 **session**
    /// 在中文语境里也常被叫"实例"，裸「实例」会和现有概念打架。
    Instances,
    /// 添加实例（装一个新发行版）。
    AddInstance,
    /// 全部 container（含运行中与已退出）。
    ///
    /// 曾经有个单独的「当前运行」页，去掉了 —— 同一个列表用状态筛一下就够了，
    /// 没必要让用户在两页之间来回切（参考 1Panel：一个容器列表 + 状态筛选）。
    Containers,
    /// 镜像。
    Images,
    /// 网络。
    Networks,
    /// 卷。
    Volumes,
    /// 应用自己的设置（刷新间隔、主题……）。
    ///
    /// 和 [`Page::Config`] 的区别：这里存的是**本程序的偏好**
    /// （`%LOCALAPPDATA%\wslc-panel\prefs.json`），
    /// 而 `Config` 管的是 `wslc` 自己的 `settings.yaml`。
    AppSettings,
    /// wlsc 配置。
    Config,
    /// WSL 自己的全局配置（`%USERPROFILE%\.wslconfig`）。
    ///
    /// 和 [`Page::Config`] 的区别：那个是 **wslc** 的 `settings.yaml`，
    /// 这个是 **WSL 本身**的配置；两者互不相干，连文件位置都不同。
    WslConfig,
    /// 关于：应用介绍、版本、构建、地址。
    ///
    /// 和「应用设置」分开：那一页是**可改**的偏好，这一页只是**看**。
    About,
}

impl Page {
    /// 全部页面（决定导航顺序）。
    ///
    /// ⚠️ 同组的页面必须**连续** —— 侧边栏靠"组名变了就插一条标题"
    /// 来分组（见 `app.rs` 的 `render`）。
    /// ⚠️ 有三页**故意不在这里**：
    ///
    /// - `Config` / `WslConfig` —— 已经变成「应用设置」页里的两个 tab
    ///   （见 [`SettingsTab`]），留在 `ALL` 里会让侧边栏多出两个重复入口；
    /// - `AddInstance` —— 只能从**实例列表页上那个「添加实例」按钮**进。
    ///   侧边栏同时列「实例列表」和「添加实例」是冗余的：后者本来就是
    ///   前者的一个动作，不是并列的一站。
    ///
    /// 所以 `ALL` 是"侧边栏列哪些页"，**不等于**"有哪些页"。
    pub const ALL: [Page; 8] = [
        Page::Dashboard,
        Page::Instances,
        Page::Containers,
        Page::Images,
        Page::Networks,
        Page::Volumes,
        Page::AppSettings,
        Page::About,
    ];

    /// 导航标签。
    pub fn label(self) -> &'static str {
        match self {
            Page::Dashboard => "基本信息",
            Page::Instances => "实例列表",
            Page::AddInstance => "添加实例",
            Page::Containers => "容器",
            Page::Images => "镜像",
            Page::Networks => "网络",
            Page::Volumes => "卷",
            Page::AppSettings => "应用设置",
            Page::Config => "wlsc 配置",
            Page::WslConfig => "WSL 配置",
            Page::About => "关于",
        }
    }

    /// 导航分组（用于在侧边栏插入分隔标题）。
    pub fn group(self) -> &'static str {
        match self {
            Page::Dashboard => "概览",
            Page::Instances | Page::AddInstance => "WSL 实例",
            Page::Containers => "容器",
            Page::Images | Page::Networks | Page::Volumes => "资源",
            Page::AppSettings | Page::Config | Page::WslConfig | Page::About => "设置",
        }
    }

    /// 这一页是不是需要 `&mut Window` 才能进去。
    ///
    /// 「添加实例」有输入框，而 `InputState::new` 需要 `&mut Window` ——
    /// 所以它必须在**点击导航时**（有 window）把表单建好，
    /// 不能在渲染时建（那会每帧重建一次输入框，打字都打不进去）。
    pub fn needs_window_to_enter(self) -> bool {
        matches!(self, Page::AddInstance)
    }
}

/// 「添加实例」页里选中的**来源类型**。
///
/// 只记"用户选了哪一种"；带值的路径 / 名字在输入框里，
/// 拼成 [`wslc_core::model::install::InstallSpec`] 与执行计划是 `app.rs` 的事。
///
/// # 为什么是这五种
///
/// 对齐参考实现（`wsl-dashboard-ref` 的「添加实例」页）之后，来源从三条变五条：
/// 它按"文件是 tar 还是 vhdx"分开，还多一条"镜像站下载 rootfs"。
/// 我们另外**保留**了它没有的 `File`（`--install --from-file`）——
/// `.wsl` 是新格式，`--import` 吃不了，现成的能力不该退。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InstallSourceKind {
    /// 从本地 tar 导入（`wsl --import`）。
    #[default]
    Tar,
    /// 从本地 VHDX 导入（`wsl --import --vhd`）。
    Vhdx,
    /// 从本地文件安装（`wsl --install --from-file`，`.wsl` / rootfs 都行）。
    File,
    /// 从镜像站下载 rootfs 再导入（走 `curl.exe`）。
    Mirror,
    /// 在线安装（`wsl --install -d`）。
    Online,
}

impl InstallSourceKind {
    /// 全部可选值（决定下拉框里的顺序与索引）。
    ///
    /// 顺序：**本地的三条在前，联网的两条在后**；联网那两条里
    /// 「微软商店」排在「镜像源」前面 —— 商店是 WSL 官方那条路，
    /// 装出来就是官方发行版，镜像源是商店/GitHub 都拉不动时的备选。
    pub const ALL: [InstallSourceKind; 5] = [
        InstallSourceKind::Tar,
        InstallSourceKind::Vhdx,
        InstallSourceKind::File,
        InstallSourceKind::Online,
        InstallSourceKind::Mirror,
    ];

    /// 下拉框里的文案。
    ///
    /// ⚠️ **措辞是我们自己写的**：参考实现是 GPL-3.0-only、本仓库是 Apache-2.0
    /// （`AGENTS.md` §6），只对齐"有哪几类"，不抄它的字。
    pub fn label(self) -> &'static str {
        match self {
            Self::Tar => "本地 rootfs 文件（tar / tar.gz / tar.xz）",
            Self::Vhdx => "导入 VHDX 虚拟磁盘",
            Self::File => "从 .wsl / 文件安装",
            Self::Online => "微软商店 (Microsoft Store)",
            Self::Mirror => "在线发行版（国内镜像源）",
        }
    }

    /// 一句话说明（显示在下拉框下面）。
    pub fn hint(self) -> &'static str {
        match self {
            Self::Tar => "最可靠：本地 tar 文件，不需要联网。只是把文件系统铺开。",
            Self::Vhdx => "本地 ext4 虚拟磁盘（`.vhdx`）会被**拷贝**一份到安装目录，原始文件不动。",
            Self::File => "交给 WSL 自己的安装器（`.wsl` 或 rootfs 都行），会做首次启动初始化（建默认用户）。",
            Self::Online => {
                "走 WSL 官方的在线安装（`wsl --install -d`）：默认从微软商店装，\
                 商店拉不动时可以切到 GitHub（`--web-download`）。\
                 下面那个发行版清单来自微软的 `DistributionInfo.json`。"
            }
            Self::Mirror => {
                "从国内镜像站下载官方 rootfs 再导入 —— 商店和 GitHub 都拉不动时用这条，\
                 只用 `curl.exe`，不碰微软的服务器。"
            }
        }
    }

    /// 需不需要让用户填一个**文件路径**。
    pub fn needs_path(self) -> bool {
        matches!(self, Self::Tar | Self::Vhdx | Self::File)
    }

    /// 路径输入框的标签。
    pub fn path_label(self) -> &'static str {
        match self {
            Self::Tar => "tar 文件路径",
            Self::Vhdx => "VHDX 文件路径",
            Self::File => "安装文件路径",
            Self::Mirror | Self::Online => "",
        }
    }

    /// 路径输入框的占位提示。
    pub fn path_placeholder(self) -> &'static str {
        match self {
            Self::Tar => r"D:\img\ubuntu-rootfs.tar",
            Self::Vhdx => r"D:\img\ext4.vhdx",
            Self::File => r"D:\img\Ubuntu-24.04.wsl",
            Self::Mirror | Self::Online => "",
        }
    }

    /// 文件选择器的类别名与后缀表。
    ///
    /// 返回 `None` 表示这条来源不需要选文件。
    pub fn file_filter(self) -> Option<(&'static str, &'static [&'static str])> {
        /// `.tar.gz` / `.tar.xz` 这类复合后缀在 WinForms 的过滤器里只能写成
        /// 两个通配（`*.tar.gz` 其实也能匹配，写上更直观）。
        const TAR: &[&str] = &["tar", "gz", "xz", "zst"];
        const VHDX: &[&str] = &["vhdx"];
        const WSL_FILES: &[&str] = &["wsl", "tar", "gz", "xz"];

        match self {
            Self::Tar => Some(("tar 文件", TAR)),
            Self::Vhdx => Some(("VHDX 文件", VHDX)),
            Self::File => Some(("安装文件", WSL_FILES)),
            Self::Mirror | Self::Online => None,
        }
    }

    /// 发行版名输入框的占位提示。
    pub fn name_placeholder(self) -> &'static str {
        match self {
            Self::Online => "Ubuntu-24.04（选在线清单，或手输它的名字）",
            Self::Mirror => "Ubuntu-24.04",
            _ => "MyDistro",
        }
    }

    /// 安装目录是不是必填。
    ///
    /// 只有在线安装可以留空（WSL 有自己的默认位置）—— 而且"留空"还有个前提：
    /// 不改名。要改名的话重定位那一步必须知道装到哪儿，校验里单独管这件事。
    pub fn requires_install_dir(self) -> bool {
        !matches!(self, Self::Online)
    }

    /// 支不支持"装完启动"（只有在线安装有 `--no-launch`）。
    pub fn supports_launch(self) -> bool {
        matches!(self, Self::Online)
    }

    /// 要不要显示「镜像站」那一块（选发行版 + 探测 + 自定义 URL）。
    pub fn is_mirror(self) -> bool {
        matches!(self, Self::Mirror)
    }

    /// 要不要显示「在线清单」那一块。
    pub fn is_online(self) -> bool {
        matches!(self, Self::Online)
    }
}

/// 一次刷新得到的全部数据。
///
/// **每个区段独立记录错误**：某个命令失败不会让整页空白，
/// 用户仍然能看到其余可用信息（这是运维工具的基本要求）。
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    /// `wslc info`
    pub info: Option<SystemInfo>,
    /// `wslc system session list`
    pub sessions: Vec<Session>,
    /// 运行中容器（列表 + 实时统计）
    pub running: Vec<ContainerSummary>,
    /// 全部容器
    pub all: Vec<ContainerSummary>,
    /// 镜像
    pub images: Vec<ImageListItem>,
    /// 网络
    pub networks: Vec<NetworkListItem>,
    /// 卷
    pub volumes: Vec<VolumeListItem>,
    /// WSL 实例（发行版）列表 —— 来自 `wsl.exe`，不是 `wslc.exe`。
    ///
    /// 已合并注册表信息（安装位置）与磁盘虚拟大小。
    pub distros: Vec<Distro>,
    /// 会话存储的位置与占用（自行计算，`wslc` 不提供）。
    ///
    /// 解析不出来时为 `None`（例如 `LOCALAPPDATA` 没定义）。
    pub storage: Option<StorageInfo>,
    /// `%USERPROFILE%\.wslconfig` 的读取与静态检查结果。
    ///
    /// **在采集里读、不在渲染里读**：渲染每帧都可能发生，读文件不该在那儿做。
    /// 这个文件很小、极少变，跟着 3 秒的刷新一起读完全够。
    pub wslconfig: WslConfigInfo,
    /// 各区段的错误信息
    pub errors: Vec<String>,
    /// 本次刷新耗时（毫秒）
    pub elapsed_ms: u128,
}

/// 当前 Unix 时间戳（秒）。拿不到系统时间时给 0。
///
/// 只用来给临时文件名加一段"不会重复"的标记，精度不重要。
fn unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `.wslconfig` 的读取结果。
///
/// **只读** —— 这一版不做编辑（写文件）。为什么不提供"一键校验"见
/// [`wslc_core::wslconfig`] 的模块说明：实测 WSL 只在 **VM 启动时**
/// 报配置告警，主动触发就得先 `wsl --shutdown`，那会打断所有正在跑的发行版。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WslConfigInfo {
    /// 文件路径；`USERPROFILE` 取不到时为 `None`。
    pub path: Option<std::path::PathBuf>,
    /// 文件内容；`None` = **文件不存在**（没配过很正常，不是错误）。
    pub text: Option<String>,
    /// 读失败的原因（权限、编码……）。
    pub error: Option<String>,
    /// 静态检查发现的、放错文件的键（见 [`wslc_core::wslconfig::check`]）。
    pub misplaced: Vec<MisplacedKey>,
}

impl Snapshot {
    /// 是否至少有一个区段拿到了数据。
    pub fn has_data(&self) -> bool {
        self.info.is_some()
            || !self.all.is_empty()
            || !self.images.is_empty()
            || !self.networks.is_empty()
            || !self.distros.is_empty()
    }
}

/// 阻塞式采集一次完整快照。
///
/// 调用方必须在**后台线程/执行器**上调用（见 `app::Shell::refresh`）：
/// 内部会串行跑 7 条 `wslc` 命令 + 1 条 `wsl` 命令 + 1 条 `reg` 命令。
///
/// # 为什么 `--status` 不在这里
///
/// 它每轮都跑没必要（默认发行版/默认版本极少变），而且它的输出是**本地化**的。
/// 单独放到 [`load_distro_status`]，由调用方按 30 秒的节奏调。
pub fn load_snapshot(wslc: &Wslc, wsl: &Wsl) -> Snapshot {
    let started = std::time::Instant::now();
    let mut snap = Snapshot::default();

    match cmd::system::info(wslc) {
        Ok(info) => snap.info = Some(info),
        Err(e) => snap.errors.push(format!("wslc info：{e}")),
    }

    match cmd::system::sessions(wslc) {
        Ok(s) => snap.sessions = s,
        Err(e) => snap.errors.push(format!("会话列表：{e}")),
    }

    match cmd::container::running_summaries(wslc) {
        Ok(v) => snap.running = v,
        Err(e) => snap.errors.push(format!("运行中容器：{e}")),
    }

    match cmd::container::all_summaries(wslc) {
        Ok(v) => snap.all = v,
        Err(e) => snap.errors.push(format!("容器列表：{e}")),
    }

    match cmd::image::list(wslc) {
        Ok(v) => snap.images = v,
        Err(e) => snap.errors.push(format!("镜像列表：{e}")),
    }

    match cmd::network::list(wslc) {
        Ok(v) => snap.networks = v,
        Err(e) => snap.errors.push(format!("网络列表：{e}")),
    }

    match cmd::volume::list(wslc) {
        Ok(v) => snap.volumes = v,
        Err(e) => snap.errors.push(format!("卷列表：{e}")),
    }

    // WSL 实例：列表（3 秒级）+ 注册表与磁盘占用。
    //
    // 注册表走 `reg.exe`，实测只要 7~27 ms，所以**不必**降到 30 秒 ——
    // 真正贵的是多起一个 `wsl.exe` 进程，也就是 `--status`（见上）。
    match cmd::distro::snapshot(wsl) {
        Ok(list) => {
            snap.distros = list.distros;
            // 没认出来的行是"如实告知"，不是致命错误，但也不该被吞掉
            snap.errors.extend(list.warnings);
        }
        Err(e) => snap.errors.push(format!("WSL 实例列表：{e}")),
    }

    // 存储占用：`storagePath` 来自 settings.yaml，会话名来自 `wslc info`。
    // 这一步纯文件系统，不会失败到需要报错 —— 拿不到就是 None。
    let configured = settings_storage_path(snap.info.as_ref());
    // `Session` 的字段是 `id` / `creator_pid` / `display_name`（中文表头解析来的）
    let session = snap.sessions.first().map(|s| s.display_name.clone());
    snap.storage = wslc_core::storage::inspect(configured.as_deref(), session.as_deref());

    // `.wslconfig`：纯文件读取（几毫秒），没有进程开销，所以每轮都读。
    snap.wslconfig = load_wslconfig();

    snap.elapsed_ms = started.elapsed().as_millis();
    snap
}

/// 读 `%USERPROFILE%\.wslconfig` 并做静态检查。
///
/// 三种结果分得很清楚，因为界面上要说的话完全不同：
///
/// - 读到了 → 做检查，可能报出几条"放错文件的键"
/// - **文件不存在** → 正常状态（没配过），不是错误
/// - 读失败 → 真的出了问题（权限、编码），要如实说
fn load_wslconfig() -> WslConfigInfo {
    let path = wslc_core::config_path();
    match wslc_core::wslconfig::read() {
        Ok(Some(text)) => WslConfigInfo {
            misplaced: wslc_core::wslconfig::check(&text),
            path,
            text: Some(text),
            error: None,
        },
        Ok(None) => WslConfigInfo {
            path,
            text: None,
            error: None,
            misplaced: Vec::new(),
        },
        Err(e) => WslConfigInfo {
            path,
            text: None,
            error: Some(e.to_string()),
            misplaced: Vec::new(),
        },
    }
}

/// `wsl --status`（默认发行版 + 默认版本）。
///
/// **单独一个函数**，因为它的采集频率和列表不一样：
/// 调用方每 30 秒才调一次（见 `app::Shell` 的 `STATUS_EVERY_N_TICKS`）。
///
/// 失败时返回 `Err`，由调用方决定是覆盖还是保留上一次的值 ——
/// 通常应该**保留旧值**（一次瞬时失败不该让界面变成空白）。
pub fn load_distro_status(wsl: &Wsl) -> Result<WslStatus> {
    cmd::distro::status(wsl)
}

/// 只为了拿 `session.storagePath` 这一个值而读一次 `settings.yaml`。
///
/// 这里刻意**不复用** `Shell` 里那份 `SettingsDoc`：
/// 采集是在后台执行器上跑的，不该依赖 UI 侧的状态；而且 settings.yaml
/// 只有几百字节，读一次不到 1ms。
fn settings_storage_path(info: Option<&SystemInfo>) -> Option<String> {
    let path = info
        .map(|i| i.client.settings_file.clone())
        .filter(|p| !p.trim().is_empty())
        .map(PathBuf::from)
        .or_else(default_settings_path)?;

    let doc = SettingsDoc::load(&path).ok()?;
    doc.values().storage_path
}

/// 默认的 `settings.yaml` 路径（`wslc info` 拿不到时的兜底）。
pub fn default_settings_path() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(base).join("wslc").join("settings.yaml"))
}

/// 加载 `settings.yaml`。
///
/// 优先用 `wslc info` 报告的路径（这是权威来源），失败再退回默认位置。
pub fn load_settings(info: Option<&SystemInfo>) -> std::result::Result<SettingsDoc, String> {
    let reported = info
        .map(|i| i.client.settings_file.clone())
        .filter(|p| !p.trim().is_empty())
        .map(PathBuf::from);

    let path = reported
        .or_else(default_settings_path)
        .ok_or_else(|| "无法确定 settings.yaml 路径（LOCALAPPDATA 未设置）".to_owned())?;

    SettingsDoc::load(&path).map_err(|e| e.to_string())
}

/// 需要用户二次确认的危险操作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingAction {
    /// 停止容器。
    StopContainer(String),
    /// 强制终止容器。
    KillContainer(String),
    /// 删除容器。
    RemoveContainer(String),
    /// 清理所有已停止的容器。
    PruneContainers,
    /// 删除镜像。
    RemoveImage(String),
    /// 删除网络。
    RemoveNetwork(String),
    /// 删除卷。
    RemoveVolume(String),
}

impl PendingAction {
    /// 确认弹窗标题。
    pub fn title(&self) -> &'static str {
        match self {
            PendingAction::StopContainer(_) => "停止容器",
            PendingAction::KillContainer(_) => "强制终止容器",
            PendingAction::RemoveContainer(_) => "删除容器",
            PendingAction::PruneContainers => "清理已停止的容器",
            PendingAction::RemoveImage(_) => "删除镜像",
            PendingAction::RemoveNetwork(_) => "删除网络",
            PendingAction::RemoveVolume(_) => "删除卷",
        }
    }

    /// 确认弹窗正文。
    pub fn body(&self) -> String {
        match self {
            PendingAction::StopContainer(name) => {
                format!("将向容器 {name} 发送停止信号。容器内的未保存数据会丢失。")
            }
            PendingAction::KillContainer(name) => {
                format!("将立即杀死容器 {name}（SIGKILL），不做优雅退出。")
            }
            PendingAction::RemoveContainer(name) => {
                format!("将删除容器 {name} 及其可写层。此操作不可撤销。")
            }
            PendingAction::PruneContainers => {
                "将删除所有**已停止**的容器及其可写层。此操作不可撤销。".to_owned()
            }
            PendingAction::RemoveImage(reference) => {
                format!("将删除镜像 {reference}。若仍有容器引用它，删除会失败。")
            }
            PendingAction::RemoveNetwork(name) => {
                format!("将删除网络 {name}。仍有容器连接时会失败。")
            }
            PendingAction::RemoveVolume(name) => {
                format!("将删除卷 {name} 及其中的数据。此操作不可撤销。")
            }
        }
    }

    /// 按钮文案。
    pub fn confirm_label(&self) -> &'static str {
        match self {
            PendingAction::StopContainer(_) => "停止",
            PendingAction::KillContainer(_) => "强制终止",
            PendingAction::RemoveContainer(_) | PendingAction::PruneContainers => "删除",
            PendingAction::RemoveImage(_) => "删除镜像",
            PendingAction::RemoveNetwork(_) => "删除网络",
            PendingAction::RemoveVolume(_) => "删除卷",
        }
    }

    /// 执行操作，返回给用户看的结果摘要。
    pub fn execute(&self, wslc: &Wslc) -> Result<String> {
        match self {
            PendingAction::StopContainer(name) => {
                let done = cmd::container::stop(wslc, std::slice::from_ref(name))?;
                Ok(format!("已停止：{}", done.join("、")))
            }
            PendingAction::KillContainer(name) => {
                let done = cmd::container::kill(wslc, std::slice::from_ref(name))?;
                Ok(format!("已终止：{}", done.join("、")))
            }
            PendingAction::RemoveContainer(name) => {
                let done = cmd::container::remove(wslc, std::slice::from_ref(name), true)?;
                Ok(format!("已删除：{}", done.join("、")))
            }
            PendingAction::PruneContainers => {
                let out = cmd::container::prune(wslc, None)?;
                Ok(if out.is_empty() {
                    "没有需要清理的容器".to_owned()
                } else {
                    out
                })
            }
            PendingAction::RemoveImage(reference) => {
                let done = cmd::image::remove(wslc, std::slice::from_ref(reference), false)?;
                Ok(format!("已删除镜像：{}", done.join("、")))
            }
            PendingAction::RemoveNetwork(name) => {
                cmd::network::remove(wslc, std::slice::from_ref(name))?;
                Ok(format!("已删除网络：{name}"))
            }
            PendingAction::RemoveVolume(name) => {
                cmd::volume::remove(wslc, std::slice::from_ref(name), true)?;
                Ok(format!("已删除卷：{name}"))
            }
        }
    }
}

// 自动刷新间隔不再是界面上的档位开关，而是 `prefs.rs` 里的持久化偏好。
// 界面只负责在"设置"页里改它。

/// 字节 → 人类可读（1024 进制）。
///
/// 放在状态层而不是 `views.rs`：确认弹窗的文案（"将删除约 18.32 GB"）
/// 也要用它，而 `views.rs` 的函数是渲染私有的。
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

/// 需要二次确认的 **WSL 发行版**动作。
///
/// 与 [`PendingAction`]（容器）并列，刻意**不合并成一个枚举**：
/// 两者的执行器不同（`Wsl` vs `Wslc`）、危险级别与文案也不同，
/// 硬塞进一个类型只会让 `execute` 长出两个参数、每个 match 都要写全两套。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DistroAction {
    /// 终止发行版（丢未保存数据）。
    Terminate(String),
    /// 关停全部发行版。
    ShutdownAll,
    /// 注销（删除）发行版及其根文件系统。
    Unregister {
        /// 发行版名。
        name: String,
        /// 将要删掉的虚拟磁盘大小（字节）；读不到时为 `None`。
        ///
        /// 带上它是为了让用户在点"删除"之前**知道要删掉多少东西**。
        vhdx_bytes: Option<u64>,
    },
    /// 压缩发行版的虚拟磁盘。
    Compact(String),
    /// 开关稀疏 VHD（开启后 WSL 会自动回收已释放的空间）。
    ///
    /// 单独一个动作而不是"切换"：界面不显示当前状态
    /// （注册表的 `Flags` 里哪一位是稀疏**没有实测确认**），
    /// 所以给两个明确的入口，让用户自己说清要开还是要关。
    SetSparse {
        /// 发行版名。
        name: String,
        /// `true` = 开启稀疏。
        sparse: bool,
    },
}

impl DistroAction {
    /// 确认弹窗标题。
    pub fn title(&self) -> &'static str {
        match self {
            DistroAction::Terminate(_) => "终止发行版",
            DistroAction::ShutdownAll => "关停所有发行版",
            DistroAction::Unregister { .. } => "删除发行版",
            DistroAction::Compact(_) => "压缩虚拟磁盘",
            DistroAction::SetSparse { sparse, .. } => {
                if *sparse {
                    "开启稀疏磁盘"
                } else {
                    "关闭稀疏磁盘"
                }
            }
        }
    }

    /// 确认弹窗正文。
    pub fn body(&self) -> String {
        match self {
            DistroAction::Terminate(name) => format!(
                "将立即终止发行版 {name}。里面**没有保存**的东西会丢失，\
                 正在跑的服务会中断。"
            ),
            DistroAction::ShutdownAll => {
                "将关停**所有**发行版和 WSL2 轻量工具虚拟机。\
                 所有发行版里没保存的东西都会丢失。"
                    .to_owned()
            }
            DistroAction::Unregister { name, vhdx_bytes } => {
                let size = match vhdx_bytes {
                    Some(bytes) => format!("（虚拟磁盘约 {}）", format_bytes(*bytes)),
                    None => String::new(),
                };
                format!(
                    "将注销发行版 {name} 并**删除它的根文件系统**{size}。\
                     此操作不可撤销，里面的数据无法找回。"
                )
            }
            DistroAction::Compact(name) => format!(
                "将压缩 {name} 的虚拟磁盘，回收已释放的空间。\
                 这个动作**不会删除任何数据**，但发行版必须处于已停止状态；\
                 大磁盘可能要几分钟。"
            ),
            DistroAction::SetSparse { name, sparse } => {
                if *sparse {
                    format!(
                        "将把 {name} 的虚拟磁盘标记为**稀疏**：WSL 之后会自动回收\
                         已释放的块，相当于持续做压缩。不会删除任何数据。"
                    )
                } else {
                    format!(
                        "将关闭 {name} 的稀疏标志。磁盘不会再自动回收空间，\
                         之后只能手动压缩。不会删除任何数据。"
                    )
                }
            }
        }
    }

    /// 按钮文案。
    pub fn confirm_label(&self) -> &'static str {
        match self {
            DistroAction::Terminate(_) => "终止",
            DistroAction::ShutdownAll => "全部关停",
            DistroAction::Unregister { .. } => "删除",
            DistroAction::Compact(_) => "压缩",
            DistroAction::SetSparse { sparse, .. } => {
                if *sparse {
                    "开启"
                } else {
                    "关闭"
                }
            }
        }
    }

    /// 执行操作，返回给用户看的结果摘要。
    pub fn execute(&self, wsl: &Wsl) -> Result<String> {
        match self {
            DistroAction::Terminate(name) => {
                cmd::distro::terminate(wsl, name)?;
                Ok(format!("已终止：{name}"))
            }
            DistroAction::ShutdownAll => {
                cmd::distro::shutdown(wsl)?;
                Ok("已关停所有发行版".to_owned())
            }
            DistroAction::Unregister { name, .. } => {
                cmd::distro::unregister(wsl, name)?;
                Ok(format!("已删除发行版：{name}"))
            }
            DistroAction::Compact(name) => {
                cmd::distro::compact(wsl, name)?;
                Ok(format!("{name} 的虚拟磁盘已压缩"))
            }
            DistroAction::SetSparse { name, sparse } => {
                cmd::distro::set_sparse(wsl, name, *sparse)?;
                Ok(format!(
                    "{name} 的稀疏磁盘已{}",
                    if *sparse { "开启" } else { "关闭" }
                ))
            }
        }
    }
}

/// 确认弹窗里可能出现的动作 —— 两个域的**统一入口**。
///
/// `AppState::confirm` 只存这一个类型，弹窗和"确认"按钮就不必各写两份。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmAction {
    /// 容器 / 镜像 / 网络 / 卷。
    Container(PendingAction),
    /// WSL 发行版。
    Distro(DistroAction),
}

impl ConfirmAction {
    /// 确认弹窗标题。
    pub fn title(&self) -> &'static str {
        match self {
            ConfirmAction::Container(action) => action.title(),
            ConfirmAction::Distro(action) => action.title(),
        }
    }

    /// 确认弹窗正文。
    pub fn body(&self) -> String {
        match self {
            ConfirmAction::Container(action) => action.body(),
            ConfirmAction::Distro(action) => action.body(),
        }
    }

    /// 按钮文案。
    pub fn confirm_label(&self) -> &'static str {
        match self {
            ConfirmAction::Container(action) => action.confirm_label(),
            ConfirmAction::Distro(action) => action.confirm_label(),
        }
    }

    /// 执行。两个域各用各的调用器。
    pub fn execute(&self, wslc: &Wslc, wsl: &Wsl) -> Result<String> {
        match self {
            ConfirmAction::Container(action) => action.execute(wslc),
            ConfirmAction::Distro(action) => action.execute(wsl),
        }
    }
}

/// 「单输入框提示弹窗」要做什么。
///
/// 这三种动作都需要一个**文本参数**（位置 / 大小 / 用户名），
/// 所以共用一个弹窗 —— 标签、占位、说明和提交后调用的命令各不相同。
///
/// 弹窗本身**就是确认**（表单里带着说明和等效命令），提交后直接执行，
/// 不再叠一个二次确认 —— 连续两个弹窗比一个信息充分的弹窗更烦人。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    /// 移动安装位置。
    MoveDistro,
    /// 调整磁盘大小。
    ResizeDistro,
    /// 设置默认用户。
    SetDefaultUser,
    /// 导出发行版到 tar 文件。
    ///
    /// ⚠️ 它和另外三个**不一样**：提交后不是跑一条 `wsl --manage` 就完事，
    /// 而是起一个长时间、可取消的流式任务 —— 见 `Shell::start_export`。
    /// 复用的只是"要用户填一个文本参数"这个外形。
    ExportDistro,
}

impl PromptKind {
    /// 弹窗标题。
    pub fn title(self) -> &'static str {
        match self {
            Self::MoveDistro => "移动安装位置",
            Self::ResizeDistro => "调整磁盘大小",
            Self::SetDefaultUser => "设置默认用户",
            Self::ExportDistro => "导出发行版",
        }
    }

    /// 输入框标签。
    pub fn label(self) -> &'static str {
        match self {
            Self::MoveDistro => "新的安装目录（绝对路径）",
            Self::ResizeDistro => "新的磁盘大小",
            Self::SetDefaultUser => "用户名",
            Self::ExportDistro => "导出到（绝对路径，要含文件名）",
        }
    }

    /// 输入框占位提示。
    pub fn placeholder(self) -> &'static str {
        match self {
            Self::MoveDistro => r"D:\wsl\MyDistro",
            Self::ResizeDistro => "50GB",
            Self::SetDefaultUser => "myuser",
            Self::ExportDistro => r"D:\backup\MyDistro.tar",
        }
    }

    /// 一句说明，讲清这个动作的代价或前提。
    pub fn note(self) -> &'static str {
        match self {
            Self::MoveDistro => {
                "跨盘移动是真的在拷数据，18 GB 的盘可能要几分钟到几十分钟；\
                 同盘则只是改路径，很快。期间请勿关机。"
            }
            Self::ResizeDistro => {
                "只认 B / KB / MB / GB / TB，例如 50GB。\
                 缩小磁盘不一定被 WSL 支持，具体以它的判断为准。"
            }
            Self::SetDefaultUser => {
                "这是发行版内**已经存在**的用户名。用户不存在时 wsl 会报错 ——\
                 本程序不会替你创建用户。"
            }
            Self::ExportDistro => {
                "导出成 **tar**，之后可以用「添加实例 → 本地 rootfs 文件」再装回来，\
                 也能拷到别的机器上用。18 GB 的盘要几分钟到几十分钟，\
                 期间有进度条、随时可以取消；取消留下的是**不完整**的文件，要自己删。"
            }
        }
    }

    /// 提交按钮文案。
    pub fn confirm_label(self) -> &'static str {
        match self {
            Self::MoveDistro => "开始移动",
            Self::ResizeDistro => "调整大小",
            Self::SetDefaultUser => "设为默认用户",
            Self::ExportDistro => "开始导出",
        }
    }

    /// 这一种要不要选路径；要的话是**文件**还是**目录**。
    ///
    /// 不是每种都有 —— 「调整大小」填的是 `50GB` 这种数字，
    /// 「设置默认用户」填的是用户名，给它们一个「浏览…」按钮只会让人困惑。
    pub fn pick_target(self) -> Option<PickTarget> {
        match self {
            // 移动位置要的是一个**目录**（新的安装位置）
            Self::MoveDistro => Some(PickTarget {
                folders: true,
                label: "",
                extensions: &[],
                title: "选择新的安装目录",
            }),
            // 导出要的是一个**文件名**（默认就是 tar 格式）
            Self::ExportDistro => Some(PickTarget {
                folders: false,
                label: "tar 文件",
                extensions: &["tar"],
                title: "导出到",
            }),
            Self::ResizeDistro | Self::SetDefaultUser => None,
        }
    }
}

/// 文件 / 目录选择器的目标。见 [`PromptKind::pick_target`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PickTarget {
    /// `true` = 选目录，`false` = 选文件。
    pub folders: bool,
    /// 选文件时的类别名（如 `"tar 文件"`）。选目录时用不到。
    pub label: &'static str,
    /// 选文件时的后缀，**不含点**。选目录时为空。
    pub extensions: &'static [&'static str],
    /// 对话框标题。
    pub title: &'static str,
}

/// 提示条的类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    /// 普通信息。
    Info,
    /// 操作成功。
    Success,
    /// 操作失败。
    Error,
}

/// 一次性提示条。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    /// 展示文本。
    pub text: String,
    /// 类型（决定颜色）。
    pub kind: ToastKind,
}

impl Toast {
    /// 成功提示。
    pub fn success(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kind: ToastKind::Success,
        }
    }
    /// 失败提示。
    pub fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kind: ToastKind::Error,
        }
    }
    /// 普通提示。
    pub fn info(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kind: ToastKind::Info,
        }
    }
}

/// **不需要二次确认**的即时操作。
///
/// 与 [`PendingAction`] / [`DistroAction`] 的区别：这些不破坏数据、可逆，
/// 而且是运维里最高频的动作 —— 每次都弹确认反而碍事。
/// 破坏性的停止/强杀/删除仍然走那两个。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImmediateAction {
    /// 启动一个已停止的容器。
    StartContainer(String),
    /// 重启一个运行中的容器。
    RestartContainer(String),
    /// 启动一个发行版，并**让它一直保持运行**。
    ///
    /// 做法是从 Windows 这边吊住一个 `wsl.exe` 不放（WSL 认为有活动会话，
    /// 就不会回收发行版）。细节和实测见 [`wslc_core::cmd::distro::start`]。
    ///
    /// ⚠️ 吊着它的那个进程属于**本面板**。面板关掉后它不会跟着退出，
    /// 发行版会继续跑 —— 所以界面上要标出"由本面板保持运行中"。
    StartDistro(String),
    /// 打开发行版的终端（`wsl -d <name>`，会开一个新的控制台窗口）。
    ///
    /// 和 [`ImmediateAction::StartDistro`] 的区别：那个由**面板**吊着，
    /// 这个由**用户自己的终端窗口**吊着 —— 终端一关，发行版约 20 秒后就停了。
    OpenDistroTerminal(String),
    /// 把发行版设为默认。
    SetDefaultDistro(String),
}

/// 拉取镜像的实时进度。
///
/// 纯数据（不含任何 GPUI 类型），所以能放在 `AppState` 里；
/// 而输入框 `Entity<InputState>` 与取消令牌留在 `Shell`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullProgress {
    /// 正在拉取的镜像引用。
    pub image: String,
    /// 最近若干行输出（新的追加在后面）。
    pub lines: Vec<String>,
}

impl PullProgress {
    /// 最多保留多少行输出。
    ///
    /// 拉取的输出可能有上千行（每层都有自己的进度行），全留着没意义，
    /// 还会让内存一直涨。
    pub const MAX_LINES: usize = 200;

    /// 新建一个进度记录。
    pub fn new(image: impl Into<String>) -> Self {
        Self {
            image: image.into(),
            lines: Vec::new(),
        }
    }

    /// 追加若干行，并裁掉超出上限的旧行。
    ///
    /// 返回**是否真的有新增** —— 调用方据此决定要不要 `cx.notify()`，
    /// 避免每个轮询周期都触发一次重绘。
    pub fn push_lines(&mut self, new_lines: impl IntoIterator<Item = String>) -> bool {
        let before = self.lines.len();
        self.lines.extend(new_lines);
        if self.lines.len() > Self::MAX_LINES {
            let excess = self.lines.len() - Self::MAX_LINES;
            self.lines.drain(..excess);
        }
        self.lines.len() != before
    }

    /// 最后一行（界面拿它做"当前在干什么"）。
    pub fn last_line(&self) -> Option<&str> {
        self.lines.last().map(String::as_str)
    }
}

/// 导出发行版的实时进度。
///
/// # 为什么它和 [`PullProgress`] 长得不一样
///
/// 拉取镜像的进度只能从 `docker` 的输出行里读（每层一行），所以那边存的是
/// **输出行**。导出不一样：`wsl --export` 几乎不打进度，但**目标文件的大小
/// 是真实且连续的** —— 直接轮询文件比解析它的输出靠谱得多。
///
/// 所以这里存的是"写了多少字节 / 跑了多久"，界面上就是一个进度条 + 计时。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportProgress {
    /// 正在导出的发行版名。
    pub name: String,
    /// 目标文件路径。
    pub path: String,
    /// 已经写入的字节数。文件还没建出来时为 `None`。
    pub written: Option<u64>,
    /// 已经跑了多少秒。
    pub elapsed_secs: u64,
    /// 最后一行输出（出错时它就是原因）。
    pub last_line: String,
}

impl ExportProgress {
    /// 新建一个进度记录。
    pub fn new(name: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            path: path.into(),
            written: None,
            elapsed_secs: 0,
            last_line: String::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// 添加实例：在线清单 / 镜像站 / 安装进度
// ---------------------------------------------------------------------------

/// 在线可安装发行版的清单状态。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OnlineDistroState {
    /// 正在拉取。
    pub loading: bool,
    /// 拉取失败的原因（成功时为空串）。
    ///
    /// 刻意用 `String` 而不是 `Option`：界面要把它**原样**显示出来
    /// （"与服务器的连接被重置"这种话本身就是解释），空串即无错误。
    pub error: String,
    /// 清单。
    pub items: Vec<OnlineDistro>,
    /// 这份清单是哪儿来的。
    pub source: OnlineListSource,
    /// 当前选中的发行版 id。
    pub selected: Option<String>,
}

impl OnlineDistroState {
    /// 新建（空清单）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 按搜索词过滤后的清单（空词 = 全部）。
    pub fn filtered(&self, query: &str) -> Vec<&OnlineDistro> {
        self.items
            .iter()
            .filter(|item| online_matches(item, query))
            .collect()
    }

    /// 选中一个（同时记下 id）。
    pub fn select(&mut self, id: impl Into<String>) {
        self.selected = Some(id.into());
    }

    /// 丢掉当前清单（拉取失败时用）。
    pub fn clear(&mut self) {
        self.items.clear();
        self.selected = None;
    }
}

/// 一个镜像候选的探测结果。
#[derive(Debug, Clone, PartialEq)]
pub struct MirrorProbeResult {
    /// 镜像站名。
    pub site: String,
    /// 完整 URL。
    pub url: String,
    /// HTTP 状态码（连接失败时 0）。
    pub code: u16,
    /// 总耗时（秒）—— 排序与显示都用它。
    pub secs: f64,
    /// 文件大小（只有 200 才有值）。
    pub bytes: Option<u64>,
}

impl MirrorProbeResult {
    /// 这一个可用吗。
    pub fn is_ok(&self) -> bool {
        self.code == 200
    }

    /// 一句话结果（界面直接显示）。
    pub fn summary(&self) -> String {
        if self.is_ok() {
            let size = self
                .bytes
                .map(mirrors::human_bytes)
                .unwrap_or_else(|| "大小未知".to_owned());
            format!("{size} · {:.2} 秒", self.secs)
        } else if self.code == 0 {
            format!("连不上（{:.1} 秒后放弃）", self.secs)
        } else {
            format!("HTTP {}（这个镜像上没有这个文件？）", self.code)
        }
    }
}

/// 已经选定的那个镜像。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorChoice {
    /// 镜像站名。
    pub site: String,
    /// 完整 URL。
    pub url: String,
    /// 版本代号（计划里显示用）。
    pub release: String,
    /// 打包格式（`tar.xz` / `wsl`……）—— 决定装法，见 `mirrors` 的模块说明。
    pub format: String,
    /// 预期大小（下载进度条的分母）；未知时 `None`。
    pub bytes: Option<u64>,
}

impl MirrorChoice {
    /// 是不是 `.wsl` 包（走 `--install --from-file` 而不是 `--import`）。
    pub fn is_bundle(&self) -> bool {
        wslc_core::model::install::InstallSource::mirror_is_bundle(&self.format, &self.url)
    }
}

/// 「在线发行版（国内镜像源）」那一块的状态。
///
/// 清单是**动态拉的**（`wslc_core::cmd::catalog`，两个 HTTP 请求走 `curl.exe`），
/// 所以这里存的是"拉回来的清单 + 探测结果"，而不是写死的表。
#[derive(Debug, Clone, PartialEq)]
pub struct MirrorState {
    /// 正在拉清单。
    pub loading: bool,
    /// 清单拉取失败的原因（成功时 `None`）。
    ///
    /// 和 `online.error` 一样是"要原样显示给用户看"的那种话
    /// （接口不通 / 解不开 / 架构不认识）。
    pub load_error: Option<String>,
    /// 清单（每个"发行版 + 版本"一条）。
    pub offers: Vec<Offer>,
    /// 清单来源说明（`"清单来自 wslui 接口（api1 → api2）"`）。
    pub origin: String,
    /// 清单的更新时间（接口给的）。
    pub updated: Option<String>,
    /// 选中的那一条（`"<name> <version>"`；空串 = 还没选，按第一条算）。
    pub distro_id: String,
    /// 正在探测。
    pub probing: bool,
    /// 每个候选的探测结果（探测完成后按耗时排序）。
    pub results: Vec<MirrorProbeResult>,
    /// 选定的那一个。
    pub chosen: Option<MirrorChoice>,
    /// 探测失败原因（比如"所有镜像上都拿不到"）。
    pub error: Option<String>,
}

impl Default for MirrorState {
    fn default() -> Self {
        Self {
            loading: false,
            load_error: None,
            offers: Vec::new(),
            origin: String::new(),
            updated: None,
            distro_id: String::new(),
            probing: false,
            results: Vec::new(),
            chosen: None,
            error: None,
        }
    }
}

impl MirrorState {
    /// 新建。
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前选中的那一条（没显式选过就是第一条）。
    pub fn selected_offer(&self) -> Option<&Offer> {
        if !self.distro_id.trim().is_empty() {
            if let Some(found) = self.offers.iter().find(|offer| offer.id() == self.distro_id) {
                return Some(found);
            }
        }
        self.offers.first()
    }

    /// 换一条清单：清掉上一次的探测结果，并在原来选的那条没了时回落到第一条。
    pub fn set_offers(&mut self, offers: Vec<Offer>, origin: String, updated: Option<String>) {
        // 原来选的那条还在新清单里吗（清单会更新，版本可能就没了）
        let keep = offers.iter().any(|offer| offer.id() == self.distro_id);
        self.offers = offers;
        self.origin = origin;
        self.updated = updated;
        self.load_error = None;
        if !keep {
            self.distro_id = self.offers.first().map(Offer::id).unwrap_or_default();
            self.reset_probe();
        }
    }

    /// 换一个发行版：清掉上一次的探测结果（否则会拿旧结果当新的）。
    pub fn select_distro(&mut self, id: impl Into<String>) {
        let id = id.into();
        if self.distro_id == id {
            return;
        }
        self.distro_id = id;
        self.reset_probe();
    }

    /// 把探测结果清空（换发行版 / 重新拉清单时用）。
    pub fn reset_probe(&mut self) {
        self.results.clear();
        self.chosen = None;
        self.error = None;
        self.probing = false;
    }
}

/// 计划里已经走完的一步。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinishedStep {
    /// 这一步在干什么。
    pub label: String,
    /// 成功了吗。
    pub ok: bool,
    /// 失败原因（成功时为空串）。
    pub detail: String,
}

/// 安装是怎么结束的（还在跑时为 `None`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallOutcome {
    /// 装好了（消息里带名字）。
    Success(String),
    /// 失败。
    Failed {
        /// 失败原因。
        detail: String,
        /// 卡在哪一步。
        step: Option<String>,
    },
    /// 被用户取消。
    Cancelled,
}

/// 一次安装的实时状态。
///
/// # 为什么它和 [`PullProgress`] / [`ExportProgress`] 都不一样
///
/// 安装是**多步**的（在线安装改名要 8 步），所以除了日志和进度，
/// 还要能画出"走到第几步、哪几步已经过了"。
/// 这里存的就是那份**纯数据**：`wslc_core::cmd::install` 发事件，
/// [`InstallProgress::apply`] 把它翻译成状态 —— 翻译逻辑是纯函数，能单测。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallProgress {
    /// 发行版名。
    pub name: String,
    /// 计划里的步骤总数。
    pub total: usize,
    /// 当前第几步（从 1 开始；0 = 还没开始）。
    pub index: usize,
    /// 当前这步在干什么。
    pub step_label: String,
    /// 当前这步对应的命令行（没有命令的步骤是空串）。
    pub step_line: String,
    /// 当前这步能不能被取消。
    ///
    /// 重定位（导出 → 注销 → 导入）那一步是 `false`：中途停下等于把刚装好的删了。
    /// 界面据此**不给**取消按钮，而不是让用户点了之后才发现没用。
    pub cancellable: bool,
    /// 已经走完的步骤。
    pub finished: Vec<FinishedStep>,
    /// 日志（环形截断）。
    pub log: Vec<String>,
    /// 最后一行输出（出错时它就是原因）。
    pub last_line: String,
    /// 已经产生多少字节（下载 / 导出）。
    pub have: u64,
    /// 预期总量；未知时 `None`。
    pub total_bytes: Option<u64>,
    /// **当前这一步**跑了多少秒（导出 / 下载那种有进度的步骤才有）。
    ///
    /// 和 [`InstallProgress::elapsed_secs`] 分开：那个是整场安装的用时
    /// （界面每轮更新），而速度要按**这一步**的时间算 ——
    /// 用整场时间去除下载量会把速度显示得偏小得离谱。
    pub step_secs: u64,
    /// 整场安装跑了多少秒。
    pub elapsed_secs: u64,
    /// 结束状态。
    pub outcome: Option<InstallOutcome>,
}

impl InstallProgress {
    /// 最多留多少行日志。
    ///
    /// 在线安装和镜像下载的输出都不少，全留着只会让内存一直涨；
    /// 用户真正要看的是**最后几行**和"走到第几步"。
    pub const MAX_LOG_LINES: usize = 2000;

    /// 新建一个进度记录。
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            total: 0,
            index: 0,
            step_label: String::new(),
            step_line: String::new(),
            cancellable: true,
            finished: Vec::new(),
            log: Vec::new(),
            last_line: String::new(),
            have: 0,
            total_bytes: None,
            step_secs: 0,
            elapsed_secs: 0,
            outcome: None,
        }
    }

    /// 还在跑吗。
    pub fn is_running(&self) -> bool {
        self.outcome.is_none()
    }

    /// 已经走完几步。
    pub fn finished_count(&self) -> usize {
        self.finished.len()
    }

    /// 结束掉（成功/失败/取消）。
    ///
    /// 会往日志里补一行结局 —— 用户回看日志时，最后一行就是结论。
    /// 同一个结局重复设置不算变化（也**不会**重复写日志）。
    pub fn finish(&mut self, outcome: InstallOutcome) -> bool {
        if self.outcome.as_ref() == Some(&outcome) {
            return false;
        }
        let line = match &outcome {
            InstallOutcome::Success(message) => format!("✔ 完成：{message}"),
            InstallOutcome::Failed { detail, .. } => format!("✘ 失败：{detail}"),
            InstallOutcome::Cancelled => "✘ 已取消".to_owned(),
        };
        self.push_log(line);
        self.outcome = Some(outcome);
        true
    }

    /// 进度百分比（0~100）；拿不到总大小时 `None`。
    pub fn percent(&self) -> Option<f64> {
        let total = self.total_bytes?;
        if total == 0 {
            return None;
        }
        Some((self.have as f64 / total as f64 * 100.0).clamp(0.0, 100.0))
    }

    /// 平均速度（人话）；没在下载 / 时间还是 0 时为 `None`。
    pub fn speed(&self) -> Option<String> {
        if self.step_secs == 0 || self.have == 0 {
            return None;
        }
        Some(mirrors::human_speed(
            self.have as f64 / self.step_secs as f64,
        ))
    }

    /// 追加一行日志（并裁掉超上限的旧行）。
    fn push_log(&mut self, line: String) {
        self.log.push(line);
        if self.log.len() > Self::MAX_LOG_LINES {
            let excess = self.log.len() - Self::MAX_LOG_LINES;
            self.log.drain(..excess);
        }
    }

    /// 吃一个事件，返回**状态是否真的变了**（界面据此决定要不要重绘）。
    ///
    /// 进度事件每 250 ms 就来一个，而其中大多数只是重复的数字 ——
    /// 不比较就重绘等于让界面一直空转（导出那边是同样的处理）。
    pub fn apply(&mut self, event: &InstallEvent) -> bool {
        match event {
            InstallEvent::Step {
                index,
                total,
                label,
                line,
                cancellable,
            } => {
                self.index = *index;
                self.total = *total;
                self.step_label = label.clone();
                self.step_line = line.clone();
                self.cancellable = *cancellable;
                // 换步骤了：进度归零（那是上一步的产物大小）
                self.have = 0;
                self.total_bytes = None;
                self.step_secs = 0;
                self.push_log(format!("▶ 第 {index}/{total} 步：{label}"));
                if !line.is_empty() {
                    self.push_log(format!("  {line}"));
                }
                true
            }
            InstallEvent::Line(text) => {
                self.last_line = text.clone();
                self.push_log(text.clone());
                true
            }
            InstallEvent::Progress { have, total, secs } => {
                let changed = self.have != *have
                    || self.total_bytes != *total
                    || self.step_secs != *secs;
                self.have = *have;
                self.total_bytes = *total;
                self.step_secs = *secs;
                changed
            }
            InstallEvent::StepDone { ok, detail, .. } => {
                self.finished.push(FinishedStep {
                    label: self.step_label.clone(),
                    ok: *ok,
                    detail: detail.clone(),
                });
                if *ok {
                    self.push_log(format!("✔ {}", self.step_label));
                } else {
                    self.push_log(format!("✘ {}：{detail}", self.step_label));
                }
                true
            }
        }
    }
}

/// 「发行版配置（`/etc/wsl.conf`）」弹窗的状态。
///
/// **纯数据** —— 输入框（`Entity<InputState>`）在 `Shell::wslconf_dialog` 里，
/// 那是 GPUI 的类型，不能进 `AppState`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WslConfState {
    /// 哪个发行版。
    pub distro: String,
    /// 保序文档：原文 + 我们管的那几个键的显式值。
    pub doc: wslc_core::model::wslconf::WslConfDoc,
    /// WSL 自己的版本号（形如 `2.6.1.0`）；**拿不到时是空串**。
    ///
    /// 界面用它做版本门控（`[boot]` / `[gpu]` / `[time]` 要不要显示）。
    pub wsl_version: String,
    /// 正在读或者正在写。
    pub busy: bool,
    /// 保存前的校验错误；非空时挡下保存。
    pub errors: Vec<String>,
    /// 要不要显示"实际会写进去的内容"。
    pub show_preview: bool,
}

impl WslConfState {
    /// 新建。
    pub fn new(
        distro: impl Into<String>,
        doc: wslc_core::model::wslconf::WslConfDoc,
        wsl_version: impl Into<String>,
    ) -> Self {
        Self {
            distro: distro.into(),
            doc,
            wsl_version: wsl_version.into(),
            busy: false,
            errors: Vec::new(),
            show_preview: false,
        }
    }
}

/// 「应用设置」页里的 tab。
///
/// 原先这是侧边栏里的三个独立页面（应用设置 / wlsc 配置 / WSL 配置），
/// 但它们都是"设置"，分开摆反而让人找不着 —— 收进一页分 tab。
/// 参考项目的设置页也是这么分的（常规 / 高级 / 界面）。
///
/// ⚠️ 这里**没有**「关于」：那是一个只能看、不能改的页面，
/// 混进设置 tab 里会让人以为里面有开关。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SettingsTab {
    /// 常规：刷新间隔、主题。
    #[default]
    General,
    /// 高级：**wslc 自己**的 `settings.yaml`。
    Advanced,
    /// WSL：`%USERPROFILE%\.wslconfig`（**WSL 本身**的全局配置）。
    Wsl,
}

impl SettingsTab {
    /// 全部 tab（决定显示顺序）。
    pub const ALL: [SettingsTab; 3] = [
        SettingsTab::General,
        SettingsTab::Advanced,
        SettingsTab::Wsl,
    ];

    /// tab 标题。
    pub fn label(self) -> &'static str {
        match self {
            SettingsTab::General => "常规",
            SettingsTab::Advanced => "高级",
            SettingsTab::Wsl => "WSL",
        }
    }
}

/// 应用的完整状态。///
/// 这是 `app::Shell` 里唯一的字段，所有页面都是它的只读视图。
///
/// （`Shell` 在 `wslc-panel` 那个 crate 里 —— 它持有 GPUI 的 `InputState`，
/// 所以不能搬到这里来；这里刻意不写 intra-doc 链接，否则 `cargo doc` 会报断链。）
#[derive(Debug)]
pub struct AppState {
    /// 容器（`wslc.exe`）调用器。
    pub wslc: Wslc,
    /// 发行版（`wsl.exe`）调用器。
    pub wsl: Wsl,
    /// 当前页面。
    pub page: Page,
    /// 最近一次采集到的数据。
    pub snapshot: Snapshot,
    /// `wsl --status` 的结果。
    ///
    /// **不在 [`Snapshot`] 里**，因为它 30 秒才采一次，
    /// 而 `Snapshot` 每轮刷新都会整体重建 —— 放进去就会被清空。
    pub distro_status: Option<WslStatus>,
    /// `wsl --status` 最近一次的失败原因（成功时为 `None`）。
    pub distro_status_error: Option<String>,
    /// 已加载的配置文件。
    pub settings: Option<SettingsDoc>,
    /// 配置文件加载失败的原因。
    pub settings_error: Option<String>,
    /// 应用自己的偏好（自动刷新间隔等）。
    ///
    /// **不是** `wslc` 的配置 —— 见 `prefs.rs` 的说明。
    pub prefs: crate::prefs::Prefs,
    /// 是否正在后台采集。
    pub busy: bool,
    /// 正在拉取的镜像；空闲时为 `None`。
    ///
    /// 拉取可能几分钟到几十分钟，期间界面显示**实时输出**并允许取消。
    pub pulling: Option<PullProgress>,
    /// 正在导出的发行版；空闲时为 `None`。
    ///
    /// 和 `pulling` 并列而不是复用它：两者的进度来源完全不同
    /// （见 [`ExportProgress`] 的说明）。同一时刻只可能有其中一个在跑 ——
    /// 拉取走 `wslc.exe`，导出走 `wsl.exe`，但界面上的浮层位置是同一个，
    /// 所以启动前会互相检查。
    pub exporting: Option<ExportProgress>,
    /// **本面板正在吊着**的发行版名（保持它们运行）。
    ///
    /// WSL 在最后一个活动会话结束约 20 秒后回收发行版；所谓"启动"，
    /// 就是从 Windows 这边吊住一个 `wsl.exe` 不放
    /// （见 [`wslc_core::cmd::distro::start`]）。
    ///
    /// 这里只放**名字**（纯数据，能进 `AppState`）；真正的进程句柄在
    /// `Shell::keep_alive` 里 —— 那是"能力"，不是界面状态。
    ///
    /// 界面上要标出来，因为这是**我们**在维持它：用户有权知道
    /// 关掉面板之后它还会继续跑。
    pub kept_alive: Vec<String>,
    /// 「发行版配置（`/etc/wsl.conf`）」弹窗；关闭时为 `None`。
    ///
    /// 纯数据（原文 + 显式值 + 版本 + 校验错误）；真正的输入框在
    /// `Shell::wslconf_dialog` 里 —— 那是 GPUI 的类型。
    pub wslconf: Option<WslConfState>,
    /// 「应用设置」页当前选中的 tab。
    pub settings_tab: SettingsTab,
    /// 待用户确认的危险操作（容器域或发行版域）。
    pub confirm: Option<ConfirmAction>,
    /// 提示条。
    pub toast: Option<Toast>,
    /// 正在安装的发行版（「添加实例」）；空闲时为 `None`。
    ///
    /// 安装可能几十分钟（下载 rootfs、铺开文件系统），期间界面显示
    /// 步骤清单 + 日志 + 进度，并且可以取消。
    pub installing: Option<InstallProgress>,
    /// 在线可安装发行版的清单状态。
    pub online: OnlineDistroState,
    /// 「在线发行版（镜像源）」那一块的状态（动态清单 + 探测结果 + 选中的那个）。
    pub mirrors: MirrorState,
}

impl AppState {
    /// 新建初始状态。
    pub fn new(wslc: Wslc, wsl: Wsl) -> Self {
        Self {
            wslc,
            wsl,
            page: Page::Dashboard,
            snapshot: Snapshot::default(),
            distro_status: None,
            distro_status_error: None,
            settings: None,
            settings_error: None,
            prefs: crate::prefs::Prefs::load(),
            busy: false,
            pulling: None,
            exporting: None,
            kept_alive: Vec::new(),
            wslconf: None,
            settings_tab: SettingsTab::default(),
            confirm: None,
            toast: None,
            installing: None,
            online: OnlineDistroState::new(),
            mirrors: MirrorState::new(),
        }
    }

    /// 现有的发行版名（装前查重名用）。
    pub fn distro_names(&self) -> Vec<String> {
        self.snapshot
            .distros
            .iter()
            .map(|distro| distro.name.clone())
            .collect()
    }

    /// 拼一个"添加实例"用的计划上下文。
    ///
    /// `wslconfig_sparse` 取的是**最近一次采集**读到的 `.wslconfig`（见
    /// [`Snapshot::wslconfig`]）—— 渲染和点击都不该去读文件：
    /// 渲染每帧都可能发生，而这个值跟着刷新走完全够用。
    pub fn plan_context(&self) -> PlanContext {
        let sparse = self
            .snapshot
            .wslconfig
            .text
            .as_deref()
            .is_some_and(wslc_core::wslconfig::sparse_vhd);

        PlanContext {
            default_dir: self.prefs.install_dir.clone(),
            temp_dir: mirrors::temp_dir().to_string_lossy().into_owned(),
            // 临时文件名要唯一：同一个发行版连装两次不能撞在同一个文件上。
            stamp: format!("{}-{}", std::process::id(), unix_secs()),
            wslconfig_sparse: sparse,
        }
    }

    /// 装前检查（界面渲染与提交前用的是**同一套规则**）。
    ///
    /// `dir_non_empty` 由调用方查（要碰文件系统）：渲染时一律给 `false`
    /// —— 每帧去 `read_dir` 是不行的，那个检查放到提交那一刻做。
    pub fn preflight(&self, spec: &InstallSpec, dir_non_empty: bool) -> Preflight {
        wslc_core::model::install::preflight(
            spec,
            &self.plan_context(),
            &self.distro_names(),
            dir_non_empty,
        )
    }

    /// 自动刷新间隔。
    ///
    /// 固定由偏好决定，界面上不再提供"暂停"之类的档位开关。
    pub fn refresh_interval(&self) -> Duration {
        Duration::from_secs(self.prefs.refresh_secs)
    }

    /// 默认发行版的名字。
    ///
    /// 优先用列表里的 `*`（英文、好解析、且是权威来源），
    /// 拿不到时才退到 `wsl --status`（本地化文本）。
    pub fn default_distro(&self) -> Option<&str> {
        cmd::distro::default_distro(&self.snapshot.distros).or_else(|| {
            self.distro_status
                .as_ref()
                .and_then(|s| s.default_distro.as_deref())
        })
    }

    /// 当前会话的可读标签。
    pub fn session_label(&self) -> String {
        self.snapshot
            .info
            .as_ref()
            .and_then(|i| i.server.sessions.first())
            .map(|s| format!("#{} {}", s.id, s.name))
            .unwrap_or_else(|| "（无活动会话）".to_owned())
    }

    /// 弹出提示。
    pub fn notify(&mut self, toast: Toast) {
        self.toast = Some(toast);
    }

    /// 记录一次**容器域**危险操作的确认请求。
    ///
    /// 保留这个名字（而不是改成 `request_container_confirm`）是为了让
    /// 容器那 20 多处调用点一个字都不用改。
    pub fn request_confirm(&mut self, action: PendingAction) {
        self.confirm = Some(ConfirmAction::Container(action));
    }

    /// 记录一次**发行版域**危险操作的确认请求。
    pub fn request_distro_confirm(&mut self, action: DistroAction) {
        self.confirm = Some(ConfirmAction::Distro(action));
    }

    /// 取消确认。
    pub fn cancel_confirm(&mut self) {
        self.confirm = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_page_has_a_label_and_group() {
        for page in Page::ALL {
            assert!(!page.label().is_empty());
            assert!(!page.group().is_empty());
        }
        // v0.3 加了「实例列表」「添加实例」「应用设置」「WSL 配置」，从 6 个变成 10 个。
        // v0.4 把「wlsc 配置」「WSL 配置」收进「应用设置」的 tab、并加了「关于」，
        // 所以是 10 - 2 + 1 = 9。**总数少了不代表功能少了** —— 那两个只是换了入口。
        // v0.4 接着把「添加实例」也从侧边栏拿掉了（改从实例列表页的按钮进），9 - 1 = 8。
        //
        // 这个断言存在的意义就是**逼人改它**：加页面时忘了同步导航分组，
        // 侧边栏会出现重复的组标题。历史上 commit 552a91c 就是被它抓到的；
        // 加 `Page::About` 这次它又抓了一次（CI 的 `纯逻辑测试` 那一关）。
        //
        // ⚠️ 注意 `cargo check --all-targets` 只编译不执行，
        // 所以它真的被跑到要靠 `cargo test -p wslc-panel-core`（见 SPIKE 7.8）。
        assert_eq!(Page::ALL.len(), 8);
    }

    #[test]
    fn only_the_add_instance_page_needs_a_window_to_enter() {
        // 有输入框的页面必须在点击时（有 window）把表单建好。
        // 哪天给别的页面加了输入框，这个测试会提醒你一起改。
        //
        // ⚠️ `Page::AddInstance` **不在 `Page::ALL` 里**（侧边栏不列它），
        // 所以不能只遍历 `ALL` 就算完 —— 那样这条断言对它就是**空的**，
        // 哪天真把它改坏了也抓不到。显式再查一遍。
        for page in Page::ALL {
            assert!(
                !page.needs_window_to_enter(),
                "{page:?} 突然需要 window 了 —— 那它得走「添加实例」那条懒创建路径"
            );
        }
        assert!(Page::AddInstance.needs_window_to_enter());
    }

    #[test]
    fn install_sources_are_listed_and_described() {
        for kind in InstallSourceKind::ALL {
            assert!(!kind.label().is_empty(), "{kind:?}");
            assert!(!kind.hint().is_empty(), "{kind:?}");
        }
        assert!(InstallSourceKind::ALL.contains(&InstallSourceKind::default()));
        // 默认必须是最不依赖网络的那条
        assert_eq!(InstallSourceKind::default(), InstallSourceKind::Tar);
    }

    #[test]
    fn install_source_capabilities_are_consistent() {
        // 只有在线安装有「装完启动」，也只有它不需要文件路径
        assert!(InstallSourceKind::Online.supports_launch());
        assert!(!InstallSourceKind::Online.needs_path());

        // 本地三种都要选文件，而且都要有标签/占位/过滤器
        for kind in [
            InstallSourceKind::Tar,
            InstallSourceKind::Vhdx,
            InstallSourceKind::File,
        ] {
            assert!(!kind.supports_launch(), "{kind:?}");
            assert!(kind.needs_path(), "{kind:?}");
            assert!(!kind.path_label().is_empty(), "{kind:?}");
            assert!(!kind.path_placeholder().is_empty(), "{kind:?}");
            let (label, extensions) = kind.file_filter().expect("本地来源要能弹选择器");
            assert!(!label.is_empty(), "{kind:?}");
            assert!(!extensions.is_empty(), "{kind:?}");
            assert!(kind.requires_install_dir(), "{kind:?}");
        }

        // 两条"网络来源"不要文件路径，也不要文件选择器
        for kind in [InstallSourceKind::Mirror, InstallSourceKind::Online] {
            assert!(!kind.needs_path(), "{kind:?}");
            assert!(kind.file_filter().is_none(), "{kind:?}");
            assert!(kind.is_online() == (kind == InstallSourceKind::Online));
        }
        // 镜像站要安装目录（下载完要 --import），在线安装可以留空
        assert!(InstallSourceKind::Mirror.is_mirror());
        assert!(InstallSourceKind::Mirror.requires_install_dir());
        assert!(!InstallSourceKind::Online.requires_install_dir());

        // 版本**不给用户选**：本项目只支持 WSL 2，安装命令里固定 `--version 2`
        // （`InstallSourceKind` 上再也没有 `supports_version` 这种东西了）。

        // 五种来源的标签不能重复（按钮上会分不清）
        let labels: Vec<&str> = InstallSourceKind::ALL.iter().map(|k| k.label()).collect();
        let unique: std::collections::HashSet<&&str> = labels.iter().collect();
        assert_eq!(unique.len(), labels.len(), "{labels:?}");

        // 「微软商店」必须**一眼看得出来**是商店 —— 用户最想找的就是它，
        // 之前它叫"在线安装"，从名字上完全看不出跟商店有关系。
        assert!(
            InstallSourceKind::Online.label().contains("微软商店"),
            "{}",
            InstallSourceKind::Online.label()
        );
        // 商店排在镜像源前面：商店是 WSL 官方那条路，镜像源是它的备选
        let order = InstallSourceKind::ALL.to_vec();
        let store = order.iter().position(|k| *k == InstallSourceKind::Online);
        let mirror = order.iter().position(|k| *k == InstallSourceKind::Mirror);
        assert!(store < mirror, "{order:?}");
    }

    #[test]
    fn install_progress_follows_the_events() {
        use wslc_core::cmd::install::InstallEvent;

        let mut progress = InstallProgress::new("MyUbuntu");
        assert!(progress.is_running());
        assert_eq!(progress.percent(), None);

        // 第一步开始
        assert!(progress.apply(&InstallEvent::Step {
            index: 1,
            total: 3,
            label: "创建安装目录".to_owned(),
            line: "创建安装目录 D:\\wsl\\MyUbuntu".to_owned(),
            cancellable: true,
        }));
        assert_eq!(progress.index, 1);
        assert_eq!(progress.total, 3);
        assert!(progress.cancellable);
        assert!(progress.log.iter().any(|l| l.contains("第 1/3 步")));
        assert!(progress.log.iter().any(|l| l.contains("创建安装目录 D:")));

        // 进度事件：数字没变就不算变化（界面据此不重绘）
        assert!(progress.apply(&InstallEvent::Progress {
            have: 100,
            total: Some(400),
            secs: 5,
        }));
        assert!(!progress.apply(&InstallEvent::Progress {
            have: 100,
            total: Some(400),
            secs: 5,
        }));
        assert_eq!(progress.percent(), Some(25.0));
        assert!(progress.speed().is_some());

        // 换步骤要把上一步的进度归零（那是上一步的产物大小）
        progress.apply(&InstallEvent::Step {
            index: 2,
            total: 3,
            label: "导入".to_owned(),
            line: String::new(),
            // 重定位那一步不可取消 —— 界面据此不给"取消"按钮
            cancellable: false,
        });
        assert!(!progress.cancellable);
        assert_eq!(progress.have, 0);
        assert_eq!(progress.percent(), None);

        // 一行的输出同时进日志和 last_line
        progress.apply(&InstallEvent::Line("正在导入...".to_owned()));
        assert_eq!(progress.last_line, "正在导入...");

        // 步骤结束 → 记一笔
        progress.apply(&InstallEvent::StepDone {
            index: 2,
            ok: false,
            detail: "退出码 -1".to_owned(),
        });
        assert_eq!(progress.finished_count(), 1);
        assert!(!progress.finished[0].ok);
        assert!(progress.log.iter().any(|l| l.contains("✘")));

        // 结束之后不算"还在跑"
        assert!(progress.finish(InstallOutcome::Failed {
            detail: "x".to_owned(),
            step: Some("导入".to_owned()),
        }));
        assert!(!progress.is_running());
        // 同一个结局重复设置 → 不算变化
        assert!(!progress.finish(InstallOutcome::Failed {
            detail: "x".to_owned(),
            step: Some("导入".to_owned()),
        }));
    }

    #[test]
    fn install_progress_log_is_capped() {
        use wslc_core::cmd::install::InstallEvent;

        let mut progress = InstallProgress::new("X");
        for i in 0..(InstallProgress::MAX_LOG_LINES + 50) {
            progress.apply(&InstallEvent::Line(format!("第 {i} 行")));
        }
        assert_eq!(progress.log.len(), InstallProgress::MAX_LOG_LINES);
        // 留下的是**最后**那些行（用户要看的是最新的）
        assert!(progress.log.last().unwrap().contains(&format!(
            "第 {} 行",
            InstallProgress::MAX_LOG_LINES + 49
        )));
    }

    #[test]
    fn online_state_filters_and_selects() {
        let mut online = OnlineDistroState::new();
        assert!(online.filtered("").is_empty());
        assert_eq!(online.source, OnlineListSource::Unknown);
        assert!(online.source.label().is_empty());

        online.items = vec![
            OnlineDistro::new("Ubuntu", "Ubuntu"),
            OnlineDistro::new("Ubuntu-24.04", "Ubuntu 24.04 LTS"),
            OnlineDistro::new("Debian", "Debian GNU/Linux"),
        ];
        online.source = OnlineListSource::FallbackJson;
        assert_eq!(online.filtered("").len(), 3);
        assert_eq!(online.filtered("ubuntu").len(), 2);
        assert_eq!(online.filtered("24").len(), 1);

        online.select("Ubuntu-24.04");
        assert_eq!(online.selected.as_deref(), Some("Ubuntu-24.04"));

        // 拉取失败时要把旧清单清掉，否则用户会挑一个已经不成立的列表
        online.clear();
        assert!(online.items.is_empty());
        assert!(online.selected.is_none());
    }

    #[test]
    fn mirror_state_forgets_previous_probes_when_switching_or_reloading() {
        fn offer(name: &str, version: &str) -> Offer {
            Offer {
                name: name.to_owned(),
                version: version.to_owned(),
                sources: vec![wslc_core::mirrors::OfferSource {
                    mirror: "lxc-tuna".to_owned(),
                    url: format!("https://mirrors.tuna.tsinghua.edu.cn/x/{name}-{version}.tar.xz"),
                    format: "tar.xz".to_owned(),
                }],
            }
        }
        fn probed() -> MirrorProbeResult {
            MirrorProbeResult {
                site: "lxc-tuna".to_owned(),
                url: "https://mirrors.tuna.tsinghua.edu.cn/x/a.tar.xz".to_owned(),
                code: 200,
                secs: 0.5,
                bytes: Some(100),
            }
        }

        let mut state = MirrorState::new();
        // 还没拉清单 → 没有可选的
        assert!(state.selected_offer().is_none());
        assert!(state.distro_id.is_empty());

        state.set_offers(
            vec![offer("Ubuntu", "24.04"), offer("Alpine", "3.22")],
            "测试清单".to_owned(),
            Some("2026-10-09T00:00:00Z".to_owned()),
        );
        // 拉回来之后默认选第一条
        assert_eq!(state.distro_id, "Ubuntu 24.04");
        assert_eq!(state.origin, "测试清单");
        assert_eq!(state.updated.as_deref(), Some("2026-10-09T00:00:00Z"));

        state.results.push(probed());
        state.chosen = Some(MirrorChoice {
            site: "lxc-tuna".to_owned(),
            url: "https://mirrors.tuna.tsinghua.edu.cn/x/a.tar.xz".to_owned(),
            release: "24.04".to_owned(),
            format: "tar.xz".to_owned(),
            bytes: Some(100),
        });

        state.select_distro("Alpine 3.22");
        assert_eq!(state.distro_id, "Alpine 3.22");
        // 换了发行版就不该留着上一条的探测结果（那会拿着 A 的 URL 去装 B）
        assert!(state.results.is_empty());
        assert!(state.chosen.is_none());
        assert!(state.error.is_none());

        // 选同一个不算换
        state.results.push(probed());
        state.select_distro("Alpine 3.22");
        assert_eq!(state.results.len(), 1);

        // 重新拉清单：原来选的那条还在 → 保留选择与探测结果
        state.set_offers(
            vec![offer("Ubuntu", "24.04"), offer("Alpine", "3.22")],
            "新清单".to_owned(),
            None,
        );
        assert_eq!(state.distro_id, "Alpine 3.22");
        assert_eq!(state.results.len(), 1);

        // 原来选的那条**没了**（版本更新）→ 回落到第一条并清掉探测结果
        state.set_offers(vec![offer("Ubuntu", "26.04")], "新清单".to_owned(), None);
        assert_eq!(state.distro_id, "Ubuntu 26.04");
        assert!(state.results.is_empty() && state.chosen.is_none());

        // id 与清单对不上（理论上不该发生）时也要能回落到第一条，而不是给 None
        state.distro_id = "不存在的 1".to_owned();
        assert_eq!(
            state.selected_offer().map(Offer::id).as_deref(),
            Some("Ubuntu 26.04")
        );
    }

    #[test]
    fn mirror_probe_summary_says_something_useful_for_every_outcome() {
        let ok = MirrorProbeResult {
            site: "清华 TUNA".to_owned(),
            url: "https://x".to_owned(),
            code: 200,
            secs: 1.234,
            bytes: Some(229_623_728),
        };
        assert!(ok.is_ok());
        let text = ok.summary();
        assert!(text.contains("229.6 MB"), "{text}");
        assert!(text.contains("1.23"), "{text}");

        // 连不上（curl 给 000）
        let dead = MirrorProbeResult {
            code: 0,
            secs: 8.0,
            ..ok.clone()
        };
        assert!(!dead.is_ok());
        assert!(dead.summary().contains("连不上"), "{}", dead.summary());

        // 404：文件可能改名了，这是最常见的一种失败
        let missing = MirrorProbeResult {
            code: 404,
            secs: 0.1,
            bytes: None,
            ..ok.clone()
        };
        assert!(missing.summary().contains("404"), "{}", missing.summary());

        // 200 但拿不到大小时也要能显示
        let unknown = MirrorProbeResult {
            bytes: None,
            ..ok.clone()
        };
        assert!(unknown.summary().contains("大小未知"), "{}", unknown.summary());
    }

    #[test]
    fn plan_context_reads_prefs_and_the_sparse_flag() {
        let mut state = AppState::new(wslc_core::Wslc::new(), wslc_core::Wsl::new());
        state.prefs.install_dir = Some(r"D:\wsl".to_owned());

        // 没读过 .wslconfig → 不开稀疏
        let ctx = state.plan_context();
        assert_eq!(ctx.default_dir.as_deref(), Some(r"D:\wsl"));
        assert!(!ctx.wslconfig_sparse);
        assert!(!ctx.temp_dir.is_empty());
        assert!(!ctx.stamp.is_empty());

        // 采集里读到的真实配置要求稀疏 → 跟着它走
        state.snapshot.wslconfig.text = Some("[experimental]\nsparseVhd=true\n".to_owned());
        assert!(state.plan_context().wslconfig_sparse);
    }

    #[test]
    fn app_state_preflight_uses_the_current_distro_list() {
        use wslc_core::model::install::{InstallSource, InstallSpec};

        let mut state = AppState::new(wslc_core::Wslc::new(), wslc_core::Wsl::new());
        // 有默认安装目录 → tar 导入的必填项能由"默认目录 + 名字"推出来
        state.prefs.install_dir = Some(r"D:\wsl".to_owned());
        let spec = InstallSpec::new(
            "Ubuntu",
            InstallSource::Tar {
                path: r"D:\a.tar".to_owned(),
            },
        );

        // 列表是空的时候它能过（只差目录非空与否）
        assert!(state.preflight(&spec, false).ok());
        assert!(!state.preflight(&spec, true).ok());

        // 列表里已经有 Ubuntu → 报重名
        state.snapshot.distros.push(wslc_core::model::Distro::new(
            "Ubuntu",
            wslc_core::model::DistroState::Stopped,
            Some(2),
            false,
        ));
        assert_eq!(state.distro_names(), vec!["Ubuntu".to_owned()]);
        let check = state.preflight(&spec, false);
        assert!(!check.ok());
        assert!(check.error_text().unwrap().contains("已经有一个"));
    }

    #[test]
    fn pages_are_grouped_in_navigation_order() {
        // 侧边栏依赖"同组的页面在 ALL 里连续"来插入分组标题。
        let groups: Vec<&str> = Page::ALL.iter().map(|p| p.group()).collect();
        let mut deduped: Vec<&str> = Vec::new();
        for g in groups {
            if deduped.last() != Some(&g) {
                deduped.push(g);
            }
        }
        // 去重后不能再出现重复的组名（否则同组被拆成两段）。
        let mut seen = std::collections::HashSet::new();
        for g in &deduped {
            assert!(seen.insert(*g), "分组 {g} 在导航里被拆成了多段");
        }
        assert_eq!(
            deduped,
            vec!["概览", "WSL 实例", "容器", "资源", "设置"]
        );
    }

    #[test]
    fn snapshot_default_is_empty_and_has_no_data() {
        let snap = Snapshot::default();
        assert!(!snap.has_data());
        assert!(snap.errors.is_empty());
    }

    #[test]
    fn refresh_interval_comes_from_prefs() {
        let mut state = AppState::new(Wslc::new(), Wsl::new());
        state.prefs.refresh_secs = 10;
        assert_eq!(state.refresh_interval(), Duration::from_secs(10));

        state.prefs.refresh_secs = 1;
        assert_eq!(state.refresh_interval(), Duration::from_secs(1));
    }

    #[test]
    fn default_refresh_interval_is_three_seconds() {
        // 默认必须是 3s —— 这是产品决定，界面不再暴露这个开关。
        assert_eq!(
            crate::prefs::Prefs::default().refresh_secs,
            3,
            "默认刷新间隔应为 3 秒"
        );
    }

    #[test]
    fn pull_progress_keeps_only_the_last_lines() {
        let mut p = PullProgress::new("alpine:latest");
        assert_eq!(p.image, "alpine:latest");
        assert!(p.last_line().is_none());

        assert!(p.push_lines((0..10).map(|i| format!("line {i}"))));
        assert_eq!(p.lines.len(), 10);
        assert_eq!(p.last_line(), Some("line 9"));

        // 超过上限后丢掉最老的，保留最新的
        assert!(p.push_lines((0..PullProgress::MAX_LINES + 20).map(|i| format!("extra {i}"))));
        assert_eq!(p.lines.len(), PullProgress::MAX_LINES);
        assert_eq!(p.last_line(), Some("extra 219"));

        // 空输入不算新增（调用方据此跳过重绘）
        assert!(!p.push_lines(Vec::new()));
        assert_eq!(p.lines.len(), PullProgress::MAX_LINES);
    }

    #[test]
    fn danger_actions_all_have_copy() {
        let actions = [
            PendingAction::StopContainer("a".into()),
            PendingAction::KillContainer("a".into()),
            PendingAction::RemoveContainer("a".into()),
            PendingAction::PruneContainers,
            PendingAction::RemoveImage("img".into()),
            PendingAction::RemoveNetwork("net".into()),
            PendingAction::RemoveVolume("vol".into()),
        ];
        for action in actions {
            assert!(!action.title().is_empty());
            assert!(!action.body().is_empty());
            assert!(!action.confirm_label().is_empty());
        }
    }

    #[test]
    fn every_distro_action_has_copy_too() {
        let actions = [
            DistroAction::Terminate("Ubuntu".into()),
            DistroAction::ShutdownAll,
            DistroAction::Unregister {
                name: "Ubuntu".into(),
                vhdx_bytes: Some(19_666_042_880),
            },
            DistroAction::Compact("Ubuntu".into()),
            DistroAction::SetSparse {
                name: "Ubuntu".into(),
                sparse: true,
            },
            DistroAction::SetSparse {
                name: "Ubuntu".into(),
                sparse: false,
            },
        ];
        for action in actions {
            assert!(!action.title().is_empty(), "{action:?}");
            assert!(!action.body().is_empty(), "{action:?}");
            assert!(!action.confirm_label().is_empty(), "{action:?}");
        }
    }

    #[test]
    fn sparse_toggle_says_which_way_it_goes() {
        // 两个方向的文案必须不一样，否则用户分不清点了会开还是关
        let on = DistroAction::SetSparse {
            name: "U".into(),
            sparse: true,
        };
        let off = DistroAction::SetSparse {
            name: "U".into(),
            sparse: false,
        };
        assert_eq!(on.title(), "开启稀疏磁盘");
        assert_eq!(off.title(), "关闭稀疏磁盘");
        assert_eq!(on.confirm_label(), "开启");
        assert_eq!(off.confirm_label(), "关闭");
        assert_ne!(on.body(), off.body());
    }

    #[test]
    fn every_prompt_kind_has_copy() {
        for kind in [
            PromptKind::MoveDistro,
            PromptKind::ResizeDistro,
            PromptKind::SetDefaultUser,
            PromptKind::ExportDistro,
        ] {
            assert!(!kind.title().is_empty(), "{kind:?}");
            assert!(!kind.label().is_empty(), "{kind:?}");
            assert!(!kind.placeholder().is_empty(), "{kind:?}");
            assert!(!kind.note().is_empty(), "{kind:?}");
            assert!(!kind.confirm_label().is_empty(), "{kind:?}");
        }
    }

    #[test]
    fn confirm_action_delegates_to_the_right_domain() {
        // 包装层只是转发，不能把两边的文案搞混
        let container = ConfirmAction::Container(PendingAction::RemoveVolume("v".into()));
        assert_eq!(container.title(), "删除卷");
        assert_eq!(container.confirm_label(), "删除卷");

        let distro = ConfirmAction::Distro(DistroAction::Terminate("Ubuntu".into()));
        assert_eq!(distro.title(), "终止发行版");
        assert_eq!(distro.confirm_label(), "终止");
        assert!(distro.body().contains("Ubuntu"));
    }

    #[test]
    fn unregister_body_shows_how_much_is_about_to_be_deleted() {
        // 用户在点"删除"之前必须知道自己要删掉多少东西
        let with_size = DistroAction::Unregister {
            name: "Ubuntu".into(),
            vhdx_bytes: Some(19_666_042_880),
        };
        let body = with_size.body();
        assert!(body.contains("18.32 GB"), "{body}");
        assert!(body.contains("不可撤销"), "{body}");

        // 读不到大小时也要能正常出文案（只是没有那句括号）
        let without_size = DistroAction::Unregister {
            name: "Ubuntu".into(),
            vhdx_bytes: None,
        };
        let body = without_size.body();
        assert!(!body.contains("虚拟磁盘约"), "{body}");
        assert!(body.contains("Ubuntu"), "{body}");
    }

    #[test]
    fn prune_warns_about_being_irreversible() {
        assert!(PendingAction::PruneContainers.body().contains("不可撤销"));
        assert!(
            PendingAction::RemoveVolume("v".into())
                .body()
                .contains("不可撤销")
        );
    }

    #[test]
    fn format_bytes_is_1024_based_and_stable() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1024), "1.00 KB");
        assert_eq!(format_bytes(19_666_042_880), "18.32 GB");
    }

    #[test]
    fn default_settings_path_points_at_localappdata() {
        // 在设置了 LOCALAPPDATA 的 Windows 上必须能给出路径。
        if std::env::var_os("LOCALAPPDATA").is_some() {
            let path = default_settings_path().expect("应能推导出路径");
            assert!(path.to_string_lossy().contains("wslc"));
            assert!(path.to_string_lossy().ends_with("settings.yaml"));
        }
    }

    #[test]
    fn new_state_starts_on_dashboard_and_is_idle() {
        let state = AppState::new(Wslc::new(), Wsl::new());
        assert_eq!(state.page, Page::Dashboard);
        assert!(!state.busy);
        assert!(state.confirm.is_none());
        assert!(state.toast.is_none());
        assert!(state.settings.is_none());
        // `prefs` 是从磁盘读的，这里只断言它落在合法范围内。
        assert!(state.prefs.refresh_secs >= crate::prefs::MIN_REFRESH_SECS);
        assert!(state.prefs.refresh_secs <= crate::prefs::MAX_REFRESH_SECS);
        assert_eq!(state.session_label(), "（无活动会话）");
    }

    #[test]
    fn confirm_can_be_requested_and_cancelled() {
        let mut state = AppState::new(Wslc::new(), Wsl::new());

        state.request_confirm(PendingAction::PruneContainers);
        assert_eq!(
            state.confirm,
            Some(ConfirmAction::Container(PendingAction::PruneContainers))
        );
        state.cancel_confirm();
        assert!(state.confirm.is_none());

        // 发行版域走同一个槽位 —— 两个域不能各有一个 confirm，
        // 否则同时触发时会出现两个叠在一起的确认框。
        state.request_distro_confirm(DistroAction::ShutdownAll);
        assert_eq!(
            state.confirm,
            Some(ConfirmAction::Distro(DistroAction::ShutdownAll))
        );
        state.cancel_confirm();
        assert!(state.confirm.is_none());
    }

    #[test]
    fn toast_constructors_set_the_right_kind() {
        assert_eq!(Toast::success("a").kind, ToastKind::Success);
        assert_eq!(Toast::error("a").kind, ToastKind::Error);
        assert_eq!(Toast::info("a").kind, ToastKind::Info);
    }

    #[test]
    fn session_label_uses_the_first_active_session() {
        use wslc_core::jsonl;
        use wslc_core::model::SystemInfo;
        let mut state = AppState::new(Wslc::new(), Wsl::new());
        state.snapshot.info = Some(
            jsonl::parse_object::<SystemInfo>(include_str!(
                "../../wslc-core/tests/fixtures/info.json"
            ))
            .unwrap(),
        );
        assert_eq!(state.session_label(), "#1 wslc-cli-76434");
    }
}
