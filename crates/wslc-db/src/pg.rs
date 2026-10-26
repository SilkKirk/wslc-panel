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
        // 同 `my.rs`：要求 TLS 就报错，不静默降级成明文。
        if spec.tls {
            return Err(DbError::new(
                DbErrorKind::Tls,
                "PostgreSQL 的 TLS 还没接（现在只能连明文库）",
            ));
        }
        // ⚠️ `NoTls`：`postgres` 0.19 的 TLS 要靠额外的 `MakeTlsConnect`
        // 实现（`postgres-native-tls` 等）。阶段 A 先只做明文 ——
        // 远程库要 TLS 是**下一步必须补的**。
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
/// 真正的判断在 [`classify_parts`]（纯函数、有测试）；这里只负责从
/// `postgres::Error` 里把「SQLSTATE」和「原文」两样东西取出来。
///
/// # 为什么要拆
///
/// `postgres::Error` **构造不出来**（字段全私有），所以只要判断逻辑还写在
/// 这个函数里，它就一行都测不到。v0.16 起那份测试实际上只断言了
/// `"28P01".starts_with("28")` —— 测的是标准库，不是我们。
fn classify(err: postgres::Error) -> DbError {
    let message = err.to_string();
    // `as_db_error()` 只对"服务端返回的错"有值；连接阶段的错它是 `None`。
    let code = err.as_db_error().map(|db| db.code().code().to_owned());
    let kind = classify_parts(code.as_deref(), &message);
    // 用服务端原文（含 `LINE n:` 和插入符），它比我们拼的任何话都清楚。
    DbError::new(kind, message)
}

/// 归类的**判据本身**（纯函数，可测）。
///
/// `code` 是 SQLSTATE；`None` = 这条错没有 SQLSTATE，也就是**连接层**的错
/// （DNS / 端口 / 拒绝 / 超时 / TLS 握手）。
///
/// # 判据
///
/// - SQLSTATE `28xxx` → 认证类（`28P01` 口令错、`28000` 权限不足）；
/// - `3D000`（库不存在）/ `42501`（权限不足）→ [`DbErrorKind::Database`]；
/// - 其余带 SQLSTATE 的 → [`DbErrorKind::Query`]（SQL 本身的问题）；
/// - 不带 SQLSTATE 的按文本猜：认证 → TLS → 连接 → Other。
///
/// ⚠️ 文本兜底**只用于连接层**（`code == None`）。服务端给出的错一律按
/// SQLSTATE 判 —— 否则一句恰好含 "password" 的语法错误会被说成"认证失败"。
///
/// ⚠️ **大小写照原样比会漏**：`postgres` 自己的措辞是
/// `error performing TLS handshake`（大写 `TLS`），而这行早先写的是
/// `contains("tls")` —— 于是 TLS 握手失败**一直被归成 `Other`**
/// （界面上说"数据库错误"，用户看不出该去查证书还是查口令）。
/// 现在统一先 `to_lowercase()` 再比；`SSL` 那条原本是特意大写写的，
/// 归一化之后 `ssl` 一样能命中。这是"判据归一化"而不是"多加一个关键词"。
fn classify_parts(code: Option<&str>, message: &str) -> DbErrorKind {
    let Some(code) = code else {
        // 连接层：把常见情况分出来，其余归 Other。
        //
        // 归一化成小写再比：错误文本的措辞是大小写混用的（驱动自己的
        // `TLS handshake`、系统给的 `Connection refused` / `Connection reset`）。
        // ⚠️ 别改回逐条列举大小写变体 —— 那正是这条判据以前漏掉 TLS 的原因。
        let lower = message.to_lowercase();
        return if lower.contains("authentication")
            || lower.contains("password")
            || lower.contains("role")
        {
            DbErrorKind::Auth
        } else if lower.contains("tls") || lower.contains("ssl") {
            DbErrorKind::Tls
        } else if lower.contains("connect")
            || lower.contains("refused")
            || lower.contains("timed out")
        {
            DbErrorKind::Connect
        } else {
            DbErrorKind::Other
        };
    };

    if code.starts_with("28") {
        DbErrorKind::Auth
    } else if code == "3D000" || code == "42501" {
        DbErrorKind::Database
    } else {
        DbErrorKind::Query
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_errors_are_recognised_by_sqlstate_prefix() {
        // SQLSTATE 的判据是 PostgreSQL 文档里定死的，不是我们的猜测：
        // `28P01` 口令错、`28000` 权限不足，都归认证类。
        // 端到端（连一个错口令的实例）本机测不出来 —— `pg_hba=trust`，
        // 见 `docs/postgres-measured.md` 里"测不出来的那一条"。
        for code in ["28P01", "28000"] {
            assert_eq!(classify_parts(Some(code), ""), DbErrorKind::Auth, "{code}");
        }
        // 库不存在不是认证问题，别混进去
        assert_eq!(classify_parts(Some("3D000"), ""), DbErrorKind::Database);
        // 权限不足（`42501`）在界面上也是"库不可用"那一档
        assert_eq!(classify_parts(Some("42501"), ""), DbErrorKind::Database);
        // 其余带 SQLSTATE 的都是 SQL 本身的问题
        assert_eq!(classify_parts(Some("42601"), ""), DbErrorKind::Query);
    }

    #[test]
    fn real_server_errors_are_classified_by_sqlstate_not_by_text() {
        // 夹具是真机抓下来的（`psql` 的原文，见 `tests/fixtures/`）。
        // 这一条钉的是"服务端的错**按 SQLSTATE 判**，不按文本猜"：
        // `postgres_error_syntax.txt` 里那句语法错误如果走文本兜底，
        // 它一个关键词都命中不了 —— 但更重要的是反过来那种：
        // 一句恰好含 "password" 的 SyntaxError 不该被说成"认证失败"。
        let syntax = include_str!("../../wslc-core/tests/fixtures/postgres_error_syntax.txt");
        assert!(syntax.contains("syntax error"), "{syntax}");
        assert_eq!(
            classify_parts(Some("42601"), syntax),
            DbErrorKind::Query,
            "语法错误被归错了，界面会给一句无关的建议"
        );
        // 即使原文里有认证类的字眼，SQLSTATE 说了算
        assert_eq!(
            classify_parts(Some("42601"), "password authentication failed"),
            DbErrorKind::Query
        );

        // 库不存在：`psql` 把 FATAL 写在连接阶段，但服务端**给了** SQLSTATE
        let missing = include_str!("../../wslc-core/tests/fixtures/postgres_error_missing_db.txt");
        assert!(missing.contains("does not exist"), "{missing}");
        assert_eq!(
            classify_parts(Some("3D000"), missing),
            DbErrorKind::Database
        );
    }

    #[test]
    fn connection_layer_errors_fall_back_to_text() {
        // 没有 SQLSTATE = 还没连上（DNS / 端口 / 拒绝 / 超时 / TLS）。
        // 这一类只能按文本分，判错的代价是那句"该怎么办"不对。
        assert_eq!(
            classify_parts(None, "error connecting to server: Connection refused"),
            DbErrorKind::Connect
        );
        assert_eq!(
            classify_parts(None, "error connecting to server: timed out"),
            DbErrorKind::Connect
        );
        assert_eq!(
            classify_parts(None, "password authentication failed for user \"app\""),
            DbErrorKind::Auth
        );
        // TLS：**大小写都要认**
        //
        // ⚠️ 这一条不是补充覆盖率，是 v0.17.0 的**回归钉**：原判据写的是
        // `contains("tls")`（全小写），而 `postgres` 自己的措辞是
        // `error performing TLS handshake` —— 大写 TLS 匹配不上，
        // 于是 TLS 握手失败一直被归成 `Other`（"数据库错误"），
        // 用户看不出该去查证书还是查口令。这个 bug 是靠这条测试暴露的。
        for message in [
            "error performing TLS handshake",
            "error performing tls handshake",
            "invalid SSL configuration",
        ] {
            assert_eq!(
                classify_parts(None, message),
                DbErrorKind::Tls,
                "TLS 类错误没被认出来：{message}"
            );
        }
        // 认不出来就老实说"数据库错误"，别瞎猜
        assert_eq!(
            classify_parts(None, "something entirely unexpected"),
            DbErrorKind::Other
        );
    }

    #[test]
    fn every_error_kind_has_a_label() {
        // 每类都要有能看的短句 —— 界面上显示的就是它
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
