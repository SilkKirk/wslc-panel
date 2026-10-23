//! SQL 结果表格：把 [`SqlResult`] 接上 gpui-kit 的 `DataTable`。
//!
//! # 为什么用 `DataTable` 而不是自己画
//!
//! gpui-kit 0.7.1 的 `crates/component/src/table/` 已经有一个完整的表格组件，
//! 文档里列的能力正好是我们要的：
//!
//! - 虚拟滚动（大结果集只渲染可见行）
//! - 列可拖宽 / 可拖动换序 / 可固定在左侧
//! - 点列头排序（`TableDelegate::perform_sort`）
//! - 单元格级选择 + 键盘导航
//! - 右键菜单（`context_menu`）
//! - `cell_text` —— 导 CSV 时直接拿每格文本
//!
//! 这些自己写要几千行，而且虚拟滚动错了只表现为"滚动时白屏"，CI 查不出来。
//!
//! # 数据从哪来
//!
//! 见 `wslc_core::model::sqlresult`：`mysql -B`（带表头）的 TSV 解析成
//! `columns: Vec<String>` + `rows: Vec<Vec<String>>`。**每个值都是 `String`**
//! —— NULL 和空串都是空串（刻意的，见那个模块的文档）。
//!
//! # 展示规则在 `wslc-panel-core::sqlview`
//!
//! 列宽估算、数字右对齐、页脚文案、CSV 导出**都不在这里**决定 ——
//! 它们在 `sqlview` 里是纯函数，`core` job 能测到。这个文件只负责
//! "把 `sqlview` 算出来的东西画到 `DataTable` 上"。

use std::sync::Arc;

// `use gpui_kit::*` 而不是逐项导入 `gpui::…`：
// `.italic()` / `.text_right()` / `.w_full()` 这些样式方法来自 gpui 的
// `Styled` 家族 trait，而 `views.rs` 那 9000 行证明**通过 `gpui_kit::*`
// 引入是靠得住的**（它自己就是 `pub use ::gpui::*`）。
// 逐项导入容易漏掉某个 trait，报一堆"no method named ..."。
use gpui_kit::*;
use gpui_kit::component::table::{Column, ColumnSort, TableDelegate};
use wslc_core::model::sqlresult::SqlResult;
use wslc_panel_core::sqlview::{self, CellLook};

/// 结果表格的 delegate。
///
/// `rows` 用 `Arc` 包着：切换结果 tab / 重绘时只克隆一个指针，
/// 而不用把几万行整个复制一遍。
pub struct SqlResultDelegate {
    /// 列定义（宽度已经按 `sqlview::estimate_column_width` 算好）。
    columns: Vec<Column>,
    /// 行数据。和 `columns` 一样是 `SqlResult` 里的原样。
    rows: Arc<Vec<Vec<String>>>,
    /// 当前排序状态（只影响显示，**不重新查库**）。
    sort: Option<(usize, ColumnSort)>,
    /// 排序后的行下标。`None` = 原始顺序。
    ///
    /// 存下标而不是重排 `rows` 本身：原数据要原样留着导出 CSV
    /// （导出的顺序该跟屏幕上一致 —— 所以导出时也要读这个）。
    order: Option<Vec<usize>>,
}

impl SqlResultDelegate {
    pub fn new(result: SqlResult) -> Self {
        let columns = result
            .columns
            .iter()
            .enumerate()
            .map(|(ix, name)| {
                // 宽度按"这一列前 100 行里最长的值"估（`sqlview` 里有单测）。
                let values = result.rows.iter().map(|r| {
                    r.get(ix).cloned().unwrap_or_default()
                });
                let width = sqlview::estimate_column_width(name, values);
                Column::new(SharedString::from(format!("c{ix}")), name.clone())
                    .width(px(width))
                    .min_width(px(sqlview::MIN_COL_WIDTH))
                    .max_width(px(sqlview::MAX_COL_WIDTH))
                    // 排序在本地做（`perform_sort`），不重查库。
                    .sortable()
            })
            .collect();
        Self {
            columns,
            rows: Arc::new(result.rows),
            sort: None,
            order: None,
        }
    }

    /// 当前显示顺序下的第 `row_ix` 行在原始数据里的下标。
    fn source_row(&self, row_ix: usize) -> usize {
        self.order
            .as_ref()
            .and_then(|o| o.get(row_ix).copied())
            .unwrap_or(row_ix)
    }

    /// 按当前 `sort` 重算 `order`。
    ///
    /// # 比较规则
    ///
    /// 数字列按**数值**比（否则 `10` 会排在 `9` 前面），其余按字符串比。
    /// 空值（NULL / 空串）**永远排最后** —— 这是数据库客户端的惯例，
    /// 而且它让"有值的行"聚在一起，比混在中间好读。
    fn recompute_order(&mut self) {
        let Some((col, dir)) = self.sort else {
            self.order = None;
            return;
        };
        let mut idx: Vec<usize> = (0..self.rows.len()).collect();
        let numeric = self.columns.get(col).map(|_| {
            // 一列算不算"数字列"：看第一个非空值像不像数字。
            // 只看一个是为了快，而实际结果集的列类型是统一的。
            self.rows
                .iter()
                .filter_map(|r| r.get(col))
                .find(|v| !v.is_empty())
                .map(|v| sqlview::looks_numeric(v))
                .unwrap_or(false)
        });

        idx.sort_by(|&a, &b| {
            let va = self.rows[a].get(col).map(String::as_str).unwrap_or("");
            let vb = self.rows[b].get(col).map(String::as_str).unwrap_or("");
            // 空值永远排最后（不管升序降序）
            let empty_a = va.is_empty();
            let empty_b = vb.is_empty();
            if empty_a || empty_b {
                return match (empty_a, empty_b) {
                    (true, true) => a.cmp(&b),
                    (true, false) => std::cmp::Ordering::Greater,
                    (false, true) => std::cmp::Ordering::Less,
                    _ => unreachable!(),
                };
            }
            let ord = if numeric == Some(true) {
                // 用 `f64` 比 —— 排序只需要相对大小，精度够；展示仍然用原文。
                let fa: f64 = va.parse().unwrap_or(0.0);
                let fb: f64 = vb.parse().unwrap_or(0.0);
                fa.partial_cmp(&fb).unwrap_or(std::cmp::Ordering::Equal)
            } else {
                va.cmp(vb)
            };
            let ord = match dir {
                ColumnSort::Descending => ord.reverse(),
                _ => ord,
            };
            // 相等时按下标定序，否则排序不稳定（每次点列头顺序都可能变）
            if ord == std::cmp::Ordering::Equal {
                a.cmp(&b)
            } else {
                ord
            }
        });
        self.order = Some(idx);
    }
}

impl TableDelegate for SqlResultDelegate {
    fn columns_count(&self, _cx: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _cx: &App) -> usize {
        self.rows.len()
    }

    fn column(&self, col_ix: usize, _cx: &App) -> Column {
        self.columns
            .get(col_ix)
            .cloned()
            .unwrap_or_else(|| Column::new("", ""))
    }

    fn perform_sort(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        _window: &mut Window,
        cx: &mut Context<gpui_kit::component::table::TableState<Self>>,
    ) {
        self.sort = match sort {
            ColumnSort::Default => None,
            other => Some((col_ix, other)),
        };
        self.recompute_order();
        cx.notify();
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        _cx: &mut Context<gpui_kit::component::table::TableState<Self>>,
    ) -> impl IntoElement {
        let src = self.source_row(row_ix);
        let value = self
            .rows
            .get(src)
            .and_then(|r| r.get(col_ix))
            .cloned()
            .unwrap_or_default();

        // 三类各画成什么，由 `sqlview::cell_look` 决定（那里有单测）。
        //
        // ⚠️ `match` 的三个分支类型不同，所以统一 `.into_any_element()`：
        // `render_td` 的返回是 `impl IntoElement`，而 `AnyElement` 实现了它。
        let cell: AnyElement = match sqlview::cell_look(&value) {
            CellLook::Empty => div()
                // 空值：淡色斜体的 "NULL"。
                //
                // ⚠️ 为什么画 NULL 而不是留白：`-B` 分不出 NULL 和空串，
                // 但**留白和"这一格没数据"也分不出来** —— 画个 `NULL` 至少
                // 让人知道"这里读到了一个空值"，而不是"表格画错了"。
                // 用淡色 + 斜体表示"这不是真的字符串 NULL"。
                .text_color(crate::theme::text_dim())
                .italic()
                .child("NULL")
                .into_any_element(),
            CellLook::Number(n) => div()
                // 数字右对齐（`Column::text_right` 管列头，这里管单元格）
                .w_full()
                .text_right()
                .child(n)
                .into_any_element(),
            CellLook::Text(t) => div().child(t).into_any_element(),
        };
        cell
    }

    /// 导 CSV 用：返回**屏幕上看到的那一格**的原文。
    fn cell_text(&self, row_ix: usize, col_ix: usize, _cx: &App) -> String {
        let src = self.source_row(row_ix);
        self.rows
            .get(src)
            .and_then(|r| r.get(col_ix))
            .cloned()
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn delegate(cols: &[&str], rows: Vec<Vec<&str>>) -> SqlResultDelegate {
        SqlResultDelegate::new(SqlResult {
            columns: cols.iter().map(|s| (*s).to_owned()).collect(),
            rows: rows
                .into_iter()
                .map(|r| r.into_iter().map(|s| s.to_owned()).collect())
                .collect(),
        })
    }

    // ⚠️ 这个 crate（`wslc-panel`）**不在任何测试目标里** ——
    // `core` job 跑的是 `--workspace --exclude wslc-panel`（见 AGENTS.md §2，
    // 排除它是为了不链接整棵 GPUI 依赖树，那是 763 秒的来源）。
    //
    // 所以下面这些 `#[test]` 只在有人本地 `cargo test -p wslc-panel` 时才会跑，
    // CI 里**不执行**。真正守逻辑的测试在：
    //
    // - `wslc_panel_core::sqlview`（列宽 / 单元格分类 / 页脚 / CSV）
    // - `wslc_core::model::sqlresult`（TSV 解析）
    // - `wslc_core::model::sqlstmt`（语句切分）
    //
    // 这里只留**不碰 `App` 的那部分**（排序是纯逻辑），当作本地开发时的护栏。

    #[test]
    fn order_is_none_until_a_sort_happens() {
        let d = delegate(&["a"], vec![vec!["2"], vec!["1"]]);
        assert!(d.order.is_none());
        assert_eq!(d.source_row(0), 0);
    }

    #[test]
    fn numeric_sort_is_numeric_not_lexicographic() {
        // 字典序会把 "10" 排在 "9" 前面 —— 数字列不能那样
        let mut d = delegate(&["n"], vec![vec!["10"], vec!["9"], vec!["100"]]);
        d.sort = Some((0, ColumnSort::Ascending));
        d.recompute_order();
        let got: Vec<&str> = (0..3).map(|i| d.rows[d.source_row(i)][0].as_str()).collect();
        assert_eq!(got, ["9", "10", "100"]);
    }

    #[test]
    fn text_sort_is_lexicographic() {
        let mut d = delegate(&["s"], vec![vec!["b"], vec!["a"], vec!["c"]]);
        d.sort = Some((0, ColumnSort::Ascending));
        d.recompute_order();
        let got: Vec<&str> = (0..3).map(|i| d.rows[d.source_row(i)][0].as_str()).collect();
        assert_eq!(got, ["a", "b", "c"]);
    }

    #[test]
    fn empty_values_sort_last_in_both_directions() {
        let data = vec![vec!["b"], vec![""], vec!["a"]];
        for dir in [ColumnSort::Ascending, ColumnSort::Descending] {
            let mut d = delegate(&["s"], data.clone());
            d.sort = Some((0, dir));
            d.recompute_order();
            let last = d.rows[d.source_row(2)][0].clone();
            assert_eq!(last, "", "空值在 {dir:?} 下都应该排最后");
        }
    }

    #[test]
    fn sorting_does_not_mutate_the_original_rows() {
        let mut d = delegate(&["n"], vec![vec!["2"], vec!["1"]]);
        d.sort = Some((0, ColumnSort::Ascending));
        d.recompute_order();
        // 原始数据顺序不变
        assert_eq!(d.rows[0][0], "2");
        assert_eq!(d.rows[1][0], "1");
        // 但按显示顺序取到的第一条是 "1"
        assert_eq!(d.rows[d.source_row(0)][0], "1");
    }
}
