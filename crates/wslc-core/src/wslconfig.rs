//! `%USERPROFILE%\.wslconfig` 的读取与**静态检查**。
//!
//! # 这个文件是"全局"配置，和发行版内的 `/etc/wsl.conf` 不是一回事
//!
//! `.wslconfig` 管的是 **WSL2 虚拟机**（内存、网络模式、内核命令行……），
//! 整个机器一份；`/etc/wsl.conf` 管的是**某一个发行版**
//! （默认用户、是否把 Windows 的 PATH 塞进去、开机跑什么……），每个发行版一份。
//!
//! 把后者写进前者，WSL 会打一行"键未知"的告警然后**忽略它** ——
//! 配置看起来生效了、其实没有，这种问题最难查。
//!
//! # 为什么不提供"点一下校验"
//!
//! 实测（WSL 3.0.1.0）：
//!
//! ```text
//! wsl --status / --version / -l -v   → 不报
//! wsl --terminate / --set-default    → 不报
//! wsl --shutdown 之后第一条进发行版的命令 → **报**
//! 紧接着的第二、第三条               → 不报
//! ```
//!
//! 也就是说这些告警来自 **WSL2 虚拟机启动时读一次 `.wslconfig`**，
//! 不是每条命令都校验。想主动触发就得先 `wsl --shutdown` ——
//! 那会**打断所有正在跑的发行版**。为了校验一个配置文件付这个代价不值得，
//! 所以这里只做**不依赖 WSL** 的静态检查（见 [`check`]），
//! 并把它明确标注成"实测已知的几条"，而不是假装覆盖了全部。

use std::path::PathBuf;

use crate::error::Result;

/// `.wslconfig` 的完整路径。
///
/// 取自 `%USERPROFILE%`。取不到（环境变量缺失）时返回 `None` ——
/// 这时候不是"文件不存在"，而是**我们连它该在哪儿都不知道**。
pub fn config_path() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join(".wslconfig"))
}

/// 读 `.wslconfig`。
///
/// **文件不存在时返回 `Ok(None)`** —— 没配过这个文件是完全正常的状态，
/// 不该当成错误弹给用户看。
pub fn read() -> Result<Option<String>> {
    let Some(path) = config_path() else {
        return Ok(None);
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(crate::error::Error::Io(e)),
    }
}

/// 一条"键放错文件"的发现。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MisplacedKey {
    /// 行号（从 1 开始，和编辑器里看到的一致）。
    pub line: usize,
    /// 这一行的原文（已 trim）。
    pub text: String,
    /// 它**真正**该待的地方。
    pub belongs_to: &'static str,
    /// 为什么放这儿不生效。
    pub why: &'static str,
}

/// 实测确认会被 WSL 报"未知键"的组合：`(节, 键)`。
///
/// ⚠️ 这是**实测结果**，不是从文档推的 —— WSL 3.0.1.0 上这两条会打：
///
/// ```text
/// wsl: interop.appendWindowsPath:C:\Users\...\.wslconfig 中的键"12"未知
/// wsl: user.default:C:\Users\...\.wslconfig 中的键"15"未知
/// ```
///
/// 故意**只列这两条**：其余"应该也属于 `/etc/wsl.conf`"的键我没有逐一实测过，
/// 列出来就是在猜。宁可少报，不要错报 —— 错报会让用户改坏一个本来正常的配置。
const KNOWN_MISPLACED: &[(&str, &str)] = &[("interop", "appendWindowsPath"), ("user", "default")];

/// 静态检查：找出**实测已知**放错文件的键。
///
/// 纯函数（不碰文件系统、不跑 WSL），所以能脱离 Windows 单测。
///
/// 解析是**够用就好**的 INI：认 `[节]`、认 `键=值`、`#` 和 `;` 开头的当注释。
/// 不处理转义、续行、多行值 —— `.wslconfig` 的语法本来就很简单，
/// 而且这里只做"提示"，不做权威解析（权威的是 WSL 自己）。
pub fn check(text: &str) -> Vec<MisplacedKey> {
    let mut found = Vec::new();
    let mut section = String::new();

    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        let line_no = index + 1;

        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }

        // 节头
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].trim().to_ascii_lowercase();
            continue;
        }

        // 键值对
        let Some((key, _)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();

        if KNOWN_MISPLACED
            .iter()
            .any(|(s, k)| *s == section && *k == key)
        {
            found.push(MisplacedKey {
                line: line_no,
                text: line.to_owned(),
                belongs_to: "/etc/wsl.conf",
                why: "它管的是**某一个发行版**，不是整台机器的 WSL2 虚拟机",
            });
        }
    }

    found
}

/// 把静态检查的结论拼成一句给人看的话。
///
/// 没有发现问题时返回 `None` —— 调用方据此显示"没看出问题"，
/// 而不是一个空列表。
pub fn summary(misplaced: &[MisplacedKey]) -> Option<String> {
    if misplaced.is_empty() {
        return None;
    }
    let lines: Vec<String> = misplaced.iter().map(|m| m.line.to_string()).collect();
    Some(format!(
        "第 {} 行是 WSL 明确报过「未知键」的写法，它们属于 {}，放在 .wslconfig 里会被忽略。",
        lines.join("、"),
        misplaced[0].belongs_to
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 用户机器上那份真实的 `.wslconfig`（实测抓下来的），
    /// 拿它当夹具比编一份更有说服力。
    const REAL: &str = "\
[wsl2]
networkingMode=Mirrored
firewall=false
hardwarePerformanceCounters=false
kernelCommandLine=sysctl.vm.panic_on_oom=0

[experimental]
hostAddressLoopback=true
sparseVhd=true

[interop]
appendWindowsPath=false

[user]
default=wt
";

    #[test]
    fn finds_the_two_keys_wsl_actually_complains_about() {
        let found = check(REAL);
        assert_eq!(found.len(), 2, "{found:#?}");

        // 行号要准 —— 用户要照着它去编辑器里找
        assert_eq!(found[0].line, 12);
        assert_eq!(found[0].text, "appendWindowsPath=false");
        assert_eq!(found[1].line, 15);
        assert_eq!(found[1].text, "default=wt");

        for m in &found {
            assert_eq!(m.belongs_to, "/etc/wsl.conf");
            assert!(!m.why.is_empty());
        }
    }

    #[test]
    fn a_clean_config_reports_nothing() {
        let clean = "\
[wsl2]
memory=8GB
processors=4

[experimental]
autoMemoryReclaim=gradual
";
        assert!(check(clean).is_empty());
        assert!(summary(&check(clean)).is_none());
    }

    #[test]
    fn section_matters_not_just_the_key_name() {
        // `default` 在别的节里就不该报 —— 我们只认 `[user] default`
        let other = "[wsl2]\ndefault=true\n[interop]\nenabled=false\n";
        assert!(check(other).is_empty(), "{:#?}", check(other));

        // 反过来，键名对了但节不对也不报
        let wrong_section = "[experimental]\nappendWindowsPath=false\n";
        assert!(check(wrong_section).is_empty());
    }

    #[test]
    fn section_and_key_matching_ignores_case_and_spacing() {
        let sloppy = "[ Interop ]\n  AppendWindowsPath = false  \n";
        let found = check(sloppy);
        assert_eq!(found.len(), 1, "{found:#?}");
        assert_eq!(found[0].line, 2);
        // 原文保留（trim 过），方便用户比对
        assert_eq!(found[0].text, "AppendWindowsPath = false");
    }

    #[test]
    fn comments_and_blank_lines_do_not_shift_the_line_numbers() {
        // 行号必须按**文件里的行**算，注释和空行也算行 ——
        // 不然用户按我们给的行号去编辑器里找会找错地方。
        let text = "# 注释\n\n[wsl2]\nmemory=8GB\n\n; 另一条注释\n[user]\ndefault=wt\n";
        let found = check(text);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].line, 8, "{found:#?}");
    }

    #[test]
    fn lines_without_an_equals_sign_are_ignored() {
        // INI 里不该有这种行，但真出现了也不能 panic
        let weird = "[user]\n这不是键值对\ndefault=wt\n";
        let found = check(weird);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].line, 3);
    }

    #[test]
    fn summary_names_the_lines_and_the_right_file() {
        let found = check(REAL);
        let text = summary(&found).expect("应该有问题");
        assert!(text.contains("12"), "{text}");
        assert!(text.contains("15"), "{text}");
        assert!(text.contains("/etc/wsl.conf"), "{text}");
    }

    #[test]
    fn config_path_points_at_the_user_profile() {
        // 不假设 USERPROFILE 一定存在（CI 上未必有），只检查"拼对了"
        if let Some(path) = config_path() {
            assert!(path.ends_with(".wslconfig"), "{path:?}");
            assert_eq!(path.file_name().unwrap(), ".wslconfig");
        }
    }
}
