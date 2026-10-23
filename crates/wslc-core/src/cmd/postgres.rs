//! PostgreSQL 的执行层：拼命令行、跑 `wslc exec`、把输出交回去。
//!
//! 形状和 `cmd::mysql` 一模一样（[`exec_argv`] → `wslc.run_checked` → 解析），
//! 但**参数和解析都不同** —— 差异全部来自实测，见 `docs/postgres-measured.md`。
//!
//! 这一层只负责"把命令跑出去"；拼什么参数、怎么解析在
//! `model::postgres`（纯函数，能脱离进程单测）。

use crate::cli::Wslc;
use crate::error::{Error, Result};
use crate::model::mysql as redact;
use crate::model::postgres as logic;
use crate::model::schematree::SchemaTree;
use crate::model::sqlresult::SqlResult;

/// 容器里的客户端二进制。
///
/// PostgreSQL 官方镜像只装 `psql`（没有第二个兼容客户端），
/// 所以这里不像 MySQL 那样需要探测。
pub const CLIENT_BIN: &str = "psql";

/// 拼出 `wslc exec …` 的完整参数。
///
/// ⚠️ 密码走 `-e PGPASSWORD=…` 环境变量，**不进命令行参数**
/// （`psql` 接受 `-W` 交互提示，但没有"命令行传口令"的选项；
/// 走环境变量也顺便避免了 `ps` 看到口令）。
pub fn exec_argv(
    container_name: &str,
    user: &str,
    password: &str,
    database: Option<&str>,
    sql: &str,
) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "exec".to_owned(),
        "-e".to_owned(),
        logic::exec_env(password),
        container_name.to_owned(),
        CLIENT_BIN.to_owned(),
    ];
    args.extend(logic::client_args(user, database, sql));
    args
}

/// 把错误里的**密码**洗掉再交出去。
///
/// 和 `cmd::mysql::sanitize` 同一个理由，两处都要洗：
///
/// 1. 参数串里的 `-e PGPASSWORD=…`；
/// 2. 子进程输出 —— 用户在 SQL 里写了 `CREATE ROLE x PASSWORD '…'` 并出错时，
///    PostgreSQL 会把整条语句回显在 stderr 里，那里面没有 `PGPASSWORD=`
///    的形状，只能拿口令原文去比对。
fn sanitize(err: Error, args: &[String], password: &str) -> Error {
    let redacted = redact::redact_password(&redact::redact_args(args).join(" "), password);
    match err {
        Error::NonZeroExit {
            program,
            code,
            stderr,
            ..
        } => Error::NonZeroExit {
            program,
            args: redacted,
            code,
            stderr: redact::redact_password(&stderr, password),
        },
        Error::Timeout {
            program, timeout, ..
        } => Error::Timeout {
            program,
            args: redacted,
            timeout,
        },
        Error::Cancelled { program, .. } => Error::Cancelled {
            program,
            args: redacted,
        },
        other => other,
    }
}

/// 跑一条 SQL，返回**标准输出**（已经是 CSV）。
pub fn run_sql(
    wslc: &Wslc,
    container_name: &str,
    user: &str,
    password: &str,
    database: Option<&str>,
    sql: &str,
) -> Result<String> {
    let args = exec_argv(container_name, user, password, database, sql);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    match wslc.run_checked(&refs) {
        Ok(out) => Ok(out.stdout),
        Err(e) => Err(sanitize(e, &args, password)),
    }
}

/// 执行一段脚本 —— **逐条**执行，每条一个结果集。
///
/// # 为什么逐条
///
/// 实测：`psql --csv -c "SELECT 1; SELECT 2"` 的输出是两个结果集直接粘连，
/// 中间没有任何分隔，分不清哪行属于哪个（和 MySQL 那边同一个问题）。
///
/// # 代价
///
/// 每条语句一次 `wslc exec`，固定开销约 70ms。所以界面上的提示写的是
/// "多条语句逐条执行（每条约 70ms）"。
///
/// # 判据
///
/// PostgreSQL 这边不需要猜"这条语句会不会返回行"：
/// **零字节输出 = 没有结果集**（`-q` 把 DDL 的命令标签压掉了，实测）。
pub fn run_script(
    wslc: &Wslc,
    container_name: &str,
    user: &str,
    password: &str,
    database: Option<&str>,
    script: &str,
) -> Result<Vec<SqlResult>> {
    let statements = logic::split_statements(script);
    if statements.is_empty() {
        return Err(Error::InvalidArgument(
            "没有可执行的语句（只有空白或注释）".to_owned(),
        ));
    }
    let mut results = Vec::with_capacity(statements.len());
    for stmt in statements {
        let raw = run_sql(wslc, container_name, user, password, database, &stmt)?;
        let table = logic::parse_csv(&raw);
        results.push(SqlResult {
            columns: table.header,
            rows: table.rows,
        });
    }
    Ok(results)
}

/// 列出所有库。
pub fn list_databases(
    wslc: &Wslc,
    container_name: &str,
    user: &str,
    password: &str,
) -> Result<Vec<String>> {
    let raw = run_sql(
        wslc,
        container_name,
        user,
        password,
        None,
        logic::databases_sql(),
    )?;
    Ok(logic::parse_databases(&logic::parse_csv(&raw)))
}

/// 读对象树（schema → 表 → 列）。
///
/// # 为什么要指定 `database`
///
/// PostgreSQL 的 `information_schema` 是**按库隔离**的：不指定库时
/// 看到的是连接默认库里的东西。要"所有库的表"就得为每个库各跑一次 exec。
///
/// 所以这里要求传当前库（`None` = 用客户端默认库）。
pub fn list_schema_tree(
    wslc: &Wslc,
    container_name: &str,
    user: &str,
    password: &str,
    database: Option<&str>,
) -> Result<SchemaTree> {
    let tables = run_sql(
        wslc,
        container_name,
        user,
        password,
        database,
        logic::schema_tables_sql(),
    )?;
    // 列读失败不算整体失败：树本身有用（能点表名生成 SELECT），
    // 补全只是少一档候选。和 MySQL 那边同一个取舍。
    let columns = run_sql(
        wslc,
        container_name,
        user,
        password,
        database,
        logic::schema_columns_sql(),
    )
    .unwrap_or_default();
    Ok(logic::parse_schema_tree(
        &logic::parse_csv(&tables),
        &logic::parse_csv(&columns),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_puts_password_in_env_not_on_the_command_line() {
        let a = exec_argv("pg", "postgres", "s3cret", Some("shop"), "SELECT 1");
        assert_eq!(a[0], "exec");
        assert_eq!(a[1], "-e");
        assert_eq!(a[2], "PGPASSWORD=s3cret");
        assert_eq!(a[3], "pg");
        assert_eq!(a[4], "psql");
        // ⚠️ 口令绝不能出现在 psql 的参数里（`ps` 能看到进程参数）
        assert!(
            !a[5..].iter().any(|x| x.contains("s3cret")),
            "口令漏进参数了：{a:?}"
        );
        // 客户端参数照实测配方
        assert!(a.contains(&"--csv".to_owned()));
        assert!(a.contains(&"-q".to_owned()));
        assert!(!a.contains(&"-A".to_owned()));
    }

    #[test]
    fn argv_omits_dash_d_without_a_database() {
        let a = exec_argv("pg", "postgres", "pw", None, "SELECT 1");
        assert!(!a.contains(&"-d".to_owned()));
    }
}
