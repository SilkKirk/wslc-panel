//! `AppState` 的初值、刷新间隔、`ui_sections`、提示条、会话标签。

use super::super::prelude::*;
use super::super::*;
use crate::util::format_bytes;

#[test]
fn refresh_interval_comes_from_prefs() {
    let mut state = AppState::new(Wslc::new(), Wsl::new());
    state.prefs.refresh_secs = 10;
    assert_eq!(state.refresh_interval(), Duration::from_secs(10));

    state.prefs.refresh_secs = 1;
    assert_eq!(state.refresh_interval(), Duration::from_secs(1));
}

#[test]
fn default_refresh_interval_is_three_seconds() {
    // 默认必须是 3s —— 这是产品决定，界面不再暴露这个开关。
    assert_eq!(
        crate::prefs::Prefs::default().refresh_secs,
        3,
        "默认刷新间隔应为 3 秒"
    );
}

#[test]
fn format_bytes_is_1024_based_and_stable() {
    assert_eq!(format_bytes(0), "0 B");
    assert_eq!(format_bytes(512), "512 B");
    assert_eq!(format_bytes(1024), "1.00 KB");
    assert_eq!(format_bytes(19_666_042_880), "18.32 GB");
}

#[test]
fn default_settings_path_points_at_localappdata() {
    // 在设置了 LOCALAPPDATA 的 Windows 上必须能给出路径。
    if std::env::var_os("LOCALAPPDATA").is_some() {
        let path = default_settings_path().expect("应能推导出路径");
        assert!(path.to_string_lossy().contains("wslc"));
        assert!(path.to_string_lossy().ends_with("settings.yaml"));
    }
}

#[test]
fn new_state_starts_on_dashboard_and_is_idle() {
    let state = AppState::new(Wslc::new(), Wsl::new());
    assert_eq!(state.page, Page::Dashboard);
    assert!(!state.busy);
    assert!(state.confirm.is_none());
    assert!(state.toast.is_none());
    assert!(state.settings.is_none());
    // `prefs` 是从磁盘读的，这里只断言它落在合法范围内。
    assert!(state.prefs.refresh_secs >= crate::prefs::MIN_REFRESH_SECS);
    assert!(state.prefs.refresh_secs <= crate::prefs::MAX_REFRESH_SECS);
    assert_eq!(state.session_label(), "（无活动会话）");
}

#[test]
fn ui_sections_start_collapsed() {
    let state = AppState::new(Wslc::new(), Wsl::new());
    for section in [
        UiSection::CreateAdvanced,
        UiSection::CreateCommandPreview,
        UiSection::ConfigRaw,
        UiSection::WslHelp,
        UiSection::DeployProxy,
        UiSection::DeployProxyMore,
        UiSection::PullSource,
        UiSection::DbEncoding,
    ] {
        assert!(!state.section_open(section), "{section:?} 应默认折叠");
    }
}

#[test]
fn ui_sections_are_independent() {
    let mut state = AppState::new(Wslc::new(), Wsl::new());
    state.set_section_open(UiSection::DeployProxy, true);
    assert!(!state.section_open(UiSection::DeployProxyMore));
    assert!(!state.section_open(UiSection::CreateAdvanced));

    state.set_section_open(UiSection::DeployProxyMore, true);
    state.set_section_open(UiSection::DeployProxy, false);
    assert!(!state.section_open(UiSection::DeployProxy));
    assert!(state.section_open(UiSection::DeployProxyMore));
    assert!(!state.section_open(UiSection::CreateAdvanced));
}

#[test]
fn pull_source_and_db_encoding_sections_are_independent() {
    let mut state = AppState::new(Wslc::new(), Wsl::new());
    state.set_section_open(UiSection::CreateAdvanced, true);
    for (section, other) in [
        (UiSection::PullSource, UiSection::DbEncoding),
        (UiSection::DbEncoding, UiSection::PullSource),
    ] {
        state.set_section_open(section, true);
        assert!(state.section_open(section));
        assert!(!state.section_open(other));
        assert!(state.section_open(UiSection::CreateAdvanced));
        state.toggle_section(section);
        assert!(!state.section_open(section));
        assert!(!state.section_open(other));
        assert!(state.section_open(UiSection::CreateAdvanced));
    }
}

#[test]
fn ui_sections_can_close_and_reopen() {
    let mut state = AppState::new(Wslc::new(), Wsl::new());
    let section = UiSection::CreateAdvanced;
    state.set_section_open(section, false);
    assert!(!state.section_open(section));
    state.set_section_open(section, true);
    state.set_section_open(section, true);
    assert!(state.section_open(section));
    state.set_section_open(section, false);
    state.set_section_open(section, false);
    assert!(!state.section_open(section));
    state.set_section_open(section, true);
    assert!(state.section_open(section));
}

#[test]
fn ui_sections_toggle_without_affecting_other_sections() {
    let mut state = AppState::new(Wslc::new(), Wsl::new());
    state.set_section_open(UiSection::ConfigRaw, true);
    for section in [UiSection::CreateCommandPreview, UiSection::WslHelp] {
        state.toggle_section(section);
        assert!(state.section_open(section));
        assert!(state.section_open(UiSection::ConfigRaw));
        state.toggle_section(section);
        assert!(!state.section_open(section));
        assert!(state.section_open(UiSection::ConfigRaw));
    }
}

#[test]
fn confirm_can_be_requested_and_cancelled() {
    let mut state = AppState::new(Wslc::new(), Wsl::new());

    state.request_confirm(PendingAction::PruneContainers);
    assert_eq!(
        state.confirm,
        Some(ConfirmAction::Container(PendingAction::PruneContainers))
    );
    state.cancel_confirm();
    assert!(state.confirm.is_none());

    // 发行版域走同一个槽位 —— 两个域不能各有一个 confirm，
    // 否则同时触发时会出现两个叠在一起的确认框。
    state.request_distro_confirm(DistroAction::ShutdownAll);
    assert_eq!(
        state.confirm,
        Some(ConfirmAction::Distro(DistroAction::ShutdownAll))
    );
    state.cancel_confirm();
    assert!(state.confirm.is_none());
}

#[test]
fn toast_constructors_set_the_right_kind() {
    assert_eq!(Toast::success("a").kind, ToastKind::Success);
    assert_eq!(Toast::error("a").kind, ToastKind::Error);
    assert_eq!(Toast::info("a").kind, ToastKind::Info);
}

#[test]
fn session_label_uses_the_first_active_session() {
    use wslc_core::jsonl;
    use wslc_core::model::SystemInfo;
    let mut state = AppState::new(Wslc::new(), Wsl::new());
    state.snapshot.info = Some(
        jsonl::parse_object::<SystemInfo>(include_str!(
            "../../../../wslc-core/tests/fixtures/info.json"
        ))
        .unwrap(),
    );
    assert_eq!(state.session_label(), "#1 wslc-cli-76434");
}

#[test]
fn notify_keeps_a_single_slot_and_the_ui_takes_it_once() {
    let mut state = AppState::new(wslc_core::Wslc::new(), wslc_core::Wsl::new());

    // 没弹过：取不到东西（界面层每帧都会问一次）
    assert!(state.take_toast().is_none());

    // 后弹的盖掉先弹的 —— 单槽语义。界面层用固定 id 的 `remove_notification`
    // 把它复现出来（见 `Shell::render` 顶部那段交接）。
    state.notify(Toast::info("第一条"));
    state.notify(Toast::error("第二条"));
    let taken = state.take_toast().expect("应当有一条待展示的提示");
    assert_eq!(taken.text, "第二条");
    assert_eq!(taken.kind, crate::state::ToastKind::Error);

    // 取走之后槽就空了：否则渲染每帧都会重复 push 一次
    assert!(state.take_toast().is_none());
}
