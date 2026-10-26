//! ③ 卷页。

use super::common::{
    cell_muted, cell_muted_full, cell_text, danger_button, empty_state, list_count, prompt_button,
    table_card, table_row,
};
use super::prelude::*;

/// 卷页。
pub(crate) fn volumes_page(state: &AppState, entity: &Entity<Shell>) -> impl IntoElement {
    // 行列表必须是具体的 `TableRow`（官方的 `TableBody::children` 要求
    // `ChildElement`，而 `AnyElement` 不满足 —— 见 `common::table_row` 的说明）。
    let rows: Vec<TableRow> = state
        .snapshot
        .volumes
        .iter()
        .enumerate()
        .map(|(ix, vol)| {
            table_row(
                VOLUME_COLUMNS,
                vec![
                    cell_text(vol.display_name().to_owned()),
                    volume_usage_cell(vol),
                    cell_muted(vol.driver.clone()),
                    cell_muted_full(vol.name.clone(), vol.mountpoint.clone()),
                    volume_row_actions(ix, &vol.name, entity),
                ],
            )
        })
        .collect();

    // 页头：**动作在左、计数在右** —— 和「镜像」「数据库」两页同一个形状
    // （原先这一页反过来放，切页的人每换一页都要重新找一眼"这页有几个"）。
    //
    // 「创建卷」复用那个共用的单输入框弹窗（`PromptKind::CreateVolume`）——
    // 注意它**不属于任何发行版**，所以主体传空串（弹窗据此不画那一行）。
    let header = h_flex()
        .w_full()
        .justify_between()
        .items_center()
        .child(
            h_flex()
                .gap_2()
                .child(prompt_button(
                    "volume-create",
                    "创建卷",
                    PromptKind::CreateVolume,
                    "",
                    entity,
                ))
                .child(
                    // 「清理」在界面上的名字要带上"未使用"三个字：
                    // 只写"清理"会让人担心它把正在用的卷也删了。
                    danger_button(
                        "volume-prune",
                        "清理未使用卷",
                        PendingAction::PruneVolumes,
                        entity,
                    )
                    .into_any_element(),
                ),
        )
        .child(list_count(format!(
            "共 {} 个卷",
            state.snapshot.volumes.len()
        )));

    v_flex().w_full().gap_3().child(header).child(table_card(
        VOLUME_COLUMNS,
        rows,
        Some(empty_state("没有卷（无卷时 wslc 输出为空）").into_any_element()),
    ))
}

/// 卷「使用中」格。
///
/// ⚠️ `InUse` 字段没有实机夹具（采集环境没有卷，见 `volume.rs` 顶部），
/// 所以 `None` 必须显示 `—`。**不要**拿它决定能不能删 —— 删除/清理
/// 仍然交给 `wslc` 自己判断。
pub(crate) fn volume_usage_cell(volume: &VolumeListItem) -> AnyElement {
    match volume.in_use() {
        Some(true) => cell_text("使用中"),
        Some(false) => div()
            .text_sm()
            .text_color(theme::text_muted())
            .child("未使用")
            .into_any_element(),
        None => cell_muted("—"),
    }
}

/// 卷行末：**直接**显示删除。
pub(crate) fn volume_row_actions(ix: usize, name: &str, entity: &Entity<Shell>) -> AnyElement {
    danger_button(
        &format!("remove-volume-{ix}"),
        "删除",
        PendingAction::RemoveVolume(name.to_owned()),
        entity,
    )
    .into_any_element()
}

// ---------------------------------------------------------------------------
// ④ WSL 实例（发行版）
// ---------------------------------------------------------------------------
