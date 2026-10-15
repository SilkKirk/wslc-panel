//! 各子命令的类型化封装。
//!
//! 分两个域：
//!
//! - [`container`] / [`image`] / [`network`] / [`volume`] / [`system`]
//!   —— `wslc.exe`（WSL **容器**），拼参数 → 跑 `wslc` → 解析 JSON
//!   （统一走 [`crate::jsonl`]，空输出、坏行、字段缺失都不会 panic）；
//! - [`distro`] —— `wsl.exe`（WSL **发行版**），它没有 JSON 输出，
//!   解析逻辑在 [`crate::model::distro`] 里；
//! - [`install`] —— 把"添加实例"的**计划**执行出来（起进程、流式读输出、
//!   轮询进度、取消），计划本身的纯逻辑在 [`crate::model::install`] 里；
//! - [`mysql`] —— 在**运行中的容器里**跑 `mysql` 客户端（数据库管理页）。
//!   它不碰 `wslc` 自己的子命令，拼的还是 [`container::exec`] 那条路
//!   （`wslc exec`），识别 / 拼参数 / SQL 生成在 [`crate::model::mysql`] 里；
//! - [`deploy`] —— 把本地文件**传进运行中的容器**并把它跑起来（部署应用页）。
//!   ⚠️ 它走的是 `wslc exec -i` 的 **stdin**（`wslc` 没有 `cp`，
//!   容器里也看不见宿主盘），以及容器内的 `nohup … &` + pidfile
//!   （`wslc exec -d` 实测**不生效**）；脚本与解析在 [`crate::model::deploy`] 里；
//! - [`catalog`] —— 拉「在线发行版（镜像源）」那份清单（两个 HTTP 请求走 `curl.exe`），
//!   解析在 [`crate::mirrors`] 里；
//! - [`registry`] —— 拉**容器镜像仓库**的 tag 列表与体积（也是 `curl.exe`），
//!   解析在 [`crate::registry`] 里。⚠️ 和 [`catalog`] 不是一回事：
//!   那个是 WSL **发行版**的 rootfs 文件，这个是容器**镜像**仓库；
//! - [`win_theme`] —— 读系统"应用主题"（浅色/深色），给界面上的「跟随系统」用。

pub mod catalog;
pub mod container;
pub mod deploy;
pub mod distro;
pub mod image;
pub mod install;
pub mod mysql;
pub mod network;
pub mod picker;
pub mod registry;
pub mod system;
pub mod volume;
pub mod win_theme;

pub use container::{LogOptions, RunSpec};
