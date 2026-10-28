//! 网络命令。

use crate::cli::Wslc;
use crate::error::{Error, Result};
use crate::jsonl;
use crate::model::NetworkListItem;
use crate::model::deploy;

/// `wslc network list --format json`
pub fn list(wslc: &Wslc) -> Result<Vec<NetworkListItem>> {
    let out = wslc.run_checked(&["network", "list", "--format", "json"])?;
    jsonl::parse_lines(&out.stdout)
}

/// 删除网络。
pub fn remove(wslc: &Wslc, names: &[String]) -> Result<()> {
    if names.is_empty() {
        return Err(Error::InvalidArgument(
            "network remove 至少需要一个网络".into(),
        ));
    }
    let mut args: Vec<String> = vec!["network".into(), "remove".into()];
    args.extend(names.iter().cloned());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    wslc.run_checked(&refs)?;
    Ok(())
}

/// 建一个用户自定义网络。
pub fn create(wslc: &Wslc, name: &str) -> Result<()> {
    wslc.run_checked(&["network", "create", name])?;
    Ok(())
}

/// 把容器接到网络（实测运行中的容器也能直接接）。
pub fn connect(wslc: &Wslc, network: &str, container: &str) -> Result<()> {
    wslc.run_checked(&["network", "connect", network, container])?;
    Ok(())
}

/// 这个网络名存不存在（按名字精确匹配）。
pub fn exists(wslc: &Wslc, name: &str) -> Result<bool> {
    Ok(list(wslc)?
        .iter()
        .any(|network| network.name.as_str() == name))
}

/// 保证两个容器在同一个**用户自定义**网络上，返回用到的网络名。
///
/// 顺序：
/// 1. 已经有共同用户网络 → 直接用（不动现有拓扑）；
/// 2. 否则用 [`deploy::PANEL_NETWORK`]，不存在就建，然后**只接缺的那一边**。
///
/// ⚠️ `bridge` 不算：默认 bridge 上容器名解析不了（实测 `getent` rc=2），
/// 反代的上游名字必须落在用户自定义网络上。
pub fn ensure_shared_network(
    wslc: &Wslc,
    left_container: &str,
    left_networks: &[String],
    right_container: &str,
    right_networks: &[String],
) -> Result<String> {
    if let Some(name) = deploy::common_user_network(left_networks, right_networks) {
        return Ok(name);
    }
    let name = deploy::PANEL_NETWORK;
    if !exists(wslc, name)? {
        create(wslc, name)?;
    }
    if !left_networks.iter().any(|network| network.as_str() == name) {
        connect(wslc, name, left_container)?;
    }
    if !right_networks
        .iter()
        .any(|network| network.as_str() == name)
    {
        connect(wslc, name, right_container)?;
    }
    Ok(name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fixture_through_the_same_path() {
        let items: Vec<NetworkListItem> =
            jsonl::parse_lines(include_str!("../../tests/fixtures/network_list.jsonl")).unwrap();
        assert_eq!(items.len(), 3);
    }

    /// 真机夹具里**只有 `bridge` 算内置**（`host` / `none` 在这份输出里没有，
    /// 所以这里断言的是"过滤之后恰好剩一个，且是 bridge"）。
    ///
    /// 它测的是 [`NetworkListItem::is_builtin`] 对**真实输出**的判据 ——
    /// 面板的「网络」页靠它决定禁不禁用删除按钮（内置网络删不掉）。
    ///
    /// （这条原先叫 `attachable_names_…`，因为那时 `cmd::network` 里有个
    /// 同判据的 `attachable_names()`；那个函数没有任何调用点，已删。）
    #[test]
    fn the_fixture_has_exactly_one_builtin_network() {
        let items: Vec<NetworkListItem> =
            jsonl::parse_lines(include_str!("../../tests/fixtures/network_list.jsonl")).unwrap();
        let names: Vec<String> = items
            .into_iter()
            .filter(|n| matches!(n.name.as_str(), "bridge") || !n.is_builtin())
            .map(|n| n.name)
            .collect();
        assert_eq!(names, vec!["bridge"]);
    }

    #[test]
    fn remove_rejects_empty_list_before_spawning() {
        let wslc = Wslc::with_program("definitely-not-a-real-binary");
        assert!(matches!(remove(&wslc, &[]), Err(Error::InvalidArgument(_))));
    }
}
