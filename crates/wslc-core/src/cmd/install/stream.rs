//! 流式输出、取消与临时文件：读子进程输出、轮询它结束、可取消的睡眠、建目录 / 删文件。
//!
//! 这一簇的共同点是**它们都围着"一个正在跑的东西"转**，而不是围着某一条命令：
//! `curl` 下载、`wsl.exe` 安装、`--move` 重试用的是同一套读取与取消，
//! 差别只在"进度看什么"（下载看产物大小，其余不看）。
//!
//! 与 `install.rs` 同一模块树（`install.rs` + `install/` 目录并存），
//! 所以 `use super::*;` 就能拿到事件类型、`emit`、`arg` 和那几个常量。

// 进度源定义在下载那一段（它只对下载有意义），但读取循环要能拿到它。
use super::{curl::ProgressSource, *};

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
///
/// **不是**"只有测试按名字用"：正式路径在上面 [`finish_stream`] 的
/// `Err(Error::NonZeroExit { stderr: failure_detail(&tail), .. })` 里。
/// 早先那句 allow 的注释是错的（照抄了别处的模板），已删 ——
/// 多余的 `#[allow(dead_code)]` 会掩盖真正的死代码。
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

/// 分段睡眠，顺便响应取消（睡 2 秒里点了取消，不用等到睡完）。
pub(super) fn sleep_cancellable(total: Duration, opts: &RunOptions) {
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

/// 建安装目录。已存在**不算错**（`create_dir_all` 的语义）。
pub(super) fn create_dir(step: &PlannedStep) -> Result<()> {
    let dir = arg(step, 0)?;
    std::fs::create_dir_all(dir).map_err(|e| Error::Install(format!("建不出安装目录 {dir}：{e}")))
}

/// 删文件。**文件本来就不在也算成功** —— 这是收尾步骤，不该因此判定失败。
pub(super) fn remove_file(step: &PlannedStep) -> Result<()> {
    let path = arg(step, 0)?;
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::Install(format!("删不掉临时文件 {path}：{e}"))),
    }
}
