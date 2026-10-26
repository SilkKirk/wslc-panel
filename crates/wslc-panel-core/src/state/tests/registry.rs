//! 镜像仓库查询状态与 tag 选择。

use super::super::prelude::*;
use super::super::*;

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
