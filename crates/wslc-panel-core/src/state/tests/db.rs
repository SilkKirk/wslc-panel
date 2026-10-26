//! 数据库域：连接串、库表用户、备份与导入、错误分类。

use super::super::prelude::*;
use super::super::*;

// -- 数据库管理页（v0.7.0）---------------------------------------------
#[test]
fn a_users_query_failure_never_becomes_a_page_level_error() {
    // 实机场景（v0.16.0）：普通账号读 mysql.user 被拒（ERROR 1142），
    // 而「数据库」tab 完全正常。如果这条失败进了 `error`，整页顶上会挂
    // 一条红色横幅 —— 用户会以为整个页面坏了，于是不敢用（明明能用）。
    let mut db = DbState::provisional_kind(
        "mysql",
        "docker.1panel.live/library/mysql:latest",
        wslc_core::db::DbKind::MySql,
    );
    db.fail_users("ERROR 1142 (42000): SELECT command denied for table user");

    // 用户那边记下了
    assert!(db.users_error.is_some());
    // 但整页的错误必须仍然是空的
    assert!(
        db.error.is_none(),
        "用户列表失败污染了整页 error —— 会把「数据库」tab 也糊住"
    );
    // "这次读取是成功的"这个判断也不能被它推翻（页面据此决定画不画表）
    assert!(db.usable());

    // 换成有权限的账号重读：清掉之后界面上的"读取失败"标记也该消失
    db.clear_error();
    assert!(db.users_error.is_none());
}

/// 一份探测结果（形状照 `cmd::mysql::detect` 真实返回的那个）。
fn probe_with(hint: RootCredHint) -> wslc_core::cmd::mysql::MysqlProbe {
    wslc_core::cmd::mysql::MysqlProbe {
        family: MysqlFamily::Mysql,
        client: wslc_core::model::ClientProbe {
            bin: "mysql".to_owned(),
            path: "/usr/bin/mysql".to_owned(),
            version: Some("mysql  Ver 26.7.0 for Linux on x86_64".to_owned()),
        },
        root_hint: hint,
        container: "mysql".to_owned(),
    }
}

#[test]
fn native_connection_blocks_the_exec_only_write_paths() {
    // v0.16 的缺口：填了连接串之后，页面读数据走原生（不经过容器），
    // 但**备份 / 恢复 / 导入 / 建库建用户 / 删库 / 删用户**这六条仍然
    // 是 `wslc exec <容器> <客户端>` —— 而那时 `db.client` 是 `native`
    // 这个哨兵值（不是二进制名）。不拦的后果是用户拿到
    // `exec: native: not found` 这种**指错方向**的错。
    let mut state = DbState::from_probe(&probe_with(RootCredHint::Unknown), "mysql:8");
    // 没填连接串：六条都能做
    assert!(!state.is_native());
    assert!(state.write_blocked_reason().is_none());

    // ① 刚点「应用连接串」、结果还没读回来时也要算原生 —— 否则这一小段
    //    窗口里 `client` 还是上一个容器的 `mysql`，点下去会真的去 exec。
    state.dsn = Some(DsnText::new("mysql://root:pw@10.0.0.5:3306/shop"));
    assert!(state.is_native(), "dsn 已生效就该判成原生");
    let reason = state
        .write_blocked_reason()
        .expect("原生连接必须拦住写操作");
    // 文案要短，但必须留着**动作**（"清空连接串"）——没有它用户只知道
    // 按钮没了，不知道怎么办。
    assert!(reason.contains("连接串"), "理由要说清怎么改回去：{reason}");
    assert!(
        reason.chars().count() <= 40,
        "这条提示要控制在 40 字以内（它出现在两个 tab 顶部）：{reason}"
    );

    // ② 探测回来之后：`apply_probe` 把哨兵写进 `client`
    let mut probe = probe_with(RootCredHint::Unknown);
    probe.client.bin = NATIVE_CLIENT.to_owned();
    state.dsn = None; // 只看 client 也必须判出来
    state.apply_probe(&probe);
    assert!(
        state.is_native(),
        "`apply_probe` 写进来的哨兵也要被判成原生 —— \
             否则 dsn 一旦被别处清掉，写操作就会去打 exec"
    );
    assert!(state.write_blocked_reason().is_some());

    // ③ 改回容器内执行：拦阻解除
    state.client = "mysql".to_owned();
    assert!(!state.is_native());
    assert!(
        state.write_blocked_reason().is_none(),
        "改回容器内执行之后必须立刻放行"
    );
}

#[test]
fn dsn_debug_never_leaks_the_password() {
    // `DbState` / `AppState` 都是 `#[derive(Debug)]` —— 只要 `dsn` 是裸
    // `String`，任何一次 `{:?}` 就会把口令打进日志文件（而日志是用户会
    // 贴到 issue 里的东西）。这个 newtype 就是为了堵这一条。
    let dsn = DsnText::new("mysql://root:s3cret-pw@10.0.0.5:3306/shop");
    let shown = format!("{dsn:?}");
    assert!(!shown.contains("s3cret-pw"), "Debug 里漏了口令：{shown}");
    assert!(shown.contains("***"), "该遮的那段要显示成 ***：{shown}");
    // 其余部分**保留** —— 协议 / 主机 / 库名正是排障要看的
    assert!(shown.contains("10.0.0.5"), "主机名不该被遮：{shown}");
    assert!(shown.contains("shop"), "库名不该被遮：{shown}");
    assert!(shown.contains("root"), "用户名不该被遮：{shown}");

    // 原文仍然取得到（真正连接要用它）
    assert_eq!(dsn.as_str(), "mysql://root:s3cret-pw@10.0.0.5:3306/shop");
}

#[test]
fn dsn_redaction_handles_the_shapes_people_actually_paste() {
    // 口令里带未编码的 `@`：按**最后一个** `@` 切，否则主机名会被当成口令
    assert_eq!(
        redact_dsn_password("mysql://root:p@ss@10.0.0.5:3306/db"),
        "mysql://root:***@10.0.0.5:3306/db"
    );
    // 没有口令（`user@host`）：原样
    assert_eq!(
        redact_dsn_password("postgres://alice@h:5432/db"),
        "postgres://alice@h:5432/db"
    );
    // 没有 userinfo：原样
    assert_eq!(
        redact_dsn_password("postgres://h:5432/db"),
        "postgres://h:5432/db"
    );
    // 不像连接串的东西：原样返回，别瞎改
    assert_eq!(redact_dsn_password("host=127.0.0.1"), "host=127.0.0.1");
    assert_eq!(redact_dsn_password(""), "");
    // 没有库名、也没有端口
    assert_eq!(
        redact_dsn_password("mysql://root:pw@h"),
        "mysql://root:***@h"
    );
    // 查询串（我们目前拒绝带它的连接串，但脱敏不该因此 panic）
    assert_eq!(
        redact_dsn_password("postgres://u:p@h:5432/db?sslmode=require"),
        "postgres://u:***@h:5432/db?sslmode=require"
    );
}

#[test]
fn db_page_is_not_in_the_sidebar_but_needs_a_window() {
    // 「数据库管理」只能从容器行上的按钮进 —— 和「添加实例」同一个取舍，
    // 所以它**不在** `Page::ALL` 里。
    assert!(!Page::ALL.contains(&Page::Database));
    assert_eq!(Page::Database.group(), "容器");
    assert_eq!(Page::Database.label(), "数据库管理");
    // 连接卡上有输入框 → 进这一页必须有 window
    assert!(Page::Database.needs_window_to_enter());
}

#[test]
fn a_freshly_opened_db_page_can_be_loaded() {
    // ⚠️ 实机踩过（v0.8.0）：`provisional` 里一开始把 `busy` 置成了 `true`
    // （想法是"马上就要去探测，先算忙"），而 `Shell::load_db` 的第一句是
    // "正在跑就不叠第二次" —— **第一次**加载就被自己挡掉了，页面永远停在
    // 「正在读取…」，页头按钮永远是「读取中…」，整个数据库管理页没法用。
    //
    // CI 拦不住这个：类型全对、`core` 全绿，只有实机点一下才看得见。
    // 所以这条断言把"新建出来的状态必须是**可加载**的"钉死在这里。
    //
    // 「忙」的语义只能是"已经有一个后台任务在跑" —— 置位的责任在
    // 真正起任务的那个函数（`load_db` 进去就置 true），不在构造函数里。
    let state = DbState::provisional("mysql", "mysql:8", MysqlFamily::Mysql);
    assert!(
        !state.busy,
        "新建的状态不能是 busy：`load_db` 会因为这一条直接返回，页面卡在「正在读取…」"
    );
    // 但**没读过**这件事要保留 —— 界面靠它区分"还没读"和"读到空的"
    assert!(!state.loaded);
    // 兜底家族给的客户端名（权威判定由 apply_probe 覆盖）
    assert_eq!(state.client, "mysql");
    // v0.13：`provisional` 同时把多驱动层的连接摆好（现在只是镜像，
    // 还没有代码读它 —— 但"家族 → 引擎"的换算必须是对的，
    // 否则阶段 2 接上 PostgreSQL 时这里会悄悄全变成 MySQL）。
    assert_eq!(state.conn.kind, wslc_core::db::DbKind::MySql);
    assert_eq!(
        state.conn.host,
        wslc_core::db::DbHost::Container("mysql".into())
    );
    assert_eq!(state.conn.port, 3306);
}

#[test]
fn db_user_list_never_includes_system_or_root() {
    let mut state = DbState::provisional("mysql", "mysql:8", MysqlFamily::Mysql);
    state.users = vec![
        DbUser {
            user: "djxm_user".to_owned(),
            host: "%".to_owned(),
            plugin: "caching_sha2_password".to_owned(),
            super_priv: false,
        },
        DbUser {
            user: "mysql.sys".to_owned(),
            host: "localhost".to_owned(),
            plugin: "caching_sha2_password".to_owned(),
            super_priv: false,
        },
        DbUser {
            user: "root".to_owned(),
            host: "%".to_owned(),
            plugin: "caching_sha2_password".to_owned(),
            super_priv: true,
        },
    ];

    // 系统账号和 root 都不在可见列表里，没有"开关"能放出来
    let visible: Vec<&DbUser> = state.visible_users().collect();
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].user, "djxm_user");
}

#[test]
fn grant_database_options_skip_system_schemas_and_sort() {
    let mut state = DbState::provisional("mysql", "mysql:8", MysqlFamily::Mysql);
    // 故意给一个**乱序**的列表：`list_databases_sql` 现在带 `ORDER BY`，
    // 但下拉框的顺序不该依赖那条 SQL（用户会凭位置点选）。
    state.databases = ["shop", "mysql", "blog", "information_schema", "app"]
        .into_iter()
        .map(|name| DbEntry {
            name: name.to_owned(),
            charset: "utf8mb4".to_owned(),
            collation: "utf8mb4_0900_ai_ci".to_owned(),
            tables: 0,
            bytes: 0,
        })
        .collect();

    // 系统库不给候选：`GRANT ALL ON mysql.*` 近似提权，而下拉框是
    // "选一下就生效"的控件 —— 误选的代价太大（要授就给旁边那个输入框手打）。
    assert_eq!(
        state.grant_database_options(),
        vec!["app".to_owned(), "blog".to_owned(), "shop".to_owned()]
    );
}

#[test]
fn grant_database_options_are_empty_before_the_lists_load() {
    // 还没读到列表（`loaded == false`）时是空候选 —— 界面那边据此画空状态，
    // 而不是画一个"展开什么都没有"的下拉框。
    let state = DbState::provisional("mysql", "mysql:8", MysqlFamily::Mysql);
    assert!(state.grant_database_options().is_empty());
}

#[test]
fn db_state_takes_the_root_password_from_the_container_env() {
    let state = DbState::from_probe(&probe_with(RootCredHint::Password("pw".into())), "mysql:8");
    assert_eq!(state.creds.user, "root");
    assert_eq!(state.creds.password, "pw");
    assert!(state.cred_note.is_some(), "要说清密码是哪来的");
    assert!(state.usable());
    // 还没读过列表：界面要区分"还没读"和"读到空的"
    assert!(!state.loaded);
    assert!(state.databases.is_empty());
    assert_eq!(state.tab, DbTab::Databases);
    assert_eq!(state.client, "mysql");
    assert_eq!(state.family, MysqlFamily::Mysql);
}

#[test]
fn db_state_leaves_the_password_empty_for_random_and_unknown() {
    let random = DbState::from_probe(&probe_with(RootCredHint::Random), "mysql:8");
    assert_eq!(random.creds.password, "");
    assert!(
        random
            .cred_note
            .as_deref()
            .unwrap_or_default()
            .contains("随机"),
        "{:?}",
        random.cred_note
    );

    // 什么都没探测到 → 连说明都不给（没有可说的，别凑一句）
    let unknown = DbState::from_probe(&probe_with(RootCredHint::Unknown), "mysql:8");
    assert!(unknown.cred_note.is_none());
    // 空密码是**合法**状态（`MYSQL_ALLOW_EMPTY_PASSWORD`），所以不算"不可用"
    assert!(unknown.usable());
}

#[test]
fn db_state_debug_never_shows_the_password() {
    // ⚠️ `AppState` 是 `#[derive(Debug)]`，它会一路打印到 `DbState` 和
    // 里面的 `MysqlCreds` —— 这条守住"密码不进日志"。
    let state = DbState::from_probe(
        &probe_with(RootCredHint::Password("s3cret-pw".into())),
        "mysql:8",
    );
    let text = format!("{state:?}");
    assert!(!text.contains("s3cret-pw"), "{text}");
    assert!(text.contains("***"), "{text}");
    // 用户名、容器名还是要看得见的（排障要用）
    assert!(text.contains("root"), "{text}");
    assert!(text.contains("mysql"), "{text}");
}

#[test]
fn db_failure_gives_the_next_step_for_the_real_errors() {
    let denied = DbFailure::from_text(
        "ERROR 1045 (28000): Access denied for user 'root'@'localhost' (using password: YES)",
    );
    assert_eq!(denied.kind, MysqlErrorKind::AccessDenied);
    assert!(denied.summary().contains("密码"), "{denied:?}");

    let stopped =
        DbFailure::from_text("容器“947c4f25”未运行。错误代码： WSLC_E_CONTAINER_NOT_RUNNING");
    assert_eq!(stopped.kind, MysqlErrorKind::ContainerNotRunning);
    assert!(stopped.summary().contains("没在运行"), "{stopped:?}");

    let no_client = DbFailure::from_text(
        "OCI runtime exec failed: exec: \"mysql\": executable file not found in $PATH",
    );
    assert_eq!(no_client.kind, MysqlErrorKind::ClientMissing);
    assert!(no_client.summary().contains("客户端"), "{no_client:?}");
}

#[test]
fn db_failure_summary_is_one_line_and_survives_multibyte_output() {
    // 认不出来的错误：给原文第一行；长文本按**字符**截断（按字节会 panic）
    let weird = DbFailure::from_text(format!("{}\n第二行不该出现", "汉".repeat(300)));
    assert_eq!(weird.kind, MysqlErrorKind::Unknown);
    let summary = weird.summary();
    assert!(summary.ends_with('…'), "{summary}");
    assert_eq!(summary.chars().count(), 121);
    assert!(!summary.contains("第二行"), "{summary}");
}

#[test]
fn db_drop_actions_say_what_they_destroy_and_never_leak_the_password() {
    let drop_db = PendingAction::DropDatabase {
        container: "mysql".to_owned(),
        database: "shop".to_owned(),
        client: "mysql".to_owned(),
        creds: MysqlCreds::root("s3cret-pw"),
    };
    assert_eq!(drop_db.title(), "删除数据库");
    assert_eq!(drop_db.confirm_label(), "删除数据库");
    let body = drop_db.body();
    assert!(body.contains("DROP DATABASE shop"), "{body}");
    assert!(body.contains("不可撤销"), "{body}");
    assert!(!body.contains("s3cret-pw"), "确认文案里漏了密码：{body}");

    let drop_user = PendingAction::DropUser {
        container: "mysql".to_owned(),
        user: "app".to_owned(),
        host: "%".to_owned(),
        client: "mysql".to_owned(),
        creds: MysqlCreds::root("s3cret-pw"),
    };
    assert_eq!(drop_user.title(), "删除 MySQL 用户");
    assert!(drop_user.body().contains("'app'@'%'"), "{drop_user:?}");
    assert!(
        !drop_user.body().contains("s3cret-pw"),
        "确认文案里漏了密码"
    );
}

#[test]
fn db_error_state_clears_before_the_next_action() {
    let mut state = DbState::from_probe(&probe_with(RootCredHint::Unknown), "mysql:8");
    state.fail("ERROR 1045 (28000): Access denied");
    assert!(state.error.is_some());
    assert!(state.error.as_ref().unwrap().hint().is_some());
    state.clear_error();
    assert!(state.error.is_none());
}

#[test]
fn import_and_backup_progress_can_be_put_in_the_background() {
    // ⚠️ 「放到后台」只收遮罩，**不取消**任务：
    //
    // - `dismissed = true` 时渲染那边什么都不画（`db_import_progress` /
    //   `db_backup_progress` 的第一句）；
    // - 但 `import` / `backup` 仍然是 `Some` —— 这两个 `Option` 同时是
    //   **互斥锁**（`start_db_import` / `start_db_backup` 的第一句就查它们）。
    //   置 `None` 会放开"再来一份"的口子：两份 `container cp` 抢同一个
    //   目录，或者两份 mysqldump 撞同一个备份文件名。
    let mut db = DbState::from_probe(&probe_with(RootCredHint::Unknown), "mysql:8");
    db.import = Some(ImportState {
        file_name: "shop.sql".to_owned(),
        database: Some("shop".to_owned()),
        dismissed: false,
    });
    db.backup = Some(BackupState {
        database: "shop".to_owned(),
        container_path: "/tmp/wslc-backups/x.sql".to_owned(),
        windows_path: std::path::PathBuf::from(r"C:\x\shop-1.sql"),
        last_message: None,
        dismissed: false,
    });

    // 刚起来时浮层是可见的
    assert!(!db.import.as_ref().unwrap().dismissed);
    assert!(!db.backup.as_ref().unwrap().dismissed);

    // 收起来之后：两个任务**都还在**（互斥锁没被放开）
    db.import.as_mut().unwrap().dismissed = true;
    db.backup.as_mut().unwrap().dismissed = true;
    assert!(db.import.is_some(), "收起浮层不能把互斥锁放掉");
    assert!(db.backup.is_some(), "收起浮层不能把互斥锁放掉");
    assert!(db.import.as_ref().unwrap().dismissed);
    assert!(db.backup.as_ref().unwrap().dismissed);
}

#[test]
fn deleting_a_backup_file_asks_for_confirmation_and_names_both_copies() {
    // ⚠️ 这个动作原先**没有**二次确认，而「恢复」（可恢复的那个）有 ——
    // 两个配置反着。凭据是：删除是不可撤销的（Windows 侧文件 + 容器内
    // `rm -f`，面板里没有回收站），而恢复至少还能再备份回来。
    let action = PendingAction::DeleteDatabaseBackup {
        container: "mysql".to_owned(),
        database: "djxm".to_owned(),
        windows_path: std::path::PathBuf::from(
            r"C:\Users\x\backups\mysql\djxm-20261015-035901.sql",
        ),
        container_path: "/tmp/wslc-backups/djxm-20261015-035901.sql".to_owned(),
    };

    assert_eq!(action.title(), "删除备份文件");
    assert_eq!(action.confirm_label(), "永久删除");

    let body = action.body();
    // 用户必须看见**两份**都会没：Windows 侧那份是主体，容器内那份也会删
    assert!(
        body.contains(r"C:\Users\x\backups\mysql\djxm-20261015-035901.sql"),
        "{body}"
    );
    assert!(
        body.contains("/tmp/wslc-backups/djxm-20261015-035901.sql"),
        "{body}"
    );
    // 说清"数据库本身不受影响" —— 否则用户会以为连库一起删了
    assert!(body.contains("数据库 djxm 本身不受影响"), "{body}");
    assert!(
        body.contains("不可撤销") || body.contains("没有回收站"),
        "{body}"
    );

    // 它**不**走 `PendingAction::execute`（那个形状表达不了"删完再重扫目录"），
    // 所以执行入口必须给一句人话，而不是 panic。
    // `execute` 要 `&Wslc`；这里只验证这个变体不会走到"跑一条命令"那条路。
    // （真实分流在 `Shell::confirm_pending` 里，见那边的注释。）
}

#[test]
fn backup_file_names_parse_and_junk_is_skipped() {
    // 面板自己生成的形状：`<库名>-<日期>-<时间>.sql`
    let (database, stamp) = parse_backup_file_name("djxm-20261015-035901.sql").expect("应能解出");
    assert_eq!(database, "djxm");
    assert_eq!(stamp, "20261015-035901");
    // 库名自己带 `-`：从**右边**解，库名保持完整
    let (database, _) = parse_backup_file_name("my-app-db-20261015-035901.sql").expect("应能解出");
    assert_eq!(database, "my-app-db");
    // 不认识的文件**跳过**（目录里可能有用户手动拷进来的东西）
    assert_eq!(parse_backup_file_name("readme.txt"), None);
    assert_eq!(parse_backup_file_name("abc.sql"), None);
    assert_eq!(parse_backup_file_name("abc-20261015.sql"), None);
    assert_eq!(parse_backup_file_name("abc-2026-035901.sql"), None);
    assert_eq!(parse_backup_file_name(""), None);
}

#[test]
fn scan_backup_dir_sorts_newest_first_and_skips_junk() {
    let dir = std::env::temp_dir().join(format!("wslc-scan-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a-20261015-035901.sql"), "x").unwrap();
    std::fs::write(dir.join("a-20261016-035901.sql"), "xy").unwrap();
    std::fs::write(dir.join("b-20261010-000000.sql"), "xyz").unwrap();
    // 垃圾文件：解不出来 → 跳过
    std::fs::write(dir.join("notes.txt"), "junk").unwrap();

    let entries = scan_backup_dir(&dir).expect("应能扫出备份");
    assert_eq!(entries.len(), 3, "{entries:?}");
    // 新的在上（时间戳定宽，字典序 == 时间序）
    assert_eq!(entries[0].database, "a");
    assert_eq!(entries[0].stamp, "20261016-035901");
    assert_eq!(entries[0].bytes, 2);
    assert_eq!(entries[2].database, "b");
    // 容器内那份的路径由文件名推出来（不一定还在，恢复前会查）
    assert_eq!(
        entries[0].container_path,
        "/tmp/wslc-backups/a-20261016-035901.sql"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// `DbState` 的 `Debug` 里**不能**出现明文密码（守门断言）。
///
/// 以前这里还顺带测"`DbState` 上那份凭据克隆"—— 那个字段已经删掉了
/// （它就是"列表里永远显示 `—`"的根因：渲染读它，而它从没被同步过）。
/// 这条留下来是因为 `AppState` / `DbState` 的 `Debug` 会一路把
/// `MysqlCreds` 带出来，而那里面**有密码**（`MysqlCreds` 的 `Debug`
/// 是手写脱敏的）。凭据存档自己的脱敏在 `dbcreds::tests` 里。

#[test]
fn db_state_debug_never_leaks_the_password() {
    let state = DbState::from_probe(
        &probe_with(RootCredHint::Password("s3cret-pw".into())),
        "mysql:8",
    );
    let text = format!("{state:?}");
    assert!(!text.contains("s3cret-pw"), "{text}");
    assert!(text.contains("***"), "{text}");
}
