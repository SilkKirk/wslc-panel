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
        let opts = mysql::Opts::from_url(&spec.mysql_url()).map_err(|e| {
            DbError::new(
                DbErrorKind::Other,
                format!("连接串解析失败：{e}"),
            )
        })?;
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

            // 列名要在迭代**之前**取（迭代会消费掉 `result`）。
            let columns: Vec<String> = {
                let cols = result.columns();
                let slice: &[mysql::Column] = cols.as_ref();
                slice.iter().map(|c| c.name_str().into_owned()).collect()
            };

            let mut rows: Vec<Vec<Option<String>>> = Vec::new();
            for row in result.by_ref() {
                let row = row.map_err(classify)?;
                let mut cells = Vec::with_capacity(row.len());
                for i in 0..row.len() {
                    // 取原始字节再自己转文本：
                    // - 外层 `Option` 是"这一列存在吗"（索引越界时为 `None`）；
                    // - 内层 `Option` 是 NULL。
                    // 用 `Option<Vec<u8>>` 而不是 `Option<String>`：后者遇到
                    // 非 UTF-8 的 BLOB 会**直接报错**，而我们只想尽量显示。
                    let raw: Option<Option<Vec<u8>>> = row.get(i);
                    cells.push(
                        raw.flatten()
                            .map(|b| String::from_utf8_lossy(&b).into_owned()),
                    );
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
/// - `1045` 口令错、`1044`/`1142` 没权限 → 认证/权限；
/// - `1049` 库不存在；
/// - `2002`/`2003`/`2005` 是**客户端侧**的连接错误（DNS / 拒绝 / 主机未知）
///   —— 它们根本没有服务端错误码，`Error::IoError` 那一支。
fn classify(err: mysql::Error) -> DbError {
    match &err {
        mysql::Error::MySqlError(e) => {
            let kind = match e.code {
                1045 => DbErrorKind::Auth,
                1044 | 1142 | 1227 => DbErrorKind::Auth,
                1049 => DbErrorKind::Database,
                _ => DbErrorKind::Query,
            };
            // 原文一定保留：`ERROR 1045 (28000): Access denied…` 里那个
            // `user@host` 是排障时唯一有用的线索。
            DbError::new(kind, err.to_string())
        }
        mysql::Error::IoError(_) => DbError::new(DbErrorKind::Connect, err.to_string()),
        mysql::Error::UrlError(_) => DbError::new(DbErrorKind::Other, err.to_string()),
        // TLS 握手失败在 mysql crate 里也走 IoError，但消息里带 "TLS"/"SSL"。
        _ => {
            let text = err.to_string();
            let kind = if text.contains("TLS") || text.contains("SSL") {
                DbErrorKind::Tls
            } else {
                DbErrorKind::Other
            };
            DbError::new(kind, text)
        }
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
    fn tls_is_refused_rather_than_silently_downgraded() {
        let mut spec = ConnSpec::new(wslc_core::db::DbKind::MySql, "10.0.0.5", 3306, "u");
        spec.tls = true;
        let err = MySqlSession::open(&spec).unwrap_err();
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
}
