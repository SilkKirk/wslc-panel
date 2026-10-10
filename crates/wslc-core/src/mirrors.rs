//! 在线发行版：清单来自 wslui 的公开接口 + 测速探测 + 下载命令。
//!
//! # 清单是**数据**，代码是自己写的
//!
//! 「在线发行版（国内镜像源）」这条来源的清单来自 `https://api1.wslui.com`：
//! 先问它"清单在哪儿"，再去那个地址取"哪些发行版、每个版本有哪些镜像上有文件"，
//! 然后测速挑最快的下载。这和参考实现（`wsl-dashboard-ref`）用的是**同一套接口**
//! —— 用户明确要求对齐它，而且这份清单是活的（2026-10-10 实测：24 个发行版、
//! 每个 2~13 个镜像站、全是国内镜像）。
//!
//! 但**代码一行没抄**：参考实现是 GPL-3.0-only、本仓库 Apache-2.0
//! （`AGENTS.md` §6），能共用的只有"接口地址与 JSON 字段"这类事实。
//!
//! ```text
//! GET https://api1.wslui.com/desktop/v1/helper/install
//!   → { err, msg, data: { online_distros: { url } } }        ← 清单地址（会变，所以要问）
//! GET <那个 url>
//!   → { err, msg, data: { distros: [ { name, version, sources: [ { url, mirror, format } ] } ],
//!                         mirrors: [ …同上，arm64 的那一份… ] } }
//! ```
//!
//! 实测（2026-10-10）：`helper/install` 441 字节；清单 52497 字节，
//! amd64 24 项 / arm64 20 项，`update_time` 是 2026-10-09。
//!
//! # ⚠️ `format` 不止一种，装法**不一样**
//!
//! 每条来源都带 `format`。实测取值有 `tar.xz` / `tar.gz` / `wsl`：
//!
//! - `tar.*` → `wsl --import`（我们本来那条路）；
//! - `wsl` → 那是**新格式的 `.wsl` 包**，`--import` 吃不了，必须
//!   `wsl --install --from-file`。Ubuntu 24.04 的 13 个来源里**有 10 个**是这种
//!   （清华/阿里/华为/网易…的 `ubuntu-releases/*.wsl`）。
//!
//! 所以"挑最快的那个然后一律 `--import`"会**直接失败** ——
//! 参考实现就是这么干的（它 `install_from_mirror` 里永远 `--import`）。
//! 这里按 `format` 分岔，见 `model::install::plan`。
//!
//! # 为什么探测与下载走 `curl.exe`
//!
//! 本仓库**不能新增依赖**：本机没有 cargo（`AGENTS.md` §1），而 CI 全部带
//! `--locked` —— 往 `Cargo.toml` 里加一个 HTTP 客户端会让锁文件与清单对不上，
//! 直接红掉，而且本地没法重新生成锁文件。`curl.exe` 从 Windows 10 1803 起
//! 随系统提供（本机实测 `C:\WINDOWS\system32\curl.exe`，8.21.0），
//! 思路与 `cmd/picker.rs` 借 `powershell.exe` 弹对话框完全一致。
//!
//! 代价：`curl.exe` 被 EDR 拦掉的环境用不了镜像源这条来源 ——
//! 界面要如实提示"改用本地 tar 导入"。
//!
//! ⚠️ 顺带记一条实测：**系统代理（dev-sidecar）我们的 curl 用不上**。
//! `wsl.exe` 自己走系统代理（所以开着代理时 `wsl -l -o` 能通），
//! 而 `curl.exe` 不读 Windows 的代理设置；手动 `--proxy` 指过去会撞
//! MITM 证书（`000`）。所以这里的请求都按**直连**设计：
//! `api1/api2.wslui.com` 与国内镜像站直连都通（实测 200）。

use std::path::PathBuf;

/// 问"清单在哪儿"的接口。
pub const DISCOVERY_URL: &str = "https://api1.wslui.com/desktop/v1/helper/install";

/// 清单地址的兜底：上面的接口挂了就直接用这个。
///
/// 2026-10-10 实测它就是接口返回的那个地址。写两份的理由是
/// "接口会变、地址不一定会变" —— 少一个请求就少一个失败点。
pub const CATALOG_FALLBACK_URL: &str = "https://api2.wslui.com/co-creation/api/online-distros";

/// 接口要一个像浏览器的 UA（实测不带也 200，但带上更稳）。
pub const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36";

/// 清单里的**一个来源**：某个镜像站上的一份文件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfferSource {
    /// 镜像站名（清单里给的短名，如 `lxc-tuna` / `tsinghua`）。
    pub mirror: String,
    /// 完整下载地址（清单给的就是完整 URL，不需要我们自己拼）。
    pub url: String,
    /// 打包格式：`tar.xz` / `tar.gz` / `wsl`……
    pub format: String,
}

impl OfferSource {
    /// 是不是 `.wsl` 包（装法和 tar 不同，见模块说明）。
    ///
    /// 除了看 `format`，也看后缀：清单里偶尔会有 `format` 缺失、
    /// 但文件名明摆着是 `.wsl` 的条目。
    pub fn is_bundle(&self) -> bool {
        self.format.eq_ignore_ascii_case("wsl") || self.url.to_lowercase().ends_with(".wsl")
    }

    /// 临时文件该用什么后缀（`.wsl` / `.tar.xz` / `.tar.gz`）。
    pub fn extension(&self) -> &'static str {
        if self.is_bundle() {
            ".wsl"
        } else if self.format.eq_ignore_ascii_case("tar.gz") {
            ".tar.gz"
        } else if self.url.to_lowercase().ends_with(".tar.gz") {
            ".tar.gz"
        } else {
            ".tar.xz"
        }
    }
}

/// 清单里的一个发行版（**某一个版本**）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    /// 发行版名（如 `Ubuntu`）。
    pub name: String,
    /// 版本（如 `24.04` / `current`）。
    pub version: String,
    /// 有哪些镜像上有它。
    pub sources: Vec<OfferSource>,
}

impl Offer {
    /// 内部 id：`"<name> <version>"`。
    ///
    /// 和参考实现同一个形状（它用 `format!("{} {}", name, version)` 当 internal_id），
    /// 这样两边的清单能直接对照着看。
    pub fn id(&self) -> String {
        format!("{} {}", self.name, self.version)
    }

    /// 显示名：`"Ubuntu 24.04"`。
    pub fn label(&self) -> String {
        format!("{} {}", self.name, self.version)
    }

    /// 候选地址（探测与下载都用它）。
    pub fn candidates(&self) -> Vec<Candidate> {
        self.sources
            .iter()
            .filter(|source| !source.url.trim().is_empty())
            .map(|source| Candidate {
                site: source.mirror.clone(),
                url: source.url.clone(),
                format: source.format.clone(),
            })
            .collect()
    }

    /// 一句话说明这条有哪些装法（界面显示用）：
    /// `"tar.xz × 3 / wsl 包 × 10"`。
    pub fn format_summary(&self) -> String {
        let mut tar = 0usize;
        let mut bundle = 0usize;
        for source in &self.sources {
            if source.is_bundle() {
                bundle += 1;
            } else {
                tar += 1;
            }
        }
        match (tar, bundle) {
            (0, 0) => "没有可用来源".to_owned(),
            (0, n) => format!("{n} 个来源（都是 .wsl 包）"),
            (n, 0) => format!("{n} 个来源（都是 tar）"),
            (t, b) => format!("{t} 个 tar / {b} 个 .wsl 包"),
        }
    }
}

/// 本机架构在清单里的写法（`amd64` / `arm64`）。
///
/// 清单里有两份数组（`distros` 是 amd64、`mirrors` 是 arm64），
/// 靠它决定取哪一份。都不是就返回 `"unknown"`，调用方据此报错
/// ——**不猜**：拿错架构的 rootfs 装出来的发行版根本起不来。
pub fn arch() -> &'static str {
    if cfg!(target_arch = "x86_64") {
        "amd64"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "unknown"
    }
}

/// 本机架构的清单拿得到吗。
pub fn available_on_this_arch() -> bool {
    arch() != "unknown"
}

/// 一个候选下载地址（站点 + 完整 URL + 格式）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// 站点名。
    pub site: String,
    /// 完整 URL。
    pub url: String,
    /// 打包格式（决定装法，见模块说明）。
    pub format: String,
}

impl Candidate {
    /// 是不是 `.wsl` 包。
    pub fn is_bundle(&self) -> bool {
        self.format.eq_ignore_ascii_case("wsl") || self.url.to_lowercase().ends_with(".wsl")
    }
}

/// 校验接口信封 `{ err, msg, data }`，返回 `data`。
///
/// 两个接口都是这个信封：`err != 0` 是**业务错误**（HTTP 仍然是 200），
/// 忽略它会把"接口说你没权限"读成"清单是空的"。
fn envelope_data(json: &str) -> Result<serde_json::Value, String> {
    let clean = json.trim_start_matches('\u{feff}').trim();
    if clean.is_empty() {
        return Err("接口返回了空内容".to_owned());
    }
    let value: serde_json::Value =
        serde_json::from_str(clean).map_err(|e| format!("接口返回的不是 JSON：{e}"))?;

    let err = value.get("err").and_then(serde_json::Value::as_i64).unwrap_or(0);
    if err != 0 {
        let msg = value
            .get("msg")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        return Err(format!("接口报错（err={err}）：{msg}"));
    }
    value
        .get("data")
        .cloned()
        .ok_or_else(|| "接口返回里没有 data".to_owned())
}

/// 从 `helper/install` 的响应里取出**清单地址**。
pub fn parse_discovery(json: &str) -> Result<String, String> {
    let data = envelope_data(json)?;
    let url = data
        .get("online_distros")
        .and_then(|it| it.get("url"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    if url.trim().is_empty() {
        return Err("接口没有给出清单地址（online_distros.url 是空的）".to_owned());
    }
    Ok(url.trim().to_owned())
}

/// 解析清单：按架构取 `distros`（amd64）或 `mirrors`（arm64）。
///
/// 空清单**算错误**：那不是"没有可装的发行版"，多半是接口换了字段名 ——
/// 界面据此说清"清单是空的"比留一个空列表有用。
pub fn parse_catalog(json: &str, arm64: bool) -> Result<Vec<Offer>, String> {
    let data = envelope_data(json)?;
    let key = if arm64 { "mirrors" } else { "distros" };
    let list = match data.get(key) {
        Some(serde_json::Value::Array(list)) => list,
        // arm64 的清单可能整份缺字段（老接口）—— 那时退回 amd64 那一份
        // 会让用户装出一个跑不起来的发行版，所以宁可报错。
        _ => return Err(format!("清单里没有 `{key}` 数组")),
    };

    let mut offers: Vec<Offer> = Vec::new();
    for item in list {
        let name = item
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim()
            .to_owned();
        if name.is_empty() {
            continue;
        }
        let version = item
            .get("version")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim()
            .to_owned();

        let mut sources = Vec::new();
        if let Some(serde_json::Value::Array(items)) = item.get("sources") {
            for source in items {
                let url = source
                    .get("url")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_owned();
                if url.is_empty() {
                    continue;
                }
                sources.push(OfferSource {
                    mirror: source
                        .get("mirror")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("未知镜像")
                        .trim()
                        .to_owned(),
                    url,
                    format: source
                        .get("format")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("")
                        .trim()
                        .to_owned(),
                });
            }
        }
        // 一个来源都没有的条目对用户没有意义（点了也装不了）
        if sources.is_empty() {
            continue;
        }
        offers.push(Offer {
            name,
            version,
            sources,
        });
    }

    if offers.is_empty() {
        return Err(format!("清单里的 `{key}` 数组是空的"));
    }
    Ok(offers)
}

/// 取清单里的 `update_time`（显示"这份清单是什么时候更新的"）。
pub fn parse_update_time(json: &str) -> Option<String> {
    let data = envelope_data(json).ok()?;
    let time = data
        .get("update_time")
        .and_then(serde_json::Value::as_str)?
        .trim()
        .to_owned();
    if time.is_empty() {
        None
    } else {
        Some(time)
    }
}

/// `curl.exe` 拉 JSON 的参数（正文走 stdout）。
///
/// `-m 25`：清单 52 KB，正常几百毫秒；给足余量但不让它挂死。
pub fn curl_json_args(url: &str) -> Vec<String> {
    vec![
        "-s".to_owned(),
        "-L".to_owned(),
        "-m".to_owned(),
        "25".to_owned(),
        "-A".to_owned(),
        BROWSER_UA.to_owned(),
        url.to_owned(),
    ]
}

/// `curl.exe` 的**探测**参数（HEAD，不打正文）。
///
/// `-w` 里的 `%{header_json}` 是拿 `Content-Length` 的唯一办法：
/// HEAD 请求的 `%{size_download}` 恒为 0，而下载进度的分母要的是
/// **声明的**大小。`header_json` 需要 curl ≥ 7.83（本机 8.21），
/// 老版本会给空串 —— 那时 [`Probe::bytes`] 是 `None`，
/// 界面退化成"已下载多少"而没有百分比，功能不受影响。
pub fn probe_args(url: &str) -> Vec<String> {
    vec![
        "-s".to_owned(),
        "-I".to_owned(),
        "-L".to_owned(),
        "-m".to_owned(),
        "8".to_owned(),
        // Windows 的"空设备"；`-o NUL` 让正文不落到 stdout
        "-o".to_owned(),
        "NUL".to_owned(),
        "-w".to_owned(),
        "%{http_code} %{time_total} %{header_json}".to_owned(),
        url.to_owned(),
    ]
}

/// 探测结果。
#[derive(Debug, Clone, PartialEq)]
pub struct Probe {
    /// HTTP 状态码（连接失败时 curl 给 `000`）。
    pub code: u16,
    /// 总耗时（秒）—— **这就是"哪个镜像快"的依据**。
    pub secs: f64,
    /// 声明的文件大小；只在 200 时采信。
    ///
    /// ⚠️ 404 的响应也带 `Content-Length`（那是错误页面的长度，
    /// 本机实测阿里云的 404 页面是 2318 字节）。当成文件大小就是一个
    /// 只会让人困惑的进度条，所以这里**只在 200 时**给值。
    pub bytes: Option<u64>,
}

impl Probe {
    /// 这个候选可用吗（200）。
    pub fn is_ok(&self) -> bool {
        self.code == 200
    }
}

/// 解析 [`probe_args`] 的输出。
///
/// 形状：`200 0.161290 {"server":["nginx/1.22.1"],\n"content-length":["229623728"],\n...}`。
/// 注意 `header_json` 是**带换行的**（curl 会美化它），所以这里不能按行切 ——
/// 只切前两个空格，剩下的整段当 JSON。
pub fn parse_probe(stdout: &str) -> Option<Probe> {
    let text = stdout.trim();
    let mut parts = text.splitn(3, ' ');
    let code = parts.next()?.trim().parse::<u16>().ok()?;
    let secs = parts.next()?.trim().parse::<f64>().ok()?;
    let bytes = parts.next().and_then(header_content_length);
    Some(Probe {
        code,
        secs,
        bytes: if code == 200 { bytes } else { None },
    })
}

/// 从 `header_json` 里抠出 `content-length`。
///
/// 键名大小写与值的形状都可能变（curl 给的是**字符串数组**），
/// 所以三种写法都试一遍，拿不到就 `None`（不报错）。
fn header_content_length(json: &str) -> Option<u64> {
    let value: serde_json::Value = serde_json::from_str(json.trim()).ok()?;
    let object = value.as_object()?;
    for key in ["content-length", "Content-Length"] {
        let Some(entry) = object.get(key) else {
            continue;
        };
        if let Some(text) = entry.as_str() {
            if let Ok(bytes) = text.trim().parse::<u64>() {
                return Some(bytes);
            }
        }
        if let Some(first) = entry.as_array().and_then(|items| items.first()) {
            if let Some(text) = first.as_str() {
                if let Ok(bytes) = text.trim().parse::<u64>() {
                    return Some(bytes);
                }
            }
        }
        if let Some(bytes) = entry.as_u64() {
            return Some(bytes);
        }
    }
    None
}

/// 从探测结果里挑最快的一个。
///
/// 纯函数，方便单测：**只认 200**，最小 `secs` 胜出；都没有就 `None`。
pub fn pick_fastest(probes: &[(Candidate, Option<Probe>)]) -> Option<(Candidate, Probe)> {
    probes
        .iter()
        .filter_map(|(candidate, probe)| probe.as_ref().filter(|p| p.is_ok()).map(|p| (candidate, p)))
        .min_by(|(_, a), (_, b)| a.secs.partial_cmp(&b.secs).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(candidate, probe)| (candidate.clone(), probe.clone()))
}

/// `curl.exe` 的**下载**参数。
pub fn download_args(url: &str, out: &str) -> Vec<String> {
    vec![
        "-s".to_owned(),
        // `-S`：出错时仍然说话。-s 单用会把错误一起吞掉，
        // 那是"点了没反应"里最难查的一种。
        "-S".to_owned(),
        "-L".to_owned(),
        "--retry".to_owned(),
        "2".to_owned(),
        "--connect-timeout".to_owned(),
        "10".to_owned(),
        "-o".to_owned(),
        out.to_owned(),
        url.to_owned(),
    ]
}

/// 下载进度的**原料**（纯数据，百分比/速度由界面算）。
///
/// # 为什么不解析 curl 自己的进度条
///
/// curl 的进度是 `\r` 刷新的，而我们的流式读取按 `\n` 切行（见 `cli.rs`）——
/// 那样每一条进度都会攒到进程结束才作为一整行吐出来，等于没有进度。
/// 所以改成**轮询目标文件的大小**：那个数字真实且连续，
/// 与导出用文件大小做进度是同一个理由（见 `cmd::distro::export_streaming`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DownloadGap {
    /// 已经写到磁盘的字节数。
    pub have: u64,
    /// 总大小（探测时拿到的 `Content-Length`；可能没有）。
    pub total: Option<u64>,
    /// 已经跑了多少秒。
    pub secs: u64,
}

impl DownloadGap {
    /// 进度百分比（0~100）；拿不到总大小时为 `None`。
    pub fn percent(&self) -> Option<f64> {
        let total = self.total?;
        if total == 0 {
            return None;
        }
        Some((self.have as f64 / total as f64 * 100.0).clamp(0.0, 100.0))
    }

    /// 平均速度（字节/秒）；时间还是 0 时为 `None`。
    pub fn bytes_per_sec(&self) -> Option<f64> {
        if self.secs == 0 {
            return None;
        }
        Some(self.have as f64 / self.secs as f64)
    }
}

/// 把字节数说成人话（`1.2 GB`）。1000 进制，和 WSL 自己的显示一致。
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// 把速度说成人话（`4.2 MB/s`）。
pub fn human_speed(bytes_per_sec: f64) -> String {
    format!("{}/s", human_bytes(bytes_per_sec.max(0.0) as u64))
}

/// 临时文件放哪儿（`%TEMP%\wslc-panel`）。
///
/// 和 [`crate::model::install::default_temp_dir`] 是同一个位置 ——
/// 镜像下载与在线安装重定位的中转文件都在这儿，方便用户自己去找、去删。
pub fn temp_dir() -> PathBuf {
    crate::model::install::default_temp_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_discovery_response_gives_the_catalog_url() {
        // 本机真实抓的响应（2026-10-10，442 字节）
        let json = include_str!("../tests/fixtures/wslui_helper_install.json");
        let url = parse_discovery(json).expect("真实响应应该能解出清单地址");
        assert!(url.starts_with("https://"), "{url}");
        assert_eq!(url, CATALOG_FALLBACK_URL, "接口给的就是兜底里那个地址");
    }

    #[test]
    fn discovery_errors_are_reported_not_silently_defaulted() {
        // 空内容 / 不是 JSON / 业务错误 / 没有 online_distros → 都要报错，
        // 让调用方自己决定要不要退到兜底地址（而不是这里偷偷给个空串）
        assert!(parse_discovery("").is_err());
        assert!(parse_discovery("<html>502</html>").is_err());
        assert!(parse_discovery(r#"{"err":1001,"msg":"bad sign","data":{}}"#)
            .unwrap_err()
            .contains("1001"));
        assert!(parse_discovery(r#"{"err":0,"msg":"ok","data":{}}"#).is_err());
        assert!(parse_discovery(r#"{"err":0,"msg":"ok","data":{"online_distros":{"url":"  "}}}"#).is_err());
    }

    #[test]
    fn real_catalog_parses_both_architectures() {
        // 本机真实抓的清单（2026-10-10，52497 字节，update_time 2026-10-09）
        let json = include_str!("../tests/fixtures/wslui_online_distros.json");

        let amd64 = parse_catalog(json, false).expect("amd64 清单应该能解析");
        assert!(amd64.len() >= 20, "只解析出 {} 项", amd64.len());
        assert_eq!(parse_update_time(json).as_deref(), Some("2026-10-09T21:35:03.166313241Z"));

        let ubuntu = amd64
            .iter()
            .find(|offer| offer.id() == "Ubuntu 24.04")
            .expect("清单里应该有 Ubuntu 24.04");
        assert!(ubuntu.sources.len() >= 5, "{}", ubuntu.sources.len());
        for source in &ubuntu.sources {
            assert!(source.url.starts_with("https://"), "{source:?}");
            assert!(!source.mirror.trim().is_empty(), "{source:?}");
        }
        // Ubuntu 24.04 的绝大多数来源是 .wsl 包 —— 这正是"不能一律 --import"的原因
        let bundles = ubuntu.sources.iter().filter(|s| s.is_bundle()).count();
        assert!(bundles >= 5, "Ubuntu 24.04 应该有多个 .wsl 来源：{bundles}");
        assert!(ubuntu.format_summary().contains("wsl"), "{}", ubuntu.format_summary());

        // arm64 那一份是另一组 URL（文件名里带 arm64）
        let arm64 = parse_catalog(json, true).expect("arm64 清单应该能解析");
        assert!(arm64.len() >= 15, "只解析出 {} 项", arm64.len());
        let arm_ubuntu = arm64
            .iter()
            .find(|offer| offer.id() == "Ubuntu 24.04")
            .expect("arm64 清单里也应该有 Ubuntu 24.04");
        assert!(
            arm_ubuntu.sources.iter().all(|s| !s.url.contains("amd64")),
            "arm64 的来源里不该出现 amd64 的文件名"
        );
        assert!(arm_ubuntu.sources.iter().all(|s| s.url.contains("arm64")));

        // 每一条都要有 id 与至少一个候选地址
        for offer in &amd64 {
            assert!(!offer.id().trim().is_empty());
            assert!(!offer.candidates().is_empty(), "{offer:?}");
        }
    }

    #[test]
    fn catalog_ignores_broken_entries_but_keeps_the_rest() {
        // 缺 name、没有 sources、url 是空的条目都要被跳过，
        // 而不是让整份清单失败（接口偶尔会有半成品条目）
        let json = r#"{"err":0,"msg":"ok","data":{"distros":[
            {"name":"","version":"1","sources":[{"url":"https://a/x","mirror":"m","format":"tar.xz"}]},
            {"name":"NoSource","version":"1","sources":[]},
            {"name":"EmptyUrl","version":"1","sources":[{"url":"  ","mirror":"m","format":"tar.xz"}]},
            {"name":"Good","version":"1","sources":[{"url":"https://a/g","mirror":"m","format":"tar.gz"}]}
        ]}}"#;
        let offers = parse_catalog(json, false).unwrap();
        assert_eq!(offers.len(), 1, "{offers:?}");
        assert_eq!(offers[0].id(), "Good 1");
        assert_eq!(offers[0].candidates()[0].format, "tar.gz");

        // 整份清单空掉时要报错（那不是"没有可装的"，多半是字段改名了）
        let empty = r#"{"err":0,"msg":"ok","data":{"distros":[]}}"#;
        assert!(parse_catalog(empty, false).unwrap_err().contains("空的"));
        // arm64 那一份缺数组时也要报错（退回 amd64 会装出跑不起来的东西）
        assert!(parse_catalog(json, true).is_err());
    }

    #[test]
    fn candidates_carry_the_install_method() {
        // tar → --import；.wsl → --install --from-file。后缀决定临时文件名。
        let tar = OfferSource {
            mirror: "lxc-tuna".to_owned(),
            url: "https://mirrors.tuna.tsinghua.edu.cn/x/rootfs.tar.xz".to_owned(),
            format: "tar.xz".to_owned(),
        };
        assert!(!tar.is_bundle());
        assert_eq!(tar.extension(), ".tar.xz");

        let bundle = OfferSource {
            mirror: "tsinghua".to_owned(),
            url: "https://mirrors.tuna.tsinghua.edu.cn/ubuntu-releases/24.04/ubuntu-24.04.5-wsl-amd64.wsl".to_owned(),
            format: "wsl".to_owned(),
        };
        assert!(bundle.is_bundle());
        assert_eq!(bundle.extension(), ".wsl");

        // format 缺失但后缀是 .wsl 也要认出来
        let by_suffix = OfferSource {
            mirror: "m".to_owned(),
            url: "https://x/y.WSL".to_owned(),
            format: String::new(),
        };
        assert!(by_suffix.is_bundle());

        let gz = OfferSource {
            mirror: "m".to_owned(),
            url: "https://x/y.tar.gz".to_owned(),
            format: String::new(),
        };
        assert_eq!(gz.extension(), ".tar.gz");
    }

    #[test]
    fn json_arguments_are_stable() {
        let args = curl_json_args("https://api1.wslui.com/x");
        assert_eq!(
            args,
            vec!["-s", "-L", "-m", "25", "-A", BROWSER_UA, "https://api1.wslui.com/x"]
        );
    }

    #[test]
    fn probe_output_of_a_successful_head_is_parsed() {
        // 本机真实输出（清华 TUNA 的 Ubuntu 24.04 rootfs，2026-10-10）
        let probe = parse_probe(include_str!("../tests/fixtures/curl_probe_ok.txt")).unwrap();
        assert_eq!(probe.code, 200);
        assert!(probe.is_ok());
        assert!(probe.secs > 0.0);
        // 关键：从 header_json 里拿到的**声明**大小
        assert_eq!(probe.bytes, Some(229_623_728));
    }

    #[test]
    fn probe_output_of_a_404_has_no_size() {
        let probe = parse_probe(include_str!("../tests/fixtures/curl_probe_404.txt")).unwrap();
        assert_eq!(probe.code, 404);
        assert!(!probe.is_ok());
        // 404 的响应体是错误页面（本机实测 2318 字节），绝不能当成 rootfs 的大小
        assert_eq!(probe.bytes, None);
    }

    #[test]
    fn probe_output_of_a_failed_connection_is_parsed_not_panicked() {
        let probe = parse_probe(include_str!("../tests/fixtures/curl_probe_fail.txt")).unwrap();
        assert_eq!(probe.code, 0);
        assert!(!probe.is_ok());
        assert_eq!(probe.bytes, None);

        // 截断/垃圾输入全部给 None，绝不 panic
        assert_eq!(parse_probe(""), None);
        assert_eq!(parse_probe("200"), None);
        assert_eq!(parse_probe("abc 1.0 {}"), None);
    }

    #[test]
    fn probe_tolerates_an_old_curl_without_header_json() {
        // 老 curl 不给 %{header_json}（给空串）→ 只是没有大小，不影响可用性
        let probe = parse_probe("200 0.5 ").unwrap();
        assert_eq!(probe.code, 200);
        assert_eq!(probe.bytes, None);
        // 空对象同理
        let probe = parse_probe("200 0.5 {}").unwrap();
        assert_eq!(probe.bytes, None);
        // 值不是数组而是裸字符串时也要认
        let probe = parse_probe(r#"200 0.5 {"content-length":"123"}"#).unwrap();
        assert_eq!(probe.bytes, Some(123));
    }

    #[test]
    fn fastest_candidate_wins_and_only_200_counts() {
        let a = Candidate {
            site: "慢".to_owned(),
            url: "https://a/x".to_owned(),
            format: "tar.xz".to_owned(),
        };
        let b = Candidate {
            site: "快".to_owned(),
            url: "https://b/x".to_owned(),
            format: "tar.xz".to_owned(),
        };
        let c = Candidate {
            site: "坏".to_owned(),
            url: "https://c/x".to_owned(),
            format: "tar.xz".to_owned(),
        };
        let probes = vec![
            (
                a.clone(),
                Some(Probe {
                    code: 200,
                    secs: 2.0,
                    bytes: Some(10),
                }),
            ),
            (
                b.clone(),
                Some(Probe {
                    code: 200,
                    secs: 0.4,
                    bytes: Some(10),
                }),
            ),
            (
                c,
                Some(Probe {
                    code: 404,
                    secs: 0.01,
                    bytes: None,
                }),
            ),
        ];
        let (winner, probe) = pick_fastest(&probes).unwrap();
        assert_eq!(winner.site, "快");
        assert_eq!(probe.secs, 0.4);

        // 一个 200 都没有 → None（界面据此说"这个镜像上没有这个文件"）
        let all_bad = vec![(
            a,
            Some(Probe {
                code: 404,
                secs: 0.1,
                bytes: None,
            }),
        )];
        assert!(pick_fastest(&all_bad).is_none());
        assert!(pick_fastest(&[]).is_none());
        // 探测没跑完（None）的候选也要能跳过
        let with_none: Vec<(Candidate, Option<Probe>)> = vec![(b, None)];
        assert!(pick_fastest(&with_none).is_none());
    }

    #[test]
    fn download_args_pin_the_url_and_output() {
        let args = download_args("https://x/y.tar.xz", r"D:\tmp\y.tar.xz");
        assert_eq!(args[0], "-s");
        assert!(args.iter().any(|a| a == "-S"), "出错必须仍然说话");
        assert_eq!(args[args.len() - 1], "https://x/y.tar.xz");
        let at = args.iter().position(|a| a == "-o").unwrap();
        assert_eq!(args[at + 1], r"D:\tmp\y.tar.xz");
    }

    #[test]
    fn gap_computes_percent_and_speed() {
        let gap = DownloadGap {
            have: 50,
            total: Some(200),
            secs: 10,
        };
        assert_eq!(gap.percent(), Some(25.0));
        assert_eq!(gap.bytes_per_sec(), Some(5.0));

        // 拿不到总大小 / 除零 / 超过总量（服务端给了 Range 或大小不准）都不能出 NaN
        let unknown = DownloadGap {
            have: 50,
            total: None,
            secs: 0,
        };
        assert_eq!(unknown.percent(), None);
        assert_eq!(unknown.bytes_per_sec(), None);
        let over = DownloadGap {
            have: 300,
            total: Some(200),
            secs: 1,
        };
        assert_eq!(over.percent(), Some(100.0));
        let zero = DownloadGap {
            have: 0,
            total: Some(0),
            secs: 1,
        };
        assert_eq!(zero.percent(), None);
    }

    #[test]
    fn human_sizes_are_readable() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(999), "999 B");
        assert_eq!(human_bytes(1000), "1.0 KB");
        assert_eq!(human_bytes(229_623_728), "229.6 MB");
        assert_eq!(human_bytes(3_509_360), "3.5 MB");
        assert_eq!(human_speed(4_200_000.0), "4.2 MB/s");
        // 负数（理论上不该出现）也要给出人话而不是 "-1.0 B"
        assert_eq!(human_speed(-1.0), "0 B/s");
    }

    #[test]
    fn probe_and_download_arguments_are_stable() {
        // 这两组参数是"界面上点了没反应"那类问题的重灾区，逐条钉住
        let probe = probe_args("https://x/y.tar.xz");
        assert_eq!(
            probe,
            vec![
                "-s",
                "-I",
                "-L",
                "-m",
                "8",
                "-o",
                "NUL",
                "-w",
                "%{http_code} %{time_total} %{header_json}",
                "https://x/y.tar.xz"
            ]
        );
    }

    #[test]
    fn temp_dir_is_the_shared_one() {
        assert_eq!(temp_dir(), crate::model::install::default_temp_dir());
        assert!(temp_dir().to_string_lossy().contains("wslc-panel"));
    }
}
