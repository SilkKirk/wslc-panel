//! `curl.exe` 的公共入口：URL 校验 + 起进程 + 超时。
//!
//! 项目里有五处要拿 HTTP（在线发行版清单、兜底 JSON、镜像探测、registry、
//! 更新检查）。它们各自解释响应，但"URL 会作为裸 argv 交给 curl，所以必须
//! 先挡掉 `-K` / `file://` 这类东西"和"起进程时统一带 `CREATE_NO_WINDOW`
//! + 超时"是同一件事 —— 只该有一份实现。
//!
//! # 这里**不**解释 HTTP 状态
//!
//! `curl` 带 `--fail-with-body` 时 4xx 也是非零退出码但正文照给。registry
//! 要读错误信封、update 要读限流原因、catalog 只要正文 —— 解释留给调用方。
//! 所以本函数成功返回的是"进程跑完了"（`CommandOutput`），不是"HTTP 200"。
//!
//! ⚠️ **阻塞**：只能在后台执行器上调用（`AGENTS.md` §7.1）。

use std::time::Duration;

use crate::cli::{self, CommandOutput};
use crate::error::{Error, Result};
use crate::mirrors;

/// 校验 `url` 并起一次 `curl.exe`，参数按原样传（调用方负责拼 `-sS` / `-m` 等）。
///
/// 返回 `Ok` 不代表 HTTP 成功；调用方要看 `CommandOutput::success()` 或正文。
pub(crate) fn curl(url: &str, args: &[&str], timeout: Duration) -> Result<CommandOutput> {
    // 纵深防御：URL 是裸 argv，一个以 `-` 开头的字符串会被 curl 当成选项
    // （`-K<file>` 能变成任意下载写盘）。调用方拼 URL 时已经挡过一次，
    // 但常量改了没人拦 —— 在这里再判一次成本为零。
    if !mirrors::is_http_url(url) {
        return Err(Error::InvalidArgument(format!(
            "不是 http(s) 地址，已拒绝调用 curl：{url}"
        )));
    }
    // `-q` 必须放在最前：curl 默认会读用户的 `_curlrc` / `.curlrc`，
    // 里面的 `--proxy` / `-K` / `--output` 能改变请求目的地或落盘位置。
    // 我们只想要一条"取这个 URL"的命令，配置文件的隐式行为不应参与。
    let mut with_quiet = Vec::with_capacity(args.len() + 1);
    if !args.contains(&"-q") {
        with_quiet.push("-q");
    }
    with_quiet.extend_from_slice(args);
    cli::run_helper_with_hint(
        "curl.exe",
        "curl.exe",
        cli::CURL_NOT_FOUND_HINT,
        &with_quiet,
        timeout,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_a_non_http_url_before_spawning_curl() {
        // 用不存在的域名也测不出区别：这个判据在起进程之前。
        for bad in ["ftp://x/y", "file:///C:/Windows/win.ini", "-K\\\\a\\\\b"] {
            let err =
                curl(bad, &[bad], Duration::from_secs(1)).expect_err("非法 URL 不该有机会到 curl");
            assert!(err.to_string().contains("已拒绝调用"), "{err}");
        }
    }
}
