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
//! | [`db`] | 数据库域：连接状态、库表用户、备份与导入。 |
//! | [`deploy`] | 部署域：记录、阶段、进度、站点配置。 |
//! | [`install`] | 安装域：来源、在线清单、镜像源探测与安装进度。 |
//! | [`logs`] | 日志 / exec / 进度三类**长任务的中间状态**。 |
//! | [`nav`] | 导航与页面枚举：哪些页、怎么分组、哪些页要有输入框才能进。 |
//! | [`registry`] | 镜像仓库（Registry V2）查询状态、容器详情巡检结果。 |
//! | [`snapshot`] | 数据快照：一次采集回来的一切，以及从它派生的总览数字。 |
//! | [`wslconf`] | `/etc/wsl.conf` 编辑器的状态与字段 id。 |
//!
//! ⚠️ 拆法是「一个域一个文件」，不是「一个类型一个文件」—— `impl AppState`
//! 整块 358 行留在 `app.rs` 里不拆，因为它的方法共享同一批字段访问。

mod prelude;

mod actions;
mod app;
mod db;
mod deploy;
mod install;
mod logs;
mod nav;
mod registry;
mod snapshot;
mod wslconf;
// 测试单独一个文件：116 条断言占了原来那份文件的 2711 行。
#[cfg(test)]
mod tests;

pub(crate) use actions::*;
pub(crate) use app::*;
pub(crate) use db::*;
pub(crate) use deploy::*;
pub(crate) use install::*;
pub(crate) use logs::*;
pub(crate) use nav::*;
pub(crate) use prelude::*;
pub(crate) use registry::*;
pub(crate) use snapshot::*;
pub(crate) use wslconf::*;
