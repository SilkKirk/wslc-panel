//! 容器镜像仓库（Registry V2）的**纯逻辑**。
//!
//! 起进程的那一半在 [`crate::cmd::registry`]，这里全是能脱离 Windows 跑单测的纯函数。
//! 拆法是仓库的既定约定（`AGENTS.md` §5），先例是 [`crate::mirrors`]。
//!
//! # 这一版**只有一个** registry
//!
//! `docker.1panel.live`。不是因为它"最好"，是因为实测下来它是唯一一个
//! **连 manifest 都免认证**的（2026-10-13 本机实测）：
//!
//! | 站点 | `tags/list` | `manifests/<tag>` |
//! |---|---|---|
//! | `docker.1panel.live` | 200 | **200（免认证）** |
//! | `docker.1ms.run` | 200 | 401 → 要它自己域名下的 Bearer token |
//! | `docker.m.daocloud.io` | 401 | 401（realm 在 `m.daocloud.io`） |
//! | `registry-1.docker.io` / `auth.docker.io` / `hub.docker.com` | **000（不可达）** | **000** |
//!
//! ⚠️ 所以这一版**没有 token / 401 挑战那套代码**。换成别的站就必须补上，
//! 补的位置是 [`crate::cmd::registry`] 里那个发请求的函数（收到 401 时先取 token
//! 再重试；token 响应的字段名实测是 `access_token` 而不是 `token`）。
//! **别把"没有认证代码"读成"registry 不需要认证"。**
//!
//! # Docker Hub 的官方接口一个都用不了
//!
//! 实测三个域名全是 000。所以**搜索、星数、官方标识、按更新时间排 tag**
//! 这些"镜像搜索界面"的常规物料一个都拿不到 —— 界面据此只能说清
//! "这里是某个 registry 的某个仓库的 tag 列表"，不假装有搜索。
//!
//! # 算体积**必然**是两次请求
//!
//! 实测 `manifests/latest` **无视 `Accept`**：三种 Accept（只 docker v2 / 只 oci /
//! 两者都收）拿到的是**同一个** 9218 B 的 OCI image index。内容协商省不掉那一次，
//! 所以：index → 挑出本机架构那一份 → 再要一次 → 累加 `layers[].size`。
//!
//! ⚠️ 挑的时候**必须同时看 `architecture` 和 `os`**：实测 hello-world 的 index 里
//! 有**两条 `amd64`** —— 一条 `linux`、两条 `windows`。只看架构就会挑到 Windows 那份。
//! 而且每条真平台后面都跟一条
//! `platform: {architecture: "unknown", os: "unknown"}` 的 **attestation-manifest**
//! （alpine 的 16 条里 8 条是它），挑"第一条"必然挑到它。

use crate::mirrors::BROWSER_UA;

/// 内置 registry —— 默认值（`RegistryState::new` 拿它填输入框）。
pub const DEFAULT_REGISTRY: &str = "docker.1panel.live";

/// 仓库名输入框下面的快捷入口。
///
/// **为什么需要它**：这一版**没有搜索接口**（见模块说明），
/// 一上来给一个空输入框等于把"该输什么"整个推给用户。
///
/// 每一项都在 `docker.1panel.live` 上实测过 200（2026-10-13），
/// 不是从文档抄的 —— 界面上的快捷按钮点下去 404 是最难解释的一种。
/// 刻意只放 10 个：`PLAN-v0.5.md` §4.4 记着"几十个按钮铺满一屏"是已经被
/// 否掉的做法（那次的结论是改用下拉框）。
pub const COMMON_IMAGES: &[&str] = &[
    "nginx",
    "redis",
    "mysql",
    "postgres",
    "mongo",
    "alpine",
    "ubuntu",
    "node",
    "python",
    "hello-world",
];

/// 快捷入口**不能又变成"几十个按钮铺满一屏"** —— 那正是 `PLAN-v0.5.md` §4.4
/// 记着已经被否掉的做法（那次的结论是改用下拉框）。现在 10 个，
/// 超过 12 个就该换成下拉框了，所以在这里编译期拦一下。
const _: () = assert!(COMMON_IMAGES.len() <= 12);

/// 一次 manifest 请求的 `Accept`。
///
/// 实测加不加它结果一样（见模块说明），但**仍然要带**：不带的话某些 registry
/// 会回一个 schema 1 的老格式，那份没有 `layers`。带上成本为零。
pub const MANIFEST_ACCEPT: &str = concat!(
    "application/vnd.docker.distribution.manifest.v2+json,",
    "application/vnd.oci.image.manifest.v1+json,",
    "application/vnd.oci.image.index.v1+json,",
    "application/vnd.docker.distribution.manifest.list.v2+json"
);

// ---------------------------------------------------------------- URL 拼装

/// `GET /v2/<repo>/tags/list`。
///
/// ⚠️ `repository` 会被**直接拼进 URL**，而这个 URL 最终作为**裸 argv** 交给
/// `curl.exe`。调用方必须先过 [`is_valid_repository`] —— 判据只有那一处。
pub fn tags_url(host: &str, repository: &str) -> String {
    format!(
        "https://{}/v2/{}/tags/list",
        host.trim().trim_end_matches('/'),
        repository
    )
}

/// `GET /v2/<repo>/manifests/<reference>`。
///
/// `reference` 是 tag（`latest`）或摘要（`sha256:…`，从 index 里拿到的那个）。
/// 同 [`tags_url`]：调用方先过 [`is_valid_reference`]。
pub fn manifest_url(host: &str, repository: &str, reference: &str) -> String {
    format!(
        "https://{}/v2/{}/manifests/{}",
        host.trim().trim_end_matches('/'),
        repository,
        reference
    )
}

/// 交给 `wslc pull` 的完整引用。
///
/// ⚠️ **必须带 registry 前缀**。实测本机 `registry-1.docker.io` 不可达，
/// 所以 `wslc pull alpine:latest` 会去 Docker Hub 找然后超时 ——
/// 而 `wslc pull docker.1panel.live/library/alpine:latest` 是通的（实测 7 秒拉完
/// 一个 hello-world）。拉下来的镜像名也就是这一整串，与「镜像」页显示的一致。
pub fn pull_reference(host: &str, repository: &str, tag: &str) -> String {
    format!(
        "{}/{}:{}",
        host.trim().trim_end_matches('/'),
        repository,
        tag
    )
}

// ---------------------------------------------------------------- 输入规范化

/// 把用户在输入框里写的东西拆成（仓库名, 可选的 tag）。
///
/// 这不是"格式校验"，是**双层**的东西：既是体验（用户会粘贴各种形状），
/// 也是**安全边界**（返回值会拼进 URL 再交给 `curl.exe`，见 [`tags_url`]）。
///
/// 实测这些形状都要认（`docker.1panel.live` 上验过）：
///
/// | 输入 | 结果 |
/// |---|---|
/// | `alpine` | `library/alpine` |
/// | `NGINX` | `library/nginx`（统一小写 —— registry 要求仓库名小写） |
/// | `library/nginx` | 原样 |
/// | `grafana/grafana` | 原样（非 `library/` 的命名空间是合法的，实测 200） |
/// | `docker.1panel.live/library/nginx` | `library/nginx`（把 registry 前缀剥掉） |
/// | `nginx:1.27` | (`library/nginx`, `1.27`) |
/// | `https://docker.1panel.live/v2/library/alpine/tags/list` | `library/alpine` |
///
/// ⚠️ **短名必须我们补 `library/`，不能指望 registry**：实测
/// `docker.1ms.run/v2/alpine/tags/list` 会自己补（还回 `name: "library/alpine"`），
/// 而 `docker.1panel.live/v2/alpine/tags/list` **404 `NAME_UNKNOWN`**。
/// 同一份界面在两台站上一台能用一台不能 —— 所以判据只有这一处。
pub fn split_input(input: &str) -> Result<(String, Option<String>), String> {
    let mut text = input.trim();
    if text.is_empty() {
        return Err("仓库名不能为空".to_owned());
    }
    if text.contains('@') {
        // 按摘要拉取这一版不做：界面上本来就只能选一个 tag。
        // 与其半支持，不如说清。
        return Err("这一版不支持按摘要（@sha256:…）拉取，请在下面选一个 tag".to_owned());
    }

    // 用户很可能把整个 URL 粘进来（浏览器里就是这么复制的），所以先把
    // 接口路径剥掉：`/v2/` 之前的是主机名，之后才是仓库。
    // ⚠️ 顺序很要紧 —— `/v2/` 在**主机名之后**，先剥主机名就找不到它了。
    for prefix in ["https://", "http://"] {
        if let Some(rest) = text.strip_prefix(prefix) {
            text = rest;
            break;
        }
    }
    if let Some(at) = text.find("/v2/") {
        text = &text[at + "/v2/".len()..];
    } else if let Some(rest) = text.strip_prefix("v2/") {
        text = rest;
    }

    // 尾部的接口路径同样剥掉：`/tags/list`，或 `/manifests/<ref>`（ref 就是 tag）。
    let mut tag_from_path: Option<String> = None;
    if let Some(rest) = text.strip_suffix("/tags/list") {
        text = rest;
    } else if let Some(at) = text.find("/manifests/") {
        let reference = text[at + "/manifests/".len()..].trim_matches('/');
        if !reference.is_empty() {
            tag_from_path = Some(reference.to_owned());
        }
        text = &text[..at];
    }

    // 以最后一个 '/' 为界找 tag：只有冒号出现在**最后一段**里才算 tag，
    // 否则 `registry:5000/x` 里的端口号会被当成 tag。
    let last_slash = text.rfind('/');
    let mut name = text;
    let mut tag: Option<String> = None;
    if let Some(colon) = text.rfind(':') {
        let after_slash = last_slash.map_or(true, |slash| colon > slash);
        if after_slash {
            name = &text[..colon];
            let raw = text[colon + 1..].trim();
            if raw.is_empty() {
                return Err("冒号后面没有 tag".to_owned());
            }
            tag = Some(raw.to_owned());
        }
    }

    let mut name = name.trim_matches('/');
    if name.is_empty() {
        return Err("仓库名不能为空".to_owned());
    }

    // 第一段带 `.` / `:`，或者就叫 `localhost` —— 按 Docker 的规矩那是 registry
    // 地址，不是命名空间。这一版只连一个 registry，所以直接剥掉。
    if let Some(slash) = name.find('/') {
        let first = &name[..slash];
        if first.contains('.') || first.contains(':') || first.eq_ignore_ascii_case("localhost") {
            name = &name[slash + 1..];
        }
    }

    let name = name.to_lowercase();
    if name.is_empty() {
        return Err("仓库名不能为空".to_owned());
    }
    // 单段（`alpine`）按官方镜像算 —— 这是 Docker Hub 的既定约定。
    let repository = if name.contains('/') {
        name
    } else {
        format!("library/{name}")
    };

    if !is_valid_repository(&repository) {
        return Err(format!(
            "仓库名不合法：{repository}（只允许小写字母、数字，以及 . _ - / ，且不能以符号开头或结尾）"
        ));
    }
    // 路径里带出来的 tag（`/manifests/1.27`）优先于冒号切出来的那个 ——
    // 两者只可能有一个存在，这里只是把两条路合成一个出口。
    let tag = tag.or(tag_from_path);
    if let Some(tag) = tag.as_ref() {
        if !is_valid_reference(tag) {
            return Err(format!("tag 不合法：{tag}"));
        }
    }
    Ok((repository, tag))
}

/// 仓库名合法吗（`alpine` 这种短名要先过 [`split_input`] 补成 `library/alpine`）。
///
/// 规则取 Docker 那套的一个**安全子集**：`/` 分段，每段由小写字母数字与
/// `.` `_` `-` 组成，且**必须以字母数字开头和结尾**。
///
/// 这一条是**安全边界**而不是好看：返回值会拼进 URL 再交给 `curl.exe` 当裸 argv
/// （判据只有这一处，同 [`crate::mirrors::is_http_url`] 的理由）。它顺带挡掉了
/// `..`（路径穿越）、空段（`a//b`）、`%`（URL 编码）、`?` `#`（查询与片段）
/// 以及一切非 ASCII。
///
/// 比 Docker 的正则**略宽**：允许连续分隔符（`my--repo`）。
/// 宽一点的代价只是"某个畸形名字多跑一次请求、然后 404"，
/// 而窄一点会把用户手里合法的镜像名挡在外面 —— 后者更糟。
pub fn is_valid_repository(repository: &str) -> bool {
    if repository.is_empty() || repository.len() > 255 {
        return false;
    }
    if repository.starts_with('/') || repository.ends_with('/') {
        return false;
    }
    repository.split('/').all(is_valid_segment)
}

/// 一段（`/` 之间）合法吗。
fn is_valid_segment(segment: &str) -> bool {
    if segment.is_empty() {
        return false;
    }
    let first = match segment.chars().next() {
        Some(ch) => ch,
        None => return false,
    };
    // 首尾必须是字母数字：`-x` / `x-` / `.x` / `x.` / `..` 一律不收。
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    let last = segment.chars().last().unwrap_or(first);
    if !last.is_ascii_alphanumeric() {
        return false;
    }
    segment
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '.' | '_' | '-'))
}

/// tag / 摘要合法吗。
///
/// tag 的字符集是 Docker 定的：`[A-Za-z0-9_][A-Za-z0-9._-]{0,127}`
/// （tag **可以有大写** —— 和仓库名不一样）。
/// 摘要走 `sha256:<十六进制>` 那条形状，长度上限另算。
pub fn is_valid_reference(reference: &str) -> bool {
    if reference.is_empty() || reference.len() > 256 {
        return false;
    }
    if let Some((algorithm, hex)) = reference.split_once(':') {
        // 摘要：算法名 + 一段够长的十六进制。index 里给的就是这个形状。
        let algorithm_ok = !algorithm.is_empty()
            && algorithm.chars().all(|ch| {
                ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '_' | '-' | '+')
            });
        if algorithm_ok && hex.len() >= 32 && hex.chars().all(|ch| ch.is_ascii_hexdigit()) {
            return true;
        }
        // 冒号出现在 tag 里不算摘要 —— 落到下面按 tag 再判一次（会被字符集挡住）。
    }
    if reference.len() > 128 {
        return false;
    }
    let first = match reference.chars().next() {
        Some(ch) => ch,
        None => return false,
    };
    if !(first.is_ascii_alphanumeric() || first == '_') {
        return false;
    }
    reference
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-'))
}

/// registry 主机名（可带端口）合法吗。
///
/// 和 [`is_valid_repository`] 一样，这一条是**安全边界**而不是好看：`host` 会拼进
/// URL（[`tags_url`] / [`manifest_url`]）再作为裸 argv 交给 `curl.exe`
/// （见 [`curl_args`] 的说明）。区别是它**现在是用户可编辑的** ——
/// v0.6.2 合并「镜像 / 镜像仓库」两页时把 registry 提成了输入框，
/// 因为不这么做就会丢掉"从别的 registry 拉"这个能力（`docs/PLAN-v0.6.md` §3.6）。
/// 常量用不着校验，输入框必须校验。
///
/// 规则按 Docker 的 registry 主机名：`[A-Za-z0-9.-]`，可选 `:端口`，
/// 每个 label 首尾必须是字母数字。刻意**不收** `/` `?` `#` `%` 空格和一切非 ASCII：
/// 那些要么是路径/查询（我们要的是纯主机名），要么会让 URL 变形。
pub fn is_valid_host(host: &str) -> bool {
    let host = host.trim();
    if host.is_empty() || host.len() > 253 {
        return false;
    }
    // 端口可选，先拆掉再判主机名。`rsplit_once` 而不是 `split_once`：
    // IPv6 字面量（`[::1]`）本来就不在支持范围内，用最后一个冒号更不容易误判。
    let (name, port) = match host.rsplit_once(':') {
        Some((name, port)) => (name, Some(port)),
        None => (host, None),
    };
    if let Some(port) = port {
        // 端口必须是**数字且 ≤ 65535**：`host:port` 这种占位符要在这里挡住，
        // 否则它会变成一个"永远超时"的地址，用户完全不知道为什么。
        // 用 `contains` 而不是 `>= 1 && <= 65535`：后者 clippy 会报
        // `manual_range_contains`（lint 任务不阻塞，但没必要留一条）。
        match port.parse::<u32>() {
            Ok(number) if (1..=65535).contains(&number) => {}
            _ => return false,
        }
    }
    if name.is_empty() {
        return false;
    }
    name.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && label.chars().next().is_some_and(|ch| ch.is_ascii_alphanumeric())
            && label.chars().last().is_some_and(|ch| ch.is_ascii_alphanumeric())
            && label
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
    })
}

/// 把用户填的 registry 地址收拾成纯主机名：去空白、去 `http(s)://`、去结尾 `/`、
/// 统一小写。
///
/// **为什么需要**：那个输入框旁边就写着"可以是别的 registry"，
/// 而用户从浏览器地址栏复制过来十有八九带着协议头或结尾斜杠
/// （[`split_input`] 认 `https://…/v2/…/tags/list` 那种形状也是同一个理由）。
/// 这里只做**形状**上的收拾，不查存在性、也不判合法性 —— 合法性由
/// [`is_valid_host`] 负责，两件事分开才不会互相掩护。
pub fn normalize_host(input: &str) -> String {
    let host = input.trim();
    let host = host
        .strip_prefix("https://")
        .or_else(|| host.strip_prefix("http://"))
        .unwrap_or(host);
    host.trim_end_matches('/').trim().to_ascii_lowercase()
}

// ---------------------------------------------------------------- 响应解析

/// JSON 正文 → `Value`。空正文单独报一句（它不是"JSON 坏了"）。
fn parse_json(json: &str) -> Result<serde_json::Value, String> {
    let clean = json.trim_start_matches('\u{feff}').trim();
    if clean.is_empty() {
        return Err("registry 返回了空内容".to_owned());
    }
    serde_json::from_str(clean).map_err(|e| format!("registry 返回的不是 JSON：{e}"))
}

/// Registry V2 的错误信封 `{"errors":[{"code","message","detail"}]}` → 人话。
///
/// 实测这个信封**在 404 / 403 上都会给**，而且正文照给（我们用的 curl 开关是
/// `--fail-with-body`）—— 所以"失败了"和"为什么失败"能同时拿到，
/// 不用去猜状态码。返回 `None` 表示"这不是一个错误信封"。
fn envelope_error(value: &serde_json::Value) -> Option<String> {
    let errors = value.get("errors")?.as_array()?;
    let first = errors.first()?;
    let code = first
        .get("code")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let message = first
        .get("message")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let name = first
        .get("detail")
        .and_then(|detail| detail.get("name"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    Some(explain_error_code(code, message, name))
}

/// 单个错误码 → 中文一句话。
///
/// 前几个码都是**实测见过**的（`NAME_UNKNOWN` / `DENIED` 见
/// `tests/fixtures/registry_error_*.json`），其余按原样带出来 ——
/// 编一句自己也不确定的中文，比把 registry 的原话给用户看更糟。
pub fn explain_error_code(code: &str, message: &str, name: &str) -> String {
    match code {
        "NAME_UNKNOWN" => {
            if name.is_empty() {
                "这个 registry 上没有这个仓库".to_owned()
            } else {
                format!("这个 registry 上没有仓库 {name}")
            }
        }
        "DENIED" | "UNAUTHORIZED" => format!(
            "registry 拒绝了这次请求（{code}）—— 这个仓库可能是私有的，这一版只支持公开仓库"
        ),
        "MANIFEST_UNKNOWN" => {
            "这个 tag 在 registry 上不存在（可能已经被删了，换一个 tag 试试）".to_owned()
        }
        "TOOMANYREQUESTS" | "TOO_MANY_REQUESTS" => {
            "registry 限流了，等一会儿再试（这个站点有按小时计数的限流）".to_owned()
        }
        "" => format!("registry 报错：{message}"),
        other => format!("registry 报错（{other}）：{message}"),
    }
}

/// 一次失败的请求 → 能给用户看的一句话。
///
/// 顺序是有讲究的：**先看正文里的错误信封**（它最具体，而且实测 404/403 都带），
/// 拿不到再退回 `curl` 自己的话（`curl: (6) Could not resolve host: …`）。
///
/// `body` 是 stdout，`stderr` 是 curl 的诊断输出 —— `--fail-with-body` 让
/// HTTP 错误时**两边都有东西**（实测 404 → 退出码 22 + 正文 + stderr）。
pub fn explain_failure(body: &str, stderr: &str) -> String {
    if let Ok(value) = parse_json(body) {
        if let Some(message) = envelope_error(&value) {
            return message;
        }
    }
    // `rfind` 而不是 `filter(..).next_back()` —— 后者 clippy 会报
    // `double_ended_iterator_last`（CI 的 lint 任务实测报了 2 条，这是其中一条）。
    let line = stderr
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .unwrap_or("");
    if line.is_empty() {
        "registry 没有响应（网络不通，或者这个 registry 地址不对）".to_owned()
    } else {
        format!("连不上 registry：{line}")
    }
}

/// 解析 `{"name":"…","tags":[…]}`。**保持 registry 给的顺序**，不排序
/// （排序是 [`sort_tags`] 的事）。
///
/// 实测正文形状（`docker.1panel.live`，2067 B / 224 个 tag）：
///
/// ```json
/// {"name":"library/alpine","tags":["2.6","2.7","20190228", …]}
/// ```
///
/// ⚠️ `tags` 可能是 `null`（仓库存在但没有 tag）。那**算错误**而不是空列表：
/// 界面上一片空白，用户分不清是"没 tag"还是"我们解错了"。
pub fn parse_tags(json: &str) -> Result<Vec<String>, String> {
    let value = parse_json(json)?;
    if let Some(message) = envelope_error(&value) {
        return Err(message);
    }
    let list = match value.get("tags") {
        Some(serde_json::Value::Array(items)) => items,
        Some(serde_json::Value::Null) | None => {
            return Err("这个仓库没有任何 tag".to_owned());
        }
        Some(_) => return Err("registry 返回的 tags 不是数组".to_owned()),
    };
    let mut tags = Vec::with_capacity(list.len());
    for item in list {
        if let Some(text) = item.as_str() {
            let text = text.trim();
            if !text.is_empty() {
                tags.push(text.to_owned());
            }
        }
    }
    if tags.is_empty() {
        return Err("这个仓库没有任何 tag".to_owned());
    }
    Ok(tags)
}

// ---------------------------------------------------------------- tag 排序

/// 把 tag 排成"用户会想点的顺序"。
///
/// ⚠️ **接口给的是字母序，照原样显示是错的。** 实测 `library/alpine` 的 224 个 tag
/// 前 3 个是 `2.6, 2.7, 20190228`，而 **`latest` 排在最后一个**（第 224 位）；
/// `3.24.x` 这些真正有用的版本号被夹在中间。排序规则：
///
/// 1. `latest` 第一；
/// 2. 看起来像版本号的按**版本倒序**（`3.24.2 > 3.24.1 > 3.24 > 3.23.6`），
///    同一版本再按 tag 原样升序（`1.31.6` 排在 `1.31.6-alpine` 前面）；
/// 3. 其余的按原样升序（`edge` / `stable` / `trixie` 这类）。
///
/// 实测结果（就是两条 fixture）：alpine →
/// `latest, 3.24.2, 3.24.1, 3.24.0, 3.24, 3.23.6, …`，日期 tag `20190228` 落到第 190 位；
/// nginx → `latest, 1.31.6, 1.31.6-alpine, …`。
pub fn sort_tags(tags: &[String]) -> Vec<String> {
    let mut latest: Vec<String> = Vec::new();
    let mut versioned: Vec<(&str, Vec<u64>)> = Vec::new();
    let mut others: Vec<&str> = Vec::new();

    for tag in tags {
        if tag.eq_ignore_ascii_case("latest") {
            latest.push(tag.clone());
            continue;
        }
        match version_key(tag) {
            Some(key) => versioned.push((tag.as_str(), key)),
            None => others.push(tag.as_str()),
        }
    }

    // 版本倒序；同版本按 tag 升序（`then_with` 让结果是确定的，可以断言）。
    versioned.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    others.sort_unstable();

    let mut out = Vec::with_capacity(tags.len());
    out.extend(latest);
    out.extend(versioned.into_iter().map(|(tag, _)| tag.to_owned()));
    out.extend(others.into_iter().map(str::to_owned));
    out
}

/// 抠出 tag 开头的 `数字(.数字)*` 当版本段；**不像版本号就返回 `None`**。
///
/// "不像版本号"的三条判据，每一条都有实测依据：
///
/// - **不以数字开头**：`edge` / `stable` / `trixie` / `alpine`；
/// - **某一段大于 9999**：alpine 里有一大批 `20190228` 这样的**日期 tag**。
///   按数字比它们会压过 `3.24.2` 排进前几名 —— 实测验证过：不排除的话
///   `20190228` 会挤进前面，排除之后落到第 190（把位置还给真正的版本号）；
/// - **段数大于 4**、**第一段大于 1000**：挡住时间戳之类的长数字。
///
/// 数字解析失败（`u64` 溢出）也返回 `None` —— 一个 30 位的"版本号"不该 panic。
fn version_key(tag: &str) -> Option<Vec<u64>> {
    let mut parts: Vec<u64> = Vec::new();
    let mut current = String::new();
    for ch in tag.chars() {
        if ch.is_ascii_digit() {
            current.push(ch);
        } else if ch == '.' && !current.is_empty() {
            parts.push(current.parse().ok()?);
            current.clear();
        } else {
            // 版本段到这儿就结束了（`3.24.2-alpine` → 只吃 `3.24.2`）
            break;
        }
    }
    if current.is_empty() {
        return None;
    }
    parts.push(current.parse().ok()?);

    if parts.len() > 4 {
        return None;
    }
    if parts.iter().any(|part| *part > 9999) {
        return None;
    }
    if parts[0] > 1000 {
        return None;
    }
    Some(parts)
}

// ---------------------------------------------------------------- manifest

/// 一次 manifest 请求拿回来的东西**是哪一种**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestKind {
    /// 多架构 index：还要再要一次它指向的那一份。
    Index {
        /// 本机架构那一份的摘要，直接当 `manifests/<digest>` 用。
        digest: String,
    },
    /// 已经是单架构 manifest：直接拿去算体积。
    Single,
}

/// 判断 manifest 正文是 index 还是单架构 manifest，并挑出**本机架构**那一份。
///
/// `arch` 由调用方给（[`crate::mirrors::arch`] → `"amd64"` / `"arm64"`），
/// 不在这里读环境 —— 那样这个函数就没法用真实 fixture 单测了。
///
/// ⚠️ 两个坑都实测过，见模块说明：必须**同时**看 `os == "linux"`
/// （hello-world 里有 `amd64/windows` 的两条），且不能"取第一条"
/// （每条真平台后面跟着一条 `unknown/unknown` 的 attestation）。
pub fn classify_manifest(json: &str, arch: &str) -> Result<ManifestKind, String> {
    let value = parse_json(json)?;
    if let Some(message) = envelope_error(&value) {
        return Err(message);
    }

    if let Some(list) = value.get("manifests").and_then(serde_json::Value::as_array) {
        for item in list {
            let platform = match item.get("platform") {
                Some(platform) => platform,
                None => continue,
            };
            let item_arch = platform
                .get("architecture")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let item_os = platform
                .get("os")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            if item_arch != arch || item_os != "linux" {
                continue;
            }
            let digest = item
                .get("digest")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .trim();
            if digest.is_empty() {
                continue;
            }
            return Ok(ManifestKind::Index {
                digest: digest.to_owned(),
            });
        }
        return Err(format!(
            "这个镜像没有 {arch}/linux 那一份（换一个 tag，或者这个镜像只有一个架构）"
        ));
    }

    if value.get("layers").is_some() {
        return Ok(ManifestKind::Single);
    }
    Err("registry 返回的 manifest 既没有 manifests 也没有 layers".to_owned())
}

/// 下载一个镜像要下多少字节。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageSize {
    /// `config.size` + 所有 `layers[].size` 之和（**压缩后**）。
    pub bytes: u64,
    /// 层数（界面上显示"7 层"）。
    pub layers: usize,
}

/// 从一个**单架构** manifest 里累加体积。
///
/// # ⚠️ 这是下载量，不是占用
///
/// 实测同一台机器上的 nginx：这个函数算出 **60.9 MB**，
/// 而「镜像」页里 `wslc images` 的 `Size` 是 **163 MB** —— 差 2.7 倍。
/// 两个数都对（一个是压缩后要下多少，一个是解压后占多少），
/// 所以界面上必须写成"**下载约** 61 MB"。
/// 把它写成"大小"，用户拿它跟镜像列表一对就会当 bug 报上来。
pub fn image_size(json: &str) -> Result<ImageSize, String> {
    let value = parse_json(json)?;
    if let Some(message) = envelope_error(&value) {
        return Err(message);
    }
    let layers = value
        .get("layers")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "manifest 里没有 layers（可能不是单架构 manifest）".to_owned())?;

    let mut bytes = value
        .get("config")
        .and_then(|config| config.get("size"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    for layer in layers {
        bytes += layer
            .get("size")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
    }
    if bytes == 0 {
        return Err("manifest 里所有层的大小都是 0".to_owned());
    }
    Ok(ImageSize {
        bytes,
        layers: layers.len(),
    })
}

// ---------------------------------------------------------------- curl 参数

/// `curl.exe` 拉 JSON 的参数（正文走 stdout）。
///
/// 和 [`crate::mirrors::curl_json_args`] 长得像，但有**三个刻意的差别**：
///
/// - `-S`：`-s` 单用会把错误一起吞掉，那是"点了没反应"里最难查的一种；
/// - `--fail-with-body`：HTTP 4xx/5xx 时**退出码非 0、但正文照给**。
///   实测 404 → 退出码 22 + stdout 是错误信封 + stderr 是
///   `curl: (22) The requested URL returned error: 404` ——
///   这是唯一能同时拿到"失败了"和"为什么失败"的开关组合（`-f` 会把正文丢掉）；
/// - 超时 30 秒：实测 `node` 的 tag 列表 152 KB / 2.4 秒，留足余量；
///   连不上的站实测 8 秒就被系统拒了，所以这个上限不会真的等满。
///
/// ⚠️ `url` 是**裸 argv**（最后一个位置参数）—— 调用方必须先过
/// [`crate::mirrors::is_http_url`]，理由见那里的说明（一个以 `-` 开头的"URL"
/// 会被 curl 当成选项，`-K<file>` 能变成任意下载写盘）。判据只有那一处。
pub fn curl_args(url: &str) -> Vec<String> {
    vec![
        "-s".to_owned(),
        "-S".to_owned(),
        "-L".to_owned(),
        "--fail-with-body".to_owned(),
        "-m".to_owned(),
        "30".to_owned(),
        "-A".to_owned(),
        BROWSER_UA.to_owned(),
        url.to_owned(),
    ]
}

/// manifest 的 `curl.exe` 参数：在 [`curl_args`] 之上加一个 `Accept` 头。
///
/// 单独一个函数而不是给 [`curl_args`] 加参数：tags 那条路不带这个头，
/// 却**必须**逐字节和实测过的那组一致（那类"参数稳定性"断言是
/// "界面上点了没反应"的重灾区）。
pub fn manifest_args(url: &str) -> Vec<String> {
    let mut args = curl_args(url);
    // `url` 是最后一个位置参数，头必须插在它前面。
    let at = args.len() - 1;
    args.insert(at, MANIFEST_ACCEPT.to_owned());
    args.insert(at, "-H".to_owned());
    args
}

/// 我们自己给子进程定的超时上限（一次请求）。
///
/// 比 curl 自己的 `-m 30` 大 5 秒：正常情况下是 curl 先干净地退出（退出码 28，
/// stderr 里有 `Operation timed out`，那是能给用户看的一句话），
/// 这一层只是防止进程彻底不动。
pub fn http_timeout() -> std::time::Duration {
    std::time::Duration::from_secs(35)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 本机真实抓的响应（2026-10-13，`docker.1panel.live`）。
    const TAGS_ALPINE: &str = include_str!("../tests/fixtures/registry_tags_alpine.json");
    const TAGS_NGINX: &str = include_str!("../tests/fixtures/registry_tags_nginx.json");
    const INDEX_ALPINE: &str = include_str!("../tests/fixtures/registry_index_alpine.json");
    const INDEX_HELLO: &str = include_str!("../tests/fixtures/registry_hello_world.json");
    const MANIFEST_ALPINE: &str =
        include_str!("../tests/fixtures/registry_manifest_alpine_amd64.json");
    const ERR_NAME_UNKNOWN: &str =
        include_str!("../tests/fixtures/registry_error_name_unknown.json");
    const ERR_DENIED: &str = include_str!("../tests/fixtures/registry_error_denied.json");

    // ------------------------------------------------------------ 输入规范化

    #[test]
    fn short_names_get_the_library_prefix_we_add_it_ourselves() {
        // 实测：1panel.live 对短名回 404 NAME_UNKNOWN，1ms.run 会自己补。
        // 判据只能有一处，所以是我们补。
        assert_eq!(
            split_input("alpine").unwrap(),
            ("library/alpine".to_owned(), None)
        );
        assert_eq!(
            split_input("  alpine  ").unwrap(),
            ("library/alpine".to_owned(), None)
        );
        // 大小写：registry 要求仓库名小写
        assert_eq!(
            split_input("NGINX").unwrap(),
            ("library/nginx".to_owned(), None)
        );
        // 已经是全名就原样
        assert_eq!(
            split_input("library/nginx").unwrap(),
            ("library/nginx".to_owned(), None)
        );
        // 非 library 的命名空间（实测 grafana/grafana 200）
        assert_eq!(
            split_input("grafana/grafana").unwrap(),
            ("grafana/grafana".to_owned(), None)
        );
    }

    #[test]
    fn a_pasted_reference_or_url_is_understood() {
        // 用户会从浏览器或 docker pull 命令里复制，形状五花八门
        assert_eq!(
            split_input("docker.1panel.live/library/nginx").unwrap(),
            ("library/nginx".to_owned(), None)
        );
        // ⚠️ 这条是 `/v2/` 在**主机名之后**的形状 —— 剥离顺序错了就会把它
        // 当成仓库名的一部分（写完自查时真踩到过）
        assert_eq!(
            split_input("https://docker.1panel.live/v2/library/alpine/tags/list").unwrap(),
            ("library/alpine".to_owned(), None)
        );
        assert_eq!(
            split_input("docker.1panel.live/alpine").unwrap(),
            ("library/alpine".to_owned(), None)
        );
        // 带 tag
        assert_eq!(
            split_input("nginx:1.27").unwrap(),
            ("library/nginx".to_owned(), Some("1.27".to_owned()))
        );
        assert_eq!(
            split_input("library/nginx:1.27-alpine").unwrap(),
            ("library/nginx".to_owned(), Some("1.27-alpine".to_owned()))
        );
        // manifest 的 URL 粘进来 → 仓库 + tag 都要认出来
        assert_eq!(
            split_input("https://docker.1panel.live/v2/library/nginx/manifests/1.27").unwrap(),
            ("library/nginx".to_owned(), Some("1.27".to_owned()))
        );
        // 裸 `v2/` 开头（用户从地址栏少复制了一截）
        assert_eq!(
            split_input("v2/library/alpine/tags/list").unwrap(),
            ("library/alpine".to_owned(), None)
        );
    }

    #[test]
    fn a_registry_port_is_not_a_tag() {
        // `registry:5000/foo` 里的冒号是端口。判据是"冒号在最后一个斜杠之后"。
        let (repository, tag) = split_input("registry:5000/foo").unwrap();
        assert_eq!(repository, "library/foo");
        assert_eq!(tag, None);
    }

    #[test]
    fn dangerous_or_nonsense_input_is_refused() {
        // 空 / 只有分隔符 / 冒号后面没东西
        for bad in ["", "   ", "/", ":", "nginx:"] {
            assert!(split_input(bad).is_err(), "不该放行：{bad:?}");
        }

        // 摘要这一版不支持，要说清而不是静默当成 tag
        let err = split_input("alpine@sha256:abc").unwrap_err();
        assert!(err.contains("摘要"), "{err}");

        // 这一组是**安全边界**：这些名字会拼进 URL 再交给 curl 当裸 argv
        for bad in [
            "../../etc/passwd",
            "library/../secret",
            "a//b",
            "-o/tmp/x",
            "library/nginx?x=1",
            "library/nginx#frag",
            "library/nginx%2f..",
            "中文仓库",
            "library/nginx:bad tag",
        ] {
            assert!(split_input(bad).is_err(), "不该放行：{bad:?}");
        }

        // 判据本身
        assert!(!is_valid_repository("../../etc/passwd"));
        assert!(!is_valid_repository("a//b"));
        assert!(!is_valid_repository("-o"));
        assert!(!is_valid_repository("nginx?x=1"));
        assert!(!is_valid_repository(""));
        assert!(!is_valid_repository("中文"));
        assert!(!is_valid_repository("/leading"));
        assert!(!is_valid_repository("trailing/"));
        // 合法的一批
        assert!(is_valid_repository("library/alpine"));
        assert!(is_valid_repository("grafana/grafana"));
        assert!(is_valid_repository("my--repo"));
        assert!(is_valid_repository("a.b_c-d/e"));
    }

    #[test]
    fn reference_validation_covers_tags_and_digests() {
        // tag 允许大写（和仓库名不一样）
        assert!(is_valid_reference("latest"));
        assert!(is_valid_reference("1.27-alpine"));
        assert!(is_valid_reference("RC1"));
        assert!(is_valid_reference("v1.2.3_rc1"));
        // 摘要：index 里给的就是这个形状
        assert!(is_valid_reference(
            "sha256:d56c381f961d307a21b3ca004cf1e3910f106644aefb1f43e654c8a56c4fd395"
        ));
        // 不合法
        assert!(!is_valid_reference(""));
        assert!(!is_valid_reference("-bad"));
        assert!(!is_valid_reference("bad tag"));
        assert!(!is_valid_reference("a/b"));
        assert!(!is_valid_reference(&"x".repeat(200)));
    }

    // ------------------------------------------------------------ URL 与引用

    #[test]
    fn urls_are_built_exactly() {
        assert_eq!(
            tags_url("docker.1panel.live", "library/alpine"),
            "https://docker.1panel.live/v2/library/alpine/tags/list"
        );
        assert_eq!(
            manifest_url("docker.1panel.live", "library/alpine", "latest"),
            "https://docker.1panel.live/v2/library/alpine/manifests/latest"
        );
        // 末尾多余斜杠要有容错（用户自填 registry 时会带上）
        assert_eq!(
            tags_url("docker.1panel.live/", "library/alpine"),
            "https://docker.1panel.live/v2/library/alpine/tags/list"
        );
        // 摘要形态的 reference 直接进路径
        assert_eq!(
            manifest_url("h", "library/a", "sha256:abc"),
            "https://h/v2/library/a/manifests/sha256:abc"
        );
    }

    #[test]
    fn pull_reference_carries_the_registry_prefix() {
        // 不带前缀的话 wslc 会去 Docker Hub 找，而它在这台机器上不可达（实测 000）
        assert_eq!(
            pull_reference("docker.1panel.live", "library/alpine", "latest"),
            "docker.1panel.live/library/alpine:latest"
        );
        // 与实测拉下来的镜像名一致：docker.1panel.live/library/hello-world:latest
        assert_eq!(
            pull_reference("docker.1panel.live", "library/hello-world", "latest"),
            "docker.1panel.live/library/hello-world:latest"
        );
    }

    // ------------------------------------------------------------ tag 解析

    #[test]
    fn parses_the_real_alpine_tag_list() {
        let tags = parse_tags(TAGS_ALPINE).expect("真实响应应该能解析");
        assert_eq!(tags.len(), 224);
        assert_eq!(tags[0], "2.6");
        assert!(tags.contains(&"latest".to_owned()));
        assert!(tags.contains(&"3.24.2".to_owned()));
    }

    #[test]
    fn the_registry_gives_alphabetical_order_which_is_why_we_sort() {
        // 这条就是"为什么必须排序"的证据本身：接口给的是字母序，
        // 于是 **latest 排在最后一个**、日期 tag 排在前面。
        let tags = parse_tags(TAGS_ALPINE).unwrap();
        assert_eq!(tags.last().map(String::as_str), Some("latest"));
        assert_eq!(
            tags.iter().position(|tag| tag.as_str() == "latest"),
            Some(tags.len() - 1)
        );
        // 前几个是上古版本和日期 tag —— 照原样显示就是把最没用的排最前
        assert_eq!(tags[0], "2.6");
        assert_eq!(tags[1], "2.7");
        assert_eq!(tags[2], "20190228");
    }

    #[test]
    fn tag_parsing_reports_the_real_failure_modes() {
        // 空正文 / 不是 JSON / 业务错误信封 / tags 是 null / tags 是空数组 / tags 不是数组
        assert!(parse_tags("").unwrap_err().contains("空内容"));
        assert!(parse_tags("<html>502</html>")
            .unwrap_err()
            .contains("不是 JSON"));
        assert!(parse_tags(ERR_NAME_UNKNOWN)
            .unwrap_err()
            .contains("library/no-such-image-xyz"));
        assert!(parse_tags(r#"{"name":"a","tags":null}"#)
            .unwrap_err()
            .contains("没有任何 tag"));
        assert!(parse_tags(r#"{"name":"a","tags":[]}"#)
            .unwrap_err()
            .contains("没有任何 tag"));
        assert!(parse_tags(r#"{"name":"a","tags":"latest"}"#)
            .unwrap_err()
            .contains("不是数组"));
        // 非字符串元素要跳过而不是 panic
        let tags = parse_tags(r#"{"name":"a","tags":["1","", 2, null, " 3 "]}"#).unwrap();
        assert_eq!(tags, vec!["1".to_owned(), "3".to_owned()]);
        // BOM 开头也认
        assert!(parse_tags(&format!("\u{feff}{TAGS_ALPINE}")).is_ok());
    }

    // ------------------------------------------------------------ tag 排序

    #[test]
    fn sorting_puts_latest_first_then_versions_descending() {
        // 实测输出（alpine 的 224 个真实 tag）。
        // ⚠️ 用 `Vec<&str>` 比，不用数组字面量 —— 数组与切片的 `PartialEq`
        // 组合在类型推断上很容易踩空，测试代码不该在这上面赌。
        let tags = sort_tags(&parse_tags(TAGS_ALPINE).unwrap());
        let head: Vec<&str> = tags[..6].iter().map(String::as_str).collect();
        assert_eq!(
            head,
            vec!["latest", "3.24.2", "3.24.1", "3.24.0", "3.24", "3.23.6"]
        );

        // nginx 的 1339 个真实 tag：同一版本按 tag 升序
        let nginx = sort_tags(&parse_tags(TAGS_NGINX).unwrap());
        let head: Vec<&str> = nginx[..3].iter().map(String::as_str).collect();
        assert_eq!(head, vec!["latest", "1.31.6", "1.31.6-alpine"]);
        // 非版本 tag 排在所有版本号后面
        let tail: Vec<&str> = nginx[nginx.len() - 3..]
            .iter()
            .map(String::as_str)
            .collect();
        assert_eq!(tail, vec!["trixie", "trixie-otel", "trixie-perl"]);
    }

    #[test]
    fn date_like_tags_are_not_treated_as_versions() {
        // 这是排序里唯一不显然的一条：alpine 里有一批 `20190228` 这样的日期 tag。
        // 按数字比它们会压过 3.24.2；排除之后它们落到后面。
        let tags = sort_tags(&parse_tags(TAGS_ALPINE).unwrap());
        let at = tags
            .iter()
            .position(|tag| tag.as_str() == "20190228")
            .unwrap();
        assert!(at > 150, "日期 tag 不该挤进前面，实际第 {at} 位");
        // 反过来钉住"版本号确实在前面"
        assert!(
            tags.iter()
                .position(|tag| tag.as_str() == "3.24.2")
                .unwrap()
                < 3
        );

        // 判据本身：像版本号的给 key，不像的给 None
        assert_eq!(version_key("3.24.2-alpine"), Some(vec![3, 24, 2]));
        assert_eq!(version_key("1.31.6"), Some(vec![1, 31, 6]));
        assert_eq!(version_key("18-alpine3.19"), Some(vec![18]));
        assert_eq!(version_key("3"), Some(vec![3]));
        assert_eq!(version_key("20190228"), None);
        assert_eq!(version_key("edge"), None);
        assert_eq!(version_key("stable-trixie"), None);
        assert_eq!(version_key("alpine"), None);
        assert_eq!(version_key(""), None);
        // 超长数字不能 panic（u64 溢出的那条路）
        assert_eq!(version_key("99999999999999999999999999"), None);
        assert_eq!(version_key("1.2.3.4.5"), None);
        assert_eq!(version_key("12345"), None);
    }

    #[test]
    fn sorting_never_loses_or_invents_tags() {
        for fixture in [TAGS_ALPINE, TAGS_NGINX] {
            let tags = parse_tags(fixture).unwrap();
            let sorted = sort_tags(&tags);
            assert_eq!(sorted.len(), tags.len());
            let mut before = tags.clone();
            let mut after = sorted;
            before.sort();
            after.sort();
            assert_eq!(before, after, "排序不能丢 tag 也不能多出来");
        }
        // 空输入不能 panic
        assert!(sort_tags(&[]).is_empty());
        // 一堆 latest 变体：都算 latest，不能互相踩掉
        let odd = vec![
            "Latest".to_owned(),
            "LATEST".to_owned(),
            "latest".to_owned(),
            "1".to_owned(),
        ];
        let sorted = sort_tags(&odd);
        assert_eq!(sorted.len(), 4);
        assert!(sorted[0].eq_ignore_ascii_case("latest"));
        assert_eq!(sorted[3], "1");
    }

    // ---- registry 主机名 -------------------------------------------------
    //
    // v0.6.2 起它**来自输入框**（合并两页时把 registry 提成了字段，见
    // `docs/PLAN-v0.6.md` §3.6）—— 所以这两个函数的判据必须写死在这里。

    #[test]
    fn accepts_the_hosts_people_actually_type() {
        for good in [
            "docker.1panel.live",
            "docker.m.daocloud.io",
            // 私有 registry：内网域名、localhost、带端口的都要认
            "registry.internal",
            "localhost",
            "localhost:5000",
            "192.168.1.10:5000",
            "192.168.1.10",
            // 大小写不敏感（DNS 本来就不敏感）
            "Docker.1Panel.Live",
        ] {
            assert!(is_valid_host(good), "{good:?} 该被接受");
        }
    }

    #[test]
    fn rejects_hosts_that_would_deform_the_url_or_argv() {
        for bad in [
            "",
            "   ",
            // 路径 / 查询 / 片段：我们要的是**纯主机名**
            "docker.1panel.live/v2",
            "docker.1panel.live?x=1",
            "docker.1panel.live#frag",
            // 空 label / 首尾非字母数字
            "a..b",
            ".leading",
            "trailing.",
            "-leading",
            // 端口：占位符、越界、非数字
            "host:port",
            "host:99999",
            "host:0",
            "host:",
            // 空白与非 ASCII
            "bad host",
            "主机.中国",
            // curl 的选项形状（纵深防御：别让它有机会被当成开关）
            "-K/tmp/x",
        ] {
            assert!(!is_valid_host(bad), "{bad:?} 该被拒绝");
        }
    }

    #[test]
    fn normalize_host_strips_what_a_copy_paste_brings() {
        // 从浏览器地址栏/别处复制过来的常见形状
        assert_eq!(normalize_host("  docker.1panel.live  "), "docker.1panel.live");
        assert_eq!(normalize_host("https://docker.1panel.live"), "docker.1panel.live");
        assert_eq!(normalize_host("http://docker.1panel.live/"), "docker.1panel.live");
        assert_eq!(normalize_host("Docker.1Panel.Live"), "docker.1panel.live");
        // 收拾完仍然不合法的东西，交给 `is_valid_host` 去拒 —— 这里不越权
        assert_eq!(normalize_host("https://"), "");
        assert!(!is_valid_host(&normalize_host("https://")));
    }

    // ------------------------------------------------------------ manifest

    #[test]
    fn picks_the_linux_manifest_of_the_requested_arch() {
        // 真实 index：16 条里 8 条是 attestation，amd64/linux 只有一条
        match classify_manifest(INDEX_ALPINE, "amd64").unwrap() {
            ManifestKind::Index { digest } => assert_eq!(
                digest,
                "sha256:d56c381f961d307a21b3ca004cf1e3910f106644aefb1f43e654c8a56c4fd395"
            ),
            other => panic!("应该是 index，实际 {other:?}"),
        }
        // arm64 那条也在（variant=v8），digest 得是 arm64 的
        match classify_manifest(INDEX_ALPINE, "arm64").unwrap() {
            ManifestKind::Index { digest } => {
                assert!(digest.starts_with("sha256:260479a1cfaf"), "{digest}")
            }
            other => panic!("应该是 index，实际 {other:?}"),
        }
    }

    #[test]
    fn never_picks_an_attestation_or_a_windows_manifest() {
        // attestation 那条是 unknown/unknown —— 挑"第一条"就会挑到它
        match classify_manifest(INDEX_ALPINE, "amd64").unwrap() {
            ManifestKind::Index { digest } => {
                assert!(
                    !digest.starts_with("sha256:350f747a86e8"),
                    "挑到 attestation 了：{digest}"
                );
                assert!(
                    !digest.starts_with("sha256:e1ca081cf7a9"),
                    "挑到 attestation 了：{digest}"
                );
            }
            other => panic!("应该是 index，实际 {other:?}"),
        }
        // hello-world 里有 **amd64/windows** 两条 —— 只看架构就会挑错
        match classify_manifest(INDEX_HELLO, "amd64").unwrap() {
            ManifestKind::Index { digest } => {
                assert!(digest.starts_with("sha256:d1a8d0a4eeb6"), "{digest}");
                assert!(
                    !digest.starts_with("sha256:d08be0dd652a"),
                    "挑到 windows 那份了：{digest}"
                );
                assert!(!digest.starts_with("sha256:8fd8355009a7"), "{digest}");
            }
            other => panic!("应该是 index，实际 {other:?}"),
        }
    }

    #[test]
    fn a_single_arch_manifest_is_recognised_directly() {
        // 单架构 manifest 没有 manifests 数组，但有 layers
        assert_eq!(
            classify_manifest(MANIFEST_ALPINE, "amd64").unwrap(),
            ManifestKind::Single
        );
        // 认不出来的时候要说清，而不是假装 Single
        assert!(classify_manifest(r#"{"schemaVersion":2}"#, "amd64").is_err());
        // 架构不存在（alpine 里没有 mips）
        let err = classify_manifest(INDEX_ALPINE, "mips").unwrap_err();
        assert!(err.contains("mips"), "{err}");
        // 错误信封要透出来
        assert!(classify_manifest(ERR_DENIED, "amd64").is_err());
    }

    #[test]
    fn image_size_sums_config_and_layers() {
        let size = image_size(MANIFEST_ALPINE).unwrap();
        // 真实的 alpine amd64 manifest：config 611 + 一层 3849738
        assert_eq!(size.layers, 1);
        assert_eq!(size.bytes, 611 + 3_849_738);
        // 说人话：3.9 MB（1000 进制，和 WSL 自己显示的一致）
        assert_eq!(crate::mirrors::human_bytes(size.bytes), "3.9 MB");
    }

    #[test]
    fn image_size_refuses_things_it_cannot_measure() {
        // 拿 index 当 manifest 用 → 没有 layers
        let err = image_size(INDEX_ALPINE).unwrap_err();
        assert!(err.contains("layers"), "{err}");
        // 层大小全是 0（畸形）→ 报错而不是给一个 0 MB 的进度
        assert!(image_size(r#"{"config":{"size":0},"layers":[{"size":0}]}"#).is_err());
        // 缺 config 也要能算（只有层）
        let size = image_size(r#"{"layers":[{"size":100},{"size":200}]}"#).unwrap();
        assert_eq!(size.bytes, 300);
        assert_eq!(size.layers, 2);
        // 字符串形态的数字不算（schema 1 那种），宁可报错
        assert!(image_size(r#"{"layers":[{"size":"100"}]}"#).is_err());
    }

    // ------------------------------------------------------------ 错误翻译

    #[test]
    fn real_error_envelopes_become_chinese_sentences() {
        // 真实抓的 404（仓库不存在）与 403（私有）
        let message = explain_failure(ERR_NAME_UNKNOWN, "");
        assert!(message.contains("library/no-such-image-xyz"), "{message}");
        assert!(!message.contains("NAME_UNKNOWN"), "不要给用户看错误码原文");
        let message = explain_failure(ERR_DENIED, "");
        assert!(message.contains("私有"), "{message}");
        // 我们替用户猜的几条
        assert!(explain_error_code("MANIFEST_UNKNOWN", "", "").contains("tag"));
        assert!(explain_error_code("TOOMANYREQUESTS", "", "").contains("限流"));
        assert!(explain_error_code("", "boom", "").contains("boom"));
        // 没见过的码要把原话带出来（编一句不确定的中文更糟）
        let message = explain_error_code("WEIRD", "something odd", "");
        assert!(message.contains("WEIRD") && message.contains("something odd"));
    }

    #[test]
    fn failures_fall_back_to_curls_own_message() {
        // 不是 JSON 的正文（HTML 错误页）→ 用 curl 的话
        let message = explain_failure(
            "<html>502 Bad Gateway</html>",
            "curl: (22) The requested URL returned error: 502",
        );
        assert!(message.contains("502"), "{message}");
        // 连不上：stdout 是空的
        let message = explain_failure("", "curl: (6) Could not resolve host: nope.invalid");
        assert!(message.contains("Could not resolve host"), "{message}");
        // 两边都没有 → 也要有一句人话，不能是空字符串
        assert!(!explain_failure("", "").is_empty());
        // 多行 stderr 取最后一行（curl 可能先打别的）
        let message = explain_failure("", "warning: x\n\ncurl: (28) Operation timed out");
        assert!(message.contains("timed out"), "{message}");
    }

    // ------------------------------------------------------------ curl 参数

    #[test]
    fn curl_arguments_are_stable() {
        // 这组开关是"点了没反应"的重灾区，逐条钉住
        assert_eq!(
            curl_args("https://h/v2/library/alpine/tags/list"),
            vec![
                "-s",
                "-S",
                "-L",
                "--fail-with-body",
                "-m",
                "30",
                "-A",
                BROWSER_UA,
                "https://h/v2/library/alpine/tags/list"
            ]
        );
        // `-S` 必须在：少了它 curl 会把错误一起吞掉
        let args = curl_args("https://h/x");
        assert!(args.iter().any(|arg| arg == "-S"), "{args:?}");
        assert!(args.iter().any(|arg| arg == "--fail-with-body"), "{args:?}");
        // url 永远是最后一个位置参数（裸 argv —— 调用方先过 is_http_url）
        assert_eq!(args[args.len() - 1], "https://h/x");
    }

    #[test]
    fn manifest_arguments_carry_the_accept_header() {
        let args = manifest_args("https://h/v2/library/alpine/manifests/latest");
        assert_eq!(
            args[args.len() - 1],
            "https://h/v2/library/alpine/manifests/latest"
        );
        let at = args.iter().position(|arg| arg == "-H").expect("要有 -H");
        assert_eq!(args[at + 1], MANIFEST_ACCEPT);
        // 头和 url 之间不能夹别的东西
        assert_eq!(at + 2, args.len() - 1);
        // 除头之外必须和 curl_args 逐字节一致
        let mut plain = curl_args("https://h/x");
        let at = plain.len() - 1;
        plain.insert(at, MANIFEST_ACCEPT.to_owned());
        plain.insert(at, "-H".to_owned());
        assert_eq!(manifest_args("https://h/x"), plain);
    }

    #[test]
    fn limits_and_constants_are_sane() {
        // 快捷入口里必须都是实测 200 的那批（这里抽查，全量核对在 PR 里）
        for name in ["nginx", "redis", "alpine", "ubuntu", "hello-world"] {
            assert!(COMMON_IMAGES.contains(&name), "{name} 应该在快捷清单里");
        }
        // 快捷清单是**短名**：界面点一下就走 split_input 那条路补 `library/`
        assert!(COMMON_IMAGES.iter().all(|name| !name.contains('/')));
        // "按钮别铺满一屏"和"tag 上限的合理范围"这两条是**常量之间**的关系，
        // 已经在定义处用 `const _: () = assert!(...)` 编译期钉住了，这里不用再判。
        assert_eq!(DEFAULT_REGISTRY, "docker.1panel.live");
        assert!(http_timeout() >= std::time::Duration::from_secs(30));
    }
}
