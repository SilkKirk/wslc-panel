//! 下载：起 `curl.exe`，把**产物大小**当进度。
//!
//! 为什么进度不看 `curl` 自己的进度条：它是 `\r` 刷新的，按行读会攒成一整行
//! （见 `install.rs` 顶部的说明）。这里轮询文件大小，数字真实且连续。
//!
//! 读输出、轮询结束、取消那一簇在 `stream.rs`（下载只是它的一个调用方）。
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
