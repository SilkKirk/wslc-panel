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
#[derive(Debug, Clone, PartialEq, Eq)]
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
