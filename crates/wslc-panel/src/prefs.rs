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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemePref {
    /// 深色（默认）。
    #[default]
    Dark,
    /// 浅色。
    Light,
}

impl ThemePref {
    /// 全部可选值（决定界面上的按钮顺序）。
    pub const ALL: [ThemePref; 2] = [ThemePref::Dark, ThemePref::Light];

    /// 界面标签。
    pub fn label(self) -> &'static str {
        match self {
            Self::Dark => "深色",
            Self::Light => "浅色",
        }
    }
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
}

fn default_refresh_secs() -> u64 {
    DEFAULT_REFRESH_SECS
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            refresh_secs: DEFAULT_REFRESH_SECS,
            theme: ThemePref::default(),
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
    /// 一个界面偏好不值得因为解析失败就拦住启动。
    pub fn load() -> Self {
        let Some(path) = Self::path() else {
            return Self::default();
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        match serde_json::from_str::<Prefs>(&text) {
            Ok(prefs) => prefs.normalized(),
            Err(e) => {
                tracing::warn!("{} 解析失败，使用默认偏好：{e}", path.display());
                Self::default()
            }
        }
    }

    /// 写回磁盘；失败只记录日志，不影响界面。
    pub fn save(&self) -> Result<(), String> {
        let Some(path) = Self::path() else {
            return Err("LOCALAPPDATA 未设置，无法确定偏好文件位置".to_owned());
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("创建 {} 失败：{e}", dir.display()))?;
        }
        let text =
            serde_json::to_string_pretty(self).map_err(|e| format!("序列化偏好失败：{e}"))?;
        std::fs::write(&path, text).map_err(|e| format!("写入 {} 失败：{e}", path.display()))
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
    }

    #[test]
    fn old_prefs_without_theme_upgrade_to_dark() {
        // v0.2 写的 prefs.json 只有 refresh_secs —— 升级后行为必须不变
        let p: Prefs = serde_json::from_str(r#"{"refresh_secs":10}"#).unwrap();
        assert_eq!(p.refresh_secs, 10);
        assert_eq!(p.theme, ThemePref::Dark, "老文件应平滑升级到深色");
    }

    #[test]
    fn theme_round_trips_as_lowercase() {
        let p = Prefs {
            refresh_secs: 5,
            theme: ThemePref::Light,
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
        }
        .normalized();
        assert_eq!(prefs.theme, ThemePref::Light);
    }

    #[test]
    fn every_theme_has_a_label_and_is_listed() {
        for theme in ThemePref::ALL {
            assert!(!theme.label().is_empty());
        }
        assert!(ThemePref::ALL.contains(&ThemePref::default()));
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
