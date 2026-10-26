//! TSV 编解码 + 库 / 用户 / 客户端探针的解析。

/// 还原 `-B`（batch）模式的转义。
///
/// 实测（`SELECT 'a\tb', 'back\\slash'`）：`-B` 下 tab 输出成**字面的
/// `\t`**（反斜杠 + t），反斜杠输出成 `\\`。所以按 tab 切完字段之后必须还原，
/// 否则库名 / 用户名里带这些字符时会错位。
///
/// 未知转义（`\x`）**原样保留**：不去猜 MySQL 的版本差异，猜错比不猜更糟。
pub fn unescape_batch_field(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('0') => out.push('\0'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// 把一个字段**转义**成 `-B`（batch）格式。
///
/// 是 [`unescape_batch_field`] 的逆运算。存在的理由见 [`to_batch_tsv`]。
pub fn escape_batch_field(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        match c {
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\0' => out.push_str("\\0"),
            '\\' => out.push_str("\\\\"),
            other => out.push(other),
        }
    }
    out
}

/// 把「行的行」拼成 `mysql -B -N` 那种 TSV 文本（列之间一个真 tab，行之间换行）。
///
/// # 为什么要"倒回去"
///
/// 原生连接（`wslc-db`）拿到的是**结构化单元格**，而 `parse_schema_tree` /
/// `parse_databases` / `parse_users` 吃的是 `-B -N` 的 TSV 文本。
///
/// 给原生连接再写一套"结构化"解析是**看起来很干净、实际很糟**的做法：
/// 同一个语义会有两份实现，而两份实现的行为差异（空值、转义、行数不齐）
/// 正是最难查的那类 bug —— 换个连接方式结果就变了一点，谁也不会想到去查解析。
///
/// 所以这里把数据还原成那套文本，**解析函数仍然只有一份**。
///
/// `None`（SQL NULL）输出成空串：调用它的都是元数据查询（库名 / 表名 / 列名），
/// 那些字段不会是 NULL；真出现 NULL 时"空串"和 `-B` 打印的 `NULL` 都不影响
/// 后续解析（那几处都会把空值丢掉）。
pub fn to_batch_tsv(rows: &[Vec<Option<String>>]) -> String {
    let mut out = String::new();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i > 0 {
                out.push('\t');
            }
            out.push_str(&escape_batch_field(cell.as_deref().unwrap_or("")));
        }
        out.push('\n');
    }
    out
}

/// 一行 TSV → 字段列表（**未转义的 tab 才是分隔符**）。
pub fn split_tsv_line(line: &str) -> Vec<String> {
    line.split('\t').map(unescape_batch_field).collect()
}

/// `information_schema.SCHEMATA` 一行（5 列）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbEntry {
    /// 库名。
    pub name: String,
    /// 默认字符集。
    pub charset: String,
    /// 默认排序规则。
    pub collation: String,
    /// 表数量。
    pub tables: u64,
    /// 数据 + 索引占用（字节）。
    pub bytes: u64,
}

impl DbEntry {
    /// 是不是 MySQL 自带的系统库。
    ///
    /// 界面上给它们一个「系统」标记：**不隐藏**（它们是真实存在的库，
    /// 而且 `sys` / `performance_schema` 偶尔真要去看），但得让用户一眼知道
    /// 「这不是我建的、删了会出事」。
    pub fn is_system(&self) -> bool {
        matches!(
            self.name.as_str(),
            "information_schema" | "mysql" | "performance_schema" | "sys"
        )
    }
}

/// 列出所有库（**一次** exec 拿全部信息，不是每个库一条命令）。
///
/// 列：库名 / 字符集 / 排序规则 / 表数 / 占用。
pub fn list_databases_sql() -> &'static str {
    "SELECT s.SCHEMA_NAME, s.DEFAULT_CHARACTER_SET_NAME, s.DEFAULT_COLLATION_NAME, \
     COALESCE((SELECT COUNT(*) FROM information_schema.TABLES t \
     WHERE t.TABLE_SCHEMA = s.SCHEMA_NAME), 0), \
     COALESCE((SELECT SUM(t.DATA_LENGTH + t.INDEX_LENGTH) FROM information_schema.TABLES t \
     WHERE t.TABLE_SCHEMA = s.SCHEMA_NAME), 0) \
     FROM information_schema.SCHEMATA s ORDER BY s.SCHEMA_NAME"
}

/// 列出所有用户。
///
/// 列：User / Host / plugin / Super_priv。
///
/// ⚠️ `mysql.user` 只有**高权限账号**读得到；普通账号会拿到
/// `ERROR 1142 … SELECT command denied`（[`MysqlErrorKind::PermissionDenied`]）。
/// 这不是 bug，所以界面上要把它说清楚，而不是显示成"读取失败"。
pub fn list_users_sql() -> &'static str {
    "SELECT User, Host, plugin, Super_priv FROM mysql.user ORDER BY User, Host"
}

/// 解析 [`list_databases_sql`] 的 `-N -B` 输出。
///
/// 列数不足的**坏行直接跳过**（这一层的设计目标：解析失败绝不 panic）。
pub fn parse_databases(tsv: &str) -> Vec<DbEntry> {
    tsv.lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| {
            let fields = split_tsv_line(line);
            if fields.len() < 5 {
                return None;
            }
            Some(DbEntry {
                name: fields[0].clone(),
                charset: fields[1].clone(),
                collation: fields[2].clone(),
                // 数字列解析不了就当 0：显示成 0 比整行消失好
                tables: fields[3].trim().parse().unwrap_or(0),
                bytes: fields[4].trim().parse().unwrap_or(0),
            })
        })
        .collect()
}

/// `mysql.user` 的一行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbUser {
    /// 用户名。
    pub user: String,
    /// 主机（`%` / `localhost` / IP …）。**同一个用户名可以有多个 host**，
    /// 所以它是身份的一部分，不能省。
    pub host: String,
    /// 认证插件（`caching_sha2_password` / `mysql_native_password` …）。
    pub plugin: String,
    /// 是否超级权限（`Super_priv = Y`）。
    pub super_priv: bool,
}

impl DbUser {
    /// `'root'@'%'` 形式（和 SQL 里的写法一致，用户一眼能对上）。
    pub fn account(&self) -> String {
        format!("'{}'@'{}'", self.user, self.host)
    }

    /// 是不是 MySQL / MariaDB 自带的内部账号
    /// （`mysql.sys` / `mysql.session` / `mysql.infoschema` / `mariadb.sys`）。
    pub fn is_system(&self) -> bool {
        matches!(
            self.user.as_str(),
            "mysql.sys" | "mysql.session" | "mysql.infoschema" | "mariadb.sys"
        )
    }

    /// 是不是 root 账号。
    ///
    /// 用户名在 MySQL 里理论上区分大小写，但 `root` 是安装时建的固定名字；
    /// 这里按不区分大小写处理，避免有人手工建了 `Root` 时被当成业务账号。
    pub fn is_root(&self) -> bool {
        self.user.eq_ignore_ascii_case("root")
    }

    /// 用户列表默认要不要藏起来。
    ///
    /// 系统账号（`mysql.*`）和 root 是 MySQL 自己建的：日常建库 / 建业务用户时
    /// 它们只会占满列表，而且 root 的「删除 / 改密码」误点代价很大。
    /// 所以默认隐藏；用户需要改 root 时，界面上有「显示系统账号」开关。
    pub fn is_hidden_by_default(&self) -> bool {
        self.is_system() || self.is_root()
    }
}

/// 解析 [`list_users_sql`] 的 `-N -B` 输出。
pub fn parse_users(tsv: &str) -> Vec<DbUser> {
    tsv.lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| {
            let fields = split_tsv_line(line);
            if fields.len() < 4 {
                return None;
            }
            Some(DbUser {
                user: fields[0].clone(),
                host: fields[1].clone(),
                plugin: fields[2].clone(),
                super_priv: fields[3].trim().eq_ignore_ascii_case("y"),
            })
        })
        .collect()
}

/// 客户端探测的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientProbe {
    /// 二进制名（`mysql` / `mariadb` / `mysqlsh`）—— 直接拿去拼命令行。
    pub bin: String,
    /// 完整路径（界面上显示"用的是哪个客户端"）。
    pub path: String,
    /// `--version` 的第一行。
    pub version: Option<String>,
}

/// 探测脚本：找到客户端并打印它的路径与版本。
///
/// `command -v a || command -v b` 的短路顺序就是**优先级**；
/// 一个都没有时 `exit 127`（并让 `wslc exec` 以非零退出，我们才能给出
/// "镜像里没有客户端"的提示）。
///
/// 这一段交给容器里的 `/bin/sh` 执行 —— 参数是**整串**传进去的（不是我们在
/// Windows 这边拼 shell 命令），所以本机不会多一层 shell 解析。
pub fn client_probe_script() -> &'static str {
    "c=$(command -v mysql || command -v mariadb || command -v mysqlsh) || exit 127; \
     echo \"$c\"; exec \"$c\" --version"
}

/// 解析 [`client_probe_script`] 的输出（第一行路径、第二行版本）。
pub fn parse_client_probe(stdout: &str) -> Option<ClientProbe> {
    let mut lines = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let path = lines.next()?.to_owned();
    let bin = path.rsplit('/').next().unwrap_or(&path).trim().to_owned();
    if bin.is_empty() {
        return None;
    }
    Some(ClientProbe {
        bin,
        path,
        version: lines.next().map(str::to_owned),
    })
}
