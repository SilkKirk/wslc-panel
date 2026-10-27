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
    ///
    /// ⚠️ 实现在 [`crate::util::short_prefix`]（用它，别在这儿手写）：
    /// `get(..n)` 而不是 `&self.id[..n]` —— `len()` 是**字节**数，
    /// 第 12 个字节正好落在多字节字符中间时，切片会直接 panic。
    /// 今天 `wslc` 给的是十六进制 ID，所以碰不到 —— 但 `id` 是
    /// `#[serde(default)]` 的自由字符串，被包装脚本改写、或某台机器上
    /// 混进一行畸形 JSON 就会碰到，而这一层的设计目标写着"解析失败
    /// 绝不能 panic"（`jsonl` 好不容易把坏行挡住了，一个 panic
    /// 会把整个界面拖掉）。`get` 落到字符边界之外时退回整串，
    /// 比"少显示几位"更诚实。
    ///
    /// 镜像 / 网络两个模型要的是逐字相同的一句话，所以那段理由现在只写在
    /// `short_prefix` 一处 —— 四份抄下来之后只有这一份带着解释。
    pub fn short_id(&self) -> &str {
        crate::util::short_prefix(&self.id, 12)
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

    /// 挂载列表（来自 `Labels` 的元数据，给的是用户认得的主机路径）。
    pub fn mounts(&self) -> Vec<WslVolumeSpec> {
        parse_wsl_volumes(&self.labels).unwrap_or_default()
    }

    /// 挂载的可读摘要：`主机 → 容器`，多个用 `; ` 连接。
    ///
    /// 没有挂载时返回 `None`（界面上显示 `—`）。
    pub fn mounts_summary(&self) -> Option<String> {
        let mounts = self.mounts();
        if mounts.is_empty() {
            return None;
        }
        Some(
            mounts
                .iter()
                .map(WslVolumeSpec::summary)
                .collect::<Vec<_>>()
                .join("; "),
        )
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
            Some(p) => format!(
                "{}:{} → {}/{}",
                self.host_ip, p, self.container_port, self.protocol
            ),
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
    let parsed = wsl_metadata_value(labels)?;
    let ports = parsed.get("V1")?.get("Ports")?;
    serde_json::from_value(ports.clone()).ok()
}

/// 取出 `com.microsoft.wsl.container.metadata` 的 JSON 值。
///
/// 端口和挂载都在这一份 metadata 里，所以抽出来共用。
fn wsl_metadata_value(labels: &str) -> Option<Value> {
    const KEY: &str = "com.microsoft.wsl.container.metadata";

    for part in split_outside_brackets(labels, ',') {
        // 注意：不能在这里用 `?`，否则遇到任意一个不含 `=` 的分片就会整体放弃。
        let Some((k, v)) = part.split_once('=') else {
            continue;
        };
        if k.trim() != KEY {
            continue;
        }
        return serde_json::from_str(v.trim()).ok();
    }
    None
}

/// `Labels` 里 WSL 元数据描述的**挂载**。
///
/// 形如：
/// ```json
/// {"ContainerPath":"/etc/nginx/conf.d/","HostPath":"E:\\code","ReadOnly":false}
/// ```
///
/// 为什么要从这里取，而不是用 `list` 的 `Mounts` 字段：
/// `Mounts` 给的是 VM 内部路径（形如 `/mnt/{078dfade-...}`），
/// 对用户**没有意义** —— 用户认得的是自己填的 `E:\code`。
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct WslVolumeSpec {
    /// 主机路径（bind 挂载的源）。
    #[serde(rename = "HostPath", default)]
    pub host_path: String,
    /// 容器内路径（挂载目标）。
    #[serde(rename = "ContainerPath", default)]
    pub container_path: String,
    /// 是否只读。
    #[serde(rename = "ReadOnly", default)]
    pub read_only: bool,
}

impl WslVolumeSpec {
    /// `主机 → 容器`（只读时带标记）。
    pub fn summary(&self) -> String {
        let suffix = if self.read_only { "（只读）" } else { "" };
        format!("{} → {}{}", self.host_path, self.container_path, suffix)
    }
}

/// 从 `Labels` 解析挂载列表。
pub fn parse_wsl_volumes(labels: &str) -> Option<Vec<WslVolumeSpec>> {
    let parsed = wsl_metadata_value(labels)?;
    let volumes = parsed.get("V1")?.get("Volumes")?;
    serde_json::from_value(volumes.clone()).ok()
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
                Self {
                    item,
                    stats: matched,
                }
            })
            .collect()
    }

    /// 容器名。
    pub fn name(&self) -> &str {
        self.item.display_name()
    }

    /// 挂载摘要（`主机 → 容器`），委托给 `list` 的静态信息。
    pub fn mounts_summary(&self) -> Option<String> {
        self.item.mounts_summary()
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
    ///
    /// ⚠️ 同 [`ContainerListItem::short_id`]：走 [`crate::util::short_prefix`]
    /// （`get(..n)` 而不是切片，避免第 12 个字节踩在多字节字符中间时 panic）。
    pub fn short_id(&self) -> Option<&str> {
        self.id().map(|s| crate::util::short_prefix(s, 12))
    }

    /// 容器名，**已去掉 `inspect` 特有的前导 `/`**。
    pub fn name(&self) -> Option<&str> {
        self.str_at(&["Name"])
            .map(|s| s.strip_prefix('/').unwrap_or(s))
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

    /// 启动命令的可读形式：`entrypoint… + cmd…`。
    ///
    /// # 为什么把两段接起来显示
    ///
    /// Docker / WSL 把启动命令拆成 `Entrypoint` 与 `Cmd` **两段**，运行时接起来执行。
    /// 只显示一段会误导 —— 实测那个 `eclipse-temurin:8-jre-alpine` 就是
    /// **只有 entrypoint、没有 cmd**（它的 entrypoint 末行是 `exec "$@"`，
    /// 于是不带参数时什么都不做、容器 1.1 ms 就正常退出）；只看 entrypoint
    /// 会读成"这镜像有命令、能跑"。
    ///
    /// 两段都空时返回 `None`（界面显示 `—`）。
    pub fn command_line(&self) -> Option<String> {
        let mut parts = self.entrypoint();
        parts.extend(self.cmd());
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(" "))
        }
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
        self.get(&["HostConfig", "NanoCpus"])
            .and_then(Value::as_i64)
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

    /// 挂载点列表（`inspect` 的 `Mounts`）。
    ///
    /// ⚠️ **别和 [`ContainerListItem::mounts`] 搞混**：两者字段名一样、
    /// 含义完全不同。列表那个 `Mounts` 是一个**字符串**，给的是 VM 里的路径
    /// （形如 `/mnt/{078dfade-…}`，见那条的说明）；`inspect` 的这个才是
    /// "哪个宿主的卷挂在容器的哪个目录"。
    ///
    /// 「部署应用」靠它推 nginx 的 Web 根（`model::deploy::nginx_web_root`）：
    /// 本机那个容器实测给出的是 `{Type: volume, Name: webdata,
    /// Destination: /usr/share/nginx/html}`。
    ///
    /// 缺字段 / 形状不对的行**跳过**，不报错也不 panic —— 和这个结构体其他
    /// 访问器同一条原则：schema 漂移只让某一项变空。
    pub fn mounts(&self) -> Vec<ContainerMount> {
        self.get(&["Mounts"])
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| {
                        let destination = m.get("Destination")?.as_str()?.to_owned();
                        // `Source` 是**卷名**（volume）或**宿主路径**（bind）；
                        // 有些驱动两个都不给，那就空着，界面显示成 `—`。
                        let source = m
                            .get("Source")
                            .or_else(|| m.get("Name"))
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned();
                        let kind = m
                            .get("Type")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned();
                        Some(ContainerMount {
                            kind,
                            source,
                            destination,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 挂载的可读摘要（`卷名/宿主路径 → 容器内路径`），**一条一行**。
    ///
    /// 用换行而不是 `; `：界面那边用 `kv_block` 画，长路径换行显示不会被截断。
    /// 没有挂载时返回 `None`（界面显示 `—`）。
    ///
    /// ⚠️ **这才是挂载的权威来源**。列表那一份（[`ContainerListItem::mounts`]）
    /// 走的是 `Labels` 里的 `com.microsoft.wsl.container.metadata` → `V1.Volumes`，
    /// 而实测（本机 WSL 3.0.3.0，2026-10）那个数组**是空的**：
    ///
    /// ```text
    /// mysql 容器：list.Mounts = "mysqldata"（只有卷名、没有容器内路径）
    ///             Labels 里的 V1.Volumes = []
    ///             inspect.Mounts = [{Type: volume, Name: mysqldata,
    ///                                Destination: /var/lib/mysql}]
    /// ```
    ///
    /// 所以详情里那一行**必须**用 inspect 的数据：只靠列表 / labels 的话，
    /// 挂了卷的容器也会显示成 `—`（v0.9.7 实机反馈："点开容器详情看不到挂载"）。
    pub fn mounts_summary(&self) -> Option<String> {
        let mounts = self.mounts();
        if mounts.is_empty() {
            return None;
        }
        Some(
            mounts
                .iter()
                .map(ContainerMount::summary)
                .collect::<Vec<_>>()
                .join("\n"),
        )
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

/// `inspect` 里的一个挂载点（`Mounts[]`）。
///
/// 字段刻意都是 `String` 而不是枚举：`wslc` 的 `Type` 值没有文档
/// （实测见过 `volume`），拿它做穷尽匹配等于把未文档化的东西钉死。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerMount {
    /// 类型：实测见过 `volume`（`bind` / `tmpfs` 是 Docker 那边常见的值）。
    pub kind: String,
    /// 卷名（`volume`）或宿主路径（`bind`）；两个都没有时是空串。
    pub source: String,
    /// 容器内路径。
    pub destination: String,
}

impl ContainerMount {
    /// `卷名 → /容器内路径`（源拿不到时就只给路径）。
    ///
    /// 形制和列表那份 [`WslVolumeSpec::summary`] 保持一致（`A → B`）——
    /// 同一个界面里两种写法会让人以为它们不是一回事。
    pub fn summary(&self) -> String {
        if self.source.is_empty() {
            self.destination.clone()
        } else {
            format!("{} → {}", self.source, self.destination)
        }
    }
}

/// 从镜像引用推一个**看得懂**的容器名（表单里"容器名"留空时用）。
///
/// # 为什么不让 `wslc` 自己起名
///
/// 留空时 `wslc`（和 Docker 一样）会生成 `shimmering_karakoram` 这种随机名 ——
/// 列表里**看不出这个容器是哪个镜像起的**（实机反馈）。这里改成"镜像名 + 序号"：
/// 第一个 `nginx`、第二个 `nginx-2`。
///
/// 规则刻意简单到可以预期：
///
/// - 取镜像引用的**最后一段**：`docker.1panel.live/library/nginx:latest` → `nginx`；
///   `grafana/grafana:11` → `grafana`；`alpine@sha256:…` → `alpine`；
///   `localhost:5000/nginx` → `nginx`（端口在主机那一段里，不会被当成 tag）；
/// - 清洗成 Docker 允许的容器名字符（`[a-zA-Z0-9][a-zA-Z0-9_.-]*`），
///   非法的换成 `-`，开头的非字母数字去掉；
/// - 和现有容器重名就往后加序号（`-2`、`-3`……）。`existing` 要传**全部**容器，
///   不只是运行中的 —— 容器名在所有容器之间必须唯一；
/// - 推不出来（镜像名是空的 / 全被占满）就返回**空串**，调用方据此不传 `--name`
///   （把起名权还给 `wslc`）。
pub fn suggest_container_name(image: &str, existing: &[String]) -> String {
    let base = name_stem(image);
    if base.is_empty() {
        return String::new();
    }
    if !existing.iter().any(|used| used == &base) {
        return base;
    }

    (2..=999)
        .map(|n| format!("{base}-{n}"))
        .find(|candidate| !existing.iter().any(|used| used == candidate))
        .unwrap_or_default()
}

/// 取镜像引用的"最后一段"，清洗成合法的容器名。
fn name_stem(image: &str) -> String {
    // digest 先切掉（`alpine@sha256:…`），它和 tag 不同时出现，但都可能有
    let without_digest = image.split('@').next().unwrap_or(image);
    let last = without_digest.rsplit('/').next().unwrap_or(without_digest);
    // tag / 端口：只取冒号**之前**那段
    let last = last.split(':').next().unwrap_or(last);

    let cleaned: String = last
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();

    // Docker 的容器名必须以字母数字开头
    cleaned
        .trim_start_matches(|c: char| !c.is_ascii_alphanumeric())
        .to_owned()
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

    /// 真实的 `Labels`（实机抓的）—— 端口和挂载都在同一段 metadata 里。
    const REAL_LABELS: &str = concat!(
        r#"com.microsoft.wsl.container.metadata={"V1":{"Flags":0,"InitProcessFlags":0,"#,
        r#""Ports":[{"BindingAddress":"127.0.0.1","ContainerPort":80,"Family":2,"#,
        r#""HostPort":80,"Protocol":6,"VmPort":20002}],"#,
        r#""Volumes":[{"ContainerPath":"/etc/nginx/conf.d/","CreateSourceIfMissing":true,"#,
        r#""HostPath":"E:\\code","ParentVMPath":"/mnt/{078dfade-e629-4e05-a2ce-8406c9b90422}","#,
        r#""ReadOnly":false,"SourceFilename":""}]}},"#,
        r#"maintainer=NGINX Docker Maintainers <docker-maint@nginx.com>"#,
    );

    #[test]
    fn parse_wsl_volumes_reads_host_and_container_paths() {
        let volumes = parse_wsl_volumes(REAL_LABELS).expect("应能解析出挂载");
        assert_eq!(volumes.len(), 1);
        assert_eq!(volumes[0].host_path, r"E:\code");
        assert_eq!(volumes[0].container_path, "/etc/nginx/conf.d/");
        assert!(!volumes[0].read_only);
        assert_eq!(volumes[0].summary(), r"E:\code → /etc/nginx/conf.d/");
    }

    #[test]
    fn parse_wsl_volumes_and_ports_share_the_same_metadata() {
        // 同一个 Labels 里两样都能解析出来 —— 这是把 metadata 取值抽成
        // `wsl_metadata_value` 的原因。
        let ports = parse_wsl_metadata(REAL_LABELS).expect("应能解析出端口");
        assert_eq!(ports.len(), 1);
        assert_eq!(ports[0].host_port, 80);

        let volumes = parse_wsl_volumes(REAL_LABELS).expect("应能解析出挂载");
        assert_eq!(volumes.len(), 1);
    }

    #[test]
    fn parse_wsl_volumes_returns_none_without_metadata() {
        assert!(parse_wsl_volumes("").is_none());
        assert!(parse_wsl_volumes("maintainer=someone").is_none());
        // metadata 在，但没有 Volumes 字段
        assert!(parse_wsl_volumes(r#"com.microsoft.wsl.container.metadata={"V1":{}}"#).is_none());
    }

    #[test]
    fn parse_wsl_volumes_handles_empty_list() {
        let labels = r#"com.microsoft.wsl.container.metadata={"V1":{"Volumes":[]}}"#;
        let volumes = parse_wsl_volumes(labels).expect("空列表也应能解析");
        assert!(volumes.is_empty());
    }

    #[test]
    fn read_only_volume_is_marked_in_summary() {
        let labels = concat!(
            r#"com.microsoft.wsl.container.metadata={"V1":{"Volumes":[{"#,
            r#""HostPath":"D:\\data","ContainerPath":"/data","ReadOnly":true}]}}"#,
        );
        let volumes = parse_wsl_volumes(labels).unwrap();
        assert!(volumes[0].read_only);
        assert_eq!(volumes[0].summary(), r"D:\data → /data（只读）");
    }

    #[test]
    fn mounts_summary_is_none_when_no_mounts() {
        let item = ContainerListItem::default();
        assert!(item.mounts_summary().is_none());
        assert!(item.mounts().is_empty());
    }

    #[test]
    fn mounts_summary_joins_multiple_mounts() {
        let labels = concat!(
            r#"com.microsoft.wsl.container.metadata={"V1":{"Volumes":["#,
            r#"{"HostPath":"C:\\a","ContainerPath":"/a","ReadOnly":false},"#,
            r#"{"HostPath":"C:\\b","ContainerPath":"/b","ReadOnly":false}]}}"#,
        );
        let item = ContainerListItem {
            labels: labels.to_owned(),
            ..Default::default()
        };
        assert_eq!(item.mounts_summary().unwrap(), r"C:\a → /a; C:\b → /b");
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
        assert_eq!(
            insp.image_ref(),
            Some("docker.1ms.run/library/alpine:latest")
        );
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

    /// 启动命令 = `Entrypoint` + `Cmd` **接起来**（运行时就是这么执行的）。
    #[test]
    fn command_line_joins_entrypoint_and_cmd() {
        let both = ContainerInspect::new(serde_json::json!({
            "Config": { "Entrypoint": ["docker-entrypoint.sh"], "Cmd": ["mysqld"] }
        }));
        assert_eq!(
            both.command_line().as_deref(),
            Some("docker-entrypoint.sh mysqld")
        );

        // 只有 entrypoint（那个 temurin 镜像的形状）：**原样给出来** ——
        // 不补个 `—`、也不假装它还有 cmd。这一条正是"镜像没 CMD、起来就退"那次事故的教训：
        // 只读 entrypoint 的人会以为"这镜像有命令、能跑"。
        let only_entry = ContainerInspect::new(serde_json::json!({
            "Config": { "Entrypoint": ["/__cacert_entrypoint.sh"], "Cmd": [] }
        }));
        assert_eq!(
            only_entry.command_line().as_deref(),
            Some("/__cacert_entrypoint.sh")
        );

        // 两段都空 / 整个 Config 都没有 → `None`（界面显示 `—`）
        let empty = ContainerInspect::new(serde_json::json!({
            "Config": { "Entrypoint": [], "Cmd": [] }
        }));
        assert_eq!(empty.command_line(), None);
        assert_eq!(
            ContainerInspect::new(serde_json::json!({})).command_line(),
            None
        );
    }

    /// 挂载摘要读的是 **`inspect` 的 `Mounts`**（权威），不是 list / labels。
    ///
    /// 实测（本机 WSL 3.0.3.0）：挂了 `mysqldata` 的 mysql 容器，`list.Mounts`
    /// 只有卷名、`Labels` 里的 `V1.Volumes` 是 **空数组** —— 所以只靠列表那边，
    /// 详情里的「挂载」永远是 `—`（用户就是这么报上来的）。
    #[test]
    fn inspect_mounts_summary_uses_the_authoritative_mounts_array() {
        let insp = ContainerInspect::new(serde_json::json!({
            "Mounts": [
                { "Type": "volume", "Name": "mysqldata", "Source": "mysqldata",
                  "Destination": "/var/lib/mysql" },
                { "Type": "bind", "Source": "E:\\code", "Destination": "/etc/nginx/conf.d" },
                { "Type": "volume", "Destination": "/no-source" }
            ]
        }));
        assert_eq!(
            insp.mounts_summary().as_deref(),
            Some("mysqldata → /var/lib/mysql\nE:\\code → /etc/nginx/conf.d\n/no-source")
        );
        assert_eq!(insp.mount_count(), 3);

        // 没挂载 / 没这个字段 → `None`（界面显示 `—`）
        assert_eq!(
            ContainerInspect::new(serde_json::json!({ "Mounts": [] })).mounts_summary(),
            None
        );
        assert_eq!(
            ContainerInspect::new(serde_json::json!({})).mounts_summary(),
            None
        );
    }

    /// 短 ID 截断**不能**假设 ID 是 ASCII。
    ///
    /// `len()` 是字节数：11 个 ASCII + 一个 3 字节汉字正好让第 12 个字节
    /// 落在字符中间。旧的 `&self.id[..12]` 在这里直接 panic ——
    /// 而 `id` 是解析出来的自由字符串，这一层的设计目标是不 panic。
    #[test]
    fn short_id_does_not_panic_on_a_multibyte_id() {
        let mut item: ContainerListItem =
            serde_json::from_value(serde_json::json!({ "ID": "abcdefghijk中" })).unwrap();
        // 12 不是字符边界 → 退回整串，而不是 panic、也不是切出半截乱码
        assert_eq!(item.short_id(), "abcdefghijk中");
        assert_eq!(item.display_name(), "abcdefghijk中");

        // 纯 ASCII 的照旧截到 12 位
        item.id = "ff0667ee90fb1234".to_owned();
        assert_eq!(item.short_id(), "ff0667ee90fb");

        // 不足 12 字节的原样返回
        item.id = "abc".to_owned();
        assert_eq!(item.short_id(), "abc");

        // inspect 那条路（返回 Option）同样不能 panic
        let insp = ContainerInspect::new(serde_json::json!({ "Id": "abcdefghijk中" }));
        assert_eq!(insp.short_id(), Some("abcdefghijk中"));
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

    #[test]
    fn container_name_comes_from_the_image() {
        let none: Vec<String> = Vec::new();
        // 带加速站前缀 + tag 的那种（本项目最常见的形式）
        assert_eq!(
            suggest_container_name("docker.1panel.live/library/nginx:latest", &none),
            "nginx"
        );
        // 带命名空间的
        assert_eq!(
            suggest_container_name("grafana/grafana:11", &none),
            "grafana"
        );
        // 裸名字 / 带 digest / 带端口的 registry
        assert_eq!(suggest_container_name("alpine", &none), "alpine");
        assert_eq!(
            suggest_container_name("alpine@sha256:abcdef", &none),
            "alpine"
        );
        assert_eq!(
            suggest_container_name("localhost:5000/nginx", &none),
            "nginx",
            "端口在主机那一段，不该被当成 tag 切掉"
        );
        // 镜像名里的非法字符换成 `-`，开头必须是字母数字
        assert_eq!(
            suggest_container_name("docker.1panel.live/library/my_app.v2:1", &none),
            "my_app.v2"
        );
        assert_eq!(suggest_container_name("...weird", &none), "weird");
    }

    #[test]
    fn container_name_avoids_taken_names() {
        let used = vec!["nginx".to_owned(), "nginx-2".to_owned()];
        assert_eq!(
            suggest_container_name("nginx:latest", &used),
            "nginx-3",
            "撞了就往后找第一个空位"
        );
        // 空的镜像引用推不出东西 —— 调用方据此**不传** `--name`
        assert_eq!(suggest_container_name("", &used), "");
        assert_eq!(suggest_container_name("   ", &used), "");
        assert_eq!(suggest_container_name(":latest", &used), "");
    }
}
