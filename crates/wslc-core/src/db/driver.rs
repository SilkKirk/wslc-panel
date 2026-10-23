//! 驱动 trait：**一个引擎怎么被使用**。
//!
//! # 这一层解决什么
//!
//! 现在（v0.12）UI 直接调 `cmd::mysql::run_statements(…)`、`cmd::mysql::list_schema_tree(…)`
//! —— 函数名里就写着引擎，加第二个引擎只能到处 `if kind == …`。
//! 有了这个 trait，UI 只认 `&dyn SqlDriver`。
//!
//! # 为什么方法都吃 `&Wslc`
//!
//! 三条 sql 驱动方案（`docs/PLAN-multi-db.md` §3）里我们选了 **exec**：
//! 真正的"连接"是 `wslc exec <容器> <客户端>`。所以每个驱动都需要那个
//! 进程启动器。阶段 5 若加进程内直连（`DbHost::Native`），它只是忽略这个参数。
//!
//! # 为什么没有 `parse` / `split` 方法
//!
//! 切分（`sqlstmt`）和解析（`sqlresult`）是**每种引擎各一套**，但它们
//! 属于 `model/` 而不是驱动 —— 驱动只负责"把命令跑出去、把 stdout 拿回来"。
//! 这样解析逻辑能脱离进程单测（`AGENTS.md` §5：解析放 `model/` 才测得动）。
//!
//! 所以 [`SqlDriver::run_script`] 是**高层**操作（切分 + 逐条跑 + 解析），
//! 每个驱动在里面决定用自己那套 `model::` 函数。

use crate::cli::Wslc;
use crate::db::conn::{DbConn, DbKind};
use crate::error::Result;
use crate::model::schematree::SchemaTree;
use crate::model::sqlresult::SqlResult;

/// 一次连接用的凭据。
///
/// # 为什么不直接用 `MysqlCreds`
///
/// `MysqlCreds` 里有 `host` / `port`（MySQL 客户端要），而 PostgreSQL 用
/// 环境变量 `PGHOST` / `PGPORT`、SQLite 压根没有。共同点只有"用户名 + 口令"，
/// 所以 trait 只认这两样，各驱动自己转成需要的形状。
#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    pub user: String,
    /// 空串是**合法**值（`MYSQL_ALLOW_EMPTY_PASSWORD=yes` 那种，实测过）。
    pub password: String,
}

impl Credentials {
    pub fn new(user: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            user: user.into(),
            password: password.into(),
        }
    }

    pub fn is_empty_password(&self) -> bool {
        self.password.is_empty()
    }
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 学 `MysqlCreds` 的做法：只说"有没有密码"，不打印内容。
        // 这个类型会进日志和错误信息，漏一次口令就够糟了。
        f.debug_struct("Credentials")
            .field("user", &self.user)
            .field(
                "password",
                &if self.password.is_empty() {
                    "<empty>"
                } else {
                    "***"
                },
            )
            .finish()
    }
}

/// 探测出来的一台服务端。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerInfo {
    /// 实际引擎（可能和用户选的不一样，比如选了 MySQL 连上发现是 MariaDB）。
    pub kind: DbKind,
    /// 客户端版本（`--version` 第一行；拿不到就是 `None`）。
    pub version: Option<String>,
    /// 容器里那个客户端二进制名（`mysql` / `mariadb` / `psql`）。
    pub client: Option<String>,
}

/// 一个数据库引擎的驱动。
///
/// `Send + Sync` 是为了能塞进 `Arc<dyn SqlDriver>` 交给后台执行器
/// （`AGENTS.md` §7.1：所有起进程的活必须在后台跑）。
pub trait SqlDriver: Send + Sync {
    /// 这个驱动对应哪个引擎。
    fn kind(&self) -> DbKind;

    /// 容器里的客户端二进制（探测不到时的**兜底**名字）。
    fn client_bin(&self) -> &'static str;

    /// 连通性 + 版本。失败时的错误信息要能直接给用户看
    /// （"镜像里没有客户端" vs "口令错" 是两类完全不同的提示）。
    fn probe(&self, wslc: &Wslc, conn: &DbConn, cred: &Credentials) -> Result<ServerInfo>;

    /// 跑一段脚本（可能多条语句），逐条返回结果集。
    fn run_script(
        &self,
        wslc: &Wslc,
        conn: &DbConn,
        cred: &Credentials,
        script: &str,
    ) -> Result<Vec<SqlResult>>;

    /// 读整棵 库 / 表 / 列 树（对象树 + 补全都用它）。
    fn schema(&self, wslc: &Wslc, conn: &DbConn, cred: &Credentials) -> Result<SchemaTree>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_debug_never_prints_the_password() {
        let c = Credentials::new("root", "hunter2");
        let text = format!("{c:?}");
        assert!(!text.contains("hunter2"), "口令漏进 Debug 了：{text}");
        assert!(text.contains("***"));
        assert!(text.contains("root"));
    }

    #[test]
    fn empty_password_is_recorded_as_empty_not_hidden() {
        // 空口令是合法配置，日志里要能看出来"确实没设密码"，
        // 而不是和"有密码但被隐去"混在一起。
        let c = Credentials::new("root", "");
        let text = format!("{c:?}");
        assert!(text.contains("<empty>"));
        assert!(c.is_empty_password());
    }
}
