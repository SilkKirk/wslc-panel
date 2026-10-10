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
//! - [`catalog`] —— 拉「在线发行版（镜像源）」那份清单（两个 HTTP 请求走 `curl.exe`），
//!   解析在 [`crate::mirrors`] 里。

pub mod catalog;
pub mod container;
pub mod distro;
pub mod image;
pub mod install;
pub mod network;
pub mod picker;
pub mod system;
pub mod volume;

pub use container::{LogOptions, RunSpec};
