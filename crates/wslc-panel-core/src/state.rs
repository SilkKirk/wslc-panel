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

use std::collections::BTreeSet;
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
// 多驱动层的连接类型（v0.13）。`DbState::conn` 用它 —— 现在只是容器 + 家族的
// 镜像，阶段 1 起它会成为"连接"的真正载体（见 `docs/PLAN-multi-db.md`）。
use wslc_core::db::{Credentials, DbConn, DbHost, DbKind, MySqlDriver, PostgresDriver, SqlDriver};
use wslc_core::model::wslconf;
use wslc_core::model::{
    AppRuntime, ContainerSummary, DbEntry, DbUser, DeployErrorKind, DeploySignal, Distro,
    ImageListItem, MysqlCreds, MysqlErrorKind, MysqlFamily, NetworkListItem, ProcessStatus,
    RootCredHint, SqlMode, SystemInfo, VolumeListItem, WslStatus, parse_size,
};
use wslc_core::settings::SettingsDoc;
// SQL 控制台的结构化结果。`SqlResult` 住在 `wslc-core` 的 `model/` 里
// （纯逻辑、能脱离 Windows 单测），这里只装它。
use wslc_core::model::sqlresult::SqlResult;
// 会话存储的位置与占用：基本信息页「存储」那一环要的是它算出来的盘容量
// （模板里那份 `StorageInfo` 自己带着 `VolumeSpace`）。
use wslc_core::storage::{StorageInfo, VolumeSpace};
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
    /// 容器里的**数据库管理**（MySQL / MariaDB 容器）。
    ///
    /// ⚠️ **不在 [`Page::ALL`] 里** —— 和 `AddInstance` 同一个取舍：它不是一个
    /// 并列的站点，而是"针对某一个容器"的页面，只能从**容器列表行上那个
    /// 「数据库」按钮**进来（只有认得出是 MySQL 的那一行才画它）。
    /// 所以页面顶部有「← 返回容器」，侧边栏里没有它。
    ///
    /// 进这一页需要 window：连接卡上有用户名 / 密码输入框，
    /// 而 `InputState::new` 要 `&mut Window`（见 [`Page::needs_window_to_enter`]）。
    Database,
    /// 容器里的**应用部署管理**（nginx / Java / Node.js / Go 容器）。
    ///
    /// ⚠️ **不在 [`Page::ALL`] 里** —— 和 `AddInstance` / `Database` 同一个取舍：
    /// 它服务**某一个**容器，只能从**容器列表行上那个「部署管理」按钮**进来
    /// （只有认得出是这几类运行时的那一行才画它）。所以顶部有「← 返回容器」，
    /// 侧边栏里没有它。
    ///
    /// 进这一页需要 window：「添加部署」弹窗里全是输入框，而
    /// `InputState::new` 要 `&mut Window`（见 [`Page::needs_window_to_enter`]）。
    Deploy,
    /// **SQL 工作区**（v0.12）：左侧库 / 表树 + 右上编辑器 + 右下结果表格。
    ///
    /// # 为什么是独立页面，而不是「数据库管理」页里的一个浮层
    ///
    /// v0.11 把它做成了浮层（`views::db_console_overlay`），实机用下来三个问题：
    ///
    /// 1. 浮层盖住页面，**看着像弹窗**，而写 SQL 往往是个持续十分钟的事；
    /// 2. 浮层里塞进"树 + 编辑器 + 表格"三块，哪块都不够高；
    /// 3. 关掉就全没了，视觉上像"退出"，而用户只是想换个查询。
    ///
    /// 用户原话："别弹窗出来，直接跳到新页面，左边的列表树左对齐，右上面是
    /// 一个输入的大框，右下显示数据"。所以这一页**独占整个右侧区域**，
    /// 树贴着左边框（没有外层内边距把它推开）。
    ///
    /// ⚠️ **不在 [`Page::ALL`] 里** —— 和 `Database` / `Deploy` 同一个取舍：
    /// 它服务**某一个**容器的某一次会话，只能从数据库列表行上那个「SQL」
    /// 按钮进来。所以侧边栏没有它，页头有「← 返回」。
    Sql,
    /// 应用自己的设置（刷新间隔、主题……）。
    ///
    /// 这里存的是**本程序的偏好**（`%LOCALAPPDATA%\wslc-panel\prefs.json`）。
    /// `wslc` 自己的 `settings.yaml`、WSL 自己的 `.wslconfig` 和「关于」
    /// 都是这一页里的 **tab**（见 [`SettingsTab`]），不是并列的页面。
    ///
    /// ⚠️ 它们曾经是 `Page::Config` / `Page::WslConfig` 两个变体。v0.4 把它们
    /// 收进这一页的 tab 之后就没**人**能导航到它们了（`ALL` 里没有、界面上
    /// 也不构造），却一直留在枚举里：`nav_id` 与页面分发里各留了两个永远走不到
    /// 的分支。已删。
    /// v0.9.22 又把「关于」从页面收进来 —— 它同样是"只能看、不能改"的低频页，
    /// 不值得占一个侧边栏入口。
    AppSettings,
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
    pub const ALL: [Page; 7] = [
        Page::Dashboard,
        Page::Instances,
        Page::Containers,
        Page::Images,
        Page::Networks,
        Page::Volumes,
        Page::AppSettings,
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
            Page::Database => "数据库管理",
            Page::Deploy => "部署管理",
            Page::Sql => "SQL 控制台",
            Page::AppSettings => "应用设置",
        }
    }

    /// 导航分组（用于在侧边栏插入分隔标题）。
    pub fn group(self) -> &'static str {
        match self {
            Page::Dashboard => "概览",
            Page::Instances | Page::AddInstance => "WSL 实例",
            Page::Containers | Page::Database | Page::Deploy | Page::Sql => "容器",
            Page::Images | Page::Networks | Page::Volumes => "资源",
            Page::AppSettings => "设置",
        }
    }

    /// 这一页是不是需要 `&mut Window` 才能进去。
    ///
    /// 有三页在这个列里，原因都是同一个：`InputState::new` 要 `&mut Window`，
    /// 而这些输入框描述的是**页面状态**（不是一次性动作），只能在点击导航
    /// 那一刻建一次；在 render 里建会每帧重建，字都打不进去。
    ///
    /// - 「添加实例」：名字 / 安装目录 / 来源路径，以及两个 `SelectState`；
    /// - 「数据库管理」：连接卡的用户名 / 密码；
    /// - 「容器」（v0.9.22）：列表上方的搜索框。
    ///
    /// ⚠️ 「部署管理」**故意不在这里**：它页面上一个输入框都没有，
    /// 「添加部署」那五个框是**点按钮那一刻**才建的（`Shell::open_deploy_dialog`）。
    /// 多写一个进去不是"更保险"：它会让 `set_page` 里多一个**永远不会被执行**
    /// 的分支，而那正是 v0.4 删掉 `Page::Config` / `Page::WslConfig` 时清掉的
    /// 那种垃圾。同理，「镜像」的拉取表单在弹窗里，也不在这里。
    pub fn needs_window_to_enter(self) -> bool {
        // `Page::Sql` 也要：SQL 编辑框 + 树里的过滤框都是 `InputState`。
        matches!(
            self,
            Page::AddInstance | Page::Database | Page::Containers | Page::Sql
        )
    }

    /// 这一页在侧边栏里应该高亮哪一项。
    ///
    /// 自己就是导航项时返回 `None`（`highlights_nav` 会走"自己等于自己"那一支）；
    /// 子页面返回它所属的父级：
    ///
    /// - 「添加实例」是「实例列表」的一个动作 → 高亮实例列表；
    /// - 「数据库管理 / 部署管理」服务某一个容器 → 高亮容器。
    ///
    /// 不加这个的话，进这三个页面时侧边栏**一个都不选中**，用户会失去"我在哪"。
    pub fn nav_parent(self) -> Option<Page> {
        match self {
            Page::AddInstance => Some(Page::Instances),
            Page::Database | Page::Deploy => Some(Page::Containers),
            // SQL 控制台是从「数据库管理」页的列表行进去的，所以高亮那一页。
            Page::Sql => Some(Page::Database),
            _ => None,
        }
    }

    /// 侧边栏的 `nav` 这一项，在当前页面 `self` 上是否算选中。
    pub fn highlights_nav(self, nav: Page) -> bool {
        self == nav || self.nav_parent() == Some(nav)
    }
}

/// 仅控制界面展开，不影响实际表单参数。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum UiSection {
    CreateAdvanced,
    CreateCommandPreview,
    ConfigRaw,
    WslHelp,
    DeployProxy,
    DeployProxyMore,
    PullSource,
    DbEncoding,
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
///
/// # ⚠️ 这里只放**界面真的会读**的字段
///
/// v0.9.16 清过一轮：`sessions`（`wslc system session list`）被删掉了 ——
/// 它唯一的消费者是「基本信息」页那张「活动会话」卡，而**同一份数据
/// `wslc info` 的 `Server.Sessions` 里已经有了**（见 [`AppState::session_label`]），
/// 也就是说那是个每 3 秒白起的 `wslc.exe` 子进程。
///
/// `storage` **留下来了**：它当时也差点被删（唯一读者是那张「存储」卡），
/// 但同一轮里基本信息页改成了一排圆环，其中「存储」那一环要的正是
/// "会话盘已用多少" —— 现在它是 [`Snapshot::storage`] 的读者。
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    /// `wslc info`
    pub info: Option<SystemInfo>,
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
    /// WSL 虚拟机拿到几个 CPU 核。
    ///
    /// **在采集里读、不在渲染里读**：来自 `.wslconfig` 的 `[wsl2] processors`，
    /// 没配就退回 Windows 的 `NUMBER_OF_PROCESSORS` —— 读环境变量是 IO，
    /// 每帧都读不合适（和 `wslconfig` 那条同一个理由）。
    ///
    /// 它只用来把 `wslc stats` 的 CPU%（**100% ≈ 1 个核**）折算成整机占用率，
    /// 见 [`Snapshot::dashboard_stats`]。
    pub wsl_cpu_cores: Option<u32>,
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

    /// 基本信息页那一排圆环要的数字。
    ///
    /// **纯算术**（只读快照里的字段），所以渲染每帧算都行 —— 但它是纯逻辑，
    /// 断言必须留在能跑的地方：这个 crate 的测试 CI 会跑到，`views.rs` 的不会。
    ///
    /// # 口径（界面上每一环下面那行小字就是照这里写的）
    ///
    /// - **CPU**：各运行中容器的 `CPUPerc` **之和**。`wslc stats` 用的是
    ///   Docker 那条公式（WSL 源码里核过，见
    ///   [`wslc_core::wslconfig::processors`] 的说明），**100% ≈ 1 个核** ——
    ///   所以想变成"整机占用率"要除以核数，除以几由 `wsl_cpu_cores` 决定。
    ///   ⚠️ 只算容器：发行版自身、面板自己、Windows 上的其它进程都不在里面，
    ///   它**不是** Task Manager 里那个 CPU 占用。
    /// - **内存**：各容器的 `MemUsage` 分子之和 / 分母（分母是 WSL 的内存上限，
    ///   各容器报的是同一个数，所以取任意一个都行）。
    /// - **存储**：会话 VHD 在宿主机上实际占的字节 ÷ **所在盘总量** ——
    ///   镜像、容器可写层、卷都在这个 VHD 里（`wslc_core::storage` 的说明），
    ///   所以这一环回答的是"容器这一摊占了这块盘多少"。
    ///   ⚠️ 分母刻意**不是**"整块盘已用了多少"：那样读数是**这台电脑**的磁盘
    ///   占用率，和面板管的这一摊没关系（实机反馈就是这个）。
    pub fn dashboard_stats(&self) -> DashboardStats {
        let mut cpu_percent = 0.0f64;
        let mut mem_used = 0u64;
        let mut mem_limit: Option<u64> = None;

        for container in &self.running {
            // 没有 stats 的（`wslc stats` 那一轮漏了它）按"不计入"处理：
            // 把缺失当成 0 会让合计看起来比真实占用小，那不是"少算一点"，
            // 是**编了一个数**。
            let Some(stats) = container.stats.as_ref() else {
                continue;
            };

            if let Some(percent) = stats.cpu_percent().filter(|value| value.is_finite()) {
                cpu_percent += percent;
            }

            if let Some((used, limit)) = stats.mem_usage_parts() {
                if let Some(bytes) = parse_size(used) {
                    mem_used = mem_used.saturating_add(bytes.max(0.0) as u64);
                }
                // 上限取"任一个报出来的"：同一个虚拟机里的容器报的是同一个数。
                // 上限是 `0B`（这是 `wslc` 在容器刚起来时的样子）就当没有。
                if mem_limit.is_none() {
                    mem_limit = parse_size(limit)
                        .filter(|bytes| *bytes > 0.0)
                        .map(|b| b as u64);
                }
            }
        }

        DashboardStats {
            running: self.running.len(),
            total: self.all.len(),
            cpu_percent,
            cpu_cores: self.wsl_cpu_cores,
            mem_used,
            mem_limit,
            disk: self.storage.as_ref().and_then(|storage| storage.volume),
            vhd_bytes: self.storage.as_ref().and_then(|storage| storage.vhd_bytes),
        }
    }

    /// 镜像页顶部那句「N 条记录（M 个镜像）· 合计 X」。
    ///
    /// # 为什么必须按 ID 去重
    ///
    /// `wslc images` 对**同一个镜像**会给出多行（不同 `Repository` 引用指向
    /// 同一个 `ID`）—— 真机夹具里 `e2ac70e7319a` 就同时是
    /// `docker.1panel.live/library/hello-world` 和 `hello-world`。
    /// 直接 `sum(Size)` 会把它的体积算两遍；直接 `len()` 会把记录数当镜像数。
    ///
    /// ID 为空时退回 `reference()` 当键（理论上不该发生，但别让空串把
    /// 所有无 ID 的行并成一条）。
    pub fn image_totals(&self) -> (usize, u64) {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut bytes = 0u64;

        for image in &self.images {
            let key = if image.id.trim().is_empty() {
                image.reference()
            } else {
                image.id.clone()
            };
            if !seen.insert(key) {
                continue;
            }
            bytes = bytes.saturating_add(image.size_bytes().unwrap_or(0.0).max(0.0) as u64);
        }

        (seen.len(), bytes)
    }
}

/// 「基本信息」页那四个圆环的数字。
///
/// 由 [`Snapshot::dashboard_stats`] 算出来 —— 单独一个类型而不是一堆散字段，
/// 是为了让"这一排圆环读了哪些东西"一眼能看全：它们全部来自快照，**不发起
/// 任何采集**。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DashboardStats {
    /// 运行中的容器数。
    pub running: usize,
    /// 全部容器数（含已停止）。
    pub total: usize,
    /// 各运行中容器的 CPU% 之和（**100% ≈ 1 个核**）。
    pub cpu_percent: f64,
    /// 折算整机占用率要除的核数；拿不到时 `None`（那就只报原始合计）。
    pub cpu_cores: Option<u32>,
    /// 各运行中容器的内存占用之和（字节）。
    pub mem_used: u64,
    /// WSL 的内存上限（字节）；一个容器都没在跑时报不出来，为 `None`。
    pub mem_limit: Option<u64>,
    /// 会话存储所在卷的容量；拿不到时为 `None`。
    ///
    /// 它在这个页面上只当**分母**用（"会话 VHD 占了这块盘的多少"）——
    /// 见 [`DashboardStats::vhd_percent_of_disk`]。
    pub disk: Option<VolumeSpace>,
    /// 当前会话 VHD 的实际占用；文件还没建出来时为 `None`。
    ///
    /// 它是「存储」那一环的**分子**（镜像、容器可写层、卷都在这个 VHD 里）。
    pub vhd_bytes: Option<u64>,
}

impl DashboardStats {
    /// 容器这一摊折算成"整机占用率"（0.0–100.0）。
    ///
    /// 拿不到核数时返回 `None` —— 那时候**不能**拿原始合计当百分比用：
    /// 多核机器上 400% 的合计是四个核跑满，画成 400% 的环只会误导。
    pub fn cpu_whole_machine_percent(&self) -> Option<f64> {
        let cores = self.cpu_cores.filter(|cores| *cores > 0)?;
        let percent = self.cpu_percent / f64::from(cores);
        percent.is_finite().then(|| percent.clamp(0.0, 100.0))
    }

    /// 容器占用折算成"用了几个核"（环下面那行小字：`0.22 / 32 核`）。
    pub fn cpu_cores_used(&self) -> f64 {
        self.cpu_percent / 100.0
    }

    /// 容器内存占 WSL 上限的百分比（0.0–100.0）。
    ///
    /// 上限读不到（一个容器都没在跑）时返回 `None`。
    pub fn mem_percent(&self) -> Option<f64> {
        let limit = self.mem_limit.filter(|limit| *limit > 0)?;
        let percent = self.mem_used as f64 / limit as f64 * 100.0;
        percent.is_finite().then(|| percent.clamp(0.0, 100.0))
    }

    /// 会话 VHD 占**所在盘总量**的百分比（0.0–100.0）。
    ///
    /// # 分子是 VHD，不是"整块盘已用了多少"
    ///
    /// 这一环要回答的是"**容器这一摊**在这块盘里占了多少" —— 镜像、容器可写层、
    /// 卷都在那个 `storage.vhdx` 里（`wslc_core::storage` 的说明），所以分子用
    /// 它在宿主机上实际占的字节（`vhd_bytes`）。
    ///
    /// 换成"整块盘已用 / 总量"的话，环读数是**这台电脑**的磁盘占用率
    /// （本机 50%），和面板管的这一摊没关系 —— 用户就是这么反馈的。
    ///
    /// 分母拿不到（读不到卷容量）或 VHD 还没建出来时返回 `None`。
    pub fn vhd_percent_of_disk(&self) -> Option<f64> {
        let total = self.disk.filter(|disk| disk.total > 0)?.total;
        let vhd = self.vhd_bytes?;
        let percent = vhd as f64 / total as f64 * 100.0;
        percent.is_finite().then(|| percent.clamp(0.0, 100.0))
    }
}

/// 阻塞式采集一次完整快照。
///
/// 调用方必须在**后台线程/执行器**上调用（见 `app::Shell::refresh`）：
/// 内部会串行跑 6 条 `wslc` 命令 + 1 条 `wsl` 命令 + 1 条 `reg` 命令。
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

    // ⚠️ 这里**曾经**还会跑一条 `wslc system session list` 并把结果放进
    // `Snapshot::sessions`。v0.9.16 去掉了：活动会话只被「基本信息」页那张
    // 被删掉的卡消费，而同一份数据 `wslc info` 的 `Server.Sessions` 里已经有了
    // —— 也就是说那是一个**每 3 秒白起的子进程**。

    // ⚠️ 一次拿两份（运行中 / 全部）：`stats` 每次约 2 秒，分别调
    // `running_summaries` + `all_summaries` 会让每一轮刷新白等两次。
    // 详见 `cmd::container::summaries` 的说明。
    match cmd::container::summaries(wslc) {
        Ok((running, all)) => {
            snap.running = running;
            snap.all = all;
        }
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

    // 会话存储位置与占用：`storagePath` 来自 `settings.yaml`，
    // 会话名来自 `wslc info`。这一步纯文件系统 + 一次卷容量查询，
    // 不会失败到需要报错 —— 拿不到就是 `None`（界面上那一环显示 `—`）。
    //
    // ⚠️ 会话名从 `snap.info` 里取，**不用**那条 `wslc system session list`：
    // 两份数据是一样的（见 `Snapshot` 上面那段），而后者要多起一个进程。
    let configured = settings_storage_path(snap.info.as_ref());
    let session = snap
        .info
        .as_ref()
        .and_then(|info| info.server.sessions.first())
        .map(|session| session.name.clone());
    snap.storage = wslc_core::storage::inspect(configured.as_deref(), session.as_deref());

    // `.wslconfig`：纯文件读取（几毫秒），没有进程开销，所以每轮都读。
    snap.wslconfig = load_wslconfig();

    // CPU 核数：`.wslconfig` 的 `[wsl2] processors`，没配就退回机器核数。
    // 读它只为一件事 —— 把 `wslc stats` 的 CPU%（100% ≈ 1 个核）折算成
    // 整机占用率时不至于差好几倍，见 `Snapshot::dashboard_stats`。
    snap.wsl_cpu_cores = wsl_cpu_cores(snap.wslconfig.text.as_deref());

    snap.elapsed_ms = started.elapsed().as_millis();
    snap
}

/// WSL 虚拟机拿到几个 CPU 核。
///
/// 优先 `.wslconfig` 的 `[wsl2] processors`（用户限过核数就得按它算），
/// 没有就退回 Windows 的 `NUMBER_OF_PROCESSORS` —— 那正是 WSL 不限核时的行为。
///
/// 两个都拿不到时返回 `None`：界面上那时候**不报百分比**，只报原始合计
/// （见 [`DashboardStats::cpu_whole_machine_percent`]）。
fn wsl_cpu_cores(config: Option<&str>) -> Option<u32> {
    config
        .and_then(wslc_core::wslconfig::processors)
        .or_else(|| {
            std::env::var("NUMBER_OF_PROCESSORS")
                .ok()?
                .trim()
                .parse::<u32>()
                .ok()
                .filter(|count| *count > 0)
        })
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
    /// **停止所有正在运行的容器**（「容器」页工具栏上那颗「停止全部」）。
    ///
    /// 为什么不是 `StopAllContainers(Vec<String>)`：那样带走的是一份
    /// **点按钮那一刻**的快照，而用户从确认框弹出来到点「全部停止」之间
    /// 可能又起了别的容器。执行时现查 `list_running` 拿到的才是"现在正在跑的"，
    /// 这和 [`PendingAction::PruneContainers`] 是一致的 —— 那个也是执行时
    /// 才去问 `wslc`，不带着名单走。
    ///
    /// `running` **只用于文案**：确认框一开就盖住了工具栏上那行"N 个在运行"，
    /// 影响面得在正文里再写一遍（和 `DistroAction::Unregister` 带 `vhdx_bytes`
    /// 是同一条理由：代价要在点之前说清）。它**不参与任何判断** ——
    /// 真要停哪些以 `execute` 当场查到的那份为准，两者不一致时以执行结果为准。
    StopAllContainers {
        /// 请求时快照里正在运行的容器数，仅供确认文案。
        running: usize,
    },
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
    /// 删除一个**数据库**（数据库管理页）。
    ///
    /// 为什么要带凭据：`PendingAction::execute` 只拿到 `&Wslc`，
    /// 而 `DROP DATABASE` 得先连上 mysqld —— 连接信息必须随动作一起过来。
    /// （`MysqlCreds` 的 `Debug` 是脱敏的，所以进 `AppState::confirm` 是安全的。）
    DropDatabase {
        /// 目标容器。
        container: String,
        /// 库名。
        database: String,
        /// 容器里的客户端二进制名（探测得来，别在这里再猜一次）。
        client: String,
        /// 连接凭据。
        creds: MysqlCreds,
    },
    /// 删除一个 **MySQL 用户**。
    ///
    /// `'user'@'host'` 才是完整身份（实测 `root@%` 和 `root@localhost` 是两条
    /// 独立的记录），所以两个都要带着。
    DropUser {
        /// 目标容器。
        container: String,
        /// 用户名。
        user: String,
        /// 主机。
        host: String,
        /// 容器里的客户端二进制名。
        client: String,
        /// 连接凭据。
        creds: MysqlCreds,
    },
    /// **恢复**一份备份（v0.8）—— 覆盖该库的现有数据，所以走二次确认。
    ///
    /// dump 从 **Windows 侧存档**送进容器再恢复（`push_dump_to_container`
    /// 的注释里有那三步的实测依据）。所以容器重建过、`/tmp` 被清过都**不影响**
    /// —— 只要那份 Windows 文件还在，恢复就能进行。
    RestoreDatabase {
        /// 目标容器。
        container: String,
        /// 要恢复的库（dump 是 `--databases` 出来的，恢复回**原名**）。
        database: String,
        /// 容器里的客户端二进制名。
        client: String,
        /// **Windows 侧**那份 dump 的路径（恢复的输入）。
        windows_path: std::path::PathBuf,
        /// 连接凭据。
        creds: MysqlCreds,
    },
    /// **删除**一份备份文件（v0.10.4 补的二次确认）。
    ///
    /// # 为什么它必须有确认，而「恢复」本来就有
    ///
    /// 原先正好反着：**恢复**（覆盖数据、但旧库还能再备份一份）有确认，
    /// **删除**（Windows 侧文件 + 容器内 `rm -f`，一起没，没有任何地方能找回来）
    /// 反而直接执行。用户在备份浮层里连着点两下就够了 —— 那份备份不可再生。
    ///
    /// 它不能像别的动作那样塞进 `PendingAction::execute`：删一份备份要做
    /// "删本地文件 + 进容器 rm + 重扫目录"三件事，而 `execute` 的形状是
    /// "跑一条 `wslc` 命令 → 返回字符串"。所以这里只带**够定位那一条**的字段，
    /// 真正的执行仍在 `Shell::delete_db_backup_confirmed` 里。
    DeleteDatabaseBackup {
        /// 目标容器（容器内那份也要删）。
        container: String,
        /// 这份备份属于哪个库（只用于文案）。
        database: String,
        /// **Windows 侧**那份文件的路径（主要产物，删不掉要报错）。
        windows_path: std::path::PathBuf,
        /// **容器内**那份的路径（尽力删，删不掉不影响结果）。
        container_path: String,
    },
}

impl PendingAction {
    /// 确认弹窗标题。
    pub fn title(&self) -> &'static str {
        match self {
            PendingAction::StopContainer(_) => "停止容器",
            PendingAction::StopAllContainers { .. } => "停止所有容器",
            PendingAction::KillContainer(_) => "强制终止容器",
            PendingAction::RemoveContainer(_) => "删除容器",
            PendingAction::PruneContainers => "清理已停止的容器",
            PendingAction::RemoveImage(_) => "删除镜像",
            PendingAction::RemoveNetwork(_) => "删除网络",
            PendingAction::RemoveVolume(_) => "删除卷",
            PendingAction::PruneVolumes => "清理未使用的卷",
            PendingAction::DropDatabase { .. } => "删除数据库",
            PendingAction::DropUser { .. } => "删除 MySQL 用户",
            PendingAction::RestoreDatabase { .. } => "恢复备份",
            PendingAction::DeleteDatabaseBackup { .. } => "删除备份文件",
        }
    }

    /// 确认弹窗正文。
    pub fn body(&self) -> String {
        match self {
            PendingAction::StopContainer(name) => {
                format!("将向容器 {name} 发送停止信号。容器内的未保存数据会丢失。")
            }
            PendingAction::StopAllContainers { running } => format!(
                "将向**所有正在运行的容器**（{running} 个）发送停止信号。\
                 这些容器里未保存的数据会丢失；已经停着的容器不受影响。"
            ),
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
            PendingAction::DropDatabase { database, .. } => format!(
                "将执行 `DROP DATABASE {database}` —— 这个库里**所有表和数据**都没了。\
                 此操作不可撤销，面板里也没有回收站。\
                 （挂着的卷不受影响：库是 mysqld 管的，删库不删卷。）"
            ),
            PendingAction::DropUser { user, host, .. } => format!(
                "将执行 `DROP USER '{user}'@'{host}'` —— 这个账号会被删掉，\
                 已经授出去的权限一并消失。已经建立的连接不会立刻断开，\
                 但之后它就连不上了。此操作不可撤销。"
            ),
            PendingAction::RestoreDatabase {
                database,
                windows_path,
                ..
            } => format!(
                "将把备份 `{}` 恢复进数据库 **{database}** ——\
                 这个库**现在的全部数据会被覆盖**（dump 是 `--databases` 形式，\
                 恢复只能回原库）。此操作不可撤销。",
                windows_path.display()
            ),
            PendingAction::DeleteDatabaseBackup {
                container,
                database,
                windows_path,
                container_path,
            } => format!(
                "将**永久删除**这份备份：\n\
                 {}\n\
                 （容器 {container} 里的 `{container_path}` 也一起删。）\n\n\
                 数据库 {database} 本身不受影响，但这份存档再也没有第二份 —— \
                 面板里没有回收站，删了就得重新备份。",
                windows_path.display()
            ),
        }
    }

    /// 按钮文案。
    pub fn confirm_label(&self) -> &'static str {
        match self {
            PendingAction::StopContainer(_) => "停止",
            PendingAction::StopAllContainers { .. } => "全部停止",
            PendingAction::KillContainer(_) => "强制终止",
            PendingAction::RemoveContainer(_) | PendingAction::PruneContainers => "删除",
            PendingAction::RemoveImage(_) => "删除镜像",
            PendingAction::RemoveNetwork(_) => "删除网络",
            PendingAction::RemoveVolume(_) => "删除卷",
            PendingAction::PruneVolumes => "清理",
            PendingAction::DropDatabase { .. } => "删除数据库",
            PendingAction::DropUser { .. } => "删除用户",
            PendingAction::RestoreDatabase { .. } => "恢复",
            PendingAction::DeleteDatabaseBackup { .. } => "永久删除",
        }
    }

    /// 执行操作，返回给用户看的结果摘要。
    pub fn execute(&self, wslc: &Wslc) -> Result<String> {
        match self {
            PendingAction::StopContainer(name) => {
                let done = cmd::container::stop(wslc, std::slice::from_ref(name))?;
                Ok(format!("已停止：{}", done.join("、")))
            }
            // 名单现查，不用界面上那份快照（理由写在枚举上那段注释里）。
            //
            // 一个都没在跑时**不能**照样调 `stop`：`cmd::container::stop` 收到空名单
            // 会返回 `InvalidArgument`（`run_names` 里那道检查），于是"确认框开着
            // 的时候容器已经被别的操作停掉了"会变成一条报错 —— 那不是错误，
            // 是用户想要的结果已经达成了。
            //
            // 一条命令停多个，而不是在这里自己循环。实测（本机 WSL 3.0.3.0）：
            //
            // - `wslc stop nginx redis` → 两行 stdout（名字，顺序与传入一致）、退出码 0
            //   —— 正好是 `run_names` 收集的那份，所以摘要里的名字是准的；
            // - 停一个**已经停着**的容器 → 退出码 0（没有"它没在跑"这种报错）；
            // - 只有"名字不存在"才失败，而且是**逐个**报错、把整个名单走完再以 1 退出
            //   （`wslc container stop no-such-a no-such-b` → 两条
            //   `WSLC_E_CONTAINER_NOT_FOUND`）。
            //
            // 也就是说"列表和执行之间容器自己退了"不会变成失败；真会失败的只剩
            // "那一瞬间它被删掉了"这种很窄的竞态 —— 那种情况下其余容器照样停了，
            // 错误信息里带着是哪个名字。拆成 N 条命令换不来什么，只多 N 次进程开销。
            PendingAction::StopAllContainers { .. } => {
                let running = cmd::container::list_running(wslc)?;
                if running.is_empty() {
                    return Ok("没有正在运行的容器".to_owned());
                }
                let names: Vec<String> = running
                    .iter()
                    .map(|c| c.display_name().to_owned())
                    .collect();
                let done = cmd::container::stop(wslc, &names)?;
                Ok(stopped_all_message(&done))
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
            // 数据库管理页的两条危险动作：走容器里的 mysql 客户端执行 DDL。
            // `run_sql` 内部已经把错误脱敏（`cmd::mysql::sanitize`），
            // 所以密码不会跟着错误冒到提示条上。
            PendingAction::DropDatabase {
                container,
                database,
                client,
                creds,
            } => {
                let sql = wslc_core::model::mysql::drop_database_sql(database)?;
                cmd::mysql::run_sql(wslc, container, client, creds, &sql, None, SqlMode::Batch)?;
                Ok(format!("已删除数据库：{database}"))
            }
            PendingAction::DropUser {
                container,
                user,
                host,
                client,
                creds,
            } => {
                let sql = wslc_core::model::mysql::drop_user_sql(user, host)?;
                cmd::mysql::run_sql(wslc, container, client, creds, &sql, None, SqlMode::Batch)?;
                Ok(format!("已删除用户：'{user}'@'{host}'"))
            }
            // 恢复：把 Windows 侧那份 dump 送进容器，再从容器内重定向进 mysql。
            //
            // 这里**不**假设容器里还有一份旧的 dump（实测：容器重建 / WSL 重启后
            // `/tmp` 里那份就没了）。送进去之前先看本地文件在不在 —— 用户可能
            // 在文件管理器里把它删了，那时要给一句人话，而不是 cp 的引擎报错。
            PendingAction::RestoreDatabase {
                container,
                database,
                client,
                windows_path,
                creds,
            } => {
                if !windows_path.is_file() {
                    return Err(wslc_core::Error::Unsupported(format!(
                        "备份文件不在了：{}。它可能被手动删掉了 —— 备份列表里\
                         会跟着消失，重新备份一份即可。",
                        windows_path.display()
                    )));
                }
                let container_path =
                    cmd::mysql::push_dump_to_container(wslc, container, windows_path)?;
                // `None` = 由 dump 自己的语句决定往哪灌（备份是 `--databases` 出来的）
                cmd::mysql::load_sql_file(wslc, container, client, creds, &container_path, None)?;
                Ok(format!("已恢复数据库：{database}（现有数据已被备份覆盖）"))
            }
            // 删除备份**不在这里执行**。
            //
            // 理由：它要做三件事（删本地文件 → 进容器 `rm -f` → 重扫目录
            // 并写回 `AppState`），而 `execute` 的形状是"跑一条 `wslc` 命令 →
            // 返回字符串"——最后一件事它表达不了（`&self`，拿不到 `AppState`）。
            // 这个分支只是为了 match 完备：`views::confirm_overlay` 会把这一类
            // 分流给 `Shell::delete_db_backup_confirmed`，走不到这里。
            //
            // 不用 `unreachable!()`：真要漏进来，宁可给一句能读的错误，
            // 也不要在用户的机器上 panic（和 `submit_prompt` 那几个分支同理）。
            PendingAction::DeleteDatabaseBackup { .. } => Err(wslc_core::Error::InvalidArgument(
                "删除备份不走这条同步路径（见 Shell::delete_db_backup_confirmed）".to_owned(),
            )),
        }
    }
}

// 自动刷新间隔不再是界面上的档位开关，而是 `prefs.rs` 里的持久化偏好。
// 界面只负责在"设置"页里改它。

/// 「停止全部」的结果摘要（成功提示条上那一行）。
///
/// 单独一个纯函数是为了**能测**：这句话只出现在提示条上，写错了的表现是
/// "用户不知道刚才到底停掉了哪个/几个"，而这类缺陷在 CI 里除了编译之外
/// 没有任何信号（`AGENTS.md` §8）。
///
/// 名字超过 [`STOP_ALL_NAMES_IN_SUMMARY`] 个就截断：提示条不是日志，
/// 二十个容器名会把整条撑成好几行，把后面真正的错误信息挤出去。
fn stopped_all_message(done: &[String]) -> String {
    if done.is_empty() {
        // `wslc stop` 一行都没打印。正常不该发生（它会把停掉的容器名打出来），
        // 但报"已停止 0 个容器"读起来像成功了却什么都没发生 —— 如实说。
        return "停止命令已执行，但 wslc 没有报告任何容器".to_owned();
    }
    let shown = done
        .iter()
        .take(STOP_ALL_NAMES_IN_SUMMARY)
        .cloned()
        .collect::<Vec<_>>()
        .join("、");
    if done.len() > STOP_ALL_NAMES_IN_SUMMARY {
        format!("已停止 {} 个容器：{shown} 等", done.len())
    } else {
        format!("已停止 {} 个容器：{shown}", done.len())
    }
}

/// 摘要里最多列几个容器名（余下的用"等"带过）。
const STOP_ALL_NAMES_IN_SUMMARY: usize = 5;

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

/// 启动时读一次数据库账号存档。
///
/// 拆成自由函数是因为它**碰文件系统**：`AppState::new` 的构造表达式里
/// 塞 IO 会让那个函数没法一眼看完，而且测试也注入不进去。
fn db_credentials_at_startup() -> (crate::dbcreds::DbCredentialStore, Option<String>) {
    crate::dbcreds::DbCredentialStore::load(&crate::dbcreds::store_path(
        crate::prefs::app_dir().unwrap_or_default(),
    ))
}

/// 启动时读一次部署记录存档（理由同 [`db_credentials_at_startup`]）。
///
/// ⚠️ 和那个不同的是两点：
///
/// 1. **这个的第二项必须真的显示到界面上**（`AppState::deploy_store_error`
///    → 部署页的提示条）。`db_credentials_error` **曾经**算出来了却没人读，
///    是个洞 —— 后来补上了（数据库页的 `creds_warning` 那一条，见
///    `views::database`）。写这里是为了记住那类错误的形状：
///    算出来 → 塞进 `AppState` → **忘了画**，是最容易静默失效的一种。
/// 2. **`LOCALAPPDATA` 没设置时不去猜路径**。`db_credentials_at_startup` 用的是
///    `app_dir().unwrap_or_default()`，那会得到**相对当前工作目录**的
///    `deployments.json` —— 读的时候降级到 CWD、写的时候（`save_deploy_store`）
///    明确拒绝，两者行为不一致。这里两边一致：都不做，并且**说出来**。
fn deploy_store_at_startup() -> (crate::deploys::DeployStore, Option<String>) {
    let Some(dir) = crate::prefs::app_dir() else {
        return (
            crate::deploys::DeployStore::default(),
            Some(
                "LOCALAPPDATA 未设置 —— 部署记录存不了盘，这一次部署的记录重开面板就没了"
                    .to_owned(),
            ),
        );
    };
    crate::deploys::DeployStore::load(&crate::deploys::store_path(dir))
}

/// 扫一个备份目录，按**时间倒序**返回（新的在上）。
///
/// 只认 `<库名>-<YYYYMMDD>-<HHMMSS>.sql` 这个形状（面板自己生成的）；
/// 不认识的文件**跳过** —— 目录里可能有用户手动拷进来的东西，报错只会烦人。
///
/// ⚠️ 碰文件系统 —— 只在**点击后**和**长任务完成回调**里调，
/// 绝不在渲染路径上（`AGENTS.md` §7.1）。
pub fn scan_backup_dir(dir: &std::path::Path) -> std::io::Result<Vec<BackupEntry>> {
    let mut out: Vec<BackupEntry> = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some((database, stamp)) = parse_backup_file_name(file_name) else {
            continue;
        };
        let bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
        out.push(BackupEntry {
            container_path: format!(
                "{}/{}",
                wslc_core::model::mysql::CONTAINER_BACKUP_DIR,
                file_name
            ),
            database,
            file_name: file_name.to_owned(),
            path,
            bytes,
            stamp,
        });
    }
    // 文件名里的时间戳是定宽的，字典序 == 时间序
    out.sort_by(|a, b| b.stamp.cmp(&a.stamp));
    Ok(out)
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
    /// 什么时候弹出来的 —— 用来算"该不该自己消失"（见 [`Toast::lifetime`]）。
    ///
    /// 时间戳放在这里（而不是 `AppState` 上另开一个字段）是因为提示条是
    /// **单条**的（`toast: Option<Toast>`）：换了新的，时间戳自然跟着新的走，
    /// 不需要额外的簿记，也不会出现"新提示被旧计时器收掉"。
    pub shown_at: std::time::Instant,
}

impl Toast {
    /// 成功提示。
    pub fn success(text: impl Into<String>) -> Self {
        Self::new(text, ToastKind::Success)
    }
    /// 失败提示。
    pub fn error(text: impl Into<String>) -> Self {
        Self::new(text, ToastKind::Error)
    }
    /// 普通提示。
    pub fn info(text: impl Into<String>) -> Self {
        Self::new(text, ToastKind::Info)
    }

    fn new(text: impl Into<String>, kind: ToastKind) -> Self {
        Self {
            text: text.into(),
            kind,
            shown_at: std::time::Instant::now(),
        }
    }

    /// 这条提示**自己收掉**要等多久。
    ///
    /// 分两档：
    /// - 成功 / 一般信息 **4 秒** —— 一眼扫完就行；
    /// - **报错 8 秒** —— 报错文本更长，而且用户往往要照着它去做点别的
    ///   （去改个密码、去看日志），给双倍时间。
    ///
    /// # 为什么要自动收
    ///
    /// 实机反馈："这个下面弹出的提示信息过几秒自动关闭不要手动点，很多这种
    /// 提示信息都需要手动点很麻烦"。手动「关闭」按钮**留着**（想立刻清掉时
    /// 仍然有用），但它不再是一条必经操作。
    pub fn lifetime(&self) -> std::time::Duration {
        match self.kind {
            ToastKind::Error => std::time::Duration::from_secs(8),
            ToastKind::Info | ToastKind::Success => std::time::Duration::from_secs(4),
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
    /// 发行版会继续跑 —— 所以界面上要写明"它由本面板吊住"。
    /// 那句话在**发行版详情弹窗**里（列表行的状态列装不下它，
    /// 曾经硬塞进去，结果溢到隔壁列上）。
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
    /// 启动一条部署（v0.9）—— 面板在容器里起一个受 pidfile 管的进程。
    ///
    /// 参数是**应用名**（不是容器名）：具体跑在哪个容器、哪个目录、哪条命令
    /// 由 `AppState::deploy` + 本地存档决定（`Shell::deploy_action` 去取）。
    /// 刻意不把容器名 / 命令塞进这个变体：那会让"界面上显示的那一条"和
    /// "实际执行的那一条"有两个真相来源。
    StartDeployment(String),
    /// 停止一条部署（只发 `TERM`，不做 `kill -9`）。
    StopDeployment(String),
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

// ---------------------------------------------------------------------------
// 容器里的数据库管理（MySQL / MariaDB）
// ---------------------------------------------------------------------------

/// 「数据库管理」页的 tab。
///
/// ⚠️ **没有「SQL 控制台」这一项**（v0.9.2 去掉的）。实机反馈：
/// "上面不需要导入 sql 和 sql 控制台，都是在具体数据库 database 操作的" ——
/// SQL 是**针对某个库**的事，所以它现在挂在数据库列表每行的「SQL」按钮上、
/// 以一个浮层出现（`Shell::open_db_console_for` + `DbState.sql_database`），
/// 而不是一个"先切 tab、再想连哪个库"的入口。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DbTab {
    /// 数据库列表 + 新建 / 删除。
    #[default]
    Databases,
    /// MySQL 用户列表 + 新建 / 改密码 / 授权 / 删除。
    Users,
}

impl DbTab {
    /// 全部 tab（决定界面上的顺序）。
    pub const ALL: [DbTab; 2] = [Self::Databases, Self::Users];

    /// 标签。
    pub fn label(self) -> &'static str {
        match self {
            Self::Databases => "数据库",
            Self::Users => "用户",
        }
    }
}

/// 一次数据库操作失败：分类 + 原文。
///
/// 分开存是为了界面上能说**两句话**：一句"该怎么办"（分类给的），
/// 一句原文（排障要用，用户可能还想把它贴给别人看）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbFailure {
    /// 分类（决定给什么建议）。
    pub kind: MysqlErrorKind,
    /// 报错原文。⚠️ 到这里的文本**已经脱敏过**（出口在 `cmd::mysql::sanitize`）。
    pub detail: String,
}

impl DbFailure {
    /// 从一段报错文本分类。
    ///
    /// 退出码不传（`None`）：能走到这里的错误都已经被
    /// [`wslc_core::error::Error`] 格式化过，退出码也写在文本里了
    /// （`… 返回退出码 126：…`），而分类里真正稳的判据是那些**文本**。
    pub fn from_text(detail: impl Into<String>) -> Self {
        let detail = detail.into();
        Self {
            kind: wslc_core::model::mysql::classify_error(&detail, None),
            detail,
        }
    }

    /// 给用户的一句话建议（`None` = 直接看原文就行）。
    pub fn hint(&self) -> Option<&'static str> {
        self.kind.hint()
    }

    /// 提示条上那一句。
    ///
    /// 有建议就给建议；没有就给原文的**第一行**并截断 —— 提示条是一条单行横幅，
    /// 整段 `stderr` 塞进去会把界面撑变形。
    pub fn summary(&self) -> String {
        if let Some(hint) = self.hint() {
            return hint.to_owned();
        }
        let first = self.detail.lines().next().unwrap_or_default().trim();
        truncate_chars(first, 120)
    }
}

/// 按**字符**（不是字节）截断，超长时加省略号。
///
/// 不能按字节切：`&s[..120]` 落在多字节字符中间会 panic，
/// 而这里的输入是**外部命令的输出**（什么字符都可能有）。
fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}

/// 原生连接时代替「容器里的客户端名」的那个**哨兵值**。
///
/// # 为什么需要一个哨兵而不只是 `Option`
///
/// `DbState.client` 被六条写路径直接塞进 `wslc exec <容器> <客户端>`，
/// 而那些路径在不走原生时必须拿到真的二进制名。给它改成
/// `Option<String>` 会把六处调用点全部改成 `let Some(bin) = … else …`，
/// 而收益只是"少一个不可能撞上的字符串" —— 容器里不会有一个叫
/// `native` 的可执行文件，撞上的概率为零。
///
/// 代价是**读它的地方必须知道这个值有特殊含义** —— 所以判据统一收在
/// [`DbState::is_native`] 里，不要在别处再比字符串。
pub const NATIVE_CLIENT: &str = "native";

/// 用户粘进来的连接串（`mysql://root:pw@10.0.0.5:3306/shop`）。
///
/// # 为什么是 newtype 而不是 `Option<String>`
///
/// `DbState` / `AppState` 都是 `#[derive(Debug)]`，而这个字符串**带口令**。
/// 如果它是裸 `String`，那么任何一次 `tracing::debug!("{:?}", state)`
/// 或者 `dbg!()` 都会把口令打进日志文件 —— 而日志是用户会贴到 issue 里的
/// 东西。这个仓库在密码这件事上一直是同一套做法
/// （[`MysqlCreds`] / `DbCredential` 都手写了脱敏 `Debug`），这里保持一致。
///
/// 用一个 newtype 而不是"给 `DbState` 手写整个 `Debug`"：后者要手写几十个
/// 字段，加一个字段忘了补就会**悄悄丢掉**诊断信息；newtype 只影响这一个字段。
///
/// [`MysqlCreds`]: wslc_core::model::MysqlCreds
#[derive(Clone, PartialEq, Eq)]
pub struct DsnText(String);

impl DsnText {
    pub fn new(text: impl Into<String>) -> Self {
        Self(text.into())
    }

    /// 原文。**只给真正要连接的地方用**（解析 / 拼 `ConnSpec`）。
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 是否为空（空串等于"没填"）。
    pub fn is_empty(&self) -> bool {
        self.0.trim().is_empty()
    }
}

impl std::fmt::Debug for DsnText {
    /// 口令段打 `***`，其余原样保留。
    ///
    /// 和 `MysqlCreds` 那种"整段打 `***`"不同：连接串里的**协议 / 主机 /
    /// 端口 / 库名 / 用户名**正是排障要看的东西（"我连的是哪台机器"），
    /// 遮掉它们等于让这条 Debug 失去价值。只遮口令那一段。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DsnText({})", redact_dsn_password(&self.0))
    }
}

/// 把 `scheme://user:password@host…` 里的 `password` 换成 `***`。
///
/// 判据刻意和 `wslc-db::redact_dsn` **一致**（那边是 `wslc-db` 里唯一
/// 有测试的那份实现，但 `wslc-panel-core` 不依赖那个 crate）：
///
/// - 先找 `://`，没有就原样返回（不像连接串的东西，没什么可遮的）；
/// - 再在**权限段**（`://` 到第一个 `/` 或 `?`）里找**最后一个** `@`；
/// - 那之后如果还有 `:`，`:` 到 `@` 之间就是口令。
///
/// ⚠️ 用**最后一个** `@`：口令里出现未编码的 `@` 很常见
/// （`mysql://root:p@ss@h/db`），按第一个切会把主机名当成口令的一部分。
fn redact_dsn_password(text: &str) -> String {
    let Some(scheme_end) = text.find("://") else {
        return text.to_owned();
    };
    let auth_start = scheme_end + 3;
    // 权限段结束：第一个 `/` 或 `?`（都没有就到末尾）
    let auth_end = text[auth_start..]
        .find(['/', '?'])
        .map(|i| auth_start + i)
        .unwrap_or(text.len());
    let auth = &text[auth_start..auth_end];
    let Some(at) = auth.rfind('@') else {
        // 没有 `@` = 没有 userinfo，也就没有口令
        return text.to_owned();
    };
    let userinfo = &auth[..at];
    let Some(colon) = userinfo.find(':') else {
        // `user@host` 形式：没有口令
        return text.to_owned();
    };
    // `user:` + `***` + `@host…`
    format!(
        "{}{}:***{}",
        &text[..auth_start],
        &userinfo[..colon],
        &text[auth_start + at..]
    )
}

/// 「数据库管理」页的状态。
///
/// 纯数据（一个 GPUI 类型都没有），所以住在 `AppState` 里、也能被单测；
/// 页面上那些输入框是 GPUI 的 `Entity<InputState>`，住在 `Shell` 里。
///
/// ⚠️ **密码在这里**（[`MysqlCreds::password`]）—— 它的 `Debug` 是手写脱敏的，
/// 所以这里派生 `Debug` 是安全的。除此之外密码**哪儿都不存**：
/// 不落盘（`prefs.json` 里没有这个字段）、不进日志、不进错误消息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbState {
    /// 目标容器名。
    pub container: String,
    /// 容器镜像引用（页面上显示，让用户确认点对了容器）。
    pub image: String,
    /// 家族（MySQL / MariaDB / Percona）。
    pub family: MysqlFamily,
    /// 容器里的客户端二进制名（探测得来）。
    ///
    /// ⚠️ 走原生连接时这里是 [`NATIVE_CLIENT`]，**不是**二进制名 ——
    /// 拿它去 `wslc exec` 会报"找不到命令"。用 [`DbState::is_native`] 判。
    pub client: String,
    /// 客户端版本那一行（`mysql  Ver 26.7.0 …`）。
    pub client_version: Option<String>,
    /// 用户粘进来的**连接串**（v0.16）。`Some` 时这一页走原生连接。
    ///
    /// # 为什么是可选的字符串而不是 `ConnSpec`
    ///
    /// `ConnSpec` 在 `wslc-db` 里，而 `wslc-panel-core` 不依赖那个 crate
    /// （它带 `postgres` 那棵树，没必要为了一个字段把整棵依赖拉进数据层）。
    /// 所以状态里存原文，**解析在 `wslc-panel` 里做** —— 解析失败要能
    /// 原样报给用户看（"端口不是合法数字：abc" 比 "连接失败" 有用得多）。
    ///
    /// ⚠️ 它**不落盘**：里面有口令。
    ///
    /// ⚠️ 类型是 [`DsnText`] 而不是 `String`：这个结构体派生了 `Debug`，
    /// 裸 `String` 会把口令打进日志（见那个 newtype 的文档）。
    ///
    /// ⚠️ 它也是 [`DbState::is_native`] 的两个判据之一 —— 填了它，那六条
    /// 只能走容器内客户端的写操作就会被拦住（见 `write_blocked_reason`）。
    pub dsn: Option<DsnText>,
    /// 多驱动层的连接（v0.13）。
    ///
    /// # 现在它是什么
    ///
    /// 一份**镜像**：由 `container` + `family` 推出来（`host` 就是那个容器、
    /// `kind` 由家族换算、`addr`/`port` 取引擎默认值）。
    ///
    /// ⚠️ 早先这里写的是"**还没有代码读它**"—— 那句话**已经过时**：
    /// `DbState::is_mysql_family` / `engine_label` 都读 `conn.kind`，
    /// 而 `db.driver()`（`wslc-panel`）也是从它选驱动的。
    /// 留着那句旧注释会让下一个人以为这是个空字段。
    ///
    /// # 为什么要有它
    ///
    /// 现在"数据库"这个概念在代码里不存在，存在的只有"容器"：`run_sql` 的
    /// 第一个参数就是容器名。要支持第二个引擎（PostgreSQL，v0.14），
    /// 就必须有一个"连接"的落点 —— 而这个落点不能是容器，否则永远只能
    /// "从容器行进入"。方案见 `docs/PLAN-multi-db.md`。
    ///
    /// ⚠️ 别急着把 `container` / `family` 删掉：界面上还在显示它们
    /// （"这是 MySQL 8.0 的容器"这种话对用户是有用的），而且
    /// `guess_kind` 的反向推理也需要镜像名。
    pub conn: DbConn,
    /// 连接凭据 —— **上一次连成功的那一份**。
    ///
    /// ⚠️ **它不是执行入口。** 所有动作（备份 / 恢复 / 导入 / SQL / 删库 / 删用户）
    /// 都必须用连接卡上**此刻**的值，唯一入口是 `Shell::db_creds(cx)`。
    ///
    /// 为什么：这个字段只在 `load_db` 里赋值一次，而「从列表行进备份浮层 /
    /// SQL 控制台」会把行上的账号写进连接卡 —— 于是同一页会出现两套账号。
    /// v0.10.3 之前的实现一半用这个字段、一半用连接卡，结果是"点某行的备份
    /// （卡换成 shop_user）再在同一个浮层点恢复"会用 **root** 去恢复，
    /// 报 Access denied 而浮层上明明写着 shop_user。
    ///
    /// 现在它的用途只剩"诊断 / 显示"（和卡上的值对照），写它的地方是
    /// `load_db` 与 `open_db_backup_dialog`（后者跟着卡一起更新）。
    pub creds: MysqlCreds,
    /// 这份凭据是怎么来的（"已从容器环境变量读出 root 密码"这类说明）。
    pub cred_note: Option<String>,
    /// 正在后台跑（探测 / 读列表 / 执行 SQL）。
    pub busy: bool,
    /// **根本性**的问题（不是 MySQL / 镜像里没客户端 / 容器没运行）。
    /// 有值时页面只显示它 —— 那些空表没有意义。
    pub blocked: Option<DbFailure>,
    /// 最近一次操作的失败。
    pub error: Option<DbFailure>,
    /// 数据库列表。
    pub databases: Vec<DbEntry>,
    /// MySQL 用户列表。
    pub users: Vec<DbUser>,
    /// 用户列表**自己**的失败（多半是权限：`ERROR 1142` 读不到 `mysql.user`）。
    ///
    /// # 为什么不复用 `error`
    ///
    /// 实机反馈（v0.16.0）：用普通账号进数据库页时，「用户」这一条查询被拒，
    /// 结果整页顶上挂着一条红色横幅 —— 而「数据库」tab 完全正常、表都列出来了。
    /// 那种画法会让人以为**整个页面坏了**，于是不敢用（明明能用）。
    ///
    /// 所以用户列表的失败只画在用户 tab 里：那是它自己的事。
    pub users_error: Option<DbFailure>,
    /// 成功读过一次没有？
    ///
    /// 用来区分「还没读」和「读到了，确实是空的」—— 两者在界面上是两句话。
    pub loaded: bool,
    /// 当前 tab。
    pub tab: DbTab,
    /// SQL 控制台：上一次执行的语句（回显用）。
    pub last_sql: String,
    /// SQL 控制台的结果：**每条语句一个**（v0.11 起）。
    ///
    /// # 为什么是 `Vec` 而不是一个 `Option<String>`
    ///
    /// 用户可能一次贴进多条语句（`SELECT ...; UPDATE ...; SELECT ...`）。
    /// 一次交给 `mysql -e` 的话多个结果集**直接粘连**（实测，见
    /// `wslc_core::model::sqlstmt` 的模块文档），分不清哪行属于哪条 ——
    /// 所以现在是**客户端切分 + 逐条执行**（`cmd::mysql::run_statements`），
    /// 每条产出一个 [`SqlResult`]，界面上按 tab 分开看。
    pub sql_results: Vec<SqlResult>,
    /// SQL 控制台的**原始** `--table` 输出（v0.11 之前唯一的形态）。
    ///
    /// 留着当"看原始输出"的兜底：排查列宽异常 / 字符集问题时，
    /// mysql 原样打了什么比任何解析结果都有说服力。
    pub sql_raw: Option<String>,
    /// 执行的语句**报错**时的原文（`None` = 没报错）。
    ///
    /// 和 `sql_raw` 分开：错误不是"结果"，它的渲染是红字 + 原样保留换行，
    /// 而结果表格是另一套。
    pub sql_error: Option<String>,
    /// 上一次执行一共花了多少毫秒（页脚那行"· 0.08 s"用）。
    ///
    /// 含 `wslc exec` 的固定开销（实测每条约 70ms）—— 所以它**不是**服务端
    /// 的查询耗时，是"用户从点下到看见结果"的时间。页脚里不说"查询耗时"，
    /// 只说数字加单位，免得被误读。
    pub sql_elapsed_ms: u64,
    /// SQL 控制台跑过没有（区分"还没跑过"和"跑完了没有输出"）。
    pub sql_ran: bool,
    /// SQL 控制台**针对哪个库**（`None` = 不指定，靠语句自己 `USE`）。
    ///
    /// 由数据库列表行上的「SQL」按钮设置（那一行是哪个库就是哪个），
    /// 也可以由控制台里的「不指定」按钮清掉。执行时作为 `mysql -D <库>` 传下去
    /// —— 不指定的话 `SELECT * FROM t` 会报 "No database selected"（实测过）。
    pub sql_database: Option<String>,
    /// SQL 控制台左侧的「库 / 表树」（v0.11.2）。
    ///
    /// # 什么时候读
    ///
    /// 打开控制台时读一次（一条 SQL 拿全，实测 118 行 / 0.08s）。
    /// **不**在每次执行 SQL 后重读 —— 那要多花一次 exec，而用户
    /// 大部分时候并不在改结构。改了 DDL 之后重开控制台就会刷新。
    ///
    /// 读失败（比如账号没权限看 `information_schema`）时是空树，
    /// 界面上就说一句"读不到表结构"，而不是画一个空白的树。
    pub schema_tree: wslc_core::model::schematree::SchemaTree,
    // ⚠️ 这里**不该**再挂一份账号存档的克隆。v0.8 挂过（`credentials`），
    // 而它从来没被同步过 —— 构造函数给的是空的，渲染却读它，于是
    // "建库同时建用户"明明成功（SQL 成功、`db-credentials.json` 也写了）、
    // 列表里那两列却永远是 `—`（实机截图抓到的）。
    // 现在渲染直接用 `AppState::db_credentials`（唯一数据源）。
    /// 正在备份的库（`None` = 空闲）。
    ///
    /// 备份是**长任务**（大库可能几分钟）：照 `ExportProgress` 的样子做 ——
    /// 状态在这里、取消令牌在 `Shell`、界面把按钮换成"备份中…"。
    pub backup: Option<BackupState>,
    /// 备份列表（**Windows 侧**目录里扫出来的）。
    ///
    /// 进页面 / 备份完成 / 删除后重扫。**纯本地目录扫描** —— 不碰容器、不起进程，
    /// 但会 `read_dir` + 每条一次 `metadata()`，所以扫描本身要在**后台执行器**上
    /// 做，再由 [`AppState::apply_backups`] 填进来；只有点击路径
    /// （`open_db_backup_dialog`）才可以同步调 [`AppState::rescan_backups`]。
    pub backups: Vec<BackupEntry>,
    /// 备份列表浮层开没开（开的对象是哪个库；`None` = 关着）。
    pub backup_dialog: Option<String>,
    /// 正在**导入**的 `.sql`（`None` = 空闲）。
    ///
    /// 和备份一样是长任务（大 dump 要几分钟），所以状态放这里、界面把它显示成
    /// 一个进度浮层 —— "点下去到有结果之间界面上必须有变化"（`AGENTS.md` §7.1）。
    ///
    /// ⚠️ 这个浮层**必须有关闭途径**（见 `views::db_import_progress`）：
    /// 导入跑在后台，关掉浮层不影响它继续跑；而没有关闭按钮的话，
    /// 一旦 `container cp` 卡住（容器停了 / 大文件），遮罩会挡住整页，
    /// 用户连"去检查容器"都做不了。
    pub import: Option<ImportState>,
}

/// 一次导入的状态（跑着的那一个）。
///
/// 「导入」= 用户挑一个本地 `.sql` → 送进容器 → `mysql` 读它。
/// 和**恢复**走的是同一条路（`cmd::mysql::load_sql_file`），区别只在
/// `database`：恢复是 `None`（dump 自带 `CREATE DATABASE`），
/// 导入多半要指定库（用户的 dump 常常没有 `USE`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportState {
    /// 文件名（显示用；完整路径太长，弹窗里已经看过一遍了）。
    pub file_name: String,
    /// 往哪个库导（`None` = 由文件里的语句决定）。
    pub database: Option<String>,
    /// 用户点过「知道了」把浮层收起来了。
    ///
    /// ⚠️ **收起来不等于取消**：导入还在后台跑（`container cp` + `mysql <`），
    /// 完成后照样发提示、照样重读列表。这个字段只决定**画不画那个遮罩**。
    ///
    /// 为什么必须有它：浮层带 `.occlude()`，没有关闭途径时一旦卡住
    /// （容器停了 / 几百 MB 的 dump），整页点不动，用户连"去容器页看看"
    /// 都做不了 —— 那是比"看不到进度"更糟的状态。
    ///
    /// 它放在 `ImportState` 里（而不是 `Option` 置 `None`）：`Option` 同时是
    /// **互斥锁**（`start_db_import` 第一句就查它），置 `None` 会放开"再来一份
    /// 导入"的口子。
    pub dismissed: bool,
}

/// 一次备份的状态（跑着的那一个）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupState {
    /// 在备份哪个库。
    pub database: String,
    /// dump 生成在**容器里**的路径（备份那一步的产物）。
    ///
    /// ⚠️ 恢复**不用**它（v0.8.2 起恢复从 Windows 存档重新送一份进去）——
    /// 留着是为了「删除备份」时把容器里这份也清掉。
    pub container_path: String,
    /// 备份文件在 **Windows** 侧的路径（完成后展示 / 打开位置）。
    pub windows_path: std::path::PathBuf,
    /// 最近一条 mysqldump 的 stderr（警告类消息，比如 GTID 那条）。
    ///
    /// 这一版是 `None`：输出直接落在容器内（见 `cmd::mysql` 的模块注释），
    /// 字段留着是给"边 dump 边传"那条路用的。
    pub last_message: Option<String>,
    /// 用户点过「放到后台」把浮层收起来了。
    ///
    /// 和 [`ImportState::dismissed`] 同一个理由：浮层带遮罩，而备份是分钟级的；
    /// 收起来不等于取消（备份没有取消能力，见 `Shell::cancel_db_backup`）。
    pub dismissed: bool,
}

/// 一条备份记录（**Windows 侧**目录里扫出来的文件）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupEntry {
    /// 备份的库（从文件名解出来：`djxm-20261015-035901.sql` → `djxm`）。
    pub database: String,
    /// 文件名（不含目录）。
    pub file_name: String,
    /// Windows 侧完整路径。
    pub path: std::path::PathBuf,
    /// 文件大小（字节）。
    pub bytes: u64,
    /// 备份时间（从文件名解出来的 `YYYYMMDD-HHMMSS`）。
    pub stamp: String,
    /// **容器内**那份备份的路径（备份时 dump 的产地）。
    ///
    /// 由文件名推出来（[`wslc_core::model::mysql::CONTAINER_BACKUP_DIR`] +
    /// 文件名），**不一定还在**（容器重建 / `/tmp` 被清就会消失）。
    /// 只用于「删除备份」时顺手清掉它 —— 恢复不依赖这个字段。
    pub container_path: String,
}

/// 从备份文件名解出 `(库名, 时间戳)`；不像的文件名返回 `None`（跳过）。
///
/// 目录里可能躺着用户手动拷进来的东西 —— 不认识的**跳过**而不是报错。
/// 把界面上的凭据（`MysqlCreds`）转成驱动认的形状（v0.14）。
///
/// 多驱动层只认"用户名 + 口令"两样（见 `db::driver::Credentials`），
/// 而连接卡上那份 `MysqlCreds` 还带着 `host`/`port`（走 socket 时都是 `None`）。
/// 转换点集中在这一个函数，将来连接卡换成通用形状时只改这里。
pub fn to_credentials(creds: &MysqlCreds) -> Credentials {
    Credentials::new(creds.user.clone(), creds.password.clone())
}
pub fn parse_backup_file_name(file_name: &str) -> Option<(String, String)> {
    let raw = file_name.strip_suffix(".sql")?;
    // 形如 `djxm-20261015-035901`：库自身也可能带 `-`，所以从**右边**找两段
    let (head, stamp2) = raw.rsplit_once('-')?;
    let (database, stamp1) = head.rsplit_once('-')?;
    if database.is_empty()
        || stamp1.len() != 8
        || stamp2.len() != 6
        || !stamp1.chars().all(|c| c.is_ascii_digit())
        || !stamp2.chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    Some((database.to_owned(), format!("{stamp1}-{stamp2}")))
}

impl DbState {
    /// 这个数据库该用哪个驱动（v0.14）。
    ///
    /// 返回 `Arc<dyn SqlDriver>` 而不是 `&dyn`：UI 那边要把它**搬进**
    /// `background_executor` 的闭包里（起进程必须离开界面线程，
    /// 见 `AGENTS.md` §7.1），所以需要 `'static + Send + Sync`。
    ///
    /// 每次调用新建一个（都是无状态的单元结构体，代价可以忽略）。
    /// MySQL 家族的驱动要带上**探测到的**客户端名 —— 镜像里可能叫
    /// `mariadb` 或 `mysql`，用错名字就是"命令不存在"。
    pub fn driver(&self) -> std::sync::Arc<dyn SqlDriver> {
        match self.conn.kind {
            DbKind::MySql | DbKind::MariaDb => std::sync::Arc::new(MySqlDriver::with_client(
                self.conn.kind,
                self.client.clone(),
            )),
            DbKind::Postgres => std::sync::Arc::new(PostgresDriver::new()),
        }
    }

    /// 是不是 MySQL 家族。
    ///
    /// 「数据库 / 用户 / 备份恢复」那几块**只有 MySQL 家族支持**（它们依赖
    /// `SHOW DATABASES`、`mysql.user`、`mysqldump`）。界面用这个判断
    /// 该不该画那些入口 —— 而不是画出来再让用户点进去看报错。
    pub fn is_mysql_family(&self) -> bool {
        !matches!(self.conn.kind, DbKind::Postgres)
    }

    /// 界面上"这是哪种库"的那句话。
    ///
    /// MySQL 家族走 `family.label()`（能显示 Percona 这种细分），
    /// PostgreSQL 走 `DbKind::label()`。
    ///
    /// ⚠️ **不要**直接用 `family.label()`：`kind == Postgres` 时那个字段
    /// 是占位值，会显示成 "MySQL"。
    pub fn engine_label(&self) -> &'static str {
        match self.conn.kind {
            DbKind::MySql | DbKind::MariaDb => self.family.label(),
            DbKind::Postgres => DbKind::Postgres.label(),
        }
    }
    /// 页面刚打开、还没探测完时的状态。
    ///
    /// `family` 来自**列表那条启发式**（镜像名 / 命令 / 3306 端口），
    /// 客户端名先用家族的兜底值 —— 这样页面立刻就能画出来，
    /// 而权威判定（`inspect` + 客户端探测）回来后由 [`Self::apply_probe`] 覆盖。
    pub fn provisional(
        container: impl Into<String>,
        image: impl Into<String>,
        family: MysqlFamily,
    ) -> Self {
        let kind = DbKind::from_family(family.label());
        let mut state = Self::provisional_kind(container, image, kind);
        // ⚠️ `from_family` 把 Percona 归到 `MySql`（协议相同），但界面上要显示
        // "Percona" 这个细分 —— 所以家族信息在这里补回去，别让抽象把显示变糙。
        state.family = family;
        state.client = family.fallback_client().to_owned();
        state
    }

    /// 按**引擎**建初始状态（v0.14，多驱动）。
    ///
    /// 和 [`Self::provisional`] 的区别：这个直接吃 `DbKind`，所以能表达
    /// PostgreSQL —— 而 `MysqlFamily` 表达不了。`provisional` 保留原签名
    /// 是为了不动现有调用点，它转调这个函数。
    ///
    /// ⚠️ `family` 字段在 `kind == Postgres` 时是**占位值**，没有意义。
    /// 要显示"这是哪种库"请用 [`Self::engine_label`]，它会按 kind 分流。
    pub fn provisional_kind(
        container: impl Into<String>,
        image: impl Into<String>,
        kind: DbKind,
    ) -> Self {
        // `conn` 要复用容器名，先落成 String（`impl Into<String>` 只能消费一次）。
        let container: String = container.into();
        let conn = DbConn::with_defaults(kind, DbHost::Container(container.clone()));
        let family = match kind {
            DbKind::MySql => MysqlFamily::Mysql,
            DbKind::MariaDb => MysqlFamily::Mariadb,
            // 占位：见上面那段注释。
            DbKind::Postgres => MysqlFamily::Mysql,
        };
        let client = match kind {
            DbKind::Postgres => kind.client_bin().to_owned(),
            _ => family.fallback_client().to_owned(),
        };
        Self {
            dsn: None,
            conn,
            container,
            image: image.into(),
            family,
            client,
            client_version: None,
            creds: MysqlCreds::root(""),
            cred_note: None,
            // ⚠️ **必须 false**。这里一开始写的是 `true`（想着"马上就要去探测，
            // 先算忙"），结果 `Shell::load_db` 的第一句就是"正在跑就不叠第二次"
            // —— 于是**第一次**加载被自己挡掉了，页面永远停在「正在读取…」，
            // 页头按钮永远是「读取中…」，实机（v0.8.0）就是这样。
            //
            // 「忙」的语义只能是"**已经有一个后台任务在跑**"：新建出来的状态
            // 没有任务在跑，忙不忙要由真正起任务的那个函数（`load_db`）来置。
            // 页面在 `load_db` 置位**之前**不会渲染一帧（同一个事件回调里），
            // 所以不需要靠这个标志来撑住"读取中"的样子。
            busy: false,
            blocked: None,
            error: None,
            databases: Vec::new(),
            users: Vec::new(),
            users_error: None,
            loaded: false,
            tab: DbTab::default(),
            last_sql: String::new(),
            sql_results: Vec::new(),
            sql_raw: None,
            sql_error: None,
            sql_elapsed_ms: 0,
            sql_ran: false,
            sql_database: None,
            schema_tree: wslc_core::model::schematree::SchemaTree::default(),
            backup: None,
            backups: Vec::new(),
            backup_dialog: None,
            import: None,
        }
    }

    /// 一次探测之后建初始状态。
    ///
    /// root 密码如果写在容器环境变量里，**直接填好** —— 那是用户自己在
    /// `-e MYSQL_ROOT_PASSWORD=…` 里给的，再让他手抄一遍没有意义。
    /// 其它情况（空密码 / 随机密码 / 什么都没有）只给一句说明。
    pub fn from_probe(probe: &wslc_core::cmd::mysql::MysqlProbe, image: impl Into<String>) -> Self {
        let mut state = Self::provisional(probe.container.clone(), image, probe.family);
        state.apply_probe(probe);
        state
    }

    /// 探测结果回来了：覆盖字段，但**绝不覆盖用户已经填好的密码**。
    ///
    /// 那条判断（"密码为空 且 用户名还是默认的 root"）是刻意的：
    /// 用户可能刚在连接卡上换成 `app` 账号并留空密码（那是这个账号真的没密码），
    /// 这时把 root 的密码塞进去会**连错账号**。
    pub fn apply_probe(&mut self, probe: &wslc_core::cmd::mysql::MysqlProbe) {
        self.family = probe.family;
        self.client = probe.client.bin.clone();
        self.client_version = probe.client.version.clone();
        if self.creds.password.is_empty() && self.creds.user == "root" {
            if let RootCredHint::Password(password) = &probe.root_hint {
                self.creds.password = password.clone();
            }
        }
        // 说明只写一次（用户自己改过账号之后就别再提环境变量了）
        if self.cred_note.is_none() {
            self.cred_note = probe.root_hint.note();
        }
        self.busy = false;
    }

    /// 这一页现在走的是**原生连接**吗（`wslc-db`，不经过容器里的客户端）。
    ///
    /// # 为什么看两处
    ///
    /// - `dsn.is_some()`：用户已经点了「应用连接串」，执行路径已经改了；
    /// - `client == NATIVE_CLIENT`：探测回来、`apply_probe` 写进去的那个哨兵。
    ///
    /// 只看 `client` 会漏掉"刚点应用、还没读回来"那一小段（那时 `client`
    /// 还是上一个容器的 `mysql`）；只看 `dsn` 则对"探测说原生、但 dsn 被
    /// 别处清掉"这种不一致不设防。两个都看，任何一边为真就算原生 ——
    /// 对这个判据来说，**宁可多拦一次，也别让一条命令去容器里找一个叫
    /// `native` 的可执行文件**。
    pub fn is_native(&self) -> bool {
        self.dsn.is_some() || self.client == NATIVE_CLIENT
    }

    /// 这一页**不能**做哪些写操作；能全做时返回 `None`。
    ///
    /// # 为什么需要这条判据
    ///
    /// 备份 / 恢复 / 导入 / 建库建用户 / 删库 / 删用户这六条，走的都是
    /// `wslc exec <容器> <客户端> …` —— 它们要的是**容器里的**
    /// `mysql` / `mysqldump`。而原生连接根本不经过容器（可以连一台
    /// 远程机器），`db.client` 那时是 [`NATIVE_CLIENT`] 这个哨兵值。
    ///
    /// 不拦的后果不是"报错"，而是**报一个指错方向的错**：用户点「备份」，
    /// 拿到的是 `exec: native: not found` 之类，看上去像容器坏了 ——
    /// 而真正的原因是这条路径压根没接原生驱动。
    ///
    /// ⚠️ 判据放这里而不是渲染层：渲染层决定"按钮画不画"，而**点下去的
    /// 那一刻**才是真正要判的地方（浮层可能是几分钟前打开的，那期间
    /// 用户可能刚应用了一条连接串）。
    pub fn write_blocked_reason(&self) -> Option<&'static str> {
        if self.is_native() {
            // ⚠️ 保持**一句话**（40 字级）。这段文案出现在两个 tab 的顶部，
            // 而它要说的其实只有"现在走原生了，这几条用不了，清空连接串即可"。
            //
            // 早先写的是 80+ 字的三行解释（还列举了 mysql / mysqldump、
            // 「SQL 控制台不受影响」）—— 那类细节该在用户**真的卡住**时才出现，
            // 不该每次进页面都铺一遍。正式软件（DataGrip / TablePlus）在这种
            // 位置都只有一行提示 + 一个动作入口。
            Some("原生连接不支持备份、导入、建库等操作，清空连接串即可恢复。")
        } else {
            None
        }
    }

    /// 记一次失败（分类 + 原文都留着）。
    pub fn fail(&mut self, error: impl std::fmt::Display) {
        self.error = Some(DbFailure::from_text(error.to_string()));
    }

    /// 记一条**用户列表自己**的失败（≠ 整页失败）。
    ///
    /// 加这个方法而不是让调用方直接赋值，是为了把"它绝不污染整页 `error`"
    /// 这条不变式写在一个**能跑测试**的地方 —— `wslc-panel` 不在任何测试目标里，
    /// 写在那边的断言永远不会执行（`AGENTS.md` §2）。
    ///
    /// 背景：v0.16.0 实机里，普通账号读 `mysql.user` 被拒（`ERROR 1142`），
    /// 结果整页顶上挂着红横幅，而「数据库」tab 明明好好地列着表。
    pub fn fail_users(&mut self, error: impl std::fmt::Display) {
        self.users_error = Some(DbFailure::from_text(error.to_string()));
    }

    /// 清掉上一次的错误（每次动作开始前调）。
    pub fn clear_error(&mut self) {
        self.error = None;
        self.users_error = None;
    }

    /// 页面能不能真的干活：探测通过、而且没有根本性问题。
    pub fn usable(&self) -> bool {
        self.blocked.is_none()
    }

    /// 用户 tab 实际要画哪些账号。
    ///
    /// **系统账号（`mysql.*` / `mariadb.sys`）和 root 永远不返回** ——
    /// 它们是安装时建的，日常管业务账号时只会占地方，root 的
    /// 「改密码 / 删除」误点代价也大。要看它们就用「SQL」控制台。
    ///
    /// 判定在 [`DbUser::is_hidden_by_default`] 里，只有一份。
    pub fn visible_users(&self) -> impl Iterator<Item = &DbUser> + '_ {
        self.users
            .iter()
            .filter(|user| !user.is_hidden_by_default())
    }

    /// 「授权到哪个库」那个下拉框里的候选（**业务库，按名字排序**）。
    ///
    /// # 为什么排除系统库
    ///
    /// 和「数据库」tab 不显示它们同一个理由，但这里更要紧：`GRANT ALL ON mysql.*`
    /// 差不多等于把一个账号提成 root（`mysql.user` 就在那个库里），而下拉框是
    /// **选一个就生效**的控件 —— 误选的代价太大。真要授给系统库，旁边那个输入框
    /// 还能手打（下拉框只是快捷方式，见 `views::grant_db_field`）。
    ///
    /// # 为什么排序
    ///
    /// 列表来自一条 `ORDER BY SCHEMA_NAME` 的 SQL，本来就是有序的；这里再排一次
    /// 是为了让"候选顺序"不依赖那条 SQL —— 将来换个查询、或者在别处填
    /// `databases`，下拉框的顺序都不该跟着变（下拉框的顺序变了，用户凭位置
    /// 点选就会点错）。
    pub fn grant_database_options(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .databases
            .iter()
            .filter(|entry| !entry.is_system())
            .map(|entry| entry.name.clone())
            .collect();
        names.sort();
        names
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

/// 「创建容器」弹窗里那一格启动命令 → `wslc run` 尾部的 argv。
///
/// # 留空**不等于**"给一条空命令"
///
/// 空 → 返回空 `Vec`，`wslc run` 就**不带命令** —— 这时容器用的是
/// **镜像自己的 `ENTRYPOINT` / `CMD`**，mysql / nginx 这类镜像全靠它跑起来。
/// 一旦返回 `["/bin/sh", "-c", ""]`，那也算"用户给了命令"，会把镜像的 `CMD`
/// 顶掉：本来能跑的镜像**填了个空反而起不来**。这条是这一格唯一的陷阱。
///
/// 非空 → 走 [`exec_argv`]，和「容器 exec」那一格同一条路、同一个理由：
/// 管道 / 重定向 / `&&` 是用户预期能用的，按空格切会让它们**静默失效**
/// （命令看起来还成功了，最难查的一类）。
///
/// `trim()` 放在这里而不是 [`exec_argv`] 里：只敲了空格的那一格等于没敲，
/// 不 trim 就变成 `sh -c " "` —— 又是一个"起来就退"。
pub fn create_command(text: &str) -> Vec<String> {
    let text = text.trim();
    if text.is_empty() {
        Vec::new()
    } else {
        exec_argv(text)
    }
}

// ---------------------------------------------------------------------------
// 容器里的应用部署管理（nginx / Java / Node.js / Go，v0.9）
// ---------------------------------------------------------------------------

/// 一次部署相关动作失败：分类 + 原文。
///
/// 和 [`DbFailure`] 同一个形状、同一个理由：分开存是为了界面上能说**两句话**
/// —— 一句"该怎么办"（分类给的），一句原文（排障要用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployFailure {
    /// 分类（决定给什么建议）。
    pub kind: DeployErrorKind,
    /// 报错原文。
    pub detail: String,
}

impl DeployFailure {
    /// 从一段报错文本分类。退出码不传（理由同 [`DbFailure::from_text`]）。
    pub fn from_text(detail: impl Into<String>) -> Self {
        let detail = detail.into();
        Self {
            kind: wslc_core::model::deploy::classify_error(&detail, None),
            detail,
        }
    }

    /// 给用户的一句话建议（`None` = 直接看原文就行）。
    pub fn hint(&self) -> Option<&'static str> {
        self.kind.hint()
    }

    /// 提示条上那一句（没建议时给原文第一行，截断 —— 提示条是单行横幅）。
    pub fn summary(&self) -> String {
        if let Some(hint) = self.hint() {
            return hint.to_owned();
        }
        let first = self.detail.lines().next().unwrap_or_default().trim();
        truncate_chars(first, 120)
    }
}

/// 「添加部署」这个长任务走到了哪一步。
///
/// 每一步都要在界面上**看得见**（`AGENTS.md` §7.1）：从点下「部署」到有结果之间
/// 可能要几十秒（一个目录里几千个文件要一个个 `wslc container cp` 过去），
/// 界面上什么都不变的话用户会以为按钮坏了。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeployPhase {
    /// 准备目标目录 + 把文件复制进去 + 核对 + 补可执行位。
    ///
    /// 合成一档而不是拆成四档：这四步在界面上**都是"在搬东西"**，
    /// 而它们的耗时完全由体积决定 —— 拆开只会让按钮文案闪来闪去。
    Uploading,
    /// 文件到位了，正在写 / 校验站点配置（nginx 站点开 / 关都会走这一档）。
    ///
    /// 单独一档是因为它可能失败在 `nginx -t`：用户要能看出"文件已经铺好了、
    /// 卡在配置上"，而不是以为整个部署都没做。
    Configuring,
    /// 文件到位了，正在起进程（进程形态）/ 这一步内容形态没有。
    Starting,
    /// 正在停掉那个受 pidfile 管的进程（「停止」，只发 `TERM`）。
    ///
    /// ⚠️ v0.9.13 才有这一档：在那之前「停止」复用的是 [`Self::Starting`]，
    /// 于是点下去那一行写的是**"正在启动…"** —— 用户按的是停止、界面说的是启动。
    Stopping,
    /// 正在删掉目标目录里的内容（"删除部署"，不可撤销）。
    Removing,
}

impl DeployPhase {
    /// 界面上那一句（按钮文案 / 提示条都用它）。
    pub fn label(self) -> &'static str {
        match self {
            Self::Uploading => "正在上传…",
            Self::Configuring => "正在写入站点配置…",
            Self::Starting => "正在启动…",
            Self::Stopping => "正在停止…",
            Self::Removing => "正在删除…",
        }
    }
}

/// 正在跑的那一次部署。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployProgress {
    /// 在部署哪个应用。
    pub app: String,
    /// 走到哪一步了。
    pub phase: DeployPhase,
}

/// 已部署列表里的一行：**本地存档的记录** + **容器报回来的现状**。
///
/// 这两半的来源刻意分开（见 `crate::deploys` 的模块注释）：
/// 记录来自本地面板，`process` / `files` 必须问容器 —— 那是事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployEntry {
    /// 本地存档里那条记录。
    pub record: crate::deploys::DeployRecord,
    /// 存档里那个运行时键还原出来的运行时。
    ///
    /// `None` = **存档里的键认不出来**（手改坏了 / 是更新的版本写的）。
    /// 这时**不猜**默认值（猜错会把文件铺到别的地方），那一行只给「删除记录」，
    /// 并说明为什么其它按钮没有。见 `AppRuntime::from_key` 的说明。
    pub runtime: Option<AppRuntime>,
    /// 进程形态：跑没跑；内容形态恒为 `None`。
    pub process: Option<ProcessStatus>,
    /// 目标目录里的文件数；`None` = 目录不存在。
    pub files: Option<u64>,
    /// **问过容器了没有**。
    ///
    /// 用来区分"还没读"（画「读取中…」）和"读了，确实没有目录"
    /// —— 两者在界面上是两句话。`files` 的 `None` 一个人扛不了这个区分。
    pub probed: bool,
    /// 问容器的时候**失败了**（`Some` = 原因）。
    ///
    /// ⚠️ 这跟"目标不存在"是**两件事**，在界面上必须分开画：
    /// 这个是"没问出来"（容器刚停、`exec` 报错、超时），那个是"真的没有了"。
    /// 把前者显示成后者会让人以为**文件丢了** —— 那是要吓死人的一句话。
    pub probe_error: Option<String>,
}

impl DeployEntry {
    /// 状态列上那一句话。
    pub fn status_label(&self) -> &'static str {
        if self.runtime.is_none() {
            return "类型认不出来";
        }
        // 问容器失败了要说"读不到"，**不能**说成"目标不存在"
        //（后者会让人以为文件丢了）
        if self.probe_error.is_some() {
            return "读不到状态";
        }
        if !self.probed {
            return "读取中…";
        }
        match self.process {
            Some(ProcessStatus::Running) => "运行中",
            Some(ProcessStatus::Stopped) => {
                if self.files.is_some() {
                    "已停止"
                } else {
                    "目标不存在"
                }
            }
            // 内容形态（nginx）：没有属于我们的进程，看文件在不在
            None => {
                if self.files.is_some_and(|n| n > 0) {
                    "已部署"
                } else if self.files.is_some() {
                    "目标目录是空的"
                } else {
                    "目标不存在"
                }
            }
        }
    }

    /// 这一行现在能不能干活（类型认得出、而且容器里能看到目标目录）。
    pub fn is_process(&self) -> bool {
        self.runtime
            .is_some_and(|r| r.kind() == wslc_core::model::DeployKind::Process)
    }
}

/// 「部署管理」页的状态。
///
/// 纯数据（一个 GPUI 类型都没有），所以住在 `AppState` 里、也能被单测；
/// 弹窗里那些输入框是 GPUI 的 `Entity<InputState>`，住在 `Shell` 里。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployState {
    /// 目标容器名。
    pub container: String,
    /// 容器镜像引用（页面上显示，让用户确认点对了容器）。
    pub image: String,
    /// 运行时（列表那条启发式先给一个，`inspect` 回来后覆盖成权威值）。
    pub runtime: AppRuntime,
    /// 判定依据（页面上说出来，用户才知道该不该信）。
    pub signal: DeploySignal,
    /// 从挂载点推出来的 nginx Web 根（「添加部署」的默认目标目录用它）。
    pub web_root: Option<String>,
    /// 容器**已经发布**的端口：`"80/tcp" -> [(host_ip, host_port)]`。
    ///
    /// 只用来**报告**（已存在的容器改不了端口发布），见
    /// `cmd::deploy::DeployProbe` 的说明；判"发布了没有"用
    /// `wslc_core::model::deploy::port_is_published`（唯一一份判据，界面也调它）。
    pub ports: wslc_core::model::PortMap,
    /// 容器当前接入的网络名（站点反代预检找共同用户网络用）。
    pub networks: Vec<String>,
    /// 正在后台跑（探测 / 读状态 / 上传）。
    pub busy: bool,
    /// **根本性**的问题（不是这类容器 / 容器没运行）。
    /// 有值时页面只显示它 —— 那张空列表没有意义。
    pub blocked: Option<DeployFailure>,
    /// 最近一次操作的失败。
    pub error: Option<DeployFailure>,
    /// 读到了没有（区分"还没读"和"读到了，确实一条记录都没有"）。
    pub loaded: bool,
    /// 已部署列表。
    pub entries: Vec<DeployEntry>,
    /// 正在跑的那一次部署（`None` = 空闲）。
    pub progress: Option<DeployProgress>,
    /// 正在看哪个应用的日志（`None` = 浮层没开）。
    ///
    /// 纯数据（应用名 + 文本 + 忙不忙），所以能住在这里；
    /// 它是个**只读浮层**，里面一个输入框都没有 —— 和 `LogsState` 同一个理由。
    pub logs: Option<DeployLogs>,
    /// 正在看哪个应用的站点配置（`None` = 浮层没开）。
    pub site_conf: Option<DeploySiteConf>,
    /// 等待二次确认的删除（要删哪个应用；`None` = 没在确认）。
    ///
    /// ⚠️ 刻意**不用** `ConfirmAction` 那套：那个的 `execute` 是"跑一条 `wslc`
    /// 命令然后返回字符串"，而删一条部署要做三件事 —— 停进程、删目录、
    /// **改本地存档并重读列表**。最后那件是 `ConfirmAction` 表达不了的，
    /// 硬塞进去只会造出一个形状不对的变体。
    pub confirm_remove: Option<String>,
}

/// 正在看的应用日志。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployLogs {
    /// 看的是哪个应用。
    pub app: String,
    /// 日志内容（`None` = 还在读）。
    pub text: Option<String>,
    /// 正在后台读。
    pub busy: bool,
}

/// 正在看的站点配置（只读浮层）。
///
/// 和 [`DeployLogs`] 同一个形状：纯数据（没有输入框），所以能住在这里；
/// 和日志分开是因为标题 / 说明 / 打开入口不同，塞进 `DeployLogs` 只会多一个
/// "这是哪种文本"的布尔。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploySiteConf {
    /// 看的是哪个应用。
    pub app: String,
    /// conf 内容（`None` = 还在读）。
    pub text: Option<String>,
    /// 正在后台读。
    pub busy: bool,
}

impl DeployState {
    /// 页面刚打开、还没探测完时的状态。
    ///
    /// `runtime` / `signal` 来自**列表那条启发式**（镜像名 / 命令），
    /// 这样页面立刻就能画出来；权威判定（`inspect`）回来后由
    /// [`Self::apply_probe`] 覆盖。
    pub fn provisional(
        container: impl Into<String>,
        image: impl Into<String>,
        runtime: AppRuntime,
        signal: DeploySignal,
    ) -> Self {
        Self {
            container: container.into(),
            image: image.into(),
            runtime,
            signal,
            web_root: None,
            ports: Vec::new(),
            networks: Vec::new(),
            // ⚠️ **必须 false**。这里的语义只能是"**已经有一个后台任务在跑**"，
            // 而不是"马上就要去探测"—— `load_deploy` 的第一句就是"正在跑就不叠
            // 第二次"，先置 true 会把第一次加载自己挡掉，页面永远停在
            // 「正在读取…」。（这个坑 v0.8 在 `DbState::provisional` 上真的踩过，
            // 见那边的注释 —— 这里照抄那条教训。）
            busy: false,
            blocked: None,
            error: None,
            loaded: false,
            entries: Vec::new(),
            progress: None,
            logs: None,
            site_conf: None,
            confirm_remove: None,
        }
    }

    /// 探测结果回来了：覆盖权威字段，并**重建**已部署列表。
    pub fn apply_probe(&mut self, probe: &wslc_core::cmd::deploy::DeployProbe) {
        self.container = probe.container.clone();
        self.image = probe.image.clone();
        self.runtime = probe.runtime;
        // 权威判定来自 `inspect`（字段完整、没被截断），依据统一记成镜像名那一档
        self.signal = DeploySignal::Image;
        self.web_root = probe.web_root.clone();
        self.ports = probe.ports.clone();
        self.networks = probe.networks.clone();
        self.busy = false;
    }

    /// 用本地存档里的记录**重建**列表（每次读盘之后都走一遍）。
    ///
    /// 走过的行会把容器报回来的现状清掉（`probed = false`）—— 记录换了，
    /// 上一轮的"运行中"就不再是这一行的现状了。
    pub fn set_records(&mut self, records: &[crate::deploys::DeployRecord]) {
        self.entries = records
            .iter()
            .map(|record| DeployEntry {
                record: record.clone(),
                runtime: AppRuntime::from_key(&record.runtime),
                process: None,
                files: None,
                probed: false,
                probe_error: None,
            })
            .collect();
    }

    /// 按应用名找一行的下标。
    pub fn index_of(&self, app: &str) -> Option<usize> {
        self.entries.iter().position(|e| e.record.app == app)
    }

    /// 记一次失败（分类 + 原文都留着）。
    pub fn fail(&mut self, error: impl std::fmt::Display) {
        self.error = Some(DeployFailure::from_text(error.to_string()));
    }

    /// 清掉上一次的错误（每次动作开始前调）。
    pub fn clear_error(&mut self) {
        self.error = None;
    }
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
    /// 每次打开按当前文档初始化，避免承接其他发行版的展开状态。
    expanded_sections: BTreeSet<&'static str>,
}

impl WslConfState {
    /// 新建。
    pub fn new(
        distro: impl Into<String>,
        doc: wslc_core::model::wslconf::WslConfDoc,
        wsl_version: impl Into<String>,
    ) -> Self {
        let wsl_version = wsl_version.into();
        let expanded_sections = wslconf::SECTIONS
            .iter()
            .filter(|section| wslconf::section_supported(section.name, &wsl_version))
            .filter(|section| {
                matches!(section.name, "network" | "user")
                    || wslconf::FIELDS.iter().any(|field| {
                        field.section == section.name && doc.is_explicit(field.section, field.key)
                    })
            })
            .map(|section| section.name)
            .collect();
        Self {
            distro: distro.into(),
            doc,
            wsl_version,
            busy: false,
            errors: Vec::new(),
            show_preview: false,
            expanded_sections,
        }
    }

    /// 当前是否展开；不随文档变化重算，保留用户手动收起的选择。
    pub fn section_open(&self, section: &str) -> bool {
        self.expanded_sections.contains(section)
    }

    pub fn set_section_open(&mut self, section: &'static str, open: bool) {
        if wslconf::find_section(section).is_none() {
            return;
        }
        if open {
            self.expanded_sections.insert(section);
        } else {
            self.expanded_sections.remove(section);
        }
    }

    pub fn toggle_section(&mut self, section: &'static str) {
        self.set_section_open(section, !self.section_open(section));
    }

    /// 只汇总面板认识的显式字段，未知键由原文保留。
    pub fn section_summary(&self, section: &str) -> String {
        let values: Vec<String> = wslconf::FIELDS
            .iter()
            .filter(|field| field.section == section && self.doc.is_explicit(section, field.key))
            .map(|field| format!("{}={}", field.label, self.doc.effective(section, field.key)))
            .collect();
        if values.is_empty() {
            "默认".to_owned()
        } else {
            values.join(" · ")
        }
    }

    /// 预览与保存共用，避免未触碰的预填默认值落入显式配置。
    pub fn apply_text_value(&mut self, section: &str, key: &str, value: &str) {
        let Some(field) = wslconf::find_field(section, key) else {
            return;
        };
        if field.kind != wslconf::FieldKind::Text || field.read_only {
            return;
        }
        let value = value.trim();
        if value == self.doc.effective(section, key).trim() {
            return;
        }
        if value.is_empty() {
            self.doc.clear(section, key);
        } else {
            self.doc.set(section, key, value);
        }
    }
}

/// 「应用设置」页里的 tab。
///
/// 原先这是侧边栏里的三个独立页面（应用设置 / wlsc 配置 / WSL 配置），
/// 但它们都是"设置"，分开摆反而让人找不着 —— 收进一页分 tab。
/// 参考项目的设置页也是这么分的（常规 / 高级 / 界面）。
///
/// v0.9.22 起「关于」也收进来了：它同样只能看、不能改，但为此单占一个
/// 侧边栏入口太贵；放在最后一个 tab 不会让人误以为里面有开关。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SettingsTab {
    /// 常规：刷新间隔、主题。
    #[default]
    General,
    /// 高级：**wslc 自己**的 `settings.yaml`。
    Advanced,
    /// WSL：`%USERPROFILE%\.wslconfig`（**WSL 本身**的全局配置）。
    Wsl,
    /// 关于：版本、构建、地址（v0.9.22 从独立页面收进来）。
    About,
}

impl SettingsTab {
    /// 全部 tab（决定显示顺序）。
    pub const ALL: [SettingsTab; 4] = [
        SettingsTab::General,
        SettingsTab::Advanced,
        SettingsTab::Wsl,
        SettingsTab::About,
    ];

    /// tab 标题。
    ///
    /// ⚠️ 「高级」是历史变体名，实际内容是 **wslc 的 `settings.yaml`**；
    /// 「WSL」才是 `.wslconfig`。标题必须说清这两个是不同的配置文件，
    /// 否则用户会以为「高级」是本程序的高级选项。
    pub fn label(self) -> &'static str {
        match self {
            SettingsTab::General => "常规",
            SettingsTab::Advanced => "wslc 配置",
            SettingsTab::Wsl => "WSL 配置",
            SettingsTab::About => "关于",
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

    /// 编辑新输入后不再复用旧 tag 或旧拉取目标。
    pub fn matches_inputs(&self, host: &str, input: &str) -> bool {
        let host = registry::normalize_host(host);
        let host = if host.is_empty() {
            registry::DEFAULT_REGISTRY
        } else {
            host.as_str()
        };
        self.host == host && self.input.trim() == input.trim()
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

    /// 下拉框里该列出哪些 tag：**列表 + （列表里缺的）用户手打的那个**。
    ///
    /// # 为什么手打的要补进去（v0.9.4 的实机 bug）
    ///
    /// 反馈原文："我想拉取 openjdk:8-jre-slim，结果啥东东都出来了"。现象是下拉框
    /// 显示 `28-ea`、"将拉取"也变成 `:28-ea` —— **拉错镜像**。
    ///
    /// 根因：用户打的 `8-jre-slim` 不在 registry 返回的列表里，于是界面那边
    /// `set_selected_value` 找不到对应项 → 下拉框没有任何选中 → 组件**自己选中
    /// 第一项**。而"列表里没有"不等于"拉不了"：**实测（2026-10-15，
    /// docker.1panel.live / library/openjdk）** 它的 `tags/list` 只回了 17282 个，
    /// 里面既没有 `8-jre-slim` 也没有 `latest`，可这两个 tag 的 `manifests/`
    /// 都是 **200**（能拉）。列表不全不是用户的错，更不该因此把要拉的东西换掉。
    ///
    /// 手打的那个放**最前面**：它是用户明确要的，而列表可能有两万项，
    /// 排在后面等于找不到。
    pub fn tag_items(&self) -> Vec<String> {
        let mut items: Vec<String> = self.tags.clone();
        if let Some(tag) = self.effective_tag() {
            if !self.tags.iter().any(|t| t == tag) {
                items.insert(0, tag.to_owned());
            }
        }
        items
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

/// 容器详情里那一份 `wslc inspect`。
///
/// # 为什么详情要单独拉一次 inspect
///
/// 详情里要显示的三样东西**列表那一份数据里没有**：
///
/// | 要显示的 | 列表（`wslc list -a`）有什么 | `inspect` 有什么 |
/// |---|---|---|
/// | 挂载 | `Mounts` 只有卷名（`mysqldata`），`Labels.V1.Volumes` 实测**是空的** | `Mounts[]` 的 `Source` + `Destination` |
/// | 启动命令 | `Command` 被 wslc **截断到 20 字符**（`"docker-entrypoint.s…"`） | `Config.Entrypoint` + `Config.Cmd` 全量 |
/// | 环境变量 | 完全没有 | `Config.Env` 全量 |
///
/// 而 `inspect` 是一次几百毫秒的子进程（`AGENTS.md` §7.1：不许放界面线程），
/// 所以**打开详情时在后台拉一次**，拉回来之前界面显示"正在读取…"。
///
/// `container` 用来防串台：用户可能点了 A、没等回来又点 B。
/// 写回之前比一次名字，不一致就丢掉 —— 否则 A 的挂载会画在 B 的详情里。
#[derive(Debug, Clone)]
pub struct DetailInspect {
    /// 这份数据属于哪个容器。
    pub container: String,
    /// 拉回来的结果；`None` = 还在读（或读失败了，看 `error`）。
    pub inspect: Option<wslc_core::model::ContainerInspect>,
    /// 读失败的原因（人话，界面上原样显示）。
    pub error: Option<String>,
}

impl DetailInspect {
    /// 开始读某个容器的详情。
    pub fn loading(container: impl Into<String>) -> Self {
        Self {
            container: container.into(),
            inspect: None,
            error: None,
        }
    }
}

/// 应用的完整状态。
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
    /// 仅记本次界面的展开状态，不保存偏好，也不影响实际表单参数。
    expanded_sections: BTreeSet<UiSection>,
    /// 容器页的筛选 / 排序状态。
    ///
    /// **页面状态，不是偏好**：不写 `prefs.json`，重启后回到「全部 + 默认排序」。
    /// 查询词不在这里 —— 它住在 `Shell` 的 `InputState` 里（那是 GPUI 类型）。
    pub container_view: crate::listview::ContainerView,
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
    /// 「关于」页那张更新卡的状态。
    ///
    /// 纯数据（`Phase` 不认识网络也不认识 GPUI）：判据在 `crate::update` 里，
    /// 起 `curl.exe` 的那一半在 `wslc_core::cmd::update`。
    pub update: crate::update::Phase,
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
    /// 关掉面板之后它还会继续跑。标的地方是**发行版详情弹窗**里那一句说明 ——
    /// 列表行的状态列挤不下第二个徽标（它会盖到隔壁「版本」列上）。
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
    /// 容器详情里那一份 `wslc inspect`；详情关着时为 `None`。
    ///
    /// 打开详情时**后台拉一次**（那几样信息只有 inspect 有，见 [`DetailInspect`]）。
    /// 同样全是纯数据，所以住在这儿而不是 `Shell`。
    pub detail_inspect: Option<DetailInspect>,
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
    /// 「数据库管理」页的状态；没在那一页时为 `None`。
    ///
    /// 一次只服务**一个**容器（页面就是"针对某个容器"的），所以这里是
    /// `Option<DbState>` 而不是 `HashMap`：切到另一个容器的数据库页时
    /// 整个换掉，连接信息不会串台。
    pub db: Option<DbState>,
    /// 数据库账号的**本地存档**（`db-credentials.json`，v0.8）。
    ///
    /// ⚠️ **明文存密码** —— 这是"列表里要展示密码"这个产品决定的直接代价，
    /// 见 `dbcreds` 的模块注释（1Panel 同样这么做）。启动时读一次，
    /// 建库 / 删库时改 + 写盘。
    pub db_credentials: crate::dbcreds::DbCredentialStore,
    /// 凭据存档**读不出来**的原因（损坏时提示；`None` = 一切正常）。
    pub db_credentials_error: Option<String>,
    /// 备份文件在磁盘上的家：`<数据目录>/backups/<容器>/<文件名>`。
    ///
    /// `None` = `LOCALAPPDATA` 没设置（备份功能整体不可用，界面上给一句说明）。
    pub backup_dir: Option<PathBuf>,
    /// 「部署管理」页的状态；没在那一页时为 `None`。
    ///
    /// 一次只服务**一个**容器（页面就是"针对某个容器"的），所以这里是
    /// `Option<DeployState>` 而不是 `HashMap`：切到另一个容器的部署页时整体替换
    /// —— 和 [`AppState::db`] 同一个取舍。
    pub deploy: Option<DeployState>,
    /// 部署记录的**本地存档**（`deployments.json`，v0.9）。
    ///
    /// 它记的是"我们部署过什么"；**"现在跑没跑"绝不从这里读**，
    /// 一律问容器（见 `crate::deploys` 的模块注释）。
    /// 启动时读一次，部署 / 删除时改 + 写盘。
    pub deploy_store: crate::deploys::DeployStore,
    /// 部署存档**读不出来**的原因（`None` = 一切正常）。
    ///
    /// ⚠️ 这个会被部署页当成提示条显示出来
    /// （`views::deploy_page` 里那条 `deploy_store_error`）。改字段名 / 挪位置
    /// 时记得同步那一处 —— 这类"算出来但没画"的错误不会有编译错误，
    /// 只会静默消失（`db_credentials_error` 就曾经空了整整一个版本）。
    pub deploy_store_error: Option<String>,
}

impl AppState {
    /// 数据库账号存档落在哪里（没设 `LOCALAPPDATA` 时是 `None`）。
    pub fn db_credentials_path(&self) -> Option<PathBuf> {
        crate::prefs::app_dir().map(crate::dbcreds::store_path)
    }

    /// 把凭据存档写回磁盘（建库 / 删库之后调）。
    ///
    /// 返回 `Option<String>`：`Some(说明)` = 没写成（界面提示），`None` = 成功。
    /// 刻意**不用 crate 的 `Result` 别名** —— 那个别名把错误类型钉死在
    /// `wslc_core::Error` 上，而这里的失败原因（磁盘满 / 路径不对）不是
    /// 子进程错误，硬塞进去只会造出一个没人会匹配的变体。
    ///
    /// 失败只返回一句说明，**不 panic**：存档写不进去最坏是
    /// "重启后账号要重新录入"，不值得为它崩掉界面。
    pub fn save_db_credentials(&self) -> Option<String> {
        let Some(path) = self.db_credentials_path() else {
            return Some("LOCALAPPDATA 未设置，账号存不了盘".to_owned());
        };
        self.db_credentials
            .save(&path)
            .err()
            .map(|e| format!("写凭据存档失败：{e}"))
    }

    /// 部署记录存档落在哪里（没设 `LOCALAPPDATA` 时是 `None`）。
    pub fn deploy_store_path(&self) -> Option<PathBuf> {
        crate::prefs::app_dir().map(crate::deploys::store_path)
    }

    /// 把部署存档写回磁盘（部署成功 / 删除之后调）。
    ///
    /// 返回语义和 [`Self::save_db_credentials`] 一样：`Some(说明)` = 没写成。
    /// 失败**不 panic**：最坏是"重启后这条记录要重新部署一次才能回来"，
    /// 而文件其实已经在容器里了 —— 不值得为它崩掉界面。
    pub fn save_deploy_store(&self) -> Option<String> {
        let Some(path) = self.deploy_store_path() else {
            return Some("LOCALAPPDATA 未设置，部署记录存不了盘".to_owned());
        };
        self.deploy_store
            .save(&path)
            .err()
            .map(|e| format!("写部署存档失败：{e}"))
    }

    /// 把某个容器的部署记录灌进当前页面状态（进页面 / 存盘后都走一遍）。
    ///
    /// 把"读存档 → 换算成视图行"这一步收在一处：调用点有三四个，
    /// 散着写迟早漏掉一次（表现是"刚部署完列表里没有它"）。
    pub fn refresh_deploy_entries(&mut self, container: &str) {
        let records: Vec<crate::deploys::DeployRecord> = self
            .deploy_store
            .for_container(container)
            .into_iter()
            .cloned()
            .collect();
        if let Some(deploy) = self.deploy.as_mut() {
            if deploy.container == container {
                deploy.set_records(&records);
            }
        }
    }

    /// 某个容器的备份目录（`<数据目录>/backups/<容器>`）。
    pub fn container_backup_dir(&self, container: &str) -> Option<PathBuf> {
        self.backup_dir.as_ref().map(|dir| {
            dir.join(wslc_core::model::mysql::WINDOWS_BACKUP_SUBDIR)
                .join(container.trim())
        })
    }

    /// 重扫一个容器的备份目录（**同步**碰文件系统）。
    ///
    /// ⚠️ `read_dir` + 每条一次 `metadata()`，目录在机械盘 / 网络盘上、
    /// 或者攒了几百份备份时会明显卡一下。所以：
    ///
    /// - **点击路径**（`open_db_backup_dialog`）可以直接调 —— 用户点一下、
    ///   等几十毫秒，可以接受；
    /// - **异步回调里**（`load_db` / 备份完成 / 删除完成的 `update_in`）
    ///   必须先在 `background_executor` 上算出 `Vec<BackupEntry>`，再用
    ///   [`AppState::apply_backups`] 塞回来 —— 那些回调跑在**界面线程**上，
    ///   "反正是后台任务的回调"这个理由是错的（v0.10.3 审查时抓到）。
    pub fn rescan_backups(&mut self, container: &str) {
        // ⚠️ 先把**目录**算出来、再借 `self.db`：`db.backups = …` 赋值时
        // `db`（对 `self.db` 的可变借用）还活着，那时再去调 `self.…`
        // 就是 E0502 —— 借用检查器看得见，人眼容易漏。
        let Some(dir) = self.container_backup_dir(container) else {
            return;
        };
        let Some(db) = self.db.as_mut().filter(|db| db.container == container) else {
            return;
        };
        db.backups = scan_backup_dir(&dir).unwrap_or_default();
    }

    /// 把（在后台线程上算好的）备份列表塞进状态。
    ///
    /// 和 [`AppState::rescan_backups`] 配成一对：那个负责**算**，这个负责**填**。
    /// 分成两个是因为 `scan_backup_dir` 碰文件系统，不能出现在界面线程上。
    pub fn apply_backups(&mut self, container: &str, entries: Vec<BackupEntry>) {
        if let Some(db) = self.db.as_mut().filter(|db| db.container == container) {
            db.backups = entries;
        }
    }

    /// 新建初始状态。
    pub fn new(wslc: Wslc, wsl: Wsl) -> Self {
        // 只读一次盘，然后把两半分别放进结构体 —— 对比上面 `db_credentials`
        // 那两行（各调一次 `db_credentials_at_startup()`，白读一遍）。
        let (deploy_store, deploy_store_error) = deploy_store_at_startup();
        let data_dir = crate::prefs::app_dir();
        Self {
            wslc,
            wsl,
            page: Page::Dashboard,
            expanded_sections: BTreeSet::new(),
            container_view: crate::listview::ContainerView::default(),
            snapshot: Snapshot::default(),
            distro_status: None,
            distro_status_error: None,
            settings: None,
            settings_error: None,
            prefs: crate::prefs::Prefs::load(),
            // 启动 8 秒后才会去查（见 `Shell::start_auto_check`），这里先记空闲。
            update: crate::update::Phase::Idle,
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
            // 详情没打开 —— 打开时才去拉 inspect（见 `DetailInspect`）
            detail_inspect: None,
            online: OnlineDistroState::new(),
            mirrors: MirrorState::new(),
            registry: RegistryState::new(),
            db: None,
            // 凭据存档 / 备份目录都挂在面板自己的数据目录下（读坏了给提示，
            // 不 panic —— 账号可以在建库时重新录入）
            db_credentials: db_credentials_at_startup().0,
            db_credentials_error: db_credentials_at_startup().1,
            backup_dir: data_dir,
            deploy: None,
            deploy_store,
            deploy_store_error,
        }
    }

    /// 查询界面区域是否展开。
    pub fn section_open(&self, section: UiSection) -> bool {
        self.expanded_sections.contains(&section)
    }

    /// 只改变显示状态，保留实际表单参数。
    pub fn set_section_open(&mut self, section: UiSection, open: bool) {
        if open {
            self.expanded_sections.insert(section);
        } else {
            self.expanded_sections.remove(&section);
        }
    }

    /// 切换界面区域的展开状态。
    pub fn toggle_section(&mut self, section: UiSection) {
        self.set_section_open(section, !self.section_open(section));
    }

    /// 现有的发行版名（装前查重名用）。
    pub fn distro_names(&self) -> Vec<String> {
        self.snapshot
            .distros
            .iter()
            .map(|distro| distro.name.clone())
            .collect()
    }

    /// 现有容器的名字（给"按镜像推一个默认容器名"查重名用）。
    ///
    /// ⚠️ 用 `all`（**含已退出的**）而不是 `running` —— 容器名在**所有**容器
    /// 之间必须唯一，只看运行中的话，同名会撞在已退出的那个上。
    pub fn container_names(&self) -> Vec<String> {
        self.snapshot
            .all
            .iter()
            .map(|summary| summary.item.display_name().to_owned())
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

    /// 提示条到点了吗？到点就收掉，返回"这次真的收掉了没有"。
    ///
    /// 由界面层那个 500ms 的小循环调（`panel-core` 里没有执行器、起不了定时器
    /// —— 见 `Shell::start_toast_reaper` 的注释）。
    ///
    /// 返回值的用处：**只有真收掉了才需要重绘**。每半秒无脑 notify 一次
    /// 会让界面一直重绘（GPUI 下这不只是浪费 —— 每一帧都要跑布局 + 光栅化）。
    pub fn clear_expired_toast(&mut self) -> bool {
        let expired = self
            .toast
            .as_ref()
            .is_some_and(|t| t.shown_at.elapsed() >= t.lifetime());
        if expired {
            self.toast = None;
        }
        expired
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
    fn a_users_query_failure_never_becomes_a_page_level_error() {
        // 实机场景（v0.16.0）：普通账号读 mysql.user 被拒（ERROR 1142），
        // 而「数据库」tab 完全正常。如果这条失败进了 `error`，整页顶上会挂
        // 一条红色横幅 —— 用户会以为整个页面坏了，于是不敢用（明明能用）。
        let mut db = DbState::provisional_kind(
            "mysql",
            "docker.1panel.live/library/mysql:latest",
            wslc_core::db::DbKind::MySql,
        );
        db.fail_users("ERROR 1142 (42000): SELECT command denied for table user");

        // 用户那边记下了
        assert!(db.users_error.is_some());
        // 但整页的错误必须仍然是空的
        assert!(
            db.error.is_none(),
            "用户列表失败污染了整页 error —— 会把「数据库」tab 也糊住"
        );
        // "这次读取是成功的"这个判断也不能被它推翻（页面据此决定画不画表）
        assert!(db.usable());

        // 换成有权限的账号重读：清掉之后界面上的"读取失败"标记也该消失
        db.clear_error();
        assert!(db.users_error.is_none());
    }

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

    fn wslconf_state(text: &str) -> WslConfState {
        WslConfState::new("Ubuntu", wslconf::WslConfDoc::parse(text), "2.6.1.0")
    }

    #[test]
    fn wslconf_common_sections_start_open() {
        let state = wslconf_state("");
        assert!(state.section_open("network"));
        assert!(state.section_open("user"));
    }

    #[test]
    fn wslconf_implicit_infrequent_sections_start_closed() {
        let state = wslconf_state("[automount]\nfutureKey = keep-me\n");
        for section in ["automount", "interop", "boot", "gpu", "time"] {
            assert!(!state.section_open(section), "{section}");
        }
    }

    #[test]
    fn wslconf_explicit_fields_open_their_section_including_systemd() {
        for field in wslconf::FIELDS {
            let mut doc = wslconf::WslConfDoc::parse("");
            doc.set(field.section, field.key, field.default);
            let state = WslConfState::new("Ubuntu", doc, "2.6.1.0");
            assert!(
                state.section_open(field.section),
                "{}.{}",
                field.section,
                field.key
            );
        }
    }

    #[test]
    fn wslconf_initial_expansion_respects_versions_and_unknown_version_semantics() {
        let doc = wslconf::WslConfDoc::parse(
            "[boot]\nsystemd = true\n[gpu]\nenabled = false\n[time]\nuseWindowsTimezone = false\n",
        );
        for version in ["0.60.0", "0.67.6", "2.6.1.0", "", "版本未知"] {
            let state = WslConfState::new("Ubuntu", doc.clone(), version);
            for section in ["boot", "gpu", "time"] {
                assert_eq!(
                    state.section_open(section),
                    wslconf::section_supported(section, version),
                    "{section}: {version}"
                );
            }
        }
    }

    #[test]
    fn wslconf_sections_can_be_closed_and_reopened() {
        let mut state = wslconf_state("[boot]\nsystemd = true\n");
        let doc = state.doc.clone();
        for section in wslconf::SECTIONS {
            state.set_section_open(section.name, false);
            assert!(!state.section_open(section.name));
            state.toggle_section(section.name);
            assert!(state.section_open(section.name));
            state.toggle_section(section.name);
            assert!(!state.section_open(section.name));
            state.set_section_open(section.name, true);
            assert!(state.section_open(section.name));
        }
        assert_eq!(state.doc, doc);
        assert!(!state.doc.is_dirty());
    }

    #[test]
    fn wslconf_unknown_sections_do_not_change_expansion_state() {
        let mut state = wslconf_state("");
        let before = state.clone();
        state.set_section_open("unknown", true);
        state.toggle_section("unknown");
        state.set_section_open("Network", true);
        assert!(!state.section_open("unknown"));
        assert_eq!(state, before);
    }

    #[test]
    fn wslconf_new_instances_have_independent_expansion_state() {
        let mut first = wslconf_state("");
        first.set_section_open("network", false);
        first.set_section_open("automount", true);
        let mut second = WslConfState::new("Debian", wslconf::WslConfDoc::parse(""), "2.6.1.0");
        assert!(second.section_open("network"));
        assert!(!second.section_open("automount"));
        second.set_section_open("user", false);
        assert!(first.section_open("user"));
        assert!(!first.section_open("network"));
        assert!(first.section_open("automount"));
    }

    #[test]
    fn wslconf_section_summary_lists_only_explicit_supported_fields() {
        let state = wslconf_state(
            "[automount]\nroot = /data/\noptions = metadata\nfutureKey = keep-me\n\
             [boot]\nsystemd = true\n[network]\nfutureKey = keep-me\n",
        );
        assert_eq!(
            state.section_summary("automount"),
            "挂载根目录=/data/ · 挂载参数=metadata"
        );
        assert_eq!(state.section_summary("boot"), "启用 systemd=true");
        for section in ["network", "interop", "user", "gpu", "time", "unknown"] {
            assert_eq!(state.section_summary(section), "默认", "{section}");
        }
    }

    #[test]
    fn wslconf_untouched_text_defaults_stay_implicit_and_clean() {
        for text in [
            "",
            "[network]\nhostname = my-box\n",
            "[network]\r\nhostname = my-box",
        ] {
            let mut state = wslconf_state(text);
            let before = state.doc.clone();
            for field in wslconf::FIELDS
                .iter()
                .filter(|field| field.kind == wslconf::FieldKind::Text)
            {
                let effective = state.doc.effective(field.section, field.key);
                state.apply_text_value(field.section, field.key, &format!("  {effective}  "));
            }
            assert_eq!(state.doc, before);
            assert_eq!(state.doc.render(), text);
            assert!(!state.doc.is_dirty());
            assert!(!state.doc.is_explicit("automount", "root"));
        }
    }

    #[test]
    fn wslconf_text_changes_and_clears_follow_the_model() {
        let mut state = wslconf_state("[automount]\nroot = /data/\n[user]\ndefault = ubuntu\n");
        state.apply_text_value("automount", "root", "  /data/  ");
        assert!(!state.doc.is_dirty());
        state.apply_text_value("automount", "root", "  /win/  ");
        assert_eq!(state.doc.get("automount", "root"), Some("/win/"));
        assert!(state.doc.is_dirty());
        state.apply_text_value("automount", "root", " /mnt/ ");
        assert_eq!(state.doc.get("automount", "root"), Some("/mnt/"));
        state.apply_text_value("automount", "root", " \t ");
        state.apply_text_value("user", "default", "");
        assert!(!state.doc.is_explicit("automount", "root"));
        assert_eq!(state.doc.effective("automount", "root"), "/mnt/");
        assert!(!state.doc.is_explicit("user", "default"));
        assert_eq!(state.doc.render(), "[automount]\n[user]\n");
    }

    #[test]
    fn wslconf_text_api_ignores_readonly_bool_and_unknown_fields() {
        let mut state = wslconf_state("[boot]\nsystemd = true\n[interop]\nenabled = false\n");
        let before = state.doc.clone();
        for field in wslconf::FIELDS
            .iter()
            .filter(|field| field.read_only || field.kind != wslconf::FieldKind::Text)
        {
            state.apply_text_value(field.section, field.key, "changed");
            state.apply_text_value(field.section, field.key, "");
        }
        for (section, key) in [
            ("unknown", "hostname"),
            ("network", "unknown"),
            ("Network", "hostname"),
        ] {
            state.apply_text_value(section, key, "changed");
        }
        assert_eq!(state.doc, before);
        assert!(!state.doc.is_dirty());
    }

    #[test]
    fn wslconf_preview_and_save_preserve_comments_unknown_keys_and_systemd() {
        let text = "# 保留手写注释\n[boot]\nsystemd = true\nfutureKey = keep-me\n\n\
                    [automount]\nroot = /data/\n; 挂载说明\n[future]\ncustom = untouched\n";
        let mut state = wslconf_state(text);
        state.set_section_open("automount", false);
        let mut preview = state.clone();
        preview.apply_text_value("automount", "root", " /win/ ");
        assert_eq!(state.doc.render(), text);
        assert!(!state.doc.is_dirty());
        state.apply_text_value("automount", "root", " /win/ ");
        let rendered = state.doc.render();
        assert_eq!(rendered, preview.doc.render());
        assert_eq!(rendered, text.replace("root = /data/", "root = /win/"));
        assert_eq!(wslconf::WslConfDoc::parse(&rendered).render(), rendered);
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
        // v0.9.22 把「关于」从侧边栏收进「应用设置」的 tab，8 - 1 = 7。
        //
        // 这个断言存在的意义就是**逼人改它**：加页面时忘了同步导航分组，
        // 侧边栏会出现重复的组标题。历史上 commit 552a91c 就是被它抓到的；
        // 加 `Page::About` 那次它又抓了一次（CI 的 `纯逻辑测试` 那一关）。
        //
        // ⚠️ 注意 `cargo check --all-targets` 只编译不执行，
        // 所以它真的被跑到要靠 `cargo test -p wslc-panel-core`（见 SPIKE 7.8）。
        assert_eq!(Page::ALL.len(), 7);
    }

    #[test]
    fn settings_tabs_are_all_labelled() {
        // 加 tab 时忘了同步 `settings_tabs` 的渲染顺序，界面上会出现
        // 一个点不到的 tab（按钮 id 用 `{:?}`，顺序由 ALL 决定）。
        assert_eq!(SettingsTab::ALL.len(), 4);
        for tab in SettingsTab::ALL {
            assert!(!tab.label().is_empty());
        }
        assert_eq!(
            SettingsTab::About.label(),
            "关于",
            "「关于」现在是设置里的 tab，不再是页面"
        );
    }

    #[test]
    fn only_the_pages_with_inputs_need_a_window_to_enter() {
        // 有输入框的页面必须在点击时（有 window）把表单建好。
        // 哪天给别的页面加了输入框，这个测试会提醒你一起改。
        //
        // v0.9.17 起「容器」页也进了这一列：搜索框是 `InputState`，
        // 只能在进入页面那一刻建（`Shell::ensure_container_search`）。
        //
        // ⚠️ `Page::AddInstance` **不在 `Page::ALL` 里**（侧边栏不列它），
        // 所以不能只遍历 `ALL` 就算完 —— 那样这条断言对它就是**空的**，
        // 哪天真把它改坏了也抓不到。显式再查一遍。
        for page in Page::ALL {
            let expected = page == Page::Containers;
            assert_eq!(
                page.needs_window_to_enter(),
                expected,
                "{page:?} 的 `needs_window_to_enter` 和实际不符 —— \
                 改了页面上的输入框就要同步改 `app.rs::set_page` 和这条断言"
            );
        }
        assert!(Page::AddInstance.needs_window_to_enter());
        // 「数据库管理」也不在 ALL 里（只能从容器行进），但它有连接卡输入框。
        assert!(Page::Database.needs_window_to_enter());
    }

    #[test]
    fn sub_pages_highlight_their_parent_in_the_sidebar() {
        assert_eq!(Page::AddInstance.nav_parent(), Some(Page::Instances));
        assert_eq!(Page::Database.nav_parent(), Some(Page::Containers));
        assert_eq!(Page::Deploy.nav_parent(), Some(Page::Containers));
        assert_eq!(Page::Dashboard.nav_parent(), None);

        assert!(Page::AddInstance.highlights_nav(Page::Instances));
        assert!(Page::Database.highlights_nav(Page::Containers));
        assert!(Page::Deploy.highlights_nav(Page::Containers));
        // 子页面不会把别的导航项点亮
        assert!(!Page::Database.highlights_nav(Page::Images));
        // 普通页面只高亮自己，不会把别的页也点亮
        assert!(Page::Containers.highlights_nav(Page::Containers));
        assert!(!Page::Containers.highlights_nav(Page::Images));
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
    fn registry_state_matches_inputs_after_host_normalization() {
        let mut state = RegistryState::new();
        state.begin_query(
            "docker.m.daocloud.io",
            "  nginx:1.27  ",
            "library/nginx",
            Some("1.27".to_owned()),
        );
        let before = state.clone();
        for host in [
            "docker.m.daocloud.io",
            "  docker.m.daocloud.io  ",
            "https://docker.m.daocloud.io/",
            "http://Docker.M.Daocloud.IO/",
        ] {
            assert!(state.matches_inputs(host, "  nginx:1.27  "), "{host}");
            assert!(state.matches_inputs(host, "nginx:1.27"), "{host}");
        }
        assert_eq!(state, before, "匹配检查不能改动查询状态");
    }

    #[test]
    fn registry_state_rejects_changed_hosts_repositories_and_tags() {
        let mut state = RegistryState::new();
        state.begin_query(
            "docker.m.daocloud.io",
            "nginx:1.27",
            "library/nginx",
            Some("1.27".to_owned()),
        );
        assert!(!state.matches_inputs(registry::DEFAULT_REGISTRY, "nginx:1.27"));
        assert!(!state.matches_inputs("docker.m.daocloud.io", "redis:1.27"));
        assert!(!state.matches_inputs("docker.m.daocloud.io", "nginx:1.29"));
        assert!(!state.matches_inputs("docker.m.daocloud.io", "nginx"));
    }

    #[test]
    fn registry_state_matches_an_empty_host_as_the_default_registry() {
        let mut state = RegistryState::new();
        state.begin_query(registry::DEFAULT_REGISTRY, "alpine", "library/alpine", None);
        for host in ["", "  ", registry::DEFAULT_REGISTRY] {
            assert!(state.matches_inputs(host, "alpine"), "{host:?}");
        }
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
        // 空快照的圆环：全是 0 / None —— 界面上那一排会画成"没有数据"，
        // **不是** 0% 满环。
        let stats = snap.dashboard_stats();
        assert_eq!(stats.running, 0);
        assert_eq!(stats.total, 0);
        assert_eq!(stats.cpu_percent, 0.0);
        assert_eq!(stats.cpu_cores, None);
        assert_eq!(stats.mem_limit, None);
        assert_eq!(stats.cpu_whole_machine_percent(), None);
        assert_eq!(stats.mem_percent(), None);
        assert!(stats.disk.is_none());
    }

    #[test]
    fn image_totals_dedupe_refs_pointing_at_the_same_id() {
        // 真机夹具：`e2ac70e7319a` 同时是
        // `docker.1panel.live/library/hello-world` 和 `hello-world` 两行。
        const IMAGES: &str = include_str!("../../wslc-core/tests/fixtures/images.jsonl");
        let images: Vec<ImageListItem> =
            wslc_core::jsonl::parse_lines(IMAGES).expect("夹具应当能解析");
        assert_eq!(images.len(), 3, "夹具本身是 3 行（2 个镜像）");

        let snap = Snapshot {
            images,
            ..Default::default()
        };
        let (distinct, bytes) = snap.image_totals();
        assert_eq!(distinct, 2, "同一个 ID 的多条引用只算一个镜像");
        // 8.42MB + 10.1kB（1000 进制，见 `parse_size`）。浮点转 u64 允许 1 字节误差。
        assert!(
            (bytes as i64 - 8_430_100).abs() <= 1,
            "合计体积应当是 8.42MB + 10.1kB，实际 {bytes}"
        );
    }

    #[test]
    fn image_totals_of_empty_snapshot_is_zero() {
        assert_eq!(Snapshot::default().image_totals(), (0, 0));
    }

    /// 一个只有 CPU / 内存指标有意义的样本（字段形状照 `wslc stats` 真机输出）。
    fn summary_with_stats(cpu: &str, mem: &str) -> ContainerSummary {
        ContainerSummary {
            item: wslc_core::model::ContainerListItem::default(),
            stats: Some(wslc_core::model::ContainerStats {
                cpu_perc: cpu.to_owned(),
                mem_usage: mem.to_owned(),
                ..Default::default()
            }),
        }
    }

    #[test]
    fn dashboard_stats_sums_the_real_machine_numbers() {
        // 真机那一组（`wslc stats` 实测）：redis 0.13% / mysql 0.56%，
        // 内存 27.45MiB + 514.1MiB，上限都是同一个虚拟机的 15.48GiB。
        let mut snap = Snapshot {
            running: vec![
                summary_with_stats("0.13%", "27.45MiB / 15.48GiB"),
                summary_with_stats("0.56%", "514.1MiB / 15.48GiB"),
            ],
            // 全部容器比运行中的多两个（已停止的）
            all: vec![
                summary_with_stats("0.13%", "27.45MiB / 15.48GiB"),
                summary_with_stats("0.56%", "514.1MiB / 15.48GiB"),
                summary_with_stats("0.00%", "3.9MiB / 15.48GiB"),
                summary_with_stats("0.00%", "35.36MiB / 15.48GiB"),
            ],
            wsl_cpu_cores: Some(32),
            ..Default::default()
        };
        // 会话盘：1000 字节里用了 600（VHD 占 120）
        let mut storage = wslc_core::storage::layout(None, PathBuf::from(r"C:\x"), None);
        storage.volume = Some(VolumeSpace {
            total: 1000,
            free: 400,
        });
        storage.vhd_bytes = Some(120);
        snap.storage = Some(storage);

        let stats = snap.dashboard_stats();
        assert_eq!(stats.running, 2);
        assert_eq!(stats.total, 4);
        assert!(
            (stats.cpu_percent - 0.69).abs() < 1e-9,
            "{}",
            stats.cpu_percent
        );
        // 内存是**字节**：MiB 是 1024 进制
        let mib = 1024.0 * 1024.0;
        assert_eq!(stats.mem_used, (27.45 * mib) as u64 + (514.1 * mib) as u64);
        // 上限取容器报的那个（同一个虚拟机，各容器一致）
        assert_eq!(stats.mem_limit, Some((15.48 * 1024.0 * mib) as u64));

        // ⚠️ 这条是这一整块存在的理由：CPU 合计 **不是**整机占用率。
        // `wslc stats` 用的是 Docker 那条公式，100% ≈ 1 个核 ——
        // 0.69% ÷ 32 核 ≈ 0.022%，而不是 0.69%。
        let whole = stats
            .cpu_whole_machine_percent()
            .expect("给了核数就该算得出来");
        assert!((whole - 0.69 / 32.0).abs() < 1e-9, "{whole}");
        // 环下面那行小字（`0.0069 / 32 核`）
        assert!((stats.cpu_cores_used() - 0.0069).abs() < 1e-9);

        // 内存那一环：用了多少 / 上限
        let mem_percent = stats.mem_percent().expect("有上限就该算得出来");
        assert!((mem_percent - (541.55 / (15.48 * 1024.0) * 100.0)).abs() < 1e-6);

        // 存储那一环：**会话 VHD** ÷ 所在盘总量 = 120 / 1000 = 12%
        //
        // ⚠️ 刻意**不是**"盘已用 / 盘总量"（600/1000 = 60%）：那是这台电脑的
        // 磁盘占用率，和面板管的这一摊没关系 —— 实机反馈就是这个
        // （环里写着 50%，而容器数据只有 2.64 GB）。
        let disk = stats.disk.expect("给了 storage 就该有盘");
        assert_eq!(disk.total, 1000);
        assert_eq!(stats.vhd_bytes, Some(120));
        let vhd_percent = stats
            .vhd_percent_of_disk()
            .expect("有 VHD、有盘总量就算得出来");
        assert!((vhd_percent - 12.0).abs() < 1e-9, "{vhd_percent}");
    }

    #[test]
    // ⚠️ `field_reassign_with_default`：这个测试要造**四种**不同的
    // `storage` 组合（无 VHD / 无卷 / 正常 / 卷容量 0），而 `Snapshot` 有
    // 11 个字段。用 `Snapshot { storage: …, ..Default::default() }` 的话
    // 每处都得重复一遍结构体名，反而把"这个测试只关心 storage"埋掉了。
    // 所以这里保留 `let mut snap = Snapshot::default(); snap.storage = …`。
    #[allow(clippy::field_reassign_with_default)]
    fn dashboard_stats_storage_ring_needs_both_the_vhd_and_the_disk() {
        // 缺哪一半都不出百分比：这是**一个比例**，只有分子或只有分母都是编数字。
        // VHD 还没建出来（新装的会话）
        let mut snap = Snapshot::default();
        snap.storage = Some(wslc_core::storage::layout(
            None,
            PathBuf::from(r"C:\x"),
            None,
        ));
        assert_eq!(snap.dashboard_stats().vhd_percent_of_disk(), None);

        // 有 VHD，但读不到卷容量
        let mut snap = Snapshot::default();
        let mut storage = wslc_core::storage::layout(None, PathBuf::from(r"C:\x"), None);
        storage.vhd_bytes = Some(120);
        snap.storage = Some(storage);
        assert_eq!(snap.dashboard_stats().vhd_percent_of_disk(), None);

        // 两个都有 → 算得出来
        let mut storage = wslc_core::storage::layout(None, PathBuf::from(r"C:\x"), None);
        storage.vhd_bytes = Some(120);
        storage.volume = Some(VolumeSpace {
            total: 1000,
            free: 400,
        });
        let mut snap = Snapshot::default();
        snap.storage = Some(storage);
        assert_eq!(snap.dashboard_stats().vhd_percent_of_disk(), Some(12.0));

        // 卷容量是 0（不该发生）→ 不当分母，别去算除零
        let mut storage = wslc_core::storage::layout(None, PathBuf::from(r"C:\x"), None);
        storage.vhd_bytes = Some(120);
        storage.volume = Some(VolumeSpace { total: 0, free: 0 });
        let mut snap = Snapshot::default();
        snap.storage = Some(storage);
        assert_eq!(snap.dashboard_stats().vhd_percent_of_disk(), None);
    }

    #[test]
    fn dashboard_stats_without_cores_reports_no_percentage() {
        // 拿不到核数（`.wslconfig` 没写、环境变量也没读到）时**不能**把
        // 原始合计当百分比：多核机器上 400% 是四个核跑满，画成 400% 的环
        // 只会误导。这时候宁可只给原始合计。
        let snap = Snapshot {
            running: vec![summary_with_stats("400.00%", "1MiB / 2MiB")],
            wsl_cpu_cores: None,
            ..Default::default()
        };
        let stats = snap.dashboard_stats();
        assert_eq!(stats.cpu_percent, 400.0);
        assert_eq!(stats.cpu_whole_machine_percent(), None);
        // 核数写 0（不该发生）也当拿不到，别去算除零
        let zero = Snapshot {
            running: vec![summary_with_stats("10.00%", "")],
            wsl_cpu_cores: Some(0),
            ..Default::default()
        };
        assert_eq!(zero.dashboard_stats().cpu_whole_machine_percent(), None);
    }

    #[test]
    fn dashboard_stats_skips_containers_without_stats_and_nan() {
        // "这一轮没读到统计"和"占用 0"是两件事：前者**不计入**合计
        // （把它当 0 会让合计数看起来比真实占用小 —— 那是编数字）。
        let mut snap = Snapshot {
            running: vec![
                summary_with_stats("1.00%", "1MiB / 4MiB"),
                // 没有 stats：只有列表项
                ContainerSummary {
                    item: wslc_core::model::ContainerListItem::default(),
                    stats: None,
                },
                // `parse_percent` 对 `nan%` 会给回 NaN —— 不能让它污染合计
                summary_with_stats("nan%", "1MiB / 4MiB"),
            ],
            wsl_cpu_cores: Some(4),
            ..Default::default()
        };
        snap.all = snap.running.clone();

        let stats = snap.dashboard_stats();
        assert_eq!(
            stats.running, 3,
            "运行中数的是**列表**，不是有 stats 的那几个"
        );
        assert_eq!(stats.cpu_percent, 1.0, "NaN 与缺失都不该进合计");
        assert_eq!(stats.mem_used, (1.0 * 1024.0 * 1024.0) as u64 * 2);
    }

    #[test]
    fn dashboard_stats_without_a_memory_limit_has_no_percentage() {
        // 上限是 `0B`（`wslc` 在容器刚起来时会这么报）→ 当没有，别去除零
        let snap = Snapshot {
            running: vec![summary_with_stats("1.00%", "0B / 0B")],
            wsl_cpu_cores: Some(4),
            ..Default::default()
        };
        let stats = snap.dashboard_stats();
        assert_eq!(stats.mem_limit, None);
        assert_eq!(stats.mem_percent(), None);
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
            PendingAction::StopAllContainers { running: 3 },
            PendingAction::KillContainer("a".into()),
            PendingAction::RemoveContainer("a".into()),
            PendingAction::PruneContainers,
            PendingAction::RemoveImage("img".into()),
            PendingAction::RemoveNetwork("net".into()),
            PendingAction::RemoveVolume("vol".into()),
            PendingAction::PruneVolumes,
        ];
        for action in actions {
            assert!(!action.title().is_empty());
            assert!(!action.body().is_empty());
            assert!(!action.confirm_label().is_empty());
        }
    }

    #[test]
    fn stop_all_body_names_the_blast_radius() {
        // 确认框一开就盖住了工具栏上那行"N 个在运行"，所以**影响面必须写进正文**
        // —— 这是"停一个"和"停全部"在文案上唯一的区别，也是这个字段存在的理由。
        let action = PendingAction::StopAllContainers { running: 4 };
        assert_eq!(action.title(), "停止所有容器");
        assert_eq!(action.confirm_label(), "全部停止");

        let body = action.body();
        assert!(body.contains('4'), "{body}");
        assert!(body.contains("正在运行的容器"), "{body}");
        // 停止 = 丢未保存数据，这句不能省（单容器那条也有同样的说明）
        assert!(body.contains("未保存的数据"), "{body}");
        // 还得说清"已经停着的不受影响"，否则用户会以为已退出的那些也要被动
        assert!(body.contains("不受影响"), "{body}");
    }

    #[test]
    fn stop_all_summary_lists_names_and_truncates() {
        assert_eq!(
            stopped_all_message(&["web".to_owned()]),
            "已停止 1 个容器：web"
        );

        // 5 个是上限：正好 5 个不加"等"，第 6 个开始截断
        let five: Vec<String> = (0..5).map(|i| format!("c{i}")).collect();
        assert_eq!(
            stopped_all_message(&five),
            "已停止 5 个容器：c0、c1、c2、c3、c4"
        );
        let six: Vec<String> = (0..6).map(|i| format!("c{i}")).collect();
        let msg = stopped_all_message(&six);
        assert!(msg.starts_with("已停止 6 个容器："), "{msg}");
        assert!(!msg.contains("c5"), "第 6 个名字不该出现在摘要里：{msg}");
        assert!(msg.ends_with('等'), "{msg}");

        // 一行都没打印时**不能**说"已停止 0 个容器"——那读起来像"成功了，但什么也没发生"
        let none: Vec<String> = Vec::new();
        let empty = stopped_all_message(&none);
        assert!(!empty.contains('0'), "{empty}");
        assert!(empty.contains("没有报告"), "{empty}");
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

    // -- 「创建容器」的启动命令那一格 ---------------------------------------

    #[test]
    fn create_command_leaves_the_image_default_alone_when_empty() {
        // 空 → **一条命令都不给**，镜像自己的 ENTRYPOINT / CMD 才有机会生效。
        // 这条是 mysql / nginx 那类镜像的命门：返回 `["/bin/sh","-c",""]` 同样
        // 算"用户给了命令"，会把 CMD 顶掉 —— 表现是"本来能跑，填了个空反而起不来"。
        assert!(create_command("").is_empty());
        // 只敲空格 / 制表符也算没敲（不 trim 就是 `sh -c " "`，起来就退）
        assert!(create_command("   ").is_empty());
        assert!(create_command("\t\n ").is_empty());
    }

    #[test]
    fn create_command_goes_through_a_shell_and_trims() {
        assert_eq!(
            create_command("  tail -f /dev/null  "),
            vec!["/bin/sh", "-c", "tail -f /dev/null"]
        );
        // `/bin/sh` 而不是 `/bin/bash`：alpine 这类镜像里没有 bash
        assert_eq!(create_command("java -jar /app/a.jar")[0], "/bin/sh");
        // 管道 / 重定向不能静默失效（理由同 `exec_argv`）
        assert_eq!(
            create_command("nginx -g 'daemon off;' | tee /tmp/log")[2],
            "nginx -g 'daemon off;' | tee /tmp/log"
        );
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
    fn ui_sections_start_collapsed() {
        let state = AppState::new(Wslc::new(), Wsl::new());
        for section in [
            UiSection::CreateAdvanced,
            UiSection::CreateCommandPreview,
            UiSection::ConfigRaw,
            UiSection::WslHelp,
            UiSection::DeployProxy,
            UiSection::DeployProxyMore,
            UiSection::PullSource,
            UiSection::DbEncoding,
        ] {
            assert!(!state.section_open(section), "{section:?} 应默认折叠");
        }
    }

    #[test]
    fn ui_sections_are_independent() {
        let mut state = AppState::new(Wslc::new(), Wsl::new());
        state.set_section_open(UiSection::DeployProxy, true);
        assert!(!state.section_open(UiSection::DeployProxyMore));
        assert!(!state.section_open(UiSection::CreateAdvanced));

        state.set_section_open(UiSection::DeployProxyMore, true);
        state.set_section_open(UiSection::DeployProxy, false);
        assert!(!state.section_open(UiSection::DeployProxy));
        assert!(state.section_open(UiSection::DeployProxyMore));
        assert!(!state.section_open(UiSection::CreateAdvanced));
    }

    #[test]
    fn pull_source_and_db_encoding_sections_are_independent() {
        let mut state = AppState::new(Wslc::new(), Wsl::new());
        state.set_section_open(UiSection::CreateAdvanced, true);
        for (section, other) in [
            (UiSection::PullSource, UiSection::DbEncoding),
            (UiSection::DbEncoding, UiSection::PullSource),
        ] {
            state.set_section_open(section, true);
            assert!(state.section_open(section));
            assert!(!state.section_open(other));
            assert!(state.section_open(UiSection::CreateAdvanced));
            state.toggle_section(section);
            assert!(!state.section_open(section));
            assert!(!state.section_open(other));
            assert!(state.section_open(UiSection::CreateAdvanced));
        }
    }

    #[test]
    fn ui_sections_can_close_and_reopen() {
        let mut state = AppState::new(Wslc::new(), Wsl::new());
        let section = UiSection::CreateAdvanced;
        state.set_section_open(section, false);
        assert!(!state.section_open(section));
        state.set_section_open(section, true);
        state.set_section_open(section, true);
        assert!(state.section_open(section));
        state.set_section_open(section, false);
        state.set_section_open(section, false);
        assert!(!state.section_open(section));
        state.set_section_open(section, true);
        assert!(state.section_open(section));
    }

    #[test]
    fn ui_sections_toggle_without_affecting_other_sections() {
        let mut state = AppState::new(Wslc::new(), Wsl::new());
        state.set_section_open(UiSection::ConfigRaw, true);
        for section in [UiSection::CreateCommandPreview, UiSection::WslHelp] {
            state.toggle_section(section);
            assert!(state.section_open(section));
            assert!(state.section_open(UiSection::ConfigRaw));
            state.toggle_section(section);
            assert!(!state.section_open(section));
            assert!(state.section_open(UiSection::ConfigRaw));
        }
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

    // -- 数据库管理页（v0.7.0）---------------------------------------------

    /// 一份探测结果（形状照 `cmd::mysql::detect` 真实返回的那个）。
    fn probe_with(hint: RootCredHint) -> wslc_core::cmd::mysql::MysqlProbe {
        wslc_core::cmd::mysql::MysqlProbe {
            family: MysqlFamily::Mysql,
            client: wslc_core::model::ClientProbe {
                bin: "mysql".to_owned(),
                path: "/usr/bin/mysql".to_owned(),
                version: Some("mysql  Ver 26.7.0 for Linux on x86_64".to_owned()),
            },
            root_hint: hint,
            container: "mysql".to_owned(),
        }
    }

    #[test]
    fn native_connection_blocks_the_exec_only_write_paths() {
        // v0.16 的缺口：填了连接串之后，页面读数据走原生（不经过容器），
        // 但**备份 / 恢复 / 导入 / 建库建用户 / 删库 / 删用户**这六条仍然
        // 是 `wslc exec <容器> <客户端>` —— 而那时 `db.client` 是 `native`
        // 这个哨兵值（不是二进制名）。不拦的后果是用户拿到
        // `exec: native: not found` 这种**指错方向**的错。
        let mut state = DbState::from_probe(&probe_with(RootCredHint::Unknown), "mysql:8");
        // 没填连接串：六条都能做
        assert!(!state.is_native());
        assert!(state.write_blocked_reason().is_none());

        // ① 刚点「应用连接串」、结果还没读回来时也要算原生 —— 否则这一小段
        //    窗口里 `client` 还是上一个容器的 `mysql`，点下去会真的去 exec。
        state.dsn = Some(DsnText::new("mysql://root:pw@10.0.0.5:3306/shop"));
        assert!(state.is_native(), "dsn 已生效就该判成原生");
        let reason = state
            .write_blocked_reason()
            .expect("原生连接必须拦住写操作");
        // 文案要短，但必须留着**动作**（"清空连接串"）——没有它用户只知道
        // 按钮没了，不知道怎么办。
        assert!(reason.contains("连接串"), "理由要说清怎么改回去：{reason}");
        assert!(
            reason.chars().count() <= 40,
            "这条提示要控制在 40 字以内（它出现在两个 tab 顶部）：{reason}"
        );

        // ② 探测回来之后：`apply_probe` 把哨兵写进 `client`
        let mut probe = probe_with(RootCredHint::Unknown);
        probe.client.bin = NATIVE_CLIENT.to_owned();
        state.dsn = None; // 只看 client 也必须判出来
        state.apply_probe(&probe);
        assert!(
            state.is_native(),
            "`apply_probe` 写进来的哨兵也要被判成原生 —— \
             否则 dsn 一旦被别处清掉，写操作就会去打 exec"
        );
        assert!(state.write_blocked_reason().is_some());

        // ③ 改回容器内执行：拦阻解除
        state.client = "mysql".to_owned();
        assert!(!state.is_native());
        assert!(
            state.write_blocked_reason().is_none(),
            "改回容器内执行之后必须立刻放行"
        );
    }

    #[test]
    fn dsn_debug_never_leaks_the_password() {
        // `DbState` / `AppState` 都是 `#[derive(Debug)]` —— 只要 `dsn` 是裸
        // `String`，任何一次 `{:?}` 就会把口令打进日志文件（而日志是用户会
        // 贴到 issue 里的东西）。这个 newtype 就是为了堵这一条。
        let dsn = DsnText::new("mysql://root:s3cret-pw@10.0.0.5:3306/shop");
        let shown = format!("{dsn:?}");
        assert!(!shown.contains("s3cret-pw"), "Debug 里漏了口令：{shown}");
        assert!(shown.contains("***"), "该遮的那段要显示成 ***：{shown}");
        // 其余部分**保留** —— 协议 / 主机 / 库名正是排障要看的
        assert!(shown.contains("10.0.0.5"), "主机名不该被遮：{shown}");
        assert!(shown.contains("shop"), "库名不该被遮：{shown}");
        assert!(shown.contains("root"), "用户名不该被遮：{shown}");

        // 原文仍然取得到（真正连接要用它）
        assert_eq!(dsn.as_str(), "mysql://root:s3cret-pw@10.0.0.5:3306/shop");
    }

    #[test]
    fn dsn_redaction_handles_the_shapes_people_actually_paste() {
        // 口令里带未编码的 `@`：按**最后一个** `@` 切，否则主机名会被当成口令
        assert_eq!(
            redact_dsn_password("mysql://root:p@ss@10.0.0.5:3306/db"),
            "mysql://root:***@10.0.0.5:3306/db"
        );
        // 没有口令（`user@host`）：原样
        assert_eq!(
            redact_dsn_password("postgres://alice@h:5432/db"),
            "postgres://alice@h:5432/db"
        );
        // 没有 userinfo：原样
        assert_eq!(
            redact_dsn_password("postgres://h:5432/db"),
            "postgres://h:5432/db"
        );
        // 不像连接串的东西：原样返回，别瞎改
        assert_eq!(redact_dsn_password("host=127.0.0.1"), "host=127.0.0.1");
        assert_eq!(redact_dsn_password(""), "");
        // 没有库名、也没有端口
        assert_eq!(
            redact_dsn_password("mysql://root:pw@h"),
            "mysql://root:***@h"
        );
        // 查询串（我们目前拒绝带它的连接串，但脱敏不该因此 panic）
        assert_eq!(
            redact_dsn_password("postgres://u:p@h:5432/db?sslmode=require"),
            "postgres://u:***@h:5432/db?sslmode=require"
        );
    }

    #[test]
    fn db_page_is_not_in_the_sidebar_but_needs_a_window() {
        // 「数据库管理」只能从容器行上的按钮进 —— 和「添加实例」同一个取舍，
        // 所以它**不在** `Page::ALL` 里。
        assert!(!Page::ALL.contains(&Page::Database));
        assert_eq!(Page::Database.group(), "容器");
        assert_eq!(Page::Database.label(), "数据库管理");
        // 连接卡上有输入框 → 进这一页必须有 window
        assert!(Page::Database.needs_window_to_enter());
    }

    #[test]
    fn deploy_page_is_not_in_the_sidebar_and_does_not_need_a_window() {
        // 「部署管理」和「数据库管理」都服务**某一个**容器，只能从容器列表行上的
        // 按钮进，所以**不在** `Page::ALL` 里。
        assert!(!Page::ALL.contains(&Page::Deploy));
        // 和 `Containers` 同组，侧边栏里不会多出一个组标题
        assert_eq!(Page::Deploy.group(), "容器");
        assert_eq!(Page::Deploy.label(), "部署管理");
        // ⚠️ **不需要 window** —— 这一页一个输入框都没有。
        // 「添加部署」那五个框是点按钮那一刻才建的（`open_deploy_dialog` 有
        // window），页面本身进来就能画。这和 v0.6.3 的「拉取镜像」是同一个形状。
        //
        // 写成 `true` 不是"更保险"：`set_page` 里会给它一个**永远走不到**的分支。
        assert!(
            !Page::Deploy.needs_window_to_enter(),
            "部署页没有常驻输入框 —— 表单在弹窗里，是点的时候建的"
        );
    }

    #[test]
    fn a_freshly_opened_deploy_page_can_be_loaded() {
        // ⚠️ 和 `a_freshly_opened_db_page_can_be_loaded` 是同一条教训：
        // `provisional` 里把 `busy` 置 true 会让 `load_deploy` 的第一句守卫
        // 把**第一次**加载挡掉，页面永远停在「正在读取…」。
        // v0.8 在数据库页上真的踩过一次，这里把它钉死。
        let state = DeployState::provisional(
            "nginx",
            "docker.1panel.live/library/nginx:latest",
            AppRuntime::Nginx,
            DeploySignal::Image,
        );
        assert!(
            !state.busy,
            "新建的状态不能是 busy：`load_deploy` 会因为这一条直接返回"
        );
        assert!(!state.loaded);
        // 列表此时是空的（存档还没读进来），但页面已经能画了
        assert!(state.entries.is_empty());
        assert!(state.progress.is_none());
    }

    /// 造一行"已部署列表"（测试用）。
    fn deploy_entry(app: &str, runtime_key: &str) -> DeployEntry {
        DeployEntry {
            record: crate::deploys::DeployRecord {
                app: app.to_owned(),
                runtime: runtime_key.to_owned(),
                source: format!(r"E:\build\{app}"),
                target: format!("/opt/{app}"),
                command: format!("./{app}"),
                artifact: app.to_owned(),
                port: 8080,
                members: vec![app.to_owned()],
                site: None,
            },
            runtime: AppRuntime::from_key(runtime_key),
            process: None,
            files: None,
            probed: false,
            probe_error: None,
        }
    }

    #[test]
    fn deploy_entries_resolve_the_runtime_key_from_the_archive() {
        let mut state = DeployState::provisional(
            "nginx",
            "nginx:latest",
            AppRuntime::Nginx,
            DeploySignal::Image,
        );
        let records = vec![
            deploy_entry("site", "nginx").record,
            deploy_entry("api", "go").record,
        ];
        state.set_records(&records);

        assert_eq!(state.entries.len(), 2);
        // 运行时从存档里的**稳定键**还原（不是从显示名，也不是靠猜）
        assert_eq!(state.entries[0].runtime, Some(AppRuntime::Nginx));
        assert_eq!(state.entries[1].runtime, Some(AppRuntime::Go));
        // `set_records` 只填记录 —— 现状要问容器，所以这时**还没问过**
        assert!(state.entries.iter().all(|e| !e.probed));
        assert!(state.entries.iter().all(|e| e.files.is_none()));

        assert_eq!(state.index_of("api"), Some(1));
        assert_eq!(state.index_of("nope"), None);
    }

    #[test]
    fn an_unreadable_runtime_key_is_reported_not_guessed() {
        // 存档被手改坏（或者更新过的版本写了别的键）时，**不猜默认值**
        //（猜错会把文件铺到别的地方），而是如实说"认不出来"。
        let mut state = DeployState::provisional(
            "nginx",
            "nginx:latest",
            AppRuntime::Nginx,
            DeploySignal::Image,
        );
        state.set_records(&[deploy_entry("weird", "rust").record]);

        assert_eq!(
            state.entries.len(),
            1,
            "认不出来也要留在列表里，不能悄悄藏起来"
        );
        assert_eq!(state.entries[0].runtime, None);
        assert_eq!(state.entries[0].status_label(), "类型认不出来");
        assert!(!state.entries[0].is_process());
    }

    #[test]
    fn deploy_status_labels_tell_the_three_shapes_apart() {
        // 还没问过容器
        let mut entry = deploy_entry("api", "go");
        assert_eq!(entry.status_label(), "读取中…");

        // 问过了：进程跑着
        entry.probed = true;
        entry.process = Some(ProcessStatus::Running);
        entry.files = Some(4);
        assert_eq!(entry.status_label(), "运行中");
        assert!(entry.is_process());

        // 进程停了，但目标目录里还有东西 → 「已停止」
        entry.process = Some(ProcessStatus::Stopped);
        assert_eq!(entry.status_label(), "已停止");

        // 进程停了、目标目录也没了 → 「目标不存在」
        // （容器被删掉重建之后就会是这一种，见 `crate::deploys` 的说明）
        entry.files = None;
        assert_eq!(entry.status_label(), "目标不存在");

        // 内容形态（nginx）没有属于我们的进程，看文件在不在
        let mut content = deploy_entry("site", "nginx");
        content.probed = true;
        content.files = Some(12);
        assert_eq!(content.status_label(), "已部署");
        assert!(!content.is_process(), "nginx 是内容形态，没有启动/停止");

        content.files = Some(0);
        assert_eq!(content.status_label(), "目标目录是空的");

        content.files = None;
        assert_eq!(content.status_label(), "目标不存在");
    }

    #[test]
    fn deploy_state_takes_the_authoritative_runtime_from_the_probe() {
        // 列表那条启发式先给一个（可能是按命令猜的），inspect 回来后覆盖
        let mut state = DeployState::provisional(
            "web",
            "my-company/web:1",
            AppRuntime::Node,
            DeploySignal::Command,
        );
        let probe = wslc_core::cmd::deploy::DeployProbe {
            container: "web".to_owned(),
            image: "docker.1panel.live/library/nginx:latest".to_owned(),
            runtime: AppRuntime::Nginx,
            web_root: Some("/usr/share/nginx/html".to_owned()),
            ports: vec![(
                "80/tcp".to_owned(),
                vec![("127.0.0.1".to_owned(), "80".to_owned())],
            )],
            networks: vec!["bridge".to_owned()],
        };
        state.apply_probe(&probe);

        assert_eq!(state.runtime, AppRuntime::Nginx);
        assert_eq!(state.image, "docker.1panel.live/library/nginx:latest");
        // 权威判定来自 inspect（字段完整、没被截断）→ 依据记成镜像名那一档
        assert_eq!(state.signal, DeploySignal::Image);
        // Web 根是从挂载点推出来的（内容形态的默认目标目录用它）
        assert_eq!(state.web_root.as_deref(), Some("/usr/share/nginx/html"));
        // 判端口发布用的是**唯一那份**纯函数（界面和这里调的是同一个）
        assert!(wslc_core::model::deploy::port_is_published(
            &probe.ports,
            80
        ));
        assert!(!wslc_core::model::deploy::port_is_published(
            &probe.ports,
            8080
        ));
        assert!(
            !state.busy,
            "探测回来必须把忙清掉，否则按钮永远是「读取中…」"
        );
    }

    #[test]
    fn every_deploy_phase_says_what_it_is_doing() {
        // ⚠️ 「停止」曾经复用 `Starting`，于是那一行写着"正在启动…" ——
        // 按钮和文案对不上，是实机一眼能看出来、CI 看不出来的那类问题。
        assert_eq!(DeployPhase::Starting.label(), "正在启动…");
        assert_eq!(DeployPhase::Stopping.label(), "正在停止…");
        assert_ne!(DeployPhase::Stopping.label(), DeployPhase::Starting.label());
        assert_eq!(DeployPhase::Uploading.label(), "正在上传…");
        assert_eq!(DeployPhase::Configuring.label(), "正在写入站点配置…");
        assert_eq!(DeployPhase::Removing.label(), "正在删除…");
    }

    #[test]
    fn deploy_failure_always_gives_a_next_step() {
        // 实测抓下来的原文（容器没运行）
        let failure = DeployFailure::from_text(
            "容器\"x\"未运行。\n错误代码： WSLC_E_CONTAINER_NOT_RUNNING".to_owned(),
        );
        assert_eq!(failure.kind, DeployErrorKind::ContainerNotRunning);
        assert!(
            failure.summary().contains("启动"),
            "提示条要说清下一步：{}",
            failure.summary()
        );
        // 分类认不出来时不能空着 —— 给原文第一行
        let other = DeployFailure::from_text("没法归类的一句话\n第二行".to_owned());
        assert_eq!(other.summary(), "没法归类的一句话");
    }

    #[test]
    fn a_freshly_opened_db_page_can_be_loaded() {
        // ⚠️ 实机踩过（v0.8.0）：`provisional` 里一开始把 `busy` 置成了 `true`
        // （想法是"马上就要去探测，先算忙"），而 `Shell::load_db` 的第一句是
        // "正在跑就不叠第二次" —— **第一次**加载就被自己挡掉了，页面永远停在
        // 「正在读取…」，页头按钮永远是「读取中…」，整个数据库管理页没法用。
        //
        // CI 拦不住这个：类型全对、`core` 全绿，只有实机点一下才看得见。
        // 所以这条断言把"新建出来的状态必须是**可加载**的"钉死在这里。
        //
        // 「忙」的语义只能是"已经有一个后台任务在跑" —— 置位的责任在
        // 真正起任务的那个函数（`load_db` 进去就置 true），不在构造函数里。
        let state = DbState::provisional("mysql", "mysql:8", MysqlFamily::Mysql);
        assert!(
            !state.busy,
            "新建的状态不能是 busy：`load_db` 会因为这一条直接返回，页面卡在「正在读取…」"
        );
        // 但**没读过**这件事要保留 —— 界面靠它区分"还没读"和"读到空的"
        assert!(!state.loaded);
        // 兜底家族给的客户端名（权威判定由 apply_probe 覆盖）
        assert_eq!(state.client, "mysql");
        // v0.13：`provisional` 同时把多驱动层的连接摆好（现在只是镜像，
        // 还没有代码读它 —— 但"家族 → 引擎"的换算必须是对的，
        // 否则阶段 2 接上 PostgreSQL 时这里会悄悄全变成 MySQL）。
        assert_eq!(state.conn.kind, wslc_core::db::DbKind::MySql);
        assert_eq!(
            state.conn.host,
            wslc_core::db::DbHost::Container("mysql".into())
        );
        assert_eq!(state.conn.port, 3306);
    }

    #[test]
    fn db_user_list_never_includes_system_or_root() {
        let mut state = DbState::provisional("mysql", "mysql:8", MysqlFamily::Mysql);
        state.users = vec![
            DbUser {
                user: "djxm_user".to_owned(),
                host: "%".to_owned(),
                plugin: "caching_sha2_password".to_owned(),
                super_priv: false,
            },
            DbUser {
                user: "mysql.sys".to_owned(),
                host: "localhost".to_owned(),
                plugin: "caching_sha2_password".to_owned(),
                super_priv: false,
            },
            DbUser {
                user: "root".to_owned(),
                host: "%".to_owned(),
                plugin: "caching_sha2_password".to_owned(),
                super_priv: true,
            },
        ];

        // 系统账号和 root 都不在可见列表里，没有"开关"能放出来
        let visible: Vec<&DbUser> = state.visible_users().collect();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].user, "djxm_user");
    }

    #[test]
    fn grant_database_options_skip_system_schemas_and_sort() {
        let mut state = DbState::provisional("mysql", "mysql:8", MysqlFamily::Mysql);
        // 故意给一个**乱序**的列表：`list_databases_sql` 现在带 `ORDER BY`，
        // 但下拉框的顺序不该依赖那条 SQL（用户会凭位置点选）。
        state.databases = ["shop", "mysql", "blog", "information_schema", "app"]
            .into_iter()
            .map(|name| DbEntry {
                name: name.to_owned(),
                charset: "utf8mb4".to_owned(),
                collation: "utf8mb4_0900_ai_ci".to_owned(),
                tables: 0,
                bytes: 0,
            })
            .collect();

        // 系统库不给候选：`GRANT ALL ON mysql.*` 近似提权，而下拉框是
        // "选一下就生效"的控件 —— 误选的代价太大（要授就给旁边那个输入框手打）。
        assert_eq!(
            state.grant_database_options(),
            vec!["app".to_owned(), "blog".to_owned(), "shop".to_owned()]
        );
    }

    #[test]
    fn grant_database_options_are_empty_before_the_lists_load() {
        // 还没读到列表（`loaded == false`）时是空候选 —— 界面那边据此画空状态，
        // 而不是画一个"展开什么都没有"的下拉框。
        let state = DbState::provisional("mysql", "mysql:8", MysqlFamily::Mysql);
        assert!(state.grant_database_options().is_empty());
    }

    #[test]
    fn db_state_takes_the_root_password_from_the_container_env() {
        let state =
            DbState::from_probe(&probe_with(RootCredHint::Password("pw".into())), "mysql:8");
        assert_eq!(state.creds.user, "root");
        assert_eq!(state.creds.password, "pw");
        assert!(state.cred_note.is_some(), "要说清密码是哪来的");
        assert!(state.usable());
        // 还没读过列表：界面要区分"还没读"和"读到空的"
        assert!(!state.loaded);
        assert!(state.databases.is_empty());
        assert_eq!(state.tab, DbTab::Databases);
        assert_eq!(state.client, "mysql");
        assert_eq!(state.family, MysqlFamily::Mysql);
    }

    #[test]
    fn db_state_leaves_the_password_empty_for_random_and_unknown() {
        let random = DbState::from_probe(&probe_with(RootCredHint::Random), "mysql:8");
        assert_eq!(random.creds.password, "");
        assert!(
            random
                .cred_note
                .as_deref()
                .unwrap_or_default()
                .contains("随机"),
            "{:?}",
            random.cred_note
        );

        // 什么都没探测到 → 连说明都不给（没有可说的，别凑一句）
        let unknown = DbState::from_probe(&probe_with(RootCredHint::Unknown), "mysql:8");
        assert!(unknown.cred_note.is_none());
        // 空密码是**合法**状态（`MYSQL_ALLOW_EMPTY_PASSWORD`），所以不算"不可用"
        assert!(unknown.usable());
    }

    #[test]
    fn db_state_debug_never_shows_the_password() {
        // ⚠️ `AppState` 是 `#[derive(Debug)]`，它会一路打印到 `DbState` 和
        // 里面的 `MysqlCreds` —— 这条守住"密码不进日志"。
        let state = DbState::from_probe(
            &probe_with(RootCredHint::Password("s3cret-pw".into())),
            "mysql:8",
        );
        let text = format!("{state:?}");
        assert!(!text.contains("s3cret-pw"), "{text}");
        assert!(text.contains("***"), "{text}");
        // 用户名、容器名还是要看得见的（排障要用）
        assert!(text.contains("root"), "{text}");
        assert!(text.contains("mysql"), "{text}");
    }

    #[test]
    fn db_failure_gives_the_next_step_for_the_real_errors() {
        let denied = DbFailure::from_text(
            "ERROR 1045 (28000): Access denied for user 'root'@'localhost' (using password: YES)",
        );
        assert_eq!(denied.kind, MysqlErrorKind::AccessDenied);
        assert!(denied.summary().contains("密码"), "{denied:?}");

        let stopped =
            DbFailure::from_text("容器“947c4f25”未运行。错误代码： WSLC_E_CONTAINER_NOT_RUNNING");
        assert_eq!(stopped.kind, MysqlErrorKind::ContainerNotRunning);
        assert!(stopped.summary().contains("没在运行"), "{stopped:?}");

        let no_client = DbFailure::from_text(
            "OCI runtime exec failed: exec: \"mysql\": executable file not found in $PATH",
        );
        assert_eq!(no_client.kind, MysqlErrorKind::ClientMissing);
        assert!(no_client.summary().contains("客户端"), "{no_client:?}");
    }

    #[test]
    fn db_failure_summary_is_one_line_and_survives_multibyte_output() {
        // 认不出来的错误：给原文第一行；长文本按**字符**截断（按字节会 panic）
        let weird = DbFailure::from_text(format!("{}\n第二行不该出现", "汉".repeat(300)));
        assert_eq!(weird.kind, MysqlErrorKind::Unknown);
        let summary = weird.summary();
        assert!(summary.ends_with('…'), "{summary}");
        assert_eq!(summary.chars().count(), 121);
        assert!(!summary.contains("第二行"), "{summary}");
    }

    #[test]
    fn db_drop_actions_say_what_they_destroy_and_never_leak_the_password() {
        let drop_db = PendingAction::DropDatabase {
            container: "mysql".to_owned(),
            database: "shop".to_owned(),
            client: "mysql".to_owned(),
            creds: MysqlCreds::root("s3cret-pw"),
        };
        assert_eq!(drop_db.title(), "删除数据库");
        assert_eq!(drop_db.confirm_label(), "删除数据库");
        let body = drop_db.body();
        assert!(body.contains("DROP DATABASE shop"), "{body}");
        assert!(body.contains("不可撤销"), "{body}");
        assert!(!body.contains("s3cret-pw"), "确认文案里漏了密码：{body}");

        let drop_user = PendingAction::DropUser {
            container: "mysql".to_owned(),
            user: "app".to_owned(),
            host: "%".to_owned(),
            client: "mysql".to_owned(),
            creds: MysqlCreds::root("s3cret-pw"),
        };
        assert_eq!(drop_user.title(), "删除 MySQL 用户");
        assert!(drop_user.body().contains("'app'@'%'"), "{drop_user:?}");
        assert!(
            !drop_user.body().contains("s3cret-pw"),
            "确认文案里漏了密码"
        );
    }

    #[test]
    fn db_error_state_clears_before_the_next_action() {
        let mut state = DbState::from_probe(&probe_with(RootCredHint::Unknown), "mysql:8");
        state.fail("ERROR 1045 (28000): Access denied");
        assert!(state.error.is_some());
        assert!(state.error.as_ref().unwrap().hint().is_some());
        state.clear_error();
        assert!(state.error.is_none());
    }

    #[test]
    fn import_and_backup_progress_can_be_put_in_the_background() {
        // ⚠️ 「放到后台」只收遮罩，**不取消**任务：
        //
        // - `dismissed = true` 时渲染那边什么都不画（`db_import_progress` /
        //   `db_backup_progress` 的第一句）；
        // - 但 `import` / `backup` 仍然是 `Some` —— 这两个 `Option` 同时是
        //   **互斥锁**（`start_db_import` / `start_db_backup` 的第一句就查它们）。
        //   置 `None` 会放开"再来一份"的口子：两份 `container cp` 抢同一个
        //   目录，或者两份 mysqldump 撞同一个备份文件名。
        let mut db = DbState::from_probe(&probe_with(RootCredHint::Unknown), "mysql:8");
        db.import = Some(ImportState {
            file_name: "shop.sql".to_owned(),
            database: Some("shop".to_owned()),
            dismissed: false,
        });
        db.backup = Some(BackupState {
            database: "shop".to_owned(),
            container_path: "/tmp/wslc-backups/x.sql".to_owned(),
            windows_path: std::path::PathBuf::from(r"C:\x\shop-1.sql"),
            last_message: None,
            dismissed: false,
        });

        // 刚起来时浮层是可见的
        assert!(!db.import.as_ref().unwrap().dismissed);
        assert!(!db.backup.as_ref().unwrap().dismissed);

        // 收起来之后：两个任务**都还在**（互斥锁没被放开）
        db.import.as_mut().unwrap().dismissed = true;
        db.backup.as_mut().unwrap().dismissed = true;
        assert!(db.import.is_some(), "收起浮层不能把互斥锁放掉");
        assert!(db.backup.is_some(), "收起浮层不能把互斥锁放掉");
        assert!(db.import.as_ref().unwrap().dismissed);
        assert!(db.backup.as_ref().unwrap().dismissed);
    }

    #[test]
    fn deleting_a_backup_file_asks_for_confirmation_and_names_both_copies() {
        // ⚠️ 这个动作原先**没有**二次确认，而「恢复」（可恢复的那个）有 ——
        // 两个配置反着。凭据是：删除是不可撤销的（Windows 侧文件 + 容器内
        // `rm -f`，面板里没有回收站），而恢复至少还能再备份回来。
        let action = PendingAction::DeleteDatabaseBackup {
            container: "mysql".to_owned(),
            database: "djxm".to_owned(),
            windows_path: std::path::PathBuf::from(
                r"C:\Users\x\backups\mysql\djxm-20261015-035901.sql",
            ),
            container_path: "/tmp/wslc-backups/djxm-20261015-035901.sql".to_owned(),
        };

        assert_eq!(action.title(), "删除备份文件");
        assert_eq!(action.confirm_label(), "永久删除");

        let body = action.body();
        // 用户必须看见**两份**都会没：Windows 侧那份是主体，容器内那份也会删
        assert!(
            body.contains(r"C:\Users\x\backups\mysql\djxm-20261015-035901.sql"),
            "{body}"
        );
        assert!(
            body.contains("/tmp/wslc-backups/djxm-20261015-035901.sql"),
            "{body}"
        );
        // 说清"数据库本身不受影响" —— 否则用户会以为连库一起删了
        assert!(body.contains("数据库 djxm 本身不受影响"), "{body}");
        assert!(
            body.contains("不可撤销") || body.contains("没有回收站"),
            "{body}"
        );

        // 它**不**走 `PendingAction::execute`（那个形状表达不了"删完再重扫目录"），
        // 所以执行入口必须给一句人话，而不是 panic。
        // `execute` 要 `&Wslc`；这里只验证这个变体不会走到"跑一条命令"那条路。
        // （真实分流在 `Shell::confirm_pending` 里，见那边的注释。）
    }

    #[test]
    fn backup_file_names_parse_and_junk_is_skipped() {
        // 面板自己生成的形状：`<库名>-<日期>-<时间>.sql`
        let (database, stamp) =
            parse_backup_file_name("djxm-20261015-035901.sql").expect("应能解出");
        assert_eq!(database, "djxm");
        assert_eq!(stamp, "20261015-035901");
        // 库名自己带 `-`：从**右边**解，库名保持完整
        let (database, _) =
            parse_backup_file_name("my-app-db-20261015-035901.sql").expect("应能解出");
        assert_eq!(database, "my-app-db");
        // 不认识的文件**跳过**（目录里可能有用户手动拷进来的东西）
        assert_eq!(parse_backup_file_name("readme.txt"), None);
        assert_eq!(parse_backup_file_name("abc.sql"), None);
        assert_eq!(parse_backup_file_name("abc-20261015.sql"), None);
        assert_eq!(parse_backup_file_name("abc-2026-035901.sql"), None);
        assert_eq!(parse_backup_file_name(""), None);
    }

    #[test]
    fn scan_backup_dir_sorts_newest_first_and_skips_junk() {
        let dir = std::env::temp_dir().join(format!("wslc-scan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a-20261015-035901.sql"), "x").unwrap();
        std::fs::write(dir.join("a-20261016-035901.sql"), "xy").unwrap();
        std::fs::write(dir.join("b-20261010-000000.sql"), "xyz").unwrap();
        // 垃圾文件：解不出来 → 跳过
        std::fs::write(dir.join("notes.txt"), "junk").unwrap();

        let entries = scan_backup_dir(&dir).expect("应能扫出备份");
        assert_eq!(entries.len(), 3, "{entries:?}");
        // 新的在上（时间戳定宽，字典序 == 时间序）
        assert_eq!(entries[0].database, "a");
        assert_eq!(entries[0].stamp, "20261016-035901");
        assert_eq!(entries[0].bytes, 2);
        assert_eq!(entries[2].database, "b");
        // 容器内那份的路径由文件名推出来（不一定还在，恢复前会查）
        assert_eq!(
            entries[0].container_path,
            "/tmp/wslc-backups/a-20261016-035901.sql"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// `DbState` 的 `Debug` 里**不能**出现明文密码（守门断言）。
    ///
    /// 以前这里还顺带测"`DbState` 上那份凭据克隆"—— 那个字段已经删掉了
    /// （它就是"列表里永远显示 `—`"的根因：渲染读它，而它从没被同步过）。
    /// 这条留下来是因为 `AppState` / `DbState` 的 `Debug` 会一路把
    /// `MysqlCreds` 带出来，而那里面**有密码**（`MysqlCreds` 的 `Debug`
    /// 是手写脱敏的）。凭据存档自己的脱敏在 `dbcreds::tests` 里。
    #[test]
    fn db_state_debug_never_leaks_the_password() {
        let state = DbState::from_probe(
            &probe_with(RootCredHint::Password("s3cret-pw".into())),
            "mysql:8",
        );
        let text = format!("{state:?}");
        assert!(!text.contains("s3cret-pw"), "{text}");
        assert!(text.contains("***"), "{text}");
    }

    #[test]
    fn toasts_expire_on_their_own() {
        // 报错给双倍时间（文本更长、用户往往要照着它做事）
        assert_eq!(
            Toast::error("boom").lifetime(),
            std::time::Duration::from_secs(8)
        );
        assert_eq!(
            Toast::info("hi").lifetime(),
            std::time::Duration::from_secs(4)
        );
        assert_eq!(
            Toast::success("ok").lifetime(),
            std::time::Duration::from_secs(4)
        );
        // 刚弹出来的**不能**被收掉（否则提示会一闪而过）
        assert!(!Toast::success("ok").lifetime().is_zero(), "4 秒不能是 0");
    }

    #[test]
    fn expired_toasts_get_cleared_and_the_rest_survive() {
        // 和这个模块里其它 AppState 测试同一个造法（`AppState::new` 会读一次
        // 偏好 / 存档，那几条测试早就这么干了）
        let mut state = AppState::new(wslc_core::Wslc::new(), wslc_core::Wsl::new());
        // 没有提示时什么都不做（也不该报"清掉了"）
        assert!(!state.clear_expired_toast());

        // 刚弹的：留着重绘信号应该是 **false**（每半秒无脑重绘会把界面拖住）
        state.notify(Toast::info("hi"));
        assert!(!state.clear_expired_toast());
        assert!(state.toast.is_some(), "刚弹的提示不该被收掉");

        // 把它"变老"到超过自己的寿命，就该被收掉、并且报告 true
        if let Some(t) = state.toast.as_mut() {
            t.shown_at =
                std::time::Instant::now() - (t.lifetime() + std::time::Duration::from_millis(1));
        }
        assert!(
            state.clear_expired_toast(),
            "过期的提示应当被收掉并报告 true"
        );
        assert!(state.toast.is_none());
        // 已经空了：再来一次不该报 true
        assert!(!state.clear_expired_toast());
    }

    #[test]
    // `field_reassign_with_default`：这个测试只关心 `tags` / `input_tag` 两个
    // 字段，而 `RegistryState` 有十几个 —— 结构体更新语法要把其余全部写一遍
    // `..Default::default()`，反而看不出"这个测试只设了这两样"。
    //
    // ⚠️ 属性放在这里（`fn` 上）而不是赋值语句上：后者是 nightly-only
    // （`E0658: attributes on expressions are experimental`，CI 抓到过）。
    #[allow(clippy::field_reassign_with_default)]
    fn a_typed_tag_that_the_list_lacks_is_still_a_choice() {
        // 实机 bug（v0.9.4）：用户打 `openjdk:8-jre-slim`，而这个 registry 的
        // tags/list 里**没有**它（实测 docker.1panel.live 对 library/openjdk
        // 只回 17282 个，没有 8-jre-slim 也没有 latest，但两者的 manifest 都是
        // 200）。那时下拉框必须**仍然把用户要的那个列出来** —— 否则组件找不到
        // 选中项就会自己选中第一项，把"将拉取"换成另一个 tag（**拉错镜像**）。
        //
        // `field_reassign_with_default`：`RegistryState` 有十几个字段，这里只
        // 关心其中两个；用结构体更新语法要把其余全部写一遍 `..Default::default()`，
        // 反而看不出"这个测试只设了 tags 和 input_tag"。
        //
        // ⚠️ `#[allow]` 挂在 **`fn`** 上（见函数签名上方），不能挂在赋值语句上
        // —— 属性放在**表达式**位置是 nightly-only（`E0658: attributes on
        // expressions are experimental`，CI 抓到过）。
        let mut registry = RegistryState::default();
        registry.tags = vec!["28-ea".to_owned(), "10-ea".to_owned()];
        registry.input_tag = Some("8-jre-slim".to_owned());

        let items = registry.tag_items();
        assert_eq!(
            items.first().map(String::as_str),
            Some("8-jre-slim"),
            "手打的 tag 要补进候选项、而且放最前面：{items:?}"
        );
        assert_eq!(items.len(), 3, "{items:?}");
        assert_eq!(
            items.iter().filter(|tag| *tag == "8-jre-slim").count(),
            1,
            "不能重复插：{items:?}"
        );

        // 列表里**已经有**它时，顺序和条数都不该变（免得列表被搅乱）
        registry.tags = vec!["8-jre-slim".to_owned(), "28-ea".to_owned()];
        assert_eq!(
            registry.tag_items(),
            vec!["8-jre-slim".to_owned(), "28-ea".to_owned()]
        );

        // 既没选、输入里也没 tag → 就是列表本身
        registry.input_tag = None;
        registry.selected = None;
        assert_eq!(
            registry.tag_items(),
            vec!["8-jre-slim".to_owned(), "28-ea".to_owned()]
        );
    }
}
