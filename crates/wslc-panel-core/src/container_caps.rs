//! 容器列表行上有哪些**专属入口**：数据库页 / 部署管理。
//!
//! # 为什么放在这里
//!
//! 这两个判据都只吃列表项里的字符串与端口，是纯逻辑；而 `views/containers.rs`
//! 每帧、每一行都要算一次。放在 `wslc-panel-core` 有两个好处：
//!
//! 1. `core` job 能真跑断言（写在 `wslc-panel` 里的测试 CI 不会执行）；
//! 2. 端口这类"容易漏传"的参数只有一个构造点 —— 原先 UI 里给
//!    `guess_kind` 传的是 `container_ports: &[]`，PostgreSQL 的 5432 兜底
//!    因此永远不生效。
//!
//! # 只看运行中的容器
//!
//! 停止的容器上这些入口点进去必然失败（数据库页要 `exec`，部署页要
//! `exec` / `cp`），所以这里直接返回默认值 —— 和视图原先的 `if running`
//! 是同一个判据，只是收进了一处。

use wslc_core::db::{ConnHint, guess_kind};
use wslc_core::model::ContainerListItem;
use wslc_core::model::deploy::{DeployCandidate, container_app_kind};
use wslc_core::model::mysql::mysql_container_kind;

/// 一个容器在列表行上能提供哪些专属入口。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ContainerCapabilities {
    /// 是否画「数据库」按钮。
    pub database: bool,
    /// 「部署管理」的判定结果；`None` = 不画。
    pub deploy: Option<DeployCandidate>,
}

/// 从列表项判断入口。
pub fn capabilities(item: &ContainerListItem) -> ContainerCapabilities {
    if !item.is_running() {
        return ContainerCapabilities::default();
    }
    ContainerCapabilities {
        database: has_database_page(item),
        deploy: container_app_kind(item),
    }
}

/// 这个容器有没有可用的「数据库」页。
///
/// MySQL 家族有专门的 `mysql_container_kind`（镜像名 / 命令 / 3306 端口）；
/// PostgreSQL 没有对应函数，走通用的 `guess_kind` —— 它**必须**拿到真实
/// 端口，否则 5432 那条低置信度兜底永远不生效。
fn has_database_page(item: &ContainerListItem) -> bool {
    if mysql_container_kind(item).is_some() {
        return true;
    }
    let ports: Vec<u16> = item
        .port_mappings()
        .iter()
        .map(|mapping| mapping.container_port)
        .collect();
    guess_kind(&ConnHint {
        image: &item.image,
        command: &item.command,
        container_ports: &ports,
    })
    .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(image: &str, command: &str, ports: &str, state: &str) -> ContainerListItem {
        ContainerListItem {
            image: image.to_owned(),
            command: command.to_owned(),
            ports: ports.to_owned(),
            state: state.to_owned(),
            ..ContainerListItem::default()
        }
    }

    #[test]
    fn stopped_containers_have_no_entry_points() {
        let caps = capabilities(&item(
            "nginx:latest",
            "nginx -g 'daemon off;'",
            "127.0.0.1:8080->80/tcp",
            "exited",
        ));
        assert_eq!(caps, ContainerCapabilities::default());
    }

    #[test]
    fn mysql_is_recognized_by_the_existing_detector() {
        let caps = capabilities(&item(
            "docker.1panel.live/library/mysql:latest",
            "",
            "",
            "running",
        ));
        assert!(caps.database);
    }

    #[test]
    fn postgres_is_recognized_from_its_published_port() {
        // 镜像名和命令都认不出来，只有 5432 这一条线索 —— 这条断言钉住
        // "端口真的被传进 guess_kind 了"（原先视图传的是空数组）。
        let caps = capabilities(&item(
            "mycompany/analytics:1.0",
            "",
            "127.0.0.1:15432->5432/tcp",
            "running",
        ));
        assert!(caps.database, "5432 端口应当能推出 PostgreSQL");
    }

    #[test]
    fn deploy_runtime_is_reported_independently_of_database() {
        let caps = capabilities(&item("nginx:latest", "", "", "running"));
        assert!(caps.deploy.is_some());
        assert!(!caps.database);
    }
}
