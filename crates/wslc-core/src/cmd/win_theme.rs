//! Windows 的**应用主题**（浅色 / 深色）—— 给「跟随系统」用。
//!
//! # 为什么读注册表，而不是问 GPUI
//!
//! 看起来 `Window::appearance()` 更"正统"，但那个 API 在本仓库里没有先例，
//! 名字和返回值只能靠猜；而注册表这一处是**确定的**（msdn 与实测都对得上）：
//!
//! ```text
//! HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize
//!     AppsUseLightTheme    REG_DWORD    0x1   ← 1 = 应用用浅色，0 = 用深色
//! ```
//!
//! ⚠️ 注意有两个键，别读错：`SystemUsesLightTheme` 管的是**任务栏/开始菜单**，
//! `AppsUseLightTheme` 才是**应用窗口**用哪套配色 —— 两者可以不一样
//! （用户完全可能任务栏深色、应用浅色）。
//!
//! 走 `reg.exe` 而不是直接读 API：本仓库已经用 `reg.exe query` 读 Lxss 的
//! `BasePath`（见 `model/distro.rs`），这条路实测 **7~27 ms**、输出恒为 UTF-8
//! （哪怕系统 ACP=936），不需要再加依赖、也不需要 unsafe 的注册表 API。
//!
//! 读不到（键不存在 / reg.exe 被拦）时返回 `None` —— 调用方决定退到哪边，
//! 这里**不猜**。

use std::time::Duration;

/// 读一次系统主题：`Some(true)` = 应用该用深色。
///
/// **阻塞**（起 `reg.exe`，实测 7~27 ms）—— 界面线程上别直接调，
/// 放 `cx.background_executor()`（见 `AGENTS.md` §7.1）。
pub fn system_is_dark() -> Option<bool> {
    let out = crate::cli::run_helper_with_hint(
        "reg.exe",
        "reg.exe query",
        "读不到系统主题（reg.exe 不可用？）—— 请在设置里手动选深色或浅色",
        &[
            "query",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
            "/v",
            "AppsUseLightTheme",
        ],
        Duration::from_secs(5),
    )
    .ok()?;

    parse_apps_use_light_theme(&out.stdout)
}

/// 从 `reg.exe query` 的输出里读 `AppsUseLightTheme`。
///
/// 真实输出长这样（实测）：
///
/// ```text
/// HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize
///     AppsUseLightTheme    REG_DWORD    0x1
/// ```
///
/// 拿不到那个值时返回 `None`（键被删过、输出格式变了都算）。
pub fn parse_apps_use_light_theme(text: &str) -> Option<bool> {
    for line in text.lines() {
        let line = line.trim();
        if !line.starts_with("AppsUseLightTheme") {
            continue;
        }
        // 形如 `AppsUseLightTheme    REG_DWORD    0x1`；也接受末尾是十进制 `1`
        let value = line.split_whitespace().last()?;
        let value = value.trim_start_matches("0x").trim_start_matches("0X");
        let light = u32::from_str_radix(value, 16).ok()?;
        // 只有 0 / 非 0 两种语义，别的值（理论上不该有）当"浅色"
        return Some(light == 0);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::parse_apps_use_light_theme;

    #[test]
    fn reads_the_real_reg_output() {
        // 实测抓下来的原文（深色系统）
        let dark = "HKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize\r\n    AppsUseLightTheme    REG_DWORD    0x0\r\n\r\n";
        assert_eq!(parse_apps_use_light_theme(dark), Some(true));

        let light = "HKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize\r\n    AppsUseLightTheme    REG_DWORD    0x1\r\n";
        assert_eq!(parse_apps_use_light_theme(light), Some(false));
    }

    #[test]
    fn does_not_confuse_the_taskbar_key() {
        // `SystemUsesLightTheme` 管任务栏，和"应用用哪套配色"是两码事
        let text = "    SystemUsesLightTheme    REG_DWORD    0x0\r\n    AppsUseLightTheme    REG_DWORD    0x1\r\n";
        assert_eq!(parse_apps_use_light_theme(text), Some(false));
    }

    #[test]
    fn missing_or_broken_key_is_none_not_a_guess() {
        assert_eq!(parse_apps_use_light_theme(""), None);
        assert_eq!(
            parse_apps_use_light_theme("错误: 系统找不到指定的注册表项或值。"),
            None
        );
        // 值不是数字 → 也不猜
        assert_eq!(
            parse_apps_use_light_theme("    AppsUseLightTheme    REG_SZ    yes\r\n"),
            None
        );
    }
}
