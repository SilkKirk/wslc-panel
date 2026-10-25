//! 容器列表的**过滤与排序**：纯逻辑，不碰 GPUI。
//!
//! # 为什么单独一个模块
//!
//! 这几条规则（关键词匹配哪些字段、`paused` 算不算「运行中」、默认排序保不保序）
//! 全是**会被人一眼看错**的东西，必须能用单测钉住。放在 `views.rs` 里的话
//! 断言跑不到（那个 test target 要链接整棵 GPUI 依赖树，见本 crate 的顶层说明）。
//!
//! # 查询词住在哪儿
//!
//! **不在这里**。它住在 `Shell::container_search` 的 `InputState` 里 ——
//! 输入框是 GPUI 类型，进不了这个 crate。这里只收一个 `&str`。

use wslc_core::model::ContainerSummary;

/// 容器列表要显示哪一段。
///
/// ⚠️ 叫「未运行」而不是「已停止」：`wslc` 的状态里还有 `created` / `paused` /
/// `dead`，它们都不是「停止」，但都不在 `running` 里。用「已停止」会把
/// `paused` 说错，而 `paused` 的容器其实还占着资源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ContainerScope {
    /// 全部（含已退出）。
    #[default]
    All,
    /// 运行中（`State == running`）。
    Running,
    /// 未运行（`created` / `paused` / `exited` / `dead` …）。
    NotRunning,
}

impl ContainerScope {
    /// 全部可选值（决定筛选按钮的顺序）。
    pub const ALL: [ContainerScope; 3] = [Self::All, Self::Running, Self::NotRunning];

    /// 按钮文案。
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "全部",
            Self::Running => "运行中",
            Self::NotRunning => "未运行",
        }
    }
}

/// 排序方式。
///
/// `Default` 刻意保留 `wslc list` 的原始顺序 —— 那是用户升级前看到的顺序，
/// 加筛选功能不该顺手把他熟悉的列表顺序也改掉。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ContainerSort {
    /// `wslc list` 给什么顺序就是什么顺序。
    #[default]
    Default,
    /// 按容器名（ASCII 小写比较；中文名不受大小写影响）。
    Name,
    /// 运行中在前，同状态按名称。
    State,
}

impl ContainerSort {
    /// 全部可选值（决定排序菜单的顺序）。
    pub const ALL: [ContainerSort; 3] = [Self::Default, Self::Name, Self::State];

    /// 菜单文案。
    pub fn label(self) -> &'static str {
        match self {
            Self::Default => "默认",
            Self::Name => "名称",
            Self::State => "状态",
        }
    }
}

/// 页面上的筛选状态（查询词不在这里，它住在 `InputState` 里）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ContainerView {
    /// 状态范围。
    pub scope: ContainerScope,
    /// 排序方式。
    pub sort: ContainerSort,
}

/// 过滤 + 排序，返回引用（不复制 `ContainerSummary`）。
///
/// 匹配规则见 [`matches_query`]；顺序规则见 [`ContainerSort`]。
pub fn visible_containers<'a>(
    items: &'a [ContainerSummary],
    query: &str,
    view: ContainerView,
) -> Vec<&'a ContainerSummary> {
    let mut visible: Vec<&ContainerSummary> = items
        .iter()
        // ⚠️ `filter` 的闭包参数是 `&&ContainerSummary`，传给要
        // `&ContainerSummary` 的参数时会自动解一层 —— 写 `*summary` 是多余的
        // （clippy 的 `explicit_auto_deref`）。
        .filter(|summary| in_scope(summary, view.scope) && matches_query(summary, query))
        .collect();

    match view.sort {
        // `sort_by` 是稳定排序；但 Default 根本不排序，直接保持原序。
        ContainerSort::Default => {}
        ContainerSort::Name => {
            // ⚠️ 别写 `(*summary)`：闭包参数的类型是 `&&ContainerSummary`，
            // 字段访问本来就会自动解引用，手写那一下会被 clippy 的
            // `explicit_auto_deref` 报出来（日志里那 5 条就是它）。
            visible.sort_by_cached_key(|summary| summary.item.display_name().to_lowercase());
        }
        ContainerSort::State => {
            // `(false, name)` 排在 `(true, name)` 前面 —— false = 运行中。
            // 第二个键让同状态的容器有稳定、可预期的顺序（否则每次刷新
            // 可能换一个排列，用户会以为容器在乱跳）。
            visible.sort_by_cached_key(|summary| {
                (
                    !summary.item.is_running(),
                    summary.item.display_name().to_lowercase(),
                )
            });
        }
    }

    visible
}

/// `(全部, 运行中, 未运行)`，给筛选按钮上的计数用。
///
/// 「运行中」用 `ContainerListItem::is_running()`，和列表行按钮、
/// 状态格用的是**同一个**判断。`wslc list` 那份「只列运行中」的输出
/// 只用于「停止全部」的影响面（它执行时会现查一遍），不在这里数。
pub fn scope_counts(items: &[ContainerSummary]) -> (usize, usize, usize) {
    let running = items.iter().filter(|summary| summary.item.is_running()).count();
    (items.len(), running, items.len().saturating_sub(running))
}

/// 这一条在不在选中的状态范围里。
fn in_scope(summary: &ContainerSummary, scope: ContainerScope) -> bool {
    let running = summary.item.is_running();
    match scope {
        ContainerScope::All => true,
        ContainerScope::Running => running,
        ContainerScope::NotRunning => !running,
    }
}

/// 查询词匹配。
///
/// 匹配：**容器名、镜像引用、ID、端口映射、网络名**。大小写不敏感。
///
/// 刻意**不匹配** `Command`：它经常是一长串 `tail -f /dev/null` 或
/// `docker-entrypoint.sh`，匹配进来会让「搜 nginx」出现一堆无关容器。
fn matches_query(summary: &ContainerSummary, query: &str) -> bool {
    let query = query.trim();
    if query.is_empty() {
        return true;
    }
    let query = query.to_lowercase();
    let item = &summary.item;
    [
        item.display_name(),
        item.image.as_str(),
        item.id.as_str(),
        item.ports.as_str(),
        item.networks.as_str(),
    ]
    .iter()
    .any(|field| field.to_lowercase().contains(&query))
}

#[cfg(test)]
mod tests {
    use super::{ContainerScope, ContainerSort, ContainerView, scope_counts, visible_containers};
    use wslc_core::model::{ContainerListItem, ContainerSummary};

    /// 造一条只带筛选相关字段的容器。
    ///
    /// 端口 / 网络只给 `web` 填：`query_matches_...` 要证明"搜端口/网络也只命中
    /// 那一条"。如果四条都填同一个 `8080` / `bridge`，那个测试就会正确地
    /// 命中四条 —— 测的就不再是"字段匹配"，而是"同名值也算匹配"。
    fn summary(name: &str, image: &str, state: &str) -> ContainerSummary {
        let is_web = name == "web";
        ContainerSummary {
            item: ContainerListItem {
                names: name.to_owned(),
                image: image.to_owned(),
                id: format!("id-{name}"),
                state: state.to_owned(),
                ports: if is_web {
                    "8080:80/tcp".to_owned()
                } else {
                    String::new()
                },
                networks: if is_web {
                    "bridge".to_owned()
                } else {
                    "backend".to_owned()
                },
                ..Default::default()
            },
            stats: None,
        }
    }

    fn sample() -> Vec<ContainerSummary> {
        vec![
            summary("web", "docker.1panel.live/library/nginx:latest", "running"),
            summary("db", "mysql:8", "exited"),
            summary("job", "alpine:latest", "created"),
            summary("cache", "redis:7", "paused"),
        ]
    }

    fn view(scope: ContainerScope, sort: ContainerSort) -> ContainerView {
        ContainerView { scope, sort }
    }

    #[test]
    fn empty_query_returns_all_in_original_order() {
        let items = sample();
        let visible = visible_containers(&items, "", view(ContainerScope::All, ContainerSort::Default));
        let names: Vec<&str> = visible.iter().map(|s| s.item.names.as_str()).collect();
        assert_eq!(names, vec!["web", "db", "job", "cache"]);
    }

    #[test]
    fn empty_input_gives_empty_output() {
        let visible = visible_containers(&[], "", ContainerView::default());
        assert!(visible.is_empty());
    }

    #[test]
    fn query_matches_name_image_id_ports_network_case_insensitively() {
        let items = sample();
        for query in ["WEB", "nginx", "id-web", "8080", "BRIDGE"] {
            let visible = visible_containers(
                &items,
                query,
                view(ContainerScope::All, ContainerSort::Default),
            );
            assert_eq!(visible.len(), 1, "查询 {query} 应该只命中 web");
            assert_eq!(visible[0].item.names, "web");
        }

        // 不匹配 Command（这里没填），也不该因为空字段匹配全部
        let visible = visible_containers(
            &items,
            "tail -f",
            view(ContainerScope::All, ContainerSort::Default),
        );
        assert!(visible.is_empty());
    }

    #[test]
    fn query_is_trimmed() {
        let items = sample();
        let visible = visible_containers(
            &items,
            "  web  ",
            view(ContainerScope::All, ContainerSort::Default),
        );
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].item.names, "web");
    }

    #[test]
    fn scope_counts_use_is_running_and_keep_paused_out_of_running() {
        let items = sample();
        // web = running；db = exited；job = created；cache = paused
        assert_eq!(scope_counts(&items), (4, 1, 3));
    }

    #[test]
    fn sort_by_name_is_case_insensitive() {
        let items = vec![
            summary("Zeta", "a", "running"),
            summary("alpha", "b", "running"),
        ];
        let visible = visible_containers(
            &items,
            "",
            view(ContainerScope::All, ContainerSort::Name),
        );
        let names: Vec<&str> = visible.iter().map(|s| s.item.names.as_str()).collect();
        assert_eq!(names, vec!["alpha", "Zeta"]);
    }

    #[test]
    fn sort_by_state_puts_running_first_then_name() {
        let items = vec![
            summary("zeta", "a", "exited"),
            summary("beta", "b", "running"),
            summary("alpha", "c", "running"),
        ];
        let visible = visible_containers(
            &items,
            "",
            view(ContainerScope::All, ContainerSort::State),
        );
        let names: Vec<&str> = visible.iter().map(|s| s.item.names.as_str()).collect();
        assert_eq!(names, vec!["alpha", "beta", "zeta"]);
    }

    #[test]
    fn sort_default_keeps_input_order() {
        let items = vec![
            summary("zeta", "a", "running"),
            summary("alpha", "b", "exited"),
        ];
        let visible = visible_containers(
            &items,
            "",
            view(ContainerScope::All, ContainerSort::Default),
        );
        let names: Vec<&str> = visible.iter().map(|s| s.item.names.as_str()).collect();
        assert_eq!(names, vec!["zeta", "alpha"]);
    }

    #[test]
    fn scope_filter_keeps_paused_in_not_running() {
        let items = sample();
        let visible = visible_containers(
            &items,
            "",
            view(ContainerScope::NotRunning, ContainerSort::Default),
        );
        let names: Vec<&str> = visible.iter().map(|s| s.item.names.as_str()).collect();
        assert_eq!(names, vec!["db", "job", "cache"]);
    }
}
