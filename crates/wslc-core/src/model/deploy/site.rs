//! nginx 站点配置：conf 生成、代理规则、server_name、DNS 与网络。

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

/// 一份 nginx 站点配置（只有 nginx 内容形态会有）。
///
/// 字段都是**扁平的字符串**，和 [`crate::model::deploy`] 里其它存档类型同一条
/// 原则：`deployments.json` 是能手改、能看懂的。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploySite {
    /// `server_name`。默认 `localhost`（端口 80 上靠它接管 `default.conf`）。
    #[serde(default = "default_server_name")]
    pub server_name: String,
    /// 单页应用回退（`try_files $uri $uri/ /index.html`）。
    #[serde(default)]
    pub spa: bool,
    /// 反代规则。P0 只有 0 或 1 条，结构上留成 `Vec`。
    #[serde(default)]
    pub proxies: Vec<ProxyRule>,
    /// 容器里的配置文件。空串 = 用 [`conf_path`] 重算。
    #[serde(default)]
    pub conf: String,
}

fn default_server_name() -> String {
    "localhost".to_owned()
}

/// 一条反向代理规则。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyRule {
    /// 路径前缀，例如 `/api/`（[`valid_proxy_prefix`] 把关）。
    pub prefix: String,
    /// 已规范化，例如 `http://api:8080`（[`normalize_upstream`] 把关）。
    pub upstream: String,
    /// 转发时去掉 `prefix`（后端收到 `/foo` 而不是 `/api/foo`）。
    #[serde(default)]
    pub strip_prefix: bool,
}

/// 配置文件名：`00-` 让它字典序排在最前（端口 80 上成为默认 server）。
pub fn conf_file_name(app: &str) -> String {
    format!("00-wslc-panel-{app}.conf")
}

/// 容器内的配置文件路径。
pub fn conf_path(app: &str) -> String {
    format!("/etc/nginx/conf.d/{}", conf_file_name(app))
}

/// 配置文件首行的标记 —— 覆盖/删除前靠它确认"这是面板生成的"。
pub fn conf_marker(app: &str) -> String {
    format!("# wslc-panel:site:{app}")
}

/// 生成一份 `server{}` 块。
///
/// # 几个**必须**这么写的点（都实测过）
///
/// - `resolver` 放**这个 `server` 里**，不能放文件顶层：多应用时第二个文件
///   再写一次 http 级的 `resolver` 会 `"resolver" directive is duplicate`
///   （`nginx -t` 直接失败）。
/// - 反代上游用**变量 + `$request_uri`**，不用静态 `proxy_pass http://api:8080`：
///   后端容器停着时名字解析不了，静态写法会让 `nginx -t` / reload / 重启全失败；
///   变量写法只让请求 502，后端起来后自动恢复。
/// - 去前缀用**正则 location**：`rewrite ... break` + 变量会 500，
///   `proxy_pass $u;`（变量带末尾 `/`）会丢后缀 —— 实测只有正则这一种能用。
/// - `root` 要加引号：目标目录可能带空格。
pub fn site_conf(
    app: &str,
    site: &DeploySite,
    root: &str,
    listen: u16,
    resolver: Option<&str>,
) -> String {
    let server_name = {
        let name = site.server_name.trim();
        if !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        {
            name
        } else {
            "localhost"
        }
    };

    let mut out = String::new();
    // 首行**必须**恰好是标记：`apply_site` / `remove_site` 读首行做等值比较，
    // 多一个后缀就会被当成"不是面板的文件"而拒绝覆盖。
    let _ = writeln!(out, "{}", conf_marker(app));
    let _ = writeln!(out, "# 由面板生成，重新部署会整份覆盖");
    let _ = writeln!(out, "server {{");
    let _ = writeln!(out, "    listen {listen};");
    let _ = writeln!(out, "    server_name {server_name};");
    if !site.proxies.is_empty() {
        if let Some(addr) = resolver.map(str::trim).filter(|addr| !addr.is_empty()) {
            let _ = writeln!(out, "    resolver {addr} ipv6=off valid=5s;");
        }
    }
    let _ = writeln!(out, "    root \"{}\";", nginx_quote(root));
    let _ = writeln!(out, "    index index.html;");
    let _ = writeln!(out);

    if site.spa {
        let _ = writeln!(out, "    location / {{");
        let _ = writeln!(out, "        try_files $uri $uri/ /index.html;");
        let _ = writeln!(out, "    }}");
        let _ = writeln!(out);
    }

    for (index, rule) in site.proxies.iter().enumerate() {
        let var = format!("wslc_upstream_{}", index + 1);
        if rule.strip_prefix {
            let _ = writeln!(out, "    location ~ ^{}(.*)$ {{", rule.prefix);
            let _ = writeln!(
                out,
                "        set ${var} \"{}\";",
                nginx_quote(&rule.upstream)
            );
            let _ = writeln!(out, "        proxy_pass ${var}/$1$is_args$args;");
        } else {
            let _ = writeln!(out, "    location {} {{", rule.prefix);
            let _ = writeln!(
                out,
                "        set ${var} \"{}\";",
                nginx_quote(&rule.upstream)
            );
            let _ = writeln!(out, "        proxy_pass ${var}$request_uri;");
        }
        let _ = writeln!(out, "        proxy_http_version 1.1;");
        let _ = writeln!(out, "        proxy_set_header Host $host;");
        let _ = writeln!(out, "        proxy_set_header X-Real-IP $remote_addr;");
        let _ = writeln!(
            out,
            "        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;"
        );
        let _ = writeln!(out, "        proxy_set_header X-Forwarded-Proto $scheme;");
        let _ = writeln!(out, "    }}");
        let _ = writeln!(out);
    }

    let _ = writeln!(out, "}}");
    out
}

/// 把 `root` / 上游这类要进 `"…"` 的字符串转义成 nginx 能吃的形状。
///
/// `$` 也要转义：nginx 在双引号里**仍然**会做变量替换，路径里带 `$` 会被吃掉。
fn nginx_quote(value: &str) -> String {
    // ⚠️ **替换顺序有依赖，别合并这几个**：先转义 `\` 再转义 `"`，
    // 否则 `"` 变成的 `\"` 会被第一步再加一层（`\\"`）。
    // 只有最后两个可以合并 —— 它们互不影响、都是"删掉"。
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('$', "\\$")
        .replace(['\r', '\n'], "")
}

/// 规范化反代上游：接受 `host:port` / `http://host:port` / `https://host:port`。
///
/// 主机只允许 `[A-Za-z0-9._-]`、端口 1~65535；不允许路径 / 空格 / `;` / `{}` /
/// `$` / 换行 —— 这些能直接拼出第二条配置。
pub fn normalize_upstream(raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("反代上游不能为空".to_owned());
    }
    let (scheme, rest) = if let Some(rest) = raw.strip_prefix("http://") {
        ("http", rest)
    } else if let Some(rest) = raw.strip_prefix("https://") {
        ("https", rest)
    } else {
        ("http", raw)
    };
    let (host, port) = rest
        .rsplit_once(':')
        .ok_or_else(|| "上游要写成 host:port（例如 http://api:8080）".to_owned())?;
    if host.is_empty()
        || !host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    {
        return Err(format!("上游主机名不合法：{host}"));
    }
    let port: u16 = port
        .parse()
        .map_err(|_| "上游端口要是 1~65535 的数字".to_owned())?;
    if port == 0 {
        return Err("上游端口不能是 0".to_owned());
    }
    Ok(format!("{scheme}://{host}:{port}"))
}

/// 校验路径前缀：`/api/` 这样（`/` 开头、`/` 结尾、只允许安全字符）。
pub fn valid_proxy_prefix(raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    if raw.len() < 2 || !raw.starts_with('/') || !raw.ends_with('/') {
        return Err("路径前缀要像 /api/ 这样（以 / 开头、以 / 结尾）".to_owned());
    }
    if !raw
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_'))
    {
        return Err("路径前缀只允许字母 / 数字 / 斜杠 / 下划线 / 连字符".to_owned());
    }
    if raw.contains("//") {
        return Err("路径前缀里不能有 //".to_owned());
    }
    Ok(raw.to_owned())
}

/// 从一个已规范化的上游里取主机名（给网络检查/测试用）。
pub fn upstream_host(upstream: &str) -> &str {
    let rest = upstream
        .strip_prefix("http://")
        .or_else(|| upstream.strip_prefix("https://"))
        .unwrap_or(upstream);
    rest.rsplit_once(':').map(|(host, _)| host).unwrap_or(rest)
}

/// 校验 `server_name`（单个主机名；空串按 `localhost`）。
///
/// 不开放空格分隔的多名字：表单只有一格，多名字放进来既难校验也难解释；
/// 真要多个域名，等 P1 的列表 UI。
pub fn valid_server_name(raw: &str) -> Result<String, String> {
    let name = raw.trim();
    if name.is_empty() {
        return Ok("localhost".to_owned());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    {
        return Err("server_name 只允许字母 / 数字 / 点 / 连字符 / 下划线".to_owned());
    }
    Ok(name.to_owned())
}

/// 把一条反代规则格式化成「更多反代」那一格的写法（预填用）。
pub fn format_proxy_rule(rule: &ProxyRule) -> String {
    let mut out = format!("{} => {}", rule.prefix, rule.upstream);
    if rule.strip_prefix {
        out.push_str(" strip");
    }
    out
}

/// 解析「更多反代」：`/api/ => http://api:8080; /auth/ => http://auth:9000 strip`
///
/// 用 `;` 分隔（单行输入框里最稳；上游本身不允许 `;`，不会有歧义）。
pub fn parse_proxy_rules(raw: &str) -> Result<Vec<ProxyRule>, String> {
    let mut out = Vec::new();
    for part in raw
        .split(';')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        let Some((prefix, rest)) = part.split_once("=>") else {
            return Err(format!("这条反代看不懂（要 /path/ => host:port）：{part}"));
        };
        let prefix = valid_proxy_prefix(prefix)?;
        let mut tokens = rest.split_whitespace();
        let upstream = tokens
            .next()
            .ok_or_else(|| format!("反代「{part}」缺少上游"))?;
        let upstream = normalize_upstream(upstream)?;
        let mut strip_prefix = false;
        for token in tokens {
            if token.eq_ignore_ascii_case("strip") {
                strip_prefix = true;
            } else {
                return Err(format!("反代「{part}」里多了一个看不懂的词：{token}"));
            }
        }
        out.push(ProxyRule {
            prefix,
            upstream,
            strip_prefix,
        });
    }
    Ok(out)
}

/// 从 `/etc/resolv.conf` 取第一个 `nameserver`（nginx 的 `resolver` 要用它）。
pub fn first_nameserver(resolv_conf: &str) -> Option<String> {
    for line in resolv_conf.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("nameserver") else {
            continue;
        };
        if !rest.chars().next().is_some_and(char::is_whitespace) {
            continue;
        }
        let addr = rest.split_whitespace().next()?;
        if !addr.is_empty()
            && addr
                .chars()
                .all(|c| c.is_ascii_hexdigit() || matches!(c, '.' | ':'))
        {
            return Some(addr.to_owned());
        }
    }
    None
}

/// 去掉 nginx 的**预期噪音**（`conflicting server name "localhost" ... ignored`）。
///
/// 这句是"两个 `server_name localhost` 在同一个端口"的必然结果（端口 80 接管
/// `default.conf` 时会出现），exit code 仍然是 0。页头「重载 nginx」会原样弹
/// 输出，不过滤的话每次重载都像报了个错。
pub fn filter_nginx_noise(text: &str) -> String {
    text.lines()
        .filter(|line| !line.contains("conflicting server name"))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

/// `nginx -t` 的脚本（`2>&1` 让错误进 stderr 合并后的输出）。
pub fn nginx_test_script() -> String {
    "nginx -t 2>&1".to_owned()
}

/// 面板自己建的网络名：两个容器没有共同用户网络时用它。
pub const PANEL_NETWORK: &str = "wslc-panel";

/// 两个容器**共同的用户自定义网络**（`bridge` / `host` / `none` 不算）。
///
/// 默认 `bridge` 上容器名解析不了（实测 `getent` rc=2，`host.docker.internal`
/// 也不通），所以反代的上游名字要么本来就有一个共同用户网络，要么得建/接一个。
/// 返回 `left` 里第一个也出现在 `right` 里的名字 —— 顺序稳定，测试可预期。
pub fn common_user_network(left: &[String], right: &[String]) -> Option<String> {
    left.iter()
        .filter(|name| is_user_network(name.as_str()))
        .find(|name| right.iter().any(|other| other == *name))
        .cloned()
}

fn is_user_network(name: &str) -> bool {
    !matches!(name.trim(), "" | "bridge" | "host" | "none")
}
