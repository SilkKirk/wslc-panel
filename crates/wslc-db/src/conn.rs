//! 一条连接的参数，以及把它拼成各驱动要的形状。
//!
//! 这一层是**纯函数**：不连库、不开 socket。所以 `core` job 能真跑它的测试 ——
//! 而拼错一个连接串的表现是"连不上，且错误信息看不出为什么"，最该被单测盯住。

use wslc_core::db::DbKind;

/// 一条数据库连接的全部参数。
///
/// # `host` 是核心
///
/// v0.15 之前"连接"等于"某个容器"，所以代码里到处都是 `container_name`。
/// 这里 `host` 就是个普通字符串：`127.0.0.1`（容器映射到宿主的端口）、
/// 内网 IP、云数据库域名 —— **同一个类型全覆盖**。这就是"能连远程库"。
// ⚠️ **不要**在 derive 里加 `Debug`：下面有手写的实现（只为把口令写成 `***`）。
// 两处都有就是 E0119（conflicting implementations）。
#[derive(Clone, PartialEq, Eq)]
pub struct ConnSpec {
    pub kind: DbKind,
    pub host: String,
    pub port: u16,
    pub user: String,
    /// 明文口令。
    ///
    /// ⚠️ **不落盘**（`docs/PLAN-native-db.md` 阶段 C 之前）。这个 struct
    /// 会进日志/错误信息，所以 `Debug` 是手写的，只写"有没有"。
    pub password: String,
    pub database: Option<String>,
    /// 是否要求 TLS。
    ///
    /// 远程连 MySQL 8 通常**必须**开（`caching_sha2_password` 在非 localhost
    /// 上要么走 TLS、要么走服务端公钥交换）；本地容器为了省事可以关。
    pub tls: bool,
}

impl ConnSpec {
    pub fn new(kind: DbKind, host: impl Into<String>, port: u16, user: impl Into<String>) -> Self {
        Self {
            kind,
            host: host.into(),
            port,
            user: user.into(),
            password: String::new(),
            database: None,
            tls: false,
        }
    }

    /// 一句话描述"连的是哪儿"（不含口令，给日志和界面用）。
    pub fn endpoint(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// libpq 风格的连接串（`postgres` crate 直接吃）。
    ///
    /// 形如 `host=127.0.0.1 port=5432 user=postgres password='a b' dbname=shop`。
    ///
    /// # 为什么要自己处理引号
    ///
    /// libpq 的 conninfo 规则：值里有空格、`'`、`\`，或者是空串，就必须用
    /// 单引号包起来，并把 `\` 和 `'` 用反斜杠转义。**不转义就会拼出一条
    /// 被截断的连接串** —— 症状是"密码对但连不上"，而且错误信息会指向
    /// 别的地方。这类 bug 只能靠单测挡。
    ///
    /// 空的 `database` / `password` 不下发：交给服务端用默认值
    /// （PostgreSQL 会连一个和用户名同名的库）。
    pub fn postgres_conninfo(&self) -> String {
        let mut parts: Vec<String> = vec![
            format!("host={}", conninfo_quote(&self.host)),
            format!("port={}", self.port),
            format!("user={}", conninfo_quote(&self.user)),
        ];
        if !self.password.is_empty() {
            parts.push(format!("password={}", conninfo_quote(&self.password)));
        }
        if let Some(db) = self.database.as_deref().filter(|s| !s.is_empty()) {
            parts.push(format!("dbname={}", conninfo_quote(db)));
        }
        if self.tls {
            // `postgres` 0.19 的 TLS 要靠额外的 `MakeTlsConnect` 实现；
            // 这里只把意图表达出来，驱动层据此决定用哪种 TLS。
            parts.push("sslmode=require".to_owned());
        } else {
            parts.push("sslmode=prefer".to_owned());
        }
        parts.join(" ")
    }

    /// MySQL 的 URL 形式（`mysql` crate 的 `Opts::from_url` 吃）。
    ///
    /// 形如 `mysql://user:pass@host:3306/db`。
    ///
    /// # 为什么要百分号编码
    ///
    /// 口令里出现 `@` `:` `/` `?` `#` 时，不编码会把 URL 解析歪：
    /// `p@ss` 会被当成"用户 p、主机 ss"。编码后才是数据。
    ///
    /// 用户名/口令为空时**不写** `user:pass@` 那一段（写空的会让
    /// `Opts::from_url` 拿到空用户名，而不是"用默认"）。
    pub fn mysql_url(&self) -> String {
        let mut url = String::from("mysql://");
        if !self.user.is_empty() {
            url.push_str(&percent_encode(&self.user));
            if !self.password.is_empty() {
                url.push(':');
                url.push_str(&percent_encode(&self.password));
            }
            url.push('@');
        }
        url.push_str(&self.host);
        url.push(':');
        url.push_str(&self.port.to_string());
        if let Some(db) = self.database.as_deref().filter(|s| !s.is_empty()) {
            url.push('/');
            url.push_str(&percent_encode(db));
        }
        url.push_str(if self.tls {
            "?ssl-mode=REQUIRED"
        } else {
            "?ssl-mode=PREFERRED"
        });
        url
    }
}

/// 解析连接串（`postgres://…` / `postgresql://…` / `mysql://…`）。
///
/// # 为什么要这个
///
/// 这是"通用工具 + 连远程库"最短的一条路：用户从云厂商控制台复制一条 DSN
/// 粘进来就能连，不需要先理解我们的表单有几个格子。
///
/// # 支持的形状
///
/// ```text
/// postgres://user:pass@host:5432/dbname
/// postgresql://user@host/dbname          （没有口令、没有端口）
/// mysql://root:p%40ss@10.0.0.5:3306/shop （口令里的 @ 要编码成 %40）
/// ```
///
/// # 刻意**不**支持的
///
/// `?sslmode=require` 这类查询参数：每个引擎的参数名都不一样
/// （MySQL 是 `ssl-mode`、PostgreSQL 是 `sslmode`、SQL Server 又是另一套），
/// 猜错比不支持更糟。TLS 由界面上的一个开关键决定（见 [`ConnSpec::tls`]）。
/// 查询串**存在但不解析**时会报错，而不是静默丢掉 ——
/// 静默丢掉会让用户以为"我明明写了 require"。
pub fn parse_dsn(text: &str) -> Result<ConnSpec, DsnError> {
    let text = text.trim();
    let (scheme, rest) = text
        .split_once("://")
        .ok_or_else(|| DsnError::new("连接串要形如 postgres://user:pass@host:5432/db"))?;
    let kind = match scheme.to_ascii_lowercase().as_str() {
        "postgres" | "postgresql" | "pg" => DbKind::Postgres,
        "mysql" => DbKind::MySql,
        "mariadb" => DbKind::MariaDb,
        other => return Err(DsnError::new(format!("不认识的协议：{other}"))),
    };

    // 查询串先摘掉，有内容就报错（见上面的说明）。
    let (rest, query) = match rest.split_once('?') {
        Some((r, q)) => (r, Some(q)),
        None => (rest, None),
    };
    if let Some(q) = query {
        if !q.trim().is_empty() {
            return Err(DsnError::new(format!(
                "连接串里的查询参数暂不支持（?{q}）；TLS 请用界面上的开关"
            )));
        }
    }

    // 最后一段是 `/库名`（库名里可能有 `/`，所以用**第一个** `/` 切）。
    let (authority, database) = match rest.split_once('/') {
        Some((a, d)) => (a, Some(percent_decode(d)?)),
        None => (rest, None),
    };

    // `userinfo@hostport`：用**最后一个** `@` 切 —— 手写时口令里出现未编码的
    // `@` 很常见，这样切至少不会把主机名弄错。
    let (userinfo, hostport) = match authority.rsplit_once('@') {
        Some((u, h)) => (Some(u), h),
        None => (None, authority),
    };

    let (user, password) = match userinfo {
        Some(info) => match info.split_once(':') {
            Some((u, p)) => (percent_decode(u)?, percent_decode(p)?),
            None => (percent_decode(info)?, String::new()),
        },
        None => (String::new(), String::new()),
    };

    // 主机可能是 `host:port`，也可能是 IPv6 的 `[::1]:5432`。
    let (host, port) = if let Some(inner) = hostport.strip_prefix('[') {
        let (h, rest) = inner
            .split_once(']')
            .ok_or_else(|| DsnError::new("IPv6 地址的方括号没闭合"))?;
        let port = rest.strip_prefix(':').unwrap_or("");
        (h.to_owned(), port)
    } else {
        match hostport.rsplit_once(':') {
            Some((h, p)) => (h.to_owned(), p),
            None => (hostport.to_owned(), ""),
        }
    };
    if host.is_empty() {
        return Err(DsnError::new("连接串里没有主机名"));
    }
    let port = if port.is_empty() {
        kind.default_port()
    } else {
        port.parse::<u16>()
            .map_err(|_| DsnError::new(format!("端口不是合法数字：{port}")))?
    };

    let mut spec = ConnSpec::new(kind, host, port, user);
    spec.password = password;
    spec.database = database.filter(|d| !d.is_empty());
    Ok(spec)
}

/// 连接串解析不出来。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DsnError(pub String);

impl DsnError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl std::fmt::Display for DsnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DsnError {}

/// 百分号解码（`%40` → `@`）。非法转义直接报错，不猜。
fn percent_decode(value: &str) -> Result<String, DsnError> {
    let bytes = value.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = value
                .get(i + 1..i + 3)
                .ok_or_else(|| DsnError::new("百分号编码不完整"))?;
            let byte = u8::from_str_radix(hex, 16)
                .map_err(|_| DsnError::new(format!("百分号编码不合法：%{hex}")))?;
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| DsnError::new("解码后不是合法的 UTF-8"))
}
/// 手写 `Debug`：**只写"有没有口令"，不写内容**。
///
/// 和 `model::mysql::MysqlCreds` / `db::driver::Credentials` 同一个理由 ——
/// 这个类型会进日志和错误信息，漏一次口令就够糟。
impl std::fmt::Debug for ConnSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnSpec")
            .field("kind", &self.kind)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("user", &self.user)
            .field(
                "password",
                &if self.password.is_empty() {
                    "<empty>"
                } else {
                    "***"
                },
            )
            .field("database", &self.database)
            .field("tls", &self.tls)
            .finish()
    }
}

/// libpq conninfo 的值：需要时用单引号包起来并转义。
///
/// 空串也要包（`password=''` 和"不写 password"是两件事）。
fn conninfo_quote(value: &str) -> String {
    let needs = value.is_empty()
        || value
            .chars()
            .any(|c| c.is_whitespace() || c == '\'' || c == '\\');
    if !needs {
        return value.to_owned();
    }
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for c in value.chars() {
        if c == '\\' || c == '\'' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('\'');
    out
}

/// 最小百分号编码：只保留 URL 里**明确安全**的字符。
///
/// 不用 `percent-encoding` crate：只有这一处需要，规则也就一行，
/// 而多一个依赖对这个仓库来说是实实在在的成本（`AGENTS.md` §2）。
fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        let c = *byte;
        let safe = c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b'~');
        if safe {
            out.push(c as char);
        } else {
            out.push('%');
            out.push_str(&format!("{c:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pg() -> ConnSpec {
        let mut c = ConnSpec::new(DbKind::Postgres, "127.0.0.1", 5432, "postgres");
        c.password = "s3cret".to_owned();
        c.database = Some("shop".to_owned());
        c
    }

    #[test]
    fn debug_never_leaks_the_password() {
        let text = format!("{:?}", pg());
        assert!(!text.contains("s3cret"), "口令漏进 Debug：{text}");
        assert!(text.contains("***"));
        assert!(text.contains("127.0.0.1"));
    }

    #[test]
    fn postgres_conninfo_has_the_expected_shape() {
        assert_eq!(
            pg().postgres_conninfo(),
            "host=127.0.0.1 port=5432 user=postgres password=s3cret dbname=shop sslmode=prefer"
        );
    }

    #[test]
    fn postgres_conninfo_quotes_awkward_values() {
        let mut c = pg();
        c.password = "a b".to_owned();
        assert!(c.postgres_conninfo().contains("password='a b'"));

        // 单引号和反斜杠都要转义，否则连接串会被截断
        c.password = "it's".to_owned();
        assert!(c.postgres_conninfo().contains(r"password='it\'s'"));

        c.password = r"back\slash".to_owned();
        assert!(c.postgres_conninfo().contains(r"password='back\\slash'"));

        // 空串也要包起来：`password=''` 与"不写 password"是两件事
        c.password = String::new();
        assert!(!c.postgres_conninfo().contains("password="));
    }

    #[test]
    fn postgres_conninfo_omits_empty_database() {
        let mut c = pg();
        c.database = Some(String::new());
        let s = c.postgres_conninfo();
        assert!(!s.contains("dbname="), "空库名不该下发：{s}");
    }

    #[test]
    fn tls_intent_is_expressed() {
        let mut c = pg();
        assert!(c.postgres_conninfo().contains("sslmode=prefer"));
        c.tls = true;
        assert!(c.postgres_conninfo().contains("sslmode=require"));
        assert!(c.mysql_url().contains("ssl-mode=REQUIRED"));
    }

    #[test]
    fn mysql_url_encodes_special_characters() {
        let mut c = ConnSpec::new(DbKind::MySql, "db.example.com", 3306, "root");
        c.password = "p@ss:word/1".to_owned();
        c.database = Some("my db".to_owned());
        let url = c.mysql_url();
        // `@` `:` `/` 不编码会把 URL 解析歪（p@ss 会被当成主机）
        assert_eq!(
            url,
            "mysql://root:p%40ss%3Aword%2F1@db.example.com:3306/my%20db?ssl-mode=PREFERRED"
        );
    }

    #[test]
    fn mysql_url_without_credentials_or_database() {
        let c = ConnSpec::new(DbKind::MySql, "10.0.0.5", 3307, "");
        assert_eq!(c.mysql_url(), "mysql://10.0.0.5:3307?ssl-mode=PREFERRED");
    }

    #[test]
    fn remote_hosts_are_just_strings() {
        // 这条测试是这一版的**目的**：远程地址和本地地址走同一条代码路径。
        for host in ["127.0.0.1", "10.0.0.5", "db.prod.example.com", "::1"] {
            let c = ConnSpec::new(DbKind::Postgres, host, 5432, "u");
            assert!(c.postgres_conninfo().contains(&format!("host={host}")));
            assert!(c.mysql_url().contains(host));
        }
    }
}

#[cfg(test)]
mod dsn_tests {
    use super::*;

    #[test]
    fn parses_a_full_postgres_dsn() {
        let c = parse_dsn("postgres://alice:s3cret@db.example.com:6432/shop").unwrap();
        assert_eq!(c.kind, DbKind::Postgres);
        assert_eq!(c.host, "db.example.com");
        assert_eq!(c.port, 6432);
        assert_eq!(c.user, "alice");
        assert_eq!(c.password, "s3cret");
        assert_eq!(c.database.as_deref(), Some("shop"));
        // 远程地址就是普通字段 —— 这是这一版的全部意义
        assert!(c.postgres_conninfo().contains("host=db.example.com"));
    }

    #[test]
    fn fills_defaults_for_missing_port_and_database() {
        let c = parse_dsn("postgresql://postgres@127.0.0.1").unwrap();
        assert_eq!(c.port, 5432, "缺端口时用引擎默认值");
        assert_eq!(c.database, None);
        assert_eq!(c.password, "");

        let m = parse_dsn("mysql://root@10.0.0.5").unwrap();
        assert_eq!(m.kind, DbKind::MySql);
        assert_eq!(m.port, 3306);
    }

    #[test]
    fn decodes_percent_escapes() {
        let c = parse_dsn("mysql://root:p%40ss%3Aword@h:3306/my%20db").unwrap();
        assert_eq!(c.password, "p@ss:word");
        assert_eq!(c.database.as_deref(), Some("my db"));
    }

    #[test]
    fn tolerates_an_unencoded_at_in_the_password() {
        // 手写 DSN 时最常见的错误：口令里有 `@` 却没编码。
        // 用**最后一个** `@` 切，至少不会把主机名认错。
        let c = parse_dsn("mysql://root:p@ss@10.0.0.5:3306/shop").unwrap();
        assert_eq!(c.host, "10.0.0.5");
        assert_eq!(c.password, "p@ss");
    }

    #[test]
    fn parses_ipv6_hosts() {
        let c = parse_dsn("postgres://u:p@[::1]:5433/db").unwrap();
        assert_eq!(c.host, "::1");
        assert_eq!(c.port, 5433);
    }

    #[test]
    fn rejects_query_parameters_instead_of_ignoring_them() {
        // 静默丢掉 `sslmode=require` 会让用户以为"我明明写了"
        let e = parse_dsn("postgres://u@h:5432/db?sslmode=require").unwrap_err();
        assert!(e.to_string().contains("查询参数"), "{e}");
        assert!(e.to_string().contains("sslmode"), "错误里要带上原文：{e}");
    }

    #[test]
    fn rejects_unknown_scheme_and_shape() {
        assert!(parse_dsn("redis://h:6379").unwrap_err().to_string().contains("不认识的协议"));
        assert!(parse_dsn("db.example.com:5432").unwrap_err().to_string().contains("postgres://"));
        assert!(parse_dsn("postgres://u@:5432/db").unwrap_err().to_string().contains("主机名"));
        assert!(parse_dsn("postgres://u@h:notaport/db").unwrap_err().to_string().contains("端口"));
    }

    #[test]
    fn round_trips_into_a_usable_conninfo() {
        // 解析 → 拼连接串：两段都要对，否则"粘进来的 DSN 连不上"
        let c = parse_dsn("postgres://alice:a%20b@10.0.0.5:5432/shop").unwrap();
        let s = c.postgres_conninfo();
        assert!(s.contains("host=10.0.0.5"));
        assert!(s.contains("port=5432"));
        assert!(s.contains("user=alice"));
        // 空格必须被引号包住 —— 否则连接串被截断
        assert!(s.contains("password='a b'"), "{s}");
        assert!(s.contains("dbname=shop"));
    }

    #[test]
    fn debug_of_a_parsed_dsn_still_hides_the_password() {
        let c = parse_dsn("postgres://u:hunter2@h:5432/db").unwrap();
        let text = format!("{c:?}");
        assert!(!text.contains("hunter2"), "口令漏了：{text}");
    }
}