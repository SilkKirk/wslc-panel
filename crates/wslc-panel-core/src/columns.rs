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
///
/// ⚠️ 「操作」的权重走过 250 → 330（v0.7.0，加了「数据库」）→ 440（v0.9.13，
/// 加了「部署管理」）→ **340**（v0.9.14，把低频动作收进「更多」菜单）。
///
/// # 为什么又收回来了
///
/// 440 那一版是按"最多 7 个按钮"算的（终端 / 数据库 / 部署管理 / 重启 / 停止 /
/// 日志 / 删除，共 17 个汉字）。实机截图里那一列确实被挤成了两行 ——
/// **但真正的解法不是把列加宽，而是别把 7 个按钮全铺出来**。
/// v0.9.14 起行内只留 `[数据库] [部署管理] [启动/停止] [更多]`（最多 **4 个**、
/// 11 个汉字），其余进菜单，于是这一列可以收回到 340。
///
/// 340 的口径仍然是 `DB_COLUMNS` 那次教训（见下面那段：4 个按钮 / 11 个字在
/// ≈273px 里被裁掉，最后给到 360/1390 ≈ 26%）：11 个字 ≈ 11×13px 文字 +
/// 4×16px 内边距 + 3×8px 间隙 ≈ 231px，加上余量 → 340/1350 ≈ 25%，
/// 在 1707 逻辑宽下给 ≈360px。**仍然是算的**，`AGENTS.md` §8 那条"实机看一眼"
/// 照旧成立 —— 好在这次的形状（4 个按钮）和数据库页那次已经验证过的形状一致。
pub const ALL_COLUMNS: &[(&str, f32)] = &[
    ("名称", 240.0),
    ("镜像", 260.0),
    ("状态", 120.0),
    ("资源", 190.0),
    ("端口", 200.0),
    ("操作", 340.0),
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
/// 「操作」要放最多 4 个按钮（打开终端 / 启动 / 终止 / 设为默认），权重给得最大。
///
/// ⚠️ 「删除」**不在**列表行里 —— v0.5.11 把它挪进了详情弹窗：行内一个红色按钮
/// 挨着「打开终端」，误点的代价太大，而详情里地方宽，能把"要删掉多少"写清楚。
/// （这条注释原先写的是"5 个按钮（含删除）"，实现早就不是了。）
pub const DISTRO_COLUMNS: &[(&str, f32)] = &[
    ("名称", 170.),
    ("状态", 85.),
    ("版本", 70.),
    ("默认", 55.),
    ("安装位置", 260.),
    ("磁盘（虚拟）", 100.),
    ("操作", 320.),
];

/// 「数据库管理」页 → **数据库列表**的列。
///
/// 数据来自一条 SQL（`information_schema.SCHEMATA` + 两个子查询），
/// 见 `wslc_core::model::mysql::list_databases_sql` —— 一次 exec 拿全，
/// 不是每个库去问一次。
///
/// 字符集和排序规则分开两列（而不是挤成一格）：它们经常一起被念出来
/// （`utf8mb4` + `utf8mb4_0900_ai_ci`），但改的时候是两件事。
///
/// v0.8 加了「用户名 / 密码」两列：**面板建的库**才有（建库时同时建的用户，
/// 记在本地 `db-credentials.json`）；别处建的库显示 `—`。密码这一列**明文**
/// 是产品决定（用户要抄去别处连），存档与风险说明见 `dbcreds` 的模块注释。
pub const DB_COLUMNS: &[(&str, f32)] = &[
    ("名称", 200.0),
    ("用户名", 170.0),
    ("密码", 150.0),
    ("字符集", 130.0),
    ("排序规则", 210.0),
    ("表数", 70.0),
    ("占用", 100.0),
    // 190 → 240 → **360**。v0.9.2 起这一列是四个按钮
    //（「备份 / 导入备份 / SQL / 删除」）。
    //
    // ⚠️ 240 那一版**实机被裁了**（用户截图：最右边的「删除」只剩半个「删」）——
    // 教训是"按像素估"估错了坐标系：这台机器是 **2560 物理 / 150% 缩放**
    // = **1707 逻辑**宽，去掉侧边栏后内容区 ≈1447 逻辑；240/(1220+240) = 18.9%
    // 只给出 ≈273px，而四个按钮（2+4+3+2 个字）连内边距 + 三个间隙
    // 实际要 ≈285px，差十几个像素就被 `overflow_hidden` 裁掉。
    //
    // 360/1390 ≈ 26%，在同样的窗口下给 ≈375px（余量 ≈90px）。
    // 这个量级也该是本仓库的默认：同为多按钮的操作列，
    // `ALL_COLUMNS` 是 340/1350 ≈ 25%（v0.9.14 起 4 个按钮 + 「更多」菜单）、
    // `DB_USER_COLUMNS` 是 320/970 ≈ 33%（3 个按钮）、
    // `BACKUP_COLUMNS` 300/1000 = 30%、`DEPLOY_COLUMNS` 330/1240 ≈ 27%。
    // 按钮是 `flex_none`（不收缩），所以**列宽不够的唯一表现就是被裁**，
    // 不会自己换行 —— 加按钮时顺手把这个数往上调，别只调一点点；
    // 但按钮超过 4 个时，更好的做法是收进「更多」菜单（见 `ALL_COLUMNS` 那段）。
    ("操作", 360.0),
];

/// 「数据库管理」页 → **用户列表**的列。
///
/// ⚠️ 「主机」必须单独一列：`'root'@'%'` 和 `'root'@'localhost'` 在 MySQL 里是
/// **两条独立的记录**（实测那个容器的 `mysql.user` 里就有这两条），
/// 不写 host 的话用户根本分不清自己在改哪一个。
///
/// 「操作」给得最宽：行内要放「改密码 / 授权 / 删除」三个按钮。
pub const DB_USER_COLUMNS: &[(&str, f32)] = &[
    ("用户", 200.0),
    ("主机", 160.0),
    ("认证插件", 200.0),
    ("超级权限", 90.0),
    ("操作", 320.0),
];

/// 「备份列表」浮层的列（v0.8）。
///
/// 文件名给得最宽（`djxm-20261015-035901.sql` 很长，且它就是要认的东西）；
/// 时间直接用文件名里那段（定宽、可排序）。
pub const BACKUP_COLUMNS: &[(&str, f32)] = &[
    ("文件", 420.0),
    ("大小", 100.0),
    ("备份时间", 180.0),
    ("操作", 300.0),
];

/// 「部署管理」页 → **已部署列表**的列（v0.9）。
///
/// 「形态」那一列是 **内容 / 进程**（见 `AppRuntime::kind`）：它决定了这一行
/// 有哪些按钮能按 —— 内容形态（nginx）没有"启动 / 停止"，只有"重新上传 / 重载"。
/// 把它画出来，用户就不用猜"为什么这个应用没有启动按钮"。
///
/// 「目标路径」给得最宽：它是这一行**最要紧的一格**（部署到底铺到了哪儿、
/// 删除会删掉什么都在它身上），而且容器路径经常很长 ——
/// 所以它在 `views.rs` 里走 `cell_muted_full`（截断了也能悬停看全文）。
///
/// 「端口」是**信息性**的：已存在的容器改不了端口发布（`-p` 只在 `run` 时给），
/// 所以这一格只能如实报"发布了没有"。
pub const DEPLOY_COLUMNS: &[(&str, f32)] = &[
    ("应用名", 200.0),
    ("形态", 90.0),
    ("目标路径", 340.0),
    ("状态", 170.0),
    ("端口", 110.0),
    ("操作", 330.0),
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
    use super::{
        ALL_COLUMNS, BACKUP_COLUMNS, DB_COLUMNS, DB_USER_COLUMNS, DEPLOY_COLUMNS,
        DISTRO_COLUMNS, IMAGE_COLUMNS, NETWORK_COLUMNS, VOLUME_COLUMNS,
    };

    /// 汇总所有表格的列定义，方便逐个检查。
    fn all_column_sets() -> Vec<&'static [(&'static str, f32)]> {
        vec![
            ALL_COLUMNS,
            IMAGE_COLUMNS,
            NETWORK_COLUMNS,
            VOLUME_COLUMNS,
            DISTRO_COLUMNS,
            DB_COLUMNS,
            DB_USER_COLUMNS,
            BACKUP_COLUMNS,
            DEPLOY_COLUMNS,
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
        //
        // 后两个是 v0.7.0 的数据库管理页（v0.8 起数据库 8 列：加了用户名 / 密码）；
        // 最后一个是 v0.8 的备份列表（4 列），倒数第二个是 v0.9 的部署列表（6 列）。
        let counts: Vec<usize> = all_column_sets().iter().map(|c| c.len()).collect();
        assert_eq!(counts, vec![6, 5, 6, 5, 7, 8, 5, 4, 6]);
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
