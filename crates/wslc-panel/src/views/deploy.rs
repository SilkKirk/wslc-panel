//! 应用部署管理页：页头、表格、行内动作。
//!
//! 四个浮层（添加部署 / 日志 / 站点配置 / 删除确认）的**内容**在 [`dialogs`]；
//! 它们的外壳一律由官方 `Dialog` 负责（构造在 `app::shell::dialogs`）。

use super::common::{
    NoContainer, cell_link, cell_muted_full, cell_text, immediate_button, log_text, overlay_button,
    page_empty, store_warning, table_card, table_row,
};
use super::prelude::*;

mod dialogs;
// 四个浮层的内容住在 `deploy/dialogs.rs`（`deploy.rs` 与 `deploy/` 目录可以并存）。
// `views/mod.rs` 从这里拿它们，所以必须再导出 —— 顺带让本文件里那些
// [`deploy_remove_confirm_body`] 之类的文档链接仍然能解析。
pub(crate) use dialogs::{
    deploy_dialog_body, deploy_logs_body, deploy_remove_confirm_body, deploy_site_conf_body,
};

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
pub(crate) fn deploy_page(shell: &Shell, _cx: &App, entity: &Entity<Shell>) -> AnyElement {
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

    // 存档读坏了要说出来 —— 不然用户会以为"我部署过的东西全没了"。
    // `AppState::deploy_store_error` 存在的唯一目的就是这一行。
    // 另一半在 `views::database` 的 `creds_warning`（账号存档读坏了）——
    // 两个存档各有一条，别只补一边。
    let store_problem = shell
        .state
        .deploy_store_error
        .as_ref()
        .map(|error| format!("部署存档读不出来：{error}"));

    v_flex()
        .w_full()
        .gap_3()
        .child(header)
        .children(store_problem.map(|text| store_warning("deploy-store", text)))
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
    failure: &wslc_panel_core::state::DeployFailure,
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
                .child(wslc_panel_core::util::strip_md(&failure.summary())),
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
pub(crate) fn deploy_failure_banner(failure: &wslc_panel_core::state::DeployFailure) -> AnyElement {
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
                .child(wslc_panel_core::util::strip_md(&failure.summary())),
        )
        .child(log_text(Some(&failure.detail), "deploy-error-detail"))
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
                    .child(wslc_panel_core::util::strip_md(deploy.runtime.note())),
            )
            .into_any_element()
    });

    v_flex()
        .w_full()
        .gap_2()
        .child(div().text_xs().text_color(theme::text_dim()).child(
            wslc_panel_core::util::strip_md(&format!(
                "共 {} 条部署。「部署过什么」来自面板自己的存档（容器停着也看得到）；\
                     「现在跑没跑」是**问容器**得来的。",
                deploy.entries.len()
            )),
        ))
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
            format!("读不到状态（{}）", wslc_panel_core::util::strip_md(error)),
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
