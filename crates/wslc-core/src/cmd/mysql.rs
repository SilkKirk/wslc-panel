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
use crate::model::mysql::{
    ClientProbe, DbEntry, DbUser, MysqlCreds, MysqlFamily, RootCredHint, SqlMode,
};
// SQL 控制台的结构化结果：`SqlResult` 是它的类型，
// `sqlresult` / `sqlstmt` 是纯逻辑（住在 `model/` 里，能脱离 Windows 单测）。
use crate::model::sqlresult::{self, SqlResult};
use crate::model::sqlstmt;
// 库 / 表树的类型与解析（纯逻辑在 `model/schematree.rs`）。
use crate::model::schematree::{self, SchemaTree};

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
    args.extend(logic::client_args(client_bin, creds, sql, database, mode));
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

/// 读整棵「库 / 表树」（SQL 控制台左侧那个）。
///
/// # 一条 SQL 拿全
///
/// 实测这台机器上 `djxm` 库有 117 张表，而 `information_schema.TABLES`
/// 一条查询就把全部（118 行 TSV，0.08s）返回了。反过来"展开一个库才查一次"
/// 要多花 N 次 `wslc exec`（每次固定 70ms）。
///
/// 用 [`SqlMode::Batch`]（`-N -B`，**不带**表头）：列名是我们自己知道的，
/// 少打一行少解析一行。
pub fn list_schema_tree(
    wslc: &Wslc,
    container_name: &str,
    client_bin: &str,
    creds: &MysqlCreds,
) -> Result<SchemaTree> {
    let out = run_sql(
        wslc,
        container_name,
        client_bin,
        creds,
        schematree::schema_tree_sql(),
        // 不指定库：`information_schema` 是全库可见的，指定了反而多一层依赖
        None,
        SqlMode::Batch,
    )?;
    let mut tree = schematree::parse_schema_tree(&out);

    // 第二条查询：列名（v0.12.4，自动补全要能补出 `别名.列名`）。
    //
    // 代价是**多一次 `wslc exec`**（固定开销约 70ms，这条本身 0.15s 量级）。
    // 为什么不合成一条 JOIN：`TABLES` 和 `COLUMNS` 是不同粒度，JOIN 出来
    // 会重复表信息、解析更绕；两条各自列序清晰。
    //
    // ⚠️ 列读失败**不算整体失败**：树本身有用（能点表名生成 SELECT），
    // 补全只是锦上添花 —— 所以这里吞掉错误，只是补全少一档候选。
    if let Ok(columns) = run_sql(
        wslc,
        container_name,
        client_bin,
        creds,
        schematree::schema_columns_sql(),
        None,
        SqlMode::Batch,
    ) {
        schematree::merge_columns(&mut tree, &columns);
    }

    Ok(tree)
}

/// 用户在 SQL 控制台里敲的一段脚本 —— **切分后逐条执行**，每条一个结果集。
///
/// # 为什么不是"一次执行整段"
///
/// 实测：`mysql -B -e "SELECT 1 AS a; SELECT 2 AS b"` 的输出是两个结果集
/// **直接粘连**（`a` / `1` / `b` / `2`，中间没有任何分隔），分不清哪行属于哪个。
/// 逐条执行就没有这个问题。切分规则见 [`sqlstmt::split_statements`]。
///
/// # 代价
///
/// 每条语句一次 `wslc exec`，实测固定开销 **~70ms**。3 条 0.26s ——
/// 交互式够用，但**一次几十条会明显变慢**（界面上要说明）。
///
/// # 遇到第一条报错就停
///
/// 和 `mysql` 客户端自己跑脚本的行为一致（不加 `--force` 时它会中止）。
/// 已经跑完的那些结果**照样返回**，`Err` 里带上"第几条出错"——
/// 否则用户看到"失败了"却不知道前面几条已经生效了，那是更糟的状态。
pub fn run_statements(
    wslc: &Wslc,
    container_name: &str,
    client_bin: &str,
    creds: &MysqlCreds,
    sql: &str,
    database: Option<&str>,
) -> Result<Vec<SqlResult>> {
    let statements = sqlstmt::split_statements(sql);
    if statements.is_empty() {
        return Err(Error::InvalidArgument(
            "没有可执行的语句（只有空白或注释）".to_owned(),
        ));
    }

    let mut results = Vec::with_capacity(statements.len());
    for (ix, stmt) in statements.iter().enumerate() {
        match run_sql(
            wslc,
            container_name,
            client_bin,
            creds,
            stmt,
            database,
            SqlMode::BatchWithHeader,
        ) {
            Ok(stdout) => results.push(sqlresult::parse_batch_with_header(&stdout)),
            Err(e) => {
                // 报错里说清"第几条"和"前几条已经生效了" —— 后者最要紧：
                // 用户以为整段失败、其实前面几条已经改了数据。
                let done = if results.is_empty() {
                    "还没有语句生效".to_owned()
                } else {
                    format!("前 {} 条已经执行成功", results.len())
                };
                return Err(Error::Unsupported(format!(
                    "第 {} 条语句出错（共 {} 条，{done}）：{e}\n\n出错的语句：\n{stmt}",
                    ix + 1,
                    statements.len(),
                )));
            }
        }
    }
    Ok(results)
}

/// 执行一条语句并返回**原始**的 `--table` 输出（"看原始输出"的兜底）。
///
/// 和 [`run_statements`] 分开：那个要结构化数据，这个要"mysql 原样打了什么"——
/// 排查列宽异常、字符集问题时，原始输出比任何解析结果都有说服力。
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
// 实测（wslc 3.0.3.0 / mysql 26.7.0）：
//
// - **备份**：容器内 `sh -c 'mkdir -p <目录> && mysqldump … > <文件>'` 可行
//   （3.7 MB 的 `mysql` 系统库一次成功）；stdout 直接落**容器里**的文件
//   （sh 的重定向做的），所以 Windows 侧没有"大输出撑爆内存"的问题；
//   ⚠️ **程序名必须是 `mysqldump`，不是客户端 `mysql`** —— v0.9.6 实测：
//   把 mysqldump 的参数喂给 `mysql` 客户端是 `unknown option '--databases'`、
//   退出码 2，而 `>` 已经在容器里留下一个 **0 字节**文件。推导在
//   [`logic::dump_bin`]（收客户端名、吐 dump 工具名），别在调用方手拼；
//   同一台机器上 `mysqldump` 版实测：exit 0、110 张表 / 2.32 MB，
//   `container cp` 回 Windows 后 **sha256 一致**；
// - **拉回 Windows**：`wslc container cp <容器>:<路径> <本地目录>` **可用**
//   （3.7 MB 完整拉回）；
// - **送进容器**：`wslc container cp <本地文件> <容器>:<已存在的目录>/`
//   **可用**（v0.8.2 才测对 —— ⚠️ 目标是**目录**，写成文件名会报
//   `ERROR_PATH_NOT_FOUND`。我 v0.8.0 只试了后一种写法就误判成"这条路是坏的"，
//   完整矩阵在 `model::mysql::restore_container_path` 的文档注释里）；
// - **恢复**：容器内 `sh -c 'mysql … < <文件>'` 可行（dump 带
//   `CREATE DATABASE`，实测恢复后表数 / 行数与 dump 前一致）。
//
// 恢复**以 Windows 侧存档为准**：dump 从本地 cp 进
// [`logic::CONTAINER_RESTORE_DIR`] 再重定向。这样"容器重建过 / `/tmp` 被清过"
// 都不再是障碍（v0.8.1 及以前只认容器内那份，那个限制现在没有理由存在）。

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
/// `client_bin` 收的是**客户端**（探测得来的 `mysql` / `mariadb`）——
/// dump 工具（`mysqldump` / `mariadb-dump`）由 [`logic::dump_bin`] 自己推导。
/// 别把这个参数当成"dump 程序"用：v0.9.6 就是这么错的（详见模块注释）。
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
///
/// # 只认退出码（实测过的一个坑）
///
/// mysqldump 会因为**权限不够但无关紧要**的两件事往 stderr 打 `Error:` 字样
/// （实测 26.7.0：`column_masking_policy` 少 SELECT、tablespaces 少 PROCESS），
/// 但**退出码仍然是 0、dump 是完整的**。所以这里只看退出码，不拿 stderr
/// 去判失败 —— 否则一个正常的备份会被判成失败。
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

/// 把容器里的一份 `.sql` 喂给 `mysql`（**覆盖**同名库的数据 —— 恢复走这条，
/// 导入也走这条；调用方必须先让用户确认过）。
///
/// # 前提
///
/// `container_sql_path` 必须是**容器里真实存在**的文件。调用方一般先用
/// [`push_dump_to_container`] 把本地那份送进去，拿到路径再调这里。
///
/// # 参数
///
/// `database`：
/// - `None` —— 由文件里的语句决定（`mysqldump --databases` 出来的 dump
///   自带 `CREATE DATABASE` / `USE`，**恢复**就是这条）；
/// - `Some(db)` —— `mysql -D <db>`，往指定库里灌（**导入**用户自己的 dump
///   多半要它：`mysqldump <db> > f.sql` 那种没有 USE，不给库名 mysql 会报
///   "No database selected"）。
pub fn load_sql_file(
    wslc: &Wslc,
    container_name: &str,
    client_bin: &str,
    creds: &MysqlCreds,
    container_sql_path: &str,
    database: Option<&str>,
) -> Result<()> {
    // 脚本是 `sh -c` 一行，路径和库名都会**过一次 shell**：
    // `load_script` 负责把它们包成单引号字面量（`'` 那种没法安全表达的
    // 直接拒掉，而不是拼出一条会被拆开的命令）。
    let script = logic::load_script(client_bin, container_sql_path, database)?;
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

/// 把 **Windows 侧**的 dump 送进容器，返回它在容器里的绝对路径。
///
/// # 三步（顺序不能换）
///
/// ① `wslc exec <容器> /bin/sh -c 'mkdir -p <目录>'` —— 目标目录**必须先存在**：
///    `cp` 的目录目标不存在时报 `ERROR_PATH_NOT_FOUND`（实测）；
/// ② `wslc container cp <本地 dump> <容器>:<目录>/` —— ⚠️ 目标要写成**目录**
///    （带尾斜杠），写成文件名会报同一个错（这是 v0.8.0 误判"这条路是坏的"
///    的原因，完整矩阵在 [`logic::restore_container_path`] 的文档注释里）；
/// ③ 文件名**保持本地那个**（cp 的语义是把源文件放进目标目录），
///    所以路径由 [`logic::restore_container_path`] 算得出来。
///
/// # 为什么恢复 / 导入都要重新送一份（而不是用容器里剩下的）
///
/// 容器内 `/tmp` 那份会随容器重建而消失（实测踩到过）。备份列表上的每一条
/// 都有 Windows 侧的文件，送一份进去是**确定能成功**的那条路。
///
/// # 文件名：用户选的那些也要能进来
///
/// 这一步进来的不只是面板生成的 `<库名>-<时间>.sql`，还有**用户自己挑的**
/// 导入文件（`shop backup (1).sql`、`订单数据.sql` 都很常见）。实测
/// `container cp` 对这类名字没问题（它是 argv，不过 shell），而后面
/// `mysql < 它` 那一步过 shell —— 所以这里只按 [`logic::sh_quote`] 的
/// 口径拦（单引号 / 换行），**不再要求 ASCII 白名单**。
pub fn push_dump_to_container(
    wslc: &Wslc,
    container_name: &str,
    windows_dump: &std::path::Path,
) -> Result<String> {
    let Some(file_name) = windows_dump.file_name().and_then(|n| n.to_str()) else {
        return Err(Error::InvalidArgument(format!(
            "备份文件名读不出来：{}",
            windows_dump.display()
        )));
    };
    if file_name.is_empty() {
        return Err(Error::InvalidArgument("文件名是空的".to_owned()));
    }
    // 让这一步就报出"这个名字过不了 shell"，而不是留到容器里变成
    // 一条被拆开的命令（错误消息也更好懂）
    logic::sh_quote(file_name)?;

    // ① 目标目录必须先存在
    let mkdir = format!("mkdir -p {}", logic::CONTAINER_RESTORE_DIR);
    wslc.run_checked(&["exec", container_name, "/bin/sh", "-c", mkdir.as_str()])?;

    // ② 源文件放进那个目录（目标写成**目录**，带尾斜杠）
    let target_dir = format!("{}:{}/", container_name, logic::CONTAINER_RESTORE_DIR);
    let local = windows_dump.to_string_lossy();
    wslc.run_checked(&["container", "cp", local.as_ref(), target_dir.as_str()])
        .map_err(|e| {
            // cp 的错误里没有密码（没带 -e），不用脱敏；但把"从哪来、到哪去"
            // 说清楚 —— 这一步失败最常见的原因是文件被手动删了
            Error::Install(format!(
                "把 .sql 送进容器失败（{} → {}/）：{e}",
                windows_dump.display(),
                logic::CONTAINER_RESTORE_DIR
            ))
        })?;

    Ok(logic::restore_container_path(file_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn creds() -> MysqlCreds {
        MysqlCreds::root("s3cret")
    }

    #[test]
    fn exec_argv_puts_the_env_option_before_the_container() {
        let args = exec_argv("mysql", "mysql", &creds(), "SELECT 1", None, SqlMode::Batch);
        assert_eq!(
            args,
            vec![
                "exec",
                "-e",
                "MYSQL_PWD=s3cret",
                "mysql",
                "mysql",
                // ⚠️ 服务器名（第 4 项）之后紧接着客户端和它的字符集开关 ——
                // 少了它，容器里（没有 LANG）客户端退回 latin1，中文会坏。
                "--default-character-set=utf8mb4",
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
    fn load_sql_file_rejects_only_what_the_shell_cannot_express() {
        let wslc = Wslc::with_program("definitely-not-a-real-binary");
        // 没法安全表达的（单引号 / 换行）→ 起进程**之前**就报参数错
        for bad in ["/tmp/a'b.sql", "/tmp/a\nb.sql"] {
            assert!(
                matches!(
                    load_sql_file(&wslc, "mysql", "mysql", &creds(), bad, None),
                    Err(Error::InvalidArgument(_))
                ),
                "{bad:?} 应被拒"
            );
            assert!(
                matches!(
                    load_sql_file(&wslc, "mysql", "mysql", &creds(), "/tmp/x.sql", Some(bad)),
                    Err(Error::InvalidArgument(_))
                ),
                "库名 {bad:?} 应被拒"
            );
        }
        // ⚠️ 空格 / 分号 / 中文**不是**问题：它们会被包进单引号里，
        // 所以这两条应当走到"起进程"那一步（可执行文件不存在 → 另一个错）。
        for ok in ["/tmp/a b.sql", "/tmp/a;b.sql", "/tmp/订单.sql"] {
            assert!(
                matches!(
                    load_sql_file(&wslc, "mysql", "mysql", &creds(), ok, None),
                    Err(Error::ExecutableNotFound { .. })
                ),
                "{ok:?} 不该在参数校验就被拒"
            );
        }
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

    #[test]
    fn pushing_a_dump_rejects_unsafe_file_names_first() {
        // 文件名会进容器内的路径（`sh -c 'mysql < <路径>'`），所以
        // "过不了 shell"的名字必须在**起进程之前**被拦下。
        // ⚠️ 但**只有**单引号 / 换行过不了 —— 用户挑的导入文件叫
        // `shop backup (1).sql` / `订单数据.sql` 都很正常，实测
        // `container cp` 对这些名字没问题，所以不能再按 ASCII 白名单拒。
        let wslc = Wslc::with_program("definitely-not-a-real-binary");
        for bad in [
            r"C:\backups\it's.sql", // 单引号
            // ⚠️ 别写 `r"C:\backups\"` —— `Path::file_name()` 会**忽略尾部分隔符**
            // 返回 `Some("backups")`（`components()` 的行为），那条会走到
            // 后半段而不是这里。真正"没有文件名"的是盘根 / `..` 这种。
            r"C:\", // 盘根：file_name() == None
        ] {
            match push_dump_to_container(&wslc, "mysql", std::path::Path::new(bad)) {
                Err(Error::InvalidArgument(_)) => {}
                other => panic!("{bad} 应被拒：{other:?}"),
            }
        }
        // 这些名字要能过校验（走到起进程那一步 → 可执行文件不存在）
        for ok in [
            r"C:\backups\my dump.sql",
            r"C:\backups\shop backup (1).sql",
            r"C:\backups\订单数据.sql",
        ] {
            match push_dump_to_container(&wslc, "mysql", std::path::Path::new(ok)) {
                Err(Error::ExecutableNotFound { .. }) => {}
                other => panic!("{ok} 不该在参数校验就被拒：{other:?}"),
            }
        }
    }

    #[test]
    fn pushing_a_dump_starts_with_mkdir_and_fails_cleanly_on_a_bogus_binary() {
        // 端到端：第一步是 `mkdir -p`（目标目录必须先存在），
        // 所以可执行文件不存在时第一刀就断 → ExecutableNotFound。
        // 这条同时钉住"顺序是从 mkdir 开始"这件事 —— 少了它，
        // 目标目录不存在会让第②步报一句引擎的 `ERROR_PATH_NOT_FOUND`。
        let wslc = Wslc::with_program("definitely-not-a-real-binary");
        match push_dump_to_container(
            &wslc,
            "mysql",
            std::path::Path::new(r"C:\backups\shop-20261015-035901.sql"),
        ) {
            Err(Error::ExecutableNotFound { .. }) => {}
            other => panic!("不应出现其它结果：{other:?}"),
        }
    }
}
