//! SQL 控制台的**端到端解析链路**：一段脚本 → 切成语句 → 每条的真实输出 → 结构化结果。
//!
//! 为什么要一个单独的集成测试文件：这条链路由两个纯函数拼成
//! （`sqlstmt::split_statements` + `sqlresult::parse_batch_with_header`），
//! 各自有单测，但**拼起来**才是用户实际走的路径。而这一段错了的表现是
//! "界面上少几行 / 列错位 / 中文变问号"——`AGENTS.md` §8 里说的那类
//! CI 查不出来、只能靠人眼发现的问题。
//!
//! 夹具全是**真机抓下来的**（本机 wslc 3.0.3.0 / mysql 26.7.0，2026-10），
//! 不是编的 —— 编的会"正好符合我的解析器"，那就测不出东西。
//!
//! 这些测试**不需要容器**（夹具是文件），所以 `core` job 里十几秒就跑完。

use wslc_core::model::sqlresult::{duplicate_column_names, parse_batch_with_header};
use wslc_core::model::sqlstmt::{returns_rows, split_statements};

// ---------------------------------------------------------------------------
// 1. 单条语句：真实输出 → 结构化结果
// ---------------------------------------------------------------------------

#[test]
fn a_simple_select_becomes_a_header_and_one_row() {
    let r = parse_batch_with_header(include_str!("fixtures/sql_basic.tsv"));
    assert_eq!(r.columns, ["id", "name"]);
    assert_eq!(r.rows, [["1", "abc"]]);
}

#[test]
fn numeric_columns_stay_as_text() {
    // 不做类型推断：`2.50` 的尾零、DECIMAL 精度、超出 f64 的 BIGINT 都会丢。
    let r = parse_batch_with_header(include_str!("fixtures/sql_multi_col.tsv"));
    assert_eq!(r.columns, ["a", "b", "c"]);
    assert_eq!(r.rows, [["1", "2.5", "x"]]);
}

#[test]
fn empty_resultset_and_statement_without_resultset_are_indistinguishable() {
    // 实测：`SELECT ... WHERE 1=0` 和 `SET @x=1` 的输出都是**零字节**。
    // 界面上统一说"执行成功，没有返回行"—— 不要假装能区分。
    let empty_rows = parse_batch_with_header(include_str!("fixtures/sql_empty_rows.tsv"));
    let no_resultset = parse_batch_with_header(include_str!("fixtures/sql_no_rows.tsv"));
    assert_eq!(empty_rows, no_resultset);
    assert!(empty_rows.is_empty());
}

// ---------------------------------------------------------------------------
// 2. `-B` 的转义：这是"能按行按列切"的全部前提
// ---------------------------------------------------------------------------

#[test]
fn a_tab_inside_a_value_does_not_become_a_column_boundary() {
    let r = parse_batch_with_header(include_str!("fixtures/sql_tab_field.tsv"));
    assert_eq!(r.columns.len(), 1);
    assert_eq!(r.rows[0].len(), 1, "字段里的 TAB 被当成了列分隔符");
    assert_eq!(r.rows[0][0], "a\tb");
}

#[test]
fn a_newline_inside_a_value_does_not_become_a_row_boundary() {
    let r = parse_batch_with_header(include_str!("fixtures/sql_newline_field.tsv"));
    assert_eq!(r.rows.len(), 1, "字段里的换行被当成了行分隔符");
    assert_eq!(r.rows[0][0], "a\nb");
}

#[test]
fn backslashes_are_unescaped() {
    let r = parse_batch_with_header(include_str!("fixtures/sql_backslash.tsv"));
    assert_eq!(r.rows[0][0], "a\\b");
}

#[test]
fn cjk_text_survives_the_round_trip() {
    let r = parse_batch_with_header(include_str!("fixtures/sql_cjk.tsv"));
    assert_eq!(r.rows[0][0], "中文测试");
}

// ---------------------------------------------------------------------------
// 3. 多语句：切分 + 逐条解析（用户实际走的路径）
// ---------------------------------------------------------------------------

/// 用真实的单条输出"拼"一次多语句执行 —— 这正是 `run_statements` 的结果形状。
#[test]
fn a_three_statement_script_yields_three_results_in_order() {
    let script = "SELECT 1 AS id, 'abc' AS name;\n\
                  SELECT 1 AS a, 2.5 AS b, 'x' AS c;\n\
                  SET @x = 1";
    let stmts = split_statements(script);
    assert_eq!(stmts.len(), 3, "{stmts:?}");

    // 前两条有结果集，第三条没有（零字节）
    let fixtures = [
        include_str!("fixtures/sql_basic.tsv"),
        include_str!("fixtures/sql_multi_col.tsv"),
        include_str!("fixtures/sql_no_rows.tsv"),
    ];
    let results: Vec<_> = fixtures
        .iter()
        .map(|f| parse_batch_with_header(f))
        .collect();

    assert_eq!(results[0].columns, ["id", "name"]);
    assert_eq!(results[1].columns, ["a", "b", "c"]);
    assert!(results[2].is_empty());

    // 关键：三个结果**各自独立**，没有粘连
    assert_eq!(results[0].rows.len(), 1);
    assert_eq!(results[1].rows.len(), 1);
    assert_eq!(results[2].rows.len(), 0);
}

#[test]
fn the_documented_split_quirks_hold_together_with_parsing() {
    // 引号里的分号不切，而且切出来的那条能被正常解析
    let stmts = split_statements("SELECT 'a;b' AS q");
    assert_eq!(stmts.len(), 1);
    let r = parse_batch_with_header(include_str!("fixtures/sql_quote_semicolon.tsv"));
    assert_eq!(r.columns, ["q"]);
    assert_eq!(r.rows[0][0], "a;b");
}

// ---------------------------------------------------------------------------
// 4. 界面要用的两个判断
// ---------------------------------------------------------------------------

#[test]
fn returns_rows_tells_the_ui_which_wording_to_use() {
    // 有结果集 → "N 行"
    assert!(returns_rows("SELECT 1"));
    assert!(returns_rows("SHOW TABLES"));
    // 没结果集 → "执行成功"
    assert!(!returns_rows("UPDATE t SET a = 1"));
    assert!(!returns_rows("CREATE TABLE t (a INT)"));
}

#[test]
fn duplicate_column_names_are_disambiguated_for_the_footer() {
    // `SELECT a.id, b.id` 的 `-B` 表头是两列都叫 `id`
    let r = parse_batch_with_header("id\tid\tname\n1\t2\tx\n");
    assert_eq!(r.columns, ["id", "id", "name"]);
    let shown = duplicate_column_names(&r.columns);
    assert_eq!(shown, ["id", "id (2)", "name"]);
}

// ---------------------------------------------------------------------------
// 5. 不 panic：任何垃圾输入
// ---------------------------------------------------------------------------

#[test]
fn neither_half_panics_on_arbitrary_input() {
    let samples = [
        "",
        " ",
        "\n",
        ";",
        ";;;",
        "-- only a comment",
        "/* only a comment */",
        "SELECT 'unclosed",
        "SELECT `unclosed",
        "SELECT /* unclosed",
        "\t\t\t",
        "a\tb\n1\n",
        "a\tb\n1\t2\t3\n",
        "中文；全角分号",
        "SELECT 1\r\nSELECT 2\r\n",
    ];
    for s in samples {
        let _ = parse_batch_with_header(s);
        let _ = split_statements(s);
        let _ = returns_rows(s);
    }
}

#[test]
fn crlf_script_is_split_and_parsed() {
    // Windows 的文本编辑器存出来是 CRLF。切分要按 `\r\n` 也能工作，
    // 解析要能吃掉行尾的 `\r`。
    let stmts = split_statements("SELECT 1;\r\nSELECT 2;\r\n");
    assert_eq!(stmts.len(), 2);
    assert_eq!(stmts[0], "SELECT 1");
    assert_eq!(stmts[1], "SELECT 2");

    let r = parse_batch_with_header("a\tb\r\n1\t2\r\n");
    assert_eq!(r.columns, ["a", "b"]);
    assert_eq!(r.rows, [["1", "2"]]);
}
