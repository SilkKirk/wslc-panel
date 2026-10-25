//! ③ 卷页。

use super::common::{
    cell_muted, cell_muted_full, cell_text, danger_button, empty_state, prompt_button,
    table_header, table_row,
};
use super::prelude::*;

/// 卷页。
pub fn volumes(state: &AppState, entity: &Entity<Shell>) -> impl IntoElement {
    let rows: Vec<AnyElement> = state
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
            .into_any_element()
        })
        .collect();

    // 页头：左边计数，右边两个动作。
    //
    // 「创建卷」复用那个共用的单输入框弹窗（`PromptKind::CreateVolume`）——
    // 注意它**不属于任何发行版**，所以主体传空串（弹窗据此不画那一行）。
    let header = h_flex()
        .w_full()
        .justify_between()
        .items_center()
        .child(
            div()
                .text_sm()
                .text_color(theme::text_muted())
                .child(format!("共 {} 个卷", state.snapshot.volumes.len())),
        )
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
        );

    v_flex().w_full().gap_3().child(header).child(
        v_flex()
            .w_full()
            .rounded_lg()
            .bg(theme::bg_card())
            .border_1()
            .border_color(theme::border())
            .overflow_hidden()
            .child(table_header(VOLUME_COLUMNS))
            .child(if rows.is_empty() {
                empty_state("没有卷（无卷时 wslc 输出为空）").into_any_element()
            } else {
                v_flex().w_full().children(rows).into_any_element()
            }),
    )
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

