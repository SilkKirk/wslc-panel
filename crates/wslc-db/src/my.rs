//! MySQL / MariaDB 的原生会话（`mysql` 28，同步客户端）。
//!
//! # 为什么这条路比 PostgreSQL 简单
//!
//! `query_iter` 走的是**文本协议**：服务端把每个值直接发成字节串，
//! 客户端不做类型解码。所以：
//!
//! - 任意列类型都能显示（不需要 `bool`/`int4`/`decimal`/`datetime` 的映射表）；
//! - 列名从 `QueryResult::columns()` 直接拿，**不用**像 PG 那样额外 `prepare` 一次。
//!
//! 代价是文本协议对二进制列（`BLOB`/`BINARY`）只能尽力而为 ——
//! 非 UTF-8 的字节会被替换成 `U+FFFD`。做数据库**管理**够用（要看二进制
//! 内容得另开一个十六进制视图，那是另一件事）。
//!
//! # 切分复用 `wslc-core`
//!
//! `sqlstmt::split_statements` 是 v0.12 就写好的（反引号 / `#` 注释 /
//! `DELIMITER` 都处理过），**和连接方式无关**，换成原生驱动照用。

use mysql::prelude::Queryable;

use crate::conn::ConnSpec;
use crate::session::{DbError, DbErrorKind, Session, TextTable};

/// MySQL 的 `binary` 字符集 id。
///
/// 列的 `character_set()` 等于它，说明这列**不是文本**（`BLOB` / `VARBINARY` /
/// `GEOMETRY` …）。判据取自 dbx（`is_mysql_binary_charset`），它的理由是"只有
/// 二进制列才会带 63"，比按 `ColumnType` 枚举去穷举稳。
const BINARY_CHARSET_ID: u16 = 63;

/// 二进制值怎么显示：`0x` + 大写十六进制。
///
/// 不这么做的后果是 `String::from_utf8_lossy` 把它们变成一串 `U+FFFD`（问号），
/// 看起来像"数据坏了" —— 其实只是我们拿二进制当文本读了。
fn hex_bytes(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(2 + bytes.len() * 2);
    out.push_str("0x");
    for b in bytes {
        out.push_str(&format!("{b:02X}"));
    }
    out
}

pub struct MySqlSession {
    conn: mysql::Conn,
}

impl MySqlSession {
    pub fn open(spec: &ConnSpec) -> Result<Self, DbError> {
        // ⚠️ 要求 TLS 时**明确报错**，不能静默用明文连上去。
        //
        // 静默降级是安全问题：用户以为"要加密"，结果口令和查询内容在网线上裸奔，
        // 而界面上一切正常。宁可连不上，也不能这样"成功"。
        // （`mysql` 28 的 TLS 要 `SslOpts` + 可信根证书，是独立一块工作量。）
        if spec.tls {
            return Err(DbError::new(
                DbErrorKind::Tls,
                "MySQL 的 TLS 还没接（现在只能连明文库）",
            ));
        }
        // `Opts::from_url` 认我们拼出来的那个 URL（用户名/口令已经百分号编码过）。
        let opts = mysql::Opts::from_url(&spec.mysql_url())
            .map_err(|e| DbError::new(DbErrorKind::Other, format!("连接串解析失败：{e}")))?;
        let conn = mysql::Conn::new(opts).map_err(classify)?;
        Ok(Self { conn })
    }
}

impl Session for MySqlSession {
    fn query(&mut self, script: &str) -> Result<Vec<TextTable>, DbError> {
        let statements = wslc_core::model::sqlstmt::split_statements(script);
        if statements.is_empty() {
            return Err(DbError::new(
                DbErrorKind::Query,
                "没有可执行的语句（只有空白或注释）",
            ));
        }

        let mut out: Vec<TextTable> = Vec::new();
        for stmt in statements {
            let mut result = self.conn.query_iter(&stmt).map_err(classify)?;

            // 列名 + "这列是不是二进制" 都要在迭代**之前**取（迭代会消费掉 `result`）。
            //
            // 二进制判据跟 dbx 一样：`character_set() == 63`（binary 字符集）。
            // 有了它，BLOB 列可以显示成十六进制而不是一堆问号 —— 后者看起来像
            // "数据坏了"，其实只是我们拿二进制当文本读了。
            let (columns, binary): (Vec<String>, Vec<bool>) = {
                let cols = result.columns();
                let slice: &[mysql::Column] = cols.as_ref();
                (
                    slice.iter().map(|c| c.name_str().into_owned()).collect(),
                    slice
                        .iter()
                        .map(|c| c.character_set() == BINARY_CHARSET_ID)
                        .collect(),
                )
            };

            let mut rows: Vec<Vec<Option<String>>> = Vec::new();
            for row in result.by_ref() {
                let row = row.map_err(classify)?;
                let mut cells = Vec::with_capacity(row.len());
                for i in 0..row.len() {
                    // 取原始字节再自己决定怎么显示：
                    // - 外层 `Option` 是"这一列存在吗"（索引越界时为 `None`）；
                    // - 内层 `Option` 是 NULL。
                    // 用 `Option<Vec<u8>>` 而不是 `Option<String>`：后者遇到
                    // 非 UTF-8 的 BLOB 会**直接报错**，而我们只想尽量显示。
                    let raw: Option<Option<Vec<u8>>> = row.get(i);
                    let is_binary = binary.get(i).copied().unwrap_or(false);
                    cells.push(raw.flatten().map(|b| {
                        if is_binary {
                            hex_bytes(&b)
                        } else {
                            String::from_utf8_lossy(&b).into_owned()
                        }
                    }));
                }
                rows.push(cells);
            }

            let mut table = TextTable { columns, rows };
            // 没有列名又没有行 = 这条语句没有结果集（DDL / INSERT），
            // 塞空表会让界面画出"0 列 0 行"的怪表格。
            if table.is_empty() {
                continue;
            }
            let width = table.rows.first().map(|r| r.len()).unwrap_or(0);
            table.ensure_columns(width);
            table.pad_rows();
            out.push(table);
        }
        Ok(out)
    }

    fn server_version(&mut self) -> Result<String, DbError> {
        // `SELECT VERSION()` 比 `@@version` 稳（后者在 MariaDB 上语法一样，
        // 但 `VERSION()` 两端都保证有）。
        let result: Option<String> = self
            .conn
            .query_first("SELECT VERSION()")
            .map_err(classify)?;
        Ok(result.unwrap_or_default())
    }
}

/// 把 `mysql::Error` 归类。
///
/// MySQL 的错误号比 PostgreSQL 的 SQLSTATE 更好用（数字区间很清楚）：
/// - `1045` 口令错、`1044`/`1142`/`1227` 没权限 → 认证类；
/// - `1049` 库不存在；
/// - 其余服务端错误 → [`DbErrorKind::Query`]。
///
/// 客户端侧（`IoError`）**没有服务端错误号**，只能按文本认，
/// 判据在 [`classify_io_error`]（纯函数、有测试）。`UrlError` 是连接串本身
/// 不合法（`mysql_url()` 拼错了），归 [`DbErrorKind::Other`]。
///
/// 原文一定保留：`ERROR 1045 (28000): Access denied for user 'root'@'localhost'`
/// 里那个 `user@host` 是排障时唯一有用的线索。
fn classify(err: mysql::Error) -> DbError {
    let text = err.to_string();
    let kind = match &err {
        mysql::Error::MySqlError(e) => classify_server_code(e.code),
        // ⚠️ **TLS 判断必须在 `IoError` 这一支里做。**
        //
        // 早先的写法是：`IoError(_) => Connect`，然后把
        // `contains("TLS") || contains("SSL")` 放在最后的 `_ =>` 里 ——
        // **而它上面那句注释正是**"TLS 握手失败在 mysql crate 里也走 `IoError`"。
        // 也就是说，按代码自己声明的那个前提，`_ =>` 那条 TLS 分支
        // **永远收不到 TLS 错误**：它们先被 `IoError` 截成"连不上服务器"了。
        // 现在先认 TLS，再退回 Connect。
        //
        // （本机没有 Rust 工具链，也没有一个要 TLS 的 MySQL 实例，
        // 所以"TLS 失败究竟是 `IoError` 还是别的变体"**没有实测**过。
        // 这里的改法是"让判断发生在两个变体都覆盖到的位置"，
        // 无论哪种都成立；而不是赌某一个变体。）
        mysql::Error::IoError(_) => classify_io_error(&text),
        // 其余都归 Other：`UrlError` 是连接串本身不合法（`mysql_url()` 拼错了 /
        // 用户填的 host 有怪字符），别的变体我们也没法更具体地说。
        // 两支合并写 —— 分开写会被 clippy 的 `match_same_arms` 抓（lint 是阻塞的）。
        _ => DbErrorKind::Other,
    };
    DbError::new(kind, text)
}

/// 服务端错误号 → 类别（判据本身，可测）。
fn classify_server_code(code: u16) -> DbErrorKind {
    match code {
        1045 | 1044 | 1142 | 1227 => DbErrorKind::Auth,
        1049 => DbErrorKind::Database,
        _ => DbErrorKind::Query,
    }
}

/// `IoError`（客户端侧、没有服务端错误号）→ 类别。
///
/// # 为什么先 `to_lowercase()` 再比
///
/// 这条是从 `pg.rs` 那个 bug 学来的（那边注释里有完整实测）：判据写死一种
/// 大小写，换个驱动 / 换个系统措辞就漏。`pg.rs` 原写 `contains("tls")`（小写），
/// 漏掉了驱动的 `error performing TLS handshake`；这里原写
/// `contains("TLS") || contains("SSL")`（大写），**是同一个错的镜像版**。
///
/// 归一化之后不必再逐条列举大小写变体 —— 那正是它以前漏掉的原因。
///
/// 只有两档：TLS，或者"连不上"。`IoError` 本来就是连接层的错误
/// （DNS / 端口 / 拒绝 / 超时 / 握手），分不分得出 TLS 只影响那一句提示。
fn classify_io_error(text: &str) -> DbErrorKind {
    let lower = text.to_lowercase();
    if lower.contains("tls") || lower.contains("ssl") {
        DbErrorKind::Tls
    } else {
        DbErrorKind::Connect
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_building_is_used_for_connecting() {
        // 这条钉的是"连接参数从哪儿来"：MySQL 走 URL，PostgreSQL 走 conninfo。
        // 两边的拼装都在 `conn.rs` 里测过，这里只确认 URL 形状能被 `from_url` 吃下。
        let mut spec = ConnSpec::new(wslc_core::db::DbKind::MySql, "127.0.0.1", 3306, "root");
        spec.password = "p@ss".to_owned();
        spec.database = Some("shop".to_owned());
        let url = spec.mysql_url();
        assert!(mysql::Opts::from_url(&url).is_ok(), "URL 解析不了：{url}");
    }

    #[test]
    fn binary_values_render_as_hex_not_mojibake() {
        // dbx 的做法：字符集 id == 63 的列按十六进制显示。
        // 不这么做，`[0xFF, 0xFE]` 会变成两个 U+FFFD —— 看起来像数据损坏。
        assert_eq!(hex_bytes(&[0x00, 0x1F, 0xFF]), "0x001FFF");
        assert_eq!(hex_bytes(&[]), "0x");
        assert_eq!(hex_bytes("hi".as_bytes()), "0x6869");
    }

    #[test]
    fn tls_is_refused_rather_than_silently_downgraded() {
        let mut spec = ConnSpec::new(wslc_core::db::DbKind::MySql, "10.0.0.5", 3306, "u");
        spec.tls = true;
        // 不用 `unwrap_err()`：它要求 `Ok` 那边（`MySqlSession`）实现 `Debug`，
        // 而持有连接的会话类型不该为了一个测试去 derive 它。
        let Err(err) = MySqlSession::open(&spec) else {
            panic!("要求 TLS 时必须失败，不能静默用明文连上去");
        };
        assert_eq!(err.kind, DbErrorKind::Tls);
        // 要说清"没接"，而不是让用户去猜口令 —— 这条错误是**给用户看的**
        assert!(err.message.contains("TLS"), "{}", err.message);
    }

    #[test]
    fn error_kinds_have_labels() {
        for kind in [DbErrorKind::Connect, DbErrorKind::Auth, DbErrorKind::Query] {
            assert!(!kind.label().is_empty());
        }
    }

    #[test]
    fn server_codes_map_to_the_right_kind() {
        // 判据是 MySQL 文档里定死的错误号，不是我们的猜测。
        assert_eq!(classify_server_code(1045), DbErrorKind::Auth, "口令错");
        assert_eq!(classify_server_code(1044), DbErrorKind::Auth, "库级权限不足");
        assert_eq!(classify_server_code(1142), DbErrorKind::Auth, "表级权限不足");
        assert_eq!(classify_server_code(1049), DbErrorKind::Database, "库不存在");
        // 其余服务端错误都是 SQL 本身的问题（语法 / 约束 / 类型）
        assert_eq!(classify_server_code(1064), DbErrorKind::Query, "语法错误");
        assert_eq!(classify_server_code(1062), DbErrorKind::Query, "唯一键冲突");
    }

    /// ⚠️ 回归钉：TLS 握手失败**必须**被认出来，而不是说成"连不上"。
    ///
    /// 两个理由，缺一都会让这个 bug 回来：
    ///
    /// 1. TLS 判断要放在**能收到那个变体**的分支里。早先它写在最后的 `_ =>`，
    ///    而按代码自己声明的"TLS 走 `IoError`"，那条分支永远收不到 ——
    ///    界面上说"连不上服务器"，用户拿着查网络的提示去排证书问题。
    /// 2. 判断用的是 `contains("TLS")`（全大写）。驱动 / 系统的措辞换个大小写
    ///    就漏 —— `pg.rs` 就是栽在镜像版上（它写的全小写，漏了大写措辞）。
    ///    现在两边都先 `to_lowercase()`。
    #[test]
    fn tls_io_errors_are_tls_not_connect() {
        for text in [
            "TLS handshake failed",
            "error performing TLS handshake",
            "tls: handshake failure",
            "SSL error: certificate verify failed",
            "ssl connection error",
        ] {
            assert_eq!(
                classify_io_error(text),
                DbErrorKind::Tls,
                "TLS 类错误没被认出来：{text}"
            );
        }
    }

    #[test]
    fn other_io_errors_are_connect() {
        for text in [
            "Connection refused (os error 10061)",
            "timed out waiting for server",
            "failed to lookup address information: Name or service not known",
        ] {
            assert_eq!(classify_io_error(text), DbErrorKind::Connect, "{text}");
        }
        // 认不出来的 IoError 仍然按"连不上"算 —— 这一类本来就是连接层
        assert_eq!(
            classify_io_error("something entirely unexpected"),
            DbErrorKind::Connect
        );
    }
}
