//! 应用状态：页面枚举、数据快照、待确认的危险操作。
//!
//! 这一层**完全不碰 GPUI**，只依赖 `wslc-core`，
//! 因此未来换渲染层（或加 CLI 模式）时这里可以原样复用。
//!
//! # 两个域
//!
//! 面板同时管两样东西，命令也不同：
//!
//! | 域 | 命令 | 对象 |
//! |---|---|---|
//! | 容器 | `wslc.exe` | container / image / network / volume |
//! | 实例 | `wsl.exe` | WSL 发行版（distro） |
//!
//! 两者的采集**频率不同**（见 [`load_distro_status`] 的说明）：
//! 列表跟随自动刷新（默认 3 秒），`wsl --status` 单独降到 30 秒。

//! # 目录
//!
//! 原先是一整份 **7492 行** 的 `state.rs`（119 个顶层条目 + 2711 行测试）。
//! 现在按域拆开，`impl` 跟着它修饰的类型走（`Page` 与 `impl Page` 同文件）：
//!
//! | 模块 | 内容 |
//! |---|---|
//! | [`actions`] | 待确认 / 待执行的动作枚举（容器域与发行版域各一套），以及提示条。 |
//! | [`app`] | **顶层**状态：`AppState` 与它直接持有的加载入口。 |
//! | [`db_state`] | 数据库域：连接状态、库表用户、备份与导入。 |
//! | [`deploy`] | 部署域：记录、阶段、进度、站点配置。 |
//! | [`install_state`] | 安装域：来源、在线清单、镜像源探测与安装进度。 |
//! | [`logs`] | 日志 / exec / 进度三类**长任务的中间状态**。 |
//! | [`nav`] | 导航与页面枚举：哪些页、怎么分组、哪些页要有输入框才能进。 |
//! | [`registry_state`] | 镜像仓库（Registry V2）查询状态、容器详情巡检结果。 |
//! | [`snapshot`] | 数据快照：一次采集回来的一切，以及从它派生的总览数字。 |
//! | [`wslconf_state`] | `/etc/wsl.conf` 编辑器的状态与字段 id。 |
//!
//! ⚠️ 拆法是「一个域一个文件」，不是「一个类型一个文件」—— `impl AppState`
//! 整块 358 行留在 `app.rs` 里不拆，因为它的方法共享同一批字段访问。

mod prelude;

mod actions;
mod app;
mod db_state;
mod deploy;
mod install_state;
mod logs;
mod nav;
mod registry_state;
mod snapshot;
// ⚠️ 这几个文件都带 `_state` 后缀：`prelude` 里有 `use wslc_core::{cmd, registry}`
// 和 `model::{db, install, wslconf}` 这类**路径段**，若模块同名，
// `use super::prelude::*;` 之后那个名字就说不清指谁（`E0659: ambiguous`）。
// 实测踩了 3 次（`wslconf` / `registry` / `db` / `install`），一次改到位。
//
// 原注释：`prelude` 里
// `pub(crate) use wslc_core::model::wslconf;` 会引入一个**同名**的
// 模块路径，两者在 `use super::prelude::*;` 之后就成了 `E0659: ambiguous`
// （实测踩过，8 处报错）。
mod wslconf_state;
// 测试放在 `tests/` 目录里，**一个域一个文件**（原先是一整份 2720 行的
// `tests.rs`，跨 10 个域 —— 见 `tests/mod.rs` 的说明）。
#[cfg(test)]
mod tests;

// `OnlineListSource` 是 `wslc-core` 里的类型，`state` 对外**转发**一份：
// 界面按它显示「在线清单来自 `wsl -l -o` 还是兜底 JSON」那句说明。
pub use prelude::OnlineListSource;

// ⚠️ 这几行必须是 `pub` 而不是 `pub(crate)`：`wslc-panel/src/main.rs` 里有一层
// 垫片 `pub use wslc_panel_core::state::*;`，那是**跨 crate**的再导出 ——
// `pub(crate)` 在另一个 crate 里完全看不见（实测：ui job 报 29 个 E0432）。
// 这些类型本来就是这个 crate 的公开 API，`pub` 才是它们原本的可见性。
pub use actions::*;
pub use app::*;
pub use db_state::*;
pub use deploy::*;
pub use install_state::*;
pub use logs::*;
pub use nav::*;
pub use registry_state::*;
pub use snapshot::*;
pub use wslconf_state::*;
