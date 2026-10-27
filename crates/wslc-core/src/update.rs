//! 更新检查里**不碰网络**的那一半。
//!
//! 三件事：版本号的解析与比较、GitHub `releases/latest` 响应的解析、
//! 以及"这次失败到底是怎么回事"的翻译。
//!
//! # 为什么放在 `wslc-core` 而不是 `wslc-panel-core`
//!
//! 它要配一份**真实抓下来的** fixture 做单测，而它的邻居
//! [`crate::mirrors`] / [`crate::registry`] 就是"外部协议 + 纯解析"这个形状。
//! 跑进程的那半边在 [`crate::cmd::update`]。两个 crate 都在 CI 的
//! `cargo test --workspace --exclude wslc-panel` 里（`AGENTS.md` §2）。
//!
//! # 为什么不用 `semver` crate
//!
//! `semver` 在 `Cargo.lock` 里只是**传递依赖**；把它提成本 crate 的直接依赖，
//! 会改 `[[package]] name = "wslc-core"` 的 `dependencies` 数组，而所有 cargo
//! 步骤都带 `--locked`，本机又没有工具链重新生成锁文件（`AGENTS.md` §3）。
//! 而本项目只产 `X.Y.Z` 这种三段版本号 —— 为此引一个 crate 不划算。
//!
//! # 边界：这里只说"有没有新版"
//!
//! **不做下载、不做校验、不做替换**。用户自己在发布页取文件。

use std::fmt;

use serde_json::Value;

/// 仓库路径。
pub const REPO: &str = "SilkKirk/wslc-panel";

/// `release.body`（发布说明）在界面上最多留多少个字符。
///
/// 实测 v0.9.25 的 body 就有 600 多字，整段塞进「关于」页的卡片会把按钮
/// 挤出屏幕。完整内容在发布页上 —— 那里才是该读的地方。
pub const NOTES_HEAD_CHARS: usize = 400;

/// 版本号：`X.Y.Z`，可选 `-pre` 后缀。
///
/// **刻意只认三段**：本项目的版本号来自 `Cargo.toml`，永远是三段；
/// 认四段（`1.2.3.4`）只会让"这个 tag 到底是什么版本"变得含糊。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    /// 主版本。
    pub major: u64,
    /// 次版本。
    pub minor: u64,
    /// 修订号。
    pub patch: u64,
    /// 预发布后缀（`rc1`）；`None` = 正式版。
    pub pre: Option<String>,
}

impl Version {
    /// 解析 `"v0.9.25"` / `"0.9.25"` / `"0.10.0-rc1"`。
    ///
    /// 前缀 `v` 可有可无 —— 仓库里两种标签都出现过（实测有 `v0.1.0`，
    /// 也有一个裸 `0.1.0`），不能假定格式统一。
    ///
    /// 解析不出来就返回 `None`，**不做任何猜测**：调用方据此把这次检查
    /// 判成"读不懂的版本"，而不是拿一个半截数字去比大小。
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        // 剥一个前缀 v/V：`str::strip_prefix` 只认一个字面量，
        // 两种大小写都要能过。`&text[1..]` 是安全的 —— 'v'/'V' 都只占 1 字节。
        let text = if text.starts_with('v') || text.starts_with('V') {
            &text[1..]
        } else {
            text
        };
        if text.is_empty() {
            return None;
        }

        let (core, pre) = match text.split_once('-') {
            Some((core, pre)) => {
                let pre = pre.trim();
                if pre.is_empty() {
                    return None;
                }
                (core, Some(pre.to_owned()))
            }
            None => (text, None),
        };

        let mut parts = core.split('.');
        let major = parse_number(parts.next()?)?;
        let minor = parse_number(parts.next()?)?;
        let patch = parse_number(parts.next()?)?;
        // 多出来的段（`1.2.3.4`）不是我们认识的形状 —— 宁可判成读不懂。
        if parts.next().is_some() {
            return None;
        }

        Some(Self {
            major,
            minor,
            patch,
            pre,
        })
    }
}

/// 一段版本号必须是纯数字。
///
/// 不用 `str::parse` 直接判：它会接受 `"+3"` 这种带符号的写法，
/// 而 `1.+3.0` 显然不是一个版本号。
fn parse_number(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if let Some(pre) = &self.pre {
            write!(f, "-{pre}")?;
        }
        Ok(())
    }
}

/// `latest` 是否比 `current` 新。
///
/// 三条规则，每一条都有理由：
///
/// 1. **数字段按数值比**，不是字符串比：`0.9.9 < 0.9.10`。字符串比会得出
///    相反的结论，而版本号从 0.9.9 跳到 0.9.10 是常态。
/// 2. **大版本优先**：`0.10.0 > 0.9.99`（由第 1 条自然得出）。
/// 3. **预发布 < 同号正式版**：`0.10.0-rc1 < 0.10.0`。所以"我装的是 rc1、
///    正式版发了"要提示；而"我装的是正式版、远端有个 rc2"不该提示。
///
/// 两边都是预发布时按字符串比 —— 这是个**已知的不精确**（`rc2 < rc10`
/// 会判反）。本项目目前不发预发布版，这条只是兜底；真要发预发布版时，
/// 得按 semver 的"点分段逐段比"重写这一支。
pub fn is_newer(latest: &Version, current: &Version) -> bool {
    use std::cmp::Ordering;

    match (latest.major, latest.minor, latest.patch).cmp(&(
        current.major,
        current.minor,
        current.patch,
    )) {
        Ordering::Greater => true,
        Ordering::Less => false,
        Ordering::Equal => match (&latest.pre, &current.pre) {
            (None, None) => false,
            // 正式版比预发布版新
            (None, Some(_)) => true,
            (Some(_), None) => false,
            (Some(latest_pre), Some(current_pre)) => latest_pre > current_pre,
        },
    }
}

/// 一次"检查更新"拿到的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// 解析出来的版本号（比较用）。
    pub version: Version,
    /// 原始版本串，**不带** `v`（`0.9.25`）。界面直接显示它，
    /// 免得我们自己重新拼一遍、拼出两种写法。
    pub raw_version: String,
    /// 原始 tag（`v0.9.25`）。用户可能手工填过"跳过这个版本"，两种写法都认。
    pub tag: String,
    /// 发布日期（`2026-10-19`）。GitHub 给的是完整时间戳，我们只留日期。
    pub published: Option<String>,
    /// 发布页地址（`html_url`）——「打开发布页」就是去这儿。
    pub page_url: String,
    /// 发布说明的**开头一小段**（见 [`NOTES_HEAD_CHARS`]）。
    pub notes_head: String,
}

/// `releases/latest` 的请求地址。
pub fn api_url() -> String {
    format!("https://api.github.com/repos/{REPO}/releases/latest")
}

/// 解析 GitHub `releases/latest` 的响应。
///
/// # 为什么要显式判 `draft` / `prerelease`
///
/// `releases/latest` 本来就不返回草稿和预发布版，所以这两条现在是**死代码**。
/// 留着是因为"改成 `/releases` 列表然后自己挑第一条"是个很容易做的改动，
/// 而那样一改，草稿和预发布版就会混进来 —— 这两行是给那次改动留的护栏。
///
/// # 错误也要能看懂
///
/// 403（配额）和 404（没有 release）的正文是
/// `{"message":"Not Found","documentation_url":…}` 这种形状，
/// 没有 `tag_name`。所以取不到 `tag_name` 时先看有没有 `message`，
/// 把 GitHub 的原话带出去，比一句"响应里没有 tag_name"有用。
pub fn parse_release(json: &str) -> Result<Release, String> {
    let value: Value =
        serde_json::from_str(json).map_err(|e| format!("GitHub 返回的不是 JSON：{e}"))?;

    let tag = value
        .get("tag_name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_owned();

    if tag.is_empty() {
        if let Some(message) = value.get("message").and_then(Value::as_str) {
            let message = message.trim();
            if !message.is_empty() {
                return Err(format!("GitHub 说：{message}"));
            }
        }
        return Err("响应里没有 tag_name".to_owned());
    }

    if value.get("draft").and_then(Value::as_bool).unwrap_or(false) {
        return Err(format!("{tag} 还是草稿，没有真正发布"));
    }
    if value
        .get("prerelease")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(format!("{tag} 是预发布版"));
    }

    let version =
        Version::parse(&tag).ok_or_else(|| format!("tag「{tag}」不是 X.Y.Z 形式，没法比较版本"))?;

    let page_url = value
        .get("html_url")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_owned();
    if page_url.is_empty() {
        return Err("响应里没有 html_url（打不开发布页）".to_owned());
    }

    // `published_at` 形如 `2026-10-19T03:58:13Z`，只留前 10 个字符。
    // 用 `chars().take(10)` 而不是字节切片：虽然 ISO 时间戳是纯 ASCII，
    // 但字节切片一旦遇到别的字符就会 panic。
    let published = value
        .get("published_at")
        .and_then(Value::as_str)
        .map(|stamp| stamp.chars().take(10).collect::<String>())
        .filter(|date| date.len() == 10 && date.contains('-'));

    let body = value.get("body").and_then(Value::as_str).unwrap_or("");

    Ok(Release {
        raw_version: version.to_string(),
        version,
        tag,
        published,
        page_url,
        notes_head: notes_head(body),
    })
}

/// 发布说明留下开头一段，超出的用省略号收尾。
///
/// 按**字符**截断而不是字节：发布说明里全是中文，按字节切会把某个字劈成
/// 两半，`String` 会直接 panic（不是"显示乱码"那么轻）。
fn notes_head(body: &str) -> String {
    let text = body.trim();
    if text.is_empty() {
        return String::new();
    }
    let mut head: String = text.chars().take(NOTES_HEAD_CHARS).collect();
    if text.chars().count() > NOTES_HEAD_CHARS {
        head.push('…');
    }
    head
}

/// 拉 `releases/latest` 的 `curl.exe` 参数。
///
/// ⚠️ `url` 是**裸 argv**（最后一个位置参数）：调用方必须先过
/// [`crate::mirrors::is_http_url`]，否则一个以 `-` 开头的字符串会被 curl
/// 当成选项（`-K<file>` 能变成任意读写盘，见那个函数的说明）。
///
/// 参数逐条的理由：
///
/// - `-sS`：静默，但**保留**错误输出（只 `-s` 会把错误一起吞掉）；
/// - `-L`：跟随重定向（有的代理会先 302 一下）；
/// - `--fail-with-body`：4xx/5xx 时退出码非 0，**但正文照给** —— 限流的原因
///   （`API rate limit exceeded`）就在正文里，没有它只能看到一句 HTTP 403；
/// - `--proto "=https"`：只许 https，防止被降级；
/// - `-m 15`：这个响应实测 6.6 KB / 2.1 秒，15 秒足够；给死总超时是安全的
///   （和"下载 30 MB 的 exe 不能给总超时"是两件事）；
/// - `Accept` / `User-Agent`：GitHub **强制**要求 User-Agent（不带给 403），
///   带上版本号也方便以后在日志里对账。
///
/// ⚠️ **这里刻意没有 `-q`**：本函数的唯一调用者是 `cmd::update::check`，
/// 它把参数交给 `cmd::http::curl`，那一层会**强制**把 `-q` 放到最前
/// （`with_quiet_first`，有测试钉住"最前 + 只一份"）。
///
/// 别在这儿自己补一个 `-q`：加了不会报错，只会传两份（第二份位置还不一定对）。
/// **判据是"这批参数交给谁"**：
/// - 交给 `cmd::http::curl` → **不要**带 `-q`（它负责）；
/// - 交给 `cli::spawn_streaming_program` 直达 curl（install 的下载）→ **必须**自带
///   （见 `mirrors::download_args`，那里第一个参数就是 `-q`）。
pub fn curl_args(url: &str) -> Vec<String> {
    vec![
        "-sS".to_owned(),
        "-L".to_owned(),
        "--fail-with-body".to_owned(),
        "--proto".to_owned(),
        "=https".to_owned(),
        "-m".to_owned(),
        "15".to_owned(),
        "-H".to_owned(),
        "Accept: application/vnd.github+json".to_owned(),
        "-H".to_owned(),
        format!("User-Agent: wslc-panel/{}", env!("CARGO_PKG_VERSION")),
        url.to_owned(),
    ]
}

/// 把"这次为什么失败"翻译成一句能给用户看的话。
///
/// 认不出来就给一句**朴素**的描述（带 curl 的退出码和原文摘录），
/// 绝不编一个"请检查网络"糊弄过去 —— 那种话对排查没有帮助。
pub fn explain_failure(exit_code: Option<i32>, text: &str) -> String {
    let lower = text.to_lowercase();

    // ① 限流。这是最常见的一种：未认证 60 次/小时/IP（实测），
    //    共享出口（公司 NAT、校园网）很容易被别人用光。
    if lower.contains("rate limit") {
        return "GitHub 接口的匿名配额用完了（每小时 60 次，同一个出口 IP 共用）—— \
                过一会儿再试，或者直接去发布页看。"
            .to_owned();
    }

    // ② 仓库还没有 release。正文是 {"message":"Not Found",…}。
    if lower.contains("\"message\":\"not found\"") || lower.contains("\"message\": \"not found\"") {
        return "这个仓库还没有发布任何 Release。".to_owned();
    }

    match exit_code {
        // curl 的退出码有明确含义（https://curl.se/docs/manpage.html#EXIT）。
        Some(6) => "域名解析失败（DNS 或代理配置？）—— 检查网络后再试。".to_owned(),
        Some(7) => "连不上 api.github.com（代理或防火墙？）—— 也可以直接去发布页看。".to_owned(),
        Some(28) => "请求超时：15 秒内没有收到响应。".to_owned(),
        Some(35) => "TLS 握手失败（可能有代理在中间拦截 HTTPS）。".to_owned(),
        Some(22) => format!("GitHub 返回了 HTTP 错误：{}", brief(text)),
        Some(code) => format!("curl 退出码 {code}：{}", brief(text)),
        None => format!("请求失败：{}", brief(text)),
    }
}

/// 把一段可能很长的输出压成一行、限长，给错误消息用。
fn brief(text: &str) -> String {
    // ⚠️ 原来这里是 `.replace('\r', " ").replace('\n', " ")` 再
    // `split_whitespace()`。那两步是**多余**的：`split_whitespace()`
    // 按定义就把 `\r` / `\n` / 连续空白全当成 ASCII 空白，行为一模一样
    // （clippy 的 `collapsible_str_replace` 报的就是它）。
    // 去掉之后语义不变，还少两次整串扫描 —— 这段输入可能是几十 KB 的 JSON。
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return "（没有更多信息）".to_owned();
    }
    let short: String = flat.chars().take(200).collect();
    if flat.chars().count() > 200 {
        format!("{short}…")
    } else {
        short
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brief_flattens_whitespace_and_caps_the_length() {
        // `brief` 出现在**每一条**更新检查错误消息里（HTTP 错 / curl 退出码 /
        // 请求失败），所以它的行为值得钉住。这条是改实现时补的：原先那两步
        // `\r`/`\n` → 空格 被 `split_whitespace()` 覆盖，去掉后必须等价。
        assert_eq!(brief("  a\r\nb  \tc  "), "a b c");
        // 多个空行 / 连续空白也压成一个
        assert_eq!(brief("a\r\n\r\n\r\n  b"), "a b");
        // 空 / 只有空白 → 一句占位（不要给用户一个空的错误消息）
        assert_eq!(brief(""), "（没有更多信息）");
        assert_eq!(brief("  \r\n\t "), "（没有更多信息）");
        // 超长按**字符**截断（按字节切会在多字节字符中间 panic）
        let long = "汉".repeat(260);
        let out = brief(&long);
        assert_eq!(out.chars().count(), 201, "200 个字符 + 一个省略号");
        assert!(out.ends_with('…'), "{out}");
        // 正好 200 个字符：不加省略号
        assert_eq!(brief(&"汉".repeat(200)).chars().count(), 200);
        // 多字节边界：200 个字符里最后一个不能是半个
        assert!(brief(&"汉".repeat(201)).starts_with("汉"));
    }

    /// 本机真实抓下来的 `releases/latest` 响应（2026-10-21，未认证，6635 字节）。
    ///
    /// 用真的而不是编的：编的 fixture 只会"正好符合我的解析器"。
    const REAL_RESPONSE: &str = include_str!("../tests/fixtures/github_release_latest.json");

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap_or_else(|| panic!("{text} 应该能解析"))
    }

    // -- 版本号 -----------------------------------------------------------

    #[test]
    fn parses_versions_with_and_without_prefix() {
        assert_eq!(v("v0.9.25"), v("0.9.25"));
        // 仓库里两种标签都出现过：一个裸 `0.1.0`，其余都是 `v` 开头。
        assert_eq!(v("0.1.0").to_string(), "0.1.0");
        assert_eq!(v("V1.2.3").to_string(), "1.2.3");
        assert_eq!(
            v("0.10.0-rc1").pre.as_deref(),
            Some("rc1"),
            "带预发布后缀要能解出来"
        );
        // 前后空白要吃掉（从 tag 列表里复制粘贴时会带）
        assert_eq!(v("  v0.9.25  "), v("0.9.25"));
    }

    #[test]
    fn rejects_malformed_versions() {
        for bad in [
            "", "  ", "v", "V", "1", "1.2", "1.2.3.4", "1.2.x", "1..3", ".1.2", "1.2.", "1.2.3-",
            "-1.2.3", "1.+3.0", "v0.9.25-", "abc",
        ] {
            assert!(Version::parse(bad).is_none(), "{bad:?} 不该被当成版本号");
        }
    }

    #[test]
    fn compares_numerically_not_lexically() {
        // 字符串比会得出 "0.9.10 < 0.9.9"（因为 '1' < '9'），这是这类 bug 的经典形态。
        assert!(is_newer(&v("0.9.10"), &v("0.9.9")));
        assert!(!is_newer(&v("0.9.9"), &v("0.9.10")));
    }

    #[test]
    fn major_wins_over_minor() {
        assert!(is_newer(&v("0.10.0"), &v("0.9.99")));
        assert!(!is_newer(&v("0.9.99"), &v("0.10.0")));
        assert!(is_newer(&v("1.0.0"), &v("0.99.99")));
    }

    #[test]
    fn prerelease_is_older_than_its_release() {
        // 我装 rc1、正式版发了 → 要提示
        assert!(is_newer(&v("0.10.0"), &v("0.10.0-rc1")));
        // 我装正式版、远端有个 rc2 → 不提示
        assert!(!is_newer(&v("0.10.0-rc2"), &v("0.10.0")));
    }

    #[test]
    fn equal_versions_are_not_newer() {
        assert!(!is_newer(&v("0.9.25"), &v("0.9.25")));
        assert!(!is_newer(&v("v0.9.25"), &v("0.9.25")));
    }

    // -- 真实响应 ---------------------------------------------------------

    #[test]
    fn parses_the_real_release_response() {
        let release = parse_release(REAL_RESPONSE).expect("真实响应必须能解析");

        assert_eq!(release.tag, "v0.9.25");
        assert_eq!(release.raw_version, "0.9.25");
        assert_eq!(release.version, v("0.9.25"));
        // `published_at` 是 `2026-10-19T03:58:13Z`，只留日期
        assert_eq!(release.published.as_deref(), Some("2026-10-19"));
        assert_eq!(
            release.page_url,
            "https://github.com/SilkKirk/wslc-panel/releases/tag/v0.9.25"
        );
        // 发布说明要留一段，但不能超过上限（+1 是省略号）
        assert!(!release.notes_head.is_empty(), "真实响应里有 body");
        assert!(release.notes_head.chars().count() <= NOTES_HEAD_CHARS + 1);
    }

    #[test]
    fn notes_head_is_truncated_by_chars_not_bytes() {
        // 全是中文：按字节切会把某个字劈成两半，String 会 panic。
        let body = "更".repeat(NOTES_HEAD_CHARS + 50);
        let release = parse_release(&format!(
            r#"{{"tag_name":"v1.0.0","html_url":"https://example.com/r","body":"{body}"}}"#
        ))
        .expect("应该能解析");
        assert_eq!(release.notes_head.chars().count(), NOTES_HEAD_CHARS + 1);
        assert!(release.notes_head.ends_with('…'));
    }

    #[test]
    fn null_body_gives_empty_notes_not_an_error() {
        let release = parse_release(
            r#"{"tag_name":"v1.0.0","html_url":"https://example.com/r","body":null}"#,
        )
        .expect("body 为 null 不该让整个解析失败");
        assert_eq!(release.notes_head, "");
        assert_eq!(release.published, None, "没有 published_at 时是 None");
    }

    #[test]
    fn rejects_prerelease_and_draft() {
        let pre = parse_release(
            r#"{"tag_name":"v1.0.0-rc1","html_url":"https://e.com","prerelease":true}"#,
        );
        assert!(pre.unwrap_err().contains("预发布"));

        let draft =
            parse_release(r#"{"tag_name":"v1.0.0","html_url":"https://e.com","draft":true}"#);
        assert!(draft.unwrap_err().contains("草稿"));
    }

    #[test]
    fn surfaces_githubs_own_message_when_there_is_no_tag() {
        // 404 的真实形状（本机抓的）
        let err = parse_release(
            r#"{"message":"Not Found","documentation_url":"https://docs.github.com/rest/releases/releases#get-the-latest-release","status":"404"}"#,
        )
        .unwrap_err();
        assert!(err.contains("Not Found"), "要把 GitHub 的原话带出来：{err}");
    }

    #[test]
    fn rejects_unparsable_tag_and_missing_url() {
        let bad_tag = parse_release(r#"{"tag_name":"nightly","html_url":"https://e.com"}"#);
        assert!(bad_tag.unwrap_err().contains("不是 X.Y.Z"));

        let no_url = parse_release(r#"{"tag_name":"v1.0.0"}"#);
        assert!(no_url.unwrap_err().contains("html_url"));
    }

    #[test]
    fn rejects_non_json_body() {
        let err = parse_release("<html>502 Bad Gateway</html>").unwrap_err();
        assert!(err.contains("不是 JSON"), "{err}");
    }

    // -- 失败翻译 ---------------------------------------------------------

    #[test]
    fn explains_rate_limit() {
        let text = r#"{"message":"API rate limit exceeded for 1.2.3.4.","documentation_url":"…"}"#;
        let message = explain_failure(Some(22), text);
        assert!(message.contains("配额"), "{message}");
    }

    #[test]
    fn explains_not_found() {
        let message = explain_failure(
            Some(22),
            r#"{"message":"Not Found","documentation_url":"…","status":"404"}"#,
        );
        assert!(message.contains("还没有发布"), "{message}");
    }

    #[test]
    fn explains_network_exit_codes() {
        assert!(explain_failure(Some(6), "").contains("域名"));
        assert!(explain_failure(Some(7), "").contains("连不上"));
        assert!(explain_failure(Some(28), "").contains("超时"));
        assert!(explain_failure(Some(35), "").contains("TLS"));
    }

    #[test]
    fn unknown_failure_keeps_the_original_text_and_is_bounded() {
        let long = "x".repeat(500);
        let message = explain_failure(Some(99), &long);
        assert!(message.contains("99"), "{message}");
        // 500 个字符要压到 200 以内，再加省略号和前缀
        assert!(message.chars().count() < 260, "{}", message.chars().count());

        let empty = explain_failure(None, "   ");
        assert!(empty.contains("没有更多信息"), "{empty}");
    }

    // -- curl 参数 ---------------------------------------------------------

    #[test]
    fn curl_arguments_are_stable() {
        let url = api_url();
        assert!(url.starts_with("https://api.github.com/repos/"), "{url}");
        assert!(url.ends_with("/releases/latest"), "{url}");

        let args = curl_args(&url);
        // URL 永远是**最后一个**位置参数（前面的都是选项）
        assert_eq!(args.last().map(String::as_str), Some(url.as_str()));
        // 这几条是行为的关键，别在重构时被悄悄删掉
        assert!(
            args.iter().any(|a| a.as_str() == "--fail-with-body"),
            "要能看到错误正文"
        );
        assert!(args.iter().any(|a| a.as_str() == "-m"), "必须有总超时");
        let ua = args
            .iter()
            .find(|a| a.starts_with("User-Agent: "))
            .expect("GitHub 强制要求 User-Agent");
        assert!(ua.starts_with("User-Agent: wslc-panel/"), "{ua}");
        assert!(
            args.iter()
                .any(|a| a.starts_with("Accept: application/vnd.github")),
            "指明媒体类型，避免将来 API 默认格式变化"
        );
    }
}
