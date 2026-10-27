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

/// `-q` 必须在**最前**：curl 默认会读用户的 `_curlrc` / `.curlrc`，
/// 里面的 `--proxy` / `-K` / `--output` 能改变请求目的地或落盘位置。
/// 我们只想要一条"取这个 URL"的命令，配置文件的隐式行为不应参与。
///
/// # 为什么是纯函数而不是就地拼
///
/// 这条约束**错了不会有任何编译错误**（`-q` 夹在中间照样能跑，只是配置文件
/// 静默生效了），而触发它的方式是"调用方传了一串带了 `-q` 的参数" ——
/// 那种事只会在某次拼参数改动之后才出现。所以把它提成 [`with_quiet_first`]，
/// 让"最前 + 只一份"这条能用断言钉住。
///
/// 返回 `Vec<&str>`（借用 `args`）：`run_helper_with_hint` 收的就是 `&[&str]`，
/// 多一层 `to_owned()` 只会多一次分配、再被 `to_owned_args` 复制一遍。
fn with_quiet_first<'a>(args: &[&'a str]) -> Vec<&'a str> {
    let mut out = Vec::with_capacity(args.len() + 1);
    // 永远是**第一个**。curl 只认第一个 `-q`，后面的 `--config` 照样生效，
    // 所以"调用方已经带了就不加"那种写法（`if !args.contains(&"-q")`）
    // 在 `-q` 位于中间时会让这条承诺静默失效。
    out.push("-q");
    // 调用方自己带的 `-q` 滤掉（避免重复；位置也不对，留着只会误导读代码的人）。
    out.extend(args.iter().filter(|arg| **arg != "-q").copied());
    out
}

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
    let with_quiet = with_quiet_first(args);
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

    /// ⚠️ `-q` 必须**第一个**，而且**只有一份**。
    ///
    /// 这条错了没有任何编译错误 —— `-q` 夹在中间时 curl 照样跑，只是用户的
    /// `_curlrc` 静默生效了（`--proxy` / `--output` 都能改请求目的地或落盘位置）。
    /// 所以规则只能用断言钉。
    #[test]
    fn quiet_flag_is_always_first_and_only_once() {
        // ① 调用方不带 `-q`：我们补在最前
        let plain = with_quiet_first(&["-sS", "-L", "https://x"]);
        assert_eq!(plain.first().copied(), Some("-q"), "{plain:?}");
        assert_eq!(
            plain,
            vec!["-q", "-sS", "-L", "https://x"],
            "其余参数顺序不能变"
        );

        // ② 调用方**自带** `-q` 但在中间：必须被提到最前，且不能留两份。
        //    这是旧写法 `if !args.contains(&"-q")` 漏掉的那种形状。
        let mid = with_quiet_first(&["-sS", "-q", "https://x"]);
        assert_eq!(mid.first().copied(), Some("-q"), "{mid:?}");
        assert_eq!(mid.iter().filter(|a| **a == "-q").count(), 1, "只该有一份");
        assert_eq!(mid, vec!["-q", "-sS", "https://x"]);

        // ③ 调用方自带 `-q` 在最后：同样被提上来
        let last = with_quiet_first(&["-sS", "https://x", "-q"]);
        assert_eq!(last.first().copied(), Some("-q"), "{last:?}");
        assert_eq!(last.iter().filter(|a| **a == "-q").count(), 1);

        // ④ 空参数：只有 `-q`
        assert_eq!(with_quiet_first(&[]), vec!["-q"]);
    }
}
