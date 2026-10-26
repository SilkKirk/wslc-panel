//! `/etc/wsl.conf` 编辑器的字段 id、分节展开与保存。

use super::super::prelude::*;
use super::super::*;

#[test]
fn every_wslconf_field_has_its_own_id() {
    // 这条测试是 `wslconf_field_id` 那个 match 的守卫：往
    // `wslconf::FIELDS` 里加字段却忘了在那里加一行时，它会先炸 ——
    // 否则新字段会静默拿到 `"wslconf-unknown"`；两个字段都忘了加就
    // **撞同一个 id**，而 GPUI 里 id 撞了会让勾选 / 焦点状态互相串。
    //
    // 这也是当初把它从 `views.rs` 搬过来的主要收益：CI 不跑
    // `wslc-panel` 的单测，那种测试放在那边等于不存在。
    let mut seen: Vec<&'static str> = Vec::new();
    for field in wslc_core::model::wslconf::FIELDS {
        let id = wslconf_field_id(field.section, field.key);
        assert_ne!(
            id, "wslconf-unknown",
            "字段 `{}.{}` 没有自己的元素 id —— 去 `wslconf_field_id` 里补一行",
            field.section, field.key
        );
        assert!(
            !seen.contains(&id),
            "元素 id 撞了：`{id}`（`{}.{}`）",
            field.section,
            field.key
        );
        seen.push(id);
    }
    // 反方向：表里没有的东西不该拿到"看起来合法"的 id
    assert_eq!(wslconf_field_id("nope", "nope"), "wslconf-unknown");
}

fn wslconf_state(text: &str) -> WslConfState {
    WslConfState::new("Ubuntu", wslconf::WslConfDoc::parse(text), "2.6.1.0")
}

#[test]
fn wslconf_common_sections_start_open() {
    let state = wslconf_state("");
    assert!(state.section_open("network"));
    assert!(state.section_open("user"));
}

#[test]
fn wslconf_implicit_infrequent_sections_start_closed() {
    let state = wslconf_state("[automount]\nfutureKey = keep-me\n");
    for section in ["automount", "interop", "boot", "gpu", "time"] {
        assert!(!state.section_open(section), "{section}");
    }
}

#[test]
fn wslconf_explicit_fields_open_their_section_including_systemd() {
    for field in wslconf::FIELDS {
        let mut doc = wslconf::WslConfDoc::parse("");
        doc.set(field.section, field.key, field.default);
        let state = WslConfState::new("Ubuntu", doc, "2.6.1.0");
        assert!(
            state.section_open(field.section),
            "{}.{}",
            field.section,
            field.key
        );
    }
}

#[test]
fn wslconf_initial_expansion_respects_versions_and_unknown_version_semantics() {
    let doc = wslconf::WslConfDoc::parse(
        "[boot]\nsystemd = true\n[gpu]\nenabled = false\n[time]\nuseWindowsTimezone = false\n",
    );
    for version in ["0.60.0", "0.67.6", "2.6.1.0", "", "版本未知"] {
        let state = WslConfState::new("Ubuntu", doc.clone(), version);
        for section in ["boot", "gpu", "time"] {
            assert_eq!(
                state.section_open(section),
                wslconf::section_supported(section, version),
                "{section}: {version}"
            );
        }
    }
}

#[test]
fn wslconf_sections_can_be_closed_and_reopened() {
    let mut state = wslconf_state("[boot]\nsystemd = true\n");
    let doc = state.doc.clone();
    for section in wslconf::SECTIONS {
        state.set_section_open(section.name, false);
        assert!(!state.section_open(section.name));
        state.toggle_section(section.name);
        assert!(state.section_open(section.name));
        state.toggle_section(section.name);
        assert!(!state.section_open(section.name));
        state.set_section_open(section.name, true);
        assert!(state.section_open(section.name));
    }
    assert_eq!(state.doc, doc);
    assert!(!state.doc.is_dirty());
}

#[test]
fn wslconf_unknown_sections_do_not_change_expansion_state() {
    let mut state = wslconf_state("");
    let before = state.clone();
    state.set_section_open("unknown", true);
    state.toggle_section("unknown");
    state.set_section_open("Network", true);
    assert!(!state.section_open("unknown"));
    assert_eq!(state, before);
}

#[test]
fn wslconf_new_instances_have_independent_expansion_state() {
    let mut first = wslconf_state("");
    first.set_section_open("network", false);
    first.set_section_open("automount", true);
    let mut second = WslConfState::new("Debian", wslconf::WslConfDoc::parse(""), "2.6.1.0");
    assert!(second.section_open("network"));
    assert!(!second.section_open("automount"));
    second.set_section_open("user", false);
    assert!(first.section_open("user"));
    assert!(!first.section_open("network"));
    assert!(first.section_open("automount"));
}

#[test]
fn wslconf_section_summary_lists_only_explicit_supported_fields() {
    let state = wslconf_state(
        "[automount]\nroot = /data/\noptions = metadata\nfutureKey = keep-me\n\
             [boot]\nsystemd = true\n[network]\nfutureKey = keep-me\n",
    );
    assert_eq!(
        state.section_summary("automount"),
        "挂载根目录=/data/ · 挂载参数=metadata"
    );
    assert_eq!(state.section_summary("boot"), "启用 systemd=true");
    for section in ["network", "interop", "user", "gpu", "time", "unknown"] {
        assert_eq!(state.section_summary(section), "默认", "{section}");
    }
}

#[test]
fn wslconf_untouched_text_defaults_stay_implicit_and_clean() {
    for text in [
        "",
        "[network]\nhostname = my-box\n",
        "[network]\r\nhostname = my-box",
    ] {
        let mut state = wslconf_state(text);
        let before = state.doc.clone();
        for field in wslconf::FIELDS
            .iter()
            .filter(|field| field.kind == wslconf::FieldKind::Text)
        {
            let effective = state.doc.effective(field.section, field.key);
            state.apply_text_value(field.section, field.key, &format!("  {effective}  "));
        }
        assert_eq!(state.doc, before);
        assert_eq!(state.doc.render(), text);
        assert!(!state.doc.is_dirty());
        assert!(!state.doc.is_explicit("automount", "root"));
    }
}

#[test]
fn wslconf_text_changes_and_clears_follow_the_model() {
    let mut state = wslconf_state("[automount]\nroot = /data/\n[user]\ndefault = ubuntu\n");
    state.apply_text_value("automount", "root", "  /data/  ");
    assert!(!state.doc.is_dirty());
    state.apply_text_value("automount", "root", "  /win/  ");
    assert_eq!(state.doc.get("automount", "root"), Some("/win/"));
    assert!(state.doc.is_dirty());
    state.apply_text_value("automount", "root", " /mnt/ ");
    assert_eq!(state.doc.get("automount", "root"), Some("/mnt/"));
    state.apply_text_value("automount", "root", " \t ");
    state.apply_text_value("user", "default", "");
    assert!(!state.doc.is_explicit("automount", "root"));
    assert_eq!(state.doc.effective("automount", "root"), "/mnt/");
    assert!(!state.doc.is_explicit("user", "default"));
    assert_eq!(state.doc.render(), "[automount]\n[user]\n");
}

#[test]
fn wslconf_text_api_ignores_readonly_bool_and_unknown_fields() {
    let mut state = wslconf_state("[boot]\nsystemd = true\n[interop]\nenabled = false\n");
    let before = state.doc.clone();
    for field in wslconf::FIELDS
        .iter()
        .filter(|field| field.read_only || field.kind != wslconf::FieldKind::Text)
    {
        state.apply_text_value(field.section, field.key, "changed");
        state.apply_text_value(field.section, field.key, "");
    }
    for (section, key) in [
        ("unknown", "hostname"),
        ("network", "unknown"),
        ("Network", "hostname"),
    ] {
        state.apply_text_value(section, key, "changed");
    }
    assert_eq!(state.doc, before);
    assert!(!state.doc.is_dirty());
}

#[test]
fn wslconf_preview_and_save_preserve_comments_unknown_keys_and_systemd() {
    let text = "# 保留手写注释\n[boot]\nsystemd = true\nfutureKey = keep-me\n\n\
                    [automount]\nroot = /data/\n; 挂载说明\n[future]\ncustom = untouched\n";
    let mut state = wslconf_state(text);
    state.set_section_open("automount", false);
    let mut preview = state.clone();
    preview.apply_text_value("automount", "root", " /win/ ");
    assert_eq!(state.doc.render(), text);
    assert!(!state.doc.is_dirty());
    state.apply_text_value("automount", "root", " /win/ ");
    let rendered = state.doc.render();
    assert_eq!(rendered, preview.doc.render());
    assert_eq!(rendered, text.replace("root = /data/", "root = /win/"));
    assert_eq!(wslconf::WslConfDoc::parse(&rendered).render(), rendered);
}
