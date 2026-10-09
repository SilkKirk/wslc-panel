//! 容器命令：`list` / `stats` / `inspect` / 生命周期 / `logs` / `exec` / `run`。

use serde_json::Value;

use crate::cli::Wslc;
use crate::error::{Error, Result};
use crate::jsonl;
use crate::model::{ContainerInspect, ContainerListItem, ContainerStats, ContainerSummary};

/// 列出容器。
///
/// `all = false` 时等价于 `wslc list`（**只列运行中**），
/// `all = true` 时等价于 `wslc list -a`。
///
/// 空结果时 `wslc` 输出 0 字节，这里返回空 `Vec` 而不是错误。
pub fn list(wslc: &Wslc, all: bool) -> Result<Vec<ContainerListItem>> {
    let out = if all {
        wslc.run_checked(&["list", "-a", "--format", "json"])?
    } else {
        wslc.run_checked(&["list", "--format", "json"])?
    };
    jsonl::parse_lines(&out.stdout)
}

/// 只列出正在运行的容器。
pub fn list_running(wslc: &Wslc) -> Result<Vec<ContainerListItem>> {
    list(wslc, false)
}

/// 实时统计（`wslc stats --format json`）。
///
/// `all = true` 时会带上已停止的容器（它们的各项指标为 0）。
pub fn stats(wslc: &Wslc, all: bool) -> Result<Vec<ContainerStats>> {
    let out = if all {
        wslc.run_checked(&["stats", "-a", "--format", "json"])?
    } else {
        wslc.run_checked(&["stats", "--format", "json"])?
    };
    jsonl::parse_lines(&out.stdout)
}

/// 运行中容器的列表 + 统计合并视图（"当前运行 container" 页直接消费）。
pub fn running_summaries(wslc: &Wslc) -> Result<Vec<ContainerSummary>> {
    let items = list_running(wslc)?;
    let stats = stats(wslc, false)?;
    Ok(ContainerSummary::merge(items, stats))
}

/// 全部容器的列表 + 统计合并视图。
pub fn all_summaries(wslc: &Wslc) -> Result<Vec<ContainerSummary>> {
    let items = list(wslc, true)?;
    let stats = stats(wslc, true)?;
    Ok(ContainerSummary::merge(items, stats))
}

/// `wslc inspect <id>`。
///
/// `size = true` 时加 `-s`，会额外返回 `SizeRootFs` / `SizeRw`。
pub fn inspect(wslc: &Wslc, container: &str, size: bool) -> Result<ContainerInspect> {
    let out = if size {
        wslc.run_checked(&["inspect", "-s", container])?
    } else {
        wslc.run_checked(&["inspect", container])?
    };

    let values: Vec<Value> = jsonl::parse_array_or_lines(&out.stdout)?;
    ContainerInspect::from_array(values).ok_or_else(|| {
        Error::Parse(format!(
            "wslc inspect {container} 返回了空数组，找不到该容器"
        ))
    })
}

/// 把一个或多个容器停掉，返回被处理的容器名。
pub fn stop(wslc: &Wslc, containers: &[String]) -> Result<Vec<String>> {
    run_names(wslc, "stop", containers, &[])
}

/// 强制终止（`kill`）。
pub fn kill(wslc: &Wslc, containers: &[String]) -> Result<Vec<String>> {
    run_names(wslc, "kill", containers, &[])
}

/// 启动。
pub fn start(wslc: &Wslc, containers: &[String]) -> Result<Vec<String>> {
    run_names(wslc, "start", containers, &[])
}

/// 重启。
pub fn restart(wslc: &Wslc, containers: &[String]) -> Result<Vec<String>> {
    run_names(wslc, "restart", containers, &[])
}

/// 删除容器。
///
/// `force = true` 时加 `-f`，可以删除正在运行的容器。
pub fn remove(wslc: &Wslc, containers: &[String], force: bool) -> Result<Vec<String>> {
    let extra: Vec<&str> = if force { vec!["-f"] } else { vec![] };
    run_names(wslc, "remove", containers, &extra)
}

/// 清理所有已停止的容器。
///
/// 使用 `-f`（`--force`）**跳过确认提示**：子进程的 stdin 是 null，
/// 若 `wslc` 试图读取确认输入会立刻得到 EOF，行为不可预期。
pub fn prune(wslc: &Wslc, filter: Option<&str>) -> Result<String> {
    let mut args: Vec<String> = vec!["container".into(), "prune".into(), "-f".into()];
    if let Some(f) = filter {
        args.push("--filter".into());
        args.push(f.to_owned());
    }
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = wslc.run_checked(&refs)?;
    Ok(out.stdout_trimmed().to_owned())
}

/// `logs` 的选项。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogOptions {
    /// 只显示末尾 N 行。
    pub tail: Option<u32>,
    /// 显示时间戳。
    pub timestamps: bool,
    /// 显示额外细节。
    pub details: bool,
}

/// 读取容器日志。
///
/// ⚠️ **不支持 `--follow`**：跟随模式永不返回，会挂住调用方。
/// 需要跟随时请用 [`Wslc::spawn_in_new_console`] 开独立终端窗口。
pub fn logs(wslc: &Wslc, container: &str, options: &LogOptions) -> Result<String> {
    let mut args: Vec<String> = vec!["logs".into()];
    if let Some(tail) = options.tail {
        args.push("--tail".into());
        args.push(tail.to_string());
    }
    if options.timestamps {
        args.push("--timestamps".into());
    }
    if options.details {
        args.push("--details".into());
    }
    args.push(container.to_owned());

    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = wslc.run_checked(&refs)?;
    // `wslc logs` 把容器 stdout/stderr 混在一起，分别解码后拼回。
    let mut combined = out.stdout;
    if !out.stderr.trim().is_empty() {
        if !combined.is_empty() && !combined.ends_with('\n') {
            combined.push('\n');
        }
        combined.push_str(&out.stderr);
    }
    Ok(combined)
}

/// 在运行中的容器里执行命令（**非交互**）。
///
/// 交互式场景（`-it`）请用 [`open_interactive_shell`]。
pub fn exec(wslc: &Wslc, container: &str, command: &[String]) -> Result<String> {
    let mut args: Vec<String> = vec!["exec".into(), container.to_owned()];
    args.extend(command.iter().cloned());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = wslc.run_checked(&refs)?;
    Ok(out.stdout)
}

/// 在新控制台窗口里打开交互式 shell。
///
/// 交互式程序不能内嵌进 GPUI，也不该被捕获输出。
pub fn open_interactive_shell(wslc: &Wslc, container: &str, shell: &str) -> Result<()> {
    wslc.spawn_in_new_console(&["exec", "-i", "-t", container, shell])
}

/// 在新控制台窗口里 `attach` 到容器主进程。
pub fn attach_in_new_console(wslc: &Wslc, container: &str) -> Result<()> {
    wslc.spawn_in_new_console(&["attach", container])
}

/// 镜像拉取策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PullPolicy {
    /// 总是拉取。
    Always,
    /// 本地没有才拉取。
    #[default]
    Missing,
    /// 从不拉取（**离线/内网环境必须用这个**）。
    Never,
}

impl PullPolicy {
    /// `wslc --pull` 接受的字符串。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Always => "always",
            Self::Missing => "missing",
            Self::Never => "never",
        }
    }
}

/// 创建 / 运行容器的描述。
///
/// 这个结构同时承担两个职责：
/// 1. 生成 `wslc run` 的参数（[`RunSpec::to_args`]）；
/// 2. 在界面上展示"等效命令"预览。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunSpec {
    /// 镜像引用（必填）。
    pub image: String,
    /// 容器名。
    pub name: Option<String>,
    /// 容器内执行的命令。
    pub command: Vec<String>,
    /// 后台运行（`-d`）。
    pub detach: bool,
    /// 端口映射，如 `18080:80`。
    pub ports: Vec<String>,
    /// 环境变量。
    pub env: Vec<(String, String)>,
    /// 卷挂载，如 `data:/var/lib/data`。
    pub volumes: Vec<String>,
    /// 网络。
    pub network: Option<String>,
    /// 内存上限，如 `512M`。
    pub memory: Option<String>,
    /// CPU 数，如 `0.5`。
    pub cpus: Option<String>,
    /// 停止后自动删除（`--rm`）。
    pub rm: bool,
    /// 拉取策略。
    pub pull: PullPolicy,
    /// 工作目录。
    pub workdir: Option<String>,
    /// 运行用户。
    pub user: Option<String>,
    /// 主机名。
    pub hostname: Option<String>,
}

impl RunSpec {
    /// 用镜像创建一个最小描述。
    pub fn new(image: impl Into<String>) -> Self {
        Self {
            image: image.into(),
            ..Default::default()
        }
    }

    /// 生成 `wslc run` 的参数列表（**不含程序名，也不含 `--session`**）。
    pub fn to_args(&self) -> Vec<String> {
        let mut args: Vec<String> = vec!["run".into()];

        if self.detach {
            args.push("-d".into());
        }
        if self.rm {
            args.push("--rm".into());
        }
        // 始终显式指定拉取策略：默认 missing，避免离线环境下卡在拉取。
        args.push("--pull".into());
        args.push(self.pull.as_str().into());

        if let Some(name) = &self.name {
            args.push("--name".into());
            args.push(name.clone());
        }
        if let Some(network) = &self.network {
            args.push("--network".into());
            args.push(network.clone());
        }
        if let Some(memory) = &self.memory {
            args.push("--memory".into());
            args.push(memory.clone());
        }
        if let Some(cpus) = &self.cpus {
            args.push("--cpus".into());
            args.push(cpus.clone());
        }
        if let Some(workdir) = &self.workdir {
            args.push("--workdir".into());
            args.push(workdir.clone());
        }
        if let Some(user) = &self.user {
            args.push("--user".into());
            args.push(user.clone());
        }
        if let Some(hostname) = &self.hostname {
            args.push("--hostname".into());
            args.push(hostname.clone());
        }
        for port in &self.ports {
            args.push("-p".into());
            args.push(port.clone());
        }
        for (k, v) in &self.env {
            args.push("-e".into());
            args.push(format!("{k}={v}"));
        }
        for volume in &self.volumes {
            args.push("-v".into());
            args.push(volume.clone());
        }

        args.push(self.image.clone());
        args.extend(self.command.iter().cloned());
        args
    }

    /// 面向用户的等效命令预览（带引号，可直接粘进终端）。
    pub fn preview(&self) -> String {
        let mut parts = vec!["wslc".to_owned()];
        for arg in self.to_args() {
            if arg.is_empty() || arg.contains(' ') || arg.contains('"') {
                parts.push(format!("\"{}\"", arg.replace('"', "\\\"")));
            } else {
                parts.push(arg);
            }
        }
        parts.join(" ")
    }

    /// 基本校验。
    pub fn validate(&self) -> Result<()> {
        if self.image.trim().is_empty() {
            return Err(Error::InvalidArgument("必须指定镜像".into()));
        }
        if let Some(name) = &self.name {
            if name.trim().is_empty() {
                return Err(Error::InvalidArgument("容器名不能为空白".into()));
            }
        }
        Ok(())
    }
}

/// 运行容器，返回 `wslc` 打印的容器 ID。
pub fn run(wslc: &Wslc, spec: &RunSpec) -> Result<String> {
    spec.validate()?;
    let args = spec.to_args();
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    // `run` 在 `--pull missing` 下可能触发网络拉取，给更长的超时。
    let out = wslc
        .run_with_timeout(&refs, std::time::Duration::from_secs(300))?
        .into_result()?;
    Ok(out.stdout_trimmed().to_owned())
}

/// 创建容器但不启动。
pub fn create(wslc: &Wslc, spec: &RunSpec) -> Result<String> {
    spec.validate()?;
    let mut args = spec.to_args();
    // `create` 与 `run` 的选项一致，只是第一个词不同。
    args[0] = "create".into();
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = wslc
        .run_with_timeout(&refs, std::time::Duration::from_secs(300))?
        .into_result()?;
    Ok(out.stdout_trimmed().to_owned())
}

/// 执行 `wslc <verb> [extra...] <container...>` 并解析它打印出来的对象名。
fn run_names(
    wslc: &Wslc,
    verb: &str,
    containers: &[String],
    extra: &[&str],
) -> Result<Vec<String>> {
    if containers.is_empty() {
        return Err(Error::InvalidArgument(format!("{} 至少需要一个容器", verb)));
    }
    let mut args: Vec<String> = vec![verb.to_owned()];
    args.extend(extra.iter().map(|s| (*s).to_owned()));
    args.extend(containers.iter().cloned());

    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = wslc.run_checked(&refs)?;
    Ok(out
        .stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_spec_generates_expected_args() {
        let spec = RunSpec {
            image: "alpine:latest".into(),
            name: Some("probe".into()),
            command: vec!["sleep".into(), "300".into()],
            detach: true,
            ports: vec!["18080:80".into()],
            env: vec![("PROBE".into(), "1".into())],
            volumes: vec!["data:/data".into()],
            network: Some("bridge".into()),
            memory: Some("512M".into()),
            cpus: Some("0.5".into()),
            pull: PullPolicy::Never,
            ..Default::default()
        };

        assert_eq!(
            spec.to_args(),
            vec![
                "run",
                "-d",
                "--pull",
                "never",
                "--name",
                "probe",
                "--network",
                "bridge",
                "--memory",
                "512M",
                "--cpus",
                "0.5",
                "-p",
                "18080:80",
                "-e",
                "PROBE=1",
                "-v",
                "data:/data",
                "alpine:latest",
                "sleep",
                "300",
            ]
        );
    }

    #[test]
    fn run_spec_defaults_to_pull_missing() {
        let spec = RunSpec::new("alpine");
        let args = spec.to_args();
        assert_eq!(args, vec!["run", "--pull", "missing", "alpine"]);
    }

    #[test]
    fn run_spec_preview_is_shell_pasteable() {
        let mut spec = RunSpec::new("docker.1ms.run/library/alpine:latest");
        spec.detach = true;
        spec.name = Some("wslc-panel-probe".into());
        spec.pull = PullPolicy::Never;
        spec.command = vec!["sleep".into(), "300".into()];
        assert_eq!(
            spec.preview(),
            "wslc run -d --pull never --name wslc-panel-probe docker.1ms.run/library/alpine:latest sleep 300"
        );
    }

    #[test]
    fn run_spec_preview_quotes_arguments_with_spaces() {
        let mut spec = RunSpec::new("alpine");
        spec.command = vec!["sh".into(), "-c".into(), "echo hello world".into()];
        assert!(spec.preview().ends_with(r#"sh -c "echo hello world""#));
    }

    #[test]
    fn run_spec_validation_rejects_empty_image() {
        let spec = RunSpec::default();
        assert!(matches!(spec.validate(), Err(Error::InvalidArgument(_))));
    }

    #[test]
    fn run_spec_validation_rejects_blank_name() {
        let spec = RunSpec {
            image: "alpine".into(),
            name: Some("   ".into()),
            ..Default::default()
        };
        assert!(spec.validate().is_err());
    }

    #[test]
    fn pull_policy_strings_match_cli() {
        assert_eq!(PullPolicy::Always.as_str(), "always");
        assert_eq!(PullPolicy::Missing.as_str(), "missing");
        assert_eq!(PullPolicy::Never.as_str(), "never");
        assert_eq!(PullPolicy::default(), PullPolicy::Missing);
    }

    #[test]
    fn lifecycle_verbs_reject_empty_container_list_before_spawning() {
        let wslc = Wslc::with_program("definitely-not-a-real-binary");
        // 应该在启动子进程之前就失败，而不是报 ExecutableNotFound。
        assert!(matches!(stop(&wslc, &[]), Err(Error::InvalidArgument(_))));
        assert!(matches!(
            remove(&wslc, &[], false),
            Err(Error::InvalidArgument(_))
        ));
        assert!(matches!(
            prune(&wslc, None),
            Err(Error::ExecutableNotFound { .. })
        ));
    }

    #[test]
    fn log_options_shape_the_command() {
        // 通过 command_args 间接验证（不真的执行）。
        let wslc = Wslc::with_program("wslc");
        let args = wslc.command_args(&["logs", "--tail", "50", "--timestamps", "probe"]);
        assert_eq!(args, vec!["logs", "--tail", "50", "--timestamps", "probe"]);
    }

    #[test]
    fn list_parses_fixture_and_empty_output() {
        // 空输出（0 个容器）必须变成空 Vec，而不是错误。
        let empty: Vec<ContainerListItem> = jsonl::parse_lines("").unwrap();
        assert!(empty.is_empty());

        let text = include_str!("../../tests/fixtures/container_list.jsonl");
        let items: Vec<ContainerListItem> = jsonl::parse_lines(text).unwrap();
        assert_eq!(items.len(), 1);
    }

    #[test]
    fn running_summaries_merges_fixtures() {
        let items: Vec<ContainerListItem> =
            jsonl::parse_lines(include_str!("../../tests/fixtures/container_list.jsonl")).unwrap();
        let stats: Vec<ContainerStats> =
            jsonl::parse_lines(include_str!("../../tests/fixtures/container_stats.jsonl")).unwrap();
        let merged = ContainerSummary::merge(items, stats);
        assert_eq!(merged.len(), 1);
        assert!(merged[0].is_running());
        assert!(merged[0].stats.is_some());
    }
}
