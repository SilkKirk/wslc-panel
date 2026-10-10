//! 镜像站：内置的 rootfs 清单 + 测速探测 + 下载命令。
//!
//! # 为什么不从"别人的 API"取清单
//!
//! 参考实现（`wsl-dashboard-ref`）的镜像清单来自它自己的服务
//! `https://api1.wslui.com`：它先问服务要一个清单 URL，再去那个 URL 取
//! 每个发行版的镜像列表，然后测速选最快的。**那套东西我们不能用** ——
//! 那是别人家的服务，随时会变、也随时会没。所以这里换成**内置表**：
//! 每个发行版列出"哪个镜像站上有哪个文件"，运行期只做两件事：
//! 逐条 HEAD 探测（挑最快的）、挑中的那条交给 `curl.exe` 下载。
//!
//! 表里的每一条 URL 都是**本机 curl 实测 200** 过的（见每条 `note`）。
//! 镜像站的文件名会随版本更新而变化（Alpine 的文件名里带小版本号），
//! 所以改表的时候必须重新实测 —— 探测失败时界面会明确说
//! "可能改名了，换一个版本或用自定义 URL"，而不是留一个转圈的空列表。
//!
//! # 为什么下载走 `curl.exe`
//!
//! 本仓库**不能新增依赖**：本机没有 cargo（`AGENTS.md` §1），而 CI 全部带
//! `--locked` —— 往 `Cargo.toml` 里加一个 HTTP 客户端会让锁文件与清单对不上，
//! 直接红掉，而且本地没法重新生成锁文件。`curl.exe` 从 Windows 10 1803 起
//! 随系统提供（本机实测 `C:\WINDOWS\system32\curl.exe`，8.21.0），
//! 思路与 `cmd/picker.rs` 借 `powershell.exe` 弹对话框完全一致。
//!
//! 代价：`curl.exe` 被 EDR 拦掉的环境用不了镜像站这条来源 ——
//! 界面要如实提示"改用本地 tar 导入"。

use std::path::PathBuf;

/// 一个镜像站。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorSite {
    /// 站点名（只用于显示，比如"清华 TUNA"）。
    pub name: &'static str,
    /// 根 URL，**不带结尾斜杠**。
    pub base: &'static str,
}

/// 内置清单里的一条：某一版发行版在哪些镜像站上有 rootfs。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorDistro {
    /// 内部 id（计划里与界面按钮的 id 用它）。
    pub id: &'static str,
    /// 给人看的名字。
    pub label: &'static str,
    /// 版本代号（显示 + preflight 的提醒里用）。
    pub release: &'static str,
    /// 相对目录（相对镜像站根，不带结尾斜杠）。
    pub dir: &'static str,
    /// 文件名。
    pub file: &'static str,
    /// 有哪些镜像站有它（写 [`MirrorSite::name`]）。
    pub sites: &'static [&'static str],
    /// 实测备注：多大、什么时候验的。
    pub note: &'static str,
}

/// 内置的镜像站。
///
/// 只放**本机实测能连上**的：Gitee 上的镜像站列表动辄几十个，
/// 但一个个试过去会让"探测最快镜像"变成一次几十秒的等待。
pub fn sites() -> &'static [MirrorSite] {
    &[
        MirrorSite {
            name: "清华 TUNA",
            base: "https://mirrors.tuna.tsinghua.edu.cn",
        },
        MirrorSite {
            name: "中科大 USTC",
            base: "https://mirrors.ustc.edu.cn",
        },
        MirrorSite {
            name: "阿里云",
            base: "https://mirrors.aliyun.com",
        },
    ]
}

/// 内置的可装清单。
///
/// ⚠️ **只有 amd64**。arm64 的 WSL 上这张表是空的（界面据此提示"用自定义 URL"）——
/// 与其编一条没验证过的 arm64 URL，不如老实说没有。
pub fn distros() -> &'static [MirrorDistro] {
    &[
        MirrorDistro {
            id: "ubuntu-24.04",
            label: "Ubuntu 24.04 LTS（noble）",
            release: "noble",
            dir: "ubuntu-cloud-images/noble/current",
            file: "noble-server-cloudimg-amd64-root.tar.xz",
            sites: &["清华 TUNA", "中科大 USTC"],
            note: "实测 2026-10-10：200，229,623,728 字节（约 219 MB）",
        },
        MirrorDistro {
            id: "ubuntu-22.04",
            label: "Ubuntu 22.04 LTS（jammy）",
            release: "jammy",
            dir: "ubuntu-cloud-images/jammy/current",
            file: "jammy-server-cloudimg-amd64-root.tar.xz",
            sites: &["清华 TUNA", "中科大 USTC"],
            note: "实测 2026-10-10：200，458,270,220 字节（约 437 MB）",
        },
        MirrorDistro {
            id: "alpine-3.21",
            label: "Alpine Linux 3.21（minirootfs）",
            release: "v3.21",
            dir: "alpine/v3.21/releases/x86_64",
            file: "alpine-minirootfs-3.21.0-x86_64.tar.gz",
            sites: &["清华 TUNA", "中科大 USTC", "阿里云"],
            note: "实测 2026-10-10：200，3,509,360 字节（约 3.3 MB）",
        },
    ]
}

/// 本机架构在镜像站文件名里的写法。
///
/// 只用于**判断这张表适不适用** —— 具体目录/文件名在表里是写死的字面量
/// （Ubuntu 用 `amd64`、Alpine 用 `x86_64`，模板反而不如字面量清楚）。
pub fn arch() -> &'static str {
    if cfg!(target_arch = "x86_64") {
        "amd64"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "unknown"
    }
}

/// 内置表在当前架构上有没有可用的条目。
pub fn available_on_this_arch() -> bool {
    arch() == "amd64"
}

/// 按名字找站点。
pub fn site_by_name(name: &str) -> Option<&'static MirrorSite> {
    sites().iter().find(|site| site.name == name)
}

/// 一个候选下载地址（站点 + 完整 URL）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// 站点名。
    pub site: String,
    /// 完整 URL。
    pub url: String,
}

/// 拼出一条条目的**全部**候选地址。
///
/// 表里写了站点名但站点表里没有那个名字时**跳过**（而不是 panic）——
/// 那是改表时的手误，单测会钉住它，运行期不该因此崩掉。
pub fn candidates(distro: &MirrorDistro) -> Vec<Candidate> {
    let mut out = Vec::new();
    for name in distro.sites {
        let Some(site) = site_by_name(name) else {
            continue;
        };
        out.push(Candidate {
            site: site.name.to_owned(),
            url: format!("{}/{}/{}", site.base, distro.dir, distro.file),
        });
    }
    out
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
    fn every_entry_resolves_to_real_sites_and_clean_urls() {
        for distro in distros() {
            assert!(!distro.sites.is_empty(), "{} 没有镜像站", distro.id);
            let found = candidates(distro);
            assert_eq!(
                found.len(),
                distro.sites.len(),
                "{} 有站点名没在站点表里（改表时的手误）",
                distro.id
            );
            for candidate in found {
                assert!(candidate.url.starts_with("https://"), "{}", candidate.url);
                // 去掉协议头之后再查 `//`：`https://` 自己就带两个斜杠，
                // 不剥掉的话这条断言永远为假（这个坑真的差点写进去）。
                let rest = candidate.url.trim_start_matches("https://");
                assert!(!rest.contains("//"), "{}", candidate.url);
                assert!(!candidate.url.ends_with('/'), "{}", candidate.url);
                assert!(candidate.url.ends_with(distro.file), "{}", candidate.url);
            }
        }
    }

    #[test]
    fn unknown_site_names_are_skipped_instead_of_panicking() {
        // 运行期不该因为改表手误而崩 —— 单测会把这种手误挡在上一条里
        let broken = MirrorDistro {
            id: "x",
            label: "x",
            release: "x",
            dir: "d",
            file: "f.tar",
            sites: &["不存在的站点"],
            note: "",
        };
        assert!(candidates(&broken).is_empty());
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
        };
        let b = Candidate {
            site: "快".to_owned(),
            url: "https://b/x".to_owned(),
        };
        let c = Candidate {
            site: "坏".to_owned(),
            url: "https://c/x".to_owned(),
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
