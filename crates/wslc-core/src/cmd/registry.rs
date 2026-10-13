//! 镜像仓库（Registry V2）的**阻塞**调用：列 tag、量体积。
//!
//! 解析全在 [`crate::registry`] 里（纯函数、配真实 fixture 单测），
//! 这里只负责"拼 URL → 起 `curl.exe` → 把正文交出去"。这个拆法是仓库的既定约定
//! （`AGENTS.md` §5），先例是 [`crate::cmd::catalog`] 与 [`crate::mirrors`]。
//!
//! # 为什么走 `curl.exe`
//!
//! 本仓库**不能新增依赖**：CI 全部带 `--locked`，而本机没有 cargo、
//! 锁文件在本地没法重新生成（`AGENTS.md` §1/§3）。所以和 [`crate::mirrors`] 一样
//! 借系统自带的 `curl.exe`（实测 8.21.0）。
//!
//! ⚠️ **阻塞**：列 tag 一次请求（实测 `node` 的 9170 个 tag 是 152 KB / 2.4 秒，
//! `alpine` 2 KB / 1.3 秒），量体积两次（index + manifest）。
//! 只能在后台执行器上调用（`AGENTS.md` §7.1）。

use crate::cli;
use crate::error::Error;
use crate::mirrors;
use crate::registry::{self, ImageSize, ManifestKind};

/// 列一个仓库的 tag（**阻塞**，已排序）。
///
/// ⚠️ 调用方要先过 [`registry::split_input`]：短名（`alpine`）必须补成
/// `library/alpine`，实测 1panel.live 对短名回 404（见 [`registry::split_input`]）。
pub fn fetch_tags(host: &str, repository: &str) -> Result<Vec<String>, String> {
    // 纵深防御：仓库名会拼进 URL 再作为裸 argv 交给 curl。
    if !registry::is_valid_repository(repository) {
        return Err(format!("仓库名不合法：{repository}"));
    }
    let body = get(&registry::tags_url(host, repository))?;
    let tags = registry::parse_tags(&body)?;
    Ok(registry::sort_tags(&tags))
}

/// 量一个 tag 的**下载**体积（**阻塞**，最多两次请求）。
///
/// 两次是省不掉的：实测 `manifests/<tag>` 无视 `Accept`，多架构镜像永远
/// 返回 index，要再要一次它指向的那份才有 `layers`（见 [`registry::classify_manifest`]）。
///
/// ⚠️ 返回的是**压缩后**的下载量，不是落盘占用 —— 界面上的措辞见
/// [`registry::image_size`] 的说明。
pub fn fetch_size(host: &str, repository: &str, reference: &str) -> Result<ImageSize, String> {
    if !registry::is_valid_repository(repository) {
        return Err(format!("仓库名不合法：{repository}"));
    }
    if !registry::is_valid_reference(reference) {
        return Err(format!("引用不合法：{reference}"));
    }
    let body = get_manifest(&registry::manifest_url(host, repository, reference))?;
    match registry::classify_manifest(&body, mirrors::arch())? {
        // 单架构镜像的 manifest 直接就能量
        ManifestKind::Single => registry::image_size(&body),
        // index：再要一次它指向的那一份
        ManifestKind::Index { digest } => {
            let inner = get_manifest(&registry::manifest_url(host, repository, &digest))?;
            registry::image_size(&inner)
        }
    }
}

/// `GET` 一个 JSON 接口（不带 `Accept`）。
fn get(url: &str) -> Result<String, String> {
    run(url, registry::curl_args(url))
}

/// `GET` 一个 manifest（带 `Accept`）。
fn get_manifest(url: &str) -> Result<String, String> {
    run(url, registry::manifest_args(url))
}

/// 起一次 `curl.exe`，正文从 stdout 出来。
///
/// # 错误是从哪儿来的
///
/// `curl` 的参数里带了 `--fail-with-body`，所以 HTTP 4xx/5xx 时它是
/// **退出码非 0、但正文照给**（实测 404 → 退出码 22 + stdout 是错误信封）。
/// 于是：
///
/// - "失败了"看 `code`；
/// - "为什么失败"看正文里的 V2 错误信封（[`registry::explain_failure`] 会先看它，
///   拿不到再退回 `curl` 自己的话）。
///
/// 这样就不用去猜状态码，也不用为了拿状态码去解析 `-w` 的输出。
fn run(url: &str, args: Vec<String>) -> Result<String, String> {
    // 纵深防御：这个字符串会作为**裸 argv** 交给 `curl.exe`
    // （一个以 `-` 开头的"URL"会被 curl 当成选项，`-K<file>` 能变成任意下载写盘，
    // 见 `registry::curl_args` 的说明）。调用方全都走拼 URL 的那几个函数，
    // 但常量改了没人拦，在这里再判一次成本为零。
    if !mirrors::is_http_url(url) {
        return Err(format!("不是 http(s) 地址，已拒绝调用 curl：{url}"));
    }
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    match cli::run_helper_with_hint(
        "curl.exe",
        "curl.exe",
        cli::CURL_NOT_FOUND_HINT,
        &refs,
        registry::http_timeout(),
    ) {
        Ok(out) if out.success() => Ok(out.stdout),
        Ok(out) => Err(registry::explain_failure(&out.stdout, &out.stderr)),
        Err(e) => Err(match e {
            // 超时那条的 `Display` 会把整条 curl 参数拼进消息里（几十个字符），
            // 给用户看没有意义 —— 换成一句能行动的。
            Error::Timeout { .. } => {
                "registry 在 35 秒内没有响应：等一会儿再试，或者换一个仓库/tag".to_owned()
            }
            other => other.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_a_bad_repository_before_spawning_curl() {
        // 这一条是**安全边界**：坏名字不该有机会到 curl 那儿。
        // 用一个不存在的域名也照样能测 —— 因为根本起不了进程。
        for bad in ["", "../etc", "a//b", "-o/tmp/x"] {
            let err = fetch_tags("no-such-host.invalid", bad).unwrap_err();
            assert!(err.contains("仓库名不合法"), "{bad:?} → {err}");
        }
        for bad in ["", "-o/tmp/x", "bad tag"] {
            let err = fetch_size("no-such-host.invalid", "library/alpine", bad).unwrap_err();
            assert!(err.contains("不合法"), "{bad:?} → {err}");
        }
    }

    #[test]
    fn refuses_a_non_http_url_before_spawning_curl() {
        // `run` 是唯一真正起进程的地方，这个判据要在那儿也成立。
        for bad in ["ftp://x/y", "file:///C:/Windows/win.ini", "-K\\\\a\\b"] {
            let err = run(bad, vec![bad.to_owned()]).unwrap_err();
            assert!(err.contains("已拒绝调用"), "{bad:?} → {err}");
        }
    }

    #[test]
    fn the_url_we_would_call_is_the_one_the_pure_layer_builds() {
        // 钉住"这条路上没有第二份 URL 拼装"：fetch_* 用的就是 registry 里那两个函数。
        let url = registry::tags_url(registry::DEFAULT_REGISTRY, "library/alpine");
        assert_eq!(
            url,
            "https://docker.1panel.live/v2/library/alpine/tags/list"
        );
        assert!(mirrors::is_http_url(&url));
    }
}
