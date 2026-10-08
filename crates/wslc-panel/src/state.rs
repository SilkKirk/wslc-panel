//! 应用状态：页面枚举、数据快照、待确认的危险操作。
//!
//! 这一层**完全不碰 GPUI**，只依赖 `wslc-core`，
//! 因此未来换渲染层（或加 CLI 模式）时这里可以原样复用。

use std::path::PathBuf;
use std::time::Duration;

use wslc_core::model::{
    ContainerSummary, ImageListItem, NetworkListItem, Session, SystemInfo, VolumeListItem,
};
use wslc_core::settings::SettingsDoc;
use wslc_core::storage::StorageInfo;
use wslc_core::{Result, Wslc, cmd};

/// 左侧导航的页面。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    /// 总览 / 基本信息。
    Dashboard,
    /// 当前运行 container。
    Running,
    /// 全部 container。
    Containers,
    /// 镜像。
    Images,
    /// 网络。
    Networks,
    /// 卷。
    Volumes,
    /// wlsc 配置。
    Config,
}

impl Page {
    /// 全部页面（决定导航顺序）。
    pub const ALL: [Page; 7] = [
        Page::Dashboard,
        Page::Running,
        Page::Containers,
        Page::Images,
        Page::Networks,
        Page::Volumes,
        Page::Config,
    ];

    /// 导航标签。
    pub fn label(self) -> &'static str {
        match self {
            Page::Dashboard => "基本信息",
            Page::Running => "当前运行",
            Page::Containers => "全部容器",
            Page::Images => "镜像",
            Page::Networks => "网络",
            Page::Volumes => "卷",
            Page::Config => "wlsc 配置",
        }
    }

    /// 导航分组（用于在侧边栏插入分隔标题）。
    pub fn group(self) -> &'static str {
        match self {
            Page::Dashboard => "概览",
            Page::Running | Page::Containers => "容器",
            Page::Images | Page::Networks | Page::Volumes => "资源",
            Page::Config => "设置",
        }
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
    }
}

/// 阻塞式采集一次完整快照。
///
/// 调用方必须在**后台线程/执行器**上调用（见 `app::Shell::refresh`）：
/// 内部会串行跑 6 条 `wslc` 命令。
pub fn load_snapshot(wslc: &Wslc) -> Snapshot {
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

    // 存储占用：`storagePath` 来自 settings.yaml，会话名来自 `wslc info`。
    // 这一步纯文件系统，不会失败到需要报错 —— 拿不到就是 None。
    let configured = settings_storage_path(snap.info.as_ref());
    // `Session` 的字段是 `id` / `creator_pid` / `display_name`（中文表头解析来的）
    let session = snap.sessions.first().map(|s| s.display_name.clone());
    snap.storage = wslc_core::storage::inspect(configured.as_deref(), session.as_deref());

    snap.elapsed_ms = started.elapsed().as_millis();
    snap
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

/// 应用的完整状态。
///
/// 这是 [`crate::app::Shell`] 里唯一的字段，所有页面都是它的只读视图。
#[derive(Debug)]
pub struct AppState {
    /// 子进程调用器。
    pub wslc: Wslc,
    /// 当前页面。
    pub page: Page,
    /// 最近一次采集到的数据。
    pub snapshot: Snapshot,
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
    /// 待用户确认的危险操作。
    pub confirm: Option<PendingAction>,
    /// 提示条。
    pub toast: Option<Toast>,
}

impl AppState {
    /// 新建初始状态。
    pub fn new(wslc: Wslc) -> Self {
        Self {
            wslc,
            page: Page::Dashboard,
            snapshot: Snapshot::default(),
            settings: None,
            settings_error: None,
            prefs: crate::prefs::Prefs::load(),
            busy: false,
            pulling: None,
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

    /// 记录一次危险操作的确认请求。
    pub fn request_confirm(&mut self, action: PendingAction) {
        self.confirm = Some(action);
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
        assert_eq!(Page::ALL.len(), 7);
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
        assert_eq!(deduped, vec!["概览", "容器", "资源", "设置"]);
    }

    #[test]
    fn snapshot_default_is_empty_and_has_no_data() {
        let snap = Snapshot::default();
        assert!(!snap.has_data());
        assert!(snap.errors.is_empty());
    }

    #[test]
    fn refresh_interval_comes_from_prefs() {
        let mut state = AppState::new(Wslc::new());
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
    fn prune_warns_about_being_irreversible() {
        assert!(PendingAction::PruneContainers.body().contains("不可撤销"));
        assert!(
            PendingAction::RemoveVolume("v".into())
                .body()
                .contains("不可撤销")
        );
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
        let state = AppState::new(Wslc::new());
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
        let mut state = AppState::new(Wslc::new());
        state.request_confirm(PendingAction::PruneContainers);
        assert_eq!(state.confirm, Some(PendingAction::PruneContainers));
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
        let mut state = AppState::new(Wslc::new());
        state.snapshot.info = Some(
            jsonl::parse_object::<SystemInfo>(include_str!(
                "../../wslc-core/tests/fixtures/info.json"
            ))
            .unwrap(),
        );
        assert_eq!(state.session_label(), "#1 wslc-cli-76434");
    }
}
