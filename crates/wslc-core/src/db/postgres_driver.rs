//! PostgreSQL 驱动（实现 [`SqlDriver`]）。
//!
//! 和 `db::mysql_driver` 一样是**薄委托**：真正的活在 `cmd::postgres`
//! （跑进程）和 `model::postgres`（拼参数 / 解析 / 切分）。
//!
//! 两个驱动放在一起看，就能看出这一层抽象到底该管什么、不该管什么 ——
//! 它只回答"这个引擎怎么被使用"，具体命令和格式各归各的模块。

use crate::cli::Wslc;
use crate::cmd::postgres as pg;
use crate::db::conn::{DbConn, DbHost, DbKind};
use crate::db::driver::{Credentials, ServerInfo, SqlDriver};
use crate::error::{Error, Result};
use crate::model::schematree::SchemaTree;
use crate::model::sqlresult::SqlResult;

/// 探测服务端版本用的 SQL。
///
/// 用 `version()` 而不是 `SHOW server_version`：前者返回完整串
/// （`PostgreSQL 16.15 (Debian …) on x86_64…`），用户和日志都用得上；
/// 后者只有 `16.15`。
const VERSION_SQL: &str = "SELECT version()";

pub struct PostgresDriver;

impl PostgresDriver {
    pub fn new() -> Self {
        Self
    }

    /// 取出容器名。
    ///
    /// `DbHost::Distro` 暂不支持（和 MySQL 驱动同一个理由：发行版里连的是
    /// 发行版本机的 5432，和"容器内 socket"是两条路径）。
    fn container_name<'a>(&self, conn: &'a DbConn) -> Result<&'a str> {
        match &conn.host {
            DbHost::Container(name) => Ok(name),
            DbHost::Distro(name) => Err(Error::InvalidArgument(format!(
                "暂不支持从发行版 {name} 连 PostgreSQL；请选择容器"
            ))),
        }
    }
}

impl Default for PostgresDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl SqlDriver for PostgresDriver {
    fn kind(&self) -> DbKind {
        DbKind::Postgres
    }

    fn client_bin(&self) -> &'static str {
        pg::CLIENT_BIN
    }

    fn probe(&self, wslc: &Wslc, conn: &DbConn, cred: &Credentials) -> Result<ServerInfo> {
        let container = self.container_name(conn)?;
        // 用真查询而不是 `psql --version`：后者给的是**客户端**版本，
        // 而用户关心的是服务端（客户端版本在镜像里恒等于服务端版本，
        // 但那是巧合，不该依赖）。
        let raw = pg::run_sql(
            wslc,
            container,
            &cred.user,
            &cred.password,
            conn.database.as_deref(),
            VERSION_SQL,
        )?;
        let table = crate::model::postgres::parse_csv(&raw);
        let version = table
            .rows
            .first()
            .and_then(|r| r.first())
            .cloned()
            .filter(|s| !s.is_empty());
        Ok(ServerInfo {
            kind: DbKind::Postgres,
            version,
            client: Some(pg::CLIENT_BIN.to_owned()),
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
        pg::run_script(
            wslc,
            container,
            &cred.user,
            &cred.password,
            conn.database.as_deref(),
            script,
        )
    }

    fn schema(&self, wslc: &Wslc, conn: &DbConn, cred: &Credentials) -> Result<SchemaTree> {
        let container = self.container_name(conn)?;
        pg::list_schema_tree(
            wslc,
            container,
            &cred.user,
            &cred.password,
            conn.database.as_deref(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifies_itself_as_postgres() {
        let d = PostgresDriver::new();
        assert_eq!(d.kind(), DbKind::Postgres);
        assert_eq!(d.client_bin(), "psql");
    }

    #[test]
    fn distro_host_is_rejected_with_a_clear_message() {
        let d = PostgresDriver::new();
        let mut conn = DbConn::with_defaults(DbKind::Postgres, DbHost::Distro("Ubuntu".into()));
        conn.port = 5432;
        let err = d.container_name(&conn).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("Ubuntu"), "要带名字：{text}");
        assert!(text.contains("容器"), "要提示改用容器：{text}");
    }

    #[test]
    fn container_host_is_accepted() {
        let d = PostgresDriver::new();
        let conn = DbConn::with_defaults(DbKind::Postgres, DbHost::Container("pg".into()));
        assert_eq!(d.container_name(&conn).unwrap(), "pg");
    }
}
