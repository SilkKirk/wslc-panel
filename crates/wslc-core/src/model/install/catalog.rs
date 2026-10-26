//! 在线清单：`wsl -l -o` 表格 / 商店 JSON 的解析，以及商店与镜像源共用的匹配。

use super::*;

/// 在线列表里的一项。
///
/// `id` 是**传给 `wsl --install -d` 的那个名字**（也是安装完成后的发行版名），
/// `label` 只是给人看的友好名（`Ubuntu 24.04 LTS`）。两者不一定相同，
/// 界面上都要显示 —— 用户以为自己装的是 "Ubuntu 24.04 LTS"，
/// 结果列表里出现一个 `Ubuntu-24.04`，那种困惑是可以避免的。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnlineDistro {
    /// 内部名（`wsl --install -d <id>`）。
    pub id: String,
    /// 友好名（只用于显示）。
    pub label: String,
}

impl OnlineDistro {
    /// 新建（`label` 为空时退回 `id`）。
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        let id = id.into();
        let label = label.into();
        let label = if label.trim().is_empty() {
            id.clone()
        } else {
            label
        };
        Self { id, label }
    }
}

/// 解析 `wsl --list --online` 的输出。
///
/// 输出形态（`NAME` / `FRIENDLY NAME` 两列，表头是英文，列用空格对齐）：
///
/// ```text
/// 以下是可安装的有效分发的列表：
/// NAME            FRIENDLY NAME
/// Ubuntu          Ubuntu
/// Ubuntu-24.04    Ubuntu 24.04 LTS
/// ```
///
/// # 本机实测：这条命令**时好时坏**
///
/// 本机（WSL 3.0.1.0）`wsl -l -o` 去 `raw.githubusercontent.com` 取清单，
/// 走的是 `wsl.exe` 自己的网络：
///
/// - 2026-10-10：连接被重置 / `WININET_E_TIMEOUT`，退出码 `-1`
///   （原始输出存成了 `tests/fixtures/wsl_list_online_failed.txt`）；
/// - 2026-10-12（开着系统代理）：0.5 秒成功，列出 24 个发行版。
///
/// 所以：
///
/// - **失败输出必须被安静地吃成空列表**（配 `strip_config_warnings` 一起用），
///   而不是抛一个解析错误 —— 上层会用另一个来源兜底（见 `cmd::distro::list_online`）；
/// - ⚠️ 下面测试里那条"成功形状"的样本**原本不是本机抓的**（一开始本机抓不到，
///   只有上面那份失败输出）。**后来补上了**：2026-10-10 开着 dev-sidecar
///   系统代理时抓到了真实成功输出，存成了
///   `tests/fixtures/wsl_list_online.txt`，`online_list_parses_the_two_column_table`
///   用的就是它。所以那一条现在是**实测过的**，不再是假设 ——
///   详情见那个测试自己的注释。
pub fn parse_online_list(text: &str) -> Vec<OnlineDistro> {
    let mut out = Vec::new();
    let mut header_seen = false;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // 表头之前的所有内容（提示语、`.wslconfig` 告警、错误信息）都跳过
        if !header_seen {
            if line.contains("NAME") {
                header_seen = true;
            }
            continue;
        }

        let mut parts = line.split_whitespace();
        let Some(id) = parts.next() else { continue };
        // 表格里的分隔线（`----  -------------`）和别的说明行会落到这里。
        // 只按"字符合法"过滤是不够的 —— `-` 本身就是合法的名字字符，
        // 于是 `----` 会被当成一个发行版 id（CI 上真的这么挂过一次）。
        // 真正的发行版名一定含字母或数字，加上这一条就干净了。
        if !id.chars().all(is_name_char) || !id.chars().any(|c| c.is_ascii_alphanumeric()) {
            continue;
        }
        let label = parts.collect::<Vec<_>>().join(" ");
        out.push(OnlineDistro::new(id, label));
    }

    out
}

/// 解析微软的 `DistributionInfo.json`（在线列表的**兜底来源**）。
///
/// 本机实测：`wsl -l -o` 拉不到清单（见 [`parse_online_list`]），
/// 但同一个 JSON 可以从 CDN 拿到（`cdn.jsdelivr.net/gh/microsoft/WSL@master/
/// distributions/DistributionInfo.json`，18481 字节，HTTP 200）。
/// 它的形状是：
///
/// ```json
/// { "ModernDistributions": { "Ubuntu": [ { "Name": "Ubuntu-24.04",
///   "FriendlyName": "Ubuntu 24.04 LTS", "Default": false }, ... ], ... } }
/// ```
///
/// 顺序按 `Default` 优先（`Ubuntu` 这种不带版本号的默认项排最前），
/// 其余保持 JSON 里的顺序 —— 用户最可能选的就是默认那个。
pub fn parse_distribution_info(json: &str) -> Result<Vec<OnlineDistro>, String> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("在线清单不是合法 JSON：{e}"))?;

    let groups = value
        .get("ModernDistributions")
        .and_then(|v| v.as_object())
        .ok_or_else(|| "在线清单里没有 ModernDistributions 字段".to_owned())?;

    let mut defaults: Vec<OnlineDistro> = Vec::new();
    let mut rest: Vec<OnlineDistro> = Vec::new();

    for entries in groups.values() {
        let Some(items) = entries.as_array() else {
            continue;
        };
        for item in items {
            let Some(id) = item.get("Name").and_then(|v| v.as_str()) else {
                continue;
            };
            let label = item
                .get("FriendlyName")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let is_default = item
                .get("Default")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let distro = OnlineDistro::new(id, label);
            if is_default {
                defaults.push(distro);
            } else {
                rest.push(distro);
            }
        }
    }

    if defaults.is_empty() && rest.is_empty() {
        return Err("在线清单里一个发行版都没有".to_owned());
    }
    defaults.extend(rest);
    Ok(defaults)
}

/// 在线清单是**从哪儿来的**。
///
/// 界面要如实显示它：本机 `wsl -l -o` 是坏的（见 [`parse_online_list`]），
/// 所以列表通常是"我们自己去拉的微软清单"——
/// 用户有知道"这份列表是怎么来的"的权利（他可能因此判断某条信息是否可靠）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OnlineListSource {
    /// 还没拉过。
    #[default]
    Unknown,
    /// `wsl --list --online`。
    Wsl,
    /// 微软那份 `DistributionInfo.json`（兜底来源）。
    FallbackJson,
}

impl OnlineListSource {
    /// 界面上那句"列表来自哪儿"（[`Self::Unknown`] 时是空串）。
    pub fn label(self) -> &'static str {
        match self {
            Self::Unknown => "",
            Self::Wsl => "来自 wsl --list --online",
            Self::FallbackJson => "来自微软的 DistributionInfo.json（wsl 自己拉不到）",
        }
    }
}

/// 一次"拉在线清单"的结果。
///
/// `items` 和 `error` **可能同时有值**（比如 wsl 报了错、但兜底来源拿到了列表）——
/// 那正是本机的常态，所以错误信息要留着给用户看，别因为拿到了列表就丢掉。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OnlineListing {
    /// 清单（可能为空）。
    pub items: Vec<OnlineDistro>,
    /// 失败原因（成功时为空串）。
    pub error: String,
    /// 这份清单来自哪儿。
    pub source: OnlineListSource,
}

/// 搜索框是否命中这一项（`id` 与友好名都参与匹配，大小写不敏感）。
pub fn online_matches(distro: &OnlineDistro, query: &str) -> bool {
    let query = query.trim().to_ascii_lowercase();
    if query.is_empty() {
        return true;
    }
    distro.id.to_ascii_lowercase().contains(&query)
        || distro.label.to_ascii_lowercase().contains(&query)
}
