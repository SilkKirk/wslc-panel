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
        } else if (c == '-' || c == '_' || c.is_whitespace()) && !last_was_hyphen && !out.is_empty() {
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
/// # 本机实测：这条命令现在是**坏的**
///
/// 本机（WSL 3.0.1.0）`wsl -l -o` 直接失败：它去 `raw.githubusercontent.com`
/// 取清单，连接被重置，退出码 `-1`（原始输出存成了
/// `tests/fixtures/wsl_list_online_failed.txt`）。所以：
///
/// - **失败输出必须被安静地吃成空列表**（配 `strip_config_warnings` 一起用），
///   而不是抛一个解析错误 —— 上层会用另一个来源兜底（见 `cmd::distro::list_online`）；
/// - ⚠️ 下面测试里那条"成功形状"的样本**不是本机抓的**（本机抓不到），
///   它来自微软文档与 `wsl --help` 的说明，属于**未在本机验证的假设**。
///   真机上第一次成功拉到列表时，应当把输出存成 fixture 替换掉它。
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
        // 表格里可能出现分隔线或额外的说明行，按"第一列必须像个名字"过滤
        if !id.chars().all(is_name_char) {
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
    },
    /// 镜像站：下载 rootfs 再按 [`InstallSource::Tar`] 那条路导入。
    ///
    /// `url` 是**已经选定的**那一个（探测最快镜像的结果，或用户手填的）。
    /// 选哪一个是界面的活（要测速、要显示结果），计划里只记结论 ——
    /// 这样计划是确定的，预览才能和执行完全一致。
    Mirror {
        /// rootfs 的下载地址。
        url: String,
        /// 镜像站名（只用于显示，比如"清华 TUNA"）。
        mirror: String,
        /// 发行版版本（只用于显示，比如 `noble`）。
        release: String,
    },
}

impl InstallSource {
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
}

impl PlannedStep {
    fn new(label: impl Into<String>, program: PlanProgram, args: Vec<String>) -> Self {
        Self {
            label: label.into(),
            program,
            args,
            cancellable: true,
        }
    }

    fn uncancellable(mut self) -> Self {
        self.cancellable = false;
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

    if !name.is_empty() && taken.iter().any(|existing| existing == name) {
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
    if let InstallSource::Online { id, web_download, .. } = &spec.source {
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
                "rootfs 会先下载到临时目录（{release}，几百 MB），装完自动删掉。"
            ));
        }
        warnings.push(
            "镜像站的文件名会随版本变。如果探测/下载报 404，说明那一版的文件改名了，\
             可以换一个版本或用「自定义 URL」。"
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
                out.push("还没有选好镜像 —— 先点「探测最快镜像」，或手填一个 rootfs 的 URL".to_owned());
            } else if !url.starts_with("http://") && !url.starts_with("https://") {
                out.push(format!("rootfs 的 URL 要以 http:// 或 https:// 开头：{url}"));
            }
            if dir.is_none() {
                out.push("从镜像站安装必须指定安装目录".to_owned());
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
            steps.push(create_dir_step(dir.as_str()));
            steps.push(PlannedStep::new(
                format!("把 {path} 展开成发行版 {name}"),
                PlanProgram::Wsl,
                own(&["--import", name.as_str(), dir.as_str(), path, "--version", "2"]),
            ));
            notes.push(
                "`--import` 只是把文件系统铺开，**不会**做首次启动初始化（不建默认用户）。\
                 想要那一步请用「从文件安装」。"
                    .to_owned(),
            );
        }
        InstallSource::Vhdx { path } => {
            let path = path.trim();
            let dir = dir.clone().unwrap_or_default();
            steps.push(create_dir_step(dir.as_str()));
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
        } => {
            let id = id.trim().to_owned();
            let same_name = id == name;
            let target_dir = dir.clone();

            let mut args = own(&["--install", "-d", id.as_str()]);
            if *web_download {
                args.push("--web-download".to_owned());
            }
            // 快路径：名字就是清单里的 id，且给了目录 —— 直接把目录交给 WSL。
            // 装完执行器会用注册表**核实**它是否真的生效，没生效再走重定位。
            if same_name {
                if let Some(dir) = target_dir.as_deref() {
                    args.push("--location".to_owned());
                    args.push(dir.to_owned());
                }
            }
            args.push("--version".to_owned());
            args.push(WSL_VERSION.to_string());
            if !launch {
                args.push("--no-launch".to_owned());
            }
            steps.push(PlannedStep::new(
                format!("从在线源安装 {id}"),
                PlanProgram::Wsl,
                args,
            ));

            steps.push(PlannedStep::new(
                format!("等 {id} 注册完成"),
                PlanProgram::WaitRegistered,
                vec![id.clone()],
            ));

            if !same_name || target_dir.is_some() {
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
                        vec![id.clone(), name.clone(), target_dir, temp_tar.clone()],
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
                        "名字和在线清单里的一致，所以直接把安装位置交给 WSL\
                         （`--install -d {id} --location …`）。装完会用注册表核实它到底装到哪儿了 ——\
                         万一 WSL 没听，再自动走一次重定位补齐。"
                    ));
                }
            }
        }
        InstallSource::Mirror {
            url,
            mirror,
            release,
        } => {
            let url = url.trim();
            let dir = dir.clone().unwrap_or_default();
            let temp_rootfs = format!(
                "{}\\wslc-panel-rootfs-{}-{}.tar.xz",
                ctx.temp_dir.trim_end_matches(['\\', '/']),
                sanitize_name(release),
                ctx.stamp
            );

            steps.push(PlannedStep::new(
                format!("从{mirror}下载 rootfs（{release}）"),
                PlanProgram::Curl,
                crate::mirrors::download_args(url, temp_rootfs.as_str()),
            ));
            steps.push(create_dir_step(dir.as_str()));
            steps.push(PlannedStep::new(
                format!("把下载的 rootfs 展开成发行版 {name}"),
                PlanProgram::Wsl,
                own(&[
                    "--import",
                    name.as_str(),
                    dir.as_str(),
                    temp_rootfs.as_str(),
                    "--version",
                    "2",
                ]),
            ));
            steps.push(PlannedStep::new(
                format!("删掉临时文件 {temp_rootfs}"),
                PlanProgram::RemoveFile,
                vec![temp_rootfs.clone()],
            ));
            notes.push(format!(
                "下载的是镜像站上的官方 rootfs（{url}）；装完会把临时文件删掉。"
            ));
        }
    }

    // 收尾：用户自己在 `.wslconfig` 里要求了稀疏 VHD，就照着做。
    if ctx.wslconfig_sparse {
        steps.push(PlannedStep::new(
            format!("按 .wslconfig 把 {name} 的 VHD 设成稀疏（自动回收空间）"),
            PlanProgram::Wsl,
            own(&["--manage", name.as_str(), "--set-sparse", "true"]),
        ));
        notes.push(
            "你的 `%USERPROFILE%\\.wslconfig` 里开了 `[experimental] sparseVhd`，\
             所以装完会补一条 `--set-sparse true`。"
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

fn create_dir_step(dir: &str) -> PlannedStep {
    PlannedStep::new(
        format!("创建安装目录 {dir}"),
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
        assert_eq!(suggest_name_from_file(r"D:\img\ubuntu-rootfs-amd64.tar.gz"), "ubuntu");
        assert_eq!(suggest_name_from_file("alpine-minirootfs-3.21.0-x86_64.tar.gz"), "alpine-mini-3.21.0");
        assert_eq!(suggest_name_from_file(r"E:\a\Ubuntu-24.04.tar"), "Ubuntu-24.04");
        assert_eq!(suggest_name_from_file("noble-server-cloudimg-amd64-root.tar.xz"), "noble-server-cloudimg");
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
    fn name_syntax_rules_match_what_we_tell_users() {
        assert!(name_syntax_errors("Ubuntu-24.04").is_empty());
        assert!(name_syntax_errors("a_b.c-d").is_empty());

        assert!(name_syntax_errors("").iter().any(|e| e.contains("不能为空")));
        assert!(name_syntax_errors("a b").iter().any(|e| e.contains(' ')));
        assert!(name_syntax_errors("中文").iter().any(|e| e.contains('中')));
        assert!(
            name_syntax_errors(&"a".repeat(MAX_NAME_LEN + 1))
                .iter()
                .any(|e| e.contains("不能超过"))
        );
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
        assert_eq!(derive_install_dir(Some("D:/wsl/"), "Ubuntu"), Some("D:/wsl/Ubuntu".to_owned()));
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
        // ⚠️ 这段**不是本机抓的**（本机抓不到成功输出，见上面那条测试）。
        // 形状来自微软文档与 `wsl --help`，属于未在本机验证的假设；
        // 真机上第一次成功拉到列表后应当换成真 fixture。
        let text = concat!(
            "以下是可安装的有效分发的列表：\n",
            "NAME            FRIENDLY NAME\n",
            "Ubuntu          Ubuntu\n",
            "Ubuntu-24.04    Ubuntu 24.04 LTS\n",
            "Debian          Debian GNU/Linux\n"
        );
        let items = parse_online_list(text);
        assert_eq!(items.len(), 3, "{items:?}");
        assert_eq!(items[0].id, "Ubuntu");
        assert_eq!(items[0].label, "Ubuntu");
        assert_eq!(items[1].id, "Ubuntu-24.04");
        assert_eq!(items[1].label, "Ubuntu 24.04 LTS");
        assert_eq!(items[2].label, "Debian GNU/Linux");
    }

    #[test]
    fn online_list_ignores_junk_lines_after_the_header() {
        let text = "NAME  FRIENDLY NAME\n----  -------------\nUbuntu  Ubuntu\n";
        let items = parse_online_list(text);
        // 分隔线被过滤掉（第一列不是合法名字字符）
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
        assert_eq!(items[0].id, "Ubuntu", "默认发行版应该排第一：{:?}", items[0]);
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
        plan.steps.iter().map(|s| format!("{:?}", s.program)).collect()
    }

    #[test]
    fn tar_plan_creates_the_dir_then_imports() {
        let spec = InstallSpec::new(
            "Ubuntu",
            InstallSource::Tar {
                path: r"D:\img\ubuntu.tar".to_owned(),
            },
        );
        let plan = plan(&spec, &ctx()).unwrap();

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
        let plan = plan(&spec, &ctx()).unwrap();
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
        let plan = plan(&spec, &ctx()).unwrap();
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
        let plan = plan(&spec, &ctx()).unwrap();
        assert!(plan.steps[0].args.iter().any(|a| a == r"E:\wsl\MyDistro"));

        // 上下文里也没有默认目录 → 老实不传 --location
        let bare = PlanContext {
            default_dir: None,
            ..ctx()
        };
        let mut no_dir = spec.clone();
        no_dir.install_dir = String::new();
        let plan = plan(&no_dir, &bare).unwrap();
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
            },
        );
        let plan = plan(&spec, &ctx()).unwrap();

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
        assert!(plan.notes.iter().any(|n| n.contains("--unregister")), "{:?}", plan.notes);

        // 要求装完启动时不加 --no-launch
        spec.source = InstallSource::Online {
            id: "Ubuntu-24.04".to_owned(),
            launch: true,
            web_download: true,
        };
        let plan = plan(&spec, &ctx()).unwrap();
        assert!(plan.steps[0].args.iter().any(|a| a == "--web-download"));
        assert!(!plan.steps[0].args.iter().any(|a| a == "--no-launch"));
    }

    #[test]
    fn online_plan_takes_the_fast_path_when_the_name_is_the_id() {
        let spec = InstallSpec::new(
            "Ubuntu-24.04",
            InstallSource::Online {
                id: "Ubuntu-24.04".to_owned(),
                launch: false,
                web_download: false,
            },
        );
        let plan = plan(&spec, &ctx()).unwrap();
        assert_eq!(
            plan.steps[0].args,
            vec![
                "--install",
                "-d",
                "Ubuntu-24.04",
                "--location",
                r"D:\wsl\Ubuntu-24.04",
                "--version",
                "2",
                "--no-launch"
            ]
        );
        // 有目录 → 仍然留一步"必要时重定位"，执行器用注册表核实后才知道要不要做
        assert_eq!(
            labels(&plan),
            vec!["Wsl", "WaitRegistered", "EnsureRelocated"]
        );
        assert!(!plan.steps[2].cancellable);
        // 没有目录、名字也相同 → 连重定位那一步都不需要
        let bare = PlanContext {
            default_dir: None,
            ..ctx()
        };
        let mut no_dir = spec.clone();
        no_dir.install_dir = String::new();
        let plan = plan(&no_dir, &bare).unwrap();
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
            },
        );
        let plan = plan(&spec, &ctx()).unwrap();
        assert_eq!(
            labels(&plan),
            vec!["Curl", "CreateDir", "Wsl", "RemoveFile"]
        );
        assert_eq!(
            plan.steps[0].args,
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
        assert_eq!(plan.steps[2].args[0], "--import");
        // 删掉的必须是**下载下来的那个文件**（curl 参数里 `-o` 后面那个）
        assert_eq!(plan.steps[3].args[0], plan.steps[0].args[8]);
        // 下载那一步是可以取消的
        assert!(plan.steps[0].cancellable);
        assert!(plan.preview_lines()[0].starts_with("1. curl -s -S -L"));
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

        let plan = plan(&spec, &ctx()).unwrap();
        let last = plan.steps.last().unwrap();
        assert_eq!(last.program, PlanProgram::Wsl);
        assert_eq!(last.args, vec!["--set-default", "Ubuntu"]);
        assert!(plan.preview_lines().last().unwrap().contains("--set-default"));

        // `.wslconfig` 要求稀疏 → 在设默认之前插一步
        let sparse_ctx = PlanContext {
            wslconfig_sparse: true,
            ..ctx()
        };
        let plan = plan(&spec, &sparse_ctx).unwrap();
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
        assert!(plan.notes.iter().any(|n| n.contains("sparseVhd")));
    }

    #[test]
    fn plan_rejects_bad_input_with_readable_messages() {
        // 名字为空 + tar 路径为空 + 没目录 → 三条错一起给出来
        let bare = PlanContext {
            default_dir: None,
            ..ctx()
        };
        let spec = InstallSpec::new("  ", InstallSource::Tar { path: " ".to_owned() });
        let err = plan(&spec, &bare).unwrap_err();
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
        let err = plan(&relative, &ctx()).unwrap_err();
        assert!(err.contains("绝对路径"), "{err}");

        // 在线安装：没选发行版
        let no_id = InstallSpec::new(
            "X",
            InstallSource::Online {
                id: "  ".to_owned(),
                launch: false,
                web_download: false,
            },
        );
        assert!(plan(&no_id, &ctx()).unwrap_err().contains("请先选"));
        // 在线安装要改名却没目录
        let no_dir = InstallSpec::new(
            "X",
            InstallSource::Online {
                id: "Ubuntu".to_owned(),
                launch: false,
                web_download: false,
            },
        );
        let err = plan(&no_dir, &bare).unwrap_err();
        assert!(err.contains("安装目录"), "{err}");

        // 镜像站没选 URL / URL 形状不对
        let no_url = InstallSpec::new(
            "X",
            InstallSource::Mirror {
                url: String::new(),
                mirror: "清华 TUNA".to_owned(),
                release: "noble".to_owned(),
            },
        );
        assert!(plan(&no_url, &ctx()).unwrap_err().contains("还没有选好镜像"));
        let bad_url = InstallSpec::new(
            "X",
            InstallSource::Mirror {
                url: r"D:\a.tar.xz".to_owned(),
                mirror: "本地".to_owned(),
                release: "noble".to_owned(),
            },
        );
        assert!(plan(&bad_url, &ctx()).unwrap_err().contains("http"));
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
    fn preflight_warns_about_the_extra_copy_when_renaming() {
        let spec = InstallSpec::new(
            "MyUbuntu",
            InstallSource::Online {
                id: "Ubuntu-24.04".to_owned(),
                launch: false,
                web_download: false,
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
            vec!["--import".to_owned(), "My Distro".to_owned(), r"D:\a b\x".to_owned()],
        );
        assert_eq!(
            step.line(),
            r#"wsl --import "My Distro" "D:\a b\x""#
        );
    }
}
