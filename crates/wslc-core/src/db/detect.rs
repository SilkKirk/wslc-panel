//! 从容器线索猜数据库引擎（"从容器一键导入连接"用）。
//!
//! # 为什么需要它
//!
//! 现在数据库页只能**从容器行进入**，而进去之前就得知道这是哪一种库 ——
//! 界面上要显示对的客户端名、对的默认端口、对的元数据查询。用户不想手选。
//!
//! # 判断顺序（重要的设计）
//!
//! 1. **镜像名**优先：`postgres:16` / `mysql:8` / `mariadb:11`。
//!    实测这台机器上的镜像名长这样：`docker.1panel.live/library/mysql:latest`
//!    —— 带 registry 前缀，所以只能做**包含匹配**，不能比开头。
//! 2. 镜像名认不出，再看**发布端口**：3306 → MySQL 家族，5432 → PostgreSQL。
//! 3. 都认不出 → `None`（**不猜**）。
//!
//! # 为什么端口是低置信度
//!
//! 端口是可以随便映射的（`-p 8080:3306` 很常见），所以"看到 5432"只能作为
//! 兜底提示，界面上应该说"看起来像 PostgreSQL"，而不是直接替用户决定。
//!
//! # 为什么不看容器里的进程
//!
//! 那要 `wslc exec` 进容器跑 `ps`，一次 ~70ms 起，而且镜像里可能没有 `ps`。
//! 已经有 `cmd::mysql::detect`（读 `SELECT VERSION()`）做真探测 ——
//! 那个才是权威，这里只是**猜**，目的是预填表单。

use super::conn::DbKind;

/// 猜的把握有多大。界面按这个决定语气（"就是 X" vs "看起来像 X"）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    /// 镜像名直接命中。
    High,
    /// 只靠端口猜的。
    Low,
}

/// 猜的时候能拿到的线索。
#[derive(Debug, Clone, Copy, Default)]
pub struct ConnHint<'a> {
    /// 镜像名（可带 registry 前缀和 tag）。
    pub image: &'a str,
    /// 容器的启动命令（有些镜像如 `manticoresearch` 靠命令区分）。
    pub command: &'a str,
    /// 发布到宿主机的端口（容器端口，不是宿主端口）。
    pub container_ports: &'a [u16],
}

impl<'a> ConnHint<'a> {
    pub fn new(image: &'a str) -> Self {
        Self {
            image,
            command: "",
            container_ports: &[],
        }
    }

    pub fn with_ports(mut self, ports: &'a [u16]) -> Self {
        self.container_ports = ports;
        self
    }
}

/// 从线索猜引擎。
pub fn guess_kind(hint: &ConnHint<'_>) -> Option<(DbKind, Confidence)> {
    let img = hint.image.to_ascii_lowercase();
    let cmd = hint.command.to_ascii_lowercase();

    // ── 第 1 档：镜像名 ────────────────────────────────────────────────
    //
    // 顺序要紧：`mariadb` 必须在 `mysql` **之前**判断吗？其实不会冲突
    // （"mariadb" 不含 "mysql"），但 `postgres` / `postgis` / `timescale`
    // 都含 "postgres" 之内的东西 —— 统统当 PostgreSQL，协议一样。
    if img.contains("mariadb") {
        return Some((DbKind::MariaDb, Confidence::High));
    }
    if img.contains("mysql") || img.contains("percona") || img.contains("tidb") {
        // Percona / TiDB 走 MySQL 协议，共用驱动（学 dbx 的"垫片"做法）
        return Some((DbKind::MySql, Confidence::High));
    }
    if img.contains("postgres") || img.contains("postgis") || img.contains("timescale") {
        return Some((DbKind::Postgres, Confidence::High));
    }
    // 有些镜像名不带引擎名，靠启动命令认（`postgres` 镜像的 entrypoint 是
    // `docker-entrypoint.sh postgres`）
    if cmd.contains("postgres") {
        return Some((DbKind::Postgres, Confidence::High));
    }
    if cmd.contains("mysqld") || cmd.contains("mariadbd") {
        return Some((DbKind::MySql, Confidence::High));
    }

    // ── 第 2 档：端口（低置信度）────────────────────────────────────────
    for port in hint.container_ports {
        match port {
            3306 => return Some((DbKind::MySql, Confidence::Low)),
            5432 => return Some((DbKind::Postgres, Confidence::Low)),
            _ => {}
        }
    }

    // ── 认不出就不猜 ──────────────────────────────────────────────────
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_image_names_from_this_machine() {
        // 实测：这台机器上的镜像名带 1panel 镜像站前缀
        assert_eq!(
            guess_kind(&ConnHint::new("docker.1panel.live/library/mysql:latest")),
            Some((DbKind::MySql, Confidence::High))
        );
        // redis 我们还不支持 → 必须返回 None，不能瞎猜成 MySQL
        assert_eq!(
            guess_kind(&ConnHint::new("docker.1panel.live/library/redis:latest")),
            None
        );
        assert_eq!(
            guess_kind(&ConnHint::new("docker.1panel.live/library/nginx:latest")),
            None
        );
    }

    #[test]
    fn official_and_versioned_names() {
        assert_eq!(
            guess_kind(&ConnHint::new("postgres:16")),
            Some((DbKind::Postgres, Confidence::High))
        );
        assert_eq!(
            guess_kind(&ConnHint::new("mariadb:11.2")),
            Some((DbKind::MariaDb, Confidence::High))
        );
        assert_eq!(
            guess_kind(&ConnHint::new("mysql:8.0.36")),
            Some((DbKind::MySql, Confidence::High))
        );
        // 大小写不敏感
        assert_eq!(
            guess_kind(&ConnHint::new("PostgreSQL_16")),
            Some((DbKind::Postgres, Confidence::High))
        );
    }

    #[test]
    fn mariadb_wins_over_mysql_when_both_substrings_appear() {
        // "mariadb" 和 "mysql" 同时出现时必须是 MariaDB：判反了会让用户
        // 在 MariaDB 上看到 MySQL 的关键字表（少一批 MariaDB 独有的词）。
        assert_eq!(
            guess_kind(&ConnHint::new("my-mariadb-mysql-proxy")),
            Some((DbKind::MariaDb, Confidence::High))
        );
    }

    #[test]
    fn postgres_family_and_forks() {
        for img in ["postgres:16-alpine", "postgis/postgis:16-3.4", "timescale/timescaledb:latest-pg16"] {
            assert_eq!(
                guess_kind(&ConnHint::new(img)),
                Some((DbKind::Postgres, Confidence::High)),
                "{img} 应该识别成 PostgreSQL"
            );
        }
    }

    #[test]
    fn mysql_protocol_forks_share_the_mysql_driver() {
        // 学 dbx 的垫片做法：同协议的只换显示名，共用驱动
        for img in ["percona:8", "pingcap/tidb:latest"] {
            assert_eq!(
                guess_kind(&ConnHint::new(img)),
                Some((DbKind::MySql, Confidence::High)),
                "{img} 走 MySQL 协议"
            );
        }
    }

    #[test]
    fn command_is_used_when_the_image_name_is_useless() {
        let hint = ConnHint {
            image: "mycompany/analytics:1.0",
            command: "docker-entrypoint.sh postgres -c max_connections=200",
            container_ports: &[],
        };
        assert_eq!(guess_kind(&hint), Some((DbKind::Postgres, Confidence::High)));
    }

    #[test]
    fn ports_are_only_a_low_confidence_fallback() {
        let ports = [3306u16];
        let hint = ConnHint::new("mycompany/whatever:1.0").with_ports(&ports);
        assert_eq!(guess_kind(&hint), Some((DbKind::MySql, Confidence::Low)));

        let ports2 = [5432u16];
        let hint2 = ConnHint::new("mycompany/whatever:1.0").with_ports(&ports2);
        assert_eq!(guess_kind(&hint2), Some((DbKind::Postgres, Confidence::Low)));
    }

    #[test]
    fn image_name_beats_ports() {
        // 镜像说 postgres、端口是 3306（谁映射的谁知道）→ 信镜像
        let ports = [3306u16];
        let hint = ConnHint::new("postgres:16").with_ports(&ports);
        assert_eq!(guess_kind(&hint), Some((DbKind::Postgres, Confidence::High)));
    }

    #[test]
    fn unknown_everything_returns_none() {
        let ports = [8080u16, 443u16];
        assert_eq!(guess_kind(&ConnHint::new("").with_ports(&ports)), None);
        assert_eq!(guess_kind(&ConnHint::default()), None);
    }
}
