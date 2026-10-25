//! 日志 / exec / 进度三类**长任务的中间状态**。

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
