//! 配置项的常用预设值。
//!
//! 从 `views.rs` 搬过来的：`presets_for` 只做一次 `match`，返回静态字符串表，
//! 但它旁边那几个断言（枚举项不给预设、数值项有预设）是纯逻辑，
//! 没必要为了跑它们去链接 GPUI（见本 crate 的顶层说明）。

use wslc_core::settings::{SETTING_KEYS, SettingKey, SettingKind};

/// 各配置项的常用预设值。
pub fn presets_for(key: &SettingKey) -> &'static [&'static str] {
    match (key.section, key.key) {
        (Some("session"), "cpuCount") => &["4", "8", "16"],
        (Some("session"), "memorySize") => &["2GB", "4GB", "8GB"],
        (Some("session"), "maxStorageSize") => &["100GB", "500GB", "1TB"],
        (Some("session"), "idleTimeout") => &["30", "60", "300"],
        (Some("session"), "hostLoopback") => &["host.wslc.internal", "none"],
        (Some("session"), "defaultBindingAddress") => &["127.0.0.1", "0.0.0.0"],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::presets_for;
    use wslc_core::settings::{SETTING_KEYS, SettingKind};

    #[test]
    fn presets_are_non_empty_for_every_key() {
        for key in SETTING_KEYS {
            for preset in presets_for(key) {
                assert!(!preset.is_empty(), "{} 的预设值不能为空", key.key);
            }
        }
    }

    #[test]
    fn enum_setting_uses_choices_instead_of_presets() {
        let cred = SETTING_KEYS
            .iter()
            .find(|k| k.key == "credentialStore")
            .expect("应存在 credentialStore");
        assert_eq!(cred.kind, SettingKind::Enum);
        assert!(presets_for(cred).is_empty());

        // 注意：不要写成 `assert_eq!(cred.choices, &["wincred", "file"])`。
        // `&[&str]` 与 `&[&str; N]` 的比较会让编译器展开出一大堆引用/去 Sized 强制转换，
        // 在 `#[test]` 里表现为 "recursion limit reached while expanding #[test]"。
        // 统一转成 `Vec` 再比，类型简单、诊断也清楚。
        let choices: Vec<&str> = cred.choices.to_vec();
        assert_eq!(choices, vec!["wincred", "file"]);
    }

    #[test]
    fn numeric_settings_offer_presets() {
        let cpu = SETTING_KEYS.iter().find(|k| k.key == "cpuCount").unwrap();
        let cpu_presets: Vec<&str> = presets_for(cpu).to_vec();
        assert_eq!(cpu_presets, vec!["4", "8", "16"]);

        let idle = SETTING_KEYS
            .iter()
            .find(|k| k.key == "idleTimeout")
            .unwrap();
        let idle_presets: Vec<&str> = presets_for(idle).to_vec();
        assert_eq!(idle_presets, vec!["30", "60", "300"]);
    }
}
