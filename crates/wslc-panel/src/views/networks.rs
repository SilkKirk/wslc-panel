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
        .enumerate()
        .map(|(ix, net)| {
            let op: AnyElement = if net.is_builtin() {
                // 10 个汉字在 880px 的窄列里会被裁；用「截断 + tooltip 全文」
                // 而不是让 `overflow_hidden` 直接切掉半句话。
                cell_muted_full(format!("net-{ix}"), "内置网络，不可删除")
            } else {
                network_row_actions(ix, &net.name, entity)
            };

            table_row(
                NETWORK_COLUMNS,
                vec![
                    cell_text(net.name.clone()),
                    cell_muted(net.driver.clone()),
                    cell_muted(if net.ipv4_enabled() {
                        "启用"
                    } else {
                        "关闭"
                    }),
                    cell_muted(if net.ipv6_enabled() {
                        "启用"
                    } else {
                        "关闭"
                    }),
                    cell_muted(if net.is_internal() { "是" } else { "否" }),
                    op,
                ],
            )
        })
        .collect();

    let body = table_card(
        NETWORK_COLUMNS,
        rows,
        Some(empty_state("没有网络").into_any_element()),
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

/// 网络行末：**直接**显示删除；内置网络仍只给一行说明。
pub(crate) fn network_row_actions(ix: usize, name: &str, entity: &Entity<Shell>) -> AnyElement {
    danger_button(
        &format!("remove-network-{ix}"),
        "删除",
        PendingAction::RemoveNetwork(name.to_owned()),
        entity,
    )
    .into_any_element()
}
