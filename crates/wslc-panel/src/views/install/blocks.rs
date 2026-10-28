//! 「添加实例」页的两块来源卡片：微软商店 / 在线发行版（镜像源）。
//!
//! 只被 `install.rs` 的页面函数调用；`install.rs` 与 `install/` 目录并存，
//! 所以不需要改成目录模型。

// 逐项导入而不是 `common::*`：`views/mod.rs` 那段说明过"逐项列"是刻意的
// ——glob 会让"这里到底依赖 common 的哪些东西"变成一件查不清的事，
// 而 `common` 有 40 多个条目，实际本文件只用到 2 个。
use super::super::common::{busy_text_xs, form_field};
use super::super::prelude::*;

/// 「微软商店」那一块：刷新 + **下拉框** + 来源说明。
///
/// 参考实现也是一个下拉框（`CustomComboBox`）—— 原先我们那种"几十个按钮
/// 铺满一屏 + 外面再挂一个搜索框"既占地方、又要用户自己找。
/// 搜索收进下拉框里（`SelectState::searchable`）。
///
/// 不需要 `cx`：下拉框的选中由 `Shell` 那边订阅，这里只负责画
/// （不像镜像源那块还要读自定义 URL 输入框）。
pub(crate) fn online_block(
    state: &AppState,
    form: &InstallForm,
    entity: &Entity<Shell>,
) -> AnyElement {
    let refresh = {
        let entity = entity.clone();
        Button::new("online-refresh")
            .label(if state.online.loading {
                "正在拉取…"
            } else {
                "刷新商店清单"
            })
            .small()
            .on_click(move |_, window, cx| {
                entity.update(cx, |shell, cx| shell.refresh_online_list(window, cx));
            })
    };

    // 下拉框：显示 `Ubuntu 24.04 LTS (Ubuntu-24.04)`，取到的是 id。
    // 选中由 `Shell` 那边订阅（`SelectEvent::Confirm`），这里只负责画。
    let picker = Select::new(&form.store_select)
        .placeholder("选一个发行版（可以打字搜索）")
        .w_full()
        .menu_max_h(px(320.));

    // 状态：正在拉 / 失败原因（原文）/ 来源说明
    let status: AnyElement =
        if state.online.loading {
            busy_text_xs("正在拉取在线清单…")
        } else if !state.online.error.trim().is_empty() {
            v_flex()
                .w_full()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::warning())
                        .child(format!("清单有问题：{}", state.online.error)),
                )
                .child(div().text_xs().text_color(theme::text_dim()).child(
                    "拉不到清单也能装：直接在「发行版名」里填在线 id（例如 Ubuntu-24.04）。",
                ))
                .into_any_element()
        } else if !state.online.items.is_empty() {
            let mut lines: Vec<AnyElement> = vec![
                div()
                    .text_xs()
                    .text_color(theme::text_dim())
                    .child(format!(
                        "共 {} 个可装。{}",
                        state.online.items.len(),
                        state.online.source.label()
                    ))
                    .into_any_element(),
            ];
            // ⚠️ 清单是**我们**用 curl 从 CDN 兜底拉到的（不是 wsl 自己拉的）时，
            // 说明 `wsl.exe` 连不上它要的那份 manifest —— 而安装那一步正是走它。
            // 实测（2026-10-10）这种状态下商店与 `--web-download` 会**双双**报
            // `Wsl/InstallDistro/WININET_E_CANNOT_CONNECT`。与其让用户装到一半才失败，
            // 不如现在就说清"这条来源现在多半装不上、该换哪条"。
            //
            // 注意这句话**会过期**（实测同一小时内 `wsl -l -o` 从成功变成 500 又变回来），
            // 所以它必须带上"怎么刷新"的出口，而不是一句判决。
            match state.online.source {
                OnlineListSource::FallbackJson => lines.push(
                    div()
                        .text_xs()
                        .text_color(theme::warning())
                        .child(
                            "注意：这份清单是面板自己从 CDN 拉的，说明 wsl 自己拉不到它要的那份 \
                         manifest —— 现在用「微软商店」装多半会连不上。\
                         先试「在线发行版（镜像源）」（那条不经过 wsl 的网络）；\
                         如果你刚把代理修好，点一下上面的「刷新商店清单」再看这行还在不在。",
                        )
                        .into_any_element(),
                ),
                OnlineListSource::Wsl => lines.push(
                    div()
                        .text_xs()
                        .text_color(theme::success())
                        .child("wsl 自己拉到了清单 —— 这条来源现在是通的。")
                        .into_any_element(),
                ),
                OnlineListSource::Unknown => {}
            }
            // 选中了"不带版本号的家族名"（`Ubuntu`）时提醒一句：实测这类条目在这台机器上
            // **装不动**（`wsl --install -d Ubuntu` 两秒就 `WININET_E_CANNOT_CONNECT`），
            // 装的时候会自动换成带版本的那条（见 `OnlineDistroState::sibling_id`）。
            //
            // ⚠️ 兄弟**已经装过**时不能提这句：那时 `sibling_id` 会给 `None`
            // （换了也只会撞 `ERROR_ALREADY_EXISTS`），说"会自动改用 X"就是假话。
            if let Some(picked) = state.online.selected.as_deref() {
                if let Some(sibling) = state.online.sibling_id(picked, &state.distro_names()) {
                    lines.push(
                        div()
                            .text_xs()
                            .text_color(theme::warning())
                            .child(format!(
                                "「{picked}」不带版本号 —— 实测这类条目在本机装不动\
                             （2 秒就报连接失败），装的时候会自动改用「{sibling}」再试一次。"
                            ))
                            .into_any_element(),
                    );
                }
            }
            v_flex().w_full().gap_1().children(lines).into_any_element()
        } else {
            div()
                .text_xs()
                .text_color(theme::text_dim())
                .child("还没拉过清单 —— 点上面的按钮，或者直接在「发行版名」里手输 id。")
                .into_any_element()
        };

    v_flex()
        .w_full()
        .gap_2()
        .child(
            h_flex()
                .w_full()
                .gap_2()
                .items_center()
                .child(refresh)
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child("选择发行版（微软商店 / Microsoft Store）"),
                ),
        )
        .child(picker)
        .child(status)
        .into_any_element()
}

/// 「在线发行版（镜像源）」那一块：清单（动态拉）+ **下拉框** + 自动测速状态 + 自定义 URL。
///
/// 清单来自 `wslc_core::cmd::catalog`（wslui 的公开接口，见 `mirrors.rs` 的说明），
/// 顺序照参考实现排（Ubuntu / Debian 在最前，见 `sort_offers_like_reference`）。
///
/// # 为什么没有"镜像站列表"了
///
/// 参考实现是**自动测速择优**：用户只选发行版，面板自己并发测一遍、挑最快的那个。
/// 原先我们让用户先点「探测最快镜像」、再对着十几行"清华 TUNA 0.16 秒 / 南大 …"
/// 自己挑 —— 那是把**面板该做的事**推给了用户（用户原话："不需要那些什么清华源
/// 之类的"）。现在测速在**选中发行版时自动开始**，界面上只留一行状态；
/// 镜像站名字进安装日志（出问题时那才是排查的地方）。
pub(crate) fn mirror_block(
    state: &AppState,
    form: &InstallForm,
    entity: &Entity<Shell>,
    cx: &App,
) -> AnyElement {
    let refresh = {
        let entity = entity.clone();
        Button::new("mirror-refresh")
            .label(if state.mirrors.loading {
                "正在拉取…"
            } else {
                "刷新清单"
            })
            .small()
            .on_click(move |_, window, cx| {
                entity.update(cx, |shell, cx| shell.refresh_mirror_catalog(window, cx));
            })
    };

    // 下拉框：显示 `Ubuntu 24.04 (12 镜像源)`，取到的是清单里的 id。
    let picker = Select::new(&form.mirror_select)
        .placeholder("选一个发行版（自动测速择优）")
        .w_full()
        .menu_max_h(px(320.));

    // 状态行：正在拉 / 拉失败（原话）/ 清单概览
    let status: AnyElement = if state.mirrors.loading {
        busy_text_xs("正在拉取发行版清单…")
    } else if let Some(error) = state.mirrors.load_error.as_ref() {
        div()
            .text_xs()
            .text_color(theme::warning())
            .child(format!("清单没拉到：{error}"))
            .into_any_element()
    } else if !state.mirrors.offers.is_empty() {
        div()
            .text_xs()
            .text_color(theme::text_dim())
            .child(format!(
                "共 {} 个可装版本。{}{}",
                state.mirrors.offers.len(),
                state.mirrors.origin,
                state
                    .mirrors
                    .updated
                    .as_ref()
                    .map(|time| format!("（清单更新于 {time}）"))
                    .unwrap_or_default()
            ))
            .into_any_element()
    } else {
        div()
            .text_xs()
            .text_color(theme::text_dim())
            .child("还没拉到清单 —— 点「刷新清单」，或者直接在下面填一个 rootfs URL。")
            .into_any_element()
    };

    // 测速状态：正在测 / 已选好 / 全都拿不到。
    //
    // ⚠️ **不写镜像站名字**（用户要求"不需要那些什么清华源之类的"）：
    // 只给"选好了 + 多大"，名字在安装日志里（`从 <site> 下载 …` 那一步）。
    let probe_line: AnyElement = if state.mirrors.probing {
        busy_text_xs("正在自动测速，挑最快的镜像…")
    } else if let Some(error) = state.mirrors.error.as_ref() {
        div()
            .text_xs()
            .text_color(theme::warning())
            .child(error.clone())
            .into_any_element()
    } else if let Some(choice) = state.mirrors.chosen.as_ref() {
        div()
            .text_xs()
            .text_color(theme::success())
            .child(format!(
                "已自动测速选好镜像（{} 个来源里最快的那个{}）",
                state.mirrors.results.len(),
                choice
                    .bytes
                    .map(|bytes| format!("，{}", mirrors::human_bytes(bytes)))
                    .unwrap_or_default()
            ))
            .into_any_element()
    } else {
        div()
            .text_xs()
            .text_color(theme::text_dim())
            .child("选中发行版后会自动测速挑最快的镜像。也可以在下面直接填 URL。")
            .into_any_element()
    };

    v_flex()
        .w_full()
        .gap_2()
        .child(
            h_flex()
                .w_full()
                .gap_2()
                .items_center()
                .child(refresh)
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child("选择发行版（在线发行版 · 镜像源）"),
                ),
        )
        .child(status)
        .child(picker)
        .child(probe_line)
        .child(form_field(
            "install-mirror-url",
            "自定义下载地址（留空 = 用自动测速挑出来的；.wsl 包和 tar 都支持）",
            &form.mirror_url,
            cx,
            true,
        ))
        .into_any_element()
}
