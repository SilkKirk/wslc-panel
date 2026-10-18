//! 「添加实例」的纯逻辑：名称与路径推导、装前检查、**执行计划**。
//!
//! # 为什么要有"计划"这一层
//!
//! v0.3 那版把安装当成"一两条命令"（`InstallSpec::to_args` + `preview_lines`）。
//! 对齐参考实现之后，在线安装最多要 8 步（装 → 等注册 → 导出 → 注销 → 导入 →
//! 删临时文件 → 开稀疏 → 设默认），镜像站还要先下载。**"一条命令"这个抽象已经不成立了**；
//! 硬套下去只能让"预览"和"执行"各写一份拼参数逻辑 —— 而它们一旦走偏，
//! 界面上那句"与实际执行完全一致"就成了假话。
//!
//! 所以这里产出的是一个**步骤列表**：界面逐条渲染当预览，执行器逐条执行，
//! 两边读的是同一份数据。（执行在 [`crate::cmd::install`]。）
//!
//! # 为什么这些函数必须是纯的
//!
//! 本机没有 Rust 工具链、CI 里 `core` 任务才是唯一能跑测试的地方，而它**不连 GPUI**。
//! 所以名称推导、重名判断、参数拼装、两个在线列表的解析全都不起进程、不碰文件系统、
//! 不看 `cfg(windows)` —— 只吃字符串。这样它们才能被真正验证（`AGENTS.md` §1、§2）。
//!
//! 只有一处例外：[`PlanContext::default`] 会用 `%TEMP%` 和进程 id 拼一个临时目录，
//! 那也是为了"计划里能写出临时文件的完整路径"（用户要能看见它）。

use std::path::PathBuf;

/// **本项目只支持 WSL 2。**
///
/// 所有"新建发行版"的路径都**显式**传 `--version 2`，而不是依赖 WSL 的默认值 ——
/// 默认值（`wsl --set-default-version`）是可以被用户改成 1 的，
/// 那样建出来的发行版本程序管不了，用户还会以为是程序坏了。
///
/// ⚠️ 例外：`--install --from-file`（[`InstallSource::File`]）**没有**这个选项
/// （实测 `wsl.exe --help`），版本由安装器自己决定 —— 这一条只能如实承认。
pub const WSL_VERSION: u8 = 2;

/// 发行版名长度上限。
///
/// 参考实现也是 25：超长的名字在 `wsl -l -v` 的表格里会把列挤歪，
/// 而且它同时是注册表键名和默认安装目录名的一部分。
pub const MAX_NAME_LEN: usize = 25;

// ---------------------------------------------------------------------------
// 名称
// ---------------------------------------------------------------------------

/// 名字里允许出现的字符。
///
/// 只放开 A-Z a-z 0-9 `.` `_` `-`：这些字符在注册表键、目录名、命令行里
/// 都不会被任何一层解释成别的东西。中文、空格、`\` `/` 一律不接受 ——
/// 它们的表现（编码、路径分隔、shell 解释）随环境变化，而发行版名是**永久**的。
fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')
}

/// 把任意文本收拾成一个能当发行版名的字符串（参考实现的规则）。
///
/// 规则：只留 `[A-Za-z0-9.]`；`-` / `_` / 空白折叠成**一个** `-`；
/// 去掉结尾的 `-` 和 `.`；截断到 [`MAX_NAME_LEN`]。
///
/// `_` 也变成 `-` 是刻意的：两种连字符混用只会让用户在别处（注册表、目录名）
/// 分不清哪个才是原名。
pub fn sanitize_name(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut last_was_hyphen = false;

    for c in raw.chars() {
        if c.is_ascii_alphanumeric() || c == '.' {
            out.push(c);
            last_was_hyphen = false;
        } else if (c == '-' || c == '_' || c.is_whitespace()) && !last_was_hyphen && !out.is_empty()
        {
            out.push('-');
            last_was_hyphen = true;
        }
    }

    while out.ends_with(['-', '.']) {
        out.pop();
    }
    // 只留 ASCII 之后 `len()` 就是字符数，截断不会落在多字节字符中间。
    if out.len() > MAX_NAME_LEN {
        out.truncate(MAX_NAME_LEN);
        while out.ends_with(['-', '.']) {
            out.pop();
        }
    }
    out
}

/// 从用户选中的文件路径猜一个发行版名。
///
/// 读的是**文件名**那一截（`\` 和 `/` 都认），然后：
///
/// 1. 剥掉已知后缀（`.tar.gz` / `.tar.xz` / `.tar` / `.wsl` / `.vhdx` …，大小写不敏感）；
/// 2. 删掉出现的 `rootfs`（各镜像站的文件名里几乎都有它）；
/// 3. 按 `-` 切开，**遇到平台或包装关键词就停**（`wsl` / `amd64` / `arm64` /
///    `x86_64` / `with` / `docker` / `vhdx` / `image`）—— 后面那些是"这个包是什么"，
///    不是"这是什么发行版"；
/// 4. 最后走一遍 [`sanitize_name`]。
///
/// ⚠️ 这是**猜**。结果可能仍然带版本尾巴（比如 `debian-12-genericcloud`），
/// 界面上把结果填进输入框、用户随时能改 —— 不要假装它一定对。
pub fn suggest_name_from_file(file: &str) -> String {
    // 只取文件名（Windows 的 `\` 与 `/` 都可能出现）
    let file_name = file.rsplit(['\\', '/']).next().unwrap_or(file);

    let mut stem = file_name.to_owned();
    // 从长到短，避免 `.tar.gz` 被 `.gz` 先吃掉
    for suffix in [
        ".tar.gz", ".tar.xz", ".tar.zst", ".tar.bz2", ".tar.lz4", ".tgz", ".txz", ".tar", ".wsl",
        ".vhdx", ".vhd",
    ] {
        if let Some(stripped) = strip_suffix_ascii_ci(&stem, suffix) {
            stem = stripped;
            break;
        }
    }

    stem = remove_all_ascii_ci(&stem, "rootfs");

    // 到这里就停：后面是平台/包装信息，不是发行版名。
    const STOP_WORDS: [&str; 8] = [
        "wsl", "amd64", "arm64", "x86_64", "with", "docker", "vhdx", "image",
    ];

    let parts: Vec<&str> = stem
        .split('-')
        .take_while(|part| {
            let lowered = part.to_ascii_lowercase();
            !STOP_WORDS.iter().any(|word| lowered.contains(word))
        })
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();

    if parts.is_empty() {
        sanitize_name(&stem)
    } else {
        sanitize_name(&parts.join("-"))
    }
}

/// 剥掉一个**纯 ASCII** 的后缀（大小写不敏感）；不匹配时返回 `None`。
///
/// 刻意不用 `to_lowercase()` 再比长度：Unicode 的小写化会改变字节长度
/// （比如 `İ` → `i̇`），按那个长度去切原串会切在多字节字符中间直接 panic。
/// 逐字节比较不会有这个问题 —— ASCII 的字节值都 < 0x80，UTF-8 的多字节序列
/// 里每个字节都 ≥ 0x80，永远不可能误配。
fn strip_suffix_ascii_ci(text: &str, suffix: &str) -> Option<String> {
    if text.len() < suffix.len() {
        return None;
    }
    let split_at = text.len() - suffix.len();
    if !text.is_char_boundary(split_at) {
        return None;
    }
    if text[split_at..].eq_ignore_ascii_case(suffix) {
        Some(text[..split_at].to_owned())
    } else {
        None
    }
}

/// 删掉所有出现的 `needle`（大小写不敏感，同样只用于 ASCII 的 needle）。
fn remove_all_ascii_ci(haystack: &str, needle: &str) -> String {
    let bytes = haystack.as_bytes();
    let needle_bytes = needle.as_bytes();
    if needle_bytes.is_empty() || bytes.len() < needle_bytes.len() {
        return haystack.to_owned();
    }

    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if i + needle_bytes.len() <= bytes.len()
            && bytes[i..i + needle_bytes.len()].eq_ignore_ascii_case(needle_bytes)
        {
            i += needle_bytes.len();
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }

    // 我们只删 ASCII 字节，剩下的仍然是合法 UTF-8；真出意外就原样返回，绝不 panic。
    String::from_utf8(out).unwrap_or_else(|_| haystack.to_owned())
}

/// 名字本身的合法性（不查重名）。
fn name_syntax_errors(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    if name.is_empty() {
        out.push("发行版名不能为空".to_owned());
        return out;
    }
    let chars = name.chars().count();
    if chars > MAX_NAME_LEN {
        out.push(format!(
            "发行版名不能超过 {MAX_NAME_LEN} 个字符（现在是 {chars} 个），它在 WSL 的列表里会把列挤歪"
        ));
    }
    if let Some(bad) = name.chars().find(|c| !is_name_char(*c)) {
        out.push(format!(
            "发行版名里不能有「{bad}」—— 只允许 A-Z a-z 0-9 . _ -"
        ));
    }
    out
}

/// 本地文件的**种类** —— 「添加实例」的本地来源只有**一个入口**，靠后缀分岔。
///
/// 三种文件在 WSL 那边是**三条不同的路**（`--import` / `--import --vhd` /
/// `--install --from-file`），所以界面虽然合一，数据层仍然要分开
/// （[`InstallSource`] 的三个变体）。
///
/// 参考实现（`wsl-dashboard-ref`）把 rootfs 与 VHDX 分成两个来源、而且没有
/// `.wsl` 那条；我们是**合一**的：用户手上拿到一个文件，本来就不该先去想
/// "它属于哪一类"。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalFileKind {
    /// `tar` / `tar.gz` / `tar.xz` … → `wsl --import`（只铺开文件系统）。
    Tar,
    /// `.vhdx` / `.vhd` → `wsl --import --vhd`（会**拷贝**一份到安装目录）。
    Vhdx,
    /// `.wsl` → `wsl --install --from-file`（会做首次启动初始化）。
    Bundle,
}

impl LocalFileKind {
    /// 界面上那句"识别为 …"（用户得能看出我们**猜**成了什么）。
    pub fn label(self) -> &'static str {
        match self {
            Self::Tar => "tar 归档 → 用 `wsl --import` 铺开",
            Self::Vhdx => "VHDX 虚拟磁盘 → 用 `wsl --import --vhd` 拷贝导入",
            Self::Bundle => ".wsl 安装包 → 交给 WSL 安装器（会做首次启动初始化）",
        }
    }
}

/// 从文件名后缀判断它是哪一类（认不出来时返回 `None`）。
///
/// 只认后缀、不看文件内容：读文件头要碰磁盘（可能是网络盘），而这一步在
/// **渲染时**也会被调用（用户一边打字一边要看到"识别为 …"）——
/// 和"安装目录非空"那条一样，碰磁盘的检查只能在提交那一刻做
/// （见 `views::add_instance` 的说明）。
///
/// 大小写不敏感；`.tar.gz` / `.tar.xz` 这类复合后缀按**长到短**匹配，
/// 免得 `.gz` 先把 `.tar.gz` 吃掉。
pub fn classify_local_file(path: &str) -> Option<LocalFileKind> {
    // 只取文件名那一截（`\` 和 `/` 都认）
    let file_name = path.trim().rsplit(['\\', '/']).next().unwrap_or("");
    if file_name.is_empty() {
        return None;
    }

    // `.wsl` 与 `.vhdx` 先判（它们不会和 tar 的复合后缀冲突）
    for (suffix, kind) in [
        (".wsl", LocalFileKind::Bundle),
        (".vhdx", LocalFileKind::Vhdx),
        (".vhd", LocalFileKind::Vhdx),
    ] {
        if strip_suffix_ascii_ci(file_name, suffix).is_some() {
            return Some(kind);
        }
    }

    for suffix in [
        ".tar.gz", ".tar.xz", ".tar.zst", ".tar.bz2", ".tar.lz4", ".tgz", ".txz", ".tar",
    ] {
        if strip_suffix_ascii_ci(file_name, suffix).is_some() {
            return Some(LocalFileKind::Tar);
        }
    }

    None
}

// ---------------------------------------------------------------------------
// 安装目录
// ---------------------------------------------------------------------------

/// 是不是 Windows 绝对路径：`D:\...`、`D:/...` 或 UNC `\\server\share`。
///
/// 刻意**不用** `Path::is_absolute()`：它在非 Windows 上对 `D:\wsl` 返回
/// `false`，而这条校验的结论不该随编译平台变（CI 与开发机要一致）。
pub fn is_absolute_windows_path(text: &str) -> bool {
    let bytes = text.as_bytes();
    // 盘符形式：`D:\` / `D:/`
    if bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return matches!(bytes[2], b'\\' | b'/');
    }
    // UNC 形式：`\\server\share`
    text.starts_with("\\\\")
}

/// 由"默认安装目录 + 发行版名"推出安装目录。
///
/// 分隔符**跟着默认目录自己**的写法走（`D:/wsl` 就继续用 `/`）：
/// 混着写虽然 Windows 认，但用户复制到终端、脚本里就会看着别扭。
/// 默认目录为空或名字为空时给 `None` —— 调用方据此要求用户自己填。
pub fn derive_install_dir(default_dir: Option<&str>, name: &str) -> Option<String> {
    let base = default_dir?.trim();
    let name = name.trim();
    if base.is_empty() || name.is_empty() {
        return None;
    }
    let sep = if base.contains('/') && !base.contains('\\') {
        '/'
    } else {
        '\\'
    };
    let base = base.trim_end_matches(['\\', '/']);
    Some(format!("{base}{sep}{name}"))
}

// ---------------------------------------------------------------------------
// 在线发行版列表
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// 安装来源与计划
// ---------------------------------------------------------------------------

/// 新发行版的**来源**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallSource {
    /// 本地 tar 文件 → `wsl --import <name> <dir> <file> --version 2`。
    ///
    /// 最可靠的一条路：**不联网也能用**，只是把文件系统铺开。
    Tar {
        /// tar 文件路径。
        path: String,
    },
    /// 本地 VHDX → `wsl --import <name> <dir> <file> --vhd --version 2`。
    ///
    /// `--vhd` 让 WSL 把给定的虚拟磁盘**拷**到安装位置（不是就地用）。
    /// 想就地注册已存在的 ext4 VHDX，那是另一个命令（`--import-in-place`），
    /// 本程序暂时不做 —— 它会动用户的原始文件，风险等级不同。
    Vhdx {
        /// vhdx 文件路径。
        path: String,
    },
    /// 本地文件，交给 WSL 自己的安装器 → `wsl --install --from-file`。
    ///
    /// 和 [`InstallSource::Tar`] 的区别不只是参数：`--import` 只是把文件系统
    /// 铺开，而 `--install` 走的是商店安装器那套，会做首次启动初始化
    /// （建默认用户等）。同一个 tar，两条路的结果不一样。
    ///
    /// ⚠️ 这条**没有** `--version` 选项（实测 `wsl.exe --help`），
    /// 版本由安装器自己决定 —— 我们想显式指定也指定不了。
    File {
        /// 安装文件路径（`.wsl` / `tar.gz` …）。
        path: String,
    },
    /// 在线安装（微软商店 / GitHub）。
    ///
    /// `id` **必须**是在线清单里的名字（`wsl -l -o` 那一列），
    /// 因为 `wsl --install -d` 只认它。用户想要别的名字时，
    /// WSL 装完的那个 `id` 会被重定位成 [`InstallSpec::name`]（计划里的
    /// `EnsureRelocated` 那一步）。
    Online {
        /// 在线清单里的发行版名（`wsl --install -d <id>`）。
        id: String,
        /// 装完是否立刻启动。
        ///
        /// 默认**不**启动：安装动辄十几分钟，装完自己弹一个终端出来很突兀。
        launch: bool,
        /// 走 `--web-download`（从网络下，而不是微软商店）。
        ///
        /// 参考实现会先探一下 GitHub 通不通再决定；本程序把探测结果当**默认值**，
        /// 但按钮在界面上，用户可以改。
        web_download: bool,
        /// 这条装不上时，改用哪个 id 再试一次。
        ///
        /// 界面上算好传进来（`OnlineDistroState::sibling_id`）：用户选了裸家族名
        /// （`Ubuntu`）时给它**带版本号的兄弟**（`Ubuntu-26.04`）。
        /// 起因见 [`crate::model::install::PlannedStep::retry_other_id`] 的说明 ——
        /// 实测本机 `-d Ubuntu` 必失败、`-d Ubuntu-24.04` 正常。
        fallback_id: Option<String>,
    },
    /// 镜像源：下载官方 rootfs / `.wsl` 包，再装成本地发行版。
    ///
    /// `url` 是**已经选定的**那一个（探测最快镜像的结果，或用户手填的）。
    /// 选哪一个是界面的活（要测速、要显示结果），计划里只记结论 ——
    /// 这样计划是确定的，预览才能和执行完全一致。
    ///
    /// # `format` 决定装法
    ///
    /// 清单里的来源有两种（实测 `wslui` 那份清单：Ubuntu 24.04 的 13 个来源里
    /// 10 个是 `.wsl`）：
    ///
    /// - `tar.*` → `wsl --import`（只是铺开文件系统）；
    /// - `wsl` → **新格式的 `.wsl` 包**，`--import` 吃不了，要走
    ///   `wsl --install --from-file`（会做首次启动初始化）。
    ///
    /// 参考实现一律按 `--import` 处理 —— 它挑到 `.wsl` 来源时必然失败。
    Mirror {
        /// 下载地址。
        url: String,
        /// 镜像站名（只用于显示，比如 `lxc-tuna` / `tsinghua`）。
        mirror: String,
        /// 发行版版本（只用于显示，比如 `24.04`）。
        release: String,
        /// 打包格式：`tar.xz` / `tar.gz` / `wsl`（空串按后缀猜）。
        format: String,
    },
}

impl InstallSource {
    /// 镜像源这条是不是 `.wsl` 包（装法与 tar 不同）。
    pub fn mirror_is_bundle(format: &str, url: &str) -> bool {
        format.eq_ignore_ascii_case("wsl") || url.to_lowercase().ends_with(".wsl")
    }
    /// 这个来源需不需要用户指定**安装目录**。
    ///
    /// 在线安装可以留空（WSL 有自己的默认位置）—— 但一旦要改名，
    /// 就必须有目录（重定位那一步的 `--import` 要它），校验里单独管这件事。
    pub fn requires_install_dir(&self) -> bool {
        matches!(
            self,
            Self::Tar { .. } | Self::Vhdx { .. } | Self::Mirror { .. }
        )
    }

    /// 这个来源需不需要一个**本地文件路径**。
    pub fn needs_path(&self) -> bool {
        matches!(
            self,
            Self::Tar { .. } | Self::Vhdx { .. } | Self::File { .. }
        )
    }

    /// 支不支持"装完立即启动"（只有在线安装有 `--no-launch`）。
    pub fn supports_launch(&self) -> bool {
        matches!(self, Self::Online { .. })
    }
}

/// 「添加实例」的全部参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallSpec {
    /// 用户想要的**最终**发行版名。
    pub name: String,
    /// 安装目录。留空表示"用默认目录 + 名字"（见 [`InstallSpec::effective_install_dir`]）。
    pub install_dir: String,
    /// 来源。
    pub source: InstallSource,
    /// 装完是否设为默认。
    ///
    /// ⚠️ 这**不是**一个命令行选项 —— `--import` 和 `--install` 都不接受
    /// `--set-default`（实测 `wsl.exe --help`）。所以它是计划里**最后一步**
    /// 单独的一条 `wsl --set-default`。
    pub set_default: bool,
}

impl InstallSpec {
    /// 新建（只给名字和来源，其余字段用默认值）。
    pub fn new(name: impl Into<String>, source: InstallSource) -> Self {
        Self {
            name: name.into(),
            install_dir: String::new(),
            source,
            set_default: false,
        }
    }

    /// 实际会用到的安装目录：用户填了就用它，没填就用"默认目录 + 名字"。
    pub fn effective_install_dir(&self, ctx: &PlanContext) -> Option<String> {
        let dir = self.install_dir.trim();
        if !dir.is_empty() {
            return Some(dir.to_owned());
        }
        derive_install_dir(ctx.default_dir.as_deref(), self.name.trim())
    }
}

/// 计划阶段需要知道的**环境**（都不是用户填的，所以单独一个结构）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanContext {
    /// 「默认安装目录」偏好。
    pub default_dir: Option<String>,
    /// 临时目录（镜像站下载、在线安装重定位时的中转 tar 都放这儿）。
    pub temp_dir: String,
    /// 临时文件名里的一段唯一标记（进程 id + 时间戳）。
    ///
    /// 计划里要写出**具体的**临时文件路径，用户才看得见"会往哪写"；
    /// 但也因此不能让两次安装撞同一个文件名。
    pub stamp: String,
    /// `.wslconfig` 里是否要求新发行版用稀疏 VHD（`[experimental] sparseVhd=true`）。
    ///
    /// 开着的话，装完补一条 `wsl --manage <name> --set-sparse true` ——
    /// 那是用户自己在 WSL 配置里要求的，不是我们自作主张（参考实现也这么做）。
    pub wslconfig_sparse: bool,
}

impl Default for PlanContext {
    fn default() -> Self {
        let stamp = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
        );
        Self {
            default_dir: None,
            temp_dir: std::env::temp_dir()
                .join("wslc-panel")
                .to_string_lossy()
                .into_owned(),
            stamp,
            wslconfig_sparse: false,
        }
    }
}

/// 计划里一步要用什么去执行。
///
/// 前四个是"起一个进程"；后两个是**动作**（执行器自己实现，不是命令行）。
/// 把它们放进同一个枚举，是为了让"预览"能按**真实顺序**列出全部步骤 ——
/// 只列命令的话，用户会以为中间那些"等待/重定位"不存在。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanProgram {
    /// 起 `wsl.exe`，`args` 是它的参数。
    Wsl,
    /// 起 `curl.exe`，`args` 是它的参数。
    Curl,
    /// `std::fs::create_dir_all(args[0])`。
    CreateDir,
    /// `std::fs::remove_file(args[0])`（文件不存在也算成功）。
    RemoveFile,
    /// 等发行版注册完成：轮询 `wsl -l -q` 直到 `args[0]` 出现。
    WaitRegistered,
    /// **必要时**重定位：`args = [源名, 目标名, 目标目录, 中转 tar 路径]`。
    ///
    /// 装完发现"名字/位置已经正好了"就什么也不做；否则
    /// `--export` → `--unregister` → `--import`。这一段**不可取消**：
    /// 走到 `--unregister` 之后被取消，等于把刚装好的东西删了。
    EnsureRelocated,
}

/// 计划里的一步。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedStep {
    /// 这一步在干什么（给人看的一句话）。
    pub label: String,
    /// 用什么执行。
    pub program: PlanProgram,
    /// 参数（含义随 `program` 变，见 [`PlanProgram`]）。
    pub args: Vec<String>,
    /// 能不能被用户取消。
    ///
    /// `false` 的只有重定位那一步 —— 它中途被打断会**丢数据**。
    /// 界面据此不给"取消"按钮，而不是让用户点了之后才发现没用。
    pub cancellable: bool,
    /// 失败时要不要**换一个下载源**（商店 ⇄ `--web-download`）重试一次。
    ///
    /// 只给在线安装那一步用。两条通道走的是**不同的下载实现**，
    /// 所以一条挂了另一条可能通：实测（2026-10-10）本机
    /// `--location` 那条通道秒报 `Wsl/InstallDistro/WININET_E_CANNOT_CONNECT`，
    /// 而参考实现那条（不带 `--location`）能连上。
    ///
    /// 用户看到的失败信息是"无法与服务器建立连接"，但**换一条路可能就好了** ——
    /// 与其让他自己去猜，不如自动换一次。
    pub retry_other_source: bool,
    /// 这一步失败**不算安装失败**（只记一条警告）。
    ///
    /// 给"收尾"步骤用：那一刻发行版已经装好了，比如"按 `.wslconfig` 开稀疏盘"
    /// —— WSL 完全可能因为自己的策略拒绝它（实测它默认就拒绝），
    /// 把整体判成失败会让人以为白装了（界面报"安装失败"、其实好好的）。
    pub best_effort: bool,
    /// 失败时换成**另一个 id** 重试（`-d` 后面那个名字）。
    ///
    /// 只给在线安装那一步用，而且只在"用户选的是裸家族名"时才给。
    /// 起因是实测（2026-10-11，本机）：`wsl -l -o` 排在**第一位**的裸 `Ubuntu`
    /// 是清单里那条旧的商店包，`wsl --install -d Ubuntu` 2 秒就
    /// `Wsl/InstallDistro/WININET_E_CANNOT_CONNECT`，而同一分钟的
    /// `-d Ubuntu-24.04` / `-d Debian` 能照常下载 —— 参考实现的列表第一项是
    /// 「Ubuntu 26.04 LTS」（`Ubuntu-26.04`），所以它一直没撞上。
    ///
    /// 换成功之后，**真实注册的名字**是那个新 id，执行器会把后面
    /// 「等注册」「重定位」两步的参数一起改过来（见 `cmd::install::run_plan`）。
    pub retry_other_id: Option<String>,
    /// 失败原因是"资源还被占着"时，**等一下再跑同一条命令**。
    ///
    /// 只给「按 `.wslconfig` 开稀疏盘」那一步用（`--set-sparse`）。
    ///
    /// # 为什么非重试不可
    ///
    /// 那一步是 [`PlannedStep::best_effort`]：失败**不会**把安装判成失败。
    /// 于是紧接着 `--install` 跑（VHDX 还被 WSL 自己开着）时，用户明明在
    /// `.wslconfig` 里写了 `[experimental] sparseVhd=true`，**结果静默没生效**
    /// —— 而界面上没有第二个地方能开它（v0.4 把详情里那个开关拿掉了，
    /// 见 `DistroAction::SetSparse` 的说明）。
    ///
    /// 本机实测（2026-10-12）：紧接着 `--install` 之后跑它就是
    /// `Wsl/Service/ERROR_SHARING_VIOLATION`（退出码 -1）；同一个发行版隔一会儿
    /// 手工再跑就"操作成功完成"。所以这里等一等就能拿到用户要的结果。
    ///
    /// ⚠️ 重试只认**错误码**（见 `cmd::install::is_busy`），而且用户点了取消
    /// 就不再试 —— 别的失败（比如 WSL 因为自己的策略拒绝开稀疏盘）等多久都一样。
    pub retry_while_busy: bool,
}

impl PlannedStep {
    fn new(label: impl Into<String>, program: PlanProgram, args: Vec<String>) -> Self {
        Self {
            label: label.into(),
            program,
            args,
            cancellable: true,
            retry_other_source: false,
            best_effort: false,
            retry_other_id: None,
            retry_while_busy: false,
        }
    }

    fn uncancellable(mut self) -> Self {
        self.cancellable = false;
        self
    }

    /// 「资源还被占着」时等一下再跑（见字段说明）。
    fn retry_while_busy(mut self) -> Self {
        self.retry_while_busy = true;
        self
    }

    /// 失败时换一个下载源再试一次（只给在线安装那一步用，见字段说明）。
    fn with_source_fallback(mut self) -> Self {
        self.retry_other_source = true;
        self
    }

    /// 失败时换成另一个 id 再试一次（见字段说明）。
    fn with_id_fallback(mut self, id: impl Into<String>) -> Self {
        self.retry_other_id = Some(id.into());
        self
    }

    /// 失败**不算安装失败**（收尾步骤用，见字段说明）。
    fn best_effort(mut self) -> Self {
        self.best_effort = true;
        self
    }

    /// 界面上那一行怎么写。
    ///
    /// `Wsl` / `Curl` 给**真命令行**（用户能直接复制到终端复现）；
    /// 其余几种不是一条命令，就如实给 label —— 编一条假的 `mkdir`
    /// 只会让人以为我们真的跑了 `mkdir`。
    pub fn line(&self) -> String {
        match self.program {
            PlanProgram::Wsl => format!("wsl {}", join_for_display(&self.args)),
            PlanProgram::Curl => format!("curl {}", join_for_display(&self.args)),
            _ => self.label.clone(),
        }
    }

    /// 这一步会不会起一个外部进程。
    pub fn spawns_process(&self) -> bool {
        matches!(self.program, PlanProgram::Wsl | PlanProgram::Curl)
    }
}

/// 拼给人看的命令行：含空格的参数加引号（路径里带空格极常见）。
fn join_for_display(args: &[String]) -> String {
    args.iter()
        .map(|arg| {
            if arg.contains(' ') && !arg.starts_with('"') {
                format!("\"{arg}\"")
            } else {
                arg.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// 装一个发行版的**完整计划**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallPlan {
    /// 最终发行版名。
    pub name: String,
    /// 最终安装目录（可能为空：在线安装且不改名时可以交给 WSL 决定）。
    pub install_dir: String,
    /// 按顺序执行的步骤。
    pub steps: Vec<PlannedStep>,
    /// 给用户看的额外说明（代价、前提）。不参与执行。
    pub notes: Vec<String>,
}

impl InstallPlan {
    /// 预览：带序号的步骤列表。
    pub fn preview_lines(&self) -> Vec<String> {
        self.steps
            .iter()
            .enumerate()
            .map(|(idx, step)| format!("{}. {}", idx + 1, step.line()))
            .collect()
    }

    /// 会真正起进程的那几步（日志里用得上）。
    pub fn process_steps(&self) -> impl Iterator<Item = &PlannedStep> {
        self.steps.iter().filter(|s| s.spawns_process())
    }

    /// 这个计划在**重定位之前**可能注册成的名字（只有在线安装会给出非空结果）。
    ///
    /// 在线安装先装成清单里的 `id`，失败时还可能换成兄弟 `id`
    /// （见 [`PlannedStep::retry_other_id`]），重定位之后才变成
    /// [`InstallPlan::name`]。所以这段时间里 `wsl -l -v` 上是**源名**。
    ///
    /// # 为什么界面需要它
    ///
    /// 安装期间界面上记的是最终名字（`InstallProgress::name`），而
    /// `AppState::busy_with` 拿它去查"这个发行版是不是正在被安装"。
    /// 改名安装时两边不一致 → **查不到** → 重定位那一步
    /// （`--export` → `--unregister` → `--import`）全程都能被别的按钮
    /// （终止 / 删除 / 打开终端）打断，而它的说明写着"中途停下等于把刚装好的删了"
    /// （见 `cmd::install` 的模块说明）。本机实测过：改名安装的中间态里，
    /// 列表上确实会先出现源名那一行。
    ///
    /// 从**步骤**里读、而不是从 [`InstallSpec`] 再算一遍：计划是唯一的事实来源，
    /// 而"换 id 重试"那一手挂在步骤上 —— 重算就会漏掉它，于是
    /// "换了兄弟 id 才装上"那条路又失去保护。
    pub fn source_names(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();

        for step in &self.steps {
            if step.program != PlanProgram::Wsl {
                continue;
            }
            // `--install -d <id>`：`-d` 后面那个才是会被注册的名字。
            let Some(at) = step.args.iter().position(|arg| arg == "-d") else {
                continue;
            };

            // 与最终名字相同的不必重复记（`busy_with` 本来就会查最终名字）；
            // 判据用 `same_name`，和 `busy_with` 那边保持一致。
            let mut push = |name: &str| {
                let name = name.trim();
                if name.is_empty()
                    || crate::model::distro::same_name(name, &self.name)
                    || out.iter().any(|seen| seen == name)
                {
                    return;
                }
                out.push(name.to_owned());
            };

            if let Some(id) = step.args.get(at + 1) {
                push(id);
            }
            if let Some(sibling) = step.retry_other_id.as_deref() {
                push(sibling);
            }
        }

        out
    }
}

/// 装前检查的结果。
///
/// 分成 errors / warnings 两类：errors **挡住**提交（不发起任何进程 ——
/// 这类错误里最没意义的就是"跑一遍命令再告诉你参数不对"），
/// warnings 只是提醒（比如"会多一次全量拷贝"）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Preflight {
    /// 必须解决才能安装。
    pub errors: Vec<String>,
    /// 建议知道，但不挡。
    pub warnings: Vec<String>,
}

impl Preflight {
    /// 能不能提交。
    pub fn ok(&self) -> bool {
        self.errors.is_empty()
    }

    /// 一条能直接进提示条的消息（没有错误时给 `None`）。
    pub fn error_text(&self) -> Option<String> {
        if self.errors.is_empty() {
            None
        } else {
            Some(self.errors.join("；"))
        }
    }

    /// 把错误落到**具体输入框**上：界面据此给对应的框标红。
    ///
    /// 这里仍然按错误文案判断，而不是在 [`validate`] 里返回结构化的
    /// `(字段, 文案)` —— 后者要改所有 `out.push(...)` 的调用点，收益只是
    /// 省掉下面几行 `contains`；而这些文案本来就定义在本文件里，改文案时
    /// 顺手看一眼这里即可。真正的保证在测试 `field_errors_map_...`：
    /// 名字 / 文件或下载地址 / 安装目录三类错误必须各自落到正确的框上。
    pub fn field_errors(&self) -> InstallFieldErrors {
        let mut fields = InstallFieldErrors::default();
        for error in &self.errors {
            if error.contains("发行版名") || error.contains("已经有一个叫") {
                fields.name = true;
            }
            if error.contains("文件路径")
                || error.contains("下载地址")
                || error.contains("在线发行版")
            {
                fields.source = true;
            }
            if error.contains("安装目录") || error.contains("目录里已经有东西") {
                fields.install_dir = true;
            }
        }
        fields
    }
}

/// 装前检查里"哪几个输入框有问题"。
///
/// 只分三块：**发行版名**、**来源**（本地文件路径 / 在线发行版 / 镜像地址共用）、
/// **安装目录**。界面不需要知道具体是哪条文案 —— 点「开始安装」时它只负责把
/// 对应的框标红，并把焦点送过去；错误文案本身不再常驻页面。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InstallFieldErrors {
    /// 发行版名（为空 / 非法 / 重名）。
    pub name: bool,
    /// 安装来源相关的输入（本地路径、在线发行版、镜像下载地址）。
    pub source: bool,
    /// 安装目录（为空但必填 / 相对路径 / 目录非空）。
    pub install_dir: bool,
}

impl InstallFieldErrors {
    /// 有没有任何一块需要标红。
    pub fn any(&self) -> bool {
        self.name || self.source || self.install_dir
    }
}

/// 装前检查（界面渲染和提交前都用它，保证"看到的"和"挡住的"是同一套规则）。
///
/// `taken` 是**已有的发行版名**（来自列表快照），`dir_non_empty` 由调用方
/// 用文件系统查出来（这里不碰磁盘）。
pub fn preflight(
    spec: &InstallSpec,
    ctx: &PlanContext,
    taken: &[String],
    dir_non_empty: bool,
) -> Preflight {
    let mut errors = validate(spec, ctx);
    let name = spec.name.trim();

    if !name.is_empty()
        && taken
            .iter()
            .any(|existing| crate::model::distro::same_name(existing, name))
    {
        errors.push(format!(
            "已经有一个叫「{name}」的发行版了 —— 换个名字，或者先把旧的删掉（本程序不会替你删）"
        ));
    }
    if dir_non_empty {
        errors.push(
            "这个安装目录里已经有东西了。换一个空目录 —— WSL 会在里面建自己的文件，\
             和别的东西混在一起不好收拾"
                .to_owned(),
        );
    }

    let mut warnings = Vec::new();
    if let InstallSource::Online {
        id, web_download, ..
    } = &spec.source
    {
        let id = id.trim();
        if !id.is_empty() && id != name {
            warnings.push(format!(
                "WSL 只认在线清单里的名字，所以会先装成「{id}」，再导出/注销/导入改成「{name}」——\
                 多一次全量拷贝，需要额外的临时空间。"
            ));
        }
        if *web_download {
            warnings.push(
                "`--web-download` 是从网络下载而不是走微软商店；网络不通时这条会失败，\
                 可以切回商店再试。"
                    .to_owned(),
            );
        }
    }
    if let InstallSource::Mirror { release, .. } = &spec.source {
        if !release.trim().is_empty() {
            warnings.push(format!(
                "文件会先下载到临时目录（{release}，几百 MB），装完自动删掉。"
            ));
        }
        warnings.push(
            "清单里给的是各个镜像站上的地址，会随版本更新而变化。\
             如果探测/下载报 404，说明那一版的文件改名了 —— 刷新清单、换一个版本，\
             或者用「自定义下载地址」。"
                .to_owned(),
        );
    }

    Preflight { errors, warnings }
}

/// 纯参数校验（不含"重名"和"目录非空"这两件需要外部信息的事）。
fn validate(spec: &InstallSpec, ctx: &PlanContext) -> Vec<String> {
    let mut out = Vec::new();
    let name = spec.name.trim();
    out.extend(name_syntax_errors(name));

    let dir = spec.effective_install_dir(ctx);

    match &spec.source {
        InstallSource::Tar { path } => {
            if path.trim().is_empty() {
                out.push("tar 文件路径不能为空".to_owned());
            }
            if dir.is_none() {
                out.push("从 tar 导入必须指定安装目录".to_owned());
            }
        }
        InstallSource::Vhdx { path } => {
            if path.trim().is_empty() {
                out.push("VHDX 文件路径不能为空".to_owned());
            }
            if dir.is_none() {
                out.push("从 VHDX 导入必须指定安装目录".to_owned());
            }
        }
        InstallSource::File { path } => {
            if path.trim().is_empty() {
                out.push("安装文件路径不能为空".to_owned());
            }
        }
        InstallSource::Online { id, .. } => {
            let id = id.trim();
            if id.is_empty() {
                out.push("请先选一个在线发行版（或手输它的名字）".to_owned());
            } else if !id.chars().all(is_name_char) {
                out.push(format!(
                    "在线发行版的名字里有非法字符：{id}（只允许 A-Z a-z 0-9 . _ -）"
                ));
            } else if id != name && dir.is_none() {
                out.push(format!(
                    "要把它改名成「{name}」，就必须给一个安装目录（重定位那一步要用）"
                ));
            }
        }
        InstallSource::Mirror { url, .. } => {
            let url = url.trim();
            if url.is_empty() {
                out.push(
                    "还没有选好下载地址 —— 清单一到就会**自动测速**挑最快的镜像，\
                     稍等几秒；也可以直接在下面手填一个下载地址"
                        .to_owned(),
                );
            } else if !crate::mirrors::is_http_url(url) {
                // 判据复用 `mirrors::is_http_url`：这个字符串会作为**裸 argv**
                // 交给 `curl.exe`，只判"以 http 开头"是不够/不一致的
                // （`-K<file>` 这类以 `-` 开头的东西必须一起挡掉）。
                out.push(format!("下载地址要以 http:// 或 https:// 开头：{url}"));
            }
            if dir.is_none() {
                out.push("从镜像源安装必须指定安装目录".to_owned());
            }
        }
    }

    // 安装目录给了就必须是绝对路径。
    //
    // ⚠️ 这里查的是 `effective_install_dir` 的结果：用户留空、由默认目录推出来的
    // 那个路径同样要过这一关 —— 默认目录本身可能是相对的。
    if let Some(dir) = dir.as_deref() {
        let dir = dir.trim();
        if !is_absolute_windows_path(dir) {
            out.push(format!(
                "安装目录要写成绝对路径（如 D:\\wsl\\MyDistro）：{dir}"
            ));
        }
    }

    out
}

/// 按 [`InstallSpec`] 拼出执行计划。
///
/// 校验不过时返回**能直接给用户看**的错误（和 [`preflight`] 用的是同一套规则）。
pub fn plan(spec: &InstallSpec, ctx: &PlanContext) -> Result<InstallPlan, String> {
    let errors = validate(spec, ctx);
    if !errors.is_empty() {
        return Err(errors.join("；"));
    }

    let name = spec.name.trim().to_owned();
    let dir = spec.effective_install_dir(ctx);
    let mut steps: Vec<PlannedStep> = Vec::new();
    let mut notes: Vec<String> = Vec::new();

    match &spec.source {
        InstallSource::Tar { path } => {
            let path = path.trim();
            let dir = dir.clone().unwrap_or_default();
            steps.push(create_dir_step("安装目录", dir.as_str()));
            steps.push(PlannedStep::new(
                format!("把 {path} 展开成发行版 {name}"),
                PlanProgram::Wsl,
                own(&[
                    "--import",
                    name.as_str(),
                    dir.as_str(),
                    path,
                    "--version",
                    "2",
                ]),
            ));
            notes.push(
                "`--import` 只是把文件系统铺开，**不会**做首次启动初始化（不建默认用户）。\
                 想要那一步请用「从 .wsl / 文件安装」。"
                    .to_owned(),
            );
        }
        InstallSource::Vhdx { path } => {
            let path = path.trim();
            let dir = dir.clone().unwrap_or_default();
            steps.push(create_dir_step("安装目录", dir.as_str()));
            steps.push(PlannedStep::new(
                format!("把 {path} 作为虚拟磁盘导入并拷贝到 {dir}"),
                PlanProgram::Wsl,
                own(&[
                    "--import",
                    name.as_str(),
                    dir.as_str(),
                    path,
                    "--vhd",
                    "--version",
                    "2",
                ]),
            ));
            notes.push(
                "`--vhd` 会把给定的磁盘**拷贝**一份到安装目录（原始文件不动）。\
                 几百 MB 到几 GB 的盘会有一次全量拷贝。"
                    .to_owned(),
            );
        }
        InstallSource::File { path } => {
            let path = path.trim();
            let mut args = own(&["--install", "--from-file", path, "--name", name.as_str()]);
            if let Some(dir) = dir.as_deref() {
                args.push("--location".to_owned());
                args.push(dir.to_owned());
            }
            steps.push(PlannedStep::new(
                format!("交给 WSL 安装器从 {path} 安装（会做首次启动初始化）"),
                PlanProgram::Wsl,
                args,
            ));
            notes.push(
                "这条路径**没有** `--version` 选项（实测 `wsl.exe --help`），\
                 版本由安装器自己决定 —— 我们指定不了。"
                    .to_owned(),
            );
        }
        InstallSource::Online {
            id,
            launch,
            web_download,
            fallback_id,
        } => {
            let id = id.trim().to_owned();
            let same_name = id == name;
            let target_dir = dir.clone();
            let fallback_id = fallback_id
                .as_deref()
                .map(str::trim)
                .filter(|candidate| !candidate.is_empty() && *candidate != id)
                .map(str::to_owned);

            // 装完要不要**再动一次**（改名 或 挪位置）——先算出来，因为
            // 它反过来决定要不要 `--no-launch`（见下）。
            let needs_relocate = !same_name || target_dir.is_some();

            let mut args = own(&["--install", "-d", id.as_str()]);
            if *web_download {
                args.push("--web-download".to_owned());
            }
            // ⚠️ **不传 `--location`** —— 这是实测出来的（2026-10-10，本机）：
            //
            // | 命令 | 结果 |
            // |---|---|
            // | `wsl --install -d Ubuntu --location D:\wsl --version 2 --no-launch` | 秒失败：`Wsl/InstallDistro/WININET_E_CANNOT_CONNECT` |
            // | `wsl --install -d Ubuntu --web-download --location D:\wsl …` | 同样秒失败 |
            // | `wsl --install -d Ubuntu --no-launch` | **能连上**（25 秒还在下载） |
            //
            // 带上 `--location` 时 WSL 走的是"自己把整包下到指定目录"那条通道，
            // 用的是不认系统代理的 WinINET；不带时走商店/清单那条，能通。
            // 参考实现也从不传 `--location` —— 它装完再搬（我们也一样，
            // 见下面那一步 `EnsureRelocated`，同名的走 `--manage --move`）。
            //
            // 代价：多一次搬运（同一个卷上是改名，跨卷是真的拷一遍）。
            // 好处：这条路能装上，而"直接装到目标目录"在这台机器上装不上。
            args.push("--version".to_owned());
            args.push(WSL_VERSION.to_string());
            // ⚠️ 要重定位时**必须** `--no-launch`，即使用户勾了"装完立即启动"。
            //
            // 原因（本机实测，2026-10-12）：`--install` 之后那个发行版的 VHDX
            // 还被 WSL 自己占着 —— 紧接着跑 `--manage <name> --move` 会
            // `Wsl/Service/MoveDistro/ERROR_SHARING_VIOLATION`，跑 `--export`
            // 同样可能撞上。而**启动它只会让这件事更糟**：运行中的发行版
            // 本来就搬不动，于是必然退到"导出 → 注销 → 导入"那条要全量拷贝的路。
            // 顺带一提，新发行版被启动时会进 OOBE（建默认用户），而我们的
            // stdin 是 `Stdio::null()`（见 `cli::build_command`）—— 用户根本
            // 看不到那个交互界面。
            //
            // 用户勾的那个开关**没有被无声吞掉**：预览里的说明会讲清这一次为什么
            // 不启动（见下面那段 `notes`）。
            if !launch || needs_relocate {
                args.push("--no-launch".to_owned());
            }
            let install_step =
                PlannedStep::new(format!("从在线源安装 {id}"), PlanProgram::Wsl, args)
                    // 商店 ⇄ GitHub 自动换一次：两条通道在不同网络环境下会各挂一条。
                    .with_source_fallback();
            // 裸名字（`Ubuntu`）→ 带版本号的兄弟（`Ubuntu-26.04`）再试一次。
            // 实测（2026-10-11，本机）：`wsl -l -o` 排在第一位的裸 `Ubuntu` 是清单里
            // 那条**旧的商店包**，`wsl --install -d Ubuntu` 2 秒就
            // `WININET_E_CANNOT_CONNECT`，而 `-d Ubuntu-24.04` / `-d Debian` 照常下载。
            // 参考实现的列表第一项是「Ubuntu 26.04 LTS」，所以它一直没撞上这个坑。
            let install_step = match fallback_id.clone() {
                Some(sibling) => install_step.with_id_fallback(sibling),
                None => install_step,
            };
            steps.push(install_step);

            steps.push(PlannedStep::new(
                format!("等 {id} 注册完成"),
                PlanProgram::WaitRegistered,
                vec![id.clone()],
            ));

            if needs_relocate {
                let target_dir = target_dir.unwrap_or_default();
                let temp_tar = format!(
                    "{}\\wslc-panel-{}-{}.tar",
                    ctx.temp_dir.trim_end_matches(['\\', '/']),
                    sanitize_name(&id),
                    ctx.stamp
                );
                steps.push(
                    PlannedStep::new(
                        format!("必要时把 {id} 重定位成 {name}（装到 {target_dir}）"),
                        PlanProgram::EnsureRelocated,
                        // `target_dir` 下面那句 note 还要用，所以这里 clone 一份
                        vec![
                            id.clone(),
                            name.clone(),
                            target_dir.clone(),
                            temp_tar.clone(),
                        ],
                    )
                    .uncancellable(),
                );
                if !same_name {
                    notes.push(format!(
                        "在线清单里没有「{name}」这个名字，所以会绕一下：先装成「{id}」，\
                         再 `--export` → `--unregister` → `--import` 改成「{name}」。\
                         这一段走完之后**不能取消**（取消等于把刚装好的删掉），\
                         中途会用到 {} 作中转。",
                        temp_tar
                    ));
                } else {
                    notes.push(format!(
                        "装的时候**不带 `--location`**（带上它 WSL 会改走一条不认系统代理的\
                         下载通道，实测在这台机器上直接报 `WININET_E_CANNOT_CONNECT`），\
                         所以装完会用注册表核实它落在哪儿，需要的话用 `--manage {id} --move` \
                         搬到 {target_dir}。"
                    ));
                }
                if *launch {
                    notes.push(format!(
                        "你勾了「装完立即启动」，但这一次要先{}，所以命令里仍然带了 \
                         `--no-launch`：运行中的发行版搬不动（实测紧接着安装之后 \
                         `--move` 会报 `ERROR_SHARING_VIOLATION`）。装完在实例列表里点\
                         「打开终端」就能进去。",
                        if same_name { "挪位置" } else { "改名" }
                    ));
                }
            }

            if let Some(sibling) = fallback_id.as_deref() {
                notes.push(format!(
                    "「{id}」在清单里是个**不带版本号的名字**（实测这类条目在这台机器上\
                     装不动：`wsl --install -d {id}` 两秒就报 `WININET_E_CANNOT_CONNECT`），\
                     所以上面那条命令失败时会自动换成「{sibling}」再试一次，\
                     装好之后的名字仍然是你要的「{name}」。"
                ));
            }
        }
        InstallSource::Mirror {
            url,
            mirror,
            release,
            format,
        } => {
            let url = url.trim();
            let dir = dir.clone().unwrap_or_default();
            let bundle = InstallSource::mirror_is_bundle(format, url);
            let extension = if bundle {
                ".wsl"
            } else if format.eq_ignore_ascii_case("tar.gz")
                || url.to_lowercase().ends_with(".tar.gz")
            {
                ".tar.gz"
            } else {
                ".tar.xz"
            };
            let temp_dir = ctx.temp_dir.trim_end_matches(['\\', '/']);
            let temp_file = format!(
                "{temp_dir}\\wslc-panel-rootfs-{}-{}{}",
                sanitize_name(release),
                ctx.stamp,
                extension
            );

            // ⚠️ **先建临时目录，再下载**。`curl -o` 不会自己建目录：父目录不存在时
            // 它给的是 `curl: (23) client returned ERROR on write of N bytes`
            // （本机实测，退出码 23），而那句话里**一个字都没提目录** ——
            // 用户只会当成网络问题去查。
            //
            // 而 `%TEMP%\wslc-panel` 平时**并不存在**：全仓库只有在线安装的
            // 重定位会顺手 `create_dir_all` 它（见 `cmd::install::ensure_relocated`），
            // 而镜像源这条路的第一步就是下载。更麻烦的是 `%TEMP%` 会被
            // Storage Sense / 磁盘清理回收 —— 所以这不是"只有第一次才会踩"，
            // 而是"某天早上突然又踩一次"（本机实测：目录不在时 curl 退出码 23，
            // 建出来之后同一条命令 200 / 19 MB）。
            //
            // 放在计划里而不是执行器里：预览和执行读的是同一份计划（见模块说明），
            // 执行器里偷偷多做一个动作就等于界面上那句预览在说谎。
            steps.push(create_dir_step("临时目录", temp_dir));

            steps.push(PlannedStep::new(
                format!(
                    "从 {mirror} 下载{}（{release}）",
                    if bundle { " .wsl 包" } else { " rootfs" }
                ),
                PlanProgram::Curl,
                crate::mirrors::download_args(url, temp_file.as_str()),
            ));

            if bundle {
                // `.wsl` 包 → 交给 WSL 自己的安装器（`--from-file`）。
                // `--name` 直接给了最终名字，所以**不需要**改名重定位；
                // 但 `--location` 不一定被采纳，所以位置仍然要核实一次。
                let mut args = own(&[
                    "--install",
                    "--from-file",
                    temp_file.as_str(),
                    "--name",
                    name.as_str(),
                ]);
                if !dir.is_empty() {
                    args.push("--location".to_owned());
                    args.push(dir.clone());
                }
                steps.push(PlannedStep::new(
                    format!("把下载的 .wsl 包安装成发行版 {name}"),
                    PlanProgram::Wsl,
                    args,
                ));
                if !dir.is_empty() {
                    // ⚠️ 中转 tar 的路径**必须**是个真路径。
                    //
                    // 这里原来传的是 `String::new()`：`--manage --move` 一失败
                    // （PLAN-v0.5 的 v0.5.2 补记写明"紧接着 `--install` 之后跑
                    // `--move` 会失败（返回 -1）"，所以这条回退路径是**设计上
                    // 会出现**的），就退到「导出 → 注销 → 导入」，
                    // 而那时跑的是 `wsl --export <name> ""` ——
                    // 只会回一句看不懂的参数错误，整条退路等于废掉
                    // （数据不会丢：export 在 unregister 之前就失败了）。
                    //
                    // 前缀与上面下载的临时文件（`wslc-panel-rootfs-…`）错开，
                    // 免得重定位把还没用完的下载产物覆盖掉。
                    let temp_tar = format!(
                        "{temp_dir}\\wslc-panel-relocate-{}-{}.tar",
                        sanitize_name(&name),
                        ctx.stamp
                    );
                    steps.push(
                        PlannedStep::new(
                            format!("必要时把 {name} 挪到 {dir}"),
                            PlanProgram::EnsureRelocated,
                            vec![name.clone(), name.clone(), dir.clone(), temp_tar],
                        )
                        .uncancellable(),
                    );
                }
                notes.push(format!(
                    "下载的是镜像站上的官方 **.wsl 包**（{url}）——\
                     它和 tar 不一样：走 WSL 自己的安装器，会做首次启动初始化（建默认用户）。\
                     装完会把临时文件删掉。"
                ));
            } else {
                steps.push(create_dir_step("安装目录", dir.as_str()));
                steps.push(PlannedStep::new(
                    format!("把下载的 rootfs 展开成发行版 {name}"),
                    PlanProgram::Wsl,
                    own(&[
                        "--import",
                        name.as_str(),
                        dir.as_str(),
                        temp_file.as_str(),
                        "--version",
                        "2",
                    ]),
                ));
                notes.push(format!(
                    "下载的是镜像站上的官方 rootfs（{url}）；\
                     `--import` 只是铺开文件系统，**不做**首次启动初始化。\
                     装完会把临时文件删掉。"
                ));
            }

            steps.push(PlannedStep::new(
                format!("删掉临时文件 {temp_file}"),
                PlanProgram::RemoveFile,
                vec![temp_file.clone()],
            ));
        }
    }

    // 收尾：用户自己在 `.wslconfig` 里要求了稀疏 VHD，就照着做。
    if ctx.wslconfig_sparse {
        // ⚠️ 必须带 `--allow-unsafe`：WSL 3.0.1.0 现在**默认拒绝**开稀疏盘
        // （理由是"潜在的数据损坏"），报错里自己给出了该加的开关：
        // `wsl.exe --manage <分发> --set-sparse true --allow-unsafe`。
        // 用户的 `.wslconfig` 明确要了稀疏盘，所以照着它给的命令来。
        //
        // 这一步是 `best_effort`：WSL 哪天又改主意不给开，也**不能**把
        // "已经装好的发行版"判成安装失败（这个坑也踩过 —— 界面报"安装失败"，
        // 其实发行版好好地在那儿）。
        //
        // ⚠️ 但它同时是 `retry_while_busy`：**紧接着 `--install` 跑的时候
        // VHDX 还被 WSL 自己开着**（实测 2026-10-12：`Wsl/Service/
        // ERROR_SHARING_VIOLATION`，退出码 -1）。那一步是 `best_effort`，
        // 所以失败只会记一条日志 —— 而用户明明在 `.wslconfig` 里要了稀疏盘，
        // 结果是**静默不生效**（界面上没有第二个地方能开它）。
        // 等一下再跑就能成，所以这里给它重试。
        steps.push(
            PlannedStep::new(
                format!("按 .wslconfig 把 {name} 的 VHD 设成稀疏（自动回收空间）"),
                PlanProgram::Wsl,
                own(&[
                    "--manage",
                    name.as_str(),
                    "--set-sparse",
                    "true",
                    "--allow-unsafe",
                ]),
            )
            .best_effort()
            .retry_while_busy(),
        );
        notes.push(
            "你的 `%USERPROFILE%\\.wslconfig` 里开了 `[experimental] sparseVhd`，\
             所以装完会补一条 `--set-sparse true --allow-unsafe`\
             （WSL 目前默认拒绝开稀疏盘，要显式加 `--allow-unsafe`）。\
             这一步没做成也不影响装好的发行版 —— 刚装完那一刻磁盘可能还被 WSL \
             占着，它会等一会儿重试几次。"
                .to_owned(),
        );
    }

    // `--import` / `--install` 都不接受 `--set-default`，所以它是最后**单独**一条。
    if spec.set_default {
        steps.push(PlannedStep::new(
            format!("把 {name} 设为默认发行版"),
            PlanProgram::Wsl,
            own(&["--set-default", name.as_str()]),
        ));
    }

    Ok(InstallPlan {
        name,
        install_dir: dir.unwrap_or_default(),
        steps,
        notes,
    })
}

/// 建目录的步骤。
///
/// `label` 说明建的是**哪个**目录（`"安装目录"` / `"临时目录"`）：计划里两处都要建，
/// 而预览那一行必须能分辨它们 —— 原先这里写死了"创建安装目录"，临时目录那一步
/// 会看着像在重复建安装目录。
fn create_dir_step(label: &str, dir: &str) -> PlannedStep {
    PlannedStep::new(
        format!("创建{label} {dir}"),
        PlanProgram::CreateDir,
        vec![dir.to_owned()],
    )
}

/// `&[&str]` → `Vec<String>`（拼参数时到处都要，起个短名字）。
fn own(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

/// 临时目录的默认位置（`%TEMP%\wslc-panel`），供界面事先展示。
pub fn default_temp_dir() -> PathBuf {
    std::env::temp_dir().join("wslc-panel")
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- 名称 --------------------------------------------------------------

    #[test]
    fn sanitize_keeps_only_safe_characters() {
        assert_eq!(sanitize_name("Ubuntu-24.04"), "Ubuntu-24.04");
        assert_eq!(sanitize_name("my distro"), "my-distro");
        assert_eq!(sanitize_name("my__distro"), "my-distro");
        assert_eq!(sanitize_name("a  b"), "a-b");
        assert_eq!(sanitize_name("a\\b/c"), "abc");
        assert_eq!(sanitize_name("中文名字"), "");
        assert_eq!(sanitize_name(""), "");
        assert_eq!(sanitize_name("   "), "");
    }

    #[test]
    fn sanitize_trims_trailing_separators() {
        // 结尾的 `-` / `.` 会被 WSL 和文件系统当成"还有下文"，必须去掉
        assert_eq!(sanitize_name("Ubuntu-"), "Ubuntu");
        assert_eq!(sanitize_name("Ubuntu."), "Ubuntu");
        assert_eq!(sanitize_name("Ubuntu-. ."), "Ubuntu");
        assert_eq!(sanitize_name("-Ubuntu"), "Ubuntu");
    }

    #[test]
    fn sanitize_truncates_to_the_limit() {
        let long = "a".repeat(40);
        assert_eq!(sanitize_name(&long).chars().count(), MAX_NAME_LEN);
        // 截断之后可能露出结尾的 `-`，要再去一次
        let name = format!("{}-bbbbbbbbbbbbbbbbbbbbbbbb", "a".repeat(24));
        let short = sanitize_name(&name);
        assert!(!short.ends_with('-'), "{short}");
        assert!(short.chars().count() <= MAX_NAME_LEN, "{short}");
    }

    #[test]
    fn suggest_name_from_file_strips_suffix_and_platform() {
        // 参考实现的例子：`rootfs` 与平台尾巴都不该进名字
        assert_eq!(
            suggest_name_from_file(r"D:\img\ubuntu-rootfs-amd64.tar.gz"),
            "ubuntu"
        );
        assert_eq!(
            suggest_name_from_file("alpine-minirootfs-3.21.0-x86_64.tar.gz"),
            "alpine-mini-3.21.0"
        );
        assert_eq!(
            suggest_name_from_file(r"E:\a\Ubuntu-24.04.tar"),
            "Ubuntu-24.04"
        );
        assert_eq!(
            suggest_name_from_file("noble-server-cloudimg-amd64-root.tar.xz"),
            "noble-server-cloudimg"
        );
        assert_eq!(suggest_name_from_file("Debian.tar.xz"), "Debian");
        assert_eq!(suggest_name_from_file("ext4.vhdx"), "ext4");
    }

    #[test]
    fn suggest_name_from_file_survives_junk() {
        // 大小写不敏感
        assert_eq!(suggest_name_from_file("ROOTFS-UBUNTU.TAR.GZ"), "UBUNTU");
        // 全是关键词 → 停词一上来就命中，`parts` 为空，退回整个 stem。
        // （参考实现也是这个行为：宁可能剩一个怪名字让用户改，也不要空。）
        assert_eq!(suggest_name_from_file("amd64.tar"), "amd64");
        assert_eq!(suggest_name_from_file(""), "");
        // 非 ASCII 文件名 → sanitize 之后是空串（界面会显示"名字为空"的红字）
        assert_eq!(suggest_name_from_file("中文镜像.tar"), "");
        // 多字节字符在后缀前面时，剥后缀不能切在字符中间（会 panic）
        assert_eq!(suggest_name_from_file("中-rootfs.tar.gz"), "");
    }

    #[test]
    fn classify_local_file_branches_by_suffix_only() {
        // 三种文件在 WSL 那边是**三条不同的路**，所以这一分必须准
        assert_eq!(
            classify_local_file(r"D:\img\ext4.vhdx"),
            Some(LocalFileKind::Vhdx)
        );
        assert_eq!(
            classify_local_file(r"D:\img\ext4.vhd"),
            Some(LocalFileKind::Vhdx)
        );
        assert_eq!(
            classify_local_file(r"D:\img\Ubuntu-24.04.wsl"),
            Some(LocalFileKind::Bundle)
        );
        for tar in [
            r"D:\img\ubuntu-rootfs.tar",
            r"D:\img\ubuntu-rootfs.tar.gz",
            r"D:\img\ubuntu-rootfs.tar.xz",
            r"D:\img\ubuntu-rootfs.tar.zst",
            r"D:\img\ubuntu-rootfs.tgz",
            r"D:\img\ubuntu-rootfs.txz",
            "/mnt/d/img/rootfs.tar.bz2",
        ] {
            assert_eq!(classify_local_file(tar), Some(LocalFileKind::Tar), "{tar}");
        }

        // 大小写不敏感（Windows 上的文件名本来就不区分大小写）
        assert_eq!(
            classify_local_file(r"D:\img\EXT4.VHDX"),
            Some(LocalFileKind::Vhdx)
        );
        assert_eq!(
            classify_local_file(r"D:\img\UBUNTU.TAR.GZ"),
            Some(LocalFileKind::Tar)
        );
        // 复合后缀按**长到短**匹配（`.gz` 不能先把 `.tar.gz` 吃掉）
        assert_eq!(classify_local_file("x.tar.gz"), Some(LocalFileKind::Tar));

        // 认不出来就**说认不出来**（调用方按 tar 处理并如实说明），别硬猜
        assert_eq!(classify_local_file(r"D:\img\rootfs"), None);
        assert_eq!(classify_local_file(r"D:\img\rootfs.zip"), None);
        assert_eq!(classify_local_file(""), None);
        assert_eq!(classify_local_file("   "), None);
        // 只给了目录、没有文件名
        assert_eq!(classify_local_file(r"D:\img\"), None);
        // 多字节字符在前也不能 panic
        assert_eq!(
            classify_local_file("中文镜像.tar"),
            Some(LocalFileKind::Tar)
        );
        assert_eq!(classify_local_file("中文镜像"), None);
    }

    #[test]
    fn name_syntax_rules_match_what_we_tell_users() {
        assert!(name_syntax_errors("Ubuntu-24.04").is_empty());
        assert!(name_syntax_errors("a_b.c-d").is_empty());

        assert!(name_syntax_errors("")
            .iter()
            .any(|e| e.contains("不能为空")));
        assert!(name_syntax_errors("a b").iter().any(|e| e.contains(' ')));
        assert!(name_syntax_errors("中文").iter().any(|e| e.contains('中')));
        assert!(name_syntax_errors(&"a".repeat(MAX_NAME_LEN + 1))
            .iter()
            .any(|e| e.contains("不能超过")));
        // 恰好等于上限是合法的
        assert!(name_syntax_errors(&"a".repeat(MAX_NAME_LEN)).is_empty());
    }

    // -- 目录 --------------------------------------------------------------

    #[test]
    fn absolute_path_check_does_not_depend_on_the_build_platform() {
        assert!(is_absolute_windows_path(r"D:\wsl"));
        assert!(is_absolute_windows_path("D:/wsl"));
        assert!(is_absolute_windows_path(r"\\server\share"));
        assert!(is_absolute_windows_path(r"c:\x"));

        assert!(!is_absolute_windows_path(r"wsl\X"));
        assert!(!is_absolute_windows_path("wsl"));
        assert!(!is_absolute_windows_path("D:"));
        assert!(!is_absolute_windows_path(r"D:relative"));
        assert!(!is_absolute_windows_path(""));
    }

    #[test]
    fn install_dir_follows_the_separator_of_the_default_dir() {
        assert_eq!(
            derive_install_dir(Some(r"D:\wsl"), "Ubuntu"),
            Some(r"D:\wsl\Ubuntu".to_owned())
        );
        // 默认目录用 `/`，我们不要自作主张混用
        assert_eq!(
            derive_install_dir(Some("D:/wsl"), "Ubuntu"),
            Some("D:/wsl/Ubuntu".to_owned())
        );
        // 结尾多余的分隔符不要拼出 `\\`
        assert_eq!(
            derive_install_dir(Some(r"D:\wsl\"), "Ubuntu"),
            Some(r"D:\wsl\Ubuntu".to_owned())
        );
        assert_eq!(
            derive_install_dir(Some("D:/wsl/"), "Ubuntu"),
            Some("D:/wsl/Ubuntu".to_owned())
        );
        // 缺一半就给 None（调用方据此要求用户自己填）
        assert_eq!(derive_install_dir(None, "Ubuntu"), None);
        assert_eq!(derive_install_dir(Some("   "), "Ubuntu"), None);
        assert_eq!(derive_install_dir(Some(r"D:\wsl"), "  "), None);
    }

    // -- 在线列表 ----------------------------------------------------------

    #[test]
    fn online_list_of_a_failed_run_is_silently_empty() {
        // 本机实测的原始输出（wsl -l -o 拉不到 raw.githubusercontent.com）
        let text = include_str!("../../tests/fixtures/wsl_list_online_failed.txt");
        assert!(
            parse_online_list(text).is_empty(),
            "失败输出必须被吃成空列表，而不是抛错"
        );
        // 空输入、只有提示语、只有表头都不能 panic
        assert!(parse_online_list("").is_empty());
        assert!(parse_online_list("以下是可安装的有效分发的列表：").is_empty());
        assert!(parse_online_list("NAME  FRIENDLY NAME").is_empty());
    }

    #[test]
    fn online_list_parses_the_two_column_table() {
        // 真实抓下来的 `wsl -l -o` **成功**输出（28 行，1394 字节）——
        // 2026-10-10 开着 dev-sidecar 系统代理时抓的：`wsl.exe` 走系统代理，
        // 所以它能去 `raw.githubusercontent.com` 取清单；
        // 而面板自己的 `curl.exe` 不走系统代理（见 `cmd::distro` 的兜底说明）。
        let text = include_str!("../../tests/fixtures/wsl_list_online.txt");
        let items = parse_online_list(text);
        assert!(items.len() >= 15, "只解析出 {} 项：{items:?}", items.len());

        // 提示语与表头之间的空行、以及中文提示行都不能进来
        assert!(
            !items.iter().any(|item| item.id.contains("以下是")),
            "{items:?}"
        );
        // 两列的名字要拆对：id 是第一列，友好名可能是**带空格**的多个词
        let ubuntu = items
            .iter()
            .find(|item| item.id == "Ubuntu-24.04")
            .expect("应该有 Ubuntu-24.04");
        assert_eq!(ubuntu.label, "Ubuntu 24.04 LTS");
        // 不带版本号的那个是默认项，它两列同名
        let default = items
            .iter()
            .find(|item| item.id == "Ubuntu")
            .expect("应该有 Ubuntu");
        assert_eq!(default.label, "Ubuntu");
    }

    #[test]
    fn online_list_ignores_junk_lines_after_the_header() {
        let text = "NAME  FRIENDLY NAME\n----  -------------\nUbuntu  Ubuntu\n";
        let items = parse_online_list(text);
        // 分隔线被过滤掉：它的第一列是 `----`，虽然每个字符都"合法"，
        // 但里面没有字母数字 —— 真正的发行版名不会长这样。
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!(items[0].id, "Ubuntu");
    }

    #[test]
    fn distribution_info_fixture_is_parsed() {
        // 真实抓下来的微软清单（cdn.jsdelivr.net 镜像的 microsoft/WSL@master，
        // 18481 字节，2026-10-10 抓取）。
        let json = include_str!("../../tests/fixtures/ms_distribution_info.json");
        let items = parse_distribution_info(json).expect("真实清单应该能解析");
        assert!(items.len() >= 5, "只解析出 {} 项", items.len());

        let ubuntu = items
            .iter()
            .find(|d| d.id == "Ubuntu-24.04")
            .expect("清单里应该有 Ubuntu-24.04");
        assert_eq!(ubuntu.label, "Ubuntu 24.04 LTS");
        // 默认项排最前（不带版本号的那个）
        assert_eq!(
            items[0].id, "Ubuntu",
            "默认发行版应该排第一：{:?}",
            items[0]
        );
        // 每项都要有 id 和非空 label
        for item in &items {
            assert!(!item.id.is_empty());
            assert!(!item.label.is_empty(), "{item:?}");
        }
    }

    #[test]
    fn distribution_info_failures_are_reported_not_panicked() {
        assert!(parse_distribution_info("").is_err());
        assert!(parse_distribution_info("{}").is_err());
        assert!(parse_distribution_info(r#"{"ModernDistributions":{}}"#).is_err());
        assert!(parse_distribution_info(r#"{"ModernDistributions":{"X":[]}}"#).is_err());
        // 条目缺 Name 的跳过，缺 FriendlyName 的退回 id
        let json = r#"{"ModernDistributions":{"X":[{"FriendlyName":"nope"},
            {"Name":"Debian"},{"Name":"Alpine","FriendlyName":"Alpine Linux"}]}}"#;
        let items = parse_distribution_info(json).unwrap();
        assert_eq!(items.len(), 2, "{items:?}");
        assert_eq!(items[0].id, "Debian");
        assert_eq!(items[0].label, "Debian");
    }

    #[test]
    fn online_search_matches_id_and_label() {
        let d = OnlineDistro::new("Ubuntu-24.04", "Ubuntu 24.04 LTS");
        assert!(online_matches(&d, ""));
        assert!(online_matches(&d, "ubuntu"));
        assert!(online_matches(&d, "24.04"));
        assert!(online_matches(&d, "LTS"));
        assert!(!online_matches(&d, "debian"));
    }

    // -- 计划 --------------------------------------------------------------

    fn ctx() -> PlanContext {
        PlanContext {
            default_dir: Some(r"D:\wsl".to_owned()),
            temp_dir: r"C:\Temp\wslc-panel".to_owned(),
            stamp: "1234-5678".to_owned(),
            wslconfig_sparse: false,
        }
    }

    fn labels(plan: &InstallPlan) -> Vec<String> {
        plan.steps
            .iter()
            .map(|s| format!("{:?}", s.program))
            .collect()
    }

    #[test]
    fn tar_plan_creates_the_dir_then_imports() {
        let spec = InstallSpec::new(
            "Ubuntu",
            InstallSource::Tar {
                path: r"D:\img\ubuntu.tar".to_owned(),
            },
        );
        let plan = super::plan(&spec, &ctx()).unwrap();

        assert_eq!(plan.name, "Ubuntu");
        assert_eq!(plan.install_dir, r"D:\wsl\Ubuntu");
        assert_eq!(labels(&plan), vec!["CreateDir", "Wsl"]);
        assert_eq!(plan.steps[0].args, vec![r"D:\wsl\Ubuntu"]);
        // 逐字节钉住参数：错了只表现为"界面上什么也没发生"
        assert_eq!(
            plan.steps[1].args,
            vec![
                "--import",
                "Ubuntu",
                r"D:\wsl\Ubuntu",
                r"D:\img\ubuntu.tar",
                "--version",
                "2"
            ]
        );
        assert_eq!(plan.preview_lines()[0], r"1. 创建安装目录 D:\wsl\Ubuntu");
        assert_eq!(
            plan.preview_lines()[1],
            r"2. wsl --import Ubuntu D:\wsl\Ubuntu D:\img\ubuntu.tar --version 2"
        );
    }

    #[test]
    fn vhdx_plan_uses_the_vhd_flag() {
        let spec = InstallSpec::new(
            "MyDisk",
            InstallSource::Vhdx {
                path: r"D:\img\ext4.vhdx".to_owned(),
            },
        );
        let plan = super::plan(&spec, &ctx()).unwrap();
        assert_eq!(
            plan.steps.last().unwrap().args,
            vec![
                "--import",
                "MyDisk",
                r"D:\wsl\MyDisk",
                r"D:\img\ext4.vhdx",
                "--vhd",
                "--version",
                "2"
            ]
        );
    }

    #[test]
    fn file_plan_passes_name_and_optional_location() {
        let mut spec = InstallSpec::new(
            "MyDistro",
            InstallSource::File {
                path: r"D:\img\rootfs.tar.gz".to_owned(),
            },
        );
        // 留空安装目录 → 用默认目录推出来，仍然是 `--location`
        let plan = super::plan(&spec, &ctx()).unwrap();
        assert_eq!(
            plan.steps[0].args,
            vec![
                "--install",
                "--from-file",
                r"D:\img\rootfs.tar.gz",
                "--name",
                "MyDistro",
                "--location",
                r"D:\wsl\MyDistro"
            ]
        );

        // 显式给目录
        spec.install_dir = r"E:\wsl\MyDistro".to_owned();
        let plan = super::plan(&spec, &ctx()).unwrap();
        assert!(plan.steps[0].args.iter().any(|a| a == r"E:\wsl\MyDistro"));

        // 上下文里也没有默认目录 → 老实不传 --location
        let bare = PlanContext {
            default_dir: None,
            ..ctx()
        };
        let mut no_dir = spec.clone();
        no_dir.install_dir = String::new();
        let plan = super::plan(&no_dir, &bare).unwrap();
        assert!(!plan.steps[0].args.iter().any(|a| a == "--location"));
        assert!(plan.install_dir.is_empty());
    }

    #[test]
    fn online_plan_relocates_when_the_name_differs() {
        let mut spec = InstallSpec::new(
            "MyUbuntu",
            InstallSource::Online {
                id: "Ubuntu-24.04".to_owned(),
                launch: false,
                web_download: false,
                fallback_id: None,
            },
        );
        let plan = super::plan(&spec, &ctx()).unwrap();

        assert_eq!(
            labels(&plan),
            vec!["Wsl", "WaitRegistered", "EnsureRelocated"]
        );
        // 名字不同时**不能**传 --location：那个目录是给最终名字准备的，
        // 先装成 id 再用它，WSL 会建出一个位置对不上的发行版。
        assert_eq!(
            plan.steps[0].args,
            vec![
                "--install",
                "-d",
                "Ubuntu-24.04",
                "--version",
                "2",
                "--no-launch"
            ]
        );
        // 换下载源的兜底同样挂着（这条与名字无关）
        assert!(plan.steps[0].retry_other_source);
        // 重定位不可取消（中途打断会丢数据）
        assert!(!plan.steps[2].cancellable);
        assert_eq!(
            plan.steps[2].args,
            vec![
                "Ubuntu-24.04",
                "MyUbuntu",
                r"D:\wsl\MyUbuntu",
                r"C:\Temp\wslc-panel\wslc-panel-Ubuntu-24.04-1234-5678.tar"
            ]
        );
        // 说明里要讲清这一段在干什么
        assert!(
            plan.notes.iter().any(|n| n.contains("--unregister")),
            "{:?}",
            plan.notes
        );

        // ⚠️ 勾了「装完立即启动」，但这一次**要改名**（`Ubuntu-24.04` → `MyUbuntu`），
        // 所以仍然必须 `--no-launch`：运行中的发行版搬不动，而实测（2026-10-12）
        // 紧接着 `--install` 之后 `--move` 就会撞 `ERROR_SHARING_VIOLATION`。
        spec.source = InstallSource::Online {
            id: "Ubuntu-24.04".to_owned(),
            launch: true,
            web_download: true,
            fallback_id: None,
        };
        let plan = super::plan(&spec, &ctx()).unwrap();
        assert!(plan.steps[0].args.iter().any(|a| a == "--web-download"));
        assert!(
            plan.steps[0].args.iter().any(|a| a == "--no-launch"),
            "要重定位时必须压住那个开关：{:?}",
            plan.steps[0].args
        );
        // 用户勾的东西不能被无声吞掉 —— 说明里要讲清为什么没启动
        assert!(
            plan.notes.iter().any(|n| n.contains("装完立即启动")),
            "{:?}",
            plan.notes
        );

        // 不需要重定位（名字就是 id、也没有目标目录）时，那个开关照旧生效
        let same = InstallSpec::new(
            "Debian",
            InstallSource::Online {
                id: "Debian".to_owned(),
                launch: true,
                web_download: false,
                fallback_id: None,
            },
        );
        let bare = PlanContext {
            default_dir: None,
            ..ctx()
        };
        let plan = super::plan(&same, &bare).unwrap();
        assert_eq!(labels(&plan), vec!["Wsl", "WaitRegistered"]);
        assert!(
            !plan.steps[0].args.iter().any(|a| a == "--no-launch"),
            "不重定位时不该压掉用户的开关：{:?}",
            plan.steps[0].args
        );
        assert!(!plan.notes.iter().any(|n| n.contains("装完立即启动")));
    }

    #[test]
    fn a_bare_online_id_carries_a_versioned_fallback() {
        // 本机实测：裸 `Ubuntu` 那条 2 秒就 WININET_E_CANNOT_CONNECT，
        // 而 `Ubuntu-26.04` 能下 —— 所以清单里给了兄弟时，计划要带上"换 id 重试"
        let spec = InstallSpec::new(
            "Ubuntu",
            InstallSource::Online {
                id: "Ubuntu".to_owned(),
                launch: false,
                web_download: false,
                fallback_id: Some("Ubuntu-26.04".to_owned()),
            },
        );
        let plan = super::plan(&spec, &ctx()).unwrap();
        assert_eq!(
            plan.steps[0].retry_other_id.as_deref(),
            Some("Ubuntu-26.04")
        );
        // 换源那一手也要在（两条通道在不同网络下各会挂一条）
        assert!(plan.steps[0].retry_other_source);

        // 没给兄弟（`Debian`）时不能瞎猜
        let spec = InstallSpec::new(
            "Debian",
            InstallSource::Online {
                id: "Debian".to_owned(),
                launch: false,
                web_download: false,
                fallback_id: None,
            },
        );
        let plan = super::plan(&spec, &ctx()).unwrap();
        assert_eq!(plan.steps[0].retry_other_id, None);

        // 和主 id 一样的"兄弟"等于没给（防止上游算错时多跑一轮）
        let spec = InstallSpec::new(
            "Ubuntu-24.04",
            InstallSource::Online {
                id: "Ubuntu-24.04".to_owned(),
                launch: false,
                web_download: false,
                fallback_id: Some("Ubuntu-24.04".to_owned()),
            },
        );
        let plan = super::plan(&spec, &ctx()).unwrap();
        assert_eq!(plan.steps[0].retry_other_id, None);
    }

    #[test]
    fn online_plan_never_passes_location_when_the_name_is_the_id() {
        let spec = InstallSpec::new(
            "Ubuntu-24.04",
            InstallSource::Online {
                id: "Ubuntu-24.04".to_owned(),
                launch: false,
                web_download: false,
                fallback_id: None,
            },
        );
        let plan = super::plan(&spec, &ctx()).unwrap();
        // ⚠️ **不能**带 `--location`：带上它 WSL 会改走一条不认系统代理的下载通道，
        // 本机实测直接 `WININET_E_CANNOT_CONNECT`（参考实现也从不传它）
        assert_eq!(
            plan.steps[0].args,
            vec![
                "--install",
                "-d",
                "Ubuntu-24.04",
                "--version",
                "2",
                "--no-launch"
            ]
        );
        // 位置由后面那一步"必要时重定位"补正（同名走 `--manage --move`）
        assert_eq!(
            labels(&plan),
            vec!["Wsl", "WaitRegistered", "EnsureRelocated"]
        );
        assert_eq!(plan.steps[2].args[2], r"D:\wsl\Ubuntu-24.04");
        assert!(!plan.steps[2].cancellable);
        // 失败时能自动换一个下载源再试（两条通道的可用性不同）
        assert!(plan.steps[0].retry_other_source);
        // 没有目录、名字也相同 → 连重定位那一步都不需要
        let bare = PlanContext {
            default_dir: None,
            ..ctx()
        };
        let mut no_dir = spec.clone();
        no_dir.install_dir = String::new();
        let plan = super::plan(&no_dir, &bare).unwrap();
        assert_eq!(labels(&plan), vec!["Wsl", "WaitRegistered"]);
    }

    #[test]
    fn mirror_plan_downloads_then_imports_then_cleans_up() {
        let spec = InstallSpec::new(
            "Ubuntu-24.04",
            InstallSource::Mirror {
                url: "https://mirrors.tuna.tsinghua.edu.cn/ubuntu-cloud-images/noble/current/noble-server-cloudimg-amd64-root.tar.xz".to_owned(),
                mirror: "清华 TUNA".to_owned(),
                release: "noble".to_owned(),
                format: "tar.xz".to_owned(),
            },
        );
        let plan = super::plan(&spec, &ctx()).unwrap();
        assert_eq!(
            labels(&plan),
            vec!["CreateDir", "Curl", "CreateDir", "Wsl", "RemoveFile"]
        );
        // 第 1 步必须是**建临时目录**：`curl -o` 不会自己建目录，父目录不存在时
        // 它只给 `curl: (23) client returned ERROR on write of N bytes`（本机实测），
        // 而 `%TEMP%\wslc-panel` 平时并不存在（还会被 %TEMP% 清理回收）。
        assert_eq!(plan.steps[0].args, vec![r"C:\Temp\wslc-panel"]);
        assert_eq!(
            plan.preview_lines()[0],
            r"1. 创建临时目录 C:\Temp\wslc-panel"
        );
        assert_eq!(
            plan.steps[1].args,
            vec![
                "-s",
                "-S",
                "-L",
                "--retry",
                "2",
                "--connect-timeout",
                "10",
                "-o",
                r"C:\Temp\wslc-panel\wslc-panel-rootfs-noble-1234-5678.tar.xz",
                "https://mirrors.tuna.tsinghua.edu.cn/ubuntu-cloud-images/noble/current/noble-server-cloudimg-amd64-root.tar.xz"
            ]
        );
        // 第 2 步建的是**安装目录**（和临时目录不是一回事）
        assert_eq!(plan.steps[2].args, vec![r"D:\wsl\Ubuntu-24.04"]);
        assert_eq!(plan.steps[3].args[0], "--import");
        // 删掉的必须是**下载下来的那个文件**（curl 参数里 `-o` 后面那个）
        assert_eq!(plan.steps[4].args[0], plan.steps[1].args[8]);
        // 下载那一步是可以取消的
        assert!(plan.steps[1].cancellable);
        assert!(plan.preview_lines()[1].starts_with("2. curl -s -S -L"));
    }

    #[test]
    fn mirror_plan_uses_the_installer_for_wsl_bundles() {
        // 清单里 Ubuntu 24.04 的多数来源是 `.wsl` 包 —— 那种必须走 `--install --from-file`，
        // `--import` 吃不了（参考实现一律 `--import`，挑到这种来源必然失败）
        let spec = InstallSpec::new(
            "Ubuntu-24.04",
            InstallSource::Mirror {
                url: "https://mirrors.tuna.tsinghua.edu.cn/ubuntu-releases/24.04/ubuntu-24.04.5-wsl-amd64.wsl".to_owned(),
                mirror: "tsinghua".to_owned(),
                release: "24.04".to_owned(),
                format: "wsl".to_owned(),
            },
        );
        let plan = super::plan(&spec, &ctx()).unwrap();
        assert_eq!(
            labels(&plan),
            vec!["CreateDir", "Curl", "Wsl", "EnsureRelocated", "RemoveFile"]
        );
        // 临时文件按格式取后缀
        assert!(
            plan.steps[1].args[8].ends_with(".wsl"),
            "{:?}",
            plan.steps[1].args
        );
        assert_eq!(
            plan.steps[2].args,
            vec![
                "--install",
                "--from-file",
                r"C:\Temp\wslc-panel\wslc-panel-rootfs-24.04-1234-5678.wsl",
                "--name",
                "Ubuntu-24.04",
                "--location",
                r"D:\wsl\Ubuntu-24.04",
            ]
        );
        // 位置要核实（`--location` 不一定被采纳），但那一步不能取消
        assert_eq!(plan.steps[3].args[0], "Ubuntu-24.04");
        assert!(!plan.steps[3].cancellable);
        // ⚠️ 中转 tar 的路径必须是**真路径**（第 4 个参数）。
        //
        // 原来是 `String::new()`：`--manage --move` 失败后会退到
        // 「导出 → 注销 → 导入」，而那条路的第一步就是
        // `wsl --export <name> ""` —— 整条退路等于废掉。
        // 而这条回退是**设计上会出现**的（本机 2026-10-12 实测：
        // 紧接着 `--install` 之后跑 `--move` 会撞 `ERROR_SHARING_VIOLATION`）。
        assert_eq!(
            plan.steps[3].args[3],
            r"C:\Temp\wslc-panel\wslc-panel-relocate-Ubuntu-24.04-1234-5678.tar"
        );
        // 删临时文件必须排在重定位之后（重定位可能还要读它？——不会，
        // `--move` 用的是发行版自己的 VHD，这里只是把顺序钉住）
        assert_eq!(plan.steps[4].program, PlanProgram::RemoveFile);
        assert!(plan.notes.iter().any(|n| n.contains(".wsl")));
    }

    #[test]
    fn every_download_step_has_its_directory_created_first() {
        /// 同时认 `\` 与 `/`。
        ///
        /// ⚠️ 这里**不能**用 `Path::parent()`：它在非 Windows 上不把 `\` 当分隔符，
        /// 这条断言会随编译平台给出不同结论（和 `is_absolute_windows_path` 的
        /// 理由一样 —— CI 与开发机的结论必须一致）。
        fn normalize_dir(path: &str) -> String {
            path.trim_end_matches(['\\', '/']).to_ascii_lowercase()
        }
        fn parent_dir(file: &str) -> String {
            let trimmed = file.trim_end_matches(['\\', '/']);
            match trimmed.rfind(['\\', '/']) {
                Some(at) => normalize_dir(&trimmed[..at]),
                None => String::new(),
            }
        }

        // 现在只有镜像源会下载；两种格式都要覆盖（它们走的是不同的安装步骤）
        let specs = [
            InstallSpec::new(
                "X",
                InstallSource::Mirror {
                    url: "https://mirrors.tuna.tsinghua.edu.cn/x/rootfs.tar.xz".to_owned(),
                    mirror: "m".to_owned(),
                    release: "noble".to_owned(),
                    format: "tar.xz".to_owned(),
                },
            ),
            InstallSpec::new(
                "X",
                InstallSource::Mirror {
                    url: "https://mirrors.tuna.tsinghua.edu.cn/x/u.wsl".to_owned(),
                    mirror: "m".to_owned(),
                    release: "24.04".to_owned(),
                    format: "wsl".to_owned(),
                },
            ),
        ];

        for spec in &specs {
            let plan = super::plan(spec, &ctx()).unwrap();
            let mut created: Vec<String> = Vec::new();

            for (index, step) in plan.steps.iter().enumerate() {
                match step.program {
                    PlanProgram::CreateDir => created.push(normalize_dir(&step.args[0])),
                    PlanProgram::Curl => {
                        let at = step
                            .args
                            .iter()
                            .position(|arg| arg == "-o")
                            .expect("下载步骤必然有 -o");
                        let parent = parent_dir(&step.args[at + 1]);
                        assert!(
                            created.contains(&parent),
                            "第 {} 步要往 {parent} 里下载，但前面没有任何一步建过这个目录 —— \
                             `curl.exe` 不会自己建，它只会回一句 `curl: (23) client returned \
                             ERROR on write of N bytes`（本机实测）。计划：{:?}",
                            index + 1,
                            plan.preview_lines()
                        );
                    }
                    _ => {}
                }
            }
        }
    }

    #[test]
    fn source_names_lists_what_the_install_registers_first() {
        // 改名安装：`wsl -l -v` 上先是 `Ubuntu-24.04`，重定位之后才是 `MyUbuntu`。
        // 界面要靠这份名单才知道"列表上那一行正在被安装"（见 `InstallPlan::source_names`）。
        let spec = InstallSpec::new(
            "MyUbuntu",
            InstallSource::Online {
                id: "Ubuntu-24.04".to_owned(),
                launch: false,
                web_download: false,
                fallback_id: Some("Ubuntu-26.04".to_owned()),
            },
        );
        let plan = super::plan(&spec, &ctx()).unwrap();
        // 兄弟 id 也要在里面：它同样会先注册成那个名字
        assert_eq!(plan.source_names(), vec!["Ubuntu-24.04", "Ubuntu-26.04"]);

        // 名字就是 id（最常见的路径）→ 不必重复记
        let same = InstallSpec::new(
            "Debian",
            InstallSource::Online {
                id: "Debian".to_owned(),
                launch: false,
                web_download: false,
                fallback_id: None,
            },
        );
        assert!(super::plan(&same, &ctx())
            .unwrap()
            .source_names()
            .is_empty());

        // 只差大小写也算"同一个名字"（`busy_with` 那边本来就不区分大小写）
        let cased = InstallSpec::new(
            "ubuntu-24.04",
            InstallSource::Online {
                id: "Ubuntu-24.04".to_owned(),
                launch: false,
                web_download: false,
                fallback_id: None,
            },
        );
        assert!(super::plan(&cased, &ctx())
            .unwrap()
            .source_names()
            .is_empty());

        // 本地来源不会"先注册成别的名字" → 永远是空的
        for source in [
            InstallSource::Tar {
                path: r"D:\a.tar".to_owned(),
            },
            InstallSource::Vhdx {
                path: r"D:\a.vhdx".to_owned(),
            },
            InstallSource::File {
                path: r"D:\a.wsl".to_owned(),
            },
            InstallSource::Mirror {
                url: "https://x/rootfs.tar.xz".to_owned(),
                mirror: "m".to_owned(),
                release: "1".to_owned(),
                format: "tar.xz".to_owned(),
            },
        ] {
            let plan = super::plan(&InstallSpec::new("MyDistro", source), &ctx()).unwrap();
            assert!(plan.source_names().is_empty(), "{:?}", plan.preview_lines());
        }
    }

    #[test]
    fn set_default_and_sparse_are_appended_steps() {
        let mut spec = InstallSpec::new(
            "Ubuntu",
            InstallSource::Tar {
                path: r"D:\a.tar".to_owned(),
            },
        );
        spec.set_default = true;

        let plan = super::plan(&spec, &ctx()).unwrap();
        let last = plan.steps.last().unwrap();
        assert_eq!(last.program, PlanProgram::Wsl);
        assert_eq!(last.args, vec!["--set-default", "Ubuntu"]);
        assert!(plan
            .preview_lines()
            .last()
            .unwrap()
            .contains("--set-default"));

        // `.wslconfig` 要求稀疏 → 在设默认之前插一步
        let sparse_ctx = PlanContext {
            wslconfig_sparse: true,
            ..ctx()
        };
        let plan = super::plan(&spec, &sparse_ctx).unwrap();
        let programs: Vec<PlanProgram> = plan.steps.iter().map(|s| s.program).collect();
        assert_eq!(
            programs,
            vec![
                PlanProgram::CreateDir,
                PlanProgram::Wsl,
                PlanProgram::Wsl,
                PlanProgram::Wsl
            ]
        );
        assert!(plan.steps[2].args.iter().any(|a| a == "--set-sparse"));
        // ⚠️ 必须带 `--allow-unsafe`，否则 WSL 默认拒绝开稀疏盘（实测它的报错里
        //    自己给出的命令就带这个开关）—— 不带的话这条步骤在真机上必然失败
        assert!(
            plan.steps[2].args.iter().any(|a| a == "--allow-unsafe"),
            "{:?}",
            plan.steps[2].args
        );
        // 而且它失败**不算安装失败**：那一刻发行版已经装好了
        assert!(plan.steps[2].best_effort);
        // ⚠️ 同时要**忙就重试**：紧接着 `--install` 跑的时候 VHDX 还被 WSL 自己
        //    开着（实测 `Wsl/Service/ERROR_SHARING_VIOLATION`），而它是
        //    `best_effort` —— 不重试就等于用户 `.wslconfig` 里的
        //    `sparseVhd=true` 静默不生效（界面上没有第二个地方能开它）。
        assert!(
            plan.steps[2].retry_while_busy,
            "开稀疏盘那一步必须能在'VHD 还被占着'时重试：{:?}",
            plan.steps[2]
        );
        // 别的步骤都不该带这个标记：它们失败的原因不是"资源被占着"，
        // 等一会儿只会白白拖住安装
        assert!(
            plan.steps
                .iter()
                .enumerate()
                .all(|(index, step)| index == 2 || !step.retry_while_busy),
            "{:?}",
            plan.preview_lines()
        );
        assert!(plan.notes.iter().any(|n| n.contains("sparseVhd")));
    }

    #[test]
    fn plan_rejects_bad_input_with_readable_messages() {
        // 名字为空 + tar 路径为空 + 没目录 → 三条错一起给出来
        let bare = PlanContext {
            default_dir: None,
            ..ctx()
        };
        let spec = InstallSpec::new(
            "  ",
            InstallSource::Tar {
                path: " ".to_owned(),
            },
        );
        let err = super::plan(&spec, &bare).unwrap_err();
        assert!(err.contains("发行版名不能为空"), "{err}");
        assert!(err.contains("tar 文件路径不能为空"), "{err}");
        assert!(err.contains("必须指定安装目录"), "{err}");

        // 相对目录
        let mut relative = InstallSpec::new(
            "X",
            InstallSource::Tar {
                path: r"D:\a.tar".to_owned(),
            },
        );
        relative.install_dir = r"wsl\X".to_owned();
        let err = super::plan(&relative, &ctx()).unwrap_err();
        assert!(err.contains("绝对路径"), "{err}");

        // 在线安装：没选发行版
        let no_id = InstallSpec::new(
            "X",
            InstallSource::Online {
                id: "  ".to_owned(),
                launch: false,
                web_download: false,
                fallback_id: None,
            },
        );
        assert!(super::plan(&no_id, &ctx()).unwrap_err().contains("请先选"));
        // 在线安装要改名却没目录
        let no_dir = InstallSpec::new(
            "X",
            InstallSource::Online {
                id: "Ubuntu".to_owned(),
                launch: false,
                web_download: false,
                fallback_id: None,
            },
        );
        let err = super::plan(&no_dir, &bare).unwrap_err();
        assert!(err.contains("安装目录"), "{err}");

        // 镜像站没选 URL / URL 形状不对
        let no_url = InstallSpec::new(
            "X",
            InstallSource::Mirror {
                url: String::new(),
                mirror: "清华 TUNA".to_owned(),
                release: "noble".to_owned(),
                format: "tar.xz".to_owned(),
            },
        );
        assert!(super::plan(&no_url, &ctx())
            .unwrap_err()
            .contains("还没有选好下载地址"));
        let bad_url = InstallSpec::new(
            "X",
            InstallSource::Mirror {
                url: r"D:\a.tar.xz".to_owned(),
                mirror: "本地".to_owned(),
                release: "noble".to_owned(),
                format: "tar.xz".to_owned(),
            },
        );
        assert!(super::plan(&bad_url, &ctx()).unwrap_err().contains("http"));
    }

    #[test]
    fn preflight_blocks_duplicate_names_and_non_empty_dirs() {
        let spec = InstallSpec::new(
            "Ubuntu",
            InstallSource::Tar {
                path: r"D:\a.tar".to_owned(),
            },
        );
        let taken = vec!["Ubuntu".to_owned(), "Debian".to_owned()];

        let clean = preflight(&spec, &ctx(), &["Debian".to_owned()], false);
        assert!(clean.ok(), "{clean:?}");

        let duplicate = preflight(&spec, &ctx(), &taken, false);
        assert!(!duplicate.ok());
        assert!(duplicate.error_text().unwrap().contains("已经有一个"));
        // 重名这件事**不能**让用户以为我们会替他删
        assert!(duplicate.error_text().unwrap().contains("不会替你删"));

        let occupied = preflight(&spec, &ctx(), &[], true);
        assert!(!occupied.ok());
        assert!(occupied.error_text().unwrap().contains("空目录"));
    }

    #[test]
    fn field_errors_map_messages_to_the_right_input() {
        // 名字 + 路径 + 目录三条一起错：三块都要标红。
        let spec = InstallSpec::new(
            "  ",
            InstallSource::Tar {
                path: " ".to_owned(),
            },
        );
        let bare = PlanContext {
            default_dir: None,
            ..ctx()
        };
        let check = preflight(&spec, &bare, &[], false);
        let fields = check.field_errors();
        assert!(check.errors.len() >= 3, "{check:?}");
        assert!(fields.name, "{check:?} / {fields:?}");
        assert!(fields.source, "{check:?} / {fields:?}");
        assert!(fields.install_dir, "{check:?} / {fields:?}");
        assert!(fields.any());

        // 重名只算名字，不要把目录也标红。
        let duplicate = InstallSpec::new(
            "Ubuntu",
            InstallSource::Tar {
                path: r"D:\a.tar".to_owned(),
            },
        );
        let fields = preflight(&duplicate, &ctx(), &["Ubuntu".to_owned()], false).field_errors();
        assert!(fields.name, "{fields:?}");
        assert!(!fields.source, "{fields:?}");
        assert!(!fields.install_dir, "{fields:?}");

        // 目录已经有东西：只标目录。
        let fields = preflight(&duplicate, &ctx(), &[], true).field_errors();
        assert!(!fields.name, "{fields:?}");
        assert!(!fields.source, "{fields:?}");
        assert!(fields.install_dir, "{fields:?}");

        // 在线来源没选：只标来源。
        let online = InstallSpec::new(
            "X",
            InstallSource::Online {
                id: String::new(),
                launch: false,
                web_download: false,
                fallback_id: None,
            },
        );
        let fields = preflight(&online, &ctx(), &[], false).field_errors();
        assert!(!fields.name, "{fields:?}");
        assert!(fields.source, "{fields:?}");
    }

    #[test]
    fn preflight_treats_names_case_insensitively() {
        // ⚠️ 这条是"成功的安装被判成失败"那个 bug 的回归保护。
        //
        // 已经有 `Ubuntu` 时用户装 `ubuntu`：preflight 若按 `==` 判，
        // 这里会放行；而装完之后 `WaitRegistered` 拿 `==` 在 `wsl -l -q`
        // 的输出里找不到 `ubuntu`（那里写的是 `Ubuntu`），
        // 于是轮询 30 秒后报"装完了却一直没出现在列表里"。
        // WSL 自己对发行版名的比较就是不区分大小写的。
        let spec = InstallSpec::new(
            "ubuntu",
            InstallSource::Tar {
                path: r"D:\a.tar".to_owned(),
            },
        );
        let taken = vec!["Ubuntu".to_owned()];

        let duplicate = preflight(&spec, &ctx(), &taken, false);
        assert!(
            !duplicate.ok(),
            "大小写不同但其实是同一个发行版，必须挡住：{duplicate:?}"
        );
        assert!(duplicate.error_text().unwrap().contains("已经有一个"));

        // 反方向也要成立（清单给 `Ubuntu`、列表里是 `ubuntu`）
        let spec = InstallSpec::new(
            "Ubuntu",
            InstallSource::Tar {
                path: r"D:\a.tar".to_owned(),
            },
        );
        let duplicate = preflight(&spec, &ctx(), &["ubuntu".to_owned()], false);
        assert!(
            duplicate.error_text().unwrap().contains("已经有一个"),
            "{duplicate:?}"
        );

        // 前后空白不算差异 —— 用户在名字框里多打一个空格不该被当成重名
        // ⚠️ 必须断言**具体那条错误**：只断言 `!ok()` 的话，
        // "名字非法"这类别的校验也能让它通过，测试就白写了。
        let spec = InstallSpec::new(
            "  Ubuntu  ",
            InstallSource::Tar {
                path: r"D:\a.tar".to_owned(),
            },
        );
        let duplicate = preflight(&spec, &ctx(), &["Ubuntu".to_owned()], false);
        assert!(
            duplicate.error_text().unwrap().contains("已经有一个"),
            "{duplicate:?}"
        );

        // 真的是不同的名字就照旧放行
        let spec = InstallSpec::new(
            "Ubuntux",
            InstallSource::Tar {
                path: r"D:\a.tar".to_owned(),
            },
        );
        let clean = preflight(&spec, &ctx(), &["Ubuntu".to_owned()], false);
        assert!(clean.ok(), "{clean:?}");
    }

    #[test]
    fn preflight_warns_about_the_extra_copy_when_renaming() {
        let spec = InstallSpec::new(
            "MyUbuntu",
            InstallSource::Online {
                id: "Ubuntu-24.04".to_owned(),
                launch: false,
                web_download: false,
                fallback_id: None,
            },
        );
        let check = preflight(&spec, &ctx(), &[], false);
        assert!(check.ok(), "{check:?}");
        assert_eq!(check.warnings.len(), 1, "{check:?}");
        assert!(check.warnings[0].contains("多一次全量拷贝"), "{check:?}");
    }

    #[test]
    fn display_quotes_arguments_that_contain_spaces() {
        let step = PlannedStep::new(
            "x",
            PlanProgram::Wsl,
            vec![
                "--import".to_owned(),
                "My Distro".to_owned(),
                r"D:\a b\x".to_owned(),
            ],
        );
        assert_eq!(step.line(), r#"wsl --import "My Distro" "D:\a b\x""#);
    }
}
