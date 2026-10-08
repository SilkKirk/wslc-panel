//! 集成测试：把**真实采集到的 `wslc` 输出**喂进公开 API，验证整条解析链路。
//!
//! 分两部分：
//!
//! 1. **fixture 测试** —— 用 `tests/fixtures/` 里的实机样本，任何机器上都能跑。
//! 2. **真机冒烟测试** —— 只有本机装了 `wslc` 才执行，否则打印一行说明后跳过。
//!    这类测试**不断言具体数据**（容器数、镜像数都会变），只断言"能解析、不 panic"。

use wslc_core::model::{
    ContainerInspect, ContainerListItem, ContainerState, ContainerStats, ContainerSummary,
    ImageListItem, NetworkListItem, Session, SystemInfo,
};
use wslc_core::settings::{SETTING_KEYS, SettingsDoc};
use wslc_core::{Wslc, jsonl};

// ---------------------------------------------------------------------------
// 1. fixture 测试
// ---------------------------------------------------------------------------

#[test]
fn container_list_running_fixture() {
    let items: Vec<ContainerListItem> =
        jsonl::parse_lines(include_str!("fixtures/container_list.jsonl")).unwrap();

    assert_eq!(items.len(), 1);
    let c = &items[0];
    assert_eq!(c.short_id(), "ff0667ee90fb");
    assert_eq!(c.display_name(), "wslc-panel-probe");
    assert_eq!(c.state_kind(), ContainerState::Running);
    assert!(c.is_running());
    // Command 字段含字面双引号，必须被清理。
    assert_eq!(c.command_clean(), "sleep 300");
    // 端口既能在 Ports 字符串里解析出来……
    assert_eq!(c.port_mappings().len(), 1);
    assert_eq!(c.port_mappings()[0].host_port, Some(18080));
    // ……也能从 Labels 里嵌的 WSL 元数据里解析出来（带 VmPort）。
    let meta = c.wsl_metadata_ports();
    assert_eq!(meta.len(), 1);
    assert_eq!(meta[0].vm_port, 20002);
    assert_eq!(meta[0].protocol_name(), "tcp");
}

#[test]
fn container_list_exited_fixture() {
    let items: Vec<ContainerListItem> =
        jsonl::parse_lines(include_str!("fixtures/container_list_stopped.jsonl")).unwrap();
    let c = &items[0];
    assert_eq!(c.state_kind(), ContainerState::Exited);
    assert!(!c.is_running());
    // 已退出容器的端口字段是空字符串。
    assert!(c.ports.is_empty());
    assert!(c.port_mappings().is_empty());
    assert!(c.status.starts_with("Exited (137)"));
}

#[test]
fn list_no_trunc_gives_64_char_id() {
    let items: Vec<ContainerListItem> =
        jsonl::parse_lines(include_str!("fixtures/container_list_notrunc.jsonl")).unwrap();
    assert_eq!(items[0].id.len(), 64);
    // 短 ID 仍然是前 12 位。
    assert_eq!(items[0].short_id(), "ff0667ee90fb");
}

#[test]
fn container_stats_fixtures() {
    let running: Vec<ContainerStats> =
        jsonl::parse_lines(include_str!("fixtures/container_stats.jsonl")).unwrap();
    assert_eq!(running[0].pids, 1); // PIDs 是数字，不是字符串
    assert_eq!(running[0].mem_usage_parts(), Some(("3.465MiB", "15.48GiB")));
    assert!(running[0].mem_ratio().unwrap() < 0.01);

    let all: Vec<ContainerStats> =
        jsonl::parse_lines(include_str!("fixtures/container_stats_all.jsonl")).unwrap();
    assert_eq!(all[0].pids, 0);
    // 已退出容器上限是 0B，不能算出比例（防除零）。
    assert_eq!(all[0].mem_ratio(), None);
}

#[test]
fn stats_and_list_ids_match_by_prefix() {
    let list: Vec<ContainerListItem> =
        jsonl::parse_lines(include_str!("fixtures/container_list.jsonl")).unwrap();
    let stats: Vec<ContainerStats> =
        jsonl::parse_lines(include_str!("fixtures/container_stats.jsonl")).unwrap();

    // list 给 12 位，stats 给 64 位 —— 必须按前缀匹配。
    assert_eq!(list[0].id.len(), 12);
    assert_eq!(stats[0].id.len(), 64);
    assert!(stats[0].matches_id(list[0].id()));

    let merged = ContainerSummary::merge(list, stats);
    assert_eq!(merged.len(), 1);
    assert!(merged[0].stats.is_some());
}

#[test]
fn inspect_fixture_full_and_size_variants() {
    let raw = include_str!("fixtures/container_inspect.json");
    let values: Vec<serde_json::Value> = serde_json::from_str(raw).unwrap();
    let insp = ContainerInspect::from_array(values).unwrap();

    // inspect 的 Name 带前导 `/`，访问器会去掉。
    assert_eq!(insp.name(), Some("wslc-panel-probe"));
    assert_eq!(insp.image_ref(), Some("docker.1ms.run/library/alpine:latest"));
    assert!(insp.is_running());
    assert_eq!(insp.cmd(), vec!["sleep", "300"]);
    assert_eq!(insp.networks()[0].1, "172.17.0.2");
    // 未启动过的容器 FinishedAt 是零值时间 → 报 None。
    assert_eq!(insp.finished_at(), None);
    assert!(insp.started_at().is_some());
    // 没加 -s 时没有体积字段。
    assert_eq!(insp.size_root_fs(), None);

    let sized = ContainerInspect::from_array(
        serde_json::from_str::<Vec<serde_json::Value>>(include_str!(
            "fixtures/container_inspect_s.json"
        ))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(sized.size_root_fs(), Some(8_422_040));
    assert_eq!(sized.size_rw(), Some(0));
}

#[test]
fn image_fixture_handles_duplicate_ids() {
    let items: Vec<ImageListItem> =
        jsonl::parse_lines(include_str!("fixtures/images.jsonl")).unwrap();
    assert_eq!(items.len(), 3);

    let hello: Vec<_> = items.iter().filter(|i| i.id == "e2ac70e7319a").collect();
    assert_eq!(hello.len(), 2, "同一镜像以两个仓库名出现");
    assert!(hello.iter().all(|i| !i.is_dangling()));

    // 浮点体积只做相对误差比较（1000 进制换算的最后一个 ulp 可能不同）。
    let bytes = items[0].size_bytes().expect("应能解析体积");
    assert!(
        (bytes - 8.42 * 1_000_000.0).abs() < 1.0,
        "实际 {bytes}"
    );
}

#[test]
fn network_fixture_includes_all_three_builtins() {
    let items: Vec<NetworkListItem> =
        jsonl::parse_lines(include_str!("fixtures/network_list.jsonl")).unwrap();
    let names: Vec<&str> = items.iter().map(|n| n.name.as_str()).collect();
    assert_eq!(names, vec!["bridge", "host", "none"]);
    assert!(items.iter().all(NetworkListItem::is_builtin));
    // 布尔语义的字段是字符串。
    assert!(items[0].ipv4_enabled());
    assert!(!items[0].ipv6_enabled());
    assert_eq!(items[2].driver, "null");
}

#[test]
fn info_fixture() {
    let info: SystemInfo = jsonl::parse_object(include_str!("fixtures/info.json")).unwrap();
    assert_eq!(info.version(), "3.0.1.0");
    assert!(info.has_session());
    assert_eq!(info.session_ids(), vec![1]);
    assert_eq!(info.server.sessions[0].name, "wslc-cli-76434");
    // Windows 路径里的反斜杠必须原样保留。
    assert!(info.settings_file().contains(r"\wslc\settings.yaml"));
}

#[test]
fn session_table_fixture() {
    let sessions: Vec<Session> =
        wslc_core::model::session::parse_session_table(include_str!("fixtures/session_list.txt"));
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, 1);
    assert_eq!(sessions[0].creator_pid, 21684);
    assert_eq!(sessions[0].display_name, "wslc-cli-76434");
}

#[test]
fn settings_fixture_round_trip_preserves_comments() {
    let mut doc = SettingsDoc::from_text("settings.yaml", include_str!("fixtures/settings.yaml"));

    // 出厂状态下每一项都是注释 = 使用内置默认值。
    assert!(doc.values().entries().iter().all(|(_, v)| v.is_none()));

    // 改写一项，必须保留它上方的英文说明和其它所有内容。
    assert!(doc.set(Some("session"), "cpuCount", Some("4")));
    assert!(doc.raw().contains("Number of virtual CPUs allocated to the session"));
    assert!(doc.raw().starts_with("# wslc user settings"));
    assert_eq!(doc.values().cpu_count.as_deref(), Some("4"));

    // 每一项都能独立设置与读回。
    for key in SETTING_KEYS {
        let sample = match key.kind {
            wslc_core::settings::SettingKind::Enum => key.choices[0],
            wslc_core::settings::SettingKind::PositiveInteger => "8",
            wslc_core::settings::SettingKind::Seconds => "60",
            wslc_core::settings::SettingKind::Size => "2GB",
            wslc_core::settings::SettingKind::Text => "sample",
        };
        assert!(doc.set(key.section, key.key, Some(sample)));
        assert_eq!(doc.get(key.section, key.key).as_deref(), Some(sample));
    }

    // 恢复默认。
    assert!(doc.set(Some("session"), "cpuCount", None));
    assert_eq!(doc.values().cpu_count, None);
}

#[test]
fn empty_outputs_never_error() {
    // 0 个容器 / 0 个卷时 wslc 输出 0 字节，必须变成空 Vec。
    assert!(jsonl::parse_lines::<ContainerListItem>("").unwrap().is_empty());
    assert!(jsonl::parse_lines::<ImageListItem>("\n\n").unwrap().is_empty());
    assert!(
        wslc_core::model::session::parse_session_table("")
            .is_empty()
    );
}

#[test]
fn malformed_lines_do_not_panic() {
    // 混入坏行时保留好行。
    let text = format!(
        "{}\n{{ this is not json\n",
        include_str!("fixtures/container_list.jsonl").trim()
    );
    let items: Vec<ContainerListItem> = jsonl::parse_lines(&text).unwrap();
    assert_eq!(items.len(), 1);

    // 全部是坏行时才报错。
    assert!(jsonl::parse_lines::<ContainerListItem>("bad\nworse\n").is_err());
}

#[test]
fn truncated_and_out_of_order_fields_still_parse() {
    // 字段缺失、类型不符都不应 panic。
    let items: Vec<ContainerListItem> =
        jsonl::parse_lines(r#"{"ID":"abc","UnexpectedField":{"nested":true}}"#).unwrap();
    assert_eq!(items[0].id, "abc");
    assert_eq!(items[0].names, "");
    assert!(!items[0].is_running());
}

// ---------------------------------------------------------------------------
// 2. 真机冒烟测试
// ---------------------------------------------------------------------------

fn wslc_if_available() -> Option<Wslc> {
    let wslc = Wslc::new();
    if wslc.is_available() {
        Some(wslc)
    } else {
        eprintln!(
            "跳过真机测试：没有找到可用的 wslc（路径 {:?}）。\
             需要 WSL 3.0 以上版本，或设置环境变量 WSLC_PATH。",
            wslc.program()
        );
        None
    }
}

#[test]
fn smoke_info_on_real_machine() {
    let Some(wslc) = wslc_if_available() else {
        return;
    };
    let info = wslc_core::cmd::system::info(&wslc).expect("wslc info 应成功");
    assert!(!info.client.version.is_empty(), "应拿到 WSL 版本号");
    // settings.yaml 路径必须存在（wslc 首次运行会创建它）。
    assert!(!info.settings_file().is_empty());
}

#[test]
fn smoke_sessions_on_real_machine() {
    let Some(wslc) = wslc_if_available() else {
        return;
    };
    // 注意：这条命令不支持 --format json，走表格解析。
    let sessions = wslc_core::cmd::system::sessions(&wslc).expect("会话列表应成功");
    for s in &sessions {
        assert!(s.id > 0);
    }
}

#[test]
fn smoke_lists_on_real_machine() {
    let Some(wslc) = wslc_if_available() else {
        return;
    };

    // 这些都不做数量断言 —— 机器上可能一个容器都没有。
    let running = wslc_core::cmd::container::list_running(&wslc).expect("list 应成功");
    let all = wslc_core::cmd::container::list(&wslc, true).expect("list -a 应成功");
    let images = wslc_core::cmd::image::list(&wslc).expect("images 应成功");
    let networks = wslc_core::cmd::network::list(&wslc).expect("network list 应成功");
    let volumes = wslc_core::cmd::volume::list(&wslc).expect("volume list 应成功");

    // 运行中的容器必然是全部容器的子集。
    assert!(running.len() <= all.len());
    // wslc 至少内置 bridge/host/none 三个网络。
    assert!(networks.iter().any(|n| n.name == "bridge"));
    eprintln!(
        "真机：运行中 {} / 全部 {} / 镜像 {} / 网络 {} / 卷 {}",
        running.len(),
        all.len(),
        images.len(),
        networks.len(),
        volumes.len()
    );
}

#[test]
fn smoke_missing_binary_reports_a_readable_error() {
    // 这条不依赖真机：路径不存在时必须给出可读错误，而不是 panic。
    let wslc = Wslc::with_program("definitely-not-a-real-binary-xyz");
    let err = wslc_core::cmd::system::info(&wslc).unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("找不到 wslc") || message.contains("执行 wslc 失败"),
        "错误信息应可读，实际是：{message}"
    );
}
