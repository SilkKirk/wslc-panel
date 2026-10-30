//! ③ 网络页。

use super::common::{
    cell_muted, cell_muted_full, cell_text, danger_button, empty_state, list_count, table_card,
    table_row,
};
use super::prelude::*;

/// 网络页。
pub(crate) fn networks_page(state: &AppState, entity: &Entity<Shell>) -> impl IntoElement {
    let rows: Vec<TableRow> = state
        .snapshot
        .networks
        .iter()
        .map(|net| {
            let op: AnyElement = if net.is_builtin() {
                // 10 个汉字在 880px 的窄列里会被裁；用「截断 + tooltip 全文」
                // 而不是让 `overflow_hidden` 直接切掉半句话。
                // id 用名字而不是下标：见下面 `remove` 那段说明。
                cell_muted_full(format!("net-{}", net.name), "内置网络，不可删除")
            } else {
                // ⚠️ id 用**网络名**，不用下标。
                //
                // 原先这里是 `format!("remove-network-{ix}")`（`enumerate()`
                // 给的下标）。两行的 id 会随「删掉上面那一行」而前移 ——
                // 而 GPUI 的 hover / 焦点状态是**按元素 id 记**的，迁移之后
                // 光标停在的那一行会忽然变成另一行。
                //
                // 全仓其它列表页（容器 / 数据库 / 部署）用的都是业务标识，
                // 网络页是最后一个用下标的。
                danger_button(
                    &format!("remove-network-{}", net.name),
                    "删除",
                    PendingAction::RemoveNetwork(net.name.clone()),
                    entity,
                )
                .into_any_element()
            };

            table_row(
                NETWORK_COLUMNS,
                vec![
                    cell_text(net.name.clone()),
                    cell_muted(net.driver.clone()),
                    cell_muted(yes_no(net.ipv4_enabled())),
                    cell_muted(yes_no(net.ipv6_enabled())),
                    // `IPv4` / `IPv6` / `内部` 三列的列头都是**名词**，填进去的值
                    // 回答的也都是"有没有启用 / 是不是内部"。
                    //
                    // 原先这三列用了两套说法：IPv4/IPv6 是 `启用 / 关闭`，
                    // 内部是 `是 / 否`。同一张表、同一类布尔值、两种措辞 ——
                    // 扫一眼会以为它们不是一回事。统一成 `是 / 否`（`yes_no`）。
                    cell_muted(yes_no(net.is_internal())),
                    op,
                ],
            )
        })
        .collect();

    let body = table_card(
        NETWORK_COLUMNS,
        rows,
        Some(
            // ⚠️ 这一页**刻意不给"下一步点哪"** —— 它没有创建入口。
            //
            // 面板没接 `wslc network create`（v0.9 起没做），用户自定义网络
            // 只会在「创建容器」选网络时由 `wslc` 自己带出来。所以这里能说的
            // 只是一句**它为什么会空**：`bridge` / `host` / `none` 是内置网络，
            // 正常机器上至少有一个 `bridge`；一个都没有通常意味着
            // `wslc network list` 读失败（原因在页面顶部的红色横幅里）。
            //
            // （「镜像」/「卷」两页的空态给的是"点右上角…"，那两页确实有
            // 创建入口 —— 别把这条规矩机械地套到这里。）
            empty_state("没有读到任何网络 —— 内置的 bridge 通常总有一个").into_any_element(),
        ),
    );

    v_flex()
        .w_full()
        .gap_3()
        .child(list_count(format!(
            "共 {} 个网络",
            state.snapshot.networks.len()
        )))
        .child(body)
}

/// 布尔列的值：`是` / `否`。
///
/// 抽出来只为让上面那四个布尔格的写法一致 —— 原先两处 `启用/关闭`、
/// 一处 `是/否`，同一张表里三种说法回答同一个问题。
fn yes_no(value: bool) -> &'static str {
    if value { "是" } else { "否" }
}
