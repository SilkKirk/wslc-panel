//! 原生（进程内）数据库连接。
//!
//! # 这个 crate 负责什么
//!
//! 把「一条连接」变成「一条真的 TCP 连接」，并且**不依赖任何 UI**。
//! 它同时是「能连远程库」这件事的落点：`host` / `port` 是普通字段，
//! 填什么就连什么，不再需要"有个容器可以 exec"。
//!
//! # 为什么单独一个 crate（而不是塞进 `wslc-core`）
//!
//! 因为驱动带 `mysql` / `postgres` 两棵很重的依赖树（TLS、加密、协议解析），
//! 而 `wslc-core` 是纯数据层：它只负责"把 wslc 的输出解析成类型"。
//! 混在一起会让只想读一个 JSON 的调用方也背上这两棵树。
//!
//! ⚠️ 早先这里写的理由是"`postgres` 要 MSRV 1.85，而 `wslc-core` 承诺 1.75"
//! —— **那条已经过时**：v0.17.0 起全 workspace 统一到 edition 2024 / 1.85，
//! 两个 crate 的 MSRV 现在一样。真正的理由只剩上面那条"依赖重量"。
//!
//! # 为什么是同步驱动
//!
//! 这个项目的铁律是"所有阻塞调用跑在 `background_executor` 上"
//! （`AGENTS.md` §7.1）。同步驱动天然符合；异步驱动（`sqlx` /
//! `tokio-postgres`）要把 tokio runtime 桥到 GPUI 后面，多一层就多一类
//! "卡界面 / 死锁"的 bug。
//!
//! # 分层
//!
//! ```text
//! conn.rs     ConnSpec + 连接串 / DSN 拼装与脱敏（纯函数，有测试）
//! session.rs  TextTable / DbError / Session trait + 按 kind 打开
//! my.rs       MySQL / MariaDB 实现（mysql 28，文本协议）
//! pg.rs       PostgreSQL 实现（postgres 0.19，simple_query + prepare）
//! ```

pub mod conn;
pub mod my;
pub mod pg;
pub mod session;

pub use conn::{ConnSpec, DsnError, parse_dsn, redact_dsn};
pub use session::{DbError, DbErrorKind, Session, TextTable, open};
