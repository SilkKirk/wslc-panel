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
    /// # 规则不在这个文件里
    ///
    /// 比较规则（数字按数值比、空值排最后、相等时按下标定序）全在
    /// [`sqlview::sort_order`] —— 那是**纯函数、有 7 条单测、`core` job 会跑**。
    /// 这里只负责把结果存下来。
    ///
    /// ⚠️ 这个 crate（`wslc-panel`）里**不能写 `#[test]`**：`use gpui_kit::*`
    /// 会把 GPUI 的 `test` 属性宏带进来，遮蔽 Rust 内建的 `#[test]`，
    /// 报 `error: recursion limit reached while expanding #[test]`
    /// （v0.11.1 真踩了）。而且 CI 的测试任务是
    /// `--workspace --exclude wslc-panel`，写在这里的测试**根本不会被执行**。
    /// `main.rs` 顶部有一段专门讲这件事。
    fn recompute_order(&mut self) {
        let Some((col, dir)) = self.sort else {
            self.order = None;
            return;
        };
        let dir = match dir {
            ColumnSort::Descending => sqlview::SortDir::Descending,
            // `ColumnSort::Default` 走不到这里（`perform_sort` 已把它变成 None）
            _ => sqlview::SortDir::Ascending,
        };
        self.order = Some(sqlview::sort_order(&self.rows, col, dir));
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
