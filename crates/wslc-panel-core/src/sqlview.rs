//! SQL 结果表格的**显示逻辑**（纯函数，不碰 GPUI，所以能单测）。
//!
//! 渲染在 `crates/wslc-panel/src/sql_table.rs`，但"该显示什么"在这里决定：
//! 列宽怎么估、单元格怎么措辞、页脚那一行怎么写。
//!
//! 这么拆的理由和 `columns.rs` 一样：这些规则错了的表现是"列挤在一起"
//! 或"页脚写着 0 行其实有 3 行"，**CI 里除了单测没有任何信号**
//! （`AGENTS.md` §8：渲染不在任何测试目标里）。

use wslc_core::model::sqlresult::SqlResult;

/// 一个单元格**画成什么**。渲染层只负责按这个结果上色和排版。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CellLook {
    /// 空（NULL 或空串 —— `-B` 分不出来，我们也不打算分）。
    Empty,
    /// 普通文本。
    Text(String),
    /// 数字（纯 ASCII 数字 / 正负号 / 小数点）。**右对齐**。
    Number(String),
}

/// 判断一个值看起来是不是数字（决定要不要右对齐）。
///
/// # 刻意不做的两件事
///
/// 1. **不转成 `f64` / `i128`**。`DECIMAL(65,30)` 和超过 2^63 的 `BIGINT`
///    转过去就丢精度，而表格只是画字，不需要数值语义；
/// 2. **不认 `1e5` 之外的科学计数法**……实际上连它也不认：MySQL 的 `-B`
///    对 `DOUBLE` 输出的是十进制展开（`100000`），`1e5` 只可能来自字符串列，
///    那时候右对齐反而是错的。
///
/// 空串不算数字（它是 [`CellLook::Empty`]）。
pub fn looks_numeric(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let mut seen_digit = false;
    let mut seen_dot = false;
    for (ix, c) in s.chars().enumerate() {
        match c {
            '0'..='9' => seen_digit = true,
            // 符号只允许在开头
            '-' | '+' if ix == 0 => {}
            '.' if !seen_dot => seen_dot = true,
            _ => return false,
        }
    }
    seen_digit
}

/// 一个单元格的显示形态。
pub fn cell_look(value: &str) -> CellLook {
    if value.is_empty() {
        CellLook::Empty
    } else if looks_numeric(value) {
        CellLook::Number(value.to_owned())
    } else {
        CellLook::Text(value.to_owned())
    }
}

/// 估一列的宽度（逻辑像素）。
///
/// # 为什么是"估"而不是"量"
///
/// 真要量得拿到 GPUI 的 `Window::text_system()`，那就只能放在渲染层、
/// 也就测不了。而列宽**不需要精确**：`DataTable` 的列可以拖动、
/// 双击列边界会调 `TableDelegate::fit_column` 自适应（那个是真量）。
/// 这里的估算只决定"第一眼看起来合不合理"。
///
/// # 公式
///
/// - 取该列**前 [`WIDTH_SAMPLE_ROWS`] 行**里最长的值，和列名比，取较大者；
/// - 中日韩字符按 **2 个宽度**算（等宽字体里它们就是占两格）；
/// - 加上左右 padding，clamp 到 `[MIN_COL_WIDTH, MAX_COL_WIDTH]`。
///
/// 只采样前若干行：一个 10 万行的结果集里逐格量长度会白花时间，
/// 而第 5000 行的超长文本本来也该靠 tooltip 看，不该把列撑到最大。
pub fn estimate_column_width(name: &str, values: impl Iterator<Item = String>) -> f32 {
    let mut widest = display_width(name);
    for (ix, v) in values.enumerate() {
        if ix >= WIDTH_SAMPLE_ROWS {
            break;
        }
        let w = display_width(&v);
        if w > widest {
            widest = w;
        }
    }
    // 一"格"按 7.2px 算（13px 字号下 Consolas 的数字宽约 7.2），
    // 再加 24px 的左右 padding。
    let px = widest as f32 * 7.2 + 24.0;
    px.clamp(MIN_COL_WIDTH, MAX_COL_WIDTH)
}

/// 采样多少行来估宽度。
pub const WIDTH_SAMPLE_ROWS: usize = 100;
/// 列宽下限（再窄就看不出是什么了）。
pub const MIN_COL_WIDTH: f32 = 72.0;
/// 列宽上限（再宽就把整张表撑出屏幕；长文本靠 tooltip / 单元格详情看）。
pub const MAX_COL_WIDTH: f32 = 360.0;

/// 显示宽度：中日韩 / 全角字符算 2，其余算 1。
///
/// 这不是完整的 Unicode East Asian Width 实现 —— 只覆盖**实际会遇到**的范围
/// （CJK 统一表意文字、假名、谚文、全角标点）。做了完整版的收益是
/// "emoji 的宽度更准"，代价是一张几百行的区间表，不值。
pub fn display_width(s: &str) -> usize {
    s.chars()
        .map(|c| {
            let cp = c as u32;
            let wide = matches!(cp,
                0x1100..=0x115F     // 谚文字母
                | 0x2E80..=0xA4CF   // CJK 部首 … 彝文（含中日韩标点）
                | 0xAC00..=0xD7A3   // 谚文音节
                | 0xF900..=0xFAFF   // CJK 兼容表意文字
                | 0xFE30..=0xFE6F   // CJK 兼容形式
                | 0xFF00..=0xFF60   // 全角形式
                | 0xFFE0..=0xFFE6   // 全角符号
                | 0x1F300..=0x1F64F // emoji（大致范围，够用）
                | 0x1F900..=0x1F9FF
                | 0x20000..=0x3FFFD // CJK 扩展 B 及以后
            );
            if wide { 2 } else { 1 }
        })
        .sum()
}

/// 页脚那一行："1–200 / 350 行 · 3 列 · 0.08s"。
///
/// # 为什么要写"没有返回行"而不是"0 行"
///
/// `-B` 对**空结果集**和**没有结果集的语句**（`SET` / `DDL`）输出都是零字节，
/// 解析器分不出来（见 `wslc_core::model::sqlresult` 的模块文档）。所以文案
/// 必须**两种情况都说得通**：`SELECT ... WHERE 1=0` 和 `UPDATE` 都显示
/// "执行成功，没有返回行"。
pub fn footer_text(result: &SqlResult, elapsed_ms: u64) -> String {
    if result.is_empty() {
        return "执行成功，没有返回行".to_owned();
    }
    let rows = result.row_count();
    if rows == 0 {
        // 有列名但没行：这是"查到了、确实是空的"，和上面那种不同 ——
        // 这时能说出列数，信息量更大。
        return format!(
            "0 行 · {} 列 · {}",
            result.columns.len(),
            format_elapsed(elapsed_ms)
        );
    }
    format!(
        "{rows} 行 · {} 列 · {}",
        result.columns.len(),
        format_elapsed(elapsed_ms)
    )
}

/// 耗时文案。小于 1 秒给毫秒，否则给秒（一位小数）。
pub fn format_elapsed(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else {
        format!("{:.1} s", ms as f64 / 1000.0)
    }
}

/// 多语句时，结果区上方那几个 tab 的标题。
///
/// 用"结果 1 / 结果 2 / …"而不是语句原文：语句可能很长（几十行），
/// 塞进 tab 里既放不下也没人读。语句原文在编辑区里就有。
pub fn result_tab_label(index: usize, result: &SqlResult) -> String {
    if result.is_empty() {
        format!("结果 {}（无输出）", index + 1)
    } else {
        format!("结果 {}（{} 行）", index + 1, result.row_count())
    }
}

/// 这次执行里**有没有**任何一条产生了表格。
///
/// 全是 DDL / DML 时，界面上不该画一个空表格，而该说"执行成功"。
pub fn any_has_grid(results: &[SqlResult]) -> bool {
    results.iter().any(|r| !r.columns.is_empty())
}

/// 把结果导成 CSV（给「导出」按钮用）。
///
/// # 转义规矩（RFC 4180）
///
/// 含 `,` `"` 换行 的字段用 `"` 包起来，内部的 `"` 写成 `""`。
/// 这是我们**唯一**会写出去的文件格式，所以规矩写死在这里、配单测。
///
/// # 行序 = 屏幕上的顺序
///
/// 表格允许点列头排序（`SqlResultDelegate::perform_sort`），而导出的
/// 顺序必须和用户看到的一致 —— 否则"点了排序再导出"会得到一份看起来
/// 没排过的文件。
///
/// 排序**不重排 `SqlResult.rows`**（原数据要原样留着），只另存一份下标表。
/// 所以这里收一个 `order`：`None` = 原始顺序；`Some(&[..])` = 那一串下标。
///
/// ⚠️ **下标越界的行会被跳过**，不会 panic —— `order` 来自界面状态，
/// 和 `result` 之间只靠"同一个结果集"这个约定对齐（切 tab 会整个换掉
/// `sql_table`，但 `to_csv` 不该依赖那个时序）。
pub fn to_csv(result: &SqlResult, order: Option<&[usize]>) -> String {
    let mut out = String::new();
    out.push_str(
        &result
            .columns
            .iter()
            .map(|c| csv_field(c))
            .collect::<Vec<_>>()
            .join(","),
    );
    out.push_str("\r\n");

    // 每一行都走同一个出口，免得"有序"和"无序"两支各写一遍循环。
    let mut push_row = |row: &Vec<String>| {
        out.push_str(
            &row.iter()
                .map(|c| csv_field(c))
                .collect::<Vec<_>>()
                .join(","),
        );
        out.push_str("\r\n");
    };

    match order {
        None => result.rows.iter().for_each(&mut push_row),
        Some(order) => order
            .iter()
            .filter_map(|&ix| result.rows.get(ix))
            .for_each(&mut push_row),
    }
    out
}

/// CSV 里一个字段的转义。
fn csv_field(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_owned()
    }
}

/// 排序方向（和 gpui-kit 的 `ColumnSort` 一一对应，但不能直接用它 ——
/// 那个类型在 `gpui-kit` 里，而 `wslc-panel-core` 不依赖 GPUI）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDir {
    Ascending,
    Descending,
}

/// 算出"按第 `col` 列排序时，行的显示顺序"。
///
/// 返回的是**原始行的下标序列**，而不是排好的数据 —— 这样原数据永远保持
/// 服务端给的顺序（导出 CSV 时能拿到原文），而显示顺序单独维护。
///
/// # 比较规则（每条都有代价）
///
/// 1. **数字列按下数值比**，其余按字符串比。否则 `10` 会排在 `9` 前面 ——
///    这是数据库客户端最容易被投诉的一类细节。
/// 2. **空值（NULL / 空串）永远排最后**，不管升序降序。数据库客户端的惯例；
///    而且它让"有值的行"聚在一起，比混在中间好读。
/// 3. **相等时按下标定序**。`sort_by` 不保证稳定（虽然现在实现是稳定的），
///    依赖它会让"每次点列头顺序都可能变"，很难查。
pub fn sort_order(rows: &[Vec<String>], col: usize, dir: SortDir) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..rows.len()).collect();

    // 这一列算不算"数字列"：看**第一个非空值**。只看一个是为了快 ——
    // 实际结果集的列类型是统一的（MySQL 不会一列里混着数字和文字）。
    let numeric = rows
        .iter()
        .filter_map(|r| r.get(col))
        .find(|v| !v.is_empty())
        .map(|v| looks_numeric(v))
        .unwrap_or(false);

    idx.sort_by(|&a, &b| {
        let va = rows[a].get(col).map(String::as_str).unwrap_or("");
        let vb = rows[b].get(col).map(String::as_str).unwrap_or("");

        let empty_a = va.is_empty();
        let empty_b = vb.is_empty();
        if empty_a || empty_b {
            return match (empty_a, empty_b) {
                (true, true) => a.cmp(&b),
                (true, false) => std::cmp::Ordering::Greater,
                (false, true) => std::cmp::Ordering::Less,
                // 上面已经排除了，走不到
                _ => std::cmp::Ordering::Equal,
            };
        }

        let ord = if numeric {
            // 用 `f64` 比 —— 排序只需要相对大小，精度够；展示仍然用原文。
            let fa: f64 = va.parse().unwrap_or(0.0);
            let fb: f64 = vb.parse().unwrap_or(0.0);
            fa.partial_cmp(&fb).unwrap_or(std::cmp::Ordering::Equal)
        } else {
            va.cmp(vb)
        };
        let ord = match dir {
            SortDir::Descending => ord.reverse(),
            SortDir::Ascending => ord,
        };
        if ord == std::cmp::Ordering::Equal {
            a.cmp(&b)
        } else {
            ord
        }
    });
    idx
}

#[cfg(test)]
mod tests {
    use super::*;

    fn res(cols: &[&str], rows: Vec<Vec<&str>>) -> SqlResult {
        SqlResult {
            columns: cols.iter().map(|s| (*s).to_owned()).collect(),
            rows: rows
                .into_iter()
                .map(|r| r.into_iter().map(|s| s.to_owned()).collect())
                .collect(),
        }
    }

    #[test]
    fn numeric_detection() {
        assert!(looks_numeric("0"));
        assert!(looks_numeric("123"));
        assert!(looks_numeric("-1"));
        assert!(looks_numeric("+1"));
        assert!(looks_numeric("2.5"));
        assert!(looks_numeric("-0.001"));
        // 不是数字
        assert!(!looks_numeric(""));
        assert!(!looks_numeric("abc"));
        assert!(!looks_numeric("1a"));
        assert!(!looks_numeric("1.2.3"));
        assert!(!looks_numeric("--1"));
        assert!(!looks_numeric("1 "));
        assert!(!looks_numeric("1e5"), "科学计数法只可能来自字符串列");
        assert!(!looks_numeric("NULL"), "字符串 NULL 不是数字");
    }

    #[test]
    fn cell_look_classifies_three_ways() {
        assert_eq!(cell_look(""), CellLook::Empty);
        assert_eq!(cell_look("42"), CellLook::Number("42".into()));
        assert_eq!(cell_look("abc"), CellLook::Text("abc".into()));
        // NULL 和空串都是空 —— 刻意的（见 sqlresult 的模块文档）
        assert_eq!(cell_look(""), cell_look(""));
    }

    #[test]
    fn column_width_grows_with_content_but_is_clamped() {
        // 名字短、值短 → 取下限附近
        let w = estimate_column_width("a", ["1".to_owned()].into_iter());
        assert!(w >= MIN_COL_WIDTH, "{w}");

        // 长文本 → 撞上限
        let long = "x".repeat(500);
        let w = estimate_column_width("name", [long].into_iter());
        assert_eq!(w, MAX_COL_WIDTH, "超长内容必须被 MAX_COL_WIDTH 截住");

        // 一般的值 → 落在中间
        let mid = "abcdefghij".to_owned(); // 10 格 ≈ 72 + 24 = 96
        let w = estimate_column_width("c", [mid].into_iter());
        assert!(w > MIN_COL_WIDTH && w < MAX_COL_WIDTH, "{w}");
    }

    #[test]
    fn column_width_uses_the_header_when_it_is_longer() {
        let w_short = estimate_column_width("c", ["1".to_owned()].into_iter());
        let w_long = estimate_column_width("a_very_long_column_name", ["1".to_owned()].into_iter());
        assert!(w_long > w_short, "列名比值长时应该按列名算");
    }

    #[test]
    fn column_width_only_samples_the_first_rows() {
        // 第 0 行很短、第 5000 行超长 —— 不该被第 5000 行撑爆
        let mut values: Vec<String> = (0..5000).map(|_| "1".to_owned()).collect();
        values.push("x".repeat(500));
        let w = estimate_column_width("c", values.into_iter());
        assert!(
            w < MAX_COL_WIDTH,
            "只采样前 {WIDTH_SAMPLE_ROWS} 行，实际 {w}"
        );
    }

    #[test]
    fn cjk_counts_as_two_cells() {
        assert_eq!(display_width("abc"), 3);
        assert_eq!(display_width("中文"), 4);
        assert_eq!(display_width("中文abc"), 7);
        assert_eq!(display_width(""), 0);
    }

    #[test]
    fn footer_says_something_true_in_all_three_shapes() {
        // 1) 空结果（空结果集 / 无结果集，分不出来）
        assert_eq!(
            footer_text(&SqlResult::default(), 12),
            "执行成功，没有返回行"
        );
        // 2) 有列名、0 行
        let r = res(&["a", "b"], vec![]);
        assert!(
            footer_text(&r, 12).starts_with("0 行 · 2 列"),
            "{}",
            footer_text(&r, 12)
        );
        // 3) 正常
        let r = res(&["a"], vec![vec!["1"], vec!["2"]]);
        assert!(
            footer_text(&r, 12).starts_with("2 行 · 1 列"),
            "{}",
            footer_text(&r, 12)
        );
    }

    #[test]
    fn elapsed_formatting() {
        assert_eq!(format_elapsed(0), "0 ms");
        assert_eq!(format_elapsed(999), "999 ms");
        assert_eq!(format_elapsed(1000), "1.0 s");
        assert_eq!(format_elapsed(1234), "1.2 s");
    }

    #[test]
    fn tab_labels_mention_row_counts() {
        assert_eq!(
            result_tab_label(0, &res(&["a"], vec![vec!["1"]])),
            "结果 1（1 行）"
        );
        assert_eq!(
            result_tab_label(1, &SqlResult::default()),
            "结果 2（无输出）"
        );
    }

    #[test]
    fn any_has_grid_distinguishes_ddl_from_select() {
        assert!(!any_has_grid(&[SqlResult::default()]));
        assert!(any_has_grid(&[res(&["a"], vec![])]));
        assert!(any_has_grid(&[SqlResult::default(), res(&["a"], vec![])]));
    }

    #[test]
    fn csv_escapes_per_rfc4180() {
        let r = res(
            &["id", "name"],
            vec![
                vec!["1", "a,b"],
                vec!["2", "he said \"hi\""],
                vec!["3", "line1\nline2"],
            ],
        );
        let csv = to_csv(&r, None);
        let lines: Vec<&str> = csv.split("\r\n").collect();
        assert_eq!(lines[0], "id,name");
        assert_eq!(lines[1], "1,\"a,b\"");
        assert_eq!(lines[2], "2,\"he said \"\"hi\"\"\"");
        assert_eq!(lines[3], "3,\"line1\nline2\"");
    }

    #[test]
    fn csv_of_empty_result_is_just_the_header() {
        assert_eq!(to_csv(&SqlResult::default(), None), "\r\n");
        let r = res(&["a", "b"], vec![]);
        assert_eq!(to_csv(&r, None), "a,b\r\n");
    }

    // ---- 导出的行序 = 屏幕上的顺序（`order` 参数）----

    #[test]
    fn csv_follows_the_given_row_order() {
        // 屏幕上的顺序来自 delegate 的排序下标表；导出必须按它写，
        // 否则"点了排序再导出"会得到一份看起来没排过的文件。
        let r = res(&["n"], vec![vec!["10"], vec!["9"], vec!["100"]]);
        let sorted = sort_order(&r.rows, 0, SortDir::Ascending);
        assert_eq!(sorted, vec![1, 0, 2], "前提：数值序是 9,10,100");

        assert_eq!(
            to_csv(&r, Some(&sorted)),
            "n\r\n9\r\n10\r\n100\r\n",
            "导出没有按屏幕顺序写"
        );
        // 不传 order 时仍是原始顺序（老行为不变）
        assert_eq!(to_csv(&r, None), "n\r\n10\r\n9\r\n100\r\n");
    }

    #[test]
    fn csv_skips_out_of_range_indices_instead_of_panicking() {
        // `order` 来自界面状态，和 `result` 之间只靠"同一个结果集"这个约定
        // 对齐。对不上时宁可少写几行，也不能 panic —— 那会把整个面板拖掉。
        let r = res(&["n"], vec![vec!["a"], vec!["b"]]);
        assert_eq!(to_csv(&r, Some(&[1, 99, 0])), "n\r\nb\r\na\r\n");
        // 全越界 → 只剩表头
        assert_eq!(to_csv(&r, Some(&[42])), "n\r\n");
    }

    // ---- 排序（v0.11.1 从 `sql_table.rs` 搬来这里，为了能被 CI 测到）----

    fn rows(data: &[&[&str]]) -> Vec<Vec<String>> {
        data.iter()
            .map(|r| r.iter().map(|s| (*s).to_owned()).collect())
            .collect()
    }

    #[test]
    fn numeric_column_sorts_by_value_not_lexicographically() {
        // 字典序会把 "10" 排在 "9" 前面 —— 这是最容易被投诉的一类细节
        let r = rows(&[&["10"], &["9"], &["100"]]);
        let got: Vec<&str> = sort_order(&r, 0, SortDir::Ascending)
            .into_iter()
            .map(|i| r[i][0].as_str())
            .collect();
        assert_eq!(got, ["9", "10", "100"]);
    }

    #[test]
    fn text_column_sorts_lexicographically() {
        let r = rows(&[&["b"], &["a"], &["c"]]);
        let got: Vec<&str> = sort_order(&r, 0, SortDir::Ascending)
            .into_iter()
            .map(|i| r[i][0].as_str())
            .collect();
        assert_eq!(got, ["a", "b", "c"]);
    }

    #[test]
    fn descending_is_the_exact_reverse() {
        let r = rows(&[&["a"], &["c"], &["b"]]);
        let got: Vec<&str> = sort_order(&r, 0, SortDir::Descending)
            .into_iter()
            .map(|i| r[i][0].as_str())
            .collect();
        assert_eq!(got, ["c", "b", "a"]);
    }

    #[test]
    fn empty_values_go_last_in_both_directions() {
        let r = rows(&[&["b"], &[""], &["a"]]);
        for dir in [SortDir::Ascending, SortDir::Descending] {
            let order = sort_order(&r, 0, dir);
            assert_eq!(
                r[*order.last().unwrap()][0],
                "",
                "空值在 {dir:?} 下都应该排最后"
            );
        }
    }

    #[test]
    fn equal_values_keep_a_deterministic_order() {
        // 三个相等的值：显示顺序必须等于原始下标顺序，否则每次点列头都会跳
        let r = rows(&[&["x", "1"], &["x", "2"], &["x", "3"]]);
        let order = sort_order(&r, 0, SortDir::Ascending);
        assert_eq!(order, vec![0, 1, 2]);
    }

    #[test]
    fn sorting_returns_indices_so_the_original_rows_stay_untouched() {
        let r = rows(&[&["2"], &["1"]]);
        let order = sort_order(&r, 0, SortDir::Ascending);
        assert_eq!(order, vec![1, 0]);
        // 原数据顺序不变
        assert_eq!(r[0][0], "2");
        assert_eq!(r[1][0], "1");
    }

    #[test]
    fn sorting_short_rows_and_empty_input_do_not_panic() {
        // 某些行缺这一列（不该发生，但解析器允许补齐失败）
        let r = rows(&[&["b"], &[]]);
        let _ = sort_order(&r, 0, SortDir::Ascending);
        // 空输入
        assert!(sort_order(&[], 0, SortDir::Ascending).is_empty());
        // 列下标越界
        let r = rows(&[&["a"]]);
        assert_eq!(sort_order(&r, 99, SortDir::Ascending), vec![0]);
    }
}
