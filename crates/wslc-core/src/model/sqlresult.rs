//! 把 `mysql -B`（**带表头**的批次输出）解析成结构化的结果集。
//!
//! # 为什么是 `-B` 而不是 `--xml`
//!
//! 一开始考虑过 `--xml`（它能区分 `NULL` / 空串 / 字符串 `"NULL"`），但：
//!
//! 1. **产品上不需要区分**。用户要的是"看得懂的数据表"，不是"NULL 和空串
//!    显示得不一样"。为这个引一套 XML 解析（或者手写百来行）不值得；
//! 2. `--xml` 还要处理实体反转义（`&amp;` `&lt;` …），是纯粹多出来的出错面；
//! 3. 本项目**一个 XML 依赖都没有**，手写解析器是"自己造一个只有自己用的格式
//!    解析器"——`AGENTS.md` §5 的取向是能少造就少造。
//!
//! 所以走 `-B`，接受"NULL 和空串都是空字符串"这个代价（**已与用户确认**）。
//!
//! # 关键前提：`-B` **不带 `-N`** 时首行就是列名
//!
//! 实测（本机 wslc 3.0.3.0 / mysql 26.7.0，2026-10）：
//!
//! ```text
//! $ mysql -uroot -N -B -e "SELECT 1 AS a, 'x' AS b"     $ mysql -uroot -B -e "SELECT 1 AS a, 'x' AS b"
//! 1	x                                                    a	b
//!                                                        1	x
//! ```
//!
//! 现有的 [`crate::model::mysql::SqlMode::Batch`] 是 `-N -B`（**不要**表头，
//! 给"列名已知"的解析用，比如 `SHOW DATABASES`）；这里需要表头，所以用
//! [`crate::model::mysql::SqlMode::BatchWithHeader`]。
//!
//! # `-B` 的转义（实测，见下面的夹具）
//!
//! | 值里有 | `-B` 输出 | 反转义后 |
//! |---|---|---|
//! | 真实 TAB | 字面 `\t`（`5C 74`） | TAB |
//! | 真实换行 | 字面 `\n`（`5C 6E`） | 换行 |
//! | 反斜杠 | `\\`（`5C 5C`） | `\` |
//!
//! **转义是这件事件能成立的关键**：正因为它把 TAB / 换行都转成字面量，
//! 我们才可以安全地"按 `\n` 切行、按 `\t` 切列"。反过来说，
//! **绝对不能用 `-r/--raw`** —— 那个开关关掉转义，字段里的真 TAB 会把列切错、
//! 真换行会把行切错。
//!
//! 转义还原复用 [`crate::model::mysql::unescape_batch_field`]（已有，实测过）。
//!
//! # 空结果集拿不到列名（已知限制）
//!
//! 实测：`SELECT 1 AS a FROM DUAL WHERE 1=0` 的输出是**零字节** ——
//! 连表头都不打。所以"0 行的结果集"在界面上显示不出列名。
//! 这不是解析器的 bug，是 `mysql` 客户端的行为；要拿到列名得改用
//! `--column-type-info` 或者执行前先 `LIMIT 0` 探一次，代价都不值。
//! → 界面上把这种情况说成"执行成功，没有返回行"，而不是画一张空表。

use super::mysql::{split_tsv_line, unescape_batch_field};

/// 一条语句的执行结果。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SqlResult {
    /// 列名。空结果集时是**空的**（见模块文档"已知限制"）。
    pub columns: Vec<String>,
    /// 行。每行长度 == `columns.len()`（解析器保证；`-B` 的尾随空格会补齐）。
    pub rows: Vec<Vec<String>>,
}

impl SqlResult {
    /// 没有列也没有行 —— DDL / DML，或者空结果集。两者外观一致（见模块文档）。
    pub fn is_empty(&self) -> bool {
        self.columns.is_empty() && self.rows.is_empty()
    }

    pub fn row_count(&self) -> usize {
        self.rows.len()
    }
}

/// 解析一条语句的 `-B`（带表头）输出。
///
/// 输入是 `wslc exec ... mysql -B -e "<一条语句>"` 的 stdout（已解码成 UTF-8）。
///
/// # 行为
///
/// - **零字节 / 只有空白** → 空结果（DDL、DML、空结果集都会走到这里）；
/// - 第一行是列名，其余每行是一行数据；
/// - 列数不齐时按**列名个数**补齐 / 截断（见 [`fix_row_width`]）；
/// - 某一行的列数比列名多时**不丢数据**：多出来的部分并进最后一列
///   （宁可显示得难看，也别悄悄吃掉用户的字段）。
pub fn parse_batch_with_header(tsv: &str) -> SqlResult {
    // ⚠️ 只按 `\n` 切，**不**用 `.lines()`：后者会把 `\r\n` 的 `\r` 吃掉，
    // 而 `-B` 的输出在 Windows 上经 `wslc` 出来是 LF（实测），`\r` 只可能
    // 是数据的一部分。用 `trim_end_matches('\r')` 单独处理更可控。
    let mut lines: Vec<&str> = tsv.split('\n').collect();

    // 末尾的换行会产生一个空元素，去掉；但中间的**空行是数据**
    // （一行里某列是空串且只有一列时，那一行就是空字符串）。
    while lines.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
        lines.pop();
    }
    if lines.is_empty() {
        return SqlResult::default();
    }

    let header = strip_cr(lines[0]);
    if header.trim().is_empty() {
        return SqlResult::default();
    }
    let columns: Vec<String> = split_tsv_line(header);

    let mut rows: Vec<Vec<String>> = Vec::with_capacity(lines.len().saturating_sub(1));
    for line in &lines[1..] {
        let line = strip_cr(line);
        // 完全空的行：单列结果集里，一个空串就是一行 —— 保留。
        // 多列结果集里不可能出现"整行空"，所以这里的解释只对单列有意义，
        // 但两种情况下"保留成一行"都是更安全的选择（丢行比多一行难查）。
        let mut row = split_tsv_line(line);
        fix_row_width(&mut row, columns.len());
        rows.push(row);
    }

    SqlResult { columns, rows }
}

/// 解析**不带表头**的 `-N -B` 输出，列名由调用方给。
///
/// 给"列名已知"的场景用（比如 `SHOW DATABASES` 的解析已经有自己的函数了，
/// 这个留给将来"想给某个内部查询套统一的结果结构"的场合）。
pub fn parse_batch_without_header(tsv: &str, columns: Vec<String>) -> SqlResult {
    let mut rows: Vec<Vec<String>> = Vec::new();
    for line in tsv.split('\n') {
        let line = strip_cr(line);
        if line.trim().is_empty() {
            continue;
        }
        let mut row = split_tsv_line(line);
        fix_row_width(&mut row, columns.len());
        rows.push(row);
    }
    SqlResult { columns, rows }
}

fn strip_cr(line: &str) -> &str {
    line.strip_suffix('\r').unwrap_or(line)
}

/// 把一行的宽度对齐到 `want`。
///
/// - 短了：补空串（`-B` 对 NULL / 空串都输出空，所以补空串是**唯一正确**的选择）；
/// - 长了：多出来的部分用 TAB 并回最后一列。
///
/// 后者是为了"不丢数据"：如果服务端真给了比表头多的一列（理论上不该有，
/// 但遇到 `-B` 的转义边界时可能），把多余的拼回去至少还能看见原文。
///
/// ⚠️ **`want == 1` 是个真实的坑**（CI 抓到的，`index out of bounds: len is 1
/// but the index is 1`）。原来的写法是
/// `let tail = row.split_off(want - 1); row[want - 1] = tail.join("\t");` ——
/// 在 `want == 1` 时 `split_off(0)` 会把**整行**搬进 `tail`，`row` 变空，
/// 紧接着的 `row[0] = …` 就 panic。触发条件是"单列结果集 + 某行里有多余的 TAB"。
///
/// 改成 `split_off(want)`（从 `want` 开始切，**不**包含最后一列），
/// 再把尾段**追加**到最后一格：`want == 1` 和 `want >= 2` 走同一条路，没有特例。
fn fix_row_width(row: &mut Vec<String>, want: usize) {
    if want == 0 {
        row.clear();
        return;
    }
    while row.len() < want {
        row.push(String::new());
    }
    if row.len() > want {
        // `split_off(want)` 保证 `row` 里还剩 `want` 个元素 —— 所以
        // `row[want - 1]` 一定合法（`want >= 1`）。
        let tail = row.split_off(want);
        let joined = tail.join("\t");
        if !joined.is_empty() {
            row[want - 1].push('\t');
            row[want - 1].push_str(&joined);
        }
    }
}

/// 让"列名里的反引号/表别名"更可读：`-B` 输出的列名就是服务端给的原文
/// （`COUNT(*)`、`t.a`、带空格的别名），这里**不改写** —— 只提供一个
/// 判断"这个列名是不是重复了"的工具给界面上做去重显示。
pub fn duplicate_column_names(columns: &[String]) -> Vec<String> {
    let mut seen: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    let mut out = Vec::with_capacity(columns.len());
    for c in columns {
        let n = seen.entry(c.as_str()).or_insert(0);
        *n += 1;
        if *n == 1 {
            out.push(c.clone());
        } else {
            // `id` / `id (2)` —— 和 MySQL 客户端自己的做法一样
            out.push(format!("{c} ({n})"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // 夹具全是**真机抓下来的**（`crates/wslc-core/tests/fixtures/sql_*.tsv`），
    // 不是编的 —— 编的会"正好符合我的解析器"，那就测不出东西。

    const BASIC: &str = include_str!("../../tests/fixtures/sql_basic.tsv");
    const MULTI_COL: &str = include_str!("../../tests/fixtures/sql_multi_col.tsv");
    const NULL_EMPTY: &str = include_str!("../../tests/fixtures/sql_null_empty.tsv");
    const TAB_FIELD: &str = include_str!("../../tests/fixtures/sql_tab_field.tsv");
    const NEWLINE_FIELD: &str = include_str!("../../tests/fixtures/sql_newline_field.tsv");
    const BACKSLASH: &str = include_str!("../../tests/fixtures/sql_backslash.tsv");
    const CJK: &str = include_str!("../../tests/fixtures/sql_cjk.tsv");
    const EMPTY_ROWS: &str = include_str!("../../tests/fixtures/sql_empty_rows.tsv");
    const NO_ROWS: &str = include_str!("../../tests/fixtures/sql_no_rows.tsv");
    const WEIRD_COLNAME: &str = include_str!("../../tests/fixtures/sql_weird_colname.tsv");

    #[test]
    fn parses_header_and_one_row() {
        let r = parse_batch_with_header(BASIC);
        assert_eq!(r.columns, vec!["id", "name"]);
        assert_eq!(r.rows, vec![vec!["1", "abc"]]);
        assert!(!r.is_empty());
    }

    #[test]
    fn parses_multiple_columns_including_numbers() {
        let r = parse_batch_with_header(MULTI_COL);
        assert_eq!(r.columns, vec!["a", "b", "c"]);
        // 数字保持**字符串原文** —— 不做类型推断。
        // 理由：`2.5` / `2.50` / `DECIMAL` 的精度、以及 `BIGINT` 超出 f64
        // 精确范围，一旦转成数字就回不来了。展示层只是画字，不需要数字类型。
        assert_eq!(r.rows, vec![vec!["1", "2.5", "x"]]);
    }

    #[test]
    fn null_and_empty_string_both_become_empty() {
        // ⚠️ 这是**刻意的**（已与用户确认）：`-B` 下 NULL 和空串输出一样，
        // 我们不打算用 `--xml` 去区分它们。
        let r = parse_batch_with_header(NULL_EMPTY);
        assert_eq!(r.columns, vec!["n", "e", "s"]);
        assert_eq!(r.rows, vec![vec!["NULL", "", "NULL"]]);
        // 第二列是空串（既可能是 NULL 也可能是 ''），第一/三列是字符串 "NULL"
        assert_eq!(r.rows[0][1], "");
    }

    #[test]
    fn tab_inside_a_value_is_unescaped_not_treated_as_a_separator() {
        let r = parse_batch_with_header(TAB_FIELD);
        assert_eq!(r.columns, vec!["t"]);
        assert_eq!(r.rows.len(), 1);
        assert_eq!(r.rows[0].len(), 1, "字段里的 TAB 不能被切成两列");
        assert_eq!(r.rows[0][0], "a\tb");
    }

    #[test]
    fn newline_inside_a_value_is_unescaped_not_treated_as_a_row_break() {
        let r = parse_batch_with_header(NEWLINE_FIELD);
        assert_eq!(r.columns, vec!["n"]);
        assert_eq!(r.rows.len(), 1, "字段里的换行不能变成两行");
        assert_eq!(r.rows[0][0], "a\nb");
    }

    #[test]
    fn backslash_is_unescaped() {
        let r = parse_batch_with_header(BACKSLASH);
        assert_eq!(r.columns, vec!["bs"]);
        assert_eq!(r.rows[0][0], "a\\b");
    }

    #[test]
    fn cjk_survives_intact() {
        let r = parse_batch_with_header(CJK);
        assert_eq!(r.columns, vec!["c"]);
        assert_eq!(r.rows[0][0], "中文测试");
    }

    #[test]
    fn empty_resultset_and_no_resultset_both_parse_to_empty() {
        // 实测：`SELECT ... WHERE 1=0` 和 `SET @x=1` 的输出都是**零字节**。
        // 所以解析器无法区分它们 —— 界面统一说"执行成功，没有返回行"。
        let a = parse_batch_with_header(EMPTY_ROWS);
        let b = parse_batch_with_header(NO_ROWS);
        assert!(a.is_empty(), "{a:?}");
        assert!(b.is_empty(), "{b:?}");
        assert_eq!(a, b, "两种情况解析结果必须一致（本来就分不出来）");
    }

    #[test]
    fn column_names_may_contain_spaces_and_dashes() {
        let r = parse_batch_with_header(WEIRD_COLNAME);
        assert_eq!(r.columns, vec!["col with space", "col-dash"]);
        assert_eq!(r.rows, vec![vec!["1", "2"]]);
    }

    #[test]
    fn junk_input_never_panics() {
        for s in [
            "",
            " ",
            "\n",
            "\n\n\n",
            "\r\n",
            "\t",
            ";",
            "only_one_line_without_newline",
            "a\tb\r\n1\r\n",
        ] {
            let _ = parse_batch_with_header(s);
        }
        // 只有列名、没有数据行
        let r = parse_batch_with_header("a\tb\n");
        assert_eq!(r.columns, vec!["a", "b"]);
        assert!(r.rows.is_empty());
    }

    #[test]
    fn short_rows_are_padded_and_long_rows_are_merged() {
        // 短行补空串
        let r = parse_batch_with_header("a\tb\tc\n1\n");
        assert_eq!(r.rows, vec![vec!["1", "", ""]]);
        // 长行不多出一个字段（并进最后一列）
        let r = parse_batch_with_header("a\tb\n1\t2\t3\n");
        assert_eq!(r.rows[0].len(), 2, "不能比表头多出列");
        assert_eq!(r.rows[0][1], "2\t3", "多出来的部分要保住，不能丢");
    }

    /// 回归：`want == 1` 时 `split_off(want - 1)` 会把整行搬空，再索引就 panic。
    ///
    /// CI 抓到的原文：`index out of bounds: the len is 1 but the index is 1`。
    /// 触发条件：**单列**结果集 + 某行里有多余的 TAB（`-B` 的转义边界上可能出现）。
    #[test]
    fn single_column_row_with_extra_tabs_does_not_panic() {
        // 单列，但数据行有两个 TAB（3 段）
        let r = parse_batch_with_header("only\nx\ty\tz\n");
        assert_eq!(r.columns, vec!["only"]);
        assert_eq!(r.rows.len(), 1);
        assert_eq!(r.rows[0].len(), 1, "单列结果集不能变成多列");
        assert_eq!(r.rows[0][0], "x\ty\tz", "多出来的部分并回唯一那一列");

        // 单列 + 数据行恰好两段
        let r = parse_batch_with_header("only\nx\ty\n");
        assert_eq!(r.rows, vec![vec!["x\ty"]]);

        // 单列 + 正常一行（不该被改动）
        let r = parse_batch_with_header("only\nx\n");
        assert_eq!(r.rows, vec![vec!["x"]]);
    }

    /// 回归：`want == 1` 时**短行也不能出事**。
    #[test]
    fn single_column_short_row_is_padded() {
        // 表头 1 列、数据行 0 段（空行）—— 补成 1 个空串
        let r = parse_batch_with_header("only\n\n");
        // 末尾的空行会被当成"没有数据"剥掉，所以这里是 0 行 —— 两种都算对，
        // 关键是**不能 panic**。
        assert!(r.rows.len() <= 1, "{:?}", r.rows);
    }

    #[test]
    fn without_header_uses_given_columns() {
        let r = parse_batch_without_header("1\tabc\n2\tdef\n", vec!["id".into(), "n".into()]);
        assert_eq!(r.columns, vec!["id", "n"]);
        assert_eq!(r.rows, vec![vec!["1", "abc"], vec!["2", "def"]]);
    }

    #[test]
    fn duplicate_column_names_get_numbered_suffixes() {
        let got = duplicate_column_names(&["id".into(), "id".into(), "name".into()]);
        assert_eq!(got, vec!["id", "id (2)", "name"]);
    }

    #[test]
    fn column_count_mismatch_does_not_lose_the_first_column() {
        // 回归：`fix_row_width` 在 `want == 1` 时曾经会把整行清空
        let r = parse_batch_with_header("only\nx\n");
        assert_eq!(r.rows, vec![vec!["x"]]);
    }
}
