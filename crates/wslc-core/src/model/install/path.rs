//! Windows 绝对路径的判断与默认安装目录的推导。

/// 是不是 Windows 绝对路径：`D:\...`、`D:/...` 或 UNC `\\server\share`。
///
/// 刻意**不用** `Path::is_absolute()`：它在非 Windows 上对 `D:\wsl` 返回
/// `false`，而这条校验的结论不该随编译平台变（CI 与开发机要一致）。
pub fn is_absolute_windows_path(text: &str) -> bool {
    let bytes = text.as_bytes();
    // 盘符形式：`D:\` / `D:/`
    if bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return matches!(bytes[2], b'\\' | b'/');
    }
    // UNC 形式：`\\server\share`
    text.starts_with("\\\\")
}

/// 由"默认安装目录 + 发行版名"推出安装目录。
///
/// 分隔符**跟着默认目录自己**的写法走（`D:/wsl` 就继续用 `/`）：
/// 混着写虽然 Windows 认，但用户复制到终端、脚本里就会看着别扭。
/// 默认目录为空或名字为空时给 `None` —— 调用方据此要求用户自己填。
pub fn derive_install_dir(default_dir: Option<&str>, name: &str) -> Option<String> {
    let base = default_dir?.trim();
    let name = name.trim();
    if base.is_empty() || name.is_empty() {
        return None;
    }
    let sep = if base.contains('/') && !base.contains('\\') {
        '/'
    } else {
        '\\'
    };
    let base = base.trim_end_matches(['\\', '/']);
    Some(format!("{base}{sep}{name}"))
}
