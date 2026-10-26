//! 数据库域的纯逻辑测试（从 `model/mysql.rs` 原样搬过来，未改一条断言）。

use super::*;
use crate::model::{ContainerInspect, ContainerListItem};
use serde_json::json;

/// 实测抓下来的夹具。
const DATABASES_TSV: &str = include_str!("../../../tests/fixtures/mysql_databases.tsv");
const USERS_TSV: &str = include_str!("../../../tests/fixtures/mysql_users.tsv");
const CLIENT_PROBE: &str = include_str!("../../../tests/fixtures/mysql_client_probe.txt");
const ERR_ACCESS_DENIED: &str =
    include_str!("../../../tests/fixtures/mysql_error_access_denied.txt");
const ERR_NOT_RUNNING: &str = include_str!("../../../tests/fixtures/mysql_error_not_running.txt");
const ERR_NO_CLIENT: &str = include_str!("../../../tests/fixtures/mysql_error_no_client.txt");

fn item(image: &str, command: &str, labels: &str) -> ContainerListItem {
    ContainerListItem {
        image: image.to_owned(),
        command: command.to_owned(),
        labels: labels.to_owned(),
        ..Default::default()
    }
}

/// 实机那条 `wslc list -a --format json` 的 Labels（端口 3306）。
fn real_mysql_labels() -> String {
    r#"com.microsoft.wsl.container.metadata={"V1":{"Flags":0,"Image":"docker.1panel.live/library/mysql:latest","InitProcessFlags":0,"Ports":[{"BindingAddress":"127.0.0.1","ContainerPort":3306,"Family":2,"HostPort":3306,"Protocol":6,"VmPort":20003},{"BindingAddress":"127.0.0.1","ContainerPort":33060,"Family":2,"HostPort":33060,"Protocol":6,"VmPort":20004}],"Volumes":[]}}"#
        .to_owned()
}

// -- 识别 ---------------------------------------------------------------

#[test]
fn recognises_the_real_mysql_container_by_image() {
    // 实机抓的那一行（Command 被 wslc 截断成 20 字符，所以认出来的
    // 只能是**镜像名**这条依据）
    let it = item(
        "docker.1panel.live/library/mysql:latest",
        "\"docker-entrypoint.s…\"",
        &real_mysql_labels(),
    );
    let found = mysql_container_kind(&it).expect("应认出 mysql");
    assert_eq!(found.family, MysqlFamily::Mysql);
    assert_eq!(found.signal, MysqlSignal::Image);
}

#[test]
fn image_segment_handles_registry_tag_digest_and_case() {
    assert_eq!(family_from_image("mysql"), Some(MysqlFamily::Mysql));
    assert_eq!(
        family_from_image("docker.1ms.run/library/mysql:8.0"),
        Some(MysqlFamily::Mysql)
    );
    assert_eq!(family_from_image("MariaDB:11"), Some(MysqlFamily::Mariadb));
    // ⚠️ 摘要形式：`registry::split_input` 对它会返回 Err，这里必须还能认
    assert_eq!(
        family_from_image("mysql@sha256:abcdef"),
        Some(MysqlFamily::Mysql)
    );
    assert_eq!(
        family_from_image("localhost:5000/percona-server:8"),
        Some(MysqlFamily::Percona)
    );
    // 认不出来 → 什么都不做
    assert_eq!(family_from_image("nginx:latest"), None);
    assert_eq!(family_from_image("postgres:16"), None);
    assert_eq!(family_from_image(""), None);
}

#[test]
fn recognises_by_command_when_untouched() {
    let it = item(
        "some.internal/mydb:1",
        "\"mysqld --defaults-file=/etc/my.cnf\"",
        "",
    );
    let found = mysql_container_kind(&it).expect("应按命令认出来");
    assert_eq!(found.signal, MysqlSignal::Command);
    assert_eq!(found.family, MysqlFamily::Mysql);

    assert_eq!(
        family_from_command("docker-entrypoint.sh mariadbd"),
        Some(MysqlFamily::Mariadb)
    );
}

#[test]
fn recognises_by_port_as_last_resort() {
    let it = item("mycompany/analytics:1", "", &real_mysql_labels());
    let found = mysql_container_kind(&it).expect("应按端口猜出来");
    assert_eq!(found.signal, MysqlSignal::Port);
    // 依据要能说出来 —— 这条是**猜**的，界面得让用户知道
    assert!(found.signal.label().contains("3306"));
}

#[test]
fn plain_container_is_not_mysql() {
    let it = item(
        "docker.1panel.live/library/nginx:latest",
        "\"/docker-entrypoint.…\"",
        "",
    );
    assert_eq!(mysql_container_kind(&it), None);
}

#[test]
fn inspect_finds_mysql_via_env_cmd_and_ports() {
    // 实机 inspect 的关键字段（Config.Env 里有 MYSQL_ROOT_PASSWORD）
    let insp = ContainerInspect::new(json!({
        "Config": {
            "Image": "docker.1panel.live/library/mysql:latest",
            "Env": ["MYSQL_ROOT_PASSWORD=root", "PATH=/usr/bin"],
            "Cmd": ["mysqld"],
            "Entrypoint": ["docker-entrypoint.sh"]
        }
    }));
    assert_eq!(family_from_inspect(&insp), Some(MysqlFamily::Mysql));
}

#[test]
fn inspect_falls_back_to_env_then_command_then_port() {
    // 镜像名认不出来，但环境变量是 mariadb 那一套
    let mariadb = ContainerInspect::new(json!({
        "Config": { "Image": "myco/db:1", "Env": ["MARIADB_ROOT_PASSWORD=x"] }
    }));
    assert_eq!(family_from_inspect(&mariadb), Some(MysqlFamily::Mariadb));

    // 只有命令
    let by_cmd = ContainerInspect::new(json!({
        "Config": { "Image": "myco/db:1", "Cmd": ["mysqld"] }
    }));
    assert_eq!(family_from_inspect(&by_cmd), Some(MysqlFamily::Mysql));

    // 只有端口（`inspect` 的 Ports 键是 `3306/tcp`）
    let by_port = ContainerInspect::new(json!({
        "Config": { "Image": "myco/db:1" },
        "Ports": { "3306/tcp": [] }
    }));
    assert_eq!(family_from_inspect(&by_port), Some(MysqlFamily::Mysql));

    // 什么线索都没有
    let none = ContainerInspect::new(json!({ "Config": { "Image": "myco/app:1" } }));
    assert_eq!(family_from_inspect(&none), None);
}

// -- 凭据 ---------------------------------------------------------------

#[test]
fn creds_debug_never_shows_the_password() {
    // ⚠️ 这条是"密码不进日志"的守门测试：`AppState` 是 `#[derive(Debug)]`，
    // 它会一路打出 `DbState` 和里面的 `MysqlCreds`。
    let creds = MysqlCreds::root("s3cret-pw");
    let text = format!("{creds:?}");
    assert!(!text.contains("s3cret-pw"), "{text}");
    assert!(text.contains("***"), "{text}");
    assert!(text.contains("root"), "{text}");
}

#[test]
fn reads_root_credential_from_the_real_container_env() {
    let env = vec![
        "MYSQL_ROOT_PASSWORD=root".to_owned(),
        "PATH=/usr/local/sbin:/usr/local/bin".to_owned(),
        "MYSQL_MAJOR=innovation".to_owned(),
    ];
    assert_eq!(
        root_credential_from_env(&env),
        RootCredHint::Password("root".to_owned())
    );
    assert!(root_credential_from_env(&env).note().is_some());
}

#[test]
fn root_credential_covers_empty_random_and_unknown() {
    assert_eq!(
        root_credential_from_env(&["MYSQL_ALLOW_EMPTY_PASSWORD=yes".to_owned()]),
        RootCredHint::Empty
    );
    assert_eq!(
        root_credential_from_env(&["MYSQL_RANDOM_ROOT_PASSWORD=1".to_owned()]),
        RootCredHint::Random
    );
    assert_eq!(
        root_credential_from_env(&["PATH=/usr/bin".to_owned()]),
        RootCredHint::Unknown
    );
    // 同一组里既有空密码标记又有明确密码 → 明确的那个说了算
    assert_eq!(
        root_credential_from_env(&[
            "MYSQL_ALLOW_EMPTY_PASSWORD=yes".to_owned(),
            "MYSQL_ROOT_PASSWORD=real".to_owned(),
        ]),
        RootCredHint::Password("real".to_owned())
    );
    // 空值的 ROOT_PASSWORD 不算"有密码"
    assert_eq!(
        root_credential_from_env(&["MYSQL_ROOT_PASSWORD=".to_owned()]),
        RootCredHint::Empty
    );
}

// -- 命令行 -------------------------------------------------------------

#[test]
fn client_args_put_the_sql_last_and_never_a_password_flag() {
    let creds = MysqlCreds::root("pw");
    let args = client_args("mysql", &creds, "SHOW DATABASES", None, SqlMode::Batch);
    assert_eq!(
        args,
        vec![
            "mysql",
            "--default-character-set=utf8mb4",
            "-u",
            "root",
            "-N",
            "-B",
            "-e",
            "SHOW DATABASES"
        ]
    );
    assert_eq!(args.last().map(String::as_str), Some("SHOW DATABASES"));
    // ⚠️ 字符集写死 utf8mb4（**位置也要在 `-e` 之前**）：容器里没有 `LANG`
    // 时客户端退回 latin1，`SELECT` 的中文会变 `?`、恢复会双重编码。
    // 实测见 `load_script` 的注释 —— 这一条是数据安全断言，不是风格问题。
    assert!(
        args.contains(&"--default-character-set=utf8mb4".to_owned()),
        "缺字符集开关：{args:?}"
    );
    // 密码**不在**参数里 —— 只走 `exec_env`
    assert!(
        !args.iter().any(|a| a.contains("pw")),
        "参数里不该出现密码：{args:?}"
    );
}

#[test]
fn client_args_table_mode_and_database_flag() {
    let creds = MysqlCreds::root("");
    let args = client_args("mariadb", &creds, "SELECT 1", Some("shop"), SqlMode::Table);
    assert_eq!(
        args,
        vec![
            "mariadb",
            "--default-character-set=utf8mb4",
            "-u",
            "root",
            "--table",
            "-D",
            "shop",
            "-e",
            "SELECT 1"
        ]
    );
    // `--table` 必须显式给：stdout 不是 tty 时 mysql 默认是 batch（实测）
    assert!(args.contains(&"--table".to_owned()));
}

#[test]
fn client_args_include_host_and_port_when_given() {
    let creds = MysqlCreds {
        user: "app".to_owned(),
        password: "x".to_owned(),
        host: Some("127.0.0.1".to_owned()),
        port: Some(3307),
    };
    let args = client_args("mysql", &creds, "SELECT 1", None, SqlMode::Batch);
    assert!(
        args.windows(2).any(|w| w[0] == "-h" && w[1] == "127.0.0.1"),
        "{args:?}"
    );
    assert!(
        args.windows(2).any(|w| w[0] == "-P" && w[1] == "3307"),
        "{args:?}"
    );
}

#[test]
fn exec_env_holds_the_password_and_nothing_else_does() {
    let creds = MysqlCreds::root("p@ss");
    assert_eq!(exec_env(&creds), "MYSQL_PWD=p@ss");
}

// -- 脱敏 ---------------------------------------------------------------

#[test]
fn redact_args_hides_every_password_shape() {
    let args = vec![
        "exec".to_owned(),
        "-e".to_owned(),
        "MYSQL_PWD=root".to_owned(),
        "mysql".to_owned(),
        "mysql".to_owned(),
        "-uroot".to_owned(),
        "-prootpw".to_owned(),
        "--password=rootpw2".to_owned(),
        "-e".to_owned(),
        "SELECT 1".to_owned(),
    ];
    let shown = redact_args(&args).join(" ");
    assert!(!shown.contains("rootpw"), "{shown}");
    assert!(!shown.contains("MYSQL_PWD=root "), "{shown}");
    assert!(shown.contains("MYSQL_PWD=***"), "{shown}");
    assert!(shown.contains("-p***"), "{shown}");
    assert!(shown.contains("--password=***"), "{shown}");
    // 不该误伤正常参数
    assert!(shown.contains("SELECT 1"), "{shown}");
    assert!(shown.contains("-uroot"), "{shown}");
}

#[test]
fn redact_password_replaces_the_value_even_inside_sql_echoes() {
    // `CREATE USER … IDENTIFIED BY 'x'` 出错时 mysql 会回显整条语句
    let stderr = "ERROR 1064 (42000) at line 1: You have an error in your SQL syntax near \
                  'IDENTIFIED BY \\'hunter2\\'' at line 1";
    let clean = redact_password(stderr, "hunter2");
    assert!(!clean.contains("hunter2"), "{clean}");
    assert!(clean.contains("***"), "{clean}");
}

#[test]
fn redact_password_leaves_text_alone_when_the_password_is_empty() {
    // 空密码不能当 pattern：`replace` 会在每个字符边界插东西
    let text = "ERROR 1045 (28000): Access denied";
    assert_eq!(redact_password(text, ""), text);
}

// -- 解析 ---------------------------------------------------------------

#[test]
fn escape_is_the_inverse_of_unescape() {
    // 这一对函数必须互为逆运算：一边改了另一边没改，原生连接和 exec 两条路
    // 就会对同一个库名给出不同结果 —— 那种差异没人会想到去查解析。
    for raw in [
        "plain",
        "a\tb",
        "line1\nline2",
        "back\\slash",
        "nul\0char",
        "cr\rhere",
    ] {
        assert_eq!(
            unescape_batch_field(&escape_batch_field(raw)),
            raw,
            "原文 {raw:?}"
        );
    }
}

#[test]
fn to_batch_tsv_round_trips_through_the_real_parser() {
    // 端到端：结构化单元格 → TSV → 现有解析函数。
    // 这就是"原生连接复用 -B 解析"的全部契约。
    let rows = vec![
        vec![
            Some("djxm".to_owned()),
            Some("t_order".to_owned()),
            Some("BASE TABLE".to_owned()),
        ],
        vec![
            Some("djxm".to_owned()),
            Some("v_x".to_owned()),
            Some("VIEW".to_owned()),
        ],
    ];
    let tsv = to_batch_tsv(&rows);
    let tree = crate::model::schematree::parse_schema_tree(&tsv);
    assert_eq!(tree.databases.len(), 1);
    assert_eq!(tree.table_count(), 2);
    assert_eq!(tree.databases[0].tables[1].name, "v_x");
}

#[test]
fn to_batch_tsv_escapes_tabs_inside_a_field() {
    // 表名里真有 tab（反引号里什么都允许）时，不能被当成列分隔符
    let rows = vec![vec![Some("db".to_owned()), Some("we\tird".to_owned())]];
    let line = to_batch_tsv(&rows);
    assert_eq!(split_tsv_line(line.trim_end()), vec!["db", "we\tird"]);
}

#[test]
fn unescapes_batch_output_like_the_real_thing() {
    // 实测：`SELECT 'a\tb'` 在 `-B` 下输出的是字面的 `a\tb`
    assert_eq!(unescape_batch_field(r"a\tb"), "a\tb");
    assert_eq!(unescape_batch_field(r"back\\slash"), r"back\slash");
    assert_eq!(unescape_batch_field(r"line\nbreak"), "line\nbreak");
    // 未知转义原样保留（不猜）
    assert_eq!(unescape_batch_field(r"\x"), r"\x");
    // 结尾的孤立反斜杠不 panic
    assert_eq!(unescape_batch_field("tail\\"), "tail\\");
}

#[test]
fn parses_the_real_database_list() {
    let db = parse_databases(DATABASES_TSV);
    assert_eq!(db.len(), 4);
    assert_eq!(db[0].name, "information_schema");
    assert_eq!(db[0].charset, "utf8mb3");
    assert_eq!(db[0].tables, 84);
    assert_eq!(db[1].name, "mysql");
    assert_eq!(db[1].collation, "utf8mb4_0900_ai_ci");
    assert_eq!(db[1].bytes, 8_257_536);
    assert!(db[1].is_system());
    assert!(!db.iter().any(|d| d.name == "shop"));
}

#[test]
fn parses_the_real_user_list() {
    let users = parse_users(USERS_TSV);
    assert_eq!(users.len(), 5);
    // 同一个用户名两个 host —— 这就是 host 必须是一列的原因
    assert_eq!(
        users.iter().filter(|u| u.user == "root").count(),
        2,
        "{users:?}"
    );
    assert_eq!(users[0].account(), "'mysql.infoschema'@'localhost'");
    assert!(!users[0].super_priv);
    assert!(users[4].super_priv);
    assert_eq!(users[4].plugin, "caching_sha2_password");
    assert!(users[0].is_system());
    assert!(!users[4].is_system());
    assert!(users[4].is_root());
}

#[test]
fn system_and_root_are_hidden_by_default_but_normal_users_are_not() {
    let users = parse_users(USERS_TSV);
    // 夹具里正好是 3 个 mysql.* + 2 个 root，全部属于"默认藏起来"
    assert_eq!(
        users.iter().filter(|u| u.is_hidden_by_default()).count(),
        5,
        "{users:?}"
    );
    assert!(users.iter().all(|u| u.is_hidden_by_default()));

    let business = DbUser {
        user: "djxm_user".to_owned(),
        host: "%".to_owned(),
        plugin: "caching_sha2_password".to_owned(),
        super_priv: false,
    };
    assert!(!business.is_system());
    assert!(!business.is_root());
    assert!(!business.is_hidden_by_default());

    // MariaDB 的 `mariadb.sys` 也算系统账号
    let mariadb = DbUser {
        user: "mariadb.sys".to_owned(),
        ..business.clone()
    };
    assert!(mariadb.is_system());
    assert!(mariadb.is_hidden_by_default());
}

#[test]
fn parsers_skip_broken_rows_instead_of_panicking() {
    assert!(parse_databases("").is_empty());
    assert!(parse_users("").is_empty());
    // 少列的行跳过、好的行留着
    let mixed = "only_one_column\nmysql\tutf8mb4\tutf8mb4_0900_ai_ci\t3\t1024\n";
    let db = parse_databases(mixed);
    assert_eq!(db.len(), 1);
    assert_eq!(db[0].name, "mysql");
    // 数字列坏掉时按 0 处理，不丢整行
    let bad_number = "shop\tutf8mb4\tutf8mb4_0900_ai_ci\t?\t?\n";
    let db = parse_databases(bad_number);
    assert_eq!(db.len(), 1);
    assert_eq!(db[0].tables, 0);
    assert_eq!(db[0].bytes, 0);
}

#[test]
fn parses_the_real_client_probe() {
    let probe = parse_client_probe(CLIENT_PROBE).expect("应解析出客户端");
    assert_eq!(probe.bin, "mysql");
    assert_eq!(probe.path, "/usr/bin/mysql");
    assert!(
        probe
            .version
            .as_deref()
            .unwrap_or_default()
            .contains("26.7.0"),
        "{probe:?}"
    );
    // 只有一行（老版本 / 探测脚本被裁剪）也要能用
    let only_path = parse_client_probe("/usr/bin/mariadb\n").unwrap();
    assert_eq!(only_path.bin, "mariadb");
    assert_eq!(only_path.version, None);
    assert_eq!(parse_client_probe(""), None);
}

// -- SQL 生成 -----------------------------------------------------------

#[test]
fn validates_identifiers_and_rejects_injection_shapes() {
    assert!(validate_ident("库名", "shop").is_ok());
    assert!(validate_ident("库名", "shop_2$").is_ok());
    assert!(validate_ident("库名", "  shop  ").is_ok());
    assert!(validate_ident("库名", "").is_err());
    assert!(validate_ident("库名", &"a".repeat(65)).is_err());
    // 下面这些都是**必须拒掉**的（它们会直接进 SQL）。
    // ⚠️ 用「中间有换行」而不是「末尾有换行」：名字两头的空白是会被
    // `trim()` 掉的（表单输入带个回车不该算错误），中间的才真非法。
    for bad in [
        "shop; DROP DATABASE mysql",
        "shop`",
        "shop'",
        "shop--",
        "shop db",
        "shop\ndrop",
        "库",
    ] {
        assert!(validate_ident("库名", bad).is_err(), "应拒绝：{bad:?}");
    }
}

#[test]
fn quotes_literals_for_passwords() {
    assert_eq!(quote_literal("plain"), "'plain'");
    assert_eq!(quote_literal(r"a\b"), r"'a\\b'");
    assert_eq!(quote_literal("it's"), r"'it\'s'");
    assert_eq!(quote_literal(r#"dq"here"#), r#"'dq\"here'"#);
    assert_eq!(quote_literal(""), "''");
}

#[test]
fn builds_create_database_with_optional_charset() {
    assert_eq!(
        create_database_sql("shop", None, None).unwrap(),
        "CREATE DATABASE `shop`"
    );
    assert_eq!(
        create_database_sql("shop", Some("utf8mb4"), None).unwrap(),
        "CREATE DATABASE `shop` DEFAULT CHARACTER SET utf8mb4"
    );
    assert_eq!(
        create_database_sql("shop", Some("utf8mb4"), Some("utf8mb4_0900_ai_ci")).unwrap(),
        "CREATE DATABASE `shop` DEFAULT CHARACTER SET utf8mb4 DEFAULT COLLATE utf8mb4_0900_ai_ci"
    );
    // 空白等于没填（表单里那一格默认是空的）
    assert_eq!(
        create_database_sql("shop", Some("   "), None).unwrap(),
        "CREATE DATABASE `shop`"
    );
    // 保留字做库名也要能跑 —— 所以标识符**一律**反引号
    assert_eq!(
        create_database_sql("order", None, None).unwrap(),
        "CREATE DATABASE `order`"
    );
    assert!(create_database_sql("shop; drop", None, None).is_err());
    assert!(create_database_sql("shop", Some("utf8mb4; drop"), None).is_err());
}

#[test]
fn builds_user_statements() {
    assert_eq!(
        create_user_sql("app", "%", "pw").unwrap(),
        "CREATE USER IF NOT EXISTS 'app'@'%' IDENTIFIED BY 'pw'"
    );
    assert_eq!(
        alter_user_password_sql("app", "localhost", "p'w").unwrap(),
        r"ALTER USER 'app'@'localhost' IDENTIFIED BY 'p\'w'"
    );
    assert_eq!(
        drop_user_sql("app", "%").unwrap(),
        "DROP USER IF EXISTS 'app'@'%'"
    );
    assert_eq!(drop_database_sql("shop").unwrap(), "DROP DATABASE `shop`");

    // host 比标识符宽松：`%` 和网段都要能过
    assert!(validate_host("%").is_ok());
    assert!(validate_host("192.168.1.%").is_ok());
    assert!(validate_host("localhost").is_ok());
    assert!(validate_host("").is_err());
    assert!(validate_host("a b").is_err());
    assert!(validate_host("a'; DROP USER x").is_err());
}

#[test]
fn grant_variants_are_all_valid_sql() {
    assert_eq!(
        grant_sql("shop", "app", "%", GrantKind::All).unwrap(),
        "GRANT ALL PRIVILEGES ON `shop`.* TO 'app'@'%'"
    );
    assert_eq!(
        grant_sql("shop", "app", "%", GrantKind::ReadOnly).unwrap(),
        "GRANT SELECT ON `shop`.* TO 'app'@'%'"
    );
    assert_eq!(
        grant_sql("shop", "app", "%", GrantKind::ReadWrite).unwrap(),
        "GRANT SELECT, INSERT, UPDATE, DELETE ON `shop`.* TO 'app'@'%'"
    );
    assert_eq!(GrantKind::ALL.len(), 3);
    assert!(GrantKind::ALL.iter().all(|k| !k.label().is_empty()));
}

#[test]
fn create_user_statements_joins_create_and_grant() {
    let sql = create_user_statements("app", "%", "pw", Some(("shop", GrantKind::All))).unwrap();
    assert_eq!(
        sql,
        "CREATE USER IF NOT EXISTS 'app'@'%' IDENTIFIED BY 'pw'; \
         GRANT ALL PRIVILEGES ON `shop`.* TO 'app'@'%'"
    );
    // 不授权时只有一条语句
    let only = create_user_statements("app", "%", "pw", None).unwrap();
    assert!(!only.contains(';'), "{only}");
    // 授权对象非法 → 整条拒绝（不会出现"建了用户但没授权"的半成品）
    assert!(create_user_statements("app", "%", "pw", Some(("bad name", GrantKind::All))).is_err());
}

// -- 建库同时建用户 / 备份（v0.8）---------------------------------------

#[test]
fn generated_passwords_are_stable_in_shape_and_unique() {
    let first = generate_password().expect("测试机应能取到系统随机数");
    assert_eq!(first.len(), 16, "{first}");
    // 字符集里没有需要转义的东西 —— 这是 quote_literal 永不触发的保证
    assert!(
        first.chars().all(|c| c.is_ascii_alphanumeric()),
        "出现需要转义的字符：{first}"
    );
    // 连续生成不相同（同一次点击链路里至少要保证这个）
    let mut seen = std::collections::HashSet::new();
    for _ in 0..32 {
        seen.insert(generate_password().expect("测试机应能取到系统随机数"));
    }
    assert_eq!(seen.len(), 32, "32 次里出了重复密码：{seen:?}");
}

#[test]
fn default_user_follows_the_database_name() {
    assert_eq!(default_user_for("djxm"), "djxm_user");
    assert_eq!(default_user_for("  shop  "), "shop_user");
    assert_eq!(default_user_for(""), "_user");
    // 拼出来的名字仍然要能过校验（库名受白名单约束，所以一定过）
    assert!(validate_ident("用户名", &default_user_for("djxm")).is_ok());
}

#[test]
fn dump_bin_is_derived_from_the_client_never_the_client_itself() {
    // v0.9.6 的实机事故：备份把**客户端**当成了 dump 程序，容器里跑出
    // `mysql … --databases …`（mysql 不认这个开关，实测 exit 2）。
    assert_eq!(dump_bin("mysql"), "mysqldump");
    assert_eq!(dump_bin("/usr/bin/mysql"), "/usr/bin/mysqldump");
    assert_eq!(dump_bin("mariadb"), "mariadb-dump");
    assert_eq!(dump_bin("/usr/bin/mariadb"), "/usr/bin/mariadb-dump");
    // 幂等：已经是 dump 工具就原样返回（`mariadb-dump` 落在 ends_with("dump")）
    assert_eq!(dump_bin("mysqldump"), "mysqldump");
    assert_eq!(dump_bin("/usr/bin/mariadb-dump"), "/usr/bin/mariadb-dump");
    // 认不出来时退回最可能的那个 —— **不能**把客户端名字原样带下去
    assert_eq!(dump_bin("mysqlsh"), "mysqldump");
    assert_eq!(dump_bin("weird"), "mysqldump");
}

#[test]
fn mysqldump_args_are_the_verified_shape() {
    let creds = MysqlCreds::root("pw");
    // ⚠️ 传进去的是**客户端**（生产里就是 `mysql`，因为探测只找客户端），
    // dump 工具由 `dump_bin` 推导。这两条断言是 v0.9.6 那次事故的回归钉 ——
    // 原来的测试传 `"mysqldump"` 进来、又从不看 argv[0]，
    // 于是"调用方传了客户端"这件事在测试里完全看不见。
    let args = mysqldump_args("mysql", &creds, "shop").unwrap();
    assert_eq!(args[0], "mysqldump", "argv[0] 必须是 dump 工具：{args:?}");
    assert_ne!(
        args[0], "mysql",
        "不能把 mysql 客户端当 dump 程序（它不认 --databases）：{args:?}"
    );
    assert!(
        args.windows(2).any(|w| w[0] == "-u" && w[1] == "root"),
        "{args:?}"
    );
    assert!(
        args.windows(2)
            .any(|w| w[0] == "--databases" && w[1] == "shop"),
        "{args:?}"
    );
    for flag in ["--single-transaction", "--quick", "--set-gtid-purged=OFF"] {
        assert!(args.contains(&flag.to_owned()), "缺 {flag}：{args:?}");
    }
    // ⚠️ `-e <sql>` 的尾巴必须被**掐掉**：dump 是命令不是 SQL，
    // 留着它会变成 `mysqldump … -e ""` 而 mysqldump 不认 `-e`。
    // 这是 `client_args` 和这里之间的一条**隐式契约**
    //（那边永远以 `-e`, `<sql>` 收尾），所以钉一条断言 ——
    // 改 `client_args` 的尾巴时这里是唯一会响的地方。
    assert!(!args.contains(&"-e".to_owned()), "没掐掉 -e：{args:?}");
    // 参数里绝不能出现密码（和 exec 同一条纪律）
    assert!(!args.iter().any(|a| a.contains("pw")), "{args:?}");
    // 非法库名在本地就拦下，不起进程
    assert!(mysqldump_args("mysql", &creds, "shop; drop").is_err());
}

#[test]
fn backup_file_names_are_sortable_and_unique_per_second() {
    // 2026-10-15 03:59:01 UTC = 1_792_036_741（用 PowerShell 换算核过）
    assert_eq!(
        backup_file_name("djxm", 1_792_036_741),
        "djxm-20261015-035901.sql"
    );
    assert_ne!(
        backup_file_name("djxm", 1_792_036_741),
        backup_file_name("djxm", 1_792_036_742)
    );
}

#[test]
fn load_script_quotes_the_path_and_optionally_picks_a_database() {
    // 恢复（面板自己的备份）：不带库名 —— 让 dump 里的 CREATE DATABASE 说话。
    // ⚠️ `--default-character-set=utf8mb4` 必须在：容器里没有 `LANG` 时
    // 客户端退回 latin1，中文会被**双重编码**存进库（实测见函数注释）。
    assert_eq!(
        load_script("mysql", "/tmp/wslc-restore/x.sql", None).unwrap(),
        "exec mysql --default-character-set=utf8mb4 < '/tmp/wslc-restore/x.sql'"
    );
    // 导入到指定库
    assert_eq!(
        load_script("mysql", "/tmp/wslc-restore/x.sql", Some("djxm")).unwrap(),
        "exec mysql --default-character-set=utf8mb4 -D 'djxm' < '/tmp/wslc-restore/x.sql'"
    );
    // ⚠️ 文件名是**用户选的**：空格 / 括号 / 中文都必须原样进去 ——
    // 不包引号的话 `mysql < /tmp/shop backup (1).sql` 会被 shell 拆开。
    assert_eq!(
        load_script("mysql", "/tmp/wslc-restore/shop backup (1).sql", None).unwrap(),
        "exec mysql --default-character-set=utf8mb4 < '/tmp/wslc-restore/shop backup (1).sql'"
    );
    assert_eq!(
        load_script("mysql", "/tmp/wslc-restore/订单.sql", Some("我的库")).unwrap(),
        "exec mysql --default-character-set=utf8mb4 -D '我的库' < '/tmp/wslc-restore/订单.sql'"
    );
    // `;` `$` 这些在**单引号里**是字面量，不需要拒
    assert_eq!(
        load_script("mysql", "/tmp/a;b$c.sql", None).unwrap(),
        "exec mysql --default-character-set=utf8mb4 < '/tmp/a;b$c.sql'"
    );
    // 只有真正没法安全表达的两个字符被拒
    assert!(load_script("mysql", "/tmp/a'b.sql", None).is_err());
    assert!(load_script("mysql", "/tmp/a\nb.sql", None).is_err());
    assert!(load_script("mysql", "/tmp/x.sql", Some("a'b")).is_err());
}

#[test]
fn sh_quote_rejects_only_what_it_cannot_express() {
    assert_eq!(sh_quote("abc").unwrap(), "'abc'");
    assert_eq!(sh_quote("a b").unwrap(), "'a b'");
    for bad in ["it's", "line\nbreak", "carriage\rreturn"] {
        assert!(sh_quote(bad).is_err(), "{bad} 应被拒");
    }
}

#[test]
fn restore_path_lands_in_its_own_directory() {
    // 恢复用的目录和备份目录**必须不同**：同名文件在这两处的语义相反
    //（一个是待恢复的导入件、一个是刚生成的导出件），混在一起会互相覆盖。
    assert_eq!(
        restore_container_path("djxm-20261015-035901.sql"),
        "/tmp/wslc-restore/djxm-20261015-035901.sql"
    );
    assert_ne!(CONTAINER_RESTORE_DIR, CONTAINER_BACKUP_DIR);
    // 面板生成的路径必须能过 `sh_quote`（它只拒单引号 / 换行）——
    // 过不了的话恢复会在本地就被拒，而那是没人会料到的
    let path = restore_container_path("djxm-20261015-035901.sql");
    assert!(
        sh_quote(&path).is_ok(),
        "面板自己生成的路径过不了 sh_quote：{path}"
    );
    assert!(
        path.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.')),
        "路径里出现了白名单外的字符：{path}"
    );
}

// -- 错误分类 -----------------------------------------------------------

#[test]
fn classifies_the_real_error_texts() {
    assert_eq!(
        classify_error(ERR_ACCESS_DENIED, Some(1)),
        MysqlErrorKind::AccessDenied
    );
    assert_eq!(
        classify_error(ERR_NOT_RUNNING, Some(1)),
        MysqlErrorKind::ContainerNotRunning
    );
    assert_eq!(
        classify_error(ERR_NO_CLIENT, Some(126)),
        MysqlErrorKind::ClientMissing
    );
    // 退出码单独也能判（126/127 是"命令没找到"）
    assert_eq!(classify_error("", Some(126)), MysqlErrorKind::ClientMissing);
}

#[test]
fn native_connection_errors_keep_their_own_classification() {
    // 原生连接（`wslc-db`）的错**已经分好类了**，`Display` 是
    // `"{标签}：{服务端原文}"`。这两条钉的是"别再拿 MySQL 的规则重猜"。
    //
    // ⚠️ 这两条是两个**实测出来的误判**，不是虚构的边界：
    //
    // ① PG 连不上（远程库 / 端口没开）：原文里有 `Connection refused`，
    //    所以旧代码命中 `ServerNotReady`，界面建议
    //    **"mysqld 还没准备好（第一次初始化要十几秒），等几秒再点"**
    //    —— 用户连的是一台远程 PostgreSQL，等多久都不会好。
    assert_eq!(
        classify_error(
            "连不上服务器：error connecting to server: Connection refused (os error 10061)",
            None
        ),
        MysqlErrorKind::Unknown,
        "原生连接失败不该被说成「mysqld 还没准备好」"
    );
    // ② PG 认证失败：旧代码一个关键词都不命中 → `Unknown` →
    //    **一句建议都没有**，用户不知道该去改口令。
    assert_eq!(
        classify_error(
            "认证失败：password authentication failed for user \"app\"",
            None
        ),
        MysqlErrorKind::AccessDenied,
        "原生认证失败要说「用户名或密码不对」"
    );
    // 库不存在 → 和 MySQL 的 `Unknown database` 同一档建议
    assert_eq!(
        classify_error("库不可用：FATAL:  database \"nodb\" does not exist", None),
        MysqlErrorKind::UnknownDatabase
    );
    // SQL 本身的错：原文照给，不给误导性的建议
    assert_eq!(
        classify_error(
            "SQL 执行出错：ERROR:  syntax error at or near \"SELEC\"",
            None
        ),
        MysqlErrorKind::SqlError
    );
    // TLS：原因和"连不上"完全不同（证书 / 服务端配置），别给"等几秒"
    assert_eq!(
        classify_error("TLS 握手失败：error performing TLS handshake", None),
        MysqlErrorKind::Unknown
    );

    // ⚠️ 反过来也要成立：**容器 exec 那条路的原文不能被误认成原生**。
    // 它没有那六个中文标签前缀，所以照旧按 MySQL 的规则走 ——
    // 否则就是"修好了原生、弄坏了原来那条"。
    assert_eq!(
        classify_error(
            "ERROR 2002 (HY000): Can't connect to local MySQL server",
            Some(1)
        ),
        MysqlErrorKind::ServerNotReady,
        "容器里的 mysqld 没起来，仍然要建议「等几秒」"
    );
}

#[test]
fn classifies_the_remaining_sql_errors() {
    assert_eq!(
        classify_error(
            "ERROR 2002 (HY000): Can't connect to local MySQL server through socket",
            Some(1)
        ),
        MysqlErrorKind::ServerNotReady
    );
    assert_eq!(
        classify_error(
            "ERROR 1142 (42000): SELECT command denied to user 'app'@'localhost'",
            Some(1)
        ),
        MysqlErrorKind::PermissionDenied
    );
    assert_eq!(
        classify_error("ERROR 1049 (42000): Unknown database 'nope'", Some(1)),
        MysqlErrorKind::UnknownDatabase
    );
    assert_eq!(
        classify_error(
            "ERROR 1064 (42000): You have an error in your SQL syntax",
            Some(1)
        ),
        MysqlErrorKind::SqlError
    );
    assert_eq!(classify_error("莫名其妙", Some(1)), MysqlErrorKind::Unknown);
}

#[test]
fn every_non_trivial_kind_tells_the_user_what_to_do() {
    for kind in [
        MysqlErrorKind::ContainerNotRunning,
        MysqlErrorKind::ClientMissing,
        MysqlErrorKind::AccessDenied,
        MysqlErrorKind::ServerNotReady,
        MysqlErrorKind::PermissionDenied,
        MysqlErrorKind::UnknownDatabase,
    ] {
        assert!(kind.hint().is_some(), "{kind:?} 应该有下一步建议");
    }
    // "等一等就好"和"出错了"要能分开
    assert!(MysqlErrorKind::ServerNotReady.is_transient());
    assert!(!MysqlErrorKind::AccessDenied.is_transient());
}
