//! **顶层**状态：`AppState` 与它直接持有的加载入口。

use super::prelude::*;
use super::{
    BackupEntry, ConfirmAction, DbState, DeployState, DetailInspect, DistroAction, ExecState,
    ExportProgress, InstallProgress, LogsState, MirrorState, OnlineDistroState, Page,
    PendingAction, PullProgress, RegistryState, SettingsTab, Snapshot, Toast, UiSection,
    WslConfState, db_credentials_at_startup, deploy_store_at_startup, scan_backup_dir, unix_secs,
};

/// 只为了拿 `session.storagePath` 这一个值而读一次 `settings.yaml`。
///
/// 这里刻意**不复用** `Shell` 里那份 `SettingsDoc`：
/// 采集是在后台执行器上跑的，不该依赖 UI 侧的状态；而且 settings.yaml
/// 只有几百字节，读一次不到 1ms。
pub(crate) fn settings_storage_path(info: Option<&SystemInfo>) -> Option<String> {
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
        // 两份存档各读一次盘，然后把两半分别放进结构体。
        //
        // ⚠️ 两半**必须来自同一次读取**：`*_at_startup()` 返回的是 `(值, 错误)`，
        // 分开调两次的话，两次读之间文件若被另一个实例改过，就会出现
        // "值读成功、错误却非空"这种自相矛盾的状态（v0.20 的 `db_credentials`
        // 就是这么写的，一次启动读两遍盘）。
        let (deploy_store, deploy_store_error) = deploy_store_at_startup();
        let (db_credentials, db_credentials_error) = db_credentials_at_startup();
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
            db_credentials,
            db_credentials_error,
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
