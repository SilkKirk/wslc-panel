//! ② 容器页：工具栏、表格、行内动作与「更多」菜单。

use super::common::{
    badge, cell_badge, cell_muted, cell_muted_full, cell_text, danger_button, immediate_button,
    name_link, overlay_button, page_empty, table_card, table_row,
};
use super::prelude::*;

/// 资源使用率：CPU 与内存**上下两行**（照 1Panel 的排版）。
///
/// 已停止的容器没有 stats，显示 `—`。
pub(crate) fn cell_resources(summary: &ContainerSummary) -> AnyElement {
    match summary.stats.as_ref() {
        None => cell_muted("—"),
        Some(stats) => v_flex()
            .gap_1()
            .child(
                div()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(format!("CPU {}", stats.cpu_perc)),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(stats.mem_usage.clone()),
            )
            .into_any_element(),
    }
}

/// 「状态」格：状态徽标 + 非健康态徽标 + 一行 `Status`（`Up 2 hours`）。
///
/// 为什么第二行是 `Status` 而不是再算一次 uptime：`RunningFor` 实测是
/// **创建至今**，和 `Status` 里的 `Up 2 hours` 不是一回事（见容器详情里
/// 「已创建」那一段的说明）。列表里要给用户看的是"它现在怎么样"，
/// 所以直接用 `wslc` 给的 `Status` 文案，不再自己算。
///
/// `healthy` 刻意**不画**：那是默认态，每行都挂一个绿色「健康」只会让
/// 真正异常的橙/红徽标被淹没。
pub(crate) fn cell_container_status(summary: &ContainerSummary) -> AnyElement {
    let item = &summary.item;
    let health = item.health_status.trim();

    let mut badges: Vec<AnyElement> = vec![cell_badge(item.state_kind())];
    if !health.is_empty() && !health.eq_ignore_ascii_case("healthy") {
        let (fg, bg) = if health.eq_ignore_ascii_case("starting") {
            (theme::warning(), theme::warning_soft())
        } else {
            (theme::danger(), theme::danger_soft())
        };
        badges.push(badge(health.to_owned(), fg, bg).into_any_element());
    }

    let status = item.status.trim();

    v_flex()
        .gap_1()
        // `flex_wrap`：状态 + 健康两枚徽标在窄列里宁可换行，也别溢出到
        // 隔壁「资源」列（`table_row` 会裁，但裁掉的是右边那一半）。
        .child(h_flex().gap_1().flex_wrap().children(badges))
        .children((!status.is_empty()).then(|| {
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .overflow_hidden()
                .truncate()
                .child(status.to_owned())
        }))
        .into_any_element()
}

/// 容器名（蓝色链接，点开看详情）。
///
/// 照 1Panel 的做法：名字就是入口，表格里不再挤一列操作按钮。
/// 挂载这类占地方的信息全部放进详情弹窗。
///
/// 实际画法在 [`common::name_link`]（实例页那个链接和它**逐行相同**，
/// 所以合到一处了；悬停/按下为什么要手写、id 为什么必须给，都写在那儿）。
pub(crate) fn container_name_link(name: &str, entity: &Entity<Shell>) -> AnyElement {
    name_link("detail", name, entity, |shell, target, cx| {
        shell.open_detail(target, cx)
    })
}

/// 行内操作：**只留最高频的那几个**，其余收进「更多」下拉菜单。
///
/// # 这一格现在是 `[数据库] [部署管理] [启动/停止] [更多]`（v0.9.14 改）
///
/// 之前它会把 7 个按钮全铺出来（终端 / 数据库 / 部署管理 / 重启 / 停止 /
/// 日志 / 删除），于是这一列在 1707 逻辑宽下被挤成两行、还要靠权重硬撑。
/// 现在行内只留：**这一类容器专属的入口**（数据库 / 部署管理）+ **最高频的
/// 状态切换**（启动 / 停止），其余（终端 / 重启 / 日志 / 删除）进「更多」。
///
/// 取舍的理由：
/// - 「数据库 / 部署管理」是**只能从这一行进**的两个页面（都不在侧边栏里）——
///   藏进菜单等于把唯一入口埋起来，所以它们必须在行内；
/// - 「启动 / 停止」是这一页最常点的动作，也是唯一一个"看着状态点"的动作；
/// - 「终端 / 重启 / 日志 / 删除」都是**低频或危险**的：删除有二次确认、
///   日志是排查时才看、终端只在运行中才有意义 —— 它们放菜单里正好。
///
/// ⚠️ 菜单里的项**只画当前有意义的**（停止的容器不画「终端」：`wslc exec`
/// 在它上面必然失败，那种"点了必然报错"的项不如不画）。
///
/// MySQL / MariaDB 容器多一个「数据库」按钮（v0.7.0）：它是那一类容器上
/// 最高频的动作，而且**只能从这里进**（数据库管理页不在侧边栏里）。
pub(crate) fn container_row_actions(
    item: &ContainerListItem,
    entity: &Entity<Shell>,
) -> AnyElement {
    let name = item.display_name();
    let running = item.is_running();
    let mut actions: Vec<AnyElement> = Vec::new();

    if running {
        // 「数据库」：只有**认得出是 MySQL 家族**的容器才有。
        //
        // 判定是**纯字符串**（镜像名 / 命令 / 3306 端口，见
        // `mysql_container_kind`）—— 渲染每帧都跑，绝不能在这里起进程。
        // 认不出来就什么都不画（和 `image_preset` 同一条原则：
        // 宁可少一个按钮，也不要给一个点进去全是报错的按钮）。
        //
        // 同样的理由省略了「停止的容器」那一支：页面里每一格都要
        // `wslc exec`，容器没跑就只有 `WSLC_E_CONTAINER_NOT_RUNNING`。
        // 先在上面那行点「启动」，按钮自然会出来。
        // v0.14：除了 MySQL 家族，**PostgreSQL** 容器也给这个按钮。
        //
        // 判定仍然是**纯字符串**（镜像名 / 命令）—— 渲染每帧都跑，
        // 绝不能在这里起进程。认不出来就什么都不画（和 `image_preset`
        // 同一条原则：宁可少一个按钮，也不要给一个点进去全是报错的按钮）。
        let has_db_page = mysql_model::mysql_container_kind(item).is_some()
            || wslc_core::db::guess_kind(&wslc_core::db::ConnHint {
                image: item.image.as_str(),
                command: item.command.as_str(),
                container_ports: &[],
            })
            .is_some();
        if has_db_page {
            let target = name.to_owned();
            let image = item.image.clone();
            actions.push(overlay_button(
                &format!("db-{name}"),
                // 依据（镜像名 / 命令 / 3306 端口）不进按钮文案 ——
                // 数据库页上写着，这里是清单，只放得下动作。
                "数据库",
                move |shell, window, cx| {
                    shell.open_database_page(target.clone(), image.clone(), window, cx)
                },
                entity,
            ));
        }

        // 「部署管理」（v0.9）：认得出是 nginx / Java / Node.js / Go 的容器才有。
        //
        // 判定同样是**纯字符串**（镜像名 / 启动命令，见 `container_app_kind`）
        // —— 渲染每帧都跑，绝不能在这里起进程。认不出来就什么都不画。
        //
        // ⚠️ 它和「数据库」会**同时**出现在一个既像 MySQL 又像这几类的容器上
        // （比如一个镜像名认不出来、命令里同时有 `mysqld` 和 `node` 的怪东西）。
        // 那两个按钮各自进去都会给出权威判定并说明"看起来不是" —— 这个代价
        //（多一个按钮）比"漏掉一个"小，和 v0.7 接受 3306 误判是同一条取舍。
        if let Some(candidate) = deploy_model::container_app_kind(item) {
            let target = name.to_owned();
            let image = item.image.clone();
            let runtime = candidate.runtime;
            let signal = candidate.signal;
            actions.push(overlay_button(
                &format!("deploy-{name}"),
                // 运行时名也不进按钮文案 —— 页面顶上写着"Nginx · 按镜像名判断"。
                "部署管理",
                move |shell, _, cx| {
                    shell.open_deploy_page(target.clone(), image.clone(), runtime, signal, cx)
                },
                entity,
            ));
        }
    }

    // 「启动 / 停止」：行内**唯一**的通用动作（不分容器类型，只看状态）。
    if running {
        actions.push(
            danger_button(
                &format!("stop-{name}"),
                "停止",
                PendingAction::StopContainer(name.to_owned()),
                entity,
            )
            .into_any_element(),
        );
    } else {
        actions.push(
            immediate_button(
                &format!("start-{name}"),
                "启动",
                ImmediateAction::StartContainer(name.to_owned()),
                entity,
            )
            .into_any_element(),
        );
    }

    // 「更多」放在**最后**：它收的是低频 / 危险动作，顺序上也让"主要动作在左"
    // 这条读起来成立（和参考实现的 `终端 / 日志 / 更多` 同一个形状）。
    actions.push(container_more_menu(item, entity));

    h_flex()
        .gap_2()
        .flex_wrap()
        .children(actions)
        .into_any_element()
}

/// 行末那颗「更多」：低频 / 危险动作都收在这里。
///
/// # 菜单里放什么（只画当前有意义的）
///
/// | 什么时候 | 菜单里 |
/// |---|---|
/// | 运行中 | 终端 / 重启 / 日志 / 删除 |
/// | 已停止 | 日志 / 删除 |
///
/// - 「终端」只有运行中才画：`wslc exec` 在停止的容器上必然失败；
/// - 「日志」**不分运行状态**：`wslc logs` 读的是容器留下的日志，
///   停掉的容器照样有东西可看（排查"它为什么退了"正是看这个）；
/// - 「删除」放最后，前面加一条分隔线 —— 它是唯一不可撤销的那个。
///
/// ⚠️ 菜单项**不用** `.disabled()`（全项目不用，见文件头的说明），
/// 也不做红色高亮：0.7.1 的 `PopupMenuItem` 没有危险态样式，
/// 而这本来就该由二次确认（`PendingAction`）来兜底，而不是靠颜色。
///
/// ⚠️ 回调是 `'static` 的（菜单可能比这一帧活得久）：所以名字 / `Entity`
/// 都要 `move` 进去，不能借 `item`。
pub(crate) fn container_more_menu(item: &ContainerListItem, entity: &Entity<Shell>) -> AnyElement {
    let name = item.display_name().to_owned();
    let running = item.is_running();
    let entity = entity.clone();

    Button::new(SharedString::from(format!("more-{name}")))
        .label("更多")
        .small()
        .dropdown_menu(move |mut menu, _window, _cx| {
            // ⚠️ 每一次都从**捕获进来的** `entity` / `name` 克隆一份，绑定名不要
            // 叫 `entity`（CI 实测报过 E0382）：内层 `move` 闭包会把那个新绑定
            // 搬走，而"遮蔽"只在同一个块里生效 —— 下一句再写
            // `let entity = entity.clone()` 克隆的就是**已经被搬走的那个**。
            if running {
                let shell_entity = entity.clone();
                let app = name.clone();
                menu = menu.item(PopupMenuItem::new("终端").on_click(move |_, _, cx| {
                    shell_entity.update(cx, |shell, cx| {
                        shell.run_immediate(ImmediateAction::OpenContainerShell(app.clone()), cx);
                    });
                }));

                let shell_entity = entity.clone();
                let app = name.clone();
                menu = menu.item(PopupMenuItem::new("重启").on_click(move |_, _, cx| {
                    shell_entity.update(cx, |shell, cx| {
                        shell.run_immediate(ImmediateAction::RestartContainer(app.clone()), cx);
                    });
                }));
            }

            let shell_entity = entity.clone();
            let app = name.clone();
            menu = menu.item(PopupMenuItem::new("日志").on_click(move |_, _, cx| {
                shell_entity.update(cx, |shell, cx| shell.open_logs(app.clone(), cx));
            }));

            // 分隔线 + 「删除」：不可撤销的那个单独放在最后
            let shell_entity = entity.clone();
            let app = name.clone();
            menu.item(PopupMenuItem::separator())
                .item(PopupMenuItem::new("删除").on_click(move |_, _, cx| {
                    shell_entity.update(cx, |shell, cx| {
                        shell.request(PendingAction::RemoveContainer(app.clone()), cx);
                    });
                }))
        })
        .into_any_element()
}

/// 全部容器页（含已退出）。
///
/// # 为什么收 `&Shell` + `&App`
///
/// 搜索框是一个 `InputState`（住在 `Shell`），而查询词要**每帧**从它里面读
/// （`Input` 的 `InputEvent::Change` 会订阅回 `Shell`，触发重绘）。
/// 过滤/排序本身是纯逻辑，在 `wslc-panel-core::listview` 里带单测。
pub(crate) fn containers_page(shell: &Shell, cx: &App, entity: &Entity<Shell>) -> impl IntoElement {
    let state = &shell.state;
    let items = &state.snapshot.all;
    let view = state.container_view;

    // 「在运行的有几个」直接用快照里那份**只列运行中**的清单的长度。
    //
    // 为什么不数 `items.iter().filter(|s| s.item.is_running())`：那等于**换一个判据**。
    // 「停止全部」执行时问的是 `wslc list`（不带 `-a`，见
    // `cmd::container::list_running`），页面上这个数字必须和它**是同一批** ——
    // 否则"确认框里写着 3 个、实际停了 4 个"就会发生。`is_running()` 是
    // `state == "running"` 的严格判断，而 `wslc list` 还会列出哪些其它状态
    // **没有逐个实测过**（`wslc` 连 `pause` 这个子命令都没有），所以这里不去猜，
    // 直接用它的输出。
    let running = state.snapshot.running.len();

    // 查询词住在 `InputState` 里；没有输入框（理论上不该发生）时按空串处理。
    let query = shell
        .forms
        .container_search
        .as_ref()
        .map(|input| input.read(cx).value().trim().to_owned())
        .unwrap_or_default();
    let visible = wslc_panel_core::listview::visible_containers(items, query.trim(), view);
    let (all_count, running_count, not_running_count) =
        wslc_panel_core::listview::scope_counts(items);

    let rows: Vec<TableRow> = visible
        .iter()
        .copied()
        .map(|summary| {
            table_row(
                ALL_COLUMNS,
                vec![
                    container_name_link(summary.item.display_name(), entity),
                    // 镜像引用通常比这一列宽（`docker.1panel.live/library/nginx:latest`），
                    // 截断之后悬停能看全文 —— 这一格用户经常要点/要认。
                    cell_muted_full(
                        summary.item.display_name().to_owned(),
                        summary.item.image.clone(),
                    ),
                    cell_container_status(summary),
                    cell_resources(summary),
                    cell_text(wslc_panel_core::util::ports_summary(summary)),
                    container_row_actions(&summary.item, entity),
                ],
            )
        })
        .collect();

    let create = {
        let entity = entity.clone();
        Button::new("create-container")
            .label("创建容器")
            .small()
            .primary()
            .on_click(move |_, window, cx| {
                // `InputState` 只能在有 window 的地方创建，
                // 所以弹窗是懒创建的。
                entity.update(cx, |shell, cx| shell.open_create_dialog(window, cx));
            })
    };

    let search: Option<AnyElement> = shell.forms.container_search.as_ref().map(|input| {
        Input::new(input)
            .id("container-search")
            .w(px(240.))
            .into_any_element()
    });

    let clear = (!query.trim().is_empty()).then(|| {
        let entity = entity.clone();
        let input = shell.forms.container_search.clone();
        Button::new("container-search-clear")
            .label("清空")
            .small()
            .on_click(move |_, window, cx| {
                if let Some(input) = input.as_ref() {
                    input.update(cx, |state, cx| state.set_value(String::new(), window, cx));
                }
                // `set_value` 会发 `InputEvent::Change`，订阅那边会 notify；
                // 这里再补一次是兜底：输入框没变（本来就是空）时父级也该重绘。
                entity.update(cx, |_, cx| cx.notify());
            })
            .into_any_element()
    });

    let chips: Vec<AnyElement> = ContainerScope::ALL
        .iter()
        .map(|scope| {
            let scope = *scope;
            let count = match scope {
                ContainerScope::All => all_count,
                ContainerScope::Running => running_count,
                ContainerScope::NotRunning => not_running_count,
            };
            let shell_entity = entity.clone();
            let mut button = Button::new(SharedString::from(format!("container-scope-{scope:?}")))
                .label(format!("{} {}", scope.label(), count))
                .small()
                .on_click(move |_, _, cx| {
                    shell_entity.update(cx, |shell, cx| {
                        shell.state.container_view.scope = scope;
                        cx.notify();
                    });
                });
            if view.scope == scope {
                button = button.primary();
            }
            button.into_any_element()
        })
        .collect();

    let sort_menu = {
        let entity = entity.clone();
        Button::new("container-sort")
            .label(format!("排序：{}", view.sort.label()))
            .small()
            .dropdown_menu(move |mut menu, _window, _cx| {
                for sort in ContainerSort::ALL {
                    let shell_entity = entity.clone();
                    menu = menu.item(PopupMenuItem::new(sort.label()).on_click(move |_, _, cx| {
                        shell_entity.update(cx, |shell, cx| {
                            shell.state.container_view.sort = sort;
                            cx.notify();
                        });
                    }));
                }
                menu
            })
    };

    let count_text = if query.trim().is_empty() && view.scope == ContainerScope::All {
        format!("共 {all_count} 个容器")
    } else {
        format!("显示 {} / 共 {all_count}", visible.len())
    };

    // 页面级动作只有两个：停止全部运行中容器、清理已停止容器。
    // 它们影响所有容器（不受筛选影响），但没必要再套一层菜单；
    // 直接显示，点击后仍走统一的二次确认。
    let prune = {
        let entity = entity.clone();
        Button::new("prune")
            .label("清理已停止")
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| {
                    shell.request(PendingAction::PruneContainers, cx);
                });
            })
    };

    // 一个都没在跑时不显示「停止全部」：点它什么也做不了。
    let stop_all = (running > 0).then(|| {
        let entity = entity.clone();
        Button::new("stop-all")
            .label("停止全部")
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| {
                    shell.request(PendingAction::StopAllContainers { running }, cx);
                });
            })
            .into_any_element()
    });

    // 筛选后为空：不要显示成"没有任何容器"（那是在说"你机器上没容器"）。
    let empty: Option<AnyElement> = if rows.is_empty() {
        if all_count == 0 {
            // 整个列表区就是空的 → 官方 `Empty`（虚线框 + 撑满）
            Some(page_empty("没有任何容器"))
        } else {
            let entity = entity.clone();
            let input = shell.forms.container_search.clone();
            let reset = Button::new("container-filter-clear")
                .label("清空筛选")
                .small()
                .on_click(move |_, window, cx| {
                    entity.update(cx, |shell, cx| {
                        shell.state.container_view = ContainerView::default();
                        cx.notify();
                    });
                    if let Some(input) = input.as_ref() {
                        input.update(cx, |state, cx| state.set_value(String::new(), window, cx));
                    }
                });
            Some(
                v_flex()
                    .w_full()
                    .items_center()
                    .gap_2()
                    .p_5()
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme::text_muted())
                            .child("没有匹配的容器"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::text_dim())
                            .child("换个关键词，或点「全部」；也可以清空筛选。"),
                    )
                    .child(reset)
                    .into_any_element(),
            )
        }
    } else {
        None
    };

    v_flex()
        .w_full()
        .gap_3()
        .child(
            h_flex()
                .w_full()
                .justify_between()
                .items_center()
                .gap_3()
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(create)
                        .children(search)
                        .children(clear),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .children(stop_all)
                        .child(prune),
                ),
        )
        .child(
            h_flex()
                .w_full()
                .gap_2()
                .items_center()
                .flex_wrap()
                .children(chips)
                .child(sort_menu)
                .child(
                    div()
                        .text_sm()
                        .text_color(theme::text_muted())
                        .child(count_text),
                ),
        )
        .child(table_card(ALL_COLUMNS, rows, empty))
}
