//! 卷模型。
//!
//! ⚠️ 采集环境里**没有任何卷**，`wslc volume list --format json` 输出 0 字节，
//! 因此下面的字段名是**按 `docker volume ls` 的惯例推断**的，尚未实机验证。
//! 所有字段都带 `#[serde(default)]` 并额外保留 `extra` 原始对象，
//! 首次遇到真实卷时请用 `VolumeListItem::extra` 校准
//! （见 `docs/wslc-schema.md` §7 的待验证标注）。

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// `wslc volume list --format json` 的一行。
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct VolumeListItem {
    /// 卷名。
    #[serde(rename = "Name", default)]
    pub name: String,
    /// 驱动。
    #[serde(rename = "Driver", default)]
    pub driver: String,
    /// 挂载点。
    #[serde(rename = "Mountpoint", default)]
    pub mountpoint: String,
    /// 作用域。
    #[serde(rename = "Scope", default)]
    pub scope: String,
    /// 创建时间。
    #[serde(rename = "CreatedAt", default)]
    pub created_at: String,
    /// 标签。
    #[serde(rename = "Labels", default)]
    pub labels: String,
    /// 未被上面的字段消费的原始键值。
    ///
    /// 这是应对 schema 漂移的兜底：新增字段会出现在这里，UI 的"原始 JSON"
    /// 面板可以直接展示。
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

impl VolumeListItem {
    /// 展示名，空名时回退到 `-`。
    pub fn display_name(&self) -> &str {
        if self.name.is_empty() {
            "-"
        } else {
            &self.name
        }
    }

    /// 是否被容器使用（未能确定时返回 `None`）。
    pub fn in_use(&self) -> Option<bool> {
        let raw = self.extra.get("InUse").or_else(|| self.extra.get("inUse"))?;
        match raw {
            Value::Bool(b) => Some(*b),
            Value::String(s) => Some(matches!(
                s.trim().to_ascii_lowercase().as_str(),
                "true" | "1" | "yes"
            )),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_conventional_volume_json() {
        let line = r#"{"Name":"pgdata","Driver":"local","Mountpoint":"/var/lib/x","Scope":"local","CreatedAt":"2026-10-08"}"#;
        let vol: VolumeListItem = serde_json::from_str(line).unwrap();
        assert_eq!(vol.name, "pgdata");
        assert_eq!(vol.driver, "local");
        assert_eq!(vol.display_name(), "pgdata");
        assert!(vol.extra.is_empty());
    }

    #[test]
    fn unknown_fields_are_preserved_in_extra() {
        let line = r#"{"Name":"v1","Size":"12MB","UsageData":{"Size":123}}"#;
        let vol: VolumeListItem = serde_json::from_str(line).unwrap();
        assert_eq!(vol.name, "v1");
        assert_eq!(vol.extra.get("Size").unwrap(), "12MB");
        assert!(vol.extra.contains_key("UsageData"));
    }

    #[test]
    fn in_use_reads_boolean_or_string() {
        let v: VolumeListItem = serde_json::from_str(r#"{"Name":"a","InUse":true}"#).unwrap();
        assert_eq!(v.in_use(), Some(true));
        let v: VolumeListItem = serde_json::from_str(r#"{"Name":"a","InUse":"true"}"#).unwrap();
        assert_eq!(v.in_use(), Some(true));
        let v: VolumeListItem = serde_json::from_str(r#"{"Name":"a"}"#).unwrap();
        assert_eq!(v.in_use(), None);
    }

    #[test]
    fn empty_object_does_not_panic() {
        let v: VolumeListItem = serde_json::from_str("{}").unwrap();
        assert_eq!(v.display_name(), "-");
    }
}
