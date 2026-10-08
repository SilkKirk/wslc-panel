//! 网络模型。

use serde::{Deserialize, Serialize};

/// `wslc network list --format json` 的一行。
///
/// ⚠️ 全部字段都是**字符串**，包括 `IPv4` / `IPv6` / `Internal` 这些布尔语义的字段
/// （`"true"` / `"false"`）。
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct NetworkListItem {
    /// 12 位网络 ID。
    #[serde(rename = "ID", default)]
    pub id: String,
    /// 网络名。
    #[serde(rename = "Name", default)]
    pub name: String,
    /// 驱动：`bridge` / `host` / `null`。
    #[serde(rename = "Driver", default)]
    pub driver: String,
    /// 作用域，通常是 `local`。
    #[serde(rename = "Scope", default)]
    pub scope: String,
    /// 是否启用 IPv4（字符串 `"true"`）。
    #[serde(rename = "IPv4", default)]
    pub ipv4: String,
    /// 是否启用 IPv6（字符串 `"true"`）。
    #[serde(rename = "IPv6", default)]
    pub ipv6: String,
    /// 是否内部网络（字符串 `"true"`）。
    #[serde(rename = "Internal", default)]
    pub internal: String,
    /// 标签。
    #[serde(rename = "Labels", default)]
    pub labels: String,
    /// 创建时间。
    #[serde(rename = "CreatedAt", default)]
    pub created_at: String,
}

impl NetworkListItem {
    /// 12 位短 ID。
    pub fn short_id(&self) -> &str {
        let n = self.id.len().min(12);
        &self.id[..n]
    }

    /// `IPv4` 字符串转布尔。
    pub fn ipv4_enabled(&self) -> bool {
        parse_bool_str(&self.ipv4)
    }

    /// `IPv6` 字符串转布尔。
    pub fn ipv6_enabled(&self) -> bool {
        parse_bool_str(&self.ipv6)
    }

    /// `Internal` 字符串转布尔。
    pub fn is_internal(&self) -> bool {
        parse_bool_str(&self.internal)
    }

    /// 是否为 wslc 内置网络（不可删除）。
    pub fn is_builtin(&self) -> bool {
        matches!(self.name.as_str(), "bridge" | "host" | "none")
    }
}

/// 把 `"true"` / `"1"` / `"yes"` 视为真。
fn parse_bool_str(s: &str) -> bool {
    matches!(
        s.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "yes" | "on"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../tests/fixtures/network_list.jsonl");

    #[test]
    fn parses_real_network_output() {
        let items: Vec<NetworkListItem> = crate::jsonl::parse_lines(FIXTURE).unwrap();
        assert_eq!(items.len(), 3);

        let bridge = &items[0];
        assert_eq!(bridge.name, "bridge");
        assert_eq!(bridge.driver, "bridge");
        assert_eq!(bridge.scope, "local");
        assert!(bridge.ipv4_enabled());
        assert!(!bridge.ipv6_enabled());
        assert!(!bridge.is_internal());
        assert!(bridge.is_builtin());
    }

    #[test]
    fn builtin_networks_are_recognised() {
        let items: Vec<NetworkListItem> = crate::jsonl::parse_lines(FIXTURE).unwrap();
        let names: Vec<&str> = items.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, vec!["bridge", "host", "none"]);
        assert!(items.iter().all(NetworkListItem::is_builtin));

        let none = items.iter().find(|n| n.name == "none").unwrap();
        assert_eq!(none.driver, "null", "driver 名是 null，不是 none");
    }

    #[test]
    fn boolean_strings_parse() {
        assert!(parse_bool_str("true"));
        assert!(parse_bool_str("TRUE"));
        assert!(parse_bool_str("1"));
        assert!(!parse_bool_str("false"));
        assert!(!parse_bool_str(""));
    }

    #[test]
    fn missing_fields_do_not_panic() {
        let net: NetworkListItem = serde_json::from_str("{}").unwrap();
        assert_eq!(net.short_id(), "");
        assert!(!net.ipv4_enabled());
        assert!(!net.is_builtin());
    }
}
