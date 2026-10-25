//! 部署域：记录、阶段、进度、站点配置。

use super::prelude::*;
use super::truncate_chars;
use crate::util::truncate_chars;

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
pub(crate) fn deploy_store_at_startup() -> (crate::deploys::DeployStore, Option<String>) {
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
