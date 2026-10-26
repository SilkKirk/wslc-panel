//! 导航与页面枚举：哪些页、怎么分组、哪些页要有输入框才能进。

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
    /// 合并的理由和那次取舍有关：当时留两个入口是
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
    /// 来分组（见 `app/` 的 `render`）。
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
    /// 有四页在这个列里，原因都是同一个：`InputState::new` 要 `&mut Window`，
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
    /// - 「数据库管理 / 部署管理 / SQL 控制台」服务某一个容器 → 高亮容器。
    ///
    /// 不加这个的话，进这些子页面时侧边栏**一个都不选中**，用户会失去"我在哪"。
    ///
    /// ⚠️ 返回值必须是**侧边栏里真的有一项**的页面。`Page::Database` 虽然
    /// 是 SQL 控制台的直接入口，但它自己不在 [`Page::ALL`] 里 —— 拿它当
    /// 返回值等于"高亮一个不存在的东西"，SQL 页就会一个都不亮。
    pub fn nav_parent(self) -> Option<Page> {
        match self {
            Page::AddInstance => Some(Page::Instances),
            Page::Database | Page::Deploy | Page::Sql => Some(Page::Containers),
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
