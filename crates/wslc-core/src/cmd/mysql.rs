//! MySQL 相关命令：在**运行中的容器里**跑 `mysql` 客户端。
//!
//! 纯逻辑（识别 / 拼参数 / 解析 / SQL 生成 / 错误分类）都在
//! [`crate::model::mysql`]；这里只负责"起进程"和"把结果交出去"，
//! 以及一件**绝不能忘**的事：**错误消息里的参数要脱敏**。
//!
//! # 为什么错误要在这里重建
//!
//! [`Error::NonZeroExit`] 的 `Display` 会把 `args` 拼进消息，而我们的参数里
//! 带着 `-e MYSQL_PWD=<明文密码>`。直接把这个错误交给界面，等于**每次 SQL 报错
//! 都把密码打进提示条**（提示条还会被用户截图、被日志收走）。
//! 所以这里出口只有一个：[`sanitize`]。

use crate::cli::Wslc;
use crate::error::{Error, Result};
use crate::model::mysql as logic;
use crate::model::mysql::{ClientProbe, DbEntry, DbUser, MysqlCreds, MysqlFamily, RootCredHint, SqlMode};

use super::container;

/// 一次探测的结果：是不是 MySQL + 用哪个客户端 + root 凭据线索。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MysqlProbe {
    /// 家族（MySQL / MariaDB / Percona）。
    pub family: MysqlFamily,
    /// 容器里的客户端（二进制名 + 版本）。
    pub client: ClientProbe,
    /// 从容器环境变量读出来的 root 凭据线索。
    pub root_hint: RootCredHint,
    /// 容器名（**不带**前导 `/`），界面直接用。
    pub container: String,
}

/// 探测一个容器：是不是 MySQL、客户端在哪、root 密码有没有现成的。
///
/// 起**两个**子进程：`wslc inspect`（权威判定 + 读环境变量）和一次客户端探测。
/// 都在后台执行器上跑（`AGENTS.md` §7.1），进页面时跑一次就够。
pub fn detect(wslc: &Wslc, container_name: &str) -> Result<MysqlProbe> {
    let inspect = container::inspect(wslc, container_name, false)?;
    let family = logic::family_from_inspect(&inspect).ok_or_else(|| {
        Error::Unsupported(format!(
            "{container_name} 看起来不是 MySQL / MariaDB 容器\
             （镜像、环境变量、启动命令、端口里都没有相关线索）"
        ))
    })?;
    let root_hint = logic::root_credential_from_env(&inspect.env());
    let client = probe_client(wslc, container_name)?;

    Ok(MysqlProbe {
        family,
        client,
        root_hint,
        container: container_name.to_owned(),
    })
}

/// 在容器里找 mysql / mariadb 客户端，并读出它的版本。
///
/// 找不到就报错 —— 但是**报哪一句**很要紧：`wslc exec` 自己给的是
/// `OCI runtime exec failed: … executable file not found in $PATH`（英文、退出码 126），
/// 所以这里换成一句中文，并把原文附在后面（排障时还是原文有用）。
pub fn probe_client(wslc: &Wslc, container_name: &str) -> Result<ClientProbe> {
    let script = logic::client_probe_script();
    let args = ["exec", container_name, "/bin/sh", "-c", script];
    match wslc.run_checked(&args) {
        Ok(out) => logic::parse_client_probe(&out.stdout).ok_or_else(|| {
            Error::Parse(format!(
                "在 {container_name} 里没找到可用的 mysql / mariadb 客户端\
                 （探测脚本的输出看不懂：{:?}）",
                out.stdout.trim()
            ))
        }),
        Err(e) => {
            let kind = logic::classify_error(&e.to_string(), None);
            Err(Error::Unsupported(match kind {
                logic::MysqlErrorKind::ClientMissing => format!(
                    "{container_name} 里没有 mysql / mariadb 客户端 —— 面板靠容器自带的\
                     客户端执行 SQL，所以这个镜像用不了。（原文：{e}）"
                ),
                logic::MysqlErrorKind::ContainerNotRunning => {
                    format!("{container_name} 没在运行，先启动它。（原文：{e}）")
                }
                _ => format!("探测 {container_name} 的 MySQL 客户端失败：{e}"),
            }))
        }
    }
}

/// 拼一条完整的 `wslc exec` 参数（**纯函数**，便于单测）。
///
/// 形状：`exec -e MYSQL_PWD=<密码> <容器> <客户端> -u <用户> … -e <SQL>`。
///
/// ⚠️ 两条不变量：
/// 1. `-e MYSQL_PWD=…` 是**选项**，必须出现在 `<容器>` **之前** ——
///    位置参数一旦开始，后面的开关都会被当成命令的一部分
///    （和 `cmd::container::logs_args` 里那条注释同一个坑）；
/// 2. 密码只在这里出现一次，而且**不在** mysql 的参数里（`-p` 那种写法会把
///    密码暴露在容器内的进程列表里）。
pub fn exec_argv(
    container_name: &str,
    client_bin: &str,
    creds: &MysqlCreds,
    sql: &str,
    database: Option<&str>,
    mode: SqlMode,
) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "exec".to_owned(),
        "-e".to_owned(),
        logic::exec_env(creds),
        container_name.to_owned(),
    ];
    args.extend(logic::client_args(
        client_bin, creds, sql, database, mode,
    ));
    args
}

/// 把错误里的**密码**洗掉之后交出去。
///
/// 洗两处，两种手法都得用上：
///
/// 1. **按形状**洗参数串（`-e MYSQL_PWD=…`），见 [`logic::redact_args`]；
/// 2. **按值**洗文本（参数串和子进程输出都要）——
///    `CREATE USER … IDENTIFIED BY 'x'` 出错时，MySQL 会把**整条语句**
///    回显在 stderr 里，而那条语句里的密码既不长得像 `MYSQL_PWD=…`、
///    也不是 `-p…`，只有拿密码原文去比对才洗得掉。
fn sanitize(err: Error, args: &[String], password: &str) -> Error {
    let redacted = logic::redact_password(&logic::redact_args(args).join(" "), password);
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
            stderr: logic::redact_password(&stderr, password),
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
        // 其余变体不含参数（`InvalidArgument` / `Parse` / `Io` …），原样返回
        other => other,
    }
}

/// 执行一条 SQL，返回**标准输出**（错误已经脱敏）。
///
/// `mode` 决定输出形态：解析用 [`SqlMode::Batch`]，给人看用 [`SqlMode::Table`]。
pub fn run_sql(
    wslc: &Wslc,
    container_name: &str,
    client_bin: &str,
    creds: &MysqlCreds,
    sql: &str,
    database: Option<&str>,
    mode: SqlMode,
) -> Result<String> {
    let args = exec_argv(container_name, client_bin, creds, sql, database, mode);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    match wslc.run_checked(&refs) {
        Ok(out) => Ok(out.stdout),
        Err(e) => Err(sanitize(e, &args, &creds.password)),
    }
}

/// 列出所有库（**一次** exec 拿全部信息：库名 / 字符集 / 排序规则 / 表数 / 占用）。
pub fn list_databases(
    wslc: &Wslc,
    container_name: &str,
    client_bin: &str,
    creds: &MysqlCreds,
) -> Result<Vec<DbEntry>> {
    let out = run_sql(
        wslc,
        container_name,
        client_bin,
        creds,
        logic::list_databases_sql(),
        None,
        SqlMode::Batch,
    )?;
    Ok(logic::parse_databases(&out))
}

/// 列出所有用户（`mysql.user`）。
///
/// ⚠️ 普通账号读不到这张表（`ERROR 1142 … command denied`），
/// 那不是 bug，界面按 [`logic::MysqlErrorKind::PermissionDenied`] 解释。
pub fn list_users(
    wslc: &Wslc,
    container_name: &str,
    client_bin: &str,
    creds: &MysqlCreds,
) -> Result<Vec<DbUser>> {
    let out = run_sql(
        wslc,
        container_name,
        client_bin,
        creds,
        logic::list_users_sql(),
        None,
        SqlMode::Batch,
    )?;
    Ok(logic::parse_users(&out))
}

/// 用户敲的一条 SQL（**原样**执行，不加任何包装）。
///
/// 用 [`SqlMode::Table`]：stdout 不是 tty 时 mysql 默认是 batch，
/// 不显式要表格的话控制台里会是一堆 tab。
pub fn run_console_sql(
    wslc: &Wslc,
    container_name: &str,
    client_bin: &str,
    creds: &MysqlCreds,
    sql: &str,
    database: Option<&str>,
) -> Result<String> {
    run_sql(
        wslc,
        container_name,
        client_bin,
        creds,
        sql,
        database,
        SqlMode::Table,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn creds() -> MysqlCreds {
        MysqlCreds::root("s3cret")
    }

    #[test]
    fn exec_argv_puts_the_env_option_before_the_container() {
        let args = exec_argv(
            "mysql",
            "mysql",
            &creds(),
            "SELECT 1",
            None,
            SqlMode::Batch,
        );
        assert_eq!(
            args,
            vec![
                "exec",
                "-e",
                "MYSQL_PWD=s3cret",
                "mysql",
                "mysql",
                "-u",
                "root",
                "-N",
                "-B",
                "-e",
                "SELECT 1",
            ]
        );

        // `-e MYSQL_PWD=…` 必须在容器名**之前** —— 位置参数开始之后，
        // `wslc` 会把后面的开关当成命令的一部分
        let container_at = args.iter().position(|a| a == "mysql").unwrap();
        let env_at = args.iter().position(|a| a == "MYSQL_PWD=s3cret").unwrap();
        assert!(env_at < container_at, "{args:?}");
        // SQL 永远是最后一个参数
        assert_eq!(args.last().map(String::as_str), Some("SELECT 1"));
    }

    #[test]
    fn sanitize_hides_the_password_from_args_and_stderr() {
        let args = exec_argv(
            "mysql",
            "mysql",
            &creds(),
            "CREATE USER 'a'@'%' IDENTIFIED BY 's3cret'",
            None,
            SqlMode::Batch,
        );
        let raw = Error::NonZeroExit {
            program: "wslc",
            args: args.join(" "),
            code: 1,
            stderr: "ERROR 1064 (42000): … near 'IDENTIFIED BY \\'s3cret\\'' at line 1".to_owned(),
        };
        let err = sanitize(raw, &args, "s3cret");
        let text = err.to_string();
        assert!(!text.contains("s3cret"), "错误消息里漏了密码：{text}");
        assert!(text.contains("MYSQL_PWD=***"), "{text}");
        assert!(text.contains("ERROR 1064"), "原文该留着，方便排障：{text}");
    }

    #[test]
    fn sanitize_also_covers_timeout_and_cancelled() {
        let args = exec_argv("mysql", "mysql", &creds(), "SELECT 1", None, SqlMode::Batch);

        let timeout = sanitize(
            Error::Timeout {
                program: "wslc",
                args: args.join(" "),
                timeout: std::time::Duration::from_secs(30),
            },
            &args,
            "s3cret",
        );
        assert!(!timeout.to_string().contains("s3cret"), "{timeout}");

        let cancelled = sanitize(
            Error::Cancelled {
                program: "wslc",
                args: args.join(" "),
            },
            &args,
            "s3cret",
        );
        assert!(!cancelled.to_string().contains("s3cret"), "{cancelled}");
    }

    #[test]
    fn sanitize_passes_through_errors_that_carry_no_arguments() {
        let err = sanitize(
            Error::InvalidArgument("库名不能为空".to_owned()),
            &[],
            "s3cret",
        );
        assert_eq!(err.to_string(), "参数错误：库名不能为空");
    }

    #[test]
    fn empty_password_never_mangles_the_message() {
        // 空密码是合法配置（`MYSQL_ALLOW_EMPTY_PASSWORD=yes`）——
        // 拿空串当 pattern 会把整段文本拆烂
        let creds = MysqlCreds::root("");
        let args = exec_argv("mysql", "mysql", &creds, "SELECT 1", None, SqlMode::Batch);
        let raw = Error::NonZeroExit {
            program: "wslc",
            args: args.join(" "),
            code: 1,
            stderr: "ERROR 1045 (28000): Access denied".to_owned(),
        };
        let err = sanitize(raw, &args, "");
        assert!(err.to_string().contains("Access denied"), "{err}");
    }
}
