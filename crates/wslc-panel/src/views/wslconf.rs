//! ⑧ 发行版配置（`/etc/wsl.conf`）浮层。

use super::common::{badge, cell_muted_full};
use super::prelude::*;

/// 「这一项显示的是 WSL 的默认值」的小标记。
///
/// 参考项目没有这个 —— 它的 UI 把每个字段都 `unwrap_or(默认值)` 再写回，
/// 用户根本分不清"我看到的是默认"还是"明确设过"。
pub(crate) fn default_tag(explicit: bool) -> Option<AnyElement> {
    if explicit {
        return None;
    }
    Some(badge("默认", theme::text_dim(), theme::bg()).into_any_element())
}

/// 表单里的一行。
///
/// 不需要 `cx`：`Input::new(input)` / `Checkbox::new(id)` 都只吃 `Entity`，
/// 不用 `App`。（原先它多收一个 `cx` 却在函数里写着 `let _ = cx;` ——
/// 那是"我其实不需要它"的标记，一并去掉了。）
pub(crate) fn wslconf_row(
    state: &WslConfState,
    dialog: &WslConfDialog,
    field: &'static wslconf::Field,
    entity: &Entity<Shell>,
) -> AnyElement {
    let explicit = state.doc.is_explicit(field.section, field.key);

    // 只读字段（目前只有 `[boot] systemd`）：**不画勾选框**。
    //
    // 项目约定是不导入 `Disableable`、全项目不用 `.disabled()`；而且一个
    // 灰掉的勾选框会让人以为"能点，只是暂时不能"。直接显示当前值 + 说明
    // 更诚实 —— 这一项我们只负责**原样写回**，不负责改。
    if field.read_only {
        return v_flex()
            .w_full()
            .gap_1()
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme::text_muted())
                            .child(field.label),
                    )
                    .child(badge("只读", theme::text_dim(), theme::bg())),
            )
            .child(
                div()
                    .font_family("Consolas")
                    .text_xs()
                    .text_color(theme::text())
                    .child(format!(
                        "{} = {}",
                        field.key,
                        state.doc.effective(field.section, field.key)
                    )),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme::text_dim())
                    .child(wslc_panel_core::util::strip_md(field.hint)),
            )
            .into_any_element();
    }

    match field.kind {
        wslconf::FieldKind::Bool => {
            let entity = entity.clone();
            Checkbox::new(wslc_panel_core::state::wslconf_field_id(
                field.section,
                field.key,
            ))
            .label(field.label)
            .checked(state.doc.effective_bool(field.section, field.key))
            // `Checkbox` 是**受控**组件：`on_change` 给的是"请求的新值"，
            // 由我们存下来再 notify（见 gpui-kit 的 checkbox 文档）。
            .on_change(move |checked, _, cx| {
                let value = *checked;
                entity.update(cx, |shell, cx| {
                    shell.set_wslconf_bool(field.section, field.key, value, cx);
                });
            })
            .into_any_element()
        }
        wslconf::FieldKind::Text => {
            let mut row = v_flex().w_full().gap_1().child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::text_dim())
                            .child(field.label),
                    )
                    .children(default_tag(explicit)),
            );

            if let Some(input) = dialog.inputs.get(&(field.section, field.key)) {
                row = row.child(
                    Input::new(input)
                        .id(wslc_panel_core::state::wslconf_field_id(
                            field.section,
                            field.key,
                        ))
                        .w_full(),
                );
            }

            if !field.hint.is_empty() {
                row = row.child(
                    div()
                        .text_xs()
                        .text_color(theme::text_dim())
                        .child(wslc_panel_core::util::strip_md(field.hint)),
                );
            }
            row.into_any_element()
        }
    }
}

/// 「发行版配置（`/etc/wsl.conf`）」弹窗。
///
/// 表单是**表驱动**的：遍历 [`wslconf::SECTIONS`] / [`wslconf::FIELDS`] 画出来，
/// 所以加字段不用改这里 —— 改 `wslc-core` 的那两张表就够了。
pub(crate) fn wslconf_dialog_body(shell: &Shell, entity: &Entity<Shell>, cx: &App) -> AnyElement {
    let (Some(saved_state), Some(dialog)) = (
        shell.state.wslconf.as_ref(),
        shell.forms.wslconf_dialog.as_ref(),
    ) else {
        return div().into_any_element();
    };

    // 预览和摘要读取当前输入；克隆上应用草稿，不让渲染改变保存状态。
    let mut draft = saved_state.clone();
    for ((section, key), input) in &dialog.inputs {
        draft.apply_text_value(section, key, input.read(cx).value().trim());
    }
    let state = &draft;

    // -- 表单正文：按节分组 --
    let mut sections: Vec<AnyElement> = Vec::new();
    for section in wslconf::SECTIONS {
        // 版本门控：这一节要求的 WSL 版本够不够。
        // 版本读不出来时 `section_supported` 返回 true —— 和参考项目相反，
        // 理由见 `wslc_core::model::wslconf::section_supported`。
        if !wslconf::section_supported(section.name, &state.wsl_version) {
            continue;
        }

        // 注意这里**不能**加 `.copied()`：`FIELDS.iter()` 已经是 `&Field`，
        // 再 `copied()` 会得到 `Field`（值），和 `Vec<&Field>` 对不上。
        // 下面那两处 `fields.iter().copied()` 才对 —— 那里的 Item 是 `&&Field`。
        let fields: Vec<&'static wslconf::Field> = wslconf::FIELDS
            .iter()
            .filter(|f| f.section == section.name)
            .collect();
        if fields.is_empty() {
            continue;
        }

        // 布尔项两两一行，在分组内共享可用宽度，文本项单独占行。
        let bool_fields: Vec<&'static wslconf::Field> = fields
            .iter()
            .copied()
            .filter(|f| f.kind == wslconf::FieldKind::Bool && !f.read_only)
            .collect();
        let other_fields: Vec<&'static wslconf::Field> = fields
            .iter()
            .copied()
            .filter(|f| f.kind != wslconf::FieldKind::Bool || f.read_only)
            .collect();

        let bool_rows: Vec<AnyElement> = bool_fields
            .chunks(2)
            .map(|pair| {
                let cells: Vec<AnyElement> = pair
                    .iter()
                    .map(|f| {
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(wslconf_row(state, dialog, f, entity))
                            .into_any_element()
                    })
                    .collect();
                h_flex().w_full().gap_4().children(cells).into_any_element()
            })
            .collect();

        let entity_toggle = entity.clone();
        let name = section.name;
        let open = state.section_open(name);
        // 标题里塞"节名 + 摘要"；展开箭头由官方 `AccordionItem` 的 trigger 提供
        // —— 不再用 `▾` / `▸` 这两个**字体字符**（不同字体下基线不一样，
        // 也不跟"减弱动态效果"的偏好走，见 `common::disclosure` 的注释）。
        let title = h_flex()
            .w_full()
            .gap_2()
            .items_center()
            .child(div().text_sm().child(section.label))
            .child(div().flex_1().min_w_0().child(cell_muted_full(
                format!("wslconf-summary-{name}"),
                state.section_summary(name),
            )));
        sections.push(
            Accordion::new(SharedString::from(format!("wslconf-section-{name}")))
                // 三处覆盖官方默认的理由同 `common::disclosure`：
                // 折叠区在弹窗卡里，不要官方那圈"整块卡片"的边框；
                // `Accordion::render` 写死了 `.size_full()`，`h_full` 会撑高；
                // 底色用卡片色而不是 `tokens.accordion`。
                .bordered(false)
                .h_auto()
                .item(move |item| {
                    item.title(title).open(open).bg(theme::bg_card()).child(
                        v_flex().w_full().gap_2().children(bool_rows).children(
                            other_fields
                                .iter()
                                .map(|f| wslconf_row(state, dialog, f, entity)),
                        ),
                    )
                })
                // 幂等判断：容器级回调在"点内容区"时也可能被触发，
                // 所以只在官方报的状态和我们的**不一致**时才翻（同 `common::disclosure`）。
                .on_toggle_click(move |open_ix, _, cx| {
                    let now_open = !open_ix.is_empty();
                    entity_toggle.update(cx, |shell, cx| {
                        let Some(state) = shell.state.wslconf.as_mut() else {
                            return;
                        };
                        if state.section_open(name) != now_open {
                            state.toggle_section(name);
                            cx.notify();
                        }
                    });
                })
                .into_any_element(),
        );
    }

    // -- 校验错误（用户不存在 / 启动命令不存在）--
    let errors: Vec<AnyElement> = state
        .errors
        .iter()
        .map(|e| {
            div()
                .text_xs()
                .text_color(theme::danger())
                .child(format!("⚠️ {e}"))
                .into_any_element()
        })
        .collect();

    // -- 预览：实际会写进去的内容 --
    let preview: Option<AnyElement> = if state.show_preview {
        Some(
            v_flex()
                // 滚动容器必须先有 id
                .id("wslconf-preview")
                .w_full()
                .max_h(px(220.))
                .overflow_y_scroll()
                .rounded_md()
                .bg(theme::bg())
                .p_3()
                .child(
                    div()
                        .font_family("Consolas")
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child(state.doc.render()),
                )
                .into_any_element(),
        )
    } else {
        None
    };

    let preview_btn = {
        let entity = entity.clone();
        Button::new("wslconf-preview")
            .label(if state.show_preview {
                "隐藏预览"
            } else {
                "预览内容"
            })
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.toggle_wslconf_preview(cx));
            })
    };
    let cancel = {
        let entity = entity.clone();
        Button::new("wslconf-cancel")
            .label("取消")
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.close_wslconf(cx));
            })
    };
    let save = {
        let entity = entity.clone();
        Button::new("wslconf-save")
            .label("保存")
            .small()
            .primary()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.save_wslconf(false, cx));
            })
    };
    let save_restart = {
        let entity = entity.clone();
        Button::new("wslconf-save-restart")
            .label("保存并重启发行版")
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.save_wslconf(true, cx));
            })
    };

    // 保存中就把两个保存按钮**换成一行字**。
    //
    // `save_wslconf` 要串起 `user_exists` + `path_exists`（两条 `wsl.exe`）
    // + 写 `/etc/wsl.conf` + 可选的 terminate，秒级到十几秒。原先这段时间
    // 弹窗一点变化都没有，两个按钮也照样能点 —— 连点会并发出两轮 `wsl.exe`，
    // 重复写一遍配置、再 terminate 一次发行版。
    //
    // 项目约定**不用** `.disabled()`（禁用态的文字几乎看不清，见 §7.5），
    // 所以这里是换掉元素、不是置灰。
    let save_actions: Vec<AnyElement> = if state.busy {
        vec![
            div()
                .text_sm()
                .text_color(theme::text_muted())
                .child("正在保存…（要读发行版状态并写文件，可能要十几秒）")
                .into_any_element(),
        ]
    } else {
        vec![save.into_any_element(), save_restart.into_any_element()]
    };

    v_flex()
        .w_full()
        .gap_3()
        // ⚠️ 标题**不在这里画** —— `Dialog::title()` 已经画过一次了。
        // 早期版本的浮层是自绘的（`modal_scrim` + `modal_card`），标题在 body 里；
        // 迁移时忘了删，实机就是同一个标题连着出现两遍（v0.20.0 报过来的）。
        .child(
            v_flex()
                .gap_1()
                .child(div().text_xs().text_color(theme::text_dim()).child(format!(
                    "{}　·　保存前会先备份到 /etc/wsl.conf.bak；\
                             注释和本程序不认识的键都会原样保留。",
                    if state.wsl_version.is_empty() {
                        "WSL 版本未知".to_owned()
                    } else {
                        format!("WSL {}", state.wsl_version)
                    }
                ))),
        )
        .child(
            v_flex()
                .id("wslconf-body")
                .w_full()
                .min_h_0()
                .max_h(px(420.))
                .overflow_y_scroll()
                .gap_3()
                .children(errors)
                .children(sections)
                .children(preview),
        )
        .child(
            v_flex()
                .w_full()
                .flex_none()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_dim())
                        .child("仅保存需下次启动才生效；重启会中断此发行版中正在运行的任务。"),
                )
                .child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .flex_wrap()
                        .justify_end()
                        .child(preview_btn)
                        .child(cancel)
                        .children(save_actions),
                ),
        )
        .into_any_element()
}
