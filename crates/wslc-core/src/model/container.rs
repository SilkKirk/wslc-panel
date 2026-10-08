//! 容器相关模型：列表项、实时统计、`inspect` 详情。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{parse_percent, parse_size, split_outside_brackets, split_pair};

// ---------------------------------------------------------------------------
// 列表
// ---------------------------------------------------------------------------

/// `wslc list -a --format json` 的一行。
///
/// 全部字段都是**字符串**（包括 `LocalVolumes`），`Platform` 是唯一嵌套对象。
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct ContainerListItem {
    /// 容器 ID。默认 12 位；`--no-trunc` 时 64 位。
    #[serde(rename = "ID", default)]
    pub id: String,
    /// 容器名（**无前导 `/`**，与 `inspect.Name` 不同）。
    #[serde(rename = "Names", default)]
    pub names: String,
    /// 镜像引用。
    #[serde(rename = "Image", default)]
    pub image: String,
    /// 启动命令，**含字面双引号**，如 `"\"sleep 300\""`。
    #[serde(rename = "Command", default)]
    pub command: String,
    /// `2026-10-08 16:34:46 +0800 GMT+8`
    #[serde(rename = "CreatedAt", default)]
    pub created_at: String,
    /// 相对时间，如 `3 seconds ago`。
    #[serde(rename = "RunningFor", default)]
    pub running_for: String,
    /// `running` / `exited` / `created` / `paused` …
    #[serde(rename = "State", default)]
    pub state: String,
    /// 人类可读状态，如 `Up 3 seconds`。
    #[serde(rename = "Status", default)]
    pub status: String,
    /// 健康检查状态，可能为空串。
    #[serde(rename = "HealthStatus", default)]
    pub health_status: String,
    /// `127.0.0.1:18080->80/tcp`，可能为空串。
    #[serde(rename = "Ports", default)]
    pub ports: String,
    /// 网络名。
    #[serde(rename = "Networks", default)]
    pub networks: String,
    /// 挂载，可能为空串。
    #[serde(rename = "Mounts", default)]
    pub mounts: String,
    /// 本地卷数量，是**数字字符串** `"0"`。
    #[serde(rename = "LocalVolumes", default)]
    pub local_volumes: String,
    /// `0B` / `8.42MB`
    #[serde(rename = "Size", default)]
    pub size: String,
    /// 逗号分隔的 `k=v`；**值里可能是嵌套 JSON**。
    #[serde(rename = "Labels", default)]
    pub labels: String,
    /// `{"architecture":"amd64","os":"linux"}`
    #[serde(rename = "Platform", default)]
    pub platform: Option<Platform>,
}

/// 容器的目标平台。
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Platform {
    /// 架构，如 `amd64`。
    #[serde(default)]
    pub architecture: String,
    /// 操作系统，如 `linux`。
    #[serde(default)]
    pub os: String,
}

/// 容器状态分类。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContainerState {
    /// 运行中。
    Running,
    /// 已退出。
    Exited,
    /// 已创建未启动。
    Created,
    /// 已暂停。
    Paused,
    /// 重启中。
    Restarting,
    /// 正在删除。
    Removing,
    /// 已死。
    Dead,
    /// 未知状态，保留原文。
    Unknown(String),
}

impl ContainerState {
    /// 从 `wslc` 的 `State` 字段解析。
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "running" | "up" => Self::Running,
            "exited" | "stopped" => Self::Exited,
            "created" => Self::Created,
            "paused" => Self::Paused,
            "restarting" => Self::Restarting,
            "removing" => Self::Removing,
            "dead" => Self::Dead,
            other => Self::Unknown(other.to_owned()),
        }
    }

    /// 中文显示名。
    pub fn label(&self) -> &str {
        match self {
            Self::Running => "运行中",
            Self::Exited => "已退出",
            Self::Created => "已创建",
            Self::Paused => "已暂停",
            Self::Restarting => "重启中",
            Self::Removing => "删除中",
            Self::Dead => "已死",
            Self::Unknown(s) => s.as_str(),
        }
    }

    /// 是否属于"占用资源"的状态。
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Running | Self::Restarting | Self::Paused)
    }
}

impl ContainerListItem {
    /// 容器 ID（原样，可能被截断）。
    pub fn id(&self) -> &str {
        &self.id
    }

    /// 12 位短 ID。
    pub fn short_id(&self) -> &str {
        let n = self.id.len().min(12);
        &self.id[..n]
    }

    /// 展示名：优先容器名，其次短 ID。
    pub fn display_name(&self) -> &str {
        if self.names.is_empty() {
            self.short_id()
        } else {
            &self.names
        }
    }

    /// 解析后的状态。
    pub fn state_kind(&self) -> ContainerState {
        ContainerState::parse(&self.state)
    }

    /// 是否运行中。
    pub fn is_running(&self) -> bool {
        self.state_kind() == ContainerState::Running
    }

    /// 去掉 `Command` 字段外层的字面双引号与转义。
    ///
    /// `"\"sleep 300\""` → `sleep 300`
    pub fn command_clean(&self) -> String {
        let trimmed = self.command.trim();
        let unquoted = trimmed
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .unwrap_or(trimmed);
        unquoted.replace("\\\"", "\"")
    }

    /// 解析 `Ports` 字符串。
    pub fn port_mappings(&self) -> Vec<PortMapping> {
        if self.ports.trim().is_empty() {
            return Vec::new();
        }
        split_outside_brackets(&self.ports, ',')
            .into_iter()
            .filter_map(PortMapping::parse)
            .collect()
    }

    /// 解析 `Labels` 成键值对（对嵌套 JSON 安全）。
    pub fn label_pairs(&self) -> Vec<(String, String)> {
        if self.labels.trim().is_empty() {
            return Vec::new();
        }
        split_outside_brackets(&self.labels, ',')
            .into_iter()
            .filter_map(|part| {
                let (k, v) = part.split_once('=')?;
                Some((k.trim().to_owned(), v.trim().to_owned()))
            })
            .collect()
    }

    /// 从 `Labels` 里取出 WSL 自己的容器元数据（端口真实映射在这里）。
    pub fn wsl_metadata_ports(&self) -> Vec<WslPortSpec> {
        parse_wsl_metadata(&self.labels).unwrap_or_default()
    }

    /// 体积（字节）。
    pub fn size_bytes(&self) -> Option<f64> {
        parse_size(&self.size)
    }
}

/// 一条端口映射，来自 `Ports` 字符串。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortMapping {
    /// 宿主机绑定地址，如 `127.0.0.1`；未指定时为 `0.0.0.0`。
    pub host_ip: String,
    /// 宿主机端口。
    pub host_port: Option<u16>,
    /// 容器端口。
    pub container_port: u16,
    /// 协议：`tcp` / `udp`。
    pub protocol: String,
}

impl PortMapping {
    /// 解析 `127.0.0.1:18080->80/tcp`。
    ///
    /// 也接受没有主机部分的写法（`80/tcp`）。
    pub fn parse(raw: &str) -> Option<Self> {
        let s = raw.trim();
        if s.is_empty() {
            return None;
        }
        let (proto_part, rest) = match s.rsplit_once('/') {
            Some((head, proto)) => (proto, head),
            None => ("tcp", s),
        };

        let (host_part, container_part) = match rest.split_once("->") {
            Some((h, c)) => (Some(h), c),
            None => (None, rest),
        };

        let container_port = container_part
            .trim()
            .rsplit(':')
            .next()?
            .trim()
            .parse::<u16>()
            .ok()?;

        let (host_ip, host_port) = match host_part {
            None => (String::from("0.0.0.0"), None),
            Some(h) => match h.rsplit_once(':') {
                Some((ip, port)) => (ip.to_owned(), port.trim().parse::<u16>().ok()),
                // 没有冒号：可能是只写了端口的简写（`8080->80/tcp`），
                // 也可能是个不带端口的主机名/地址。纯数字按端口处理。
                //
                // `wslc list` 实际输出的是 `127.0.0.1:18080->80/tcp` 这种完整形式，
                // 但简写在这里如果不处理就会被当成"IP 叫 8080"，展示时会很怪。
                None => match h.trim().parse::<u16>() {
                    Ok(port) => (String::from("0.0.0.0"), Some(port)),
                    Err(_) => (h.to_owned(), None),
                },
            },
        };

        Some(Self {
            host_ip,
            host_port,
            container_port,
            protocol: proto_part.trim().to_ascii_lowercase(),
        })
    }

    /// `127.0.0.1:18080 → 80/tcp` 形式的展示文本。
    pub fn display(&self) -> String {
        match self.host_port {
            Some(p) => format!("{}:{} → {}/{}", self.host_ip, p, self.container_port, self.protocol),
            None => format!("{}/{}", self.container_port, self.protocol),
        }
    }
}

/// `Labels` 里 `com.microsoft.wsl.container.metadata` 描述的端口。
///
/// 比 `Ports` 字符串更精确：带 `VmPort`（WSL 虚拟机内部转发端口）。
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct WslPortSpec {
    /// 宿主机绑定地址。
    #[serde(rename = "BindingAddress", default)]
    pub binding_address: String,
    /// 容器端口。
    #[serde(rename = "ContainerPort", default)]
    pub container_port: u16,
    /// 地址族（2 = IPv4）。
    #[serde(rename = "Family", default)]
    pub family: u8,
    /// 宿主机端口。
    #[serde(rename = "HostPort", default)]
    pub host_port: u16,
    /// 协议号：**6 = TCP，17 = UDP**。
    #[serde(rename = "Protocol", default)]
    pub protocol: u8,
    /// WSL 虚拟机内的转发端口。
    #[serde(rename = "VmPort", default)]
    pub vm_port: u16,
}

impl WslPortSpec {
    /// 协议名。
    pub fn protocol_name(&self) -> &'static str {
        match self.protocol {
            6 => "tcp",
            17 => "udp",
            _ => "?",
        }
    }
}

/// 从 `Labels` 字符串里抽出 WSL 容器元数据中的端口列表。
///
/// 输入形如：
/// ```text
/// com.microsoft.wsl.container.metadata={"V1":{"Flags":0,...,"Ports":[{...}],"Volumes":[]}}
/// ```
pub fn parse_wsl_metadata(labels: &str) -> Option<Vec<WslPortSpec>> {
    const KEY: &str = "com.microsoft.wsl.container.metadata";

    for part in split_outside_brackets(labels, ',') {
        // 注意：不能在这里用 `?`，否则遇到任意一个不含 `=` 的分片就会整体放弃。
        let Some((k, v)) = part.split_once('=') else {
            continue;
        };
        if k.trim() != KEY {
            continue;
        }
        let parsed: Value = serde_json::from_str(v.trim()).ok()?;
        let ports = parsed.get("V1")?.get("Ports")?;
        return serde_json::from_value(ports.clone()).ok();
    }
    None
}

// ---------------------------------------------------------------------------
// 实时统计
// ---------------------------------------------------------------------------

/// `wslc stats --format json` 的一行。
///
/// ⚠️ `ID` 是 **64 位全 ID**，而 `list` 默认给 12 位短 ID，
/// 两者必须按**前缀**匹配。
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct ContainerStats {
    /// 64 位全 ID。
    #[serde(rename = "ID", default)]
    pub id: String,
    /// 容器名。
    #[serde(rename = "Name", default)]
    pub name: String,
    /// `0.00%`
    #[serde(rename = "CPUPerc", default)]
    pub cpu_perc: String,
    /// `0.02%`
    #[serde(rename = "MemPerc", default)]
    pub mem_perc: String,
    /// `3.465MiB / 15.48GiB`
    #[serde(rename = "MemUsage", default)]
    pub mem_usage: String,
    /// `1.04kB / 0B`（收 / 发）
    #[serde(rename = "NetIO", default)]
    pub net_io: String,
    /// `1.51MB / 0B`（读 / 写）
    #[serde(rename = "BlockIO", default)]
    pub block_io: String,
    /// ⚠️ 这是**数字**，不是字符串。
    #[serde(rename = "PIDs", default)]
    pub pids: u32,
}

impl ContainerStats {
    /// CPU 占用百分比。
    pub fn cpu_percent(&self) -> Option<f64> {
        parse_percent(&self.cpu_perc)
    }

    /// 内存占用百分比。
    pub fn mem_percent(&self) -> Option<f64> {
        parse_percent(&self.mem_perc)
    }

    /// `(已用, 上限)`
    pub fn mem_usage_parts(&self) -> Option<(&str, &str)> {
        split_pair(&self.mem_usage)
    }

    /// `(接收, 发送)`
    pub fn net_io_parts(&self) -> Option<(&str, &str)> {
        split_pair(&self.net_io)
    }

    /// `(读, 写)`
    pub fn block_io_parts(&self) -> Option<(&str, &str)> {
        split_pair(&self.block_io)
    }

    /// 内存使用率（0.0–1.0），用于进度条；上限为 `0B` 时返回 `None`。
    pub fn mem_ratio(&self) -> Option<f64> {
        let (used, limit) = self.mem_usage_parts()?;
        let used = parse_size(used)?;
        let limit = parse_size(limit)?;
        if limit <= 0.0 {
            return None;
        }
        Some((used / limit).clamp(0.0, 1.0))
    }

    /// 判断这条统计是否属于给定的容器 ID（**前缀匹配**）。
    pub fn matches_id(&self, id: &str) -> bool {
        if id.is_empty() || self.id.is_empty() {
            return false;
        }
        self.id.starts_with(id) || id.starts_with(&self.id)
    }
}

// ---------------------------------------------------------------------------
// 列表 + 统计 合并视图
// ---------------------------------------------------------------------------

/// UI 直接消费的合并视图：`list` 的静态信息 + `stats` 的实时指标。
#[derive(Debug, Clone, PartialEq)]
pub struct ContainerSummary {
    /// `list` 的静态信息。
    pub item: ContainerListItem,
    /// 实时统计；容器未运行时为 `None`。
    pub stats: Option<ContainerStats>,
}

impl ContainerSummary {
    /// 把 `list` 与 `stats` 按 ID 前缀配对。
    ///
    /// `stats` 里没有对应项的容器，`stats` 字段为 `None`。
    pub fn merge(items: Vec<ContainerListItem>, stats: Vec<ContainerStats>) -> Vec<Self> {
        let mut stats = stats;
        items
            .into_iter()
            .map(|item| {
                let idx = stats.iter().position(|s| {
                    s.matches_id(item.id())
                        || (!item.display_name().is_empty() && s.name == item.display_name())
                });
                let matched = idx.map(|i| stats.remove(i));
                Self { item, stats: matched }
            })
            .collect()
    }

    /// 容器名。
    pub fn name(&self) -> &str {
        self.item.display_name()
    }

    /// 是否运行中。
    pub fn is_running(&self) -> bool {
        self.item.is_running()
    }
}

// ---------------------------------------------------------------------------
// inspect
// ---------------------------------------------------------------------------

/// `wslc inspect <id>` 的结果。
///
/// **刻意不把字段全部定死**：`wslc` 的 inspect schema 未文档化，
/// 未来版本可能增删字段。这里保留完整原始 JSON（供"原始 JSON"面板展示），
/// 同时提供常用的类型化访问器。这样 schema 漂移只会让某个访问器返回 `None`，
/// **不会让整个页面崩掉**。
#[derive(Debug, Clone, PartialEq)]
pub struct ContainerInspect {
    /// `wslc inspect` 返回的原始对象。
    pub raw: Value,
}

impl ContainerInspect {
    /// 用原始 JSON 构造。
    pub fn new(raw: Value) -> Self {
        Self { raw }
    }

    /// 从 `wslc inspect` 的输出了解析（输入是 JSON 数组，取第一个元素）。
    pub fn from_array(mut values: Vec<Value>) -> Option<Self> {
        if values.is_empty() {
            return None;
        }
        Some(Self::new(values.remove(0)))
    }

    fn str_at(&self, path: &[&str]) -> Option<&str> {
        self.get(path)?.as_str()
    }

    fn get(&self, path: &[&str]) -> Option<&Value> {
        let mut cur = &self.raw;
        for key in path {
            cur = cur.get(*key)?;
        }
        Some(cur)
    }

    /// 64 位容器 ID。
    pub fn id(&self) -> Option<&str> {
        self.str_at(&["Id"])
    }

    /// 12 位短 ID。
    pub fn short_id(&self) -> Option<&str> {
        self.id().map(|s| &s[..s.len().min(12)])
    }

    /// 容器名，**已去掉 `inspect` 特有的前导 `/`**。
    pub fn name(&self) -> Option<&str> {
        self.str_at(&["Name"]).map(|s| s.strip_prefix('/').unwrap_or(s))
    }

    /// 镜像引用名（`Config.Image`，如 `alpine:latest`）。
    pub fn image_ref(&self) -> Option<&str> {
        self.str_at(&["Config", "Image"])
    }

    /// 镜像摘要（顶层 `Image`，如 `sha256:...`）。
    pub fn image_digest(&self) -> Option<&str> {
        self.str_at(&["Image"])
    }

    /// 创建时间（RFC3339 纳秒）。
    pub fn created(&self) -> Option<&str> {
        self.str_at(&["Created"])
    }

    /// `State.Status`。
    pub fn state_status(&self) -> Option<&str> {
        self.str_at(&["State", "Status"])
    }

    /// 是否运行中。
    pub fn is_running(&self) -> bool {
        self.get(&["State", "Running"])
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    /// 启动时间；**是零值时间时返回 `None`**。
    pub fn started_at(&self) -> Option<&str> {
        self.non_zero_time(&["State", "StartedAt"])
    }

    /// 结束时间；**是零值时间时返回 `None`**。
    pub fn finished_at(&self) -> Option<&str> {
        self.non_zero_time(&["State", "FinishedAt"])
    }

    /// 退出码。
    pub fn exit_code(&self) -> Option<i64> {
        self.get(&["State", "ExitCode"]).and_then(Value::as_i64)
    }

    /// 零值时间判定：`0001-01-01T00:00:00Z` 表示"从未发生"。
    fn non_zero_time(&self, path: &[&str]) -> Option<&str> {
        let s = self.str_at(path)?;
        if s.starts_with("0001-01-01") {
            None
        } else {
            Some(s)
        }
    }

    /// 环境变量。
    pub fn env(&self) -> Vec<String> {
        self.get(&["Config", "Env"])
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 命令数组。
    pub fn cmd(&self) -> Vec<String> {
        self.string_array(&["Config", "Cmd"])
    }

    /// entrypoint 数组。
    pub fn entrypoint(&self) -> Vec<String> {
        self.string_array(&["Config", "Entrypoint"])
    }

    /// 工作目录。
    pub fn working_dir(&self) -> Option<&str> {
        self.str_at(&["Config", "WorkingDir"])
    }

    /// 运行用户。
    pub fn user(&self) -> Option<&str> {
        self.str_at(&["Config", "User"])
    }

    /// 网络模式。
    pub fn network_mode(&self) -> Option<&str> {
        self.str_at(&["HostConfig", "NetworkMode"])
    }

    /// 内存上限（字节）；`0` 表示未限制。
    pub fn memory_limit(&self) -> Option<i64> {
        self.get(&["HostConfig", "Memory"]).and_then(Value::as_i64)
    }

    /// CPU 上限（纳 CPU）；`0` 表示未限制。
    pub fn nano_cpus(&self) -> Option<i64> {
        self.get(&["HostConfig", "NanoCpus"]).and_then(Value::as_i64)
    }

    /// 根文件系统占用（需要 `-s`）。
    pub fn size_root_fs(&self) -> Option<i64> {
        self.get(&["SizeRootFs"]).and_then(Value::as_i64)
    }

    /// 可写层占用（需要 `-s`）。
    pub fn size_rw(&self) -> Option<i64> {
        self.get(&["SizeRw"]).and_then(Value::as_i64)
    }

    /// 端口映射表：`"80/tcp" -> [host:port, ...]`。
    pub fn ports(&self) -> Vec<(String, Vec<(String, String)>)> {
        let Some(map) = self.get(&["Ports"]).and_then(Value::as_object) else {
            return Vec::new();
        };
        map.iter()
            .map(|(k, v)| {
                let bindings = v
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|b| {
                                let ip = b.get("HostIp")?.as_str()?.to_owned();
                                let port = b.get("HostPort")?.as_str()?.to_owned();
                                Some((ip, port))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                (k.clone(), bindings)
            })
            .collect()
    }

    /// 网络端点：`(网络名, IP, 网关, MAC)`。
    pub fn networks(&self) -> Vec<(String, String, String, String)> {
        let Some(map) = self
            .get(&["NetworkSettings", "Networks"])
            .and_then(Value::as_object)
        else {
            return Vec::new();
        };
        map.iter()
            .map(|(name, v)| {
                let get = |k: &str| {
                    v.get(k)
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned()
                };
                (
                    name.clone(),
                    get("IPAddress"),
                    get("Gateway"),
                    get("MacAddress"),
                )
            })
            .collect()
    }

    /// 挂载点数量。
    pub fn mount_count(&self) -> usize {
        self.get(&["Mounts"])
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0)
    }

    fn string_array(&self, path: &[&str]) -> Vec<String> {
        self.get(path)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_running() -> &'static str {
        include_str!("../../tests/fixtures/container_list.jsonl").trim()
    }

    fn sample_exited() -> &'static str {
        include_str!("../../tests/fixtures/container_list_stopped.jsonl").trim()
    }

    #[test]
    fn parses_real_running_container_line() {
        let item: ContainerListItem = serde_json::from_str(sample_running()).unwrap();
        assert_eq!(item.id, "ff0667ee90fb");
        assert_eq!(item.names, "wslc-panel-probe");
        assert_eq!(item.image, "docker.1ms.run/library/alpine:latest");
        assert_eq!(item.state, "running");
        assert!(item.is_running());
        assert_eq!(item.state_kind(), ContainerState::Running);
        assert_eq!(item.local_volumes, "0");
        assert_eq!(item.platform.as_ref().unwrap().architecture, "amd64");
    }

    #[test]
    fn command_strips_literal_quotes() {
        let item: ContainerListItem = serde_json::from_str(sample_running()).unwrap();
        // 原始 JSON 里是 "\"sleep 300\""
        assert_eq!(item.command_clean(), "sleep 300");
    }

    #[test]
    fn parses_real_exited_container_line() {
        let item: ContainerListItem = serde_json::from_str(sample_exited()).unwrap();
        assert_eq!(item.state, "exited");
        assert!(!item.is_running());
        assert_eq!(item.state_kind(), ContainerState::Exited);
        assert!(item.ports.is_empty());
        assert_eq!(item.status, "Exited (137) 2 seconds ago");
    }

    #[test]
    fn parses_port_mappings() {
        let item: ContainerListItem = serde_json::from_str(sample_running()).unwrap();
        let ports = item.port_mappings();
        assert_eq!(ports.len(), 1);
        assert_eq!(ports[0].host_ip, "127.0.0.1");
        assert_eq!(ports[0].host_port, Some(18080));
        assert_eq!(ports[0].container_port, 80);
        assert_eq!(ports[0].protocol, "tcp");
        assert_eq!(ports[0].display(), "127.0.0.1:18080 → 80/tcp");
    }

    #[test]
    fn port_parse_accepts_short_form() {
        // `8080->80/tcp`（省掉主机地址的简写）：纯数字的宿主段是**端口**，不是 IP。
        // 这条断言也是 CI 抓出来的 —— 原实现把 "8080" 当成了 host_ip。
        let p = PortMapping::parse("8080->80/tcp").unwrap();
        assert_eq!(p.host_ip, "0.0.0.0");
        assert_eq!(p.host_port, Some(8080));
        assert_eq!(p.container_port, 80);
        assert_eq!(p.protocol, "tcp");
    }

    #[test]
    fn port_parse_handles_container_port_only() {
        // 只有容器端口时（未发布），宿主侧应为通配地址且无端口。
        let p = PortMapping::parse("80/tcp").unwrap();
        assert_eq!(p.host_ip, "0.0.0.0");
        assert_eq!(p.host_port, None);
        assert_eq!(p.container_port, 80);
    }

    #[test]
    fn extracts_wsl_metadata_ports_from_labels() {
        let item: ContainerListItem = serde_json::from_str(sample_running()).unwrap();
        let ports = item.wsl_metadata_ports();
        assert_eq!(ports.len(), 1);
        assert_eq!(ports[0].host_port, 18080);
        assert_eq!(ports[0].container_port, 80);
        assert_eq!(ports[0].vm_port, 20002);
        assert_eq!(ports[0].protocol_name(), "tcp");
        assert_eq!(ports[0].binding_address, "127.0.0.1");
    }

    #[test]
    fn label_pairs_survive_nested_json() {
        let item: ContainerListItem = serde_json::from_str(sample_running()).unwrap();
        let pairs = item.label_pairs();
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0, "com.microsoft.wsl.container.metadata");
        assert!(pairs[0].1.starts_with('{'));
    }

    #[test]
    fn parses_real_stats_line() {
        let text = include_str!("../../tests/fixtures/container_stats.jsonl");
        let stats: ContainerStats = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(stats.name, "wslc-panel-probe");
        assert_eq!(stats.pids, 1);
        assert_eq!(stats.cpu_percent(), Some(0.0));
        assert_eq!(stats.mem_percent(), Some(0.02));
        assert_eq!(stats.mem_usage_parts(), Some(("3.465MiB", "15.48GiB")));
        let ratio = stats.mem_ratio().unwrap();
        assert!(ratio > 0.0 && ratio < 0.01);
    }

    #[test]
    fn stats_id_is_full_64_chars_and_matches_short_id_by_prefix() {
        let text = include_str!("../../tests/fixtures/container_stats.jsonl");
        let stats: ContainerStats = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(stats.id.len(), 64);
        assert!(stats.matches_id("ff0667ee90fb"));
        assert!(stats.matches_id(&stats.id));
        assert!(!stats.matches_id("deadbeef"));
    }

    #[test]
    fn stats_all_fixture_reports_zero_for_exited() {
        let text = include_str!("../../tests/fixtures/container_stats_all.jsonl");
        let stats: ContainerStats = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(stats.pids, 0);
        assert_eq!(stats.mem_usage_parts(), Some(("0B", "0B")));
        // 上限为 0B 时不应算出比例（避免除零）。
        assert_eq!(stats.mem_ratio(), None);
    }

    #[test]
    fn merges_list_and_stats_by_prefix() {
        let item: ContainerListItem = serde_json::from_str(sample_running()).unwrap();
        let stats: ContainerStats =
            serde_json::from_str(include_str!("../../tests/fixtures/container_stats.jsonl").trim())
                .unwrap();
        let merged = ContainerSummary::merge(vec![item], vec![stats.clone()]);
        assert_eq!(merged.len(), 1);
        assert!(merged[0].stats.is_some());
        assert_eq!(merged[0].stats.as_ref().unwrap().pids, 1);
        assert_eq!(merged[0].name(), "wslc-panel-probe");
    }

    #[test]
    fn merge_leaves_unmatched_items_without_stats() {
        let item: ContainerListItem = serde_json::from_str(sample_running()).unwrap();
        let merged = ContainerSummary::merge(vec![item], vec![]);
        assert!(merged[0].stats.is_none());
    }

    #[test]
    fn parses_real_inspect_output() {
        let text = include_str!("../../tests/fixtures/container_inspect.json");
        let values: Vec<Value> = serde_json::from_str(text).unwrap();
        let insp = ContainerInspect::from_array(values).unwrap();

        assert_eq!(insp.short_id(), Some("ff0667ee90fb"));
        // inspect 的 Name 带前导 `/`，访问器必须去掉。
        assert_eq!(insp.name(), Some("wslc-panel-probe"));
        assert_eq!(insp.image_ref(), Some("docker.1ms.run/library/alpine:latest"));
        assert_eq!(insp.state_status(), Some("running"));
        assert!(insp.is_running());
        assert_eq!(insp.cmd(), vec!["sleep", "300"]);
        assert_eq!(insp.working_dir(), Some("/"));
        assert_eq!(insp.network_mode(), Some("bridge"));
        assert_eq!(insp.memory_limit(), Some(0));
        assert_eq!(insp.env().len(), 2);
        assert_eq!(insp.mount_count(), 0);

        let nets = insp.networks();
        assert_eq!(nets.len(), 1);
        assert_eq!(nets[0].0, "bridge");
        assert_eq!(nets[0].1, "172.17.0.2");

        let ports = insp.ports();
        assert_eq!(ports.len(), 1);
        assert_eq!(ports[0].0, "80/tcp");
        assert_eq!(ports[0].1[0], ("127.0.0.1".to_owned(), "18080".to_owned()));
    }

    #[test]
    fn zero_times_are_reported_as_none() {
        let text = include_str!("../../tests/fixtures/container_inspect.json");
        let values: Vec<Value> = serde_json::from_str(text).unwrap();
        let insp = ContainerInspect::from_array(values).unwrap();
        // 运行中的容器 StartedAt 有效、FinishedAt 是零值。
        assert!(insp.started_at().is_some());
        assert_eq!(insp.finished_at(), None);
    }

    #[test]
    fn inspect_accessors_return_none_instead_of_panicking_on_missing_fields() {
        let insp = ContainerInspect::new(serde_json::json!({}));
        assert_eq!(insp.id(), None);
        assert_eq!(insp.short_id(), None);
        assert_eq!(insp.name(), None);
        assert_eq!(insp.image_ref(), None);
        assert!(!insp.is_running());
        assert!(insp.env().is_empty());
        assert!(insp.ports().is_empty());
        assert!(insp.networks().is_empty());
        assert_eq!(insp.mount_count(), 0);
        assert_eq!(insp.exit_code(), None);
    }

    #[test]
    fn inspect_from_empty_array_is_none() {
        assert!(ContainerInspect::from_array(vec![]).is_none());
    }

    #[test]
    fn inspect_with_size_fields() {
        let text = include_str!("../../tests/fixtures/container_inspect_s.json");
        let values: Vec<Value> = serde_json::from_str(text).unwrap();
        let insp = ContainerInspect::from_array(values).unwrap();
        assert_eq!(insp.size_root_fs(), Some(8_422_040));
        assert_eq!(insp.size_rw(), Some(0));
    }

    #[test]
    fn container_state_parsing() {
        assert_eq!(ContainerState::parse("running"), ContainerState::Running);
        assert_eq!(ContainerState::parse("EXITED"), ContainerState::Exited);
        assert_eq!(ContainerState::parse("created"), ContainerState::Created);
        assert_eq!(
            ContainerState::parse("weird"),
            ContainerState::Unknown("weird".into())
        );
        assert!(ContainerState::Running.is_active());
        assert!(!ContainerState::Exited.is_active());
        assert_eq!(ContainerState::Running.label(), "运行中");
    }
}
