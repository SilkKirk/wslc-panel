//! 建议应用名 / 端口 / 默认目标目录，以及目标目录重叠的判定。

use std::collections::BTreeSet;

use super::*;

/// 新应用的默认名字：`app` / `app-2` / `app-3` ……
///
/// # 为什么需要它
///
/// 「添加部署」以前永远预填 [`DEFAULT_APP_NAME`]。同一个容器里再点一次，应用名
/// 还是 `app` —— `DeployStore::put` 按 `<容器>/<应用>` 覆盖，于是**第二条部署
/// 会静默替换第一条**，看起来像"部署成功，但原来那个不见了"。
///
/// `app-2` 这种带后缀的名字既满足 [`valid_app_name`]，又让"第二条"一眼可读。
/// 认不出来时（理论上到不了）返回 `app`。
pub fn suggest_app_name(existing: &[String]) -> String {
    let used: BTreeSet<&str> = existing
        .iter()
        .map(|name| name.trim())
        .filter(|name| !name.is_empty())
        .collect();
    if !used.contains(DEFAULT_APP_NAME) {
        return DEFAULT_APP_NAME.to_owned();
    }
    let mut n: u32 = 2;
    while n < u32::MAX {
        let candidate = format!("{DEFAULT_APP_NAME}-{n}");
        if !used.contains(candidate.as_str()) {
            return candidate;
        }
        n += 1;
    }
    DEFAULT_APP_NAME.to_owned()
}

/// 新应用的默认端口。
///
/// 优先用**已发布到主机、且没被记录占用**的容器端口（升序里第一个）；
/// 都占了就退回 `default` 往上找第一个没被占用的 —— 这时候**不保证已发布**，
/// 调用方要据此提醒用户"只能在容器内访问"。
///
/// `existing` 只应该包含**真的会 bind** 的端口（见
/// `wslc_panel_core::deploys::DeployStore::binding_ports`）：nginx 站点关的
/// 记录不监听端口，不该占掉一个号。
pub fn suggest_app_port(existing: &[u16], published: &[u16], default: u16) -> u16 {
    let used: BTreeSet<u16> = existing.iter().copied().filter(|p| *p > 0).collect();

    let mut published: Vec<u16> = published.iter().copied().filter(|p| *p > 0).collect();
    published.sort_unstable();
    published.dedup();
    for port in published {
        if !used.contains(&port) {
            return port;
        }
    }

    let mut port = default.max(1);
    while used.contains(&port) {
        match port.checked_add(1) {
            Some(next) => port = next,
            // 65535 都占满了：返回 default，让上层校验/警告去说。
            None => return default.max(1),
        }
    }
    port
}

/// `inspect.Ports` → **已发布的容器 TCP 端口**（升序、去重）。
///
/// 三条判据：
/// - 键必须是 `<端口>/tcp`（应用监听 TCP；`/udp` 不算）；
/// - **必须有 bindings**：`inspect` 在"容器声明了端口但没映射"时给的是空数组；
/// - 端口 0 丢掉。
pub fn published_container_ports(ports: &PortMap) -> Vec<u16> {
    let mut out: Vec<u16> = ports
        .iter()
        .filter(|item| item.0.ends_with("/tcp") && !item.1.is_empty())
        .filter_map(|item| item.0.split('/').next()?.parse::<u16>().ok())
        .filter(|port| *port > 0)
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// 主机上能打开这个应用的 URL（`None` = 这个容器端口没发布）。
///
/// `127.0.0.1` / `0.0.0.0` / `::` / `::1` / 空都算"本机" → 用 `localhost`：
/// 实测 `wslc` 的发布绑在 **`127.0.0.1`** 上（不是 `0.0.0.0`），直接拿它拼
/// URL 也能用，但用户看着会以为是外网地址。
pub fn app_url(ports: &PortMap, container_port: u16) -> Option<String> {
    let key = format!("{container_port}/tcp");
    let (_, bindings) = ports.iter().find(|item| item.0.as_str() == key.as_str())?;
    let (host, port) = bindings.first()?;
    let host = match host.trim() {
        "" | "0.0.0.0" | "127.0.0.1" | "::" | "::1" => "localhost",
        other => other,
    };
    let port = port.trim();
    if port.is_empty() {
        return None;
    }
    Some(format!("http://{host}:{port}"))
}

/// 新应用的默认目标目录。
///
/// - nginx：`<web_root>/<app>`（**不是 Web 根本身** —— 多应用必须一应用一个
///   子目录，否则 `removals_script` 的逐名删除会把另一个应用的同名文件删掉）；
///   `web_root` 推不出来时用官方镜像的 `/usr/share/nginx/html`。
/// - 其余：`/opt/<app>`（和 [`AppRuntime::default_target_dir`] 一致）。
pub fn default_app_target(runtime: AppRuntime, app: &str, web_root: Option<&str>) -> String {
    match runtime {
        AppRuntime::Nginx => {
            let root = web_root
                .map(str::trim)
                .filter(|root| !root.is_empty())
                .unwrap_or("/usr/share/nginx/html");
            container_join(root, app)
        }
        _ => format!("/opt/{app}"),
    }
}

/// 两个目标目录的**方向**关系。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetOverlap {
    /// 完全同一个目录。
    Same,
    /// 新目标在已有目标**里面**（`/root/app2` vs `/root`）—— 迁移时要允许。
    NewInsideExisting,
    /// 新目标是已有目标的**父目录**（`/root` vs `/root/app2`）—— 危险。
    ExistingInsideNew,
}

/// 比较"新目标"和"已有目标"的方向关系；没有重叠返回 `None`。
///
/// 只看路径边界：`/opt/a` 和 `/opt/ab` **没有**重叠。
/// 尾部 `/` 会归一化，所以 `/opt/a/` 和 `/opt/a` 是 [`TargetOverlap::Same`]。
pub fn target_overlap(new_target: &str, existing_target: &str) -> Option<TargetOverlap> {
    let new = normalize_target(new_target);
    let old = normalize_target(existing_target);
    if new == old {
        return Some(TargetOverlap::Same);
    }
    if is_under(new, old) {
        return Some(TargetOverlap::NewInsideExisting);
    }
    if is_under(old, new) {
        return Some(TargetOverlap::ExistingInsideNew);
    }
    None
}

/// 去掉尾部 `/`；空串（或全 `/`）归一成 `/`。
///
/// `pub` 是因为 `wslc-panel-core::deploys` 的目录冲突判定要用**同一个**
/// 归一化：那边原先自己抄了一份逐字相同的实现，两份一旦漂，"同一个目录"
/// 在面板的存档层和模型层就会给出不同答案。
pub fn normalize_target(path: &str) -> &str {
    let trimmed = path.trim().trim_end_matches('/');
    if trimmed.is_empty() { "/" } else { trimmed }
}

/// `path` 是不是在 `parent` 里面（按路径边界）。
///
/// 同上：`pub` 给 `wslc-panel-core::deploys` 复用。
pub fn is_under(path: &str, parent: &str) -> bool {
    if parent == "/" {
        return path != "/" && path.starts_with('/');
    }
    path.starts_with(parent) && path.as_bytes().get(parent.len()) == Some(&b'/')
}

/// `path` 在 `parent` 底下的**第一段**（两个都先按 [`normalize_target`] 归一）。
///
/// 返回 `None` = `path` 不在 `parent` 底下，或者底下那一段是空的。
///
/// 原先这是 `wslc-panel-core::deploys` 的私有函数，而它依赖的
/// `normalize_target` 在那里也有一份副本 —— 现在归到规则本身该在的地方。
pub fn first_segment_under<'a>(path: &'a str, parent: &str) -> Option<&'a str> {
    let path = normalize_target(path);
    let parent = normalize_target(parent);
    let rest = if parent == "/" {
        path.strip_prefix('/')?
    } else {
        path.strip_prefix(parent)?.strip_prefix('/')?
    };
    rest.split('/').next().filter(|name| !name.is_empty())
}
