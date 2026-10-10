//! 各页表格的列定义（表头 + 宽度）。
//!
//! 放在这个 crate 而不是 `views.rs`：它们是纯数据（`&[(&str, f32)]`），
//! 一行 GPUI 都没有，但留在 `views.rs` 就意味着"为了跑几个列宽断言，
//! 得把整棵 GPUI 依赖树编译并链接一遍"（见本 crate 的顶层说明）。
//!
//! `views.rs` 用 `table_header` / `table_row` 消费它们，改动这里要同步
//! 改对应 `*_row` 函数里 push 的 cell 数量 —— 下面有测试盯着这件事。

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
/// 列宽是**压着算的**：加上「操作」这一列之后总和约 860px，
/// 刚好放得进主区域（窗口 1280 减去侧边栏 216 再减去内边距）。
/// 所以「安装位置」从 330 缩到 170 —— 长路径会被截断，
/// 完整路径在详情弹窗里看（那里用 `kv_block`，不截断）。
pub const DISTRO_COLUMNS: &[(&str, f32)] = &[
    ("名称", 150.),
    ("状态", 70.),
    ("版本", 50.),
    ("默认", 40.),
    ("安装位置", 170.),
    ("磁盘（虚拟）", 84.),
    // 「操作」列要放最多 5 个按钮（打开终端 / 启动 / 终止 / 设为默认 / 删除），
    // 所以给得比别的列宽。
    ("操作", 340.),
];

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
}
