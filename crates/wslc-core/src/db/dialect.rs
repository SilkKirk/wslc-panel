//! 方言：**数据**，不是 `if kind == X`。
//!
//! # 为什么是静态数据
//!
//! dbx 把方言做成 38 份 YAML + 热加载（`plugins/dialects/*.yaml`，30+ 能力位）。
//! 那是因为它要覆盖 100+ 数据库，维护成本靠数据化摊薄。
//!
//! 我们只有三种引擎：**几个 `const` 结构体
//! 的成本远低于**"解析器 + 校验器 + 热加载 + 快照测试"那一套。
//! 但形状按数据design —— 将来真要搬进文件，只改 [`dialect_for`] 一处。
//!
//! # 这张表解决什么问题
//!
//! 这些知识原先散在好几处、而且都是写死的：
//!
//! | 知识 | 原先在哪儿 | 现在 |
//! |---|---|---|
//! | 关键字表 | `wslc-panel-core/sqlcomplete.rs` 的两个 `const` | [`DialectData::keywords`] |
//! | 标识符引号 | `schematree` 里写死反引号 | [`DialectData::quote_ident`] |
//! | `LIMIT` 语法 | `schematree` 里写死 `LIMIT 200` | [`DialectData::limit_clause`] |
//! | 点表名那条 SQL | `schematree` 与 `postgres` **各一份** | [`DialectData::select_from_table_sql`] |
//!
//! 三种引擎各有各的写法，散着写必然出现"MySQL 的规则漏到 PostgreSQL 上" ——
//! 点表名那条就是这么坏过一次：控制台一律走 MySQL 那份，于是 PostgreSQL 下
//! 填进编辑区的是反引号 SQL（语法错误）。

/// 标识符怎么包。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuoteStyle {
    /// `` `name` `` —— MySQL / MariaDB。内部反引号双写。
    Backtick,
    /// `"name"` —— PostgreSQL（及 SQL 标准）。内部双引号双写。
    DoubleQuote,
}

/// 取前 N 行的语法。
///
/// # 为什么这是个枚举而不是一个字符串
///
/// 这决定了"点表名时自动生成的那条 SQL"长什么样：
///
/// - MySQL / MariaDB / PostgreSQL 都支持 `LIMIT n`；
/// - 但 PostgreSQL 的标准写法是 `FETCH FIRST n ROWS ONLY`
///   （8.4+ 才有 `LIMIT`，虽然现在都支持）。
///
/// 我们用 `LIMIT` 给两种引擎（PostgreSQL 从 8.4 起就支持，实际不会踩到），
/// 但把差异记在类型里 —— 将来加 SQL Server 时它是 `SELECT TOP n`，
/// 那是**前缀**不是后缀，改一个枚举变体比改拼字符串的地方安全。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitSyntax {
    /// `… LIMIT n`（后缀）。
    Limit,
    /// `… FETCH FIRST n ROWS ONLY`（后缀，SQL 标准）。
    FetchFirst,
}

/// 能力位：这个引擎**有没有**某个功能。
///
/// # 为什么要这个
///
/// 界面按能力位显隐，
/// 而不是在 `views/` 里写 `if kind == Postgres`。
///
/// 用 bool 字段而不是 `bitflags` crate：只有 5 个位，引一个依赖不值得，
/// 而且 `caps.can_show_tables` 比 `caps.contains(CAP_SHOW_TABLES)` 好读。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// 支持 `SHOW TABLES` 这类 MySQL 专有快捷语句。
    pub show_statements: bool,
    /// 有"库"这一层（PostgreSQL 里对应 database；SQLite 没有）。
    pub multiple_databases: bool,
    /// 有 schema 这一层（PostgreSQL 的 `public`；MySQL 里 schema == database）。
    pub schemas: bool,
    /// 支持 `CREATE DATABASE`（SQLite 是单文件，没有这个动作）。
    pub create_database: bool,
    /// 支持在 SQL 里用 `DELIMITER` 定义存储过程（只有 MySQL 客户端这么干）。
    pub delimiter_statement: bool,
}

/// 一种引擎的方言。
pub struct DialectData {
    /// 标识符引号风格。
    pub quote: QuoteStyle,
    /// 取前 N 行的语法。
    pub limit: LimitSyntax,
    /// 行注释的开始符号（`--` 都有，MySQL 还多一个 `#`）。
    pub line_comments: &'static [&'static str],
    /// 关键字 + 常用函数（补全用，按"先骨架后函数"排，不按字母）。
    pub words: &'static [&'static str],
    /// 能力位。
    pub caps: Capabilities,
}

impl DialectData {
    /// 包一个标识符。
    pub fn quote_ident(&self, name: &str) -> String {
        match self.quote {
            QuoteStyle::Backtick => format!("`{}`", name.replace('`', "``")),
            QuoteStyle::DoubleQuote => format!("\"{}\"", name.replace('"', "\"\"")),
        }
    }

    /// 补全用的关键字表。
    ///
    /// 返回 `Vec` 而不是切片：公共词 + 方言词两张静态表要在运行时拼起来，
    /// `const` 里做不了。每次补全调一次，几十个元素的分配相对于"弹一个
    /// 补全菜单"可以忽略。
    pub fn keywords(&self) -> Vec<&'static str> {
        COMMON_WORDS
            .iter()
            .chain(self.words.iter())
            .copied()
            .collect()
    }

    /// `SELECT * FROM … LIMIT n` 的尾部。
    pub fn limit_clause(&self, n: usize) -> String {
        match self.limit {
            LimitSyntax::Limit => format!("LIMIT {n}"),
            LimitSyntax::FetchFirst => format!("FETCH FIRST {n} ROWS ONLY"),
        }
    }

    /// `\`库\`.\`表\`` 形式（引号风格按本方言）。
    pub fn qualified_name(&self, database: &str, table: &str) -> String {
        format!("{}.{}", self.quote_ident(database), self.quote_ident(table))
    }

    /// 点表名时填进编辑区的那条 SQL。
    ///
    /// # 这一份是**唯一**实现（v0.22.3 收口）
    ///
    /// 收之前有两份手写：`model::schematree::select_from_table_sql`（写死反引号）
    /// 和 `model::postgres::select_from_table_sql`（写死双引号）。而 SQL 控制台
    /// 点树时**一律**走 MySQL 那份 —— 于是 PostgreSQL 下点一张表，填进编辑区的是
    /// ``SELECT * FROM `public`.`t_user` LIMIT 200``，反引号在 PostgreSQL 里是
    /// 语法错误，**点表这个动作直接不可用**。
    ///
    /// 根因不是"漏了一个分支"，是"引号风格这两个地方各写了一遍，而别处已经
    /// 有 [`DialectData::quote_ident`]"。所以这里收成一份，调用方按 [`dialect_for`]
    /// 取本引擎的方言就行。
    pub fn select_from_table_sql(&self, database: &str, table: &str, limit: usize) -> String {
        format!(
            "SELECT * FROM {} {}",
            self.qualified_name(database, table),
            self.limit_clause(limit)
        )
    }
}

/// 点表名时默认取多少行。
///
/// 和两个引擎共用的数：一屏看 20~30 行，200 行够翻几屏，又不至于把整张表
/// 拉进内存（`schematree` 的文档里有实测依据）。
pub const DEFAULT_LIMIT: usize = 200;

/// 三种引擎共有的语句骨架（语句 → 子句 → 最常用函数）。
///
/// 不按字母排：用户敲 `SEL` 时 `SELECT` 应该排在 `SHOW` 前面。
const COMMON_WORDS: &[&str] = &[
    "SELECT",
    "FROM",
    "WHERE",
    "GROUP BY",
    "HAVING",
    "ORDER BY",
    "LIMIT",
    "OFFSET",
    "JOIN",
    "LEFT JOIN",
    "RIGHT JOIN",
    "INNER JOIN",
    "CROSS JOIN",
    "ON",
    "USING",
    "INSERT INTO",
    "VALUES",
    "UPDATE",
    "SET",
    "DELETE FROM",
    "CREATE TABLE",
    "CREATE DATABASE",
    "CREATE INDEX",
    "CREATE VIEW",
    "ALTER TABLE",
    "DROP TABLE",
    "DROP DATABASE",
    "TRUNCATE TABLE",
    "DISTINCT",
    "AS",
    "AND",
    "OR",
    "NOT",
    "NULL",
    "IS NULL",
    "IS NOT NULL",
    "IN",
    "BETWEEN",
    "LIKE",
    "EXISTS",
    "UNION",
    "UNION ALL",
    "CASE",
    "WHEN",
    "THEN",
    "ELSE",
    "END",
    "ASC",
    "DESC",
    "COUNT",
    "SUM",
    "AVG",
    "MIN",
    "MAX",
    "COALESCE",
    "NOW",
];

const MYSQL_WORDS: &[&str] = &[
    "SHOW DATABASES",
    "SHOW TABLES",
    "SHOW COLUMNS",
    "SHOW INDEX",
    "SHOW CREATE TABLE",
    "SHOW VARIABLES",
    "SHOW STATUS",
    "SHOW PROCESSLIST",
    "DESCRIBE",
    "EXPLAIN",
    "USE",
    "REPLACE INTO",
    "STRAIGHT_JOIN",
    "SQL_CALC_FOUND_ROWS",
    "FOUND_ROWS",
    "GROUP_CONCAT",
    "IFNULL",
    "DATE_FORMAT",
    "ON DUPLICATE KEY UPDATE",
];

const MARIA_WORDS: &[&str] = &[
    "SHOW DATABASES",
    "SHOW TABLES",
    "SHOW COLUMNS",
    "SHOW INDEX",
    "SHOW CREATE TABLE",
    "SHOW VARIABLES",
    "SHOW STATUS",
    "SHOW PROCESSLIST",
    "DESCRIBE",
    "EXPLAIN",
    "USE",
    "REPLACE INTO",
    "RETURNING",
    "GROUP_CONCAT",
    "IFNULL",
    "DATE_FORMAT",
    "SEQUENCE",
    "NEXT VALUE FOR",
];

/// PostgreSQL 的词表。
///
/// ⚠️ 和 MySQL 的差异必须是真的差异，不能想当然：
/// - `SHOW TABLES` **不存在**（要查 `information_schema` 或 `\dt`）；
/// - `IFNULL` **不存在**（是 `COALESCE`，而 `COALESCE` 在公共表里）；
/// - 取前 N 行是 `LIMIT`（8.4+）或 `FETCH FIRST`；
/// - 有 `RETURNING`（比 MariaDB 早得多，且是 INSERT/UPDATE/DELETE 都能用）；
/// - 有 `SERIAL` / `ON CONFLICT` / `ILIKE` / `DISTINCT ON` 这些独有的东西。
const POSTGRES_WORDS: &[&str] = &[
    "RETURNING",
    "ON CONFLICT",
    "DO NOTHING",
    "DO UPDATE SET",
    "ILIKE",
    "DISTINCT ON",
    "SERIAL",
    "BIGSERIAL",
    "JSONB",
    "TEXT",
    "BOOLEAN",
    "TIMESTAMPTZ",
    "UUID",
    "GEN_RANDOM_UUID",
    "EXPLAIN ANALYZE",
    "VACUUM",
    "ANALYZE",
    "TRUNCATE",
    "SET search_path",
    "CURRENT_TIMESTAMP",
    "DATE_TRUNC",
    "TO_CHAR",
    "ARRAY_AGG",
    "STRING_AGG",
];

static MYSQL_DIALECT: DialectData = DialectData {
    quote: QuoteStyle::Backtick,
    limit: LimitSyntax::Limit,
    // `#` 是 MySQL 的"非标准但真的能用"的行注释 —— MariaDB 也支持。
    line_comments: &["--", "#"],
    words: MYSQL_WORDS,
    caps: Capabilities {
        show_statements: true,
        multiple_databases: true,
        // MySQL 里 schema 就是 database（`CREATE SCHEMA` 是 `CREATE DATABASE` 的别名），
        // 所以不该在界面上再显示一层 schema。
        schemas: false,
        create_database: true,
        delimiter_statement: true,
    },
};

static MARIA_DIALECT: DialectData = DialectData {
    quote: QuoteStyle::Backtick,
    limit: LimitSyntax::Limit,
    line_comments: &["--", "#"],
    words: MARIA_WORDS,
    caps: Capabilities {
        show_statements: true,
        multiple_databases: true,
        schemas: false,
        create_database: true,
        delimiter_statement: true,
    },
};

static POSTGRES_DIALECT: DialectData = DialectData {
    quote: QuoteStyle::DoubleQuote,
    limit: LimitSyntax::Limit,
    // PostgreSQL 只有 `--` 和 `/* */`，**没有** `#`（写了会被当语法错误）。
    line_comments: &["--"],
    words: POSTGRES_WORDS,
    caps: Capabilities {
        show_statements: false,
        multiple_databases: true,
        schemas: true,
        create_database: true,
        delimiter_statement: false,
    },
};

/// 取某个引擎的方言。
pub fn dialect_for(kind: crate::db::conn::DbKind) -> &'static DialectData {
    use crate::db::conn::DbKind;
    match kind {
        DbKind::MySql => &MYSQL_DIALECT,
        DbKind::MariaDb => &MARIA_DIALECT,
        DbKind::Postgres => &POSTGRES_DIALECT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::conn::DbKind;

    #[test]
    fn backtick_and_double_quote_escape_differently() {
        let my = dialect_for(DbKind::MySql);
        let pg = dialect_for(DbKind::Postgres);
        assert_eq!(my.quote_ident("a`b"), "`a``b`");
        // PostgreSQL 用双引号，反引号是普通字符，不转义
        assert_eq!(pg.quote_ident("a`b"), "\"a`b\"");
        assert_eq!(pg.quote_ident("a\"b"), "\"a\"\"b\"");
    }

    #[test]
    fn limit_clause_is_per_dialect() {
        assert_eq!(dialect_for(DbKind::MySql).limit_clause(200), "LIMIT 200");
        assert_eq!(dialect_for(DbKind::Postgres).limit_clause(200), "LIMIT 200");
        // FETCH FIRST 这一支现在没有引擎在用，但分支必须是活的
        let fetch = DialectData {
            limit: LimitSyntax::FetchFirst,
            quote: QuoteStyle::DoubleQuote,
            line_comments: &["--"],
            words: &[],
            caps: POSTGRES_DIALECT.caps,
        };
        assert_eq!(fetch.limit_clause(5), "FETCH FIRST 5 ROWS ONLY");
    }

    #[test]
    fn hash_comments_are_mysql_only() {
        // PostgreSQL 里 `#` 不是注释，写成注释会直接语法错误
        assert!(dialect_for(DbKind::MySql).line_comments.contains(&"#"));
        assert!(dialect_for(DbKind::MariaDb).line_comments.contains(&"#"));
        assert!(!dialect_for(DbKind::Postgres).line_comments.contains(&"#"));
        for kind in [DbKind::MySql, DbKind::MariaDb, DbKind::Postgres] {
            assert!(dialect_for(kind).line_comments.contains(&"--"));
        }
    }

    /// ⚠️ 回归钉（v0.22.3）：点表名生成的 SQL **必须按方言引号**。
    ///
    /// 坏过的样子：控制台一律走 `schematree` 那份（反引号），于是 PostgreSQL
    /// 下点一张表填进编辑区的是 ``SELECT * FROM `public`.`t_user` LIMIT 200``
    /// —— 反引号在 PostgreSQL 里是语法错误，那个动作**点了没用**。
    #[test]
    fn select_from_table_sql_follows_the_dialect() {
        assert_eq!(
            dialect_for(DbKind::MySql).select_from_table_sql("djxm", "sys_user", DEFAULT_LIMIT),
            "SELECT * FROM `djxm`.`sys_user` LIMIT 200"
        );
        assert_eq!(
            dialect_for(DbKind::Postgres).select_from_table_sql("public", "t_user", DEFAULT_LIMIT),
            r#"SELECT * FROM "public"."t_user" LIMIT 200"#
        );
        // 引号内部的引号要双写，否则是注入
        assert_eq!(
            dialect_for(DbKind::MySql).select_from_table_sql("d`b", "t`bl", 10),
            "SELECT * FROM `d``b`.`t``bl` LIMIT 10"
        );
        assert_eq!(
            dialect_for(DbKind::Postgres).select_from_table_sql("pub\"lic", "t", 10),
            r#"SELECT * FROM "pub""lic"."t" LIMIT 10"#
        );
    }

    #[test]
    fn keywords_are_dialect_specific_but_always_include_the_skeleton() {
        let my = DbKind::MySql.keywords();
        let maria = DbKind::MariaDb.keywords();
        let pg = DbKind::Postgres.keywords();

        // 骨架三种都有
        for kw in ["SELECT", "FROM", "WHERE", "JOIN", "COALESCE"] {
            assert!(my.contains(&kw), "MySQL 缺 {kw}");
            assert!(maria.contains(&kw), "MariaDB 缺 {kw}");
            assert!(pg.contains(&kw), "PostgreSQL 缺 {kw}");
        }
        // 各自的独有词，而且**不能串味**
        assert!(my.contains(&"SHOW TABLES"));
        assert!(!pg.contains(&"SHOW TABLES"), "PostgreSQL 没有 SHOW TABLES");
        assert!(maria.contains(&"RETURNING"));
        assert!(my.contains(&"ON DUPLICATE KEY UPDATE"));
        assert!(!pg.contains(&"ON DUPLICATE KEY UPDATE"), "那是 MySQL 的");
        assert!(pg.contains(&"ON CONFLICT"));
        assert!(pg.contains(&"ILIKE"));
        assert!(!my.contains(&"ILIKE"));
        assert!(my.contains(&"IFNULL"), "MySQL 有 IFNULL");
        assert!(!pg.contains(&"IFNULL"), "PostgreSQL 没有 IFNULL");
    }

    #[test]
    fn capability_bits_match_reality() {
        // MySQL 有 SHOW 系列快捷语句，PostgreSQL 没有
        assert!(dialect_for(DbKind::MySql).caps.show_statements);
        assert!(!dialect_for(DbKind::Postgres).caps.show_statements);
        // MySQL 里 schema == database，不该多画一层
        assert!(!dialect_for(DbKind::MySql).caps.schemas);
        assert!(dialect_for(DbKind::Postgres).caps.schemas);
        // DELIMITER 是 mysql 客户端的指令，不是服务端 SQL
        assert!(dialect_for(DbKind::MySql).caps.delimiter_statement);
        assert!(!dialect_for(DbKind::Postgres).caps.delimiter_statement);
    }

    #[test]
    fn keyword_lists_have_no_duplicates() {
        // 重复项会在补全菜单里出现两次
        for kind in [DbKind::MySql, DbKind::MariaDb, DbKind::Postgres] {
            let words = dialect_for(kind).keywords();
            let mut seen = std::collections::HashSet::new();
            for w in &words {
                assert!(seen.insert(*w), "{:?} 里 {:?} 重复了", kind, w);
            }
        }
    }
}
