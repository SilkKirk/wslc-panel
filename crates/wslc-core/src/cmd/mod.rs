//! 各子命令的类型化封装。
//!
//! 分两个域：
//!
//! - [`container`] / [`image`] / [`network`] / [`volume`] / [`system`]
//!   —— `wslc.exe`（WSL **容器**），拼参数 → 跑 `wslc` → 解析 JSON
//!   （统一走 [`crate::jsonl`]，空输出、坏行、字段缺失都不会 panic）；
//! - [`distro`] —— `wsl.exe`（WSL **发行版**），它没有 JSON 输出，
//!   解析逻辑在 [`crate::model::distro`] 里。

pub mod container;
pub mod distro;
pub mod image;
pub mod network;
pub mod picker;
pub mod system;
pub mod volume;

pub use container::{LogOptions, RunSpec};
