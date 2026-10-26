//! 安装域的纯逻辑测试（从 `model/install.rs` 原样搬过来，未改一条断言）。

use super::*;

// -- 名称 --------------------------------------------------------------

#[test]
fn sanitize_keeps_only_safe_characters() {
    assert_eq!(sanitize_name("Ubuntu-24.04"), "Ubuntu-24.04");
    assert_eq!(sanitize_name("my distro"), "my-distro");
    assert_eq!(sanitize_name("my__distro"), "my-distro");
    assert_eq!(sanitize_name("a  b"), "a-b");
    assert_eq!(sanitize_name("a\\b/c"), "abc");
    assert_eq!(sanitize_name("中文名字"), "");
    assert_eq!(sanitize_name(""), "");
    assert_eq!(sanitize_name("   "), "");
}

#[test]
fn sanitize_trims_trailing_separators() {
    // 结尾的 `-` / `.` 会被 WSL 和文件系统当成"还有下文"，必须去掉
    assert_eq!(sanitize_name("Ubuntu-"), "Ubuntu");
    assert_eq!(sanitize_name("Ubuntu."), "Ubuntu");
    assert_eq!(sanitize_name("Ubuntu-. ."), "Ubuntu");
    assert_eq!(sanitize_name("-Ubuntu"), "Ubuntu");
}

#[test]
fn sanitize_truncates_to_the_limit() {
    let long = "a".repeat(40);
    assert_eq!(sanitize_name(&long).chars().count(), MAX_NAME_LEN);
    // 截断之后可能露出结尾的 `-`，要再去一次
    let name = format!("{}-bbbbbbbbbbbbbbbbbbbbbbbb", "a".repeat(24));
    let short = sanitize_name(&name);
    assert!(!short.ends_with('-'), "{short}");
    assert!(short.chars().count() <= MAX_NAME_LEN, "{short}");
}

#[test]
fn suggest_name_from_file_strips_suffix_and_platform() {
    // 参考实现的例子：`rootfs` 与平台尾巴都不该进名字
    assert_eq!(
        suggest_name_from_file(r"D:\img\ubuntu-rootfs-amd64.tar.gz"),
        "ubuntu"
    );
    assert_eq!(
        suggest_name_from_file("alpine-minirootfs-3.21.0-x86_64.tar.gz"),
        "alpine-mini-3.21.0"
    );
    assert_eq!(
        suggest_name_from_file(r"E:\a\Ubuntu-24.04.tar"),
        "Ubuntu-24.04"
    );
    assert_eq!(
        suggest_name_from_file("noble-server-cloudimg-amd64-root.tar.xz"),
        "noble-server-cloudimg"
    );
    assert_eq!(suggest_name_from_file("Debian.tar.xz"), "Debian");
    assert_eq!(suggest_name_from_file("ext4.vhdx"), "ext4");
}

#[test]
fn suggest_name_from_file_survives_junk() {
    // 大小写不敏感
    assert_eq!(suggest_name_from_file("ROOTFS-UBUNTU.TAR.GZ"), "UBUNTU");
    // 全是关键词 → 停词一上来就命中，`parts` 为空，退回整个 stem。
    // （参考实现也是这个行为：宁可能剩一个怪名字让用户改，也不要空。）
    assert_eq!(suggest_name_from_file("amd64.tar"), "amd64");
    assert_eq!(suggest_name_from_file(""), "");
    // 非 ASCII 文件名 → sanitize 之后是空串（界面会显示"名字为空"的红字）
    assert_eq!(suggest_name_from_file("中文镜像.tar"), "");
    // 多字节字符在后缀前面时，剥后缀不能切在字符中间（会 panic）
    assert_eq!(suggest_name_from_file("中-rootfs.tar.gz"), "");
}

#[test]
fn classify_local_file_branches_by_suffix_only() {
    // 三种文件在 WSL 那边是**三条不同的路**，所以这一分必须准
    assert_eq!(
        classify_local_file(r"D:\img\ext4.vhdx"),
        Some(LocalFileKind::Vhdx)
    );
    assert_eq!(
        classify_local_file(r"D:\img\ext4.vhd"),
        Some(LocalFileKind::Vhdx)
    );
    assert_eq!(
        classify_local_file(r"D:\img\Ubuntu-24.04.wsl"),
        Some(LocalFileKind::Bundle)
    );
    for tar in [
        r"D:\img\ubuntu-rootfs.tar",
        r"D:\img\ubuntu-rootfs.tar.gz",
        r"D:\img\ubuntu-rootfs.tar.xz",
        r"D:\img\ubuntu-rootfs.tar.zst",
        r"D:\img\ubuntu-rootfs.tgz",
        r"D:\img\ubuntu-rootfs.txz",
        "/mnt/d/img/rootfs.tar.bz2",
    ] {
        assert_eq!(classify_local_file(tar), Some(LocalFileKind::Tar), "{tar}");
    }

    // 大小写不敏感（Windows 上的文件名本来就不区分大小写）
    assert_eq!(
        classify_local_file(r"D:\img\EXT4.VHDX"),
        Some(LocalFileKind::Vhdx)
    );
    assert_eq!(
        classify_local_file(r"D:\img\UBUNTU.TAR.GZ"),
        Some(LocalFileKind::Tar)
    );
    // 复合后缀按**长到短**匹配（`.gz` 不能先把 `.tar.gz` 吃掉）
    assert_eq!(classify_local_file("x.tar.gz"), Some(LocalFileKind::Tar));

    // 认不出来就**说认不出来**（调用方按 tar 处理并如实说明），别硬猜
    assert_eq!(classify_local_file(r"D:\img\rootfs"), None);
    assert_eq!(classify_local_file(r"D:\img\rootfs.zip"), None);
    assert_eq!(classify_local_file(""), None);
    assert_eq!(classify_local_file("   "), None);
    // 只给了目录、没有文件名
    assert_eq!(classify_local_file(r"D:\img\"), None);
    // 多字节字符在前也不能 panic
    assert_eq!(
        classify_local_file("中文镜像.tar"),
        Some(LocalFileKind::Tar)
    );
    assert_eq!(classify_local_file("中文镜像"), None);
}

#[test]
fn name_syntax_rules_match_what_we_tell_users() {
    assert!(name_syntax_errors("Ubuntu-24.04").is_empty());
    assert!(name_syntax_errors("a_b.c-d").is_empty());

    assert!(
        name_syntax_errors("")
            .iter()
            .any(|e| e.contains("不能为空"))
    );
    assert!(name_syntax_errors("a b").iter().any(|e| e.contains(' ')));
    assert!(name_syntax_errors("中文").iter().any(|e| e.contains('中')));
    assert!(
        name_syntax_errors(&"a".repeat(MAX_NAME_LEN + 1))
            .iter()
            .any(|e| e.contains("不能超过"))
    );
    // 恰好等于上限是合法的
    assert!(name_syntax_errors(&"a".repeat(MAX_NAME_LEN)).is_empty());
}

// -- 目录 --------------------------------------------------------------

#[test]
fn absolute_path_check_does_not_depend_on_the_build_platform() {
    assert!(is_absolute_windows_path(r"D:\wsl"));
    assert!(is_absolute_windows_path("D:/wsl"));
    assert!(is_absolute_windows_path(r"\\server\share"));
    assert!(is_absolute_windows_path(r"c:\x"));

    assert!(!is_absolute_windows_path(r"wsl\X"));
    assert!(!is_absolute_windows_path("wsl"));
    assert!(!is_absolute_windows_path("D:"));
    assert!(!is_absolute_windows_path(r"D:relative"));
    assert!(!is_absolute_windows_path(""));
}

#[test]
fn install_dir_follows_the_separator_of_the_default_dir() {
    assert_eq!(
        derive_install_dir(Some(r"D:\wsl"), "Ubuntu"),
        Some(r"D:\wsl\Ubuntu".to_owned())
    );
    // 默认目录用 `/`，我们不要自作主张混用
    assert_eq!(
        derive_install_dir(Some("D:/wsl"), "Ubuntu"),
        Some("D:/wsl/Ubuntu".to_owned())
    );
    // 结尾多余的分隔符不要拼出 `\\`
    assert_eq!(
        derive_install_dir(Some(r"D:\wsl\"), "Ubuntu"),
        Some(r"D:\wsl\Ubuntu".to_owned())
    );
    assert_eq!(
        derive_install_dir(Some("D:/wsl/"), "Ubuntu"),
        Some("D:/wsl/Ubuntu".to_owned())
    );
    // 缺一半就给 None（调用方据此要求用户自己填）
    assert_eq!(derive_install_dir(None, "Ubuntu"), None);
    assert_eq!(derive_install_dir(Some("   "), "Ubuntu"), None);
    assert_eq!(derive_install_dir(Some(r"D:\wsl"), "  "), None);
}

// -- 在线列表 ----------------------------------------------------------

#[test]
fn online_list_of_a_failed_run_is_silently_empty() {
    // 本机实测的原始输出（wsl -l -o 拉不到 raw.githubusercontent.com）
    let text = include_str!("../../../tests/fixtures/wsl_list_online_failed.txt");
    assert!(
        parse_online_list(text).is_empty(),
        "失败输出必须被吃成空列表，而不是抛错"
    );
    // 空输入、只有提示语、只有表头都不能 panic
    assert!(parse_online_list("").is_empty());
    assert!(parse_online_list("以下是可安装的有效分发的列表：").is_empty());
    assert!(parse_online_list("NAME  FRIENDLY NAME").is_empty());
}

#[test]
fn online_list_parses_the_two_column_table() {
    // 真实抓下来的 `wsl -l -o` **成功**输出（28 行，1394 字节）——
    // 2026-10-10 开着 dev-sidecar 系统代理时抓的：`wsl.exe` 走系统代理，
    // 所以它能去 `raw.githubusercontent.com` 取清单；
    // 而面板自己的 `curl.exe` 不走系统代理（见 `cmd::distro` 的兜底说明）。
    let text = include_str!("../../../tests/fixtures/wsl_list_online.txt");
    let items = parse_online_list(text);
    assert!(items.len() >= 15, "只解析出 {} 项：{items:?}", items.len());

    // 提示语与表头之间的空行、以及中文提示行都不能进来
    assert!(
        !items.iter().any(|item| item.id.contains("以下是")),
        "{items:?}"
    );
    // 两列的名字要拆对：id 是第一列，友好名可能是**带空格**的多个词
    let ubuntu = items
        .iter()
        .find(|item| item.id == "Ubuntu-24.04")
        .expect("应该有 Ubuntu-24.04");
    assert_eq!(ubuntu.label, "Ubuntu 24.04 LTS");
    // 不带版本号的那个是默认项，它两列同名
    let default = items
        .iter()
        .find(|item| item.id == "Ubuntu")
        .expect("应该有 Ubuntu");
    assert_eq!(default.label, "Ubuntu");
}

#[test]
fn online_list_ignores_junk_lines_after_the_header() {
    let text = "NAME  FRIENDLY NAME\n----  -------------\nUbuntu  Ubuntu\n";
    let items = parse_online_list(text);
    // 分隔线被过滤掉：它的第一列是 `----`，虽然每个字符都"合法"，
    // 但里面没有字母数字 —— 真正的发行版名不会长这样。
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0].id, "Ubuntu");
}

#[test]
fn distribution_info_fixture_is_parsed() {
    // 真实抓下来的微软清单（cdn.jsdelivr.net 镜像的 microsoft/WSL@master，
    // 18481 字节，2026-10-10 抓取）。
    let json = include_str!("../../../tests/fixtures/ms_distribution_info.json");
    let items = parse_distribution_info(json).expect("真实清单应该能解析");
    assert!(items.len() >= 5, "只解析出 {} 项", items.len());

    let ubuntu = items
        .iter()
        .find(|d| d.id == "Ubuntu-24.04")
        .expect("清单里应该有 Ubuntu-24.04");
    assert_eq!(ubuntu.label, "Ubuntu 24.04 LTS");
    // 默认项排最前（不带版本号的那个）
    assert_eq!(
        items[0].id, "Ubuntu",
        "默认发行版应该排第一：{:?}",
        items[0]
    );
    // 每项都要有 id 和非空 label
    for item in &items {
        assert!(!item.id.is_empty());
        assert!(!item.label.is_empty(), "{item:?}");
    }
}

#[test]
fn distribution_info_failures_are_reported_not_panicked() {
    assert!(parse_distribution_info("").is_err());
    assert!(parse_distribution_info("{}").is_err());
    assert!(parse_distribution_info(r#"{"ModernDistributions":{}}"#).is_err());
    assert!(parse_distribution_info(r#"{"ModernDistributions":{"X":[]}}"#).is_err());
    // 条目缺 Name 的跳过，缺 FriendlyName 的退回 id
    let json = r#"{"ModernDistributions":{"X":[{"FriendlyName":"nope"},
        {"Name":"Debian"},{"Name":"Alpine","FriendlyName":"Alpine Linux"}]}}"#;
    let items = parse_distribution_info(json).unwrap();
    assert_eq!(items.len(), 2, "{items:?}");
    assert_eq!(items[0].id, "Debian");
    assert_eq!(items[0].label, "Debian");
}

#[test]
fn online_search_matches_id_and_label() {
    let d = OnlineDistro::new("Ubuntu-24.04", "Ubuntu 24.04 LTS");
    assert!(online_matches(&d, ""));
    assert!(online_matches(&d, "ubuntu"));
    assert!(online_matches(&d, "24.04"));
    assert!(online_matches(&d, "LTS"));
    assert!(!online_matches(&d, "debian"));
}

// -- 计划 --------------------------------------------------------------

fn ctx() -> PlanContext {
    PlanContext {
        default_dir: Some(r"D:\wsl".to_owned()),
        temp_dir: r"C:\Temp\wslc-panel".to_owned(),
        stamp: "1234-5678".to_owned(),
        wslconfig_sparse: false,
    }
}

fn labels(plan: &InstallPlan) -> Vec<String> {
    plan.steps
        .iter()
        .map(|s| format!("{:?}", s.program))
        .collect()
}

#[test]
fn tar_plan_creates_the_dir_then_imports() {
    let spec = InstallSpec::new(
        "Ubuntu",
        InstallSource::Tar {
            path: r"D:\img\ubuntu.tar".to_owned(),
        },
    );
    let plan = super::plan(&spec, &ctx()).unwrap();

    assert_eq!(plan.name, "Ubuntu");
    assert_eq!(plan.install_dir, r"D:\wsl\Ubuntu");
    assert_eq!(labels(&plan), vec!["CreateDir", "Wsl"]);
    assert_eq!(plan.steps[0].args, vec![r"D:\wsl\Ubuntu"]);
    // 逐字节钉住参数：错了只表现为"界面上什么也没发生"
    assert_eq!(
        plan.steps[1].args,
        vec![
            "--import",
            "Ubuntu",
            r"D:\wsl\Ubuntu",
            r"D:\img\ubuntu.tar",
            "--version",
            "2"
        ]
    );
    assert_eq!(plan.preview_lines()[0], r"1. 创建安装目录 D:\wsl\Ubuntu");
    assert_eq!(
        plan.preview_lines()[1],
        r"2. wsl --import Ubuntu D:\wsl\Ubuntu D:\img\ubuntu.tar --version 2"
    );
}

#[test]
fn vhdx_plan_uses_the_vhd_flag() {
    let spec = InstallSpec::new(
        "MyDisk",
        InstallSource::Vhdx {
            path: r"D:\img\ext4.vhdx".to_owned(),
        },
    );
    let plan = super::plan(&spec, &ctx()).unwrap();
    assert_eq!(
        plan.steps.last().unwrap().args,
        vec![
            "--import",
            "MyDisk",
            r"D:\wsl\MyDisk",
            r"D:\img\ext4.vhdx",
            "--vhd",
            "--version",
            "2"
        ]
    );
}

#[test]
fn file_plan_passes_name_and_optional_location() {
    let mut spec = InstallSpec::new(
        "MyDistro",
        InstallSource::File {
            path: r"D:\img\rootfs.tar.gz".to_owned(),
        },
    );
    // 留空安装目录 → 用默认目录推出来，仍然是 `--location`
    let plan = super::plan(&spec, &ctx()).unwrap();
    assert_eq!(
        plan.steps[0].args,
        vec![
            "--install",
            "--from-file",
            r"D:\img\rootfs.tar.gz",
            "--name",
            "MyDistro",
            "--location",
            r"D:\wsl\MyDistro"
        ]
    );

    // 显式给目录
    spec.install_dir = r"E:\wsl\MyDistro".to_owned();
    let plan = super::plan(&spec, &ctx()).unwrap();
    assert!(plan.steps[0].args.iter().any(|a| a == r"E:\wsl\MyDistro"));

    // 上下文里也没有默认目录 → 老实不传 --location
    let bare = PlanContext {
        default_dir: None,
        ..ctx()
    };
    let mut no_dir = spec.clone();
    no_dir.install_dir = String::new();
    let plan = super::plan(&no_dir, &bare).unwrap();
    assert!(!plan.steps[0].args.iter().any(|a| a == "--location"));
    assert!(plan.install_dir.is_empty());
}

#[test]
fn online_plan_relocates_when_the_name_differs() {
    let mut spec = InstallSpec::new(
        "MyUbuntu",
        InstallSource::Online {
            id: "Ubuntu-24.04".to_owned(),
            launch: false,
            web_download: false,
            fallback_id: None,
        },
    );
    let plan = super::plan(&spec, &ctx()).unwrap();

    assert_eq!(
        labels(&plan),
        vec!["Wsl", "WaitRegistered", "EnsureRelocated"]
    );
    // 名字不同时**不能**传 --location：那个目录是给最终名字准备的，
    // 先装成 id 再用它，WSL 会建出一个位置对不上的发行版。
    assert_eq!(
        plan.steps[0].args,
        vec![
            "--install",
            "-d",
            "Ubuntu-24.04",
            "--version",
            "2",
            "--no-launch"
        ]
    );
    // 换下载源的兜底同样挂着（这条与名字无关）
    assert!(plan.steps[0].retry_other_source);
    // 重定位不可取消（中途打断会丢数据）
    assert!(!plan.steps[2].cancellable);
    assert_eq!(
        plan.steps[2].args,
        vec![
            "Ubuntu-24.04",
            "MyUbuntu",
            r"D:\wsl\MyUbuntu",
            r"C:\Temp\wslc-panel\wslc-panel-Ubuntu-24.04-1234-5678.tar"
        ]
    );
    // 说明里要讲清这一段在干什么
    assert!(
        plan.notes.iter().any(|n| n.contains("--unregister")),
        "{:?}",
        plan.notes
    );

    // ⚠️ 勾了「装完立即启动」，但这一次**要改名**（`Ubuntu-24.04` → `MyUbuntu`），
    // 所以仍然必须 `--no-launch`：运行中的发行版搬不动，而实测（2026-10-12）
    // 紧接着 `--install` 之后 `--move` 就会撞 `ERROR_SHARING_VIOLATION`。
    spec.source = InstallSource::Online {
        id: "Ubuntu-24.04".to_owned(),
        launch: true,
        web_download: true,
        fallback_id: None,
    };
    let plan = super::plan(&spec, &ctx()).unwrap();
    assert!(plan.steps[0].args.iter().any(|a| a == "--web-download"));
    assert!(
        plan.steps[0].args.iter().any(|a| a == "--no-launch"),
        "要重定位时必须压住那个开关：{:?}",
        plan.steps[0].args
    );
    // 用户勾的东西不能被无声吞掉 —— 说明里要讲清为什么没启动
    assert!(
        plan.notes.iter().any(|n| n.contains("装完立即启动")),
        "{:?}",
        plan.notes
    );

    // 不需要重定位（名字就是 id、也没有目标目录）时，那个开关照旧生效
    let same = InstallSpec::new(
        "Debian",
        InstallSource::Online {
            id: "Debian".to_owned(),
            launch: true,
            web_download: false,
            fallback_id: None,
        },
    );
    let bare = PlanContext {
        default_dir: None,
        ..ctx()
    };
    let plan = super::plan(&same, &bare).unwrap();
    assert_eq!(labels(&plan), vec!["Wsl", "WaitRegistered"]);
    assert!(
        !plan.steps[0].args.iter().any(|a| a == "--no-launch"),
        "不重定位时不该压掉用户的开关：{:?}",
        plan.steps[0].args
    );
    assert!(!plan.notes.iter().any(|n| n.contains("装完立即启动")));
}

#[test]
fn a_bare_online_id_carries_a_versioned_fallback() {
    // 本机实测：裸 `Ubuntu` 那条 2 秒就 WININET_E_CANNOT_CONNECT，
    // 而 `Ubuntu-26.04` 能下 —— 所以清单里给了兄弟时，计划要带上"换 id 重试"
    let spec = InstallSpec::new(
        "Ubuntu",
        InstallSource::Online {
            id: "Ubuntu".to_owned(),
            launch: false,
            web_download: false,
            fallback_id: Some("Ubuntu-26.04".to_owned()),
        },
    );
    let plan = super::plan(&spec, &ctx()).unwrap();
    assert_eq!(
        plan.steps[0].retry_other_id.as_deref(),
        Some("Ubuntu-26.04")
    );
    // 换源那一手也要在（两条通道在不同网络下各会挂一条）
    assert!(plan.steps[0].retry_other_source);

    // 没给兄弟（`Debian`）时不能瞎猜
    let spec = InstallSpec::new(
        "Debian",
        InstallSource::Online {
            id: "Debian".to_owned(),
            launch: false,
            web_download: false,
            fallback_id: None,
        },
    );
    let plan = super::plan(&spec, &ctx()).unwrap();
    assert_eq!(plan.steps[0].retry_other_id, None);

    // 和主 id 一样的"兄弟"等于没给（防止上游算错时多跑一轮）
    let spec = InstallSpec::new(
        "Ubuntu-24.04",
        InstallSource::Online {
            id: "Ubuntu-24.04".to_owned(),
            launch: false,
            web_download: false,
            fallback_id: Some("Ubuntu-24.04".to_owned()),
        },
    );
    let plan = super::plan(&spec, &ctx()).unwrap();
    assert_eq!(plan.steps[0].retry_other_id, None);
}

#[test]
fn online_plan_never_passes_location_when_the_name_is_the_id() {
    let spec = InstallSpec::new(
        "Ubuntu-24.04",
        InstallSource::Online {
            id: "Ubuntu-24.04".to_owned(),
            launch: false,
            web_download: false,
            fallback_id: None,
        },
    );
    let plan = super::plan(&spec, &ctx()).unwrap();
    // ⚠️ **不能**带 `--location`：带上它 WSL 会改走一条不认系统代理的下载通道，
    // 本机实测直接 `WININET_E_CANNOT_CONNECT`（参考实现也从不传它）
    assert_eq!(
        plan.steps[0].args,
        vec![
            "--install",
            "-d",
            "Ubuntu-24.04",
            "--version",
            "2",
            "--no-launch"
        ]
    );
    // 位置由后面那一步"必要时重定位"补正（同名走 `--manage --move`）
    assert_eq!(
        labels(&plan),
        vec!["Wsl", "WaitRegistered", "EnsureRelocated"]
    );
    assert_eq!(plan.steps[2].args[2], r"D:\wsl\Ubuntu-24.04");
    assert!(!plan.steps[2].cancellable);
    // 失败时能自动换一个下载源再试（两条通道的可用性不同）
    assert!(plan.steps[0].retry_other_source);
    // 没有目录、名字也相同 → 连重定位那一步都不需要
    let bare = PlanContext {
        default_dir: None,
        ..ctx()
    };
    let mut no_dir = spec.clone();
    no_dir.install_dir = String::new();
    let plan = super::plan(&no_dir, &bare).unwrap();
    assert_eq!(labels(&plan), vec!["Wsl", "WaitRegistered"]);
}

#[test]
fn mirror_plan_downloads_then_imports_then_cleans_up() {
    let spec = InstallSpec::new(
        "Ubuntu-24.04",
        InstallSource::Mirror {
            url: "https://mirrors.tuna.tsinghua.edu.cn/ubuntu-cloud-images/noble/current/noble-server-cloudimg-amd64-root.tar.xz".to_owned(),
            mirror: "清华 TUNA".to_owned(),
            release: "noble".to_owned(),
            format: "tar.xz".to_owned(),
        },
    );
    let plan = super::plan(&spec, &ctx()).unwrap();
    assert_eq!(
        labels(&plan),
        vec!["CreateDir", "Curl", "CreateDir", "Wsl", "RemoveFile"]
    );
    // 第 1 步必须是**建临时目录**：`curl -o` 不会自己建目录，父目录不存在时
    // 它只给 `curl: (23) client returned ERROR on write of N bytes`（本机实测），
    // 而 `%TEMP%\wslc-panel` 平时并不存在（还会被 %TEMP% 清理回收）。
    assert_eq!(plan.steps[0].args, vec![r"C:\Temp\wslc-panel"]);
    assert_eq!(
        plan.preview_lines()[0],
        r"1. 创建临时目录 C:\Temp\wslc-panel"
    );
    assert_eq!(
        plan.steps[1].args,
        vec![
            "-s",
            "-S",
            "-L",
            "--retry",
            "2",
            "--connect-timeout",
            "10",
            "-o",
            r"C:\Temp\wslc-panel\wslc-panel-rootfs-noble-1234-5678.tar.xz",
            "https://mirrors.tuna.tsinghua.edu.cn/ubuntu-cloud-images/noble/current/noble-server-cloudimg-amd64-root.tar.xz"
        ]
    );
    // 第 2 步建的是**安装目录**（和临时目录不是一回事）
    assert_eq!(plan.steps[2].args, vec![r"D:\wsl\Ubuntu-24.04"]);
    assert_eq!(plan.steps[3].args[0], "--import");
    // 删掉的必须是**下载下来的那个文件**（curl 参数里 `-o` 后面那个）
    assert_eq!(plan.steps[4].args[0], plan.steps[1].args[8]);
    // 下载那一步是可以取消的
    assert!(plan.steps[1].cancellable);
    assert!(plan.preview_lines()[1].starts_with("2. curl -s -S -L"));
}

#[test]
fn mirror_plan_uses_the_installer_for_wsl_bundles() {
    // 清单里 Ubuntu 24.04 的多数来源是 `.wsl` 包 —— 那种必须走 `--install --from-file`，
    // `--import` 吃不了（参考实现一律 `--import`，挑到这种来源必然失败）
    let spec = InstallSpec::new(
        "Ubuntu-24.04",
        InstallSource::Mirror {
            url: "https://mirrors.tuna.tsinghua.edu.cn/ubuntu-releases/24.04/ubuntu-24.04.5-wsl-amd64.wsl".to_owned(),
            mirror: "tsinghua".to_owned(),
            release: "24.04".to_owned(),
            format: "wsl".to_owned(),
        },
    );
    let plan = super::plan(&spec, &ctx()).unwrap();
    assert_eq!(
        labels(&plan),
        vec!["CreateDir", "Curl", "Wsl", "EnsureRelocated", "RemoveFile"]
    );
    // 临时文件按格式取后缀
    assert!(
        plan.steps[1].args[8].ends_with(".wsl"),
        "{:?}",
        plan.steps[1].args
    );
    assert_eq!(
        plan.steps[2].args,
        vec![
            "--install",
            "--from-file",
            r"C:\Temp\wslc-panel\wslc-panel-rootfs-24.04-1234-5678.wsl",
            "--name",
            "Ubuntu-24.04",
            "--location",
            r"D:\wsl\Ubuntu-24.04",
        ]
    );
    // 位置要核实（`--location` 不一定被采纳），但那一步不能取消
    assert_eq!(plan.steps[3].args[0], "Ubuntu-24.04");
    assert!(!plan.steps[3].cancellable);
    // ⚠️ 中转 tar 的路径必须是**真路径**（第 4 个参数）。
    //
    // 原来是 `String::new()`：`--manage --move` 失败后会退到
    // 「导出 → 注销 → 导入」，而那条路的第一步就是
    // `wsl --export <name> ""` —— 整条退路等于废掉。
    // 而这条回退是**设计上会出现**的（本机 2026-10-12 实测：
    // 紧接着 `--install` 之后跑 `--move` 会撞 `ERROR_SHARING_VIOLATION`）。
    assert_eq!(
        plan.steps[3].args[3],
        r"C:\Temp\wslc-panel\wslc-panel-relocate-Ubuntu-24.04-1234-5678.tar"
    );
    // 删临时文件必须排在重定位之后（重定位可能还要读它？——不会，
    // `--move` 用的是发行版自己的 VHD，这里只是把顺序钉住）
    assert_eq!(plan.steps[4].program, PlanProgram::RemoveFile);
    assert!(plan.notes.iter().any(|n| n.contains(".wsl")));
}

#[test]
fn every_download_step_has_its_directory_created_first() {
    /// 同时认 `\` 与 `/`。
    ///
    /// ⚠️ 这里**不能**用 `Path::parent()`：它在非 Windows 上不把 `\` 当分隔符，
    /// 这条断言会随编译平台给出不同结论（和 `is_absolute_windows_path` 的
    /// 理由一样 —— CI 与开发机的结论必须一致）。
    fn normalize_dir(path: &str) -> String {
        path.trim_end_matches(['\\', '/']).to_ascii_lowercase()
    }
    fn parent_dir(file: &str) -> String {
        let trimmed = file.trim_end_matches(['\\', '/']);
        match trimmed.rfind(['\\', '/']) {
            Some(at) => normalize_dir(&trimmed[..at]),
            None => String::new(),
        }
    }

    // 现在只有镜像源会下载；两种格式都要覆盖（它们走的是不同的安装步骤）
    let specs = [
        InstallSpec::new(
            "X",
            InstallSource::Mirror {
                url: "https://mirrors.tuna.tsinghua.edu.cn/x/rootfs.tar.xz".to_owned(),
                mirror: "m".to_owned(),
                release: "noble".to_owned(),
                format: "tar.xz".to_owned(),
            },
        ),
        InstallSpec::new(
            "X",
            InstallSource::Mirror {
                url: "https://mirrors.tuna.tsinghua.edu.cn/x/u.wsl".to_owned(),
                mirror: "m".to_owned(),
                release: "24.04".to_owned(),
                format: "wsl".to_owned(),
            },
        ),
    ];

    for spec in &specs {
        let plan = super::plan(spec, &ctx()).unwrap();
        let mut created: Vec<String> = Vec::new();

        for (index, step) in plan.steps.iter().enumerate() {
            match step.program {
                PlanProgram::CreateDir => created.push(normalize_dir(&step.args[0])),
                PlanProgram::Curl => {
                    let at = step
                        .args
                        .iter()
                        .position(|arg| arg == "-o")
                        .expect("下载步骤必然有 -o");
                    let parent = parent_dir(&step.args[at + 1]);
                    assert!(
                        created.contains(&parent),
                        "第 {} 步要往 {parent} 里下载，但前面没有任何一步建过这个目录 —— \
                         `curl.exe` 不会自己建，它只会回一句 `curl: (23) client returned \
                         ERROR on write of N bytes`（本机实测）。计划：{:?}",
                        index + 1,
                        plan.preview_lines()
                    );
                }
                _ => {}
            }
        }
    }
}

#[test]
fn source_names_lists_what_the_install_registers_first() {
    // 改名安装：`wsl -l -v` 上先是 `Ubuntu-24.04`，重定位之后才是 `MyUbuntu`。
    // 界面要靠这份名单才知道"列表上那一行正在被安装"（见 `InstallPlan::source_names`）。
    let spec = InstallSpec::new(
        "MyUbuntu",
        InstallSource::Online {
            id: "Ubuntu-24.04".to_owned(),
            launch: false,
            web_download: false,
            fallback_id: Some("Ubuntu-26.04".to_owned()),
        },
    );
    let plan = super::plan(&spec, &ctx()).unwrap();
    // 兄弟 id 也要在里面：它同样会先注册成那个名字
    assert_eq!(plan.source_names(), vec!["Ubuntu-24.04", "Ubuntu-26.04"]);

    // 名字就是 id（最常见的路径）→ 不必重复记
    let same = InstallSpec::new(
        "Debian",
        InstallSource::Online {
            id: "Debian".to_owned(),
            launch: false,
            web_download: false,
            fallback_id: None,
        },
    );
    assert!(
        super::plan(&same, &ctx())
            .unwrap()
            .source_names()
            .is_empty()
    );

    // 只差大小写也算"同一个名字"（`busy_with` 那边本来就不区分大小写）
    let cased = InstallSpec::new(
        "ubuntu-24.04",
        InstallSource::Online {
            id: "Ubuntu-24.04".to_owned(),
            launch: false,
            web_download: false,
            fallback_id: None,
        },
    );
    assert!(
        super::plan(&cased, &ctx())
            .unwrap()
            .source_names()
            .is_empty()
    );

    // 本地来源不会"先注册成别的名字" → 永远是空的
    for source in [
        InstallSource::Tar {
            path: r"D:\a.tar".to_owned(),
        },
        InstallSource::Vhdx {
            path: r"D:\a.vhdx".to_owned(),
        },
        InstallSource::File {
            path: r"D:\a.wsl".to_owned(),
        },
        InstallSource::Mirror {
            url: "https://x/rootfs.tar.xz".to_owned(),
            mirror: "m".to_owned(),
            release: "1".to_owned(),
            format: "tar.xz".to_owned(),
        },
    ] {
        let plan = super::plan(&InstallSpec::new("MyDistro", source), &ctx()).unwrap();
        assert!(plan.source_names().is_empty(), "{:?}", plan.preview_lines());
    }
}

#[test]
fn set_default_and_sparse_are_appended_steps() {
    let mut spec = InstallSpec::new(
        "Ubuntu",
        InstallSource::Tar {
            path: r"D:\a.tar".to_owned(),
        },
    );
    spec.set_default = true;

    let plan = super::plan(&spec, &ctx()).unwrap();
    let last = plan.steps.last().unwrap();
    assert_eq!(last.program, PlanProgram::Wsl);
    assert_eq!(last.args, vec!["--set-default", "Ubuntu"]);
    assert!(
        plan.preview_lines()
            .last()
            .unwrap()
            .contains("--set-default")
    );

    // `.wslconfig` 要求稀疏 → 在设默认之前插一步
    let sparse_ctx = PlanContext {
        wslconfig_sparse: true,
        ..ctx()
    };
    let plan = super::plan(&spec, &sparse_ctx).unwrap();
    let programs: Vec<PlanProgram> = plan.steps.iter().map(|s| s.program).collect();
    assert_eq!(
        programs,
        vec![
            PlanProgram::CreateDir,
            PlanProgram::Wsl,
            PlanProgram::Wsl,
            PlanProgram::Wsl
        ]
    );
    assert!(plan.steps[2].args.iter().any(|a| a == "--set-sparse"));
    // ⚠️ 必须带 `--allow-unsafe`，否则 WSL 默认拒绝开稀疏盘（实测它的报错里
    //    自己给出的命令就带这个开关）—— 不带的话这条步骤在真机上必然失败
    assert!(
        plan.steps[2].args.iter().any(|a| a == "--allow-unsafe"),
        "{:?}",
        plan.steps[2].args
    );
    // 而且它失败**不算安装失败**：那一刻发行版已经装好了
    assert!(plan.steps[2].best_effort);
    // ⚠️ 同时要**忙就重试**：紧接着 `--install` 跑的时候 VHDX 还被 WSL 自己
    //    开着（实测 `Wsl/Service/ERROR_SHARING_VIOLATION`），而它是
    //    `best_effort` —— 不重试就等于用户 `.wslconfig` 里的
    //    `sparseVhd=true` 静默不生效（界面上没有第二个地方能开它）。
    assert!(
        plan.steps[2].retry_while_busy,
        "开稀疏盘那一步必须能在'VHD 还被占着'时重试：{:?}",
        plan.steps[2]
    );
    // 别的步骤都不该带这个标记：它们失败的原因不是"资源被占着"，
    // 等一会儿只会白白拖住安装
    assert!(
        plan.steps
            .iter()
            .enumerate()
            .all(|(index, step)| index == 2 || !step.retry_while_busy),
        "{:?}",
        plan.preview_lines()
    );
    assert!(plan.notes.iter().any(|n| n.contains("sparseVhd")));
}

#[test]
fn plan_rejects_bad_input_with_readable_messages() {
    // 名字为空 + tar 路径为空 + 没目录 → 三条错一起给出来
    let bare = PlanContext {
        default_dir: None,
        ..ctx()
    };
    let spec = InstallSpec::new(
        "  ",
        InstallSource::Tar {
            path: " ".to_owned(),
        },
    );
    let err = super::plan(&spec, &bare).unwrap_err();
    assert!(err.contains("发行版名不能为空"), "{err}");
    assert!(err.contains("tar 文件路径不能为空"), "{err}");
    assert!(err.contains("必须指定安装目录"), "{err}");

    // 相对目录
    let mut relative = InstallSpec::new(
        "X",
        InstallSource::Tar {
            path: r"D:\a.tar".to_owned(),
        },
    );
    relative.install_dir = r"wsl\X".to_owned();
    let err = super::plan(&relative, &ctx()).unwrap_err();
    assert!(err.contains("绝对路径"), "{err}");

    // 在线安装：没选发行版
    let no_id = InstallSpec::new(
        "X",
        InstallSource::Online {
            id: "  ".to_owned(),
            launch: false,
            web_download: false,
            fallback_id: None,
        },
    );
    assert!(super::plan(&no_id, &ctx()).unwrap_err().contains("请先选"));
    // 在线安装要改名却没目录
    let no_dir = InstallSpec::new(
        "X",
        InstallSource::Online {
            id: "Ubuntu".to_owned(),
            launch: false,
            web_download: false,
            fallback_id: None,
        },
    );
    let err = super::plan(&no_dir, &bare).unwrap_err();
    assert!(err.contains("安装目录"), "{err}");

    // 镜像站没选 URL / URL 形状不对
    let no_url = InstallSpec::new(
        "X",
        InstallSource::Mirror {
            url: String::new(),
            mirror: "清华 TUNA".to_owned(),
            release: "noble".to_owned(),
            format: "tar.xz".to_owned(),
        },
    );
    assert!(
        super::plan(&no_url, &ctx())
            .unwrap_err()
            .contains("还没有选好下载地址")
    );
    let bad_url = InstallSpec::new(
        "X",
        InstallSource::Mirror {
            url: r"D:\a.tar.xz".to_owned(),
            mirror: "本地".to_owned(),
            release: "noble".to_owned(),
            format: "tar.xz".to_owned(),
        },
    );
    assert!(super::plan(&bad_url, &ctx()).unwrap_err().contains("http"));
}

#[test]
fn preflight_blocks_duplicate_names_and_non_empty_dirs() {
    let spec = InstallSpec::new(
        "Ubuntu",
        InstallSource::Tar {
            path: r"D:\a.tar".to_owned(),
        },
    );
    let taken = vec!["Ubuntu".to_owned(), "Debian".to_owned()];

    let clean = preflight(&spec, &ctx(), &["Debian".to_owned()], false);
    assert!(clean.ok(), "{clean:?}");

    let duplicate = preflight(&spec, &ctx(), &taken, false);
    assert!(!duplicate.ok());
    assert!(duplicate.error_text().unwrap().contains("已经有一个"));
    // 重名这件事**不能**让用户以为我们会替他删
    assert!(duplicate.error_text().unwrap().contains("不会替你删"));

    let occupied = preflight(&spec, &ctx(), &[], true);
    assert!(!occupied.ok());
    assert!(occupied.error_text().unwrap().contains("空目录"));
}

#[test]
fn field_errors_map_messages_to_the_right_input() {
    // 名字 + 路径 + 目录三条一起错：三块都要标红。
    let spec = InstallSpec::new(
        "  ",
        InstallSource::Tar {
            path: " ".to_owned(),
        },
    );
    let bare = PlanContext {
        default_dir: None,
        ..ctx()
    };
    let check = preflight(&spec, &bare, &[], false);
    let fields = check.field_errors();
    assert!(check.errors.len() >= 3, "{check:?}");
    assert!(fields.name, "{check:?} / {fields:?}");
    assert!(fields.source, "{check:?} / {fields:?}");
    assert!(fields.install_dir, "{check:?} / {fields:?}");
    assert!(fields.any());

    // 重名只算名字，不要把目录也标红。
    let duplicate = InstallSpec::new(
        "Ubuntu",
        InstallSource::Tar {
            path: r"D:\a.tar".to_owned(),
        },
    );
    let fields = preflight(&duplicate, &ctx(), &["Ubuntu".to_owned()], false).field_errors();
    assert!(fields.name, "{fields:?}");
    assert!(!fields.source, "{fields:?}");
    assert!(!fields.install_dir, "{fields:?}");

    // 目录已经有东西：只标目录。
    let fields = preflight(&duplicate, &ctx(), &[], true).field_errors();
    assert!(!fields.name, "{fields:?}");
    assert!(!fields.source, "{fields:?}");
    assert!(fields.install_dir, "{fields:?}");

    // 在线来源没选：只标来源。
    let online = InstallSpec::new(
        "X",
        InstallSource::Online {
            id: String::new(),
            launch: false,
            web_download: false,
            fallback_id: None,
        },
    );
    let fields = preflight(&online, &ctx(), &[], false).field_errors();
    assert!(!fields.name, "{fields:?}");
    assert!(fields.source, "{fields:?}");
}

#[test]
fn preflight_treats_names_case_insensitively() {
    // ⚠️ 这条是"成功的安装被判成失败"那个 bug 的回归保护。
    //
    // 已经有 `Ubuntu` 时用户装 `ubuntu`：preflight 若按 `==` 判，
    // 这里会放行；而装完之后 `WaitRegistered` 拿 `==` 在 `wsl -l -q`
    // 的输出里找不到 `ubuntu`（那里写的是 `Ubuntu`），
    // 于是轮询 30 秒后报"装完了却一直没出现在列表里"。
    // WSL 自己对发行版名的比较就是不区分大小写的。
    let spec = InstallSpec::new(
        "ubuntu",
        InstallSource::Tar {
            path: r"D:\a.tar".to_owned(),
        },
    );
    let taken = vec!["Ubuntu".to_owned()];

    let duplicate = preflight(&spec, &ctx(), &taken, false);
    assert!(
        !duplicate.ok(),
        "大小写不同但其实是同一个发行版，必须挡住：{duplicate:?}"
    );
    assert!(duplicate.error_text().unwrap().contains("已经有一个"));

    // 反方向也要成立（清单给 `Ubuntu`、列表里是 `ubuntu`）
    let spec = InstallSpec::new(
        "Ubuntu",
        InstallSource::Tar {
            path: r"D:\a.tar".to_owned(),
        },
    );
    let duplicate = preflight(&spec, &ctx(), &["ubuntu".to_owned()], false);
    assert!(
        duplicate.error_text().unwrap().contains("已经有一个"),
        "{duplicate:?}"
    );

    // 前后空白不算差异 —— 用户在名字框里多打一个空格不该被当成重名
    // ⚠️ 必须断言**具体那条错误**：只断言 `!ok()` 的话，
    // "名字非法"这类别的校验也能让它通过，测试就白写了。
    let spec = InstallSpec::new(
        "  Ubuntu  ",
        InstallSource::Tar {
            path: r"D:\a.tar".to_owned(),
        },
    );
    let duplicate = preflight(&spec, &ctx(), &["Ubuntu".to_owned()], false);
    assert!(
        duplicate.error_text().unwrap().contains("已经有一个"),
        "{duplicate:?}"
    );

    // 真的是不同的名字就照旧放行
    let spec = InstallSpec::new(
        "Ubuntux",
        InstallSource::Tar {
            path: r"D:\a.tar".to_owned(),
        },
    );
    let clean = preflight(&spec, &ctx(), &["Ubuntu".to_owned()], false);
    assert!(clean.ok(), "{clean:?}");
}

#[test]
fn preflight_warns_about_the_extra_copy_when_renaming() {
    let spec = InstallSpec::new(
        "MyUbuntu",
        InstallSource::Online {
            id: "Ubuntu-24.04".to_owned(),
            launch: false,
            web_download: false,
            fallback_id: None,
        },
    );
    let check = preflight(&spec, &ctx(), &[], false);
    assert!(check.ok(), "{check:?}");
    assert_eq!(check.warnings.len(), 1, "{check:?}");
    assert!(check.warnings[0].contains("多一次全量拷贝"), "{check:?}");
}

#[test]
fn display_quotes_arguments_that_contain_spaces() {
    let step = PlannedStep::new(
        "x",
        PlanProgram::Wsl,
        vec![
            "--import".to_owned(),
            "My Distro".to_owned(),
            r"D:\a b\x".to_owned(),
        ],
    );
    assert_eq!(step.line(), r#"wsl --import "My Distro" "D:\a b\x""#);
}
