//! ④ WSL 实例（发行版）页。

use super::common::{
    badge, card, cell_muted, cell_muted_full, danger_button_distro, empty_state, immediate_button,
    table_header, table_row,
};
use super::prelude::*;

/// WSL 实例（发行版）列表。
///
/// # 能干什么
///
/// 行内放**高频**动作（打开终端 / 终止 / 设为默认 / 删除），
/// 低频但重要的（改版本 / 压缩 / 打开安装位置）收进详情弹窗 ——
/// 和容器页同一套取舍。
///
/// ⚠️ 「启动」是**真的启动**：它从 Windows 这边吊住一个 `wsl.exe` 不放，
/// WSL 就认为有活动会话，发行版会一直运行下去（机制和实测见
/// [`wslc_core::cmd::distro::start`]）。
///
/// 代价是那个进程属于**本面板**：面板关掉它也不会退，发行版继续跑。
/// 这件事写在**详情弹窗**里 —— 用户有权知道是谁在维持它、
/// 以及关掉面板之后它会怎样（只对少数几行成立，塞进列表行会把那一格挤破，
/// 见 [`distro_row`]）。
///
/// # 为什么"没有实例"不是错误
///
/// 一台机器上没装发行版是完全正常的状态，所以这里给的是引导文案
/// （怎么让 `wsl.exe` 被找到），而不是一条红色错误。
pub fn instances(state: &AppState, entity: &Entity<Shell>) -> AnyElement {
    let distros = &state.snapshot.distros;

    if distros.is_empty() {
        return v_flex()
            .w_full()
            .gap_4()
            .child(card(
                "WSL 实例",
                v_flex()
                    .w_full()
                    .gap_3()
                    .child(empty_state("没有检测到任何 WSL 发行版"))
                    .child(div().text_xs().text_color(theme::text_dim()).child(
                        "若确实安装过，请确认 wsl.exe 可用：程序会自动在 \
                         C:\\Program Files\\WSL\\ 下查找，也可以用环境变量 WSL_PATH 指定。",
                    ))
                    // 一个实例都没有的时候，最该看到的就是"怎么装一个"
                    .child(h_flex().child(add_instance_button(entity))),
            ))
            .into_any_element();
    }

    let default_name = state.default_distro().map(|name| name.to_owned());
    let rows: Vec<AnyElement> = distros
        .iter()
        .map(|distro| {
            // 本面板正在吊着它吗？（决定行内还给不给「启动」按钮 ——
            // 再吊一个没有意义。这件事本身写在详情弹窗里，列表里不显示标记）
            let kept = state.kept_alive.iter().any(|n| n == &distro.name);
            distro_row(distro, default_name.as_deref(), kept, entity)
        })
        .collect();

    let running = distros.iter().filter(|d| d.state.is_running()).count();

    // 原来这里有一张「实例概览」卡（运行中 / 全部 / 磁盘合计 / 默认发行版），
    // 和工具栏那行计数、表格本身各说了一遍同样的数字。v0.9.22 把它压成
    // **一行**：信息一个不少，但不再占一张卡的高度。
    let known = distros.iter().filter(|d| d.vhdx_bytes.is_some()).count();
    let total_bytes: u64 = distros.iter().filter_map(|d| d.vhdx_bytes).sum();
    // 一个都没读到就别写 "0 B"（那是在编一个"总共没占空间"的数）。
    let total_label = if known == 0 {
        "—".to_owned()
    } else {
        format_bytes(total_bytes)
    };
    let mut summary = format!(
        "共 {} 个发行版 · {} 个在运行 · 磁盘合计 {total_label}（虚拟）",
        distros.len(),
        running,
    );
    if let Some(default) = state.default_distro() {
        summary.push_str(&format!(" · 默认 {default}"));
    }
    if known != distros.len() {
        // 读不到大小的那些按 0 算进合计了，必须如实说有几个 ——
        // 否则用户会以为合计是准的。
        summary.push_str(&format!(" · {} 个没读到大小", distros.len() - known));
    }

    // 「关停全部」是**页面级**动作（影响所有发行版），所以放工具栏，
    // 不放进任何一行 —— 放行里会让人以为只影响那一行。
    //
    // 一个都没在跑时干脆不显示：`--shutdown` 虽然不会失败，
    // 但让用户确认一个没有效果的动作是没意义的。
    let shutdown_all: AnyElement = if running == 0 {
        div().into_any_element()
    } else {
        let entity = entity.clone();
        Button::new("shutdown-all")
            .label("关停全部")
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| {
                    shell.request_distro(DistroAction::ShutdownAll, cx);
                });
            })
            .into_any_element()
    };

    v_flex()
        .w_full()
        .gap_3()
        .child(
            h_flex()
                .w_full()
                .justify_between()
                .items_center()
                .child(h_flex().gap_3().child(add_instance_button(entity)))
                .child(shutdown_all),
        )
        .child(
            div()
                .w_full()
                .text_sm()
                .text_color(theme::text_muted())
                .child(summary),
        )
        .child(
            v_flex()
                .w_full()
                .rounded_lg()
                .bg(theme::bg_card())
                .border_1()
                .border_color(theme::border())
                .overflow_hidden()
                .child(table_header(DISTRO_COLUMNS))
                .child(v_flex().w_full().children(rows)),
        )
        .into_any_element()
}

/// 一行实例。
///
/// `kept` = **本面板正吊着它**（见 [`KeepAlive`](crate::app)）。
/// 列表里它只影响「启动」按钮给不给（已经吊着了，再吊一个没意义）；
/// "谁在维持这个发行版"那句话在详情弹窗里，不在这一行 —— 原因见下面的注释。
pub(crate) fn distro_row(
    distro: &Distro,
    default_name: Option<&str>,
    kept: bool,
    entity: &Entity<Shell>,
) -> AnyElement {
    let is_default = default_name == Some(distro.name.as_str());

    let location = distro
        .base_path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "—".to_owned());

    let disk = match distro.vhdx_bytes {
        Some(bytes) => format_bytes(bytes),
        None => "—".to_owned(),
    };

    // 状态列**只放状态徽标本身**，不在这里再标"由本面板保持运行"。
    //
    // ⚠️ 原先运行中的行会在状态右边再挂一个「本面板保持」徽标，实机截图里
    // 它**盖到了隔壁「版本」列的数字上**（两个字叠在一起，成了一段乱码）。
    // 原因是两个徽标都是 `flex_none`（不压缩），加起来比状态列宽
    // （权重 85 / 总和 1060），而 `table_row` 的单元格**不裁剪**内容 ——
    // 超出的部分就这么画进了下一列。单独加宽状态列只是把阈值往后推，
    // 换一种字体 / 缩放到 125% 又会溢出来。
    // 「谁在维持它」是低频信息，改写在详情弹窗里（`distro_detail_overlay`）。
    let status = cell_distro_badge(distro.state);

    table_row(
        DISTRO_COLUMNS,
        vec![
            distro_name_link(&distro.name, entity),
            status,
            cell_muted(distro.version_label()),
            if is_default {
                badge("是", theme::primary(), theme::primary_soft()).into_any_element()
            } else {
                cell_muted("—")
            },
            cell_muted_full(distro.name.clone(), location),
            cell_muted(disk),
            distro_row_actions(distro, is_default, kept, entity),
        ],
    )
    .into_any_element()
}

/// 发行版名字 —— 点击打开详情弹窗。
///
/// 和容器页一样：名字就是入口，低频动作都收在详情里。
/// 悬停/按下同 [`container_name_link`]。
pub(crate) fn distro_name_link(name: &str, entity: &Entity<Shell>) -> AnyElement {
    let entity = entity.clone();
    let for_click = name.to_owned();
    div()
        .id(SharedString::from(format!("distro-detail-{name}")))
        .cursor_pointer()
        .text_sm()
        .text_color(theme::primary())
        .hover(|this| this.text_color(theme::link_hover()))
        .active(|this| this.text_color(theme::primary()))
        .overflow_hidden()
        .truncate()
        .child(name.to_owned())
        .on_click(move |_, _, cx| {
            let target = for_click.clone();
            entity.update(cx, |shell, cx| shell.open_distro_detail(target, cx));
        })
        .into_any_element()
}

/// 「添加实例」入口按钮 —— 跳到「添加实例」页。
///
/// 跳转走 `set_page`（需要 `window`）：那一页的输入框必须在点击时创建。
pub(crate) fn add_instance_button(entity: &Entity<Shell>) -> AnyElement {
    let entity = entity.clone();
    Button::new("goto-add-instance")
        .label("添加实例")
        .small()
        .primary()
        .on_click(move |_, window, cx| {
            entity.update(cx, |shell, cx| {
                shell.set_page(Page::AddInstance, window, cx);
            });
        })
        .into_any_element()
}

/// 发行版行内操作：**只放高频的**。
///
/// 低频但重要的（改版本 / 压缩 / 打开安装位置）都在详情弹窗里 ——
/// 列表里塞满按钮，常用的那个反而找不到。
///
/// `kept` = 本面板正吊着它（见 [`distro_row`]）。
pub(crate) fn distro_row_actions(
    distro: &Distro,
    is_default: bool,
    kept: bool,
    entity: &Entity<Shell>,
) -> AnyElement {
    let name = distro.name.as_str();
    let mut actions: Vec<AnyElement> = Vec::new();

    // 打开终端同时就把发行版启动了，所以它既是"启动"也是"进去干活"。
    actions.push(
        immediate_button(
            &format!("term-{name}"),
            "打开终端",
            ImmediateAction::OpenDistroTerminal(name.to_owned()),
            entity,
        )
        .into_any_element(),
    );

    // 「启动」是**真的**启动：本面板会吊住一个 `wsl.exe` 让发行版一直运行
    // （见 `wslc_core::cmd::distro::start`）。
    // 已经由本面板吊着的就不再给这个按钮 —— 再吊一个没有意义，只会多一个进程。
    if !distro.state.is_running() && !distro.state.is_transitional() && !kept {
        actions.push(
            immediate_button(
                &format!("start-{name}"),
                "启动",
                ImmediateAction::StartDistro(name.to_owned()),
                entity,
            )
            .into_any_element(),
        );
    }

    // 只有运行中才谈得上"终止"。
    if distro.state.is_running() {
        actions.push(
            danger_button_distro(
                &format!("terminate-{name}"),
                "终止",
                DistroAction::Terminate(name.to_owned()),
                entity,
            )
            .into_any_element(),
        );
    }

    // 已经是默认了就不必再显示这个按钮（详情里会写明"当前是默认"）。
    // 同上：过渡态下一律不给写操作（这段代码上方的「启动」按钮
    // 也是用 `distro.state.is_transitional()` 挡的）。
    if !is_default && !distro.state.is_transitional() {
        actions.push(
            immediate_button(
                &format!("default-{name}"),
                "设为默认",
                ImmediateAction::SetDefaultDistro(name.to_owned()),
                entity,
            )
            .into_any_element(),
        );
    }

    // 「删除」**不在这里** —— 它挪去了详情。列表行是横向排布的，
    // 一个红色的「删除」和「打开终端」挨着，误点的代价太大；
    // 详情里地方宽，能把"要删掉多少"写清楚（见 `distro_detail_overlay`）。

    h_flex()
        .gap_2()
        .flex_wrap()
        .children(actions)
        .into_any_element()
}

/// 发行版状态徽标单元格。
pub(crate) fn cell_distro_badge(state: DistroState) -> AnyElement {
    let (fg, bg) = theme::distro_state_colors(&state);
    badge(state.label().to_owned(), fg, bg).into_any_element()
}

// ---------------------------------------------------------------------------
// ④b 添加实例
// ---------------------------------------------------------------------------
