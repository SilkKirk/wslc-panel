//! ③ 镜像页：表格、行内动作、拉取弹窗与进度。

use super::common::{
    cell_muted, cell_text, cell_text_full, danger_button, disclosure, empty_state, form_field,
    list_count, page_header, table_card, table_row,
};
use super::prelude::*;

/// 镜像页（v0.6.2 合并成一页，v0.6.3 把拉取表单收进弹窗）。
///
/// 页面只有两样东西：一个「拉取镜像…」按钮 + **本地镜像**表。
/// 拉取表单（registry / 仓库名 / tag 下拉框 / 体积 / 拉取）在弹窗里，
/// 见 [`pull_dialog_overlay`] —— 它和这里**是同一个入口**：点这个按钮才会
/// 建表单、才可能拉取（`AGENTS.md` §5「同一个动作只给一个入口」）。
///
/// # 行内为什么**直接**摆两个按钮（v0.9.24 改回来）
///
/// v0.9.22 曾把「创建容器 / 删除」一起收进「更多」菜单，理由是"操作列在窄窗下
/// 放得下"。实机看下来那个理由不成立，而且代价明显：
///
/// - 镜像行只有**两个**动作，套一层菜单等于让每个动作多点一次；
/// - 「创建容器」是镜像页最常用的一件事（拉完镜像的下一步就是它），
///   藏进菜单后用户会以为没有这个功能。
///
/// 所以 v0.9.24 改回**直接摆**。和容器页的区别在于动作**数量**：
/// 容器行最多 4 个动作（还含危险操作），镜像行 2 个。`AGENTS.md` §5 那条
/// 「超过 4 个就收进更多」针对的是前者 —— 别把这条规则套到只有两个按钮的行上。
///
/// ⚠️ 这一段原先写的是"行内只留「更多」，两个都进菜单"，而实现一直是两个
/// 直接按钮 —— 注释和实现相反，正是 `AGENTS.md` §5 警告过的那种"后来的人
/// 会照着注释继续加重复"的源头。
pub(crate) fn images_page(state: &AppState, entity: &Entity<Shell>) -> AnyElement {
    let rows: Vec<TableRow> = state
        .snapshot
        .images
        .iter()
        .enumerate()
        .map(|(ix, img)| {
            let reference = img.reference();
            table_row(
                IMAGE_COLUMNS,
                vec![
                    cell_text_full(reference.clone(), reference.clone()),
                    cell_muted(img.size.clone()),
                    image_usage_cell(img),
                    cell_muted(img.created_since.clone()),
                    image_row_actions(ix, &reference, entity),
                ],
            )
        })
        .collect();

    let open_pull = {
        let entity = entity.clone();
        // 刻意**不**用 `.disabled()`：全项目都不导入 `Disableable`
        // （禁用态的文字几乎看不清，实机截图确认过）。重复点击由
        // `open_pull_dialog` 里的守卫 + 提示条处理。
        Button::new("open-pull-dialog")
            .label("拉取镜像…")
            .primary()
            .small()
            .on_click(move |_, window, cx| {
                // 表单里的 `InputState` / `SelectState` 只能在有 window 的地方创建，
                // 这里正好有 —— 所以表单是**点开弹窗那一刻**才建的。
                entity.update(cx, |shell, cx| shell.open_pull_dialog(window, cx));
            })
    };

    // 拉取中时这一行说明"在忙什么"：进度本身在弹窗里（而且那个弹窗关不掉），
    // 这一行只是让页面本身也看得出它在忙。
    //
    // 没在拉取时顺带报"记录数 / 不同镜像数 / 合计体积"—— 同一 ID 的多条引用
    // 只算一次，否则体积会翻倍（见 `Snapshot::image_totals`）。
    let status = match &state.pulling {
        Some(progress) => format!("正在拉取 {} …", progress.image),
        None => {
            let (distinct, total) = state.snapshot.image_totals();
            // `if/else` 比 `(cond).then(...).unwrap_or_default()` 直白
            // （clippy 的 `unnecessary_lazy_evaluations` 一类建议）。
            let total_part = if total > 0 {
                format!(" · 合计 {}", format_bytes(total))
            } else {
                String::new()
            };
            format!(
                "共 {} 条记录（{} 个镜像）{}",
                state.snapshot.images.len(),
                distinct,
                total_part
            )
        }
    };

    let body = table_card(
        IMAGE_COLUMNS,
        rows,
        Some(empty_state("没有本地镜像").into_any_element()),
    );

    v_flex()
        .w_full()
        .gap_3()
        .child(page_header(open_pull, list_count(status)))
        .child(body)
        .into_any_element()
}

/// 镜像「使用中」格。
///
/// `Containers` 字段缺失（空串）时显示 `—`，**不能**显示「未使用」——
/// "读不到"和"确实是 0"是两件事，后者才允许说这个镜像没被用。
pub(crate) fn image_usage_cell(image: &ImageListItem) -> AnyElement {
    if image.containers.trim().is_empty() {
        return cell_muted("—");
    }
    match image.container_count() {
        0 => div()
            .text_sm()
            .text_color(theme::text_muted())
            .child("未使用")
            .into_any_element(),
        count => cell_text(format!("{count} 个容器")),
    }
}

/// 镜像行末：**直接**给「创建容器 / 删除」两个按钮。
///
/// 只有两个动作，而且是这一行专属的入口；再套一层「更多」只会让用户
/// 多点一次。容器列表行不一样（那里最多 4 个动作），「更多」只留那处。
pub(crate) fn image_row_actions(ix: usize, reference: &str, entity: &Entity<Shell>) -> AnyElement {
    let create_ref = reference.to_owned();
    let remove_ref = reference.to_owned();
    let entity_for_create = entity.clone();

    let create = Button::new(SharedString::from(format!("image-create-{ix}")))
        .label("创建容器")
        .tooltip("用这个镜像创建容器")
        .small()
        .on_click(move |_, window, cx| {
            let image = create_ref.clone();
            entity_for_create.update(cx, |shell, cx| shell.use_pulled_image(image, window, cx));
        });

    h_flex()
        .w_full()
        .gap_2()
        .flex_wrap()
        .child(create)
        .child(danger_button(
            &format!("remove-image-{ix}"),
            "删除",
            PendingAction::RemoveImage(remove_ref),
            entity,
        ))
        .into_any_element()
}

/// 「拉取镜像」对话框的**内容**（标题 / 底部关闭由 `Dialog` 负责）。
///
/// 两种形态：没在拉取时是**表单**（[`pull_dialog_form`]），拉取中是**进度**
/// （[`pull_progress_body`]，里面自带「取消拉取」）。
pub(crate) fn pull_dialog_body(
    form: &ImageForm,
    state: &AppState,
    entity: &Entity<Shell>,
    cx: &App,
) -> AnyElement {
    match &state.pulling {
        Some(progress) => pull_progress_body(progress, entity),
        None => pull_dialog_form(form, state, entity, cx),
    }
}

/// 弹窗里那张**表单**（没在拉取时）。
///
/// 拆成独立函数只是为了让 [`pull_dialog_overlay`] 的两种形态一眼看得清。
pub(crate) fn pull_dialog_form(
    form: &ImageForm,
    state: &AppState,
    entity: &Entity<Shell>,
    cx: &App,
) -> AnyElement {
    let page = &state.registry;
    let typed_host = form.host.read(cx).value().trim().to_owned();
    let typed_repository = form.repository.read(cx).value().trim().to_owned();
    let normalized_host = registry::normalize_host(&typed_host);
    let shown_host = if normalized_host.is_empty() {
        registry::DEFAULT_REGISTRY
    } else {
        normalized_host.as_str()
    };
    let inputs_match = page.matches_inputs(&typed_host, &typed_repository);

    let presets = {
        let entity = entity.clone();
        Button::new("image-presets")
            .label("常用镜像")
            .small()
            .dropdown_menu(move |mut menu, _, _| {
                for &name in registry::COMMON_IMAGES {
                    let entity = entity.clone();
                    menu = menu.item(PopupMenuItem::new(name).on_click(move |_, window, cx| {
                        entity.update(cx, |shell, cx| {
                            shell.apply_registry_preset(name, window, cx)
                        });
                    }));
                }
                menu
            })
    };
    let query = {
        let entity = entity.clone();
        Button::new("image-query")
            .label(if page.loading {
                "查询中…"
            } else {
                "查询"
            })
            .primary()
            .small()
            .on_click(move |_, window, cx| {
                entity.update(cx, |shell, cx| shell.query_registry(window, cx));
            })
    };
    let source = disclosure(
        UiSection::PullSource,
        format!("更换镜像源 · {shown_host}"),
        state,
        entity,
        v_flex()
            .w_full()
            .gap_1()
            .child(form_field("image-host", "镜像源地址", &form.host, cx, true))
            .child(
                div()
                    .text_xs()
                    .text_color(theme::text_dim())
                    .child("留空使用默认镜像源；修改后重新查询。"),
            ),
    );

    // 编辑后的输入不再对应旧 tag 列表：保留表单，隐藏旧结果直到再次查询。
    let status: Option<AnyElement> = if !inputs_match && !typed_repository.is_empty() {
        Some(
            div()
                .text_xs()
                .text_color(theme::text_dim())
                .child("输入已修改，请重新查询。")
                .into_any_element(),
        )
    } else if !inputs_match || page.loading {
        None
    } else if let Some(error) = page.error.as_ref() {
        Some(
            v_flex()
                .w_full()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .text_color(theme::danger())
                        .child(error.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_dim())
                        .child("tag 列表不可用时，可在名称中指定 :tag 后尝试拉取。"),
                )
                .into_any_element(),
        )
    } else if page.loaded.is_some() {
        let mut text = format!("{} 个 tag", page.tags.len());
        if let Some(note) = page.normalization_note() {
            text.push_str(&format!(" · {note}"));
        }
        if let Some(tag) = page
            .effective_tag()
            .filter(|tag| !page.tags.iter().any(|t| t.as_str() == *tag))
        {
            text.push_str(&format!(" · {tag} 不在列表中，仍可尝试拉取"));
        }
        Some(
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child(text)
                .into_any_element(),
        )
    } else {
        None
    };

    let tag_picker = (inputs_match
        && !page.loading
        && (!page.tags.is_empty() || page.effective_tag().is_some()))
    .then(|| {
        v_flex()
            .w_full()
            .gap_1()
            .child(div().text_xs().text_color(theme::text_dim()).child("tag"))
            .child(
                Select::new(&form.tags_select)
                    .placeholder("选择或搜索 tag")
                    .w_full()
                    .menu_max_h(px(360.)),
            )
            .into_any_element()
    });
    let reference = if inputs_match {
        page.selected_reference()
    } else {
        None
    };
    let action = reference.map(|reference| {
        let pulled = state
            .snapshot
            .images
            .iter()
            .any(|image| image.reference() == reference);
        let size_line = if page.sizing {
            Some(cell_muted("正在获取下载体积…"))
        } else if let Some(label) = page.size_label() {
            // manifest 是压缩后的下载体积，与本地镜像占用不同。
            Some(cell_muted(label))
        } else {
            page.size_error.as_ref().map(|error| {
                div()
                    .text_xs()
                    .text_color(theme::warning())
                    .child(format!("无法获取下载体积：{error}"))
                    .into_any_element()
            })
        };
        let pull = {
            let entity = entity.clone();
            Button::new("image-pull")
                .label("拉取")
                .primary()
                .small()
                .on_click(move |_, _, cx| {
                    entity.update(cx, |shell, cx| shell.pull_registry_selection(cx));
                })
        };
        let create = pulled.then(|| {
            let entity = entity.clone();
            let image = reference.clone();
            Button::new("image-create")
                .label("创建容器")
                .small()
                .on_click(move |_, window, cx| {
                    let image = image.clone();
                    entity.update(cx, |shell, cx| shell.use_pulled_image(image, window, cx));
                })
                .into_any_element()
        });
        v_flex()
            .w_full()
            .gap_2()
            .child(cell_text_full(
                "pull-selected-reference",
                format!("将拉取 {reference}"),
            ))
            .child(
                h_flex()
                    .w_full()
                    .gap_3()
                    .items_center()
                    .flex_wrap()
                    .children(size_line)
                    .child(pull)
                    .children(create),
            )
            .into_any_element()
    });

    v_flex()
        .w_full()
        .gap_3()
        .child(
            h_flex()
                .w_full()
                .gap_2()
                .items_end()
                .child(div().flex_1().min_w_0().child(form_field(
                    "image-repository",
                    "镜像名称或完整引用（如 nginx:1.27）",
                    &form.repository,
                    cx,
                    true,
                )))
                .child(query)
                .child(presets),
        )
        .child(source)
        .children(tag_picker)
        .children(status)
        .children(action)
        .into_any_element()
}

/// 拉取进度（v0.6.2 之后内联在「镜像」页的「正在拉取」卡片里）。
///
/// 只渲染最后 [`PROGRESS_TAIL_LINES`] 行、且**不滚动** ——
/// 拉取的输出动辄上千行，滚动容器会把用户带到最老的那几行，
/// 而这里永远显示"刚刚发生了什么"。
///
/// 它**不是浮层**：页面本身就是"正在拉"的上下文，再盖一层没有额外信息
/// （v0.6.2 之前「拉取镜像」弹窗的进度形态也复用了它，那个弹窗已经删了）。
pub(crate) fn pull_progress_body(progress: &PullProgress, entity: &Entity<Shell>) -> AnyElement {
    /// 进度区显示多少行。
    const PROGRESS_TAIL_LINES: usize = 12;

    let tail: Vec<AnyElement> = progress
        .lines
        .iter()
        .skip(progress.lines.len().saturating_sub(PROGRESS_TAIL_LINES))
        .map(|line| {
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child(line.clone())
                .into_any_element()
        })
        .collect();

    let body = if tail.is_empty() {
        vec![
            div()
                .text_xs()
                .text_color(theme::text_dim())
                .child("等待输出…")
                .into_any_element(),
        ]
    } else {
        tail
    };

    let abort = {
        let entity = entity.clone();
        Button::new("pull-abort")
            .label("取消拉取")
            .small()
            .danger()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.cancel_pull(cx));
            })
    };

    v_flex()
        .w_full()
        .gap_3()
        .child(
            div()
                .text_sm()
                .text_color(theme::text())
                .child(format!("正在拉取 {} ……", progress.image)),
        )
        .child(
            v_flex()
                .w_full()
                .gap_1()
                .rounded_md()
                .bg(theme::bg())
                .p_3()
                .children(body),
        )
        .child(
            h_flex()
                .w_full()
                .justify_between()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_dim())
                        .child(format!("已收到 {} 行输出", progress.lines.len())),
                )
                .child(abort),
        )
        .into_any_element()
}
