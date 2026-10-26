//! 运行时与形态：`AppRuntime` / `DeployKind` / `SourceKind` 与默认值。

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
                let bin = artifact
                    .map(str::to_owned)
                    .unwrap_or_else(|| app.to_owned());
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
/// "部署把镜像自带的 JDK 删掉"那次的现场路径
///（`eclipse-temurin` 的 JDK 就在 `/opt/java/openjdk`）。逐名删除之后面板
/// 已经不会删它了，但把用户往那个路径上引没有任何好处 —— 而当初给用户的
/// 自救建议恰恰是"目标目录改成 `/opt/app`"。
///
/// 应用名只是**存档键 + 默认目录名**，用户随时能改；一个中性的默认值
/// 比一个"看起来像是猜出来的"名字更容易被改掉。
pub const DEFAULT_APP_NAME: &str = "app";
