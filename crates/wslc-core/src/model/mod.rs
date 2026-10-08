//! wslc 输出的数据模型。
//!
//! 所有字段都带 `#[serde(default)]`：`wslc` 的 JSON 字段类型不稳定
//! （数字可能是字符串、可空字段可能整个缺失），**解析失败绝不能 panic**。
//!
//! 字段字典见 `docs/wslc-schema.md`。

mod container;
mod image;
mod network;
// `pub` 是因为表格解析函数 `parse_session_table` 需要被 `cmd::system` 复用。
pub mod session;
mod system;
mod volume;

pub use container::{
    parse_wsl_metadata, ContainerInspect, ContainerListItem, ContainerState, ContainerStats,
    ContainerSummary, Platform, PortMapping, WslPortSpec,
};
pub use image::ImageListItem;
pub use network::NetworkListItem;
pub use session::{parse_session_table, Session};
pub use system::{ClientInfo, ServerInfo, SessionInfo, SystemInfo};
pub use volume::VolumeListItem;

/// 把 `"8.42MB"` 这类人类可读的体积解析成字节数。
///
/// 采用 **1000 进制**（与 `docker`/`wslc` 的显示一致：`kB`/`MB`/`GB`），
/// 但接受 `KiB`/`MiB`/`GiB` 的 1024 进制写法（`wslc stats` 用的是后者）。
/// 无法解析或 `N/A`、`<none>` 时返回 `None`。
pub fn parse_size(input: &str) -> Option<f64> {
    let s = input.trim();
    if s.is_empty() || s == "N/A" || s == "<none>" || s == "0B" {
        return if s == "0B" { Some(0.0) } else { None };
    }

    let split_at = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+'))
        .unwrap_or(s.len());
    let (num, unit) = s.split_at(split_at);
    let value: f64 = num.trim().parse().ok()?;
    let unit = unit.trim();

    let multiplier = match unit {
        "" | "B" => 1.0,
        "kB" | "KB" => 1_000.0,
        "MB" => 1_000_000.0,
        "GB" => 1_000_000_000.0,
        "TB" => 1_000_000_000_000.0,
        "KiB" => 1024.0,
        "MiB" => 1024.0 * 1024.0,
        "GiB" => 1024.0 * 1024.0 * 1024.0,
        "TiB" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some(value * multiplier)
}

/// 把 `"3.465MiB / 15.48GiB"` 这类 `A / B` 组合拆成 `(A, B)`。
///
/// 拆不出两段时返回 `None`，调用方自行兜底显示原文。
pub fn split_pair(input: &str) -> Option<(&str, &str)> {
    let (a, b) = input.split_once('/')?;
    let a = a.trim();
    let b = b.trim();
    if a.is_empty() || b.is_empty() {
        return None;
    }
    Some((a, b))
}

/// 把 `"0.02%"` 解析成 `0.02`。
pub fn parse_percent(input: &str) -> Option<f64> {
    input.trim().trim_end_matches('%').trim().parse().ok()
}

/// 按 `sep` 切分，但**忽略括号内部**的分隔符。
///
/// `wslc` 的 `Labels` 字段里嵌套着 JSON，例如：
/// `com.microsoft.wsl.container.metadata={"V1":{...,"Ports":[{...},{...}],...}}`
/// —— 直接用 `split(',')` 会把它切碎。
pub fn split_outside_brackets(input: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth: i32 = 0;
    let mut start = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for (idx, ch) in input.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' | '[' | '(' => depth += 1,
            '}' | ']' | ')' => depth -= 1,
            c if c == sep && depth <= 0 => {
                parts.push(&input[start..idx]);
                start = idx + ch.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&input[start..]);
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 浮点体积只做相对误差比较：`8.42 * 1_000_000.0` 这类表达式
    /// 在不同结合顺序下可能差最后一个 ulp。
    fn assert_size(actual: Option<f64>, expected: f64) {
        let actual = actual.expect("应能解析出体积");
        let tolerance = expected.abs() * 1e-9;
        assert!(
            (actual - expected).abs() <= tolerance,
            "期望约 {expected}，实际 {actual}"
        );
    }

    #[test]
    fn parses_decimal_sizes() {
        assert_size(parse_size("8.42MB"), 8.42 * 1_000_000.0);
        assert_size(parse_size("10.1kB"), 10.1 * 1_000.0);
        assert_size(parse_size("1GB"), 1_000_000_000.0);
        assert_eq!(parse_size("0B"), Some(0.0));
    }

    #[test]
    fn parses_binary_sizes_from_stats() {
        assert_size(parse_size("3.465MiB"), 3.465 * 1024.0 * 1024.0);
        assert_size(parse_size("15.48GiB"), 15.48 * 1024.0 * 1024.0 * 1024.0);
    }

    #[test]
    fn rejects_unparseable_sizes() {
        assert_eq!(parse_size("N/A"), None);
        assert_eq!(parse_size("<none>"), None);
        assert_eq!(parse_size(""), None);
        assert_eq!(parse_size("??"), None);
    }

    #[test]
    fn splits_pairs() {
        assert_eq!(
            split_pair("3.465MiB / 15.48GiB"),
            Some(("3.465MiB", "15.48GiB"))
        );
        assert_eq!(split_pair("1.04kB / 0B"), Some(("1.04kB", "0B")));
        assert_eq!(split_pair("0B / 0B"), Some(("0B", "0B")));
        assert_eq!(split_pair("nope"), None);
    }

    #[test]
    fn parses_percents() {
        assert_eq!(parse_percent("0.00%"), Some(0.0));
        assert_eq!(parse_percent("12.5%"), Some(12.5));
        assert_eq!(parse_percent("x"), None);
    }

    #[test]
    fn split_ignores_separators_inside_json() {
        let labels = concat!(
            r#"com.microsoft.wsl.container.metadata={"V1":{"Ports":[{"HostPort":18080,"#,
            r#""ContainerPort":80}]}},"#,
            r#"other=1"#
        );
        let parts = split_outside_brackets(labels, ',');
        assert_eq!(
            parts.len(),
            2,
            "嵌入的 JSON 里的逗号不应被当作分隔符：{parts:?}"
        );
        assert!(parts[0].starts_with("com.microsoft.wsl.container.metadata="));
        assert_eq!(parts[1], "other=1");
    }

    #[test]
    fn split_handles_escaped_quotes_in_strings() {
        let labels = r#"a={\"x\":\"1,2\"},b=3"#;
        // 此处引号被转义，字符串状态机应把它当成普通字符流处理而不崩。
        let parts = split_outside_brackets(labels, ',');
        assert!(!parts.is_empty());
    }

    #[test]
    fn split_of_empty_input_yields_one_empty_part() {
        assert_eq!(split_outside_brackets("", ','), vec![""]);
    }
}
