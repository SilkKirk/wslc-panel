//! 镜像模型。

use serde::{Deserialize, Serialize};

use super::parse_size;

/// `wslc images --format json` 的一行。
///
/// ⚠️ **同一个镜像会出现多行**：不同 `Repository` 引用指向同一个 `ID`
/// （实机采集到 `docker.1panel.live/library/hello-world` 与 `hello-world`
/// 都是 `e2ac70e7319a`）。UI 若按镜像去重，应使用 `ID`；
/// 若按"可引用的名字"列表，则应使用 `Reference`。
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct ImageListItem {
    /// 12 位镜像 ID。
    #[serde(rename = "ID", default)]
    pub id: String,
    /// 仓库名，可能是 `<none>`。
    #[serde(rename = "Repository", default)]
    pub repository: String,
    /// 标签，可能是 `<none>`。
    #[serde(rename = "Tag", default)]
    pub tag: String,
    /// 摘要，通常是 `<none>`。
    #[serde(rename = "Digest", default)]
    pub digest: String,
    /// 绝对时间。
    #[serde(rename = "CreatedAt", default)]
    pub created_at: String,
    /// 相对时间。
    #[serde(rename = "CreatedSince", default)]
    pub created_since: String,
    /// `8.42MB`
    #[serde(rename = "Size", default)]
    pub size: String,
    /// 常为 `N/A`。
    #[serde(rename = "SharedSize", default)]
    pub shared_size: String,
    /// 常为 `N/A`。
    #[serde(rename = "UniqueSize", default)]
    pub unique_size: String,
    /// 使用该镜像的容器数（**数字字符串**）。
    #[serde(rename = "Containers", default)]
    pub containers: String,
}

impl ImageListItem {
    /// `Repository:Tag`，`<none>` 时回退到短 ID。
    pub fn reference(&self) -> String {
        let repo_is_none = self.repository.is_empty() || self.repository == "<none>";
        if repo_is_none {
            return self.short_id().to_owned();
        }
        let tag_is_none = self.tag.is_empty() || self.tag == "<none>";
        if tag_is_none {
            self.repository.clone()
        } else {
            format!("{}:{}", self.repository, self.tag)
        }
    }

    /// 12 位短 ID。
    ///
    /// ⚠️ `get(..n)` 而不是 `&self.id[..n]`：`len()` 是**字节**数，
    /// 第 12 个字节落在多字节字符中间时切片会 panic。
    /// 这类"解析出来的自由字符串"一律不能假设是 ASCII。
    pub fn short_id(&self) -> &str {
        self.id.get(..self.id.len().min(12)).unwrap_or(&self.id)
    }

    /// 是否为悬空镜像（没有仓库名也没有标签）。
    pub fn is_dangling(&self) -> bool {
        (self.repository.is_empty() || self.repository == "<none>")
            && (self.tag.is_empty() || self.tag == "<none>")
    }

    /// 体积（字节）。
    pub fn size_bytes(&self) -> Option<f64> {
        parse_size(&self.size)
    }

    /// 引用该镜像的容器数。
    pub fn container_count(&self) -> u32 {
        self.containers.trim().parse().unwrap_or(0)
    }
}

/// 一个镜像**声明**的端口与启动命令（`wslc image inspect` 里我们真正要用的那几项）。
///
/// 用途只有一个：**创建容器时把端口预填好**（[`suggest_port_mappings`]），
/// 并在表单上写一句"这个镜像到底会跑什么"。所以刻意只取这两项 ——
/// inspect 的输出有几十个字段，全建一遍就是一大堆没人用的代码
/// （字段字典见 `docs/wslc-schema.md`）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageFacts {
    /// 镜像声明要监听的**端口**（`ExposedPorts` 的键，只留 tcp，已排序去重）。
    pub exposed_ports: Vec<u16>,
    /// 它到底会跑什么：`Entrypoint` + `Cmd` 拼起来，给人看的一句话。
    pub command: String,
}

/// 解析 `wslc image inspect <ref> --format json` 的输出。
///
/// ⚠️ 形状有**两处**不一致，两种都得认（实测 WSL 3.0.3.0）：
///
/// 1. 带 `--format json` 时是**单行数组**（`inspect` 接受多个镜像），
///    不带 `--format` 时是**缩进的对象** —— 两种都收，取第一个；
/// 2. `Cmd` / `Entrypoint` 可能是字符串数组，也可能是单个字符串（老镜像），
///    还可能整个缺失。
///
/// 空数组（镜像不在本地时 `wslc` 会打 `[]`）**算失败**：与其返回一个
/// "什么都没有"的 facts 让界面显示"这个镜像没有端口"，不如给一句能看的原因。
pub fn parse_image_inspect(json: &str) -> Result<ImageFacts, String> {
    let clean = json.trim_start_matches('\u{feff}').trim();
    if clean.is_empty() {
        return Err("image inspect 返回了空内容".to_owned());
    }
    let value: serde_json::Value =
        serde_json::from_str(clean).map_err(|e| format!("image inspect 返回的不是 JSON：{e}"))?;
    let value = match value {
        serde_json::Value::Array(items) => items
            .into_iter()
            .next()
            .ok_or_else(|| "image inspect 返回了空数组（镜像不在本地？）".to_owned())?,
        other => other,
    };

    let config = value.get("Config");
    let mut exposed_ports = Vec::new();
    if let Some(map) = config
        .and_then(|config| config.get("ExposedPorts"))
        .and_then(serde_json::Value::as_object)
    {
        for key in map.keys() {
            // 键的形状是 `80/tcp`（协议也可能缺省）。
            // **只收 tcp**：`-p` 的预填是"建议"，udp 让用户自己写 ——
            // 我们不去猜他想不想映射一个 DNS 端口。
            let (port, proto) = match key.split_once('/') {
                Some((port, proto)) => (port, proto),
                None => (key.as_str(), "tcp"),
            };
            if !proto.eq_ignore_ascii_case("tcp") {
                continue;
            }
            if let Ok(port) = port.trim().parse::<u16>() {
                exposed_ports.push(port);
            }
        }
    }
    exposed_ports.sort_unstable();
    exposed_ports.dedup();

    // `Entrypoint` 在前、`Cmd` 在后 —— 和容器实际跑的那条命令同序。
    let mut parts: Vec<String> = Vec::new();
    for field in ["Entrypoint", "Cmd"] {
        parts.extend(command_parts(config.and_then(|config| config.get(field))));
    }

    Ok(ImageFacts {
        exposed_ports,
        command: parts.join(" "),
    })
}

/// 把 `Cmd` / `Entrypoint` 的两种形状都读成字符串片段。
///
/// 单个字符串**不按空白切开**：`"/bin/sh -c 'a b'"` 里那个引号是有意义的，
/// 切开只会把命令展示错（这里只用来给人看，不拿去执行）。
fn command_parts(value: Option<&serde_json::Value>) -> Vec<String> {
    match value {
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.as_str().map(str::to_owned))
            .collect(),
        Some(serde_json::Value::String(text)) => vec![text.clone()],
        _ => Vec::new(),
    }
}

/// 从镜像声明的端口推一个**建议**的 `-p` 映射（`8080:80, 8443:443`）。
///
/// 规则刻意简单到可以预期：**特权端口**（`< 1024`）映射到 `8000 + 端口`
/// —— 于是 80 → 8080、443 → 8443（都是约定俗成的那个数），
/// 而 1024 以上保持原样（3000 → 3000）。撞了就往后挪一格
/// （镜像同时暴露 80 和 8080 时会算出两个 8080）。
///
/// 没有声明端口时返回空串 —— 调用方据此**不预填**，别写一个空的 `-p`。
pub fn suggest_port_mappings(ports: &[u16]) -> String {
    let mut used: Vec<u16> = Vec::new();
    let mut out: Vec<String> = Vec::new();
    for &port in ports {
        let mut host = if port < 1024 {
            8000 + port
        } else {
            port
        };
        while used.contains(&host) {
            host = host.saturating_add(1);
        }
        used.push(host);
        out.push(format!("{host}:{port}"));
    }
    out.join(", ")
}

/// 从 `wslc images` 的结果里挑出**可用的镜像引用**（去重、排序、排除悬空）。
///
/// 给"创建容器时从本地镜像里选"用：`cmd::image::local_references` 与面板的
/// 下拉框都走这里 —— 面板直接用快照里的 `images`（每 3 秒采集一次），
/// **不额外起一个 `wslc`**。
///
/// ⚠️ 必须按 `reference()` 去重：同一个镜像 ID 会有多个 `Repository` 引用
/// （实测 `docker.1panel.live/library/hello-world:latest` 与 `hello-world:latest`
/// 是同一个 `e2ac70e7319a`），而用户要选的是"能拿去 run 的那个名字"。
pub fn local_references(items: &[ImageListItem]) -> Vec<String> {
    let mut refs: Vec<String> = items
        .iter()
        .filter(|item| !item.is_dangling())
        .map(ImageListItem::reference)
        .collect();
    refs.sort();
    refs.dedup();
    refs
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../tests/fixtures/images.jsonl");
    /// 真机抓的 `wslc image inspect --format json`（nginx：声明了 80/tcp）。
    const INSPECT_NGINX: &str = include_str!("../../tests/fixtures/image_inspect_nginx.json");
    /// 同上，`hello-world`：**没有** ExposedPorts，只有一条 Cmd。
    const INSPECT_HELLO: &str = include_str!("../../tests/fixtures/image_inspect_hello_world.json");

    #[test]
    fn local_references_excludes_dangling_and_dedups() {
        // 这份 fixture 里 hello-world 同一个 ID 有两个引用 —— **两个都留着**
        // （它们是两个都能 `run` 的名字），而 `<none>` 那类悬空镜像要排除。
        let items: Vec<ImageListItem> = crate::jsonl::parse_lines(FIXTURE).unwrap();
        assert_eq!(
            local_references(&items),
            vec![
                "docker.1ms.run/library/alpine:latest".to_owned(),
                "docker.1panel.live/library/hello-world:latest".to_owned(),
                "hello-world:latest".to_owned(),
            ]
        );
        // 一个镜像都没有时不该炸（下拉框会显示占位符）
        assert!(local_references(&[]).is_empty());
    }

    #[test]
    fn parses_real_inspect_output_of_an_image_with_ports() {
        let facts = parse_image_inspect(INSPECT_NGINX).expect("应能解析");
        assert_eq!(facts.exposed_ports, vec![80]);
        // `Entrypoint` + `Cmd` 按顺序拼起来
        assert_eq!(facts.command, "/docker-entrypoint.sh nginx -g daemon off;");
        assert_eq!(suggest_port_mappings(&facts.exposed_ports), "8080:80");
    }

    #[test]
    fn parses_an_image_without_exposed_ports() {
        let facts = parse_image_inspect(INSPECT_HELLO).expect("应能解析");
        assert!(facts.exposed_ports.is_empty(), "{facts:?}");
        assert_eq!(facts.command, "/hello");
        // 没有声明端口就**不预填**（空串），别写一个空的 `-p`
        assert_eq!(suggest_port_mappings(&facts.exposed_ports), "");
    }

    #[test]
    fn accepts_the_object_shape_too() {
        // 不带 `--format json` 时 `wslc` 给的是对象（不是数组）——
        // 实测两种形状都出现过，所以两种都要认。
        let array: serde_json::Value = serde_json::from_str(INSPECT_NGINX).unwrap();
        let single = array[0].clone();
        let facts = parse_image_inspect(&single.to_string()).expect("对象形状也要能解析");
        assert_eq!(facts.exposed_ports, vec![80]);
        assert_eq!(facts.command, "/docker-entrypoint.sh nginx -g daemon off;");
    }

    #[test]
    fn tolerates_legacy_command_shapes() {
        // 单字符串的 Cmd / Entrypoint（老镜像），以及缺字段
        let json = r#"{"Config":{"Cmd":"nginx -g 'daemon off;'","Entrypoint":"/init"}}"#;
        let facts = parse_image_inspect(json).unwrap();
        assert_eq!(facts.command, "/init nginx -g 'daemon off;'");
        assert!(facts.exposed_ports.is_empty());

        let facts = parse_image_inspect(r#"{"Config":{}}"#).unwrap();
        assert_eq!(facts.command, "");
        assert!(facts.exposed_ports.is_empty());
        // 连 Config 都没有
        let facts = parse_image_inspect(r#"{"Id":"x"}"#).unwrap();
        assert_eq!(facts.command, "");
    }

    #[test]
    fn exposes_ports_without_protocol_and_skips_udp() {
        // 键可能是 `80`（无协议）或 `53/udp`；只收 tcp，非数字直接丢
        let json = r#"{"Config":{"ExposedPorts":{"80":{},"53/udp":{},"8080/tcp":{},"x/tcp":{}}}}"#;
        let facts = parse_image_inspect(json).unwrap();
        assert_eq!(facts.exposed_ports, vec![80, 8080], "udp 与非数字都该被丢掉");
    }

    #[test]
    fn rejects_empty_or_broken_output() {
        // 镜像不在本地时 `wslc` 打 `[]` 并以退出码 1 结束；真走到这里也要报错，
        // 而不是给一个"这个镜像没有端口"的假结论。
        let err = parse_image_inspect("[]").unwrap_err();
        assert!(err.contains("空数组"), "{err}");
        assert!(parse_image_inspect("").is_err());
        assert!(parse_image_inspect("not json").is_err());
    }

    #[test]
    fn ports_are_sorted_and_deduped() {
        let json = r#"{"Config":{"ExposedPorts":{"443/tcp":{},"80/tcp":{},"80/tcp ":{},"80/tcp":{}}}}"#;
        let facts = parse_image_inspect(json).unwrap();
        assert_eq!(facts.exposed_ports, vec![80, 443]);
    }

    #[test]
    fn suggested_mappings_avoid_collisions() {
        assert_eq!(suggest_port_mappings(&[]), "");
        // 特权端口 → 8000+，其余保持原样
        assert_eq!(suggest_port_mappings(&[80]), "8080:80");
        assert_eq!(suggest_port_mappings(&[80, 443]), "8080:80, 8443:443");
        assert_eq!(suggest_port_mappings(&[3000]), "3000:3000");
        assert_eq!(suggest_port_mappings(&[1024]), "1024:1024");
        // 撞了往后挪：80 → 8080，而镜像自己暴露的 8080 只能落到 8081
        assert_eq!(suggest_port_mappings(&[80, 8080]), "8080:80, 8081:8080");
    }

    #[test]
    fn parses_real_images_output() {
        let items: Vec<ImageListItem> = crate::jsonl::parse_lines(FIXTURE).unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].id, "320994c3b997");
        assert_eq!(items[0].repository, "docker.1ms.run/library/alpine");
        assert_eq!(items[0].tag, "latest");
        assert_eq!(items[0].digest, "<none>");
        assert_eq!(items[0].size, "8.42MB");
        assert_eq!(items[0].reference(), "docker.1ms.run/library/alpine:latest");
        assert_eq!(items[0].container_count(), 0);
        assert!(!items[0].is_dangling());
    }

    #[test]
    fn same_image_appears_under_multiple_references() {
        let items: Vec<ImageListItem> = crate::jsonl::parse_lines(FIXTURE).unwrap();
        let hello: Vec<_> = items.iter().filter(|i| i.id == "e2ac70e7319a").collect();
        assert_eq!(hello.len(), 2, "同一镜像 ID 应出现两次（不同 Repository）");
        // 注意：`hello` 是 `Vec<&ImageListItem>`，所以 `hello.iter()` 给出的是
        // `&&ImageListItem`，不能直接把 `ImageListItem::reference` 当函数用。
        let refs: Vec<String> = hello.iter().map(|i| i.reference()).collect();
        assert!(refs.contains(&"docker.1panel.live/library/hello-world:latest".to_owned()));
        assert!(refs.contains(&"hello-world:latest".to_owned()));
    }

    #[test]
    fn dangling_detection() {
        let img = ImageListItem {
            id: "abc123def456".into(),
            repository: "<none>".into(),
            tag: "<none>".into(),
            ..Default::default()
        };
        assert!(img.is_dangling());
        assert_eq!(img.reference(), "abc123def456");
    }

    #[test]
    fn size_parsing() {
        let img = ImageListItem {
            size: "10.1kB".into(),
            ..Default::default()
        };
        let bytes = img.size_bytes().expect("应能解析");
        assert!((bytes - 10_100.0).abs() < 1e-6, "实际 {bytes}");
    }

    #[test]
    fn missing_fields_do_not_panic() {
        let img: ImageListItem = serde_json::from_str("{}").unwrap();
        assert_eq!(img.reference(), "");
        assert_eq!(img.container_count(), 0);
        assert_eq!(img.size_bytes(), None);
    }
}
