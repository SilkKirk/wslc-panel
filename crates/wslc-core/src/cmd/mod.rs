//! 各子命令的类型化封装。
//!
//! 每个函数都只做三件事：拼参数 → 跑 `wslc` → 解析成模型。
//! 解析统一走 [`crate::jsonl`]，因此空输出、坏行、字段缺失都不会 panic。

pub mod container;
pub mod image;
pub mod network;
pub mod system;
pub mod volume;

pub use container::{LogOptions, RunSpec};
