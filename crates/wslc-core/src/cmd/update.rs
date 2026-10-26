//! 更新检查的**阻塞**那一半：起一次 `curl.exe`。
//!
//! 解析在 [`crate::update`]，这里只干"拼参数 → 跑命令 → 把正文交出去"，
//! 和 [`crate::cmd::registry`] / [`crate::cmd::catalog`] 是同一个形状
//! （`AGENTS.md` §5：解析放纯逻辑层，跑进程放 `cmd/`）。
//!
//! ⚠️ **阻塞**（实测 ~2 s：api.github.com 2.1 秒、响应 6.6 KB）。
//! 只能在后台执行器上调用（`AGENTS.md` §7.1）。
//!
//! 本版**不做下载** —— 用户自己在发布页取文件。所以这里没有进度、没有取消、
//! 没有断点续传，一个函数就够了。

use std::time::Duration;

// 逐个导入而不是 `use crate::update::{self, …}`：**本模块就叫 `update`**，
// 再把 `crate::update` 整个导成 `update` 会让读者分不清 `update::api_url()`
// 指的是哪一层。名字写全反而省事。
use crate::update::{Release, api_url, curl_args, explain_failure, parse_release};

/// 整条命令最多等多久。
///
/// `curl` 自己带 `-m 15`；这里多给 10 秒是留给"起进程 + 读完输出"的余量。
/// 两个都留：`curl` 的超时管网络，这里的超时管子进程本身卡住（例如被 EDR 拦住）。
const TIMEOUT: Duration = Duration::from_secs(25);

/// 拉一次 `releases/latest` 并解析。**阻塞**。
///
/// 返回值里的错误都是"能给用户看的一句话"：配额用完、仓库没有 release、
/// 连不上、返回的不是 JSON…… 界面直接显示它。
pub fn check() -> Result<Release, String> {
    let url = api_url();
    let args = curl_args(&url);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();

    match super::http::curl(&url, &refs, TIMEOUT) {
        Ok(out) if out.success() => parse_release(&out.stdout),

        // ⚠️ 这里**不能**只给 `out.combined()`：它优先返回 stderr，而 403 的
        // **原因**（`API rate limit exceeded`）在 stdout 的正文里。
        // 两条都拼上，`explain_failure` 才认得出来是哪一种失败。
        Ok(out) => Err(explain_failure(
            out.code,
            &format!("{}\n{}", out.stdout, out.stderr),
        )),

        // 超时那条的 `Display` 会把整条 curl 参数拼进消息里（十几个选项），
        // 给用户看没有意义 —— 换成一句能行动的。和 `cmd::registry` 的处理一致。
        Err(crate::error::Error::Timeout { .. }) => {
            Err("检查更新超时（25 秒）：网络太慢或者被拦住了。".to_owned())
        }
        Err(e) => Err(e.to_string()),
    }
}
