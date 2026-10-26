//! 数据库连接的数据形状：引擎 / 从哪儿连 / 连接本身。
//!
//! 这一层**只有数据**：不连库、不拼命令。拼命令在各 driver 里（见
//! `crate::cmd::mysql` 这一批的现状，PostgreSQL 在 v0.14 加）。
//!
//! 放 `wslc-core` 而不是 `wslc-panel-core` 的理由：拼命令、解析输出都要用它，
//! 而那些逻辑在 `cmd/` 与 `model/` 里 —— 这两个都在 `wslc-core`。
//! 反过来 `wslc-panel-core` 的补全也要用它的方言数据，方向是单向的。

use crate::db::dialect::{DialectData, dialect_for};

/// 支持的数据库引擎。
///
/// # 为什么只有三个变体
///
/// 用户定了范围：**MySQL 家族 + PostgreSQL**。dbx 的 `DatabaseType` 有 100+
/// 变体（`crates/dbx-types/src/models/connection.rs` 有 235 KB），但那是
/// "插件平台"的规模。我们是单体工具，加一档就要有对应的驱动实现、
/// 元数据 SQL、输出解析和夹具 —— 不能先摆一个空变体。
///
/// 同族差异（Percona / TiDB / StarRocks 都走 MySQL 协议）不在这里表达：
/// 它们共用 [`DbKind::MySql`] 的驱动，只差版本探测文案。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DbKind {
    /// 官方 MySQL。
    MySql,
    /// MariaDB（协议兼容 MySQL，但有一批独有关键字）。
    MariaDb,
    /// PostgreSQL。
    Postgres,
}

impl DbKind {
    /// 界面上显示的名字。
    pub fn label(self) -> &'static str {
        match self {
            Self::MySql => "MySQL",
            Self::MariaDb => "MariaDB",
            Self::Postgres => "PostgreSQL",
        }
    }

    /// 容器里那个**命令行客户端**叫什么。
    ///
    /// 这是我们这套（exec）方案能成立的前提：官方镜像都自带客户端。
    /// 实测：`mysql:latest` 有 `mysql`，`postgres:*` 有 `psql`，
    /// `mariadb:*` 有 `mariadb`（也有 `mysql` 兼容链接，但用真名更稳）。
    pub fn client_bin(self) -> &'static str {
        match self {
            Self::MySql => "mysql",
            Self::MariaDb => "mariadb",
            Self::Postgres => "psql",
        }
    }

    /// 默认端口。
    pub fn default_port(self) -> u16 {
        match self {
            Self::MySql | Self::MariaDb => 3306,
            Self::Postgres => 5432,
        }
    }

    /// 默认用户。
    ///
    /// MySQL 家族几乎都是 `root`；PostgreSQL 官方镜像的超级用户是 `postgres`
    /// （用 `root` 连会直接被拒，报 role "root" does not exist）。
    pub fn default_user(self) -> &'static str {
        match self {
            Self::MySql | Self::MariaDb => "root",
            Self::Postgres => "postgres",
        }
    }

    /// 从探测到的**家族名**猜引擎。
    ///
    /// 认不出就当 MySQL —— 那是历史默认（v0.12 之前只有 MySQL），
    /// 保持它可以让"探测不到家族名"的旧路径行为不变。
    ///
    /// ⚠️ 这条判断只看字符串，不连库。真正的探测在
    /// `cmd::mysql::detect`（读 `SELECT VERSION()`）和将来的
    /// `cmd::postgres::detect` 里。
    pub fn from_family(family: &str) -> Self {
        let lower = family.to_ascii_lowercase();
        if lower.contains("maria") {
            Self::MariaDb
        } else if lower.contains("postgres") || lower.contains("pg") {
            Self::Postgres
        } else {
            Self::MySql
        }
    }

    /// 用这个引擎的规则包一个标识符。
    pub fn quote_ident(self, name: &str) -> String {
        dialect_for(self).quote_ident(name)
    }

    /// 这个引擎的关键字 / 常用函数（补全用）。
    pub fn keywords(self) -> Vec<&'static str> {
        dialect_for(self).keywords()
    }

    /// 这个引擎的方言数据。
    pub fn dialect(self) -> &'static DialectData {
        dialect_for(self)
    }
}

/// 从**哪儿**发起连接。
///
/// # 为什么这是一个枚举而不是一个字符串
///
/// `wslc exec <容器>` 和 `wsl -d <发行版>` 是两条完全不同的命令，
/// 参数位置、退出码语义、编码处理都不一样（`wsl.exe` 那边还要
/// `WSL_UTF8=1`，见 `decode.rs`）。用枚举让"漏了一个分支"变成编译错误。
///
/// 阶段 5 若加进程内直连，只是多一个 `Native` 变体。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DbHost {
    /// WSL 容器（走 `wslc exec`）。
    Container(String),
    /// WSL 发行版（走 `wsl -d <name> -e`）。
    Distro(String),
}

impl DbHost {
    /// 界面上怎么称呼它。
    pub fn label(&self) -> String {
        match self {
            Self::Container(name) => format!("容器 {name}"),
            Self::Distro(name) => format!("发行版 {name}"),
        }
    }

    /// 名字（进了日志/错误信息就别再带"容器"前缀了）。
    pub fn name(&self) -> &str {
        match self {
            Self::Container(name) => name,
            Self::Distro(name) => name,
        }
    }
}

/// 一条数据库连接。
///
/// # 密码故意不在这里
///
/// v0.12 的密码活在内存里的表单（`DbPageForm::password`）上，**从不落盘**。
/// 多连接之后用户会想"存起来下次不用重填"，但明文写进
/// `%APPDATA%\...\prefs.json` 是不行的（那是个普通文件），
/// 正确做法是 Windows DPAPI（要新依赖）。
///
/// 定下的做法是**阶段 1 不存密码**，
/// 所以类型里根本没有这个字段 —— 想加的人必须先回答"存哪儿"。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbConn {
    pub kind: DbKind,
    pub host: DbHost,
    /// 容器内部能解析的地址。
    ///
    /// 注意：**不是**宿主机的 `127.0.0.1:3306`。我们是在容器里执行客户端，
    /// 所以填 `127.0.0.1`（同一个容器内）或另一个容器的名字。
    pub addr: String,
    pub port: u16,
    pub user: String,
    pub database: Option<String>,
}

impl DbConn {
    /// 按引擎给一套默认值（新建连接时预填）。
    pub fn with_defaults(kind: DbKind, host: DbHost) -> Self {
        Self {
            kind,
            host,
            addr: "127.0.0.1".to_owned(),
            port: kind.default_port(),
            user: kind.default_user().to_owned(),
            database: None,
        }
    }

    /// `addr:port` —— 错误信息里用。
    pub fn endpoint(&self) -> String {
        format!("{}:{}", self.addr, self.port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_have_distinct_ports_and_clients() {
        // 端口撞了说明同一个引擎写了两个变体；客户端撞了说明有一个跑的不是
        // 自己那一族的命令（`psql` 跑不出 MySQL 的结果）。
        let kinds = [DbKind::MySql, DbKind::MariaDb, DbKind::Postgres];
        let clients: Vec<&str> = kinds.iter().map(|k| k.client_bin()).collect();
        assert_eq!(clients, vec!["mysql", "mariadb", "psql"]);
        assert_eq!(DbKind::MySql.default_port(), 3306);
        assert_eq!(DbKind::MariaDb.default_port(), 3306);
        assert_eq!(DbKind::Postgres.default_port(), 5432);
    }

    #[test]
    fn postgres_defaults_to_its_own_superuser() {
        // 实测过：PostgreSQL 官方镜像里没有 root 这个 role，
        // 用 root 连会报 role "root" does not exist。
        assert_eq!(DbKind::Postgres.default_user(), "postgres");
        assert_eq!(DbKind::MySql.default_user(), "root");
    }

    #[test]
    fn family_names_map_to_kinds() {
        assert_eq!(DbKind::from_family("MariaDB"), DbKind::MariaDb);
        assert_eq!(DbKind::from_family("mariadb 10.6"), DbKind::MariaDb);
        assert_eq!(DbKind::from_family("MySQL 8.0"), DbKind::MySql);
        assert_eq!(DbKind::from_family("PostgreSQL"), DbKind::Postgres);
        assert_eq!(DbKind::from_family("postgres 16.2"), DbKind::Postgres);
        // 认不出 → MySQL（保持 v0.12 的行为）
        assert_eq!(DbKind::from_family(""), DbKind::MySql);
        assert_eq!(DbKind::from_family("unknown"), DbKind::MySql);
    }

    #[test]
    fn quoting_follows_the_dialect() {
        assert_eq!(DbKind::MySql.quote_ident("order"), "`order`");
        assert_eq!(DbKind::MariaDb.quote_ident("order"), "`order`");
        // PostgreSQL 用双引号，而双引号里的反引号**不转义**
        assert_eq!(DbKind::Postgres.quote_ident("order"), "\"order\"");
        assert_eq!(DbKind::Postgres.quote_ident("a\"b"), "\"a\"\"b\"");
        // 反引号在双引号里是普通字符
        assert_eq!(DbKind::Postgres.quote_ident("a`b"), "\"a`b\"");
    }

    #[test]
    fn host_label_and_name() {
        let c = DbHost::Container("mysql".into());
        assert_eq!(c.name(), "mysql");
        assert_eq!(c.label(), "容器 mysql");
        let d = DbHost::Distro("Ubuntu".into());
        assert_eq!(d.name(), "Ubuntu");
        assert_eq!(d.label(), "发行版 Ubuntu");
    }

    #[test]
    fn with_defaults_gives_a_usable_connection() {
        let conn = DbConn::with_defaults(DbKind::Postgres, DbHost::Container("pg".into()));
        assert_eq!(conn.addr, "127.0.0.1");
        assert_eq!(conn.port, 5432);
        assert_eq!(conn.user, "postgres");
        assert_eq!(conn.database, None);
        assert_eq!(conn.endpoint(), "127.0.0.1:5432");
    }
}
