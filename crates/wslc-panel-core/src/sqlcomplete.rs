//! SQL 编辑器的**方言感知 + 自动补全**（纯逻辑，可单测）。
//!
//! # 这一层只管"该给哪些候选"，不管怎么画
//!
//! GPUI 那一侧要把结果包成 `lsp_types::CompletionItem`（`CompletionProvider`
//! 的签名就是这么定的），但**判断逻辑**全在这里：给定「文本 + 光标」，
//! 算出「替换哪一段 + 给出哪些候选」。这样它能脱离 GPUI 单测 ——
//! 而这类逻辑错了只表现为"补全列表里东西不对"，界面上根本查不出来。
//!
//! # 参考的成品（dbx / DBeaver / DataGrip）都有三档候选
//!
//! 1. **关键字**：按方言给（MySQL 与 MariaDB 有交集也有差集）；
//! 2. **对象名**：库 / 表 / 列，来自 `information_schema` 的元数据；
//! 3. **上下文**：`FROM ` 后面只给表，`别名.` 后面只给那个表的列。
//!
//! 第 3 条是"上下文补全"和"关键词提示"的分水岭 —— 只做第 2 条的编辑器
//! 会在 `FROM ` 后面给你一堆 `SELECT` / `WHERE`，那比没有更烦。
//!
//! # 已知限制
//!
//! - **不做语法分析**：别名靠"扫 FROM/JOIN 后面的词"推出来，不是语法树。
//!   `WITH x AS (...) SELECT * FROM x` 里敲 `x.` 补不出列（`x` 不在表里）。
//!   真要做得建 parser —— 那是另一个量级，而且本项目的定位是
//!   "面板不发 SQL 解析器"（见 `AGENTS.md` 关于不做 XML/文本解析的取舍）。
//! - 列信息来自 `information_schema.COLUMNS`（`schematree::merge_columns`），
//!   没读到就只有表名可补。

use std::collections::HashMap;

use wslc_core::model::schematree::{SchemaTree, TableNode};

/// SQL 方言。
///
/// 只区分我们**真的会连**的两种：MySQL 与 MariaDB。别的数据库这个面板
/// 根本不支持，为它们留变体只会让 match 多几支死代码。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    MySql,
    MariaDb,
}

impl Dialect {
    /// 从探测到的家族名猜方言。
    ///
    /// 认不出就当 MySQL：那是最保守的默认（MariaDB 是它的超集，
    /// 少数几个 MariaDB 独有关键字补不出来，但不会补出**错**的）。
    pub fn from_family(family: &str) -> Self {
        if family.to_ascii_lowercase().contains("maria") {
            Self::MariaDb
        } else {
            Self::MySql
        }
    }

    /// 用反引号包一个标识符（内部反引号双写）。
    ///
    /// MySQL / MariaDB 都是反引号。`ANSI_QUOTES` 模式下双引号也能用，
    /// 但那是可配的 `sql_mode`，不能假设 —— 反引号在两种模式下都成立。
    pub fn quote_ident(self, name: &str) -> String {
        format!("`{}`", name.replace('`', "``"))
    }

    /// 这个方言的关键字 / 常用函数（公共部分 + 方言独有）。
    ///
    /// 返回 `Vec` 而不是 `&'static [&str]`：两张静态表要在运行时拼起来，
    /// 而 `const` 里做不了 Vec 拼接。每次补全调一次，几十个元素的分配
    /// 相对于"弹一个补全菜单"可以忽略。
    ///
    /// 顺序就是列表顺序：先语句骨架、再子句、最后函数 —— 不按字母排，
    /// 用户敲 `SEL` 时 `SELECT` 应该排在 `SHOW` 前面。
    pub fn keywords(self) -> Vec<&'static str> {
        let extra: &[&'static str] = match self {
            Self::MySql => MYSQL_EXTRA,
            Self::MariaDb => MARIA_EXTRA,
        };
        COMMON_WORDS.iter().chain(extra.iter()).copied().collect()
    }
}

/// 两种方言共有：语句骨架 + 子句 + 最常用函数。
const COMMON_WORDS: &[&str] = &[
    "SELECT", "FROM", "WHERE", "GROUP BY", "HAVING", "ORDER BY", "LIMIT", "OFFSET", "JOIN",
    "LEFT JOIN", "RIGHT JOIN", "INNER JOIN", "CROSS JOIN", "ON", "USING", "INSERT INTO", "VALUES",
    "UPDATE", "SET", "DELETE FROM", "REPLACE INTO", "CREATE TABLE", "CREATE DATABASE",
    "CREATE INDEX", "CREATE VIEW", "ALTER TABLE", "DROP TABLE", "DROP DATABASE", "TRUNCATE TABLE",
    "DISTINCT", "AS", "AND", "OR", "NOT", "NULL", "IS NULL", "IS NOT NULL", "IN", "BETWEEN",
    "LIKE", "EXISTS", "UNION", "UNION ALL", "CASE", "WHEN", "THEN", "ELSE", "END", "ASC", "DESC",
    "COUNT", "SUM", "AVG", "MIN", "MAX", "COALESCE", "IFNULL", "NOW", "DATE_FORMAT",
];

/// MySQL 独有 / 惯用。
const MYSQL_EXTRA: &[&str] = &[
    "SHOW DATABASES", "SHOW TABLES", "SHOW COLUMNS", "SHOW INDEX", "SHOW CREATE TABLE",
    "SHOW VARIABLES", "SHOW STATUS", "SHOW PROCESSLIST", "DESCRIBE", "EXPLAIN", "USE",
    "STRAIGHT_JOIN", "SQL_CALC_FOUND_ROWS", "FOUND_ROWS", "GROUP_CONCAT", "ON DUPLICATE KEY UPDATE",
];

/// MariaDB 独有 / 惯用（它比 MySQL 多的那一批）。
const MARIA_EXTRA: &[&str] = &[
    "SHOW DATABASES", "SHOW TABLES", "SHOW COLUMNS", "SHOW INDEX", "SHOW CREATE TABLE",
    "SHOW VARIABLES", "SHOW STATUS", "SHOW PROCESSLIST", "DESCRIBE", "EXPLAIN", "USE", "RETURNING",
    "GROUP_CONCAT", "SEQUENCE", "NEXT VALUE FOR",
];

/// 候选类别（决定列表右侧那句说明）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateKind {
    Keyword,
    Database,
    Table,
    Column,
}

impl CandidateKind {
    pub fn detail(self) -> &'static str {
        match self {
            Self::Keyword => "关键字",
            Self::Database => "库",
            Self::Table => "表",
            Self::Column => "列",
        }
    }
}

/// 一个候选。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// 列表里显示的字，也是插入的文本。
    pub label: String,
    pub kind: CandidateKind,
}

impl Candidate {
    fn new(label: impl Into<String>, kind: CandidateKind) -> Self {
        Self {
            label: label.into(),
            kind,
        }
    }
}

/// 补全结果。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Completion {
    /// 要把文本的**哪一段**换掉（字节偏移）。区间是 `[replace_start, cursor)`。
    ///
    /// 比如敲了 `sel`，就替换这 3 个字节 —— **不能**替换整行，
    /// 否则 `SELECT * FROM x WHERE sel` 补全会把前面的语句吃掉。
    pub replace_start: usize,
    pub items: Vec<Candidate>,
}

impl Completion {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// 候选上限。110 张表 + 800 个列全塞进一个弹窗没法用；50 条已经超过
/// 一屏能扫的量，再多就靠继续敲字收窄。
pub const MAX_ITEMS: usize = 50;

/// 主入口：给定文本 + 光标，算出这次该补什么。
///
/// `cursor` 是**字节偏移**（GPUI 的 `offset` 就是），越界会被夹到文本长度。
pub fn complete(tree: &SchemaTree, dialect: Dialect, text: &str, cursor: usize) -> Completion {
    let cursor = clamp_to_char_boundary(text, cursor.min(text.len()));
    let start = word_start_of(text, cursor);
    let prefix = &text[start..cursor];
    let lower = prefix.to_lowercase();

    let aliases = scan_aliases(text);

    // `别名.` / `库.` 这种限定前缀：只补"点后面"那一类。
    if let Some(q) = qualifier_before_dot(text, start) {
        let lower_q = q.to_lowercase();
        // 先按"别名 / 表名"找列，再按"库名"找表。
        if let Some(t) = resolve_table(tree, &lower_q, &aliases) {
            let items = t
                .columns
                .iter()
                .map(|c| Candidate::new(c.clone(), CandidateKind::Column))
                .collect();
            return build(start, items, &lower, dialect, false);
        }
        if let Some(items) = tables_of_db(tree, &lower_q) {
            return build(start, items, &lower, dialect, false);
        }
        // 认不出（`x.` 而 x 既不是表也不是库）—— **什么都不给**。
        // 退回"全部候选"会把用户的 `x.foo` 变成 `SELECT.foo`，比不补更糟。
        return Completion {
            replace_start: start,
            items: Vec::new(),
        };
    }

    let mut items: Vec<Candidate> = Vec::new();
    if expects_table(&text[..start]) {
        // `FROM `/`JOIN ` 后面只给表 —— 给关键字纯属干扰。
        for db in &tree.databases {
            for t in &db.tables {
                items.push(Candidate::new(
                    format!("{}.{}", db.name, t.name),
                    CandidateKind::Table,
                ));
            }
        }
    } else {
        // 列名也补：单表查询里敲 `na|` 就该出 `name`。
        for db in &tree.databases {
            for t in &db.tables {
                for c in &t.columns {
                    items.push(Candidate::new(c.clone(), CandidateKind::Column));
                }
            }
        }
        for db in &tree.databases {
            items.push(Candidate::new(db.name.clone(), CandidateKind::Database));
            for t in &db.tables {
                items.push(Candidate::new(
                    format!("{}.{}", db.name, t.name),
                    CandidateKind::Table,
                ));
            }
        }
    }

    build(start, items, &lower, dialect, true)
}

/// 按前缀筛一遍、去重、排序、截断。
fn build(
    replace_start: usize,
    items: Vec<Candidate>,
    lower_prefix: &str,
    dialect: Dialect,
    with_keywords: bool,
) -> Completion {
    let mut out: Vec<Candidate> = Vec::new();

    if with_keywords {
        for kw in dialect.keywords() {
            if prefix_matches(kw, lower_prefix) {
                out.push(Candidate::new(kw, CandidateKind::Keyword));
            }
        }
    }

    for it in items {
        // `库.表` 形式下也允许用**裸表名**匹配 —— 用户敲 `sys_user`
        // 想找的是 `djxm.sys_user`。
        let base = it.label.rsplit('.').next().unwrap_or(&it.label);
        if prefix_matches(&it.label, lower_prefix) || prefix_matches(base, lower_prefix) {
            out.push(it);
        }
    }

    // 去重：多个库里有同名表时保留第一个。
    let mut seen = std::collections::HashSet::new();
    out.retain(|c| seen.insert(c.label.clone()));

    // 排序：**短的优先**（`name` 比 `name_full` 更可能是想打的），
    // 等长按字母序 —— 保证每次弹出来的顺序一致，不"跳"。
    out.sort_by(|a, b| a.label.len().cmp(&b.label.len()).then(a.label.cmp(&b.label)));
    out.truncate(MAX_ITEMS);

    Completion {
        replace_start,
        items: out,
    }
}

fn prefix_matches(haystack: &str, lower_prefix: &str) -> bool {
    lower_prefix.is_empty() || haystack.to_lowercase().starts_with(lower_prefix)
}

/// 光标紧跟在 `x.` 后面时，返回 `x`（去反引号）。
fn qualifier_before_dot(text: &str, word_start: usize) -> Option<&str> {
    if word_start == 0 {
        return None;
    }
    let before = &text[..word_start];
    let dot = before.len() - 1;
    if before.as_bytes()[dot] != b'.' {
        return None;
    }
    let q_start = word_start_of(before, dot);
    let q = before[q_start..dot].trim_matches('`');
    if q.is_empty() { None } else { Some(q) }
}

/// `end` 之前的标识符起点（反引号会被一起算进来，替换时一并吃掉）。
fn word_start_of(text: &str, end: usize) -> usize {
    let bytes = text.as_bytes();
    let mut i = end;
    while i > 0 {
        let b = bytes[i - 1];
        if b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b >= 0x80 {
            i -= 1;
        } else {
            break;
        }
    }
    if i > 0 && bytes[i - 1] == b'`' {
        i -= 1;
    }
    i
}

/// 把偏移夹到合法的 UTF-8 字符边界。
///
/// 中文/日文库名里 `cursor` 落在多字节字符中间会让切片 **panic** ——
/// 这是"偶尔崩一次、极难复现"那类 bug，必须在这里挡住。
fn clamp_to_char_boundary(text: &str, mut cursor: usize) -> usize {
    while cursor > 0 && !text.is_char_boundary(cursor) {
        cursor -= 1;
    }
    cursor
}

/// 前一个词是不是"等着要表名"的关键字。
///
/// 这是上下文补全的核心判断：`FROM ` 后面给关键字就是干扰。
fn expects_table(prefix: &str) -> bool {
    match last_word(prefix).as_deref() {
        Some("FROM") | Some("JOIN") | Some("INTO") | Some("UPDATE") | Some("TABLE")
        | Some("DESCRIBE") | Some("DESC") => true,
        _ => false,
    }
}

/// `prefix` 里最后一个词（大写、去掉反引号和标点）。
fn last_word(prefix: &str) -> Option<String> {
    let mut cur = String::new();
    let mut last: Option<String> = None;
    for ch in prefix.chars() {
        if ch.is_alphanumeric() || ch == '_' {
            cur.push(ch);
        } else if !cur.is_empty() {
            last = Some(std::mem::take(&mut cur).to_uppercase());
        }
    }
    if !cur.is_empty() {
        last = Some(cur.to_uppercase());
    }
    last
}

/// 扫出 `FROM 表 [AS] 别名` / `JOIN 表 别名` 的映射（别名 → 表名，都小写）。
///
/// ⚠️ 这是**词级启发式**，不是语法分析：它不认识子查询、不区分
/// `FROM` 出现在字符串里还是注释里。够用是因为它的失败方式是
/// "别名没认出来 → 那一档候选不给"，而不是"给错候选"。
fn scan_aliases(text: &str) -> HashMap<String, String> {
    let tokens = tokens(text);
    let mut map = HashMap::new();
    let mut i = 0;
    while i < tokens.len() {
        let t = tokens[i].to_uppercase();
        if t == "FROM" || t == "JOIN" {
            // 表名
            if i + 1 < tokens.len() {
                let table = strip_qualifier(&tokens[i + 1]);
                let mut k = i + 2;
                // 可选的 AS
                if k < tokens.len() && tokens[k].to_uppercase() == "AS" {
                    k += 1;
                }
                if k < tokens.len() {
                    let alias = tokens[k].to_lowercase();
                    // 别名不能是关键字（`FROM t WHERE` 里的 WHERE 不是别名）
                    if !is_clause_word(&alias) && !alias.contains('.') {
                        map.insert(alias, table.to_lowercase());
                    }
                }
            }
        }
        i += 1;
    }
    map
}

/// `库.表` → `表`。
fn strip_qualifier(s: &str) -> &str {
    s.rsplit('.').next().unwrap_or(s)
}

/// 这个词是不是子句关键字（用来排除"被误当成别名"的情况）。
fn is_clause_word(lower: &str) -> bool {
    matches!(
        lower,
        "where"
            | "group"
            | "order"
            | "having"
            | "limit"
            | "offset"
            | "on"
            | "using"
            | "join"
            | "left"
            | "right"
            | "inner"
            | "cross"
            | "union"
            | "set"
            | "values"
    )
}

/// 把文本切成词：标识符（含 `.` 与反引号里的内容）。
fn tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    for ch in text.chars() {
        match ch {
            '`' => {
                in_quote = !in_quote;
                cur.push(ch);
            }
            _ if in_quote => cur.push(ch),
            c if c.is_alphanumeric() || c == '_' || c == '.' || c == '$' => cur.push(c),
            _ => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    // 去掉反引号，`a`.`b` 这样的会被 `.` 连着切，这里只做去引号
    out.into_iter()
        .map(|t| t.replace('`', ""))
        .filter(|t| !t.is_empty())
        .collect()
}

/// 按"库.表 / 裸表名 / 别名"找表节点。
fn resolve_table<'a>(
    tree: &'a SchemaTree,
    lower_qualifier: &str,
    aliases: &HashMap<String, String>,
) -> Option<&'a TableNode> {
    // 1) 别名
    if let Some(table) = aliases.get(lower_qualifier) {
        if let Some(t) = find_table(tree, table, None) {
            return Some(t);
        }
    }
    // 2) `库.表`
    if let Some((db, table)) = lower_qualifier.split_once('.') {
        if let Some(t) = find_table(tree, table, Some(db)) {
            return Some(t);
        }
    }
    // 3) 裸表名（多个库同名时取第一个）
    find_table(tree, lower_qualifier, None)
}

fn find_table<'a>(
    tree: &'a SchemaTree,
    table_lower: &str,
    db_lower: Option<&str>,
) -> Option<&'a TableNode> {
    for db in &tree.databases {
        if let Some(want) = db_lower {
            if db.name.to_lowercase() != want {
                continue;
            }
        }
        for t in &db.tables {
            if t.name.to_lowercase() == table_lower {
                return Some(t);
            }
        }
    }
    None
}

/// 某个库下的所有表。
fn tables_of_db(tree: &SchemaTree, lower_db: &str) -> Option<Vec<Candidate>> {
    tree.databases
        .iter()
        .find(|d| d.name.to_lowercase() == lower_db)
        .map(|d| {
            d.tables
                .iter()
                .map(|t| Candidate::new(t.name.clone(), CandidateKind::Table))
                .collect()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wslc_core::model::schematree::{parse_schema_tree, DbNode, TableKind, TableNode};

    fn sample() -> SchemaTree {
        let mut tree = parse_schema_tree(
            "djxm\tsys_user\tBASE TABLE\n\
             djxm\tsys_job\tBASE TABLE\n\
             shop\torders\tBASE TABLE\n",
        );
        // 手工挂列（真实路径是 `merge_columns` 从 information_schema 读）。
        for db in &mut tree.databases {
            for t in &mut db.tables {
                t.columns = match t.name.as_str() {
                    "sys_user" => vec!["id".into(), "user_name".into(), "nick_name".into()],
                    "sys_job" => vec!["job_id".into(), "job_name".into()],
                    _ => vec!["order_id".into()],
                };
            }
        }
        tree
    }

    fn labels(c: &Completion) -> Vec<String> {
        c.items.iter().map(|i| i.label.clone()).collect()
    }

    fn at_end(c: &Completion, text: &str) -> usize {
        text.len() - c.replace_start
    }

    #[test]
    fn dialect_is_detected_from_family_name() {
        assert_eq!(Dialect::from_family("MariaDB"), Dialect::MariaDb);
        assert_eq!(Dialect::from_family("mariadb 10.6"), Dialect::MariaDb);
        assert_eq!(Dialect::from_family("MySQL"), Dialect::MySql);
        // 认不出就当 MySQL（保守默认）
        assert_eq!(Dialect::from_family(""), Dialect::MySql);
        assert_eq!(Dialect::from_family("PostgreSQL"), Dialect::MySql);
    }

    #[test]
    fn dialect_quoters_escape_backticks() {
        assert_eq!(Dialect::MySql.quote_ident("order"), "`order`");
        assert_eq!(Dialect::MySql.quote_ident("a`b"), "`a``b`");
        assert_eq!(Dialect::MariaDb.quote_ident("order"), "`order`");
    }

    #[test]
    fn keywords_differ_between_dialects() {
        assert!(Dialect::MariaDb.keywords().contains(&"RETURNING"));
        assert!(!Dialect::MySql.keywords().contains(&"RETURNING"));
        assert!(Dialect::MySql.keywords().contains(&"STRAIGHT_JOIN"));
        assert!(!Dialect::MariaDb.keywords().contains(&"STRAIGHT_JOIN"));
        // 公共部分两边都要有
        for kw in ["SELECT", "FROM", "WHERE"] {
            assert!(Dialect::MySql.keywords().contains(&kw));
            assert!(Dialect::MariaDb.keywords().contains(&kw));
        }
    }

    #[test]
    fn after_from_only_tables_are_offered() {
        let t = sample();
        let text = "SELECT * FROM ";
        let c = complete(&t, Dialect::MySql, text, text.len());
        assert!(!c.is_empty());
        assert!(
            c.items.iter().all(|i| i.kind == CandidateKind::Table),
            "FROM 后面不该给关键字：{:?}",
            labels(&c)
        );
        assert!(labels(&c).contains(&"djxm.sys_user".to_string()));
    }

    #[test]
    fn after_alias_dot_only_that_tables_columns_are_offered() {
        let t = sample();
        let text = "SELECT u. FROM djxm.sys_user u";
        let cursor = text.find(" FROM").unwrap();
        let c = complete(&t, Dialect::MySql, text, cursor);
        assert_eq!(
            labels(&c),
            vec!["id", "user_name", "nick_name"],
            "`u.` 应该只出 sys_user 的列（靠别名推出来的）"
        );
        assert!(c.items.iter().all(|i| i.kind == CandidateKind::Column));
        assert_eq!(c.replace_start, cursor);
    }

    #[test]
    fn alias_with_as_keyword_is_recognized() {
        let t = sample();
        let text = "SELECT j.job FROM djxm.sys_job AS j";
        let cursor = text.find(" FROM").unwrap();
        let c = complete(&t, Dialect::MySql, text, cursor);
        assert_eq!(labels(&c), vec!["job_id", "job_name"]);
    }

    #[test]
    fn database_qualifier_lists_its_tables() {
        let t = sample();
        let text = "SELECT * FROM shop.";
        let c = complete(&t, Dialect::MySql, text, text.len());
        assert_eq!(labels(&c), vec!["orders"]);
    }

    #[test]
    fn unknown_qualifier_gives_nothing_rather_than_everything() {
        let t = sample();
        let text = "SELECT x.";
        let c = complete(&t, Dialect::MySql, text, text.len());
        assert!(
            c.is_empty(),
            "认不出的限定符不该退回全部候选（会把 x.foo 变成 SELECT.foo）"
        );
    }

    #[test]
    fn prefix_filters_and_keywords_come_first() {
        let t = sample();
        let text = "sel";
        let c = complete(&t, Dialect::MySql, text, text.len());
        assert_eq!(c.items[0].label, "SELECT");
        assert_eq!(c.items[0].kind, CandidateKind::Keyword);
        assert_eq!(c.replace_start, 0);
    }

    #[test]
    fn replacing_only_the_prefix_not_the_whole_line() {
        let t = sample();
        let text = "SELECT * FROM djxm.sys_us";
        let c = complete(&t, Dialect::MySql, text, text.len());
        assert_eq!(at_end(&c, text), "sys_us".len());
        assert!(labels(&c).contains(&"djxm.sys_user".to_string()));
    }

    #[test]
    fn case_insensitive_matching() {
        let t = sample();
        let text = "select * from SYS_U";
        let c = complete(&t, Dialect::MySql, text, text.len());
        assert!(labels(&c).contains(&"djxm.sys_user".to_string()));
    }

    #[test]
    fn empty_prefix_offers_a_bounded_list() {
        let t = sample();
        let c = complete(&t, Dialect::MySql, "", 0);
        assert!(!c.is_empty());
        assert!(c.items.len() <= MAX_ITEMS);
    }

    #[test]
    fn cursor_inside_multibyte_text_does_not_panic() {
        let t = sample();
        let text = "SELECT '中文";
        for cut in 0..=text.len() {
            let _ = complete(&t, Dialect::MySql, text, cut);
        }
        let mut t2 = sample();
        t2.databases.push(DbNode {
            name: "测试库".into(),
            tables: vec![TableNode::new("用户表", TableKind::Base)],
        });
        let text2 = "SELECT * FROM 测试";
        let c2 = complete(&t2, Dialect::MySql, text2, text2.len());
        assert!(c2.items.iter().any(|i| i.label.contains("测试库")));
    }

    #[test]
    fn backtick_quoted_word_replaces_from_the_backtick() {
        let t = sample();
        let text = "SELECT * FROM `djxm`.`sys_us";
        let c = complete(&t, Dialect::MySql, text, text.len());
        // 反引号要被一起替换，否则会变成 `` `djxm`.`djxm.sys_user` ``
        assert_eq!(text[c.replace_start..].trim_start_matches('`'), "sys_us");
    }

    #[test]
    fn nothing_typed_after_dot_lists_all_columns() {
        let t = sample();
        let text = "SELECT sys_user.";
        let c = complete(&t, Dialect::MySql, text, text.len());
        assert_eq!(labels(&c), vec!["id", "user_name", "nick_name"]);
    }

    #[test]
    fn clause_words_are_not_taken_as_aliases() {
        let t = sample();
        // `WHERE` 不该被当成 sys_user 的别名
        let text = "SELECT * FROM djxm.sys_user WHERE ";
        let c = complete(&t, Dialect::MySql, text, text.len());
        // WHERE 后面给的是关键字/列，不该只有 sys_user 的列
        assert!(
            c.items.iter().any(|i| i.kind == CandidateKind::Keyword),
            "WHERE 之后应该给关键字"
        );
    }
}
