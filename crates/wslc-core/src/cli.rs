//! `wslc.exe` 子进程封装。
//!
//! 职责：
//!
//! 1. **注入 `WSL_UTF8=1`** —— 否则 `wslc` 输出 UTF-16LE（实测，见
//!    `docs/wslc-schema.md` §0.1）。
//! 2. **不弹控制台窗口** —— 这是个 GUI 程序，每次调用 `wslc` 都闪一个黑框
//!    是不可接受的，因此用 `CREATE_NO_WINDOW`。
//! 3. **超时与取消** —— `exec` / `attach` 这类命令可能永远不返回，
//!    必须能超时并杀掉，且不能阻塞 UI 线程。
//! 4. **并发读 stdout/stderr** —— 单线程读其中一个管道会死锁。

use std::ffi::OsString;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::decode;
use crate::error::{Error, Result};

/// 非交互命令的默认超时。
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// 交互式命令（`attach` / `exec -it`）不会被内嵌执行，此常量仅作说明。
pub const INTERACTIVE_TIMEOUT: Duration = Duration::from_secs(2);

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
#[cfg(windows)]
const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;

/// 一次 `wslc` 调用的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    /// 实际传给 `wslc` 的参数（不含程序名）。
    pub args: Vec<String>,
    /// 退出码；被信号杀死时为 `None`。
    pub code: Option<i32>,
    /// 标准输出（已解码）。
    pub stdout: String,
    /// 标准错误（已解码）。
    pub stderr: String,
}

impl CommandOutput {
    /// 是否成功（退出码为 0）。
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }

    /// 标准输出去掉首尾空白。
    pub fn stdout_trimmed(&self) -> &str {
        self.stdout.trim()
    }

    /// 用于报错的合并输出。
    pub fn combined(&self) -> String {
        let mut s = self.stderr.trim().to_owned();
        if s.is_empty() {
            s = self.stdout.trim().to_owned();
        }
        s
    }

    /// 退出码转成错误（成功时返回 `Ok(self)`）。
    pub fn into_result(self) -> Result<Self> {
        if self.success() {
            Ok(self)
        } else {
            Err(Error::NonZeroExit {
                args: self.args.join(" "),
                code: self.code.unwrap_or(-1),
                stderr: self.combined(),
            })
        }
    }
}

/// `wslc` 调用器。
///
/// 克隆代价很低（只有一个 `PathBuf` 和几个 `Copy` 字段），可以放心塞进
/// `Arc` 或直接存进 UI 状态。
#[derive(Debug, Clone)]
pub struct Wslc {
    program: PathBuf,
    session: Option<u32>,
    timeout: Duration,
}

impl Default for Wslc {
    fn default() -> Self {
        Self::new()
    }
}

impl Wslc {
    /// 自动定位 `wslc.exe`。
    ///
    /// 顺序：环境变量 `WSLC_PATH` → 常见安装路径 → 依赖 `PATH` 里的 `wslc`。
    pub fn new() -> Self {
        Self {
            program: resolve_program(),
            session: None,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// 指定可执行文件路径。
    pub fn with_program(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            session: None,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// 设置会话（对应全局选项 `--session <id>`）。
    pub fn session(mut self, session: Option<u32>) -> Self {
        self.session = session;
        self
    }

    /// 设置超时。
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// 当前使用的可执行文件路径。
    pub fn program(&self) -> &std::path::Path {
        &self.program
    }

    /// 当前会话。
    pub fn session_id(&self) -> Option<u32> {
        self.session
    }

    /// 当前超时。
    pub fn timeout_duration(&self) -> Duration {
        self.timeout
    }

    /// 构造最终参数列表（含 `--session` 前缀）。
    pub fn command_args(&self, args: &[&str]) -> Vec<String> {
        let mut out = Vec::with_capacity(args.len() + 2);
        if let Some(session) = self.session {
            out.push("--session".to_owned());
            out.push(session.to_string());
        }
        out.extend(args.iter().map(|s| (*s).to_owned()));
        out
    }

    /// 执行并返回输出（**不**检查退出码）。
    pub fn run(&self, args: &[&str]) -> Result<CommandOutput> {
        self.run_owned(&self.command_args(args))
    }

    /// 执行并返回输出，非零退出码转成错误。
    ///
    /// 注意：`wslc` 有些命令（如 `list`）在结果为空时退出码仍为 0，
    /// 所以这里只看退出码，不看输出是否为空。
    pub fn run_checked(&self, args: &[&str]) -> Result<CommandOutput> {
        self.run(args)?.into_result()
    }

    /// 执行已经拼好的参数列表（已含 `--session`）。
    pub fn run_owned(&self, args: &[String]) -> Result<CommandOutput> {
        self.execute(args, self.timeout, None)
    }

    /// 执行并支持取消。
    ///
    /// `cancel` 被置为 `true` 时立即杀掉子进程并返回 [`Error::Cancelled`]。
    pub fn run_cancellable(
        &self,
        args: &[&str],
        cancel: Option<Arc<AtomicBool>>,
    ) -> Result<CommandOutput> {
        let owned = self.command_args(args);
        self.execute(&owned, self.timeout, cancel)
    }

    /// 执行并指定超时。
    pub fn run_with_timeout(&self, args: &[&str], timeout: Duration) -> Result<CommandOutput> {
        let owned = self.command_args(args);
        self.execute(&owned, timeout, None)
    }

    /// `wslc` 是否可用（跑一次 `version`）。
    pub fn is_available(&self) -> bool {
        self.run_with_timeout(&["version"], Duration::from_secs(5))
            .map(|o| o.success())
            .unwrap_or(false)
    }

    /// `wslc version` 的输出。
    pub fn version(&self) -> Result<String> {
        let out = self.run_checked(&["version"])?;
        Ok(out.stdout_trimmed().to_owned())
    }

    /// 在**新的控制台窗口**里启动交互式命令（`attach` / `exec -it`）。
    ///
    /// 交互式程序不能内嵌到 GPUI 的消息循环里，也不该被捕获输出，
    /// 因此这里让 Windows 开一个真正的终端窗口。
    pub fn spawn_in_new_console(&self, args: &[&str]) -> Result<()> {
        let owned = self.command_args(args);
        let mut cmd = self.build_command(&owned);
        cmd.stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());

        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(CREATE_NEW_CONSOLE);
        }

        cmd.spawn().map(|_| ()).map_err(|e| self.map_spawn_error(e))
    }

    /// 分离启动，**不等待**子进程结束，也不捕获输出。
    ///
    /// 用于 `wslc settings`（拉起默认编辑器）这类会长时间驻留的命令：
    /// 用 [`Wslc::run`] 会一直阻塞到编辑器关闭。
    pub fn spawn_detached(&self, args: &[&str]) -> Result<()> {
        let owned = self.command_args(args);
        let mut cmd = self.build_command(&owned);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        cmd.spawn().map(|_| ()).map_err(|e| self.map_spawn_error(e))
    }

    /// 构造 `Command`（不含超时/取消逻辑）。
    fn build_command(&self, args: &[String]) -> Command {
        let mut cmd = Command::new(&self.program);
        cmd.args(args);

        // 关键：不设这个，wslc 输出 UTF-16LE。
        cmd.env("WSL_UTF8", "1");
        cmd.env("NO_COLOR", "1");

        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        cmd
    }

    fn map_spawn_error(&self, e: std::io::Error) -> Error {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::ExecutableNotFound(self.program.display().to_string())
        } else {
            Error::Io(e)
        }
    }

    fn execute(
        &self,
        args: &[String],
        timeout: Duration,
        cancel: Option<Arc<AtomicBool>>,
    ) -> Result<CommandOutput> {
        let display = args.join(" ");

        let mut child = self
            .build_command(args)
            .spawn()
            .map_err(|e| self.map_spawn_error(e))?;

        // 必须并发读取两个管道，否则大输出会互相堵死。
        let stdout_pipe = child.stdout.take().expect("stdout 已设为 piped");
        let stderr_pipe = child.stderr.take().expect("stderr 已设为 piped");
        let stdout_handle = std::thread::spawn(move || read_all(stdout_pipe));
        let stderr_handle = std::thread::spawn(move || read_all(stderr_pipe));

        let deadline = Instant::now() + timeout;
        let mut timed_out = false;
        let mut cancelled = false;

        let code = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status.code(),
                Ok(None) => {}
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(Error::Io(e));
                }
            }

            if cancel
                .as_ref()
                .is_some_and(|flag| flag.load(Ordering::Relaxed))
            {
                cancelled = true;
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }

            if Instant::now() >= deadline {
                timed_out = true;
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }

            std::thread::sleep(Duration::from_millis(15));
        };

        // 等待两个读取线程收尾，保证拿到完整输出。
        let stdout_bytes = stdout_handle.join().unwrap_or_default();
        let stderr_bytes = stderr_handle.join().unwrap_or_default();

        if cancelled {
            return Err(Error::Cancelled { args: display });
        }
        if timed_out {
            return Err(Error::Timeout {
                args: display,
                timeout,
            });
        }

        Ok(CommandOutput {
            args: args.to_vec(),
            code,
            stdout: decode::decode(&stdout_bytes),
            stderr: decode::decode(&stderr_bytes),
        })
    }

    /// 未加 `--session` 的原始参数（供需要精确控制的调用方使用）。
    pub fn raw_args(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| (*s).to_owned()).collect()
    }
}

fn read_all(mut reader: impl Read) -> Vec<u8> {
    let mut buf = Vec::new();
    // 读失败时返回已读到的部分，不 panic。
    let _ = reader.read_to_end(&mut buf);
    buf
}

/// 定位 `wslc.exe`。
fn resolve_program() -> PathBuf {
    if let Some(explicit) = std::env::var_os("WSLC_PATH") {
        if !explicit.is_empty() {
            return PathBuf::from(explicit);
        }
    }

    for candidate in candidate_paths() {
        if candidate.is_file() {
            return candidate;
        }
    }

    // 交给 PATH 解析；真的没有时 spawn 会返回 ExecutableNotFound。
    PathBuf::from("wslc")
}

/// `wslc.exe` 的常见安装位置。
fn candidate_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();

    for var in ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"] {
        if let Some(base) = std::env::var_os(var) {
            out.push(PathBuf::from(&base).join("WSL").join("wslc.exe"));
        }
    }

    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        out.push(
            PathBuf::from(&local)
                .join("Microsoft")
                .join("WindowsApps")
                .join("wslc.exe"),
        );
        out.push(PathBuf::from(&local).join("wslc").join("wslc.exe"));
    }

    // 实测本机的安装位置就是 Program Files\WSL\wslc.exe（WSL 3.x）。
    out
}

/// 把 `Vec<String>` 转成 `Vec<OsString>`（测试与内部使用）。
#[doc(hidden)]
pub fn to_os_strings(args: &[String]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_option_is_prepended() {
        let wslc = Wslc::with_program("wslc.exe").session(Some(3));
        let args = wslc.command_args(&["list", "-a", "--format", "json"]);
        assert_eq!(
            args,
            vec!["--session", "3", "list", "-a", "--format", "json"]
        );
    }

    #[test]
    fn without_session_args_are_untouched() {
        let wslc = Wslc::with_program("wslc.exe");
        let args = wslc.command_args(&["info", "--format", "json"]);
        assert_eq!(args, vec!["info", "--format", "json"]);
    }

    #[test]
    fn builders_are_chainable_and_cheap_to_clone() {
        let wslc = Wslc::with_program("x")
            .session(Some(1))
            .timeout(Duration::from_secs(5));
        assert_eq!(wslc.session_id(), Some(1));
        assert_eq!(wslc.timeout_duration(), Duration::from_secs(5));
        let clone = wslc.clone();
        assert_eq!(clone.session_id(), Some(1));
        assert_eq!(clone.program().to_string_lossy(), "x");
    }

    #[test]
    fn default_timeout_is_thirty_seconds() {
        assert_eq!(Wslc::new().timeout_duration(), DEFAULT_TIMEOUT);
        assert_eq!(DEFAULT_TIMEOUT, Duration::from_secs(30));
    }

    #[test]
    fn candidate_paths_include_the_real_install_location() {
        // 实测本机：C:\Program Files\WSL\wslc.exe
        let candidates = candidate_paths();
        assert!(
            candidates.iter().any(|p| p.ends_with("WSL\\wslc.exe")
                || p.to_string_lossy().contains("WSL")),
            "候选路径里应包含 Program Files\\WSL\\wslc.exe：{candidates:?}"
        );
    }

    #[test]
    fn command_output_success_and_error_mapping() {
        let ok = CommandOutput {
            args: vec!["info".into()],
            code: Some(0),
            stdout: "{}".into(),
            stderr: String::new(),
        };
        assert!(ok.success());
        assert!(ok.clone().into_result().is_ok());

        let bad = CommandOutput {
            args: vec!["bogus".into()],
            code: Some(1),
            stdout: String::new(),
            stderr: "当前命令的选项名称未被识别".into(),
        };
        assert!(!bad.success());
        let err = bad.into_result().unwrap_err();
        assert!(matches!(err, Error::NonZeroExit { code: 1, .. }));
        assert!(err.to_string().contains("选项名称未被识别"));
    }

    #[test]
    fn combined_output_prefers_stderr_then_stdout() {
        let o = CommandOutput {
            args: vec![],
            code: Some(1),
            stdout: "out".into(),
            stderr: "err".into(),
        };
        assert_eq!(o.combined(), "err");
        let o = CommandOutput {
            stderr: "   ".into(),
            ..o
        };
        assert_eq!(o.combined(), "out");
    }

    #[test]
    fn to_os_strings_roundtrip() {
        let args = vec!["a".to_owned(), "b".to_owned()];
        assert_eq!(to_os_strings(&args).len(), 2);
    }

    /// 需要本机安装了 wslc；没有时自动跳过。
    #[test]
    fn real_wslc_is_available_on_this_machine() {
        let wslc = Wslc::new();
        if !wslc.is_available() {
            eprintln!("跳过：本机没有可用的 wslc");
            return;
        }
        let version = wslc.version().expect("wslc version 应成功");
        assert!(!version.is_empty());
    }

    /// 超时保护：故意给一个极短超时去跑 `info`。
    ///
    /// `info` 通常很快，所以这里只断言"要么成功、要么是超时错误"，
    /// **不会** flaky。
    #[test]
    fn short_timeout_either_succeeds_or_times_out_cleanly() {
        let wslc = Wslc::new();
        if !wslc.is_available() {
            return;
        }
        match wslc.run_with_timeout(&["info", "--format", "json"], Duration::from_nanos(1)) {
            Ok(out) => assert!(out.success()),
            Err(Error::Timeout { .. }) => {}
            Err(other) => panic!("不应出现其它错误：{other}"),
        }
    }
}
