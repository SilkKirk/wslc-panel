//! 会话：一个打开的数据库连接，以及"跑一段 SQL 拿回结果"。
//!
//! # 结果只要文本（这是刻意的）
//!
//! 面板要的是"把服务端返回的东西画成表格"，不需要把值还原成 Rust 的
//! i32/f64/Timestamp。所以每个单元格都是 `Option<String>`：
//! `None` = NULL，`Some("")` = 空串。这和项目既有决定一致
//! （NULL 与空串在界面上不区分）。
//!
//! 好处是两个引擎能给出**同一个形状**，不必为每个引擎写一套类型映射 ——
//! 那正是 dbx 里 `types.rs` / 每驱动几千行的来源。
//!
//! # 拿列名的办法（PostgreSQL）
//!
//! `simple_query` 返回的值是纯文本，但它的行对象**不一定**暴露列名
//! （不同版本行为不同，没法赌）。所以：
//!
//! 1. `prepare(sql)` 拿 `Statement::columns()` → 列名（一次额外往返）；
//! 2. `simple_query(sql)` 拿值（纯文本，零类型映射）。
//!
//! `prepare` 失败（DDL、多语句等）不算错：那就没有列名，用"列 1/列 2"兜底。

use wslc_core::db::DbKind;

use crate::conn::ConnSpec;

/// 一张结果集（全是文本）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextTable {
    pub columns: Vec<String>,
    /// 每行每格：`None` = NULL，`Some("")` = 空串。
    pub rows: Vec<Vec<Option<String>>>,
}

impl TextTable {
    /// 补齐列名到 `width`（`prepare` 拿不到列名时的兜底）。
    ///
    /// 用「列 1 / 列 2」而不是空串：表头空着看起来像界面坏了，
    /// 而"列 N"明确表达"这里有个值，只是不知道它叫什么"。
    pub fn ensure_columns(&mut self, width: usize) {
        if self.columns.len() >= width {
            return;
        }
        for i in self.columns.len()..width {
            self.columns.push(format!("列 {}", i + 1));
        }
    }

    /// 行宽对齐到列宽（少的补 NULL）。
    pub fn pad_rows(&mut self) {
        let want = self.columns.len();
        for row in &mut self.rows {
            if row.len() < want {
                row.resize(want, None);
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.columns.is_empty() && self.rows.is_empty()
    }
}

/// 错误类别 —— 决定界面上该说什么。
///
/// "口令错"和"网络不通"对用户是完全不同的两件事，把它们混成一条
/// "连接失败"等于让人自己猜。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbErrorKind {
    /// 连不上（DNS / 端口 / 网络 / 服务没起）。
    Connect,
    /// 认证失败（用户名或口令）。
    Auth,
    /// 库不存在 / 没权限。
    Database,
    /// SQL 本身的问题（语法 / 约束 / 类型）。
    Query,
    /// TLS 握手失败。
    Tls,
    /// 其它。
    Other,
}

impl DbErrorKind {
    /// 界面上那句短提示。
    pub fn label(self) -> &'static str {
        match self {
            Self::Connect => "连不上服务器",
            Self::Auth => "认证失败",
            Self::Database => "库不可用",
            Self::Query => "SQL 执行出错",
            Self::Tls => "TLS 握手失败",
            Self::Other => "数据库错误",
        }
    }
}

/// 一个数据库错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbError {
    pub kind: DbErrorKind,
    /// 服务端原文（**不要**改写成中文：排障时要看原文）。
    pub message: String,
}

impl DbError {
    pub fn new(kind: DbErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// 界面上显示的一句话。
    pub fn summary(&self) -> String {
        format!("{}：{}", self.kind.label(), self.message)
    }
}

impl std::fmt::Display for DbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.summary())
    }
}

impl std::error::Error for DbError {}

/// 一个打开的会话。
pub trait Session {
    /// 执行一段脚本，逐条返回结果集。
    ///
    /// **没有结果集的语句**（DDL / INSERT / UPDATE）不产生条目 ——
    /// 界面据此显示"执行成功，没有返回行"。
    fn query(&mut self, script: &str) -> Result<Vec<TextTable>, DbError>;

    /// 服务端版本（显示在连接卡上）。
    fn server_version(&mut self) -> Result<String, DbError>;
}

/// 按连接参数打开一个会话。
pub fn open(spec: &ConnSpec) -> Result<Box<dyn Session>, DbError> {
    match spec.kind {
        DbKind::Postgres => Ok(Box::new(crate::pg::PgSession::open(spec)?)),
        DbKind::MySql | DbKind::MariaDb => Err(DbError::new(
            DbErrorKind::Other,
            "MySQL 的原生连接还没接（阶段 B）",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_columns_fills_only_when_short() {
        let mut t = TextTable {
            columns: vec!["id".into()],
            rows: vec![],
        };
        t.ensure_columns(3);
        assert_eq!(t.columns, vec!["id", "列 2", "列 3"]);
        // 已有列名够长时**不动**（别把服务端给的名字覆盖掉）
        t.ensure_columns(1);
        assert_eq!(t.columns[0], "id");
    }

    #[test]
    fn pad_rows_aligns_to_column_count() {
        let mut t = TextTable {
            columns: vec!["a".into(), "b".into()],
            rows: vec![vec![Some("1".into())]],
        };
        t.pad_rows();
        assert_eq!(t.rows[0], vec![Some("1".into()), None]);
    }

    #[test]
    fn error_summary_says_which_kind() {
        let e = DbError::new(DbErrorKind::Auth, "password authentication failed");
        assert!(e.summary().contains("认证失败"));
        // 服务端原文必须保留 —— 排障时那是唯一线索
        assert!(e.summary().contains("password authentication failed"));
    }

    #[test]
    fn empty_table_is_empty() {
        assert!(TextTable::default().is_empty());
        let t = TextTable {
            columns: vec!["id".into()],
            rows: vec![],
        };
        // 有列名、零行 —— 这是"空结果集"，不是"没有结果集"
        assert!(!t.is_empty());
    }
}
