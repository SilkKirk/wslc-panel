//! 网络命令。

use crate::cli::Wslc;
use crate::error::{Error, Result};
use crate::jsonl;
use crate::model::NetworkListItem;

/// `wslc network list --format json`
pub fn list(wslc: &Wslc) -> Result<Vec<NetworkListItem>> {
    let out = wslc.run_checked(&["network", "list", "--format", "json"])?;
    jsonl::parse_lines(&out.stdout)
}

/// 可连接的网络名（排除 `host` / `none`，它们不能作为 `--network` 的通用目标）。
pub fn attachable_names(wslc: &Wslc) -> Result<Vec<String>> {
    Ok(list(wslc)?
        .into_iter()
        .filter(|n| matches!(n.name.as_str(), "bridge") || !n.is_builtin())
        .map(|n| n.name)
        .collect())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fixture_through_the_same_path() {
        let items: Vec<NetworkListItem> =
            jsonl::parse_lines(include_str!("../../tests/fixtures/network_list.jsonl")).unwrap();
        assert_eq!(items.len(), 3);
    }

    #[test]
    fn attachable_names_keeps_bridge_and_drops_host_and_none() {
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
