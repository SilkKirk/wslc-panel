//! 系统信息模型（`wslc info --format json`）。

use serde::{Deserialize, Serialize};

/// `wslc info --format json` 的顶层对象。
///
/// 注意：这是**单个 JSON 对象**，不是 JSON Lines。
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct SystemInfo {
    /// 客户端（Windows 侧）信息。
    #[serde(rename = "Client", default)]
    pub client: ClientInfo,
    /// 服务器（会话管理器）信息。
    #[serde(rename = "Server", default)]
    pub server: ServerInfo,
}

/// 客户端信息。
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct ClientInfo {
    /// WSL 版本，如 `3.0.1.0`。
    #[serde(rename = "Version", default)]
    pub version: String,
    /// Linux 内核版本。
    #[serde(rename = "KernelVersion", default)]
    pub kernel_version: String,
    /// Direct3D 版本。
    #[serde(rename = "Direct3DVersion", default)]
    pub direct3d_version: String,
    /// DXCore 版本。
    #[serde(rename = "DxCoreVersion", default)]
    pub dxcore_version: String,
    /// Windows 版本。
    #[serde(rename = "WindowsVersion", default)]
    pub windows_version: String,
    /// `settings.yaml` 的绝对路径。
    #[serde(rename = "SettingsFile", default)]
    pub settings_file: String,
}

/// 服务器信息。
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct ServerInfo {
    /// 会话管理器版本。
    #[serde(rename = "SessionManagerVersion", default)]
    pub session_manager_version: String,
    /// 活动会话列表。
    #[serde(rename = "Sessions", default)]
    pub sessions: Vec<SessionInfo>,
}

/// 会话条目（来自 `wslc info`，与 `system session list` 的表格等价）。
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct SessionInfo {
    /// 会话 ID。
    #[serde(rename = "ID", default)]
    pub id: u32,
    /// 会话显示名。
    #[serde(rename = "Name", default)]
    pub name: String,
    /// 创建者进程 PID。
    #[serde(rename = "CreatorPid", default)]
    pub creator_pid: u32,
}

impl SystemInfo {
    /// 主版本号字符串，如 `3.0.1.0`。
    pub fn version(&self) -> &str {
        &self.client.version
    }

    /// `settings.yaml` 路径。
    pub fn settings_file(&self) -> &str {
        &self.client.settings_file
    }

    /// 当前活动的会话 ID 列表。
    pub fn session_ids(&self) -> Vec<u32> {
        self.server.sessions.iter().map(|s| s.id).collect()
    }

    /// 是否至少有一个活动会话。
    pub fn has_session(&self) -> bool {
        !self.server.sessions.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../tests/fixtures/info.json");

    #[test]
    fn parses_real_info_output() {
        let info: SystemInfo = crate::jsonl::parse_object(FIXTURE).unwrap();
        assert_eq!(info.client.version, "3.0.1.0");
        assert_eq!(info.client.kernel_version, "6.18.40.1-1");
        assert_eq!(info.client.direct3d_version, "1.611.1-81528511");
        assert_eq!(info.client.windows_version, "10.0.26300.9550");
        assert_eq!(info.server.session_manager_version, "3.0.1");
        assert_eq!(info.version(), "3.0.1.0");
        assert!(info.has_session());
        assert_eq!(info.session_ids(), vec![1]);
    }

    #[test]
    fn settings_path_preserves_windows_backslashes() {
        let info: SystemInfo = crate::jsonl::parse_object(FIXTURE).unwrap();
        assert_eq!(
            info.settings_file(),
            r"C:\Users\76434\AppData\Local\wslc\settings.yaml"
        );
    }

    #[test]
    fn parses_session_entries() {
        let info: SystemInfo = crate::jsonl::parse_object(FIXTURE).unwrap();
        let s = &info.server.sessions[0];
        assert_eq!(s.id, 1);
        assert_eq!(s.name, "wslc-cli-76434");
        assert_eq!(s.creator_pid, 21684);
    }

    #[test]
    fn missing_fields_default_to_empty() {
        let info: SystemInfo = crate::jsonl::parse_object("{}").unwrap();
        assert_eq!(info.version(), "");
        assert!(!info.has_session());
        assert!(info.session_ids().is_empty());
    }
}
