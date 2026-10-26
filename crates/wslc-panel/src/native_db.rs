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

use wslc_core::db::DbKind;
use wslc_core::model::schematree::SchemaTree;
use wslc_core::model::sqlresult::SqlResult;
use wslc_db::{ConnSpec, DbError, DbErrorKind, TextTable};

/// 原生连接路径的错误：保留 `wslc-db` 的分类 + 一句可显示的话。
///
/// 早先这里所有函数都返回 `Result<_, String>`，`DbErrorKind` 在边界上就被
/// 拍平了，UI 只能拿文本再猜一次。分类本身是有用的（认证 / 连不上 / SQL），
/// 所以在这一层保留；`Display` 仍是原来的 `原文`，调用方零改动也能继续用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeDbError {
    /// 引擎 / 网络 / SQL 的分类。
    pub kind: DbErrorKind,
    /// 可显示的原文（已经脱敏过；口令不在里面）。
    pub message: String,
}

impl NativeDbError {
    pub fn new(kind: DbErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for NativeDbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for NativeDbError {}

impl From<DbError> for NativeDbError {
    fn from(error: DbError) -> Self {
        Self {
            kind: error.kind,
            message: error.message,
        }
    }
}

/// 解析连接串；错误消息直接可显示。
pub fn parse(text: &str) -> Result<ConnSpec, NativeDbError> {
    wslc_db::parse_dsn(text).map_err(|e| {
        // `DsnError` 没有 kind；归到 `Other`，消息前缀让 UI / 日志能看出
        // 是"连接串本身不合法"，而不是服务端拒绝。
        NativeDbError::new(DbErrorKind::Other, format!("连接串不合法：{e}"))
    })
}

/// 跑一段脚本。
///
/// 每条语句一个结果集；没有结果集的语句（DDL / INSERT）不出现。
pub fn run_sql(spec: &ConnSpec, script: &str) -> Result<Vec<SqlResult>, NativeDbError> {
    let mut session = wslc_db::open(spec)?;
    let tables = session.query(script)?;
    Ok(tables.iter().map(to_sql_result).collect())
}

/// 界面上显示连接串时打掉口令段 —— 实现在 `wslc-db`。
///
/// ⚠️ **不要**把实现搬回这个文件：`wslc-panel` 不在任何测试目标里
/// （`AGENTS.md` §2），写在这里的 `#[test]` 只编译不执行。
/// `wslc-db` 的测试是真跑的（`core` job）。
pub use wslc_db::redact_dsn;
/// 列出所有库（原生连接）。
///
/// 两个引擎的库列表语义不同，所以各走各的元数据查询 —— 但**解析只有一份**
/// （MySQL 侧用 `to_batch_tsv` 把单元格还原成 `-B -N` 文本，见那个函数的注释）。
pub fn databases(spec: &ConnSpec) -> Result<Vec<String>, NativeDbError> {
    let mut session = wslc_db::open(spec)?;
    match spec.kind {
        DbKind::Postgres => {
            // v0.15 对着真容器抓的（`datallowconn AND NOT datistemplate`）
            let tables = session.query(wslc_core::model::postgres::databases_sql())?;
            Ok(tables
                .first()
                .map(|t| {
                    t.rows
                        .iter()
                        .filter_map(|r| r.first().cloned().flatten())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default())
        }
        DbKind::MySql | DbKind::MariaDb => {
            let tables = session.query(wslc_core::model::mysql::list_databases_sql())?;
            let tsv = tables.first().map(to_tsv).unwrap_or_default();
            Ok(wslc_core::model::mysql::parse_databases(&tsv)
                .into_iter()
                .map(|d| d.name)
                .collect())
        }
    }
}

/// 服务端版本（原生连接）。
pub fn server_version(spec: &ConnSpec) -> Result<String, NativeDbError> {
    let mut session = wslc_db::open(spec)?;
    Ok(session.server_version()?)
}

/// 读对象树（库 → 表 → 列）。
///
/// 复用 `wslc_core::model::{postgres,schematree}` 的元数据 SQL 与解析：
/// 那两套都是对着真容器抓出来的，**和连接方式无关**。
pub fn schema(spec: &ConnSpec) -> Result<SchemaTree, NativeDbError> {
    let mut session = wslc_db::open(spec)?;
    // 列读失败不算整体失败（和 exec 那条路同一个取舍）：
    // 树本身有用，补全少一档候选而已。
    match spec.kind {
        DbKind::Postgres => {
            let tables = session.query(wslc_core::model::postgres::schema_tables_sql())?;
            let columns = session
                .query(wslc_core::model::postgres::schema_columns_sql())
                .unwrap_or_default();
            Ok(wslc_core::model::postgres::parse_schema_tree(
                &tables.first().map(to_csv).unwrap_or_default(),
                &columns.first().map(to_csv).unwrap_or_default(),
            ))
        }
        DbKind::MySql | DbKind::MariaDb => {
            let tables = session.query(wslc_core::model::schematree::schema_tree_sql())?;
            let columns = session
                .query(wslc_core::model::schematree::schema_columns_sql())
                .unwrap_or_default();
            let mut tree = wslc_core::model::schematree::parse_schema_tree(
                &tables.first().map(to_tsv).unwrap_or_default(),
            );
            wslc_core::model::schematree::merge_columns(
                &mut tree,
                &columns.first().map(to_tsv).unwrap_or_default(),
            );
            Ok(tree)
        }
    }
}

/// MySQL 的读页面（原生连接）：探测结果 + 库列表 + 用户列表。
///
/// # 为什么要造一个 `MysqlProbe`
///
/// 数据库页的「库」「用户」两个 tab、以及连接卡上的"用的是哪个客户端"
/// 都吃 `MysqlProbe`。原生连接没有"容器里的客户端"这回事，所以这里造一个
/// 等价的：`family` 从 `DbKind` 推、`client.bin` 记为 `native`、
/// `root_hint` 留 `Unknown`（连接串里已经带了账号，不需要从环境变量猜）。
///
/// 这样 `load_db` 的后半段（`apply_probe` + 填库/用户 + 备份扫描）**一行都不用改**。
///
/// 错误用 [`NativeDbError`]（带 `wslc-db` 的分类）而不是 `anyhow::Error`：
/// `wslc-panel` 没有 anyhow 依赖，而这里需要的"分类 + 一句可显示的话"
/// 用一个本地类型就够，不必为它加依赖。
pub fn mysql_probe(
    text: &str,
    container: &str,
) -> Result<wslc_core::cmd::mysql::MysqlProbe, NativeDbError> {
    use wslc_core::cmd::mysql::MysqlProbe;
    use wslc_core::model::mysql::{ClientProbe, MysqlFamily, RootCredHint};

    let spec = parse(text)?;
    let version = server_version(&spec)?;
    let family = match spec.kind {
        DbKind::MariaDb => MysqlFamily::Mariadb,
        _ => MysqlFamily::Mysql,
    };
    Ok(MysqlProbe {
        family,
        client: ClientProbe {
            // ⚠️ 这不是二进制名，是 `DbState::NATIVE_CLIENT` 那个哨兵 ——
            // 谁把它塞进 `wslc exec` 就会报"找不到命令"。判据统一走
            // `DbState::is_native` / `write_blocked_reason`，别在这里比字符串。
            // 界面上它显示在连接卡上，让人一眼看出"这次没用容器里的客户端"。
            bin: wslc_panel_core::state::NATIVE_CLIENT.to_owned(),
            path: "（面板内建连接）".to_owned(),
            version: Some(version),
        },
        root_hint: RootCredHint::Unknown,
        container: container.to_owned(),
    })
}

/// MySQL 的库列表（原生连接）。复用 `-B -N` 的解析（见 `to_batch_tsv`）。
pub fn mysql_databases(
    text: &str,
) -> Result<Vec<wslc_core::model::mysql::DbEntry>, NativeDbError> {
    let spec = parse(text)?;
    let mut session = wslc_db::open(&spec)?;
    let tables = session.query(wslc_core::model::mysql::list_databases_sql())?;
    let tsv = tables.first().map(to_tsv).unwrap_or_default();
    Ok(wslc_core::model::mysql::parse_databases(&tsv))
}

/// MySQL 的用户列表（原生连接）。
///
/// ⚠️ 普通账号读不到 `mysql.user`（`ERROR 1142`）—— 调用方必须把它的失败
/// **单独**处理，不能连带把库列表一起判成失败（`load_db` 里就是这么做的）。
pub fn mysql_users(text: &str) -> Result<Vec<wslc_core::model::mysql::DbUser>, NativeDbError> {
    let spec = parse(text)?;
    let mut session = wslc_db::open(&spec)?;
    let tables = session.query(wslc_core::model::mysql::list_users_sql())?;
    let tsv = tables.first().map(to_tsv).unwrap_or_default();
    Ok(wslc_core::model::mysql::parse_users(&tsv))
}

/// `TextTable` → `-B -N` 那种 TSV 文本（复用 MySQL 侧的解析）。
fn to_tsv(table: &TextTable) -> String {
    wslc_core::model::mysql::to_batch_tsv(&table.rows)
}

/// `TextTable` → `SqlResult`。
///
/// 翻译本体在 [`SqlResult::from_cells`] —— 它住在 `wslc-core`，因为**那边
/// 有测试**（这个 crate 的 `#[test]` 不会被执行，见文件开头的说明）。
/// 这里只做参数搬运。
fn to_sql_result(table: &TextTable) -> SqlResult {
    SqlResult::from_cells(table.columns.clone(), &table.rows)
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

// ⚠️ 这个文件里**不要写 `#[cfg(test)] mod tests`**。
//
// CI 的测试任务是 `cargo test --workspace --exclude wslc-panel`（为了不把整棵
// GPUI 依赖树链接一遍，见 `AGENTS.md` §2），所以写在这里的断言**只编译、
// 不执行** —— 它看起来像守卫，实际是装饰。v0.16 起这里曾经躺着 3 条
// （DSN 解析 ×2 + NULL 边界 ×1），一次都没跑过。
//
// 该放哪儿：
// - DSN 解析 → `wslc-db/src/conn.rs`（已经覆盖了同样这几条，还更多）；
// - `TextTable` → `SqlResult` 的翻译 → `wslc_core::model::sqlresult`
//   （见 `SqlResult::from_cells`）；
// - 只有 GPUI 类型才能表达的东西 → 本来就不该测。
