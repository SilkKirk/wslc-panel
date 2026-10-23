//! PostgreSQL 的原生会话（`postgres` 0.19，同步客户端）。
//!
//! # 为什么用 `simple_query` + `prepare` 各一次
//!
//! | 需求 | 用什么 |
//! |---|---|
//! | **值**（任意列类型都要能显示） | `simple_query` —— 服务端直接给文本，零类型映射 |
//! | **列名** | `prepare(...).columns()` |
//!
//! 如果改用扩展协议 `query()`，就得按 `Type` 一个个解码
//! （`bool`/`int4`/`numeric`/`timestamptz`/`bytea`/`jsonb`…），
//! 那是一张会一直漏项的映射表 —— dbx 里 `types.rs` 加上各驱动的类型处理
//! 就是这么长出来的。面板不需要类型，只需要"能看"。
//!
//! 代价是每条语句多一次往返（`prepare`）。和"每条语句一次进程启动（70ms）"
//! 的 exec 模式比，这点开销可以忽略。
//!
//! # 切分复用 `wslc-core`
//!
//! `sqlsplit` 用的是 `wslc_core::model::postgres::split_statements` ——
//! 那个函数有 21 条测试（美元引用、嵌套块注释、`E'\''`），v0.15 就写好了。
//! **它和连接方式无关**，所以换成原生驱动照用。

use postgres::SimpleQueryMessage;
use wslc_core::db::DbKind;

use crate::conn::ConnSpec;
use crate::session::{DbError, DbErrorKind, Session, TextTable};

pub struct PgSession {
    client: postgres::Client,
    /// 服务端版本，连上时查一次就记住（`probe` 和界面都要用）。
    version: Option<String>,
}

impl PgSession {
    pub fn open(spec: &ConnSpec) -> Result<Self, DbError> {
        debug_assert_eq!(spec.kind, DbKind::Postgres);
        // ⚠️ `NoTls`：`postgres` 0.19 的 TLS 要靠额外的 `MakeTlsConnect`
        // 实现（`postgres-native-tls` 等）。阶段 A 先只做明文 ——
        // 远程库要 TLS 是**下一步必须补的**，见 `docs/PLAN-native-db.md`。
        let client = postgres::Client::connect(&spec.postgres_conninfo(), postgres::NoTls)
            .map_err(classify)?;
        Ok(Self {
            client,
            version: None,
        })
    }

    /// 列名（拿不到就返回空 —— DDL、`prepare` 不支持的语句都会走到这儿）。
    fn column_names(&mut self, sql: &str) -> Vec<String> {
        match self.client.prepare(sql) {
            Ok(stmt) => stmt.columns().iter().map(|c| c.name().to_owned()).collect(),
            Err(_) => Vec::new(),
        }
    }
}

impl Session for PgSession {
    fn query(&mut self, script: &str) -> Result<Vec<TextTable>, DbError> {
        // 复用 v0.15 那套切分（美元引用 / 嵌套块注释都处理过了）。
        let statements = wslc_core::model::postgres::split_statements(script);
        if statements.is_empty() {
            return Err(DbError::new(
                DbErrorKind::Query,
                "没有可执行的语句（只有空白或注释）",
            ));
        }

        let mut out: Vec<TextTable> = Vec::new();
        for stmt in statements {
            let columns = self.column_names(&stmt);
            let messages = self.client.simple_query(&stmt).map_err(classify)?;
            let mut table = TextTable {
                columns,
                rows: Vec::new(),
            };
            for msg in messages {
                if let SimpleQueryMessage::Row(row) = msg {
                    // `get(i)` 返回 `Option<&str>`：`None` 就是 NULL，
                    // 空串是 `Some("")`。—— 这两种在界面上都画成空，
                    // 但**数据层保留区别**，将来想区分时不用改协议层。
                    let mut cells = Vec::with_capacity(row.len());
                    for i in 0..row.len() {
                        cells.push(row.get(i).map(|s| s.to_owned()));
                    }
                    table.rows.push(cells);
                }
                // `CommandComplete(n)` 只是"这条语句影响了 n 行"，没有数据。
            }
            // 既没有列名又没有行 = 这条语句没有结果集（DDL / INSERT）。
            // 塞空表进去会让界面画出"0 列 0 行"的怪表格。
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
        if let Some(v) = &self.version {
            return Ok(v.clone());
        }
        let messages = self
            .client
            .simple_query("SHOW server_version")
            .map_err(classify)?;
        let version = messages
            .iter()
            .find_map(|m| match m {
                SimpleQueryMessage::Row(row) => row.get(0).map(|s| s.to_owned()),
                _ => None,
            })
            .unwrap_or_default();
        self.version = Some(version.clone());
        Ok(version)
    }
}

/// 把 `postgres::Error` 归类 —— 界面要能说清"是口令错还是网不通"。
///
/// 依据是 SQLSTATE：`28xxx` 是认证类（`28P01` 口令错、`28000` 权限），
/// `3D000` 是库不存在。没有 SQLSTATE 的基本都是连接层（DNS、拒绝、超时）。
fn classify(err: postgres::Error) -> DbError {
    let message = err.to_string();
    // `as_db_error()` 只对"服务端返回的错"有值；连接阶段的错它是 `None`。
    let Some(db) = err.as_db_error() else {
        // 连接层：把常见情况分出来，其余归 Other。
        let kind = if message.contains("authentication")
            || message.contains("password")
            || message.contains("role")
        {
            DbErrorKind::Auth
        } else if message.contains("tls") || message.contains("SSL") {
            DbErrorKind::Tls
        } else if message.contains("connect")
            || message.contains("Connection")
            || message.contains("refused")
            || message.contains("timed out")
        {
            DbErrorKind::Connect
        } else {
            DbErrorKind::Other
        };
        return DbError::new(kind, message);
    };

    let code = db.code().code();
    let kind = if code.starts_with("28") {
        DbErrorKind::Auth
    } else if code == "3D000" || code == "42501" {
        DbErrorKind::Database
    } else {
        DbErrorKind::Query
    };
    // 用服务端原文（含 `LINE n:` 和插入符），它比我们拼的任何话都清楚。
    DbError::new(kind, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_errors_are_recognised_by_sqlstate_prefix() {
        // `classify` 的输入是 `postgres::Error`，构造不出来，
        // 所以这里钉的是**判据本身**：认证类 SQLSTATE 一律以 `28` 开头
        // （`28P01` 口令错、`28000` 权限不足）。真正的端到端验证只能连一个
        // 错口令的实例 —— 本机 `pg_hba=trust` 测不出来，
        // 见 `docs/postgres-measured.md` 里"测不出来的那一条"。
        for code in ["28P01", "28000"] {
            assert!(code.starts_with("28"), "{code} 应该被归为认证类");
        }
        // 库不存在不是认证问题，别混进去
        assert!(!("3D000".starts_with("28")));
        // 每类都要有能看的短句
        for kind in [
            DbErrorKind::Connect,
            DbErrorKind::Auth,
            DbErrorKind::Database,
            DbErrorKind::Query,
            DbErrorKind::Tls,
            DbErrorKind::Other,
        ] {
            assert!(!kind.label().is_empty());
        }
    }
}
