//! `wslc-core` —— WSL 容器（wslc）CLI 的封装层与数据模型。
//!
//! 本 crate **不依赖任何 UI 框架**，因此可以在没有 GPU、没有窗口的环境下
//! 用 `cargo test -p wslc-core` 跑完整测试。
//!
//! # 设计约束（全部来自实机采集，详见 `docs/wslc-schema.md`）
//!
//! 1. `wslc` 默认输出 **UTF-16LE**，必须给子进程注入 `WSL_UTF8=1`；
//!    即便如此仍需保留 UTF-16LE 兜底解码（见 [`decode`]）。
//! 2. `--format json` 输出的是 **JSON Lines**（每行一个对象），
//!    只有 `inspect` 输出标准 JSON 数组（见 [`jsonl`]）。
//! 3. 结果为空时输出 **0 字节**，而不是 `[]`。
//! 4. 字段类型不稳定（数字可能是字符串、可空字段可能是 `null`），
//!    因此所有模型字段都带 `#[serde(default)]`，解析失败绝不 panic。
//!
//! # 示例
//!
//! ```no_run
//! use wslc_core::{Wslc, cmd};
//!
//! # fn main() -> wslc_core::Result<()> {
//! let wslc = Wslc::new();
//! let info = cmd::system::info(&wslc)?;
//! println!("WSL {}", info.client.version);
//!
//! for c in cmd::container::list(&wslc, true)? {
//!     println!("{} {}", c.short_id(), c.display_name());
//! }
//! # Ok(())
//! # }
//! ```

pub mod cli;
pub mod cmd;
pub mod decode;
pub mod error;
pub mod jsonl;
pub mod model;
pub mod settings;
pub mod storage;

pub use cli::{CommandOutput, Wslc};
pub use error::{Error, Result};
pub use storage::{StorageInfo, StoragePathOrigin, VolumeSpace};
