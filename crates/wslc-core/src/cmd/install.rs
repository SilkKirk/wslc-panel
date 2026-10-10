//! 执行「添加实例」的**计划**：起进程、流式读输出、轮询进度、可取消。
//!
//! 计划本身是纯逻辑（[`crate::model::install`]），这里只负责"把它做出来"：
//!
//! - 每一步都发 [`InstallEvent`]，界面据此画步骤、日志、进度；
//! - 长步骤**不设固定超时**（在线安装、下载 rootfs 都可能几十分钟），
//!   靠 [`InstallCancel`] 取消 —— 与导出用的是同一套取舍；
//! - 进度不看子进程的输出，而是**轮询产物的大小**
//!   （下载的文件 / 导出的 tar）：那个数字真实且连续，
//!   而 `curl` 的进度条是 `\r` 刷新的、按行读会攒成一整行（见 [`crate::mirrors`]）。
//!
//! # 取消这件事的边界
//!
//! [`PlannedStep::cancellable`] 为 `false` 的步骤（重定位）**刻意不看取消标志**：
//! 那一步会先 `--unregister` 掉刚装好的发行版，中途停下等于把它删了。
//! 界面据此不给"取消"按钮，而不是让用户点了之后才发现没用。
//!
//! 另外，取消下载之后**半成品文件会被删掉** —— 留着只会占空间，
//! 而且用户多半以为它是个能用的 rootfs。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::cli::{self, StreamHandle, Wsl};
use crate::error::{Error, Result};
use crate::model::install::{InstallPlan, PlannedStep, PlanProgram};

/// 轮询子进程是否结束的间隔。
///
/// 250 ms 是"界面看起来是活的"和"别把 CPU 烧在这上面"之间的折中；
/// 导出那边用的是 500 ms，这里稍微快一点是因为安装的输出更密。
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// 等发行版注册：最多轮询几次。
const REGISTER_TRIES: usize = 15;
/// 等发行版注册：每次之间等多久。
///
/// 15 × 2 s = 30 s。`wsl --install -d <id>` 正常返回时其实已经注册好了，
/// 这一等是给"返回了但还没落进列表"留的余量（参考实现也是 15 次）。
const REGISTER_WAIT: Duration = Duration::from_secs(2);

/// 短命令（`wsl -l -q`）的超时。
const QUICK_TIMEOUT: Duration = Duration::from_secs(30);

/// 下载用的程序。
///
/// 为什么是 `curl.exe` 而不是一个 HTTP 客户端 crate：见 [`crate::mirrors`] 的模块说明
/// （一句话：本仓库不能新增依赖，因为本机没有 cargo 而 CI 全部 `--locked`）。
const CURL: &str = "curl.exe";
/// 错误消息里用的程序名。
const CURL_LABEL: &str = "curl.exe";

// ---------------------------------------------------------------------------
// 事件与选项
// ---------------------------------------------------------------------------

/// 安装过程中发给界面的事件。
///
/// 故意做成**纯数据**：它要跨线程（后台执行器 → 界面），
/// 带上任何 GPUI 类型都会把这条通路堵死。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallEvent {
    /// 开始某一步。
    Step {
        /// 第几步（从 1 开始）。
        index: usize,
        /// 一共几步。
        total: usize,
        /// 这一步在干什么。
        label: String,
        /// 界面上那一行（可能是真命令行，见 [`PlannedStep::line`]）。
        line: String,
        /// 这一步能不能被取消。
        ///
        /// 界面据此决定**给不给**取消按钮：重定位那一步不接受取消
        /// （中途停下等于把刚装好的删了），让用户点了之后才发现没用更糟。
        cancellable: bool,
    },
    /// 子进程的一行输出。
    Line(String),
    /// 进度（下载 / 导出）。`total` 未知时界面只显示已完成的量。
    Progress {
        /// 已经产生多少字节。
        have: u64,
        /// 预期总量（探测时拿到的 `Content-Length`）；未知时为 `None`。
        total: Option<u64>,
        /// 这一步已经跑了多少秒。
        secs: u64,
    },
    /// 某一步结束。
    StepDone {
        /// 第几步（与 [`InstallEvent::Step`] 对应）。
        index: usize,
        /// 成功了吗。
        ok: bool,
        /// 失败原因（成功时为空串）。
        detail: String,
    },
}

/// 可克隆的取消标志。
///
/// 界面握一个、执行器握一个：用户点「取消」时把标志置上，
/// 执行器在轮询循环里看到它就杀掉当前子进程。
#[derive(Clone, Default)]
pub struct InstallCancel(Arc<AtomicBool>);

impl InstallCancel {
    /// 新建（未取消）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 请求取消。重复调用无害。
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// 已经被请求取消了吗。
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// 执行一次安装需要的运行时选项。
pub struct RunOptions {
    /// 取消标志（界面持有同一个）。
    pub cancel: InstallCancel,
    /// 事件回调。会在**多个线程**上被调用，实现里不该做重活。
    pub on_event: Arc<dyn Fn(InstallEvent) + Send + Sync>,
    /// 下载步骤的预期总字节数（界面探测镜像时拿到的）。
    ///
    /// 放在这里而不是塞进步骤参数：那是**显示用**的信息，
    /// 不该混进真正要执行的命令行里（否则"等效命令"就多出一个假参数）。
    pub expected_bytes: Option<u64>,
}

impl RunOptions {
    /// 新建（默认不取消、不带预期大小）。
    pub fn new(on_event: impl Fn(InstallEvent) + Send + Sync + 'static) -> Self {
        Self {
            cancel: InstallCancel::new(),
            on_event: Arc::new(on_event),
            expected_bytes: None,
        }
    }
}

/// 一次安装的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallSummary {
    /// 最终发行版名。
    pub name: String,
    /// 最终安装目录（可能为空）。
    pub install_dir: String,
    /// 成功了吗。
    pub ok: bool,
    /// 是被用户取消的吗。
    pub cancelled: bool,
    /// 一句话结果（成功或失败原因）。
    pub detail: String,
    /// 失败的步骤（成功时为 `None`）。
    pub failed_step: Option<String>,
}

// ---------------------------------------------------------------------------
// 执行
// ---------------------------------------------------------------------------

/// 按计划装一个发行版。**阻塞**，必须在后台执行器上跑。
pub fn run_plan(wsl: &Wsl, plan: &InstallPlan, opts: RunOptions) -> InstallSummary {
    let total = plan.steps.len();

    for (index, step) in plan.steps.iter().enumerate() {
        let number = index + 1;
        emit(
            &opts,
            InstallEvent::Step {
                index: number,
                total,
                label: step.label.clone(),
                line: step.line(),
                cancellable: step.cancellable,
            },
        );

        let outcome = match step.program {
            PlanProgram::Wsl => run_wsl_step(wsl, step, &opts),
            PlanProgram::Curl => run_curl_step(step, &opts),
            PlanProgram::CreateDir => create_dir(step),
            PlanProgram::RemoveFile => remove_file(step),
            PlanProgram::WaitRegistered => wait_registered(wsl, step, &opts),
            PlanProgram::EnsureRelocated => ensure_relocated(wsl, step, &opts),
        };

        match outcome {
            Ok(()) => emit(
                &opts,
                InstallEvent::StepDone {
                    index: number,
                    ok: true,
                    detail: String::new(),
                },
            ),
            Err(e) => {
                let detail = e.to_string();
                emit(
                    &opts,
                    InstallEvent::StepDone {
                        index: number,
                        ok: false,
                        detail: detail.clone(),
                    },
                );
                return InstallSummary {
                    name: plan.name.clone(),
                    install_dir: plan.install_dir.clone(),
                    ok: false,
                    cancelled: matches!(e, Error::Cancelled { .. }),
                    detail,
                    failed_step: Some(step.label.clone()),
                };
            }
        }
    }

    InstallSummary {
        name: plan.name.clone(),
        install_dir: plan.install_dir.clone(),
        ok: true,
        cancelled: false,
        detail: format!("{} 已安装", plan.name),
        failed_step: None,
    }
}

fn emit(opts: &RunOptions, event: InstallEvent) {
    (opts.on_event)(event);
}

/// 取步骤里的第 `index` 个参数（计划是我们自己拼的，但执行器不该因此 panic）。
fn arg(step: &PlannedStep, index: usize) -> Result<&str> {
    step.args.get(index).map(String::as_str).ok_or_else(|| {
        Error::Install(format!(
            "内部错误：步骤「{}」缺少第 {} 个参数",
            step.label,
            index + 1
        ))
    })
}

/// 起一个 `wsl.exe` 步骤，边跑边读。
///
/// 带 [`PlannedStep::retry_other_source`] 的步骤（只有在线安装那一步）失败时会
/// **换一个下载源**（微软商店 ⇄ `--web-download`）重试一次：两条通道走的是不同的
/// 下载实现，实测本机上一条秒失败、另一条能连上 —— 换一次比让用户自己猜好。
fn run_wsl_step(wsl: &Wsl, step: &PlannedStep, opts: &RunOptions) -> Result<()> {
    let first = run_wsl_once(wsl, &step.args, step.cancellable, opts);
    if first.is_ok() || !step.retry_other_source {
        return first;
    }

    let Some(args) = args_with_source_toggled(&step.args) else {
        return first;
    };
    emit(
        opts,
        InstallEvent::Line("这一次没连上 —— 换一个下载源再试一次（微软商店 ⇄ GitHub）".to_owned()),
    );
    run_wsl_once(wsl, &args, step.cancellable, opts)
}

/// 跑一条 `wsl.exe` 命令（一步，不重试）。
fn run_wsl_once(
    wsl: &Wsl,
    args: &[String],
    cancellable: bool,
    opts: &RunOptions,
) -> Result<()> {
    let (sink, last) = line_sink(opts);
    let handle = {
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        wsl.spawn_streaming(&refs, sink)?
    };
    finish_stream(handle, "wsl", args, last, cancellable, None, opts)
}

/// 商店 ⇄ `--web-download`：有就去掉，没有就插在 `-d <id>` 后面。
///
/// 返回 `None` 表示这条命令根本不是在装某个发行版（没有 `-d`）——
/// 那种情况不该乱改参数。
fn args_with_source_toggled(args: &[String]) -> Option<Vec<String>> {
    if let Some(at) = args.iter().position(|arg| arg == "--web-download") {
        let mut out = Vec::with_capacity(args.len() - 1);
        out.extend(args[..at].iter().cloned());
        out.extend(args[at + 1..].iter().cloned());
        return Some(out);
    }

    // 插在 `-d <id>` 后面：参数顺序对 wsl 无所谓，但这样日志里读起来和文档一致
    let at = args.iter().position(|arg| arg == "-d")?;
    let mut out = Vec::with_capacity(args.len() + 1);
    out.extend(args[..=at + 1].iter().cloned());
    out.push("--web-download".to_owned());
    out.extend(args.get(at + 2..).unwrap_or_default().iter().cloned());
    Some(out)
}

/// 起一个 `curl.exe` 步骤（下载），并轮询产物大小当进度。
fn run_curl_step(step: &PlannedStep, opts: &RunOptions) -> Result<()> {
    let out = output_path(&step.args)?;
    let (sink, last) = line_sink(opts);
    let handle = cli::spawn_streaming_program(
        CURL,
        CURL_LABEL,
        cli::CURL_NOT_FOUND_HINT,
        &step.args,
        sink,
    )?;
    let progress = ProgressSource {
        path: out.clone(),
        total: opts.expected_bytes,
        started: Instant::now(),
    };

    let result = finish_stream(
        handle,
        CURL_LABEL,
        &step.args,
        last,
        step.cancellable,
        Some(&progress),
        opts,
    );

    // 下载没成功 → 把半成品删掉。留着只会占空间，而且用户会把它当成能用的 rootfs。
    if result.is_err() && out.exists() {
        match std::fs::remove_file(&out) {
            Ok(()) => emit(
                opts,
                InstallEvent::Line(format!("已删掉没下完的临时文件 {}", out.display())),
            ),
            Err(e) => emit(
                opts,
                InstallEvent::Line(format!("临时文件 {} 删不掉（{e}），要自己清理", out.display())),
            ),
        }
    }

    result
}

/// 轮询到子进程结束（或取消），把结果翻译成 `Result`。
#[allow(clippy::too_many_arguments)]
fn finish_stream(
    handle: StreamHandle,
    program: &'static str,
    args: &[String],
    last: Arc<Mutex<String>>,
    cancellable: bool,
    progress: Option<&ProgressSource>,
    opts: &RunOptions,
) -> Result<()> {
    loop {
        if cancellable && opts.cancel.is_cancelled() {
            handle.cancel_token().cancel();
            // 杀掉之后还要等它退出，否则读取线程会跟着走掉、日志丢尾巴。
            let _ = handle.finish();
            return Err(Error::Cancelled {
                program,
                args: args.join(" "),
            });
        }

        match handle.try_wait() {
            Ok(Some(code)) => {
                let _ = handle.finish();
                if code == 0 {
                    return Ok(());
                }
                let detail = last
                    .lock()
                    .map(|guard| guard.clone())
                    .unwrap_or_default();
                return Err(Error::NonZeroExit {
                    program,
                    args: args.join(" "),
                    code,
                    stderr: if detail.trim().is_empty() {
                        "（它没有给出任何输出）".to_owned()
                    } else {
                        detail
                    },
                });
            }
            Ok(None) => {}
            Err(e) => {
                let _ = handle.cancel_token().cancel();
                let _ = handle.finish();
                return Err(e);
            }
        }

        if let Some(progress) = progress {
            progress.emit(opts);
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// 造一个"把每行输出存下最后一行、同时发给界面"的回调。
fn line_sink(opts: &RunOptions) -> (impl Fn(&str) + Send + Sync + 'static, Arc<Mutex<String>>) {
    let events = Arc::clone(&opts.on_event);
    let last: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let slot = Arc::clone(&last);

    let sink = move |line: &str| {
        if let Ok(mut guard) = slot.lock() {
            *guard = line.to_owned();
        }
        events(InstallEvent::Line(line.to_owned()));
    };

    (sink, last)
}

/// 从 `curl` 的参数里找出 `-o` 指的文件。
fn output_path(args: &[String]) -> Result<PathBuf> {
    let at = args.iter().position(|a| a == "-o").ok_or_else(|| {
        Error::Install("内部错误：下载步骤里没有 `-o`，拿不到产物路径".to_owned())
    })?;
    let path = args.get(at + 1).ok_or_else(|| {
        Error::Install("内部错误：下载步骤的 `-o` 后面没有路径".to_owned())
    })?;
    Ok(PathBuf::from(path))
}

/// 轮询产物大小的进度源。
struct ProgressSource {
    path: PathBuf,
    total: Option<u64>,
    started: Instant,
}

impl ProgressSource {
    fn emit(&self, opts: &RunOptions) {
        // 文件还没建出来时算 0 —— 不报错，用户看到的是"0 B"。
        let have = std::fs::metadata(&self.path)
            .map(|meta| meta.len())
            .unwrap_or(0);
        emit(
            opts,
            InstallEvent::Progress {
                have,
                total: self.total,
                secs: self.started.elapsed().as_secs(),
            },
        );
    }
}

fn create_dir(step: &PlannedStep) -> Result<()> {
    let dir = arg(step, 0)?;
    std::fs::create_dir_all(dir)
        .map_err(|e| Error::Install(format!("建不出安装目录 {dir}：{e}")))
}

/// 删文件。**文件本来就不在也算成功** —— 这是收尾步骤，不该因此判定失败。
fn remove_file(step: &PlannedStep) -> Result<()> {
    let path = arg(step, 0)?;
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::Install(format!("删不掉临时文件 {path}：{e}"))),
    }
}

/// 等发行版出现在 `wsl -l -q` 里。
fn wait_registered(wsl: &Wsl, step: &PlannedStep, opts: &RunOptions) -> Result<()> {
    let name = arg(step, 0)?;

    for attempt in 0..REGISTER_TRIES {
        if opts.cancel.is_cancelled() {
            return Err(Error::Cancelled {
                program: "wsl",
                args: format!("--list --quiet（等 {name} 注册）"),
            });
        }

        if installed_names(wsl).iter().any(|item| item == name) {
            emit(opts, InstallEvent::Line(format!("{name} 已经出现在列表里")));
            return Ok(());
        }

        emit(
            opts,
            InstallEvent::Line(format!(
                "还在等 {name} 注册（第 {} / {REGISTER_TRIES} 次）",
                attempt + 1
            )),
        );
        sleep_cancellable(REGISTER_WAIT, opts);
    }

    Err(Error::Install(format!(
        "{name} 装完了却一直没出现在 `wsl -l -q` 里（等了 {} 秒）。\
         去实例列表看一眼：如果它其实装好了，这条报错可以忽略；如果没装好，重试一次。",
        REGISTER_TRIES as u64 * REGISTER_WAIT.as_secs()
    )))
}

/// 读 `wsl -l -q`（只要名字）。
fn installed_names(wsl: &Wsl) -> Vec<String> {
    match wsl.run_with_timeout(&["--list", "--quiet"], QUICK_TIMEOUT) {
        Ok(out) if out.success() => out
            .stdout
            .lines()
            .map(|line| line.trim().trim_start_matches('*').trim().to_owned())
            .filter(|line| !line.is_empty())
            .collect(),
        Ok(out) => {
            tracing::warn!("wsl -l -q 退出码 {:?}", out.code);
            Vec::new()
        }
        Err(e) => {
            tracing::warn!("wsl -l -q 失败：{e}");
            Vec::new()
        }
    }
}

/// 分段睡眠，顺便响应取消（睡 2 秒里点了取消，不用等到睡完）。
fn sleep_cancellable(total: Duration, opts: &RunOptions) {
    let step = Duration::from_millis(200);
    let mut slept = Duration::ZERO;
    while slept < total {
        if opts.cancel.is_cancelled() {
            return;
        }
        std::thread::sleep(step);
        slept += step;
    }
}

/// 把刚装好的发行版弄成"用户要的名字 + 用户要的位置"。
///
/// 三种情况，各有各的做法（都比"一律导出再导入"省事）：
///
/// | 情况 | 做法 |
/// |---|---|
/// | 名字已经对、位置也对 | 什么也不做 |
/// | 名字对、位置不对 | `wsl --manage <name> --move <dir>`（WSL 自己搬，比导来导去稳妥） |
/// | 名字不对 | `--export` → `--unregister` → `--import`（WSL 没有"改名"命令） |
///
/// ⚠️ 第三步里 `--unregister` 之后数据只在那个中转 tar 里了。
/// 所以：导入失败时**保留** tar 并告诉用户它在哪（这一点和参考实现不同 ——
/// 它无论如何都删掉了）。
fn ensure_relocated(wsl: &Wsl, step: &PlannedStep, opts: &RunOptions) -> Result<()> {
    let from = arg(step, 0)?.to_owned();
    let to = arg(step, 1)?.to_owned();
    let dir = arg(step, 2)?.to_owned();
    let temp_tar = arg(step, 3)?.to_owned();

    // 注册表读不到时**不猜**：宁可少做一步并说明，也不要在没有依据的情况下
    // 去导出/注销用户的东西。
    let entries = crate::cmd::distro::read_registry();
    let base_of = |name: &str| {
        entries
            .iter()
            .find(|entry| entry.name == name)
            .and_then(|entry| entry.base_path.clone())
    };

    if from == to {
        let Some(current) = base_of(&to) else {
            emit(
                opts,
                InstallEvent::Line(format!(
                    "读不到注册表，没法核实 {to} 到底装在哪儿 —— 这里就不动了。\
                     位置不对的话，可以在实例详情里用「移动」。"
                )),
            );
            return Ok(());
        };
        if dir.trim().is_empty() || same_dir(&current, &dir) {
            emit(
                opts,
                InstallEvent::Line(format!("{to} 已经装好了，不需要重定位")),
            );
            return Ok(());
        }

        emit(
            opts,
            InstallEvent::Line(format!(
                "{to} 现在在 {}，要挪到 {dir}（用 `wsl --manage --move`）",
                current.display()
            )),
        );
        return run_wsl_args(
            wsl,
            &["--manage", to.as_str(), "--move", dir.as_str()],
            false,
            None,
            opts,
        );
    }

    // 名字要改：WSL 没有改名命令，只能导出再导入。
    emit(
        opts,
        InstallEvent::Line(format!("要把 {from} 改名成 {to}，先导出到 {temp_tar}")),
    );
    if let Some(parent) = Path::new(&temp_tar).parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| Error::Install(format!("建不出临时目录 {}：{e}", parent.display())))?;
    }

    let export_progress = ProgressSource {
        path: PathBuf::from(&temp_tar),
        total: None,
        started: Instant::now(),
    };
    run_wsl_args(
        wsl,
        &["--export", from.as_str(), temp_tar.as_str()],
        false,
        Some(&export_progress),
        opts,
    )?;

    emit(opts, InstallEvent::Line(format!("注销 {from}")));
    run_wsl_args(wsl, &["--unregister", from.as_str()], false, None, opts)?;

    std::fs::create_dir_all(&dir)
        .map_err(|e| Error::Install(format!("建不出安装目录 {dir}：{e}")))?;

    emit(opts, InstallEvent::Line(format!("把中转 tar 导入成 {to}（装到 {dir}）")));
    let version = crate::model::install::WSL_VERSION.to_string();
    let import = run_wsl_args(
        wsl,
        &[
            "--import",
            to.as_str(),
            dir.as_str(),
            temp_tar.as_str(),
            "--version",
            version.as_str(),
        ],
        false,
        None,
        opts,
    );

    match import {
        Ok(()) => {
            match std::fs::remove_file(&temp_tar) {
                Ok(()) => emit(
                    opts,
                    InstallEvent::Line(format!("已删掉中转文件 {temp_tar}")),
                ),
                Err(e) => emit(
                    opts,
                    InstallEvent::Line(format!("中转文件 {temp_tar} 删不掉（{e}），要自己清理")),
                ),
            }
            Ok(())
        }
        Err(e) => {
            // ⚠️ 这时候 `from` 已经被注销，**数据只在这个 tar 里**。
            // 删掉它等于把刚装好的东西扔了，所以保留并说清位置。
            emit(
                opts,
                InstallEvent::Line(format!(
                    "导入失败，但{from}已经注销了 —— 数据只在中转文件里，所以**故意保留**它：\
                     {temp_tar}。可以手动 `wsl --import {to} <目录> \"{temp_tar}\" --version 2` 再试。"
                )),
            );
            Err(e)
        }
    }
}

/// 跑一条 `wsl.exe` 命令（内部用，参数是拼好的）。
fn run_wsl_args(
    wsl: &Wsl,
    args: &[&str],
    cancellable: bool,
    progress: Option<&ProgressSource>,
    opts: &RunOptions,
) -> Result<()> {
    let owned: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
    let (sink, last) = line_sink(opts);
    let handle = wsl.spawn_streaming(args, sink)?;
    finish_stream(
        handle,
        "wsl",
        &owned,
        last,
        cancellable,
        progress,
        opts,
    )
}

/// 两个目录是不是同一个（忽略结尾分隔符与大小写）。
///
/// Windows 路径不区分大小写，而用户在表单里可能写成 `d:\wsl\X`。
fn same_dir(left: &Path, right: &str) -> bool {
    let normalize = |text: &str| text.trim_end_matches(['\\', '/']).to_ascii_lowercase();
    normalize(&left.to_string_lossy()) == normalize(right)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::install::{PlanContext, InstallSource, InstallSpec};

    #[test]
    fn cancel_flag_is_shared_and_sticky() {
        let cancel = InstallCancel::new();
        assert!(!cancel.is_cancelled());
        let clone = cancel.clone();
        clone.cancel();
        // 界面和执行器握的是同一个标志
        assert!(cancel.is_cancelled());
        assert!(clone.is_cancelled());
    }

    #[test]
    fn output_path_is_read_from_the_curl_arguments() {
        let owned = |items: &[&str]| -> Vec<String> {
            items.iter().map(|item| (*item).to_owned()).collect()
        };
        let args = owned(&["-s", "-o", r"D:\tmp\a.tar.xz", "https://x"]);
        assert_eq!(output_path(&args).unwrap(), PathBuf::from(r"D:\tmp\a.tar.xz"));

        // 没有 -o / -o 后面没东西 → 明确的内部错误，而不是 panic
        assert!(matches!(
            output_path(&owned(&["-s", "https://x"])),
            Err(Error::Install(_))
        ));
        assert!(matches!(output_path(&owned(&["-o"])), Err(Error::Install(_))));
    }

    #[test]
    fn same_dir_ignores_case_and_trailing_separators() {
        assert!(same_dir(Path::new(r"D:\wsl\X"), r"D:\wsl\X"));
        assert!(same_dir(Path::new(r"D:\wsl\X\"), r"D:\wsl\X"));
        assert!(same_dir(Path::new(r"D:\WSL\x"), r"d:\wsl\X"));
        assert!(!same_dir(Path::new(r"D:\wsl\X"), r"D:\wsl\Y"));
    }

    #[test]
    fn missing_arguments_are_errors_not_panics() {
        let step = PlannedStep {
            label: "x".to_owned(),
            program: PlanProgram::CreateDir,
            args: Vec::new(),
            cancellable: true,
            retry_other_source: false,
        };
        assert!(matches!(arg(&step, 0), Err(Error::Install(_))));
    }

    #[test]
    fn the_download_source_can_be_toggled_for_the_retry() {
        let owned = |items: &[&str]| -> Vec<String> {
            items.iter().map(|item| (*item).to_owned()).collect()
        };

        // 商店 → GitHub：插在 `-d <id>` 后面
        let store = owned(&["--install", "-d", "Ubuntu-24.04", "--version", "2", "--no-launch"]);
        assert_eq!(
            args_with_source_toggled(&store).unwrap(),
            owned(&[
                "--install",
                "-d",
                "Ubuntu-24.04",
                "--web-download",
                "--version",
                "2",
                "--no-launch"
            ])
        );

        // GitHub → 商店：去掉那个开关，其余原样
        let web = owned(&[
            "--install",
            "-d",
            "Ubuntu",
            "--web-download",
            "--version",
            "2",
        ]);
        assert_eq!(
            args_with_source_toggled(&web).unwrap(),
            owned(&["--install", "-d", "Ubuntu", "--version", "2"])
        );

        // 不是在装发行版（没有 `-d`）→ 不重试（返回 None），不许乱改命令
        assert_eq!(args_with_source_toggled(&owned(&["--list", "--online"])), None);
        assert_eq!(args_with_source_toggled(&[]), None);
    }

    #[test]
    fn a_plan_with_only_cleanup_steps_runs_to_completion() {
        // 这条测试**不起任何进程**：`CreateDir` 与 `RemoveFile` 是纯文件系统操作，
        // 所以能真跑一遍 executor 的主循环（包括事件顺序），
        // 而"起 wsl.exe"那几条只能在真机上验 —— 见 §12 的手测清单。
        let dir = std::env::temp_dir().join(format!("wslc-panel-install-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("temp.txt");

        let plan = InstallPlan {
            name: "X".to_owned(),
            install_dir: dir.to_string_lossy().into_owned(),
            steps: vec![
                PlannedStep {
                    label: "创建安装目录".to_owned(),
                    program: PlanProgram::CreateDir,
                    args: vec![dir.to_string_lossy().into_owned()],
                    cancellable: true,
            retry_other_source: false,
                },
                PlannedStep {
                    label: "删掉临时文件（本来就不在，也算成功）".to_owned(),
                    program: PlanProgram::RemoveFile,
                    args: vec![file.to_string_lossy().into_owned()],
                    cancellable: true,
            retry_other_source: false,
                },
            ],
            notes: Vec::new(),
        };

        let events: Arc<Mutex<Vec<InstallEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let opts = RunOptions::new(move |event| {
            if let Ok(mut guard) = sink.lock() {
                guard.push(event);
            }
        });

        // 用一个必然不存在的可执行文件：这两步都不该碰它
        let wsl = Wsl::with_program("definitely-not-a-real-binary");
        let summary = run_plan(&wsl, &plan, opts);

        assert!(summary.ok, "{summary:?}");
        assert!(dir.exists(), "CreateDir 应该真的建出目录");
        let _ = std::fs::remove_dir_all(&dir);

        let events = events.lock().unwrap();
        let steps = events
            .iter()
            .filter(|e| matches!(e, InstallEvent::Step { .. }))
            .count();
        let done = events
            .iter()
            .filter(|e| matches!(e, InstallEvent::StepDone { ok: true, .. }))
            .count();
        assert_eq!(steps, 2, "{events:?}");
        assert_eq!(done, 2, "{events:?}");
        // 第一步的事件里要带上序号和总数（界面靠它画"第 1 / 2 步"）
        match &events[0] {
            InstallEvent::Step { index, total, .. } => {
                assert_eq!((*index, *total), (1, 2));
            }
            other => panic!("第一个事件应该是 Step：{other:?}"),
        }
    }

    #[test]
    fn a_failing_plan_stops_at_the_first_bad_step() {
        // 用"起一个不存在的可执行文件"造一次失败：`Wsl` 的 spawn 会返回
        // ExecutableNotFound，执行器应当**停在那里**并把原因交出来。
        let plan = InstallPlan {
            name: "X".to_owned(),
            install_dir: String::new(),
            steps: vec![
                PlannedStep {
                    label: "导入".to_owned(),
                    program: PlanProgram::Wsl,
                    args: vec!["--import".to_owned()],
                    cancellable: true,
            retry_other_source: false,
                },
                PlannedStep {
                    label: "不该被执行到".to_owned(),
                    program: PlanProgram::CreateDir,
                    args: vec![std::env::temp_dir().to_string_lossy().into_owned()],
                    cancellable: true,
            retry_other_source: false,
                },
            ],
            notes: Vec::new(),
        };

        let seen: Arc<Mutex<Vec<InstallEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let opts = RunOptions::new(move |event| {
            if let Ok(mut guard) = sink.lock() {
                guard.push(event);
            }
        });

        let wsl = Wsl::with_program("definitely-not-a-real-binary");
        let summary = run_plan(&wsl, &plan, opts);

        assert!(!summary.ok);
        assert!(!summary.cancelled);
        assert_eq!(summary.failed_step.as_deref(), Some("导入"));
        assert!(summary.detail.contains("找不到"), "{}", summary.detail);

        let events = seen.lock().unwrap();
        let steps = events
            .iter()
            .filter(|e| matches!(e, InstallEvent::Step { .. }))
            .count();
        assert_eq!(steps, 1, "失败之后不该继续跑下一步：{events:?}");
        assert!(events.iter().any(|e| matches!(
            e,
            InstallEvent::StepDone { ok: false, .. }
        )));
    }

    #[test]
    fn an_already_cancelled_run_stops_before_the_first_step() {
        // WaitRegistered 是最容易"取消在第一步之前就生效"的一步：
        // 它每轮都先看标志。用它来钉住"取消不会假装成功"。
        let plan = InstallPlan {
            name: "X".to_owned(),
            install_dir: String::new(),
            steps: vec![PlannedStep {
                label: "等注册".to_owned(),
                program: PlanProgram::WaitRegistered,
                args: vec!["X".to_owned()],
                cancellable: true,
            retry_other_source: false,
            }],
            notes: Vec::new(),
        };

        let opts = RunOptions::new(|_| {});
        opts.cancel.cancel();

        let wsl = Wsl::with_program("definitely-not-a-real-binary");
        let summary = run_plan(&wsl, &plan, opts);
        assert!(!summary.ok);
        assert!(summary.cancelled, "{summary:?}");
    }

    #[test]
    fn a_plan_built_from_a_real_spec_has_the_documented_shape() {
        // 执行器依赖"计划里的参数位置"（`arg(step, 0)` 那些），
        // 所以这里把"计划 → 参数位置"这件事连起来钉一次：
        // 纯逻辑那侧的测试管参数内容，这条管它们的位置对得上。
        let ctx = PlanContext {
            default_dir: Some(r"D:\wsl".to_owned()),
            temp_dir: r"C:\Temp\wslc-panel".to_owned(),
            stamp: "1-2".to_owned(),
            wslconfig_sparse: false,
        };
        let spec = InstallSpec::new(
            "MyUbuntu",
            InstallSource::Online {
                id: "Ubuntu-24.04".to_owned(),
                launch: false,
                web_download: false,
            },
        );
        let plan = crate::model::install::plan(&spec, &ctx).unwrap();

        let relocate = plan
            .steps
            .iter()
            .find(|s| s.program == PlanProgram::EnsureRelocated)
            .expect("改名时必须有重定位那一步");
        assert_eq!(arg(relocate, 0).unwrap(), "Ubuntu-24.04");
        assert_eq!(arg(relocate, 1).unwrap(), "MyUbuntu");
        assert_eq!(arg(relocate, 2).unwrap(), r"D:\wsl\MyUbuntu");
        assert!(arg(relocate, 3).unwrap().ends_with(".tar"));

        let wait = plan
            .steps
            .iter()
            .find(|s| s.program == PlanProgram::WaitRegistered)
            .expect("在线安装要等注册");
        assert_eq!(arg(wait, 0).unwrap(), "Ubuntu-24.04");
    }
}
