//! 页面枚举、分组、侧边栏高亮、谁能直接进。

use super::super::*;

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
                 改了页面上的输入框就要同步改 `Shell::set_page` 和这条断言"
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
