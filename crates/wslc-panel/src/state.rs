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

use wslc_core::model::{
    ContainerSummary, Distro, ImageListItem, NetworkListItem, Session, SystemInfo, VolumeListItem,
    WslStatus,
};
use wslc_core::settings::SettingsDoc;
use wslc_core::storage::StorageInfo;
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
}

impl Page {
    /// 全部页面（决定导航顺序）。
    ///
    /// ⚠️ 同组的页面必须**连续** —— 侧边栏靠"组名变了就插一条标题"
    /// 来分组（见 `app.rs` 的 `render`）。
    pub const ALL: [Page; 9] = [
        Page::Dashboard,
        Page::Instances,
        Page::AddInstance,
        Page::Containers,
        Page::Images,
        Page::Networks,
        Page::Volumes,
        Page::AppSettings,
        Page::Config,
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
        }
    }

    /// 导航分组（用于在侧边栏插入分隔标题）。
    pub fn group(self) -> &'static str {
        match self {
            Page::Dashboard => "概览",
            Page::Instances | Page::AddInstance => "WSL 实例",
            Page::Containers => "容器",
            Page::Images | Page::Networks | Page::Volumes => "资源",
            Page::AppSettings | Page::Config => "设置",
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
/// 拼成 [`wslc_core::cmd::distro::InstallSpec`] 是 `app.rs` 的事。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InstallSourceKind {
    /// 从本地 tar 导入（`wsl --import`）。
    #[default]
    Tar,
    /// 从本地文件安装（`wsl --install --from-file`）。
    File,
    /// 在线安装（`wsl --install -d`）。
    Online,
}

impl InstallSourceKind {
    /// 全部可选值（决定界面上的按钮顺序）。
    ///
    /// 顺序刻意是 **tar → 文件 → 在线**：越靠前越不依赖网络。
    /// 本机实测 `wsl --list --online` 是坏的（解析不了
    /// `raw.githubusercontent.com`），所以在线那条最不该当默认。
    pub const ALL: [InstallSourceKind; 3] = [
        InstallSourceKind::Tar,
        InstallSourceKind::File,
        InstallSourceKind::Online,
    ];

    /// 按钮文案。
    pub fn label(self) -> &'static str {
        match self {
            Self::Tar => "从 tar 导入",
            Self::File => "从文件安装",
            Self::Online => "在线安装",
        }
    }

    /// 一句话说明（显示在按钮下面）。
    pub fn hint(self) -> &'static str {
        match self {
            Self::Tar => "最可靠：本地 tar 文件，不需要联网。只是把文件系统铺开。",
            Self::File => "交给 WSL 自己的安装器，会做首次启动初始化（建默认用户）。",
            Self::Online => "从微软的源下载。发行版名要手输 —— 本机拉不到在线列表。",
        }
    }

    /// 需不需要让用户填一个**文件路径**。
    pub fn needs_path(self) -> bool {
        !matches!(self, Self::Online)
    }

    /// 路径输入框的标签。
    pub fn path_label(self) -> &'static str {
        match self {
            Self::Tar => "tar 文件路径",
            Self::File => "安装文件路径",
            Self::Online => "",
        }
    }

    /// 路径输入框的占位提示。
    pub fn path_placeholder(self) -> &'static str {
        match self {
            Self::Tar => r"D:\img\ubuntu-rootfs.tar",
            Self::File => r"D:\img\Ubuntu-24.04-rootfs.tar.gz",
            Self::Online => "",
        }
    }

    /// 发行版名输入框的占位提示。
    pub fn name_placeholder(self) -> &'static str {
        match self {
            Self::Online => "Ubuntu-24.04（要手输，本机拉不到在线列表）",
            _ => "MyDistro",
        }
    }

    /// 安装目录是不是必填。
    ///
    /// 在线安装可以留空（WSL 有自己的默认位置），另两条必须给。
    pub fn requires_install_dir(self) -> bool {
        !matches!(self, Self::Online)
    }

    /// 支不支持"装完启动"（只有在线安装有 `--no-launch`）。
    pub fn supports_launch(self) -> bool {
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
    /// 各区段的错误信息
    pub errors: Vec<String>,
    /// 本次刷新耗时（毫秒）
    pub elapsed_ms: u128,
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

    snap.elapsed_ms = started.elapsed().as_millis();
    snap
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
                "导出成 **tar**，之后可以用「添加实例 → 从 tar 导入」再装回来，\
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
    /// 唤醒一个已停止的发行版。
    ///
    /// ⚠️ 这是**会自己失效**的动作：实测 WSL 3.x 在约 20 秒后会把发行版
    /// 收回 Stopped（后台常驻进程也留不住）。界面上必须写明这一点，
    /// 详见 [`wslc_core::cmd::distro::start`]。
    StartDistro(String),
    /// 打开发行版的终端（`wsl -d <name>`，会开一个新的控制台窗口）。
    ///
    /// 这才是"让它持续运行"的正确入口 —— 终端开着，发行版就一直是运行中。
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

/// 应用的完整状态。
///
/// 这是 [`crate::app::Shell`] 里唯一的字段，所有页面都是它的只读视图。
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
    /// 待用户确认的危险操作（容器域或发行版域）。
    pub confirm: Option<ConfirmAction>,
    /// 提示条。
    pub toast: Option<Toast>,
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
            confirm: None,
            toast: None,
        }
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
        // v0.3 加了「实例列表」「添加实例」「应用设置」，从 6 个变成 9 个。
        //
        // 这个断言存在的意义就是**逼人改它**：加页面时忘了同步导航分组，
        // 侧边栏会出现重复的组标题。历史上 commit 552a91c 就是被它抓到的。
        //
        // ⚠️ 注意 `cargo check --all-targets` 只编译不执行，
        // 所以它真的被跑到要靠 CI 里的 `cargo test -p wslc-panel --bins`（见 SPIKE 7.8）。
        assert_eq!(Page::ALL.len(), 9);
    }

    #[test]
    fn only_the_add_instance_page_needs_a_window_to_enter() {
        // 有输入框的页面必须在点击时（有 window）把表单建好。
        // 哪天给别的页面加了输入框，这个测试会提醒你一起改。
        for page in Page::ALL {
            assert_eq!(
                page.needs_window_to_enter(),
                page == Page::AddInstance,
                "{page:?} 的 needs_window_to_enter 不对"
            );
        }
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
        for kind in [InstallSourceKind::Tar, InstallSourceKind::File] {
            assert!(!kind.supports_launch(), "{kind:?}");
            assert!(kind.needs_path(), "{kind:?}");
            assert!(!kind.path_label().is_empty(), "{kind:?}");
            assert!(!kind.path_placeholder().is_empty(), "{kind:?}");
        }

        // 版本**不给用户选**：本项目只支持 WSL 2，安装命令里固定 `--version 2`
        // （`InstallSourceKind` 上再也没有 `supports_version` 这种东西了）。

        // 只有在线安装允许留空安装目录
        assert!(InstallSourceKind::Tar.requires_install_dir());
        assert!(InstallSourceKind::File.requires_install_dir());
        assert!(!InstallSourceKind::Online.requires_install_dir());
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
