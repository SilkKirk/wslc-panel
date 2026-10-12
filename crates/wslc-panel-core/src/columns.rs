//! 各页表格的列定义（表头 + 宽度**权重**）。
//!
//! 放在这个 crate 而不是 `views.rs`：它们是纯数据（`&[(&str, f32)]`），
//! 一行 GPUI 都没有，但留在 `views.rs` 就意味着"为了跑几个列宽断言，
//! 得把整棵 GPUI 依赖树编译并链接一遍"（见本 crate 的顶层说明）。
//!
//! `views.rs` 用 `table_header` / `table_row` 消费它们，改动这里要同步
//! 改对应 `*_row` 函数里 push 的 cell 数量 —— 下面有测试盯着这件事。
//!
//! # 这里的数字是**权重**，不是像素
//!
//! 表格曾经按固定像素宽渲染：宽度之和小于窗口时，右边就留一大片空白 ——
//! 在 2244 宽的窗口上，看起来就是"前几列挤在一起、最后一列拖出去一大截"。
//! 现在按 [`column_fractions`] 换成比例（`宽度 / Σ宽度`），
//! 由 `flex_grow` 分配实际宽度，于是**永远占满整行、随窗口伸缩**。
//!
//! 所以调这里的数字只影响**相对**宽窄：想让某列更宽就调大它，
//! 想让整体更宽松就加窗口 —— 不会再有"总和超了/少了"的问题。

/// 「全部容器」页的列。
pub const ALL_COLUMNS: &[(&str, f32)] = &[
    ("名称", 240.0),
    ("镜像", 260.0),
    ("状态", 120.0),
    ("资源", 190.0),
    ("端口", 200.0),
    ("操作", 250.0),
];

/// 镜像页的列。
pub const IMAGE_COLUMNS: &[(&str, f32)] = &[
    ("仓库:标签", 380.0),
    ("ID", 130.0),
    ("大小", 100.0),
    ("创建于", 200.0),
    ("操作", 120.0),
];

/// 网络页的列。
pub const NETWORK_COLUMNS: &[(&str, f32)] = &[
    ("名称", 200.0),
    ("ID", 140.0),
    ("驱动", 100.0),
    ("作用域", 100.0),
    ("IPv6", 80.0),
    ("操作", 220.0),
];

/// 卷页的列。
pub const VOLUME_COLUMNS: &[(&str, f32)] = &[
    ("名称", 240.0),
    ("驱动", 120.0),
    ("作用域", 120.0),
    ("挂载点", 320.0),
    ("操作", 120.0),
];

/// 实例列表的列（名称 / 状态 / 版本 / 默认 / 安装位置 / 磁盘 / 操作）。
///
/// 「磁盘」是 VHDX 的**虚拟大小**，不是实际占用 —— 见 `views::instances` 的说明。
///
/// 「安装位置」给得比别的列宽：路径长，宽一点少截断几个字
/// （完整路径在详情弹窗里看，那里用 `kv_block`，不截断）。
/// 「操作」要放最多 5 个按钮（打开终端 / 启动 / 终止 / 设为默认 / 删除），
/// 权重给得最大。
pub const DISTRO_COLUMNS: &[(&str, f32)] = &[
    ("名称", 170.),
    ("状态", 85.),
    ("版本", 70.),
    ("默认", 55.),
    ("安装位置", 260.),
    ("磁盘（虚拟）", 100.),
    ("操作", 320.),
];

/// 把"权重"换算成**比例**（加起来正好 1），交给 `flex_grow` 分配实际宽度。
///
/// 全是 0 / 负数（不该发生，但别 panic）时退回等分。
// ⚠️ 别照 clippy 的 `neg_cmp_op_on_partial_ord` 把下面那行改成
// `total <= 0.0`：`!(total > 0.0)` 和它**不等价**。`total` 是 `f32`，
// 一旦是 NaN，`NaN > 0.0` 和 `NaN <= 0.0` **都是 false** ——
// 用 `<=` 会漏掉 NaN，继续走到下面按 NaN 算出一堆 NaN 宽度，
// 结果是一行完全错位的表格。这里要的是"只要不是正数（含 NaN）就等分"。
// 这个写法是有意的，所以就地允许这条 lint，而不是改语义。
#[allow(clippy::neg_cmp_op_on_partial_ord)]
pub fn column_fractions(columns: &[(&str, f32)]) -> Vec<f32> {
    let total: f32 = columns.iter().map(|(_, weight)| *weight).sum();
    if columns.is_empty() {
        return Vec::new();
    }
    if !(total > 0.0) {
        return vec![1.0 / columns.len() as f32; columns.len()];
    }
    columns.iter().map(|(_, weight)| weight / total).collect()
}

#[cfg(test)]
mod tests {
    use super::{ALL_COLUMNS, DISTRO_COLUMNS, IMAGE_COLUMNS, NETWORK_COLUMNS, VOLUME_COLUMNS};

    /// 汇总所有表格的列定义，方便逐个检查。
    fn all_column_sets() -> Vec<&'static [(&'static str, f32)]> {
        vec![
            ALL_COLUMNS,
            IMAGE_COLUMNS,
            NETWORK_COLUMNS,
            VOLUME_COLUMNS,
            DISTRO_COLUMNS,
        ]
    }

    #[test]
    fn table_columns_have_names_and_positive_widths() {
        for columns in all_column_sets() {
            for &(name, width) in columns {
                assert!(!name.is_empty(), "列名不能为空");
                assert!(width > 0.0, "列宽必须为正数：{name}");
            }
        }
    }

    #[test]
    fn every_table_has_the_expected_column_count() {
        // 行内的 cell 数量少于列数只会留下空白，多出来则会被丢弃；
        // 这里把"必须一一对应"的约束固化下来，避免改表头时忘记改行。
        //
        // `DISTRO_COLUMNS` 是 7 列（P2 加了「操作」），
        // 和 `distro_row` 里 push 的 7 个 cell 对应 ——
        // 改一边就必须改另一边，这个断言就是盯着这件事的。
        let counts: Vec<usize> = all_column_sets().iter().map(|c| c.len()).collect();
        assert_eq!(counts, vec![6, 5, 6, 5, 7]);
    }

    #[test]
    fn distro_row_cells_match_the_column_count() {
        // 上面那个测试只保证"列定义"本身没问题，管不到行里塞了几个 cell。
        // 这里直接把列数钉死，配合 `distro_row` 的 7 个 cell 使用。
        assert_eq!(
            DISTRO_COLUMNS.len(),
            7,
            "distro_row 里的 cell 数量必须与之同步"
        );
    }

    #[test]
    fn column_fractions_add_up_to_one() {
        // 表格是靠这些比例 + `flex_grow` 占满整行的：和不是 1 就会留白或者溢出
        for columns in all_column_sets() {
            let fractions = super::column_fractions(columns);
            assert_eq!(fractions.len(), columns.len());
            let sum: f32 = fractions.iter().sum();
            assert!(
                (sum - 1.0).abs() < 1e-5,
                "比例之和应当是 1，实际 {sum}：{columns:?}"
            );
            assert!(fractions.iter().all(|f| *f > 0.0), "{fractions:?}");
        }
    }

    #[test]
    fn empty_or_zero_weights_still_give_a_usable_row() {
        assert!(super::column_fractions(&[]).is_empty());
        // 权重全是 0（不该发生）→ 等分，而不是 NaN 或 panic
        let equal = super::column_fractions(&[("a", 0.0), ("b", 0.0)]);
        assert_eq!(equal.len(), 2);
        assert!((equal[0] - 0.5).abs() < 1e-6, "{equal:?}");
    }

    #[test]
    fn nan_weights_fall_back_to_equal_columns() {
        // ⚠️ 这条是"别照 clippy 把 `!(total > 0.0)` 改成 `total <= 0.0`"的证据。
        //
        // 对 NaN 来说 `NaN > 0.0` 和 `NaN <= 0.0` **都是 false**，
        // 所以 `<=` 会一路走到下面按 NaN 算宽度 —— 表格整行错位。
        // `!(total > 0.0)` 才是"只要不是正数就等分"，NaN 落在这里。
        let with_nan = super::column_fractions(&[("a", f32::NAN), ("b", 1.0)]);
        assert_eq!(with_nan.len(), 2);
        assert!(
            with_nan
                .iter()
                .all(|f| f.is_finite() && (*f - 0.5).abs() < 1e-6),
            "NaN 权重必须退回等分，而不是算出 NaN：{with_nan:?}"
        );

        // 权重里混一个负的（也不该发生）同样按"不是正数"处理
        let negative = super::column_fractions(&[("a", -1.0), ("b", -1.0)]);
        assert!(
            negative.iter().all(|f| (*f - 0.5).abs() < 1e-6),
            "{negative:?}"
        );
    }
}
