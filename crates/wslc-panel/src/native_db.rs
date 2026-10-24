//! 原生连接的门面：把「一条连接串」变成「查询结果 / 对象树」。
//!
//! # 这一层很薄，但有用
//!
//! `wslc-db` 是纯逻辑 + 驱动，不认识 `wslc_core` 的 `SqlResult` / `SchemaTree`；
//! 这里做两件事：
//!
//! 1. **解析连接串**，把错误变成一句能直接给用户看的话；
//! 2. **把 `TextTable` 翻译成面板既有的类型**，让 SQL 控制台 / 表结构树
//!    在上层完全看不出"这次是原生连的还是 exec 连的"。
//!
//! # 为什么每次查询都重新连
//!
//! 没做连接池：`Session` 是同步句柄，存进 `DbState` 就要处理 `Send`/生命周期，
//! 而面板的每次操作本来就是一次"用户点了按钮"。本地/TCP 建连是毫秒级，
//! 远程也就几十毫秒 —— 相对一次查询可以忽略。等真的需要长连接（比如
//! 要做"监听通知"）再引池子。

use wslc_core::model::sqlresult::SqlResult;
use wslc_core::model::schematree::SchemaTree;
use wslc_db::{ConnSpec, TextTable};

/// 解析连接串；错误消息直接可显示。
pub fn parse(text: &str) -> Result<ConnSpec, String> {
    wslc_db::parse_dsn(text).map_err(|e| e.to_string())
}

/// 跑一段脚本。
///
/// 每条语句一个结果集；没有结果集的语句（DDL / INSERT）不出现。
pub fn run_sql(spec: &ConnSpec, script: &str) -> Result<Vec<SqlResult>, String> {
    let mut session = wslc_db::open(spec).map_err(|e| e.to_string())?;
    let tables = session.query(script).map_err(|e| e.to_string())?;
    Ok(tables.iter().map(to_sql_result).collect())
}

/// 读对象树（schema → 表 → 列）。
///
/// 复用 `wslc_core::model::postgres` 的元数据 SQL 和解析：
/// 那套是 v0.15 对着真容器抓出来的，**和连接方式无关**。
pub fn schema(spec: &ConnSpec) -> Result<SchemaTree, String> {
    let mut session = wslc_db::open(spec).map_err(|e| e.to_string())?;
    let tables = session
        .query(wslc_core::model::postgres::schema_tables_sql())
        .map_err(|e| e.to_string())?;
    // 列读失败不算整体失败（和 exec 那条路同一个取舍）：
    // 树本身有用，补全少一档候选而已。
    let columns = session
        .query(wslc_core::model::postgres::schema_columns_sql())
        .unwrap_or_default();
    Ok(wslc_core::model::postgres::parse_schema_tree(
        &tables.first().map(to_csv).unwrap_or_default(),
        &columns.first().map(to_csv).unwrap_or_default(),
    ))
}

/// `TextTable` → `SqlResult`。
///
/// ⚠️ **NULL 在这里变成空串**：`SqlResult` 的单元格是 `Vec<Vec<String>>`，
/// 表达不了 NULL。这是项目既有的取舍（NULL 与空串在界面上不区分），
/// 所以翻译点放在最外层，`wslc-db` 内部仍然保留 `Option`。
fn to_sql_result(table: &TextTable) -> SqlResult {
    SqlResult {
        columns: table.columns.clone(),
        rows: table
            .rows
            .iter()
            .map(|row| row.iter().map(|c| c.clone().unwrap_or_default()).collect())
            .collect(),
    }
}

/// `TextTable` → `CsvTable`（只为复用 `parse_schema_tree` 的解析）。
///
/// 元数据（schema / 表名 / 列名）不会是 NULL，所以这里的 `None → ""` 不丢信息。
/// `header` 留空：那个解析函数只读 `rows`。
fn to_csv(table: &TextTable) -> wslc_core::model::postgres::CsvTable {
    wslc_core::model::postgres::CsvTable {
        header: Vec::new(),
        rows: table
            .rows
            .iter()
            .map(|row| row.iter().map(|c| c.clone().unwrap_or_default()).collect())
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dsn_errors_are_readable() {
        // 错误要能直接显示给用户（带原文），不能只是一句"失败"
        let e = parse("not-a-dsn").unwrap_err();
        assert!(!e.is_empty());
        assert!(e.contains("postgres://") || e.contains("://"), "{e}");
        let e2 = parse("postgres://u@h:abc/db").unwrap_err();
        assert!(e2.contains("端口"), "{e2}");
    }

    #[test]
    fn parses_a_remote_dsn() {
        let spec = parse("postgres://alice:pw@db.example.com:5432/shop").unwrap();
        assert_eq!(spec.host, "db.example.com");
        assert_eq!(spec.port, 5432);
        assert_eq!(spec.database.as_deref(), Some("shop"));
    }

    #[test]
    fn null_becomes_empty_string_at_the_boundary() {
        // 这是刻意的取舍：界面不区分 NULL 与空串（项目既有决定），
        // 但 `wslc-db` 内部仍然保留 Option —— 翻译只发生在这一层。
        let table = TextTable {
            columns: vec!["a".into(), "b".into()],
            rows: vec![vec![None, Some("".into())], vec![Some("x".into()), None]],
        };
        let r = to_sql_result(&table);
        assert_eq!(r.columns, vec!["a", "b"]);
        assert_eq!(r.rows[0], vec!["", ""]);
        assert_eq!(r.rows[1], vec!["x", ""]);
    }
}
