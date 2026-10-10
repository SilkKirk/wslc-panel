//! wslc-panel 里**不依赖 GPUI 的那一半**：偏好、状态采集，以及界面用到的纯数据。
//!
//! # 为什么单独一个 crate
//!
//! 为了 **CI 的编译时间**。`state.rs` / `prefs.rs` 这一层完全不碰 GPUI
//! （见 [`state`] 的模块说明），但它们的单测原本和 bin 在同一个 test target 里 ——
//! 于是 `cargo test -p wslc-panel --bins` 必须把整棵 GPUI 依赖树 codegen
//! 并**链接**一遍，才能跑几个字符串切分和列宽的断言。
//!
//! 实测（GitHub Actions，windows-latest，缓存已命中）：那一步要 **763 秒**，
//! 占掉一轮 CI 的 72%；而 `cargo check` 产出的只是元数据，对 test 那一步
//! 一点用都没有 —— 每轮都得从头再来。
//!
//! 搬到这里之后，测试走 `cargo test -p wslc-panel-core`，
//! 依赖只有 `wslc-core` + serde，和 `wslc-core` 的 job 一个量级（几十秒）。
//!
//! # 边界
//!
//! 这里只放**不依赖 GPUI 的东西**。判断标准很简单：`use gpui_kit::` 出现在哪个文件，
//! 那个文件就留在 `wslc-panel` 里。
//!
//! `wslc-panel` 那边在 `main.rs` 里用一层 re-export 垫片继续暴露
//! `crate::state::…` / `crate::prefs::…` 这些旧路径 ——
//! 所以 `app.rs` / `views.rs` 里的调用点一行都没改。

pub mod columns;
pub mod prefs;
pub mod presets;
pub mod state;
pub mod util;
