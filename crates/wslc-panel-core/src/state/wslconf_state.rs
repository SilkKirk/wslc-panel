//! `/etc/wsl.conf` 编辑器的状态与字段 id。

use super::prelude::*;

/// `/etc/wsl.conf` 表单里某个字段的**稳定元素 id**。
///
/// 用 `match` 而不是 `format!`：`Input::id` / `Checkbox::new` 要的是
/// `&'static str`，而且 GPUI 的交互元素本来就要求 id 稳定。
///
/// ⚠️ 这个 match 对应的是**表驱动**的 [`wslc_core::model::wslconf::FIELDS`]，
/// 所以往那张表里加字段时**很容易忘了在这里加一行**。后果不是编译错误：
/// 新字段会静默拿到 `"wslconf-unknown"`；两个字段都忘了加就**撞同一个 id**，
/// 而 GPUI 里 id 撞了会让勾选 / 焦点状态互相串。
/// 测试 `every_wslconf_field_has_its_own_id` 就是盯这件事的。
///
/// （它原先住在 `views.rs`。搬过来的理由见 `AGENTS.md` §2：
/// 这是纯函数，而放在那边就永远没有测试 —— CI 不跑 `wslc-panel` 的单测。）
pub fn wslconf_field_id(section: &str, key: &str) -> &'static str {
    match (section, key) {
        ("automount", "enabled") => "wslconf-automount-enabled",
        ("automount", "mountFsTab") => "wslconf-automount-mountfstab",
        ("automount", "root") => "wslconf-automount-root",
        ("automount", "options") => "wslconf-automount-options",
        ("network", "generateHosts") => "wslconf-network-generatehosts",
        ("network", "generateResolvConf") => "wslconf-network-generateresolvconf",
        ("network", "hostname") => "wslconf-network-hostname",
        ("interop", "enabled") => "wslconf-interop-enabled",
        ("interop", "appendWindowsPath") => "wslconf-interop-appendwindowspath",
        ("user", "default") => "wslconf-user-default",
        ("boot", "systemd") => "wslconf-boot-systemd",
        ("boot", "command") => "wslconf-boot-command",
        ("boot", "protectBinfmt") => "wslconf-boot-protectbinfmt",
        ("gpu", "enabled") => "wslconf-gpu-enabled",
        ("time", "useWindowsTimezone") => "wslconf-time-usewindowstimezone",
        _ => "wslconf-unknown",
    }
}

/// 「发行版配置（`/etc/wsl.conf`）」弹窗的状态。
///
/// **纯数据** —— 输入框（`Entity<InputState>`）在 `Shell::wslconf_dialog` 里，
/// 那是 GPUI 的类型，不能进 `AppState`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WslConfState {
    /// 哪个发行版。
    pub distro: String,
    /// 保序文档：原文 + 我们管的那几个键的显式值。
    pub doc: wslc_core::model::wslconf::WslConfDoc,
    /// WSL 自己的版本号（形如 `2.6.1.0`）；**拿不到时是空串**。
    ///
    /// 界面用它做版本门控（`[boot]` / `[gpu]` / `[time]` 要不要显示）。
    pub wsl_version: String,
    /// 正在**保存**（写 `/etc/wsl.conf`）。
    ///
    /// ⚠️ 只有保存时会置位 —— 打开弹窗时读文件那一趟**没有**置位
    /// （原先这里的注释写着"正在读或者正在写"，与实现不符）。
    /// 界面据此把两个保存按钮换成一行「正在保存…」，见 `views::wslconf_overlay`。
    pub busy: bool,
    /// 保存前的校验错误；非空时挡下保存。
    pub errors: Vec<String>,
    /// 要不要显示"实际会写进去的内容"。
    pub show_preview: bool,
    /// 每次打开按当前文档初始化，避免承接其他发行版的展开状态。
    pub(crate) expanded_sections: BTreeSet<&'static str>,
}

impl WslConfState {
    /// 新建。
    pub fn new(
        distro: impl Into<String>,
        doc: wslc_core::model::wslconf::WslConfDoc,
        wsl_version: impl Into<String>,
    ) -> Self {
        let wsl_version = wsl_version.into();
        let expanded_sections = wslconf::SECTIONS
            .iter()
            .filter(|section| wslconf::section_supported(section.name, &wsl_version))
            .filter(|section| {
                matches!(section.name, "network" | "user")
                    || wslconf::FIELDS.iter().any(|field| {
                        field.section == section.name && doc.is_explicit(field.section, field.key)
                    })
            })
            .map(|section| section.name)
            .collect();
        Self {
            distro: distro.into(),
            doc,
            wsl_version,
            busy: false,
            errors: Vec::new(),
            show_preview: false,
            expanded_sections,
        }
    }

    /// 当前是否展开；不随文档变化重算，保留用户手动收起的选择。
    pub fn section_open(&self, section: &str) -> bool {
        self.expanded_sections.contains(section)
    }

    pub fn set_section_open(&mut self, section: &'static str, open: bool) {
        if wslconf::find_section(section).is_none() {
            return;
        }
        if open {
            self.expanded_sections.insert(section);
        } else {
            self.expanded_sections.remove(section);
        }
    }

    pub fn toggle_section(&mut self, section: &'static str) {
        self.set_section_open(section, !self.section_open(section));
    }

    /// 只汇总面板认识的显式字段，未知键由原文保留。
    pub fn section_summary(&self, section: &str) -> String {
        let values: Vec<String> = wslconf::FIELDS
            .iter()
            .filter(|field| field.section == section && self.doc.is_explicit(section, field.key))
            .map(|field| format!("{}={}", field.label, self.doc.effective(section, field.key)))
            .collect();
        if values.is_empty() {
            "默认".to_owned()
        } else {
            values.join(" · ")
        }
    }

    /// 预览与保存共用，避免未触碰的预填默认值落入显式配置。
    pub fn apply_text_value(&mut self, section: &str, key: &str, value: &str) {
        let Some(field) = wslconf::find_field(section, key) else {
            return;
        };
        if field.kind != wslconf::FieldKind::Text || field.read_only {
            return;
        }
        let value = value.trim();
        if value == self.doc.effective(section, key).trim() {
            return;
        }
        if value.is_empty() {
            self.doc.clear(section, key);
        } else {
            self.doc.set(section, key, value);
        }
    }
}
