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

// ---------------------------------------------------------------------------
// 备份 / 恢复（v0.8）
// ---------------------------------------------------------------------------
//
// 实测（wslc 3.0.3.0 / mysql 26.7.0，`docs/PLAN-v0.8.md` 记录全过程）：
//
// - **备份**：容器内 `sh -c 'mkdir -p <目录> && mysqldump … > <文件>'` 可行
//   （3.7 MB 的 `mysql` 系统库一次成功）；stdout 直接落**容器里**的文件
//   （sh 的重定向做的），所以 Windows 侧没有"大输出撑爆内存"的问题；
// - **拉回 Windows**：`wslc container cp <容器>:<路径> <本地>` **可用**
//   （3.7 MB 完整拉回）；⚠️ "本地→容器"方向实测是**坏的**
//   （`ERROR_PATH_NOT_FOUND`，连根目录都写不进）—— 那是 wslc 的 bug，
//   不是我们用法的问题；
// - **恢复**：容器内 `sh -c 'mysql … < <文件>'` 可行（dump 带
//   `CREATE DATABASE`，实测恢复后表数 / 行数与 dump 前一致）。

/// 一次备份的落点（两个都要）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupPaths {
    /// 容器内的绝对路径（恢复时从这里读）。
    pub container_path: String,
    /// Windows 侧的完整路径（留档，容器删了它还在）。
    pub windows_path: std::path::PathBuf,
}

/// 算一次备份的落点。
///
/// `data_dir` 是面板的数据目录（`%LOCALAPPDATA%\wslc-panel`），
/// 落点是 `<data_dir>/backups/<容器>/<文件名>` —— **路径里带容器名**，
/// 两个容器的备份互不混。
pub fn backup_paths(
    data_dir: &std::path::Path,
    container: &str,
    database: &str,
    now_secs: u64,
) -> Result<BackupPaths> {
    // 库名会进**文件名**和**容器内的路径** —— 非法名在这里就拦下
    //（`backup_database` 的第一件事也是这条校验，这里是让它更早失败）
    logic::validate_ident("库名", database)?;
    let file_name = logic::backup_file_name(database, now_secs);
    let windows_path = data_dir
        .join(logic::WINDOWS_BACKUP_SUBDIR)
        .join(container.trim())
        .join(&file_name);
    Ok(BackupPaths {
        container_path: format!("{}/{}", logic::CONTAINER_BACKUP_DIR, file_name),
        windows_path,
    })
}

/// 备份一个数据库：容器内 dump 到 `/tmp` → `container cp` 拉回 Windows。
///
/// # 返回
///
/// [`BackupPaths`] —— 界面要两个都报（容器内那份是恢复用的）。
///
/// # 为什么这一版没有实时进度
///
/// mysqldump 的输出直接落**容器内**的文件（sh 的重定向），Windows 侧
/// 拿不到数据流，所以界面上只有"正在备份…"和完成提示。
/// `Wslc::spawn_to_file`（stdout 落**本地**文件那条路）是给将来
/// "边 dump 边传"留的地基，见 `cli.rs` 的注释。
pub fn backup_database(
    wslc: &Wslc,
    container_name: &str,
    client_bin: &str,
    creds: &MysqlCreds,
    database: &str,
    paths: &BackupPaths,
) -> Result<BackupPaths> {
    logic::validate_ident("库名", database)?;

    // ① 容器内 dump。参数不含用户输入（库名已过白名单，无 shell 元字符），
    //    所以拼进 sh 脚本是安全的。
    let dump_args = logic::mysqldump_args(client_bin, creds, database)?;
    let inner = format!(
        "mkdir -p {dir} && {dump} > {out}",
        dir = logic::CONTAINER_BACKUP_DIR,
        dump = dump_args.join(" "),
        out = paths.container_path,
    );
    let exec_args = [
        "exec",
        "-e",
        &logic::exec_env(creds),
        container_name,
        "/bin/sh",
        "-c",
        inner.as_str(),
    ];
    if let Err(e) = wslc.run_checked(&exec_args) {
        let owned: Vec<String> = exec_args.iter().map(|s| (*s).to_owned()).collect();
        return Err(sanitize(e, &owned, &creds.password));
    }

    // ② 拉回 Windows 留档（容器→本地方向实测可用）。
    if let Some(parent) = paths.windows_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            Error::Io(std::io::Error::new(
                e.kind(),
                format!("建不出备份目录 {}：{e}", parent.display()),
            ))
        })?;
    }
    let cp_args = [
        "container",
        "cp",
        "-q",
        &format!("{}:{}", container_name, paths.container_path),
        &paths.windows_path.to_string_lossy(),
    ];
    wslc.run_checked(&cp_args).map_err(|e| {
        // cp 的错误不带 MYSQL_PWD（没有 -e），不用脱敏；但要把最常见的原因
        // 说在前头：容器重建过 / /tmp 被清了，那份容器内备份已经不在
        Error::Install(format!(
            "备份已生成在容器里（{}），但拉回 Windows 失败：{e}",
            paths.container_path
        ))
    })?;

    Ok(paths.clone())
}

/// 恢复一个数据库（**覆盖**同名库的数据 —— 调用方必须先让用户确认过）。
///
/// # 前提（界面上要说清）
///
/// dump 文件必须在**容器里**的 [`logic::CONTAINER_BACKUP_DIR`] 下。
/// "本地→容器"的拷贝方向实测不可用（见 [`backup_database`]），所以恢复
/// **只认本容器内**的备份 —— [`container_backup_exists`] 会在恢复前替
/// 用户问一句。
pub fn restore_database(
    wslc: &Wslc,
    container_name: &str,
    client_bin: &str,
    creds: &MysqlCreds,
    container_dump_path: &str,
) -> Result<()> {
    // 路径由面板生成（不是用户输入），但仍然防一手：sh 脚本里出现引号 /
    // 分号 / 空格都会被拆 —— 生成处已保证，这里再拦一次。
    if !container_dump_path
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.'))
    {
        return Err(Error::InvalidArgument(format!(
            "备份路径不安全：{container_dump_path}"
        )));
    }

    let script = logic::restore_script(client_bin, container_dump_path);
    let args = [
        "exec",
        "-e",
        &logic::exec_env(creds),
        container_name,
        "/bin/sh",
        "-c",
        script.as_str(),
    ];
    match wslc.run_checked(&args) {
        Ok(_) => Ok(()),
        Err(e) => {
            let owned: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
            Err(sanitize(e, &owned, &creds.password))
        }
    }
}

/// 检查容器里的备份文件还在不在（恢复前调，不在就给明确的一句话）。
pub fn container_backup_exists(
    wslc: &Wslc,
    container_name: &str,
    container_path: &str,
) -> Result<bool> {
    let out = wslc.run_checked(&["exec", container_name, "ls", container_path])?;
    // `ls` 对存在的文件输出文件名；不存在时退出码非零（走 Err）
    Ok(!out.stdout.trim().is_empty())
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

    // -- 备份 / 恢复（v0.8）--------------------------------------------------

    #[test]
    fn backup_paths_scope_by_container_and_database() {
        let dir = std::path::Path::new(r"C:\Users\x\AppData\Local\wslc-panel");
        let paths = backup_paths(dir, "mysql", "djxm", 1_792_036_741).expect("应能算出落点");
        assert_eq!(
            paths.container_path,
            "/tmp/wslc-backups/djxm-20261015-035901.sql"
        );
        assert_eq!(
            paths.windows_path,
            dir.join("backups")
                .join("mysql")
                .join("djxm-20261015-035901.sql")
        );
        // 另一个容器的落点必须不同（目录里带了容器名）
        let other = backup_paths(dir, "mysql2", "djxm", 1_792_036_741).unwrap();
        assert_ne!(paths.windows_path, other.windows_path);
        // 非法库名本地拦下
        assert!(backup_paths(dir, "mysql", "shop; drop", 0).is_err());
    }

    #[test]
    fn backup_rejects_bad_names_before_spawning() {
        // 参数校验要在起进程**之前**失败（和 lifecycle 命令同一个规矩）
        let wslc = Wslc::with_program("definitely-not-a-real-binary");
        let dir = std::env::temp_dir();
        let paths = backup_paths(&dir, "mysql", "shop", 0).unwrap();
        assert!(matches!(
            backup_database(&wslc, "mysql", "mysql", &creds(), "shop; drop", &paths),
            Err(Error::InvalidArgument(_))
        ));
    }

    #[test]
    fn restore_rejects_unsafe_paths_before_spawning() {
        let wslc = Wslc::with_program("definitely-not-a-real-binary");
        // 面板生成的路径只有字母数字 / `/-_.` —— 带空格或分号的一律拒掉
        assert!(matches!(
            restore_database(&wslc, "mysql", "mysql", &creds(), "/tmp/a b.sql"),
            Err(Error::InvalidArgument(_))
        ));
        assert!(matches!(
            restore_database(&wslc, "mysql", "mysql", &creds(), "/tmp/a;rm.sql"),
            Err(Error::InvalidArgument(_))
        ));
    }

    #[test]
    fn backup_with_a_bogus_binary_fails_cleanly() {
        // 端到端的"起了进程但跑不动"路径：可执行文件不存在 → ExecutableNotFound
        let wslc = Wslc::with_program("definitely-not-a-real-binary");
        let dir = std::env::temp_dir();
        let paths = backup_paths(&dir, "mysql", "shop", 0).unwrap();
        match backup_database(&wslc, "mysql", "mysql", &creds(), "shop", &paths) {
            Err(Error::ExecutableNotFound { .. }) => {}
            other => panic!("不应出现其它结果：{other:?}"),
        }
    }
}
