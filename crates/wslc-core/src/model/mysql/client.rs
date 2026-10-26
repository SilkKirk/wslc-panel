//! 怎么调客户端：`SqlMode` / `client_args` / `exec_env`，以及命令行与文本的脱敏。

use super::*;

/// 输出模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqlMode {
    /// 给**机器解析**用：`-N`（不要表头）+ `-B`（tab 分隔）。
    ///
    /// 用于"列名由我们自己知道"的查询（`SHOW DATABASES` 之类）。
    Batch,
    /// 给**机器解析**用，但**带表头**：只要 `-B`，不要 `-N`。
    ///
    /// 实测：`-B` 不带 `-N` 时，输出的**第一行就是列名**。
    /// SQL 控制台要展示任意查询的结果，列名只有服务端知道 —— 所以用它。
    /// 解析在 [`super::sqlresult::parse_batch_with_header`]。
    ///
    /// ⚠️ **不能加 `-r/--raw`**：那个开关关掉转义，字段里的真 TAB / 真换行
    /// 会把列和行切错。`-B` 的转义（`\t` / `\n` / `\\`）正是"按行按列切"能成立的
    /// 前提。
    BatchWithHeader,
    /// 给**人看**用：`--table`（ASCII 表格）。
    ///
    /// ⚠️ `--table` 必须显式给：stdout 不是 tty 时 `mysql` **默认就是 batch**
    /// （实测：不加参数时输出的是 `a\tb` 而不是带框的表格）。
    ///
    /// 现在只留作"看原始输出"的兜底（`DbState.sql_raw`）。
    Table,
}

impl SqlMode {
    /// 对应的开关。
    pub fn flags(self) -> &'static [&'static str] {
        match self {
            Self::Batch => &["-N", "-B"],
            Self::BatchWithHeader => &["-B"],
            Self::Table => &["--table"],
        }
    }
}

/// 拼 `mysql` 客户端的参数（**不含程序名之前的 `wslc exec` 前缀**）。
///
/// # 不变量（每一条都有代价）
///
/// 1. **`-e <SQL>` 必须在最后** —— `wslc` 的位置参数解析一旦遇到非选项就只认
///    位置参数了（`cmd::container::logs_args` 那条注释同理）；
/// 2. **永不出现 `-p`** —— 密码走 [`exec_env`]（`MYSQL_PWD`）。`-p<密码>` 会
///    出现在容器内的进程列表里，而 `-e KEY=VALUE` 不会；
/// 3. 库名用 `-D` 传，**不拼进 SQL** —— 省掉一次字符串拼接就省掉一类拼错；
/// 4. `-N -B` / `--table` 由 [`SqlMode`] 决定，调用方不要自己加。
pub fn client_args(
    bin: &str,
    creds: &MysqlCreds,
    sql: &str,
    database: Option<&str>,
    mode: SqlMode,
) -> Vec<String> {
    let mut args: Vec<String> = vec![bin.to_owned()];

    // ⚠️ **必须显式指定 utf8mb4**：容器里通常没有 `LANG`，而 mysql 客户端的
    // `--default-character-set` 默认是 `auto` —— 认不出 locale 时**退回 latin1**。
    // 实测（本机容器，mysql 26.7.0）：不指定时 `character_set_client` /
    // `character_set_results` 都是 **latin1**，于是
    // ① `SELECT` 出来的中文变成 `?`（服务端转不成 latin1）；
    // ② 更要命的是恢复 / 导入：dump 是 utf8mb4 字节，客户端当 latin1 再转一次
    //    → **双重编码存进库**（实测 `中文测试` 的 HEX 从 `E4B8AD…` 变成 `C3A4C2B8…`，
    //    数据静默损坏）。同一容器上加上这个开关后 HEX 原样一致。
    args.push("--default-character-set=utf8mb4".to_owned());

    args.push("-u".to_owned());
    args.push(creds.user.clone());
    if let Some(host) = &creds.host {
        args.push("-h".to_owned());
        args.push(host.clone());
    }
    if let Some(port) = creds.port {
        args.push("-P".to_owned());
        args.push(port.to_string());
    }

    args.extend(mode.flags().iter().map(|f| (*f).to_owned()));

    if let Some(db) = database {
        args.push("-D".to_owned());
        args.push(db.to_owned());
    }

    args.push("-e".to_owned());
    args.push(sql.to_owned());
    args
}

/// 密码那一条环境变量（`wslc exec -e` 的值）。
///
/// ⚠️ 这是**唯一**放密码的地方。凡是把这个字符串交给错误消息 / 日志的地方，
/// 都必须先过 [`redact_args`]。
pub fn exec_env(creds: &MysqlCreds) -> String {
    format!("MYSQL_PWD={}", creds.password)
}

// ---------------------------------------------------------------------------
// 脱敏
// ---------------------------------------------------------------------------

/// 把一个参数里的**密码形状**打成 `***`。
///
/// 认这三种形状（都是 mysql 客户端 / 我们自己会写出来的）：
/// 含 `PASSWORD`/`PWD` 的 `KEY=VALUE`、`--password=…`、`-p<值>`。
fn redact_one(arg: &str) -> String {
    if let Some((key, _)) = arg.split_once('=') {
        let upper = key.to_ascii_uppercase();
        if upper.ends_with("PASSWORD") || upper.ends_with("PWD") {
            return format!("{key}=***");
        }
    }
    if arg.starts_with("-p") && arg.len() > 2 {
        return "-p***".to_owned();
    }
    arg.to_owned()
}

/// 脱敏一整串参数（**错误消息的唯一出口**）。
///
/// # 为什么必须有它
///
/// [crate::error::Error::NonZeroExit] 的 `Display` 会把 `args` **拼进消息**：
/// `wslc exec -e MYSQL_PWD=root mysql … 返回退出码 1：…`。
/// 不脱敏的话，任何一次 SQL 报错都会把明文密码送到提示条和日志里。
pub fn redact_args(args: &[String]) -> Vec<String> {
    args.iter().map(|a| redact_one(a)).collect()
}

/// 把 `text` 里出现的 `password` 原文换成 `***`。
///
/// 这是给**子进程输出**用的（`stderr`）：`CREATE USER … IDENTIFIED BY 'x'`
/// 出错时，MySQL 会把**整条语句**回显在错误里（"near 'x'' at line 1"），
/// 于是在那边按"形状"脱敏是不够的 —— 得按**值**替换。
///
/// ⚠️ 密码为空串时**原样返回**：否则空串会把整段文本拆得面目全非
/// （`str::replace` 对空 pattern 会在每个字符边界插入）。
pub fn redact_password(text: &str, password: &str) -> String {
    if password.is_empty() {
        return text.to_owned();
    }
    text.replace(password, "***")
}
