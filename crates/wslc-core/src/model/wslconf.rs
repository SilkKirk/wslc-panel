//! `/etc/wsl.conf` 的**保序**读写模型。
//!
//! # 这个文件和 `.wslconfig` 不是一回事
//!
//! - `.wslconfig`（`%USERPROFILE%` 下）：**整台机器**一份，管 WSL2 虚拟机
//!   —— 内存、网络模式、内核命令行。见 [`crate::wslconfig`]。
//! - `/etc/wsl.conf`（每个发行版**里面**）：管**这一个发行版**
//!   —— 默认用户、要不要把 Windows 的 PATH 塞进来、开机跑什么。
//!
//! 把后者的键写进前者，WSL 会打一行"键未知"然后**忽略它**（实测见
//! [`crate::wslconfig`]）—— 配置看着生效、其实没有。这两个文件是这台机器上
//! 最常见的一处配置错位。
//!
//! # 表驱动
//!
//! 全部 16 个字段都列在 [`FIELDS`] 里（分在 [`SECTIONS`] 下）。界面**遍历**
//! 这两张表来画表单，加一个字段只需要动这里一行 —— 不用改 UI 代码。
//!
//! # 和参考实现最大的不同：**保存时从原文改**
//!
//! `owu/wsl-dashboard`（GPL-3.0，只读来理解机制）把 `wsl.conf` 解析成结构体，
//! 保存时**从头重建**整个文件 —— 于是注释、空行、以及它不认识的新键
//! **全都会丢**；而且它的 UI 把每个字段都 `unwrap_or(默认值)` 再 `Some(...)`
//! 写回，所以"只改一个主机名"也会把 16 个键全写出来，
//! **把 WSL 以后改默认值的机会钉死**。
//!
//! 这里改成：
//!
//! - 以**原文为底**，只动我们管的那几行；
//! - 值只在"文件里写过或用户动过"时才存在；没动过的项**不落进文件**，
//!   界面上显示有效默认值 + 一个「默认」标记。
//!
//! 代价是代码复杂一点（见 [`WslConfDoc::render`]），换来的是
//! "用户手写的注释不会因为我们点了一次保存就消失"，
//! 以及"没碰过的项不会被固化进配置文件"。

use std::collections::BTreeMap;

/// 一个节（`[name]`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Section {
    /// 写进文件里的节名。
    pub name: &'static str,
    /// 界面上的分组标题。
    pub label: &'static str,
    /// 这一节要求的最低 WSL 版本；`None` = 所有版本都支持。
    ///
    /// 依据是参考项目 `check_wsl_version_support` 里的判断
    /// （只读来理解机制，数字是 WSL 自己的发布节奏）：
    /// `[boot]` 要 0.67.6+，`[gpu]` / `[time]` 要 1.0.0+。
    pub requires: Option<&'static str>,
}

/// 全部节，按界面上的顺序。
pub const SECTIONS: &[Section] = &[
    Section {
        name: "automount",
        label: "自动挂载",
        requires: None,
    },
    Section {
        name: "network",
        label: "网络设置",
        requires: None,
    },
    Section {
        name: "interop",
        label: "系统交互",
        requires: None,
    },
    Section {
        name: "user",
        label: "用户设置",
        requires: None,
    },
    Section {
        name: "boot",
        label: "启动设置",
        requires: Some("0.67.6"),
    },
    Section {
        name: "gpu",
        label: "GPU",
        requires: Some("1.0.0"),
    },
    Section {
        name: "time",
        label: "时间",
        requires: Some("1.0.0"),
    },
];

/// 字段的类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    /// 布尔开关（写进文件是 `true` / `false`）。
    Bool,
    /// 自由文本。
    Text,
}

/// 一个字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Field {
    /// 所属节（必须能在 [`SECTIONS`] 里找到）。
    pub section: &'static str,
    /// 键名 —— **大小写敏感**，按 WSL 文档的驼峰写法。
    pub key: &'static str,
    /// 界面上的标签。
    pub label: &'static str,
    /// 类型。
    pub kind: FieldKind,
    /// 文件里没写时 WSL 用的**有效默认值**。
    ///
    /// 界面上显示它，并标一个「默认」—— 让用户知道"不填就是什么"。
    pub default: &'static str,
    /// 文本字段的占位提示。
    pub placeholder: &'static str,
    /// 一句说明（界面上的小字）。
    pub hint: &'static str,
    /// 这一项是不是**只读**的。
    ///
    /// `[boot] systemd` 是唯一一个：改它要重启才生效，而发行版里没装
    /// systemd 的话会**起不来**，很难救。但**必须解析出来并在保存时原样写回**
    /// —— 不然用户点一次保存就把它抹了。
    pub read_only: bool,
}

/// 全部字段，按界面上的顺序。
///
/// ⚠️ 大小写：节名和键名在 `wsl.conf` 里是**大小写敏感**的
/// （`appendWindowsPath` 不能写成 `appendwindowspath`），所以这里保持
/// WSL 文档里的驼峰写法，比较时也按**原样**比 —— 和 [`crate::wslconfig`]
/// 那边刻意不同：那边是实测 WSL 自己忽略大小写，这边没有实测依据，不猜。
pub const FIELDS: &[Field] = &[
    // ---- [automount] ----
    Field {
        section: "automount",
        key: "enabled",
        label: "自动挂载磁盘",
        kind: FieldKind::Bool,
        default: "true",
        placeholder: "",
        hint: "关掉之后 /mnt/c 这类 Windows 盘符不会自动出现。",
        read_only: false,
    },
    Field {
        section: "automount",
        key: "mountFsTab",
        label: "处理 /etc/fstab",
        kind: FieldKind::Bool,
        default: "true",
        placeholder: "",
        hint: "启动时按发行版里的 /etc/fstab 挂载。",
        read_only: false,
    },
    Field {
        section: "automount",
        key: "root",
        label: "挂载根目录",
        kind: FieldKind::Text,
        default: "/mnt/",
        placeholder: "/mnt/",
        hint: "Windows 盘符挂到哪儿。改了之后路径都跟着变，注意别让已有脚本失效。",
        read_only: false,
    },
    Field {
        section: "automount",
        key: "options",
        label: "挂载参数",
        kind: FieldKind::Text,
        default: "",
        placeholder: "例如: metadata,uid=1000",
        hint: "逗号分隔，会传给 mount。`metadata` 能保留 Linux 权限位。",
        read_only: false,
    },
    // ---- [network] ----
    Field {
        section: "network",
        key: "generateHosts",
        label: "生成 /etc/hosts",
        kind: FieldKind::Bool,
        default: "true",
        placeholder: "",
        hint: "关掉之后要自己维护 /etc/hosts。",
        read_only: false,
    },
    Field {
        section: "network",
        key: "generateResolvConf",
        label: "生成 /etc/resolv.conf",
        kind: FieldKind::Bool,
        default: "true",
        placeholder: "",
        hint: "关掉之后要自己配 DNS，通常和 systemd-resolved 一起用。",
        read_only: false,
    },
    Field {
        section: "network",
        key: "hostname",
        label: "计算机名",
        kind: FieldKind::Text,
        default: "",
        placeholder: "自定义主机名",
        hint: "留空就用 WSL 自己生成的名字。",
        read_only: false,
    },
    // ---- [interop] ----
    Field {
        section: "interop",
        key: "enabled",
        label: "启用 Windows 交互",
        kind: FieldKind::Bool,
        default: "true",
        placeholder: "",
        hint: "关掉之后在 Linux 里跑不了 .exe。",
        read_only: false,
    },
    Field {
        section: "interop",
        key: "appendWindowsPath",
        label: "追加 Windows 路径",
        kind: FieldKind::Bool,
        default: "true",
        placeholder: "",
        hint: "关掉能让 Linux 里的命令解析快不少（PATH 里少一大串 /mnt/c/...）。",
        read_only: false,
    },
    // ---- [user] ----
    Field {
        section: "user",
        key: "default",
        label: "默认登录用户",
        kind: FieldKind::Text,
        default: "",
        placeholder: "例如: root, ubuntu",
        hint: "保存前会检查这个用户在发行版里**是否存在** —— 写错了发行版下次启动会失败。",
        read_only: false,
    },
    // ---- [boot] ----
    Field {
        section: "boot",
        key: "systemd",
        label: "启用 systemd",
        kind: FieldKind::Bool,
        default: "false",
        placeholder: "",
        hint: "只读：改它要重启才生效，而发行版里没装 systemd 的话会**起不来**。\
               这里只负责把你原来的值原样写回，不会抹掉。",
        read_only: true,
    },
    Field {
        section: "boot",
        key: "command",
        label: "启动命令",
        kind: FieldKind::Text,
        default: "",
        placeholder: "例如: /usr/local/bin/init.sh",
        hint: "每次发行版启动时以 root 跑一次。保存前会检查这个路径是否存在。",
        read_only: false,
    },
    Field {
        section: "boot",
        key: "protectBinfmt",
        label: "保护 binfmt_misc",
        kind: FieldKind::Bool,
        default: "true",
        placeholder: "",
        hint: "防止发行版里的进程往共享的 binfmt_misc 里注册解释器。",
        read_only: false,
    },
    // ---- [gpu] ----
    Field {
        section: "gpu",
        key: "enabled",
        label: "启用 GPU 直通",
        kind: FieldKind::Bool,
        default: "true",
        placeholder: "",
        hint: "关掉之后发行版里看不到宿主机的 GPU。",
        read_only: false,
    },
    // ---- [time] ----
    Field {
        section: "time",
        key: "useWindowsTimezone",
        label: "使用 Windows 时区",
        kind: FieldKind::Bool,
        default: "true",
        placeholder: "",
        hint: "让 Linux 和 Windows 用同一个时区，省得双系统时钟来回跳。",
        read_only: false,
    },
];

/// 按 `(节, 键)` 找一个字段。
pub fn find_field(section: &str, key: &str) -> Option<&'static Field> {
    FIELDS
        .iter()
        .find(|f| f.section == section && f.key == key)
}

/// 找一个节。
pub fn find_section(name: &str) -> Option<&'static Section> {
    SECTIONS.iter().find(|s| s.name == name)
}

/// 这个节在给定 WSL 版本下受不受支持。
///
/// # 解析不出来时**返回 true**（和参考项目相反）
///
/// 参考实现检测失败时把 `[boot]` / `[gpu]` / `[time]` 整节**藏起来**。
/// 这里选择**显示**：藏起来意味着用户根本没法配它，
/// 而"版本没认出来"不该有这种后果；真写了不支持的键，
/// WSL 也只会打一行"键未知"然后忽略，代价小得多。
pub fn section_supported(name: &str, wsl_version: &str) -> bool {
    let Some(section) = find_section(name) else {
        return false;
    };
    match section.requires {
        None => true,
        Some(min) => version_at_least(wsl_version, min).unwrap_or(true),
    }
}

/// `version >= min` 吗？任一边解析不出来就返回 `None`。
///
/// 版本号形如 `2.6.1.0`（`wsl --version` 里那行）。按**数字段**逐段比，
/// 缺的段当 0（`2.6` 和 `2.6.0` 一样）。
fn version_at_least(version: &str, min: &str) -> Option<bool> {
    let parse = |text: &str| -> Option<Vec<u64>> {
        let parts: Vec<u64> = text
            .trim()
            .split('.')
            .map(|p| p.trim().parse::<u64>())
            .collect::<std::result::Result<_, _>>()
            .ok()?;
        if parts.is_empty() { None } else { Some(parts) }
    };

    let actual = parse(version)?;
    let required = parse(min)?;
    let len = actual.len().max(required.len());

    for i in 0..len {
        let a = actual.get(i).copied().unwrap_or(0);
        let b = required.get(i).copied().unwrap_or(0);
        if a != b {
            return Some(a > b);
        }
    }
    Some(true)
}

/// 一份 `wsl.conf` 文档：原文 + 我们管的值。
///
/// **`original` 必须留着** —— [`WslConfDoc::render`] 是在它的基础上改，
/// 不是从值重新生成。这是这个类型存在的全部理由。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WslConfDoc {
    /// 读进来的原文（**逐字**保留，含注释和空行）。
    original: String,
    /// 我们管的键的**显式**值：`(节, 键) -> 值`。
    ///
    /// 只在文件里出现过、或用户改过的键才会在这里 ——
    /// **不在 = 跟着 WSL 的默认走**，保存时不会被写出去。
    values: BTreeMap<(String, String), String>,
}

impl WslConfDoc {
    /// 解析一份 `wsl.conf`。
    ///
    /// 解析器**够用就好**：认 `[节]`、认 `键 = 值`、`#` 和 `;` 开头的当注释。
    /// 不做转义、续行、多行值 —— `wsl.conf` 的语法本来就这么简单，
    /// 而且不认识的写法会被**原样保留**（不丢），所以"没解析到"不等于"丢了"。
    pub fn parse(text: &str) -> Self {
        let mut values = BTreeMap::new();
        let mut section = String::new();

        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            if let Some(name) = section_header(line) {
                section = name.to_owned();
                continue;
            }
            if let Some((key, value)) = split_kv(line) {
                // 只收我们管的键；别的留着不动（`render` 会原样抄过去）
                if find_field(&section, key).is_some() {
                    values.insert((section.clone(), key.to_owned()), value.to_owned());
                }
            }
        }

        Self {
            original: text.to_owned(),
            values,
        }
    }

    /// 原文（界面上的"原始内容"用）。
    pub fn original(&self) -> &str {
        &self.original
    }

    /// 取一个键的**显式**值（文件里没写 = `None`）。
    pub fn get(&self, section: &str, key: &str) -> Option<&str> {
        self.values
            .get(&(section.to_owned(), key.to_owned()))
            .map(String::as_str)
    }

    /// 这一项是不是**文件里显式写了**。
    ///
    /// 界面用它决定要不要标一个「默认」—— 让用户分得清
    /// "我看到的是 WSL 的默认"还是"明确设过"。
    pub fn is_explicit(&self, section: &str, key: &str) -> bool {
        self.get(section, key).is_some()
    }

    /// 取**有效值**：文件里写了就用它，没写就用字段的默认值。
    ///
    /// 界面显示用这个。只读字段（`systemd`）也靠它拿到"当前是什么"。
    pub fn effective(&self, section: &str, key: &str) -> String {
        match self.get(section, key) {
            Some(v) => v.to_owned(),
            None => find_field(section, key)
                .map(|f| f.default.to_owned())
                .unwrap_or_default(),
        }
    }

    /// 取**有效布尔值**。
    ///
    /// 认 `true` / `1` / `yes`（大小写不敏感），其余当 `false` ——
    /// 和参考实现的判断一致。文件里没写就用字段默认值。
    pub fn effective_bool(&self, section: &str, key: &str) -> bool {
        let raw = self.effective(section, key);
        matches!(
            raw.trim().to_ascii_lowercase().as_str(),
            "true" | "1" | "yes"
        )
    }

    /// 设一个显式值。不在 [`FIELDS`] 里的键会被忽略。
    pub fn set(&mut self, section: &str, key: &str, value: impl Into<String>) {
        if find_field(section, key).is_none() {
            return;
        }
        self.values
            .insert((section.to_owned(), key.to_owned()), value.into());
    }

    /// 设一个布尔值（写成 `true` / `false`）。
    pub fn set_bool(&mut self, section: &str, key: &str, value: bool) {
        self.set(section, key, if value { "true" } else { "false" });
    }

    /// 清掉显式值 —— 让这一项**回到 WSL 的默认**（保存时会从文件里删掉）。
    pub fn clear(&mut self, section: &str, key: &str) {
        self.values.remove(&(section.to_owned(), key.to_owned()));
    }

    /// 生成新的文件内容。
    ///
    /// # 算法
    ///
    /// 1. 逐行走过原文，记下当前在哪个节；
    /// 2. 碰到**我们管的**键：有显式值就替换那一行，没有就**删掉那一行**；
    /// 3. 其余的行（注释、空行、别的键、我们看不懂的东西）**原样抄过去**；
    /// 4. 最后，把"我们管、原文里没有、但现在有显式值"的键**补进它该在的节**；
    ///    节不存在就在文件末尾新建一个。
    ///
    /// 第 4 步的插入位置是**那一节内容的末尾**（下一个节头之前），
    /// 而不是节头正下方 —— 后者会把用户的键挤到注释前面，很难看。
    pub fn render(&self) -> String {
        let mut out: Vec<String> = Vec::new();
        // 每个节的内容在 `out` 里的结束位置（用于第 4 步插入）
        let mut section_end: Vec<(String, usize)> = Vec::new();
        let mut section = String::new();
        // 原文里已经处理过的 (节, 键)
        let mut seen: Vec<(String, String)> = Vec::new();

        for raw in self.original.lines() {
            let line = raw.trim();

            if let Some(name) = section_header(line) {
                section = name.to_owned();
                out.push(raw.to_owned());
                // 先把节头记上；后面每抄一行内容都会更新它
                section_end.push((section.clone(), out.len()));
                continue;
            }

            let managed = if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                None
            } else {
                split_kv(line)
                    .and_then(|(k, _)| find_field(&section, k))
                    .map(|f| f.key)
            };

            if let Some(key) = managed {
                seen.push((section.clone(), key.to_owned()));
                // 有显式值 → 用**规范化**的写法替换这一行（`key = value`）
                if let Some(value) = self.get(&section, key) {
                    out.push(format!("{key} = {value}"));
                }
                // 没有 → 整行丢掉（用户把这一项恢复成默认了）
            } else {
                out.push(raw.to_owned());
            }

            // 记下这一节内容延伸到了哪里（跳过末尾的空行）
            if !line.is_empty() {
                if let Some(entry) = section_end.iter_mut().rev().find(|(s, _)| *s == section) {
                    entry.1 = out.len();
                }
            }
        }

        // 第 4 步：补上"原文里没有、但现在有显式值"的键。
        //
        // 从后往前插 —— 否则前面插一行会把后面记下的下标全推偏。
        let mut inserts: Vec<(usize, String, Vec<String>)> = Vec::new();
        for section_def in SECTIONS {
            let lines: Vec<String> = FIELDS
                .iter()
                .filter(|f| f.section == section_def.name)
                .filter(|f| !seen.iter().any(|(s, k)| s == f.section && k == f.key))
                .filter_map(|f| self.get(f.section, f.key).map(|v| format!("{} = {v}", f.key)))
                .collect();
            if lines.is_empty() {
                continue;
            }
            match section_end
                .iter()
                .rev()
                .find(|(s, _)| *s == section_def.name)
            {
                Some((_, at)) => inserts.push((*at, section_def.name.to_owned(), lines)),
                // 这一节原文里没有 → 在文件末尾新建
                None => inserts.push((out.len(), section_def.name.to_owned(), lines)),
            }
        }
        inserts.sort_by_key(|(at, _, _)| std::cmp::Reverse(*at));

        for (at, section_name, lines) in inserts {
            let mut block: Vec<String> = Vec::new();
            // 新建的节前面留一个空行（除非文件本来是空的）
            if at >= out.len() && !out.is_empty() {
                block.push(String::new());
            }
            if !section_end.iter().any(|(s, _)| *s == section_name) {
                block.push(format!("[{section_name}]"));
            }
            block.extend(lines);
            for (offset, line) in block.into_iter().enumerate() {
                out.insert(at + offset, line);
            }
        }

        // 用**从原文推断出来的**换行符拼回去。
        //
        // 硬写 `"\n"` 会把 CRLF 文件的整个行尾改成 LF，而 `is_dirty()` 是拿
        // 结果和原文比字节的 —— 那样 CRLF 文件每次打开都算"脏"。
        let eol = if self.original.contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        let mut text = out.join(eol);
        // 结尾换行也跟着**原文**走，而不是无条件补：
        //
        // - 原文以换行结尾 → 补一个（POSIX 的规矩；`cat` 出来的东西不该少这一下）；
        // - 原文是**空文件**   → 补一个（下面要新建节，得让它以换行结尾）；
        // - 原文没有尾换行  → **不补**。否则 `render() != original` 恒成立，
        //   `is_dirty()` 就永远是 true —— 用户什么都没改、点一次保存，
        //   也会整文件重写一遍真实系统配置。这条是被实测踩出来的。
        if !text.is_empty() && (self.original.is_empty() || self.original.ends_with('\n')) {
            text.push_str(eol);
        }
        text
    }

    /// 内容有没有被改过。
    ///
    /// 用 [`WslConfDoc::render`] 的结果和原文比 —— 这样"只是打开看了一下
    /// 又保存"不会写出一个无谓的新文件（也不会在 `.bak` 里留一份垃圾）。
    pub fn is_dirty(&self) -> bool {
        self.render() != self.original
    }
}

/// 认出节头 `[name]`，返回里面的名字。
fn section_header(line: &str) -> Option<&str> {
    let inner = line.strip_prefix('[')?.strip_suffix(']')?;
    Some(inner.trim())
}

/// 拆 `键 = 值`。两边都 trim；值里的 `=` 不受影响（只按**第一个** `=` 拆）。
fn split_kv(line: &str) -> Option<(&str, &str)> {
    let (key, value) = line.split_once('=')?;
    let key = key.trim();
    if key.is_empty() {
        return None;
    }
    Some((key, value.trim()))
}

/// 生成把 `text` 写进 `/etc/wsl.conf` 的 `sh -c` 脚本。
///
/// # 为什么用 heredoc
///
/// 实测（WSL 3.0.1.0）：**多行内容当单个 argv 传给 `wsl.exe` 是可行的**，
/// 而且用**带引号**的 heredoc 定界符（`<< 'EOF'`）之后，
/// `$HOME`、双引号、反斜杠都会**原样写入**，不会被 shell 展开。
///
/// # 定界符为什么要挑
///
/// heredoc 在遇到"某一整行正好等于定界符"时结束。如果内容里真有那么一行，
/// 后面的内容就会漏到 shell 里去执行。所以这里挑一个**内容里没出现过**的
/// 定界符 —— 而不是赌它不会出现。
///
/// 返回的是要交给 `sh -c` 的脚本（不含 `wsl -d ... -e` 那一段）。
pub fn write_script(text: &str) -> String {
    // 定界符必须**独占一行**，heredoc 才会结束。
    //
    // 如果 `text` 没有以换行结尾，脚本最后一行就成了
    // `default = uWSL_CONF_EOF` —— 没有任何一行等于定界符，sh 会以
    // "here-document delimited by end-of-file" 收尾，并把定界符**当正文**
    // 写进 `/etc/wsl.conf`，那一行就成了一个坏配置项（`default` 变成不存在的
    // 用户 → 发行版下次启动直接失败，而错误要到那时候才暴露）。
    //
    // 所以这里**自己兜底**，不指望调用方记得给结尾换行 ——
    // 原先的契约只是"text"，而现有 4 个测试全都传了尾换行，等于没覆盖。
    let body = if text.is_empty() || text.ends_with('\n') {
        text.to_owned()
    } else {
        format!("{text}\n")
    };
    // 定界符要跟**真正写进去的**内容比，不是跟传进来的 `text` 比。
    let delimiter = pick_delimiter(&body);
    format!("cat << '{delimiter}' > /etc/wsl.conf\n{body}{delimiter}\n")
}

/// 挑一个 `text` 里没出现过的 heredoc 定界符。
///
/// 纯函数，能单测 —— 这件事错了会**静默地**把配置写坏（多出来的行会被
/// shell 当命令执行），属于最难查的那一类。
fn pick_delimiter(text: &str) -> String {
    let base = "WSL_CONF_EOF";
    let mut candidate = base.to_owned();
    let mut n = 0;
    while text.lines().any(|line| line.trim_end() == candidate) {
        n += 1;
        candidate = format!("{base}_{n}");
    }
    candidate
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一份"用户手写过"的配置：有注释、有空行、有我们不认识的键。
    const REAL: &str = "\
# 我自己加的注释，别给我删了
[network]
hostname = my-box

[interop]
enabled = true
appendWindowsPath = false

# 下面这个是微软以后可能加的新键，我们不认识
futureKey = keep-me

[user]
default = ubuntu

[automount]
enabled = true
options = \"metadata,umask=22\"
";

    // -- 字段表 ------------------------------------------------------------

    #[test]
    fn every_field_belongs_to_a_known_section() {
        for f in FIELDS {
            assert!(
                find_section(f.section).is_some(),
                "字段 {}.{} 的节不在 SECTIONS 里",
                f.section,
                f.key
            );
        }
    }

    #[test]
    fn there_are_no_duplicate_fields() {
        for (i, a) in FIELDS.iter().enumerate() {
            for b in &FIELDS[i + 1..] {
                assert!(
                    !(a.section == b.section && a.key == b.key),
                    "重复字段：{}.{}",
                    a.section,
                    a.key
                );
            }
        }
    }

    #[test]
    fn every_bool_field_has_a_parseable_default() {
        for f in FIELDS {
            if f.kind == FieldKind::Bool {
                assert!(
                    matches!(f.default, "true" | "false"),
                    "{}.{} 是布尔字段，默认值却是 {:?}",
                    f.section,
                    f.key,
                    f.default
                );
            }
            assert!(!f.label.is_empty(), "{}.{} 没有标签", f.section, f.key);
        }
    }

    #[test]
    fn the_field_table_covers_every_section() {
        // 每一节都得有字段，否则界面上会出现一个空标题
        for s in SECTIONS {
            assert!(
                FIELDS.iter().any(|f| f.section == s.name),
                "节 {} 一个字段都没有",
                s.name
            );
        }
    }

    #[test]
    fn only_systemd_is_read_only() {
        // 钉住这个事实：只读是特例，不是常态。
        let ro: Vec<&str> = FIELDS
            .iter()
            .filter(|f| f.read_only)
            .map(|f| f.key)
            .collect();
        assert_eq!(ro, vec!["systemd"], "{ro:?}");
    }

    #[test]
    fn the_field_table_matches_the_reference_feature_set() {
        // 参考项目 UI 上暴露的全部字段（只读来核对范围，没有抄代码）。
        // 哪天漏掉一个，这条会说话。
        let expected = [
            ("automount", "enabled"),
            ("automount", "mountFsTab"),
            ("automount", "root"),
            ("automount", "options"),
            ("network", "generateHosts"),
            ("network", "generateResolvConf"),
            ("network", "hostname"),
            ("interop", "enabled"),
            ("interop", "appendWindowsPath"),
            ("user", "default"),
            ("boot", "systemd"),
            ("boot", "command"),
            ("boot", "protectBinfmt"),
            ("gpu", "enabled"),
            ("time", "useWindowsTimezone"),
        ];
        for (section, key) in expected {
            assert!(
                find_field(section, key).is_some(),
                "少了字段 {section}.{key}"
            );
        }
        assert_eq!(FIELDS.len(), expected.len(), "字段数量对不上");
    }

    // -- 解析 / 渲染 -------------------------------------------------------

    #[test]
    fn parses_the_keys_we_manage() {
        let doc = WslConfDoc::parse(REAL);
        assert_eq!(doc.get("network", "hostname"), Some("my-box"));
        assert_eq!(doc.get("interop", "enabled"), Some("true"));
        assert_eq!(doc.get("interop", "appendWindowsPath"), Some("false"));
        assert_eq!(doc.get("user", "default"), Some("ubuntu"));
        assert_eq!(doc.get("automount", "enabled"), Some("true"));
        // 没写的就是没写 —— 不能给一个"看起来设过"的值
        assert_eq!(doc.get("boot", "command"), None);
        assert!(!doc.is_explicit("boot", "systemd"));
    }

    #[test]
    fn effective_falls_back_to_the_field_default() {
        let doc = WslConfDoc::parse(REAL);
        assert_eq!(doc.effective("interop", "enabled"), "true");
        assert_eq!(doc.effective("boot", "systemd"), "false");
        assert_eq!(doc.effective("automount", "root"), "/mnt/");
        assert!(doc.effective_bool("automount", "mountFsTab"));
        assert!(!doc.effective_bool("boot", "systemd"));
        // 写了 false 的不能被默认值顶掉
        assert!(!doc.effective_bool("interop", "appendWindowsPath"));
    }

    #[test]
    fn a_fresh_parse_is_not_dirty() {
        let doc = WslConfDoc::parse(REAL);
        assert!(!doc.is_dirty(), "\n{}", doc.render());
    }

    #[test]
    fn render_keeps_comments_blank_lines_and_unknown_keys() {
        // **这是这个模块存在的全部理由。** 参考实现是重建整个文件，
        // 这些全都会丢。
        let mut doc = WslConfDoc::parse(REAL);
        doc.set("network", "hostname", "renamed");
        let out = doc.render();

        assert!(out.contains("# 我自己加的注释，别给我删了"), "{out}");
        assert!(out.contains("futureKey = keep-me"), "{out}");
        assert!(out.contains("[automount]"), "{out}");
        assert!(out.contains("options = \"metadata,umask=22\""), "{out}");
        assert!(out.contains("hostname = renamed"), "{out}");
        assert!(!out.contains("hostname = my-box"), "{out}");
        assert!(out.contains("\n\n"), "{out}");
    }

    #[test]
    fn untouched_fields_are_not_materialized() {
        // 参考实现的 UI 把每个字段都 unwrap_or(默认) 再写回，
        // 于是"只改一个主机名"也会把 16 个键全写出来 ——
        // **把 WSL 以后改默认值的机会钉死**。这里不能那样。
        let mut doc = WslConfDoc::parse("[network]\nhostname = a\n");
        doc.set("network", "hostname", "b");
        let out = doc.render();

        for f in FIELDS {
            if f.key == "hostname" {
                continue;
            }
            assert!(
                !out.contains(&format!("{} =", f.key)),
                "没碰过的 {} 被写进文件了：\n{out}",
                f.key
            );
        }
    }

    #[test]
    fn clearing_a_value_deletes_its_line() {
        let mut doc = WslConfDoc::parse(REAL);
        doc.clear("user", "default");
        let out = doc.render();
        assert!(!out.contains("default = ubuntu"), "{out}");
        // 但 [user] 这个节头本身不是我们管的，留着
        assert!(out.contains("[user]"), "{out}");
        assert!(doc.is_dirty());
    }

    #[test]
    fn setting_a_value_that_was_only_a_default_makes_it_explicit() {
        let mut doc = WslConfDoc::parse("[network]\nhostname = a\n");
        assert!(!doc.is_explicit("automount", "root"));
        // 用户没动 → 不写
        assert!(!doc.render().contains("root ="));
        // 用户动了 → 写
        doc.set("automount", "root", "/win/");
        assert!(doc.render().contains("root = /win/"));
    }

    #[test]
    fn adds_a_key_into_an_existing_section_at_its_end() {
        // 原文有 [interop]，但没写 enabled —— 补进去时应该落在这一节**末尾**，
        // 而不是插在节头正下方（那样会把用户的键挤到注释前面）
        let text =
            "[network]\nhostname = x\n\n[interop]\nappendWindowsPath = false\n\n[user]\ndefault = u\n";
        let mut doc = WslConfDoc::parse(text);
        doc.set("interop", "enabled", "true");
        let out = doc.render();

        let lines: Vec<&str> = out.lines().collect();
        let interop_at = lines.iter().position(|l| *l == "[interop]").unwrap();
        let user_at = lines.iter().position(|l| *l == "[user]").unwrap();
        let enabled_at = lines.iter().position(|l| l.starts_with("enabled")).unwrap();
        let append_at = lines
            .iter()
            .position(|l| l.starts_with("appendWindowsPath"))
            .unwrap();
        assert!(interop_at < append_at && append_at < enabled_at, "{out}");
        assert!(enabled_at < user_at, "{out}");
    }

    #[test]
    fn creates_a_missing_section_at_the_end() {
        let mut doc = WslConfDoc::parse("[network]\nhostname = x\n");
        doc.set("user", "default", "ubuntu");
        let out = doc.render();
        assert!(out.contains("[user]"), "{out}");
        assert!(out.contains("default = ubuntu"), "{out}");
        let net = out.find("[network]").unwrap();
        let user = out.find("[user]").unwrap();
        assert!(net < user, "{out}");
        assert!(out.contains("\n\n[user]"), "{out:?}");
    }

    #[test]
    fn adds_a_whole_section_to_an_empty_file() {
        let mut doc = WslConfDoc::parse("");
        doc.set_bool("interop", "enabled", true);
        assert_eq!(doc.render(), "[interop]\nenabled = true\n");
    }

    #[test]
    fn inserting_into_two_different_sections_does_not_shift_each_other() {
        // 两个节都要插新键 —— 从后往前插，前面的下标才不会被推偏
        let mut doc = WslConfDoc::parse("[network]\nhostname = x\n\n[user]\n");
        doc.set("automount", "root", "/win/");
        doc.set("time", "useWindowsTimezone", "true");
        let out = doc.render();

        assert!(out.contains("[automount]"), "{out}");
        assert!(out.contains("root = /win/"), "{out}");
        assert!(out.contains("[time]"), "{out}");
        assert!(out.contains("useWindowsTimezone = true"), "{out}");
        assert!(out.contains("[network]"), "{out}");
        assert!(out.contains("hostname = x"), "{out}");
        assert!(out.contains("[user]"), "{out}");
    }

    #[test]
    fn only_the_first_equals_sign_splits_a_pair() {
        let doc = WslConfDoc::parse("[automount]\noptions = a=b=c\n");
        // 这个键在 FIELDS 里，所以值应该被正确取到（`=` 后面的全部）
        assert_eq!(doc.get("automount", "options"), Some("a=b=c"));
        assert!(doc.render().contains("options = a=b=c"));
    }

    #[test]
    fn comments_and_semicolons_are_not_treated_as_keys() {
        let doc =
            WslConfDoc::parse("# hostname = nope\n; default = nope\n[network]\nhostname = real\n");
        assert_eq!(doc.get("network", "hostname"), Some("real"));
        assert_eq!(doc.get("user", "default"), None);
    }

    #[test]
    fn section_names_are_case_sensitive() {
        // 我们**不**猜大小写：`[Network]` 不是 `[network]`（没有实测依据
        // 说 WSL 忽略大小写，所以不学 wslconfig 那边的做法）
        let doc = WslConfDoc::parse("[Network]\nhostname = x\n");
        assert_eq!(doc.get("network", "hostname"), None);
        assert!(doc.render().contains("[Network]"));
        assert!(doc.render().contains("hostname = x"));
    }

    #[test]
    fn set_ignores_keys_that_are_not_in_the_field_table() {
        let mut doc = WslConfDoc::parse("");
        doc.set("automount", "madeUpKey", "x");
        assert!(!doc.render().contains("madeUpKey"), "{}", doc.render());
    }

    #[test]
    fn systemd_is_parsed_so_we_do_not_wipe_it() {
        // 表单上 systemd 是只读的，但**必须**解析出来并在保存时写回 ——
        // 不然用户点一次保存就把 systemd 配置抹了
        let doc = WslConfDoc::parse("[boot]\nsystemd = true\n");
        assert_eq!(doc.get("boot", "systemd"), Some("true"));
        assert!(doc.effective_bool("boot", "systemd"));
        assert!(!doc.is_dirty());

        let mut doc = doc;
        doc.set("user", "default", "u");
        let out = doc.render();
        assert!(out.contains("systemd = true"), "{out}");
    }

    #[test]
    fn bool_parsing_accepts_the_usual_spellings() {
        for text in ["true", "TRUE", "1", "yes", "Yes"] {
            let doc = WslConfDoc::parse(&format!("[interop]\nenabled = {text}\n"));
            assert!(doc.effective_bool("interop", "enabled"), "{text}");
        }
        for text in ["false", "FALSE", "0", "no", "whatever"] {
            let doc = WslConfDoc::parse(&format!("[interop]\nenabled = {text}\n"));
            assert!(!doc.effective_bool("interop", "enabled"), "{text}");
        }
    }

    #[test]
    fn round_trip_is_stable() {
        let mut doc = WslConfDoc::parse(REAL);
        doc.set("boot", "command", "/usr/local/bin/init.sh");
        doc.set_bool("time", "useWindowsTimezone", false);
        let once = doc.render();
        let twice = WslConfDoc::parse(&once).render();
        assert_eq!(
            once, twice,
            "\n--- once ---\n{once}\n--- twice ---\n{twice}"
        );
    }

    #[test]
    fn render_is_a_fixpoint_so_a_no_op_save_writes_nothing() {
        // 回归：`render()` 曾经无条件补 `'\n'`、并且用 `"\n"` 拼行，
        // 于是 ① CRLF 文件 ② 原文没有结尾换行的文件，`render() != original`
        // 恒成立 → `is_dirty()` 永远为 true。真实路径上 `read_wsl_conf`
        // 还会再 `trim_end()` 一次，把普通 LF 文件也一起拖下水：
        // 用户**什么都没改**、点一次保存，也会整文件重写用户的 /etc/wsl.conf。
        //
        // 注意：我们管的键本来就会被规范化成 `key = value`（见 `render` 第 3 步），
        // 所以这里的输入都用**规范写法**，否则测的就不是"不动点"而是规范化了。
        for text in [
            "[network]\r\nhostname = my-box\r\n",      // CRLF
            "[network]\r\nhostname = my-box\r\n\r\n",  // CRLF + 结尾空行
            "[network]\nhostname = my-box\n",          // LF
            "[network]\nhostname = my-box\n\n",        // LF + 结尾空行
            "[network]\nhostname = my-box",            // LF，**没有**结尾换行
            "",                                        // 空文件
        ] {
            let doc = WslConfDoc::parse(text);
            assert_eq!(
                doc.render(),
                text,
                "render() 不是原文的不动点，会被判成「有改动」：{text:?}"
            );
            assert!(!doc.is_dirty(), "没改动却判定为脏：{text:?}");
        }
    }

    #[test]
    fn crlf_files_keep_their_line_endings() {
        // 保存一次不能把用户的 CRLF 文件整篇改成 LF。
        let mut doc = WslConfDoc::parse("[network]\r\nhostname = my-box\r\n");
        doc.set("network", "hostname", "renamed");
        let out = doc.render();
        assert!(out.contains("hostname = renamed\r\n"), "{out:?}");
        assert!(
            !out.contains("renamed\n"),
            "CRLF 被改成了 LF：{out:?}"
        );
        // 结尾也得还是 CRLF，不能变成裸 LF
        assert!(out.ends_with("\r\n"), "{out:?}");
    }

    // -- 版本门控 ----------------------------------------------------------

    #[test]
    fn version_comparison_handles_ragged_numbers() {
        assert_eq!(version_at_least("2.6.1.0", "1.0.0"), Some(true));
        assert_eq!(version_at_least("0.67.6", "0.67.6"), Some(true));
        assert_eq!(version_at_least("0.67.5", "0.67.6"), Some(false));
        assert_eq!(version_at_least("0.68.0", "0.67.6"), Some(true));
        // 缺的段当 0：`2.6` 和 `2.6.0` 一样
        assert_eq!(version_at_least("2.6", "2.6.0"), Some(true));
        assert_eq!(version_at_least("2.6", "2.6.1"), Some(false));
        // 高位决胜
        assert_eq!(version_at_least("10.0.0", "9.9.9"), Some(true));
        // 解析不出来
        assert_eq!(version_at_least("", "1.0.0"), None);
        assert_eq!(version_at_least("abc", "1.0.0"), None);
    }

    #[test]
    fn sections_without_a_requirement_are_always_supported() {
        for name in ["automount", "network", "interop", "user"] {
            assert!(section_supported(name, ""), "{name}");
            assert!(section_supported(name, "垃圾版本"), "{name}");
        }
        assert!(!section_supported("不存在的节", "2.0.0"));
    }

    #[test]
    fn version_gating_matches_the_declared_requirements() {
        assert!(!section_supported("boot", "0.60.0"));
        assert!(!section_supported("gpu", "0.60.0"));
        assert!(!section_supported("time", "0.60.0"));
        assert!(section_supported("boot", "0.67.6"));
        assert!(!section_supported("gpu", "0.67.6"));
        assert!(section_supported("boot", "2.6.1.0"));
        assert!(section_supported("gpu", "2.6.1.0"));
        assert!(section_supported("time", "2.6.1.0"));
    }

    #[test]
    fn undetectable_version_keeps_the_sections_visible() {
        // **和参考项目刻意相反**：它检测失败时把这三节藏起来。
        // 藏起来意味着用户根本没法配它，而"版本没认出来"不该有这种后果。
        for name in ["boot", "gpu", "time"] {
            assert!(section_supported(name, ""), "{name}");
            assert!(section_supported(name, "版本号解析不了"), "{name}");
        }
    }

    // -- 写入脚本 ----------------------------------------------------------

    #[test]
    fn write_script_uses_a_quoted_heredoc() {
        let script = write_script("[user]\ndefault = u\n");
        assert!(
            script.starts_with("cat << 'WSL_CONF_EOF' > /etc/wsl.conf\n"),
            "{script}"
        );
        assert!(script.contains("[user]\ndefault = u\n"), "{script}");
        assert!(script.trim_end().ends_with("WSL_CONF_EOF"), "{script}");
    }

    #[test]
    fn write_script_picks_a_delimiter_that_cannot_collide() {
        // 内容里正好有定界符那一行 —— 不换定界符的话，后面的内容会
        // 漏到 shell 里去**执行**。这是静默写坏配置的一类 bug。
        let nasty = "[user]\ndefault = u\nWSL_CONF_EOF\nrm -rf /tmp/oops\n";
        let script = write_script(nasty);
        assert!(!script.starts_with("cat << 'WSL_CONF_EOF'"), "{script}");
        assert!(script.contains("WSL_CONF_EOF_1"), "{script}");
        assert!(script.contains("rm -rf /tmp/oops"), "{script}");
    }

    #[test]
    fn write_script_terminates_even_without_a_trailing_newline() {
        // 回归：`text` 不以换行结尾时，定界符会**粘在最后一行**上，
        // heredoc 不结束 —— sh 报 "here-document delimited by end-of-file"，
        // 并把定界符当正文写进文件（`default = uWSL_CONF_EOF` → 默认用户
        // 变成一个不存在的用户 → 发行版下次启动直接失败）。
        let script = write_script("[user]\ndefault = u");

        // 定界符必须独占一行
        assert!(
            script.ends_with("u\nWSL_CONF_EOF\n"),
            "定界符没有独占一行：{script:?}"
        );
        // 而且不能有"粘在一起"的那一行
        assert!(!script.contains("uWSL_CONF_EOF"), "{script:?}");

        // 空输入也该给出一个合法的空文件写入脚本
        let empty = write_script("");
        assert_eq!(empty, "cat << 'WSL_CONF_EOF' > /etc/wsl.conf\nWSL_CONF_EOF\n");
    }

    #[test]
    fn write_script_survives_repeated_collisions() {
        let nasty = "a\nWSL_CONF_EOF\nWSL_CONF_EOF_1\nWSL_CONF_EOF_2\n";
        let script = write_script(nasty);
        assert!(script.contains("WSL_CONF_EOF_3"), "{script}");
    }
}
