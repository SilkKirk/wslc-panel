//! 「部署应用」的**纯逻辑**：nginx / Java / Node.js / Go 容器的识别、shell 脚本
//! 生成、`tar` 参数拼装、输出解析、错误分类。
//!
//! 这个模块里**一行进程都不起**（所以能脱离 Windows 单测）—— 真去跑命令的那部分
//! 在 [`crate::cmd::deploy`]。拆法和 `model/mysql.rs` ↔ `cmd/mysql.rs` 一样。
//!
//! # 文件怎么进容器：`wslc container cp`
//!
//! ⚠️ **`cp` 不在 `wslc --help` 的顶层命令表里**（那张表列了 `exec` / `run` /
//! `attach` / `export` / `import` … 但**没有 `cp`**），它只作为
//! `wslc container cp` 存在 —— `wslc cp` 会直接报「无法识别的命令」。
//! 我第一版方案就是照着顶层表下的结论（"`wslc` 没有 `cp`，只能往 stdin 里灌
//! tar"），直到有人拿 `wslc container cp --help` 的截图来问。
//! **结论：查 CLI 的能力要看子命令那一层，别只看顶层表。**
//!
//! 实测（`wslc 3.0.3.0` + 真实的 nginx 容器）：
//!
//! | 试的 | 结果 |
//! |---|---|
//! | `wslc container cp <文件> <容器>:/tmp/`（`/tmp` **已存在**） | exit 0，落在 `/tmp/<文件名>` |
//! | `wslc container cp <文件> <容器>:/tmp/不存在` | exit **1**，`ERROR_PATH_NOT_FOUND` |
//! | `wslc container cp <目录> <容器>:/tmp/已存在目录` | exit 0，**递归**复制（含隐藏文件），并且**多套一层目录名** |
//! | `-a` / `--archive` | 和不用它**结果一样**（帮助里写着"为 Docker CLI 兼容性而接受"） |
//! | 8 MB 二进制 | exit 0，sha256 与本地**完全一致** |
//! | 复制过去的文件权限 | **`666`** —— 没有可执行位（目录倒是 `777`） |
//! | `wslc container cp <文件> <**没运行**的容器>:/tmp/` | **exit 0、什么都不做、连一个字都不输出** ⚠️ |
//!
//! 最后一条是这一版最阴的一条：**`cp` 在容器没运行时静默成功**。
//! 所以流程里必须 ① **先**用 `exec` 准备目标目录（那一条会老老实实报
//! `WSLC_E_CONTAINER_NOT_RUNNING`），② 复制完**数一下文件**核对（见
//! `crate::cmd::deploy` 的 `verify_copy`）。光看 `cp` 的退出码会得到
//! "部署成功、容器里什么都没有"。
//!
//! 也因为是"多套一层目录名"，**目录来源要逐个顶层条目去 `cp`**
//! （见 [`copy_members`]）—— 这样落到目标里的才是目录的**内容**，
//! 而不是 `<目标>/<本地目录名>/…`。
//!
//! # 起进程：不能靠 `wslc exec -d`
//!
//! 实测 `wslc exec -d nginx sh -c "sleep 5"` **耗时 5.08 秒** —— `-d` 在 `exec`
//! 上不生效，它照样等命令结束。所以"启动应用"用的是**在容器里把它甩到后台**：
//! `nohup <命令> > <日志> 2>&1 & echo $! > <pidfile>`，实测 **0.17 秒**返回、
//! 进程在 exec 结束之后**还活着**。整套判活 / 停止 / 看日志都围着这个 pidfile 转。
//!
//! ⚠️ 那对大括号（`{ … }`）是**必须的**：`&` 的优先级最低，写成
//! `mkdir -p D && cd D && nohup C & echo $! > D/pid` 会把 `echo $! > pid` 甩到
//! 后台组**外面**去 —— 它和 `mkdir` 之间于是有了竞态。实测踩到过：
//! `cannot create …: Directory nonexistent`，而命令"看起来成功了"。

use std::path::{Path, PathBuf};

use super::{image_last_segment, ContainerInspect, ContainerListItem};

// ---------------------------------------------------------------------------
// 运行时与形态
// ---------------------------------------------------------------------------

/// 「部署应用」认得的运行时。
///
/// 表刻意小，认不出来就**什么都不做**（和 `model::image_preset` 同一条原则：
/// 宁可少一个按钮，也不要给一个点进去全是报错的按钮）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppRuntime {
    /// 官方 `nginx` 一族。
    Nginx,
    /// JVM 一族（openjdk / temurin / corretto / tomcat / maven …）。
    Java,
    /// Node.js 一族。
    Node,
    /// Go 一族。
    Go,
}

/// 部署形态。**这一版最重要的一个区分**。
///
/// 见 [`AppRuntime::kind`] 的说明：nginx 不能按 Java 那样"再起一个"。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeployKind {
    /// **内容**：部署的产物是**文件**（静态站点），运行靠容器里本来就在跑的那个
    /// 服务进程。动作是"重新上传" + "重载"。
    Content,
    /// **进程**：部署的产物是文件，运行是面板**另起**一个受 pidfile 管的进程。
    Process,
}

impl DeployKind {
    /// 中文显示名（界面上标出来）。
    pub fn label(self) -> &'static str {
        match self {
            Self::Content => "内容",
            Self::Process => "进程",
        }
    }
}

/// 来源是目录还是文件（决定「浏览…」弹哪个对话框）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// 一个目录（打它的**内容**，不是这个目录本身）。
    Directory,
    /// 单个文件。
    File,
}

impl AppRuntime {
    /// 全部运行时（决定下拉框 / 表格里的顺序）。
    pub const ALL: [AppRuntime; 4] = [Self::Nginx, Self::Java, Self::Node, Self::Go];

    /// 中文显示名 —— 也叫**按钮文案**（「部署管理」那一行不写运行时名，
    /// 页面里才写）。
    pub fn label(self) -> &'static str {
        match self {
            Self::Nginx => "Nginx",
            Self::Java => "Java",
            Self::Node => "Node.js",
            Self::Go => "Go",
        }
    }

    /// 部署形态。
    ///
    /// # 为什么 nginx 是 `Content` 而不是 `Process`
    ///
    /// 容器里那个 nginx 主进程**已经占着 80 端口**了。我们再 `nohup nginx`
    /// 起一个只会端口冲突（退一步说，就算起来了也监听不到同一个端口）。
    /// 而 nginx **每个请求都从磁盘读**，所以"部署"这个动作的产物就是**文件**，
    /// 铺进 Web 根目录就生效 —— 根本不需要起进程。
    ///
    /// 其余三个都是"一个应用 = 一个进程"，归 `Process`。
    pub fn kind(self) -> DeployKind {
        match self {
            Self::Nginx => DeployKind::Content,
            Self::Java | Self::Node | Self::Go => DeployKind::Process,
        }
    }

    /// 默认端口（信息性 —— 已存在的容器改不了端口发布，见 `cmd::deploy` 的说明）。
    pub fn default_port(self) -> u16 {
        match self {
            Self::Nginx => 80,
            Self::Java | Self::Go => 8080,
            Self::Node => 3000,
        }
    }

    /// 来源是目录还是文件。
    pub fn source_kind(self) -> SourceKind {
        match self {
            Self::Nginx | Self::Node => SourceKind::Directory,
            Self::Java | Self::Go => SourceKind::File,
        }
    }

    /// 「浏览文件」时的后缀过滤（**不含点**，见 `cmd::picker::pick_file`）。
    ///
    /// Go 给空表 = 不过滤：编译出来的二进制**没有固定后缀**
    /// （`go build` 的默认名字是目录名）。按后缀过滤等于把 Go 全挡在外面。
    pub fn source_extensions(self) -> &'static [&'static str] {
        match self {
            Self::Java => &["jar", "war"],
            _ => &[],
        }
    }

    /// 「浏览…」按钮的类别名（`pick_file` 的 `label`）。
    pub fn source_label(self) -> &'static str {
        match self {
            Self::Java => "JAR 包",
            Self::Go => "可执行文件",
            _ => "文件",
        }
    }

    /// 默认目标目录。
    ///
    /// ⚠️ nginx 的返回值只是个**兜底**：真正的 Web 根要从 `inspect` 的挂载点推
    /// （[`nginx_web_root`]），推不出来才用这个（官方镜像的默认值）。
    pub fn default_target_dir(self, app: &str) -> String {
        match self {
            Self::Nginx => "/usr/share/nginx/html".to_owned(),
            _ => format!("/opt/{app}"),
        }
    }

    /// 默认启动命令。`artifact` 是**源文件的名字**（`myapp-1.2.0.jar` 这种），
    /// 还不知道就给 `None`。
    ///
    /// ⚠️ Java / Go 要的是**源文件的名字**而不是应用名：用户不改这一格就得到
    /// `Error: Unable to access jarfile`。所以选了来源之后要**重算**这一格。
    ///
    /// ⚠️ Node 的 `npm start` 是**推的**：它要求源目录里有 `package.json`
    /// 而且 `scripts.start` 存在。这里**刻意不去猜**"该 `npm start` 还是
    /// `node server.js`" —— 猜错的表现是"部署成功了但起不来"，比明确报错更难查。
    pub fn default_start_command(self, app: &str, artifact: Option<&str>) -> Option<String> {
        match self {
            // 内容形态没有"启动"这回事（见 kind 的说明）
            Self::Nginx => None,
            Self::Java => {
                let jar = artifact
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("{app}.jar"));
                Some(format!("java -jar {jar}"))
            }
            Self::Node => Some("npm start".to_owned()),
            Self::Go => {
                let bin = artifact.map(str::to_owned).unwrap_or_else(|| app.to_owned());
                Some(format!("./{bin}"))
            }
        }
    }

    /// 解包之后要不要 `chmod +x`（只有"要被执行的文件"才要）。
    ///
    /// Java 的 jar 不需要（`java -jar` 读它，不执行它），Node 的源码不需要，
    /// nginx 更没有这回事。
    pub fn needs_exec_bit(self) -> bool {
        matches!(self, Self::Go)
    }

    /// 这一族在界面上的补充说明。
    pub fn note(self) -> &'static str {
        match self {
            Self::Nginx => {
                "部署的是**静态文件**：铺进 Web 根目录就生效，不需要重启 nginx。\
                 nginx 每个请求都从磁盘读。"
            }
            Self::Java => "部署一个 JAR 包，用 `java -jar` 起进程。",
            Self::Node => {
                "部署一个目录。启动命令默认是 `npm start` —— 它要求目录里有 \
                 package.json 且配了 scripts.start；不是这样的话请自己改这一格。"
            }
            Self::Go => {
                "部署一个编译好的二进制。⚠️ 从 Windows 传过去的文件**没有可执行位**，\
                 面板会自动补一句 `chmod +x`。"
            }
        }
    }

    /// **稳定键**：写进本地存档（`deploys.json`）的那个值。
    ///
    /// ⚠️ 刻意**不用** [`Self::label`] 当键：label 是 `"Node.js"`（有大小写、有点），
    /// 也不是 Rust 的变体名（`Go`）—— 存档要让用户能看懂、能手改，
    /// 而且改了枚举的写法不该把老档读废。
    pub fn key(self) -> &'static str {
        match self {
            Self::Nginx => "nginx",
            Self::Java => "java",
            Self::Node => "node",
            Self::Go => "go",
        }
    }

    /// 从稳定键还原（读存档时用）。认不出来返回 `None` ——
    /// 调用方把那条记录跳过，**不要**猜一个默认值（猜错会把文件铺到别的地方）。
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.key() == key.trim())
    }
}

/// 「添加部署」弹窗里**应用名**那一格的默认值（见 [`AppRuntime::key`] 的反例）。
///
/// ⚠️ 刻意**不**用 [`AppRuntime::key`]（v0.9.13 改）。拿运行时键当初值的话，
/// Java 容器预填出来是 `java` → 默认目标目录正好是 **`/opt/java`** —— 即
/// `docs/PLAN-v0.9.md` §15 那次"部署把镜像自带的 JDK 删掉"的现场路径
///（`eclipse-temurin` 的 JDK 就在 `/opt/java/openjdk`）。逐名删除之后面板
/// 已经不会删它了，但把用户往那个路径上引没有任何好处 —— 而 §15.7 给用户的
/// 自救建议恰恰是"目标目录改成 `/opt/app`"。
///
/// 应用名只是**存档键 + 默认目录名**，用户随时能改；一个中性的默认值
/// 比一个"看起来像是猜出来的"名字更容易被改掉。
pub const DEFAULT_APP_NAME: &str = "app";

// ---------------------------------------------------------------------------
// 识别
// ---------------------------------------------------------------------------

/// 「凭什么认为它是这一类容器」。
///
/// 两种依据的可靠程度不一样，所以不合并成一个 `bool`：界面上可以把依据说出来，
/// 用户就知道该不该信。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeploySignal {
    /// 镜像名认出来的（最可靠）。
    Image,
    /// 启动命令里有 `nginx` / `java` / `node` …
    ///
    /// ⚠️ 实测 `wslc list` 的 `Command` **被截断成 20 字符**
    /// （官方 nginx 镜像那行是 `"/docker-entrypoint.…"`），所以官方镜像在这条上
    /// 基本命中不了。留着它是因为"用户显式把命令写成 `nginx`"时它**最准**。
    Command,
}

impl DeploySignal {
    /// 给用户看的一句话。
    pub fn label(self) -> &'static str {
        match self {
            Self::Image => "按镜像名判断",
            Self::Command => "按启动命令判断",
        }
    }
}

/// 一次判定结果：是哪一族 + 依据是什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeployCandidate {
    /// 运行时。
    pub runtime: AppRuntime,
    /// 判定依据。
    pub signal: DeploySignal,
}

/// 从**列表数据**判断这个容器能部署什么（零成本，渲染期可用）。
///
/// ⚠️ **没有端口依据**。`model::mysql` 有一条"发布了 3306 就当它是 MySQL"的
/// 猜测，能那么做是因为 3306 是 IANA 专门分给 MySQL 的。**这里不成立**：
/// 80 和 8080 谁都在用，拿端口猜只会把按钮画到一堆不相干的容器上。
/// 这是**被否掉的方案**，不是漏了。
pub fn container_app_kind(item: &ContainerListItem) -> Option<DeployCandidate> {
    if let Some(runtime) = runtime_from_image(&item.image) {
        return Some(DeployCandidate {
            runtime,
            signal: DeploySignal::Image,
        });
    }
    if let Some(runtime) = runtime_from_command(&item.command_clean()) {
        return Some(DeployCandidate {
            runtime,
            signal: DeploySignal::Command,
        });
    }
    None
}

/// 按镜像名认运行时。认不出来返回 `None`（**什么都不做**）。
///
/// 带 registry 前缀的走 [`image_last_segment`] 取最后一段，所以
/// `docker.1panel.live/library/nginx:latest` → `nginx` **自动命中**
/// （这正是本机那个 nginx 容器的实测值）。
pub fn runtime_from_image(image: &str) -> Option<AppRuntime> {
    match image_last_segment(image).as_str() {
        // nginx
        "nginx" | "openresty" | "tengine" => Some(AppRuntime::Nginx),
        // JVM 一族：官方 jdk 镜像 + 常见的应用服务器 / 构建工具镜像
        "openjdk" | "temurin" | "eclipse-temurin" | "amazoncorretto" | "corretto" | "zulu"
        | "liberica" | "maven" | "gradle" | "tomcat" | "jetty" | "wildfly" => Some(AppRuntime::Java),
        // Node.js
        "node" | "nodejs" | "iojs" => Some(AppRuntime::Node),
        // Go
        "golang" | "go" => Some(AppRuntime::Go),
        _ => None,
    }
}

/// 按启动命令认运行时。
///
/// 顺序有讲究：`java -jar node-something` 这种先看 Java（`-jar` 是强信号）。
pub fn runtime_from_command(command: &str) -> Option<AppRuntime> {
    let lower = command.to_ascii_lowercase();
    // nginx 的启动命令是 `nginx -g "daemon off;"`；`openresty` 同理
    if lower.contains("nginx") || lower.contains("openresty") {
        return Some(AppRuntime::Nginx);
    }
    // `java -jar x.jar` / `catalina.sh run` / `mvn spring-boot:run`
    if lower.contains("java")
        || lower.contains("-jar")
        || lower.contains("catalina")
        || lower.contains("jetty")
    {
        return Some(AppRuntime::Java);
    }
    // `node server.js` / `npm start` / `npx …` / `yarn …` / `pnpm …`
    if lower.contains("node")
        || lower.contains("npm")
        || lower.contains("npx")
        || lower.contains("yarn")
        || lower.contains("pnpm")
    {
        return Some(AppRuntime::Node);
    }
    // Go **刻意不猜**：编译出来的二进制名字是任意的（`go build` 默认用目录名），
    // 启动命令里既没有 "go" 也没有 "golang"。`go run main.go` 是开发用法，
    // 部署场景基本不用。所以 Go 只认镜像名。
    None
}

/// 从 `inspect` 的结果判断（**权威判定**，进页面时跑一次）。
///
/// 顺序：镜像引用 → 环境变量 → 命令 / entrypoint。
/// 都比列表那条启发式强：`inspect` 的字段是**完整**的（没有被截断的 `Command`）。
///
/// ⚠️ 环境变量那几条里，只有 `NGINX_VERSION` 是**实测**的（本机那个 nginx 容器
/// 的 `Config.Env` 里就有它）。`NODE_VERSION` / `GOLANG_VERSION` / `JAVA_HOME`
/// 是**官方镜像的惯例**，属于**推的** —— 所以它们只作为"镜像名认不出来时"
/// 的补充，排在镜像名之后。
pub fn runtime_from_inspect(inspect: &ContainerInspect) -> Option<AppRuntime> {
    if let Some(runtime) = inspect.image_ref().and_then(runtime_from_image) {
        return Some(runtime);
    }

    // 环境变量（官方镜像都会把自己那套版本号放进 Env）
    let env = inspect.env();
    let has_prefix = |prefix: &str| env.iter().any(|line| line.starts_with(prefix));
    if has_prefix("NGINX_VERSION") {
        return Some(AppRuntime::Nginx);
    }
    if has_prefix("NODE_VERSION") {
        return Some(AppRuntime::Node);
    }
    if has_prefix("GOLANG_VERSION") {
        return Some(AppRuntime::Go);
    }
    if has_prefix("JAVA_HOME") || has_prefix("JAVA_VERSION") {
        return Some(AppRuntime::Java);
    }

    // 命令 / entrypoint：这里**没有截断**，所以 `nginx` / `java` 是可靠的
    let mut line = inspect.cmd().join(" ");
    line.push(' ');
    line.push_str(&inspect.entrypoint().join(" "));
    runtime_from_command(&line)
}

// ---------------------------------------------------------------------------
// nginx 的 Web 根
// ---------------------------------------------------------------------------

/// 从 `inspect` 的挂载点推 nginx 的 Web 根。
///
/// 本机那个容器的实测值是 `{Type: volume, Name: webdata,
/// Destination: /usr/share/nginx/html}` —— 也就是说**站点内容落在一个卷里**，
/// 容器重建也不会丢。这正是要把 Web 根推出来的理由：直接写死
/// `/usr/share/nginx/html` 会在"用户自己挂了别的目录"时把文件铺到**没人读**的地方。
///
/// 规则：`Destination` 在 `/usr/share/nginx` 底下、且**不是** `/usr/share/nginx`
/// 本身（那挂的是"站点目录的父目录"，往里铺文件没人读）时取它。
///
/// ⚠️ 这条**是推的**：本机只有一个 nginx 容器，我验的是"这条路能读到那个挂载点"，
/// 没验过"各种 nginx 变体的挂载点都长这样"。所以界面上的目标目录**可编辑**，
/// 而且会写明这个默认值是从哪儿来的。
///
/// # ⚠️ `/etc/nginx` 底下的挂载点**不算**站点根（v0.9.13 改）
///
/// 早先这里还有第二段："再退一步找 `/etc/nginx` 底下的（有的镜像把 `conf.d`
/// 单独挂出来）"。**那是错的**，而且错得会伤到用户：
///
/// - `/etc/nginx/conf.d` 挂的是**配置**，不是站点 —— `-v ./conf.d:/etc/nginx/conf.d`
///   是极常见的挂法，而那种容器的站点内容往往**烤在镜像里、根本没有挂载**。
///   于是这个函数会返回配置目录，「添加部署」把它预填进「容器内目标目录」，
///   用户不改就点下去：文件铺进去**没有任何效果**（nginx 不会把 `.html`
///   当配置读），而面板会说"部署成功"；
/// - 更糟的是源目录里如果有个 `default.conf`，[`removals_script`] 会先删掉
///   容器里那个**同名配置文件**再覆盖它 —— 那是在改用户的 nginx 配置。
///
/// 所以现在只认 `/usr/share/nginx`（官方镜像的站点目录就在那儿）。拿不到就
/// 返回 `None`，由调用方回落到 [`AppRuntime::default_target_dir`]
/// （`/usr/share/nginx/html`，官方镜像的默认值）—— 那个值至少在"站点没挂卷"
/// 时是对的，而且它在安全闸的白名单里。
pub fn nginx_web_root(inspect: &ContainerInspect) -> Option<String> {
    for mount in inspect.mounts() {
        let dest = mount.destination.trim_end_matches('/');
        if dest.starts_with("/usr/share/nginx") && dest != "/usr/share/nginx" {
            return Some(mount.destination.clone());
        }
    }
    None
}

// ---------------------------------------------------------------------------
// shell 安全
// ---------------------------------------------------------------------------

/// 把一个值变成**安全的单引号 shell 字面量**。
///
/// 目标目录、应用名、pidfile 路径都是**用户输入**，而它们要拼进 `sh -c` 的
/// 脚本文本里。这不是"参数被当成选项"那个问题，是**真的 shell 注入**：
/// 一个叫 `a; rm -rf /` 的目标目录会真的去执行那半截。
///
/// 做法是 POSIX 的经典写法：整体用单引号包起来，内部的 `'` 写成 `'\''`
/// （结束当前单引号串 → 一个转义的 `'` → 重新开始单引号串）。
/// 单引号内部**除 `'` 外没有任何字符有特殊含义**，所以 `$`、反引号、`\`、
/// 换行都不需要额外处理。
pub fn shell_quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for ch in value.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// 应用名白名单：`[A-Za-z0-9_-]`，长度 1..=64。
///
/// 和 v0.7 的库名白名单同一条原则：**不合法就拒绝**，把选择权交回用户，
/// 而不是我们猜他的意图去转义。它是目标目录名、也是本地存档的键。
///
/// 刻意**不允许点**：`..` 会变成上一级目录，那是路径穿越。
pub fn valid_app_name(name: &str) -> bool {
    let len = name.chars().count();
    (1..=64).contains(&len)
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// 允许作为目标目录的**前缀白名单**。
///
/// 见 [`safe_target`]：因为"部署"和"删除"都会往目标目录里**放 / 删文件**，
/// 而目标目录是用户填的。
const SAFE_TARGET_PREFIXES: &[&str] = &[
    "/opt/",
    "/srv/",
    "/app/",
    "/data/",
    "/www/",
    "/workspace/",
    "/home/",
    "/tmp/",
    "/var/www/",
    // nginx 官方镜像的站点目录（本机那个容器的实测值就是它）
    "/usr/share/nginx/",
    "/usr/local/",
];

/// 目标目录的**安全闸**：不合规的路径直接拒绝。
///
/// # 为什么必须有这个东西
///
/// [`prepare_target_script`] 与 [`remove_script`] 都会在目标目录里**删东西**
/// （现在删的是我们自己铺的那些名字，见 [`removals_script`]），而目标目录是
/// **用户填的**。填成 `/usr` 就等于往系统目录里塞文件、还可能在删的时候
/// 撞上系统文件 —— 容器照样会被搞坏。
///
/// # 为什么是白名单而不是黑名单
///
/// 黑名单要枚举所有危险路径（`/usr`、`/usr/share`、`/etc`、`/var`、
/// `/var/lib`……），漏一个就是灾难。
/// 白名单反过来：**只认识这几种**，其余一律拒绝，并把允许的写进提示里。
///
/// 这条刻意保守 —— 想加一个前缀就是 [`SAFE_TARGET_PREFIXES`] 里加一行。
/// 用户真要部署到别处，用「终端」自己拷。
///
/// 另外两条：必须是**绝对路径**（相对路径的含义取决于容器的 WORKDIR，
/// 太含糊），且不许出现 `..`（路径穿越）。
pub fn safe_target(target: &str) -> bool {
    let target = target.trim();
    if !target.starts_with('/') || target.contains("..") || target.contains('\0') {
        return false;
    }
    // 根目录本身、以及任何白名单前缀之外的东西都不行
    SAFE_TARGET_PREFIXES
        .iter()
        .any(|prefix| target.starts_with(prefix) && target.len() > prefix.len())
}

/// [`safe_target`] 的**加一条**版本：除了白名单，还放行**这个容器自己的站点目录**。
///
/// # 为什么需要它
///
/// nginx 形态的目标目录默认值是 `Shell::open_deploy_dialog` **从 `inspect`
/// 的挂载点推出来的**（那个容器的站点落在一个卷上，不能写死
/// `/usr/share/nginx/html` —— 用户自己挂了别的目录时，写死会把文件铺到
/// **没人读**的地方）。而用户完全可以把站点挂在白名单之外（`-v webdata:/site`）。
///
/// 那种情况下**面板自己预填的路径会被自己的安全闸拒掉** —— 用户看到的是
/// "只允许填在 … 底下"，而正确的那个就摆在输入框里、是我们填的。这是个死胡同。
///
/// 所以这里多放行 `web_root` 本身、以及它**底下**的路径。
///
/// ⚠️ 这**不是**把闸门放宽成"任意路径"：`web_root` 来自**容器自己的配置**
/// （`inspect` 的 `Mounts[].Destination`），是那个运行时真正对外提供内容的地方
/// —— 往它里面铺站点文件正是"部署"的语义。而白名单仍然是默认那一层：拿不到 `web_root`
/// 时（`None`）、或者它自己就是个怪路径时，一行都不多放。
///
/// ⚠️ 判"在它底下"要看**路径边界**：`/sitemap` 不在 `/site` 底下。
pub fn safe_target_with_site(target: &str, web_root: Option<&str>) -> bool {
    if safe_target(target) {
        return true;
    }
    let Some(root) = web_root.map(str::trim).filter(|root| !root.is_empty()) else {
        return false;
    };
    let root = root.trim_end_matches('/');
    // 站点目录自己也必须先像个正常路径。`root.len() < 2` 挡的是 `/` ——
    // 站点目录是根目录时，借它放行等于把闸门整个拆掉。
    if root.len() < 2 || !root.starts_with('/') || root.contains("..") || root.contains('\0') {
        return false;
    }
    let target = target.trim();
    if !target.starts_with('/') || target.contains("..") || target.contains('\0') {
        return false;
    }
    target == root || target.starts_with(&format!("{root}/"))
}

/// 目标目录不合规时给用户看的那一句（把允许的前缀列出来）。
pub fn safe_target_hint() -> String {
    format!(
        "目标目录只允许填在 {} 底下 —— 面板要往这个目录里**放文件**，\
         所以不能是系统目录。要放到别处请用「终端」自己拷。",
        SAFE_TARGET_PREFIXES.join("、")
    )
}

/// 取一个本地路径的**文件名**（最后一段），并且保证它不含路径分隔符。
///
/// 用在两处：给 Java / Go 拼**默认启动命令**（`java -jar app.jar` 要的是
/// 源文件的名字，不是应用名），以及存进本地存档的 `artifact` 字段。
///
/// ⚠️ 它**不**再参与拼 tar 的成员名了（v0.9 的搬运改成 `wslc container cp`，
/// 目录来源用的是 [`copy_members`]，成员名一律取本地路径本身）。
///
/// 返回 `None` 表示这个路径**没有可用的文件名**（以分隔符结尾、是根、或者是空的）。
pub fn artifact_name(source: &Path) -> Option<String> {
    let name = source.file_name()?.to_string_lossy().to_string();
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains('\0') {
        return None;
    }
    Some(name)
}

/// 在容器内的路径上拼一段（只处理 `/`，不碰 Windows 的分隔符）。
///
/// `dir` 是 `/` 或空时拼成 `/name`；否则去掉末尾多余的 `/` 再拼 ——
/// 免得出来 `<dir>//app.log` 这种能跑但难看的路径（而且它会在提示条里显示给用户）。
pub fn container_join(dir: &str, name: &str) -> String {
    let trimmed = dir.trim_end_matches('/');
    if trimmed.is_empty() {
        format!("/{name}")
    } else {
        format!("{trimmed}/{name}")
    }
}

// ---------------------------------------------------------------------------
// 脚本
// ---------------------------------------------------------------------------

/// 进程形态的日志文件名（放在目标目录里）。
pub const LOG_FILE: &str = "app.log";
/// 进程形态的 pidfile 名（放在目标目录里）。
pub const PID_FILE: &str = "app.pid";

/// 判活脚本输出的两个哨兵值。
///
/// 用**定值**而不是去猜容器里的语言环境：`wslc` 的输出可能是中文，
/// 镜像里的 `sh` 可能是 dash / busybox，唯一稳定的东西是我们自己 echo 出来的。
pub const STATUS_RUNNING: &str = "__WSLC_RUNNING__";
/// 见 [`STATUS_RUNNING`]。
pub const STATUS_STOPPED: &str = "__WSLC_STOPPED__";
/// 「目标目录不存在」的哨兵（数文件那条脚本用）。
pub const NO_DIR: &str = "__WSLC_NO_DIR__";
/// 「还没有日志文件」的哨兵。
pub const NO_LOG: &str = "__WSLC_NO_LOG__";
/// 「目标目录里已经有东西，而它不是我们铺的」的哨兵。
///
/// 见 [`prepare_target_script`]：这一条是 v0.9.8 加的，代价是一次**真实的灾难**
/// （部署到 `/opt/java` 把镜像自带的 JDK 删了）。
pub const TARGET_NOT_EMPTY: &str = "__WSLC_TARGET_NOT_EMPTY__";

/// 目标目录非空、且不是我们铺的时的**提醒**（给用户看的那一句）。
///
/// ⚠️ 这是一句**提醒**，不是错误：面板**照常部署**（只替换自己铺的那些名字，
/// 见 [`removals_script`]），只是告诉用户"这个目录里本来有别的东西，我没动它们"。
///
/// # 为什么从"拦"改成"提醒"（v0.9.11 改）
///
/// v0.9.9 是**拒绝部署**的（`exit 0` + 哨兵）。当时它还是数据安全的最后一道防线
/// —— 再往前一版是整目录 `find -delete` 清空。而 v0.9.10 改成逐名删除之后，
/// **它拦的已经不是危险了**：`/opt/java` 里那个 `openjdk/` 根本不在删除列表里。
///
/// 代价实测：用户删掉旧记录、重建容器之后重新部署，目标目录被预填成 `/opt/java`
/// —— 面板直接拒绝，而那次部署**本来是安全的**，`java` 也不会丢。
/// 挡住的只是用户自己。所以现在：**干完再说一句**。
///
/// ⚠️ **第一行要能独立成立**：提示条（toast）只取第一行并截断到 120 字符。
pub fn target_not_empty_hint(target: &str) -> String {
    format!(
        "目标目录 {target} 里本来就有别的东西（不是面板铺的）—— 面板只替换自己那几个文件，\
         没动它们。\n\
         那里面可能是**镜像自己要用的文件**：实测 `eclipse-temurin` 的 JDK 就在 \
         `/opt/java/openjdk`（面板不会删它）。想把目录清干净，用「终端」自己来；\
         或者换个目标目录（比如 `/opt/<你的应用名>`、`/app/...`）。"
    )
}

/// 拼"**只删我们自己铺的那些名字**"的片段（不清空整个目录）。
///
/// # ⚠️ 为什么不再 `find <目标> -mindepth 1 -delete`（v0.9.10 改）
///
/// 整目录清空造成过一次**真实的数据损坏**：应用名就叫 `java`，默认目标目录
/// `/opt/java` 正好是 `eclipse-temurin:8-jre-alpine` 放 JDK 的地方
/// （`/opt/java/openjdk`）—— 一个 `find -delete` 就把容器里的 `java` 删了，
/// 应用从此起不来。白名单（[`safe_target`]）挡不住这种事：它只知道
/// "`/opt` 底下允许"，**不知道镜像在 `/opt` 底下放了什么**。
///
/// v0.9.9 加过一层"不是我们铺的就拒绝"，但那层依赖**本地存档里有没有记录**
/// —— 而记录是**按容器名**索引的，容器删掉重建之后记录还在，
/// 于是"重新部署"又会把新容器里的 JDK 删一遍（同一个坑的第二只脚）。
///
/// 所以现在改成逐个删**我们自己写进去的名字**：
///
/// - `members`：这次要铺进去的成员名（`cmd::deploy::member_names` 算得出来），
///   外加调用方从存档里带上的**上一次**铺过的名字（`DeployRecord::members`），
///   后者把"上一次有、这一次没有"的旧文件也收拾掉；
/// - 外加进程形态的 `app.log` / `app.pid` —— 那两个也是我们写的；
/// - **其余一律不碰**：镜像自带的文件、用户自己放的东西都留着。
///
/// 代价要说清（这是刻意的取舍）：结果目录**不再等于源目录**，
/// 目录里不是我们放的东⻄会留下。比起"悄悄删掉一个 JDK"，
/// 多留几个文件是划算的。
///
/// 名字里出现 `/`、`\`、`..` 或空串时**跳过**：成员名来自本地文件名，
/// 不该带路径；真带了说明上游错了，宁可少删也不能删到目标目录外面去。
pub fn removals_script(target: &str, members: &[String]) -> String {
    let mut names: Vec<String> = Vec::new();
    for member in members {
        let member = member.trim();
        if !is_safe_member(member) {
            continue;
        }
        names.push(shell_quote(&container_join(target, member)));
    }
    // 日志与 pidfile 无条件带上（它们的名字是面板定的，不是一个"成员"）
    for fixed in [LOG_FILE, PID_FILE] {
        names.push(shell_quote(&container_join(target, fixed)));
    }
    format!("rm -rf {}", names.join(" "))
}

/// 成员名能不能安全地拼成 `<目标>/<名字>`。
fn is_safe_member(name: &str) -> bool {
    !name.is_empty() && !name.contains('/') && !name.contains('\\') && !name.contains("..")
}

/// 准备目标目录：建出来，然后删掉**我们自己铺的那些名字**（[`removals_script`]）。
///
/// # ⚠️ 为什么不能写 `rm -rf <目标>`
///
/// nginx 那种容器的 Web 根是一个**卷挂载点**。本机实测
/// （`wslc inspect nginx`）：`/usr/share/nginx/html` 挂在 `/dev/sdc` 上
/// （`/proc/mounts` 里查得到，设备号 2080，和根目录的 35 不是一个）。
/// 而对挂载点 `rm -rf` 会**返回 1**：
///
/// ```text
/// rm: cannot remove '/usr/share/nginx/html': Device or resource busy
/// ```
///
/// 它**确实清空了内容**，但退出码是 1 —— 于是 `&&` 链在那里就断了，
/// 后面的步骤根本不会跑。表现是"点了部署，界面上说失败了，什么都没有"。
///
/// 所以只删**里面的名字**、不碰目录本身（[`removals_script`]）。
///
/// # `ours`：只决定"要不要提醒一句"，**不决定拦不拦**（v0.9.11 改）
///
/// | `ours` | 行为 |
/// |---|---|
/// | `true`（本地存档里有这个目录的记录） | 直接按 [`removals_script`] 收拾 + 铺，什么都不说 |
/// | `false`（第一次往这个目录铺） | 目录非空就先 echo [`TARGET_NOT_EMPTY`]（**提醒**），然后**照常铺** |
///
/// ⚠️ v0.9.9 那种"非空就拒绝"已经去掉了：逐名删除之后它拦的**不是危险**，
/// 而是用户自己的正常部署（实测：删掉旧记录 + 重建容器之后重新部署
/// `/opt/java` 被硬拦，而那次部署本来完全安全）。理由写在
/// [`target_not_empty_hint`] 那段。**哨兵后面没有 `exit`** —— 它只是给调用方
/// 一个"要不要说一句"的信号，脚本一定往下走。
///
/// ⚠️ 这一步必须在 `cp` **之前**跑，两条理由：
///
/// 1. 它（通过 `exec`）会**报出"容器没在运行"** —— 而 `cp` 在那种情况下
///    **静默成功**（实测 exit 0、不输出、什么都不做）；
/// 2. `cp` **要求目标目录已经存在**（实测不存在的路径报
///    `ERROR_PATH_NOT_FOUND`），它不会替我们建。
pub fn prepare_target_script(target: &str, ours: bool, members: &[String]) -> String {
    let target_q = shell_quote(target);
    let notice = if ours {
        String::new()
    } else {
        // `ls -A` 认隐藏文件（`.hidden` 也要算"已经有东西"），
        // 目录不存在时它自己报错但我们不看退出码（`2>/dev/null`）——
        // 那种情况本来就没什么可提醒的。
        //
        // ⚠️ **不要加 `exit`**：这一句只是"要不要说一句"的信号，脚本必须往下走
        // （v0.9.9 那里是 `exit 0`，等于拒绝部署 —— 见上面那段为什么改掉）。
        // 也**不要**用非零退出码表达"目录非空"：那会被 `run_script` 变成
        // "参数 + stderr" 的原始错误，调用方既读不到哨兵、又拿到一句看不懂的失败。
        format!(
            "if test -d {target_q} && test -n \"$(ls -A {target_q} 2>/dev/null)\"; then \
             echo {TARGET_NOT_EMPTY}; fi; "
        )
    };
    let removals = removals_script(target, members);
    format!("{notice}mkdir -p {target_q} && {removals}")
}

/// `chmod +x <路径>`。
///
/// 见模块文档：`cp` 过去的文件权限是 **`666`** —— 任何"要被执行的文件"
/// （Go 二进制、shell 脚本）不补这一步就会稳定失败在 `Permission denied`
/// （exit 126），而部署本身看起来是**成功**的（文件在、大小对）。
pub fn chmod_exec_script(path: &str) -> String {
    format!("chmod +x {}", shell_quote(path))
}

/// 一次部署要 `cp` 哪些本地路径。
///
/// - **文件来源**：就是它自己 → 落到 `<目标>/<文件名>`；
/// - **目录来源**：它的**每一个顶层条目** → 落到 `<目标>/<条目名>`。
///
/// # ⚠️ 为什么目录要逐个顶层条目去 cp
///
/// `wslc container cp` 复制目录时**会把目录名一起带过去**（实测：
/// `cp <本地>\site nginx:/tmp/d1` 的结果是 `/tmp/d1/site/…`）。
/// 直接整目录复制的话目标里会平白多一层，而 nginx 的 Web 根多一层就等于
/// **站点打不开**（`/usr/share/nginx/html/site/index.html` 不是 nginx 认的入口）。
///
/// 逐个顶层条目复制，落到目标里的才是目录的**内容**。
///
/// `entries` 由调用方给（要读磁盘，不能在这个纯逻辑模块里做）——
/// 只传**顶层**那一层，且必须**非空**（空目录部署出来就是空的，调用方要能说出
/// 这件事而不是当作成功）。
pub fn copy_members(source: &Path, kind: SourceKind, entries: &[PathBuf]) -> Vec<PathBuf> {
    match kind {
        SourceKind::File => vec![source.to_path_buf()],
        SourceKind::Directory => entries.to_vec(),
    }
}

/// 拼 `wslc container cp` 的参数。
///
/// 形状：`container cp -q <本地路径> <容器>:<目标目录>`。
///
/// - 位置参数在**开关后面**（`wslc` 的位置参数解析遇到第一个非选项就只认
///   位置参数了，和 `exec` / `logs` 那两条不变量同理）；
/// - `-q` 抑制进度输出：实测非 tty 时本来就没有输出，加它是为了**防止**
///   将来版本往 stdout 里写进度条（那会把调用方的输出解析搅乱）；
/// - ⚠️ 目标**必须是已存在的目录**（实测不存在的路径报
///   `ERROR_PATH_NOT_FOUND`）→ 调用方先跑 [`prepare_target_script`]。
pub fn cp_args(source: &Path, container: &str, target_dir: &str) -> Vec<String> {
    vec![
        "container".to_owned(),
        "cp".to_owned(),
        "-q".to_owned(),
        source.to_string_lossy().to_string(),
        format!("{container}:{target_dir}"),
    ]
}

/// 启动脚本：把应用甩到后台，并把 pid 写进 pidfile。
///
/// ⚠️ `command` 是**原样**进脚本的（**不做** [`shell_quote`]）。这是刻意的，
/// 理由和 `wslc_panel_core::state::exec_argv` 那段一样：用户在这一格里写的就是
/// 他要执行的**命令行**（`java -jar x.jar --spring.profiles.active=prod`），
/// 把它当数据引号起来就没法用了。界面上必须把这件事说出来。
///
/// 路径（`dir` / 日志 / pidfile）则**一律**引号 —— 那些是数据。
pub fn start_script(dir: &str, command: &str) -> String {
    let dir_q = shell_quote(dir);
    let log_q = shell_quote(&container_join(dir, LOG_FILE));
    let pid_q = shell_quote(&container_join(dir, PID_FILE));
    // ⚠️ 大括号不能省：`&` 的优先级最低（见模块文档那条实测）。
    // `{{` / `}}` 是 `format!` 的转义，出来是单个花括号。
    format!(
        "mkdir -p {dir_q} && cd {dir_q} && {{ nohup {command} > {log_q} 2>&1 & echo $! > {pid_q}; }}"
    )
}

/// 判活脚本：pidfile 在、那个 pid 还活着、**而且它不是僵尸** → `__WSLC_RUNNING__`。
///
/// # ⚠️ 为什么要多看一眼 `/proc/<pid>/stat`（v0.9.8 加）
///
/// `kill -0` 对**僵尸进程**也返回 0 —— 而"启动失败"恰好会留下一个僵尸：
///
/// ```text
/// $ nohup java -jar app.jar > app.log 2>&1 & echo $! > app.pid
/// nohup: failed to run command 'java': No such file or directory
/// $ ps -o pid,ppid,args
///   PID  PPID COMMAND
///     1     0 tail -f /dev/null     ← PID 1 是面板吊容器用的 tail，它**不回收子进程**
///    29     1 [nohup]               ← 僵尸：`kill -0 29` 仍然是成功
/// ```
///
/// 于是 pidfile 里那个 pid "还活着"，界面上一直显示**运行中**（实机报过来的就是
/// 这个："java 容器日志和部署的 java 应用日志都看不到"，其实进程早就死了）。
///
/// 修法：读 `/proc/<pid>/stat` 的**第三个字段**（进程状态），`Z` 就当死。
/// 这里不用 `awk`：镜像里不一定有它（busybox 有、精简镜像没有），
/// 而纯 shell 剥掉 `comm` 那一段只需要 `${s##*) }` 一步 ——
/// ⚠️ 必须用 `##`（**最长**匹配）而不是 `#`：`comm` 自己可能带 `)` 甚至 `) `，
/// 短的匹配会停在 comm 中间，剥出来的"状态"就成了 comm 的残渣（实测这一条是推的，
/// 但代价只有两个字符）。
///
/// `test -f X && kill -0 … && … && echo R || echo S` 的结合性正是我们要的
/// （链上任何一环失败都会走到 `echo S`）。
pub fn status_script(pid_path: &str) -> String {
    let pid_q = shell_quote(pid_path);
    format!(
        "test -f {pid_q} && p=$(cat {pid_q}) && kill -0 \"$p\" 2>/dev/null && \
         s=$(cat /proc/\"$p\"/stat 2>/dev/null) && s=${{s##*) }} && st=${{s%% *}} && \
         [ -n \"$st\" ] && [ \"$st\" != Z ] && echo {STATUS_RUNNING} || echo {STATUS_STOPPED}"
    )
}

/// 停止脚本：发 `TERM` 然后删掉 pidfile。
///
/// ⚠️ v0.9 **不做 `kill -9`**。容器里**没有 `ps`、没有 `pkill`**（实测），
/// 要确认"它是不是真的没退"得自己扫 `/proc`，收益不抵复杂度。
/// 界面上把"只发 TERM"这件事写出来。
///
/// 用 `if test -f` 包着 `kill`：pidfile 不在时 `kill $(cat …)` 会因为**没有参数**
/// 而报一句用法错误（那行会进 stderr，被用户当成"停止失败"）。
pub fn stop_script(pid_path: &str) -> String {
    let pid_q = shell_quote(pid_path);
    format!(
        "if test -f {pid_q}; then kill $(cat {pid_q}) 2>/dev/null; fi; rm -f {pid_q}; echo {STATUS_STOPPED}"
    )
}

/// 看日志：最后 `lines` 行；没有日志文件时输出哨兵 [`NO_LOG`]。
///
/// `|| echo NO_LOG` 有个已知的副作用：`test -f` 过了但 `tail` 自己失败时也会
/// 打哨兵。那属于"文件在但读不了"的怪情况，按"没有日志"处理可以接受。
pub fn tail_script(log_path: &str, lines: usize) -> String {
    let log_q = shell_quote(log_path);
    format!("test -f {log_q} && tail -n {lines} {log_q} || echo {NO_LOG}")
}

/// 数目标目录里的文件数（内容形态用它区分"没部署过"和"部署过了"）。
pub fn count_files_script(dir: &str) -> String {
    let dir_q = shell_quote(dir);
    format!("if test -d {dir_q}; then find {dir_q} -type f | wc -l; else echo {NO_DIR}; fi")
}

/// 数目标目录的**顶层条目**数。
///
/// 给"复制真的发生了吗"那条核对用（见 `crate::cmd::deploy::verify_copy`）。
///
/// ⚠️ 判据刻意用**顶层条目**而不是文件数：一个只有空子目录的**合法**来源，
/// 文件数本来就是 0，拿文件数判会把它误报成失败。而顶层条目数正好等于
/// 我们要复制过去的那些名字的个数。
///
/// 解析仍走 [`parse_file_count`]（它就是"解析 `wc -l` 的输出或 `NO_DIR` 哨兵"，
/// 与数的是文件还是条目无关）。
pub fn count_entries_script(target: &str) -> String {
    let target_q = shell_quote(target);
    format!(
        "if test -d {target_q}; then find {target_q} -mindepth 1 -maxdepth 1 | wc -l; else echo {NO_DIR}; fi"
    )
}

/// 重载 nginx（铺完配置 / 内容之后想让它立刻生效时用）。
///
/// `2>&1` 是必须的：nginx 把 `signal process started` 那句写在 **stderr**，
/// 不并过来用户在提示条上就什么也看不到。
pub fn reload_nginx_script() -> String {
    "nginx -s reload 2>&1".to_owned()
}

/// 一次 exec 同时拿到"跑没跑"和"目录里有多少文件"。
///
/// # 为什么要合成一条
///
/// 列表里**每一条**部署都要问一次状态，分两次 exec 就是两倍往返。
/// 两条脚本用 `;` 串起来，顺序固定（**进程状态在前、文件数在后**），
/// 解析按位置取 —— 见 [`parse_probe_output`]。
///
/// 内容形态没有进程可问（nginx 是容器自己的主进程，见 [`AppRuntime::kind`]），
/// 所以只数文件。
pub fn probe_script(kind: DeployKind, dir: &str) -> String {
    let count = count_files_script(dir);
    match kind {
        DeployKind::Content => count,
        DeployKind::Process => {
            let status = status_script(&container_join(dir, PID_FILE));
            format!("{status}; {count}")
        }
    }
}

/// 一次探测拿到的两件事。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeResult {
    /// 进程形态：跑没跑。内容形态恒为 `None`（没有属于我们的进程可问）。
    pub process: Option<ProcessStatus>,
    /// 目标目录里的文件数；目录不存在 → `None`（**不是** `Some(0)` ——
    /// "没部署过"和"部署了一个空目录"在界面上是两句话）。
    pub files: Option<u64>,
}

/// 解析 [`probe_script`] 的输出。
///
/// ⚠️ **按位置取，不能都取最后一行**：进程那条在前、文件数在后。
/// [`parse_process_status`] 和 [`parse_file_count`] 各自都找"最后一个非空行"
/// （它们服务的是单条脚本），拿它们直接套合并输出会双双落空。
pub fn parse_probe_output(kind: DeployKind, stdout: &str) -> ProbeResult {
    let lines: Vec<&str> = stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    match kind {
        DeployKind::Content => ProbeResult {
            process: None,
            files: lines.last().and_then(|l| parse_file_count(l)),
        },
        DeployKind::Process => ProbeResult {
            process: lines.first().and_then(|l| parse_process_status(l)),
            files: lines.last().and_then(|l| parse_file_count(l)),
        },
    }
}

/// 删除一次部署（「删除」按钮用）：只删**我们自己铺的那些名字**。
///
/// ⚠️ 不碰目录本身（理由见 [`prepare_target_script`]：目标可能是卷挂载点，
/// 对挂载点 `rm -rf` 返回 1），也**不碰别人的文件** —— 只删 `members`
/// （调用方从 `DeployRecord::members` 取，也就是上一次真的铺了什么）
/// 加上 `app.log` / `app.pid`。见 [`removals_script`] 那段"为什么要改"：
/// 整目录清空在 v0.9.8 把 `eclipse-temurin` 的 JDK 删掉过一次。
///
/// 用 `if test -d` 包着是因为目录可能已经不在（容器重建过）——
/// 那种情况**不算失败**，用户要的结果（里面没我们的东西了）已经成立了。
///
/// ⚠️ 这一步**不可撤销**，而且目标是卷挂载点时会删掉卷里的数据 ——
/// 所以界面上必须是二次确认，而且要把完整路径显示出来。
pub fn remove_script(target: &str, members: &[String]) -> String {
    let target_q = shell_quote(target);
    let removals = removals_script(target, members);
    format!("if test -d {target_q}; then {removals}; fi; echo {STATUS_STOPPED}")
}

// ---------------------------------------------------------------------------
// 参数拼装
// ---------------------------------------------------------------------------

/// 拼"在运行中的容器里执行一条命令"的参数（非交互）。
///
/// 形状 `exec <容器> <命令> …`：位置参数在开关**后面**（`wslc` 的位置参数解析
/// 遇到第一个非选项就只认位置参数了，和 `logs_args` 那条不变量同理）。
pub fn exec_args(container: &str, script: &str) -> Vec<String> {
    vec![
        "exec".to_owned(),
        container.to_owned(),
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        script.to_owned(),
    ]
}

// 这里**有意没有** `exec_i_args`（"拼 `exec -i` 的参数"）。
//
// 它不是没想过：`-i` 在容器名前面、实测 `wslc exec -i nginx sh -c '…'`
// 能收到管道里的字节。但传文件已经改用 `wslc container cp`（见 `cp_args`），
// 而"往容器里灌一段文本"这条路也没做 —— 因为
// `crate::cli::build_command` 写死 `Stdio::null()`，子进程的 stdin 是关的，
// 要用它得先给 `cli.rs` 加一条新路。
//
// **没有调用方就不要留在公共 API 里**：它只会让下一个人以为"这条路能用"。
// 真要用时再加，`exec_args` 就在上面、照着写即可。

// ---------------------------------------------------------------------------
// 输出解析
// ---------------------------------------------------------------------------

/// 进程形态的判活结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessStatus {
    /// pidfile 在、进程还活着。
    Running,
    /// pidfile 不在、或者那个 pid 已经没了。
    Stopped,
}

/// 解析 [`status_script`] 的输出。
///
/// 认不出来返回 `None`（比如容器里的 `sh` 把脚本打错了）——
/// 调用方要把它当成"读不到状态"而不是"已停止"，两者在界面上是两句话。
pub fn parse_process_status(stdout: &str) -> Option<ProcessStatus> {
    // 只看最后一行：脚本只有一条 echo，但前面可能混进别的行
    let last = stdout.lines().rev().find(|l| !l.trim().is_empty())?;
    if last.contains(STATUS_RUNNING) {
        Some(ProcessStatus::Running)
    } else if last.contains(STATUS_STOPPED) {
        Some(ProcessStatus::Stopped)
    } else {
        None
    }
}

/// 目标目录里有多少个文件；目录不存在 → `None`。
///
/// `wc -l` 的输出**左边有空格**（右对齐），所以必须 `trim` 再 parse。
pub fn parse_file_count(stdout: &str) -> Option<u64> {
    let last = stdout.lines().rev().find(|l| !l.trim().is_empty())?;
    if last.contains(NO_DIR) {
        return None;
    }
    last.trim().parse().ok()
}

/// 把 [`tail_script`] 的输出变成要给用户看的一段文本。
///
/// 哨兵在这里翻译成人话 —— 把界面文案留在 Rust 里，而不是塞进容器里跑的脚本
/// （那样以后改文案就要连脚本一起改，而且没法单测）。
pub fn render_log_output(stdout: &str) -> String {
    if stdout.contains(NO_LOG) {
        return "还没有日志文件 —— 这个应用可能还没启动过。".to_owned();
    }
    let trimmed = stdout.trim_end();
    if trimmed.is_empty() {
        return "（日志是空的 —— 应用启动了，但还没有任何输出）".to_owned();
    }
    trimmed.to_owned()
}

// ---------------------------------------------------------------------------
// 端口发布（只能报告，不能发布）
// ---------------------------------------------------------------------------

/// `inspect.Ports` 的形状：`"80/tcp" -> [(host_ip, host_port)]`。
///
/// 单独写一个别名，是因为这个形状出现在两处**结构体签名**里
/// （`cmd::deploy::DeployProbe::ports` 与 `wslc_panel_core::state::DeployState::ports`）
/// —— 元组套元组写两遍必然会有人抄错一层。判据函数
/// [`port_is_published`] 收的是它的**切片**形态（这样测试里传
/// `&[…]` / `&vec![…]` 都能用）。
pub type PortMap = Vec<(String, Vec<(String, String)>)>;

/// 这个**容器端口**有没有发布到主机。
///
/// # 为什么要抽成纯函数（v0.9.13）
///
/// 早先这个判据有两份：`cmd::deploy::DeployProbe::publishes()`（有单测、**没有
/// 生产调用点**）和 `views::deploy_port_cell` 里内联复制的一段。也就是说
/// **跑在界面上的那份从来没被测过** —— 正是本仓库最忌讳的"测的不是跑的那份"。
/// 现在两边都调这里，测试也就真的盯着在跑的那份。
///
/// ⚠️ 匹配要**带那个 `/`**：容器端口的键是 `"80/tcp"` 这种形状，只用
/// `starts_with("80")` 会把 `8080/tcp` 也算成"80 已发布"。
///
/// ⚠️ 有键但**没有实际绑定**（`"80/tcp": []`）不算发布出去 —— `inspect` 在
/// 容器**声明**了端口却没映射到主机时就是这个形状（本机那个 nginx 容器
/// 的 `Labels` 里也能看到同样的空数组）。
pub fn port_is_published(ports: &[(String, Vec<(String, String)>)], port: u16) -> bool {
    let prefix = format!("{port}/");
    ports
        .iter()
        .any(|(key, bindings)| key.starts_with(&prefix) && !bindings.is_empty())
}

// ---------------------------------------------------------------------------
// 错误分类
// ---------------------------------------------------------------------------

/// 一次部署 / 运维动作失败的类型。
///
/// 分类只为了给**一句话的下一步** —— 原文照样留着（用户可能要贴给别人看）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeployErrorKind {
    /// 容器没在运行（一切都是 `wslc exec`，它必然失败）。
    ContainerNotRunning,
    /// 镜像里没有我们要用的工具（`/bin/sh`）。
    MissingTool,
    /// 没有可执行位 —— 就是那个 `666` 的坑。
    PermissionDenied,
    /// 路径不存在 / 建不出来 / 被别的东西占着。
    PathNotFound,
    /// 认不出来的。
    Other,
}

impl DeployErrorKind {
    /// 给用户的一句话下一步（`None` = 直接看原文就行）。
    pub fn hint(self) -> Option<&'static str> {
        match self {
            Self::ContainerNotRunning => {
                Some("容器没在运行 —— 先到「容器」页把它启动起来（这一页每个动作都要进容器）")
            }
            // ⚠️ v0.9 的搬运改成 `wslc container cp` 之后，要用到的只剩 `/bin/sh`
            //（准备目录 / 起停 / 判活 / 看日志全是 `sh -c` 一条脚本）。
            // 这句里早先写着"部署需要 /bin/sh **和 tar**"，而 tar 那条路
            // （往 stdin 灌 tar）已经被否掉了 —— 留着只会把人往一个
            // 已经不存在的问题上引。同族的 `BadArchive` / `NoHostTar`
            // 两个变体也一起删了：`classify_error` 造不出它们了。
            Self::MissingTool => Some(
                "镜像里缺我们要用的命令。整套动作只用容器里的 `/bin/sh`\
                 （准备目录、起停、判活、看日志都是一条 `sh -c` 脚本）——\
                 这个镜像精简到连它都没有的话，换一个完整点的镜像，\
                 或者用「终端」自己进去装",
            ),
            Self::PermissionDenied => Some(
                "文件没有可执行位（从 Windows 传过去的文件默认没有）。\
                 这是面板的 bug，请把它报出来",
            ),
            Self::PathNotFound => {
                Some("容器里的这个路径不存在、建不出来，或者被别的东西占着 —— 换一个目录试试（比如 /opt 底下）")
            }
            Self::Other => None,
        }
    }

    /// 给用户看的一句话（没有建议时用原文兜底）。
    pub fn label(self) -> &'static str {
        match self {
            Self::ContainerNotRunning => "容器没在运行",
            Self::MissingTool => "镜像里缺命令",
            Self::PermissionDenied => "没有可执行位",
            Self::PathNotFound => "路径不可用",
            Self::Other => "操作失败",
        }
    }
}

/// 从一段报错文本 + 退出码分类。
///
/// 判据都是**实测抓下来的原文**（见模块文档那张表），顺序从"最确定"到"最泛"：
/// 错的关键词排在泛的前面，否则会被泛的吃掉。
pub fn classify_error(text: &str, code: Option<i32>) -> DeployErrorKind {
    let lower = text.to_ascii_lowercase();

    // 容器没运行：这条最要紧，因为它是"每个动作都会碰到"的那一个
    if text.contains("WSLC_E_CONTAINER_NOT_RUNNING")
        || text.contains("未运行")
        || lower.contains("is not running")
    {
        return DeployErrorKind::ContainerNotRunning;
    }

    // 没有可执行位（实测：`sh: 1: /opt/probe/runme: Permission denied` + exit 126）
    if lower.contains("permission denied") {
        return DeployErrorKind::PermissionDenied;
    }

    // 归档解不开那一条**随 tar 方案一起删了**（v0.9.13）：搬运现在只走
    // `wslc container cp`，它不产生 tar 报错，那两个关键词分支是死的。
    // 留着它的代价不是"多几行"—— 是下一个人会以为"有一条 tar 的路能用"。

    // 镜像里缺命令：exit 127，或者 OCI runtime 的 "executable file not found"
    // （后者是 `cmd::mysql` 那边实测到的原文）
    if lower.contains("not found")
        || lower.contains("executable file not found")
        || code == Some(127)
    {
        return DeployErrorKind::MissingTool;
    }

    // 路径不可用。判据全是**本机跑出来的原文**（wslc 3.0.3.0 + 一个 alpine 容器，
    // 2026-10-16 复核）：
    //
    // - `mkdir -p <已存在的文件>` → `mkdir: cannot create directory '/tmp/afile':
    //   File exists`（用户把目标目录填成某个文件名，是**最像手滑**的一种）；
    // - `mkdir -p <穿过一个文件的路径>` → 同上，但尾句是 `Not a directory`；
    // - `wslc container cp <文件> <容器>:/不存在的目录/` → exit 1 +
    //   `Could not find the file /tmp/nope-dir/ in container …` +
    //   `错误代码： ERROR_PATH_NOT_FOUND`
    //   （⚠️ 注意那是 `ERROR_PATH_NOT_FOUND`：下划线形态**匹配不到**
    //    上面那句 `lower.contains("not found")`，所以它落在这一支里是对的）；
    // - 早先实测的 `The system cannot find the path specified.`（Windows 侧）
    //   与 `No such file or directory`（容器里）继续留着。
    //
    // ⚠️ 这三条是 v0.9.13 补的：在补之前它们全都落到 `Other`，
    // 而 `Other` 的 `hint()` 是 `None` —— 用户只拿到一句引擎原文，
    // 明明这一支的"换个目录试试"就是对的。
    if lower.contains("no such file or directory")
        || lower.contains("cannot find the path")
        || lower.contains("directory nonexistent")
        || lower.contains("could not find the file")
        || lower.contains("error_path_not_found")
        || lower.contains("cannot create directory")
        || lower.contains("not a directory")
        || lower.contains("file exists")
    {
        return DeployErrorKind::PathNotFound;
    }

    DeployErrorKind::Other
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ContainerInspect;
    use serde_json::json;

    fn item(image: &str, command: &str) -> ContainerListItem {
        // 走真实的 serde 路径构造，而不是手搓结构体 —— 夹具的字段名必须和
        // `wslc list` 真给的一致（见 `docs/wslc-schema.md`）。
        let value = json!({
            "Id": "68da9a6b0012978d72fb1553c45797e05671b8d70b1e002d3a72cd0deb7a57fd",
            "Image": image,
            "Command": command,
            "Created": "2026-10-14T08:55:52Z",
            "State": "running",
            "Status": "Up 2 seconds",
            "Name": "nginx",
        });
        serde_json::from_value(value).expect("夹具应当能解析")
    }

    // -----------------------------------------------------------------------
    // 识别
    // -----------------------------------------------------------------------

    #[test]
    fn recognizes_runtimes_by_image_name() {
        assert_eq!(runtime_from_image("nginx"), Some(AppRuntime::Nginx));
        assert_eq!(runtime_from_image("nginx:1.27-alpine"), Some(AppRuntime::Nginx));
        assert_eq!(runtime_from_image("openresty/openresty"), Some(AppRuntime::Nginx));

        assert_eq!(runtime_from_image("openjdk"), Some(AppRuntime::Java));
        assert_eq!(runtime_from_image("eclipse-temurin:21-jre"), Some(AppRuntime::Java));
        assert_eq!(runtime_from_image("amazoncorretto"), Some(AppRuntime::Java));
        assert_eq!(runtime_from_image("tomcat"), Some(AppRuntime::Java));

        assert_eq!(runtime_from_image("node"), Some(AppRuntime::Node));
        assert_eq!(runtime_from_image("node:20-slim"), Some(AppRuntime::Node));

        assert_eq!(runtime_from_image("golang"), Some(AppRuntime::Go));
        assert_eq!(runtime_from_image("golang:1.23"), Some(AppRuntime::Go));
    }

    #[test]
    fn image_name_with_registry_prefix_still_matches() {
        // ⚠️ 这条是**本机那个真容器的实测值**：带上 registry 前缀之后
        // 必须还能认出 `nginx`，否则本机这个容器上根本不会出现按钮。
        assert_eq!(
            runtime_from_image("docker.1panel.live/library/nginx:latest"),
            Some(AppRuntime::Nginx)
        );
        // 带端口的 registry 主机名不能把端口当 tag 吃掉
        assert_eq!(
            runtime_from_image("localhost:5000/golang"),
            Some(AppRuntime::Go)
        );
        // 摘要形式（合法引用，但 `registry::split_input` 会拒它 —— 所以这里
        // 刻意不走那个函数）
        assert_eq!(
            runtime_from_image("alpine@sha256:abc"),
            None,
            "认不出来就是认不出来，不能报错"
        );
    }

    #[test]
    fn unknown_images_are_not_guessed() {
        for image in ["mysql", "alpine", "redis", "postgres", "hello-world", ""] {
            assert_eq!(runtime_from_image(image), None, "{image} 不该被认成能部署的应用");
        }
    }

    #[test]
    fn recognizes_runtimes_by_command() {
        assert_eq!(
            runtime_from_command(r#"nginx -g "daemon off;""#),
            Some(AppRuntime::Nginx)
        );
        assert_eq!(
            runtime_from_command("java -jar /app/app.jar"),
            Some(AppRuntime::Java)
        );
        assert_eq!(runtime_from_command("node server.js"), Some(AppRuntime::Node));
        assert_eq!(runtime_from_command("npm start"), Some(AppRuntime::Node));
        // Go **刻意不猜**：二进制的名字是任意的，启动命令里没有线索
        assert_eq!(runtime_from_command("./myapp"), None);
        assert_eq!(runtime_from_command("sleep infinity"), None);
    }

    #[test]
    fn java_wins_over_node_when_both_appear() {
        // `java -jar node-exporter.jar` —— `-jar` 是强信号，别被 "node" 抢走
        assert_eq!(
            runtime_from_command("java -jar node-exporter.jar"),
            Some(AppRuntime::Java)
        );
    }

    #[test]
    fn list_heuristic_prefers_image_over_command() {
        let by_image = container_app_kind(&item("nginx", "./something")).unwrap();
        assert_eq!(by_image.runtime, AppRuntime::Nginx);
        assert_eq!(by_image.signal, DeploySignal::Image);

        let by_command = container_app_kind(&item("my-company/web", "node server.js")).unwrap();
        assert_eq!(by_command.runtime, AppRuntime::Node);
        assert_eq!(by_command.signal, DeploySignal::Command);

        assert!(container_app_kind(&item("mysql", "mysqld")).is_none());
    }

    #[test]
    fn the_real_nginx_container_is_recognized() {
        // ⚠️ 两个字段值都是从**本机那个真容器**上抄下来的（`wslc list`）。
        // 注意 `Command` 被截断成 20 字符 —— 所以能认出它**全靠镜像名**，
        // 这正是"没有端口依据也够用"的证据。
        let real = item(
            "docker.1panel.live/library/nginx:latest",
            "\"/docker-entrypoint.…\"",
        );
        let found = container_app_kind(&real).expect("真容器必须认得出");
        assert_eq!(found.runtime, AppRuntime::Nginx);
        assert_eq!(found.signal, DeploySignal::Image);
    }

    #[test]
    fn inspect_recognizes_by_env_when_image_name_is_useless() {
        let insp = ContainerInspect::new(json!({
            "Config": {
                "Image": "my-company/thing:1",
                "Env": ["PATH=/usr/bin", "NGINX_VERSION=1.31.6", "NJS_VERSION=1.0.1"],
            }
        }));
        assert_eq!(runtime_from_inspect(&insp), Some(AppRuntime::Nginx));
    }

    #[test]
    fn inspect_uses_untruncated_command() {
        // `inspect` 的 Cmd **没有**被截断 —— 所以这条比列表那条启发式强
        let insp = ContainerInspect::new(json!({
            "Config": {
                "Image": "my-company/totally-unknown:1",
                "Cmd": ["nginx", "-g", "daemon off;"],
            }
        }));
        assert_eq!(runtime_from_inspect(&insp), Some(AppRuntime::Nginx));
    }

    #[test]
    fn inspect_of_a_real_nginx_container_recognizes_it() {
        // 字段是从本机 `wslc inspect nginx` 的真实输出里摘的（原样，没改）。
        let insp = ContainerInspect::new(json!({
            "Config": {
                "Cmd": ["nginx", "-g", "daemon off;"],
                "Entrypoint": ["/docker-entrypoint.sh"],
                "Env": [
                    "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
                    "NGINX_VERSION=1.31.6",
                    "NJS_VERSION=1.0.1",
                    "NJS_RELEASE=1~trixie",
                    "ACME_VERSION=0.4.1",
                    "PKG_RELEASE=1~trixie"
                ],
                "Image": "docker.1panel.live/library/nginx:latest"
            },
            "Mounts": [
                {
                    "Destination": "/usr/share/nginx/html",
                    "Name": "webdata",
                    "ReadWrite": true,
                    "Source": "webdata",
                    "Type": "volume"
                }
            ],
            "Ports": { "80/tcp": [ { "HostIp": "127.0.0.1", "HostPort": "80" } ] }
        }));
        assert_eq!(runtime_from_inspect(&insp), Some(AppRuntime::Nginx));
    }

    // -----------------------------------------------------------------------
    // Web 根
    // -----------------------------------------------------------------------

    #[test]
    fn picks_the_volume_mounted_under_the_nginx_web_root() {
        // 本机那个容器的实测结构
        let insp = ContainerInspect::new(json!({
            "Mounts": [
                { "Type": "volume", "Name": "webdata", "Source": "webdata",
                  "Destination": "/usr/share/nginx/html", "ReadWrite": true }
            ]
        }));
        assert_eq!(
            nginx_web_root(&insp).as_deref(),
            Some("/usr/share/nginx/html")
        );
    }

    #[test]
    fn web_root_keeps_the_site_mount_when_a_config_mount_is_also_there() {
        let insp = ContainerInspect::new(json!({
            "Mounts": [
                { "Type": "bind", "Source": "C:\\conf", "Destination": "/etc/nginx/conf.d" },
                { "Type": "volume", "Name": "web", "Destination": "/usr/share/nginx/html" }
            ]
        }));
        assert_eq!(
            nginx_web_root(&insp).as_deref(),
            Some("/usr/share/nginx/html")
        );
    }

    /// ⚠️ 回归钉（v0.9.13）：`/etc/nginx` 底下的挂载点**不是站点根**。
    ///
    /// 早先这里会返回 `/etc/nginx/conf.d`，于是「添加部署」把配置目录预填成
    /// 「容器内目标目录」——文件铺进去没有任何效果（那是配置，不是站点），
    /// 而删的时候还会先删掉容器里同名的 `.conf`。见 [`nginx_web_root`] 那段。
    #[test]
    fn config_mounts_are_never_treated_as_the_web_root() {
        for destination in [
            "/etc/nginx",
            "/etc/nginx/conf.d",
            "/etc/nginx/conf.d/",
            "/usr/share/nginx", // 站点目录的**父目录**：往里铺文件也没人读
        ] {
            let insp = ContainerInspect::new(json!({
                "Mounts": [ { "Type": "volume", "Name": "conf", "Destination": destination } ]
            }));
            assert_eq!(
                nginx_web_root(&insp),
                None,
                "{destination} 不该被当成站点根（配置 / 父目录），要回落默认值"
            );
        }
    }

    #[test]
    fn no_mounts_means_no_web_root_so_caller_uses_the_default() {
        assert_eq!(nginx_web_root(&ContainerInspect::new(json!({}))), None);
        // 形状不对的行**跳过**，不 panic
        let broken = ContainerInspect::new(json!({
            "Mounts": [ { "Type": "volume" }, "不是对象", 42 ]
        }));
        assert_eq!(nginx_web_root(&broken), None);
    }

    // -----------------------------------------------------------------------
    // shell 引号
    // -----------------------------------------------------------------------

    #[test]
    fn quotes_plain_values() {
        assert_eq!(shell_quote("/opt/app"), "'/opt/app'");
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("a b"), "'a b'");
    }

    #[test]
    fn escapes_single_quotes_the_posix_way() {
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        // 引号在最前 / 最后也要对
        assert_eq!(shell_quote("'"), r"''\'''");
    }

    #[test]
    fn injection_shapes_are_neutralized() {
        // 这几种形状如果没引号，就是真的命令执行
        let cases = [
            "; rm -rf /",
            "$(cat /etc/passwd)",
            "`id`",
            "a\nrm -rf /",
            "x' ; rm -rf / ; '",
            "&& curl evil.example",
            "${IFS}",
            "*",
            "~",
        ];
        for case in cases {
            let quoted = shell_quote(case);
            assert!(
                quoted.starts_with('\'') && quoted.ends_with('\''),
                "{quoted}"
            );
            // ⚠️ 判据：**剥掉外层引号之后**，内部不允许出现裸单引号 ——
            // 每个 `'` 都必须以 `'\''` 这个四字符组的形式出现。
            //
            // （第一版这条断言是错的：它在**没剥外层**的串上找 `'\''`，
            // 于是外层那两个引号永远算成"裸的"，九个用例**全都**断言失败。
            // 单测自己写错，比不写还坏 —— 它会让"红"变成噪音。）
            let inner = &quoted[1..quoted.len() - 1];
            let escaped = inner.matches(r"'\''").count();
            assert_eq!(
                inner.matches('\'').count(),
                escaped * 3,
                "内部只允许 `'\\''` 这一种转义：{quoted}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // 应用名白名单
    // -----------------------------------------------------------------------

    #[test]
    fn accepts_reasonable_app_names() {
        for name in ["site", "my-app", "my_app", "app1", "A", &"x".repeat(64)] {
            assert!(valid_app_name(name), "{name} 应当合法");
        }
    }

    #[test]
    fn rejects_dangerous_or_odd_app_names() {
        for name in [
            "",
            ".",
            "..",
            "../etc",
            "a/b",
            "a b",
            "a;b",
            "a$b",
            "中文",
            &"x".repeat(65),
        ] {
            assert!(!valid_app_name(name), "{name:?} 应当被拒");
        }
    }

    // -----------------------------------------------------------------------
    // 脚本
    // -----------------------------------------------------------------------

    #[test]
    fn prepare_target_removes_only_our_own_names() {
        let members = vec!["ruoyi-admin.jar".to_owned()];
        assert_eq!(
            prepare_target_script("/opt/app", true, &members),
            "mkdir -p '/opt/app' && rm -rf '/opt/app/ruoyi-admin.jar' \
             '/opt/app/app.log' '/opt/app/app.pid'"
        );
        // 目录不存在时 `mkdir -p` 要先建出来（`cp` 要求目标已存在）
        assert!(prepare_target_script("/opt/app", true, &members).starts_with("mkdir -p "));
        // ⚠️ **一个 `-delete` 都不许有**：整目录清空就是 v0.9.8 删掉 JDK 的那一手
        assert!(!prepare_target_script("/opt/app", true, &members).contains("-delete"));
    }

    /// ⚠️ 回归钉（v0.9.10）：容器重建过、存档里那条记录还在时，
    /// 「重新部署」**也不许**碰目录里不是我们铺的东西。
    ///
    /// 场景就是现场那次：目标目录 `/opt/java` 里躺着镜像自带的 `openjdk/`，
    /// 而我们要铺的只有 `ruoyi-admin.jar` —— 删除列表里**绝不能**出现
    /// `openjdk`，也绝不能出现 `find … -delete`。
    #[test]
    fn removals_never_touch_files_that_are_not_ours() {
        let members = vec!["ruoyi-admin.jar".to_owned()];
        for script in [
            prepare_target_script("/opt/java", true, &members),
            remove_script("/opt/java", &members),
        ] {
            assert!(script.contains("'/opt/java/ruoyi-admin.jar'"), "{script}");
            assert!(!script.contains("openjdk"), "{script}");
            assert!(!script.contains("-delete"), "{script}");
            // 目录本身永远不删（它可能是挂载点，`rm -rf` 会返回 1）
            assert!(!script.contains("rm -rf '/opt/java'"), "{script}");
        }
    }

    /// 成员名里带路径 / `..` 时**跳过**：宁可少删，也不能删到目标目录外面去。
    #[test]
    fn removals_skip_unsafe_member_names() {
        let members = vec![
            "ok.txt".to_owned(),
            "../escape.txt".to_owned(),
            "a/b.txt".to_owned(),
            "a\\b.txt".to_owned(),
            String::new(),
            "   ".to_owned(),
        ];
        let script = removals_script("/opt/app", &members);
        assert!(script.contains("'/opt/app/ok.txt'"), "{script}");
        assert!(!script.contains("escape"), "{script}");
        assert!(!script.contains("a/b"), "{script}");
        assert!(!script.contains("a\\b"), "{script}");
        // 固定那两个（日志 / pidfile）永远在删除列表里
        assert!(script.contains("'/opt/app/app.log'"), "{script}");
        assert!(script.contains("'/opt/app/app.pid'"), "{script}");
    }

    /// ⚠️ 回归钉（v0.9.11）：**第一次往非空目录铺时不再拒绝**。
    ///
    /// v0.9.9 是拒绝的（`exit 0` + 哨兵）。逐名删除（v0.9.10）之后那层拒绝
    /// 拦的已经不是危险，而是用户自己的正常部署 —— 实测：删掉旧记录 + 重建容器
    /// 之后重新部署 `/opt/java`（那次完全安全）被直接拒绝，用户只能来问为什么。
    #[test]
    fn prepare_target_warns_but_still_proceeds_on_a_foreign_directory() {
        let members = vec!["ruoyi-admin.jar".to_owned()];
        let script = prepare_target_script("/opt/java", false, &members);
        // 目录非空会打哨兵 —— 调用方拿它去**说一句**（不是拿去报错）
        assert!(script.contains(TARGET_NOT_EMPTY), "{script}");
        assert!(script.contains("ls -A"), "{script}");
        // ⚠️ 哨兵**不许**带 `exit`：带了就是"拒绝部署"（v0.9.9 的行为，已废）
        assert!(!script.contains("exit"), "{script}");
        // 而且照样建目录、照样删我们自己的名字（= 照样往下走）
        assert!(script.contains("mkdir -p '/opt/java'"), "{script}");
        assert!(script.contains("'/opt/java/ruoyi-admin.jar'"), "{script}");
        // 哨兵排在动手之前（读起来是"先看一眼，再动手"）
        assert!(
            script.find(TARGET_NOT_EMPTY).unwrap() < script.find("rm -rf").unwrap(),
            "{script}"
        );
        // `ours = true` 时连那一句都不说（我们自己铺的目录，没什么好提醒的）
        assert!(!prepare_target_script("/opt/java", true, &members).contains("ls -A"));
    }

    #[test]
    fn prepare_target_never_rm_rf_s_the_target_itself() {
        // ⚠️ 这条是回归闸：目标可能是**卷挂载点**（nginx 的 Web 根就是），
        // 而对挂载点 `rm -rf` 返回 **1**（`Device or resource busy`）——
        // 写成 `rm -rf <目标> && …` 的话 `&&` 链会断在第一步。
        // 实测踩到过。
        let members = vec!["index.html".to_owned()];
        for target in ["/opt/app", "/usr/share/nginx/html"] {
            // 两种 `ours` 都要查：守卫那段也不许出现 `rm -rf <目标>`
            for ours in [true, false] {
                let script = prepare_target_script(target, ours, &members);
                assert!(
                    !script.contains(&format!("rm -rf '{target}'")),
                    "不能对目标目录本身 rm -rf：{script}"
                );
                // 删的是**里面的名字**，而且没有整目录清空那一手
                assert!(
                    script.contains(&format!("rm -rf '{target}/index.html'")),
                    "{script}"
                );
                assert!(!script.contains("-delete"), "{script}");
            }
        }
    }

    #[test]
    fn chmod_exec_bit_script_quotes_the_path() {
        assert_eq!(
            chmod_exec_script("/opt/app/bin"),
            "chmod +x '/opt/app/bin'"
        );
        // 注入形状必须被引号吃掉
        let injected = chmod_exec_script("/opt/a; rm -rf /");
        assert!(injected.contains("'/opt/a; rm -rf /'"), "{injected}");
    }

    #[test]
    fn copy_members_takes_a_file_as_is_and_a_directory_by_its_entries() {
        let file = PathBuf::from(r"E:\build\myapp.jar");
        assert_eq!(
            copy_members(&file, SourceKind::File, &[]),
            vec![file.clone()],
            "文件来源只复制它自己"
        );

        // ⚠️ 目录来源要**逐个顶层条目**（`wslc container cp` 复制目录时会把
        // 目录名一起带过去，整目录复制会让目标里多一层）
        let dir = PathBuf::from(r"E:\build\site");
        let entries = vec![
            dir.join("index.html"),
            dir.join("assets"),
            dir.join(".hidden"),
        ];
        assert_eq!(
            copy_members(&dir, SourceKind::Directory, &entries),
            entries,
            "目录来源要的就是那些顶层条目（含隐藏文件）"
        );
        // 空目录 → 没有东西可传（调用方要能把这件事说出来，而不是当作成功）
        assert!(copy_members(&dir, SourceKind::Directory, &[]).is_empty());
    }

    #[test]
    fn cp_args_put_positionals_last_and_use_the_container_colon_form() {
        let args = cp_args(Path::new(r"E:\build\site\index.html"), "nginx", "/opt/site");
        assert_eq!(
            args,
            vec![
                "container",
                "cp",
                "-q",
                r"E:\build\site\index.html",
                "nginx:/opt/site"
            ]
        );
        // `container cp` 是**子命令**，不是 `cp`（顶层表里没有 `cp`，实测
        // `wslc cp` 报「无法识别的命令」）
        assert_eq!(args[0], "container");
        assert_eq!(args[1], "cp");
    }

    #[test]
    fn start_script_groups_the_background_job() {
        let script = start_script("/opt/app", "java -jar app.jar");
        // ⚠️ 大括号必须在：`mkdir && cd && nohup … & echo $! > pid` 里
        // `&` 优先级最低，不分组就会把 pidfile 的写入甩到后台组外面去
        // （实测报 `cannot create …: Directory nonexistent`）。
        assert!(script.contains("&& { nohup "), "{script}");
        assert!(script.contains("& echo $! > "), "{script}");
        assert!(script.ends_with("; }"), "{script}");
        // 顺序也不能反：pidfile 必须在 `cd` 之后写，且 `$!` 紧跟 `&`
        let amp = script.find(" & echo $!").expect("要有 `& echo $!`");
        let brace = script.find("{ nohup").expect("要有分组");
        assert!(brace < amp, "{script}");
    }

    #[test]
    fn start_script_quotes_paths_but_not_the_command() {
        let script = start_script("/opt/my app", "node server.js --flag=1");
        // 路径是数据 → 引号
        assert!(script.contains("mkdir -p '/opt/my app'"), "{script}");
        assert!(script.contains("> '/opt/my app/app.log'"), "{script}");
        assert!(script.contains("> '/opt/my app/app.pid'"), "{script}");
        // 命令是用户写的**代码** → 原样，不能被引号包起来
        assert!(script.contains("nohup node server.js --flag=1 >"), "{script}");
    }

    #[test]
    fn start_script_neutralizes_an_injected_directory() {
        // 目录名里塞命令 —— 引号之后它必须只是一个**字面量目录名**。
        //
        // ⚠️ 这里**不能**用 `!script.contains("; rm -rf /; echo '")` 来判：
        // 那个串**一定会出现** —— 转义把 `'\''` 里的那个 `'` 刚好插在
        // `; rm -rf /; echo ` 后面。第一版就是这么写的，CI 上
        // `core` 因此红了一条（478 过 / 1 挂）。
        //
        // 有意义的是下面两件事：
        //   ① 引号之后的**整体形式**逐字钉死（注入的那串只是数据）；
        //   ② 除了 `'\''` 这一种转义，里面不许有裸单引号。
        let dir = "/opt/a'; rm -rf /; echo '";
        let script = start_script(dir, "true");

        // ① `shell_quote(dir)` 的完整形态
        let dir_q = "'/opt/a'\\''; rm -rf /; echo '\\'''";
        assert!(script.contains(dir_q), "{script}");

        // ② 数一遍（和 `shell_quote` 那条测试同一个判据）
        let inner = &dir_q[1..dir_q.len() - 1];
        let escaped = inner.matches(r"'\''").count();
        assert_eq!(inner.matches('\'').count(), escaped * 3, "{dir_q}");

        // ③ 命令本身没被目录名吃掉，而且大括号分组还在
        //   （`&` 的优先级最低，少了 `{{ }}` 那个 `echo $! > …` 会跑到
        //    分组外面去 —— 模块文档里那条实测）
        assert!(script.contains("nohup true >"), "{script}");
        assert!(script.ends_with("; }"), "{script}");
    }

    #[test]
    fn status_script_and_its_parser_agree() {
        let script = status_script("/opt/app/app.pid");
        assert!(script.contains(STATUS_RUNNING), "{script}");
        assert!(script.contains(STATUS_STOPPED), "{script}");
        assert!(script.contains("kill -0"), "{script}");
        // ⚠️ 僵尸必须算"没在跑"：`kill -0` 对僵尸也返回 0，而"启动失败"恰好留下
        // 一个僵尸（实机：pidfile 里那个 `[nohup]`，PPID=1，PID 1 不回收子进程）
        // —— 少了这一条，界面会一直显示"运行中"，而应用早就死了。
        assert!(script.contains("/proc/"), "{script}");
        assert!(script.contains("!= Z"), "{script}");
        // 读状态不能用 `awk`：精简镜像里不一定有它（纯 shell 剥 `(comm)` 就够）
        assert!(!script.contains("awk"), "{script}");

        assert_eq!(
            parse_process_status(STATUS_RUNNING),
            Some(ProcessStatus::Running)
        );
        assert_eq!(
            parse_process_status(&format!("{STATUS_STOPPED}\n")),
            Some(ProcessStatus::Stopped)
        );
        // 认不出来是 `None`，**不是**"已停止" —— 两者界面文案不同
        assert_eq!(parse_process_status("error: 别的东西"), None);
        assert_eq!(parse_process_status(""), None);
    }

    #[test]
    fn stop_script_guards_the_missing_pidfile() {
        let script = stop_script("/opt/app/app.pid");
        // 没有 `test -f` 的话 `kill $(cat …)` 会因为**没有参数**报一句用法错误
        assert!(script.contains("if test -f"), "{script}");
        assert!(script.contains("rm -f"), "{script}");
    }

    #[test]
    fn tail_and_count_scripts_use_sentinels() {
        assert!(tail_script("/opt/app/app.log", 200).contains("tail -n 200"));
        assert!(tail_script("/opt/app/app.log", 200).contains(NO_LOG));
        assert!(count_files_script("/opt/app").contains("find"));
        assert!(count_files_script("/opt/app").contains(NO_DIR));
    }

    #[test]
    fn entry_count_only_looks_at_the_top_level() {
        // ⚠️ 必须是 `-mindepth 1 -maxdepth 1`：只数**顶层条目**。
        // 少了 `-maxdepth 1` 就会把整棵树都数进来（那和"复制了几个名字"
        // 对不上），少了 `-mindepth 1` 会把目标目录自己也算一个。
        let script = count_entries_script("/opt/app");
        assert!(script.contains("-mindepth 1 -maxdepth 1"), "{script}");
        assert!(script.contains(NO_DIR), "{script}");
        // 解析用的是同一个函数（`wc -l` 的输出形状一样）
        assert_eq!(parse_file_count("      3\n"), Some(3));
        assert_eq!(parse_file_count(NO_DIR), None);
    }

    #[test]
    fn probe_script_asks_for_the_process_only_in_process_shape() {
        let content = probe_script(DeployKind::Content, "/usr/share/nginx/html");
        assert!(!content.contains(STATUS_RUNNING), "{content}");
        assert!(content.contains(NO_DIR), "{content}");

        let process = probe_script(DeployKind::Process, "/opt/app");
        assert!(process.contains(STATUS_RUNNING), "{process}");
        // pidfile 是从目录推出来的，不需要调用方再传一遍
        assert!(process.contains("/opt/app/app.pid"), "{process}");
        // 顺序固定：进程状态**在前**、文件数在后（解析按位置取）
        let status_at = process.find(STATUS_RUNNING).unwrap();
        let count_at = process.find("find ").unwrap();
        assert!(status_at < count_at, "{process}");
    }

    #[test]
    fn probe_output_is_parsed_by_position_not_by_last_line() {
        // ⚠️ 关键：两行都要各自拿到，不能都取最后一行
        let out = format!("{STATUS_RUNNING}\n      12\n");
        let parsed = parse_probe_output(DeployKind::Process, &out);
        assert_eq!(parsed.process, Some(ProcessStatus::Running));
        assert_eq!(parsed.files, Some(12));

        // 停止 + 目录不在（"部署过但目录没了"）
        let out = format!("{STATUS_STOPPED}\n{NO_DIR}\n");
        let parsed = parse_probe_output(DeployKind::Process, &out);
        assert_eq!(parsed.process, Some(ProcessStatus::Stopped));
        assert_eq!(parsed.files, None, "目录不在是 None，不是 Some(0)");

        // 内容形态没有进程可问
        let parsed = parse_probe_output(DeployKind::Content, "      3\n");
        assert_eq!(parsed.process, None);
        assert_eq!(parsed.files, Some(3));

        let parsed = parse_probe_output(DeployKind::Content, NO_DIR);
        assert_eq!(parsed.files, None);

        // 空输出（不该发生）不能 panic
        let parsed = parse_probe_output(DeployKind::Process, "");
        assert_eq!(parsed.process, None);
        assert_eq!(parsed.files, None);
    }

    #[test]
    fn remove_script_quotes_and_confirms() {
        let members = vec!["index.html".to_owned()];
        let script = remove_script("/usr/share/nginx/html", &members);
        assert_eq!(
            script,
            "if test -d '/usr/share/nginx/html'; then rm -rf \
             '/usr/share/nginx/html/index.html' '/usr/share/nginx/html/app.log' \
             '/usr/share/nginx/html/app.pid'; fi; echo __WSLC_STOPPED__"
        );
        // 目录已经不在了**不算失败**（容器重建过就是这种）
        assert!(script.contains("if test -d"), "{script}");
        // 注入形状必须被引号吃掉
        let injected = remove_script("/opt/a; rm -rf /", &members);
        assert!(injected.contains("'/opt/a; rm -rf /'"), "{injected}");
    }

    // -----------------------------------------------------------------------
    // 目标目录的安全闸
    // -----------------------------------------------------------------------

    #[test]
    fn safe_target_allows_what_the_panel_itself_prefills() {
        // 面板自己预填的那两个必须在白名单里，否则用户"不改就点"会直接被拦
        assert!(
            safe_target(&AppRuntime::Nginx.default_target_dir("site")),
            "nginx 的默认 Web 根必须允许"
        );
        assert!(safe_target(&AppRuntime::Java.default_target_dir("shop")));
        assert!(safe_target("/usr/share/nginx/html"));
        assert!(safe_target("/var/www/site"));
        assert!(safe_target("/srv/web"));
        assert!(safe_target("/opt/app/"));
    }

    #[test]
    fn safe_target_rejects_the_catastrophic_ones() {
        for target in [
            // 根目录 / 一层目录 —— `find <它> -mindepth 1 -delete` 会毁掉容器
            "/",
            "/usr",
            "/etc",
            "/var",
            "/home",
            "/opt",
            "/tmp",
            "/app",
            // `/usr/share` 只差一层，但危险面是"这一层的一切"
            "/usr/share",
            "/usr/lib",
            // 白名单之外
            "/mnt/data",
            "/media/usb",
            "/root/x",
            "/boot/x",
            // 相对路径的含义取决于容器的 WORKDIR，太含糊
            "opt/app",
            "/opt",
            "",
            "   ",
            // 路径穿越
            "/opt/../usr/share",
            "/opt/app/../../etc",
            // 前缀对了但就是前缀本身（`target.len() > prefix.len()` 那条）
            "/opt/",
        ] {
            assert!(!safe_target(target), "{target:?} 必须被拒");
        }
    }

    #[test]
    fn safe_target_also_allows_the_containers_own_site_dir() {
        // 白名单里的照旧（多传一个站点目录不该改变原有结论）
        assert!(safe_target_with_site("/opt/site", Some("/site")));
        assert!(safe_target_with_site("/opt/site", None));

        // ⚠️ 这是这个函数存在的**唯一**理由：用户把站点挂在白名单外，
        // 面板从挂载点推出来的默认值必须能过自己的闸 —— 否则是死胡同。
        assert!(safe_target_with_site("/site", Some("/site")));
        assert!(safe_target_with_site("/site/v2", Some("/site")));
        assert!(safe_target_with_site("/site/", Some("/site/")));
        assert!(safe_target_with_site("/webroot", Some("/webroot")));

        // 站点目录**之外**的仍然一律拒
        assert!(!safe_target_with_site("/etc", Some("/site")));
        assert!(!safe_target_with_site("/usr", Some("/site")));
        // 路径边界：`/sitemap` 不在 `/site` 底下（前缀相同但不是一层）
        assert!(!safe_target_with_site("/sitemap", Some("/site")));
        // 站点目录往上一层也不行（`..` 那条）
        assert!(!safe_target_with_site("/site", Some("/site/../etc")));

        // 站点目录自己就不是个正常路径时，**一行都不多放**
        assert!(!safe_target_with_site("/usr", Some("/")));
        assert!(!safe_target_with_site("/usr", Some("")));
        assert!(!safe_target_with_site("/usr", Some("   ")));
        assert!(!safe_target_with_site("/usr", Some("site")));
        assert!(!safe_target_with_site("/usr", Some("/site/../..")));
    }

    #[test]
    fn safe_target_hint_lists_what_is_allowed() {
        let hint = safe_target_hint();
        assert!(hint.contains("/opt/"), "{hint}");
        // ⚠️ 这里**不再**断言"清空"：v0.9.10 起部署只替换自己铺的文件，
        // 提示里那句话也就跟着改了（断言旧文案会让测试和界面一起撒谎）
        assert!(hint.contains("不能是系统目录"), "要说清为什么被拒：{hint}");
        assert!(hint.len() > 20, "不能是一句空话：{hint}");
    }

    #[test]
    fn log_renderer_translates_the_sentinel() {
        let text = render_log_output(NO_LOG);
        assert!(text.contains("还没有日志文件"), "{text}");
        assert!(!text.contains(NO_LOG), "哨兵不该露给用户：{text}");
        assert_eq!(render_log_output(""), "（日志是空的 —— 应用启动了，但还没有任何输出）");
        assert_eq!(render_log_output("a\nb\n\n"), "a\nb");
    }

    #[test]
    fn file_count_parser_handles_wc_padding_and_missing_dir() {
        // `wc -l` 的输出左边有空格
        assert_eq!(parse_file_count("      12\n"), Some(12));
        assert_eq!(parse_file_count("0"), Some(0));
        assert_eq!(parse_file_count(NO_DIR), None);
        assert_eq!(parse_file_count("垃圾"), None);
    }

    // -----------------------------------------------------------------------
    // 参数
    // -----------------------------------------------------------------------

    #[test]
    fn exec_args_put_positionals_last() {
        // `wslc` 的位置参数解析遇到第一个非选项就只认位置参数了 ——
        // 开关跑到容器名后面会被吞掉
        let args = exec_args("web", "nginx -s reload");
        assert_eq!(args[0], "exec");
        assert_eq!(args[1], "web");
        assert_eq!(args[2..], ["/bin/sh", "-c", "nginx -s reload"]);
        // 非交互那条**不带 `-i`**：部署里跑的都是"跑完就返回"的命令。
        assert!(!args.contains(&"-i".to_owned()));
    }

    #[test]
    fn artifact_name_takes_the_last_segment_only() {
        assert_eq!(
            artifact_name(Path::new(r"E:\build\myapp.jar")).as_deref(),
            Some("myapp.jar")
        );
        assert_eq!(artifact_name(Path::new("")), None);
    }

    #[test]
    fn container_join_never_doubles_the_slash() {
        assert_eq!(container_join("/opt/app", "app.log"), "/opt/app/app.log");
        assert_eq!(container_join("/opt/app/", "app.log"), "/opt/app/app.log");
        assert_eq!(container_join("/", "app.log"), "/app.log");
        assert_eq!(container_join("", "app.log"), "/app.log");
    }

    // -----------------------------------------------------------------------
    // 默认值
    // -----------------------------------------------------------------------

    #[test]
    fn formats_defaults_per_runtime() {
        assert_eq!(AppRuntime::Nginx.kind(), DeployKind::Content);
        assert_eq!(AppRuntime::Java.kind(), DeployKind::Process);

        assert_eq!(AppRuntime::Nginx.default_target_dir("x"), "/usr/share/nginx/html");
        assert_eq!(AppRuntime::Java.default_target_dir("shop"), "/opt/shop");

        assert_eq!(AppRuntime::Nginx.default_start_command("x", None), None);
        // Java / Go 要的是**源文件的名字**（见函数文档）
        assert_eq!(
            AppRuntime::Java.default_start_command("shop", Some("shop-1.2.jar")).as_deref(),
            Some("java -jar shop-1.2.jar")
        );
        assert_eq!(
            AppRuntime::Java.default_start_command("shop", None).as_deref(),
            Some("java -jar shop.jar")
        );
        assert_eq!(
            AppRuntime::Go.default_start_command("api", Some("api")).as_deref(),
            Some("./api")
        );
        assert_eq!(
            AppRuntime::Node.default_start_command("web", None).as_deref(),
            Some("npm start")
        );

        // 只有 Go 要补可执行位
        assert!(AppRuntime::Go.needs_exec_bit());
        assert!(!AppRuntime::Java.needs_exec_bit());
        assert!(!AppRuntime::Node.needs_exec_bit());
        assert!(!AppRuntime::Nginx.needs_exec_bit());

        // 端口只是信息性的默认值
        assert_eq!(AppRuntime::Nginx.default_port(), 80);
        assert_eq!(AppRuntime::Node.default_port(), 3000);
    }

    #[test]
    fn every_runtime_has_a_label_and_a_note() {
        for runtime in AppRuntime::ALL {
            assert!(!runtime.label().is_empty());
            assert!(!runtime.note().is_empty());
            assert!(!runtime.kind().label().is_empty());
        }
    }

    #[test]
    fn runtime_keys_are_stable_distinct_and_round_trip() {
        // 存档里的键必须是**稳定的小写键**，不是显示名（`Node.js`）也不是
        // Rust 变体名（`Go`）—— 否则手改存档会很难看，改名还会读废老档
        let keys: Vec<&str> = AppRuntime::ALL.iter().map(|r| r.key()).collect();
        assert_eq!(keys, vec!["nginx", "java", "node", "go"]);
        for runtime in AppRuntime::ALL {
            assert_eq!(AppRuntime::from_key(runtime.key()), Some(runtime));
            assert!(runtime.key().chars().all(|c| c.is_ascii_lowercase()));
        }
        // 认不出来的键 → `None`（调用方跳过那条，别猜默认值）
        assert_eq!(AppRuntime::from_key("rust"), None);
        assert_eq!(AppRuntime::from_key(""), None);
        // 前后空格容错
        assert_eq!(AppRuntime::from_key(" go "), Some(AppRuntime::Go));
    }

    // -----------------------------------------------------------------------
    // 错误分类
    // -----------------------------------------------------------------------

    #[test]
    fn classifies_the_texts_we_actually_measured() {
        // 全部是实测抓下来的原文（见模块文档那张表）
        assert_eq!(
            classify_error(
                "容器\"947c4f25…\"未运行。\n错误代码： WSLC_E_CONTAINER_NOT_RUNNING",
                Some(1)
            ),
            DeployErrorKind::ContainerNotRunning
        );
        assert_eq!(
            classify_error("sh: 1: /opt/probe/runme: Permission denied", Some(126)),
            DeployErrorKind::PermissionDenied
        );
        assert_eq!(
            classify_error("sh: 1: tar: not found", Some(127)),
            DeployErrorKind::MissingTool
        );
        assert_eq!(
            classify_error("The system cannot find the path specified.", Some(1)),
            DeployErrorKind::PathNotFound
        );
        assert_eq!(
            classify_error("tar: /nope: Cannot open: No such file or directory", Some(2)),
            DeployErrorKind::PathNotFound
        );
        assert_eq!(
            classify_error("OCI runtime exec failed: exec: \"mysql\": executable file not found in $PATH", Some(126)),
            DeployErrorKind::MissingTool
        );
        assert_eq!(classify_error("没什么线索", None), DeployErrorKind::Other);
    }

    /// ⚠️ 回归钉（v0.9.13）：`mkdir` / `cp` 真实报出来的那三种"路径不可用"。
    ///
    /// 三段文本都是**本机跑出来的原文**（wslc 3.0.3.0 + 一个 alpine 容器）：
    /// 前两条是把目标目录填成"一个已存在的文件"和"穿过一个文件的路径"
    /// （最像手滑的两种），第三条是 `cp` 打到不存在的目录。
    ///
    /// 在补这一条之前，它们**全都落到 `Other`**，而 `Other` 没有 `hint()`
    /// —— 用户只拿到引擎原文，尽管"换一个目录试试"就是对的。
    #[test]
    fn classifies_the_path_errors_we_just_measured() {
        for text in [
            "mkdir: cannot create directory '/tmp/afile': File exists",
            "mkdir: cannot create directory '/tmp/afile': Not a directory",
            "来自引擎的错误信息: Could not find the file /tmp/nope-dir/ in container 453766a51f44\n错误代码： ERROR_PATH_NOT_FOUND",
        ] {
            assert_eq!(
                classify_error(text, Some(1)),
                DeployErrorKind::PathNotFound,
                "{text}"
            );
        }
        // `lower` 是拿整段做的，所以大小写 / 前缀都不影响判断
        assert_eq!(
            classify_error("ERROR_PATH_NOT_FOUND", None),
            DeployErrorKind::PathNotFound
        );
    }

    #[test]
    fn container_not_running_wins_over_everything_else() {
        // 这条报错里同时有中文"未运行"，别被后面的关键词抢走
        let text = "容器\"x\"未运行。错误代码： WSLC_E_CONTAINER_NOT_RUNNING\nPermission denied";
        assert_eq!(
            classify_error(text, Some(1)),
            DeployErrorKind::ContainerNotRunning
        );
    }

    #[test]
    fn every_error_kind_has_a_label_and_an_actionable_hint() {
        for kind in [
            DeployErrorKind::ContainerNotRunning,
            DeployErrorKind::MissingTool,
            DeployErrorKind::PermissionDenied,
            DeployErrorKind::PathNotFound,
        ] {
            assert!(!kind.label().is_empty());
            let hint = kind.hint().expect("这几类都要给下一步");
            assert!(hint.len() > 10, "建议不能是一句空话：{hint}");
        }
        // `Other` 刻意没有建议（就看原文）
        assert_eq!(DeployErrorKind::Other.hint(), None);
    }

    // -----------------------------------------------------------------------
    // 端口发布
    // -----------------------------------------------------------------------

    #[test]
    fn publishing_is_matched_by_container_port() {
        // 本机那个 nginx 容器的实测值
        let ports = vec![(
            "80/tcp".to_owned(),
            vec![("127.0.0.1".to_owned(), "80".to_owned())],
        )];
        assert!(port_is_published(&ports, 80));
        assert!(!port_is_published(&ports, 8080), "没发布的端口要如实说不");
        // ⚠️ `8080` 不能被 `80/tcp` 匹配上（前缀要带那个 `/`）
        assert!(!port_is_published(&ports, 8));
    }

    #[test]
    fn a_port_key_with_no_bindings_counts_as_not_published() {
        // 容器**声明**了端口但没映射到主机时就是这个形状（`"80/tcp": []`）
        let ports = vec![("80/tcp".to_owned(), Vec::new())];
        assert!(
            !port_is_published(&ports, 80),
            "有键但没有实际绑定（HostIp/HostPort 缺失）不算发布出去"
        );
        // 一个映射都没有时不 panic
        assert!(!port_is_published(&[], 80));
    }
}
