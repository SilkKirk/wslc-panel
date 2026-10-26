//! MySQL / MariaDB 容器的**纯逻辑**：识别、命令行拼装、输出解析、SQL 生成、错误分类。
//!
//! 这个模块里**一行进程都不起**（所以能脱离 Windows 单测）—— 真去跑命令的那部分在
// [`crate::cmd::mysql`]。拆法和 `model/distro.rs` ↔ `cmd/distro.rs` 一样。
//!
//! # 为什么走容器里的 `mysql` 客户端，而不是自己连 3306
//!
//! 自连要引一个 MySQL 协议库（`mysql` / `mysql_async`）——本机没有 cargo，
//! 加依赖意味着 `Cargo.lock` 只能靠 `gen-lock.yml` 重生成；而且它还要求
//! 容器**发布了 3306**、还要自己处理 `caching_sha2_password` 与 TLS。
//! 走 `wslc exec` 则是白拿的：不用端口、不用协议、不用认证插件，代价只有两条
//! ——**容器得在运行**、**镜像里得有客户端**。这两条都能给出明确提示
//! （见 [`MysqlErrorKind`]，用的是实测抓下来的报错原文）。
//!
//! # 实测（本机 wslc 3.0.3.0 / mysql 26.7.0，2026-10）
//!
//! | 试的 | 结果 |
//! |---|---|
//! | `wslc exec -e MYSQL_PWD=root mysql mysql -uroot -N -B -e "SHOW DATABASES"` | exit 0，4 行 TSV |
//! | 不给 `--default-character-set` 时客户端协商到的字符集 | **latin1**（容器里没有 `LANG`，客户端 `auto` 认不出 locale）—— `SELECT` 的中文变 `?`、**恢复时中文被双重编码**，实测与修法见 [`load_script`]；所以 argv 里现在写死 `--default-character-set=utf8mb4` |
//! | `wslc exec --help` | **有 `-e/--env`**，所以密码能走环境变量、不进 argv |
//! | 客户端在哪 | `/usr/bin/mysql`（另有 `/usr/bin/mysqlsh`） |
//! | stdout 不是 tty 时的默认输出 | **batch（TSV）** —— 所以"给人看的表格"必须显式 `--table` |
//! | `-B` 对一个含 tab 的值 | 输出 `a\tb`（字面反斜杠 + t），反斜杠本身输出成 `\\` |
//! | SQL 里带 `'` `"` `\` 空格 | 经 argv 原样到达（没有 shell，所以不存在 shell 注入面） |
//! | 密码错 | exit 1，`ERROR 1045 … Access denied` |
//! | 容器没运行 | exit 1，`… 未运行。错误代码： WSLC_E_CONTAINER_NOT_RUNNING` |
//! | 镜像里没客户端 | exit **126**，`… exec: "mysql": executable file not found in $PATH` |
//! | `Config.ExposedPorts`（`inspect`） | **是空的** —— 端口只能看 `list` 的 `Labels` 元数据 |
//! | `list` 的 `Command` 字段 | 被截断成 20 字符（`"docker-entrypoint.s…"`），认不出 `mysqld` |
//!
//! # 文件怎么分
//!
//! | 文件 | 覆盖 |
//! |---|---|
//! | [`family`] | 家族与识别（镜像名 / 命令 / inspect） |
//! | [`creds`] | 凭据与 root 线索（`Debug` 手动脱敏） |
//! | [`client`] | 怎么调客户端（`SqlMode` / 参数 / 环境 / 脱敏） |
//! | [`table`] | TSV 编解码与库 / 用户 / 探针解析 |
//! | [`ddl`] | SQL 生成与校验 |
//! | [`backup`] | 备份 / 恢复的路径与参数 |
//! | [`error`] | 失败分类 |
//!
//! 对外路径没有变：`mod.rs` 把子模块 `pub use` 出来，
//! `wslc_core::model::mysql::parse_databases` 这类写法照旧。

mod backup;
mod client;
mod creds;
mod ddl;
mod error;
mod family;
mod table;

#[cfg(test)]
mod tests;

pub use backup::*;
pub use client::*;
pub use creds::*;
pub use ddl::*;
pub use error::*;
pub use family::*;
pub use table::*;
