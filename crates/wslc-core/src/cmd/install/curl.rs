//! 下载与流式输出：起 `curl.exe`、收它的输出、把**产物大小**当进度。
//!
//! 为什么进度不看 `curl` 自己的进度条：它是 `\r` 刷新的，按行读会攒成一整行
//! （见 `install.rs` 顶部的说明）。这里轮询文件大小，数字真实且连续。
//!
//! 与 `install.rs` 同一模块树（`install.rs` + `install/` 目录并存）。

use super::*;
/// 起一个 `curl.exe` 步骤（下载），并轮询产物大小当进度。
pub(super) fn run_curl_step(step: &PlannedStep, opts: &RunOptions) -> Result<()> {
    let out = output_path(&step.args)?;
    let (sink, last) = line_sink(opts);
    let handle =
        cli::spawn_streaming_program(CURL, CURL_LABEL, cli::CURL_NOT_FOUND_HINT, &step.args, sink)?;
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
                InstallEvent::Line(format!(
                    "临时文件 {} 删不掉（{e}），要自己清理",
                    out.display()
                )),
            ),
        }
    }

    result
}

/// 轮询到子进程结束（或取消），把结果翻译成 `Result`。
#[allow(clippy::too_many_arguments)]
pub(super) fn finish_stream(
    handle: StreamHandle,
    program: &'static str,
    args: &[String],
    last: Arc<Mutex<Vec<String>>>,
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
                let tail = last.lock().map(|guard| guard.clone()).unwrap_or_default();
                return Err(Error::NonZeroExit {
                    program,
                    args: args.join(" "),
                    code,
                    stderr: failure_detail(&tail),
                });
            }
            Ok(None) => {}
            Err(e) => {
                // `cancel()` 返回 `()`，所以不要写成 `let _ = ...`：
                // 那会让 clippy 报 `let_unit_value`，也让人误以为这里在丢弃
                // 一个 `Result`（真正需要看结果的是下面那句 `finish()`）。
                handle.cancel_token().cancel();
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

/// 造一个"记住最后几行输出、同时发给界面"的回调。
///
/// ⚠️ 记**最后几行**而不是最后一行：`wsl.exe` 失败时会把真正的原因写在前面，
/// 末尾跟一句固定的"如果此错误是意外错误，请考虑搜索现有问题…"。
/// 只留最后一行，用户（和我们）看到的就是那句废话 —— 这个坑真的踩过一次：
/// `--manage --move` 当时失败，界面上只有那句套话，查了半天才发现是
/// "装完还没稳定"。
pub(super) fn line_sink(
    opts: &RunOptions,
) -> (
    impl Fn(&str) + Send + Sync + 'static,
    Arc<Mutex<Vec<String>>>,
) {
    /// 留多少行。
    const KEEP: usize = 6;

    let events = Arc::clone(&opts.on_event);
    let tail: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let slot = Arc::clone(&tail);

    let sink = move |line: &str| {
        if let Ok(mut guard) = slot.lock() {
            if !line.trim().is_empty() {
                guard.push(line.to_owned());
                if guard.len() > KEEP {
                    guard.remove(0);
                }
            }
        }
        events(InstallEvent::Line(line.to_owned()));
    };

    (sink, tail)
}

/// 把"最后几行"拼成一句能给人看的失败原因。
///
/// 去掉 `wsl.exe` 那句固定套话（它每行都一样，且不含任何信息），
/// 剩下的按行拼起来 —— 真正的错误码（`Wsl/InstallDistro/...`）就在里面。
// 只有测试按名字用（正式构建里没有调用点）—— `AGENTS.md` §2 那类"三件套"。
#[allow(dead_code)]
pub(super) fn failure_detail(tail: &[String]) -> String {
    let meaningful: Vec<&str> = tail
        .iter()
        .map(String::as_str)
        .filter(|line| {
            let line = line.trim();
            !line.is_empty()
                && !line.contains("如果此错误是意外错误")
                && !line.contains("github.com/microsoft/WSL/issues")
        })
        .collect();

    if meaningful.is_empty() {
        "（它没有给出任何有用的输出）".to_owned()
    } else {
        meaningful.join(" / ")
    }
}

/// 从 `curl` 的参数里找出 `-o` 指的文件。
// 只有测试按名字用（正式构建里没有调用点）—— `AGENTS.md` §2 那类"三件套"。
#[allow(dead_code)]
pub(super) fn output_path(args: &[String]) -> Result<PathBuf> {
    let at = args.iter().position(|a| a == "-o").ok_or_else(|| {
        Error::Install("内部错误：下载步骤里没有 `-o`，拿不到产物路径".to_owned())
    })?;
    let path = args
        .get(at + 1)
        .ok_or_else(|| Error::Install("内部错误：下载步骤的 `-o` 后面没有路径".to_owned()))?;
    Ok(PathBuf::from(path))
}

/// 轮询产物大小的进度源。
pub(super) struct ProgressSource {
    pub(super) path: PathBuf,
    pub(super) total: Option<u64>,
    pub(super) started: Instant,
}

impl ProgressSource {
    pub(super) fn emit(&self, opts: &RunOptions) {
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
