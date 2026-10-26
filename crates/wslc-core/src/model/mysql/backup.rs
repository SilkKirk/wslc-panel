//! 备份与恢复：路径约定、`mysqldump` / `mysql <` 的参数、文件名与时间戳。

use chrono::{DateTime, Utc};

use crate::error::{Error, Result};

use super::*;

/// 生成一个随机密码（**16 位**）。
///
/// # 为什么是这条字符集
///
/// 密码要被拼进 SQL 字符串字面量（[`quote_literal`] 会转义 `'` / `\` / `"`），
/// 但**少给用户添麻烦**更重要：字符集里干脆**不放**那三个字符，转义就永远
/// 不会被触发；也不放空格 / `` ` `` / `$`，让它能原样贴进任何客户端。
///
/// # 随机源
///
/// 用 `getrandom` 直接取操作系统的 CSPRNG。早先用的是
/// `DefaultHasher` + LCG + 纳秒 / 计数器 / 栈地址，注释里也写明"不是密码学
/// 安全的随机" —— 生成的是数据库账号口令，不该继续这样。
///
/// 字节落到 `228..=255` 时丢弃再取：`256 / 57 = 4` 余 28，直接 `% 57`
/// 会让前 28 个字符多出约 1/4 的偏置。16 位密码只多几次系统调用。
///
/// 系统熵源不可用时返回 `Err`（由界面提示），**不**退回弱随机：一个
/// "看起来生成了密码、实际可预测"的账号比明确失败更糟。
pub fn generate_password() -> std::result::Result<String, String> {
    // 无歧义字符（没有 0/O、1/l/I）—— 密码是要抄到别的机器上用的。
    const ALPHABET: &[u8] = b"abcdefghijkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    // 256 / 57 = 4，4 * 57 = 228；>= 228 的字节丢弃，避免 modulo bias。
    const LIMIT: u8 = 228;

    let mut out = String::with_capacity(16);
    let mut buf = [0u8; 16];
    while out.len() < 16 {
        getrandom::fill(&mut buf).map_err(|e| format!("取系统随机数失败：{e}"))?;
        for &byte in &buf {
            if byte >= LIMIT {
                continue;
            }
            out.push(ALPHABET[(byte % ALPHABET.len() as u8) as usize] as char);
            if out.len() == 16 {
                break;
            }
        }
    }
    Ok(out)
}

/// **建议**的用户名规则：`<库名>_user`（`djxm` → `djxm_user`）。
///
/// ⚠️ v0.9.3 起它只是**建议值**，不再是"建库时的用户名" —— 弹窗里那个用户名
/// 由用户手填（实机反馈"创建数据库的创建用户名这里让手填，不要自动生成"），
/// 这个函数只负责算「用建议值」按钮该填什么。**别再把它当默认值用**。
///
/// ⚠️ 结果**仍要**过 [`validate_ident`]（调用方自己查）：库名受白名单约束时
/// 拼上 `_user` 一定合法，但这个函数不知道调用方的库名从哪来。
pub fn default_user_for(database: &str) -> String {
    format!("{}_user", database.trim())
}

/// 从**客户端**的二进制名推出配套的 dump 工具（**纯函数**）。
///
/// # 为什么需要它（v0.9.6 的实机事故）
///
/// 面板探测容器时只找 `mysql` / `mariadb` / `mysqlsh` 客户端（那是执行 SQL 用的），
/// 从来没有单独找过 dump 工具。而备份第一步却**把客户端当成了 dump 程序**：
/// `mysqldump_args(client_bin, …)` 原来直接把 `client_bin` 当成 argv[0]，
/// 于是容器里真正跑起来的是
///
/// ```text
/// mysql -u <用户> -N -B --databases <库> --single-transaction --quick --set-gtid-purged=OFF
/// ```
///
/// —— 把 **mysqldump 的参数喂给了 mysql 客户端**。实测（本机容器，mysql 26.7.0）：
///
/// | 形状 | 结果 |
/// |---|---|
/// | `mysql … --databases djxm …`（改之前） | `mysql: [ERROR] unknown option '--databases'.` **exit 2** |
/// | `mysqldump … --databases djxm …`（改之后） | exit 0，110 张表 / 2.3 MB |
///
/// 之所以一直没人发现：这个函数**只有一个调用方**（`cmd::mysql::backup_database`），
/// 而"备份"这个按钮从 v0.8 到 v0.9.6 之间**根本不存在**，
/// 单测又只传 `"mysqldump"` 进来、从不检查 argv[0]，于是两边一起沉默。
///
/// # 规则
///
/// 只换**基名**、保留目录（`/usr/bin/mysql` → `/usr/bin/mysqldump`）：
/// 客户端和 dump 工具在官方镜像里永远同目录。
///
/// | 客户端 | dump 工具 |
/// |---|---|
/// | `mysql` | `mysqldump` |
/// | `mariadb` | `mariadb-dump` |
/// | `mysqlsh` | `mysqldump`（⚠️ 这一条是**推的**：官方 mysql-shell 镜像里有没有 `mysqldump` 没实测过） |
/// | 已经叫 `*dump` 的 | 原样返回（幂等 —— 手写测试/将来直接传 dumper 都不会被改坏） |
/// | 其它 | `mysqldump`（退回最可能的那个；真没有时容器会报 `not found`） |
pub fn dump_bin(client_bin: &str) -> String {
    let (dir, base) = match client_bin.rsplit_once('/') {
        Some((dir, base)) => (Some(dir), base),
        None => (None, client_bin),
    };
    let dump = match base {
        "mariadb" => "mariadb-dump",
        // 幂等：已经是个 dump 工具就原样用（`mariadb-dump` 也落在这里）
        other if other.ends_with("dump") => other,
        _ => "mysqldump",
    };
    match dir {
        Some(dir) => format!("{dir}/{dump}"),
        None => dump.to_owned(),
    }
}

/// `mysqldump` 的参数（**纯函数**，`cmd::mysql` 拿它去拼 `wslc exec`）。
///
/// ⚠️ 收的是**客户端**的二进制名（探测得来的那个），dump 工具由 [`dump_bin`] 推导 ——
/// 调用方**不要**自己传 `mysqldump`，v0.9.6 就是在这里把客户端当成了 dump 程序。
///
/// # 实测（wslc 3.0.3.0 / mysql 26.7.0）
///
/// - `--databases <库名>`：dump 里带 `CREATE DATABASE`，恢复时能落回原库；
/// - `--single-transaction`：InnoDB 一致性快照，不锁表（MyISAM 不吃这套，
///   那类表本来就该 dump 时锁 —— 权衡写在这里）；
/// - `--quick`：逐行取，不在内存里攒整个结果集 —— 对大表是**必须的**；
/// - `--set-gtid-purged=OFF`：实测不加会往 stderr 打一条 GTID 警告；
///   恢复到同一个实例的场景 GTID 本来就不该带，加上它把警告也消了。
/// - `--default-character-set=utf8mb4` 也是复用 `client_args` 带进来的（见那边）：
///   对 dump 来说等于显式声明"写出来的是 utf8mb4"，不再依赖容器 locale。
/// - 前面那对 `-N -B` 同样是复用 `client_args(SqlMode::Batch)` 带进来的：
///   `-B` 在 mysqldump 里就是 `--databases`（重复但无害），`-N` = `--no-set-names`
///   （dump 里因此少了 `SET NAMES` 那一行）。
///
/// # `-N` 与中文：实测过，**它不是问题**（但"去掉它"也不是解）
///
/// 在真容器里对一份含 `中文测试` 的库做完整往返（建库 → dump → 删库 → 恢复 → 比 HEX，
/// 原始值 `E4B8ADE69687E6B58BE8AF95`）：
///
/// | 恢复时用的客户端 | 恢复后 HEX | 结论 |
/// |---|---|---|
/// | `mysql -uroot < dump`（面板 v0.9.6 的形状） | `C3A4C2B8C2ADC3A6…` | ❌ 双重编码 |
/// | 同一份 dump，但 dump **去掉 `-N`**（自带 `SET NAMES utf8mb4`） | `C3A4C2B8C2ADC3A6…` | ❌ 一样坏 |
/// | 同一份 `-N` dump + 客户端 `--default-character-set=utf8mb4` | `E4B8ADE69687E6B58BE8AF95` | ✅ 一致 |
///
/// 结论：**问题在消费端的客户端字符集，不在 dump 的 `-N`** —— 客户端自己的字符集
/// 不对时，dump 里那句 `SET NAMES` 救不回来。所以 `-N` 保留（少一行无害的语句），
/// 真正的修复在 `client_args` / `load_script` 里那个显式字符集开关。
pub fn mysqldump_args(client_bin: &str, creds: &MysqlCreds, database: &str) -> Result<Vec<String>> {
    validate_ident("库名", database)?;
    let mut args = client_args(&dump_bin(client_bin), creds, "", None, SqlMode::Batch);
    // client_args 的尾巴是 `-e <sql>`，dump 不需要它 —— 掐掉再接自己的参数
    args.truncate(args.len() - 2);
    args.push("--databases".to_owned());
    args.push(database.trim().to_owned());
    args.push("--single-transaction".to_owned());
    args.push("--quick".to_owned());
    args.push("--set-gtid-purged=OFF".to_owned());
    Ok(args)
}

/// 把一段文本包成 shell 里的**单引号字面量**（纯函数）。
///
/// # 为什么需要它
///
/// 容器内那一步是**一条 `sh -c` 脚本**（`mysql < 文件` 要用 shell 的重定向
/// —— `wslc exec` 的 stdin 被我们的封装写死成 null，见 [`load_script`]）。
/// 所以路径和库名都会**过一次 shell**。而这是**用户选的文件**：
/// `shop backup (1).sql`、`订单数据.sql` 都很正常，实测 `container cp`
/// 对这种名字没问题 —— 那就必须原样送进 shell，不能被它拆开
///（不包引号时 `mysql < /tmp/shop backup.sql` 会被拆成两个参数）。
///
/// # 为什么遇到 `'` 直接拒
///
/// 单引号里没法转义单引号（`'\''` 那种拼法可读性太差，而且这是一条
/// 要出现在错误消息里的命令）。所以宁可**报一句人话**，也不拼出一条
/// 会被拆开的命令 —— 文件名带单引号的用户几乎没有，让他改个名就行。
pub fn sh_quote(value: &str) -> Result<String> {
    if value.contains('\'') || value.contains('\n') || value.contains('\r') {
        return Err(Error::InvalidArgument(format!(
            "路径 / 名字里不能有单引号或换行：{value}"
        )));
    }
    // 转义本身只留一份实现；上面那条"拒绝不可读形状"的**策略**留在这里。
    Ok(crate::util::shell_quote(value))
}

/// 把一份 `.sql` 文件喂给 `mysql` 的容器内脚本（**纯函数**）。
///
/// 恢复（面板自己的备份）和导入（用户挑的文件）**走的是同一条路** ——
/// 都是"容器里有个 .sql，让 mysql 读它"。区别只有 `database`：
///
/// - `None`：**由文件自己的语句决定**。`mysqldump --databases` 出来的 dump
///   自带 `CREATE DATABASE` / `USE`，恢复就走这条；
/// - `Some(db)`：`mysql -D <db>`，往指定的库里灌。用户的 dump 若没有
///   `CREATE DATABASE` / `USE`（`mysqldump <db> > f.sql` 就是这种），
///   不给库名 mysql 会报 **"No database selected"**（实测过这条报错）。
///
/// 为什么不用 stdin：我们的 `Wslc::build_command` 把 stdin 写死成 null ——
/// 不是 wslc 不支持（`wslc exec --help` 有 `-i/--interactive`），是封装没做
/// "stdin 来自文件"。要走那条路得先给 `cli.rs` 加一个同族方法。
pub fn load_script(bin: &str, sql_path: &str, database: Option<&str>) -> Result<String> {
    let path = sh_quote(sql_path)?;
    // ⚠️ 这个开关是**防数据损坏**的，不是"顺手加个好习惯"：容器里没有 `LANG` 时
    // 客户端默认字符集退回 **latin1**（实测 `character_set_client=latin1`），
    // 而 dump / 用户选的 `.sql` 都是 utf8mb4 字节 —— 客户端按 latin1 解释再转一次，
    // 中文就变成双重编码**静默存进库**。
    // 实测（本机容器，mysql 26.7.0，`中文测试` = HEX `E4B8ADE69687E6B58BE8AF95`）：
    //
    // | 恢复时用的客户端 | 恢复后 HEX | 结论 |
    // |---|---|---|
    // | `mysql -uroot < dump`（面板 v0.9.6 的形状） | `C3A4C2B8C2ADC3A6…` | ❌ 坏了 |
    // | 同一份 dump + `--default-character-set=utf8mb4` | `E4B8ADE69687E6B58BE8AF95` | ✅ 一致 |
    //
    // ⚠️ 顺带记一条**反直觉**的实测：光靠 dump 里那句 `SET NAMES utf8mb4`
    // **救不回来**（同样变成 `C3A4C2B8…`）—— 客户端自己的字符集不对时，
    // 服务端那句 SET NAMES 改不了客户端怎么解释它读到的字节。
    // 所以"去掉 dump 的 `-N` 让 dump 自带 SET NAMES"**不是**这个问题的解。
    let charset = "--default-character-set=utf8mb4";
    match database {
        // `-D` 的值也过一次 shell，所以同样要引起来（库名可能带空格 / 中文）
        Some(db) => Ok(format!(
            "exec {bin} {charset} -D {} < {path}",
            sh_quote(db)?
        )),
        None => Ok(format!("exec {bin} {charset} < {path}")),
    }
}

/// 备份文件在**容器里**的目录（备份时 dump 落在这里）。
///
/// 放 `/tmp` 是刻意的：它一定可写，而且不用先探容器里哪块盘有空。
/// ⚠️ 这份**会随容器重建 / `/tmp` 被清而消失**（实测发生过一次），
/// 所以恢复**不依赖它** —— 恢复用的是 Windows 侧存档，见
/// [`CONTAINER_RESTORE_DIR`]。
pub const CONTAINER_BACKUP_DIR: &str = "/tmp/wslc-backups";

/// 要**送进容器**的 `.sql` 落脚的目录（恢复和导入共用）。
///
/// # 为什么恢复要重新送一份进去（v0.8.2 改的）
///
/// v0.8.0/0.8.1 的恢复**只认容器内那份**，理由是当时以为
/// `wslc container cp` 的"本地→容器"方向不可用。**那个结论是错的**
/// （见 [`restore_container_path`] 里那段实测）：于是"容器重建过就没法恢复"
/// 这个限制根本不需要存在 —— 备份列表上的每一条都有 Windows 侧的文件，
/// 恢复时把它 cp 进这个目录再重定向即可。
///
/// 导入（用户挑的本地 `.sql`）走的是**同一个目录、同一条路**：
/// `mkdir -p` → `container cp` 进来 → `mysql < 它`。所以这里叫
/// `RESTORE` 是历史名字，实际语义是"**待导入的件**"。
///
/// 用**独立目录**而不是 [`CONTAINER_BACKUP_DIR`]：那里放"刚导出的备份"，
/// 混在一起会让两份同名文件互相覆盖（同名文件在这两个目录里语义相反：
/// 一个是源、一个是刚生成的目标）。
pub const CONTAINER_RESTORE_DIR: &str = "/tmp/wslc-restore";

/// dump 被送进容器后的**绝对路径**（纯函数）。
///
/// # 实测（wslc 3.0.3.0，2026 年，容器 `mysql` 处于 Up）
///
/// `wslc container cp` 有三种形式（`wslc container cp --help`）：
/// 本地→容器、容器→本地、**stdin→容器**（`cp - CONTAINER:PATH`）。
///
/// | 试的 | 结果 |
/// |---|---|
/// | `cp <本地文件> mysql:/tmp/probe.txt`（**目标写成文件名**） | ❌ `ERROR_PATH_NOT_FOUND`：「Could not find the file /tmp/probe.txt in container …」 |
/// | `cp <本地文件> mysql:/tmp/`（**目标是已存在的目录**） | ✅ 落在 `/tmp/<本地文件名>` |
/// | `cp <本地文件> mysql:/tmp`（目录、无斜杠） | ✅ 同上 |
/// | `cp <本地文件> mysql:/不存在的目录/` | ❌ `ERROR_PATH_NOT_FOUND` —— **目标目录必须先存在** |
/// | 先 `mkdir -p` 再 `cp <本地文件> mysql:/tmp/wslc-restore/` | ✅ |
/// | 4 MB 随机二进制（sha256 对照） | ✅ **逐字节一致** |
/// | 本地路径含空格（`…\.tmp-cp dir with space\dump file.sql`） | ✅ |
/// | 同名文件再传一次（覆盖） | ✅ |
/// | `cp - mysql:/tmp/`（stdin 形式，经 `cmd /c "… < file"`） | ❌ `unexpected EOF` / `E_FAIL` —— **我没找到正确的喂法**，但目录目标的形式已经够用，所以没往下查 |
///
/// ⚠️ 我 v0.8.0 时只试了**第一行**那种写法，就得出"本地→容器是坏的"，
/// 并在代码注释里写死了这个错结论 —— 用户拿 `--help` 指出来的。
/// 教训：**"这条路不通"的结论必须多试几种参数形式**（尤其目标写成
/// 文件名 vs 目录，这是两套语义），否则会把工具的能力误判成缺陷。
///
/// 所以调用方（`cmd::mysql::push_dump_to_container`）的顺序是：
/// ① `mkdir -p <目录>` ② `cp <本地 dump> <容器>:<目录>/` ③ 用本函数算出路径。
pub fn restore_container_path(file_name: &str) -> String {
    format!("{CONTAINER_RESTORE_DIR}/{file_name}")
}

/// 备份在 **Windows 侧**的子目录名（`<数据目录>/backups/<容器>/<文件名>`）。
///
/// 备份做成**两份**：容器内那份是 dump 的产地（也是"同容器内立刻恢复"的
/// 快路径）、Windows 侧那份留档。**恢复以 Windows 侧那份为准** ——
/// 容器内那份在 `/tmp`，容器重建就没了（实测踩到过）。
pub const WINDOWS_BACKUP_SUBDIR: &str = "backups";

/// 备份文件名（`<库名>-<YYYYMMDD>-<HHMMSS>.sql`，定宽、可排序）。
///
/// 同一秒同库会撞名 —— 调用方撞了就报"刚备份过一次"，比自动加序号诚实。
///
/// 时间按 **UTC** 算（和早先的手写实现一致）：备份文件名只用于排序 / 识别，
/// 不承担"给用户看本地时间"的职责。
pub fn backup_file_name(database: &str, now_secs: u64) -> String {
    let stamp = i64::try_from(now_secs)
        .ok()
        .and_then(|secs| DateTime::<Utc>::from_timestamp(secs, 0))
        .map(|at| at.format("%Y%m%d-%H%M%S").to_string())
        // 只有 now_secs 超出 chrono 可表示范围才会走到这里；保留一个
        // 不会 panic 的兜底，正常路径永远是定宽格式。
        .unwrap_or_else(|| now_secs.to_string());
    format!("{database}-{stamp}.sql")
}
