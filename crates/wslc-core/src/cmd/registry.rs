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

use crate::error::Error;
use crate::mirrors;
use crate::registry::{self, ImageSize, ManifestKind};

/// 列一个仓库的 tag（**阻塞**，已排序）。
///
/// ⚠️ 调用方要先过 [`registry::split_input`]：短名（`alpine`）必须补成
/// `library/alpine`，实测 1panel.live 对短名回 404（见 [`registry::split_input`]）。
///
/// # 为什么一次把全量拿回来（不分页）
///
/// 1. 接口的分页（`Link: </v2/…?last=…&n=5>`）要引入"翻页游标 + 增量状态"
///    一整套新状态，而它省下的只有二十几 KB（实测 `node` 的 **9170** 个 tag
///    一共 152 KB / 2.4 秒）；
/// 2. 界面那边是**本地**过滤：v0.6.2 换成可搜索下拉框之后，过滤由组件自己做
///    （它的列表是虚拟化的，只有可见的几行会被建出来），比翻页快得多 ——
///    翻页过滤每敲一个字都要等一次网络。
pub fn fetch_tags(host: &str, repository: &str) -> Result<Vec<String>, String> {
    // 纵深防御：registry 地址与仓库名都会拼进 URL，再作为裸 argv 交给 curl。
    // v0.6.2 起 host 来自输入框，这条判据从"常量用不着校验"变成了必需品。
    let host = registry::normalize_host(host);
    if !registry::is_valid_host(&host) {
        return Err(format!("registry 地址不合法：{host}"));
    }
    if !registry::is_valid_repository(repository) {
        return Err(format!("仓库名不合法：{repository}"));
    }
    let body = get(&registry::tags_url(&host, repository))?;
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
    // 同 `fetch_tags`：host 现在来自输入框，必须在这里挡一次。
    let host = registry::normalize_host(host);
    if !registry::is_valid_host(&host) {
        return Err(format!("registry 地址不合法：{host}"));
    }
    if !registry::is_valid_repository(repository) {
        return Err(format!("仓库名不合法：{repository}"));
    }
    if !registry::is_valid_reference(reference) {
        return Err(format!("引用不合法：{reference}"));
    }
    let body = get_manifest(&registry::manifest_url(&host, repository, reference))?;
    match registry::classify_manifest(&body, mirrors::arch())? {
        // 单架构镜像的 manifest 直接就能量
        ManifestKind::Single => registry::image_size(&body),
        // index：再要一次它指向的那一份
        ManifestKind::Index { digest } => {
            let inner = get_manifest(&registry::manifest_url(&host, repository, &digest))?;
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
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    match super::http::curl(url, &refs, registry::http_timeout()) {
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
    fn rejects_a_bad_registry_host_before_spawning_curl() {
        // v0.6.2：host 来自输入框，所以"坏地址"必须在起进程**之前**被挡住。
        // 用一个不存在的域名/path 都行 —— 根本起不了进程。
        for bad in [
            "",
            "   ",
            "bad host",
            "docker.1panel.live/v2",
            "-K/tmp/x",
            "host:port",
            "host:99999",
        ] {
            let err = fetch_tags(bad, "library/alpine").unwrap_err();
            assert!(err.contains("registry 地址不合法"), "{bad:?} → {err}");
            let err = fetch_size(bad, "library/alpine", "latest").unwrap_err();
            assert!(err.contains("registry 地址不合法"), "{bad:?} → {err}");
        }
    }

    #[test]
    fn a_normalized_host_is_what_reaches_curl() {
        // 用户在输入框里粘一整条 URL 是常态：收拾干净之后再拼 URL，
        // 别拼出 `https://https://…`。
        // （这里只看"能不能过校验并走到拼 URL 那一步"，不起进程 ——
        // 用不合法的仓库名把它挡在 curl 之前。）
        let err = fetch_tags("https://docker.1panel.live/", "../etc").unwrap_err();
        assert!(err.contains("仓库名不合法"), "{err}");
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
