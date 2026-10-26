//! ⑥ wslc 配置页与 ⑥b WSL 配置（`.wslconfig`）页。
//!
//! 两页共用 [`setting_row`] 那套「一行一个设置项」的画法，所以合在一个模块里。

use super::common::{card, cell_muted_full, disclosure, empty_state, kv, kv_block};
use super::prelude::*;

/// 配置页。
///
/// 当前能力：逐项展示 + 预设值快捷写入 + 恢复默认 + 原始 YAML 查看 +
/// 带时间戳备份的保存 + 用系统编辑器打开。
///
/// 任意文本输入的完整表单需要 GPUI 的 `InputState` / `TextInput`，
/// 属于下一步（见 `docs/PLAN.md` M6）。
pub fn config(state: &AppState, entity: &Entity<Shell>) -> AnyElement {
    let Some(doc) = &state.settings else {
        let message = state
            .settings_error
            .clone()
            .unwrap_or_else(|| "尚未加载配置文件".to_owned());
        return v_flex()
            .w_full()
            .gap_4()
            .child(card(
                "wslc 配置",
                v_flex()
                    .w_full()
                    .gap_3()
                    .child(div().text_sm().text_color(theme::danger()).child(message))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(open_in_editor_button(entity))
                            .child(reload_button(entity)),
                    ),
            ))
            .into_any_element();
    };

    let values = doc.values();
    let dirty = doc.is_dirty();

    let rows: Vec<AnyElement> = SETTING_KEYS
        .iter()
        .map(|key| setting_row(key, values.get(key), entity))
        .collect();

    let path = doc.path().display().to_string();
    let raw = doc.raw().to_owned();

    v_flex()
        .w_full()
        .gap_4()
        .child(card(
            "配置文件",
            v_flex()
                .w_full()
                .gap_3()
                .child(kv("路径", path))
                .child(kv(
                    "状态",
                    if dirty {
                        "有未保存的修改"
                    } else {
                        "已同步"
                    },
                ))
                .child(div().text_xs().text_color(theme::text_dim()).child(
                    "使用内置默认值的配置以注释保存。自定义值请使用系统编辑器；保存前自动备份。",
                ))
                .child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .child(open_in_editor_button(entity))
                        .child(save_button(entity, dirty))
                        .child(reload_button(entity)),
                ),
        ))
        .child(card("配置项", v_flex().w_full().gap_2().children(rows)))
        .child(disclosure(
            UiSection::ConfigRaw,
            "查看原始 YAML",
            state,
            entity,
            // 同 app/：滚动容器必须先有 id。
            v_flex()
                .id("settings-raw-yaml")
                .w_full()
                .max_h(px(420.))
                .overflow_y_scroll()
                .rounded_md()
                .bg(theme::bg())
                .p_3()
                .child(
                    div()
                        .font_family("Consolas")
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child(raw),
                ),
        ))
        .into_any_element()
}

// ---------------------------------------------------------------------------
// ⑥b WSL 配置（.wslconfig）
// ---------------------------------------------------------------------------

/// 「WSL 配置」页 —— `%USERPROFILE%\.wslconfig`。
///
/// # 这一页是**只读**的
///
/// 不做编辑，也不提供"一键校验"。原因是"校验"在这里做不到轻量：
/// 实测 WSL 只在 **VM 启动时**读一次 `.wslconfig` 并报出认不出的键，
/// 想主动触发就得先 `wsl --shutdown` —— 那会打断所有正在跑的发行版。
/// 详见 `wslc_core::wslconfig` 的模块说明。
///
/// 所以这一页只做**不需要跑 WSL 就能做**的部分：把文件摊开、
/// 把实测已知放错文件的键指出来、真要改就交给系统编辑器。
pub fn wsl_config(state: &AppState, entity: &Entity<Shell>) -> AnyElement {
    let info = &state.snapshot.wslconfig;

    let path_text = info
        .path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "（拿不到：USERPROFILE 没有定义）".to_owned());

    let open = {
        let entity = entity.clone();
        Button::new("open-wslconfig")
            .label("用系统默认程序打开")
            .small()
            .primary()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.open_wslconfig(cx));
            })
    };

    // 三种状态说的话完全不同：读到了 / 文件不存在 / 读失败
    let body: AnyElement = if let Some(error) = &info.error {
        v_flex()
            .w_full()
            .gap_2()
            .child(empty_state("读不到这个文件"))
            .child(
                div()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(error.clone()),
            )
            .into_any_element()
    } else if let Some(text) = &info.text {
        v_flex()
            // 滚动容器必须先有 id（同 config 页的说明）
            .id("wslconfig-raw")
            .w_full()
            .max_h(px(420.))
            .overflow_y_scroll()
            .rounded_md()
            .bg(theme::bg())
            .p_3()
            .child(
                div()
                    .font_family("Consolas")
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(text.clone()),
            )
            .into_any_element()
    } else {
        v_flex()
            .w_full()
            .gap_2()
            .child(empty_state("还没有这个文件"))
            .child(div().text_xs().text_color(theme::text_dim()).child(
                "没配过 .wslconfig 是完全正常的状态 —— WSL 会用全部默认值。\
                 想从头配一份就点下面的按钮（会先建一个空文件再打开）。",
            ))
            .into_any_element()
    };

    let misplaced: AnyElement = if info.misplaced.is_empty() {
        div()
            .text_xs()
            .text_color(theme::text_dim())
            .child(if info.text.is_some() {
                "未发现已知的错位键（仅静态检查，不代表完整校验通过）。"
            } else {
                "文件内容不可用，尚未进行静态检查。"
            })
            .into_any_element()
    } else {
        let rows: Vec<AnyElement> = info
            .misplaced
            .iter()
            .map(|m| {
                v_flex()
                    .w_full()
                    .gap_1()
                    .p_3()
                    .rounded_md()
                    .bg(theme::bg())
                    .child(
                        div()
                            .font_family("Consolas")
                            .text_xs()
                            .text_color(theme::text())
                            .child(format!("第 {} 行：{}", m.line, m.text)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::text_muted())
                            // `m.why` 来自 `wslc_core::wslconfig::check`，带 `**…**`
                            // （见那里 `KNOWN_MISPLACED` 的注释）。`m.text` 是原文不动。
                            .child(wslc_panel_core::util::strip_md(&format!(
                                "应该放到 {} —— {}",
                                m.belongs_to, m.why
                            ))),
                    )
                    .into_any_element()
            })
            .collect();

        v_flex()
            .w_full()
            .gap_2()
            .child(
                div()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child("这几行 WSL 明确报过「未知键」，会被忽略 —— 配置看着生效、其实没有。"),
            )
            .children(rows)
            .into_any_element()
    };

    v_flex()
        .w_full()
        .gap_3()
        .child(card(
            "WSL 全局配置",
            v_flex().w_full().gap_2()
                .child(kv_block("位置", path_text))
                .child(div().text_xs().text_color(theme::text_dim())
                    .child("管理 WSL2 虚拟机；发行版配置请到实例详情中编辑。"))
                .child(h_flex().justify_end().child(open)),
        ))
        .child(card("内容", body))
        .child(misplaced)
        .child(div().text_xs().text_color(theme::text_dim())
            .child("配置在 WSL 启动时读取；关闭 WSL 以重新加载会中断正在运行的任务。"))
        .child(disclosure(
            UiSection::WslHelp, "检查范围与生效方式", state, entity,
            v_flex().w_full().gap_2()
                // 这段在**折叠区**里，用户主动点开才看到 —— 所以详细一点是对的。
                // 但原来三段说的是同一件事的三面（检查范围 / 告警时机 / 两个文件），
                // 合成两段更易读：一段讲"这个页面做什么"，一段讲"两个文件别混"。
                .child(div().text_xs().text_color(theme::text_dim())
                    .child("只检查已知的错位键，不是 WSL 的完整键表。WSL 只在虚拟机启动时报告配置告警 —— 读取状态或刷新列表不会触发校验，本页也不会为校验而关闭实例。"))
                .child(div().text_xs().text_color(theme::text_dim())
                    .child("全局配置文件是 Windows 用户目录下的 .wslconfig；发行版内部的 /etc/wsl.conf 管理默认用户、挂载和网络等，两者的键不能混用。")),
        ))
        .into_any_element()
}

/// 单个配置项的一行。
pub(crate) fn setting_row(
    key: &'static SettingKey,
    effective: Option<String>,
    entity: &Entity<Shell>,
) -> AnyElement {
    let is_default = effective.is_none();
    let shown = effective.unwrap_or_else(|| format!("默认（{}）", key.default));

    let choices = if key.kind == SettingKind::Enum {
        key.choices
    } else {
        presets_for(key)
    };
    let actions: AnyElement = if choices.is_empty() && is_default {
        // 没有候选值的路径项用页面唯一的系统编辑器入口修改。
        cell_muted_full(format!("setting-help-{}", key.key), "使用系统编辑器修改")
    } else {
        let shell_entity = entity.clone();
        Button::new(SharedString::from(format!("edit-setting-{}", key.key)))
            .label("修改")
            .tooltip(format!(
                "{}（{}）",
                wslc_panel_core::util::strip_md(key.help),
                key.key
            ))
            .small()
            .dropdown_menu(move |mut menu, _window, _cx| {
                for choice in choices.iter().copied() {
                    let entity = shell_entity.clone();
                    menu = menu.item(PopupMenuItem::new(choice).on_click(move |_, _, cx| {
                        entity.update(cx, |shell, cx| shell.set_setting(key, Some(choice), cx));
                    }));
                }
                if !is_default {
                    if !choices.is_empty() {
                        menu = menu.item(PopupMenuItem::separator());
                    }
                    let entity = shell_entity.clone();
                    menu = menu.item(PopupMenuItem::new("恢复默认值").on_click(move |_, _, cx| {
                        entity.update(cx, |shell, cx| shell.set_setting(key, None, cx));
                    }));
                }
                menu
            })
            .into_any_element()
    };

    let warning: AnyElement = if key.needs_warning {
        div()
            .text_xs()
            .text_color(theme::warning())
            .child("⚠ 修改此项不会迁移已有的容器与镜像，会在新路径下创建一个空会话。")
            .into_any_element()
    } else {
        div().into_any_element()
    };

    v_flex()
        .w_full()
        .gap_1()
        .py_2()
        .border_b_1()
        .border_color(theme::border())
        .child(
            h_flex()
                .w_full()
                .gap_3()
                .child(
                    div()
                        .w(px(150.))
                        .flex_none()
                        .text_sm()
                        .font_semibold()
                        .text_color(theme::text())
                        .child(key.label),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .text_color(if is_default {
                            theme::text_dim()
                        } else {
                            theme::primary()
                        })
                        .overflow_hidden()
                        .truncate()
                        .child(shown),
                )
                .child(actions),
        )
        .child(warning)
        .into_any_element()
}

pub(crate) fn open_in_editor_button(entity: &Entity<Shell>) -> impl IntoElement {
    let entity = entity.clone();
    Button::new("open-settings")
        .label("用系统编辑器打开")
        .primary()
        .on_click(move |_, _, cx| {
            entity.update(cx, |shell, cx| shell.open_settings_in_editor(cx));
        })
}

/// 保存按钮。
///
/// 没有改动时**不返回禁用按钮**，而是一行绿色文字。
/// 原因：gpui-component 的禁用态文字几乎看不清（实机截图确认过），
/// 而"已保存"本身是个**状态**，用文字表达比用灰按钮更准确。
pub(crate) fn save_button(entity: &Entity<Shell>, dirty: bool) -> AnyElement {
    if !dirty {
        return h_flex()
            .items_center()
            .child(
                div()
                    .text_sm()
                    .text_color(theme::success())
                    .child("✓ 已保存"),
            )
            .into_any_element();
    }

    let entity = entity.clone();
    Button::new("save-settings")
        .label("备份并保存")
        .primary()
        .on_click(move |_, _, cx| {
            entity.update(cx, |shell, cx| shell.save_settings(cx));
        })
        .into_any_element()
}

pub(crate) fn reload_button(entity: &Entity<Shell>) -> impl IntoElement {
    let entity = entity.clone();
    Button::new("reload-settings")
        .label("重新加载")
        .on_click(move |_, _, cx| {
            entity.update(cx, |shell, cx| {
                shell.reload_settings(cx);
            });
        })
}
