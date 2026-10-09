//! WSL 发行版（实例）的数据模型与**输出解析**。
//!
//! # 实测依据
//!
//! 全部来自本机 `WSL 3.0.1.0`，见 `docs/PLAN-v0.3.md` §3。
//!
//! ## `wsl -l -v`（`WSL_UTF8=1`，stdout 恰好 80 字节）
//!
//! ```text
//!   NAME            STATE           VERSION\r\n
//! * Ubuntu-26.04    Stopped         2\r\n
//! ```
//!
//! 逐字节：
//!
//! ```text
//! 20 20 4E 41 4D 45 20*12 53 54 41 54 45 20*11 56 45 52 53 49 4F 4E 0D 0A
//! 2A 20 55 62 75 6E 74 75 2D 32 36 2E 30 34 20*4 53 74 6F 70 70 65 64 20*9 32 0D 0A
//! ```
//!
//! 列起点是 2 / 18 / 34，但**不能写死**：NAME 列宽是 WSL 按最长名字算出来的。
//!
//! ## `wsl --status` 是**中文**的，和 `-l -v` 不是一种语言
//!
//! ```text
//! 默认分发: Ubuntu-26.04
//! 默认版本: 2
//! ```
//!
//! 同一个程序两种语言 → 解析器**不能按文字匹配**（见 [`WslStatus::parse`]）。

use std::path::PathBuf;

/// 发行版的运行状态。
///
/// `wsl -l -v` 的 STATE 列是**英文**枚举（实测）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DistroState {
    /// 正在运行。
    Running,
    /// 已停止。
    Stopped,
    /// 正在安装。
    Installing,
    /// 正在卸载。
    Uninstalling,
    /// 正在转换（WSL 1 ↔ 2）。
    Converting,
}

impl DistroState {
    /// 全部已知状态（导航 / 错误提示用）。
    pub const ALL: [DistroState; 5] = [
        DistroState::Running,
        DistroState::Stopped,
        DistroState::Installing,
        DistroState::Uninstalling,
        DistroState::Converting,
    ];

    /// 错误提示里列的候选值（与 [`DistroState::parse`] 接受的集合一致）。
    pub const TOKENS: &'static str = "Running/Stopped/Installing/Uninstalling/Converting";

    /// 从 STATE 列解析。
    ///
    /// 大小写不敏感 —— 不同 WSL 版本的大小写不保证一致。
    ///
    /// 认不出来时返回 `None`，**不猜**：猜错状态比少显示一行更糟。
    pub fn parse(token: &str) -> Option<Self> {
        match token.trim().to_ascii_lowercase().as_str() {
            "running" => Some(Self::Running),
            "stopped" => Some(Self::Stopped),
            "installing" => Some(Self::Installing),
            "uninstalling" => Some(Self::Uninstalling),
            "converting" => Some(Self::Converting),
            _ => None,
        }
    }

    /// 界面标签。
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "运行中",
            Self::Stopped => "已停止",
            Self::Installing => "安装中",
            Self::Uninstalling => "卸载中",
            Self::Converting => "转换中",
        }
    }

    /// 是否正在运行。
    pub fn is_running(self) -> bool {
        self == Self::Running
    }

    /// 是否是"过渡态"（安装 / 卸载 / 转换中）。
    ///
    /// 过渡态下**不能**对发行版做任何写操作 —— `wsl` 自己会拒绝，
    /// 界面应该把按钮灰掉而不是让用户点出一个错误。
    pub fn is_transitional(self) -> bool {
        matches!(self, Self::Installing | Self::Uninstalling | Self::Converting)
    }
}

/// 一个 WSL 发行版（实例）。
///
/// 前四个字段来自 `wsl -l -v`；后四个来自**注册表**
/// （`wsl` 命令不报告安装位置，只有注册表有，见 `docs/PLAN-v0.3.md` §3.4）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Distro {
    /// 发行版名。
    pub name: String,
    /// 运行状态。
    pub state: DistroState,
    /// WSL 版本（`1` / `2`）；解析不出来时为 `None`。
    pub version: Option<u8>,
    /// 是否是默认发行版（`wsl -l -v` 里的 `*`）。
    pub is_default: bool,

    /// 安装位置（注册表 `BasePath`）。
    ///
    /// 实测：`D:\linux\Ubuntu-26.04`。**这是唯一能拿到"装在哪个盘"的来源。**
    pub base_path: Option<PathBuf>,
    /// 根文件系统虚拟磁盘的路径。
    pub vhdx_path: Option<PathBuf>,
    /// 虚拟磁盘的**虚拟大小**（字节，即文件的 `Length`）。
    ///
    /// ⚠️ 这是虚拟大小，不是磁盘实际占用。实际可回收量要用
    /// `wsl --manage <name> --compact` 问（见 `docs/PLAN-v0.3.md` §3.5）。
    pub vhdx_bytes: Option<u64>,
    /// 默认用户 UID（注册表 `DefaultUid`）。
    pub default_uid: Option<u32>,
}

impl Distro {
    /// 用 `wsl -l -v` 那一行的信息构造；注册表字段留空。
    pub fn new(name: impl Into<String>, state: DistroState, version: Option<u8>, is_default: bool) -> Self {
        Self {
            name: name.into(),
            state,
            version,
            is_default,
            base_path: None,
            vhdx_path: None,
            vhdx_bytes: None,
            default_uid: None,
        }
    }

    /// 版本的显示文本。
    pub fn version_label(&self) -> String {
        match self.version {
            Some(v) => format!("WSL {v}"),
            None => "WSL ?".to_owned(),
        }
    }
}

/// 解析 `wsl -l -v` 的输出。
///
/// 返回 `(发行版列表, 警告)`。
///
/// # 为什么返回警告而不是直接报错
///
/// 一行解析不出来（比如微软加了新的状态值）**不该**让整个列表变空 ——
/// 那会让面板看起来像"一个发行版都没有"。所以这里跳过坏行、
/// 把原因收集起来，让界面能如实说明"有 N 行没认出来"。
///
/// # 解析策略
///
/// **在 token 里找已知的 STATE 枚举**，而不是按列偏移切：
///
/// - 列宽会随最长的名字变，写死偏移早晚会错；
/// - 名字里**可能有空格**（`Ubuntu 24.04 LTS`），按空白切也会错。
///
/// 找到 STATE 之后：它左边（去掉 `*`）整体是名字，最右边一个 token 是版本。
/// 认不出 STATE 的行直接跳过并记警告。
pub fn parse_distro_list(stdout: &str) -> (Vec<Distro>, Vec<String>) {
    let mut distros = Vec::new();
    let mut warnings = Vec::new();

    for (index, raw) in stdout.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }

        // 表头行：三个列名同时出现。比"第一行"稳 —— 前面可能有空行。
        if line.contains("NAME") && line.contains("STATE") && line.contains("VERSION") {
            continue;
        }

        match parse_distro_row(line) {
            Ok(distro) => distros.push(distro),
            Err(reason) => warnings.push(format!("第 {} 行没认出来（{reason}）：{line}", index + 1)),
        }
    }

    (distros, warnings)
}

/// 解析 `wsl -l -v` 的**一行**。
///
/// 失败时返回给用户看的原因（会进警告列表）。
fn parse_distro_row(line: &str) -> std::result::Result<Distro, String> {
    // 1) 默认发行版标记 `*`。注意实测里 `*` 后面还有一个空格。
    let (is_default, rest) = match line.strip_prefix('*') {
        Some(rest) => (true, rest.trim_start()),
        None => (false, line),
    };

    // 2) 按空白切分（会自动合并连续空格）
    let tokens: Vec<&str> = rest.split_whitespace().collect();

    // 最少三段：名字、状态、版本。
    // 用 `< 3` 而不是 `< 2`：两段的情况只可能是"名字为空"或"版本缺失"，
    // 两种都不该硬猜。
    if tokens.len() < 3 {
        return Err(format!(
            "字段不足（要至少「名字 状态 版本」三段，实际 {} 段）",
            tokens.len()
        ));
    }

    // 3) 在**中间**找状态：最左一段是名字的一部分，最右一段是版本，
    //    所以状态只可能在 `[1, len-1)` 这个区间里。
    //    用 `rposition` 从右往左找 —— 名字里含 "Running" 这类词时也不会误判。
    let state_index = tokens[1..tokens.len() - 1]
        .iter()
        .rposition(|token| DistroState::parse(token).is_some())
        .map(|offset| offset + 1)
        .ok_or_else(|| format!("状态列不在已知集合里（只认 {}）", DistroState::TOKENS))?;

    // 4) 左边是名字（可能含空格，所以 join 回去），右边是版本
    let name = tokens[..state_index].join(" ");
    let state = DistroState::parse(tokens[state_index]).expect("上一步刚判定过，必然成立");
    let version = tokens[tokens.len() - 1].parse::<u8>().ok();

    Ok(Distro::new(name, state, version, is_default))
}

/// `wsl --status` 的关键信息。
///
/// ⚠️ 这只是**兜底**来源。默认发行版的权威来源是
/// `wsl -l -v` 里那个 `*`（英文、好解析）；
/// `--status` 的价值在于还能拿到「默认版本」。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WslStatus {
    /// 默认发行版名。
    pub default_distro: Option<String>,
    /// 新发行版的默认 WSL 版本。
    pub default_version: Option<u8>,
}

impl WslStatus {
    /// 从 `wsl --status` 的输出解析。
    ///
    /// # 为什么不匹配"默认分发"这几个字
    ///
    /// 实测中文系统输出 `默认分发: Ubuntu-26.04`，
    /// 英文系统是 `Default Distribution: ...` —— 文字会变，**结构不变**：
    /// 永远是「键: 值」。所以这里只按冒号切，然后看**值**长什么样：
    ///
    /// - 纯数字 → 默认版本
    /// - 其余 → 默认发行版名
    ///
    /// 全角冒号 `：` 也顺手兼容（有些本地化版本会用）。
    pub fn parse(stdout: &str) -> Self {
        let mut status = Self::default();

        for line in stdout.lines() {
            let Some((_key, value)) = line
                .split_once(':')
                .or_else(|| line.split_once('：'))
            else {
                continue;
            };

            let value = value.trim();
            if value.is_empty() {
                continue;
            }

            // 只取第一次出现的值：`--status` 里没有重复键，
            // 但万一将来加了别的数字行，也别把默认版本冲掉。
            match value.parse::<u8>() {
                Ok(version) => {
                    if status.default_version.is_none() {
                        status.default_version = Some(version);
                    }
                }
                Err(_) => {
                    if status.default_distro.is_none() {
                        status.default_distro = Some(value.to_owned());
                    }
                }
            }
        }

        status
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 本机实测的原始输出。
    ///
    /// 实测是 **80 字节、行尾 `\r\n`**；fixture 里存成 LF（78 字节）——
    /// 解析器必须对两种行尾等价，这一点由
    /// [`handles_lf_and_crlf_identically`] 单独盯着。
    const REAL_LIST: &str = include_str!("../../tests/fixtures/distro_list.txt");

    #[test]
    fn parses_the_real_machine_output() {
        let (distros, warnings) = parse_distro_list(REAL_LIST);
        assert!(warnings.is_empty(), "实测样本不该有警告：{warnings:?}");
        assert_eq!(distros.len(), 1);

        let d = &distros[0];
        assert_eq!(d.name, "Ubuntu-26.04");
        assert_eq!(d.state, DistroState::Stopped);
        assert_eq!(d.version, Some(2));
        assert!(d.is_default, "`*` 前缀应被识别为默认发行版");
    }

    #[test]
    fn header_line_is_not_mistaken_for_a_distro() {
        let (distros, warnings) = parse_distro_list(REAL_LIST);
        assert!(warnings.is_empty());
        assert!(
            !distros.iter().any(|d| d.name.contains("NAME")),
            "表头不该被当成发行版：{distros:?}"
        );
    }

    #[test]
    fn parses_multiple_distros_including_a_name_with_spaces() {
        let text = include_str!("../../tests/fixtures/distro_list_multi.txt");
        let (distros, warnings) = parse_distro_list(text);
        assert!(warnings.is_empty(), "不该有警告：{warnings:?}");

        let names: Vec<&str> = distros.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["Ubuntu 24.04 LTS", "Debian", "kali-linux"],
            "名字里有空格也要完整取出来"
        );

        assert_eq!(distros[0].state, DistroState::Running);
        assert!(!distros[0].is_default);
        assert_eq!(distros[1].state, DistroState::Stopped);
        assert!(distros[1].is_default);
        assert_eq!(distros[2].state, DistroState::Installing);
        assert_eq!(distros[2].version, Some(1));
    }

    #[test]
    fn distro_named_like_a_state_does_not_confuse_the_parser() {
        // 名字叫 "Running"、状态是 "Stopped" —— 必须取**最右**那个能认出的状态
        let text = "  NAME    STATE      VERSION\n* Running  Stopped    2\n";
        let (distros, warnings) = parse_distro_list(text);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(distros.len(), 1);
        assert_eq!(distros[0].name, "Running");
        assert_eq!(distros[0].state, DistroState::Stopped);
    }

    #[test]
    fn unknown_state_is_skipped_with_a_warning_not_guessed() {
        // 微软将来加了新状态值时，宁可少显示一行，也不要显示错
        let text = "  NAME    STATE      VERSION\n  Ubuntu  Frozen     2\n  Debian  Running    2\n";
        let (distros, warnings) = parse_distro_list(text);

        assert_eq!(distros.len(), 1, "认不出的行应被跳过");
        assert_eq!(distros[0].name, "Debian");
        assert_eq!(warnings.len(), 1, "跳过的行要留下原因");
        assert!(warnings[0].contains("Frozen"), "{warnings:?}");
        assert!(warnings[0].contains("第 2 行"), "{warnings:?}");
    }

    #[test]
    fn empty_output_yields_empty_list_and_no_warnings() {
        // 一个发行版都没有时，`wsl -l -v` 只输出表头（或干脆没输出）
        for text in ["", "\r\n", "  NAME  STATE  VERSION\r\n"] {
            let (distros, warnings) = parse_distro_list(text);
            assert!(distros.is_empty(), "{text:?}");
            assert!(warnings.is_empty(), "{text:?} → {warnings:?}");
        }
    }

    #[test]
    fn handles_lf_and_crlf_identically() {
        let crlf = "  NAME  STATE  VERSION\r\n  Ubuntu  Running  2\r\n";
        let lf = "  NAME  STATE  VERSION\n  Ubuntu  Running  2\n";
        assert_eq!(parse_distro_list(crlf), parse_distro_list(lf));
    }

    #[test]
    fn state_parsing_is_case_insensitive_and_rejects_junk() {
        assert_eq!(DistroState::parse("running"), Some(DistroState::Running));
        assert_eq!(DistroState::parse("RUNNING"), Some(DistroState::Running));
        assert_eq!(DistroState::parse(" Stopped "), Some(DistroState::Stopped));
        assert_eq!(DistroState::parse(""), None);
        assert_eq!(DistroState::parse("Frozen"), None);
    }

    #[test]
    fn transitional_states_are_flagged() {
        assert!(DistroState::Installing.is_transitional());
        assert!(DistroState::Uninstalling.is_transitional());
        assert!(DistroState::Converting.is_transitional());
        assert!(!DistroState::Running.is_transitional());
        assert!(!DistroState::Stopped.is_transitional());
    }

    #[test]
    fn every_state_has_a_label_and_is_parseable_from_its_own_name() {
        for state in DistroState::ALL {
            assert!(!state.label().is_empty());
            // `ALL` 与 `parse` 接受的集合必须一致，否则错误提示会骗人
            let token = format!("{state:?}");
            assert_eq!(
                DistroState::parse(&token),
                Some(state),
                "{token} 应该能被 parse 认出来"
            );
        }
    }

    #[test]
    fn version_label_tolerates_a_missing_version() {
        let d = Distro::new("x", DistroState::Stopped, None, false);
        assert_eq!(d.version_label(), "WSL ?");
        let d = Distro::new("x", DistroState::Stopped, Some(2), false);
        assert_eq!(d.version_label(), "WSL 2");
    }

    // -- `wsl --status` ----------------------------------------------------

    #[test]
    fn parses_the_real_status_output() {
        let text = include_str!("../../tests/fixtures/wsl_status.txt");
        let status = WslStatus::parse(text);
        assert_eq!(status.default_distro.as_deref(), Some("Ubuntu-26.04"));
        assert_eq!(status.default_version, Some(2));
    }

    #[test]
    fn status_parsing_does_not_depend_on_the_language() {
        // 英文系统的输出，结构一样、文字不同 —— 必须解析出同样的结果
        let english = "Default Distribution: Ubuntu-26.04\nDefault Version: 2\n";
        let chinese = "默认分发: Ubuntu-26.04\n默认版本: 2\n";
        assert_eq!(WslStatus::parse(english), WslStatus::parse(chinese));
    }

    #[test]
    fn status_parsing_accepts_fullwidth_colon() {
        let status = WslStatus::parse("默认分发：Ubuntu-26.04\n默认版本：2\n");
        assert_eq!(status.default_distro.as_deref(), Some("Ubuntu-26.04"));
        assert_eq!(status.default_version, Some(2));
    }

    #[test]
    fn status_parsing_of_empty_output_is_all_none() {
        assert_eq!(WslStatus::parse(""), WslStatus::default());
        // 只有冒号没有值
        assert_eq!(WslStatus::parse("默认分发:\n默认版本:\n"), WslStatus::default());
    }
}
