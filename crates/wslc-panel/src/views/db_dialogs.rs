//! 数据库的弹窗与进度浮层：新建库 / 建用户 / 改密 / 授权 / 备份 / 导入 / 基础信息。

use super::common::{
    ModalSize, cell_muted, cell_text_full, disclosure, form_field, modal_card, modal_footer,
    modal_scrim, modal_title, overlay_button, page_empty, table_header, table_row,
};
use super::database::db_connection_card;
use super::prelude::*;

/// 「基础信息」浮层：连接卡（用户名 / 密码 + 那两段说明）。
///
/// # 为什么从页面上搬进浮层
///
/// 原来连接卡常驻页面顶部（`AGENTS.md` §5 的"同一动作只给一个入口"当时是
/// 按"它描述这一页现在连的是谁、扫一眼就该知道"来定的）。实机反馈
/// **"不用进来就在上面占用空间"** —— 一页里最有价值的是那两张表，
/// 而占掉顶部 ~250px 的是一份**很少改**的连接信息。
///
/// 取舍：现在要"确认自己在用哪个账号"得点一下按钮。为了不让这一步变成
/// 盲区，页头的按钮就写着「基础信息」，而**连接失败时**页面上的错误条
/// 与「重新读取」仍然直接把注意力引到它上面。
pub fn db_info_overlay(
    db: &DbState,
    form: &DbPageForm,
    entity: &Entity<Shell>,
    cx: &App,
) -> AnyElement {
    let close = {
        let entity = entity.clone();
        Button::new("db-info-close")
            .label("关闭")
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.close_db_info(cx));
            })
    };

    modal_scrim(
        "db-info",
        modal_card(ModalSize::Medium)
            .id("db-info-card")
            .max_h(px(560.))
            .overflow_y_scroll()
            .child(
                v_flex()
                    .gap_1()
                    .child(modal_title("基础信息"))
                    .child(div().text_xs().text_color(theme::text_dim()).child(format!(
                        "容器 {} · {} · {} · 客户端 {}",
                        db.container,
                        db.engine_label(),
                        db.image,
                        db.client_version.as_deref().unwrap_or(db.client.as_str()),
                    ))),
            )
            // 连接卡本体照旧用它自己那份渲染（表单 + 说明都在里面）
            .child(db_connection_card(db, form, entity, cx))
            .child(h_flex().w_full().justify_end().child(close)),
    )
    .into_any_element()
}

/// 「数据库管理」页的动作弹窗（新建库 / 新建用户 / 改密码 / 授权）。
///
/// 四种动作共用一个弹窗：同时只可能开一个，而且它们共用"用户名 / 主机 / 密码 /
/// 库名"那几格。哪种动作决定**画哪几格**，也决定提交时拼哪条 SQL
/// （拼装在 `app.rs::submit_db_dialog` 里，用的是 `wslc-core` 的纯函数）。
///
/// ⚠️ 提交前的校验（标识符白名单）在 `submit_db_dialog` 里：不合法就**不关弹窗**，
/// 只给一条提示 —— 用户就在那一格上，改一个字比重打一遍强。
pub fn db_dialog_overlay(shell: &Shell, entity: &Entity<Shell>, cx: &App) -> AnyElement {
    let Some(dialog) = shell.db_dialog.as_ref() else {
        return div().into_any_element();
    };

    let kind = dialog.kind.clone();
    let locked_user = matches!(
        kind,
        DbDialogKind::AlterPassword { .. } | DbDialogKind::Grant { .. }
    );

    // 按动作画不同的字段
    let mut fields: Vec<AnyElement> = Vec::new();
    match &kind {
        DbDialogKind::NewDatabase => {
            fields.push(form_field(
                "db-dialog-name",
                "库名",
                &dialog.database,
                cx,
                true,
            ));
            fields.push(
                h_flex()
                    .w_full()
                    .gap_3()
                    .items_center()
                    .child(
                        Checkbox::new("db-dialog-create-user")
                            .label("同时创建用户并授权")
                            .checked(dialog.create_user)
                            .on_change({
                                let entity = entity.clone();
                                move |checked: &bool, _, cx| {
                                    let create_user = *checked;
                                    entity.update(cx, |shell, cx| {
                                        shell.set_db_create_user(create_user, cx)
                                    });
                                }
                            }),
                    )
                    .children(dialog.create_user.then(|| {
                        div()
                            .text_xs()
                            .text_color(theme::text_dim())
                            .child("主机 %（允许远程连接）")
                    }))
                    .into_any_element(),
            );
            if dialog.create_user {
                // 用户名保持手填；建议只在明确点击后写入，不随库名自动改动。
                let database = dialog.database.read(cx).value().trim().to_owned();
                let hint = if database.is_empty() {
                    "先填写库名，再使用建议用户名".to_owned()
                } else {
                    format!("填入 {}", mysql_model::default_user_for(&database))
                };
                let suggest = {
                    let entity = entity.clone();
                    Button::new("db-dialog-suggest-user")
                        .label("用建议值")
                        .tooltip(hint)
                        .small()
                        .on_click(move |_, window, cx| {
                            entity.update(cx, |shell, cx| shell.fill_db_username(window, cx));
                        })
                };
                fields.push(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .items_end()
                        .child(div().flex_1().min_w_0().child(form_field(
                            "db-dialog-db-user",
                            "数据库用户名",
                            &dialog.db_user,
                            cx,
                            true,
                        )))
                        .child(suggest)
                        .into_any_element(),
                );
                fields.push(form_field(
                    "db-dialog-password",
                    "密码（已随机生成，可修改）",
                    &dialog.password,
                    cx,
                    true,
                ));
            }
            let charset = dialog.charset.read(cx).value().trim().to_owned();
            let collation = dialog.collation.read(cx).value().trim().to_owned();
            let summary = if charset.is_empty() && collation.is_empty() {
                "字符集与排序规则 · 服务器默认".to_owned()
            } else {
                format!(
                    "字符集 {} · 排序规则 {}",
                    if charset.is_empty() {
                        "服务器默认"
                    } else {
                        charset.as_str()
                    },
                    if collation.is_empty() {
                        "默认"
                    } else {
                        collation.as_str()
                    }
                )
            };
            fields.push(disclosure(
                UiSection::DbEncoding,
                summary,
                &shell.state,
                entity,
                h_flex()
                    .w_full()
                    .gap_3()
                    .child(form_field(
                        "db-dialog-charset",
                        "字符集（可留空）",
                        &dialog.charset,
                        cx,
                        false,
                    ))
                    .child(form_field(
                        "db-dialog-collation",
                        "排序规则（可留空）",
                        &dialog.collation,
                        cx,
                        false,
                    )),
            ));
        }
        DbDialogKind::NewUser => {
            fields.push(
                h_flex()
                    .w_full()
                    .gap_3()
                    .child(form_field(
                        "db-dialog-user",
                        "用户名",
                        &dialog.user,
                        cx,
                        false,
                    ))
                    .child(form_field(
                        "db-dialog-host",
                        "主机",
                        &dialog.host,
                        cx,
                        false,
                    ))
                    .into_any_element(),
            );
            fields.push(form_field(
                "db-dialog-password",
                "密码",
                &dialog.password,
                cx,
                true,
            ));
            fields.push(grant_db_field(
                dialog,
                "db-dialog-grant-db",
                "授权到哪个库（可留空）",
            ));
            fields.push(grant_kind_row(dialog.grant, entity));
        }
        DbDialogKind::AlterPassword { user, host } => {
            fields.push(
                div()
                    .text_xs()
                    .text_color(theme::text_dim())
                    .child(format!(
                        "账号：'{user}'@'{host}'（用户名和主机不能在这里改）"
                    ))
                    .into_any_element(),
            );
            fields.push(form_field(
                "db-dialog-password",
                "新密码",
                &dialog.password,
                cx,
                true,
            ));
        }
        DbDialogKind::Grant { user, host } => {
            fields.push(
                div()
                    .text_xs()
                    .text_color(theme::text_dim())
                    .child(format!("账号：'{user}'@'{host}'"))
                    .into_any_element(),
            );
            fields.push(grant_db_field(dialog, "db-dialog-grant-db", "授权到哪个库"));
            fields.push(grant_kind_row(dialog.grant, entity));
        }
        DbDialogKind::ImportSql { database } => {
            // 目标库**写死**（就是数据库列表上被点的那一行）：不给输入框，
            // 也就没有"填错库"这件事。要导到别的库，去点那一行的按钮。
            fields.push(
                div()
                    .text_xs()
                    .text_color(theme::text_dim())
                    .child(format!("导入到：{database}"))
                    .into_any_element(),
            );
            // 文件那一格 + 「选择文件…」按钮并排：路径可以直接粘（用户常从
            // 别处拷过来），也可以点按钮挑 —— 两条路都通。
            let pick = {
                let entity = entity.clone();
                Button::new("db-import-pick")
                    .label("选择文件…")
                    .small()
                    .on_click(move |_, window, cx| {
                        entity.update(cx, |shell, cx| shell.pick_import_file(window, cx));
                    })
            };
            fields.push(
                h_flex()
                    .w_full()
                    .gap_2()
                    // ⚠️ 用 `items_center` 而不是 `items_end`：后者在本仓库
                    // 没有先例（`items_center` 到处都是），而本机**没有 Rust
                    // 工具链**、只能靠 CI 验证 —— 不值得为一点对齐赌一轮 15 分钟。
                    .items_center()
                    .child(div().flex_1().child(form_field(
                        "db-import-path",
                        "本机的 .sql 文件",
                        &dialog.import_path,
                        cx,
                        false,
                    )))
                    .child(pick)
                    .into_any_element(),
            );
            fields.push(
                div()
                    .text_xs()
                    .text_color(theme::danger())
                    .child(
                        "文件里的语句会被原样执行 —— 它可能建表、改数据，\
                         甚至删掉同名的表。导入前请确认这份文件是你想要的那份。",
                    )
                    .into_any_element(),
            );
        }
    }

    // 用户名 / 主机在"改密码 / 授权"里是**锁定**的：那样画出来才看得出
    // 这条动作作用在谁身上（而不是让用户以为还能改）。
    if locked_user {
        fields.insert(
            0,
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child("下面这条语句作用在上面那个账号上。")
                .into_any_element(),
        );
    }

    let cancel = {
        let entity = entity.clone();
        Button::new("db-dialog-cancel")
            .label("取消")
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.close_db_dialog(cx));
            })
    };
    let confirm = {
        let entity = entity.clone();
        // 导入不是一条 SQL（它是"送个文件进去让 mysql 读"），走自己的提交路径；
        // 其余四种都是"拼一条 SQL 然后执行"。
        let is_import = kind.is_import();
        Button::new("db-dialog-confirm")
            .label(kind.confirm_label())
            .small()
            .primary()
            .on_click(move |_, window, cx| {
                entity.update(cx, |shell, cx| {
                    if is_import {
                        shell.start_db_import(window, cx);
                    } else {
                        shell.submit_db_dialog(window, cx);
                    }
                });
            })
    };

    modal_scrim(
        "db-dialog",
        modal_card(ModalSize::Medium)
            .id("db-dialog-card")
            .max_h(px(820.))
            .overflow_y_scroll()
            .child(
                v_flex()
                    .gap_1()
                    .child(modal_title(kind.title()))
                    .child(div().text_xs().text_color(theme::text_dim()).child(format!(
                                "在容器 {} 上执行 —— 用的是数据库页连接卡里那个账号。",
                                shell
                                    .state
                                    .db
                                    .as_ref()
                                    .map(|db| db.container.clone())
                                    .unwrap_or_default()
                            ))),
            )
            .children(fields)
            .child(modal_footer([cancel, confirm])),
    )
    .into_any_element()
}

/// 「授权到哪个库」那一格：**下拉框（选已有的库）+ 输入框（手打 / 留空）**。
///
/// # 为什么两个控件都留着
///
/// 下拉框的候选来自已读到的库列表，而那个列表可能**还没读出来**（页面刚进、
/// 读失败），也可能**不全**（普通账号读不到别人的库 —— `mysql.user` 那条
/// 已经实测会给 `ERROR 1142`，库列表是同一类权限问题）。只给下拉框就会把
/// "授权"这件事堵死在一个可能为空的清单上。
///
/// 所以：**输入框是唯一的事实来源**，下拉框选完把值写进输入框
/// （`Shell::pick_grant_database`），提交时只读输入框（`submit_db_dialog`）。
/// 这个形状和「创建容器」里"镜像引用输入框 + 本地镜像下拉框"完全一致，
/// 理由也一样（那边把下拉框定义成"从本地已有的镜像里挑一个的快捷方式"）。
///
/// ⚠️ 同步只有**单向**（下拉框 → 输入框）：反方向会在 `SelectState` 的
/// `Confirm` 事件栈里重入 `select.update`，GPUI 会 panic。代价是用户手改输入框
/// 之后下拉框可能还显示着上一次选中的库，所以在输入框下面写明"以这里为准"。
///
/// ⚠️ 订阅（选中 → 写回输入框）在 `Shell::open_db_dialog` 里建，不在这里：
/// 渲染函数每帧都跑，`subscribe_in` 放这儿就等于每帧叠一个新订阅。
///
/// # 为什么下拉框画在输入框**上面**
///
/// 用户的心智是"先挑一个常见的，挑不到再手打"；反过来的话，手打完看见下面
/// 还挂着一个"请选择"的下拉框，会以为那一格没填好。
pub(crate) fn grant_db_field(
    dialog: &DbDialog,
    id: &'static str,
    label: &'static str,
) -> AnyElement {
    v_flex()
        .w_full()
        .gap_1()
        .child(div().text_xs().text_color(theme::text_dim()).child(label))
        .child(
            Select::new(&dialog.grant_db_select)
                // `placeholder` 是"**还没选**"的文案，不是空状态（v0.6.5 踩过：
                // 空状态写进 placeholder，于是展开明明有候选、触发器却写着
                // "本地还没有镜像"）。这里说的正是"还没从列表里选"。
                .placeholder("从已有的库里选一个…（也可以在下面手打）")
                // 候选菜单**默认和触发器一样宽**（`menu_width` 默认
                // `Length::Auto`）；这一格在 680px 的卡片里还要和标签对齐，
                // 不给宽一点就是每个候选都挂省略号。
                .menu_width(px(420.))
                .menu_max_h(px(320.))
                .empty(|_, _| {
                    div()
                        .px_3()
                        .py_2()
                        .text_sm()
                        .text_color(theme::text_muted())
                        .child("还没读到库列表 —— 直接在下面手打库名")
                })
                .w_full(),
        )
        .child(Input::new(&dialog.grant_db).id(id).w_full())
        // 这句话是**功能**不是装饰：两格都可能显示得不一样（单向同步的代价），
        // 不说清楚用户就得猜提交时用的是哪一个。
        //
        // ⚠️ 后半句跟着动作走：「新建用户」可以留空（先建账号，以后再授权，
        // `submit_db_dialog` 那边允许），「授权」**不能**留空（那边会报
        // "请填要授权给哪个数据库"）。两处都写"留空 = 不授权"就是在
        // 一个必填的弹窗里说它可以留空 —— 用户会照着做然后吃到一条错误。
        //
        // 过 `strip_md` 是守规矩而不是当下需要：`AGENTS.md` §5 那条规矩是
        // 给"以后加粗的人"的守卫。
        .child({
            // 匹配**引用**：`DbDialogKind` 不是 `Copy`（里面带着 `String`），
            // 按值匹配在共享引用后面借用不过。
            let suffix = match &dialog.kind {
                DbDialogKind::Grant { .. } => "必填。",
                _ => "留空 = 先不授权。",
            };
            div()
                .text_xs()
                .text_color(theme::text_dim())
                .child(crate::util::strip_md(&format!(
                    "以输入框里的库名为准；下拉框只是帮你填入。{suffix}"
                )))
        })
        .into_any_element()
}

/// 授权级别那一排按钮（选中态用 `primary` 配色，和别处一致）。
pub(crate) fn grant_kind_row(current: GrantKind, entity: &Entity<Shell>) -> AnyElement {
    let buttons: Vec<AnyElement> = GrantKind::ALL
        .iter()
        .map(|kind| {
            let kind = *kind;
            let entity = entity.clone();
            let mut button = Button::new(SharedString::from(format!("db-grant-{kind:?}")))
                .label(kind.label())
                .small()
                .on_click(move |_, _, cx| {
                    entity.update(cx, |shell, cx| shell.set_db_grant(kind, cx));
                });
            if current == kind {
                button = button.primary();
            }
            button.into_any_element()
        })
        .collect();

    h_flex()
        .w_full()
        .gap_2()
        .items_center()
        .child(div().text_xs().text_color(theme::text_dim()).child("权限"))
        .children(buttons)
        .into_any_element()
}

/// 备份进度浮层（备份跑着的时候盖在页面上）。
///
/// 没在备份时返回空 —— 进度不是常驻的（跑完就关）。
/// ⚠️ 这一版**没有实时进度**（dump 输出直接落在容器里，见 `cmd::mysql`
/// 的模块注释），所以只有一句"备份中…"和目标文件路径。
///
/// 「放到后台」= 只收遮罩，**不**打断备份（这一版本来也打断不了，
/// 见 `Shell::cancel_db_backup`）。按钮不叫「知道了」：那个名字会让人以为
/// 它取消的是备份。
pub fn db_backup_progress(db: &DbState, entity: &Entity<Shell>) -> AnyElement {
    let Some(state) = db.backup.as_ref() else {
        return div().into_any_element();
    };
    if state.dismissed {
        return div().into_any_element();
    }

    let dismiss = {
        let entity = entity.clone();
        Button::new("db-backup-dismiss")
            .label("放到后台")
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.cancel_db_backup(cx));
            })
    };

    modal_scrim(
        "db-backup-progress",
        modal_card(ModalSize::Narrow)
            .child(
                v_flex()
                    .gap_1()
                    .child(modal_title(format!("正在备份 {}", state.database)))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::text_dim())
                            .child(format!("目标文件：{}", state.windows_path.display())),
                    ),
            )
            .child(div().text_sm().text_color(theme::text_muted()).child(
                "在容器里执行 mysqldump，一般几十秒内结束（大库会久一些）。\
                     完成后会在 Windows 侧存一份档案，容器内 /tmp 也留一份用于恢复。",
            ))
            .child(
                h_flex()
                    .w_full()
                    .justify_end()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme::text_muted())
                            .child("备份中…"),
                    )
                    .child(dismiss),
            ),
    )
    .into_any_element()
}

/// 导入进度浮层（导入跑着的时候盖在页面上）。
///
/// 没在导入时返回空。和备份那个一样：**没有实时百分比**（`container cp` 和
/// `mysql` 都不吐进度），但"点下去之后界面上有变化"这条必须有
/// （`AGENTS.md` §7.1）—— 所以起码要显示"在导哪个文件、进哪个库"。
///
/// # 为什么必须有「放到后台」
///
/// 见函数体里那段注释：遮罩挡住整页 + 导入是分钟级的，没有关闭途径时用户
/// 会被锁在屏幕前。收起来不等于取消。
pub fn db_import_progress(db: &DbState, entity: &Entity<Shell>) -> AnyElement {
    let Some(state) = db.import.as_ref() else {
        return div().into_any_element();
    };
    // 用户点过「放到后台」—— 浮层收起来，但导入照跑（见 `ImportState::dismissed`）。
    if state.dismissed {
        return div().into_any_element();
    }

    let target = state
        .database
        .as_deref()
        .map(|d| format!("导入到 {d}"))
        .unwrap_or_else(|| "导入到文件里指定的库".to_owned());

    // ⚠️ 这个按钮是**必需的**：浮层带 `.occlude()`，遮罩挡住整页，而导入是
    // 分钟级的（几百 MB 的 dump / 容器刚停时 `container cp` 会卡住）。
    // 没有关闭途径时，那段时间用户既看不到进度、又点不到任何按钮 ——
    // 连"去容器页看看它是不是挂了"都做不到。
    // 收起来**不等于取消**：导入继续跑，完成后照样发提示、照样重读列表。
    let dismiss = {
        let entity = entity.clone();
        Button::new("db-import-dismiss")
            .label("放到后台")
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.dismiss_db_import(cx));
            })
    };

    modal_scrim(
        "db-import-progress",
        modal_card(ModalSize::Narrow)
            .child(
                v_flex()
                    .gap_1()
                    .child(modal_title(format!("正在导入 {}", state.file_name)))
                    .child(div().text_xs().text_color(theme::text_dim()).child(target)),
            )
            .child(div().text_sm().text_color(theme::text_muted()).child(
                "先把文件送进容器，再让容器里的 mysql 读它。\
                     大文件会慢一些 —— 完成后这里会自动关掉，列表也会重读一次。",
            ))
            .child(
                h_flex()
                    .w_full()
                    .justify_end()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme::text_muted())
                            .child("导入中…"),
                    )
                    .child(dismiss),
            ),
    )
    .into_any_element()
}

/// 某个库的**备份列表**浮层（v0.8）——也是这个库「备份 / 恢复」的唯一入口。
///
/// `credentials` 是 `AppState` 上那份账号存档：这个浮层要在顶部写清
/// **这一行的备份 / 恢复会用哪个账号**（面板记的那份，还是连接卡上那份），
/// 而那句话只有查了存档才说得准。
///
/// 收 `shell` 而不收 `DbPageForm`：这个浮层里的每个按钮都**按行走**
/// （`Shell::creds_for_row`），一个输入框都不读 —— 收个 `&DbPageForm`
/// 只会让人以为它和连接卡有关。
pub fn db_backup_dialog_overlay(
    db: &DbState,
    credentials: &wslc_panel_core::dbcreds::DbCredentialStore,
    shell: &Shell,
    entity: &Entity<Shell>,
    cx: &App,
) -> AnyElement {
    let Some(database) = db.backup_dialog.as_ref() else {
        return div().into_any_element();
    };
    let database = database.clone();

    // 「立即备份」——**这个浮层唯一的创建入口**。
    //
    // ⚠️ 它曾经不存在：`Shell::start_db_backup` 从 v0.8 写出来就**没有任何调用点**
    // （全仓库只此一份定义），而列表行上那个「备份」按钮打开的是**本浮层**，
    // 浮层里只有恢复 / 打开位置 / 删除。于是"备份"这个功能整个是死的：
    // 弹窗永远写着"还没有备份"，而且永远不会有第一份 —— 实机报过来就是这个
    // （"怎么没有一个按钮点击后可以把这个库直接整个备份导出来"）。
    // 入口只此一处：行上的「备份」是进这个浮层，创建在这里（同一动作不给两个入口）。
    let start: AnyElement = if db.backup.is_some() {
        // 跑着的时候把按钮换成这一句（项目里不用 `.disabled()`）
        div()
            .text_sm()
            .text_color(theme::text_muted())
            .child("备份中…")
            .into_any_element()
    } else {
        overlay_button(
            "db-backup-start",
            "立即备份",
            {
                let database = database.clone();
                move |shell, window, cx| shell.start_db_backup(database.clone(), window, cx)
            },
            entity,
        )
    };

    // 这个浮层里的**每个**动作都钉在这个库上：备份、恢复，以及上面那个
    // 「立即备份」。账号用的就是这一行的（`Shell::creds_for_row`）——
    // 面板记过就用记的那份，没记过才退回连接卡。
    //
    // 为什么必须写出来：`Access denied for user 'root'@'localhost'` 是这里
    // 最常见的一条报错，而用户看不到我们到底用了谁。卡藏在页头的
    // 「基础信息」后面，写一句就等于把"该去改哪儿"指出来。
    let account: AnyElement = {
        let row = shell.creds_for_row(&db.container, &database, cx);
        let recorded = credentials.get(&db.container, &database);
        let text = if let Some(cred) = recorded {
            format!(
                "备份 / 恢复都用面板记的账号 {}（这个库自己的账号，建库时授的是 \
                 `GRANT ALL ON {database}.*`）。要换就改「基础信息」里那张卡 —— \
                 但那样只影响**没记账号**的库。",
                cred.user
            )
        } else if row.password.trim().is_empty() {
            format!(
                "这个库不是面板建的（没有记账号），会退回用「基础信息」卡上的 {}，\
                 而那张卡的密码现在是空的 —— 先去补上，否则 mysqldump 会报 Access denied。",
                row.user
            )
        } else {
            format!(
                "这个库不是面板建的（没有记账号），会退回用「基础信息」卡上的 {}；\
                 要换账号就去那里改。",
                row.user
            )
        };
        div()
            .text_xs()
            .text_color(theme::text_dim())
            .child(crate::util::strip_md(&text))
            .into_any_element()
    };

    let rows: Vec<TableRow> = db
        .backups
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.database == database)
        .map(|(index, entry)| {
            let restore = Button::new(SharedString::from(format!("db-restore-{index}")))
                .label("恢复")
                .small()
                .primary()
                .on_click({
                    let entity = entity.clone();
                    move |_, _, cx| {
                        entity.update(cx, |shell, cx| shell.restore_db_backup(index, cx));
                    }
                })
                .into_any_element();
            let open_dir = overlay_button(
                &format!("db-open-backup-{index}"),
                "打开位置",
                {
                    let dir = entry.path.parent().map(|p| p.to_path_buf());
                    move |shell, _, cx| shell.reveal_backup_dir(dir.clone(), cx)
                },
                entity,
            );
            let delete = {
                let entity = entity.clone();
                Button::new(SharedString::from(format!("db-delete-backup-{index}")))
                    .label("删除")
                    .small()
                    .on_click(move |_, _, cx| {
                        entity.update(cx, |shell, cx| shell.delete_db_backup(index, cx));
                    })
                    .into_any_element()
            };
            table_row(
                BACKUP_COLUMNS,
                vec![
                    cell_text_full(entry.file_name.clone(), entry.file_name.clone()),
                    cell_muted(format_bytes(entry.bytes)),
                    cell_muted(entry.stamp.clone()),
                    h_flex()
                        .gap_2()
                        .child(restore)
                        .child(open_dir)
                        .child(delete)
                        .into_any_element(),
                ],
            )
        })
        .collect();

    let close = {
        let entity = entity.clone();
        Button::new("db-backup-close")
            .label("关闭")
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.close_db_backup_dialog(cx));
            })
    };

    modal_scrim(
        "db-backup-dialog",
        modal_card(ModalSize::Wide)
            .id("db-backup-dialog-card")
            .max_h(px(560.))
            .overflow_y_scroll()
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        h_flex()
                            .w_full()
                            .justify_between()
                            .items_center()
                            .child(modal_title(format!("备份 —— {database}")))
                            .child(start),
                    )
                    .child(account)
                    .child(div().text_xs().text_color(theme::text_dim()).child(
                        crate::util::strip_md(
                            "列表是 Windows 侧的存档（容器删了它们还在），\
                                     也是**恢复的输入**：恢复时会把这份文件送进容器再导入，\
                                     所以容器重建过也照样能恢复。\
                                     恢复**覆盖**这个库现有的全部数据。",
                        ),
                    )),
            )
            .child(if rows.is_empty() {
                // 备份列表整块是空的 → 官方 `Empty`
                page_empty("还没有备份 —— 点右上角的「立即备份」生成第一份")
            } else {
                // 和 8 个列表页同一套官方组件，只有底色不同：
                // 这张表画在**弹窗卡**（`bg_card`）里，得用 `theme::bg()` 才有对比，
                // 所以没有走 `common::table_card`（它的底色写死 `bg_card`）。
                //
                // ⚠️ 必须包 `TableBody`：官方 `TableRow` 只在 `ix > 0` 时画**上边框**，
                // 而 `ix` 是 `TableBody` 在 enumerate 时通过 `with_ix` 喂进去的。
                // 直接把 `TableRow` 丢进 `v_flex` 的话所有行的 ix 都是 0 →
                // 行与行之间的分隔线会**整片消失**（不是样式差一点，是线没了）。
                Table::new()
                    .rounded_lg()
                    .bg(theme::bg())
                    .border_1()
                    .border_color(theme::border())
                    .child(
                        TableHeader::new()
                            .bg(theme::bg())
                            .child(table_header(BACKUP_COLUMNS)),
                    )
                    .child(TableBody::new().children(rows))
                    .into_any_element()
            })
            .child(h_flex().w_full().justify_end().child(close)),
    )
    .into_any_element()
}

// ---------------------------------------------------------------------------
// ②c 容器里的应用部署管理（nginx / Java / Node.js / Go，v0.9）
// ---------------------------------------------------------------------------
