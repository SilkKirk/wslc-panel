//! 多数据库驱动层（v0.13 起）。
//!
//! # 为什么要有这一层
//!
//! v0.12 及以前，"数据库"这个概念在代码里**不存在** —— 存在的只有"容器"：
//! `cmd::mysql::run_sql(wslc, container_name, client_bin, creds, …)` 第一个
//! 参数就是容器名，元数据 SQL 直接写死 `information_schema.TABLES`，
//! 补全的关键字写死在 `wslc-panel-core::sqlcomplete` 的两个 `const` 里。
//!
//! 要加第二个引擎，就得把这些"其实和数据库引擎有关"的东西从"容器"里拆出来。
//! 拆法参考 [t8y2/dbx](https://github.com/t8y2/dbx)（Apache-2.0，只读架构）：
//!
//! | 层 | dbx 的做法 | 我们的做法 |
//! |---|---|---|
//! | 引擎 | 6 个 protocol driver crate | [`DbKind`] + 一个 driver 模块 |
//! | 方言 | 38 份 YAML + 能力位 | [`DialectData`]（Rust 静态数据） |
//! | 连接 | `DatabaseType` 100+ 变体 + 每族参数 | [`DbConn`] + [`DbHost`] |
//!
//! # 和 dbx 最大的分歧：`DbHost`
//!
//! dbx 是**进程内直连 TCP**。我们是 WSL **容器**面板，走的是
//! `wslc exec <容器> <客户端> …`。所以"连接"除了地址和口令，还必须带上
//! **从哪儿发起** —— 这就是 [`DbHost`]。把这条写进类型里，阶段 5 想加
//! "进程内直连"时只是多一个枚举变体，不用改所有调用点。

pub mod conn;
pub mod detect;
pub mod dialect;
pub mod driver;
pub mod mysql_driver;
pub mod postgres_driver;

pub use conn::{DbConn, DbHost, DbKind};
pub use detect::{Confidence, ConnHint, guess_kind};
pub use dialect::{Capabilities, DialectData, LimitSyntax, QuoteStyle, dialect_for};
pub use driver::{Credentials, ServerInfo, SqlDriver};
pub use mysql_driver::MySqlDriver;
pub use postgres_driver::PostgresDriver;
