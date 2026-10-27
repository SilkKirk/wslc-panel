//! 应用自己的偏好设置。
//!
//! **刻意不写进 `wslc` 的 `settings.yaml`** —— 那个文件由 `wslc` 拥有，
//! 往里塞自定义键既可能被 `wslc` 重置，也会让用户困惑
//! （用户以为自己改的是 wslc 的行为）。
//!
//! 存放位置：`%LOCALAPPDATA%\wslc-panel\prefs.json`

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// 默认自动刷新间隔（秒）。
pub const DEFAULT_REFRESH_SECS: u64 = 3;

/// 允许的最小刷新间隔。再快没有意义 ——
/// 一次采集要串行跑 7 条 `wslc` 命令 + 1 条 `wsl` + 1 条 `reg`。
pub const MIN_REFRESH_SECS: u64 = 1;

/// 允许的最大刷新间隔。
pub const MAX_REFRESH_SECS: u64 = 600;

/// 界面上提供的快捷档位。
pub const REFRESH_PRESETS: &[u64] = &[1, 3, 5, 10, 30];

/// 应用数据目录：`%LOCALAPPDATA%\wslc-panel`。
///
/// 日志、偏好都放在这里。
pub fn app_dir() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(base).join("wslc-panel"))
}

/// 界面主题。
///
/// ⚠️ 这不只是"我们自己画的那些 `div` 用什么颜色"。
/// gpui-component 的控件（`Input` / `Button` / `Select`…）**只认它自己的主题**，
/// 所以切换时必须同时调用 `gpui_kit::component::Theme::change` ——
/// 光改 `theme.rs` 里的颜色，输入框还是浅色的白底浅灰字，根本看不清
/// （`main.rs` 里那段注释记的就是这个坑）。
///
/// 现在是**三选一**：跟随系统 / 深色 / 浅色。`System` 要问系统（读注册表，
/// 见 `wslc_core::cmd::win_theme`），所以本枚举只负责"要不要问"，
/// 真正解析出来的模式由 [`ThemePref::resolve_is_dark`] 给出。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemePref {
    /// 深色（默认）。
    ///
    /// 默认值刻意留成深色而不是"跟随系统"：老版本的 `prefs.json` 没有这个键，
    /// 一旦默认成 System，那些用户的界面会在升级后**自己变样**。
    #[default]
    Dark,
    /// 浅色。
    Light,
    /// 跟随系统。
    System,
}

impl ThemePref {
    /// 全部可选值（决定界面上的按钮顺序：先"跟随系统"再两个固定值）。
    pub const ALL: [ThemePref; 3] = [ThemePref::System, ThemePref::Dark, ThemePref::Light];

    /// 界面标签。
    pub fn label(self) -> &'static str {
        match self {
            Self::System => "跟随系统",
            Self::Dark => "深色",
            Self::Light => "浅色",
        }
    }

    /// 固定值直接给出"是不是深色"；[`ThemePref::System`] 给 `None`（要去问系统）。
    pub fn fixed_is_dark(self) -> Option<bool> {
        match self {
            Self::Dark => Some(true),
            Self::Light => Some(false),
            Self::System => None,
        }
    }

    /// 落到具体模式：`system_is_dark` 是**系统**当前的应用主题。
    ///
    /// 拿不到系统主题时调用方该传 `true`（退回深色，和默认值一致）——
    /// 见 `main.rs` 里那句 `unwrap_or(true)`。
    pub fn resolve_is_dark(self, system_is_dark: bool) -> bool {
        self.fixed_is_dark().unwrap_or(system_is_dark)
    }
}

/// 新终端的起始目录。
///
/// # 为什么需要这个偏好
///
/// `wsl.exe -d <name>` **不带 `--cd`** 时会继承**调用它的那个进程**的工作目录，
/// WSL 再把这个 Windows 路径翻译成 `/mnt/<盘>/...`。于是"从实例列表打开终端"
/// 落在哪儿，取决于用户是从哪个文件夹双击的 `wslc-panel.exe`。本机实测：
///
/// | 面板自己被启动时的 cwd | 终端里的起点 |
/// |---|---|
/// | `C:\Users\76434\Downloads` | `/mnt/c/Users/76434/Downloads` |
/// | `E:\cnb\wslc-panel` | `/mnt/e/cnb/wslc-panel` |
/// | 翻译不了的（UNC / 从 WSL 里起 exe） | `~`＋一行 `Failed to translate` |
///
/// 这既不可预期也不是用户要的，所以默认改成显式 `--cd ~` ——
/// 和 Windows Terminal 自己的 WSL 档案（`startingDirectory` 默认 Linux home）一致。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TerminalStartDir {
    /// 发行版的 Linux home（`--cd ~`）。
    ///
    /// ⚠️ 默认值刻意选了它，而不是"沿用老行为（继承面板 cwd）"：
    /// 老行为不是谁设计出来的，是**从来没人设过 `--cd`** 的副作用，落点随机。
    /// 这一条和 [`ThemePref`] 的默认值取舍刚好相反 —— 那里"保持老行为"是对的，
    /// 这里"保持老行为"等于把一个 bug 保持下去。
    #[default]
    Home,
    /// 继承面板自己的工作目录（= 加 `--cd` 之前的行为）。
    ///
    /// 留着是因为它偶尔有用：把 exe 放进某个项目目录里启动，终端就落在那里。
    /// 但**不默认**选它。
    Panel,
    /// 用户自己写的 Linux 路径。
    ///
    /// 存的是**用户输入的原文**（校验只在 [`parse_custom_dir`] 里做一次，
    /// 读的时候不再校验 —— 手工把 `prefs.json` 改坏了也不该炸）。
    Custom(String),
}

impl TerminalStartDir {
    /// 界面上那排固定档位。「自定义」是单独一个按钮（它要配一个输入框）。
    pub const FIXED: [TerminalStartDir; 2] = [TerminalStartDir::Home, TerminalStartDir::Panel];

    /// 档位名字（不含自定义路径）。
    ///
    /// 自定义那一档也返回一个名字：界面上它是按钮的标签，
    /// 具体路径由 [`Self::describe`] 显示。
    pub fn label(&self) -> &'static str {
        match self {
            Self::Home => "Linux home（~）",
            Self::Panel => "面板所在目录",
            Self::Custom(_) => "自定义路径",
        }
    }

    /// 给人看的当前值（自定义时带上路径）。
    pub fn describe(&self) -> String {
        match self {
            Self::Home => "发行版的 home（~）".to_owned(),
            Self::Panel => "面板所在目录（不传 --cd）".to_owned(),
            Self::Custom(dir) => dir.clone(),
        }
    }

    /// 传给 `wsl.exe --cd` 的值；`None` = **不加** `--cd`（继承面板自己的 cwd）。
    pub fn cd_arg(&self) -> Option<&str> {
        match self {
            Self::Home => Some("~"),
            Self::Panel => None,
            Self::Custom(dir) => Some(dir.as_str()),
        }
    }

    /// 传给 `wslc exec -w` 的值（容器终端）。
    ///
    /// 和 [`Self::cd_arg`] 只差一处，但那处很重要：容器里**没有**"Linux home"
    /// 这个概念 —— `~` 是谁（`/root`？`/home/xxx`？根本不存在？）由**镜像**决定，
    /// 面板不替它猜。而且 `wslc exec -w ~` 到底怎么解释那个 `~`，我们没实测过
    /// （本机一个容器都没有）—— **这一条是推的**，所以干脆不往下传：
    /// 只有明确的绝对路径才给 `-w`，其余一律交给镜像自己的 WORKDIR。
    pub fn container_workdir(&self) -> Option<&str> {
        match self {
            Self::Custom(dir) if dir.starts_with('/') => Some(dir.as_str()),
            _ => None,
        }
    }
}

/// 校验并规范化"自定义起始目录"。
///
/// 只做**语法**校验，不查存在性：路径在发行版里，从 Windows 这边够不着，
/// 每改一次就去起一个 `wsl.exe` 查一遍不值当。而且实测（WSL 3.0.3.0）：
/// `--cd` 指向不存在的目录时 `wsl.exe` **不报错**，只在 stderr 留一行
/// `chdir(...) failed 2`，然后落在 `/` —— 不会变成"点了没反应"。
///
/// 接受 `~`、`~/xxx`（实测 `wsl.exe` 自己会把 `~` 展开成 `/home/<用户>`）
/// 和 `/` 开头的绝对路径。刻意**不接受** Windows 路径：`wsl --cd D:\wsl`
/// 确实会被翻译成 `/mnt/d/wsl`，但让用户在两个坐标系之间猜是最容易出错的，
/// 而"面板所在目录"那一档已经把 Windows 路径这个需求覆盖了。
pub fn parse_custom_dir(input: &str) -> Result<String, String> {
    let dir = input.trim();
    if dir.is_empty() {
        return Err("填一个发行版里的路径，例如 /home/wt/project 或 ~".to_owned());
    }

    // 先单独认一下 Windows 路径：这是**最可能**的输入错误（面板里到处都是
    // `D:\wsl\...` 这种框），值得一句专门的提示。
    let looks_windows = dir.starts_with(r"\\") || dir.as_bytes().get(1) == Some(&b':');
    if looks_windows {
        return Err(format!(
            "{dir} 是 Windows 路径 —— 这里要写**发行版里**的路径（如 /home/wt）。\
             想让终端落在 Windows 目录里，选「面板所在目录」并把面板从那个目录启动。"
        ));
    }

    if dir == "~" || dir.starts_with("~/") || dir.starts_with('/') {
        return Ok(dir.to_owned());
    }

    Err(format!(
        "要 Linux 绝对路径（以 / 开头）或 ~，收到的是：{dir}"
    ))
}

/// 持久化的偏好。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prefs {
    /// 自动刷新间隔（秒）。
    #[serde(default = "default_refresh_secs")]
    pub refresh_secs: u64,
    /// 界面主题。
    ///
    /// 老版本的 `prefs.json` 里没有这个键 —— `#[serde(default)]`
    /// 让它平滑升级到深色（和以前的行为一致）。
    #[serde(default)]
    pub theme: ThemePref,
    /// 「新实例默认安装目录」（「添加实例」页预填 + 由名字推路径）。
    ///
    /// 老版本的 `prefs.json` 里没有这个键 —— `None` 表示"没设过"，
    /// 那时安装目录要用户自己填（和以前的行为一致）。
    #[serde(default)]
    pub install_dir: Option<String>,
    /// 新终端的起始目录（见 [`TerminalStartDir`]）。
    ///
    /// 老版本的 `prefs.json` 里没有这个键 —— `#[serde(default)]` 让它升级成
    /// [`TerminalStartDir::Home`]。⚠️ 这**会改变**老用户看到的行为：以前终端
    /// 继承面板的 cwd（落点随机），升级后固定落在 `~`。这是**有意的**，
    /// 理由写在 [`TerminalStartDir`] 的文档里。
    #[serde(default)]
    pub terminal_dir: TerminalStartDir,
    /// 启动时自动检查更新（默认开）。
    ///
    /// 关掉之后**只有**「关于」页那颗「检查更新」会发请求 —— 有些人就是不想让
    /// 一个本地工具往外连，这不该由我们替他决定。
    ///
    /// ⚠️ 默认值是 `true`：老版本的 `prefs.json` 里没有这个键，
    /// 升级上来会**开始**每天查一次 GitHub。这是有意的（否则更新提示等于没有），
    /// 但它确实是一个新出现的外发请求 —— README 里要写清"查什么、多久一次"。
    #[serde(default = "default_true")]
    pub auto_check_update: bool,
    /// 上次检查更新的时间（Unix 秒）。
    ///
    /// 只用来做 24 小时节流（见 `crate::update::should_auto_check`）。
    /// `None` = 从来没查过。
    #[serde(default)]
    pub last_update_check: Option<u64>,
    /// 用户点了「跳过这个版本」之后记住它，不再为它提示。
    ///
    /// 存的是**用户当时看到的那串版本号**（`0.10.0`），比较时两种写法都认。
    /// 换了新版本之后这个值会被覆盖或清空，不需要额外清理。
    #[serde(default)]
    pub skipped_version: Option<String>,
}

fn default_refresh_secs() -> u64 {
    DEFAULT_REFRESH_SECS
}

fn default_true() -> bool {
    true
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            refresh_secs: DEFAULT_REFRESH_SECS,
            theme: ThemePref::default(),
            install_dir: None,
            terminal_dir: TerminalStartDir::default(),
            auto_check_update: true,
            last_update_check: None,
            skipped_version: None,
        }
    }
}

impl Prefs {
    /// 偏好文件路径。
    pub fn path() -> Option<PathBuf> {
        Some(app_dir()?.join("prefs.json"))
    }

    /// 把 `refresh_secs` 夹到合法范围。
    ///
    /// 手工编辑过 `prefs.json` 的时候会用到 —— 不要因为一个越界值就崩掉。
    pub fn normalized(mut self) -> Self {
        self.refresh_secs = self.refresh_secs.clamp(MIN_REFRESH_SECS, MAX_REFRESH_SECS);
        self
    }

    /// 读取偏好；文件不存在或内容坏了都退回默认值（**不报错**）。
    ///
    /// 一个界面偏好不值得因为解析失败就拦住启动 —— 所以和另外两个存档
    /// 用同一个 `fileio::read_json`（兜底语义一致），只是**不把原因往上传**，
    /// 而是就地 `warn` 一句进日志。这是"三个存档里唯一允许静默"的一个，
    /// 因为它的内容坏掉时用户看不出任何差别，弹一条提示只是噪音。
    pub fn load() -> Self {
        let Some(path) = Self::path() else {
            return Self::default();
        };
        let (prefs, problem): (Prefs, _) =
            crate::fileio::read_json(&path, "偏好文件", "已改用默认值");
        if let Some(problem) = problem {
            tracing::warn!("{problem}");
        }
        prefs.normalized()
    }

    /// 写回磁盘；失败只记录日志，不影响界面。
    pub fn save(&self) -> Result<(), String> {
        let Some(path) = Self::path() else {
            return Err("LOCALAPPDATA 未设置，无法确定偏好文件位置".to_owned());
        };
        crate::fileio::write_json(&path, self)
            .map_err(|e| format!("写入 {} 失败：{e}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_three_seconds() {
        assert_eq!(Prefs::default().refresh_secs, 3);
        assert_eq!(DEFAULT_REFRESH_SECS, 3);
    }

    #[test]
    fn missing_field_falls_back_to_default() {
        // 手工写了个 `{}` 也不该炸。
        let p: Prefs = serde_json::from_str("{}").unwrap();
        assert_eq!(p.refresh_secs, DEFAULT_REFRESH_SECS);
        assert_eq!(p.theme, ThemePref::Dark);
        // 没有「默认安装目录」这个键 = 没设过，而不是空字符串
        assert_eq!(p.install_dir, None);
        // 没有「终端起始目录」= 用默认档位
        assert_eq!(p.terminal_dir, TerminalStartDir::Home);
    }

    // -- 更新提示 -----------------------------------------------------------

    #[test]
    fn update_fields_default_to_checks_on() {
        let p: Prefs = serde_json::from_str("{}").unwrap();
        // 默认开着：否则"更新提示"这个功能对老用户等于不存在。
        assert!(p.auto_check_update);
        assert_eq!(p.last_update_check, None, "没查过");
        assert_eq!(p.skipped_version, None, "没跳过任何版本");
        assert!(Prefs::default().auto_check_update);
    }

    #[test]
    fn old_prefs_without_update_fields_still_load() {
        // v0.9.x 写的 prefs.json 里没有这三个键 —— 升级上来不能报错。
        let p: Prefs = serde_json::from_str(r#"{"refresh_secs":10}"#).unwrap();
        assert_eq!(p.refresh_secs, 10);
        assert!(p.auto_check_update);
        assert_eq!(p.last_update_check, None);
        assert_eq!(p.skipped_version, None);
    }

    #[test]
    fn update_fields_round_trip() {
        let p = Prefs {
            auto_check_update: false,
            last_update_check: Some(1_792_550_104),
            skipped_version: Some("0.10.0".to_owned()),
            ..Prefs::default()
        };
        let text = serde_json::to_string(&p).unwrap();
        // 落盘形状锁一下：手工编辑 prefs.json 的人看到的就是这个
        assert!(text.contains(r#""auto_check_update":false"#), "{text}");
        assert!(text.contains(r#""skipped_version":"0.10.0""#), "{text}");
        assert_eq!(serde_json::from_str::<Prefs>(&text).unwrap(), p);
    }

    #[test]
    fn normalized_does_not_touch_update_fields() {
        // 同 normalized_does_not_touch_terminal_dir 的理由：顺手"清理"别的字段
        // 是最容易埋进去的 bug（改一次刷新间隔就把"跳过这个版本"抹掉，
        // 用户第二天又会收到同一条提示，而且联想不到是我们干的）。
        let prefs = Prefs {
            refresh_secs: 99999,
            auto_check_update: false,
            last_update_check: Some(42),
            skipped_version: Some("0.10.0".to_owned()),
            ..Prefs::default()
        }
        .normalized();
        assert!(!prefs.auto_check_update);
        assert_eq!(prefs.last_update_check, Some(42));
        assert_eq!(prefs.skipped_version.as_deref(), Some("0.10.0"));
    }

    #[test]
    fn old_prefs_without_theme_upgrade_to_dark() {
        // v0.2 写的 prefs.json 只有 refresh_secs —— 升级后行为必须不变
        let p: Prefs = serde_json::from_str(r#"{"refresh_secs":10}"#).unwrap();
        assert_eq!(p.refresh_secs, 10);
        assert_eq!(p.theme, ThemePref::Dark, "老文件应平滑升级到深色");
        // v0.4 的文件里也没有 install_dir
        assert_eq!(p.install_dir, None);
    }

    #[test]
    fn theme_round_trips_as_lowercase() {
        let p = Prefs {
            refresh_secs: 5,
            theme: ThemePref::Light,
            install_dir: Some(r"D:\wsl".to_owned()),
            ..Prefs::default()
        };
        let text = serde_json::to_string(&p).unwrap();
        assert!(text.contains(r#""theme":"light""#), "{text}");
        assert_eq!(serde_json::from_str::<Prefs>(&text).unwrap(), p);
    }

    #[test]
    fn round_trips_through_json() {
        let p = Prefs {
            refresh_secs: 10,
            theme: ThemePref::Light,
            install_dir: Some(r"E:\wsl".to_owned()),
            ..Prefs::default()
        };
        let text = serde_json::to_string(&p).unwrap();
        let back: Prefs = serde_json::from_str(&text).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn normalized_clamps_out_of_range_values() {
        assert_eq!(
            Prefs {
                refresh_secs: 0,
                ..Prefs::default()
            }
            .normalized()
            .refresh_secs,
            MIN_REFRESH_SECS
        );
        assert_eq!(
            Prefs {
                refresh_secs: 99999,
                ..Prefs::default()
            }
            .normalized()
            .refresh_secs,
            MAX_REFRESH_SECS
        );
        assert_eq!(
            Prefs {
                refresh_secs: 7,
                ..Prefs::default()
            }
            .normalized()
            .refresh_secs,
            7
        );
    }

    #[test]
    fn normalized_does_not_touch_the_theme() {
        let prefs = Prefs {
            refresh_secs: 99999,
            theme: ThemePref::Light,
            install_dir: None,
            ..Prefs::default()
        }
        .normalized();
        assert_eq!(prefs.theme, ThemePref::Light);
    }

    #[test]
    fn normalized_does_not_touch_the_install_dir() {
        // 归一化只管刷新间隔 —— 顺手"清理"别的字段是最容易埋进去的 bug
        // （改一次刷新间隔就把安装目录抹掉，用户根本联想不到）。
        let prefs = Prefs {
            refresh_secs: 99999,
            theme: ThemePref::Dark,
            install_dir: Some(r"D:\wsl".to_owned()),
            ..Prefs::default()
        }
        .normalized();
        assert_eq!(prefs.install_dir.as_deref(), Some(r"D:\wsl"));
    }

    #[test]
    fn normalized_does_not_touch_the_terminal_dir() {
        // 同上：这是"以后再加字段时别忘了它"的守卫 ——
        // 终端起点被一次刷新间隔改动顺手重置，用户完全联想不到。
        let custom = TerminalStartDir::Custom("/home/wt/src".to_owned());
        let prefs = Prefs {
            refresh_secs: 99999,
            terminal_dir: custom.clone(),
            ..Prefs::default()
        }
        .normalized();
        assert_eq!(prefs.terminal_dir, custom);
    }

    // -- 新终端的起始目录 ---------------------------------------------------

    #[test]
    fn terminal_dir_defaults_to_home() {
        assert_eq!(Prefs::default().terminal_dir, TerminalStartDir::Home);
        assert_eq!(TerminalStartDir::default(), TerminalStartDir::Home);
    }

    #[test]
    fn old_prefs_without_terminal_dir_upgrade_to_home() {
        // 0.6.0 及更早的 prefs.json 里没有这个键（那时终端继承面板的 cwd）——
        // 升级后固定落在 `~`。**这是有意的行为改变**，见 `TerminalStartDir`。
        let p: Prefs = serde_json::from_str(r#"{"refresh_secs":10}"#).unwrap();
        assert_eq!(p.terminal_dir, TerminalStartDir::Home);
    }

    #[test]
    fn terminal_dir_round_trips_through_json() {
        for dir in [
            TerminalStartDir::Home,
            TerminalStartDir::Panel,
            TerminalStartDir::Custom("/home/wt/src".to_owned()),
            // 路径里有 `"` 和空格也要活着回来（字符串是嵌进 JSON 的）
            TerminalStartDir::Custom("/opt/my \"stuff\"".to_owned()),
        ] {
            let prefs = Prefs {
                terminal_dir: dir.clone(),
                ..Prefs::default()
            };
            let text = serde_json::to_string(&prefs).unwrap();
            let back: Prefs = serde_json::from_str(&text).unwrap();
            assert_eq!(back.terminal_dir, dir, "{text}");
        }
        // 落盘形状也锁一下：固定档位是裸字符串，自定义是 `{"custom": ...}` ——
        // 手工编辑 `prefs.json` 的人看到的就是这个。
        let home = serde_json::to_string(&Prefs::default()).unwrap();
        assert!(home.contains(r#""terminal_dir":"home""#), "{home}");
        let custom = serde_json::to_string(&Prefs {
            terminal_dir: TerminalStartDir::Custom("/srv".to_owned()),
            ..Prefs::default()
        })
        .unwrap();
        assert!(
            custom.contains(r#""terminal_dir":{"custom":"/srv"}"#),
            "{custom}"
        );
    }

    #[test]
    fn cd_arg_maps_each_variant() {
        assert_eq!(TerminalStartDir::Home.cd_arg(), Some("~"));
        // `None` 就是"不加 --cd" —— 加一个空字符串会被 `wsl.exe` 当成
        // 一个不存在的目录（实测：chdir 失败后落在 `/`）。
        assert_eq!(TerminalStartDir::Panel.cd_arg(), None);
        assert_eq!(
            TerminalStartDir::Custom("/home/wt".to_owned()).cd_arg(),
            Some("/home/wt")
        );
    }

    #[test]
    fn container_workdir_only_passes_absolute_paths() {
        // 容器里没有"Linux home"这个概念：`~` 交给镜像自己的 WORKDIR 决定。
        assert_eq!(TerminalStartDir::Home.container_workdir(), None);
        assert_eq!(TerminalStartDir::Panel.container_workdir(), None);
        assert_eq!(
            TerminalStartDir::Custom("~".to_owned()).container_workdir(),
            None
        );
        assert_eq!(
            TerminalStartDir::Custom("~/src".to_owned()).container_workdir(),
            None
        );
        assert_eq!(
            TerminalStartDir::Custom("/srv/app".to_owned()).container_workdir(),
            Some("/srv/app")
        );
    }

    #[test]
    fn custom_dir_accepts_home_and_absolute_linux_paths() {
        // 前后空白要吃掉：用户从别处复制过来十有八九带一个空格
        assert_eq!(parse_custom_dir("  /home/wt  "), Ok("/home/wt".to_owned()));
        assert_eq!(parse_custom_dir("/"), Ok("/".to_owned()));
        // `~` 与 `~/xxx`：实测 `wsl.exe --cd ~/tmp` 会自己展开成 /home/<用户>/tmp
        assert_eq!(parse_custom_dir("~"), Ok("~".to_owned()));
        assert_eq!(parse_custom_dir("~/src"), Ok("~/src".to_owned()));
    }

    #[test]
    fn custom_dir_rejects_windows_paths_with_a_specific_hint() {
        // 这是**最可能**的输入错误（面板里到处都是 `D:\wsl\...` 这种框），
        // 所以错误信息要专门提一句，而不是只说"要绝对路径"。
        for win in [r"D:\wsl", r"\\nas\share", r"C:\Users\76434"] {
            let err = parse_custom_dir(win).expect_err("Windows 路径该被拒绝");
            assert!(err.contains("Windows 路径"), "{win} → {err}");
        }
    }

    #[test]
    fn custom_dir_rejects_empty_and_relative_paths() {
        for bad in ["", "   ", "home/wt", "./here", ".."] {
            assert!(parse_custom_dir(bad).is_err(), "{bad:?} 该被拒绝");
        }
    }

    #[test]
    fn every_fixed_terminal_dir_has_a_label_and_is_listed() {
        for dir in TerminalStartDir::FIXED {
            assert!(!dir.label().is_empty());
            assert!(!dir.describe().is_empty());
        }
        assert!(TerminalStartDir::FIXED.contains(&TerminalStartDir::default()));
        // 自定义不在固定档位里：它要配一个输入框，是单独一个按钮
        assert!(
            !TerminalStartDir::FIXED
                .iter()
                .any(|d| matches!(d, TerminalStartDir::Custom(_)))
        );
    }

    #[test]
    fn every_theme_has_a_label_and_is_listed() {
        for theme in ThemePref::ALL {
            assert!(!theme.label().is_empty());
        }
        assert!(ThemePref::ALL.contains(&ThemePref::default()));
        // 按钮顺序：先"跟随系统"，再两个固定值
        assert_eq!(ThemePref::ALL[0], ThemePref::System);
    }

    #[test]
    fn theme_resolution_covers_system_and_fixed_values() {
        // 固定值不受系统影响
        assert!(ThemePref::Dark.resolve_is_dark(false));
        assert!(ThemePref::Dark.resolve_is_dark(true));
        assert!(!ThemePref::Light.resolve_is_dark(true));
        assert!(!ThemePref::Light.resolve_is_dark(false));
        // 跟随系统：系统深就深、系统浅就浅
        assert!(ThemePref::System.resolve_is_dark(true));
        assert!(!ThemePref::System.resolve_is_dark(false));
        // `fixed_is_dark` 只对固定值有值 —— UI 靠它决定"要不要去问系统"
        assert_eq!(ThemePref::System.fixed_is_dark(), None);
        assert_eq!(ThemePref::Dark.fixed_is_dark(), Some(true));
        assert_eq!(ThemePref::Light.fixed_is_dark(), Some(false));
    }

    #[test]
    fn the_system_theme_is_persisted_as_lowercase() {
        use super::Prefs;
        let json = serde_json::to_string(&Prefs {
            theme: ThemePref::System,
            ..Prefs::default()
        })
        .expect("应能序列化");
        assert!(json.contains(r#""theme":"system""#), "{json}");
        let back: Prefs = serde_json::from_str(&json).expect("应能反序列化");
        assert_eq!(back.theme, ThemePref::System);
    }

    #[test]
    fn presets_are_within_range_and_include_the_default() {
        for p in REFRESH_PRESETS {
            assert!(*p >= MIN_REFRESH_SECS && *p <= MAX_REFRESH_SECS);
        }
        assert!(REFRESH_PRESETS.contains(&DEFAULT_REFRESH_SECS));
    }

    #[test]
    fn app_dir_points_at_localappdata() {
        if std::env::var_os("LOCALAPPDATA").is_some() {
            let dir = app_dir().expect("应能推导出目录");
            assert!(dir.to_string_lossy().ends_with("wslc-panel"));
            let path = Prefs::path().expect("应能推导出路径");
            assert!(path.to_string_lossy().ends_with("prefs.json"));
        }
    }
}
