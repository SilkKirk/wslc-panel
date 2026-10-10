//! 界面用到的纯函数助手。
//!
//! 放在这个 crate 而不是 `app.rs`：它们是纯逻辑，单测不该为了跑它们
//! 去链接 GPUI（见本 crate 的顶层说明）。

/// 按逗号（中英文）或换行切分，去掉空白项。
///
/// 刻意**不按空格切**：环境变量的值里完全可能有空格
/// （`MESSAGE=hello world`），按空格切会把它切成两条。
pub fn split_list(text: &str) -> Vec<String> {
    text.split([',', '，', '\n', '\r'])
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}

/// 顶部刷新按钮的文案。
///
/// `user_initiated` 为假（3 秒一次的自动刷新）时**永远是「刷新」** ——
/// 自动刷新是后台行为，按钮不该跟着闪。用户点了按钮才是「刷新中…」。
pub fn refresh_label(busy: bool, user_initiated: bool) -> &'static str {
    if busy && user_initiated {
        "刷新中…"
    } else {
        "刷新"
    }
}

#[cfg(test)]
mod tests {
    // 这个 crate 里没有 `gpui_kit`，所以不存在 views.rs / app.rs 里那个
    // "`use super::*` 会把 gpui 的 `test` 属性宏继承进来"的坑。
    use super::{refresh_label, split_list};

    #[test]
    fn split_list_handles_commas_and_newlines() {
        assert_eq!(split_list("8080:80, 9090:90"), vec!["8080:80", "9090:90"]);
        assert_eq!(split_list("a\nb\r\nc"), vec!["a", "b", "c"]);
        // 中文逗号也认 —— 用户从中文文档里复制粘贴很常见
        assert_eq!(split_list("a，b"), vec!["a", "b"]);
    }

    #[test]
    fn split_list_drops_empty_items() {
        assert!(split_list("").is_empty());
        assert!(split_list("  ,  , \n ").is_empty());
        assert_eq!(split_list(" , a , "), vec!["a"]);
    }

    #[test]
    fn split_list_does_not_split_on_spaces() {
        // 环境变量的值里完全可能有空格，按空格切会把它切成两条
        assert_eq!(
            split_list("MESSAGE=hello world"),
            vec!["MESSAGE=hello world"]
        );
    }

    #[test]
    fn auto_refresh_keeps_button_label_unchanged() {
        // 后台自动刷新期间按钮**不能**变成「刷新中…」——
        // 3 秒一次的话按钮就永远停在「刷新中…」上了。
        assert_eq!(refresh_label(true, false), "刷新");
        // 用户点的按钮才显示进度。
        assert_eq!(refresh_label(true, true), "刷新中…");
        // 空闲时当然是「刷新」。
        assert_eq!(refresh_label(false, false), "刷新");
        assert_eq!(refresh_label(false, true), "刷新");
    }
}
