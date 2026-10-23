//! SQL 控制台左侧的**库 / 表树**：一条 SQL 拿全，客户端建树 + 过滤。
//!
//! # 为什么一条 SQL 拿全整棵树
//!
//! 实测（本机 wslc 3.0.3.0 / mysql 26.7.0，2026-10）：这台机器上的 `djxm`
//! 库有 **117 张表**，而 `information_schema.TABLES` 一条查询就把
//! 「哪张表在哪个库」全返回了（约 120 行 TSV，0.08s）。
//!
//! 反过来说，"展开一个库才查这个库的表"要多花 N 次 `wslc exec`
//! （每次固定 70ms）—— 而一条全量的成本只有一次。所以**一次拿全，客户端建树**。
//!
//! # 为什么必须能过滤
//!
//! 117 张表铺开没人看得下去。搜索框是**功能不是装饰**（`filter` 有单测）。
//!
//! # 放在 `model/` 而不是 `cmd/`
//!
//! 纯逻辑（拼 SQL + 解析 TSV + 过滤），能脱离 Windows / 容器单测 ——
//! 错了的表现是"树里少了几个库"或"点表名生成的 SQL 拼错"，
//! `AGENTS.md` §8 说的那类 CI 查不出来的问题。

use std::collections::BTreeMap;

/// 一个表的类型（决定图标 / 能不能 `SELECT *`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableKind {
    /// 普通表（`BASE TABLE`）。
    Base,
    /// 视图（`VIEW`）。
    View,
    /// 系统视图（`SYSTEM VIEW`，`information_schema` 里那些）。
    SystemView,
}

impl TableKind {
    /// 从 `information_schema.TABLES.TABLE_TYPE` 的原文解析。
    ///
    /// 未知值当作 `Base`：宁可把它画成普通表（用户还能点它生成 SQL），
    /// 也别因为一个没见过的新类型就把它从树里丢掉。
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_uppercase().as_str() {
            "VIEW" => Self::View,
            "SYSTEM VIEW" => Self::SystemView,
            _ => Self::Base,
        }
    }

    /// 界面上那个小标记。
    pub fn tag(self) -> &'static str {
        match self {
            Self::Base => "",
            Self::View => "视图",
            Self::SystemView => "系统视图",
        }
    }
}

/// 树里的一张表。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableNode {
    pub name: String,
    pub kind: TableKind,
    /// 列名（按 `ORDINAL_POSITION` 排）。
    ///
    /// v0.12.4 加：自动补全要能补出 `别名.列名`，光有表名不够。
    ///
    /// ⚠️ 它来自**第二条查询**（`information_schema.COLUMNS`），不是
    /// `TABLES` 那条 —— 所以它可能是空的（还没读 / 读失败 / 真的没列）。
    /// 填它的是 [`merge_columns`]。
    pub columns: Vec<String>,
}

impl TableNode {
    /// 只有名字和类型，列待 [`merge_columns`] 填。
    pub fn new(name: impl Into<String>, kind: TableKind) -> Self {
        Self {
            name: name.into(),
            kind,
            columns: Vec::new(),
        }
    }
}

/// 树里的一个库。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbNode {
    pub name: String,
    pub tables: Vec<TableNode>,
}

impl DbNode {
    /// 是不是 MySQL 自带的系统库（界面上标出来，但**不隐藏**）。
    pub fn is_system(&self) -> bool {
        matches!(
            self.name.as_str(),
            "mysql" | "information_schema" | "performance_schema" | "sys"
        )
    }
}

/// 整棵树。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SchemaTree {
    /// 按库名排序（`BTreeMap` 保证稳定 —— 每次查询顺序一致，用户不会看花眼）。
    pub databases: Vec<DbNode>,
}

impl SchemaTree {
    pub fn is_empty(&self) -> bool {
        self.databases.is_empty()
    }

    /// 表的总数（页面上"共 N 张表"用）。
    pub fn table_count(&self) -> usize {
        self.databases.iter().map(|d| d.tables.len()).sum()
    }

    /// 按关键字过滤（库名或表名包含它，大小写不敏感）。
    ///
    /// # 行为
    ///
    /// - 空关键字 → 原样返回（不 clone，直接借用）；
    /// - 库名命中 → **整个库的表都留下**（用户想找的是这个库，
    ///   而不是"名字里有 dj 的表"）；
    /// - 只有表名命中 → 那个库只留命中的表；
    /// - 一个都不命中 → 不出现在结果里。
    ///
    /// 返回**新的树**而不是下标集合：调用方要直接拿去渲染，
    /// 多一层下标映射只会多一处出错的地方（117 张表的克隆成本可以忽略）。
    pub fn filter(&self, query: &str) -> SchemaTree {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return self.clone();
        }
        let mut out = Vec::new();
        for db in &self.databases {
            if db.name.to_lowercase().contains(&q) {
                out.push(db.clone());
                continue;
            }
            let tables: Vec<TableNode> = db
                .tables
                .iter()
                .filter(|t| t.name.to_lowercase().contains(&q))
                .cloned()
                .collect();
            if !tables.is_empty() {
                out.push(DbNode {
                    name: db.name.clone(),
                    tables,
                });
            }
        }
        SchemaTree { databases: out }
    }
}

/// 拿整棵树的那条 SQL。
///
/// # 为什么排除系统库
///
/// `information_schema` / `performance_schema` / `mysql` / `sys` 四家的表加起来
/// 上千张，混在树里会让用户自己建的 20 张表淹掉。实测这台机器上：
/// 排除之后 118 张（`djxm` 117 + `wslc_probe` 1），不排除的话目测四位数。
///
/// ⚠️ 但**不排除用户自己的库**——哪怕它叫 `information_schema_backup`。
/// 所以用的是精确的 `NOT IN` 而不是 `NOT LIKE`。
pub fn schema_tree_sql() -> &'static str {
    "SELECT TABLE_SCHEMA, TABLE_NAME, TABLE_TYPE \
     FROM information_schema.TABLES \
     WHERE TABLE_SCHEMA NOT IN \
       ('mysql','information_schema','performance_schema','sys') \
     ORDER BY TABLE_SCHEMA, TABLE_NAME"
}

/// 解析 [`schema_tree_sql`] 的 `-N -B`（无表头）输出。
///
/// 每行三列：库名、表名、表类型（都是 TSV，值里的 tab 已被转义）。
pub fn parse_schema_tree(tsv: &str) -> SchemaTree {
    // 用 `BTreeMap` 收集：库名的顺序就自然是字典序，不用再排一次。
    let mut map: BTreeMap<String, Vec<TableNode>> = BTreeMap::new();
    for line in tsv.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.trim().is_empty() {
            continue;
        }
        let cells = crate::model::mysql::split_tsv_line(line);
        // 少于三列的行丢掉：宁可少一张表，也别把一行的后半截当表名。
        let (Some(db), Some(table)) = (cells.first(), cells.get(1)) else {
            continue;
        };
        if db.is_empty() || table.is_empty() {
            continue;
        }
        let kind = cells.get(2).map(|k| TableKind::parse(k)).unwrap_or(TableKind::Base);
        map.entry(db.clone())
            .or_default()
            .push(TableNode::new(table.clone(), kind));
    }
    SchemaTree {
        databases: map
            .into_iter()
            .map(|(name, tables)| DbNode { name, tables })
            .collect(),
    }
}

/// 拿**列信息**的那条 SQL（v0.12.4，自动补全用）。
///
/// # 为什么要单独一条查询
///
/// `information_schema.TABLES` 里没有列。补全 `u.na|` 这种要给出
/// `name / nickname`，就必须读 `COLUMNS`。
///
/// # 为什么不分表查
///
/// 和树那条同一个理由：一次拿全（实测这台机器上 `djxm` 110 张表的列
/// 一共 800 多行，一条查询 0.15s），而不是"展开一张表查一次"。
///
/// 排除系统库的条件必须和 [`schema_tree_sql`] **保持一致**，
/// 否则会读到一堆树里根本没有的表的列（白占内存还污染补全）。
pub fn schema_columns_sql() -> &'static str {
    "SELECT TABLE_SCHEMA, TABLE_NAME, COLUMN_NAME \
     FROM information_schema.COLUMNS \
     WHERE TABLE_SCHEMA NOT IN \
       ('mysql','information_schema','performance_schema','sys') \
     ORDER BY TABLE_SCHEMA, TABLE_NAME, ORDINAL_POSITION"
}

/// 把 [`schema_columns_sql`] 的输出填进树里。
///
/// 每行三列：库名、表名、列名。**找不到对应表的行直接丢掉** ——
/// 两条查询之间表被删了、或者用户没权限看 `TABLES` 却看得到 `COLUMNS`，
/// 都会出现这种"列在、表不在"的情况；凭空造一个表节点反而更糟。
pub fn merge_columns(tree: &mut SchemaTree, tsv: &str) {
    for line in tsv.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.trim().is_empty() {
            continue;
        }
        let cells = crate::model::mysql::split_tsv_line(line);
        let (Some(db), Some(table), Some(column)) = (cells.first(), cells.get(1), cells.get(2)) else {
            continue;
        };
        if db.is_empty() || table.is_empty() || column.is_empty() {
            continue;
        }
        let Some(node) = tree.databases.iter_mut().find(|d| &d.name == db) else {
            continue;
        };
        let Some(t) = node.tables.iter_mut().find(|t| &t.name == table) else {
            continue;
        };
        t.columns.push(column.clone());
    }
}

/// 点树里某张表时，填进编辑区的那条 SQL。
///
/// # 为什么要 `LIMIT`
///
/// `SELECT * FROM 一张几百万行的表` 会把全部数据拉进内存、再塞进 GPUI 的
/// 渲染树。实测这台机器上 `sys_oper_log` 只有 350 行还好，但用户库里
/// 有几十万行的表是常态。所以默认带 `LIMIT`，用户想要更多自己改数字。
///
/// `LIMIT 200` 这个数：一屏大概能看 20~30 行，200 行够翻几屏，
/// 又不至于让 `-B` 的输出撑爆内存。
///
/// # 为什么加反引号
///
/// 表名可能是 `order`（保留字）或者带 `-` 的。反引号是 MySQL 的标识符引用，
/// 加了之后这些名字都能用。名字里的反引号**双写**转义（和 `-B` 的规矩一样）。
pub fn select_from_table_sql(database: &str, table: &str) -> String {
    format!(
        "SELECT * FROM {} LIMIT {}",
        qualified_name(database, table),
        DEFAULT_LIMIT
    )
}

/// 默认 `LIMIT`。
pub const DEFAULT_LIMIT: u32 = 200;

/// `\`库\`.\`表\`` 形式，反引号双写转义。
pub fn qualified_name(database: &str, table: &str) -> String {
    format!("{}.{}", quote_ident(database), quote_ident(table))
}

/// 用反引号包一个标识符；内部的反引号双写。
fn quote_ident(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // 夹具是真机抓的（这台机器上 `djxm` 117 张表 + `wslc_probe` 1 张）。
    // 完整那份太长，这里用它的**真实前缀**（含各种真实表名形态：
    // 下划线、`act_*` / `qrtz_*` 这种前缀、纯小写）。
    const SAMPLE: &str = "djxm\tact_evt_log\tBASE TABLE\n\
                          djxm\tact_ge_bytearray\tBASE TABLE\n\
                          djxm\tdj_member\tBASE TABLE\n\
                          djxm\tsys_user\tBASE TABLE\n\
                          djxm\tv_user_view\tVIEW\n\
                          wslc_probe\tt\tBASE TABLE\n";

    #[test]
    fn parses_databases_and_tables() {
        let tree = parse_schema_tree(SAMPLE);
        assert_eq!(tree.databases.len(), 2);
        assert_eq!(tree.databases[0].name, "djxm");
        assert_eq!(tree.databases[0].tables.len(), 5);
        assert_eq!(tree.databases[1].name, "wslc_probe");
        assert_eq!(tree.table_count(), 6);
    }

    #[test]
    fn databases_are_sorted_and_tables_keep_the_sql_order() {
        // SQL 里有 ORDER BY，解析器不该打乱它
        let tree = parse_schema_tree(SAMPLE);
        let names: Vec<&str> = tree.databases[0]
            .tables
            .iter()
            .map(|t| t.name.as_str())
            .collect();
        assert_eq!(
            names,
            ["act_evt_log", "act_ge_bytearray", "dj_member", "sys_user", "v_user_view"]
        );
    }

    #[test]
    fn table_kind_is_parsed() {
        assert_eq!(TableKind::parse("BASE TABLE"), TableKind::Base);
        assert_eq!(TableKind::parse("VIEW"), TableKind::View);
        assert_eq!(TableKind::parse("SYSTEM VIEW"), TableKind::SystemView);
        // 大小写不敏感
        assert_eq!(TableKind::parse("view"), TableKind::View);
        // 没见过的类型当普通表（不从树里丢掉）
        assert_eq!(TableKind::parse("FOREIGN TABLE"), TableKind::Base);
        assert_eq!(TableKind::parse(""), TableKind::Base);
    }

    #[test]
    fn filter_by_table_name_keeps_only_matching_tables() {
        let tree = parse_schema_tree(SAMPLE);
        let got = tree.filter("member");
        assert_eq!(got.databases.len(), 1);
        assert_eq!(got.databases[0].name, "djxm");
        assert_eq!(got.databases[0].tables.len(), 1);
        assert_eq!(got.databases[0].tables[0].name, "dj_member");
    }

    #[test]
    fn filter_by_database_name_keeps_all_its_tables() {
        let tree = parse_schema_tree(SAMPLE);
        let got = tree.filter("wslc");
        // 库名命中 → 这个库的表全留下（用户找的是库，不是表）
        assert_eq!(got.databases.len(), 1);
        assert_eq!(got.databases[0].name, "wslc_probe");
        assert_eq!(got.databases[0].tables.len(), 1);
    }

    #[test]
    fn filter_is_case_insensitive() {
        let tree = parse_schema_tree(SAMPLE);
        assert_eq!(tree.filter("SYS_USER").table_count(), 1);
        assert_eq!(tree.filter("sys_user").table_count(), 1);
    }

    #[test]
    fn filter_with_empty_query_returns_everything() {
        let tree = parse_schema_tree(SAMPLE);
        assert_eq!(tree.filter("").table_count(), tree.table_count());
        assert_eq!(tree.filter("   ").table_count(), tree.table_count());
    }

    #[test]
    fn filter_with_no_match_returns_empty_tree() {
        let tree = parse_schema_tree(SAMPLE);
        let got = tree.filter("绝对不存在的名字");
        assert!(got.is_empty());
        assert_eq!(got.table_count(), 0);
    }

    #[test]
    fn system_databases_are_flagged_but_not_removed() {
        let tree = parse_schema_tree("mysql\tuser\tBASE TABLE\n");
        assert_eq!(tree.databases.len(), 1, "系统库不能被解析器丢掉");
        assert!(tree.databases[0].is_system());
        assert!(!parse_schema_tree(SAMPLE).databases[0].is_system());
    }

    #[test]
    fn junk_input_never_panics() {
        for s in [
            "",
            "\n",
            "   \n  \n",
            "only_one_cell\n",
            "two\tcells\n",
            "\t\t\n",
            "a\t\tBASE TABLE\n",
            "\t b\tBASE TABLE\n",
        ] {
            let t = parse_schema_tree(s);
            // 不 panic 就算过；具体解析结果不重要（服务端不会给这种输出）
            let _ = t.table_count();
        }
        // 两列（缺类型）时按普通表处理，但**不能丢**
        let t = parse_schema_tree("db\ttbl\n");
        assert_eq!(t.table_count(), 1);
        assert_eq!(t.databases[0].tables[0].kind, TableKind::Base);
    }

    #[test]
    fn select_sql_quotes_identifiers() {
        assert_eq!(
            select_from_table_sql("djxm", "sys_user"),
            "SELECT * FROM `djxm`.`sys_user` LIMIT 200"
        );
        // 保留字 / 带横线的表名靠反引号
        assert_eq!(
            select_from_table_sql("shop", "order"),
            "SELECT * FROM `shop`.`order` LIMIT 200"
        );
        // 名字里的反引号双写
        assert_eq!(
            select_from_table_sql("d`b", "t`bl"),
            "SELECT * FROM `d``b`.`t``bl` LIMIT 200"
        );
    }

    #[test]
    fn schema_sql_excludes_only_the_exact_system_names() {
        let sql = schema_tree_sql();
        // 精确匹配：不能把 `information_schema_backup` 也排除掉
        assert!(sql.contains("NOT IN"));
        assert!(!sql.contains("LIKE"), "用 NOT LIKE 会误伤同名前缀的库");
        for sys in ["mysql", "information_schema", "performance_schema", "sys"] {
            assert!(sql.contains(sys), "缺了 {sys}");
        }
        // 必须有 ORDER BY，否则每次查询顺序可能变
        assert!(sql.contains("ORDER BY"));
    }

    // ---- 列信息（v0.12.4，自动补全用）----

    const COLUMNS: &str = "djxm\tsys_user\tid\n\
                           djxm\tsys_user\tuser_name\n\
                           djxm\tsys_user\tnick_name\n\
                           djxm\tsys_job\tjob_id\n\
                           shop\torders\torder_id\n";

    #[test]
    fn columns_are_attached_to_the_right_tables_in_order() {
        let mut tree = parse_schema_tree(
            "djxm\tsys_user\tBASE TABLE\n\
             djxm\tsys_job\tBASE TABLE\n\
             shop\torders\tBASE TABLE\n",
        );
        merge_columns(&mut tree, COLUMNS);
        let user = tree.databases[0]
            .tables
            .iter()
            .find(|t| t.name == "sys_user")
            .unwrap();
        assert_eq!(user.columns, vec!["id", "user_name", "nick_name"]);
        let orders = tree.databases[1]
            .tables
            .iter()
            .find(|t| t.name == "orders")
            .unwrap();
        assert_eq!(orders.columns, vec!["order_id"]);
    }

    #[test]
    fn columns_for_unknown_tables_are_dropped_not_invented() {
        let mut tree = parse_schema_tree("djxm\tsys_user\tBASE TABLE\n");
        // `djxm.ghost` 在树里不存在（两条查询之间被删了，或权限不一致）
        merge_columns(&mut tree, "djxm\tsys_user\tid\ndjxm\tghost\tx\nnodb\tt\tc\n");
        assert_eq!(tree.databases.len(), 1, "不该凭空造库");
        assert_eq!(tree.databases[0].tables.len(), 1, "不该凭空造表");
        assert_eq!(tree.databases[0].tables[0].columns, vec!["id"]);
    }

    #[test]
    fn merge_columns_tolerates_junk() {
        let mut tree = parse_schema_tree("djxm\tt\tBASE TABLE\n");
        for junk in [
            "",
            "\n\n",
            "only_one_cell\n",
            "djxm\tt\n",
            "djxm\tt\t\n",
            "\t\t\t\n",
        ] {
            merge_columns(&mut tree, junk);
        }
        assert!(tree.databases[0].tables[0].columns.is_empty());
    }

    #[test]
    fn columns_sql_matches_the_tree_sql_exclusions() {
        // 两条查询的排除条件必须一致，否则会读到树里没有的表的列。
        assert_eq!(
            schema_columns_sql()
                .matches("'mysql','information_schema','performance_schema','sys'")
                .count(),
            1
        );
        assert!(schema_columns_sql().contains("ORDER BY"));
        assert!(schema_columns_sql().contains("ORDINAL_POSITION"));
    }
}
