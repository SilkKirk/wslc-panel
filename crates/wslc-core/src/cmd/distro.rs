//! `wsl.exe` 子命令的类型化封装（WSL 发行版 / 实例）。
//!
//! # 与 `cmd::container` 等的区别
//!
//! `wsl.exe` **没有 `--format json`** —— 它的输出全是给人看的表格。
//! 所以这里只负责「拼参数 → 跑命令 → 把注册表信息合并进来」，
//! 真正的解析逻辑在 [`crate::model::distro`] 里（那样才能脱离 Windows 跑单测）。
//!
//! # 为什么还要读注册表
//!
//! `wsl` 命令**不报告**发行版装在哪个盘。唯一来源是
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Lxss\<GUID>` 下的
//! `BasePath` / `VhdFileName`（实测，见 `docs/PLAN-v0.3.md` §3.4）。
//!
//! 读取方式选了 `reg.exe query` 而不是注册表 API，理由见
//! [`crate::cli::run_helper`] 的文档（实测 7~27 ms，且输出恒为 UTF-8）。

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::cli::{self, Wsl};
use crate::error::Result;
use crate::model::distro::{parse_distro_list, Distro, WslStatus};

/// 注册表根键：发行版元数据都在这里。
pub const LXSS_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Lxss";

/// `reg.exe` 的调用超时。
///
/// 实测 7~27 ms，给 10 秒足够宽松 —— 这是为了防止极端情况下卡住界面。
const REG_TIMEOUT: Duration = Duration::from_secs(10);

/// `wsl --list --verbose` 的结果。
///
/// 把「认出来的」和「没认出来的」分开返回：一行解析失败不该让整个列表变空
/// （那会让面板看起来像"一个发行版都没有"）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DistroList {
    /// 认出来的发行版。
    pub distros: Vec<Distro>,
    /// 没认出来的行（原因 + 原文），交给界面如实展示。
    pub warnings: Vec<String>,
}

/// 列出全部发行版（`wsl --list --verbose`）。
pub fn list(wsl: &Wsl) -> Result<DistroList> {
    let out = wsl.run_checked(&["--list", "--verbose"])?;
    let (distros, warnings) = parse_distro_list(&out.stdout);
    Ok(DistroList { distros, warnings })
}

/// 读取 `wsl --status`（默认发行版 + 默认版本）。
///
/// ⚠️ 这只是**兜底**：默认发行版的权威来源是列表里的 `*`。
/// 而且 `--status` 的输出是**本地化**的（中文系统是「默认分发:」），
/// 所以采集频率刻意放低（30 秒），见 `docs/PLAN-v0.3.md` §10。
pub fn status(wsl: &Wsl) -> Result<WslStatus> {
    let out = wsl.run_checked(&["--status"])?;
    Ok(WslStatus::parse(&out.stdout))
}

/// 默认发行版的名字（列表里的 `*`）。
pub fn default_distro(distros: &[Distro]) -> Option<&str> {
    distros
        .iter()
        .find(|d| d.is_default)
        .map(|d| d.name.as_str())
}

/// 采集一次完整的发行版快照：列表 + 注册表 + 磁盘占用。
///
/// **阻塞调用**，必须在后台执行器上跑（会串行起 `wsl.exe` 和 `reg.exe`）。
pub fn snapshot(wsl: &Wsl) -> Result<DistroList> {
    let mut list = list(wsl)?;
    enrich(&mut list.distros);
    Ok(list)
}

// ---------------------------------------------------------------------------
// 注册表
// ---------------------------------------------------------------------------

/// 注册表里的发行版元数据。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegistryEntry {
    /// 发行版名（`DistributionName`）。
    pub name: String,
    /// 安装位置（`BasePath`）。
    pub base_path: Option<PathBuf>,
    /// 根文件系统虚拟磁盘的**文件名**（`VhdFileName`）。
    ///
    /// 实测是 `ext4.vhdx`。注册表直接给了名字，就不用去猜 / 扫目录。
    pub vhd_file_name: Option<String>,
    /// 默认用户 UID（`DefaultUid`）。
    pub default_uid: Option<u32>,
    /// WSL 版本（`Version`）。
    pub version: Option<u8>,
}

/// 读取注册表里的全部发行版元数据。
///
/// # 失败时返回空列表，而不是错误
///
/// 注册表读不到（被组策略挡了、`reg.exe` 不在 `PATH` 里……）时，
/// 面板**仍然应该能显示发行版列表** —— 只是没有安装位置和磁盘占用。
/// 让整个页面因为一个辅助信息而报错是不划算的。
pub fn read_registry() -> Vec<RegistryEntry> {
    match cli::run_helper("reg.exe", "reg", &["query", LXSS_KEY, "/s"], REG_TIMEOUT) {
        Ok(out) if out.success() => parse_registry(&out.stdout),
        Ok(out) => {
            tracing::warn!(
                "reg query 退出码 {:?}：{}",
                out.code,
                out.combined().trim()
            );
            Vec::new()
        }
        Err(e) => {
            tracing::warn!("读取注册表失败：{e}");
            Vec::new()
        }
    }
}

/// 解析 `reg.exe query <Lxss> /s` 的输出。
///
/// # 实测格式（`docs/PLAN-v0.3.md` §3.4）
///
/// ```text
///
/// HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Lxss
///     DefaultVersion    REG_DWORD    0x2
///     DefaultDistribution    REG_SZ    {856a6800-...}
///
/// HKEY_CURRENT_USER\...\Lxss\{856a6800-...}
///     DistributionName    REG_SZ    Ubuntu-26.04
///     BasePath    REG_SZ    D:\linux\Ubuntu-26.04
///     VhdFileName    REG_SZ    ext4.vhdx
///     DefaultUid    REG_DWORD    0x0
///     Version    REG_DWORD    0x2
/// ```
///
/// 只有**最后一段是 GUID 的键**才算发行版条目 —— 根键最后一段是 `Lxss`。
/// 用这个区分，比匹配整条路径稳（路径前缀可能随版本变）。
pub fn parse_registry(stdout: &str) -> Vec<RegistryEntry> {
    let mut entries: Vec<RegistryEntry> = Vec::new();
    let mut current: Option<RegistryEntry> = None;

    // 收尾：一个条目只有在拿到 `DistributionName` 之后才算数
    // （注册表里可能有空的 GUID 键）。
    fn flush(current: &mut Option<RegistryEntry>, entries: &mut Vec<RegistryEntry>) {
        if let Some(entry) = current.take() {
            if !entry.name.is_empty() {
                entries.push(entry);
            }
        }
    }

    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // 键头
        if trimmed.starts_with("HKEY_") {
            flush(&mut current, &mut entries);
            if is_distro_key(trimmed) {
                current = Some(RegistryEntry::default());
            }
            continue;
        }

        let Some(entry) = current.as_mut() else {
            continue;
        };

        let Some((name, data)) = parse_registry_value(line) else {
            continue;
        };

        match name.as_str() {
            "DistributionName" => entry.name = data,
            "BasePath" => entry.base_path = Some(PathBuf::from(data)),
            "VhdFileName" => entry.vhd_file_name = Some(data),
            "DefaultUid" => entry.default_uid = parse_dword(&data),
            "Version" => {
                entry.version = parse_dword(&data).and_then(|v| u8::try_from(v).ok());
            }
            // 其余键（State / Flags / Flavor / ShortcutPath …）暂时用不上。
            // 不是错误，忽略即可。
            _ => {}
        }
    }

    flush(&mut current, &mut entries);
    entries
}

/// 键路径的最后一段是不是 GUID（`{...}`）。
fn is_distro_key(path: &str) -> bool {
    path.rsplit('\\')
        .next()
        .is_some_and(|last| last.starts_with('{') && last.ends_with('}'))
}

/// 拆一行注册表值。
///
/// ```text
/// "    DefaultVersion    REG_DWORD    0x2"  →  ("DefaultVersion", "0x2")
/// "    BasePath    REG_SZ    D:\my path"   →  ("BasePath", "D:\my path")
/// ```
///
/// 用「找 `REG_` 类型词」来切，**不按固定列宽** ——
/// 值名长度不同、列宽会变，而数据里可能有空格（路径）。
fn parse_registry_value(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim();

    // 第一次出现的 `REG_` 就是类型词：值名里不会含它（我们要的那几个都不含）。
    let (name, rest) = trimmed.split_once("REG_")?;
    let name = name.trim();
    if name.is_empty() {
        return None;
    }

    // rest 形如 "DWORD    0x2" / "SZ    D:\my path"
    let (_kind, data) = rest.split_once(char::is_whitespace)?;

    Some((name.to_owned(), data.trim().to_owned()))
}

/// 解析 `0x2` / `0xf` 这类十六进制 DWORD。
fn parse_dword(text: &str) -> Option<u32> {
    let hex = text.trim();
    let hex = hex
        .strip_prefix("0x")
        .or_else(|| hex.strip_prefix("0X"))
        .unwrap_or(hex);
    u32::from_str_radix(hex, 16).ok()
}

// ---------------------------------------------------------------------------
// 合并：注册表信息 + 磁盘占用 → 发行版列表
// ---------------------------------------------------------------------------

/// 把注册表信息与磁盘占用合并进发行版列表。
///
/// 名字匹配**大小写不敏感** —— 理论上两边一致，但没必要因为大小写差异丢数据。
pub fn enrich(distros: &mut [Distro]) {
    let entries = read_registry();

    for distro in distros.iter_mut() {
        let Some(entry) = entries
            .iter()
            .find(|e| e.name.eq_ignore_ascii_case(&distro.name))
        else {
            continue;
        };

        distro.base_path = entry.base_path.clone();
        distro.default_uid = entry.default_uid;
        // `-l -v` 没给出可用版本时，用注册表的补上
        if distro.version.is_none() {
            distro.version = entry.version;
        }

        let Some(base) = entry.base_path.as_deref() else {
            continue;
        };
        let Some(vhdx) = find_vhdx(base, entry.vhd_file_name.as_deref()) else {
            continue;
        };
        distro.vhdx_bytes = file_len(&vhdx);
        distro.vhdx_path = Some(vhdx);
    }
}

/// 找到发行版的虚拟磁盘文件。
///
/// 优先用注册表报的 `VhdFileName`（实测 `ext4.vhdx`）——
/// 这比"扫目录里最大的 `.vhdx`"可靠（用户可能往目录里放了别的东西）。
/// 注册表没报时才退回到扫 `.vhdx`，并取最大的那个。
fn find_vhdx(base: &Path, file_name: Option<&str>) -> Option<PathBuf> {
    if let Some(name) = file_name {
        let candidate = base.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    let mut best: Option<(PathBuf, u64)> = None;
    for entry in std::fs::read_dir(base).ok()?.flatten() {
        let path = entry.path();
        let is_vhdx = path
            .extension()
            .map(|ext| ext.to_string_lossy().eq_ignore_ascii_case("vhdx"))
            .unwrap_or(false);
        if !is_vhdx {
            continue;
        }

        let len = entry.metadata().map(|m| m.len()).unwrap_or(0);
        // 不用 `Option::is_none_or` —— 它在 1.82 才稳定，
        // 而本 crate 声明的 MSRV 是 1.75。
        let better = match &best {
            None => true,
            Some((_, best_len)) => len > *best_len,
        };
        if better {
            best = Some((path, len));
        }
    }

    best.map(|(path, _)| path)
}

/// 文件长度（字节）；读不到时 `None`。
fn file_len(path: &Path) -> Option<u64> {
    std::fs::metadata(path).ok().map(|m| m.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 本机实测的 `reg.exe query ... /s` 输出。
    const REAL_REGISTRY: &str = include_str!("../../tests/fixtures/lxss_query.txt");

    #[test]
    fn parses_the_real_registry_dump() {
        let entries = parse_registry(REAL_REGISTRY);

        assert_eq!(entries.len(), 1, "根键不该被当成发行版条目：{entries:?}");

        let e = &entries[0];
        assert_eq!(e.name, "Ubuntu-26.04");
        assert_eq!(e.base_path.as_deref(), Some(Path::new(r"D:\linux\Ubuntu-26.04")));
        assert_eq!(e.vhd_file_name.as_deref(), Some("ext4.vhdx"));
        assert_eq!(e.default_uid, Some(0));
        assert_eq!(e.version, Some(2));
    }

    #[test]
    fn root_key_is_not_treated_as_a_distro() {
        let entries = parse_registry(REAL_REGISTRY);
        assert!(
            !entries.iter().any(|e| e.name.is_empty()),
            "空名字的条目应被丢弃"
        );
    }

    #[test]
    fn parses_multiple_distros() {
        let text = "\
\r
HKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\CurrentVersion\\Lxss\r
    DefaultVersion    REG_DWORD    0x2\r
\r
HKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\CurrentVersion\\Lxss\\{aaa}\r
    DistributionName    REG_SZ    Debian\r
    BasePath    REG_SZ    C:\\wsl\\Debian\r
    DefaultUid    REG_DWORD    0x3e8\r
    Version    REG_DWORD    0x2\r
\r
HKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\CurrentVersion\\Lxss\\{bbb}\r
    DistributionName    REG_SZ    Ubuntu 24.04 LTS\r
    BasePath    REG_SZ    D:\\linux\\Ubuntu 24.04 LTS\r
    Version    REG_DWORD    0x1\r
";
        let entries = parse_registry(text);
        assert_eq!(entries.len(), 2, "{entries:?}");

        assert_eq!(entries[0].name, "Debian");
        assert_eq!(entries[0].default_uid, Some(1000), "0x3e8 = 1000");
        assert_eq!(entries[1].name, "Ubuntu 24.04 LTS");
        assert_eq!(
            entries[1].base_path.as_deref(),
            Some(Path::new(r"D:\linux\Ubuntu 24.04 LTS")),
            "路径里的空格不该把值截断"
        );
        assert_eq!(entries[1].version, Some(1));
    }

    #[test]
    fn distro_key_entries_without_a_name_are_dropped() {
        let text = "\
HKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\CurrentVersion\\Lxss\\{ccc}\r
    BasePath    REG_SZ    C:\\empty\r
";
        assert!(parse_registry(text).is_empty());
    }

    #[test]
    fn is_distro_key_requires_a_guid_last_segment() {
        assert!(is_distro_key(
            r"HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Lxss\{856a6800-7118-4a80-b013-a88c95dc788f}"
        ));
        assert!(!is_distro_key(
            r"HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Lxss"
        ));
        // 半截大括号不算
        assert!(!is_distro_key(r"HKEY_CURRENT_USER\Software\{abc"));
    }

    #[test]
    fn registry_value_parsing_handles_every_type_prefix() {
        assert_eq!(
            parse_registry_value("    DefaultVersion    REG_DWORD    0x2"),
            Some(("DefaultVersion".to_owned(), "0x2".to_owned()))
        );
        assert_eq!(
            parse_registry_value("    BasePath    REG_SZ    D:\\wsl"),
            Some(("BasePath".to_owned(), "D:\\wsl".to_owned()))
        );
        assert_eq!(
            parse_registry_value("    ShortcutPath    REG_EXPAND_SZ    %USERPROFILE%\\x.lnk"),
            Some((
                "ShortcutPath".to_owned(),
                "%USERPROFILE%\\x.lnk".to_owned()
            ))
        );
        // 键头 / 空行 / 没有类型词的行都不该被当成值
        assert_eq!(parse_registry_value("HKEY_CURRENT_USER\\Software"), None);
        assert_eq!(parse_registry_value(""), None);
        assert_eq!(parse_registry_value("    REG_SZ    x"), None);
    }

    #[test]
    fn dword_parsing_accepts_hex_with_and_without_prefix() {
        assert_eq!(parse_dword("0x2"), Some(2));
        assert_eq!(parse_dword("0xf"), Some(15));
        assert_eq!(parse_dword("0x0"), Some(0));
        assert_eq!(parse_dword("0X10"), Some(16));
        assert_eq!(parse_dword("  0x3e8  "), Some(1000));
        assert_eq!(parse_dword("not-a-number"), None);
    }

    #[test]
    fn empty_registry_output_yields_nothing() {
        assert!(parse_registry("").is_empty());
        assert!(parse_registry("\r\n\r\n").is_empty());
    }

    #[test]
    fn default_distro_picks_the_starred_entry() {
        let distros = vec![
            Distro::new("Debian", crate::model::distro::DistroState::Stopped, Some(2), false),
            Distro::new("Ubuntu", crate::model::distro::DistroState::Running, Some(2), true),
        ];
        assert_eq!(default_distro(&distros), Some("Ubuntu"));
        assert_eq!(default_distro(&[]), None);
    }
}
