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
use wslc_core::model::install::{
    InstallSpec, OnlineDistro, PlanContext, Preflight, online_matches,
};
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
use wslc_core::{Result, Wsl, Wslc, cmd, registry};

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
    /// 镜像：**远端浏览 + 拉取 + 本地已有**，三件事一页。
    ///
    /// ⚠️ v0.6.2 之前这是**两页**（「镜像」= 本地已有，「镜像仓库」= 远端浏览），
    /// 外加「镜像」页上一个「按引用拉取…」弹窗 —— 三个入口背后是同一段拉取实现。
    /// 合并的理由和 `docs/PLAN-v0.6.md` §3.6 记的那次取舍有关：当时留两个入口是
    /// 为了不丢掉"从别的 registry 拉"这个能力，而**正确的合并方向就是把 registry
    /// 提成可编辑字段**（现在做了：`RegistryState::host` 来自输入框）。
    /// 于是三个入口收成一个，`AGENTS.md` §5「同一个动作只给一个入口」重新成立。
    Images,
    /// 网络。
    Networks,
    /// 卷。
    Volumes,
    /// 应用自己的设置（刷新间隔、主题……）。
    ///
    /// 这里存的是**本程序的偏好**（`%LOCALAPPDATA%\wslc-panel\prefs.json`）。
    /// `wslc` 自己的 `settings.yaml` 和 WSL 自己的 `.wslconfig` 是这一页里的
    /// 两个 **tab**（见 [`SettingsTab`]），不是并列的页面。
    ///
    /// ⚠️ 它们曾经是 `Page::Config` / `Page::WslConfig` 两个变体。v0.4 把它们
    /// 收进这一页的 tab 之后就没**人**能导航到它们了（`ALL` 里没有、界面上
    /// 也不构造），却一直留在枚举里：`nav_id` 与页面分发里各留了两个永远走不到
    /// 的分支。已删。
    AppSettings,
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
    /// ⚠️ 有一页**故意不在这里**：
    ///
    /// - `AddInstance` —— 只能从**实例列表页上那个「添加实例」按钮**进。
    ///   侧边栏同时列「实例列表」和「添加实例」是冗余的：后者本来就是
    ///   前者的一个动作，不是并列的一站。
    ///
    /// 所以 `ALL` 是"侧边栏列哪些页"，**不等于**"有哪些页"。
    /// （`wlsc 配置` / `WSL 配置` 曾经也在"不在这里"之列，但它们现在是
    /// [`SettingsTab`] 的两个 tab、不再是页面了。）
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
            Page::AppSettings | Page::About => "设置",
        }
    }

    /// 这一页是不是需要 `&mut Window` 才能进去。
    ///
    /// 「添加实例」有输入框，而 `InputState::new` 需要 `&mut Window` ——
    /// 所以它必须在**点击导航时**（有 window）把表单建好，
    /// 不能在渲染时建（那会每帧重建一次输入框，打字都打不进去）。
    ///
    /// 「镜像」也一样：它现在有 registry / 仓库名两个输入框**和**一个 tag 下拉框
    /// （`SelectState::new` 同样要 `&mut Window`）。v0.6.2 合并之前这里只有
    /// 「镜像仓库」有输入框；合并之后本地镜像那半页也跟着要 window 了。
    ///
    /// ⚠️ v0.6.3 起这里**只剩「添加实例」**：拉取表单挪进了弹窗，
    /// 是在点「拉取镜像…」那一刻建的（`Shell::open_pull_dialog`）——
    /// 页面本身只剩一个按钮和一张表，进来不需要 window。
    pub fn needs_window_to_enter(self) -> bool {
        matches!(self, Page::AddInstance)
    }
}

/// 「添加实例」页里选中的**来源类型**。
///
/// 只记"用户选了哪一种"；带值的路径 / 名字在输入框里，
/// 拼成 [`wslc_core::model::install::InstallSpec`] 与执行计划是 `app.rs` 的事。
///
/// # 为什么是这三种（原来是五种）
///
/// 原先本地那三条是**分开的三个按钮**（tar / VHDX / `.wsl`），参考实现也这么分
/// （它的 `add.sources.rootfs` 与 `add.sources.vhdx` 是两项，而且**没有** `.wsl`）。
/// 但用户手上拿到的是**一个文件**，"它属于哪一类"不该由他来想 —— 所以合成
/// 一个「本地文件」，由后缀自动分岔（[`wslc_core::model::install::classify_local_file`]）。
///
/// ⚠️ **数据层仍然分三种**（`InstallSource` 的 `Tar` / `Vhdx` / `File`）：
/// 它们在 WSL 那边是三条不同的路（`--import` / `--import --vhd` /
/// `--install --from-file`），合并的只是**界面入口**。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InstallSourceKind {
    /// 本地文件：tar 归档 / `.vhdx` / `.wsl`，按后缀自动分岔。
    #[default]
    Local,
    /// 在线安装（`wsl --install -d`，微软商店或 `--web-download`）。
    Online,
    /// 从镜像站下载 rootfs 再导入（走 `curl.exe`）。
    Mirror,
}

impl InstallSourceKind {
    /// 全部可选值（决定下拉框里的顺序与索引）。
    ///
    /// 顺序：**本地在前，联网的两条在后**；联网那两条里「微软商店」排在
    /// 「镜像源」前面 —— 商店是 WSL 官方那条路，装出来就是官方发行版，
    /// 镜像源是商店/GitHub 都拉不动时的备选。
    pub const ALL: [InstallSourceKind; 3] = [
        InstallSourceKind::Local,
        InstallSourceKind::Online,
        InstallSourceKind::Mirror,
    ];

    /// 下拉框里的文案。
    ///
    /// ⚠️ **措辞是我们自己写的**：参考实现是 GPL-3.0-only、本仓库是 Apache-2.0
    /// （`AGENTS.md` §6），只对齐"有哪几类"，不抄它的字。
    pub fn label(self) -> &'static str {
        match self {
            Self::Local => "本地文件（tar / tar.gz / tar.xz / .vhdx / .wsl）",
            Self::Online => "微软商店 (Microsoft Store)",
            Self::Mirror => "在线发行版（镜像源）",
        }
    }

    /// 一句话说明（显示在来源按钮下面）。
    pub fn hint(self) -> &'static str {
        match self {
            Self::Local => {
                "手上是什么文件都行，按**后缀**自动分岔：tar 归档 → `wsl --import`（只是铺开\
                 文件系统）；`.vhdx` → `--import --vhd`（会拷贝一份，原始文件不动）；\
                 `.wsl` 包 → 交给 WSL 安装器（会做首次启动初始化、建默认用户）。\
                 不联网，最可靠。"
            }
            Self::Online => {
                "走 WSL 官方的在线安装（`wsl --install -d`）：默认从微软商店装，\
                 商店拉不动时可以切到 GitHub（`--web-download`）。"
            }
            Self::Mirror => {
                "从国内镜像站下载官方 rootfs 再导入 —— 商店和 GitHub 都拉不动时用这条，\
                 只用 `curl.exe`，不碰微软的服务器。**自动测速择优**，不用自己挑镜像站。"
            }
        }
    }

    /// 需不需要让用户填一个**文件路径**。
    pub fn needs_path(self) -> bool {
        matches!(self, Self::Local)
    }

    /// 路径输入框的标签。
    pub fn path_label(self) -> &'static str {
        match self {
            Self::Local => "本地文件路径",
            Self::Mirror | Self::Online => "",
        }
    }

    /// 路径输入框的占位提示。
    pub fn path_placeholder(self) -> &'static str {
        match self {
            Self::Local => r"D:\img\ubuntu-rootfs.tar（也可以是 ext4.vhdx 或 Ubuntu-24.04.wsl）",
            Self::Mirror | Self::Online => "",
        }
    }

    /// 文件选择器的类别名与后缀表。
    ///
    /// 返回 `None` 表示这条来源不需要选文件。
    ///
    /// 本地来源只有一条，所以这里的后缀表是**并集**：用户不用先想"我这是哪一类"。
    /// 复合后缀（`.tar.gz`）在 WinForms 的过滤器里写成两个通配就够
    /// （`*.tar.gz` 也能匹配，写上更直观）。
    pub fn file_filter(self) -> Option<(&'static str, &'static [&'static str])> {
        const LOCAL_FILES: &[&str] = &[
            "tar", "gz", "xz", "zst", "bz2", "lz4", "tgz", "txz", "vhdx", "vhd", "wsl",
        ];

        match self {
            Self::Local => Some(("安装文件", LOCAL_FILES)),
            Self::Mirror | Self::Online => None,
        }
    }

    /// 发行版名输入框的**标签**。
    ///
    /// 联网那两条路里它同时是"清单拉不到时手输的 id"，所以标签要长一些 ——
    /// 原先这个函数住在 `views.rs`，而它旁边的 `label()` / `hint()` /
    /// `path_label()` / `name_placeholder()` 全都在这里，漏了它一个。
    pub fn name_field_label(self) -> &'static str {
        if self.is_online() {
            "发行版名（留空 = 用下面选中的那个 id；清单拉不到时也可以直接填，如 Ubuntu-24.04）"
        } else {
            "发行版名"
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

    /// 要不要显示「镜像站」那一块（选发行版 + 自动测速状态 + 自定义 URL）。
    pub fn is_mirror(self) -> bool {
        matches!(self, Self::Mirror)
    }

    /// 要不要显示「在线清单」那一块。
    pub fn is_online(self) -> bool {
        matches!(self, Self::Online)
    }

    /// 要不要显示「本地文件」那一块（路径 + 浏览 + "识别为 …"）。
    pub fn is_local(self) -> bool {
        matches!(self, Self::Local)
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

/// 需要用户二次确认的危险操作（**`wslc.exe` 这一域**）。
///
/// 容器 / 镜像 / 网络 / 卷 / 会话都属于 `wslc`；`wsl.exe` 那一域见
/// [`DistroAction`]。
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
    /// 清理所有**没有被任何容器使用**的卷。
    PruneVolumes,
    /// 终止一个 wslc 会话。
    ///
    /// 会话是 `wslc` 的运行单元：每个会话有自己的 `storage.vhdx`
    /// （镜像 / 容器 / 卷都在里面，见 `wslc_core::storage` 的说明）。
    /// 所以终止它会**连带停掉这个会话里正在运行的容器** —— 这也是它
    /// 需要二次确认的原因。数据不会因此被删（终止的是运行状态，不是文件）。
    TerminateSession {
        /// 会话 ID，对应 `wslc system session terminate <id>`。
        id: u32,
        /// 显示名（`wslc-cli-76434` 这种），只用于文案 —— 只给一个数字
        /// 用户没法确认自己点的是哪一个。
        name: String,
    },
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
            PendingAction::PruneVolumes => "清理未使用的卷",
            PendingAction::TerminateSession { .. } => "终止会话",
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
            PendingAction::PruneVolumes => {
                "将删除所有**没有被任何容器使用**的卷及其中的数据。此操作不可撤销。\
                 正在被使用的卷不受影响；一个都没有时什么也不会发生。"
                    .to_owned()
            }
            PendingAction::TerminateSession { id, name } => format!(
                "将终止会话 {name}（#{id}）。这个会话里**正在运行**的容器会一起停下。\
                 镜像 / 容器 / 卷都在这个会话的 storage.vhdx 里，终止会话不删它们。"
            ),
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
            PendingAction::PruneVolumes => "清理",
            PendingAction::TerminateSession { .. } => "终止会话",
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
            PendingAction::PruneVolumes => {
                let out = cmd::volume::prune(wslc)?;
                Ok(if out.is_empty() {
                    "没有需要清理的卷".to_owned()
                } else {
                    out
                })
            }
            PendingAction::TerminateSession { id, name } => {
                cmd::system::terminate_session(wslc, *id)?;
                Ok(format!("已终止会话：{name}"))
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
    /// 所以给两个明确的动作，让用户自己说清要开还是要关。
    ///
    /// ⚠️ **目前没有界面入口**：v0.4 按需求把发行版详情里那个稀疏开关拿掉了
    /// （理由见 `views::distro_detail_overlay` 里的说明）。日常要开就走
    /// `.wslconfig` 的 `[experimental] sparseVhd=true`（「应用设置 → WSL」），
    /// 装新实例时由安装计划里那一步补上。这个变体和
    /// [`crate::cmd::distro::set_sparse`] 留下来的是**能力**，不是"还有入口"——
    /// 别照着一句过期的注释去界面上找那个按钮。
    SetSparse {
        /// 发行版名。
        name: String,
        /// `true` = 开启稀疏。
        sparse: bool,
    },
}

impl DistroAction {
    /// 这个动作作用在哪个发行版上。
    ///
    /// `ShutdownAll` 返回 `None`：它压的是 `wsl --shutdown`，作用在**所有**
    /// 发行版上，比"动某一个"更粗暴 —— 调用方要单独处理它
    /// （见 [`AppState::long_task`]），别把它当成"碰不到特定的那个"而放行。
    pub fn distro_name(&self) -> Option<&str> {
        match self {
            Self::ShutdownAll => None,
            Self::Terminate(name) | Self::Compact(name) => Some(name),
            Self::Unregister { name, .. } | Self::SetSparse { name, .. } => Some(name),
        }
    }

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
            DistroAction::ShutdownAll => "将关停**所有**发行版和 WSL2 轻量工具虚拟机。\
                 所有发行版里没保存的东西都会丢失。"
                .to_owned(),
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
/// 这几种动作都需要一个**文本参数**（位置 / 大小 / 用户名 / 卷名），
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
    /// 创建一个卷（`wslc volume create <名字>`）。
    ///
    /// ⚠️ 它有两处和前四个都不一样，是**刻意**收进来的：
    /// - 走的是 `wslc.exe` 而不是 `wsl.exe`（前四个全是发行版动作）；
    /// - 它**不属于任何发行版** —— 卷是全局的，所以弹窗里那一行
    ///   「发行版：…」不画（`TextPrompt::distro` 是空串）。
    ///
    /// 收进来而不是另做一个弹窗：外形完全一致（一个标签 + 一个输入框 +
    /// 一句说明 + 等效命令预览），另做一份就是把这套布局抄第二遍。
    CreateVolume,
}

impl PromptKind {
    /// 弹窗标题。
    pub fn title(self) -> &'static str {
        match self {
            Self::MoveDistro => "移动安装位置",
            Self::ResizeDistro => "调整磁盘大小",
            Self::SetDefaultUser => "设置默认用户",
            Self::ExportDistro => "导出发行版",
            Self::CreateVolume => "创建卷",
        }
    }

    /// 输入框标签。
    pub fn label(self) -> &'static str {
        match self {
            Self::MoveDistro => "新的安装目录（绝对路径）",
            Self::ResizeDistro => "新的磁盘大小",
            Self::SetDefaultUser => "用户名",
            Self::ExportDistro => "导出到（绝对路径，要含文件名）",
            Self::CreateVolume => "卷名",
        }
    }

    /// 输入框占位提示。
    pub fn placeholder(self) -> &'static str {
        match self {
            Self::MoveDistro => r"D:\wsl\MyDistro",
            Self::ResizeDistro => "50GB",
            Self::SetDefaultUser => "myuser",
            Self::ExportDistro => r"D:\backup\MyDistro.tar",
            Self::CreateVolume => "webdata",
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
            Self::CreateVolume => {
                "命名卷：名字由你指定，容器里用 `-v 名字:/路径` 挂载它，\
                 数据存在这个会话的 storage.vhdx 里。\
                 名字重复时会失败 —— wslc 不会替你改名或复用。"
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
            Self::CreateVolume => "创建",
        }
    }

    /// 这一种要不要选路径；要的话是**文件**还是**目录**。
    ///
    /// 不是每种都有 —— 「调整大小」填的是 `50GB` 这种数字，
    /// 「设置默认用户」填的是用户名，「创建卷」填的是卷名，
    /// 给它们一个「浏览…」按钮只会让人困惑。
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
            Self::ResizeDistro | Self::SetDefaultUser | Self::CreateVolume => None,
        }
    }
}

/// 卷名能不能直接用。
///
/// # 只挡一类：以 `-` 开头的名字
///
/// 卷名会被当成 `wslc volume create` 的**裸 argv**。`wslc volume create --help`
/// 里的 `--help` 会被解析成选项而不是卷名 —— 用户以为建了个卷，
/// 实际什么都没建（或者某个开关被静默生效）。这类"看着成功其实没做"
/// 是最难查的，所以在**本地**先挡掉，不白起一个进程。
///
/// # 别的规则**故意**不自己编
///
/// 字符集、长度、大小写这些 `wslc` 自己的规则没在真机上测过
/// （采集环境里一个卷都没有，见 `model/volume.rs` 的说明）。
/// 自己编一套只会把合法名字挡在外面 —— 那是比"让 wslc 报错"更糟的结果：
/// 用户找不到任何理由。
pub fn validate_volume_name(name: &str) -> std::result::Result<(), &'static str> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("卷名不能为空");
    }
    if trimmed.starts_with('-') {
        return Err("卷名不能以 - 开头（会被 wslc 当成选项，而不是名字）");
    }
    Ok(())
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
    /// 在新终端窗口里打开容器的交互式 shell（`wslc exec -it <容器> /bin/sh`）。
    ///
    /// 交互式程序不能内嵌进 GPUI，也不该被捕获输出 —— 和
    /// [`ImmediateAction::OpenDistroTerminal`] 是同一个理由。
    OpenContainerShell(String),
    /// 在新终端窗口里 attach 到容器主进程（`wslc attach <容器>`）。
    AttachContainer(String),
    /// 在新终端窗口里跟随容器日志（`wslc logs --follow`）。
    ///
    /// 不带行数 / 时间戳参数：那两项从 `AppState::logs` 里现读
    /// （按钮就在日志浮层上，用户刚选的就是他要的）。
    FollowContainerLogs(String),
}

/// 「容器日志」浮层的状态。
///
/// 纯数据（不含任何 GPUI 类型），所以住在 `AppState` 里 —— 因此它也能被
/// 测试。这一屏只有按钮没有输入框，所以 `Shell` 那边一个字段都不用加。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogsState {
    /// 看的是哪个容器。
    pub container: String,
    /// 只取末尾多少行；`None` = 全部。
    ///
    /// 默认**不是** `None`：大容器"全部"可能是几百 MB，而且是每一轮
    /// 切档位都要重新拉一遍。
    pub tail: Option<u32>,
    /// 要不要时间戳。
    pub timestamps: bool,
    /// 正在加载（后台那条 `wslc logs` 还没回来）。
    pub loading: bool,
    /// 拿到的日志原文；还没加载过时是 `None`（和"加载出来是空的"区分开）。
    pub text: Option<String>,
    /// 上一次加载的错误。
    pub error: Option<String>,
    /// 已经发起了多少轮加载。用来**丢弃过期响应**，见 [`LogsState::begin_load`]。
    load_seq: u64,
}

impl LogsState {
    /// 可选的"末尾多少行"档位。`None` 放在最后：它是代价最大的那个。
    pub const TAILS: [Option<u32>; 4] = [Some(200), Some(1000), Some(5000), None];

    /// 默认档位。
    const DEFAULT_TAIL: Option<u32> = Some(200);

    /// 打开日志浮层时的初始状态。
    pub fn new(container: impl Into<String>) -> Self {
        Self {
            container: container.into(),
            tail: Self::DEFAULT_TAIL,
            timestamps: false,
            loading: false,
            text: None,
            error: None,
            load_seq: 0,
        }
    }

    /// 这一档在界面上的名字。
    pub fn tail_label(tail: Option<u32>) -> String {
        match tail {
            Some(n) => format!("{n} 行"),
            None => "全部".to_owned(),
        }
    }

    /// 交给 `cmd::container::logs` 的选项。
    ///
    /// 单独一个方法而不是在调用处现拼：`tail` / `timestamps` 各对应一个
    /// 界面控件，映射关系写错只表现为"日志内容不对"（少了行、时间戳没出来），
    /// 是最不容易被发现的那类错。
    pub fn options(&self) -> wslc_core::cmd::container::LogOptions {
        wslc_core::cmd::container::LogOptions {
            tail: self.tail,
            timestamps: self.timestamps,
            // 「详细」目前没有界面控件。开着它会让输出里多出 `wslc` 自己的
            // 元信息行 —— 想加控件时再打开，别默认打开。
            details: false,
        }
    }

    /// 开始一轮加载，返回这一轮的**票据**。
    ///
    /// # 为什么需要票据
    ///
    /// 用户连点「1000 行」→「全部」会有两个请求在飞，**先发的可能后回**。
    /// 不认票据的话，后到的旧响应会把新结果覆盖掉 —— 表现为
    /// "选了 5000 行，显示的却是 1000 行的内容"。
    pub fn begin_load(&mut self) -> u64 {
        self.load_seq += 1;
        self.loading = true;
        self.error = None;
        self.load_seq
    }

    /// 这一轮的响应还作数吗？（期间没有发起更新的加载才作数）
    pub fn accepts(&self, ticket: u64) -> bool {
        self.load_seq == ticket
    }

    /// 收下一轮加载的结果；过期的票据会被丢掉并返回 `false`。
    pub fn finish_load(
        &mut self,
        ticket: u64,
        result: std::result::Result<String, String>,
    ) -> bool {
        if !self.accepts(ticket) {
            return false;
        }
        self.loading = false;
        match result {
            Ok(text) => {
                self.text = Some(text);
                self.error = None;
            }
            Err(e) => self.error = Some(e),
        }
        true
    }
}

/// 「容器 exec」浮层的状态。
///
/// 输入框（`Entity<InputState>`）不在这里 —— 它是 GPUI 类型，住在
/// `Shell::exec_input` 里；这里只放"跑过什么、结果是什么"。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExecState {
    /// 在哪个容器里执行。
    pub container: String,
    /// 正在执行（那条 `wslc exec` 还没回来）。
    pub running: bool,
    /// 上一次执行的**原文**（界面上回显，让用户知道这份输出是哪条命令的）。
    pub last_command: String,
    /// 输出；还没跑过时是 `None`。
    pub output: Option<String>,
    /// 上一次的错误。
    pub error: Option<String>,
}

impl ExecState {
    /// 打开 exec 浮层时的初始状态。
    pub fn new(container: impl Into<String>) -> Self {
        Self {
            container: container.into(),
            ..Default::default()
        }
    }
}

/// 把用户敲的一行命令变成 `wslc exec` 的 argv。
///
/// # 为什么走 `/bin/sh -c` 而不是自己按空格切
///
/// - 按空格切的话 `ls -la /tmp | head` 会被当成一串参数丢给 `ls` ——
///   管道没生效，但命令**看起来成功返回了**，这种"看着对、结果不对"
///   是最难查的一类；
/// - 用容器面板的人默认预期能写管道、重定向、`&&`（`docker exec` 就是这样）；
/// - `/bin/sh` 是 POSIX 保证存在的（busybox / dash / bash-as-sh 都算），
///   而 `/bin/bash` 在 alpine 这类镜像里**没有**，用它会让一部分容器点下去就报错。
///
/// 代价要说清：这样一来 `;` `&&` `$()` 全都能用，也就是**等于把整个
/// shell 交给用户**。这不是漏洞 —— 用户本来就能在自己的机器上跑这些；
/// 面板要防的是"参数被当成选项"（那是容器名那一侧的事），
/// 而不是拦用户自己想执行的命令。
pub fn exec_argv(text: &str) -> Vec<String> {
    vec!["/bin/sh".to_owned(), "-c".to_owned(), text.to_owned()]
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

    /// 给一个"家族名"（比如 `Ubuntu`）找一个**带版本号的兄弟 id**（比如 `Ubuntu-26.04`）。
    ///
    /// # 为什么需要它（实测 2026-10-11，本机）
    ///
    /// `wsl -l -o` 把裸名字（`Ubuntu`）排在列表**第一位**，而它是清单里那条**旧的
    /// 商店包**（`PackageFamilyName = CanonicalGroupLimited.Ubuntu_79rhkp1fndgsc`，
    /// 而且清单里它的 `Amd64Url`/`Arm64Url` 都是空的）：
    ///
    /// | 命令 | 结果 |
    /// |---|---|
    /// | `wsl --install -d Ubuntu --no-launch` | **2~5 秒失败**：`Wsl/InstallDistro/WININET_E_CANNOT_CONNECT` |
    /// | `wsl --install -d Ubuntu-24.04 --no-launch` | 正常下载 |
    /// | `wsl --install -d Debian --no-launch` | 正常下载 |
    ///
    /// 同一分钟、同一台机器、同一个 `wsl.exe` —— 只有 id 不同。参考实现的列表第一项是
    /// 「Ubuntu 26.04 LTS」（传的是 `Ubuntu-26.04`），所以它一直没事；我们照搬
    /// `wsl -l -o` 的顺序，用户一上手就正好点在这个坏条目上。
    ///
    /// 规则：取 `<id>-…` 里版本号最大的那个（`Ubuntu-26.04` > `Ubuntu-24.04`）。
    /// 没有兄弟（`Debian`、`kali-linux`）就返回 `None`，绝不硬猜。
    ///
    /// # `taken`：**已经装过**的发行版名，必须排除
    ///
    /// 本机就是现成的例子：清单第一项是裸 `Ubuntu`，它的兄弟是 `Ubuntu-26.04`，
    /// 而那个**已经装在 `D:\linux\Ubuntu-26.04` 了**。换过去只会立刻撞上 WSL 的
    /// `Wsl/InstallDistro/ERROR_ALREADY_EXISTS`（实测 2026-10-12：对已安装的 id
    /// 跑 `wsl --install -d <id> --no-launch`，2 秒返回 -1，"当文件已存在时，
    /// 无法创建该文件"）。那次失败对用户毫无帮助 —— 不如**一开始就不换**
    /// （返回 `None`），让"这条来源依赖 wsl.exe 自己的网络，改用国内镜像源"
    /// 那句提示照常出现。
    pub fn sibling_id(&self, id: &str, taken: &[String]) -> Option<String> {
        let prefix = format!("{id}-");
        self.items
            .iter()
            .filter(|item| item.id.starts_with(&prefix) && item.id.len() > prefix.len())
            .filter(|item| {
                !taken
                    .iter()
                    .any(|existing| wslc_core::model::distro::same_name(existing, &item.id))
            })
            .max_by(|left, right| version_key(&left.id).cmp(&version_key(&right.id)))
            .map(|item| item.id.clone())
    }
}

/// 把一个 id 里的数字段抽出来做排序键（`Ubuntu-26.04` → `[26, 4]`）。
///
/// 只用于"哪个版本更新"这种比较：数字段按数值比、段数多的更大，
/// 没有数字的 id（`Ubuntu`、`kali-linux`）得到空键 —— 排在最后，不会被选成兄弟。
fn version_key(id: &str) -> Vec<u32> {
    id.split(|c: char| !c.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse::<u32>().ok())
        .collect()
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

/// 「在线发行版（镜像源）」那一块的状态。
///
/// 清单是**动态拉的**（`wslc_core::cmd::catalog`，两个 HTTP 请求走 `curl.exe`），
/// 所以这里存的是"拉回来的清单 + 探测结果"，而不是写死的表。
///
/// `Default` 直接派生：手写的那份每个字段都是"该类型的默认值"
/// （`Vec::new()` / `String::new()` / `None` / `false`），逐字段写一遍
/// 只是给了日后**加字段时漏写一个**的机会（clippy 的 `derivable_impls`）。
#[derive(Debug, Clone, Default, PartialEq)]
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

impl MirrorState {
    /// 新建。
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前选中的那一条（没显式选过就是第一条）。
    pub fn selected_offer(&self) -> Option<&Offer> {
        if !self.distro_id.trim().is_empty() {
            if let Some(found) = self
                .offers
                .iter()
                .find(|offer| offer.id() == self.distro_id)
            {
                return Some(found);
            }
        }
        self.offers.first()
    }

    /// 换一条清单：先按参考实现的顺序排，再清掉上一次的探测结果，
    /// 并在原来选的那条没了时回落到第一条。
    pub fn set_offers(&mut self, mut offers: Vec<Offer>, origin: String, updated: Option<String>) {
        sort_offers_like_reference(&mut offers);

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

    /// 按耗时排序：可用的在前（快的更前），连不上的 / 报错的统一排到后面。
    ///
    /// 界面上那一列是"哪个镜像快"，而探测是按**清单顺序**发起的 ——
    /// 不排一下，最快的那条会散落在中间（`chosen` 有徽标，但用户想比较
    /// 其余几条时就只能自己找）。
    ///
    /// ⚠️ **不能只按 `secs` 排**：探测没跑成的候选（`code == 0` 且
    /// `secs == 0.0`，见 `app::Shell::probe_mirrors` 里那个 `unwrap_or`）
    /// 会因此**排在最前面**，看起来像"最快的"。
    /// 用 `total_cmp` 而不是 `partial_cmp().unwrap()`：`secs` 是 f64，
    /// 万一出现 NaN，`unwrap()` 会让界面崩在"探测"这个动作上。
    pub fn sort_results_by_speed(&mut self) {
        self.results.sort_by(|left, right| {
            right
                .is_ok()
                .cmp(&left.is_ok())
                .then_with(|| left.secs.total_cmp(&right.secs))
        });
    }

    /// 按**微软商店清单里的 id**去镜像清单里找同一个发行版。
    ///
    /// 用途：商店那条路装不上时（`wsl.exe` 自己的网络不通）一键换成镜像源，
    /// 并且**尽量落在同一个发行版**上，不让用户自己在清单里翻一遍。
    ///
    /// 三轮匹配（都是"只比字母数字、忽略大小写与分隔符"）：
    ///
    /// 1. `名字 + 版本` 拼起来对上 —— `Ubuntu-24.04` ↔ `Ubuntu` + `24.04`；
    /// 2. 只对上名字 —— 在线 id 常常不带版本（`Debian`、`Ubuntu`）；
    /// 3. 名字**前缀**对上 —— 两边的叫法有出入时兜底，实测这几对就是靠它：
    ///    `kali-linux` ↔ `Kali`、`FedoraLinux-44` ↔ `Fedora 44`、
    ///    `archlinux` ↔ `Arch Linux`、`OracleLinux_9_5` ↔ `Oracle Linux`。
    ///
    /// 第 3 轮故意做得"宽"（可能落到同系列的另一个版本上）——反正选完能在界面上
    /// 看到是哪个版本，比"什么都不选、让用户自己去翻"好。找不到返回 `None`。
    pub fn match_online_id(&self, online_id: &str) -> Option<String> {
        let want = normalize_id(online_id);
        if want.is_empty() {
            return None;
        }

        if let Some(offer) = self
            .offers
            .iter()
            .find(|offer| normalize_id(&format!("{}{}", offer.name, offer.version)) == want)
        {
            return Some(offer.id());
        }
        if let Some(offer) = self
            .offers
            .iter()
            .find(|offer| normalize_id(&offer.name) == want)
        {
            return Some(offer.id());
        }
        self.offers
            .iter()
            .filter_map(|offer| {
                let name = normalize_id(&offer.name);
                // 至少 4 个字符才敢做前缀匹配（`kali` 是 4，`or`/`os` 这种太容易误伤）
                let hit = name.len() >= 4 && (want.starts_with(&name) || name.starts_with(&want));
                hit.then(|| (name.len(), offer.id()))
            })
            .max_by_key(|(len, _)| *len)
            .map(|(_, id)| id)
    }
}

/// 只留字母数字、转小写 —— 用来比较"两个来源里的同一版发行版"。
///
/// `Ubuntu-24.04` / `Ubuntu 24.04` / `ubuntu24.04` 都归一成 `ubuntu2404`。
fn normalize_id(text: &str) -> String {
    text.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// 发行版的**常用度**排序键（越小越靠前）。
///
/// ⚠️ 这张表的顺序是照参考实现（`wsl-dashboard-ref` 的 `get_distro_sort_rank`）
/// 写的：用户明确要求"在线发行版的清单照 wsl-dashboard 来"，而它把
/// Ubuntu / Debian 排在最前、没列到的排最后。
///
/// 代码是我们自己写的：它是 **GPL-3.0-only**、本仓库 Apache-2.0
/// （`AGENTS.md` §6），能对齐的只有"哪几个发行版常用"这个**事实**。
fn distro_rank(name: &str) -> usize {
    let lower = name.to_lowercase();
    for (needle, rank) in [
        ("ubuntu", 1),
        ("debian", 2),
        ("kali", 3),
        ("arch", 4),
        ("fedora", 5),
        ("opensuse", 6),
        // 参考实现把 SLES 也单列一档（`sles` 是它的缩写）
        ("suse linux enterprise", 7),
        ("sles", 7),
        ("almalinux", 8),
        ("rocky", 9),
        ("oracle", 10),
        ("alpine", 11),
        ("openeuler", 12),
        ("mint", 13),
    ] {
        if lower.contains(needle) {
            return rank;
        }
    }
    999
}

/// 按参考实现的顺序排：先常用度，同族再按**版本倒序**（新的在前）。
///
/// ⚠️ 版本这里按**数值**比，而不是照参考实现那样按 "名字+版本" 字符串倒序：
/// 同一族里混着 `16.0` / `15.6` / `tumbleweed` 这种，字符串倒序会把
/// `tumbleweed` 排到 `16.0` 前面（`t` > `1`），而数值比把"没有数字的"排最后
/// —— 滚动版放最后才符合直觉。同族里数值键相同的（都是滚动版）再按名字排，
/// 保证顺序**确定**（否则每次刷新清单的行序都可能不一样）。
fn sort_offers_like_reference(offers: &mut [Offer]) {
    offers.sort_by(|left, right| {
        let (left_rank, right_rank) = (distro_rank(&left.name), distro_rank(&right.name));
        if left_rank != right_rank {
            return left_rank.cmp(&right_rank);
        }
        version_key(&right.version)
            .cmp(&version_key(&left.version))
            .then_with(|| left.version.cmp(&right.version))
    });
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
    /// 发行版名（用户想要的**最终**名字）。
    pub name: String,
    /// 装的时候**先**注册成的那些名字（在线安装的源 id，以及失败时换的那个兄弟 id）。
    ///
    /// `wsl -l -v` 上先是它们，重定位之后才变成 [`Self::name`]。
    /// [`AppState::busy_with`] 必须两个都认 —— 否则改名安装的中间态里，
    /// 列表上那一行（源名）不受任何互斥保护，而重定位正是
    /// 「导出 → 注销 → 导入」那段不可逆的操作。
    /// 来源见 `wslc_core::model::install::InstallPlan::source_names`。
    pub source_names: Vec<String>,
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
    ///
    /// `source_names` 是"装的时候会先注册成哪些名字"（见 [`Self::source_names`]）——
    /// 由 `InstallPlan::source_names()` 算好传进来，这里不再猜。
    pub fn new(name: impl Into<String>, source_names: Vec<String>) -> Self {
        Self {
            name: name.into(),
            source_names,
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
                let changed =
                    self.have != *have || self.total_bytes != *total || self.step_secs != *secs;
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

/// `/etc/wsl.conf` 表单里某个字段的**稳定元素 id**。
///
/// 用 `match` 而不是 `format!`：`Input::id` / `Checkbox::new` 要的是
/// `&'static str`，而且 GPUI 的交互元素本来就要求 id 稳定。
///
/// ⚠️ 这个 match 对应的是**表驱动**的 [`wslc_core::model::wslconf::FIELDS`]，
/// 所以往那张表里加字段时**很容易忘了在这里加一行**。后果不是编译错误：
/// 新字段会静默拿到 `"wslconf-unknown"`；两个字段都忘了加就**撞同一个 id**，
/// 而 GPUI 里 id 撞了会让勾选 / 焦点状态互相串。
/// 测试 `every_wslconf_field_has_its_own_id` 就是盯这件事的。
///
/// （它原先住在 `views.rs`。搬过来的理由见 `AGENTS.md` §2：
/// 这是纯函数，而放在那边就永远没有测试 —— CI 不跑 `wslc-panel` 的单测。）
pub fn wslconf_field_id(section: &str, key: &str) -> &'static str {
    match (section, key) {
        ("automount", "enabled") => "wslconf-automount-enabled",
        ("automount", "mountFsTab") => "wslconf-automount-mountfstab",
        ("automount", "root") => "wslconf-automount-root",
        ("automount", "options") => "wslconf-automount-options",
        ("network", "generateHosts") => "wslconf-network-generatehosts",
        ("network", "generateResolvConf") => "wslconf-network-generateresolvconf",
        ("network", "hostname") => "wslconf-network-hostname",
        ("interop", "enabled") => "wslconf-interop-enabled",
        ("interop", "appendWindowsPath") => "wslconf-interop-appendwindowspath",
        ("user", "default") => "wslconf-user-default",
        ("boot", "systemd") => "wslconf-boot-systemd",
        ("boot", "command") => "wslconf-boot-command",
        ("boot", "protectBinfmt") => "wslconf-boot-protectbinfmt",
        ("gpu", "enabled") => "wslconf-gpu-enabled",
        ("time", "useWindowsTimezone") => "wslconf-time-usewindowstimezone",
        _ => "wslconf-unknown",
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
    /// 正在**保存**（写 `/etc/wsl.conf`）。
    ///
    /// ⚠️ 只有保存时会置位 —— 打开弹窗时读文件那一趟**没有**置位
    /// （原先这里的注释写着"正在读或者正在写"，与实现不符）。
    /// 界面据此把两个保存按钮换成一行「正在保存…」，见 `views::wslconf_overlay`。
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

/// 「镜像」页**远端那一半**的状态（v0.6 建，v0.6.2 并进「镜像」页）。
///
/// 全是纯数据 —— registry / 仓库名两个输入框和 tag 下拉框是
/// `Entity<InputState>` / `Entity<SelectState<_>>`，留在 `wslc-panel` 的 `Shell` 里
/// （和「添加实例」页同一个分工）。
///
/// # 为什么要分开记 `repository` 与 `loaded`
///
/// 用户从 `nginx` 换到 `redis` 时，网络那 1~2 秒里**结果还是 nginx 的**。
/// 如果界面只认一个仓库名，那段时间就会把 nginx 的 tag 挂在 redis 的标题下 ——
/// 而这恰恰是用户最容易误判成"redis 有这些 tag"的时刻（串台）。
/// 所以：`repository` 是"正在查谁"，`loaded` 是"列表里的东西是谁的"，
/// 两者不等时界面显示"正在查 X"，不显示旧结果（[`RegistryState::begin_query`] 会清）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryState {
    /// registry 地址（**用户可编辑**，见 [`registry::is_valid_host`]）。
    ///
    /// v0.6.2 之前这是个常量：合并两页时它必须变成字段，否则"从别的 registry 拉"
    /// 这个能力就没了（当初正是为了它才留着两个拉取入口，见
    /// `docs/PLAN-v0.6.md` §3.6）。
    pub host: String,
    /// 用户输进框里的原文（用来显示"由 … 解析为 …"）。
    pub input: String,
    /// 正在查的仓库名（**规范化之后**的，`alpine` → `library/alpine`）。
    pub repository: String,
    /// 上一次查成功、且列表里正显示的那个仓库名。
    pub loaded: Option<String>,
    /// tag 列表（已排序：`latest` 优先、版本倒序）——
    /// 整个列表都会喂给 tag 下拉框：它的列表是**虚拟化**的，
    /// 所以实测里 `node` 那种 9170 个 tag 也不会每帧建 9170 个元素
    /// （v0.6.2 之前是"本地过滤 + 只渲染前 200 行"）。
    pub tags: Vec<String>,
    /// 正在查 tag。
    pub loading: bool,
    /// 查 tag 失败的原因（能给用户看的一句话）。
    pub error: Option<String>,
    /// 选中的 tag（下拉框点出来的）。
    pub selected: Option<String>,
    /// **用户输入里自带的** tag（`nginx:1.27` 的 `1.27`）。
    ///
    /// 为什么要有它：tags 列表拉不到时（别的 registry 的 `tags/list` 可能要认证，
    /// 实测 daocloud 就 401）下拉框里一个 tag 都没有，但用户其实已经把 tag
    /// 写在输入里了 —— 那就该能拉。它是 [`RegistryState::effective_tag`] 的兜底，
    /// 也是"按完整引用拉取"这条能力在合并之后唯一的落点。
    pub input_tag: Option<String>,
    /// 选中 tag 的下载体积（字节，**压缩后**）。
    pub size: Option<u64>,
    /// 选中 tag 的层数。
    pub layers: Option<usize>,
    /// 正在查体积。
    pub sizing: bool,
    /// 查体积失败的原因。
    pub size_error: Option<String>,
}

impl Default for RegistryState {
    fn default() -> Self {
        Self::new()
    }
}

impl RegistryState {
    /// 初始状态：registry 是内置那个（用户可改），其余全是空的。
    pub fn new() -> Self {
        Self {
            host: registry::DEFAULT_REGISTRY.to_owned(),
            input: String::new(),
            repository: String::new(),
            loaded: None,
            tags: Vec::new(),
            loading: false,
            error: None,
            selected: None,
            input_tag: None,
            size: None,
            layers: None,
            sizing: false,
            size_error: None,
        }
    }

    /// 开始查一个仓库：**先把上一次的结果清干净**。
    ///
    /// ⚠️ 这一步不能省，理由见类型文档（换仓库时串台）。
    ///
    /// `input_tag` 是输入里自带的 tag（没有就 `None`）——
    /// 查询回来之后由调用方拿它选中（见 [`RegistryState::effective_tag`]）。
    pub fn begin_query(
        &mut self,
        host: &str,
        input: &str,
        repository: &str,
        input_tag: Option<String>,
    ) {
        self.host = host.to_owned();
        self.input = input.to_owned();
        self.repository = repository.to_owned();
        self.clear_results();
        self.input_tag = input_tag;
        self.loading = true;
    }

    /// 清掉"上一次查询的结果"，但保留 registry 与用户输入。
    pub fn clear_results(&mut self) {
        self.tags.clear();
        self.loaded = None;
        self.error = None;
        self.selected = None;
        self.input_tag = None;
        self.size = None;
        self.layers = None;
        self.size_error = None;
        self.sizing = false;
    }

    /// 查 tag 成功。
    pub fn set_tags(&mut self, repository: &str, tags: Vec<String>) {
        self.repository = repository.to_owned();
        self.loaded = Some(repository.to_owned());
        self.loading = false;
        self.error = None;
        self.tags = tags;
        // 换了仓库，选中的那个 tag 就不属于新列表了。
        self.selected = None;
        self.size = None;
        self.layers = None;
        self.size_error = None;
        self.sizing = false;
    }

    /// 查 tag 失败。结果一并清掉 —— 报错的同时还留着一列旧 tag，
    /// 用户会以为那些是"这次查到的"。
    pub fn set_error(&mut self, message: String) {
        self.loading = false;
        self.sizing = false;
        self.error = Some(message);
        self.tags.clear();
        self.loaded = None;
        self.selected = None;
        self.size = None;
        self.layers = None;
        self.size_error = None;
    }

    /// 选中一个 tag。体积要**重新查**，所以先把旧的那个清掉。
    pub fn select_tag(&mut self, tag: &str) {
        self.selected = Some(tag.to_owned());
        self.size = None;
        self.layers = None;
        self.size_error = None;
        self.sizing = true;
    }

    /// 体积到手。
    pub fn set_size(&mut self, bytes: u64, layers: usize) {
        self.size = Some(bytes);
        self.layers = Some(layers);
        self.sizing = false;
        self.size_error = None;
    }

    /// 体积查失败（tag 本身还是选中的，只是量不出来）。
    pub fn set_size_error(&mut self, message: String) {
        self.sizing = false;
        self.size = None;
        self.layers = None;
        self.size_error = Some(message);
    }

    /// 要拉的那个 tag：**下拉框选的优先，其次用输入里自带的那个**。
    ///
    /// 兜底的用途见 [`RegistryState::input_tag`]：别的 registry 的 tag 列表
    /// 可能拉不到（要认证），那时用户写在输入里的 `:tag` 是唯一的凭据。
    pub fn effective_tag(&self) -> Option<&str> {
        self.selected.as_deref().or(self.input_tag.as_deref())
    }

    /// 将要拉取的完整镜像引用（`docker.1panel.live/library/alpine:latest`）。
    ///
    /// 没有 tag（既没选、输入里也没写）或还没查过仓库就是 `None` ——
    /// 「拉取」按钮据此决定渲不渲染。
    ///
    /// ⚠️ 必须带 registry 前缀：不带的话 `wslc pull` 会去 Docker Hub 找，
    /// 而那台机器上 `registry-1.docker.io` 不可达（实测 000）。
    pub fn selected_reference(&self) -> Option<String> {
        let tag = self.effective_tag()?;
        let repository = self.loaded.as_deref().unwrap_or(self.repository.as_str());
        if repository.is_empty() || self.host.is_empty() {
            return None;
        }
        Some(registry::pull_reference(&self.host, repository, tag))
    }

    /// 一句话说明"这个仓库名是怎么来的"—— 原文与规范化结果**不同**时才给。
    pub fn normalization_note(&self) -> Option<String> {
        let input = self.input.trim();
        if input.is_empty() || input == self.repository {
            return None;
        }
        Some(format!("由 {input} 解析为 {}", self.repository))
    }

    /// 体积说人话：`下载约 3.9 MB（1 层）`。
    ///
    /// ⚠️ 措辞必须是"**下载约**"：这是 manifest 累加的**压缩后**大小，
    /// 而「镜像」页里 `wslc images` 的 `Size` 是解压后占用的（同机 nginx 实测
    /// 60.9 MB vs 163 MB，差 2.7 倍）。写成"大小"用户一对就会当 bug 报。
    pub fn size_label(&self) -> Option<String> {
        let bytes = self.size?;
        let layers = self.layers.unwrap_or(0);
        Some(format!(
            "下载约 {}（{layers} 层）",
            mirrors::human_bytes(bytes)
        ))
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
    /// 系统的**应用主题**是不是深色（None = 没读到，按深色算）。
    ///
    /// 读一次是 7~27 ms（起 `reg.exe`，见 `wslc_core::cmd::win_theme`），
    /// 所以**启动时读一次存这儿**，界面上的「跟随系统」用缓存值：
    /// 点按钮是纯逻辑，不会因为读注册表把界面卡一下。
    /// 点「跟随系统」时会顺手在后台再读一次（系统主题可能刚改过）。
    pub system_is_dark: Option<bool>,
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
    /// 容器日志浮层；关闭时为 `None`。
    ///
    /// 放在 `AppState`（而不是 `Shell`）是因为它**全是纯数据** ——
    /// 这一屏只有按钮没有输入框，所以没有 `Entity<InputState>` 要存。
    pub logs: Option<LogsState>,
    /// 容器 exec 浮层；关闭时为 `None`。
    ///
    /// 命令文本本身在 `Shell::exec_input` 里（那是 GPUI 类型），
    /// 这里只放"跑过什么、结果是什么"。
    pub exec: Option<ExecState>,
    /// 在线可安装发行版的清单状态。
    pub online: OnlineDistroState,
    /// 「在线发行版（镜像源）」那一块的状态（动态清单 + 探测结果 + 选中的那个）。
    pub mirrors: MirrorState,
    /// 「镜像」页里**远端那一半**的状态（v0.6 建，v0.6.2 并进「镜像」页：
    /// 浏览远端 registry 的 tag → 拉取）。
    ///
    /// 和 [`AppState::mirrors`] 不是一回事：那个是 WSL **发行版**的 rootfs 文件，
    /// 这个是容器**镜像**仓库（Registry V2）。
    pub registry: RegistryState,
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
            // 真正的值由 `main.rs` 在开窗之前填（那里才知道要不要读注册表）
            system_is_dark: None,
            busy: false,
            pulling: None,
            exporting: None,
            kept_alive: Vec::new(),
            wslconf: None,
            settings_tab: SettingsTab::default(),
            confirm: None,
            toast: None,
            installing: None,
            logs: None,
            exec: None,
            online: OnlineDistroState::new(),
            mirrors: MirrorState::new(),
            registry: RegistryState::new(),
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

    /// 有长任务正在**动这个发行版**吗？有就返回它在干什么（`"安装"` / `"导出"`）。
    ///
    /// # 为什么需要它
    ///
    /// 安装和「终止 / 删除 / 压缩 / 导出 / 移动」之间原先**没有任何互斥**。
    /// 安装的重定位阶段是「导出 → 注销 → 导入」，而 `cmd::install` 自己的
    /// 注释写着"中途停下等于把刚装好的删了" —— 这时对**同一个**发行版点一下
    /// 终止或导出就能把它打断，代价是不可逆的。
    ///
    /// 名字比较用 [`wslc_core::model::distro::same_name`]：安装时源名与用户
    /// 想要的名字可能只差大小写，用 `==` 会漏掉。
    ///
    /// ⚠️ **两个名字都要查**（`name` + `source_names`）：改名安装的中间态里
    /// `wsl -l -v` 上是**源名**，只查最终名字等于那段时间完全没保护
    /// —— 而重定位恰好就在那段时间里跑。见 [`InstallProgress::source_names`]。
    pub fn busy_with(&self, distro: &str) -> Option<&'static str> {
        // 这里用 `is_some_and` 而不是嵌套的 `if let`：后者会被 clippy 的
        // `collapsible_if` 建议改成 let-chain，而 let-chain 要 Rust 1.88，
        // 本 crate 声明的 MSRV 是 1.85（见 `Cargo.toml` 的 `rust-version`）。
        let installing = self.installing.as_ref().is_some_and(|install| {
            install.is_running()
                && (wslc_core::model::distro::same_name(&install.name, distro)
                    || install
                        .source_names
                        .iter()
                        .any(|name| wslc_core::model::distro::same_name(name, distro)))
        });
        if installing {
            return Some("安装");
        }
        let exporting = self
            .exporting
            .as_ref()
            .is_some_and(|export| wslc_core::model::distro::same_name(&export.name, distro));
        if exporting {
            return Some("导出");
        }
        None
    }

    /// 有**任何**长任务在跑吗？返回一句话说明在干什么。
    ///
    /// 「关停全部」（`wsl --shutdown`）要用它：那个动作压的是整个 WSL 服务，
    /// 会把安装 / 导出正在跑的 `wsl.exe` 一起掐掉 —— 它不是"动某一个发行版"，
    /// 所以 [`AppState::busy_with`] 拦不住它。
    pub fn long_task(&self) -> Option<String> {
        if let Some(install) = self.installing.as_ref().filter(|i| i.is_running()) {
            return Some(format!("正在安装 {}", install.name));
        }
        if let Some(export) = self.exporting.as_ref() {
            return Some(format!("正在导出 {}", export.name));
        }
        if let Some(pull) = self.pulling.as_ref() {
            return Some(format!("正在拉取 {}", pull.image));
        }
        None
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
    fn every_wslconf_field_has_its_own_id() {
        // 这条测试是 `wslconf_field_id` 那个 match 的守卫：往
        // `wslconf::FIELDS` 里加字段却忘了在那里加一行时，它会先炸 ——
        // 否则新字段会静默拿到 `"wslconf-unknown"`；两个字段都忘了加就
        // **撞同一个 id**，而 GPUI 里 id 撞了会让勾选 / 焦点状态互相串。
        //
        // 这也是当初把它从 `views.rs` 搬过来的主要收益：CI 不跑
        // `wslc-panel` 的单测，那种测试放在那边等于不存在。
        let mut seen: Vec<&'static str> = Vec::new();
        for field in wslc_core::model::wslconf::FIELDS {
            let id = wslconf_field_id(field.section, field.key);
            assert_ne!(
                id, "wslconf-unknown",
                "字段 `{}.{}` 没有自己的元素 id —— 去 `wslconf_field_id` 里补一行",
                field.section, field.key
            );
            assert!(
                !seen.contains(&id),
                "元素 id 撞了：`{id}`（`{}.{}`）",
                field.section,
                field.key
            );
            seen.push(id);
        }
        // 反方向：表里没有的东西不该拿到"看起来合法"的 id
        assert_eq!(wslconf_field_id("nope", "nope"), "wslconf-unknown");
    }

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
        // v0.6 加了「镜像仓库」（在线浏览 registry 的 tag 再拉取），8 + 1 = 9。
        // v0.6.2 把它**并回「镜像」页**（远端浏览 + 拉取 + 本地已有，三件事一页），
        // 9 - 1 = 8 —— 同样是"总数少了不代表功能少了"。
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
    fn only_the_pages_with_inputs_need_a_window_to_enter() {
        // 有输入框的页面必须在点击时（有 window）把表单建好。
        // 哪天给别的页面加了输入框，这个测试会提醒你一起改。
        //
        // ⚠️ `Page::AddInstance` **不在 `Page::ALL` 里**（侧边栏不列它），
        // 所以不能只遍历 `ALL` 就算完 —— 那样这条断言对它就是**空的**，
        // 哪天真把它改坏了也抓不到。显式再查一遍。
        for page in Page::ALL {
            // v0.6.3：侧边栏这几页里一个有输入框的都没有了 ——
            // 「镜像」的拉取表单进了弹窗，是在点「拉取镜像…」那一刻建的。
            assert!(
                !page.needs_window_to_enter(),
                "{page:?} 现在说它需要 window —— 如果它真加了输入框，记得在 app.rs 的 \
                 `set_page` 里懒创建表单，并把这条断言改回去"
            );
        }
        assert!(Page::AddInstance.needs_window_to_enter());
    }

    #[test]
    fn registry_state_never_shows_a_stale_repository() {
        let mut state = RegistryState::new();
        assert_eq!(state.host, registry::DEFAULT_REGISTRY);

        state.begin_query(registry::DEFAULT_REGISTRY, "alpine", "library/alpine", None);
        assert!(state.loading, "提交查询就该进入 loading");
        assert_eq!(
            state.normalization_note().as_deref(),
            Some("由 alpine 解析为 library/alpine"),
            "原文与规范化结果不同时要说清"
        );
        state.set_tags(
            "library/alpine",
            vec!["latest".to_owned(), "3.24.2".to_owned()],
        );
        assert!(!state.loading);
        assert!(state.error.is_none());
        assert_eq!(state.loaded.as_deref(), Some("library/alpine"));
        assert_eq!(state.tags.len(), 2);

        // 原文就是规范名时不该多嘴
        state.begin_query(
            registry::DEFAULT_REGISTRY,
            "library/alpine",
            "library/alpine",
            None,
        );
        assert_eq!(state.normalization_note(), None);

        // ⚠️ 换仓库：旧结果必须**立刻**消失。留着的话，网络那 1~2 秒里
        // 界面会把 nginx 的 tag 挂在 redis 的标题下 —— 用户最容易误判的时刻。
        state.set_tags("library/nginx", vec!["1.31.6".to_owned()]);
        state.select_tag("1.31.6");
        assert_eq!(state.selected.as_deref(), Some("1.31.6"));
        state.begin_query(registry::DEFAULT_REGISTRY, "redis", "library/redis", None);
        assert!(state.tags.is_empty(), "换仓库时旧 tag 不能留着");
        assert!(state.loaded.is_none(), "换仓库时不能说列表是谁的");
        assert!(state.selected.is_none(), "换仓库时选中的 tag 也不算数了");

        // 失败时同样不能留着旧结果（报错旁边挂着一列 tag，会被读成"这次查到的"）
        state.set_tags("library/redis", vec!["7.4".to_owned()]);
        state.set_error("这个 registry 上没有仓库 library/nope".to_owned());
        assert!(state.tags.is_empty());
        assert!(state.loaded.is_none());
        assert!(!state.loading);
        assert!(state.error.is_some());
    }

    #[test]
    fn registry_state_builds_the_pull_reference_with_the_registry_prefix() {
        let mut state = RegistryState::new();
        // 还没查过 → 没有可拉的
        assert_eq!(state.selected_reference(), None);

        state.begin_query(registry::DEFAULT_REGISTRY, "alpine", "library/alpine", None);
        state.set_tags("library/alpine", vec!["latest".to_owned()]);
        // 查到了但没选 tag → 还是不能拉
        assert_eq!(state.selected_reference(), None);

        state.select_tag("latest");
        assert!(state.sizing, "选中 tag 就该开始量体积");
        // ⚠️ 必须带 registry 前缀：不带的话 `wslc pull` 会去 Docker Hub 找，
        // 而它在这台机器上不可达（实测 000）。
        assert_eq!(
            state.selected_reference().as_deref(),
            Some("docker.1panel.live/library/alpine:latest")
        );

        state.set_size(3_850_349, 1);
        assert!(!state.sizing);
        assert_eq!(state.size_label().as_deref(), Some("下载约 3.9 MB（1 层）"));

        // 换 tag → 体积要重量，不能留着上一个 tag 的数字
        state.select_tag("3.24.2");
        assert!(state.size.is_none());
        assert!(state.size_label().is_none());
        // 量不出来时给原因，而不是留一个空的体积
        state.set_size_error("这个镜像没有 amd64/linux 那一份".to_owned());
        assert!(!state.sizing);
        assert!(state.size_error.is_some());
        // 但那个 tag 仍然是选中的（用户还能拉它）
        assert_eq!(
            state.selected_reference().as_deref(),
            Some("docker.1panel.live/library/alpine:3.24.2")
        );
    }

    #[test]
    fn registry_state_falls_back_to_the_tag_typed_in_the_input() {
        // 别的 registry 的 tags/list 可能要认证（实测 daocloud 401）——
        // 那时一个 tag 都列不出来，但用户已经在输入里写了 `:1.27`，
        // 那就必须能拉。这是合并两页（去掉「按引用拉取」弹窗）之后
        // "按完整引用拉取"唯一的落点。
        let mut state = RegistryState::new();
        state.begin_query(
            "docker.m.daocloud.io",
            "nginx:1.27",
            "library/nginx",
            Some("1.27".to_owned()),
        );
        assert_eq!(state.host, "docker.m.daocloud.io", "registry 是可编辑的");
        assert_eq!(state.effective_tag(), Some("1.27"));
        assert_eq!(
            state.selected_reference().as_deref(),
            Some("docker.m.daocloud.io/library/nginx:1.27"),
            "列表拉不到（甚至还没回来）也要能拼出引用"
        );

        // 查询失败也不影响它 —— 这正是它存在的理由
        state.set_error("401 Unauthorized".to_owned());
        assert!(state.tags.is_empty());
        assert_eq!(
            state.selected_reference().as_deref(),
            Some("docker.m.daocloud.io/library/nginx:1.27")
        );

        // 下拉框里选了一个就以选的为准（用户后来看清楚了要哪个）
        state.select_tag("1.29");
        assert_eq!(state.effective_tag(), Some("1.29"));
        assert_eq!(
            state.selected_reference().as_deref(),
            Some("docker.m.daocloud.io/library/nginx:1.29")
        );

        // 换仓库要把它清掉，否则下一次查询会莫名其妙带上上一次的 tag
        state.begin_query("docker.1panel.live", "redis", "library/redis", None);
        assert_eq!(state.effective_tag(), None);
        assert_eq!(state.selected_reference(), None);
    }

    #[test]
    fn install_sources_are_listed_and_described() {
        for kind in InstallSourceKind::ALL {
            assert!(!kind.label().is_empty(), "{kind:?}");
            assert!(!kind.hint().is_empty(), "{kind:?}");
        }
        assert!(InstallSourceKind::ALL.contains(&InstallSourceKind::default()));
        // 默认必须是最不依赖网络的那条
        assert_eq!(InstallSourceKind::default(), InstallSourceKind::Local);
    }

    #[test]
    fn install_source_capabilities_are_consistent() {
        // 只有在线安装有「装完启动」，也只有它不需要文件路径
        assert!(InstallSourceKind::Online.supports_launch());
        assert!(!InstallSourceKind::Online.needs_path());

        // 本地那一条要选文件，而且要有标签/占位/过滤器
        // （它把原来的 tar / VHDX / `.wsl` 三条合一了 —— 后缀由
        // `classify_local_file` 分岔，见 `wslc_core::model::install`）
        let kind = InstallSourceKind::Local;
        assert!(!kind.supports_launch(), "{kind:?}");
        assert!(kind.needs_path(), "{kind:?}");
        assert!(kind.is_local(), "{kind:?}");
        assert!(!kind.path_label().is_empty(), "{kind:?}");
        assert!(!kind.path_placeholder().is_empty(), "{kind:?}");
        assert!(kind.requires_install_dir(), "{kind:?}");
        let (label, extensions) = kind.file_filter().expect("本地来源要能弹选择器");
        assert!(!label.is_empty(), "{kind:?}");
        // 后缀表必须是**并集**：tar 系列 + vhdx + .wsl，少一个用户就选不到
        for expected in ["tar", "gz", "xz", "vhdx", "vhd", "wsl"] {
            assert!(
                extensions.contains(&expected),
                "少了 {expected}：{extensions:?}"
            );
        }

        // 两条"网络来源"不要文件路径，也不要文件选择器
        for kind in [InstallSourceKind::Mirror, InstallSourceKind::Online] {
            assert!(!kind.needs_path(), "{kind:?}");
            assert!(kind.file_filter().is_none(), "{kind:?}");
            assert!(!kind.is_local(), "{kind:?}");
            assert!(kind.is_online() == (kind == InstallSourceKind::Online));
        }
        // 镜像站要安装目录（下载完要 --import），在线安装可以留空
        assert!(InstallSourceKind::Mirror.is_mirror());
        assert!(InstallSourceKind::Mirror.requires_install_dir());
        assert!(!InstallSourceKind::Online.requires_install_dir());

        // 版本**不给用户选**：本项目只支持 WSL 2，安装命令里固定 `--version 2`
        // （`InstallSourceKind` 上再也没有 `supports_version` 这种东西了）。

        // 三种来源的标签不能重复（按钮上会分不清）
        let labels: Vec<&str> = InstallSourceKind::ALL.iter().map(|k| k.label()).collect();
        let unique: std::collections::HashSet<&&str> = labels.iter().collect();
        assert_eq!(unique.len(), labels.len(), "{labels:?}");
        assert_eq!(labels.len(), 3, "本地那三条已经合一：{labels:?}");

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

        let mut progress = InstallProgress::new("MyUbuntu", vec!["Ubuntu-24.04".to_owned()]);
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
    fn busy_with_also_knows_the_name_the_install_registers_first() {
        // 改名安装的中间态：`wsl -l -v` 上是**源名**（`Ubuntu-24.04`），
        // 而用户要的是 `MyUbuntu`。只认最终名字的话，那段时间里列表上那一行
        // 的「终止 / 删除 / 打开终端」全都可点 —— 而重定位正是
        // 「导出 → 注销 → 导入」那段不可逆的操作（见 `busy_with` 的说明）。
        let mut state = AppState::new(wslc_core::Wslc::new(), wslc_core::Wsl::new());

        // 还没开始装：谁都不拦
        assert_eq!(state.busy_with("Ubuntu-24.04"), None);

        state.installing = Some(InstallProgress::new(
            "MyUbuntu",
            vec!["Ubuntu-24.04".to_owned()],
        ));
        assert_eq!(state.busy_with("MyUbuntu"), Some("安装"));
        assert_eq!(state.busy_with("Ubuntu-24.04"), Some("安装"), "源名也要认");
        // 大小写不敏感（WSL 自己就不区分）
        assert_eq!(state.busy_with("ubuntu-24.04"), Some("安装"));
        // 别的发行版照旧放行 —— 安装要几十分钟，不能把整页锁住
        assert_eq!(state.busy_with("Debian"), None);

        // 装完之后不再拦
        if let Some(progress) = state.installing.as_mut() {
            progress.finish(InstallOutcome::Success("好了".to_owned()));
        }
        assert_eq!(state.busy_with("Ubuntu-24.04"), None);
        assert_eq!(state.busy_with("MyUbuntu"), None);
    }

    #[test]
    fn install_progress_log_is_capped() {
        use wslc_core::cmd::install::InstallEvent;

        let mut progress = InstallProgress::new("X", Vec::new());
        for i in 0..(InstallProgress::MAX_LOG_LINES + 50) {
            progress.apply(&InstallEvent::Line(format!("第 {i} 行")));
        }
        assert_eq!(progress.log.len(), InstallProgress::MAX_LOG_LINES);
        // 留下的是**最后**那些行（用户要看的是最新的）
        assert!(
            progress
                .log
                .last()
                .unwrap()
                .contains(&format!("第 {} 行", InstallProgress::MAX_LOG_LINES + 49))
        );
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
    fn online_ids_match_the_mirror_catalog() {
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

        let mut state = MirrorState::new();
        // 名字与版本都照真实清单（wslui 那份）写
        state.set_offers(
            vec![
                offer("Ubuntu", "24.04"),
                offer("Ubuntu", "22.04"),
                offer("Debian", "13"),
                offer("Kali", "current"),
                offer("Arch Linux", "current"),
                offer("Fedora", "44"),
                offer("AlmaLinux", "9"),
                offer("openSUSE", "tumbleweed"),
            ],
            String::new(),
            None,
        );

        // ① 名字 + 版本拼起来对上
        assert_eq!(
            state.match_online_id("Ubuntu-24.04").as_deref(),
            Some("Ubuntu 24.04")
        );
        assert_eq!(
            state.match_online_id("ubuntu 22.04").as_deref(),
            Some("Ubuntu 22.04")
        );
        assert_eq!(
            state.match_online_id("AlmaLinux-9").as_deref(),
            Some("AlmaLinux 9")
        );
        assert_eq!(
            state.match_online_id("openSUSE-Tumbleweed").as_deref(),
            Some("openSUSE tumbleweed")
        );
        // ② 只对上名字（在线 id 不带版本）
        assert_eq!(
            state.match_online_id("Ubuntu").as_deref(),
            Some("Ubuntu 24.04")
        );
        assert_eq!(
            state.match_online_id("Debian").as_deref(),
            Some("Debian 13")
        );
        // ③ 名字前缀（两边叫法有出入）
        assert_eq!(
            state.match_online_id("kali-linux").as_deref(),
            Some("Kali current")
        );
        assert_eq!(
            state.match_online_id("archlinux").as_deref(),
            Some("Arch Linux current")
        );
        assert_eq!(
            state.match_online_id("FedoraLinux-44").as_deref(),
            Some("Fedora 44")
        );
        // 认不出来 / 空串 → 不硬猜
        assert_eq!(state.match_online_id("SUSE-Linux-Enterprise-15-SP7"), None);
        assert_eq!(state.match_online_id(""), None);
        assert_eq!(state.match_online_id("   "), None);
    }

    #[test]
    fn a_bare_family_name_falls_back_to_its_newest_versioned_sibling() {
        // 就是本机的真实清单（顺序也照 `wsl -l -o`：裸名字在最前）
        let mut online = OnlineDistroState::new();
        online.items = vec![
            OnlineDistro::new("Ubuntu", "Ubuntu"),
            OnlineDistro::new("Ubuntu-26.04", "Ubuntu 26.04 LTS"),
            OnlineDistro::new("Ubuntu-24.04", "Ubuntu 24.04 LTS"),
            OnlineDistro::new("Ubuntu-22.04", "Ubuntu 22.04 LTS"),
            OnlineDistro::new("Debian", "Debian GNU/Linux"),
            OnlineDistro::new("kali-linux", "Kali Linux Rolling"),
            OnlineDistro::new("AlmaLinux-10", "AlmaLinux OS 10"),
            OnlineDistro::new("AlmaLinux-9", "AlmaLinux OS 9"),
        ];

        // 裸 `Ubuntu` → 最新的那个版本号（不是列表里的下一个）
        let none: Vec<String> = Vec::new();
        assert_eq!(
            online.sibling_id("Ubuntu", &none).as_deref(),
            Some("Ubuntu-26.04")
        );
        // 带版本的 id 自己不再往下找
        assert_eq!(online.sibling_id("Ubuntu-24.04", &none), None);
        assert_eq!(online.sibling_id("AlmaLinux-10", &none), None);
        // 没有兄弟的不硬猜
        assert_eq!(online.sibling_id("Debian", &none), None);
        assert_eq!(online.sibling_id("kali-linux", &none), None);
        assert_eq!(online.sibling_id("", &none), None);
        // 版本比较按数值而不是按字典序（22 < 24 < 26）
        assert_ne!(
            online.sibling_id("Ubuntu", &none).as_deref(),
            Some("Ubuntu-22.04")
        );

        // ⚠️ 已经装过的兄弟不能选：本机就是这个状态（`Ubuntu-26.04` 装着），
        // 换过去只会撞 `Wsl/InstallDistro/ERROR_ALREADY_EXISTS`（实测）。
        let taken = vec!["Ubuntu-26.04".to_owned()];
        assert_eq!(
            online.sibling_id("Ubuntu", &taken).as_deref(),
            Some("Ubuntu-24.04"),
            "最新的那个装过了，就退到下一个版本"
        );
        // 大小写不同也是同一个发行版
        let taken = vec!["ubuntu-26.04".to_owned()];
        assert_eq!(
            online.sibling_id("Ubuntu", &taken).as_deref(),
            Some("Ubuntu-24.04")
        );
        // 同族的全装过了 → 干脆不换（返回 None，让界面照实说这条路装不动）
        let taken = vec![
            "Ubuntu-26.04".to_owned(),
            "Ubuntu-24.04".to_owned(),
            "Ubuntu-22.04".to_owned(),
        ];
        assert_eq!(online.sibling_id("Ubuntu", &taken), None);
        // 装了别的发行版不影响
        let taken = vec!["Debian".to_owned()];
        assert_eq!(
            online.sibling_id("Ubuntu", &taken).as_deref(),
            Some("Ubuntu-26.04")
        );
    }

    #[test]
    fn probe_results_are_sorted_fastest_first_with_failures_last() {
        // 探测是**按清单顺序**发起的（见 `Shell::probe_mirrors`），所以结果必须
        // 排一下 —— 否则界面上"哪个镜像快"要靠用户自己在十几行里找。
        fn result(site: &str, code: u16, secs: f64) -> MirrorProbeResult {
            MirrorProbeResult {
                site: site.to_owned(),
                url: format!("https://x/{site}"),
                code,
                secs,
                bytes: if code == 200 { Some(10) } else { None },
            }
        }

        let mut state = MirrorState::new();
        state.results = vec![
            result("慢", 200, 2.0),
            result("连不上", 0, 8.0),
            result("快", 200, 0.3),
            result("404", 404, 0.1),
            // ⚠️ 探测没跑成的候选：`code == 0` 且 `secs == 0.0`
            // （`probe_mirrors` 里那个 `unwrap_or(0.0)`）
            result("没探", 0, 0.0),
        ];
        state.sort_results_by_speed();

        let sites: Vec<&str> = state.results.iter().map(|r| r.site.as_str()).collect();
        assert_eq!(
            sites,
            vec!["快", "慢", "没探", "404", "连不上"],
            "可用的按耗时升序排前面，失败的排后面；\
             只按 `secs` 排的话「没探」（0.0）会跑到最前面，看着像最快的那个"
        );
    }

    #[test]
    fn the_catalog_is_sorted_like_the_reference() {
        // 用户要求"在线发行版的清单照 wsl-dashboard 来"：Ubuntu 最前、Debian 次之，
        // 同族按版本**倒序**（新的在前），滚动版（没有版本号）排最后。
        fn offer(name: &str, version: &str) -> Offer {
            Offer {
                name: name.to_owned(),
                version: version.to_owned(),
                sources: vec![wslc_core::mirrors::OfferSource {
                    mirror: "lxc-tuna".to_owned(),
                    url: format!("https://x/{name}-{version}.tar.xz"),
                    format: "tar.xz".to_owned(),
                }],
            }
        }

        let mut state = MirrorState::new();
        state.set_offers(
            vec![
                offer("openSUSE", "tumbleweed"),
                offer("Rocky Linux", "10"),
                offer("Ubuntu", "20.04"),
                offer("Debian", "12"),
                offer("openSUSE", "16.0"),
                offer("Ubuntu", "24.04"),
                offer("Debian", "13"),
                offer("Ubuntu", "22.04"),
                offer("Alpine", "3.22"),
                offer("openEuler", "24.03"),
            ],
            "测试清单".to_owned(),
            None,
        );

        let ids: Vec<String> = state.offers.iter().map(Offer::id).collect();
        assert_eq!(
            ids,
            vec![
                // 1 常用度：Ubuntu 最前，同族版本倒序
                "Ubuntu 24.04",
                "Ubuntu 22.04",
                "Ubuntu 20.04",
                // 2 Debian
                "Debian 13",
                "Debian 12",
                // 6 openSUSE：16.0 在滚动版前面（数值键 > 空键）
                "openSUSE 16.0",
                "openSUSE tumbleweed",
                // 9 / 11 / 12
                "Rocky Linux 10",
                "Alpine 3.22",
                "openEuler 24.03",
            ]
        );
        // 排完之后默认选中第一条 —— 参考实现里用户打开就是 Ubuntu 24.04
        assert_eq!(state.distro_id, "Ubuntu 24.04");
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
        assert!(
            unknown.summary().contains("大小未知"),
            "{}",
            unknown.summary()
        );
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
        assert_eq!(deduped, vec!["概览", "WSL 实例", "容器", "资源", "设置"]);
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
            PendingAction::PruneVolumes,
            PendingAction::TerminateSession {
                id: 1,
                name: "wslc-cli-76434".into(),
            },
        ];
        for action in actions {
            assert!(!action.title().is_empty());
            assert!(!action.body().is_empty());
            assert!(!action.confirm_label().is_empty());
        }
    }

    #[test]
    fn terminate_session_body_names_which_session() {
        // 界面上同时可能有好几个会话。只写"将终止会话"用户没法确认
        // 自己点的是哪一个 —— 名字和编号都要在。
        let action = PendingAction::TerminateSession {
            id: 7,
            name: "wslc-cli-76434".into(),
        };
        let body = action.body();
        assert!(body.contains("wslc-cli-76434"), "{body}");
        assert!(body.contains("#7"), "{body}");
        // 还得说清"运行中的容器会停"以及"数据不会被删"这两件事
        assert!(body.contains("容器"), "{body}");
        assert!(body.contains("storage.vhdx"), "{body}");
    }

    #[test]
    fn prune_volumes_says_it_only_touches_unused_ones() {
        // 「清理」这个词本身很含糊，用户最怕的是"把我正在用的卷删了"。
        let body = PendingAction::PruneVolumes.body();
        assert!(body.contains("没有被任何容器使用"), "{body}");
        assert!(body.contains("不可撤销"), "{body}");
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
            PromptKind::CreateVolume,
        ] {
            assert!(!kind.title().is_empty(), "{kind:?}");
            assert!(!kind.label().is_empty(), "{kind:?}");
            assert!(!kind.placeholder().is_empty(), "{kind:?}");
            assert!(!kind.note().is_empty(), "{kind:?}");
            assert!(!kind.confirm_label().is_empty(), "{kind:?}");
        }
    }

    #[test]
    fn only_the_two_path_prompts_offer_a_picker() {
        // 「浏览…」只对"要填路径"的那两种有意义。给「创建卷」加一个
        // 会让用户以为该去选一个文件。
        assert!(PromptKind::MoveDistro.pick_target().is_some());
        assert!(PromptKind::ExportDistro.pick_target().is_some());
        assert!(PromptKind::ResizeDistro.pick_target().is_none());
        assert!(PromptKind::SetDefaultUser.pick_target().is_none());
        assert!(PromptKind::CreateVolume.pick_target().is_none());
    }

    // -- 卷名与 exec 命令的纯逻辑 ------------------------------------------

    #[test]
    fn volume_name_refuses_anything_that_looks_like_a_flag() {
        // 这条是本地挡下来的**唯一**一类：卷名会变成裸 argv，
        // `--help` 之类会被 wslc 当选项而不是名字。
        assert!(validate_volume_name("webdata").is_ok());
        assert!(validate_volume_name("  webdata  ").is_ok());
        // 名字**中间**有横线是正常的，不该被挡
        assert!(validate_volume_name("web-data_1").is_ok());

        assert!(validate_volume_name("--help").is_err());
        assert!(validate_volume_name("-x").is_err());
        assert!(validate_volume_name("").is_err());
        assert!(validate_volume_name("   ").is_err());
        // 前导空格不能让 `-` 蒙混过去（trim 之后再判）
        assert!(validate_volume_name("  --force").is_err());
    }

    #[test]
    fn exec_argv_goes_through_a_shell() {
        // 锁住这个形状：**不要**改成按空格切分。管道 / 重定向 / `&&`
        // 是用户预期能用的，切开之后它们会静默失效（命令看起来还成功了）。
        assert_eq!(
            exec_argv("ls -la /tmp | head -5"),
            vec!["/bin/sh", "-c", "ls -la /tmp | head -5"]
        );
        // `/bin/sh` 而不是 `/bin/bash`：alpine 这类镜像里没有 bash
        assert_eq!(exec_argv("true")[0], "/bin/sh");
        // 原文一个字都不改地传下去（包括引号）
        assert_eq!(exec_argv("echo 'a b'")[2], "echo 'a b'");
    }

    // -- 日志浮层的状态 ----------------------------------------------------

    #[test]
    fn logs_state_maps_its_controls_to_log_options() {
        let mut logs = LogsState::new("web");
        // 默认给一个有限的档位 —— "全部"在大容器上可能是几百 MB
        assert_eq!(logs.tail, Some(200));
        assert_eq!(logs.options().tail, Some(200));
        assert!(!logs.options().timestamps);
        assert!(!logs.options().details, "详细默认不开");

        logs.tail = None;
        logs.timestamps = true;
        let opts = logs.options();
        assert_eq!(opts.tail, None, "None 是「全部」");
        assert!(opts.timestamps);
    }

    #[test]
    fn log_tail_labels_cover_every_preset() {
        assert_eq!(LogsState::tail_label(Some(200)), "200 行");
        assert_eq!(LogsState::tail_label(None), "全部");
        // 档位表里那个"全部"必须真的是 None，不然界面上点它没反应
        assert_eq!(LogsState::TAILS[3], None);
    }

    #[test]
    fn a_stale_log_response_is_dropped() {
        // 连点「1000 行」→「全部」会有两个请求在飞，先发的可能后回。
        // 只认最后一次发起的那个 —— 否则界面会显示上一次选择的结果。
        let mut logs = LogsState::new("web");

        let first = logs.begin_load();
        assert!(logs.loading);
        let second = logs.begin_load();
        assert_ne!(first, second, "每轮票据必须不同");

        // 旧的那个后到了：必须被丢掉
        assert!(!logs.finish_load(first, Ok("旧结果".into())));
        assert!(logs.loading, "还在等新的那个，不能因为旧响应到了就收工");
        assert_eq!(logs.text, None, "旧结果不能覆盖");

        // 新的那个到了：收下
        assert!(logs.finish_load(second, Ok("新结果".into())));
        assert!(!logs.loading);
        assert_eq!(logs.text.as_deref(), Some("新结果"));
    }

    #[test]
    fn a_failed_log_load_keeps_the_previous_text() {
        // 拉失败时**不该**把上一次的内容擦掉：用户宁可看着旧日志，
        // 也不想面对一片空白（而且错误信息会单独显示）。
        let mut logs = LogsState::new("web");
        let t = logs.begin_load();
        assert!(logs.finish_load(t, Ok("上一次的内容".into())));

        let t2 = logs.begin_load();
        assert!(logs.finish_load(t2, Err("连不上".into())));
        assert_eq!(logs.text.as_deref(), Some("上一次的内容"));
        assert_eq!(logs.error.as_deref(), Some("连不上"));
        assert!(!logs.loading);
    }

    #[test]
    fn begin_load_clears_the_previous_error() {
        // 重试时上一条错误必须先消失，否则界面会一直挂着它
        let mut logs = LogsState::new("web");
        let t = logs.begin_load();
        logs.finish_load(t, Err("第一次失败".into()));
        assert!(logs.error.is_some());

        logs.begin_load();
        assert_eq!(logs.error, None);
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
