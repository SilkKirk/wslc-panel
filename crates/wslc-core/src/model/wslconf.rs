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
//! # 和参考实现最大的不同：**保存时从原文改**
//!
//! `owu/wsl-dashboard`（GPL-3.0，只读来理解机制）的做法是把 `wsl.conf`
//! 解析成一个结构体，保存时**从头重建**整个文件 —— 于是注释、空行、
//! 以及它不认识的新键（微软以后加的）**全都会丢**。
//!
//! 这里改成：**以原文为底，只动我们管的那几行**。别的原样保留。
//! 代价是代码复杂一点（见 [`WslConfDoc::render`]），换来的是
//! "用户手写的注释不会因为我们点了一次保存就消失"。
//!
//! # 我们管哪几个键
//!
//! 只列表单上有的那几个（[`MANAGED`]）。其余的一律原样保留 ——
//! 包括我们**能读懂但没做进表单**的（`automount.*` / `gpu.*` / `time.*`……），
//! 以及**完全不认识**的。

/// 我们**管**的键：`(节, 键)`。
///
/// 只有在这张表里的键才会被 [`WslConfDoc::render`] 改写或删除；
/// 不在表里的一律原样留着。
///
/// ⚠️ 大小写：节名和键名在 `.conf` 里是**大小写敏感**的（`appendWindowsPath`
/// 不能写成 `appendwindowspath`），所以这里保持 WSL 文档里的驼峰写法，
/// 比较时也按**原样**比 —— 和 [`crate::wslconfig`] 那边刻意不同：
/// 那边是实测 WSL 自己忽略大小写，这边没有实测依据，不猜。
pub const MANAGED: &[(&str, &str)] = &[
    ("network", "hostname"),
    ("interop", "enabled"),
    ("interop", "appendWindowsPath"),
    ("user", "default"),
    ("boot", "command"),
    ("boot", "systemd"),
];

/// 我们管的那几个键的**当前值**。
///
/// `None` = 文件里没写（跟着 WSL 的默认走）。
/// 空字符串是**有意义**的（比如 `hostname =`），所以不用空串表示"没写"。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WslConfValues {
    /// `[network] hostname` —— 自定义主机名。
    pub hostname: Option<String>,
    /// `[interop] enabled` —— 能不能从 Linux 里跑 Windows 程序。
    pub interop_enabled: Option<String>,
    /// `[interop] appendWindowsPath` —— 要不要把 Windows 的 PATH 追加进来。
    pub append_windows_path: Option<String>,
    /// `[user] default` —— 默认登录用户。
    pub default_user: Option<String>,
    /// `[boot] command` —— 开机跑的命令。
    pub boot_command: Option<String>,
    /// `[boot] systemd` —— 是否用 systemd 当 PID 1。
    ///
    /// 表单上是**只读**的（改它要重启发行版才生效，而且改坏了很难救），
    /// 但**必须解析出来并在保存时原样写回** —— 不然用户点一次保存
    /// 就把 systemd 配置抹了。
    pub systemd: Option<String>,
}

impl WslConfValues {
    /// 按 `(节, 键)` 取一个值。
    ///
    /// 找不到这个键（不在 [`MANAGED`] 里）时返回 `None` ——
    /// 和"键存在但没写值"（`Some(None)`）是两回事。
    fn lookup(&self, section: &str, key: &str) -> Option<&Option<String>> {
        match (section, key) {
            ("network", "hostname") => Some(&self.hostname),
            ("interop", "enabled") => Some(&self.interop_enabled),
            ("interop", "appendWindowsPath") => Some(&self.append_windows_path),
            ("user", "default") => Some(&self.default_user),
            ("boot", "command") => Some(&self.boot_command),
            ("boot", "systemd") => Some(&self.systemd),
            _ => None,
        }
    }

    /// 按 `(节, 键)` 写一个值。不在 [`MANAGED`] 里的键会被忽略。
    fn assign(&mut self, section: &str, key: &str, value: Option<String>) {
        let slot = match (section, key) {
            ("network", "hostname") => &mut self.hostname,
            ("interop", "enabled") => &mut self.interop_enabled,
            ("interop", "appendWindowsPath") => &mut self.append_windows_path,
            ("user", "default") => &mut self.default_user,
            ("boot", "command") => &mut self.boot_command,
            ("boot", "systemd") => &mut self.systemd,
            _ => return,
        };
        *slot = value;
    }

    /// 这个键有没有值（界面上用它判断"这一项填了没有"）。
    pub fn is_set(&self, section: &str, key: &str) -> bool {
        self.lookup(section, key)
            .is_some_and(|v| v.as_ref().is_some_and(|s| !s.trim().is_empty()))
    }
}

/// 一份 `wsl.conf` 文档：原文 + 我们管的值。
///
/// **`original` 必须留着** —— [`WslConfDoc::render`] 是在它的基础上改，
/// 不是从 [`WslConfValues`] 重新生成。这是这个类型存在的全部理由。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WslConfDoc {
    /// 读进来的原文（**逐字节**保留，含注释和空行）。
    original: String,
    /// 我们管的那几个键。
    values: WslConfValues,
}

impl WslConfDoc {
    /// 解析一份 `wsl.conf`。
    ///
    /// 解析器**够用就好**：认 `[节]`、认 `键 = 值`、`#` 和 `;` 开头的当注释。
    /// 不做转义、续行、多行值 —— `wsl.conf` 的语法本来就这么简单，
    /// 而且不认识的写法会被**原样保留**（不丢），所以"没解析到"不等于"丢了"。
    pub fn parse(text: &str) -> Self {
        let mut values = WslConfValues::default();
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
                values.assign(&section, key, Some(value.to_owned()));
            }
        }

        Self {
            original: text.to_owned(),
            values,
        }
    }

    /// 我们管的那几个值（只读）。
    pub fn values(&self) -> &WslConfValues {
        &self.values
    }

    /// 我们管的那几个值（可改）。
    pub fn values_mut(&mut self) -> &mut WslConfValues {
        &mut self.values
    }

    /// 原文（界面上的"原始内容"用）。
    pub fn original(&self) -> &str {
        &self.original
    }

    /// 生成新的文件内容。
    ///
    /// # 算法
    ///
    /// 1. 逐行走过原文，记下当前在哪个节；
    /// 2. 碰到**我们管的**键：有新值就替换那一行，没值就**删掉那一行**；
    /// 3. 其余的行（注释、空行、别的键、我们看不懂的东西）**原样抄过去**；
    /// 4. 最后，把"我们管、原文里没有、但现在有值"的键**补进它该在的节**；
    ///    节不存在就在文件末尾新建一个。
    ///
    /// 第 4 步的插入位置是**那一节内容的末尾**（下一个节头之前），
    /// 而不是节头正下方 —— 后者会把用户的键挤到注释前面，很难看。
    pub fn render(&self) -> String {
        let mut out: Vec<String> = Vec::new();
        // 每个节的内容在 `out` 里的结束位置（用于第 4 步插入）
        let mut section_end: Vec<(String, usize)> = Vec::new();
        let mut section = String::new();
        // 原文里已经出现过的 (节, 键)
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

            let managed_key = if line.is_empty() || line.starts_with('#') || line.starts_with(';')
            {
                None
            } else {
                split_kv(line).and_then(|(k, _)| {
                    MANAGED
                        .iter()
                        .find(|(s, key)| *s == section && *key == k)
                        .map(|(_, key)| key.to_owned())
                })
            };

            if let Some(key) = managed_key {
                seen.push((section.clone(), key.clone()));
                // 有值 → 用**规范化**的写法替换这一行（`key = value`）
                if let Some(value) = self.values.lookup(&section, &key).and_then(|v| v.clone()) {
                    out.push(format!("{key} = {value}"));
                }
                // 没值 → 整行丢掉（用户把这一项清空了）
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

        // 第 4 步：补上"原文里没有、但现在有值"的键。
        //
        // 从后往前插 —— 否则前面插一行会把后面记下的下标全推偏。
        let mut inserts: Vec<(usize, String, Vec<String>)> = Vec::new();
        for (section_name, keys) in group_missing(&self.values, &seen) {
            let lines: Vec<String> = keys
                .iter()
                .filter_map(|key| {
                    self.values
                        .lookup(&section_name, key)
                        .and_then(|v| v.clone())
                        .map(|value| format!("{key} = {value}"))
                })
                .collect();
            if lines.is_empty() {
                continue;
            }
            match section_end.iter().rev().find(|(s, _)| *s == section_name) {
                Some((_, at)) => inserts.push((*at, section_name, lines)),
                // 这一节原文里没有 → 在文件末尾新建
                None => inserts.push((out.len(), section_name, lines)),
            }
        }
        inserts.sort_by_key(|(at, _, _)| std::cmp::Reverse(*at));

        let had_sections = !section_end.is_empty();
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
        let _ = had_sections;

        let mut text = out.join("\n");
        // 文件末尾留一个换行（POSIX 的规矩；`cat` 出来的东西不该少这一下）
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
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

/// 找出"我们管、原文里没有、但现在有值"的键，按节分组。
///
/// 节的顺序跟 [`MANAGED`] 走 —— 输出稳定，测试才好写。
fn group_missing(values: &WslConfValues, seen: &[(String, String)]) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();

    for (section, key) in MANAGED {
        let already = seen.iter().any(|(s, k)| s == section && k == key);
        if already {
            continue;
        }
        // 没值就不用补
        //
        // 用 `is_some_and` 而不是 `is_none_or`：后者是 Rust 1.82 才稳定的，
        // 而本 crate 声明 MSRV 1.75（见 `crates/wslc-core/Cargo.toml`）。
        if !values.lookup(section, key).is_some_and(|v| v.is_some()) {
            continue;
        }
        match out.iter_mut().find(|(s, _)| s == section) {
            Some((_, keys)) => keys.push((*key).to_owned()),
            None => out.push(((*section).to_owned(), vec![(*key).to_owned()])),
        }
    }

    out
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
    let delimiter = pick_delimiter(text);
    format!("cat << '{delimiter}' > /etc/wsl.conf\n{text}{delimiter}\n")
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

    #[test]
    fn parses_the_keys_we_manage() {
        let doc = WslConfDoc::parse(REAL);
        let v = doc.values();
        assert_eq!(v.hostname.as_deref(), Some("my-box"));
        assert_eq!(v.interop_enabled.as_deref(), Some("true"));
        assert_eq!(v.append_windows_path.as_deref(), Some("false"));
        assert_eq!(v.default_user.as_deref(), Some("ubuntu"));
        assert_eq!(v.boot_command, None);
        assert_eq!(v.systemd, None);
    }

    #[test]
    fn a_fresh_parse_is_not_dirty() {
        // 打开又保存不该写出一个"看起来一样但字节不同"的文件
        let doc = WslConfDoc::parse(REAL);
        assert!(!doc.is_dirty(), "\n{}", doc.render());
    }

    #[test]
    fn render_keeps_comments_blank_lines_and_unknown_keys() {
        // **这是这个模块存在的全部理由。** 参考实现是重建整个文件，
        // 这些全都会丢。
        let mut doc = WslConfDoc::parse(REAL);
        doc.values_mut().hostname = Some("renamed".to_owned());
        let out = doc.render();

        assert!(out.contains("# 我自己加的注释，别给我删了"), "{out}");
        assert!(out.contains("futureKey = keep-me"), "{out}");
        assert!(out.contains("[automount]"), "{out}");
        assert!(out.contains("options = \"metadata,umask=22\""), "{out}");
        assert!(out.contains("hostname = renamed"), "{out}");
        // 被改掉的那一行不该还在
        assert!(!out.contains("hostname = my-box"), "{out}");
        // 空行也得在（用它隔开的两段注释不能粘在一起）
        assert!(out.contains("\n\n"), "{out}");
    }

    #[test]
    fn clearing_a_value_deletes_its_line() {
        let mut doc = WslConfDoc::parse(REAL);
        doc.values_mut().default_user = None;
        let out = doc.render();
        assert!(!out.contains("default = ubuntu"), "{out}");
        // 但 [user] 这个节头本身不是我们管的，留着
        assert!(out.contains("[user]"), "{out}");
        assert!(doc.is_dirty());
    }

    #[test]
    fn adds_a_key_into_an_existing_section_at_its_end() {
        // 原文有 [interop]，但没写 enabled —— 补进去时应该落在这一节**末尾**，
        // 而不是插在节头正下方（那样会把用户的键挤到注释前面）
        let text = "[network]\nhostname = x\n\n[interop]\nappendWindowsPath = false\n\n[user]\ndefault = u\n";
        let mut doc = WslConfDoc::parse(text);
        doc.values_mut().interop_enabled = Some("true".to_owned());
        let out = doc.render();

        let lines: Vec<&str> = out.lines().collect();
        let interop_at = lines.iter().position(|l| *l == "[interop]").unwrap();
        let user_at = lines.iter().position(|l| *l == "[user]").unwrap();
        let enabled_at = lines
            .iter()
            .position(|l| l.starts_with("enabled"))
            .unwrap();
        assert!(interop_at < enabled_at && enabled_at < user_at, "{out}");
        // 而且要在 appendWindowsPath 之后（那是这一节原本的末尾）
        let append_at = lines
            .iter()
            .position(|l| l.starts_with("appendWindowsPath"))
            .unwrap();
        assert!(append_at < enabled_at, "{out}");
    }

    #[test]
    fn creates_a_missing_section_at_the_end() {
        let mut doc = WslConfDoc::parse("[network]\nhostname = x\n");
        doc.values_mut().default_user = Some("ubuntu".to_owned());
        let out = doc.render();
        assert!(out.contains("[user]"), "{out}");
        assert!(out.contains("default = ubuntu"), "{out}");
        // 新节应该在原文之后
        let net = out.find("[network]").unwrap();
        let user = out.find("[user]").unwrap();
        assert!(net < user, "{out}");
        // 新节前面留一个空行，别和上一节粘在一起
        assert!(out.contains("\n\n[user]"), "{out:?}");
    }

    #[test]
    fn adds_a_whole_section_to_an_empty_file() {
        let mut doc = WslConfDoc::parse("");
        doc.values_mut().interop_enabled = Some("true".to_owned());
        let out = doc.render();
        assert_eq!(out, "[interop]\nenabled = true\n");
    }

    #[test]
    fn only_the_first_equals_sign_splits_a_pair() {
        // `options = "metadata,umask=22"` 这种值里带 `=`，不能拆错
        let doc = WslConfDoc::parse("[automount]\noptions = a=b=c\n");
        // 这个键不在 MANAGED 里，所以值取不到；但要确保解析没 panic、
        // 而且它会被原样保留
        assert!(doc.render().contains("options = a=b=c"));
    }

    #[test]
    fn comments_and_semicolons_are_not_treated_as_keys() {
        let doc = WslConfDoc::parse("# hostname = nope\n; default = nope\n[network]\nhostname = real\n");
        assert_eq!(doc.values().hostname.as_deref(), Some("real"));
        assert_eq!(doc.values().default_user, None);
    }

    #[test]
    fn section_names_are_case_sensitive() {
        // 我们**不**猜大小写：`[Network]` 不是 `[network]`（没有实测依据
        // 说 WSL 忽略大小写，所以不学 wslconfig 那边的做法）
        let doc = WslConfDoc::parse("[Network]\nhostname = x\n");
        assert_eq!(doc.values().hostname, None);
        // 但它必须被原样保留
        assert!(doc.render().contains("[Network]"));
        assert!(doc.render().contains("hostname = x"));
    }

    #[test]
    fn systemd_is_parsed_so_we_do_not_wipe_it() {
        // 表单上 systemd 是只读的，但**必须**解析出来并在保存时写回 ——
        // 不然用户点一次保存就把 systemd 配置抹了
        let doc = WslConfDoc::parse("[boot]\nsystemd = true\n");
        assert_eq!(doc.values().systemd.as_deref(), Some("true"));
        assert!(!doc.is_dirty());

        let mut doc = doc;
        doc.values_mut().default_user = Some("u".to_owned());
        let out = doc.render();
        assert!(out.contains("systemd = true"), "{out}");
    }

    #[test]
    fn write_script_uses_a_quoted_heredoc() {
        let script = write_script("[user]\ndefault = u\n");
        // 带引号的定界符：$ / 反引号 / 反斜杠都不会被展开（实测过）
        assert!(script.starts_with("cat << 'WSL_CONF_EOF' > /etc/wsl.conf\n"), "{script}");
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
        // 内容必须完整在脚本里（没被截断）
        assert!(script.contains("rm -rf /tmp/oops"), "{script}");
    }

    #[test]
    fn write_script_survives_repeated_collisions() {
        let nasty = "a\nWSL_CONF_EOF\nWSL_CONF_EOF_1\nWSL_CONF_EOF_2\n";
        let script = write_script(nasty);
        assert!(script.contains("WSL_CONF_EOF_3"), "{script}");
    }

    #[test]
    fn round_trip_is_stable() {
        // 解析 → 渲染 → 再解析，值必须一样（幂等）
        let mut doc = WslConfDoc::parse(REAL);
        doc.values_mut().boot_command = Some("/usr/local/bin/init.sh".to_owned());
        let once = doc.render();
        let twice = WslConfDoc::parse(&once).render();
        assert_eq!(once, twice, "\n--- once ---\n{once}\n--- twice ---\n{twice}");
    }
}
