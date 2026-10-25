//! `state` 的单测（116 条）。
//!
//! 原先嵌在 `state.rs` 末尾，占了那份 7492 行的 **2711 行**——
//! 想把「定义区」读完得先翻过全部测试。提到单独一个文件之后，
//! `state.rs` 的定义只剩 4780 行。
//!
//! 这些断言全是**纯逻辑**（页面分组、`--cd` 参数、镜像排序、备份文件名……），
//! 所以它们跟着 `core` job 跑，不必编译 GPUI（见那个 crate 的顶层说明）。

use super::*;

use super::*;

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

#[test]
fn every_page_has_a_label_and_group() {
    for page in Page::ALL {
        assert!(!page.label().is_empty());
        assert!(!page.group().is_empty());
    }
    // v0.3 加了「实例列表」「添加实例」「应用设置」「WSL 配置」，从 6 个变成 10 个。
    // v0.4 把「wlsc 配置」「WSL 配置」收进「应用设置」的 tab、并加了「关于」，
    // 所以是 10 - 2 + 1 = 9。**总数少了不代表功能少了** —— 那两个只是换了入口。
    // v0.4 接着把「添加实例」也从侧边栏拿掉了（改从实例列表页的按钮进），9 - 1 = 8。
    // v0.6 加了「镜像仓库」（在线浏览 registry 的 tag 再拉取），8 + 1 = 9。
    // v0.6.2 把它**并回「镜像」页**（远端浏览 + 拉取 + 本地已有，三件事一页），
    // 9 - 1 = 8 —— 同样是"总数少了不代表功能少了"。
    // v0.9.22 把「关于」从侧边栏收进「应用设置」的 tab，8 - 1 = 7。
    //
    // 这个断言存在的意义就是**逼人改它**：加页面时忘了同步导航分组，
    // 侧边栏会出现重复的组标题。历史上 commit 552a91c 就是被它抓到的；
    // 加 `Page::About` 那次它又抓了一次（CI 的 `纯逻辑测试` 那一关）。
    //
    // ⚠️ 注意 `cargo check --all-targets` 只编译不执行，
    // 所以它真的被跑到要靠 `cargo test -p wslc-panel-core`（见 SPIKE 7.8）。
    assert_eq!(Page::ALL.len(), 7);
}

#[test]
fn settings_tabs_are_all_labelled() {
    // 加 tab 时忘了同步 `settings_tabs` 的渲染顺序，界面上会出现
    // 一个点不到的 tab（按钮 id 用 `{:?}`，顺序由 ALL 决定）。
    assert_eq!(SettingsTab::ALL.len(), 4);
    for tab in SettingsTab::ALL {
        assert!(!tab.label().is_empty());
    }
    assert_eq!(
        SettingsTab::About.label(),
        "关于",
        "「关于」现在是设置里的 tab，不再是页面"
    );
}

#[test]
fn only_the_pages_with_inputs_need_a_window_to_enter() {
    // 有输入框的页面必须在点击时（有 window）把表单建好。
    // 哪天给别的页面加了输入框，这个测试会提醒你一起改。
    //
    // v0.9.17 起「容器」页也进了这一列：搜索框是 `InputState`，
    // 只能在进入页面那一刻建（`Shell::ensure_container_search`）。
    //
    // ⚠️ `Page::AddInstance` **不在 `Page::ALL` 里**（侧边栏不列它），
    // 所以不能只遍历 `ALL` 就算完 —— 那样这条断言对它就是**空的**，
    // 哪天真把它改坏了也抓不到。显式再查一遍。
    for page in Page::ALL {
        let expected = page == Page::Containers;
        assert_eq!(
            page.needs_window_to_enter(),
            expected,
            "{page:?} 的 `needs_window_to_enter` 和实际不符 —— \
                 改了页面上的输入框就要同步改 `app.rs::set_page` 和这条断言"
        );
    }
    assert!(Page::AddInstance.needs_window_to_enter());
    // 「数据库管理」也不在 ALL 里（只能从容器行进），但它有连接卡输入框。
    assert!(Page::Database.needs_window_to_enter());
}

#[test]
fn sub_pages_highlight_their_parent_in_the_sidebar() {
    assert_eq!(Page::AddInstance.nav_parent(), Some(Page::Instances));
    assert_eq!(Page::Database.nav_parent(), Some(Page::Containers));
    assert_eq!(Page::Deploy.nav_parent(), Some(Page::Containers));
    assert_eq!(Page::Dashboard.nav_parent(), None);

    assert!(Page::AddInstance.highlights_nav(Page::Instances));
    assert!(Page::Database.highlights_nav(Page::Containers));
    assert!(Page::Deploy.highlights_nav(Page::Containers));
    // 子页面不会把别的导航项点亮
    assert!(!Page::Database.highlights_nav(Page::Images));
    // 普通页面只高亮自己，不会把别的页也点亮
    assert!(Page::Containers.highlights_nav(Page::Containers));
    assert!(!Page::Containers.highlights_nav(Page::Images));
}

#[test]
fn registry_state_never_shows_a_stale_repository() {
    let mut state = RegistryState::new();
    assert_eq!(state.host, registry::DEFAULT_REGISTRY);

    state.begin_query(registry::DEFAULT_REGISTRY, "alpine", "library/alpine", None);
    assert!(state.loading, "提交查询就该进入 loading");
    assert_eq!(
        state.normalization_note().as_deref(),
        Some("由 alpine 解析为 library/alpine"),
        "原文与规范化结果不同时要说清"
    );
    state.set_tags(
        "library/alpine",
        vec!["latest".to_owned(), "3.24.2".to_owned()],
    );
    assert!(!state.loading);
    assert!(state.error.is_none());
    assert_eq!(state.loaded.as_deref(), Some("library/alpine"));
    assert_eq!(state.tags.len(), 2);

    // 原文就是规范名时不该多嘴
    state.begin_query(
        registry::DEFAULT_REGISTRY,
        "library/alpine",
        "library/alpine",
        None,
    );
    assert_eq!(state.normalization_note(), None);

    // ⚠️ 换仓库：旧结果必须**立刻**消失。留着的话，网络那 1~2 秒里
    // 界面会把 nginx 的 tag 挂在 redis 的标题下 —— 用户最容易误判的时刻。
    state.set_tags("library/nginx", vec!["1.31.6".to_owned()]);
    state.select_tag("1.31.6");
    assert_eq!(state.selected.as_deref(), Some("1.31.6"));
    state.begin_query(registry::DEFAULT_REGISTRY, "redis", "library/redis", None);
    assert!(state.tags.is_empty(), "换仓库时旧 tag 不能留着");
    assert!(state.loaded.is_none(), "换仓库时不能说列表是谁的");
    assert!(state.selected.is_none(), "换仓库时选中的 tag 也不算数了");

    // 失败时同样不能留着旧结果（报错旁边挂着一列 tag，会被读成"这次查到的"）
    state.set_tags("library/redis", vec!["7.4".to_owned()]);
    state.set_error("这个 registry 上没有仓库 library/nope".to_owned());
    assert!(state.tags.is_empty());
    assert!(state.loaded.is_none());
    assert!(!state.loading);
    assert!(state.error.is_some());
}

#[test]
fn registry_state_matches_inputs_after_host_normalization() {
    let mut state = RegistryState::new();
    state.begin_query(
        "docker.m.daocloud.io",
        "  nginx:1.27  ",
        "library/nginx",
        Some("1.27".to_owned()),
    );
    let before = state.clone();
    for host in [
        "docker.m.daocloud.io",
        "  docker.m.daocloud.io  ",
        "https://docker.m.daocloud.io/",
        "http://Docker.M.Daocloud.IO/",
    ] {
        assert!(state.matches_inputs(host, "  nginx:1.27  "), "{host}");
        assert!(state.matches_inputs(host, "nginx:1.27"), "{host}");
    }
    assert_eq!(state, before, "匹配检查不能改动查询状态");
}

#[test]
fn registry_state_rejects_changed_hosts_repositories_and_tags() {
    let mut state = RegistryState::new();
    state.begin_query(
        "docker.m.daocloud.io",
        "nginx:1.27",
        "library/nginx",
        Some("1.27".to_owned()),
    );
    assert!(!state.matches_inputs(registry::DEFAULT_REGISTRY, "nginx:1.27"));
    assert!(!state.matches_inputs("docker.m.daocloud.io", "redis:1.27"));
    assert!(!state.matches_inputs("docker.m.daocloud.io", "nginx:1.29"));
    assert!(!state.matches_inputs("docker.m.daocloud.io", "nginx"));
}

#[test]
fn registry_state_matches_an_empty_host_as_the_default_registry() {
    let mut state = RegistryState::new();
    state.begin_query(registry::DEFAULT_REGISTRY, "alpine", "library/alpine", None);
    for host in ["", "  ", registry::DEFAULT_REGISTRY] {
        assert!(state.matches_inputs(host, "alpine"), "{host:?}");
    }
}

#[test]
fn registry_state_builds_the_pull_reference_with_the_registry_prefix() {
    let mut state = RegistryState::new();
    // 还没查过 → 没有可拉的
    assert_eq!(state.selected_reference(), None);

    state.begin_query(registry::DEFAULT_REGISTRY, "alpine", "library/alpine", None);
    state.set_tags("library/alpine", vec!["latest".to_owned()]);
    // 查到了但没选 tag → 还是不能拉
    assert_eq!(state.selected_reference(), None);

    state.select_tag("latest");
    assert!(state.sizing, "选中 tag 就该开始量体积");
    // ⚠️ 必须带 registry 前缀：不带的话 `wslc pull` 会去 Docker Hub 找，
    // 而它在这台机器上不可达（实测 000）。
    assert_eq!(
        state.selected_reference().as_deref(),
        Some("docker.1panel.live/library/alpine:latest")
    );

    state.set_size(3_850_349, 1);
    assert!(!state.sizing);
    assert_eq!(state.size_label().as_deref(), Some("下载约 3.9 MB（1 层）"));

    // 换 tag → 体积要重量，不能留着上一个 tag 的数字
    state.select_tag("3.24.2");
    assert!(state.size.is_none());
    assert!(state.size_label().is_none());
    // 量不出来时给原因，而不是留一个空的体积
    state.set_size_error("这个镜像没有 amd64/linux 那一份".to_owned());
    assert!(!state.sizing);
    assert!(state.size_error.is_some());
    // 但那个 tag 仍然是选中的（用户还能拉它）
    assert_eq!(
        state.selected_reference().as_deref(),
        Some("docker.1panel.live/library/alpine:3.24.2")
    );
}

#[test]
fn registry_state_falls_back_to_the_tag_typed_in_the_input() {
    // 别的 registry 的 tags/list 可能要认证（实测 daocloud 401）——
    // 那时一个 tag 都列不出来，但用户已经在输入里写了 `:1.27`，
    // 那就必须能拉。这是合并两页（去掉「按引用拉取」弹窗）之后
    // "按完整引用拉取"唯一的落点。
    let mut state = RegistryState::new();
    state.begin_query(
        "docker.m.daocloud.io",
        "nginx:1.27",
        "library/nginx",
        Some("1.27".to_owned()),
    );
    assert_eq!(state.host, "docker.m.daocloud.io", "registry 是可编辑的");
    assert_eq!(state.effective_tag(), Some("1.27"));
    assert_eq!(
        state.selected_reference().as_deref(),
        Some("docker.m.daocloud.io/library/nginx:1.27"),
        "列表拉不到（甚至还没回来）也要能拼出引用"
    );

    // 查询失败也不影响它 —— 这正是它存在的理由
    state.set_error("401 Unauthorized".to_owned());
    assert!(state.tags.is_empty());
    assert_eq!(
        state.selected_reference().as_deref(),
        Some("docker.m.daocloud.io/library/nginx:1.27")
    );

    // 下拉框里选了一个就以选的为准（用户后来看清楚了要哪个）
    state.select_tag("1.29");
    assert_eq!(state.effective_tag(), Some("1.29"));
    assert_eq!(
        state.selected_reference().as_deref(),
        Some("docker.m.daocloud.io/library/nginx:1.29")
    );

    // 换仓库要把它清掉，否则下一次查询会莫名其妙带上上一次的 tag
    state.begin_query("docker.1panel.live", "redis", "library/redis", None);
    assert_eq!(state.effective_tag(), None);
    assert_eq!(state.selected_reference(), None);
}

#[test]
fn install_sources_are_listed_and_described() {
    for kind in InstallSourceKind::ALL {
        assert!(!kind.label().is_empty(), "{kind:?}");
        assert!(!kind.hint().is_empty(), "{kind:?}");
    }
    assert!(InstallSourceKind::ALL.contains(&InstallSourceKind::default()));
    // 默认必须是最不依赖网络的那条
    assert_eq!(InstallSourceKind::default(), InstallSourceKind::Local);
}

#[test]
fn install_source_capabilities_are_consistent() {
    // 只有在线安装有「装完启动」，也只有它不需要文件路径
    assert!(InstallSourceKind::Online.supports_launch());
    assert!(!InstallSourceKind::Online.needs_path());

    // 本地那一条要选文件，而且要有标签/占位/过滤器
    // （它把原来的 tar / VHDX / `.wsl` 三条合一了 —— 后缀由
    // `classify_local_file` 分岔，见 `wslc_core::model::install`）
    let kind = InstallSourceKind::Local;
    assert!(!kind.supports_launch(), "{kind:?}");
    assert!(kind.needs_path(), "{kind:?}");
    assert!(kind.is_local(), "{kind:?}");
    assert!(!kind.path_label().is_empty(), "{kind:?}");
    assert!(!kind.path_placeholder().is_empty(), "{kind:?}");
    assert!(kind.requires_install_dir(), "{kind:?}");
    let (label, extensions) = kind.file_filter().expect("本地来源要能弹选择器");
    assert!(!label.is_empty(), "{kind:?}");
    // 后缀表必须是**并集**：tar 系列 + vhdx + .wsl，少一个用户就选不到
    for expected in ["tar", "gz", "xz", "vhdx", "vhd", "wsl"] {
        assert!(
            extensions.contains(&expected),
            "少了 {expected}：{extensions:?}"
        );
    }

    // 两条"网络来源"不要文件路径，也不要文件选择器
    for kind in [InstallSourceKind::Mirror, InstallSourceKind::Online] {
        assert!(!kind.needs_path(), "{kind:?}");
        assert!(kind.file_filter().is_none(), "{kind:?}");
        assert!(!kind.is_local(), "{kind:?}");
        assert!(kind.is_online() == (kind == InstallSourceKind::Online));
    }
    // 镜像站要安装目录（下载完要 --import），在线安装可以留空
    assert!(InstallSourceKind::Mirror.is_mirror());
    assert!(InstallSourceKind::Mirror.requires_install_dir());
    assert!(!InstallSourceKind::Online.requires_install_dir());

    // 版本**不给用户选**：本项目只支持 WSL 2，安装命令里固定 `--version 2`
    // （`InstallSourceKind` 上再也没有 `supports_version` 这种东西了）。

    // 三种来源的标签不能重复（按钮上会分不清）
    let labels: Vec<&str> = InstallSourceKind::ALL.iter().map(|k| k.label()).collect();
    let unique: std::collections::HashSet<&&str> = labels.iter().collect();
    assert_eq!(unique.len(), labels.len(), "{labels:?}");
    assert_eq!(labels.len(), 3, "本地那三条已经合一：{labels:?}");

    // 「微软商店」必须**一眼看得出来**是商店 —— 用户最想找的就是它，
    // 之前它叫"在线安装"，从名字上完全看不出跟商店有关系。
    assert!(
        InstallSourceKind::Online.label().contains("微软商店"),
        "{}",
        InstallSourceKind::Online.label()
    );
    // 商店排在镜像源前面：商店是 WSL 官方那条路，镜像源是它的备选
    let order = InstallSourceKind::ALL.to_vec();
    let store = order.iter().position(|k| *k == InstallSourceKind::Online);
    let mirror = order.iter().position(|k| *k == InstallSourceKind::Mirror);
    assert!(store < mirror, "{order:?}");
}

#[test]
fn install_progress_follows_the_events() {
    use wslc_core::cmd::install::InstallEvent;

    let mut progress = InstallProgress::new("MyUbuntu", vec!["Ubuntu-24.04".to_owned()]);
    assert!(progress.is_running());
    assert_eq!(progress.percent(), None);

    // 第一步开始
    assert!(progress.apply(&InstallEvent::Step {
        index: 1,
        total: 3,
        label: "创建安装目录".to_owned(),
        line: "创建安装目录 D:\\wsl\\MyUbuntu".to_owned(),
        cancellable: true,
    }));
    assert_eq!(progress.index, 1);
    assert_eq!(progress.total, 3);
    assert!(progress.cancellable);
    assert!(progress.log.iter().any(|l| l.contains("第 1/3 步")));
    assert!(progress.log.iter().any(|l| l.contains("创建安装目录 D:")));

    // 进度事件：数字没变就不算变化（界面据此不重绘）
    assert!(progress.apply(&InstallEvent::Progress {
        have: 100,
        total: Some(400),
        secs: 5,
    }));
    assert!(!progress.apply(&InstallEvent::Progress {
        have: 100,
        total: Some(400),
        secs: 5,
    }));
    assert_eq!(progress.percent(), Some(25.0));
    assert!(progress.speed().is_some());

    // 换步骤要把上一步的进度归零（那是上一步的产物大小）
    progress.apply(&InstallEvent::Step {
        index: 2,
        total: 3,
        label: "导入".to_owned(),
        line: String::new(),
        // 重定位那一步不可取消 —— 界面据此不给"取消"按钮
        cancellable: false,
    });
    assert!(!progress.cancellable);
    assert_eq!(progress.have, 0);
    assert_eq!(progress.percent(), None);

    // 一行的输出同时进日志和 last_line
    progress.apply(&InstallEvent::Line("正在导入...".to_owned()));
    assert_eq!(progress.last_line, "正在导入...");

    // 步骤结束 → 记一笔
    progress.apply(&InstallEvent::StepDone {
        index: 2,
        ok: false,
        detail: "退出码 -1".to_owned(),
    });
    assert_eq!(progress.finished_count(), 1);
    assert!(!progress.finished[0].ok);
    assert!(progress.log.iter().any(|l| l.contains("✘")));

    // 结束之后不算"还在跑"
    assert!(progress.finish(InstallOutcome::Failed {
        detail: "x".to_owned(),
        step: Some("导入".to_owned()),
    }));
    assert!(!progress.is_running());
    // 同一个结局重复设置 → 不算变化
    assert!(!progress.finish(InstallOutcome::Failed {
        detail: "x".to_owned(),
        step: Some("导入".to_owned()),
    }));
}

#[test]
fn busy_with_also_knows_the_name_the_install_registers_first() {
    // 改名安装的中间态：`wsl -l -v` 上是**源名**（`Ubuntu-24.04`），
    // 而用户要的是 `MyUbuntu`。只认最终名字的话，那段时间里列表上那一行
    // 的「终止 / 删除 / 打开终端」全都可点 —— 而重定位正是
    // 「导出 → 注销 → 导入」那段不可逆的操作（见 `busy_with` 的说明）。
    let mut state = AppState::new(wslc_core::Wslc::new(), wslc_core::Wsl::new());

    // 还没开始装：谁都不拦
    assert_eq!(state.busy_with("Ubuntu-24.04"), None);

    state.installing = Some(InstallProgress::new(
        "MyUbuntu",
        vec!["Ubuntu-24.04".to_owned()],
    ));
    assert_eq!(state.busy_with("MyUbuntu"), Some("安装"));
    assert_eq!(state.busy_with("Ubuntu-24.04"), Some("安装"), "源名也要认");
    // 大小写不敏感（WSL 自己就不区分）
    assert_eq!(state.busy_with("ubuntu-24.04"), Some("安装"));
    // 别的发行版照旧放行 —— 安装要几十分钟，不能把整页锁住
    assert_eq!(state.busy_with("Debian"), None);

    // 装完之后不再拦
    if let Some(progress) = state.installing.as_mut() {
        progress.finish(InstallOutcome::Success("好了".to_owned()));
    }
    assert_eq!(state.busy_with("Ubuntu-24.04"), None);
    assert_eq!(state.busy_with("MyUbuntu"), None);
}

#[test]
fn install_progress_log_is_capped() {
    use wslc_core::cmd::install::InstallEvent;

    let mut progress = InstallProgress::new("X", Vec::new());
    for i in 0..(InstallProgress::MAX_LOG_LINES + 50) {
        progress.apply(&InstallEvent::Line(format!("第 {i} 行")));
    }
    assert_eq!(progress.log.len(), InstallProgress::MAX_LOG_LINES);
    // 留下的是**最后**那些行（用户要看的是最新的）
    assert!(
        progress
            .log
            .last()
            .unwrap()
            .contains(&format!("第 {} 行", InstallProgress::MAX_LOG_LINES + 49))
    );
}

#[test]
fn online_state_filters_and_selects() {
    let mut online = OnlineDistroState::new();
    assert!(online.filtered("").is_empty());
    assert_eq!(online.source, OnlineListSource::Unknown);
    assert!(online.source.label().is_empty());

    online.items = vec![
        OnlineDistro::new("Ubuntu", "Ubuntu"),
        OnlineDistro::new("Ubuntu-24.04", "Ubuntu 24.04 LTS"),
        OnlineDistro::new("Debian", "Debian GNU/Linux"),
    ];
    online.source = OnlineListSource::FallbackJson;
    assert_eq!(online.filtered("").len(), 3);
    assert_eq!(online.filtered("ubuntu").len(), 2);
    assert_eq!(online.filtered("24").len(), 1);

    online.select("Ubuntu-24.04");
    assert_eq!(online.selected.as_deref(), Some("Ubuntu-24.04"));

    // 拉取失败时要把旧清单清掉，否则用户会挑一个已经不成立的列表
    online.clear();
    assert!(online.items.is_empty());
    assert!(online.selected.is_none());
}

#[test]
fn online_ids_match_the_mirror_catalog() {
    fn offer(name: &str, version: &str) -> Offer {
        Offer {
            name: name.to_owned(),
            version: version.to_owned(),
            sources: vec![wslc_core::mirrors::OfferSource {
                mirror: "lxc-tuna".to_owned(),
                url: format!("https://mirrors.tuna.tsinghua.edu.cn/x/{name}-{version}.tar.xz"),
                format: "tar.xz".to_owned(),
            }],
        }
    }

    let mut state = MirrorState::new();
    // 名字与版本都照真实清单（wslui 那份）写
    state.set_offers(
        vec![
            offer("Ubuntu", "24.04"),
            offer("Ubuntu", "22.04"),
            offer("Debian", "13"),
            offer("Kali", "current"),
            offer("Arch Linux", "current"),
            offer("Fedora", "44"),
            offer("AlmaLinux", "9"),
            offer("openSUSE", "tumbleweed"),
        ],
        String::new(),
        None,
    );

    // ① 名字 + 版本拼起来对上
    assert_eq!(
        state.match_online_id("Ubuntu-24.04").as_deref(),
        Some("Ubuntu 24.04")
    );
    assert_eq!(
        state.match_online_id("ubuntu 22.04").as_deref(),
        Some("Ubuntu 22.04")
    );
    assert_eq!(
        state.match_online_id("AlmaLinux-9").as_deref(),
        Some("AlmaLinux 9")
    );
    assert_eq!(
        state.match_online_id("openSUSE-Tumbleweed").as_deref(),
        Some("openSUSE tumbleweed")
    );
    // ② 只对上名字（在线 id 不带版本）
    assert_eq!(
        state.match_online_id("Ubuntu").as_deref(),
        Some("Ubuntu 24.04")
    );
    assert_eq!(
        state.match_online_id("Debian").as_deref(),
        Some("Debian 13")
    );
    // ③ 名字前缀（两边叫法有出入）
    assert_eq!(
        state.match_online_id("kali-linux").as_deref(),
        Some("Kali current")
    );
    assert_eq!(
        state.match_online_id("archlinux").as_deref(),
        Some("Arch Linux current")
    );
    assert_eq!(
        state.match_online_id("FedoraLinux-44").as_deref(),
        Some("Fedora 44")
    );
    // 认不出来 / 空串 → 不硬猜
    assert_eq!(state.match_online_id("SUSE-Linux-Enterprise-15-SP7"), None);
    assert_eq!(state.match_online_id(""), None);
    assert_eq!(state.match_online_id("   "), None);
}

#[test]
fn a_bare_family_name_falls_back_to_its_newest_versioned_sibling() {
    // 就是本机的真实清单（顺序也照 `wsl -l -o`：裸名字在最前）
    let mut online = OnlineDistroState::new();
    online.items = vec![
        OnlineDistro::new("Ubuntu", "Ubuntu"),
        OnlineDistro::new("Ubuntu-26.04", "Ubuntu 26.04 LTS"),
        OnlineDistro::new("Ubuntu-24.04", "Ubuntu 24.04 LTS"),
        OnlineDistro::new("Ubuntu-22.04", "Ubuntu 22.04 LTS"),
        OnlineDistro::new("Debian", "Debian GNU/Linux"),
        OnlineDistro::new("kali-linux", "Kali Linux Rolling"),
        OnlineDistro::new("AlmaLinux-10", "AlmaLinux OS 10"),
        OnlineDistro::new("AlmaLinux-9", "AlmaLinux OS 9"),
    ];

    // 裸 `Ubuntu` → 最新的那个版本号（不是列表里的下一个）
    let none: Vec<String> = Vec::new();
    assert_eq!(
        online.sibling_id("Ubuntu", &none).as_deref(),
        Some("Ubuntu-26.04")
    );
    // 带版本的 id 自己不再往下找
    assert_eq!(online.sibling_id("Ubuntu-24.04", &none), None);
    assert_eq!(online.sibling_id("AlmaLinux-10", &none), None);
    // 没有兄弟的不硬猜
    assert_eq!(online.sibling_id("Debian", &none), None);
    assert_eq!(online.sibling_id("kali-linux", &none), None);
    assert_eq!(online.sibling_id("", &none), None);
    // 版本比较按数值而不是按字典序（22 < 24 < 26）
    assert_ne!(
        online.sibling_id("Ubuntu", &none).as_deref(),
        Some("Ubuntu-22.04")
    );

    // ⚠️ 已经装过的兄弟不能选：本机就是这个状态（`Ubuntu-26.04` 装着），
    // 换过去只会撞 `Wsl/InstallDistro/ERROR_ALREADY_EXISTS`（实测）。
    let taken = vec!["Ubuntu-26.04".to_owned()];
    assert_eq!(
        online.sibling_id("Ubuntu", &taken).as_deref(),
        Some("Ubuntu-24.04"),
        "最新的那个装过了，就退到下一个版本"
    );
    // 大小写不同也是同一个发行版
    let taken = vec!["ubuntu-26.04".to_owned()];
    assert_eq!(
        online.sibling_id("Ubuntu", &taken).as_deref(),
        Some("Ubuntu-24.04")
    );
    // 同族的全装过了 → 干脆不换（返回 None，让界面照实说这条路装不动）
    let taken = vec![
        "Ubuntu-26.04".to_owned(),
        "Ubuntu-24.04".to_owned(),
        "Ubuntu-22.04".to_owned(),
    ];
    assert_eq!(online.sibling_id("Ubuntu", &taken), None);
    // 装了别的发行版不影响
    let taken = vec!["Debian".to_owned()];
    assert_eq!(
        online.sibling_id("Ubuntu", &taken).as_deref(),
        Some("Ubuntu-26.04")
    );
}

#[test]
fn probe_results_are_sorted_fastest_first_with_failures_last() {
    // 探测是**按清单顺序**发起的（见 `Shell::probe_mirrors`），所以结果必须
    // 排一下 —— 否则界面上"哪个镜像快"要靠用户自己在十几行里找。
    fn result(site: &str, code: u16, secs: f64) -> MirrorProbeResult {
        MirrorProbeResult {
            site: site.to_owned(),
            url: format!("https://x/{site}"),
            code,
            secs,
            bytes: if code == 200 { Some(10) } else { None },
        }
    }

    let mut state = MirrorState::new();
    state.results = vec![
        result("慢", 200, 2.0),
        result("连不上", 0, 8.0),
        result("快", 200, 0.3),
        result("404", 404, 0.1),
        // ⚠️ 探测没跑成的候选：`code == 0` 且 `secs == 0.0`
        // （`probe_mirrors` 里那个 `unwrap_or(0.0)`）
        result("没探", 0, 0.0),
    ];
    state.sort_results_by_speed();

    let sites: Vec<&str> = state.results.iter().map(|r| r.site.as_str()).collect();
    assert_eq!(
        sites,
        vec!["快", "慢", "没探", "404", "连不上"],
        "可用的按耗时升序排前面，失败的排后面；\
             只按 `secs` 排的话「没探」（0.0）会跑到最前面，看着像最快的那个"
    );
}

#[test]
fn the_catalog_is_sorted_like_the_reference() {
    // 用户要求"在线发行版的清单照 wsl-dashboard 来"：Ubuntu 最前、Debian 次之，
    // 同族按版本**倒序**（新的在前），滚动版（没有版本号）排最后。
    fn offer(name: &str, version: &str) -> Offer {
        Offer {
            name: name.to_owned(),
            version: version.to_owned(),
            sources: vec![wslc_core::mirrors::OfferSource {
                mirror: "lxc-tuna".to_owned(),
                url: format!("https://x/{name}-{version}.tar.xz"),
                format: "tar.xz".to_owned(),
            }],
        }
    }

    let mut state = MirrorState::new();
    state.set_offers(
        vec![
            offer("openSUSE", "tumbleweed"),
            offer("Rocky Linux", "10"),
            offer("Ubuntu", "20.04"),
            offer("Debian", "12"),
            offer("openSUSE", "16.0"),
            offer("Ubuntu", "24.04"),
            offer("Debian", "13"),
            offer("Ubuntu", "22.04"),
            offer("Alpine", "3.22"),
            offer("openEuler", "24.03"),
        ],
        "测试清单".to_owned(),
        None,
    );

    let ids: Vec<String> = state.offers.iter().map(Offer::id).collect();
    assert_eq!(
        ids,
        vec![
            // 1 常用度：Ubuntu 最前，同族版本倒序
            "Ubuntu 24.04",
            "Ubuntu 22.04",
            "Ubuntu 20.04",
            // 2 Debian
            "Debian 13",
            "Debian 12",
            // 6 openSUSE：16.0 在滚动版前面（数值键 > 空键）
            "openSUSE 16.0",
            "openSUSE tumbleweed",
            // 9 / 11 / 12
            "Rocky Linux 10",
            "Alpine 3.22",
            "openEuler 24.03",
        ]
    );
    // 排完之后默认选中第一条 —— 参考实现里用户打开就是 Ubuntu 24.04
    assert_eq!(state.distro_id, "Ubuntu 24.04");
}

#[test]
fn mirror_state_forgets_previous_probes_when_switching_or_reloading() {
    fn offer(name: &str, version: &str) -> Offer {
        Offer {
            name: name.to_owned(),
            version: version.to_owned(),
            sources: vec![wslc_core::mirrors::OfferSource {
                mirror: "lxc-tuna".to_owned(),
                url: format!("https://mirrors.tuna.tsinghua.edu.cn/x/{name}-{version}.tar.xz"),
                format: "tar.xz".to_owned(),
            }],
        }
    }
    fn probed() -> MirrorProbeResult {
        MirrorProbeResult {
            site: "lxc-tuna".to_owned(),
            url: "https://mirrors.tuna.tsinghua.edu.cn/x/a.tar.xz".to_owned(),
            code: 200,
            secs: 0.5,
            bytes: Some(100),
        }
    }

    let mut state = MirrorState::new();
    // 还没拉清单 → 没有可选的
    assert!(state.selected_offer().is_none());
    assert!(state.distro_id.is_empty());

    state.set_offers(
        vec![offer("Ubuntu", "24.04"), offer("Alpine", "3.22")],
        "测试清单".to_owned(),
        Some("2026-10-09T00:00:00Z".to_owned()),
    );
    // 拉回来之后默认选第一条
    assert_eq!(state.distro_id, "Ubuntu 24.04");
    assert_eq!(state.origin, "测试清单");
    assert_eq!(state.updated.as_deref(), Some("2026-10-09T00:00:00Z"));

    state.results.push(probed());
    state.chosen = Some(MirrorChoice {
        site: "lxc-tuna".to_owned(),
        url: "https://mirrors.tuna.tsinghua.edu.cn/x/a.tar.xz".to_owned(),
        release: "24.04".to_owned(),
        format: "tar.xz".to_owned(),
        bytes: Some(100),
    });

    state.select_distro("Alpine 3.22");
    assert_eq!(state.distro_id, "Alpine 3.22");
    // 换了发行版就不该留着上一条的探测结果（那会拿着 A 的 URL 去装 B）
    assert!(state.results.is_empty());
    assert!(state.chosen.is_none());
    assert!(state.error.is_none());

    // 选同一个不算换
    state.results.push(probed());
    state.select_distro("Alpine 3.22");
    assert_eq!(state.results.len(), 1);

    // 重新拉清单：原来选的那条还在 → 保留选择与探测结果
    state.set_offers(
        vec![offer("Ubuntu", "24.04"), offer("Alpine", "3.22")],
        "新清单".to_owned(),
        None,
    );
    assert_eq!(state.distro_id, "Alpine 3.22");
    assert_eq!(state.results.len(), 1);

    // 原来选的那条**没了**（版本更新）→ 回落到第一条并清掉探测结果
    state.set_offers(vec![offer("Ubuntu", "26.04")], "新清单".to_owned(), None);
    assert_eq!(state.distro_id, "Ubuntu 26.04");
    assert!(state.results.is_empty() && state.chosen.is_none());

    // id 与清单对不上（理论上不该发生）时也要能回落到第一条，而不是给 None
    state.distro_id = "不存在的 1".to_owned();
    assert_eq!(
        state.selected_offer().map(Offer::id).as_deref(),
        Some("Ubuntu 26.04")
    );
}

#[test]
fn mirror_probe_summary_says_something_useful_for_every_outcome() {
    let ok = MirrorProbeResult {
        site: "清华 TUNA".to_owned(),
        url: "https://x".to_owned(),
        code: 200,
        secs: 1.234,
        bytes: Some(229_623_728),
    };
    assert!(ok.is_ok());
    let text = ok.summary();
    assert!(text.contains("229.6 MB"), "{text}");
    assert!(text.contains("1.23"), "{text}");

    // 连不上（curl 给 000）
    let dead = MirrorProbeResult {
        code: 0,
        secs: 8.0,
        ..ok.clone()
    };
    assert!(!dead.is_ok());
    assert!(dead.summary().contains("连不上"), "{}", dead.summary());

    // 404：文件可能改名了，这是最常见的一种失败
    let missing = MirrorProbeResult {
        code: 404,
        secs: 0.1,
        bytes: None,
        ..ok.clone()
    };
    assert!(missing.summary().contains("404"), "{}", missing.summary());

    // 200 但拿不到大小时也要能显示
    let unknown = MirrorProbeResult {
        bytes: None,
        ..ok.clone()
    };
    assert!(
        unknown.summary().contains("大小未知"),
        "{}",
        unknown.summary()
    );
}

#[test]
fn plan_context_reads_prefs_and_the_sparse_flag() {
    let mut state = AppState::new(wslc_core::Wslc::new(), wslc_core::Wsl::new());
    state.prefs.install_dir = Some(r"D:\wsl".to_owned());

    // 没读过 .wslconfig → 不开稀疏
    let ctx = state.plan_context();
    assert_eq!(ctx.default_dir.as_deref(), Some(r"D:\wsl"));
    assert!(!ctx.wslconfig_sparse);
    assert!(!ctx.temp_dir.is_empty());
    assert!(!ctx.stamp.is_empty());

    // 采集里读到的真实配置要求稀疏 → 跟着它走
    state.snapshot.wslconfig.text = Some("[experimental]\nsparseVhd=true\n".to_owned());
    assert!(state.plan_context().wslconfig_sparse);
}

#[test]
fn app_state_preflight_uses_the_current_distro_list() {
    use wslc_core::model::install::{InstallSource, InstallSpec};

    let mut state = AppState::new(wslc_core::Wslc::new(), wslc_core::Wsl::new());
    // 有默认安装目录 → tar 导入的必填项能由"默认目录 + 名字"推出来
    state.prefs.install_dir = Some(r"D:\wsl".to_owned());
    let spec = InstallSpec::new(
        "Ubuntu",
        InstallSource::Tar {
            path: r"D:\a.tar".to_owned(),
        },
    );

    // 列表是空的时候它能过（只差目录非空与否）
    assert!(state.preflight(&spec, false).ok());
    assert!(!state.preflight(&spec, true).ok());

    // 列表里已经有 Ubuntu → 报重名
    state.snapshot.distros.push(wslc_core::model::Distro::new(
        "Ubuntu",
        wslc_core::model::DistroState::Stopped,
        Some(2),
        false,
    ));
    assert_eq!(state.distro_names(), vec!["Ubuntu".to_owned()]);
    let check = state.preflight(&spec, false);
    assert!(!check.ok());
    assert!(check.error_text().unwrap().contains("已经有一个"));
}

#[test]
fn pages_are_grouped_in_navigation_order() {
    // 侧边栏依赖"同组的页面在 ALL 里连续"来插入分组标题。
    let groups: Vec<&str> = Page::ALL.iter().map(|p| p.group()).collect();
    let mut deduped: Vec<&str> = Vec::new();
    for g in groups {
        if deduped.last() != Some(&g) {
            deduped.push(g);
        }
    }
    // 去重后不能再出现重复的组名（否则同组被拆成两段）。
    let mut seen = std::collections::HashSet::new();
    for g in &deduped {
        assert!(seen.insert(*g), "分组 {g} 在导航里被拆成了多段");
    }
    assert_eq!(deduped, vec!["概览", "WSL 实例", "容器", "资源", "设置"]);
}

#[test]
fn snapshot_default_is_empty_and_has_no_data() {
    let snap = Snapshot::default();
    assert!(!snap.has_data());
    assert!(snap.errors.is_empty());
    // 空快照的圆环：全是 0 / None —— 界面上那一排会画成"没有数据"，
    // **不是** 0% 满环。
    let stats = snap.dashboard_stats();
    assert_eq!(stats.running, 0);
    assert_eq!(stats.total, 0);
    assert_eq!(stats.cpu_percent, 0.0);
    assert_eq!(stats.cpu_cores, None);
    assert_eq!(stats.mem_limit, None);
    assert_eq!(stats.cpu_whole_machine_percent(), None);
    assert_eq!(stats.mem_percent(), None);
    assert!(stats.disk.is_none());
}

#[test]
fn image_totals_dedupe_refs_pointing_at_the_same_id() {
    // 真机夹具：`e2ac70e7319a` 同时是
    // `docker.1panel.live/library/hello-world` 和 `hello-world` 两行。
    const IMAGES: &str = include_str!("../../wslc-core/tests/fixtures/images.jsonl");
    let images: Vec<ImageListItem> = wslc_core::jsonl::parse_lines(IMAGES).expect("夹具应当能解析");
    assert_eq!(images.len(), 3, "夹具本身是 3 行（2 个镜像）");

    let snap = Snapshot {
        images,
        ..Default::default()
    };
    let (distinct, bytes) = snap.image_totals();
    assert_eq!(distinct, 2, "同一个 ID 的多条引用只算一个镜像");
    // 8.42MB + 10.1kB（1000 进制，见 `parse_size`）。浮点转 u64 允许 1 字节误差。
    assert!(
        (bytes as i64 - 8_430_100).abs() <= 1,
        "合计体积应当是 8.42MB + 10.1kB，实际 {bytes}"
    );
}

#[test]
fn image_totals_of_empty_snapshot_is_zero() {
    assert_eq!(Snapshot::default().image_totals(), (0, 0));
}

/// 一个只有 CPU / 内存指标有意义的样本（字段形状照 `wslc stats` 真机输出）。
fn summary_with_stats(cpu: &str, mem: &str) -> ContainerSummary {
    ContainerSummary {
        item: wslc_core::model::ContainerListItem::default(),
        stats: Some(wslc_core::model::ContainerStats {
            cpu_perc: cpu.to_owned(),
            mem_usage: mem.to_owned(),
            ..Default::default()
        }),
    }
}

#[test]
fn dashboard_stats_sums_the_real_machine_numbers() {
    // 真机那一组（`wslc stats` 实测）：redis 0.13% / mysql 0.56%，
    // 内存 27.45MiB + 514.1MiB，上限都是同一个虚拟机的 15.48GiB。
    let mut snap = Snapshot {
        running: vec![
            summary_with_stats("0.13%", "27.45MiB / 15.48GiB"),
            summary_with_stats("0.56%", "514.1MiB / 15.48GiB"),
        ],
        // 全部容器比运行中的多两个（已停止的）
        all: vec![
            summary_with_stats("0.13%", "27.45MiB / 15.48GiB"),
            summary_with_stats("0.56%", "514.1MiB / 15.48GiB"),
            summary_with_stats("0.00%", "3.9MiB / 15.48GiB"),
            summary_with_stats("0.00%", "35.36MiB / 15.48GiB"),
        ],
        wsl_cpu_cores: Some(32),
        ..Default::default()
    };
    // 会话盘：1000 字节里用了 600（VHD 占 120）
    let mut storage = wslc_core::storage::layout(None, PathBuf::from(r"C:\x"), None);
    storage.volume = Some(VolumeSpace {
        total: 1000,
        free: 400,
    });
    storage.vhd_bytes = Some(120);
    snap.storage = Some(storage);

    let stats = snap.dashboard_stats();
    assert_eq!(stats.running, 2);
    assert_eq!(stats.total, 4);
    assert!(
        (stats.cpu_percent - 0.69).abs() < 1e-9,
        "{}",
        stats.cpu_percent
    );
    // 内存是**字节**：MiB 是 1024 进制
    let mib = 1024.0 * 1024.0;
    assert_eq!(stats.mem_used, (27.45 * mib) as u64 + (514.1 * mib) as u64);
    // 上限取容器报的那个（同一个虚拟机，各容器一致）
    assert_eq!(stats.mem_limit, Some((15.48 * 1024.0 * mib) as u64));

    // ⚠️ 这条是这一整块存在的理由：CPU 合计 **不是**整机占用率。
    // `wslc stats` 用的是 Docker 那条公式，100% ≈ 1 个核 ——
    // 0.69% ÷ 32 核 ≈ 0.022%，而不是 0.69%。
    let whole = stats
        .cpu_whole_machine_percent()
        .expect("给了核数就该算得出来");
    assert!((whole - 0.69 / 32.0).abs() < 1e-9, "{whole}");
    // 环下面那行小字（`0.0069 / 32 核`）
    assert!((stats.cpu_cores_used() - 0.0069).abs() < 1e-9);

    // 内存那一环：用了多少 / 上限
    let mem_percent = stats.mem_percent().expect("有上限就该算得出来");
    assert!((mem_percent - (541.55 / (15.48 * 1024.0) * 100.0)).abs() < 1e-6);

    // 存储那一环：**会话 VHD** ÷ 所在盘总量 = 120 / 1000 = 12%
    //
    // ⚠️ 刻意**不是**"盘已用 / 盘总量"（600/1000 = 60%）：那是这台电脑的
    // 磁盘占用率，和面板管的这一摊没关系 —— 实机反馈就是这个
    // （环里写着 50%，而容器数据只有 2.64 GB）。
    let disk = stats.disk.expect("给了 storage 就该有盘");
    assert_eq!(disk.total, 1000);
    assert_eq!(stats.vhd_bytes, Some(120));
    let vhd_percent = stats
        .vhd_percent_of_disk()
        .expect("有 VHD、有盘总量就算得出来");
    assert!((vhd_percent - 12.0).abs() < 1e-9, "{vhd_percent}");
}

#[test]
// ⚠️ `field_reassign_with_default`：这个测试要造**四种**不同的
// `storage` 组合（无 VHD / 无卷 / 正常 / 卷容量 0），而 `Snapshot` 有
// 11 个字段。用 `Snapshot { storage: …, ..Default::default() }` 的话
// 每处都得重复一遍结构体名，反而把"这个测试只关心 storage"埋掉了。
// 所以这里保留 `let mut snap = Snapshot::default(); snap.storage = …`。
#[allow(clippy::field_reassign_with_default)]
fn dashboard_stats_storage_ring_needs_both_the_vhd_and_the_disk() {
    // 缺哪一半都不出百分比：这是**一个比例**，只有分子或只有分母都是编数字。
    // VHD 还没建出来（新装的会话）
    let mut snap = Snapshot::default();
    snap.storage = Some(wslc_core::storage::layout(
        None,
        PathBuf::from(r"C:\x"),
        None,
    ));
    assert_eq!(snap.dashboard_stats().vhd_percent_of_disk(), None);

    // 有 VHD，但读不到卷容量
    let mut snap = Snapshot::default();
    let mut storage = wslc_core::storage::layout(None, PathBuf::from(r"C:\x"), None);
    storage.vhd_bytes = Some(120);
    snap.storage = Some(storage);
    assert_eq!(snap.dashboard_stats().vhd_percent_of_disk(), None);

    // 两个都有 → 算得出来
    let mut storage = wslc_core::storage::layout(None, PathBuf::from(r"C:\x"), None);
    storage.vhd_bytes = Some(120);
    storage.volume = Some(VolumeSpace {
        total: 1000,
        free: 400,
    });
    let mut snap = Snapshot::default();
    snap.storage = Some(storage);
    assert_eq!(snap.dashboard_stats().vhd_percent_of_disk(), Some(12.0));

    // 卷容量是 0（不该发生）→ 不当分母，别去算除零
    let mut storage = wslc_core::storage::layout(None, PathBuf::from(r"C:\x"), None);
    storage.vhd_bytes = Some(120);
    storage.volume = Some(VolumeSpace { total: 0, free: 0 });
    let mut snap = Snapshot::default();
    snap.storage = Some(storage);
    assert_eq!(snap.dashboard_stats().vhd_percent_of_disk(), None);
}

#[test]
fn dashboard_stats_without_cores_reports_no_percentage() {
    // 拿不到核数（`.wslconfig` 没写、环境变量也没读到）时**不能**把
    // 原始合计当百分比：多核机器上 400% 是四个核跑满，画成 400% 的环
    // 只会误导。这时候宁可只给原始合计。
    let snap = Snapshot {
        running: vec![summary_with_stats("400.00%", "1MiB / 2MiB")],
        wsl_cpu_cores: None,
        ..Default::default()
    };
    let stats = snap.dashboard_stats();
    assert_eq!(stats.cpu_percent, 400.0);
    assert_eq!(stats.cpu_whole_machine_percent(), None);
    // 核数写 0（不该发生）也当拿不到，别去算除零
    let zero = Snapshot {
        running: vec![summary_with_stats("10.00%", "")],
        wsl_cpu_cores: Some(0),
        ..Default::default()
    };
    assert_eq!(zero.dashboard_stats().cpu_whole_machine_percent(), None);
}

#[test]
fn dashboard_stats_skips_containers_without_stats_and_nan() {
    // "这一轮没读到统计"和"占用 0"是两件事：前者**不计入**合计
    // （把它当 0 会让合计数看起来比真实占用小 —— 那是编数字）。
    let mut snap = Snapshot {
        running: vec![
            summary_with_stats("1.00%", "1MiB / 4MiB"),
            // 没有 stats：只有列表项
            ContainerSummary {
                item: wslc_core::model::ContainerListItem::default(),
                stats: None,
            },
            // `parse_percent` 对 `nan%` 会给回 NaN —— 不能让它污染合计
            summary_with_stats("nan%", "1MiB / 4MiB"),
        ],
        wsl_cpu_cores: Some(4),
        ..Default::default()
    };
    snap.all = snap.running.clone();

    let stats = snap.dashboard_stats();
    assert_eq!(
        stats.running, 3,
        "运行中数的是**列表**，不是有 stats 的那几个"
    );
    assert_eq!(stats.cpu_percent, 1.0, "NaN 与缺失都不该进合计");
    assert_eq!(stats.mem_used, (1.0 * 1024.0 * 1024.0) as u64 * 2);
}

#[test]
fn dashboard_stats_without_a_memory_limit_has_no_percentage() {
    // 上限是 `0B`（`wslc` 在容器刚起来时会这么报）→ 当没有，别去除零
    let snap = Snapshot {
        running: vec![summary_with_stats("1.00%", "0B / 0B")],
        wsl_cpu_cores: Some(4),
        ..Default::default()
    };
    let stats = snap.dashboard_stats();
    assert_eq!(stats.mem_limit, None);
    assert_eq!(stats.mem_percent(), None);
}

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
fn pull_progress_keeps_only_the_last_lines() {
    let mut p = PullProgress::new("alpine:latest");
    assert_eq!(p.image, "alpine:latest");
    assert!(p.last_line().is_none());

    assert!(p.push_lines((0..10).map(|i| format!("line {i}"))));
    assert_eq!(p.lines.len(), 10);
    assert_eq!(p.last_line(), Some("line 9"));

    // 超过上限后丢掉最老的，保留最新的
    assert!(p.push_lines((0..PullProgress::MAX_LINES + 20).map(|i| format!("extra {i}"))));
    assert_eq!(p.lines.len(), PullProgress::MAX_LINES);
    assert_eq!(p.last_line(), Some("extra 219"));

    // 空输入不算新增（调用方据此跳过重绘）
    assert!(!p.push_lines(Vec::new()));
    assert_eq!(p.lines.len(), PullProgress::MAX_LINES);
}

#[test]
fn danger_actions_all_have_copy() {
    let actions = [
        PendingAction::StopContainer("a".into()),
        PendingAction::StopAllContainers { running: 3 },
        PendingAction::KillContainer("a".into()),
        PendingAction::RemoveContainer("a".into()),
        PendingAction::PruneContainers,
        PendingAction::RemoveImage("img".into()),
        PendingAction::RemoveNetwork("net".into()),
        PendingAction::RemoveVolume("vol".into()),
        PendingAction::PruneVolumes,
    ];
    for action in actions {
        assert!(!action.title().is_empty());
        assert!(!action.body().is_empty());
        assert!(!action.confirm_label().is_empty());
    }
}

#[test]
fn stop_all_body_names_the_blast_radius() {
    // 确认框一开就盖住了工具栏上那行"N 个在运行"，所以**影响面必须写进正文**
    // —— 这是"停一个"和"停全部"在文案上唯一的区别，也是这个字段存在的理由。
    let action = PendingAction::StopAllContainers { running: 4 };
    assert_eq!(action.title(), "停止所有容器");
    assert_eq!(action.confirm_label(), "全部停止");

    let body = action.body();
    assert!(body.contains('4'), "{body}");
    assert!(body.contains("正在运行的容器"), "{body}");
    // 停止 = 丢未保存数据，这句不能省（单容器那条也有同样的说明）
    assert!(body.contains("未保存的数据"), "{body}");
    // 还得说清"已经停着的不受影响"，否则用户会以为已退出的那些也要被动
    assert!(body.contains("不受影响"), "{body}");
}

#[test]
fn stop_all_summary_lists_names_and_truncates() {
    assert_eq!(
        stopped_all_message(&["web".to_owned()]),
        "已停止 1 个容器：web"
    );

    // 5 个是上限：正好 5 个不加"等"，第 6 个开始截断
    let five: Vec<String> = (0..5).map(|i| format!("c{i}")).collect();
    assert_eq!(
        stopped_all_message(&five),
        "已停止 5 个容器：c0、c1、c2、c3、c4"
    );
    let six: Vec<String> = (0..6).map(|i| format!("c{i}")).collect();
    let msg = stopped_all_message(&six);
    assert!(msg.starts_with("已停止 6 个容器："), "{msg}");
    assert!(!msg.contains("c5"), "第 6 个名字不该出现在摘要里：{msg}");
    assert!(msg.ends_with('等'), "{msg}");

    // 一行都没打印时**不能**说"已停止 0 个容器"——那读起来像"成功了，但什么也没发生"
    let none: Vec<String> = Vec::new();
    let empty = stopped_all_message(&none);
    assert!(!empty.contains('0'), "{empty}");
    assert!(empty.contains("没有报告"), "{empty}");
}

#[test]
fn prune_volumes_says_it_only_touches_unused_ones() {
    // 「清理」这个词本身很含糊，用户最怕的是"把我正在用的卷删了"。
    let body = PendingAction::PruneVolumes.body();
    assert!(body.contains("没有被任何容器使用"), "{body}");
    assert!(body.contains("不可撤销"), "{body}");
}

#[test]
fn every_distro_action_has_copy_too() {
    let actions = [
        DistroAction::Terminate("Ubuntu".into()),
        DistroAction::ShutdownAll,
        DistroAction::Unregister {
            name: "Ubuntu".into(),
            vhdx_bytes: Some(19_666_042_880),
        },
        DistroAction::Compact("Ubuntu".into()),
        DistroAction::SetSparse {
            name: "Ubuntu".into(),
            sparse: true,
        },
        DistroAction::SetSparse {
            name: "Ubuntu".into(),
            sparse: false,
        },
    ];
    for action in actions {
        assert!(!action.title().is_empty(), "{action:?}");
        assert!(!action.body().is_empty(), "{action:?}");
        assert!(!action.confirm_label().is_empty(), "{action:?}");
    }
}

#[test]
fn sparse_toggle_says_which_way_it_goes() {
    // 两个方向的文案必须不一样，否则用户分不清点了会开还是关
    let on = DistroAction::SetSparse {
        name: "U".into(),
        sparse: true,
    };
    let off = DistroAction::SetSparse {
        name: "U".into(),
        sparse: false,
    };
    assert_eq!(on.title(), "开启稀疏磁盘");
    assert_eq!(off.title(), "关闭稀疏磁盘");
    assert_eq!(on.confirm_label(), "开启");
    assert_eq!(off.confirm_label(), "关闭");
    assert_ne!(on.body(), off.body());
}

#[test]
fn every_prompt_kind_has_copy() {
    for kind in [
        PromptKind::MoveDistro,
        PromptKind::ResizeDistro,
        PromptKind::SetDefaultUser,
        PromptKind::ExportDistro,
        PromptKind::CreateVolume,
    ] {
        assert!(!kind.title().is_empty(), "{kind:?}");
        assert!(!kind.label().is_empty(), "{kind:?}");
        assert!(!kind.placeholder().is_empty(), "{kind:?}");
        assert!(!kind.note().is_empty(), "{kind:?}");
        assert!(!kind.confirm_label().is_empty(), "{kind:?}");
    }
}

#[test]
fn only_the_two_path_prompts_offer_a_picker() {
    // 「浏览…」只对"要填路径"的那两种有意义。给「创建卷」加一个
    // 会让用户以为该去选一个文件。
    assert!(PromptKind::MoveDistro.pick_target().is_some());
    assert!(PromptKind::ExportDistro.pick_target().is_some());
    assert!(PromptKind::ResizeDistro.pick_target().is_none());
    assert!(PromptKind::SetDefaultUser.pick_target().is_none());
    assert!(PromptKind::CreateVolume.pick_target().is_none());
}

// -- 卷名与 exec 命令的纯逻辑 ------------------------------------------

#[test]
fn volume_name_refuses_anything_that_looks_like_a_flag() {
    // 这条是本地挡下来的**唯一**一类：卷名会变成裸 argv，
    // `--help` 之类会被 wslc 当选项而不是名字。
    assert!(validate_volume_name("webdata").is_ok());
    assert!(validate_volume_name("  webdata  ").is_ok());
    // 名字**中间**有横线是正常的，不该被挡
    assert!(validate_volume_name("web-data_1").is_ok());

    assert!(validate_volume_name("--help").is_err());
    assert!(validate_volume_name("-x").is_err());
    assert!(validate_volume_name("").is_err());
    assert!(validate_volume_name("   ").is_err());
    // 前导空格不能让 `-` 蒙混过去（trim 之后再判）
    assert!(validate_volume_name("  --force").is_err());
}

#[test]
fn exec_argv_goes_through_a_shell() {
    // 锁住这个形状：**不要**改成按空格切分。管道 / 重定向 / `&&`
    // 是用户预期能用的，切开之后它们会静默失效（命令看起来还成功了）。
    assert_eq!(
        exec_argv("ls -la /tmp | head -5"),
        vec!["/bin/sh", "-c", "ls -la /tmp | head -5"]
    );
    // `/bin/sh` 而不是 `/bin/bash`：alpine 这类镜像里没有 bash
    assert_eq!(exec_argv("true")[0], "/bin/sh");
    // 原文一个字都不改地传下去（包括引号）
    assert_eq!(exec_argv("echo 'a b'")[2], "echo 'a b'");
}

// -- 「创建容器」的启动命令那一格 ---------------------------------------

#[test]
fn create_command_leaves_the_image_default_alone_when_empty() {
    // 空 → **一条命令都不给**，镜像自己的 ENTRYPOINT / CMD 才有机会生效。
    // 这条是 mysql / nginx 那类镜像的命门：返回 `["/bin/sh","-c",""]` 同样
    // 算"用户给了命令"，会把 CMD 顶掉 —— 表现是"本来能跑，填了个空反而起不来"。
    assert!(create_command("").is_empty());
    // 只敲空格 / 制表符也算没敲（不 trim 就是 `sh -c " "`，起来就退）
    assert!(create_command("   ").is_empty());
    assert!(create_command("\t\n ").is_empty());
}

#[test]
fn create_command_goes_through_a_shell_and_trims() {
    assert_eq!(
        create_command("  tail -f /dev/null  "),
        vec!["/bin/sh", "-c", "tail -f /dev/null"]
    );
    // `/bin/sh` 而不是 `/bin/bash`：alpine 这类镜像里没有 bash
    assert_eq!(create_command("java -jar /app/a.jar")[0], "/bin/sh");
    // 管道 / 重定向不能静默失效（理由同 `exec_argv`）
    assert_eq!(
        create_command("nginx -g 'daemon off;' | tee /tmp/log")[2],
        "nginx -g 'daemon off;' | tee /tmp/log"
    );
}

// -- 日志浮层的状态 ----------------------------------------------------

#[test]
fn logs_state_maps_its_controls_to_log_options() {
    let mut logs = LogsState::new("web");
    // 默认给一个有限的档位 —— "全部"在大容器上可能是几百 MB
    assert_eq!(logs.tail, Some(200));
    assert_eq!(logs.options().tail, Some(200));
    assert!(!logs.options().timestamps);
    assert!(!logs.options().details, "详细默认不开");

    logs.tail = None;
    logs.timestamps = true;
    let opts = logs.options();
    assert_eq!(opts.tail, None, "None 是「全部」");
    assert!(opts.timestamps);
}

#[test]
fn log_tail_labels_cover_every_preset() {
    assert_eq!(LogsState::tail_label(Some(200)), "200 行");
    assert_eq!(LogsState::tail_label(None), "全部");
    // 档位表里那个"全部"必须真的是 None，不然界面上点它没反应
    assert_eq!(LogsState::TAILS[3], None);
}

#[test]
fn a_stale_log_response_is_dropped() {
    // 连点「1000 行」→「全部」会有两个请求在飞，先发的可能后回。
    // 只认最后一次发起的那个 —— 否则界面会显示上一次选择的结果。
    let mut logs = LogsState::new("web");

    let first = logs.begin_load();
    assert!(logs.loading);
    let second = logs.begin_load();
    assert_ne!(first, second, "每轮票据必须不同");

    // 旧的那个后到了：必须被丢掉
    assert!(!logs.finish_load(first, Ok("旧结果".into())));
    assert!(logs.loading, "还在等新的那个，不能因为旧响应到了就收工");
    assert_eq!(logs.text, None, "旧结果不能覆盖");

    // 新的那个到了：收下
    assert!(logs.finish_load(second, Ok("新结果".into())));
    assert!(!logs.loading);
    assert_eq!(logs.text.as_deref(), Some("新结果"));
}

#[test]
fn a_failed_log_load_keeps_the_previous_text() {
    // 拉失败时**不该**把上一次的内容擦掉：用户宁可看着旧日志，
    // 也不想面对一片空白（而且错误信息会单独显示）。
    let mut logs = LogsState::new("web");
    let t = logs.begin_load();
    assert!(logs.finish_load(t, Ok("上一次的内容".into())));

    let t2 = logs.begin_load();
    assert!(logs.finish_load(t2, Err("连不上".into())));
    assert_eq!(logs.text.as_deref(), Some("上一次的内容"));
    assert_eq!(logs.error.as_deref(), Some("连不上"));
    assert!(!logs.loading);
}

#[test]
fn begin_load_clears_the_previous_error() {
    // 重试时上一条错误必须先消失，否则界面会一直挂着它
    let mut logs = LogsState::new("web");
    let t = logs.begin_load();
    logs.finish_load(t, Err("第一次失败".into()));
    assert!(logs.error.is_some());

    logs.begin_load();
    assert_eq!(logs.error, None);
}

#[test]
fn confirm_action_delegates_to_the_right_domain() {
    // 包装层只是转发，不能把两边的文案搞混
    let container = ConfirmAction::Container(PendingAction::RemoveVolume("v".into()));
    assert_eq!(container.title(), "删除卷");
    assert_eq!(container.confirm_label(), "删除卷");

    let distro = ConfirmAction::Distro(DistroAction::Terminate("Ubuntu".into()));
    assert_eq!(distro.title(), "终止发行版");
    assert_eq!(distro.confirm_label(), "终止");
    assert!(distro.body().contains("Ubuntu"));
}

#[test]
fn unregister_body_shows_how_much_is_about_to_be_deleted() {
    // 用户在点"删除"之前必须知道自己要删掉多少东西
    let with_size = DistroAction::Unregister {
        name: "Ubuntu".into(),
        vhdx_bytes: Some(19_666_042_880),
    };
    let body = with_size.body();
    assert!(body.contains("18.32 GB"), "{body}");
    assert!(body.contains("不可撤销"), "{body}");

    // 读不到大小时也要能正常出文案（只是没有那句括号）
    let without_size = DistroAction::Unregister {
        name: "Ubuntu".into(),
        vhdx_bytes: None,
    };
    let body = without_size.body();
    assert!(!body.contains("虚拟磁盘约"), "{body}");
    assert!(body.contains("Ubuntu"), "{body}");
}

#[test]
fn prune_warns_about_being_irreversible() {
    assert!(PendingAction::PruneContainers.body().contains("不可撤销"));
    assert!(
        PendingAction::RemoveVolume("v".into())
            .body()
            .contains("不可撤销")
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
        jsonl::parse_object::<SystemInfo>(include_str!("../../wslc-core/tests/fixtures/info.json"))
            .unwrap(),
    );
    assert_eq!(state.session_label(), "#1 wslc-cli-76434");
}

// -- 数据库管理页（v0.7.0）---------------------------------------------

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
fn deploy_page_is_not_in_the_sidebar_and_does_not_need_a_window() {
    // 「部署管理」和「数据库管理」都服务**某一个**容器，只能从容器列表行上的
    // 按钮进，所以**不在** `Page::ALL` 里。
    assert!(!Page::ALL.contains(&Page::Deploy));
    // 和 `Containers` 同组，侧边栏里不会多出一个组标题
    assert_eq!(Page::Deploy.group(), "容器");
    assert_eq!(Page::Deploy.label(), "部署管理");
    // ⚠️ **不需要 window** —— 这一页一个输入框都没有。
    // 「添加部署」那五个框是点按钮那一刻才建的（`open_deploy_dialog` 有
    // window），页面本身进来就能画。这和 v0.6.3 的「拉取镜像」是同一个形状。
    //
    // 写成 `true` 不是"更保险"：`set_page` 里会给它一个**永远走不到**的分支。
    assert!(
        !Page::Deploy.needs_window_to_enter(),
        "部署页没有常驻输入框 —— 表单在弹窗里，是点的时候建的"
    );
}

#[test]
fn a_freshly_opened_deploy_page_can_be_loaded() {
    // ⚠️ 和 `a_freshly_opened_db_page_can_be_loaded` 是同一条教训：
    // `provisional` 里把 `busy` 置 true 会让 `load_deploy` 的第一句守卫
    // 把**第一次**加载挡掉，页面永远停在「正在读取…」。
    // v0.8 在数据库页上真的踩过一次，这里把它钉死。
    let state = DeployState::provisional(
        "nginx",
        "docker.1panel.live/library/nginx:latest",
        AppRuntime::Nginx,
        DeploySignal::Image,
    );
    assert!(
        !state.busy,
        "新建的状态不能是 busy：`load_deploy` 会因为这一条直接返回"
    );
    assert!(!state.loaded);
    // 列表此时是空的（存档还没读进来），但页面已经能画了
    assert!(state.entries.is_empty());
    assert!(state.progress.is_none());
}

/// 造一行"已部署列表"（测试用）。
fn deploy_entry(app: &str, runtime_key: &str) -> DeployEntry {
    DeployEntry {
        record: crate::deploys::DeployRecord {
            app: app.to_owned(),
            runtime: runtime_key.to_owned(),
            source: format!(r"E:\build\{app}"),
            target: format!("/opt/{app}"),
            command: format!("./{app}"),
            artifact: app.to_owned(),
            port: 8080,
            members: vec![app.to_owned()],
            site: None,
        },
        runtime: AppRuntime::from_key(runtime_key),
        process: None,
        files: None,
        probed: false,
        probe_error: None,
    }
}

#[test]
fn deploy_entries_resolve_the_runtime_key_from_the_archive() {
    let mut state = DeployState::provisional(
        "nginx",
        "nginx:latest",
        AppRuntime::Nginx,
        DeploySignal::Image,
    );
    let records = vec![
        deploy_entry("site", "nginx").record,
        deploy_entry("api", "go").record,
    ];
    state.set_records(&records);

    assert_eq!(state.entries.len(), 2);
    // 运行时从存档里的**稳定键**还原（不是从显示名，也不是靠猜）
    assert_eq!(state.entries[0].runtime, Some(AppRuntime::Nginx));
    assert_eq!(state.entries[1].runtime, Some(AppRuntime::Go));
    // `set_records` 只填记录 —— 现状要问容器，所以这时**还没问过**
    assert!(state.entries.iter().all(|e| !e.probed));
    assert!(state.entries.iter().all(|e| e.files.is_none()));

    assert_eq!(state.index_of("api"), Some(1));
    assert_eq!(state.index_of("nope"), None);
}

#[test]
fn an_unreadable_runtime_key_is_reported_not_guessed() {
    // 存档被手改坏（或者更新过的版本写了别的键）时，**不猜默认值**
    //（猜错会把文件铺到别的地方），而是如实说"认不出来"。
    let mut state = DeployState::provisional(
        "nginx",
        "nginx:latest",
        AppRuntime::Nginx,
        DeploySignal::Image,
    );
    state.set_records(&[deploy_entry("weird", "rust").record]);

    assert_eq!(
        state.entries.len(),
        1,
        "认不出来也要留在列表里，不能悄悄藏起来"
    );
    assert_eq!(state.entries[0].runtime, None);
    assert_eq!(state.entries[0].status_label(), "类型认不出来");
    assert!(!state.entries[0].is_process());
}

#[test]
fn deploy_status_labels_tell_the_three_shapes_apart() {
    // 还没问过容器
    let mut entry = deploy_entry("api", "go");
    assert_eq!(entry.status_label(), "读取中…");

    // 问过了：进程跑着
    entry.probed = true;
    entry.process = Some(ProcessStatus::Running);
    entry.files = Some(4);
    assert_eq!(entry.status_label(), "运行中");
    assert!(entry.is_process());

    // 进程停了，但目标目录里还有东西 → 「已停止」
    entry.process = Some(ProcessStatus::Stopped);
    assert_eq!(entry.status_label(), "已停止");

    // 进程停了、目标目录也没了 → 「目标不存在」
    // （容器被删掉重建之后就会是这一种，见 `crate::deploys` 的说明）
    entry.files = None;
    assert_eq!(entry.status_label(), "目标不存在");

    // 内容形态（nginx）没有属于我们的进程，看文件在不在
    let mut content = deploy_entry("site", "nginx");
    content.probed = true;
    content.files = Some(12);
    assert_eq!(content.status_label(), "已部署");
    assert!(!content.is_process(), "nginx 是内容形态，没有启动/停止");

    content.files = Some(0);
    assert_eq!(content.status_label(), "目标目录是空的");

    content.files = None;
    assert_eq!(content.status_label(), "目标不存在");
}

#[test]
fn deploy_state_takes_the_authoritative_runtime_from_the_probe() {
    // 列表那条启发式先给一个（可能是按命令猜的），inspect 回来后覆盖
    let mut state = DeployState::provisional(
        "web",
        "my-company/web:1",
        AppRuntime::Node,
        DeploySignal::Command,
    );
    let probe = wslc_core::cmd::deploy::DeployProbe {
        container: "web".to_owned(),
        image: "docker.1panel.live/library/nginx:latest".to_owned(),
        runtime: AppRuntime::Nginx,
        web_root: Some("/usr/share/nginx/html".to_owned()),
        ports: vec![(
            "80/tcp".to_owned(),
            vec![("127.0.0.1".to_owned(), "80".to_owned())],
        )],
        networks: vec!["bridge".to_owned()],
    };
    state.apply_probe(&probe);

    assert_eq!(state.runtime, AppRuntime::Nginx);
    assert_eq!(state.image, "docker.1panel.live/library/nginx:latest");
    // 权威判定来自 inspect（字段完整、没被截断）→ 依据记成镜像名那一档
    assert_eq!(state.signal, DeploySignal::Image);
    // Web 根是从挂载点推出来的（内容形态的默认目标目录用它）
    assert_eq!(state.web_root.as_deref(), Some("/usr/share/nginx/html"));
    // 判端口发布用的是**唯一那份**纯函数（界面和这里调的是同一个）
    assert!(wslc_core::model::deploy::port_is_published(
        &probe.ports,
        80
    ));
    assert!(!wslc_core::model::deploy::port_is_published(
        &probe.ports,
        8080
    ));
    assert!(
        !state.busy,
        "探测回来必须把忙清掉，否则按钮永远是「读取中…」"
    );
}

#[test]
fn every_deploy_phase_says_what_it_is_doing() {
    // ⚠️ 「停止」曾经复用 `Starting`，于是那一行写着"正在启动…" ——
    // 按钮和文案对不上，是实机一眼能看出来、CI 看不出来的那类问题。
    assert_eq!(DeployPhase::Starting.label(), "正在启动…");
    assert_eq!(DeployPhase::Stopping.label(), "正在停止…");
    assert_ne!(DeployPhase::Stopping.label(), DeployPhase::Starting.label());
    assert_eq!(DeployPhase::Uploading.label(), "正在上传…");
    assert_eq!(DeployPhase::Configuring.label(), "正在写入站点配置…");
    assert_eq!(DeployPhase::Removing.label(), "正在删除…");
}

#[test]
fn deploy_failure_always_gives_a_next_step() {
    // 实测抓下来的原文（容器没运行）
    let failure = DeployFailure::from_text(
        "容器\"x\"未运行。\n错误代码： WSLC_E_CONTAINER_NOT_RUNNING".to_owned(),
    );
    assert_eq!(failure.kind, DeployErrorKind::ContainerNotRunning);
    assert!(
        failure.summary().contains("启动"),
        "提示条要说清下一步：{}",
        failure.summary()
    );
    // 分类认不出来时不能空着 —— 给原文第一行
    let other = DeployFailure::from_text("没法归类的一句话\n第二行".to_owned());
    assert_eq!(other.summary(), "没法归类的一句话");
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

#[test]
fn toasts_expire_on_their_own() {
    // 报错给双倍时间（文本更长、用户往往要照着它做事）
    assert_eq!(
        Toast::error("boom").lifetime(),
        std::time::Duration::from_secs(8)
    );
    assert_eq!(
        Toast::info("hi").lifetime(),
        std::time::Duration::from_secs(4)
    );
    assert_eq!(
        Toast::success("ok").lifetime(),
        std::time::Duration::from_secs(4)
    );
    // 刚弹出来的**不能**被收掉（否则提示会一闪而过）
    assert!(!Toast::success("ok").lifetime().is_zero(), "4 秒不能是 0");
}

#[test]
fn expired_toasts_get_cleared_and_the_rest_survive() {
    // 和这个模块里其它 AppState 测试同一个造法（`AppState::new` 会读一次
    // 偏好 / 存档，那几条测试早就这么干了）
    let mut state = AppState::new(wslc_core::Wslc::new(), wslc_core::Wsl::new());
    // 没有提示时什么都不做（也不该报"清掉了"）
    assert!(!state.clear_expired_toast());

    // 刚弹的：留着重绘信号应该是 **false**（每半秒无脑重绘会把界面拖住）
    state.notify(Toast::info("hi"));
    assert!(!state.clear_expired_toast());
    assert!(state.toast.is_some(), "刚弹的提示不该被收掉");

    // 把它"变老"到超过自己的寿命，就该被收掉、并且报告 true
    if let Some(t) = state.toast.as_mut() {
        t.shown_at =
            std::time::Instant::now() - (t.lifetime() + std::time::Duration::from_millis(1));
    }
    assert!(
        state.clear_expired_toast(),
        "过期的提示应当被收掉并报告 true"
    );
    assert!(state.toast.is_none());
    // 已经空了：再来一次不该报 true
    assert!(!state.clear_expired_toast());
}

#[test]
// `field_reassign_with_default`：这个测试只关心 `tags` / `input_tag` 两个
// 字段，而 `RegistryState` 有十几个 —— 结构体更新语法要把其余全部写一遍
// `..Default::default()`，反而看不出"这个测试只设了这两样"。
//
// ⚠️ 属性放在这里（`fn` 上）而不是赋值语句上：后者是 nightly-only
// （`E0658: attributes on expressions are experimental`，CI 抓到过）。
#[allow(clippy::field_reassign_with_default)]
fn a_typed_tag_that_the_list_lacks_is_still_a_choice() {
    // 实机 bug（v0.9.4）：用户打 `openjdk:8-jre-slim`，而这个 registry 的
    // tags/list 里**没有**它（实测 docker.1panel.live 对 library/openjdk
    // 只回 17282 个，没有 8-jre-slim 也没有 latest，但两者的 manifest 都是
    // 200）。那时下拉框必须**仍然把用户要的那个列出来** —— 否则组件找不到
    // 选中项就会自己选中第一项，把"将拉取"换成另一个 tag（**拉错镜像**）。
    //
    // `field_reassign_with_default`：`RegistryState` 有十几个字段，这里只
    // 关心其中两个；用结构体更新语法要把其余全部写一遍 `..Default::default()`，
    // 反而看不出"这个测试只设了 tags 和 input_tag"。
    //
    // ⚠️ `#[allow]` 挂在 **`fn`** 上（见函数签名上方），不能挂在赋值语句上
    // —— 属性放在**表达式**位置是 nightly-only（`E0658: attributes on
    // expressions are experimental`，CI 抓到过）。
    let mut registry = RegistryState::default();
    registry.tags = vec!["28-ea".to_owned(), "10-ea".to_owned()];
    registry.input_tag = Some("8-jre-slim".to_owned());

    let items = registry.tag_items();
    assert_eq!(
        items.first().map(String::as_str),
        Some("8-jre-slim"),
        "手打的 tag 要补进候选项、而且放最前面：{items:?}"
    );
    assert_eq!(items.len(), 3, "{items:?}");
    assert_eq!(
        items.iter().filter(|tag| *tag == "8-jre-slim").count(),
        1,
        "不能重复插：{items:?}"
    );

    // 列表里**已经有**它时，顺序和条数都不该变（免得列表被搅乱）
    registry.tags = vec!["8-jre-slim".to_owned(), "28-ea".to_owned()];
    assert_eq!(
        registry.tag_items(),
        vec!["8-jre-slim".to_owned(), "28-ea".to_owned()]
    );

    // 既没选、输入里也没 tag → 就是列表本身
    registry.input_tag = None;
    registry.selected = None;
    assert_eq!(
        registry.tag_items(),
        vec!["8-jre-slim".to_owned(), "28-ea".to_owned()]
    );
}
