//! MySQL / MariaDB 容器的**纯逻辑**：识别、命令行拼装、输出解析、SQL 生成、错误分类。
//!
//! 这个模块里**一行进程都不起**（所以能脱离 Windows 单测）—— 真去跑命令的那部分在
// [`crate::cmd::mysql`]。拆法和 `model/distro.rs` ↔ `cmd/distro.rs` 一样。
//!
//! # 为什么走容器里的 `mysql` 客户端，而不是自己连 3306
//!
//! 自连要引一个 MySQL 协议库（`mysql` / `mysql_async`）——本机没有 cargo，
//! 加依赖意味着 `Cargo.lock` 只能靠 `gen-lock.yml` 重生成；而且它还要求
//! 容器**发布了 3306**、还要自己处理 `caching_sha2_password` 与 TLS。
//! 走 `wslc exec` 则是白拿的：不用端口、不用协议、不用认证插件，代价只有两条
//! ——**容器得在运行**、**镜像里得有客户端**。这两条都能给出明确提示
//! （见 [`MysqlErrorKind`]，用的是实测抓下来的报错原文）。
//!
//! # 实测（本机 wslc 3.0.3.0 / mysql 26.7.0，2026-10）
//!
//! | 试的 | 结果 |
//! |---|---|
//! | `wslc exec -e MYSQL_PWD=root mysql mysql -uroot -N -B -e "SHOW DATABASES"` | exit 0，4 行 TSV |
//! | `wslc exec --help` | **有 `-e/--env`**，所以密码能走环境变量、不进 argv |
//! | 客户端在哪 | `/usr/bin/mysql`（另有 `/usr/bin/mysqlsh`） |
//! | stdout 不是 tty 时的默认输出 | **batch（TSV）** —— 所以"给人看的表格"必须显式 `--table` |
//! | `-B` 对一个含 tab 的值 | 输出 `a\tb`（字面反斜杠 + t），反斜杠本身输出成 `\\` |
//! | SQL 里带 `'` `"` `\` 空格 | 经 argv 原样到达（没有 shell，所以不存在 shell 注入面） |
//! | 密码错 | exit 1，`ERROR 1045 … Access denied` |
//! | 容器没运行 | exit 1，`… 未运行。错误代码： WSLC_E_CONTAINER_NOT_RUNNING` |
//! | 镜像里没客户端 | exit **126**，`… exec: "mysql": executable file not found in $PATH` |
//! | `Config.ExposedPorts`（`inspect`） | **是空的** —— 端口只能看 `list` 的 `Labels` 元数据 |
//! | `list` 的 `Command` 字段 | 被截断成 20 字符（`"docker-entrypoint.s…"`），认不出 `mysqld` |

use crate::error::{Error, Result};

use super::{ContainerInspect, ContainerListItem};

// ---------------------------------------------------------------------------
// 识别：这个容器是不是 MySQL
// ---------------------------------------------------------------------------

/// 认出来的镜像家族。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MysqlFamily {
    /// 官方 `mysql` 镜像。
    Mysql,
    /// 官方 `mariadb` 镜像（协议兼容，客户端可能叫 `mariadb`）。
    Mariadb,
    /// Percona Server。
    Percona,
}

impl MysqlFamily {
    /// 中文显示名（界面上标出"这是哪一族"）。
    pub fn label(self) -> &'static str {
        match self {
            Self::Mysql => "MySQL",
            Self::Mariadb => "MariaDB",
            Self::Percona => "Percona Server",
        }
    }

    /// 家族默认端口。
    pub fn default_port(self) -> u16 {
        3306
    }

    /// 兜底的客户端二进制名（探测失败时用）。
    ///
    /// 三个家族**都是 `mysql`**：mariadb 镜像里 `mysql` 是指向 `mariadb` 的
    /// 兼容符号链接，percona 直接就是 mysql 的客户端。
    pub fn fallback_client(self) -> &'static str {
        "mysql"
    }
}

/// 「凭什么认为它是 MySQL」。
///
/// 三种依据的可靠程度**不一样**，所以不合并成一个 `bool`：界面上可以直接
/// 把依据说出来（"按端口 3306 猜的"），用户就知道该不该信。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MysqlSignal {
    /// 镜像名认出来的（最可靠）。
    Image,
    /// 启动命令里有 `mysqld` / `mariadbd`。
    Command,
    /// 发布了 3306 端口（**猜**的 —— 3306 是 IANA 给 MySQL 的端口）。
    Port,
}

impl MysqlSignal {
    /// 给用户看的一句话。
    pub fn label(self) -> &'static str {
        match self {
            Self::Image => "按镜像名判断",
            Self::Command => "按启动命令判断",
            Self::Port => "按 3306 端口判断（可能是别的程序）",
        }
    }
}

/// 一次判定结果：是哪一族 + 依据是什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MysqlCandidate {
    /// 家族。
    pub family: MysqlFamily,
    /// 判定依据。
    pub signal: MysqlSignal,
}

/// 从**列表数据**判断这个容器是不是 MySQL（零成本，渲染期可用）。
///
/// 三层依据，便宜的在前面：
///
/// 1. 镜像名（`Image`）—— 最可靠，且**不受截断影响**；
/// 2. 启动命令（`Command`）—— ⚠️ 实测 `wslc list` 把它截断成 20 字符
///    （`"docker-entrypoint.s…"`），所以官方 mysql 镜像在这条上**认不出来**，
///    只有用户显式把命令写成 `mysqld` 才命中；
/// 3. 发布端口里有 3306（`Labels` 元数据）—— 这条是**猜**，一个恰好发布
///    3306 的非 MySQL 容器会被误判。代价可接受：点进去之后 L2/L3 会给出
///    权威结论（`cmd::mysql::detect`），误判只表现为"多了一个按钮"。
pub fn mysql_container_kind(item: &ContainerListItem) -> Option<MysqlCandidate> {
    if let Some(family) = family_from_image(&item.image) {
        return Some(MysqlCandidate {
            family,
            signal: MysqlSignal::Image,
        });
    }
    if let Some(family) = family_from_command(&item.command_clean()) {
        return Some(MysqlCandidate {
            family,
            signal: MysqlSignal::Command,
        });
    }
    if item
        .wsl_metadata_ports()
        .iter()
        .any(|p| p.container_port == 3306)
    {
        return Some(MysqlCandidate {
            family: MysqlFamily::Mysql,
            signal: MysqlSignal::Port,
        });
    }
    None
}

/// 按镜像名认家族。认不出来返回 `None`（**什么都不做**，和 `image_preset` 同一条原则）。
///
/// 表刻意小：只认最常见的几个。命名带前缀的那种（`docker.1panel.live/library/mysql:8`）
/// 走 [`image_last_segment`] 取最后一段，所以自动命中。
pub fn family_from_image(image: &str) -> Option<MysqlFamily> {
    match image_last_segment(image).as_str() {
        "mysql" | "mysql-server" | "mysql-community-server" => Some(MysqlFamily::Mysql),
        "mariadb" | "mariadb-server" => Some(MysqlFamily::Mariadb),
        "percona" | "percona-server" => Some(MysqlFamily::Percona),
        _ => None,
    }
}

/// 按启动命令认家族（`mysqld` / `mariadbd`）。
pub fn family_from_command(command: &str) -> Option<MysqlFamily> {
    let lower = command.to_ascii_lowercase();
    if lower.contains("mariadbd") {
        Some(MysqlFamily::Mariadb)
    } else if lower.contains("mysqld") {
        // Percona 的进程也叫 mysqld，光看命令分不出来 —— 归 MySQL
        // （两者界面上的行为完全一致，分错也没有后果）。
        Some(MysqlFamily::Mysql)
    } else {
        None
    }
}

/// 取镜像引用的**最后一段**（去掉 registry 前缀、tag、digest），小写。
///
/// ⚠️ 刻意**不用** `registry::split_input`：那个函数在遇到
/// `alpine@sha256:…` 这类**合法**引用时会返回 `Err`（它服务的是"拉取"那条路，
/// 按摘要拉取这一版不支持）。而这里的调用点在渲染路径上 ——
/// 判定失败必须是"认不出来"，不能是"报错"。
fn image_last_segment(image: &str) -> String {
    let without_digest = image.split('@').next().unwrap_or(image);
    let last = without_digest.rsplit('/').next().unwrap_or(without_digest);
    // ⚠️ 只切**最后一段**里的冒号：`localhost:5000/mysql` 的端口在主机名那段，
    // 先 rsplit('/') 之后已经不在 last 里了。
    let last = last.split(':').next().unwrap_or(last);
    last.trim().to_ascii_lowercase()
}

/// 从 `inspect` 的结果判断（**权威判定**，进页面时跑一次）。
///
/// 顺序：镜像引用 → 环境变量 → 命令 / entrypoint → 端口。
/// 都比列表那条启发式强：`inspect` 的字段是**完整**的（没有被截断的 `Command`）。
pub fn family_from_inspect(inspect: &ContainerInspect) -> Option<MysqlFamily> {
    if let Some(family) = inspect.image_ref().and_then(family_from_image) {
        return Some(family);
    }

    // 环境变量：官方入口脚本认的那几个键都是 `MYSQL_` / `MARIADB_` 打头的
    // （`MYSQL_ROOT_PASSWORD` / `MARIADB_ROOT_PASSWORD` / `MYSQL_DATABASE` ……），
    // 所以按**前缀**认就够了 —— 列一张键名表反而会漏掉新版本加的键。
    // mariadb 放前面：它的入口脚本两套名字都认，但用户建的是 mariadb。
    let env = inspect.env();
    let has_prefix = |prefix: &str| env.iter().any(|line| line.starts_with(prefix));
    if has_prefix("MARIADB_") {
        return Some(MysqlFamily::Mariadb);
    }
    if has_prefix("MYSQL_") {
        return Some(MysqlFamily::Mysql);
    }

    // 命令 / entrypoint：这里**没有截断**，所以 `mysqld` 是可靠的。
    let mut line = inspect.cmd().join(" ");
    line.push(' ');
    line.push_str(&inspect.entrypoint().join(" "));
    if let Some(family) = family_from_command(&line) {
        return Some(family);
    }

    // 端口（`inspect` 的 Ports 键形如 `3306/tcp`）。
    if inspect.ports().iter().any(|(key, _)| key.starts_with("3306/")) {
        return Some(MysqlFamily::Mysql);
    }

    None
}

// ---------------------------------------------------------------------------
// 凭据
// ---------------------------------------------------------------------------

/// 连接用的凭据（用户名 / 密码 / 可选的主机端口）。
///
/// # ⚠️ 密码绝不能进日志
///
/// `Debug` 是**手写**的：`AppState` 是 `#[derive(Debug)]`，而 `DbState` 住在
/// 它里面 —— 派生出来的 `Debug` 会在任何一次调试打印（`tracing`、panic 消息、
/// `assert_eq!` 失败输出）里把明文密码打出来。这里打成 `"***"`。
///
/// 同理，密码**不进任何命令行参数**：它只出现在 [`exec_env`] 里
/// （`wslc exec -e MYSQL_PWD=…`），而错误消息里的参数串一律先过
/// [`redact_args`]。
#[derive(Clone, PartialEq, Eq)]
pub struct MysqlCreds {
    /// 用户名（面板默认填 `root`）。
    pub user: String,
    /// 密码；空串是**合法**值（`MYSQL_ALLOW_EMPTY_PASSWORD=yes` 那种）。
    pub password: String,
    /// 主机；`None` = 让客户端自己决定（容器里跑就是 unix socket）。
    pub host: Option<String>,
    /// 端口；`None` = 客户端默认。
    pub port: Option<u16>,
}

impl std::fmt::Debug for MysqlCreds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MysqlCreds")
            .field("user", &self.user)
            // 只说"有没有"，不说是什么
            .field(
                "password",
                &if self.password.is_empty() {
                    "<empty>"
                } else {
                    "***"
                },
            )
            .field("host", &self.host)
            .field("port", &self.port)
            .finish()
    }
}

impl MysqlCreds {
    /// 用 root + 密码建一份。
    pub fn root(password: impl Into<String>) -> Self {
        Self {
            user: "root".to_owned(),
            password: password.into(),
            host: None,
            port: None,
        }
    }

    /// 用户名 / 主机对（错误提示里用它说清"是谁连不上"）。
    pub fn user_label(&self) -> String {
        format!("{}@{}", self.user, self.host.as_deref().unwrap_or("localhost"))
    }
}

/// 从容器环境变量里读出来的 root 凭据线索。
///
/// 这是"用户不用手打密码"的来源：`inspect` 的 `Config.Env` 里就有
/// `MYSQL_ROOT_PASSWORD`（**实测**本机那个 mysql 容器就是 `MYSQL_ROOT_PASSWORD=root`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootCredHint {
    /// 环境变量里直接写着 root 密码。
    Password(String),
    /// 镜像允许空密码（`MYSQL_ALLOW_EMPTY_PASSWORD=yes` 之类）。
    Empty,
    /// 镜像生成了随机密码，写在容器日志里 —— 我们读不到，只能让用户自己去看日志。
    Random,
    /// 什么都没有（用户得自己填）。
    Unknown,
}

impl RootCredHint {
    /// 给用户看的一句话（`None` = 不用说什么）。
    pub fn note(&self) -> Option<String> {
        match self {
            Self::Password(_) => Some(
                "已从容器环境变量（MYSQL_ROOT_PASSWORD）读出 root 密码，直接点「连接」即可。\
                 密码只存在内存里，关掉面板就没了。"
                    .to_owned(),
            ),
            Self::Empty => {
                Some("容器允许 root 空密码（MYSQL_ALLOW_EMPTY_PASSWORD），密码留空即可。".to_owned())
            }
            Self::Random => Some(
                "这个容器用的是随机 root 密码（MYSQL_RANDOM_ROOT_PASSWORD）—— 它只写在\
                 容器日志里。请先到「日志」里找 \u{201c}GENERATED ROOT PASSWORD\u{201d} 那一行，再填到这里。"
                    .to_owned(),
            ),
            Self::Unknown => None,
        }
    }
}

/// 从 `Config.Env`（形如 `KEY=VALUE` 的字符串数组）里找 root 凭据线索。
///
/// 优先级：**明确的密码** > 空密码 > 随机密码 —— 都设了的话，
/// 入口脚本认的是明确的那个（实测的容器就是 `MYSQL_ROOT_PASSWORD=root`）。
pub fn root_credential_from_env(env: &[String]) -> RootCredHint {
    let value = |key: &str| -> Option<String> {
        env.iter()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
            .map(|v| v.trim().to_owned())
    };
    let truthy = |key: &str| -> bool {
        value(key).is_some_and(|v| {
            let v = v.to_ascii_lowercase();
            matches!(v.as_str(), "yes" | "y" | "true" | "1" | "on")
        })
    };

    for key in ["MYSQL_ROOT_PASSWORD", "MARIADB_ROOT_PASSWORD"] {
        if let Some(v) = value(key) {
            return if v.is_empty() {
                RootCredHint::Empty
            } else {
                RootCredHint::Password(v)
            };
        }
    }
    for key in [
        "MYSQL_ALLOW_EMPTY_PASSWORD",
        "MARIADB_ALLOW_EMPTY_ROOT_PASSWORD",
    ] {
        if truthy(key) {
            return RootCredHint::Empty;
        }
    }
    for key in ["MYSQL_RANDOM_ROOT_PASSWORD", "MARIADB_RANDOM_ROOT_PASSWORD"] {
        if truthy(key) {
            return RootCredHint::Random;
        }
    }
    RootCredHint::Unknown
}

// ---------------------------------------------------------------------------
// 命令行拼装
// ---------------------------------------------------------------------------

/// 输出模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqlMode {
    /// 给**机器解析**用：`-N`（不要表头）+ `-B`（tab 分隔）。
    Batch,
    /// 给**人看**用：`--table`（ASCII 表格）。
    ///
    /// ⚠️ `--table` 必须显式给：stdout 不是 tty 时 `mysql` **默认就是 batch**
    /// （实测：不加参数时输出的是 `a\tb` 而不是带框的表格）。
    Table,
}

impl SqlMode {
    /// 对应的开关。
    pub fn flags(self) -> &'static [&'static str] {
        match self {
            Self::Batch => &["-N", "-B"],
            Self::Table => &["--table"],
        }
    }
}

/// 拼 `mysql` 客户端的参数（**不含程序名之前的 `wslc exec` 前缀**）。
///
/// # 不变量（每一条都有代价）
///
/// 1. **`-e <SQL>` 必须在最后** —— `wslc` 的位置参数解析一旦遇到非选项就只认
///    位置参数了（`cmd::container::logs_args` 那条注释同理）；
/// 2. **永不出现 `-p`** —— 密码走 [`exec_env`]（`MYSQL_PWD`）。`-p<密码>` 会
///    出现在容器内的进程列表里，而 `-e KEY=VALUE` 不会；
/// 3. 库名用 `-D` 传，**不拼进 SQL** —— 省掉一次字符串拼接就省掉一类拼错；
/// 4. `-N -B` / `--table` 由 [`SqlMode`] 决定，调用方不要自己加。
pub fn client_args(
    bin: &str,
    creds: &MysqlCreds,
    sql: &str,
    database: Option<&str>,
    mode: SqlMode,
) -> Vec<String> {
    let mut args: Vec<String> = vec![bin.to_owned()];

    args.push("-u".to_owned());
    args.push(creds.user.clone());
    if let Some(host) = &creds.host {
        args.push("-h".to_owned());
        args.push(host.clone());
    }
    if let Some(port) = creds.port {
        args.push("-P".to_owned());
        args.push(port.to_string());
    }

    args.extend(mode.flags().iter().map(|f| (*f).to_owned()));

    if let Some(db) = database {
        args.push("-D".to_owned());
        args.push(db.to_owned());
    }

    args.push("-e".to_owned());
    args.push(sql.to_owned());
    args
}

/// 密码那一条环境变量（`wslc exec -e` 的值）。
///
/// ⚠️ 这是**唯一**放密码的地方。凡是把这个字符串交给错误消息 / 日志的地方，
/// 都必须先过 [`redact_args`]。
pub fn exec_env(creds: &MysqlCreds) -> String {
    format!("MYSQL_PWD={}", creds.password)
}

// ---------------------------------------------------------------------------
// 脱敏
// ---------------------------------------------------------------------------

/// 把一个参数里的**密码形状**打成 `***`。
///
/// 认这三种形状（都是 mysql 客户端 / 我们自己会写出来的）：
/// 含 `PASSWORD`/`PWD` 的 `KEY=VALUE`、`--password=…`、`-p<值>`。
fn redact_one(arg: &str) -> String {
    if let Some((key, _)) = arg.split_once('=') {
        let upper = key.to_ascii_uppercase();
        if upper.ends_with("PASSWORD") || upper.ends_with("PWD") {
            return format!("{key}=***");
        }
    }
    if arg.starts_with("-p") && arg.len() > 2 {
        return "-p***".to_owned();
    }
    arg.to_owned()
}

/// 脱敏一整串参数（**错误消息的唯一出口**）。
///
/// # 为什么必须有它
///
/// [crate::error::Error::NonZeroExit] 的 `Display` 会把 `args` **拼进消息**：
/// `wslc exec -e MYSQL_PWD=root mysql … 返回退出码 1：…`。
/// 不脱敏的话，任何一次 SQL 报错都会把明文密码送到提示条和日志里。
pub fn redact_args(args: &[String]) -> Vec<String> {
    args.iter().map(|a| redact_one(a)).collect()
}

/// 把 `text` 里出现的 `password` 原文换成 `***`。
///
/// 这是给**子进程输出**用的（`stderr`）：`CREATE USER … IDENTIFIED BY 'x'`
/// 出错时，MySQL 会把**整条语句**回显在错误里（"near 'x'' at line 1"），
/// 于是在那边按"形状"脱敏是不够的 —— 得按**值**替换。
///
/// ⚠️ 密码为空串时**原样返回**：否则空串会把整段文本拆得面目全非
/// （`str::replace` 对空 pattern 会在每个字符边界插入）。
pub fn redact_password(text: &str, password: &str) -> String {
    if password.is_empty() {
        return text.to_owned();
    }
    text.replace(password, "***")
}

// ---------------------------------------------------------------------------
// 输出解析
// ---------------------------------------------------------------------------

/// 还原 `-B`（batch）模式的转义。
///
/// 实测（`SELECT 'a\tb', 'back\\slash'`）：`-B` 下 tab 输出成**字面的
/// `\t`**（反斜杠 + t），反斜杠输出成 `\\`。所以按 tab 切完字段之后必须还原，
/// 否则库名 / 用户名里带这些字符时会错位。
///
/// 未知转义（`\x`）**原样保留**：不去猜 MySQL 的版本差异，猜错比不猜更糟。
pub fn unescape_batch_field(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('0') => out.push('\0'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// 一行 TSV → 字段列表（**未转义的 tab 才是分隔符**）。
pub fn split_tsv_line(line: &str) -> Vec<String> {
    line.split('\t').map(unescape_batch_field).collect()
}

/// `information_schema.SCHEMATA` 一行（5 列）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbEntry {
    /// 库名。
    pub name: String,
    /// 默认字符集。
    pub charset: String,
    /// 默认排序规则。
    pub collation: String,
    /// 表数量。
    pub tables: u64,
    /// 数据 + 索引占用（字节）。
    pub bytes: u64,
}

impl DbEntry {
    /// 是不是 MySQL 自带的系统库。
    ///
    /// 界面上给它们一个「系统」标记：**不隐藏**（它们是真实存在的库，
    /// 而且 `sys` / `performance_schema` 偶尔真要去看），但得让用户一眼知道
    /// 「这不是我建的、删了会出事」。
    pub fn is_system(&self) -> bool {
        matches!(
            self.name.as_str(),
            "information_schema" | "mysql" | "performance_schema" | "sys"
        )
    }
}

/// 列出所有库（**一次** exec 拿全部信息，不是每个库一条命令）。
///
/// 列：库名 / 字符集 / 排序规则 / 表数 / 占用。
pub fn list_databases_sql() -> &'static str {
    "SELECT s.SCHEMA_NAME, s.DEFAULT_CHARACTER_SET_NAME, s.DEFAULT_COLLATION_NAME, \
     COALESCE((SELECT COUNT(*) FROM information_schema.TABLES t \
     WHERE t.TABLE_SCHEMA = s.SCHEMA_NAME), 0), \
     COALESCE((SELECT SUM(t.DATA_LENGTH + t.INDEX_LENGTH) FROM information_schema.TABLES t \
     WHERE t.TABLE_SCHEMA = s.SCHEMA_NAME), 0) \
     FROM information_schema.SCHEMATA s ORDER BY s.SCHEMA_NAME"
}

/// 列出所有用户。
///
/// 列：User / Host / plugin / Super_priv。
///
/// ⚠️ `mysql.user` 只有**高权限账号**读得到；普通账号会拿到
/// `ERROR 1142 … SELECT command denied`（[`MysqlErrorKind::PermissionDenied`]）。
/// 这不是 bug，所以界面上要把它说清楚，而不是显示成"读取失败"。
pub fn list_users_sql() -> &'static str {
    "SELECT User, Host, plugin, Super_priv FROM mysql.user ORDER BY User, Host"
}

/// 解析 [`list_databases_sql`] 的 `-N -B` 输出。
///
/// 列数不足的**坏行直接跳过**（这一层的设计目标：解析失败绝不 panic）。
pub fn parse_databases(tsv: &str) -> Vec<DbEntry> {
    tsv.lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| {
            let fields = split_tsv_line(line);
            if fields.len() < 5 {
                return None;
            }
            Some(DbEntry {
                name: fields[0].clone(),
                charset: fields[1].clone(),
                collation: fields[2].clone(),
                // 数字列解析不了就当 0：显示成 0 比整行消失好
                tables: fields[3].trim().parse().unwrap_or(0),
                bytes: fields[4].trim().parse().unwrap_or(0),
            })
        })
        .collect()
}

/// `mysql.user` 的一行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbUser {
    /// 用户名。
    pub user: String,
    /// 主机（`%` / `localhost` / IP …）。**同一个用户名可以有多个 host**，
    /// 所以它是身份的一部分，不能省。
    pub host: String,
    /// 认证插件（`caching_sha2_password` / `mysql_native_password` …）。
    pub plugin: String,
    /// 是否超级权限（`Super_priv = Y`）。
    pub super_priv: bool,
}

impl DbUser {
    /// `'root'@'%'` 形式（和 SQL 里的写法一致，用户一眼能对上）。
    pub fn account(&self) -> String {
        format!("'{}'@'{}'", self.user, self.host)
    }

    /// 是不是 MySQL 自带的内部账号（`mysql.sys` / `mysql.session` / `mysql.infoschema`）。
    pub fn is_system(&self) -> bool {
        matches!(
            self.user.as_str(),
            "mysql.sys" | "mysql.session" | "mysql.infoschema"
        )
    }
}

/// 解析 [`list_users_sql`] 的 `-N -B` 输出。
pub fn parse_users(tsv: &str) -> Vec<DbUser> {
    tsv.lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| {
            let fields = split_tsv_line(line);
            if fields.len() < 4 {
                return None;
            }
            Some(DbUser {
                user: fields[0].clone(),
                host: fields[1].clone(),
                plugin: fields[2].clone(),
                super_priv: fields[3].trim().eq_ignore_ascii_case("y"),
            })
        })
        .collect()
}

/// 客户端探测的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientProbe {
    /// 二进制名（`mysql` / `mariadb` / `mysqlsh`）—— 直接拿去拼命令行。
    pub bin: String,
    /// 完整路径（界面上显示"用的是哪个客户端"）。
    pub path: String,
    /// `--version` 的第一行。
    pub version: Option<String>,
}

/// 探测脚本：找到客户端并打印它的路径与版本。
///
/// `command -v a || command -v b` 的短路顺序就是**优先级**；
/// 一个都没有时 `exit 127`（并让 `wslc exec` 以非零退出，我们才能给出
/// "镜像里没有客户端"的提示）。
///
/// 这一段交给容器里的 `/bin/sh` 执行 —— 参数是**整串**传进去的（不是我们在
/// Windows 这边拼 shell 命令），所以本机不会多一层 shell 解析。
pub fn client_probe_script() -> &'static str {
    "c=$(command -v mysql || command -v mariadb || command -v mysqlsh) || exit 127; \
     echo \"$c\"; exec \"$c\" --version"
}

/// 解析 [`client_probe_script`] 的输出（第一行路径、第二行版本）。
pub fn parse_client_probe(stdout: &str) -> Option<ClientProbe> {
    let mut lines = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let path = lines.next()?.to_owned();
    let bin = path.rsplit('/').next().unwrap_or(&path).trim().to_owned();
    if bin.is_empty() {
        return None;
    }
    Some(ClientProbe {
        bin,
        path,
        version: lines.next().map(str::to_owned),
    })
}

// ---------------------------------------------------------------------------
// SQL 生成
// ---------------------------------------------------------------------------

/// 校验 MySQL **标识符**（库名 / 用户名 / 字符集名）。
///
/// # 为什么是白名单，而不是"转义后拼接"
///
/// 用户输入会直接进 SQL。转义要处理的是"哪种引号、反斜杠、`NO_BACKSLASH_ESCAPES`
/// 模式、多字节编码"，每一条都能写错；白名单只有一条规则：**只有
/// `[A-Za-z0-9_$]` 且不超过 64 字节才算合法**。不合法的**拒绝**（把选择权交回用户），
/// 而不是我们猜他的意图。
///
/// ⚠️ 通过校验之后**仍然**用反引号包起来（`ORDER` 这种保留字做库名是合法的，
/// 不包就变成语法错误）。
pub fn validate_ident(kind: &str, name: &str) -> Result<()> {
    let name = name.trim();
    if name.is_empty() {
        return Err(Error::InvalidArgument(format!("{kind}不能为空")));
    }
    if name.len() > 64 {
        return Err(Error::InvalidArgument(format!(
            "{kind}不能超过 64 个字符（MySQL 的限制），当前 {} 个",
            name.len()
        )));
    }
    if let Some(bad) = name
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || *c == '_' || *c == '$'))
    {
        return Err(Error::InvalidArgument(format!(
            "{kind}「{name}」里有不支持的字符 `{bad}` —— 只允许字母、数字、下划线和 $"
        )));
    }
    Ok(())
}

/// 校验 MySQL 的 **host** 部分（`'user'@'host'` 里那个 host）。
///
/// 比标识符宽松：可以是 `%`、`192.168.1.%`、`localhost`、域名。
pub fn validate_host(host: &str) -> Result<()> {
    let host = host.trim();
    if host.is_empty() {
        return Err(Error::InvalidArgument("主机不能为空（本机请填 localhost）".to_owned()));
    }
    if host.len() > 255 {
        return Err(Error::InvalidArgument("主机名过长".to_owned()));
    }
    if let Some(bad) = host.chars().find(|c| {
        !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '%' | ':' | '*'))
    }) {
        return Err(Error::InvalidArgument(format!(
            "主机「{host}」里有不支持的字符 `{bad}`"
        )));
    }
    Ok(())
}

/// 校验字符集 / 排序规则名（`utf8mb4` / `utf8mb4_0900_ai_ci`）。
pub fn validate_charset(kind: &str, value: &str) -> Result<()> {
    let value = value.trim();
    if value.is_empty() {
        return Err(Error::InvalidArgument(format!("{kind}不能为空")));
    }
    if let Some(bad) = value
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || *c == '_'))
    {
        return Err(Error::InvalidArgument(format!(
            "{kind}「{value}」里有不支持的字符 `{bad}`"
        )));
    }
    Ok(())
}

/// 把一段文本变成 MySQL 的**单引号字符串字面量**（含两侧引号）。
///
/// 只做两件事：反斜杠 → `\\`、单引号 → `\'`；另外把双引号也转掉
/// （`"` 在 MySQL 里只有开了 `ANSI_QUOTES` 才有特殊含义，转掉它没有任何代价，
/// 而它正好会让**拼给 `wslc.exe` 的整条命令行**更容易被 Windows 的参数规则
/// 弄坏 —— 实际测过带引号的 SQL 能原样到达，见模块文档那张表）。
///
/// 用在**密码**上（密码允许任意字符，所以这里必须转义，不能像标识符那样白名单）。
pub fn quote_literal(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '"' => out.push_str("\\\""),
            _ => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// 反引号包起来的标识符（**必须先过 [`validate_ident`]**）。
fn ident(name: &str) -> String {
    format!("`{}`", name.trim())
}

/// `CREATE DATABASE`。
///
/// 字符集 / 排序规则留空就交给服务器的默认值（**不替用户猜**：
/// 猜一个 `utf8mb4` 看起来贴心，实际上是替他做了一个他看不见的决定）。
pub fn create_database_sql(
    name: &str,
    charset: Option<&str>,
    collation: Option<&str>,
) -> Result<String> {
    validate_ident("库名", name)?;
    let mut sql = format!("CREATE DATABASE {}", ident(name));
    if let Some(charset) = charset.map(str::trim).filter(|s| !s.is_empty()) {
        validate_charset("字符集", charset)?;
        sql.push_str(&format!(" DEFAULT CHARACTER SET {charset}"));
    }
    if let Some(collation) = collation.map(str::trim).filter(|s| !s.is_empty()) {
        validate_charset("排序规则", collation)?;
        sql.push_str(&format!(" DEFAULT COLLATE {collation}"));
    }
    Ok(sql)
}

/// `DROP DATABASE`（**不加** `IF EXISTS`：用户是确认过才点的，
/// 库要是不在了，报一句 `ERROR 1008` 比"静默成功"诚实）。
pub fn drop_database_sql(name: &str) -> Result<String> {
    validate_ident("库名", name)?;
    Ok(format!("DROP DATABASE {}", ident(name)))
}

/// `CREATE USER`（`IF NOT EXISTS`：同名同 host 的账号已存在时报错没有任何价值）。
pub fn create_user_sql(user: &str, host: &str, password: &str) -> Result<String> {
    validate_ident("用户名", user)?;
    validate_host(host)?;
    Ok(format!(
        "CREATE USER IF NOT EXISTS '{}'@'{}' IDENTIFIED BY {}",
        user.trim(),
        host.trim(),
        quote_literal(password)
    ))
}

/// `ALTER USER … IDENTIFIED BY`（改密码）。
pub fn alter_user_password_sql(user: &str, host: &str, password: &str) -> Result<String> {
    validate_ident("用户名", user)?;
    validate_host(host)?;
    Ok(format!(
        "ALTER USER '{}'@'{}' IDENTIFIED BY {}",
        user.trim(),
        host.trim(),
        quote_literal(password)
    ))
}

/// 删除用户（`IF EXISTS`：删两次不该报错，用户要的结果是"它不在了"）。
pub fn drop_user_sql(user: &str, host: &str) -> Result<String> {
    validate_ident("用户名", user)?;
    validate_host(host)?;
    Ok(format!("DROP USER IF EXISTS '{}'@'{}'", user.trim(), host.trim()))
}

/// 授权级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantKind {
    /// 全部权限（等价于面板里最常见的那一下）。
    All,
    /// 只读。
    ReadOnly,
    /// 读写（不改结构、不授权）。
    ReadWrite,
}

impl GrantKind {
    /// 全部可选值（界面上按这个顺序排按钮）。
    pub const ALL: [GrantKind; 3] = [Self::All, Self::ReadOnly, Self::ReadWrite];

    /// 中文标签。
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "全部权限",
            Self::ReadOnly => "只读（SELECT）",
            Self::ReadWrite => "读写（增删改查）",
        }
    }

    /// 给 `GRANT` 用的权限串。
    pub fn privileges(self) -> &'static str {
        match self {
            Self::All => "ALL PRIVILEGES",
            Self::ReadOnly => "SELECT",
            Self::ReadWrite => "SELECT, INSERT, UPDATE, DELETE",
        }
    }
}

/// `GRANT <权限> ON <库>.* TO 'user'@'host'`。
///
/// MySQL 8 起 `GRANT` 会自己刷新权限表，**不需要** `FLUSH PRIVILEGES`。
pub fn grant_sql(database: &str, user: &str, host: &str, kind: GrantKind) -> Result<String> {
    validate_ident("库名", database)?;
    validate_ident("用户名", user)?;
    validate_host(host)?;
    Ok(format!(
        "GRANT {} ON {}.* TO '{}'@'{}'",
        kind.privileges(),
        ident(database),
        user.trim(),
        host.trim()
    ))
}

/// 建用户 + （可选）授权，合成**一条** `-e` 语句（用 `;` 分隔，实测能跑）。
///
/// 合成而不是跑两次：少一次 `wslc exec`（每次几百毫秒），而且**原子性更好** ——
/// 授权失败时不会留下一个"建好了但没权限"的账号让用户以为哪一步没生效
/// （MySQL 的 DDL 不回滚，但至少错误是一起报出来的）。
pub fn create_user_statements(
    user: &str,
    host: &str,
    password: &str,
    grant: Option<(&str, GrantKind)>,
) -> Result<String> {
    let mut sql = create_user_sql(user, host, password)?;
    if let Some((database, kind)) = grant {
        sql.push_str("; ");
        sql.push_str(&grant_sql(database, user, host, kind)?);
    }
    Ok(sql)
}

// ---------------------------------------------------------------------------
// 错误分类
// ---------------------------------------------------------------------------

/// 子进程报错大致是哪一类。
///
/// 判据全部来自**实测抓下来的原文**（`tests/fixtures/mysql_error_*.txt`），
/// 不靠猜。分类的意义是给出**下一步**：光把
/// `OCI runtime exec failed: …` 甩给用户，他不知道该干什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MysqlErrorKind {
    /// 容器没在运行（`WSLC_E_CONTAINER_NOT_RUNNING`）。
    ContainerNotRunning,
    /// 镜像里没有 mysql 客户端（exit 126 + `executable file not found`）。
    ClientMissing,
    /// 用户名或密码不对（`ERROR 1045`）。
    AccessDenied,
    /// mysqld 还没起来 / 连不上（`ERROR 2002` / `2003`）——
    /// 第一次初始化要十几秒，这是**等一会儿就好**的情况。
    ServerNotReady,
    /// 权限不足（`ERROR 1142` 之类）—— 典型是普通账号去读 `mysql.user`。
    PermissionDenied,
    /// 库不存在（`ERROR 1049` / `1008`）。
    UnknownDatabase,
    /// 其它 SQL 错误（语法、约束……）。原文照给。
    SqlError,
    /// 认不出来。
    Unknown,
}

impl MysqlErrorKind {
    /// 给用户的一句话建议（`None` = 直接看原文就行）。
    pub fn hint(self) -> Option<&'static str> {
        match self {
            Self::ContainerNotRunning => Some(
                "容器没在运行 —— 先到「容器」页把它启动起来，再回来点「重新读取」。",
            ),
            Self::ClientMissing => Some(
                "这个镜像里没有 mysql / mariadb 客户端，面板没法在里面执行 SQL。\
                 请换官方 mysql / mariadb 镜像，或者用「终端」自己进去装一个。",
            ),
            Self::AccessDenied => {
                Some("用户名或密码不对。密码可以在容器详情里核对，或者看容器日志里那行临时密码。")
            }
            Self::ServerNotReady => Some(
                "mysqld 还没准备好（第一次初始化要十几秒）。等几秒再点一次「重新读取」。",
            ),
            Self::PermissionDenied => {
                Some("当前账号权限不够（比如读 mysql.user 需要高权限账号）。换个账号试试。")
            }
            Self::UnknownDatabase => Some("这个库不存在 —— 可能是名字打错了，或者已经被删掉了。"),
            Self::SqlError | Self::Unknown => None,
        }
    }

    /// 要不要在界面上显示成「错误」（而不是"稍等"）。
    ///
    /// `ServerNotReady` 是**暂时**的，界面上用提醒色更合适。
    pub fn is_transient(self) -> bool {
        matches!(self, Self::ServerNotReady)
    }
}

/// 按**报错原文**（和退出码）分类。
///
/// ⚠️ 退出码**不能**单独作为判据：实测三种情况分别是 1（密码错）、
/// 1（容器没运行）、**126**（没有客户端），而 126 只是"命令没找到"的约定，
/// 真正稳的是那些文本（`WSLC_E_CONTAINER_NOT_RUNNING` / `executable file not found`
/// 是英文的稳定标识，不会随系统语言变）。
pub fn classify_error(text: &str, code: Option<i32>) -> MysqlErrorKind {
    let lower = text.to_ascii_lowercase();

    if text.contains("WSLC_E_CONTAINER_NOT_RUNNING") {
        return MysqlErrorKind::ContainerNotRunning;
    }
    if lower.contains("executable file not found")
        || lower.contains("no such file or directory")
        || code == Some(126)
        || code == Some(127)
    {
        return MysqlErrorKind::ClientMissing;
    }
    if lower.contains("error 1045") || lower.contains("access denied") {
        return MysqlErrorKind::AccessDenied;
    }
    if lower.contains("error 2002")
        || lower.contains("error 2003")
        || lower.contains("can't connect")
        || lower.contains("connection refused")
    {
        return MysqlErrorKind::ServerNotReady;
    }
    if lower.contains("error 1142")
        || lower.contains("error 1227")
        || lower.contains("command denied")
    {
        return MysqlErrorKind::PermissionDenied;
    }
    if lower.contains("error 1049") || lower.contains("unknown database") {
        return MysqlErrorKind::UnknownDatabase;
    }
    if lower.contains("error ") {
        return MysqlErrorKind::SqlError;
    }
    MysqlErrorKind::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ContainerListItem;
    use serde_json::json;

    /// 实测抓下来的夹具。
    const DATABASES_TSV: &str = include_str!("../../tests/fixtures/mysql_databases.tsv");
    const USERS_TSV: &str = include_str!("../../tests/fixtures/mysql_users.tsv");
    const CLIENT_PROBE: &str = include_str!("../../tests/fixtures/mysql_client_probe.txt");
    const ERR_ACCESS_DENIED: &str =
        include_str!("../../tests/fixtures/mysql_error_access_denied.txt");
    const ERR_NOT_RUNNING: &str = include_str!("../../tests/fixtures/mysql_error_not_running.txt");
    const ERR_NO_CLIENT: &str = include_str!("../../tests/fixtures/mysql_error_no_client.txt");

    fn item(image: &str, command: &str, labels: &str) -> ContainerListItem {
        ContainerListItem {
            image: image.to_owned(),
            command: command.to_owned(),
            labels: labels.to_owned(),
            ..Default::default()
        }
    }

    /// 实机那条 `wslc list -a --format json` 的 Labels（端口 3306）。
    fn real_mysql_labels() -> String {
        r#"com.microsoft.wsl.container.metadata={"V1":{"Flags":0,"Image":"docker.1panel.live/library/mysql:latest","InitProcessFlags":0,"Ports":[{"BindingAddress":"127.0.0.1","ContainerPort":3306,"Family":2,"HostPort":3306,"Protocol":6,"VmPort":20003},{"BindingAddress":"127.0.0.1","ContainerPort":33060,"Family":2,"HostPort":33060,"Protocol":6,"VmPort":20004}],"Volumes":[]}}"#
            .to_owned()
    }

    // -- 识别 ---------------------------------------------------------------

    #[test]
    fn recognises_the_real_mysql_container_by_image() {
        // 实机抓的那一行（Command 被 wslc 截断成 20 字符，所以认出来的
        // 只能是**镜像名**这条依据）
        let it = item(
            "docker.1panel.live/library/mysql:latest",
            "\"docker-entrypoint.s…\"",
            &real_mysql_labels(),
        );
        let found = mysql_container_kind(&it).expect("应认出 mysql");
        assert_eq!(found.family, MysqlFamily::Mysql);
        assert_eq!(found.signal, MysqlSignal::Image);
    }

    #[test]
    fn image_segment_handles_registry_tag_digest_and_case() {
        assert_eq!(family_from_image("mysql"), Some(MysqlFamily::Mysql));
        assert_eq!(
            family_from_image("docker.1ms.run/library/mysql:8.0"),
            Some(MysqlFamily::Mysql)
        );
        assert_eq!(family_from_image("MariaDB:11"), Some(MysqlFamily::Mariadb));
        // ⚠️ 摘要形式：`registry::split_input` 对它会返回 Err，这里必须还能认
        assert_eq!(
            family_from_image("mysql@sha256:abcdef"),
            Some(MysqlFamily::Mysql)
        );
        assert_eq!(
            family_from_image("localhost:5000/percona-server:8"),
            Some(MysqlFamily::Percona)
        );
        // 认不出来 → 什么都不做
        assert_eq!(family_from_image("nginx:latest"), None);
        assert_eq!(family_from_image("postgres:16"), None);
        assert_eq!(family_from_image(""), None);
    }

    #[test]
    fn recognises_by_command_when_untouched() {
        let it = item("some.internal/mydb:1", "\"mysqld --defaults-file=/etc/my.cnf\"", "");
        let found = mysql_container_kind(&it).expect("应按命令认出来");
        assert_eq!(found.signal, MysqlSignal::Command);
        assert_eq!(found.family, MysqlFamily::Mysql);

        assert_eq!(
            family_from_command("docker-entrypoint.sh mariadbd"),
            Some(MysqlFamily::Mariadb)
        );
    }

    #[test]
    fn recognises_by_port_as_last_resort() {
        let it = item("mycompany/analytics:1", "", &real_mysql_labels());
        let found = mysql_container_kind(&it).expect("应按端口猜出来");
        assert_eq!(found.signal, MysqlSignal::Port);
        // 依据要能说出来 —— 这条是**猜**的，界面得让用户知道
        assert!(found.signal.label().contains("3306"));
    }

    #[test]
    fn plain_container_is_not_mysql() {
        let it = item(
            "docker.1panel.live/library/nginx:latest",
            "\"/docker-entrypoint.…\"",
            "",
        );
        assert_eq!(mysql_container_kind(&it), None);
    }

    #[test]
    fn inspect_finds_mysql_via_env_cmd_and_ports() {
        // 实机 inspect 的关键字段（Config.Env 里有 MYSQL_ROOT_PASSWORD）
        let insp = ContainerInspect::new(json!({
            "Config": {
                "Image": "docker.1panel.live/library/mysql:latest",
                "Env": ["MYSQL_ROOT_PASSWORD=root", "PATH=/usr/bin"],
                "Cmd": ["mysqld"],
                "Entrypoint": ["docker-entrypoint.sh"]
            }
        }));
        assert_eq!(family_from_inspect(&insp), Some(MysqlFamily::Mysql));
    }

    #[test]
    fn inspect_falls_back_to_env_then_command_then_port() {
        // 镜像名认不出来，但环境变量是 mariadb 那一套
        let mariadb = ContainerInspect::new(json!({
            "Config": { "Image": "myco/db:1", "Env": ["MARIADB_ROOT_PASSWORD=x"] }
        }));
        assert_eq!(family_from_inspect(&mariadb), Some(MysqlFamily::Mariadb));

        // 只有命令
        let by_cmd = ContainerInspect::new(json!({
            "Config": { "Image": "myco/db:1", "Cmd": ["mysqld"] }
        }));
        assert_eq!(family_from_inspect(&by_cmd), Some(MysqlFamily::Mysql));

        // 只有端口（`inspect` 的 Ports 键是 `3306/tcp`）
        let by_port = ContainerInspect::new(json!({
            "Config": { "Image": "myco/db:1" },
            "Ports": { "3306/tcp": [] }
        }));
        assert_eq!(family_from_inspect(&by_port), Some(MysqlFamily::Mysql));

        // 什么线索都没有
        let none = ContainerInspect::new(json!({ "Config": { "Image": "myco/app:1" } }));
        assert_eq!(family_from_inspect(&none), None);
    }

    // -- 凭据 ---------------------------------------------------------------

    #[test]
    fn creds_debug_never_shows_the_password() {
        // ⚠️ 这条是"密码不进日志"的守门测试：`AppState` 是 `#[derive(Debug)]`，
        // 它会一路打出 `DbState` 和里面的 `MysqlCreds`。
        let creds = MysqlCreds::root("s3cret-pw");
        let text = format!("{creds:?}");
        assert!(!text.contains("s3cret-pw"), "{text}");
        assert!(text.contains("***"), "{text}");
        assert!(text.contains("root"), "{text}");
    }

    #[test]
    fn reads_root_credential_from_the_real_container_env() {
        let env = vec![
            "MYSQL_ROOT_PASSWORD=root".to_owned(),
            "PATH=/usr/local/sbin:/usr/local/bin".to_owned(),
            "MYSQL_MAJOR=innovation".to_owned(),
        ];
        assert_eq!(
            root_credential_from_env(&env),
            RootCredHint::Password("root".to_owned())
        );
        assert!(root_credential_from_env(&env).note().is_some());
    }

    #[test]
    fn root_credential_covers_empty_random_and_unknown() {
        assert_eq!(
            root_credential_from_env(&["MYSQL_ALLOW_EMPTY_PASSWORD=yes".to_owned()]),
            RootCredHint::Empty
        );
        assert_eq!(
            root_credential_from_env(&["MYSQL_RANDOM_ROOT_PASSWORD=1".to_owned()]),
            RootCredHint::Random
        );
        assert_eq!(
            root_credential_from_env(&["PATH=/usr/bin".to_owned()]),
            RootCredHint::Unknown
        );
        // 同一组里既有空密码标记又有明确密码 → 明确的那个说了算
        assert_eq!(
            root_credential_from_env(&[
                "MYSQL_ALLOW_EMPTY_PASSWORD=yes".to_owned(),
                "MYSQL_ROOT_PASSWORD=real".to_owned(),
            ]),
            RootCredHint::Password("real".to_owned())
        );
        // 空值的 ROOT_PASSWORD 不算"有密码"
        assert_eq!(
            root_credential_from_env(&["MYSQL_ROOT_PASSWORD=".to_owned()]),
            RootCredHint::Empty
        );
    }

    // -- 命令行 -------------------------------------------------------------

    #[test]
    fn client_args_put_the_sql_last_and_never_a_password_flag() {
        let creds = MysqlCreds::root("pw");
        let args = client_args("mysql", &creds, "SHOW DATABASES", None, SqlMode::Batch);
        assert_eq!(
            args,
            vec!["mysql", "-u", "root", "-N", "-B", "-e", "SHOW DATABASES"]
        );
        assert_eq!(args.last().map(String::as_str), Some("SHOW DATABASES"));
        // 密码**不在**参数里 —— 只走 `exec_env`
        assert!(
            !args.iter().any(|a| a.contains("pw")),
            "参数里不该出现密码：{args:?}"
        );
    }

    #[test]
    fn client_args_table_mode_and_database_flag() {
        let creds = MysqlCreds::root("");
        let args = client_args(
            "mariadb",
            &creds,
            "SELECT 1",
            Some("shop"),
            SqlMode::Table,
        );
        assert_eq!(
            args,
            vec!["mariadb", "-u", "root", "--table", "-D", "shop", "-e", "SELECT 1"]
        );
        // `--table` 必须显式给：stdout 不是 tty 时 mysql 默认是 batch（实测）
        assert!(args.contains(&"--table".to_owned()));
    }

    #[test]
    fn client_args_include_host_and_port_when_given() {
        let creds = MysqlCreds {
            user: "app".to_owned(),
            password: "x".to_owned(),
            host: Some("127.0.0.1".to_owned()),
            port: Some(3307),
        };
        let args = client_args("mysql", &creds, "SELECT 1", None, SqlMode::Batch);
        assert!(
            args.windows(2).any(|w| w[0] == "-h" && w[1] == "127.0.0.1"),
            "{args:?}"
        );
        assert!(
            args.windows(2).any(|w| w[0] == "-P" && w[1] == "3307"),
            "{args:?}"
        );
    }

    #[test]
    fn exec_env_holds_the_password_and_nothing_else_does() {
        let creds = MysqlCreds::root("p@ss");
        assert_eq!(exec_env(&creds), "MYSQL_PWD=p@ss");
    }

    // -- 脱敏 ---------------------------------------------------------------

    #[test]
    fn redact_args_hides_every_password_shape() {
        let args = vec![
            "exec".to_owned(),
            "-e".to_owned(),
            "MYSQL_PWD=root".to_owned(),
            "mysql".to_owned(),
            "mysql".to_owned(),
            "-uroot".to_owned(),
            "-prootpw".to_owned(),
            "--password=rootpw2".to_owned(),
            "-e".to_owned(),
            "SELECT 1".to_owned(),
        ];
        let shown = redact_args(&args).join(" ");
        assert!(!shown.contains("rootpw"), "{shown}");
        assert!(!shown.contains("MYSQL_PWD=root "), "{shown}");
        assert!(shown.contains("MYSQL_PWD=***"), "{shown}");
        assert!(shown.contains("-p***"), "{shown}");
        assert!(shown.contains("--password=***"), "{shown}");
        // 不该误伤正常参数
        assert!(shown.contains("SELECT 1"), "{shown}");
        assert!(shown.contains("-uroot"), "{shown}");
    }

    #[test]
    fn redact_password_replaces_the_value_even_inside_sql_echoes() {
        // `CREATE USER … IDENTIFIED BY 'x'` 出错时 mysql 会回显整条语句
        let stderr = "ERROR 1064 (42000) at line 1: You have an error in your SQL syntax near \
                      'IDENTIFIED BY \\'hunter2\\'' at line 1";
        let clean = redact_password(stderr, "hunter2");
        assert!(!clean.contains("hunter2"), "{clean}");
        assert!(clean.contains("***"), "{clean}");
    }

    #[test]
    fn redact_password_leaves_text_alone_when_the_password_is_empty() {
        // 空密码不能当 pattern：`replace` 会在每个字符边界插东西
        let text = "ERROR 1045 (28000): Access denied";
        assert_eq!(redact_password(text, ""), text);
    }

    // -- 解析 ---------------------------------------------------------------

    #[test]
    fn unescapes_batch_output_like_the_real_thing() {
        // 实测：`SELECT 'a\tb'` 在 `-B` 下输出的是字面的 `a\tb`
        assert_eq!(unescape_batch_field(r"a\tb"), "a\tb");
        assert_eq!(unescape_batch_field(r"back\\slash"), r"back\slash");
        assert_eq!(unescape_batch_field(r"line\nbreak"), "line\nbreak");
        // 未知转义原样保留（不猜）
        assert_eq!(unescape_batch_field(r"\x"), r"\x");
        // 结尾的孤立反斜杠不 panic
        assert_eq!(unescape_batch_field("tail\\"), "tail\\");
    }

    #[test]
    fn parses_the_real_database_list() {
        let db = parse_databases(DATABASES_TSV);
        assert_eq!(db.len(), 4);
        assert_eq!(db[0].name, "information_schema");
        assert_eq!(db[0].charset, "utf8mb3");
        assert_eq!(db[0].tables, 84);
        assert_eq!(db[1].name, "mysql");
        assert_eq!(db[1].collation, "utf8mb4_0900_ai_ci");
        assert_eq!(db[1].bytes, 8_257_536);
        assert!(db[1].is_system());
        assert!(!db.iter().any(|d| d.name == "shop"));
    }

    #[test]
    fn parses_the_real_user_list() {
        let users = parse_users(USERS_TSV);
        assert_eq!(users.len(), 5);
        // 同一个用户名两个 host —— 这就是 host 必须是一列的原因
        assert_eq!(
            users.iter().filter(|u| u.user == "root").count(),
            2,
            "{users:?}"
        );
        assert_eq!(users[0].account(), "'mysql.infoschema'@'localhost'");
        assert!(!users[0].super_priv);
        assert!(users[4].super_priv);
        assert_eq!(users[4].plugin, "caching_sha2_password");
        assert!(users[0].is_system());
        assert!(!users[4].is_system());
    }

    #[test]
    fn parsers_skip_broken_rows_instead_of_panicking() {
        assert!(parse_databases("").is_empty());
        assert!(parse_users("").is_empty());
        // 少列的行跳过、好的行留着
        let mixed = "only_one_column\nmysql\tutf8mb4\tutf8mb4_0900_ai_ci\t3\t1024\n";
        let db = parse_databases(mixed);
        assert_eq!(db.len(), 1);
        assert_eq!(db[0].name, "mysql");
        // 数字列坏掉时按 0 处理，不丢整行
        let bad_number = "shop\tutf8mb4\tutf8mb4_0900_ai_ci\t?\t?\n";
        let db = parse_databases(bad_number);
        assert_eq!(db.len(), 1);
        assert_eq!(db[0].tables, 0);
        assert_eq!(db[0].bytes, 0);
    }

    #[test]
    fn parses_the_real_client_probe() {
        let probe = parse_client_probe(CLIENT_PROBE).expect("应解析出客户端");
        assert_eq!(probe.bin, "mysql");
        assert_eq!(probe.path, "/usr/bin/mysql");
        assert!(
            probe.version.as_deref().unwrap_or_default().contains("26.7.0"),
            "{probe:?}"
        );
        // 只有一行（老版本 / 探测脚本被裁剪）也要能用
        let only_path = parse_client_probe("/usr/bin/mariadb\n").unwrap();
        assert_eq!(only_path.bin, "mariadb");
        assert_eq!(only_path.version, None);
        assert_eq!(parse_client_probe(""), None);
    }

    // -- SQL 生成 -----------------------------------------------------------

    #[test]
    fn validates_identifiers_and_rejects_injection_shapes() {
        assert!(validate_ident("库名", "shop").is_ok());
        assert!(validate_ident("库名", "shop_2$").is_ok());
        assert!(validate_ident("库名", "  shop  ").is_ok());
        assert!(validate_ident("库名", "").is_err());
        assert!(validate_ident("库名", &"a".repeat(65)).is_err());
        // 下面这些都是**必须拒掉**的（它们会直接进 SQL）。
        // ⚠️ 用「中间有换行」而不是「末尾有换行」：名字两头的空白是会被
        // `trim()` 掉的（表单输入带个回车不该算错误），中间的才真非法。
        for bad in [
            "shop; DROP DATABASE mysql",
            "shop`",
            "shop'",
            "shop--",
            "shop db",
            "shop\ndrop",
            "库",
        ] {
            assert!(
                validate_ident("库名", bad).is_err(),
                "应拒绝：{bad:?}"
            );
        }
    }

    #[test]
    fn quotes_literals_for_passwords() {
        assert_eq!(quote_literal("plain"), "'plain'");
        assert_eq!(quote_literal(r"a\b"), r"'a\\b'");
        assert_eq!(quote_literal("it's"), r"'it\'s'");
        assert_eq!(quote_literal(r#"dq"here"#), r#"'dq\"here'"#);
        assert_eq!(quote_literal(""), "''");
    }

    #[test]
    fn builds_create_database_with_optional_charset() {
        assert_eq!(
            create_database_sql("shop", None, None).unwrap(),
            "CREATE DATABASE `shop`"
        );
        assert_eq!(
            create_database_sql("shop", Some("utf8mb4"), None).unwrap(),
            "CREATE DATABASE `shop` DEFAULT CHARACTER SET utf8mb4"
        );
        assert_eq!(
            create_database_sql("shop", Some("utf8mb4"), Some("utf8mb4_0900_ai_ci")).unwrap(),
            "CREATE DATABASE `shop` DEFAULT CHARACTER SET utf8mb4 DEFAULT COLLATE utf8mb4_0900_ai_ci"
        );
        // 空白等于没填（表单里那一格默认是空的）
        assert_eq!(
            create_database_sql("shop", Some("   "), None).unwrap(),
            "CREATE DATABASE `shop`"
        );
        // 保留字做库名也要能跑 —— 所以标识符**一律**反引号
        assert_eq!(
            create_database_sql("order", None, None).unwrap(),
            "CREATE DATABASE `order`"
        );
        assert!(create_database_sql("shop; drop", None, None).is_err());
        assert!(create_database_sql("shop", Some("utf8mb4; drop"), None).is_err());
    }

    #[test]
    fn builds_user_statements() {
        assert_eq!(
            create_user_sql("app", "%", "pw").unwrap(),
            "CREATE USER IF NOT EXISTS 'app'@'%' IDENTIFIED BY 'pw'"
        );
        assert_eq!(
            alter_user_password_sql("app", "localhost", "p'w").unwrap(),
            r"ALTER USER 'app'@'localhost' IDENTIFIED BY 'p\'w'"
        );
        assert_eq!(
            drop_user_sql("app", "%").unwrap(),
            "DROP USER IF EXISTS 'app'@'%'"
        );
        assert_eq!(
            drop_database_sql("shop").unwrap(),
            "DROP DATABASE `shop`"
        );

        // host 比标识符宽松：`%` 和网段都要能过
        assert!(validate_host("%").is_ok());
        assert!(validate_host("192.168.1.%").is_ok());
        assert!(validate_host("localhost").is_ok());
        assert!(validate_host("").is_err());
        assert!(validate_host("a b").is_err());
        assert!(validate_host("a'; DROP USER x").is_err());
    }

    #[test]
    fn grant_variants_are_all_valid_sql() {
        assert_eq!(
            grant_sql("shop", "app", "%", GrantKind::All).unwrap(),
            "GRANT ALL PRIVILEGES ON `shop`.* TO 'app'@'%'"
        );
        assert_eq!(
            grant_sql("shop", "app", "%", GrantKind::ReadOnly).unwrap(),
            "GRANT SELECT ON `shop`.* TO 'app'@'%'"
        );
        assert_eq!(
            grant_sql("shop", "app", "%", GrantKind::ReadWrite).unwrap(),
            "GRANT SELECT, INSERT, UPDATE, DELETE ON `shop`.* TO 'app'@'%'"
        );
        assert_eq!(GrantKind::ALL.len(), 3);
        assert!(GrantKind::ALL.iter().all(|k| !k.label().is_empty()));
    }

    #[test]
    fn create_user_statements_joins_create_and_grant() {
        let sql = create_user_statements("app", "%", "pw", Some(("shop", GrantKind::All))).unwrap();
        assert_eq!(
            sql,
            "CREATE USER IF NOT EXISTS 'app'@'%' IDENTIFIED BY 'pw'; \
             GRANT ALL PRIVILEGES ON `shop`.* TO 'app'@'%'"
        );
        // 不授权时只有一条语句
        let only = create_user_statements("app", "%", "pw", None).unwrap();
        assert!(!only.contains(';'), "{only}");
        // 授权对象非法 → 整条拒绝（不会出现"建了用户但没授权"的半成品）
        assert!(create_user_statements("app", "%", "pw", Some(("bad name", GrantKind::All))).is_err());
    }

    // -- 错误分类 -----------------------------------------------------------

    #[test]
    fn classifies_the_real_error_texts() {
        assert_eq!(
            classify_error(ERR_ACCESS_DENIED, Some(1)),
            MysqlErrorKind::AccessDenied
        );
        assert_eq!(
            classify_error(ERR_NOT_RUNNING, Some(1)),
            MysqlErrorKind::ContainerNotRunning
        );
        assert_eq!(
            classify_error(ERR_NO_CLIENT, Some(126)),
            MysqlErrorKind::ClientMissing
        );
        // 退出码单独也能判（126/127 是"命令没找到"）
        assert_eq!(classify_error("", Some(126)), MysqlErrorKind::ClientMissing);
    }

    #[test]
    fn classifies_the_remaining_sql_errors() {
        assert_eq!(
            classify_error(
                "ERROR 2002 (HY000): Can't connect to local MySQL server through socket",
                Some(1)
            ),
            MysqlErrorKind::ServerNotReady
        );
        assert_eq!(
            classify_error(
                "ERROR 1142 (42000): SELECT command denied to user 'app'@'localhost'",
                Some(1)
            ),
            MysqlErrorKind::PermissionDenied
        );
        assert_eq!(
            classify_error("ERROR 1049 (42000): Unknown database 'nope'", Some(1)),
            MysqlErrorKind::UnknownDatabase
        );
        assert_eq!(
            classify_error("ERROR 1064 (42000): You have an error in your SQL syntax", Some(1)),
            MysqlErrorKind::SqlError
        );
        assert_eq!(classify_error("莫名其妙", Some(1)), MysqlErrorKind::Unknown);
    }

    #[test]
    fn every_non_trivial_kind_tells_the_user_what_to_do() {
        for kind in [
            MysqlErrorKind::ContainerNotRunning,
            MysqlErrorKind::ClientMissing,
            MysqlErrorKind::AccessDenied,
            MysqlErrorKind::ServerNotReady,
            MysqlErrorKind::PermissionDenied,
            MysqlErrorKind::UnknownDatabase,
        ] {
            assert!(kind.hint().is_some(), "{kind:?} 应该有下一步建议");
        }
        // "等一等就好"和"出错了"要能分开
        assert!(MysqlErrorKind::ServerNotReady.is_transient());
        assert!(!MysqlErrorKind::AccessDenied.is_transient());
    }
}
