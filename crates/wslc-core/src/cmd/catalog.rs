//! 拉「在线发行版」清单：两个 HTTP 请求，都走 `curl.exe`。
//!
//! ```text
//! GET https://api1.wslui.com/desktop/v1/helper/install   → 清单地址
//! GET <那个地址>                                          → 发行版 + 镜像列表
//! ```
//!
//! # 为什么要问两次
//!
//! 清单地址会变（换 CDN、换域名），所以它由接口下发；
//! [`mirrors::CATALOG_FALLBACK_URL`] 是那个地址的兜底 ——
//! 第一个请求失败时直接用它，少一次失败机会。
//!
//! # 为什么这一段不在 `mirrors.rs`
//!
//! 仓库的约定是**解析放 `model/`、跑进程放 `cmd/`**（`AGENTS.md` §5）：
//! `mirrors.rs` 里全是纯函数（能脱离 Windows 单测），而这里会真的起 `curl.exe`。
//!
//! ⚠️ **阻塞**：最长两次 25 秒。只能在后台执行器上调用（`AGENTS.md` §7.1）。
//!
//! ⚠️ 系统代理：本机的 `curl.exe` **不读** Windows 的代理设置（实测见
//! `mirrors.rs` 模块说明），所以这里按直连设计 —— 实测两个接口直连都通。

use std::time::Duration;

use crate::cli;
use crate::mirrors::{self, Offer};

/// 清单的来源，界面要如实显示"这份清单是哪儿来的"。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CatalogOrigin {
    /// 走接口下发的地址拿到的（正常情况）。
    Discovered,
    /// 接口挂了，用了写死的兜底地址。
    FallbackUrl,
    /// 没拿到。
    #[default]
    None,
}

impl CatalogOrigin {
    /// 一句话说明。
    pub fn label(self) -> &'static str {
        match self {
            Self::Discovered => "清单来自 wslui 接口（api1 → api2）",
            Self::FallbackUrl => "接口没答上，用了内置的清单地址（api2）",
            Self::None => "",
        }
    }
}

/// 拉回来的清单。
#[derive(Debug, Clone)]
pub struct Catalog {
    /// 发行版（某一版一条）。
    pub offers: Vec<Offer>,
    /// 这份清单是哪儿来的。
    pub origin: CatalogOrigin,
    /// 清单的更新时间（接口给的，界面上显示）。
    pub updated: Option<String>,
}

/// 拉清单（**阻塞**）。
///
/// 错误全部是"能给用户看的一句话"：接口不通、返回的不是 JSON、
/// 业务错误码、架构不认识……界面直接显示它，并指向"自定义 URL"那条出口。
pub fn fetch() -> Result<Catalog, String> {
    let arm64 = match mirrors::arch() {
        "amd64" => false,
        "arm64" => true,
        other => {
            return Err(format!(
                "清单里只有 amd64 / arm64 两份，这台机器是 {other} —— 请用「自定义 URL」"
            ));
        }
    };

    // 1) 先问清单地址。**问不到不致命**：直接用兜底地址。
    let mut origin = CatalogOrigin::Discovered;
    let url = match curl_json(mirrors::DISCOVERY_URL) {
        Ok(body) => match mirrors::parse_discovery(&body) {
            Ok(url) => url,
            Err(e) => {
                tracing::warn!("清单地址没解出来（{e}），改用内置地址");
                origin = CatalogOrigin::FallbackUrl;
                mirrors::CATALOG_FALLBACK_URL.to_owned()
            }
        },
        Err(e) => {
            tracing::warn!("问清单地址失败（{e}），改用内置地址");
            origin = CatalogOrigin::FallbackUrl;
            mirrors::CATALOG_FALLBACK_URL.to_owned()
        }
    };

    // 2) 再拿清单本身。
    let body = curl_json(&url).map_err(|e| {
        format!("拉不到发行版清单（{url}）：{e}。也可以直接用「自定义 URL」填一个 rootfs 地址。")
    })?;
    let offers = mirrors::parse_catalog(&body, arm64)
        .map_err(|e| format!("清单解不开：{e}。也可以直接用「自定义 URL」填一个 rootfs 地址。"))?;

    Ok(Catalog {
        offers,
        origin,
        updated: mirrors::parse_update_time(&body),
    })
}

/// 起一次 `curl.exe` 拿 JSON 正文。
fn curl_json(url: &str) -> Result<String, String> {
    let args = mirrors::curl_json_args(url);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    match cli::run_helper_with_hint(
        "curl.exe",
        "curl.exe",
        cli::CURL_NOT_FOUND_HINT,
        &refs,
        Duration::from_secs(30),
    ) {
        Ok(out) => Ok(out.stdout),
        Err(e) => Err(e.to_string()),
    }
}
