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
// 数据库账号的本地存档（v0.8：建库同时建用户，列表要展示用户名 / 密码 ——
// MySQL 的密码是单向哈希，面板必须自己记）。
pub mod dbcreds;
// 部署记录的本地存档（v0.9：部署过什么由面板记；"它现在跑没跑"问容器）。
pub mod deploys;
// 存档的原子写 + 崩溃恢复（v0.9.20：删旧档再 rename 有丢档窗口）。
pub mod fileio;
// 容器列表的过滤 / 排序（纯逻辑，带单测）—— 状态筛选的判据、排序规则
// 都在这里，`views.rs` 只负责画按钮和读输入框。
pub mod listview;
pub mod prefs;
pub mod presets;
// SQL 结果表格的**显示逻辑**（列宽估算、单元格分类、页脚文案、CSV 导出）。
// 纯函数 —— 渲染在 `crates/wslc-panel/src/sql_table.rs`，但"该显示什么"
// 在这里决定，所以 `core` job 能测到（`AGENTS.md` §8：渲染层没有测试）。
pub mod sqlview;
// SQL 编辑器的**方言感知 + 自动补全**（v0.12.4）。
//
// 判断逻辑（该给哪些候选、替换哪一段、有没有认出别名）全在这里；
// 渲染层 `crates/wslc-panel/src/sql_complete.rs` 只负责把结果包成
// `lsp_types::CompletionItem`。这么分是因为"补全给错了"在界面上
// 根本看不出来 —— 必须能在 `core` job 里断言。
pub mod sqlcomplete;
pub mod state;
// 更新提示的状态机与判据（什么时候查、查到了要不要提示、卡片上写什么）。
// 只读 JSON、只做判断，不碰网络也不碰 GPUI —— 所以能在这儿单测。
pub mod update;
pub mod util;
