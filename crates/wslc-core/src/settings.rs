//! `settings.yaml` 读写。
//!
//! # 为什么不用 `serde_yaml` 整体序列化
//!
//! `wslc` 生成的 `settings.yaml` **每个配置项都带一段英文注释**，
//! 而且所有配置**默认全部是注释掉的**（表示"用内置默认值"）：
//!
//! ```yaml
//! session:
//!   # Number of virtual CPUs allocated to the session (e.g. 4 default: all available CPUs)
//!   # cpuCount: default
//! ```
//!
//! 如果解析成结构体再序列化回去，**这些注释会全部丢失** —— 对一个
//! 用户要长期手工维护的配置文件来说，这是不可接受的破坏性修改。
//!
//! 所以这里采用**面向行的定点改写**：
//! 只动目标键所在的那一行，其余字节原样保留。

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// 配置项的值类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingKind {
    /// 自由文本。
    Text,
    /// 尺寸（`2GB` / `500GB`）。
    Size,
    /// 秒数。
    Seconds,
    /// 正整数。
    PositiveInteger,
    /// 枚举，只能取 [`SettingKey::choices`] 里的值。
    Enum,
}

/// 一条配置项的描述。
#[derive(Debug, Clone, Copy)]
pub struct SettingKey {
    /// 所在小节；`None` 表示顶层键。
    pub section: Option<&'static str>,
    /// 键名。
    pub key: &'static str,
    /// 中文标签。
    pub label: &'static str,
    /// 内置默认值（用于界面提示）。
    pub default: &'static str,
    /// 说明文字。
    pub help: &'static str,
    /// 值类型。
    pub kind: SettingKind,
    /// 允许的取值（仅 [`SettingKind::Enum`]）。
    pub choices: &'static [&'static str],
    /// 改动后是否**不会**影响已有容器（需要在界面上显式警告）。
    pub needs_warning: bool,
}

/// `settings.yaml` 支持的全部配置项（实测采集自 WSL 3.0.1.0）。
pub const SETTING_KEYS: &[SettingKey] = &[
    SettingKey {
        section: Some("session"),
        key: "cpuCount",
        label: "CPU 核数",
        default: "全部可用 CPU",
        help: "分配给会话的虚拟 CPU 数，例如 4。",
        kind: SettingKind::PositiveInteger,
        choices: &[],
        needs_warning: false,
    },
    SettingKey {
        section: Some("session"),
        key: "memorySize",
        label: "内存上限",
        default: "物理内存的一半",
        help: "会话可用的内存上限，例如 2GB。",
        kind: SettingKind::Size,
        choices: &[],
        needs_warning: false,
    },
    SettingKey {
        section: Some("session"),
        key: "maxStorageSize",
        label: "磁盘上限",
        default: "1TB",
        help: "会话磁盘镜像的最大尺寸，例如 500GB。",
        kind: SettingKind::Size,
        choices: &[],
        needs_warning: false,
    },
    SettingKey {
        section: Some("session"),
        key: "storagePath",
        label: "存储路径",
        default: "%LOCALAPPDATA%",
        help: "会话存储的基目录（必须是绝对路径）。VHDX 会创建在 <storagePath>\\wslc\\sessions\\<session>\\storage.vhdx。",
        kind: SettingKind::Text,
        choices: &[],
        // 关键警告：改这个不会迁移已有数据。
        needs_warning: true,
    },
    SettingKey {
        section: Some("session"),
        key: "defaultBindingAddress",
        label: "默认绑定地址",
        default: "127.0.0.1",
        help: "使用 `run -p` 但未指定地址时，发布端口绑定的宿主地址。",
        kind: SettingKind::Text,
        choices: &[],
        needs_warning: false,
    },
    SettingKey {
        section: Some("session"),
        key: "hostLoopback",
        label: "宿主回环 DNS 名",
        default: "host.wslc.internal",
        help: "解析到宿主回环地址的 DNS 名；设为 none 可禁用。",
        kind: SettingKind::Text,
        choices: &[],
        needs_warning: false,
    },
    SettingKey {
        section: Some("session"),
        key: "idleTimeout",
        label: "空闲回收秒数",
        default: "30",
        help: "空闲会话虚拟机在被回收前保持运行的秒数。",
        kind: SettingKind::Seconds,
        choices: &[],
        needs_warning: false,
    },
    SettingKey {
        section: None,
        key: "credentialStore",
        label: "凭据后端",
        default: "wincred",
        help: "镜像仓库凭据的存储方式。",
        kind: SettingKind::Enum,
        choices: &["wincred", "file"],
        needs_warning: false,
    },
];

/// 读取出的**生效值**集合。
///
/// 字段为 `None` 表示该键在文件里是注释状态 —— 也就是"使用内置默认值"。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SettingsValues {
    /// `session.cpuCount`
    pub cpu_count: Option<String>,
    /// `session.memorySize`
    pub memory_size: Option<String>,
    /// `session.maxStorageSize`
    pub max_storage_size: Option<String>,
    /// `session.storagePath`
    pub storage_path: Option<String>,
    /// `session.defaultBindingAddress`
    pub default_binding_address: Option<String>,
    /// `session.hostLoopback`
    pub host_loopback: Option<String>,
    /// `session.idleTimeout`
    pub idle_timeout: Option<String>,
    /// `credentialStore`
    pub credential_store: Option<String>,
}

impl SettingsValues {
    /// 按 [`SETTING_KEYS`] 的顺序取出 `(描述, 生效值)` 列表。
    ///
    /// 刻意**不叫 `iter`**：它返回的是 `Vec` 而不是迭代器，
    /// 叫 `iter` 会遮蔽 `slice::iter`，让 `values.iter().all(...)` 这类写法
    /// 产生"method not found"的迷惑错误（CI 上踩过一次）。
    pub fn entries(&self) -> Vec<(&'static SettingKey, Option<String>)> {
        SETTING_KEYS.iter().map(|k| (k, self.get(k))).collect()
    }

    /// 按配置项描述取生效值。
    pub fn get(&self, key: &SettingKey) -> Option<String> {
        let field = match (key.section, key.key) {
            (Some("session"), "cpuCount") => &self.cpu_count,
            (Some("session"), "memorySize") => &self.memory_size,
            (Some("session"), "maxStorageSize") => &self.max_storage_size,
            (Some("session"), "storagePath") => &self.storage_path,
            (Some("session"), "defaultBindingAddress") => &self.default_binding_address,
            (Some("session"), "hostLoopback") => &self.host_loopback,
            (Some("session"), "idleTimeout") => &self.idle_timeout,
            (None, "credentialStore") => &self.credential_store,
            _ => return None,
        };
        field.clone()
    }
}

/// 一份 `settings.yaml` 文档，支持**保留注释**的定点改写。
#[derive(Debug, Clone)]
pub struct SettingsDoc {
    path: PathBuf,
    raw: String,
    original: String,
}

impl SettingsDoc {
    /// 从路径读取。
    pub fn load(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let raw = fs::read_to_string(&path).map_err(|e| {
            Error::Settings(format!("读取 {} 失败：{e}", path.display()))
        })?;
        Ok(Self {
            path,
            original: raw.clone(),
            raw,
        })
    }

    /// 直接用文本构造（测试与"首次创建"场景使用）。
    pub fn from_text(path: impl Into<PathBuf>, raw: impl Into<String>) -> Self {
        let raw = raw.into();
        Self {
            path: path.into(),
            original: raw.clone(),
            raw,
        }
    }

    /// 配置文件路径。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 完整原文。
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// 相对上次加载/保存是否有改动。
    pub fn is_dirty(&self) -> bool {
        self.raw != self.original
    }

    /// 把改动标记为已保存。
    pub fn mark_saved(&mut self) {
        self.original = self.raw.clone();
    }

    /// 用外部文本整体替换（"原始 YAML 模式"用）。
    pub fn replace_raw(&mut self, raw: impl Into<String>) {
        self.raw = raw.into();
    }

    /// 读取全部生效值。
    pub fn values(&self) -> SettingsValues {
        let get = |section: Option<&str>, key: &str| self.get(section, key);
        SettingsValues {
            cpu_count: get(Some("session"), "cpuCount"),
            memory_size: get(Some("session"), "memorySize"),
            max_storage_size: get(Some("session"), "maxStorageSize"),
            storage_path: get(Some("session"), "storagePath"),
            default_binding_address: get(Some("session"), "defaultBindingAddress"),
            host_loopback: get(Some("session"), "hostLoopback"),
            idle_timeout: get(Some("session"), "idleTimeout"),
            credential_store: get(None, "credentialStore"),
        }
    }

    /// 取某个键的**生效值**；被注释掉时返回 `None`。
    pub fn get(&self, section: Option<&str>, key: &str) -> Option<String> {
        let idx = self.find_key_line(section, key)?;
        let line = self.raw.lines().nth(idx)?;
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            return None;
        }
        let value = extract_value(line)?;
        Some(value)
    }

    /// 设置某个键的值。
    ///
    /// - `value = Some(v)`：写入 `key: v`（若原本是注释状态则取消注释）。
    /// - `value = None`：恢复成注释状态 `# key: default`（即"使用内置默认值"）。
    ///
    /// 返回是否真的改动了内容。
    pub fn set(&mut self, section: Option<&str>, key: &str, value: Option<&str>) -> bool {
        let before = self.raw.clone();

        match value {
            Some(v) => {
                let new_line = match self.find_key_line(section, key) {
                    Some(idx) => {
                        let old = self.line(idx).to_owned();
                        let indent = leading_whitespace(&old);
                        // 只有原本就是"生效行"（`key: 值 # 说明`）时才保留行尾注释。
                        //
                        // 如果原行是被注释掉的（`# key: default`），
                        // `inline_comment` 会把那个 `#` 误认成行尾注释，
                        // 结果生成 `key: 4 # key: default` —— CI 抓到的就是这个。
                        let suffix = if old.trim_start().starts_with('#') {
                            ""
                        } else {
                            inline_comment(&old)
                        };
                        format!("{indent}{key}: {v}{suffix}")
                    }
                    None => {
                        // 键不存在时插入：先找小节块尾，再找顶层文件尾。
                        let indent = self.child_indent(section);
                        let line = format!("{indent}{key}: {v}");
                        let at = self.insert_position(section);
                        let mut new_raw = self.raw.clone();
                        insert_line(&mut new_raw, at, &line);
                        self.raw = new_raw;
                        return self.raw != before;
                    }
                };
                let idx = self.find_key_line(section, key).unwrap();
                let mut lines: Vec<String> = self.raw.lines().map(str::to_owned).collect();
                lines[idx] = new_line;
                self.raw = join_lines(&lines, &self.raw);
            }
            None => {
                let Some(idx) = self.find_key_line(section, key) else {
                    return false;
                };
                let old = self.line(idx).to_owned();
                let indent = leading_whitespace(&old);
                let new_line = format!("{indent}# {key}: default");
                let mut lines: Vec<String> = self.raw.lines().map(str::to_owned).collect();
                lines[idx] = new_line;
                self.raw = join_lines(&lines, &self.raw);
            }
        }

        self.raw != before
    }

    /// 写入文件，并先做一次时间戳备份。
    ///
    /// 返回备份文件路径（原文件不存在时为 `None`）。
    pub fn save_with_backup(&mut self) -> Result<Option<PathBuf>> {
        let backup = if self.path.exists() {
            let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
            let backup = self
                .path
                .with_extension(format!("yaml.bak-{stamp}"));
            fs::copy(&self.path, &backup).map_err(|e| {
                Error::Settings(format!("备份到 {} 失败：{e}", backup.display()))
            })?;
            Some(backup)
        } else {
            None
        };

        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                fs::create_dir_all(parent).map_err(|e| {
                    Error::Settings(format!("创建目录 {} 失败：{e}", parent.display()))
                })?;
            }
        }

        fs::write(&self.path, &self.raw)
            .map_err(|e| Error::Settings(format!("写入 {} 失败：{e}", self.path.display())))?;
        self.mark_saved();
        Ok(backup)
    }

    // -- 内部工具 ----------------------------------------------------------

    fn line(&self, idx: usize) -> &str {
        self.raw.lines().nth(idx).unwrap_or("")
    }

    /// 找到键所在行的下标（无论该行是注释还是生效状态）。
    fn find_key_line(&self, section: Option<&str>, key: &str) -> Option<usize> {
        let (start, end) = self.section_range(section)?;
        (start..end).find(|&i| line_matches_key(self.line(i), key))
    }

    /// 小节的行范围 `[start, end)`。
    ///
    /// 顶层（`section = None`）覆盖整个文件。
    fn section_range(&self, section: Option<&str>) -> Option<(usize, usize)> {
        let total = self.raw.lines().count();
        let Some(section) = section else {
            return Some((0, total));
        };

        let mut start = None;
        for (i, line) in self.raw.lines().enumerate() {
            let trimmed = line.trim_start();
            // 只看顶层键（无缩进、非注释）。
            if !line.starts_with(char::is_whitespace)
                && !trimmed.starts_with('#')
                && section_line_matches(trimmed, section)
            {
                start = Some(i + 1);
                break;
            }
        }
        let start = start?;

        // 块结束于下一个顶层非注释行。
        let mut end = total;
        for (i, line) in self.raw.lines().enumerate().skip(start) {
            let trimmed = line.trim_start();
            if !line.is_empty()
                && !line.starts_with(char::is_whitespace)
                && !trimmed.starts_with('#')
            {
                end = i;
                break;
            }
        }
        Some((start, end))
    }

    /// 推断小节内子项的缩进（取小节内第一条非空行的缩进）。
    fn child_indent(&self, section: Option<&str>) -> String {
        let Some(section) = section else {
            return String::new();
        };
        if let Some((start, end)) = self.section_range(Some(section)) {
            for i in start..end {
                let line = self.line(i);
                if !line.trim().is_empty() {
                    let indent = leading_whitespace(line);
                    if !indent.is_empty() {
                        return indent.to_owned();
                    }
                }
            }
        }
        "  ".to_owned()
    }

    /// 该在哪里插入一个新键。
    fn insert_position(&self, section: Option<&str>) -> usize {
        match section {
            None => self.raw.lines().count(),
            Some(s) => {
                let Some((start, end)) = self.section_range(Some(s)) else {
                    return self.raw.lines().count();
                };
                // 回退跳过块尾的空行。
                let mut at = end;
                while at > start && self.line(at - 1).trim().is_empty() {
                    at -= 1;
                }
                at
            }
        }
    }
}

/// 行是否描述给定键（允许前导空白与可选 `#`）。
fn line_matches_key(line: &str, key: &str) -> bool {
    let stripped = line.trim_start().trim_start_matches('#').trim_start();
    let Some(rest) = stripped.strip_prefix(key) else {
        return false;
    };
    // `key` 后必须紧跟 `:`，避免 `cpuCountFoo` 这类误匹配。
    rest.trim_start().starts_with(':')
}

/// 顶层小节行是否匹配（`session:`）。
fn section_line_matches(trimmed: &str, section: &str) -> bool {
    match trimmed.strip_prefix(section) {
        Some(rest) => rest.trim_start().starts_with(':'),
        None => false,
    }
}

/// 取行首空白。
fn leading_whitespace(line: &str) -> &str {
    let n = line.len() - line.trim_start().len();
    &line[..n]
}

/// 提取 `key: value` 里的 `value`，去掉行尾注释与引号。
fn extract_value(line: &str) -> Option<String> {
    let (_, after) = line.split_once(':')?;
    let value = after.trim();
    // 去掉行尾注释（仅当 `#` 前面有空白时才算注释）。
    let value = match value.find(" #") {
        Some(pos) => value[..pos].trim(),
        None => value,
    };
    if value.is_empty() {
        return None;
    }
    let unquoted = value
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| value.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(value);
    Some(unquoted.to_owned())
}

/// 取行尾注释（含前导空白），没有则返回空串。
fn inline_comment(line: &str) -> &str {
    match line.find(" #") {
        Some(pos) => &line[pos..],
        None => "",
    }
}

/// 在指定行插入一行文本（保留原有换行风格）。
fn insert_line(text: &mut String, at: usize, new_line: &str) {
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let at = at.min(lines.len());
    lines.insert(at, new_line.to_owned());
    *text = join_lines(&lines, text);
}

/// 用原有文本推断换行符，把行拼回去。
fn join_lines(lines: &[String], original: &str) -> String {
    let eol = if original.contains("\r\n") { "\r\n" } else { "\n" };
    let mut out = lines.join(eol);
    // 原文本以换行结尾时保持一致。
    if original.ends_with('\n') || original.is_empty() {
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../tests/fixtures/settings.yaml");

    fn doc() -> SettingsDoc {
        SettingsDoc::from_text("settings.yaml", FIXTURE)
    }

    #[test]
    fn default_fixture_has_every_key_commented_out() {
        let values = doc().values();
        assert_eq!(values.cpu_count, None);
        assert_eq!(values.credential_store, None);
        assert!(values.entries().iter().all(|(_, v)| v.is_none()));
    }

    #[test]
    fn set_uncomments_a_key_in_place() {
        let mut d = doc();
        assert!(d.set(Some("session"), "cpuCount", Some("4")));

        let values = d.values();
        assert_eq!(values.cpu_count.as_deref(), Some("4"));
        assert!(d.raw().contains("\n  cpuCount: 4\n"));
        // 该键上方的英文说明注释必须保留。
        assert!(d.raw().contains("Number of virtual CPUs allocated to the session"));
        // 其它键仍然处于注释状态。
        assert_eq!(d.values().memory_size, None);
        assert!(d.is_dirty());
    }

    #[test]
    fn set_then_unset_restores_commented_form() {
        let mut d = doc();
        d.set(Some("session"), "idleTimeout", Some("120"));
        assert_eq!(d.values().idle_timeout.as_deref(), Some("120"));

        d.set(Some("session"), "idleTimeout", None);
        assert_eq!(d.values().idle_timeout, None);
        assert!(d.raw().contains("# idleTimeout: default"));
    }

    #[test]
    fn top_level_key_is_edited_outside_any_section() {
        let mut d = doc();
        assert!(d.set(None, "credentialStore", Some("file")));
        assert_eq!(d.values().credential_store.as_deref(), Some("file"));
        assert!(d.raw().contains("credentialStore: file"));
        // 因为它不在 session 小节里，session 的其它键不受影响。
        assert_eq!(d.values().cpu_count, None);
    }

    #[test]
    fn unrelated_bytes_are_preserved() {
        let mut d = doc();
        let before_percent = d.raw().len();
        d.set(Some("session"), "cpuCount", Some("8"));
        // 只应该多出少量字符（注释符号和 "default" 被替换）。
        let after_percent = d.raw().len();
        assert!(
            after_percent.abs_diff(before_percent) < 64,
            "改写不应大幅改变文件：{before_percent} -> {after_percent}"
        );
        // 头部说明注释原样存在。
        assert!(d.raw().starts_with("# wslc user settings"));
        assert!(d.raw().contains("https://aka.ms/wslc-settings"));
    }

    #[test]
    fn values_round_trip_for_every_key() {
        let mut d = doc();
        for (key, _) in SETTING_KEYS.iter().map(|k| (k, ())) {
            let sample = match key.kind {
                SettingKind::Enum => key.choices[0],
                SettingKind::PositiveInteger => "8",
                SettingKind::Seconds => "60",
                SettingKind::Size => "2GB",
                SettingKind::Text => "sample-value",
            };
            assert!(
                d.set(key.section, key.key, Some(sample)),
                "设置 {} 应产生改动",
                key.key
            );
            assert_eq!(
                d.get(key.section, key.key).as_deref(),
                Some(sample),
                "{} 应能读回",
                key.key
            );
        }
        // 每个键都能被独立读回，且互不干扰。
        let values = d.values();
        assert_eq!(values.cpu_count.as_deref(), Some("8"));
        assert_eq!(values.idle_timeout.as_deref(), Some("60"));
        assert_eq!(values.memory_size.as_deref(), Some("2GB"));
        assert_eq!(values.credential_store.as_deref(), Some("wincred"));
    }

    #[test]
    fn inserting_a_missing_key_lands_inside_its_section() {
        let text = "session:\n  # cpuCount: default\n\ncredentialStore: wincred\n";
        let mut d = SettingsDoc::from_text("s.yaml", text);
        assert!(d.set(Some("session"), "idleTimeout", Some("45")));
        let raw = d.raw();
        let session_pos = raw.find("session:").unwrap();
        let inserted = raw.find("idleTimeout: 45").unwrap();
        let credential_pos = raw.find("credentialStore").unwrap();
        assert!(session_pos < inserted && inserted < credential_pos, "{raw}");
        assert_eq!(d.get(Some("session"), "idleTimeout").as_deref(), Some("45"));
    }

    #[test]
    fn inserting_an_unknown_top_level_key_appends_at_end() {
        let text = "session:\n  # cpuCount: default\n";
        let mut d = SettingsDoc::from_text("s.yaml", text);
        assert!(d.set(None, "credentialStore", Some("file")));
        assert!(d.raw().trim_end().ends_with("credentialStore: file"));
    }

    #[test]
    fn missing_key_returns_none_instead_of_panicking() {
        let d = SettingsDoc::from_text("s.yaml", "session:\n");
        assert_eq!(d.get(Some("session"), "cpuCount"), None);
        assert_eq!(d.get(Some("nope"), "cpuCount"), None);
        assert_eq!(d.get(None, "credentialStore"), None);
    }

    #[test]
    fn key_prefix_is_not_matched_by_longer_key() {
        // `cpuCountFoo` 不应被当成 `cpuCount`。
        assert!(line_matches_key("  # cpuCount: default", "cpuCount"));
        assert!(line_matches_key("  cpuCount: 4", "cpuCount"));
        assert!(line_matches_key("cpuCount: 4", "cpuCount"));
        assert!(!line_matches_key("  # cpuCountFoo: 1", "cpuCount"));
        assert!(!line_matches_key("  memorySize: 2GB", "cpuCount"));
    }

    #[test]
    fn extract_value_strips_quotes_and_trailing_comments() {
        assert_eq!(extract_value("  cpuCount: 4").as_deref(), Some("4"));
        assert_eq!(
            extract_value("  storagePath: D:\\data").as_deref(),
            Some("D:\\data")
        );
        assert_eq!(
            extract_value(r#"  hostLoopback: "host.wslc.internal""#).as_deref(),
            Some("host.wslc.internal")
        );
        assert_eq!(
            extract_value("  idleTimeout: 30 # 秒").as_deref(),
            Some("30")
        );
        assert_eq!(extract_value("  cpuCount:"), None);
        assert_eq!(extract_value("nonsense"), None);
    }

    #[test]
    fn crlf_files_keep_their_line_endings() {
        let text = "session:\r\n  # cpuCount: default\r\n";
        let mut d = SettingsDoc::from_text("s.yaml", text);
        d.set(Some("session"), "cpuCount", Some("4"));
        assert!(d.raw().contains("\r\n"));
        assert!(!d.raw().contains("\n\n"));
        assert_eq!(d.get(Some("session"), "cpuCount").as_deref(), Some("4"));
    }

    #[test]
    fn replace_raw_supports_the_raw_yaml_mode() {
        let mut d = doc();
        d.replace_raw("session:\n  cpuCount: 16\n");
        assert_eq!(d.values().cpu_count.as_deref(), Some("16"));
        assert!(d.is_dirty());

        d.mark_saved();
        assert!(!d.is_dirty());
    }

    #[test]
    fn set_reports_no_change_when_value_is_identical() {
        let mut d = doc();
        d.set(Some("session"), "cpuCount", Some("4"));
        assert!(
            !d.set(Some("session"), "cpuCount", Some("4")),
            "写入相同值不应报告改动"
        );
    }

    #[test]
    fn setting_a_nonexistent_section_key_still_terminates() {
        let mut d = doc();
        // 小节不存在时走 "追加到文件尾" 分支，不应死循环或 panic。
        let changed = d.set(Some("noSuchSection"), "foo", Some("bar"));
        assert!(changed);
        assert!(d.raw().contains("foo: bar"));
    }

    #[test]
    fn schema_covers_every_documented_key() {
        let keys: Vec<(&str, &str)> = SETTING_KEYS
            .iter()
            .map(|k| (k.section.unwrap_or(""), k.key))
            .collect();
        assert!(keys.contains(&("session", "cpuCount")));
        assert!(keys.contains(&("session", "memorySize")));
        assert!(keys.contains(&("session", "maxStorageSize")));
        assert!(keys.contains(&("session", "storagePath")));
        assert!(keys.contains(&("session", "defaultBindingAddress")));
        assert!(keys.contains(&("session", "hostLoopback")));
        assert!(keys.contains(&("session", "idleTimeout")));
        assert!(keys.contains(&("", "credentialStore")));
        assert_eq!(keys.len(), 8);
    }

    #[test]
    fn only_storage_path_carries_a_warning() {
        let warned: Vec<&str> = SETTING_KEYS
            .iter()
            .filter(|k| k.needs_warning)
            .map(|k| k.key)
            .collect();
        assert_eq!(warned, vec!["storagePath"]);
    }
}
