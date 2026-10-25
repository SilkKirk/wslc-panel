//! MySQL 家族的驱动（MySQL / MariaDB / Percona 共用这一个）。
//!
//! # 为什么一个驱动管三个家族
//!
//! Percona 是 MySQL 的兼容发行版，MariaDB 是 fork —— 三者的**客户端命令行
//! 参数、输出格式、协议**都一样。dbx 也是这么做的：`dbx-driver-mysql` 一个
//! crate 里塞了 `dolt` / `doris` / `starrocks` / `tidb` / `oceanbase` 一堆
//! 兼容垫片，各自只处理差异（见 `docs/PLAN-multi-db.md` §2.1）。
//!
//! 差异只有两处，都不影响"怎么跑命令"：
//! - 客户端二进制名（`mysql` / `mariadb`）；
//! - 补全的关键字表（MariaDB 有 `RETURNING` / `SEQUENCE`）。
//!
//! 后一条已经收在 `db::dialect` 里了，所以这里只剩前者。
//!
//! # 这一层是**委托**，不是重写
//!
//! 真正的活还在 `cmd::mysql` 里（拼命令行、跑 `wslc exec`、解析输出），
//! 这里只把它的签名转成 trait 的形状。这样阶段 0 能做到**行为零变化** ——
//! 一旦把逻辑重写一遍，"重构"就变成了"改功能"，而两种改动混在一起时
//! 出了问题分不清是谁造成的。

use crate::cli::Wslc;
use crate::cmd::mysql as my;
use crate::db::conn::{DbConn, DbHost, DbKind};
use crate::db::driver::{Credentials, ServerInfo, SqlDriver};
use crate::error::{Error, Result};
use crate::model::mysql::MysqlCreds;
use crate::model::schematree::SchemaTree;
use crate::model::sqlresult::SqlResult;

/// MySQL 家族驱动。
pub struct MySqlDriver {
    kind: DbKind,
    /// 探测到的客户端二进制名。
    ///
    /// 为什么不能直接用 [`DbKind::client_bin`]：容器里的实际名字可能不一样
    /// （MariaDB 镜像两个名字都有、某些镜像只有 `mysqlsh`）。面板原来的做法
    /// 是先探测再用探测结果（`DbState::client`），这里保留那个行为 ——
    /// 用 `None` 时才回落到按引擎猜的默认名。
    client: Option<String>,
}

impl MySqlDriver {
    pub fn new(kind: DbKind) -> Self {
        Self { kind, client: None }
    }

    /// 用探测到的客户端名构造。
    pub fn with_client(kind: DbKind, client: impl Into<String>) -> Self {
        Self {
            kind,
            client: Some(client.into()),
        }
    }

    /// 只要 MySQL 家族才返回 `Some`。
    pub fn for_kind(kind: DbKind) -> Option<Self> {
        match kind {
            DbKind::MySql | DbKind::MariaDb => Some(Self::new(kind)),
            DbKind::Postgres => None,
        }
    }

    fn bin(&self) -> &str {
        self.client
            .as_deref()
            .unwrap_or_else(|| self.kind.client_bin())
    }

    /// 取出容器名。
    ///
    /// `DbHost::Distro` 目前**不支持**：发行版里的 MySQL 客户端连的是
    /// 发行版本机的 3306，和"容器里 socket 直连"是两条完全不同的路径，
    /// 而阶段 0 的目标是行为零变化。给一句明确的错误，而不是悄悄当容器用。
    fn container_name<'a>(&self, conn: &'a DbConn) -> Result<&'a str> {
        match &conn.host {
            DbHost::Container(name) => Ok(name),
            DbHost::Distro(name) => Err(Error::InvalidArgument(format!(
                "暂不支持从发行版 {name} 连 MySQL；请选择容器"
            ))),
        }
    }
}

impl SqlDriver for MySqlDriver {
    fn kind(&self) -> DbKind {
        self.kind
    }

    fn client_bin(&self) -> &'static str {
        self.kind.client_bin()
    }

    fn probe(&self, wslc: &Wslc, conn: &DbConn, _cred: &Credentials) -> Result<ServerInfo> {
        let container = self.container_name(conn)?;
        let probe = my::detect(wslc, container)?;
        // 家族是**权威判定**（`detect` 读的是容器 inspect + `SELECT VERSION()`），
        // 所以用它覆盖用户选的 kind：选了 MySQL 实际连上 MariaDB 很常见。
        let kind = DbKind::from_family(probe.family.label());
        Ok(ServerInfo {
            kind,
            version: probe.client.version.clone(),
            client: Some(probe.client.bin.clone()),
        })
    }

    fn run_script(
        &self,
        wslc: &Wslc,
        conn: &DbConn,
        cred: &Credentials,
        script: &str,
    ) -> Result<Vec<SqlResult>> {
        let container = self.container_name(conn)?;
        let creds = to_mysql_creds(cred);
        my::run_statements(
            wslc,
            container,
            self.bin(),
            &creds,
            script,
            conn.database.as_deref(),
        )
    }

    fn schema(&self, wslc: &Wslc, conn: &DbConn, cred: &Credentials) -> Result<SchemaTree> {
        let container = self.container_name(conn)?;
        let creds = to_mysql_creds(cred);
        my::list_schema_tree(wslc, container, self.bin(), &creds)
    }
}

/// 把通用凭据转成 MySQL 客户端要的形状。
///
/// # 为什么 `host` / `port` 都传 `None`
///
/// 我们是在**容器内部**跑客户端，所以走的是容器的 unix socket ——
/// 这也是面板从 v0.9 到现在的行为。填 `127.0.0.1:3306` 反而会多走一圈 TCP，
/// 而且容器里的 MySQL 未必监听 TCP。
///
/// ⚠️ `DbConn::addr` / `port` 目前**不参与**这条路径，它们是为将来的
/// 进程内直连（`DbHost::Native`，阶段 5）和"跨容器连"准备的。
/// 这一点写在类型上也写在注释里，免得后来的人以为端口没生效是 bug。
fn to_mysql_creds(cred: &Credentials) -> MysqlCreds {
    MysqlCreds {
        user: cred.user.clone(),
        password: cred.password.clone(),
        host: None,
        port: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn(kind: DbKind, host: DbHost) -> DbConn {
        DbConn::with_defaults(kind, host)
    }

    #[test]
    fn only_mysql_family_has_a_driver() {
        assert!(MySqlDriver::for_kind(DbKind::MySql).is_some());
        assert!(MySqlDriver::for_kind(DbKind::MariaDb).is_some());
        assert!(MySqlDriver::for_kind(DbKind::Postgres).is_none());
    }

    #[test]
    fn client_bin_falls_back_to_the_kind_default() {
        assert_eq!(MySqlDriver::new(DbKind::MySql).bin(), "mysql");
        assert_eq!(MySqlDriver::new(DbKind::MariaDb).bin(), "mariadb");
        // 探测到的名字优先于按引擎猜的名字
        assert_eq!(
            MySqlDriver::with_client(DbKind::MariaDb, "mysql").bin(),
            "mysql"
        );
    }

    #[test]
    fn distro_host_is_rejected_with_a_clear_message() {
        let d = MySqlDriver::new(DbKind::MySql);
        let c = conn(DbKind::MySql, DbHost::Distro("Ubuntu".into()));
        let err = d.container_name(&c).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("Ubuntu"), "错误信息要带名字：{text}");
        assert!(text.contains("容器"), "要提示改用容器：{text}");
    }

    #[test]
    fn mysql_creds_use_unix_socket_not_tcp() {
        // 这条断言是给未来的自己看的：host/port 传 None 是**刻意**的，
        // 不是漏填。改成 Some 会让容器里的连接改走 TCP。
        //
        // 注意签名里**没有** `DbConn`：正因为走 socket，地址和端口在这条
        // 路径上一个都不需要。将来真要做"exec 进 A 容器连 B 容器的库"，
        // 那时才该把 `conn` 传进来并填 host/port。
        let creds = to_mysql_creds(&Credentials::new("root", "pw"));
        assert_eq!(creds.host, None);
        assert_eq!(creds.port, None);
        assert_eq!(creds.user, "root");
        assert_eq!(creds.password, "pw");
    }
}
