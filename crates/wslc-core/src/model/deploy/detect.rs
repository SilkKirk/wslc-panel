//! 识别：镜像名 / 启动命令 / inspect 里能不能看出跑的是哪一类应用。

use super::super::{ContainerInspect, ContainerListItem, image_last_segment};

use super::*;

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
        | "liberica" | "maven" | "gradle" | "tomcat" | "jetty" | "wildfly" => {
            Some(AppRuntime::Java)
        }
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
