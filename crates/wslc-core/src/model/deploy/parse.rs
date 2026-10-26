//! 解析那些脚本的输出（进程判活、文件数、日志尾部）。

use super::*;

/// 进程形态的判活结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessStatus {
    /// pidfile 在、进程还活着。
    Running,
    /// pidfile 不在、或者那个 pid 已经没了。
    Stopped,
}

/// 解析 [`status_script`] 的输出。
///
/// 认不出来返回 `None`（比如容器里的 `sh` 把脚本打错了）——
/// 调用方要把它当成"读不到状态"而不是"已停止"，两者在界面上是两句话。
pub fn parse_process_status(stdout: &str) -> Option<ProcessStatus> {
    // 只看最后一行：脚本只有一条 echo，但前面可能混进别的行
    let last = stdout.lines().rev().find(|l| !l.trim().is_empty())?;
    if last.contains(STATUS_RUNNING) {
        Some(ProcessStatus::Running)
    } else if last.contains(STATUS_STOPPED) {
        Some(ProcessStatus::Stopped)
    } else {
        None
    }
}

/// 目标目录里有多少个文件；目录不存在 → `None`。
///
/// `wc -l` 的输出**左边有空格**（右对齐），所以必须 `trim` 再 parse。
pub fn parse_file_count(stdout: &str) -> Option<u64> {
    let last = stdout.lines().rev().find(|l| !l.trim().is_empty())?;
    if last.contains(NO_DIR) {
        return None;
    }
    last.trim().parse().ok()
}

/// 把 [`tail_script`] 的输出变成要给用户看的一段文本。
///
/// 哨兵在这里翻译成人话 —— 把界面文案留在 Rust 里，而不是塞进容器里跑的脚本
/// （那样以后改文案就要连脚本一起改，而且没法单测）。
pub fn render_log_output(stdout: &str) -> String {
    if stdout.contains(NO_LOG) {
        return "还没有日志文件 —— 这个应用可能还没启动过。".to_owned();
    }
    let trimmed = stdout.trim_end();
    if trimmed.is_empty() {
        return "（日志是空的 —— 应用启动了，但还没有任何输出）".to_owned();
    }
    trimmed.to_owned()
}
