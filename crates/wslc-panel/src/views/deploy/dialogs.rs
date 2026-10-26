//! 「部署管理」页的四个浮层**内容**（添加部署 / 日志 / 站点配置 / 删除确认）。
//!
//! 外壳一律由官方 `Dialog` 负责（构造在 `app::shell::dialogs`）—— 本文件只产出
//! 内容，不碰遮罩、宽度、标题与退出路径。

use super::super::common::{busy_text, disclosure, empty_state, form_field, kv_block, log_text};
use super::super::prelude::*;

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
pub(crate) fn deploy_dialog_body(shell: &Shell, entity: &Entity<Shell>, cx: &App) -> AnyElement {
    let Some(form) = shell.forms.deploy_form.as_ref() else {
        return div().into_any_element();
    };
    let Some(deploy) = shell.state.deploy.as_ref() else {
        return div().into_any_element();
    };
    let runtime = deploy.runtime;
    let re_deploy = form.editing.is_some();

    // 站点块出现后弹窗会高出一截；带 id 的滚动容器（AGENTS §7.5）。
    // 弹窗宽度 800 在 `dialogs.rs` 的 `DialogKey::DeployForm` 那个臂里给 ——
    // 这个弹窗有「目标目录」「启动命令」这类长路径，560 宽时几乎全是省略号。
    let card = v_flex()
        .w_full()
        .gap_3()
        .id("deploy-dialog-scroll")
        .max_h(px(760.))
        .overflow_y_scroll()
        // ⚠️ 标题**不在这里画**：`Dialog::title()` 里已经是"添加部署 · Nginx" /
        // "重新部署 app"。这里只留那句"目标容器"。
        .child(
            div()
                .text_xs()
                .text_color(theme::text_dim())
                .child(format!("目标容器 {}", deploy.container)),
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
                .child(wslc_panel_core::util::strip_md(
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
        .child(div().text_xs().text_color(theme::text_dim()).child(
            wslc_panel_core::util::strip_md(
                "命令原样交给 /bin/sh -c，支持管道与 &&；日志位于目标目录的 app.log。",
            ),
        ))
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
        );

    // 底部按钮**不在这里**：`Dialog` 的 footer 由 `app::shell::dialogs` 统一挂。
    // 这里再画一份就是"两排按钮 + 两套同 id 的元素" —— v0.20.1 的 bug。

    card.into_any_element()
}

/// 应用日志对话框的**内容**（外壳/标题/底部由 Dialog 负责）。
///
/// 只读：一个输入框都没有，所以状态是纯数据。
pub(crate) fn deploy_logs_body(logs: &DeployLogs) -> AnyElement {
    let body: AnyElement = if logs.busy {
        busy_text("正在读取…")
    } else {
        log_text(logs.text.as_deref(), "deploy-logs-text")
    };

    v_flex()
        .w_full()
        .gap_3()
        .child(div().text_xs().text_color(theme::text_dim()).child(
            wslc_panel_core::util::strip_md(
                "是这个应用**自己**的输出（启动时把它重定向到了目标目录的 \
                 app.log）—— 不是容器主进程的日志，那个在「日志」按钮里。",
            ),
        ))
        .child(body)
        .into_any_element()
}

/// 「站点配置」只读浮层。
///
/// 文本**不**过 `strip_md`、不逐行重排：conf 里的缩进 / `#` / 空行都是有意义的，
/// 逐行处理只会让用户没法对照 `nginx -t` 的错误行。
pub(crate) fn deploy_site_conf_body(conf: &DeploySiteConf) -> AnyElement {
    let body: AnyElement = if conf.busy {
        busy_text("正在读取…")
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
        .child(div().text_xs().text_color(theme::text_dim()).child(
            wslc_panel_core::util::strip_md(
                "容器里的 `/etc/nginx/conf.d/00-wslc-panel-<应用名>.conf`（只读）。\
                 重新部署会整份覆盖；要改就改部署弹窗里的站点区，别直接改这个文件。",
            ),
        ))
        .child(div().text_xs().text_color(theme::text_dim()).child(
            wslc_panel_core::util::strip_md(
                "容器重建后这份 conf 会丢 —— 可以点「重写配置」补回来\
                 （只写 conf，不重新铺文件 / 不重新接网络）。",
            ),
        ))
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

    // ⚠️ 这一处**不用** `danger_button` 那套红色标题：标题由 `Dialog` 给
    // （`DialogKey::DeployRemoveConfirm`）。危险感靠**正文说清代价**表达 ——
    // 那个差异是**有意义的**（用户要一眼看出这是不可撤销的操作），不是散落。
    // 宽度交给 `Dialog` 的 `.w(560.)` —— 尺寸该统一，配色该区分。
    v_flex()
        .w_full()
        .gap_3()
        .child(
            div()
                .text_sm()
                .text_color(theme::text())
                .child(wslc_panel_core::util::strip_md(if is_process {
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
                .child(wslc_panel_core::util::strip_md(
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
