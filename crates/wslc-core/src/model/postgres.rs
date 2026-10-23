//! PostgreSQL 的纯逻辑：拼参数、解析输出、切分语句。
//!
//! # 全部规则都是**实测**出来的
//!
//! 依据见 `docs/postgres-measured.md`（真容器 `postgres:16` 上抓的）。
//! 和 MySQL 的四处关键差异，每一条都决定了这里的代码：
//!
//! | 差异 | 这里怎么应对 |
//! |---|---|
//! | `psql -A` 默认分隔符是 `\|` | **不用 `-A`**，改 `--csv` |
//! | `psql -A` 带表头时尾部有 `(N rows)` | `--csv` 模式下**没有**这一行（实测） |
//! | DDL 会打印 `CREATE TABLE` 命令标签 | 加 `-q`（实测压成 0 字节） |
//! | `psql -A` **不转义**值里的 tab/换行 | `--csv` 会正确加引号（这就是不用 `-A` 的原因） |
//!
//! # 为什么是 CSV 不是 TSV
//!
//! 实测最刺眼的一条：`-A -t -F "\t"` 下，一个含 tab 的值会被切成**两列**、
//! 一个含换行的值会被切成**两行** —— `psql` 在 unaligned 模式下**完全不做转义**。
//! 而数据库里的 `text` 列带换行是**常态**（JSON、SQL 片段、日志）。
//!
//! `mysql -B` 会把 tab/换行转义成 `\t`/`\n`（`unescape_batch_field` 再转回来），
//! 所以它是无损的；`psql` 没有这个机制，只能靠 CSV 的引号规则。
//!
//! **意外收获**：`--csv` 模式下空结果集仍然带表头（`id\n`，3 字节），
//! 而 MySQL 的 `-B` 是 0 字节 —— 也就是说这一路连列名都能拿到。

/// 默认取多少行（点表名自动生成 SELECT 时）。
///
/// 和 MySQL 那边同一个数：一屏看 20~30 行，200 行够翻几屏，
/// 又不至于把整张表拉进内存。
pub const DEFAULT_LIMIT: usize = 200;

/// 拼出执行一条 SQL 的 `psql` 参数。
///
/// # 每个开关的理由（都是实测的）
///
/// - `-X`：忽略 `~/.psqlrc`。**不能用用户的配置** —— 那里可能改了输出格式
///   （`\pset format` / `\pset null`），一改我们的解析就崩。
/// - `-q`：压掉 `CREATE TABLE` / `INSERT 0 1` 这类**命令标签**。
///   不加的话，执行一条 CREATE 会被解析成"一行一列、值为 `CREATE TABLE` 的
///   结果集"，界面上就是一张假表格。
/// - `-v ON_ERROR_STOP=1`：多语句中途报错就停。虽然我们**逐条**执行，
///   但用户可能在一条语句里塞了 `BEGIN; …; COMMIT;` 这种块。
/// - `-U <user>` + `PGPASSWORD` 环境变量（见 [`exec_env`]）。
/// - `--csv`：见模块文档。
/// - `-c <sql>`：单条。**不用 `-f`** —— 那要先把文件塞进容器。
///
/// ⚠️ 故意**没有** `-h`：容器内走 unix socket。填 `127.0.0.1` 会多走一圈
/// TCP，而且容器里的 PostgreSQL 未必监听 TCP（和 MySQL 那边同一个决定）。
pub fn client_args(user: &str, database: Option<&str>, sql: &str) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-X".to_owned(),
        "-q".to_owned(),
        "-v".to_owned(),
        "ON_ERROR_STOP=1".to_owned(),
        "-U".to_owned(),
        user.to_owned(),
    ];
    if let Some(db) = database {
        args.push("-d".to_owned());
        args.push(db.to_owned());
    }
    args.push("--csv".to_owned());
    args.push("-c".to_owned());
    args.push(sql.to_owned());
    args
}

/// 传给 `wslc exec -e` 的环境变量。
///
/// `PGPASSWORD` 是 `psql` 认的变量；比 `-W`（交互式提示）和 `.pgpass` 文件
/// 都省事 —— 后者要先往容器里写文件。
///
/// ⚠️ 空口令也照传（`PGPASSWORD=`）：不传的话 `psql` 会去试 `.pgpass`，
/// 那会让"口令错"和"没配过"混在一起。
pub fn exec_env(password: &str) -> String {
    format!("PGPASSWORD={password}")
}

/// 一张 CSV 表格。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CsvTable {
    /// 第一行（列名）。空输出时是空 vector。
    pub header: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

impl CsvTable {
    /// **零字节输出** = 这条语句没有结果集（DDL / INSERT / 被 `-q` 压掉）。
    ///
    /// 这是 PostgreSQL 这边最干净的一条判据：不需要事先判断"这条语句
    /// 会不会返回行"（MySQL 那边要靠 `sqlstmt::returns_rows` 猜）。
    pub fn is_empty(&self) -> bool {
        self.header.is_empty() && self.rows.is_empty()
    }

    /// 列表宽度以表头为准（数据行可能比表头短 —— 见 [`parse_csv`] 的补齐）。
    pub fn column_count(&self) -> usize {
        self.header.len()
    }
}

/// 解析 `psql --csv` 的输出。
///
/// # 规则
///
/// - 第一条记录是**列名**，其余是数据行；
/// - **零字节输出**返回空表（没有结果集）；
/// - 有表头但没有数据行 = 空结果集（PostgreSQL 会给表头，MySQL 不给）；
/// - 数据行比表头短时**用空串补齐** —— 宁可多一个空格子，
///   也别让"少一列"的行把整张表的列错位（`sqlresult::fix_row_width` 同一个理由）。
pub fn parse_csv(text: &str) -> CsvTable {
    let mut records = parse_records(text);
    if records.is_empty() {
        return CsvTable::default();
    }
    let header = records.remove(0);
    let want = header.len();
    for row in &mut records {
        if row.len() < want {
            row.resize(want, String::new());
        }
    }
    CsvTable {
        header,
        rows: records,
    }
}

/// 按 RFC 4180 切记录。
///
/// 手写而不是引 `csv` crate：只需要"读"，不需要写、也不需要类型推断，
/// 而这个仓库对新增依赖很克制（`AGENTS.md` §2 的成本那一段）。
///
/// # psql 的三处特殊行为（都实测过）
///
/// 1. 记录分隔符是 **LF**（不是 CRLF）—— 但这里也吃掉 `\r`，防止将来变；
/// 2. **空值不加引号**：NULL 和空串输出都是"什么都没有"，两者分不开
///    （正好符合项目"不区分 NULL 与空串"的决定）；
/// 3. 引号内的换行**原样保留**（一个单元格可以跨多行）—— 这是不能用
///    `lines()` 切的原因。
fn parse_records(text: &str) -> Vec<Vec<String>> {
    let mut records: Vec<Vec<String>> = Vec::new();
    let mut record: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut it = text.chars().peekable();

    while let Some(c) = it.next() {
        if in_quotes {
            if c == '"' {
                // `""` 是转义出来的一个引号
                if it.peek() == Some(&'"') {
                    it.next();
                    field.push('"');
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }
        match c {
            '"' => in_quotes = true,
            ',' => record.push(std::mem::take(&mut field)),
            '\n' => {
                record.push(std::mem::take(&mut field));
                records.push(std::mem::take(&mut record));
            }
            // CRLF 的 CR 直接丢掉；字段里的 `\r` 只可能来自引号内，
            // 那条路径不会走到这里。
            '\r' => {}
            _ => field.push(c),
        }
    }

    // 收尾：输出没有以换行结束时，最后一条记录还没推。
    if !field.is_empty() || !record.is_empty() {
        record.push(field);
        records.push(record);
    }
    records
}

/// 把一段脚本切成单条语句。
///
/// # 为什么不能用 MySQL 那套 `sqlstmt::split_statements`
///
/// PostgreSQL 多出两样 MySQL 没有的东西，切错了会把一条语句拦腰截断：
///
/// 1. **美元引用**（`$$ … $$` / `$tag$ … $tag$`）—— 函数体里面全是分号。
///    这是 PostgreSQL 写函数的**标准写法**，比 MySQL 的 `DELIMITER` 常用得多。
/// 2. **可嵌套的块注释**（`/* a /* b */ c */`，PostgreSQL 真的支持嵌套；
///    MySQL 不支持，遇到第一个 `*/` 就结束）。
///
/// 另外 `E'…'`（转义字符串）里 `\'` 是合法的，不能按"两个单引号"处理。
pub fn split_statements(script: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut chars = script.chars().peekable();
    // 当前处于哪种"不该切分"的状态
    #[derive(PartialEq)]
    enum Mode {
        Normal,
        SingleQuote,
        /// `E'…'`：反斜杠转义生效
        EscapeString,
        DoubleQuote,
        LineComment,
        /// 块注释嵌套深度
        BlockComment(usize),
        /// 美元引用，存 tag（`$$` 是空串）
        Dollar(String),
    }
    let mut mode = Mode::Normal;

    while let Some(c) = chars.next() {
        // ── 先处理"已经在特殊状态里"的情况 ──────────────────────────────
        match &mode {
            Mode::SingleQuote => {
                cur.push(c);
                if c == '\'' {
                    if chars.peek() == Some(&'\'') {
                        cur.push(chars.next().unwrap());
                    } else {
                        mode = Mode::Normal;
                    }
                }
                continue;
            }
            Mode::EscapeString => {
                cur.push(c);
                if c == '\\' {
                    if let Some(n) = chars.next() {
                        cur.push(n);
                    }
                } else if c == '\'' {
                    if chars.peek() == Some(&'\'') {
                        cur.push(chars.next().unwrap());
                    } else {
                        mode = Mode::Normal;
                    }
                }
                continue;
            }
            Mode::DoubleQuote => {
                cur.push(c);
                if c == '"' {
                    if chars.peek() == Some(&'"') {
                        cur.push(chars.next().unwrap());
                    } else {
                        mode = Mode::Normal;
                    }
                }
                continue;
            }
            Mode::LineComment => {
                cur.push(c);
                if c == '\n' {
                    mode = Mode::Normal;
                }
                continue;
            }
            Mode::BlockComment(depth) => {
                let depth = *depth;
                cur.push(c);
                if c == '/' && chars.peek() == Some(&'*') {
                    cur.push(chars.next().unwrap());
                    mode = Mode::BlockComment(depth + 1);
                } else if c == '*' && chars.peek() == Some(&'/') {
                    cur.push(chars.next().unwrap());
                    mode = if depth <= 1 {
                        Mode::Normal
                    } else {
                        Mode::BlockComment(depth - 1)
                    };
                }
                continue;
            }
            Mode::Dollar(tag) => {
                // 找 `$tag$` 收尾
                if c == '$' {
                    let mut tail = String::from("$");
                    let mut ok = true;
                    for want in tag.chars() {
                        match chars.next() {
                            Some(n) if n == want => tail.push(n),
                            Some(n) => {
                                tail.push(n);
                                ok = false;
                                break;
                            }
                            None => {
                                ok = false;
                                break;
                            }
                        }
                    }
                    if ok {
                        if chars.peek() == Some(&'$') {
                            tail.push(chars.next().unwrap());
                            cur.push_str(&tail);
                            mode = Mode::Normal;
                        } else {
                            cur.push_str(&tail);
                        }
                    } else {
                        cur.push_str(&tail);
                    }
                } else {
                    cur.push(c);
                }
                continue;
            }
            Mode::Normal => {}
        }

        // ── 正常状态：识别各种开头 ──────────────────────────────────────
        match c {
            '\'' => {
                // 前一个是 `E` / `e` 且再前面不是标识符字符 → `E'…'`
                let is_escape = {
                    let bytes = cur.as_bytes();
                    let n = bytes.len();
                    n > 0
                        && (bytes[n - 1] == b'E' || bytes[n - 1] == b'e')
                        && (n < 2 || !is_ident_byte(bytes[n - 2]))
                };
                cur.push(c);
                mode = if is_escape {
                    Mode::EscapeString
                } else {
                    Mode::SingleQuote
                };
            }
            '"' => {
                cur.push(c);
                mode = Mode::DoubleQuote;
            }
            '-' => {
                cur.push(c);
                if chars.peek() == Some(&'-') {
                    cur.push(chars.next().unwrap());
                    mode = Mode::LineComment;
                }
            }
            '/' => {
                cur.push(c);
                if chars.peek() == Some(&'*') {
                    cur.push(chars.next().unwrap());
                    mode = Mode::BlockComment(1);
                }
            }
            '$' => {
                // 可能是美元引用 `$$` / `$tag$`，也可能是参数 `$1`。
                // 先把后面可能的 tag 收进来看看。
                let mut tag = String::new();
                let mut probe = String::new();
                let mut closed = false;
                while let Some(&n) = chars.peek() {
                    if n == '$' {
                        chars.next();
                        closed = true;
                        break;
                    }
                    if n.is_ascii_alphanumeric() || n == '_' {
                        tag.push(n);
                        probe.push(n);
                        chars.next();
                    } else {
                        break;
                    }
                }
                if closed && (tag.is_empty() || tag.chars().next().is_some_and(|c| !c.is_ascii_digit())) {
                    cur.push('$');
                    cur.push_str(&tag);
                    cur.push('$');
                    mode = Mode::Dollar(tag.clone());
                } else {
                    // 不是美元引用（`$1` 或孤立的 `$`），原样写回
                    cur.push('$');
                    cur.push_str(&probe);
                    if closed {
                        cur.push('$');
                    }
                }
            }
            ';' => {
                let stmt = cur.trim().to_owned();
                if !is_blank_statement(&stmt) {
                    out.push(stmt);
                }
                cur.clear();
            }
            _ => cur.push(c),
        }
    }

    let stmt = cur.trim().to_owned();
    if !is_blank_statement(&stmt) {
        out.push(stmt);
    }
    out
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b >= 0x80
}

/// 这条语句是不是"只有空白和注释"（那就没什么可执行的）。
fn is_blank_statement(stmt: &str) -> bool {
    let mut chars = stmt.chars().peekable();
    let mut depth = 0usize;
    let mut saw_code = false;
    while let Some(c) = chars.next() {
        if depth > 0 {
            if c == '/' && chars.peek() == Some(&'*') {
                chars.next();
                depth += 1;
            } else if c == '*' && chars.peek() == Some(&'/') {
                chars.next();
                depth -= 1;
            }
            continue;
        }
        if c == '-' && chars.peek() == Some(&'-') {
            // 跳到行尾
            for n in chars.by_ref() {
                if n == '\n' {
                    break;
                }
            }
            continue;
        }
        if c == '/' && chars.peek() == Some(&'*') {
            chars.next();
            depth = 1;
            continue;
        }
        if !c.is_whitespace() {
            saw_code = true;
            break;
        }
    }
    !saw_code
}

/// 点表名时要填进编辑区的那条 SQL。
///
/// `schema.table` 两段都要引号：PostgreSQL 的标识符大小写敏感
/// （不加引号会被折叠成小写，`"MyTable"` 和 `mytable` 是两张表）。
pub fn select_from_table_sql(schema: &str, table: &str, limit: usize) -> String {
    format!(
        "SELECT * FROM \"{}\".\"{}\" LIMIT {limit}",
        schema.replace('"', "\"\""),
        table.replace('"', "\"\"")
    )
}

/// 库列表。
///
/// `datallowconn` 排掉不允许连接的库（`template0` 那种），
/// `NOT datistemplate` 排掉模板库 —— 两者都不该出现在树里。
pub fn databases_sql() -> &'static str {
    "SELECT datname FROM pg_database \
     WHERE datallowconn AND NOT datistemplate \
     ORDER BY datname"
}

/// 表 / 视图。
///
/// ⚠️ 排除的是两个系统 **schema**（`pg_catalog` / `information_schema`），
/// 不是 MySQL 那种四个系统 **库**。照抄 MySQL 的条件会把 `public` 之外的
/// 用户 schema 一起排掉，或者漏掉 `pg_toast`。
pub fn schema_tables_sql() -> &'static str {
    "SELECT table_schema, table_name, table_type \
     FROM information_schema.tables \
     WHERE table_schema NOT IN ('pg_catalog','information_schema') \
     ORDER BY table_schema, table_name"
}

/// 列名（按 `ordinal_position` 排，和表定义顺序一致）。
pub fn schema_columns_sql() -> &'static str {
    "SELECT table_schema, table_name, column_name \
     FROM information_schema.columns \
     WHERE table_schema NOT IN ('pg_catalog','information_schema') \
     ORDER BY table_schema, table_name, ordinal_position"
}

/// 库名列表（每行一个）。
pub fn parse_databases(table: &CsvTable) -> Vec<String> {
    table
        .rows
        .iter()
        .filter_map(|r| r.first())
        .filter(|s| !s.is_empty())
        .cloned()
        .collect()
}

/// 把实测的两条元数据查询拼成对象树。
///
/// # 关键决定：schema 当"库"用
///
/// PostgreSQL 的实际层级是 **database → schema → table**，比 MySQL 多一层。
/// 我们的界面只有两层（库 → 表），所以这里把 **schema 映射成 `DbNode`**。
///
/// 为什么不映射 database：`information_schema` 是**按库隔离**的 ——
/// 查它只能看到当前连的那个库里的东西，要拿到所有库就得为每个库各跑一次
/// `wslc exec`（每次固定开销约 70ms，库多了就是秒级）。而 schema 在同一次
/// 连接里全都看得到，一次查询就够。
///
/// 代价写在明处：树里显示的是 `public` / `myschema` 而不是库名。
/// 所以界面上要同时显示"当前连的是哪个库"（`DbState::sql_database` 那种）。
pub fn parse_schema_tree(tables: &CsvTable, columns: &CsvTable) -> crate::model::schematree::SchemaTree {
    use crate::model::schematree::{DbNode, SchemaTree, TableKind, TableNode};
    use std::collections::BTreeMap;

    // schema -> 表（BTreeMap 让 schema 和表名都有稳定顺序）
    let mut map: BTreeMap<String, Vec<TableNode>> = BTreeMap::new();
    for row in &tables.rows {
        let (Some(schema), Some(name)) = (row.first(), row.get(1)) else {
            continue;
        };
        if schema.is_empty() || name.is_empty() {
            continue;
        }
        let kind = match row.get(2).map(String::as_str) {
            // 实测：PostgreSQL 给的是 `BASE TABLE` / `VIEW`（带空格），
            // 正好和 `TableKind::parse` 认的字面量一致 —— 但这是**测出来的巧合**，
            // 不是标准保证。所以留一条兜底到 Base。
            Some(k) => TableKind::parse(k),
            None => TableKind::Base,
        };
        map.entry(schema.clone())
            .or_default()
            .push(TableNode::new(name.clone(), kind));
    }

    // 列按实测的 `ordinal_position` 顺序挂上去（CSV 里已经是那个顺序）
    for row in &columns.rows {
        let (Some(schema), Some(table), Some(column)) = (row.first(), row.get(1), row.get(2)) else {
            continue;
        };
        let Some(tables) = map.get_mut(schema) else {
            continue;
        };
        let Some(node) = tables.iter_mut().find(|t| &t.name == table) else {
            // 列在、表不在：两条查询之间表被删了，或者权限不一致。
            // 丢掉这一行，而不是凭空造一张表。
            continue;
        };
        node.columns.push(column.clone());
    }

    SchemaTree {
        databases: map
            .into_iter()
            .map(|(name, tables)| DbNode { name, tables })
            .collect(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    // ── 真实夹具（`docs/postgres-measured.md` 里那些抓取命令的产物）──

    /// 三行：含 NULL、空串、中文、数组字面量。
    const FIX_ROWS: &str = include_str!("../../tests/fixtures/postgres_csv_rows.csv");
    /// 空结果集：**只有表头**（PostgreSQL 比 MySQL 强的地方）。
    const FIX_EMPTY: &str = include_str!("../../tests/fixtures/postgres_csv_empty.csv");
    /// 脏字符：单元格里有 tab / 换行 / 逗号 / 双引号 / 反斜杠。
    const FIX_DIRTY: &str = include_str!("../../tests/fixtures/postgres_csv_dirty.csv");
    const FIX_TABLES: &str = include_str!("../../tests/fixtures/postgres_schema_tables.csv");
    const FIX_COLUMNS: &str = include_str!("../../tests/fixtures/postgres_schema_columns.csv");
    const FIX_TYPES: &str = include_str!("../../tests/fixtures/postgres_types.csv");

    /// `Vec<String>` → `Vec<&str>`，让断言能直接写字面量。
    ///
    /// （`Vec<String>` 和 `Vec<&str>` 之间没有 `PartialEq`，
    /// 不加这一层每一行断言都要写 `.to_owned()`，读起来全是噪音。）
    fn strs(v: &[String]) -> Vec<&str> {
        v.iter().map(|s| s.as_str()).collect()
    }

    #[test]
    fn args_match_the_measured_recipe() {
        let a = client_args("postgres", Some("shop"), "SELECT 1");
        assert_eq!(
            strs(&a),
            vec![
                "-X", "-q", "-v", "ON_ERROR_STOP=1", "-U", "postgres", "-d", "shop", "--csv", "-c",
                "SELECT 1"
            ]
        );
        // 不指定库时不出现 `-d`
        let b = client_args("postgres", None, "SELECT 1");
        assert!(!b.iter().any(|x| x == "-d"));
        // 绝不能有 `-h`：容器内要走 unix socket
        assert!(!b.iter().any(|x| x == "-h"), "加了 -h 会改走 TCP");
        // `--csv` 和 `-q` 都在（缺一个解析就错）
        assert!(b.iter().any(|x| x == "--csv"));
        assert!(b.iter().any(|x| x == "-q"));
        // `-A` 绝不能出现：它不转义值里的 tab/换行
        assert!(!b.iter().any(|x| x == "-A"));
    }

    #[test]
    fn env_passes_password_even_when_empty() {
        assert_eq!(exec_env("s3cret"), "PGPASSWORD=s3cret");
        // 空口令也要传：不传的话 psql 会去试 .pgpass
        assert_eq!(exec_env(""), "PGPASSWORD=");
    }

    #[test]
    fn parses_real_rows_including_null_and_empty_string() {
        let t = parse_csv(FIX_ROWS);
        assert_eq!(strs(&t.header), vec!["id", "name", "nick", "active", "tags"]);
        assert_eq!(t.rows.len(), 3);
        assert_eq!(strs(&t.rows[0]), vec!["1", "alice", "", "t", "{a,b}"]);
        // 中文
        assert_eq!(strs(&t.rows[1]), vec!["2", "bob", "小博", "f", ""]);
        // NULL 与空串**都是空**（项目已定的取舍）
        assert_eq!(strs(&t.rows[2]), vec!["3", "", "", "t", "{}"]);
        assert!(!t.is_empty());
        assert_eq!(t.column_count(), 5);
    }

    #[test]
    fn empty_result_set_still_has_a_header() {
        let t = parse_csv(FIX_EMPTY);
        assert_eq!(strs(&t.header), vec!["id"]);
        assert!(t.rows.is_empty());
        // ⚠️ 有表头就不算"空"：界面要能显示列名
        assert!(!t.is_empty());
    }

    #[test]
    fn zero_bytes_means_no_result_set() {
        // DDL + `-q` 的实测输出就是零字节
        let t = parse_csv("");
        assert!(t.is_empty());
        assert!(t.header.is_empty());
        assert!(t.rows.is_empty());
    }

    #[test]
    fn quoted_cells_survive_tabs_newlines_commas_and_quotes() {
        let t = parse_csv(FIX_DIRTY);
        assert_eq!(strs(&t.header), vec!["id", "v"]);
        assert_eq!(t.rows.len(), 5, "含换行的值不能变成两行");
        // tab 原样保留（这就是不用 TSV 的原因：`-A` 会把它切成两列）
        assert_eq!(t.rows[0][1], "a\tb");
        // 引号内的换行原样保留
        assert_eq!(t.rows[1][1], "line1\nline2");
        assert_eq!(t.rows[2][1], "has,comma");
        assert_eq!(t.rows[3][1], "has\"quote");
        assert_eq!(t.rows[4][1], "has\\backslash");
    }

    #[test]
    fn parses_real_metadata() {
        let t = parse_csv(FIX_TABLES);
        assert_eq!(
            strs(&t.header),
            vec!["table_schema", "table_name", "table_type"]
        );
        assert!(t
            .rows
            .iter()
            .any(|r| strs(r) == vec!["public", "t_user", "BASE TABLE"]));
        // 视图的表类型是 `VIEW`
        assert!(t.rows.iter().any(|r| r[2] == "VIEW"));
        let c = parse_csv(FIX_COLUMNS);
        assert_eq!(c.header.len(), 3);
        assert!(c.rows.iter().all(|r| r[0] == "public"));
    }

    #[test]
    fn parses_types_as_text() {
        let t = parse_csv(FIX_TYPES);
        assert_eq!(t.rows.len(), 1);
        // 数组带逗号 → 被引号包住；数字保尾零；布尔是 t
        assert_eq!(t.rows[0][0], "{1,2}");
        assert_eq!(t.rows[0][2], "1.50");
        assert_eq!(t.rows[0][3], "t");
    }

    #[test]
    fn short_rows_are_padded_not_left_ragged() {
        // 手工造一行少一列的（理论上 psql 不会给，但别让列错位）
        let t = parse_csv("a,b,c\n1,2\n");
        assert_eq!(strs(&t.rows[0]), vec!["1", "2", ""]);
    }

    #[test]
    fn no_trailing_newline_still_parses_the_last_record() {
        let t = parse_csv("a,b\n1,2");
        assert_eq!(strs(&t.header), vec!["a", "b"]);
        assert_eq!(strs(&t.rows[0]), vec!["1", "2"]);
    }

    #[test]
    fn crlf_records_are_tolerated() {
        // psql 实测是 LF，但别让一次换行变化把整张表打乱
        let t = parse_csv("a,b\r\n1,2\r\n");
        assert_eq!(strs(&t.header), vec!["a", "b"]);
        assert_eq!(strs(&t.rows[0]), vec!["1", "2"]);
    }

    // ── 切分 ──────────────────────────────────────────────────────────

    #[test]
    fn splits_simple_statements() {
        assert_eq!(
            split_statements("SELECT 1; SELECT 2;"),
            vec!["SELECT 1", "SELECT 2"]
        );
    }

    #[test]
    fn semicolon_inside_string_is_not_a_separator() {
        assert_eq!(
            split_statements("SELECT 'a;b'; SELECT 2"),
            vec!["SELECT 'a;b'", "SELECT 2"]
        );
        // `''` 是转义出来的单引号，不能让解析器以为字符串结束
        assert_eq!(
            split_statements("SELECT 'it''s; x'"),
            vec!["SELECT 'it''s; x'"]
        );
    }

    #[test]
    fn dollar_quoted_bodies_are_not_split() {
        // PostgreSQL 写函数的标准写法：函数体里全是分号
        let sql = "CREATE FUNCTION f() RETURNS int AS $$ BEGIN PERFORM 1; RETURN 2; END; $$ LANGUAGE plpgsql; SELECT 1;";
        let parts = split_statements(sql);
        assert_eq!(parts.len(), 2, "美元引用里不能切：{parts:?}");
        assert!(parts[0].contains("RETURN 2;"));
        assert_eq!(parts[1], "SELECT 1");
    }

    #[test]
    fn tagged_dollar_quotes_work_and_do_not_confuse_parameters() {
        let sql = "SELECT $tag$ a; b $tag$; SELECT $1";
        let parts = split_statements(sql);
        assert_eq!(parts.len(), 2, "{parts:?}");
        assert_eq!(parts[1], "SELECT $1");
    }

    #[test]
    fn nested_block_comments_are_respected() {
        // PostgreSQL 支持嵌套块注释，MySQL 不支持 —— 遇到第一个 */ 就结束会切错
        let sql = "SELECT 1 /* a /* b; */ c; */; SELECT 2";
        let parts = split_statements(sql);
        assert_eq!(parts.len(), 2, "{parts:?}");
        assert_eq!(parts[1], "SELECT 2");
    }

    #[test]
    fn escape_strings_handle_backslash_quote() {
        // `E'\';'` 里那个 `\'` 不是字符串结束
        let parts = split_statements(r"SELECT E'\';' AS x; SELECT 2");
        assert_eq!(parts.len(), 2, "{parts:?}");
    }

    #[test]
    fn comment_only_and_blank_scripts_produce_nothing() {
        assert!(split_statements("").is_empty());
        assert!(split_statements("   \n\t ").is_empty());
        assert!(split_statements("-- just a note").is_empty());
        assert!(split_statements("/* nothing; here */").is_empty());
        assert!(split_statements(";;;").is_empty());
    }

    #[test]
    fn double_quoted_identifiers_may_contain_semicolons() {
        assert_eq!(
            split_statements(r#"SELECT * FROM "weird;name""#),
            vec![r#"SELECT * FROM "weird;name""#]
        );
    }

    #[test]
    fn select_template_quotes_both_parts() {
        assert_eq!(
            select_from_table_sql("public", "t_user", 200),
            r#"SELECT * FROM "public"."t_user" LIMIT 200"#
        );
        // 引号要双写，否则注入
        assert_eq!(
            select_from_table_sql("pub\"lic", "t", 10),
            r#"SELECT * FROM "pub""lic"."t" LIMIT 10"#
        );
    }

    #[test]
    fn metadata_sql_excludes_pg_schemas_not_mysql_databases() {
        let t = schema_tables_sql();
        assert!(t.contains("table_schema NOT IN ('pg_catalog','information_schema')"));
        // 别把 MySQL 那四个系统库照抄进来
        assert!(!t.contains("'mysql'"));
        assert!(!t.contains("performance_schema"));
        assert!(t.contains("ORDER BY"));
        assert!(schema_columns_sql().contains("ordinal_position"));
        assert!(databases_sql().contains("datallowconn"));
        assert!(databases_sql().contains("NOT datistemplate"));
    }
}
/// 元数据解析的测试（和上面那批分开，因为它要用到 `schematree` 的类型）。
#[cfg(test)]
mod meta_tests {
    use super::*;

    const FIX_TABLES: &str = include_str!("../../tests/fixtures/postgres_schema_tables.csv");
    const FIX_COLUMNS: &str = include_str!("../../tests/fixtures/postgres_schema_columns.csv");
    const FIX_DATABASES: &str = include_str!("../../tests/fixtures/postgres_databases.csv");

    #[test]
    fn parses_database_list() {
        let dbs = parse_databases(&parse_csv(FIX_DATABASES));
        assert_eq!(dbs, vec!["postgres"]);
    }

    #[test]
    fn parses_tree_with_schema_as_top_level() {
        let tree = parse_schema_tree(&parse_csv(FIX_TABLES), &parse_csv(FIX_COLUMNS));
        // 实测这个库里只有一个 schema：public
        assert_eq!(tree.databases.len(), 1);
        let public = &tree.databases[0];
        assert_eq!(public.name, "public");

        // 表都在，视图也在
        let names: Vec<&str> = public.tables.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"t_user"), "缺 t_user：{names:?}");
        assert!(names.contains(&"v_user"), "缺 v_user：{names:?}");

        // 视图的类型必须认出来（`VIEW`），否则界面上会被当成表
        let view = public.tables.iter().find(|t| t.name == "v_user").unwrap();
        assert_eq!(view.kind, crate::model::schematree::TableKind::View);

        // 列挂上了，而且顺序就是表定义顺序
        let user = public.tables.iter().find(|t| t.name == "t_user").unwrap();
        assert_eq!(
            user.columns,
            vec!["id", "name", "nick", "active", "tags", "created"]
        );
    }

    #[test]
    fn columns_for_unknown_tables_are_dropped_not_invented() {
        let tables = parse_csv("table_schema,table_name,table_type\npublic,t,BASE TABLE\n");
        let columns = parse_csv(
            "table_schema,table_name,column_name\npublic,t,c1\npublic,ghost,c2\nother,t,c3\n",
        );
        let tree = parse_schema_tree(&tables, &columns);
        assert_eq!(tree.databases.len(), 1, "不该凭空造 schema");
        assert_eq!(tree.databases[0].tables.len(), 1, "不该凭空造表");
        assert_eq!(tree.databases[0].tables[0].columns, vec!["c1"]);
    }

    #[test]
    fn empty_input_gives_an_empty_tree() {
        let tree = parse_schema_tree(&CsvTable::default(), &CsvTable::default());
        assert!(tree.databases.is_empty());
        assert!(tree.is_empty());
    }
}