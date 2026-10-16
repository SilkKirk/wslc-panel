//! 数据模型。
//!
//! 两个域：
//!
//! - **`wslc`（容器）** —— 来自 `--format json`，所有字段都带
//!   `#[serde(default)]`：字段类型不稳定（数字可能是字符串、可空字段可能整个缺失），
//!   **解析失败绝不能 panic**。字段字典见 `docs/wslc-schema.md`。
//! - **`wsl`（发行版）** —— 来自给人看的**表格**（没有 JSON 输出），
//!   解析规则见 [`distro`] 的模块文档。

mod container;
mod image;
mod network;
// 「部署应用」的纯逻辑：nginx / Java / Node.js / Go 的识别、脚本生成、输出解析。
// `pub` 的理由和 `mysql` 一样：`container_app_kind` 在**渲染期**就要用
// （见 `views::container_row_actions`），面板也直接装它的类型。
pub mod deploy;
// MySQL 容器的识别 / 命令行拼装 / 输出解析 / SQL 生成。
// `pub` 是因为面板要直接用它的类型（`MysqlCreds` / `DbEntry` ……）和函数
// （`mysql_container_kind` 在**渲染期**就要用，见 `views::container_row_actions`）。
pub mod mysql;
// `pub` 是因为表格解析函数 `parse_session_table` 需要被 `cmd::system` 复用。
pub mod session;
mod system;
mod volume;

// `pub` 是因为解析函数（`parse_distro_list` / `parse_registry`）
// 需要被 `cmd::distro` 复用，而且它们**不依赖 Windows**，可以独立跑单测。
pub mod distro;

// `/etc/wsl.conf` 的保序读写模型。纯逻辑（不碰文件系统、不跑 WSL），
// 所以能脱离 Windows 单测 —— 解析和"改哪几行"的逻辑都在这里。
pub mod wslconf;

// 「添加实例」的纯逻辑：名称/路径推导、装前检查、**执行计划**。
// 同样不碰文件系统、不起进程，见模块自己的说明。
pub mod install;

pub use container::{
    parse_wsl_metadata, suggest_container_name, ContainerInspect, ContainerListItem,
    ContainerMount, ContainerState, ContainerStats, ContainerSummary, Platform, PortMapping,
    WslPortSpec,
};
// 「部署应用」那一族的类型，和 mysql 一样在这里平铺出来
// （函数仍走 `model::deploy::`，一看就知道来自哪）。
pub use deploy::{
    container_app_kind, AppRuntime, DeployCandidate, DeployErrorKind, DeployKind, DeploySignal,
    PortMap, ProbeResult, ProcessStatus, SourceKind,
};
pub use distro::{parse_distro_list, Distro, DistroState, WslStatus};
pub use image::{
    image_preset, local_references, parse_image_inspect, prefilled_env_left_empty,
    suggest_port_mappings, ImageFacts, ImageListItem, ImagePreset,
};
pub use install::{
    plan, preflight, InstallPlan, InstallSource, InstallSpec, PlanContext, PlanProgram,
    PlannedStep, Preflight, WSL_VERSION,
};
pub use network::NetworkListItem;
// MySQL 这一族的类型：面板的 `DbState`（在 `wslc-panel-core` 里）直接装它们，
// 所以常用的那些在这里平铺出来（函数仍走 `model::mysql::`，一看就知道来自哪）。
pub use mysql::{
    ClientProbe, DbEntry, DbUser, GrantKind, MysqlCandidate, MysqlCreds, MysqlErrorKind,
    MysqlFamily, MysqlSignal, RootCredHint, SqlMode,
};
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

/// 取镜像引用的**最后一段**（去掉 registry 前缀、tag、digest），小写。
///
/// 例如 `docker.1panel.live/library/nginx:latest` → `nginx`。
///
/// ⚠️ 刻意**不用** `registry::split_input`：那个函数在遇到
/// `alpine@sha256:…` 这类**合法**引用时会返回 `Err`（它服务的是"拉取"那条路，
/// 按摘要拉取这一版不支持）。而这里的调用点全在**渲染路径**上 ——
/// 判定失败必须是"认不出来"，不能是"报错"。
///
/// 放在 `mod.rs` 而不是某个子模块里：`mysql`（认 MySQL 家族）和 `deploy`
/// （认 nginx / Java / Node / Go）都要它，两个模块各存一份必然会漂移。
pub fn image_last_segment(image: &str) -> String {
    let without_digest = image.split('@').next().unwrap_or(image);
    let last = without_digest.rsplit('/').next().unwrap_or(without_digest);
    // ⚠️ 只切**最后一段**里的冒号：`localhost:5000/mysql` 的端口在主机名那段，
    // 先 rsplit('/') 之后已经不在 last 里了。
    let last = last.split(':').next().unwrap_or(last);
    last.trim().to_ascii_lowercase()
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
