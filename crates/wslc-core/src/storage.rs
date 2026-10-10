//! 会话存储的位置与占用统计。
//!
//! # 为什么这些要自己算
//!
//! - `wslc` **没有** `system df`（`system` 只有 `events` / `info` / `session`）；
//! - `wslc info` **不报**解析后的 `storagePath`，只报 `settings.yaml` 自己的路径；
//! - 镜像 / 容器 / 卷**共用同一个 `storage.vhdx`**，Windows 侧看到的是一个
//!   不透明的虚拟磁盘文件，分不出内部各占多少。
//!
//! # `storagePath` 的真实语义
//!
//! 来自 `settings.yaml` 自带注释：
//!
//! > Base directory for the default session's storage; the session VHD is created at
//! > `<storagePath>\wslc\sessions\<session>\storage.vhdx`.
//! > Must be an absolute path (e.g. `D:\data`, default: `%LOCALAPPDATA%`).
//!
//! 所以默认情况下 VHD 的真实位置是
//! `%LOCALAPPDATA%\wslc\sessions\<session>\storage.vhdx`。
//!
//! # 一个容易踩的点
//!
//! `settings.yaml` 出厂把**所有**键都写成注释（`# storagePath: default`），
//! 于是我们读到的永远是"未设置"。界面必须把默认值**展开成真实路径**再显示，
//! 否则用户看到的是"（默认：%LOCALAPPDATA%）"这种等于没说的东西。

use std::path::{Path, PathBuf};

/// 内置默认值占位符，与 `settings.yaml` 注释里的写法一致。
pub const DEFAULT_PLACEHOLDER: &str = "%LOCALAPPDATA%";

/// VHD 文件名。
pub const VHD_FILE_NAME: &str = "storage.vhdx";

/// `storagePath` 的来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoragePathOrigin {
    /// `settings.yaml` 里显式配置了。
    Configured,
    /// 没有配置（或写的 `default`）→ 用内置默认值。
    Default,
}

impl StoragePathOrigin {
    /// 界面上显示的说明。
    pub fn label(self) -> &'static str {
        match self {
            Self::Configured => "已在 settings.yaml 中配置",
            Self::Default => "未设置，使用内置默认值",
        }
    }
}

/// 卷的容量信息。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VolumeSpace {
    /// 总容量（字节）。
    pub total: u64,
    /// 当前可用（字节）。
    pub free: u64,
}

impl VolumeSpace {
    /// 已用字节数。
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.free)
    }

    /// 已用百分比（0.0 ~ 100.0）。
    pub fn used_percent(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.used() as f64 / self.total as f64 * 100.0
        }
    }
}

/// 会话存储的解析 + 探测结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageInfo {
    /// `settings.yaml` 里读到的原始值（`None` = 未设置或 `default`）。
    pub configured: Option<String>,
    /// 这个路径是怎么来的。
    pub origin: StoragePathOrigin,
    /// 解析后的 base 目录（即 `<storagePath>`）。
    pub base: PathBuf,
    /// `<base>\wslc\sessions`。
    pub sessions_dir: PathBuf,
    /// 当前会话名（用于定位 VHD）。
    pub session: Option<String>,
    /// 当前会话的 VHD 完整路径。
    pub vhd: Option<PathBuf>,
    /// VHD 的实际占用字节数；文件不存在时为 `None`。
    pub vhd_bytes: Option<u64>,
    /// `sessions` 目录下所有 `storage.vhdx` 的合计字节数。
    pub sessions_bytes: u64,
    /// 找到了几个会话 VHD。
    pub sessions_count: usize,
    /// VHD 所在卷的容量；拿不到时为 `None`。
    pub volume: Option<VolumeSpace>,
}

impl StorageInfo {
    /// `sessions` 目录下其他会话占用的字节数（不含当前会话）。
    pub fn other_sessions_bytes(&self) -> u64 {
        self.sessions_bytes
            .saturating_sub(self.vhd_bytes.unwrap_or(0))
    }

    /// 是否存在除当前会话之外的会话。
    pub fn has_other_sessions(&self) -> bool {
        self.sessions_count > 1
    }
}

// ---------------------------------------------------------------------------
// 纯逻辑（不碰文件系统，可测）
// ---------------------------------------------------------------------------

/// 把 `settings.yaml` 里的原始值规整成"有效配置"。
///
/// `None`、空串、只有空白、以及 `default`（不区分大小写）都算"未设置"——
/// 这是 `settings.yaml` 注释里写明的约定：
/// *All settings support string value "default" which uses built-in defaults.*
pub fn effective_configured(raw: Option<&str>) -> Option<String> {
    let value = raw?.trim();
    if value.is_empty() || value.eq_ignore_ascii_case("default") {
        return None;
    }
    Some(value.to_owned())
}

/// 展开 Windows 风格的环境变量：`%LOCALAPPDATA%\x` → `C:\...\x`。
///
/// 找不到的环境变量**原样保留**（连同百分号），这样用户能看出是哪个变量没定义，
/// 而不是看到一个被悄悄清空的路径。
pub fn expand_env(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '%' {
            if let Some(offset) = chars[i + 1..].iter().position(|c| *c == '%') {
                let name: String = chars[i + 1..i + 1 + offset].iter().collect();
                if !name.is_empty() {
                    if let Ok(value) = std::env::var(&name) {
                        out.push_str(&value);
                        i += offset + 2;
                        continue;
                    }
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }

    out
}

/// 内置默认 base 目录：`%LOCALAPPDATA%` 展开后的结果。
pub fn default_base() -> Option<PathBuf> {
    let expanded = expand_env(DEFAULT_PLACEHOLDER);
    // 环境变量没定义时 `expand_env` 会原样返回 `%LOCALAPPDATA%`，
    // 那不是个可用路径，判掉。
    if expanded == DEFAULT_PLACEHOLDER || expanded.is_empty() {
        return None;
    }
    Some(PathBuf::from(expanded))
}

/// 按 `storagePath` 的语义拼出各个路径。
///
/// 不碰文件系统，纯粹是路径拼接，方便单测。
pub fn layout(configured: Option<&str>, base: PathBuf, session: Option<&str>) -> StorageInfo {
    let sessions_dir = base.join("wslc").join("sessions");
    let vhd = session.map(|s| sessions_dir.join(s).join(VHD_FILE_NAME));

    StorageInfo {
        configured: configured.map(|s| s.to_owned()),
        origin: if configured.is_some() {
            StoragePathOrigin::Configured
        } else {
            StoragePathOrigin::Default
        },
        base,
        sessions_dir,
        session: session.map(|s| s.to_owned()),
        vhd,
        vhd_bytes: None,
        sessions_bytes: 0,
        sessions_count: 0,
        volume: None,
    }
}

// ---------------------------------------------------------------------------
// 文件系统探测
// ---------------------------------------------------------------------------

/// 探测 VHD 与卷的占用情况。
pub fn probe(info: &mut StorageInfo) {
    info.vhd_bytes = info
        .vhd
        .as_ref()
        .and_then(|p| std::fs::metadata(p).ok())
        .filter(|m| m.is_file())
        .map(|m| m.len());

    let mut count = 0usize;
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(&info.sessions_dir) {
        for entry in entries.flatten() {
            if let Ok(meta) = std::fs::metadata(entry.path().join(VHD_FILE_NAME)) {
                if meta.is_file() {
                    count += 1;
                    total = total.saturating_add(meta.len());
                }
            }
        }
    }
    info.sessions_count = count;
    info.sessions_bytes = total;

    info.volume = volume_space(&info.base);
}

/// 解析 + 探测一步到位。
///
/// `configured_raw` 是 `settings.yaml` 里 `session.storagePath` 的原始值；
/// `session` 是当前会话名（来自 `wslc info`）。
pub fn inspect(configured_raw: Option<&str>, session: Option<&str>) -> Option<StorageInfo> {
    let configured = effective_configured(configured_raw);

    let base = match &configured {
        // 配置了就用配置的（可能带 `%VAR%`，一并展开）
        Some(raw) => PathBuf::from(expand_env(raw)),
        // 没配置就用内置默认值
        None => default_base()?,
    };

    let mut info = layout(configured.as_deref(), base, session);
    probe(&mut info);
    Some(info)
}

/// 取路径所在的卷根（`C:\Users\x` → `C:\`）。
///
/// 用卷根而不是原路径去查容量：目标目录可能还不存在（配置了一个尚未创建的
/// `storagePath`），但卷容量仍然是能查到的。
fn volume_root(path: &Path) -> Option<PathBuf> {
    use std::path::Component;
    match path.components().next()? {
        Component::Prefix(prefix) => Some(PathBuf::from(format!(
            "{}\\",
            prefix.as_os_str().to_string_lossy()
        ))),
        Component::RootDir => Some(PathBuf::from("\\")),
        _ => None,
    }
}

/// 查询卷的总容量与可用空间。
///
/// `pub` 是因为「添加实例」要用它做**空间检查**：在线安装改名时要先导出成一个
/// 中转 tar、再导入，那一刻临时盘和目标盘都得放得下整个发行版 ——
/// 与其让用户跑到一半才失败，不如开始前就说清"还差多少"。
#[cfg(windows)]
pub fn volume_space(path: &Path) -> Option<VolumeSpace> {
    use windows::core::HSTRING;
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let root = volume_root(path)?;
    let root = HSTRING::from(root.to_string_lossy().as_ref());

    let mut free_to_caller: u64 = 0;
    let mut total: u64 = 0;
    let mut total_free: u64 = 0;

    // SAFETY: 三个输出参数都是本函数栈上的有效 `u64`；
    // `GetDiskFreeSpaceExW` 只会写入它们，失败时返回 `Err`（我们丢掉）。
    unsafe {
        GetDiskFreeSpaceExW(
            &root,
            Some(&mut free_to_caller),
            Some(&mut total),
            Some(&mut total_free),
        )
    }
    .ok()?;

    Some(VolumeSpace {
        total,
        free: free_to_caller,
    })
}

/// 非 Windows 平台不做探测（本项目只面向 Windows，留着是为了能跨平台 `cargo check`）。
#[cfg(not(windows))]
pub fn volume_space(_path: &Path) -> Option<VolumeSpace> {
    None
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_treated_as_unset() {
        assert_eq!(effective_configured(None), None);
        assert_eq!(effective_configured(Some("")), None);
        assert_eq!(effective_configured(Some("   ")), None);
        assert_eq!(effective_configured(Some("default")), None);
        assert_eq!(effective_configured(Some("DEFAULT")), None);
        assert_eq!(effective_configured(Some(" Default ")), None);
    }

    #[test]
    fn real_path_is_kept() {
        assert_eq!(
            effective_configured(Some(r"D:\data")),
            Some(r"D:\data".to_owned())
        );
        assert_eq!(
            effective_configured(Some("  D:\\data  ")),
            Some(r"D:\data".to_owned())
        );
    }

    #[test]
    fn expand_env_replaces_known_variable() {
        // 刻意用进程里**一定存在**的变量，而不是自己 `set_var`：
        // 测试是多线程跑的，改环境变量会影响其它测试。
        let Some(path_value) = std::env::var_os("PATH") else {
            return;
        };
        let path_value = path_value.to_string_lossy().to_string();

        assert_eq!(expand_env("%PATH%"), path_value);
        assert_eq!(expand_env(r"%PATH%\x"), format!("{path_value}\\x"));
        assert_eq!(
            expand_env("%PATH%%PATH%"),
            format!("{path_value}{path_value}")
        );
    }

    #[test]
    fn expand_env_keeps_unknown_variable_intact() {
        // 宁可原样显示，也不要悄悄替换成空串 —— 用户能看出是哪个变量没定义。
        assert_eq!(
            expand_env(r"%WSLC_DEFINITELY_NOT_SET_12345%\x"),
            r"%WSLC_DEFINITELY_NOT_SET_12345%\x"
        );
    }

    #[test]
    fn expand_env_handles_bare_percent() {
        assert_eq!(expand_env("100%"), "100%");
        assert_eq!(expand_env("%%"), "%%");
        assert_eq!(expand_env(""), "");
    }

    #[test]
    fn layout_follows_the_documented_rule() {
        // 规则来自 settings.yaml 注释：
        // <storagePath>\wslc\sessions\<session>\storage.vhdx
        let info = layout(
            None,
            PathBuf::from(r"C:\Users\me\AppData\Local"),
            Some("wslc-cli-1"),
        );
        assert_eq!(info.origin, StoragePathOrigin::Default);
        assert_eq!(
            info.sessions_dir,
            PathBuf::from(r"C:\Users\me\AppData\Local\wslc\sessions")
        );
        assert_eq!(
            info.vhd.unwrap(),
            PathBuf::from(r"C:\Users\me\AppData\Local\wslc\sessions\wslc-cli-1\storage.vhdx")
        );
    }

    #[test]
    fn layout_marks_configured_origin() {
        let info = layout(Some(r"D:\data"), PathBuf::from(r"D:\data"), Some("s1"));
        assert_eq!(info.origin, StoragePathOrigin::Configured);
        assert_eq!(info.configured.as_deref(), Some(r"D:\data"));
    }

    #[test]
    fn layout_without_session_has_no_vhd() {
        let info = layout(None, PathBuf::from(r"C:\base"), None);
        assert!(info.vhd.is_none());
        assert!(info.session.is_none());
    }

    #[test]
    fn volume_root_extracts_drive() {
        assert_eq!(
            volume_root(Path::new(r"C:\Users\me\AppData\Local")).unwrap(),
            PathBuf::from(r"C:\")
        );
        assert_eq!(
            volume_root(Path::new(r"D:\data")).unwrap(),
            PathBuf::from(r"D:\")
        );
    }

    #[test]
    fn volume_space_math() {
        let v = VolumeSpace {
            total: 1000,
            free: 250,
        };
        assert_eq!(v.used(), 750);
        assert!((v.used_percent() - 75.0).abs() < f64::EPSILON);

        // 总容量为 0 时不能除零
        let zero = VolumeSpace { total: 0, free: 0 };
        assert_eq!(zero.used_percent(), 0.0);
    }

    #[test]
    fn other_sessions_bytes_excludes_current() {
        let mut info = layout(None, PathBuf::from(r"C:\base"), Some("s1"));
        info.vhd_bytes = Some(100);
        info.sessions_bytes = 250;
        info.sessions_count = 2;
        assert_eq!(info.other_sessions_bytes(), 150);
        assert!(info.has_other_sessions());

        info.sessions_count = 1;
        info.sessions_bytes = 100;
        assert_eq!(info.other_sessions_bytes(), 0);
        assert!(!info.has_other_sessions());
    }
}
