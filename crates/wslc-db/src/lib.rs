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
//! `postgres` 0.19 的 MSRV 是 **1.85**，而 `wslc-core` 对外承诺 **1.75**。
//! 把驱动塞进去等于悄悄抬高整个数据层的 MSRV —— 那种"顺手破坏兼容性"
//! 的改动最容易被忽略。所以驱动单独一个 crate，`wslc-core` 不知道它存在。
//!
//! # 为什么是同步驱动
//!
//! 这个项目的铁律是"所有阻塞调用跑在 `background_executor` 上"
//! （`AGENTS.md` §7.1）。同步驱动天然符合；异步驱动（`sqlx` /
//! `tokio-postgres`）要把 tokio runtime 桥到 GPUI 后面，多一层就多一类
//! "卡界面 / 死锁"的 bug。见 `docs/PLAN-native-db.md` §2。
//!
//! # 分层
//!
//! ```text
//! conn.rs     ConnSpec + 连接串拼装（纯函数，有测试）
//! session.rs  TextTable / DbError / Session trait + 按 kind 打开
//! pg.rs       PostgreSQL 实现（postgres 0.19）
//! ```

pub mod conn;
pub mod pg;
pub mod session;

pub use conn::{parse_dsn, ConnSpec, DsnError};
pub use session::{open, DbError, DbErrorKind, Session, TextTable};
