//! 名称与本地文件类型：重名后缀、文件名推导、合法性检查、后缀识别。

use super::*;

/// 名字里允许出现的字符。
///
/// 只放开 A-Z a-z 0-9 `.` `_` `-`：这些字符在注册表键、目录名、命令行里
/// 都不会被任何一层解释成别的东西。中文、空格、`\` `/` 一律不接受 ——
/// 它们的表现（编码、路径分隔、shell 解释）随环境变化，而发行版名是**永久**的。
pub(crate) fn is_name_char(c: char) -> bool {
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
pub(crate) fn name_syntax_errors(name: &str) -> Vec<String> {
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
