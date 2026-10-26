//! ②c 应用部署管理页：表格、行内动作、三个浮层。

// ⚠️ `modal_title_danger` / `modal_footer` 不再导入：三个已迁的浮层
// （日志 / 站点配置 / 删除确认）把标题和底部交给 `Dialog` + 调用处的
// `modal_footer`，本文件只产出内容。剩下的「添加部署」弹窗仍用
// `modal_card` / `modal_scrim` / `modal_title`。
// ⚠️ `modal_title_danger` 不再需要（删除确认的标题交给 `Dialog`），
// 但 `modal_footer` **仍要留着**：「添加部署」弹窗（未迁）还在用它。
use super::common::{
    ModalSize, NoContainer, cell_link, cell_muted_full, cell_text, disclosure, empty_state,
    form_field, immediate_button, kv_block, log_text, modal_card, modal_footer, modal_scrim,
    modal_title, overlay_button, page_empty, table_card, table_row,
};
use super::prelude::*;

/// 「部署管理」页。
///
/// # 它为什么不在侧边栏
///
/// 和「数据库管理」同一个取舍：它服务**一个**容器（`AppState::deploy` 里记着是
/// 哪个），只能从容器列表行上的「部署管理」按钮进，所以顶部有「← 返回容器」。
///
/// # 三种形态（和 `database` 一一对应）
///
/// 1. **探测就失败**（不是这几类容器 / 容器没运行）：只画那张说明卡片。
///    这时候再摆一张空表只会干扰判断；
/// 2. **正常**：页头 + 「添加部署」+ 已部署列表；
/// 3. **还没读到数据**：表格的位置放一句"正在读取…"。
///
/// # 和数据库页最大的不同
///
/// 这一页的数据有**两个来源**，界面要如实区分：
/// 「部署过什么」来自**本地面板**的存档（所以容器停着也看得到），
/// 「现在跑没跑 / 目录里还有没有」**问容器**（那是事实）。
/// 所以每一行的状态列可能是"读取中…"——那不是说没有，是说还没问回来。
///
/// ⚠️ 这一页**不需要 `cx`**（所有动作都交给 `entity` 上的 `Shell` 方法去做），
/// 但签名和隔壁 `database` / `containers` 保持同一个形状，`views::page()` 那边
/// 就不用为它单写一个分支。用 `_cx` 把"这里故意不用"写出来。
pub fn deploy_page(shell: &Shell, _cx: &App, entity: &Entity<Shell>) -> AnyElement {
    let Some(deploy) = shell.state.deploy.as_ref() else {
        // 理论上来不到（只可能从容器行上的按钮进，那里已经建好状态了），
        // 但真到了这里就说一句人话，别画一片空白。
        return page_empty(NoContainer::Deploy.empty_hint());
    };

    let header = deploy_header(deploy, entity);

    if let Some(blocked) = deploy.blocked.as_ref() {
        return v_flex()
            .w_full()
            .gap_3()
            .child(header)
            .child(deploy_blocked_card(deploy, blocked))
            .into_any_element();
    }

    let body: AnyElement = if deploy.loaded {
        deploy_table(deploy, entity)
    } else {
        deploy_loading(deploy.busy)
    };

    v_flex()
        .w_full()
        .gap_3()
        .child(header)
        // 存档读坏了要说出来 —— 不然用户会以为"我部署过的东西全没了"。
        // `AppState::deploy_store_error` 存在的唯一目的就是这一行。
        // 另一半在 `views::database` 的 `creds_warning`（账号存档读坏了）——
        // 两个存档各有一条，别只补一边。
        .children(
            shell
                .state
                .deploy_store_error
                .as_ref()
                .map(|error| deploy_error_banner(&format!("部署存档读不出来：{error}"))),
        )
        .children(
            deploy
                .error
                .as_ref()
                // 直接传函数本身，不写 `|failure| f(failure)`
                // （clippy 的 `redundant_closure`）。
                .map(deploy_failure_banner),
        )
        .child(body)
        .into_any_element()
}

/// 页头那一行"已发布到主机的端口"。
///
/// 只**报告**：已建好的容器改不了端口发布（`-p` 只在建容器时给），所以这里
/// 不画按钮，只把 `主机 URL → 容器端口` 摆出来。端口列的 URL 是逐应用的，
/// 这里是容器级的全量清单。
pub(crate) fn deploy_published_line(deploy: &DeployState) -> String {
    let ports = deploy_model::published_container_ports(&deploy.ports);
    if ports.is_empty() {
        return "还没有端口发布到主机（-p 只能在建容器时给）".to_owned();
    }
    let items: Vec<String> = ports
        .iter()
        .filter_map(|port| {
            deploy_model::app_url(&deploy.ports, *port).map(|url| format!("{url} → 容器 {port}"))
        })
        .collect();
    if items.is_empty() {
        "有发布记录，但没有可用的主机绑定".to_owned()
    } else {
        format!("已发布：{}", items.join("，"))
    }
}

/// 页头：容器 / 镜像 / 运行时 + 依据 + 「← 返回容器」「重新读取」「添加部署…」。
pub(crate) fn deploy_header(deploy: &DeployState, entity: &Entity<Shell>) -> AnyElement {
    let back = {
        let entity = entity.clone();
        Button::new("deploy-back")
            .label("← 返回容器")
            .small()
            .on_click(move |_, window, cx| {
                entity.update(cx, |shell, cx| shell.close_deploy_page(window, cx));
            })
    };

    // 读取期间把按钮**换掉**（项目里不用 `.disabled()`）：置灰的按钮会让人
    // 反复去点，而"读取中…"说清了现在在等什么。
    let reload: AnyElement = if deploy.busy {
        div()
            .text_sm()
            .text_color(theme::text_muted())
            .child("读取中…")
            .into_any_element()
    } else {
        let entity = entity.clone();
        Button::new("deploy-reload")
            .label("重新读取")
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.load_deploy(cx));
            })
            .into_any_element()
    };

    // 「添加部署…」——弹出一次性的表单（`InputState::new` 要 `&mut Window`，
    // 所以只能在点击这一刻建）。
    let add: AnyElement = if deploy.progress.is_some() {
        let label = deploy
            .progress
            .as_ref()
            .map(|progress| progress.phase.label())
            .unwrap_or("正在…");
        div()
            .text_sm()
            .text_color(theme::text_muted())
            .child(label)
            .into_any_element()
    } else {
        let entity = entity.clone();
        Button::new("deploy-add")
            .label("添加部署…")
            .small()
            .primary()
            .on_click(move |_, window, cx| {
                entity.update(cx, |shell, cx| shell.open_deploy_dialog(None, window, cx));
            })
            .into_any_element()
    };

    // nginx 的「重载」放在页头而不是行内：它作用于**容器里那个 nginx 进程**，
    // 不属于任何一条部署（清单里可能有多个站点，重载一次全生效）。
    let reload_nginx: AnyElement = if deploy.runtime == wslc_core::model::AppRuntime::Nginx {
        let entity = entity.clone();
        Button::new("deploy-reload-nginx")
            .label("重载 nginx")
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.reload_nginx_in_container(cx));
            })
            .into_any_element()
    } else {
        div().into_any_element()
    };

    h_flex()
        .w_full()
        .justify_between()
        .items_center()
        .child(
            v_flex()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .font_semibold()
                        .child(format!("容器 {}", deploy.container)),
                )
                .child(div().text_xs().text_color(theme::text_dim()).child(format!(
                    "{} · {} · {}",
                    deploy.runtime.label(),
                    deploy.image,
                    deploy.signal.label(),
                )))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_dim())
                        .max_w(px(560.0))
                        .truncate()
                        .child(deploy_published_line(deploy)),
                ),
        )
        .child(
            h_flex()
                .gap_2()
                .child(back)
                .child(reload_nginx)
                .child(reload)
                .child(add),
        )
        .into_any_element()
}

/// 探测就失败时的说明卡（不是这几类容器 / 容器没运行）。
pub(crate) fn deploy_blocked_card(
    deploy: &DeployState,
    failure: &crate::state::DeployFailure,
) -> AnyElement {
    let note = if failure.kind == wslc_core::model::DeployErrorKind::ContainerNotRunning {
        "这一页的每一个动作都要进容器里执行（准备目录 → 传文件 → 起进程），\
         所以容器得先跑起来。「启动」就在容器列表那一行上。"
    } else {
        // ⚠️ 「终端」v0.9.14 起在列表行的「更多」菜单里 —— 这句话要跟着改，
        // 否则用户去行内找那个按钮会找不到（`AGENTS.md` §5：过期注释/文案
        // 会让后来的人照着它继续错）。
        "「部署应用」服务的是 nginx / Java / Node.js / Go 这几类容器 —— \
         面板按镜像引用、环境变量、启动命令三样认人，一个线索都没有时不会去猜。\
         想自己进去看，用「容器」页那一行「更多」菜单里的「终端」。"
    };

    v_flex()
        .w_full()
        .gap_3()
        .p_4()
        .rounded_lg()
        .bg(theme::danger_soft())
        .border_1()
        .border_color(theme::danger_edge())
        .child(
            div()
                .text_sm()
                .font_semibold()
                .text_color(theme::danger())
                .child(format!("这个容器现在部署不了（{}）", failure.kind.label())),
        )
        .child(
            div()
                .text_sm()
                .text_color(theme::text())
                .child(crate::util::strip_md(&failure.summary())),
        )
        .child(div().text_xs().text_color(theme::text_muted()).child(note))
        .child(log_text(Some(&failure.detail), "deploy-blocked-detail"))
        .child(div().text_xs().text_color(theme::text_dim()).child(format!(
            "刚才是在读容器 {} 的时候失败的。",
            deploy.container
        )))
        .into_any_element()
}

/// 上一次失败：一句"该怎么办" +（可滚动的）原文。
pub(crate) fn deploy_failure_banner(failure: &crate::state::DeployFailure) -> AnyElement {
    v_flex()
        .w_full()
        .gap_2()
        .p_3()
        .rounded_lg()
        .bg(theme::danger_soft())
        .border_1()
        .border_color(theme::danger_edge())
        .child(
            div()
                .text_sm()
                .font_semibold()
                .text_color(theme::danger())
                .child(crate::util::strip_md(&failure.summary())),
        )
        .child(log_text(Some(&failure.detail), "deploy-error-detail"))
        .into_any_element()
}

/// 存档读坏了的提示条（一行字，不折叠）。
pub(crate) fn deploy_error_banner(text: &str) -> AnyElement {
    div()
        .w_full()
        .p_3()
        .rounded_lg()
        .bg(theme::danger_soft())
        .border_1()
        .border_color(theme::danger_edge())
        .child(
            div()
                .text_sm()
                .text_color(theme::danger())
                .child(text.to_owned()),
        )
        .into_any_element()
}

/// 「正在读取…」占位（和 `db_loading` 同一形状）。
pub(crate) fn deploy_loading(busy: bool) -> AnyElement {
    use gpui_kit::component::spinner::Spinner;

    // 和 `db_loading` 同一个判断：busy 时必须有**转圈**，只换文字不够。
    // 另外那句括号里的实现细节（"先看容器认不认得出…"）删掉了 ——
    // 用户只想知道"还要不要等"。
    let body: AnyElement = if busy {
        h_flex()
            .gap_2()
            .items_center()
            .child(Spinner::new())
            .child(
                div()
                    .text_sm()
                    .text_color(theme::text_muted())
                    .child("正在读取…"),
            )
            .into_any_element()
    } else {
        div()
            .text_sm()
            .text_color(theme::text_muted())
            .child("还没读到数据 —— 点右上角「重新读取」")
            .into_any_element()
    };
    div()
        .w_full()
        .p_6()
        .rounded_lg()
        .bg(theme::bg_card())
        .border_1()
        .border_color(theme::border())
        .child(body)
        .into_any_element()
}

/// 已部署列表。
pub(crate) fn deploy_table(deploy: &DeployState, entity: &Entity<Shell>) -> AnyElement {
    let rows: Vec<TableRow> = deploy
        .entries
        .iter()
        .map(|entry| {
            table_row(
                DEPLOY_COLUMNS,
                vec![
                    cell_text(entry.record.app.clone()),
                    cell_text(
                        entry
                            .runtime
                            .map(|runtime| runtime.kind().label())
                            .unwrap_or("—"),
                    ),
                    // 目标路径是这一行最要紧的一格（部署铺到了哪儿、
                    // 删除会删掉什么），而且容器路径经常很长 ——
                    // 截断了也要能悬停看全文。
                    cell_muted_full(entry.record.app.clone(), entry.record.target.clone()),
                    deploy_status_cell(entry, deploy),
                    deploy_port_cell(entry, deploy, entity),
                    deploy_row_actions(entry, deploy, entity),
                ],
            )
        })
        .collect();

    // 空状态是两行说明（第二行还带 `strip_md`），所以自己拼一个 `AnyElement`
    // 交给 `table_card`，而不是传一句文案。
    let empty: Option<AnyElement> = deploy.entries.is_empty().then(|| {
        v_flex()
            .w_full()
            .gap_2()
            .p_6()
            .items_center()
            .child(
                div()
                    .text_sm()
                    .text_color(theme::text_muted())
                    .child("这个容器上还没有部署过任何应用"),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme::text_dim())
                    // `note()` 的文案里写着 Markdown（`**静态文件**` / 反引号），
                    // 而这里是纯文本 → 必须过 `strip_md`（见 `util::strip_md`：
                    // "与其在每条文案里手工删标记，不如在渲染处统一清掉"）。
                    .child(crate::util::strip_md(deploy.runtime.note())),
            )
            .into_any_element()
    });

    v_flex()
        .w_full()
        .gap_2()
        .child(
            div()
                .text_xs()
                .text_color(theme::text_dim())
                .child(crate::util::strip_md(&format!(
                    "共 {} 条部署。「部署过什么」来自面板自己的存档（容器停着也看得到）；\
                     「现在跑没跑」是**问容器**得来的。",
                    deploy.entries.len()
                ))),
        )
        .child(table_card(DEPLOY_COLUMNS, rows, empty))
        .into_any_element()
}

/// 状态那一格。
///
/// 「读不到」和「目标不存在」是**两件事**，所以分开画：前者是"问容器的时候
/// 出错了"（可能是容器刚停、网络抖动），后者是"真的没有了"。
/// 把前者显示成后者会让人以为文件丢了。
pub(crate) fn deploy_status_cell(entry: &DeployEntry, deploy: &DeployState) -> AnyElement {
    if let Some(error) = entry.probe_error.as_ref() {
        // 用 `cell_muted_full` 承载 tooltip：它拿组件库的 `Button` 当壳，
        // 所以截断的文字能悬停看全文（`AGENTS.md` §7.5）。
        return cell_muted_full(
            format!("deploy-status-{}-{}", deploy.container, entry.record.app),
            format!("读不到状态（{}）", crate::util::strip_md(error)),
        );
    }
    let mut extra = String::new();
    if entry.probed {
        match entry.files {
            Some(count) => extra = format!(" · {count} 个文件"),
            None => extra = " · 目标目录不在".to_owned(),
        }
    }
    let label = entry.status_label();
    div()
        .text_sm()
        .text_color(
            if entry.process == Some(wslc_core::model::ProcessStatus::Running) {
                theme::text()
            } else {
                theme::text_muted()
            },
        )
        .child(format!("{label}{extra}"))
        .into_any_element()
}

/// 端口那一格：只**报告**已经发布出去的映射。
///
/// ⚠️ 已存在的容器改不了端口发布（`-p` 只在 `run` / `create` 时给），
/// 所以这里绝不画一个点了没用的「发布」按钮 —— 而是把下一步说清楚。
pub(crate) fn deploy_port_cell(
    entry: &DeployEntry,
    deploy: &DeployState,
    entity: &Entity<Shell>,
) -> AnyElement {
    let port = entry.record.port;
    if port == 0 {
        return cell_text("—");
    }
    // 判据只有一份：`model::deploy::port_is_published`（纯函数、有单测）。
    // 早先这里是**内联复制**的一段，而真正被测的那份（`DeployProbe::publishes`）
    // 全仓库没有一个调用点 —— "测的不是跑的那份"（v0.9.13 改）。
    let published = deploy_model::port_is_published(&deploy.ports, port);
    if published {
        let key = format!("deploy-port-{}-{}", deploy.container, entry.record.app);
        let is_nginx = entry.runtime == Some(wslc_core::model::AppRuntime::Nginx);
        // 站点关的 nginx 由**默认站点**提供，路径不是 `/`（`/<app>/` 或用户自己
        // 填的相对目录）。这一格不猜路径，只把"已发布 + 站点关"说清楚；
        // 站点开 / 进程型才有独立 URL。
        //
        // ⚠️ 不拼 URL 的具体理由：默认站点的 path 是 `<target 相对 Web 根>`，
        // 不一定等于 `<app>`；老记录还可能直接铺在 Web 根（path 就是 `/`）。
        if is_nginx && entry.record.site.is_none() {
            return cell_muted_full(key, format!("{port}（已发布；站点关，由默认站点提供）"));
        }
        if let Some(url) = deploy_model::app_url(&deploy.ports, port) {
            // URL 直接可点：部署完最自然的下一步就是打开看一眼。
            // 单元格仍然保留 tooltip，长 URL 被截断时悬停能看全。
            return cell_link(key, format!("{url}（容器 {port}）"), url, entity);
        }
        cell_text(format!("{port}（已发布）"))
    } else {
        cell_muted_full(
            format!("deploy-port-{}-{}", deploy.container, entry.record.app),
            format!(
                "{port}（没发布到主机 —— 端口只能在建容器时给，已建好的容器改不了，\
                 外部访问不了）"
            ),
        )
    }
}

/// 行内操作。
///
/// 按形态分岔 —— 这不是"多给一个入口"，而是**两类东西的动作本来就不同**：
/// 内容形态（nginx）没有属于我们的进程，画「启动 / 停止」只会点了报错。
/// 只画**当前有意义**的那几个（停止时画「启动」，运行时画「停止」）。
pub(crate) fn deploy_row_actions(
    entry: &DeployEntry,
    deploy: &DeployState,
    entity: &Entity<Shell>,
) -> AnyElement {
    let app = entry.record.app.clone();
    let running = entry.process == Some(wslc_core::model::ProcessStatus::Running);
    let busy = deploy.progress.as_ref().is_some_and(|p| p.app == app);
    let mut actions: Vec<AnyElement> = Vec::new();

    if busy {
        // 这一条正在跑一个动作 —— 把按钮换掉而不是留着（`.disabled()` 全项目不用）
        let label = deploy
            .progress
            .as_ref()
            .map(|p| p.phase.label())
            .unwrap_or("正在…");
        return div()
            .text_xs()
            .text_color(theme::text_muted())
            .child(label)
            .into_any_element();
    }

    // 类型认不出来：**不猜**该用哪套动作（见 `DeployEntry::runtime` 的说明），
    // 只给「删除记录」并说明为什么别的按钮没有。
    if entry.runtime.is_none() {
        actions.push(deploy_danger_button(
            &format!("deploy-forget-{app}"),
            "删除记录",
            {
                let app = app.clone();
                move |shell, cx| shell.forget_deployment_record(app.clone(), cx)
            },
            entity,
        ));
        actions.push(
            div()
                .text_xs()
                .text_color(theme::text_dim())
                .child("存档里的类型认不出来，只能删掉这条记录")
                .into_any_element(),
        );
        return h_flex()
            .gap_2()
            .flex_wrap()
            .children(actions)
            .into_any_element();
    }

    let is_process = entry.is_process();

    // 启动 / 停止（只有进程形态有）
    if is_process {
        if running {
            actions.push(
                immediate_button(
                    &format!("deploy-stop-{app}"),
                    "停止",
                    ImmediateAction::StopDeployment(app.clone()),
                    entity,
                )
                .into_any_element(),
            );
        } else {
            actions.push(
                immediate_button(
                    &format!("deploy-start-{app}"),
                    "启动",
                    ImmediateAction::StartDeployment(app.clone()),
                    entity,
                )
                .into_any_element(),
            );
        }
    } else {
        // 内容形态：铺进去就生效，这里给的是"我改完本地文件想再传一次"
        actions.push(overlay_button(
            &format!("deploy-redeploy-{app}"),
            "重新上传",
            {
                let app = app.clone();
                move |shell, window, cx| shell.open_deploy_dialog(Some(app.clone()), window, cx)
            },
            entity,
        ));
    }

    // 日志（只有进程形态有 —— 内容形态的产物是文件，没有我们起的进程）
    if is_process {
        actions.push(overlay_button(
            &format!("deploy-logs-{app}"),
            "日志",
            {
                let app = app.clone();
                move |shell, _, cx| shell.open_deploy_logs(app.clone(), cx)
            },
            entity,
        ));
    }

    // 站点配置（只有 nginx 站点开的记录有 —— 站点关没有面板生成的 conf）
    if !is_process && entry.record.site.is_some() {
        actions.push(overlay_button(
            &format!("deploy-site-{app}"),
            "站点配置",
            {
                let app = app.clone();
                move |shell, _, cx| shell.open_deploy_site_conf(app.clone(), cx)
            },
            entity,
        ));
    }

    // 进程形态才需要"重新部署"（内容形态上面那个就是它）
    if is_process {
        actions.push(overlay_button(
            &format!("deploy-redeploy-{app}"),
            "重新部署",
            {
                let app = app.clone();
                move |shell, window, cx| shell.open_deploy_dialog(Some(app.clone()), window, cx)
            },
            entity,
        ));
    }

    actions.push(deploy_danger_button(
        &format!("deploy-remove-{app}"),
        "删除",
        {
            let app = app.clone();
            move |shell, cx| shell.request_remove_deployment(app.clone(), cx)
        },
        entity,
    ));

    h_flex()
        .gap_2()
        .flex_wrap()
        .children(actions)
        .into_any_element()
}

/// 部署页的红色按钮：**直接**调一个 `Shell` 方法，不走 `PendingAction`。
///
/// 为什么不复用 [`danger_button`]：那一个把动作交给 `ConfirmAction::execute`，
/// 而它的形状是"跑一条 `wslc` 命令 → 返回字符串"（同步、单个子进程）。
/// 删一条部署要做三件事 —— 停进程、删掉面板铺进去的那些文件、**改本地存档并重读列表** ——
/// 最后那件它表达不了。所以删除确认是部署页自己的浮层
///（[`deploy_remove_confirm_body`]），按钮直接把动作交给 `Shell`。
pub(crate) fn deploy_danger_button(
    id: &str,
    label: &'static str,
    run: impl Fn(&mut Shell, &mut Context<Shell>) + 'static,
    entity: &Entity<Shell>,
) -> AnyElement {
    let entity = entity.clone();
    Button::new(SharedString::from(id.to_owned()))
        .label(label)
        .small()
        .danger()
        .on_click(move |_, _, cx| {
            entity.update(cx, |shell, cx| run(shell, cx));
        })
        .into_any_element()
}

/// 「添加部署 / 重新部署」弹窗。
///
/// # 为什么是弹窗而不是页面上的表单
///
/// 五个输入框，而 `InputState::new` 要 `&mut Window` —— 只能懒创建
/// （渲染时建会每帧重建输入框，**字都打不进去**，见 `AGENTS.md` §7.5）。
/// 和「拉取镜像」「新建数据库」同一条。
///
/// # 表单里的说明文字都是**必须的**
///
/// - 「目标目录里会被替换的是哪几个文件」：部署是"删掉我们铺过的名字再铺"，
///   用户有权在点之前知道（v0.9.10 起不再整目录清空 —— 见
///   `model::deploy::removals_script`）；
/// - 「启动命令会原样交给 /bin/sh -c」：它**不做**引号转义（刻意的，
///   见 `model::deploy::start_script`），不说清楚用户会以为它是参数。
pub fn deploy_dialog_overlay(shell: &Shell, entity: &Entity<Shell>, cx: &App) -> AnyElement {
    let Some(form) = shell.deploy_form.as_ref() else {
        return div().into_any_element();
    };
    let Some(deploy) = shell.state.deploy.as_ref() else {
        return div().into_any_element();
    };
    let runtime = deploy.runtime;
    let re_deploy = form.editing.is_some();

    // 站点块出现后弹窗会高出一截；带 id 的滚动容器（AGENTS §7.5）。
    // 宽度从 560 提到 `ModalSize::Wide`（800）：这个弹窗里有「目标目录」
    // 「启动命令」这类长路径，560 宽时输入框里的路径几乎全是省略号。
    let card = modal_card(ModalSize::Wide)
        .id("deploy-dialog-scroll")
        .max_h(px(760.))
        .overflow_y_scroll()
        .child(
            v_flex()
                .gap_1()
                .child(modal_title(if re_deploy {
                    format!("重新部署 {}", form.editing.clone().unwrap_or_default())
                } else {
                    format!("添加部署 · {}", runtime.label())
                }))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_dim())
                        .child(format!("目标容器 {}", deploy.container)),
                ),
        );

    // 重新部署时**不给**应用名的输入框，只把名字写出来 ——
    // 和「数据库管理」的「改密码 / 授权」锁定用户名 / 主机是同一个手法
    //（改了名字就是另一个部署了，那应该走「添加部署」）。
    let card = if re_deploy {
        card.child(kv_block("应用名", form.editing.clone().unwrap_or_default()))
    } else {
        card.child(form_field("deploy-app", "应用名", &form.app, cx, true))
    };

    let card = card
        .child(
            h_flex()
                .w_full()
                .gap_2()
                .items_end()
                .child(form_field(
                    "deploy-source",
                    if runtime.source_kind() == deploy_model::SourceKind::Directory {
                        "本地目录"
                    } else {
                        "本地文件"
                    },
                    &form.source,
                    cx,
                    false,
                ))
                .child({
                    let entity = entity.clone();
                    Button::new("deploy-browse")
                        .label("浏览…")
                        .small()
                        .on_click(move |_, window, cx| {
                            entity.update(cx, |shell, cx| shell.browse_deploy_source(window, cx));
                        })
                }),
        )
        .child(form_field(
            "deploy-target",
            "容器内目标目录",
            &form.target,
            cx,
            true,
        ))
        .child(
            div()
                .text_xs()
                .text_color(theme::danger())
                .child(crate::util::strip_md(
                    "部署会替换面板上次写入的文件，保留其他文件。请使用独立应用目录；\
                     仅允许 /opt、/srv、/app、/var/www、/usr/share/nginx 下的路径。",
                )),
        );

    // 进程形态才有「启动命令」这一格；内容形态（nginx）铺完就生效。
    let card = if runtime.kind() == wslc_core::model::DeployKind::Process {
        card.child(form_field(
            "deploy-command",
            "启动命令",
            &form.command,
            cx,
            true,
        ))
        .child(
            div()
                .text_xs()
                .text_color(theme::text_dim())
                .child(crate::util::strip_md(
                    "命令原样交给 /bin/sh -c，支持管道与 &&；日志位于目标目录的 app.log。",
                )),
        )
    } else {
        card.child(
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child("静态文件上传后生效。"),
        )
    };

    // 反代是可选配置；已保存的规则在打开表单时自动展开，折叠不丢参数。
    let proxy_filled = !form.proxy_prefix.read(cx).value().trim().is_empty()
        || !form.proxy_upstream.read(cx).value().trim().is_empty()
        || !form.proxy_more.read(cx).value().trim().is_empty();
    let more_filled = !form.proxy_more.read(cx).value().trim().is_empty();
    let proxy_block = disclosure(
        UiSection::DeployProxy,
        if proxy_filled {
            "反向代理 · 已填写"
        } else {
            "反向代理 · 未配置"
        },
        &shell.state,
        entity,
        v_flex()
            .w_full()
            .gap_2()
            .child(form_field(
                "deploy-proxy-prefix",
                "路径前缀（如 /api/）",
                &form.proxy_prefix,
                cx,
                true,
            ))
            .child(form_field(
                "deploy-proxy-upstream",
                "上游地址（如 http://api:8080）",
                &form.proxy_upstream,
                cx,
                true,
            ))
            .child({
                let entity = entity.clone();
                Checkbox::new("deploy-proxy-strip")
                    .label("转发时去掉路径前缀")
                    .checked(form.proxy_strip)
                    .on_change(move |checked: &bool, _, cx| {
                        let enabled = *checked;
                        entity.update(cx, |shell, cx| shell.set_deploy_proxy_strip(enabled, cx));
                    })
            })
            .child(disclosure(
                UiSection::DeployProxyMore,
                if more_filled {
                    "更多规则 · 已填写"
                } else {
                    "添加更多规则"
                },
                &shell.state,
                entity,
                v_flex()
                    .w_full()
                    .gap_1()
                    .child(form_field(
                        "deploy-proxy-more",
                        "更多反代规则",
                        &form.proxy_more,
                        cx,
                        true,
                    ))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::text_dim())
                            // 这是**语法说明**（用户必须知道怎么写），不是解释性长文 ——
                            // 所以留，但压到一行：示例本身已经把格式说清了，
                            // "路径前缀不能重复"是校验规则，等真冲突时报错即可
                            // （校验在 `valid_proxy_prefix` 里，有测试）。
                            .child("每行一条，分号分隔：/auth/ => http://auth:9000"),
                    ),
            ))
            .child({
                let entity = entity.clone();
                Checkbox::new("deploy-auto-connect")
                    .label("自动接通上游容器的用户网络")
                    .checked(form.auto_connect)
                    .on_change(move |checked: &bool, _, cx| {
                        let enabled = *checked;
                        entity.update(cx, |shell, cx| shell.set_deploy_auto_connect(enabled, cx));
                    })
            })
            .child(
                div()
                    .text_xs()
                    .text_color(theme::text_dim())
                    .child("上游容器需网络可达；关闭自动接通后须自行连接用户网络。"),
            ),
    );
    let site_block = if runtime == wslc_core::model::AppRuntime::Nginx {
        Some(v_flex().w_full().gap_2().p_3().rounded_md()
            .bg(theme::bg()).border_1().border_color(theme::border())
            .child({
                let entity = entity.clone();
                Checkbox::new("deploy-site-enabled")
                    .label("启用独立站点")
                    .checked(form.site_enabled)
                    .on_change(move |checked: &bool, _, cx| {
                        let enabled = *checked;
                        entity.update(cx, |shell, cx| shell.set_deploy_site_enabled(enabled, cx));
                    })
            })
            .child(div().text_xs().text_color(theme::text_dim())
                .child("开启后生成站点配置；使用 80 端口会接管默认站点。关闭时由默认站点提供 /应用名/。"))
            .children(form.site_enabled.then(|| {
                v_flex().w_full().gap_2()
                    .child({
                        let entity = entity.clone();
                        Checkbox::new("deploy-site-spa")
                            .label("单页应用回退到 index.html")
                            .checked(form.site_spa)
                            .on_change(move |checked: &bool, _, cx| {
                                let enabled = *checked;
                                entity.update(cx, |shell, cx| shell.set_deploy_site_spa(enabled, cx));
                            })
                    })
                    .child(form_field("deploy-server-name", "域名（server_name）", &form.server_name, cx, true))
                    .child(proxy_block)
                    .into_any_element()
            }))
            .into_any_element())
    } else {
        None
    };

    let card = card
        .children(site_block)
        .child(form_field(
            "deploy-port",
            if runtime == wslc_core::model::AppRuntime::Nginx {
                if form.site_enabled {
                    "站点监听端口"
                } else {
                    "端口（检查主机映射）"
                }
            } else {
                "应用监听端口"
            },
            &form.port,
            cx,
            true,
        ))
        .child(
            div()
                .text_xs()
                .text_color(theme::text_dim())
                .child("每个应用使用独立名称、目录和端口；主机端口映射须在创建容器时设置。"),
        )
        .children(
            (runtime.kind() == wslc_core::model::DeployKind::Process).then(|| {
                div()
                    .text_xs()
                    .text_color(theme::text_dim())
                    .child("启动命令须监听此端口，面板不会自动修改命令。")
            }),
        )
        .child(modal_footer([
            {
                let entity = entity.clone();
                Button::new("deploy-cancel")
                    .label("取消")
                    .small()
                    .on_click(move |_, _, cx| {
                        entity.update(cx, |shell, cx| shell.close_deploy_dialog(cx));
                    })
            },
            {
                let entity = entity.clone();
                Button::new("deploy-submit")
                    .label(if re_deploy { "重新部署" } else { "部署" })
                    .small()
                    .primary()
                    .on_click(move |_, window, cx| {
                        entity.update(cx, |shell, cx| shell.submit_deploy(window, cx));
                    })
            },
        ]));

    modal_scrim("deploy-dialog-scrim", card).into_any_element()
}

/// 应用日志对话框的**内容**（外壳/标题/底部由 Dialog 负责）。
///
/// 只读：一个输入框都没有，所以状态是纯数据。
pub(crate) fn deploy_logs_body(logs: &DeployLogs) -> AnyElement {
    let body: AnyElement = if logs.busy {
        div()
            .text_sm()
            .text_color(theme::text_muted())
            .child("正在读取…")
            .into_any_element()
    } else {
        log_text(logs.text.as_deref(), "deploy-logs-text")
    };

    v_flex()
        .w_full()
        .gap_3()
        .child(
            div()
                .text_xs()
                .text_color(theme::text_dim())
                .child(crate::util::strip_md(
                    "是这个应用**自己**的输出（启动时把它重定向到了目标目录的 \
                 app.log）—— 不是容器主进程的日志，那个在「日志」按钮里。",
                )),
        )
        .child(body)
        .into_any_element()
}

/// 「站点配置」只读浮层。
///
/// 文本**不**过 `strip_md`、不逐行重排：conf 里的缩进 / `#` / 空行都是有意义的，
/// 逐行处理只会让用户没法对照 `nginx -t` 的错误行。
pub(crate) fn deploy_site_conf_body(conf: &DeploySiteConf) -> AnyElement {
    let body: AnyElement = if conf.busy {
        div()
            .text_sm()
            .text_color(theme::text_muted())
            .child("正在读取…")
            .into_any_element()
    } else {
        match conf.text.as_deref() {
            Some(text) if !text.trim().is_empty() => v_flex()
                .id("deploy-site-conf-text")
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
                        .text_color(theme::text())
                        .child(text.to_owned()),
                )
                .into_any_element(),
            Some(_) => empty_state("（配置是空的）").into_any_element(),
            None => div()
                .text_sm()
                .text_color(theme::text_dim())
                .child("正在加载…")
                .into_any_element(),
        }
    };

    v_flex()
        .w_full()
        .gap_3()
        .child(
            div()
                .text_xs()
                .text_color(theme::text_dim())
                .child(crate::util::strip_md(
                    "容器里的 `/etc/nginx/conf.d/00-wslc-panel-<应用名>.conf`（只读）。\
                 重新部署会整份覆盖；要改就改部署弹窗里的站点区，别直接改这个文件。",
                )),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme::text_dim())
                .child(crate::util::strip_md(
                    "容器重建后这份 conf 会丢 —— 可以点「重写配置」补回来\
                 （只写 conf，不重新铺文件 / 不重新接网络）。",
                )),
        )
        .child(body)
        .into_any_element()
}

/// 删除二次确认。
///
/// ⚠️ 必须把**完整路径**摆出来：删的虽然只有面板自己铺过的那几个名字，
/// 但目标可能是**卷挂载点**（本机那个 nginx 容器的 Web 根就挂在 `/dev/sdc` 上）
/// —— 那意味着删掉的是卷里的数据，而且不可撤销。
pub(crate) fn deploy_remove_confirm_body(app: &str, deploy: Option<&DeployState>) -> AnyElement {
    let container = deploy.map(|d| d.container.clone()).unwrap_or_default();
    let target = deploy
        .and_then(|d| d.index_of(app).map(|i| d.entries[i].record.target.clone()))
        .unwrap_or_else(|| "（找不到目标目录）".to_owned());
    let is_process = deploy
        .and_then(|d| d.index_of(app).map(|i| d.entries[i].is_process()))
        .unwrap_or(false);

    // ⚠️ 这一处**不能**用 `modal_card`：它是唯一一个"危险配色"的确认框
    // （`danger_edge` 边框 + 红色标题），而 `modal_card` 给的是常规边框。
    // 那个差异是**有意义的**（用户要一眼看出这是不可撤销的操作），不是散落。
    // 但宽度仍走 `ModalSize::Narrow` —— 尺寸该统一，配色该区分。
    v_flex()
        .w_full()
        .gap_3()
        .child(
            div()
                .text_sm()
                .text_color(theme::text())
                .child(crate::util::strip_md(if is_process {
                    "面板会：① 停掉这个应用的进程；② 删掉面板铺进目标目录的那几个文件\
                     （`app.log` / `app.pid` 与上次部署的文件，**别人的东西不动**）；\
                     ③ 删掉本地的那条记录。"
                } else {
                    "面板会：删掉面板铺进目标目录的那几个文件（内容形态没有我们起的进程；\
                     镜像自带的文件、你自己放的东西都不动），然后删掉本地的那条记录。"
                })),
        )
        .child(kv_block("容器", container))
        .child(kv_block("目标目录", target))
        .child(
            div()
                .text_xs()
                .text_color(theme::danger())
                .child(crate::util::strip_md(
                    // ⚠️ 别写成"被清掉的是卷里的数据"：v0.9.10 之前那句是对的
                    //（那时确实是整目录 `find -delete`）。现在删的**只有面板自己
                    // 铺过的那几个名字**，别人的文件一个都不碰 —— 这句要说的是
                    // "删掉的那几个文件在**卷**上、删了就不会回来"，不是"清空目录"。
                    "⚠️ 这一步不可撤销。如果那个目录挂在卷上（比如 nginx 的站点目录），\
                     删掉的就是**卷里**的那几个文件 —— 卷还在，文件不会回来。",
                )),
        )
        .into_any_element()
}

// ---------------------------------------------------------------------------
// ③ 镜像 / 网络 / 卷
// ---------------------------------------------------------------------------
