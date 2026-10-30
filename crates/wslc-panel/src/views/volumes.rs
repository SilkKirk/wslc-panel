//! ③ 卷页。

use super::common::{
    cell_muted, cell_muted_full, cell_text, danger_button, empty_state, list_count, page_header,
    prompt_button, table_card, table_row,
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
        .map(|vol| {
            table_row(
                VOLUME_COLUMNS,
                vec![
                    cell_text(vol.display_name().to_owned()),
                    volume_usage_cell(vol),
                    cell_muted(vol.driver.clone()),
                    cell_muted_full(vol.name.clone(), vol.mountpoint.clone()),
                    // ⚠️ id 用**卷名**，不是 `enumerate()` 给的下标 ——
                    // GPUI 的 hover / 焦点状态按元素 id 记，下标会在删掉上一行
                    // 之后整体前移，光标停的那一行就变成另一行了。
                    // 全仓其它列表页用的都是业务标识（见 `views/networks.rs`
                    // 同一处说明）。
                    danger_button(
                        &format!("remove-volume-{}", vol.name),
                        "删除",
                        PendingAction::RemoveVolume(vol.name.clone()),
                        entity,
                    )
                    .into_any_element(),
                ],
            )
        })
        .collect();

    // 页头：**动作在左、计数在右** —— 和「镜像」「数据库」两页同一个形状
    // （原先这一页反过来放，切页的人每换一页都要重新找一眼"这页有几个"）。
    //
    // 「创建卷」复用那个共用的单输入框弹窗（`PromptKind::CreateVolume`）——
    // 注意它**不属于任何发行版**，所以主体传空串（弹窗据此不画那一行）。
    let actions = h_flex()
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
        );
    let header = page_header(
        actions,
        list_count(format!("共 {} 个卷", state.snapshot.volumes.len())),
    );

    v_flex().w_full().gap_3().child(header).child(table_card(
        VOLUME_COLUMNS,
        rows,
        Some(
            // 空态给下一步（和「镜像」页同一份理由）：这一页有「创建卷」，
            // 而原先只写"没有卷（无卷时 wslc 输出为空）"—— 括号里那句是
            // **给我们自己看的**（解释为什么是空的），对用户没有下一步。
            empty_state("还没有卷 —— 点左上角的「创建卷」建一个").into_any_element(),
        ),
    ))
}

/// 卷「使用中」格。
///
/// ⚠️ `InUse` 字段没有实机夹具（采集环境没有卷，见 `volume.rs` 顶部），
/// 所以 `None` 必须显示 `—`。**不要**拿它决定能不能删 —— 删除/清理
/// 仍然交给 `wslc` 自己判断。
///
/// 三档的**颜色**是有意的：`使用中` 用正文色（值得注意）、`未使用` 用次要色
/// （默认状态）、`—` 用更淡的色（我们不知道）。三者都是一句话，所以都走
/// `cell_*`，别手写 `div` —— 原先 `Some(false)` 那支是自己拼的
/// `div().text_sm().text_color(...)`，和另外两支不同源。
fn volume_usage_cell(volume: &VolumeListItem) -> AnyElement {
    match volume.in_use() {
        Some(true) => cell_text("使用中"),
        Some(false) => cell_muted("未使用"),
        None => cell_muted("—"),
    }
}
