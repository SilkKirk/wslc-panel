//! ②b 数据库页：连接卡、库 / 用户两张表、tab 行。
//!
//! 弹窗与进度浮层在 [`super::db_dialogs`]。

use super::common::{
    NoContainer, REOPEN_DB_PAGE, badge, card, cell_muted, cell_text, danger_button, empty_state,
    form_field, log_text, overlay_button, page_empty, tab_bar, table_card, table_row,
};
use super::prelude::*;

/// 「数据库管理」页。
///
/// # 它为什么不在侧边栏
///
/// 它服务**一个**容器（`AppState::db` 里记着是哪个），所以顶部有
/// 「← 返回容器」—— 和「添加实例」是同一类页面（只能从列表上的按钮进）。
///
/// # 三种形态
///
/// 1. **探测就失败**（不是 MySQL / 容器没运行 / 镜像里没客户端）：
///    只画那张说明卡片。这时候再摆一排空表和连接卡只会干扰判断。
/// 2. **正常**：连接卡 + tab（数据库 / 用户 / SQL 控制台）。
/// 3. **还没读到数据**：表格的位置放一句"正在读取…"。
///
/// # 密码
///
/// 连接卡上的密码是**明文**的（`DbPageForm` 的注释里写了为什么这一版不做掩码），
/// 而它只存在内存里：不写进 `prefs.json`、不进日志（`MysqlCreds` 的 `Debug`
/// 是手写脱敏的）、也不进错误消息（出口是 `cmd::mysql::sanitize`）。
pub fn database(shell: &Shell, cx: &App, entity: &Entity<Shell>) -> AnyElement {
    // 这一页正文一个输入框都没有（连接卡 → 「基础信息」浮层、控制台 → 每行的
    // 「SQL」），但 `cx` **真的要用**：列表行上的「删库」要把**执行时**的账号
    // 装进 `PendingAction`，而那份账号是按行取的（`Shell::creds_for_row`，
    // 它读的是账号存档 + 连接卡的 `InputState`）。
    //
    // ⚠️ 别改成 `db.creds`：它只在 `load_db` 里赋值一次，于是同一次会话里
    // 「备份用 djxm_user、删除用 root」—— 这条不一致是实机可复现的。

    let Some(db) = shell.state.db.as_ref() else {
        // 理论上来不到（只可能从容器行上的按钮进，那里已经建好状态了），
        // 但真到了这里就说一句人话，别画一片空白。
        return page_empty(NoContainer::Database.empty_hint());
    };
    // 表单没建好就没法渲染（输入框是懒创建的）。
    //
    // 这里**不再绑一个 `form`**：连接卡搬进浮层之后，这一页正文一个输入框
    // 都不用（`form` 在「基础信息」对话框里用；v0.12 起 SQL 控制台有自己
    // 的整页 `sql_console_page`，那儿的编辑器是 `EditorState`，也不在这儿读），
    // 绑了就是一个 unused 警告。
    if shell.db_form.is_none() {
        return empty_state(REOPEN_DB_PAGE).into_any_element();
    }

    // 页头（含「基础信息」按钮）。
    let header = db_header(db, entity);

    // ⚠️ 连接卡**不再常驻页面**：实机反馈"不用进来就在上面占用空间"。
    // 它现在在「基础信息」对话框里（点页头那个按钮打开），
    // 所以这里一个 `connecting` 都不画 —— 两处都画就等于同一份信息两个入口。
    if let Some(blocked) = db.blocked.as_ref() {
        return v_flex()
            .w_full()
            .gap_3()
            .child(header)
            .child(db_blocked_card(blocked))
            .into_any_element();
    }

    let body: AnyElement = if db.loaded {
        if !db.is_mysql_family() {
            // v0.14：PostgreSQL 目前只接了「SQL 控制台 + 表结构 + 补全」。
            // 数据库 / 用户 / 备份恢复依赖 `SHOW DATABASES`、`mysql.user`、
            // `mysqldump`，对 PostgreSQL 全都不成立 —— 与其把那些按钮画出来
            // 让用户点进去看报错，不如明说这一页支持到哪儿。
            db_postgres_card(db, entity)
        } else {
            match db.tab {
                // ⚠️ 凭据取**单一数据源**（`AppState.db_credentials`），不要从
                // `DbState` 上再挂一份克隆 —— v0.8 就是这么错的：`DbState.credentials`
                // 从没被同步过（构造函数里给的是空的），于是"建库同时建用户"
                // 明明成功、列表里那两列却永远是 `—`（实机截图抓到的）。
                DbTab::Databases => {
                    db_databases_card(db, &shell.state.db_credentials, shell, cx, entity)
                }
                DbTab::Users => db_users_card(db, shell, cx, entity),
            }
        }
    } else {
        db_loading(db.busy)
    };

    // 存档读坏了要说出来 —— 否则那两列全是 `—`，用户只会以为
    // "面板没记住我建的账号"，而真实原因是存档解析失败。
    let creds_warning: Option<AnyElement> = shell
        .state
        .db_credentials_error
        .as_ref()
        .map(|error| db_store_warning(&format!("数据库账号存档读不出来：{error}")));

    v_flex()
        .w_full()
        .gap_3()
        .child(header)
        .children(db.error.as_ref().map(db_error_banner))
        .children(creds_warning)
        .child(db_tabs(
            db.tab,
            entity,
            db.is_mysql_family(),
            // 用户列表读失败时给 tab 打个标记（横幅只画在那个 tab 里）
            db.users_error.is_some(),
        ))
        .child(body)
        .into_any_element()
}

/// 页头：容器 / 镜像 / 家族 / 客户端 + 「← 返回容器」「基础信息」「重新读取」。
pub(crate) fn db_header(db: &DbState, entity: &Entity<Shell>) -> AnyElement {
    let back = {
        let entity = entity.clone();
        Button::new("db-back")
            .label("← 返回容器")
            .small()
            .on_click(move |_, window, cx| {
                entity.update(cx, |shell, cx| shell.close_database_page(window, cx));
            })
    };

    // 连接卡（用户名 / 密码 + 说明）现在住在这个按钮后面 —— 它以前常驻页面顶部，
    // 实机反馈"不用进来就在上面占用空间"。按钮文案就叫「基础信息」，
    // 和浮层标题一致，用户点下去就知道会看到什么。
    let info = {
        let entity = entity.clone();
        Button::new("db-info")
            .label("基础信息")
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.open_db_info(cx));
            })
    };

    // 读取期间把按钮**换掉**（项目里不用 `.disabled()`）：置灰的按钮会让人
    // 反复去点，而"读取中…"说清了现在在等什么。
    let reload: AnyElement = if db.busy {
        div()
            .text_sm()
            .text_color(theme::text_muted())
            .child("读取中…")
            .into_any_element()
    } else {
        let entity = entity.clone();
        Button::new("db-reload")
            .label("重新读取")
            .small()
            .primary()
            .on_click(move |_, window, cx| {
                entity.update(cx, |shell, cx| shell.load_db(window, cx));
            })
            .into_any_element()
    };

    // 「备份中 / 导入中」的**常驻**标记。
    //
    // 它和浮层是**两件事**，不是重复：
    // - 浮层（`db_backup_progress_body` / `db_import_progress_body`，现在跑在官方 `Dialog` 栈里）会挡住整页；
    // - 这一行是**不挡任何东西**的状态说明，浮层开着时它在底下（看不见），
    //   用户点「放到后台」把遮罩收掉之后，它就是唯一的"还在跑"的证据。
    //
    // 为什么必须有：不收遮罩的话用户被锁住；收了又没有这一行的话，
    // 用户会以为已经结束了，接着去点「立即备份」或再导一次。
    // 这也正是 `AGENTS.md` §7.1 那条"界面必须有变化"的延长线 ——
    // 收起浮层是用户主动选的，但"还在跑"这个事实不能跟着消失。
    let running: Option<AnyElement> = if let Some(backup) = db.backup.as_ref() {
        Some(
            div()
                .text_xs()
                .text_color(theme::primary())
                .child(format!(
                    "正在备份 {}…（可以继续用，完成后会提示）",
                    backup.database
                ))
                .into_any_element(),
        )
    } else {
        db.import.as_ref().map(|import| {
            let target = import
                .database
                .as_deref()
                .map(|d| format!("到 {d}"))
                .unwrap_or_default();
            div()
                .text_xs()
                .text_color(theme::primary())
                .child(format!(
                    "正在导入 {}{target}…（可以继续用，完成后会提示）",
                    import.file_name
                ))
                .into_any_element()
        })
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
                        .child(format!("容器 {}", db.container)),
                )
                .child(div().text_xs().text_color(theme::text_dim()).child(format!(
                    "{} · {} · 客户端 {}",
                    db.engine_label(),
                    db.image,
                    db.client_version.as_deref().unwrap_or(db.client.as_str()),
                )))
                .children(running),
        )
        .child(h_flex().gap_2().child(back).child(info).child(reload))
        .into_any_element()
}

/// 连接卡：用户名 / 密码 + 一句说明。
///
/// ⚠️ 它**现在住在「基础信息」对话框里**（`views::db_info_body`，点页头那个
/// 按钮打开，走官方 `Dialog`），不再常驻页面顶部 —— 实机反馈"不用进来就在
/// 上面占用空间"。原先这里的注释写的是"挂在**页面**上而不是弹窗里：用户扫一眼
/// 就该知道这一页连的是谁"，那条取舍被这条反馈推翻了：那一页最有价值的是
/// 两张表，而顶部 ~250px 花在一份很少改的连接信息上。
pub(crate) fn db_connection_card(
    db: &DbState,
    form: &DbPageForm,
    entity: &Entity<Shell>,
    cx: &App,
) -> AnyElement {
    let mut body = v_flex().w_full().gap_3().child(
        h_flex()
            .w_full()
            .gap_3()
            .child(form_field("db-user", "用户名", &form.user, cx, false))
            .child(form_field(
                "db-password",
                "密码（只存在内存里）",
                &form.password,
                cx,
                false,
            )),
    );

    // 连接串（v0.16）：填了就走**原生连接**，能连远程库；留空回到"容器里执行"。
    //
    // 放在用户名/密码下面单独一行，是因为它和那两格**互斥**：
    // 填了它，用户名密码就由连接串里的人带了（那两格不再参与）。
    let apply_dsn = {
        let entity = entity.clone();
        Button::new("db-dsn-apply")
            .label("应用连接串")
            .small()
            .on_click(move |_, window, cx| {
                entity.update(cx, |shell, cx| shell.apply_db_dsn(window, cx));
            })
    };
    body = body.child(form_field(
        "db-dsn",
        "连接串（可选，填了就走原生连接）",
        &form.dsn,
        cx,
        true,
    ));
    body = body.child(
        h_flex()
            .w_full()
            .gap_2()
            .items_center()
            .child(apply_dsn)
            // 当前生效的是哪条路要说出来 —— 否则用户不知道"到底按哪份配置在跑"。
            .child(div().text_xs().text_color(theme::text_dim()).child(
                match db.dsn.as_ref().map(|d| d.as_str()) {
                    Some(dsn) => wslc_panel_core::util::strip_md(&format!(
                        "当前：**原生连接** {}",
                        // 连接串里有口令，界面上只显示协议+主机+库，不显示口令段
                        crate::native_db::redact_dsn(dsn)
                    )),
                    None => wslc_panel_core::util::strip_md("当前：在容器里执行（未使用连接串）"),
                },
            )),
    );

    if let Some(note) = db.cred_note.clone() {
        body = body.child(
            div()
                .text_xs()
                .text_color(theme::text_dim())
                .child(wslc_panel_core::util::strip_md(&note)),
        );
    }

    body = body.child(div().text_xs().text_color(theme::text_muted()).child(
        // Markdown 文案 → 渲染处统一清标记（`util::strip_md`）
        wslc_panel_core::util::strip_md(
            "不填连接串时：面板靠**容器自带的** mysql / mariadb 客户端执行 SQL —— 所以容器要在运行、\
                     镜像里得有客户端（不要求把端口发布到主机）。密码走 MYSQL_PWD 环境变量，\
                     不进命令行。填了连接串则改为**由面板自己连**（TCP），可以不经过容器、\
                     也能连远程库。改完点右上角「重新读取」生效。"
        ),
    ));

    card("连接", body).into_any_element()
}

/// 探测就失败时的说明卡（不是 MySQL / 没客户端 / 容器没运行）。
pub(crate) fn db_blocked_card(failure: &DbFailure) -> AnyElement {
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
                .child("这个容器现在管不了数据库"),
        )
        .child(
            div()
                .text_sm()
                .text_color(theme::text())
                .child(wslc_panel_core::util::strip_md(&failure.summary())),
        )
        .child(div().text_xs().text_color(theme::text_muted()).child(
            "面板不自己连 3306，而是驱动容器里的客户端 —— 所以它不依赖端口有没有\
                     发布出来，但要求那个客户端存在。想自己进去看，用「容器」页那一行\
                     「更多」菜单里的「终端」。",
        ))
        .child(log_text(Some(&failure.detail), "db-blocked-detail"))
        .into_any_element()
}

/// 上一次失败：一句"该怎么办" +（可滚动的）原文。
///
/// # 为什么这里**不**用官方的 `Alert`
///
/// 读 `alert.rs` 确认过：`Alert` 的 `message` 是 `impl Into<Text>`，而 `Text`
/// 只实现了 `From<SharedString | String | &str | TextView>` —— 接不了任意
/// `Element`（`TextView` 另需 `Entity<TextViewState>`，是为了可编辑富文本）。
///
/// 而这个横幅是**两段**（摘要 + 可滚动的原文），压成一句就没法滚动、
/// 也丢掉了"原文保留换行"这个排查价值。所以外壳保留手写，
/// 但配色仍走 `theme::danger_*`（和 `Alert` 背后是同一组色）。
pub(crate) fn db_error_banner(failure: &DbFailure) -> AnyElement {
    v_flex()
        .w_full()
        .gap_2()
        .p_3()
        .rounded_md()
        .bg(theme::danger_soft())
        .border_1()
        .border_color(theme::danger_edge())
        .child(
            div()
                .text_sm()
                .text_color(theme::danger())
                .child(wslc_panel_core::util::strip_md(&failure.summary())),
        )
        .child(log_text(Some(&failure.detail), "db-error-detail"))
        .into_any_element()
}

/// 「账号存档读不出来」这类**纯文字**警告。
///
/// 和 [`db_error_banner`] 分开：那个吃 `DbFailure`（会再给一条"该怎么办"的
/// 提示，是针对 SQL 报错分类的）；存档读坏不是 SQL 失败，没有可分类的东西，
/// 硬塞成 `DbFailure` 只会让分类器瞎猜一条不相干的建议。
pub(crate) fn db_store_warning(text: &str) -> AnyElement {
    use gpui_kit::component::alert::Alert;

    // 官方 `Alert`：底色 / 边框 / 图标 / 关闭按钮都由组件库给，而它的
    // `danger` 已经由 `theme::tune_component_theme` 换成我们自己的红 ——
    // 所以观感和其余页面一致，不用再手写「底色 + 边框 + 文字色」三件套。
    //
    // `banner()`：整条铺满、无圆角（错误横幅的形态），不是那种飘在内容里的
    // 圆角小卡片。
    Alert::error("db-store-warning", text.to_owned())
        .banner()
        .into_any_element()
}

/// 原生连接下"这几条写操作用不了"的一行说明。
///
/// # 为什么是**一行灰字**而不是警告色卡片
///
/// 早先这里是一整块 `warning_soft` 底色 + 边框的卡片。文案缩到 29 字之后
/// 那个视觉重量完全不匹配了 —— 一块黄色的块状提示会让人以为**页面出错了**，
/// 而实际只是"这个连接方式不支持某几个动作，清空输入框就回去了"。
///
/// 正式软件（DataGrip 的 "read-only connection"、TablePlus 的受限提示）
/// 在这个位置都是**一行次要文字**，不加底色。规则：
/// 只有"需要你立刻处理"的才配警告色；"知道有这回事就行"的是灰字。
///
/// 文案来自 [`DbState::write_blocked_reason`] —— 判据和文字在同一处，
/// 改了判据不会忘了改文案。
pub(crate) fn db_native_write_notice(reason: &'static str) -> AnyElement {
    div()
        .w_full()
        .text_xs()
        .text_color(theme::text_dim())
        .child(wslc_panel_core::util::strip_md(reason))
        .into_any_element()
}

/// 还没读到数据时占位。
///
/// 忙着的时候用官方 `Spinner`（转圈），不忙时是一句静态提示 ——
/// **两者的区别要一眼看得出**：只换文字（"正在读取…" / "还没有读到数据"）
/// 在截图和余光里几乎一样，用户会以为卡住了。正式软件都有那个转圈。
pub(crate) fn db_loading(busy: bool) -> AnyElement {
    use gpui_kit::component::spinner::Spinner;

    if busy {
        return v_flex()
            .w_full()
            .py_8()
            .items_center()
            .justify_center()
            .gap_2()
            .child(Spinner::new())
            .child(
                div()
                    .text_sm()
                    .text_color(theme::text_dim())
                    .child("正在读取…"),
            )
            .into_any_element();
    }
    v_flex()
        .w_full()
        .py_8()
        .items_center()
        .justify_center()
        .child(
            div()
                .text_sm()
                .text_color(theme::text_dim())
                .child("还没有读到数据 —— 点右上角「重新读取」"),
        )
        .into_any_element()
}

/// 「数据库 / 用户」两个 tab。
///
/// `mysql_family` 为假时**一个都不画**：那两个 tab 里的动作（建库 / 建用户 /
/// 授权）全是 MySQL 专有的，对 PostgreSQL 点了只会报错 —— 那一页走
/// [`db_postgres_card`]。
///
/// `users_failed` 只改**标签文案**（"用户（读取失败）"）：整页横幅已经不占
/// 地方了，用户至少要能看出点进去会看到什么。
///
/// 走 `common::tab_bar`（官方 `TabBar` + `Tab`）—— 和设置页、SQL 结果那两处
/// 是同一份接线。旧写法是"两个 `Button`、选中那个 `.primary()`"：那是把
/// **状态**画成了**动作**，而且每个 tab 都要各建一个 `on_click`。
pub(crate) fn db_tabs(
    current: DbTab,
    entity: &Entity<Shell>,
    mysql_family: bool,
    users_failed: bool,
) -> AnyElement {
    if !mysql_family {
        return div().into_any_element();
    }

    let labels: Vec<SharedString> = DbTab::ALL
        .iter()
        .map(|tab| {
            let tab = *tab;
            SharedString::from(if users_failed && tab == DbTab::Users {
                "用户（读取失败）"
            } else {
                tab.label()
            })
        })
        .collect();
    let selected = DbTab::ALL
        .iter()
        .position(|tab| *tab == current)
        .unwrap_or(0);

    tab_bar("db-tabs", labels, selected, entity, |shell, ix, _, cx| {
        if let Some(tab) = DbTab::ALL.get(ix) {
            let tab = *tab;
            shell.set_db_tab(tab, cx);
        }
    })
}

/// PostgreSQL 连接在数据库页上的说明卡（v0.14）。
///
/// # 为什么不是把那两张 MySQL 表"适配"成 PostgreSQL
///
/// 那两页的每一格都建在 MySQL 专有的东西上：`SHOW DATABASES` 的字符集 /
/// 排序规则、`mysql.user` 的 `Host` 列、`mysqldump` 的备份文件。
/// PostgreSQL 里这些概念要么不存在、要么叫别的名字（`pg_roles` /
/// `pg_dump`），"差不多的填一填"只会得到一张**看起来能用、实际全不对**的表。
///
/// 所以这一页明说支持到哪儿，并给一个真的能用的入口（SQL 控制台）。
/// 用户要的是"能跑 SQL、能看到表结构"，这两样已经通了。
pub(crate) fn db_postgres_card(db: &DbState, entity: &Entity<Shell>) -> AnyElement {
    // 打开控制台时连哪个库：优先"当前库"，否则第一个库。
    //
    // 为什么必须给一个具体的库：PostgreSQL 的元数据和 SQL 都**按库隔离**
    // （不指定库时 `psql` 连的是 `postgres` 这个维护库，用户会以为
    // "我建的表怎么不见了"）。
    let target_db = db
        .sql_database
        .clone()
        .or_else(|| db.databases.first().map(|d| d.name.clone()))
        .unwrap_or_default();

    let open = {
        let entity = entity.clone();
        let target_db = target_db.clone();
        Button::new("db-pg-console")
            .label("打开 SQL 控制台")
            .small()
            .primary()
            .on_click(move |_, window, cx| {
                let db_name = target_db.clone();
                entity.update(cx, |shell, cx| {
                    shell.open_db_console_for(db_name, window, cx)
                });
            })
    };

    let names: Vec<AnyElement> = db
        .databases
        .iter()
        .map(|d| {
            // 库名是**用户数据**，必须过 `strip_md`（`AGENTS.md` §5）。
            div()
                .text_sm()
                .text_color(theme::text())
                .child(wslc_panel_core::util::strip_md(&d.name))
                .into_any_element()
        })
        .collect();

    v_flex()
        .w_full()
        .gap_2()
        .child(
            div()
                .text_sm()
                .font_semibold()
                .text_color(theme::text())
                .child(format!(
                    "{} · 共 {} 个库",
                    db.engine_label(),
                    db.databases.len()
                )),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme::text_dim())
                .child("这一页已支持：SQL 控制台、表结构树、方言感知补全。"),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme::text_dim())
                .child("数据库 / 用户 / 备份恢复 是 MySQL 专有功能，暂未适配 PostgreSQL。"),
        )
        .child(div().h(px(4.)))
        .children(names)
        .child(div().h(px(4.)))
        .child(open)
        .into_any_element()
}

/// 库名那一格：名字 + （系统库时）一个「系统」标记。
///
/// 系统库现在**不出现在列表里**（[`db_databases_card`] 里过滤掉了），
/// 所以这个标记实际只在别处（比如将来"显示系统库"的开关）还有用 ——
/// 留着是因为"这一行是不是系统库"这件事本身仍然要能一眼看出来。
pub(crate) fn db_name_cell(entry: &wslc_core::model::DbEntry) -> AnyElement {
    h_flex()
        .gap_2()
        .min_w_0()
        .child(
            div()
                .text_sm()
                .overflow_hidden()
                .truncate()
                .child(entry.name.clone()),
        )
        .children(
            entry.is_system().then(|| {
                badge("系统", theme::text_dim(), theme::neutral_soft()).into_any_element()
            }),
        )
        .into_any_element()
}

/// 「数据库」tab。
///
/// `credentials` 是 `AppState` 上那份**唯一**的账号存档 —— 别从 `DbState`
/// 里取（那里曾经有一份从没同步过的克隆，见 [`database`] 里的注释）。
///
/// `shell` / `cx` 是给「删库」那一行的：它要 `PendingAction::DropDatabase`，
/// 而动作里的凭据必须和这一行**同一个来源**（`Shell::creds_for_row`，
/// 面板记的账号优先、没记过才用连接卡）。
pub(crate) fn db_databases_card(
    db: &DbState,
    credentials: &wslc_panel_core::dbcreds::DbCredentialStore,
    shell: &Shell,
    cx: &App,
    entity: &Entity<Shell>,
) -> AnyElement {
    // 原生连接：这一页的写操作（备份 / 恢复 / 导入 / 建库建用户 / 删库）
    // 走的是 `wslc exec <容器> <客户端>`，而那时客户端是 `native` 这个
    // 哨兵值 —— 点下去只会拿到一句指错方向的 `not found`。
    //
    // 所以按钮**不画**（`AGENTS.md` §7.5：全项目不用 `.disabled()`），
    // 并在下面给一张说清"为什么、怎么改回去、哪条还能用"的卡片。
    // 判据在 `DbState::write_blocked_reason`（那边有测试）。
    let blocked = db.write_blocked_reason();

    let new_database = {
        let entity = entity.clone();
        Button::new("db-new-database")
            .label("新建数据库…")
            .small()
            .primary()
            .on_click(move |_, window, cx| {
                entity.update(cx, |shell, cx| {
                    shell.open_db_dialog(DbDialogKind::NewDatabase, window, cx)
                });
            })
    };

    // ⚠️ 页头**没有**「导入 SQL」按钮（v0.9.2 拿掉的）：导入是"往**某个库**灌
    // 数据"的事，入口在每一行的「导入备份」上。实机反馈：
    // "数据库列表那里要一个导入备份，点击导入这个库的数据，上面不需要导入 sql"。

    // ⚠️ **系统库不显示**（用户要求）。它们仍然真实存在、也仍然能被 SQL 控制台
    // 访问 —— 只是不该混在"我的库"里（`sys` / `performance_schema` 还占着
    // 表数和占用那两列，看着像自己的数据）。数量在下面那行里报出来，
    // 免得用户以为读漏了。
    let visible: Vec<&wslc_core::model::DbEntry> =
        db.databases.iter().filter(|e| !e.is_system()).collect();
    let hidden_system = db.databases.len() - visible.len();

    let rows: Vec<TableRow> = visible
        .iter()
        // `.copied()` 把 `&&DbEntry` 变回 `&DbEntry`：`iter()` 给的是双重引用，
        // 而 `db_name_cell(entry)` 要的是 `&DbEntry` —— 函数参数不做自动解引用
        // （方法调用才会），少这一步就是 E0308。
        .copied()
        .map(|entry| {
            // 账号按**这一行**取（`Shell::creds_for_row`）：面板给 `djxm` 记过
            // `djxm_user`，那这一行四个按钮全用 `djxm_user` —— 建库时授的是
            // `GRANT ALL ON djxm.*`，连 `DROP` 都在里面，所以删库它也干得了。
            // 没记过（比如 `wslc_probe`）才退回连接卡。
            let row_creds = shell.creds_for_row(&db.container, &entry.name, cx);
            let action = PendingAction::DropDatabase {
                container: db.container.clone(),
                database: entry.name.clone(),
                client: db.client.clone(),
                creds: row_creds,
            };
            // 「用户名 / 密码」只对**面板建的库**有记录（建库时同时建的用户，
            // 记在本地存档里）。别处建的库显示 `—` —— 不猜、不编。
            let credential = credentials.get(&db.container, &entry.name);
            let backup_button = overlay_button(
                &format!("db-backup-{}", entry.name),
                "备份",
                {
                    let name = entry.name.clone();
                    move |shell, window, cx| shell.open_db_backup_dialog(name.clone(), window, cx)
                },
                entity,
            );
            // 「SQL」：开控制台并**用这一行的库 + 这一行的账号**（面板建的库
            // 才有账号记录，别的库沿用连接卡上那个）。
            let sql_button = overlay_button(
                &format!("db-sql-{}", entry.name),
                "SQL",
                {
                    let name = entry.name.clone();
                    move |shell, window, cx| shell.open_db_console_for(name.clone(), window, cx)
                },
                entity,
            );
            // 「导入备份」：把这个库的数据从一个本地 `.sql` 灌进去。
            // 目标库**就是这一行**（写进 kind 里），弹窗里没有可改的库名。
            let import_button = overlay_button(
                &format!("db-import-{}", entry.name),
                "导入备份",
                {
                    let name = entry.name.clone();
                    move |shell, window, cx| {
                        shell.open_db_dialog(
                            DbDialogKind::ImportSql {
                                database: name.clone(),
                            },
                            window,
                            cx,
                        )
                    }
                },
                entity,
            );
            // 原生连接下只留「SQL」：其余三个（备份 / 导入备份 / 删除）都要
            // 容器里的客户端。判据见 `DbState::write_blocked_reason`。
            let actions: AnyElement = if blocked.is_some() {
                h_flex().gap_2().child(sql_button).into_any_element()
            } else {
                h_flex()
                    .gap_2()
                    .child(backup_button)
                    .child(import_button)
                    .child(sql_button)
                    .child(
                        danger_button(&format!("db-drop-{}", entry.name), "删除", action, entity)
                            .into_any_element(),
                    )
                    .into_any_element()
            };
            table_row(
                DB_COLUMNS,
                vec![
                    db_name_cell(entry),
                    match credential {
                        Some(cred) => cell_text(cred.user.clone()),
                        None => cell_muted("—"),
                    },
                    match credential {
                        Some(cred) => cell_text(cred.password.clone()),
                        None => cell_muted("—"),
                    },
                    cell_muted(entry.charset.clone()),
                    cell_muted(entry.collation.clone()),
                    cell_muted(entry.tables.to_string()),
                    cell_muted(format_bytes(entry.bytes)),
                    actions,
                ],
            )
        })
        .collect();

    let count_text = if hidden_system > 0 {
        format!(
            "共 {} 个数据库（另有 {hidden_system} 个系统库未显示）",
            visible.len()
        )
    } else {
        format!("共 {} 个数据库", visible.len())
    };

    // 页头那颗按钮：原生连接时不画（建库是 DDL，走容器里的客户端）。
    //
    // 用 `Vec` 而不是 `.when(…)`：项目里没验证过 `when` 存在
    // （`views.rs` 里同类的注释，见树那一段），而 `.children(vec)` 到处都在用。
    let head_buttons: Vec<AnyElement> = if blocked.is_none() {
        vec![new_database.into_any_element()]
    } else {
        Vec::new()
    };

    let body = table_card(
        DB_COLUMNS,
        rows,
        Some(empty_state("没有读到任何数据库").into_any_element()),
    );

    v_flex()
        .w_full()
        .gap_3()
        // 原生连接的说明放在**最上面**：它解释了为什么这一页少了四个按钮，
        // 而"按钮不见了但不解释"正是 `AGENTS.md` §7.5 说的那种"以为功能没了"。
        .children(blocked.map(db_native_write_notice))
        .child(
            h_flex()
                .w_full()
                .justify_between()
                .items_center()
                // 页头只剩「新建数据库…」：库级动作（导入备份 / SQL / 备份 /
                // 删除）全在下面每一行上。
                .children(head_buttons)
                .child(
                    div()
                        .text_sm()
                        .text_color(theme::text_muted())
                        .child(count_text),
                ),
        )
        .child(body)
        .into_any_element()
}

/// 「用户」tab。
///
/// `shell` / `cx` 只给「删除」那一行用：`PendingAction::DropUser` 里的凭据
/// 必须和「改密码 / 授权」走同一个来源（`Shell::db_creds`，读连接卡）。
pub(crate) fn db_users_card(
    db: &DbState,
    shell: &Shell,
    cx: &App,
    entity: &Entity<Shell>,
) -> AnyElement {
    // 用户列表**自己**读失败（多半是权限：普通账号读不到 `mysql.user`，`ERROR 1142`）:
    // 只在这个 tab 里画。整页红色横幅会让人以为整个页面坏了，
    // 而「数据库」tab 明明好好地列着表（v0.16.0 实机反馈）。
    if let Some(failure) = db.users_error.as_ref() {
        return v_flex()
            .w_full()
            .gap_3()
            .child(db_error_banner(failure))
            .child(
                div()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(wslc_panel_core::util::strip_md(
                        "想管用户的话：去「基础信息」换成高权限账号（比如 root），再点「重新读取」。数据库列表不受影响。",
                    )),
            )
            .into_any_element();
    }

    // 原生连接：建用户 / 改密码 / 授权 / 删用户全是 DDL，走容器里的客户端。
    // `users_error` 那段已经 return 了，所以这里的判据是**第二道**入口检查
    // （第一道在 `db_databases_card`，但用户可以停在这个 tab 上再去应用连接串）。
    let blocked = db.write_blocked_reason();

    let new_user = {
        let entity = entity.clone();
        Button::new("db-new-user")
            .label("新建用户…")
            .small()
            .primary()
            .on_click(move |_, window, cx| {
                entity.update(cx, |shell, cx| {
                    shell.open_db_dialog(DbDialogKind::NewUser, window, cx)
                });
            })
    };

    // 只画业务账号：系统账号（`mysql.*` / `mariadb.sys`）和 root 不显示。
    // 判定在 `DbUser::is_hidden_by_default()`，这里只消费。
    let visible: Vec<&DbUser> = db.visible_users().collect();

    let rows: Vec<TableRow> = visible
        .iter()
        .map(|user| {
            let user_name = user.user.clone();
            let host = user.host.clone();

            let alter = overlay_button(
                &format!("db-pass-{user_name}-{host}"),
                "改密码",
                {
                    let (user_name, host) = (user_name.clone(), host.clone());
                    move |shell, window, cx| {
                        shell.open_db_dialog(
                            DbDialogKind::AlterPassword {
                                user: user_name.clone(),
                                host: host.clone(),
                            },
                            window,
                            cx,
                        )
                    }
                },
                entity,
            );
            let grant = overlay_button(
                &format!("db-grant-{user_name}-{host}"),
                "授权",
                {
                    let (user_name, host) = (user_name.clone(), host.clone());
                    move |shell, window, cx| {
                        shell.open_db_dialog(
                            DbDialogKind::Grant {
                                user: user_name.clone(),
                                host: host.clone(),
                            },
                            window,
                            cx,
                        )
                    }
                },
                entity,
            );
            let drop = danger_button(
                &format!("db-drop-user-{user_name}-{host}"),
                "删除",
                PendingAction::DropUser {
                    container: db.container.clone(),
                    user: user_name.clone(),
                    host: host.clone(),
                    client: db.client.clone(),
                    // ⚠️ 这一处**刻意**用连接卡，而不是 `creds_for_row`。
                    //
                    // 「用户」tab 每一行的对象是**账号**，不是某个库 —— 没有库名
                    // 可以拿去找面板记的那份凭据。而更重要的是权限：
                    // 面板建库时授的是 `GRANT ALL ON <库>.*`，**不含** `CREATE USER`
                    // / `DROP USER`，所以那个账号删不了人。删账号只能用一个
                    // 有全局权限的账号（root 或手工授过的人），也就是卡上那个。
                    creds: shell.db_creds(cx),
                },
                entity,
            )
            .into_any_element();

            table_row(
                DB_USER_COLUMNS,
                vec![
                    h_flex()
                        .gap_2()
                        .min_w_0()
                        .child(
                            div()
                                .text_sm()
                                .overflow_hidden()
                                .truncate()
                                .child(user.user.clone()),
                        )
                        .into_any_element(),
                    cell_muted(user.host.clone()),
                    cell_muted(user.plugin.clone()),
                    if user.super_priv {
                        badge("Y", theme::success(), theme::success_soft()).into_any_element()
                    } else {
                        cell_muted("N")
                    },
                    // 原生连接下这一列不画：改密码 / 授权 / 删除都是 DDL，
                    // 走的是容器里的客户端（判据见 `DbState::write_blocked_reason`）。
                    if blocked.is_some() {
                        cell_muted("—").into_any_element()
                    } else {
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .child(alter)
                            .child(grant)
                            .child(drop)
                            .into_any_element()
                    },
                ],
            )
        })
        .collect();

    let count_text = format!(
        "共 {} 条账号记录（同一个用户名可以有多个主机）",
        visible.len()
    );

    // 原生连接时不画「新建用户…」—— 它是一条 DDL。
    // 同上：用 `Vec` + `.children(vec)`，不用没验证过的 `.when(…)`。
    let head_buttons: Vec<AnyElement> = if blocked.is_none() {
        vec![new_user.into_any_element()]
    } else {
        Vec::new()
    };

    let body = table_card(
        DB_USER_COLUMNS,
        rows,
        Some(
            empty_state(if db.users.is_empty() {
                "没有读到任何用户（当前账号可能没有读 mysql.user 的权限）"
            } else {
                "没有业务账号（系统 / root 账号不显示）"
            })
            .into_any_element(),
        ),
    );

    v_flex()
        .w_full()
        .gap_3()
        // 和「数据库」tab 同一张说明卡：用户切到这个 tab 上才知道按钮没了，
        // 所以这里也要有（文案同一份，来自 `write_blocked_reason`）。
        .children(blocked.map(db_native_write_notice))
        .child(
            h_flex()
                .w_full()
                .justify_between()
                .items_center()
                .children(head_buttons)
                .child(
                    div()
                        .text_sm()
                        .text_color(theme::text_muted())
                        .child(count_text),
                ),
        )
        .child(body)
        .into_any_element()
}
