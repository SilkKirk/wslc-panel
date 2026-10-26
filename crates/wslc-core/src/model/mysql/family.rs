//! 家族与识别：`MysqlFamily` / `MysqlSignal` / `mysql_container_kind` 与三个 `family_from_*`。

use super::super::{ContainerInspect, ContainerListItem, image_last_segment};

/// 认出来的镜像家族。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MysqlFamily {
    /// 官方 `mysql` 镜像。
    Mysql,
    /// 官方 `mariadb` 镜像（协议兼容，客户端可能叫 `mariadb`）。
    Mariadb,
    /// Percona Server。
    Percona,
}

impl MysqlFamily {
    /// 中文显示名（界面上标出"这是哪一族"）。
    pub fn label(self) -> &'static str {
        match self {
            Self::Mysql => "MySQL",
            Self::Mariadb => "MariaDB",
            Self::Percona => "Percona Server",
        }
    }

    /// 家族默认端口。
    pub fn default_port(self) -> u16 {
        3306
    }

    /// 兜底的客户端二进制名（探测失败时用）。
    ///
    /// 三个家族**都是 `mysql`**：mariadb 镜像里 `mysql` 是指向 `mariadb` 的
    /// 兼容符号链接，percona 直接就是 mysql 的客户端。
    pub fn fallback_client(self) -> &'static str {
        "mysql"
    }
}

/// 「凭什么认为它是 MySQL」。
///
/// 三种依据的可靠程度**不一样**，所以不合并成一个 `bool`：界面上可以直接
/// 把依据说出来（"按端口 3306 猜的"），用户就知道该不该信。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MysqlSignal {
    /// 镜像名认出来的（最可靠）。
    Image,
    /// 启动命令里有 `mysqld` / `mariadbd`。
    Command,
    /// 发布了 3306 端口（**猜**的 —— 3306 是 IANA 给 MySQL 的端口）。
    Port,
}

impl MysqlSignal {
    /// 给用户看的一句话。
    pub fn label(self) -> &'static str {
        match self {
            Self::Image => "按镜像名判断",
            Self::Command => "按启动命令判断",
            Self::Port => "按 3306 端口判断（可能是别的程序）",
        }
    }
}

/// 一次判定结果：是哪一族 + 依据是什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MysqlCandidate {
    /// 家族。
    pub family: MysqlFamily,
    /// 判定依据。
    pub signal: MysqlSignal,
}

/// 从**列表数据**判断这个容器是不是 MySQL（零成本，渲染期可用）。
///
/// 三层依据，便宜的在前面：
///
/// 1. 镜像名（`Image`）—— 最可靠，且**不受截断影响**；
/// 2. 启动命令（`Command`）—— ⚠️ 实测 `wslc list` 把它截断成 20 字符
///    （`"docker-entrypoint.s…"`），所以官方 mysql 镜像在这条上**认不出来**，
///    只有用户显式把命令写成 `mysqld` 才命中；
/// 3. 发布端口里有 3306（`Labels` 元数据）—— 这条是**猜**，一个恰好发布
///    3306 的非 MySQL 容器会被误判。代价可接受：点进去之后 L2/L3 会给出
///    权威结论（`cmd::mysql::detect`），误判只表现为"多了一个按钮"。
pub fn mysql_container_kind(item: &ContainerListItem) -> Option<MysqlCandidate> {
    if let Some(family) = family_from_image(&item.image) {
        return Some(MysqlCandidate {
            family,
            signal: MysqlSignal::Image,
        });
    }
    if let Some(family) = family_from_command(&item.command_clean()) {
        return Some(MysqlCandidate {
            family,
            signal: MysqlSignal::Command,
        });
    }
    if item
        .wsl_metadata_ports()
        .iter()
        .any(|p| p.container_port == 3306)
    {
        return Some(MysqlCandidate {
            family: MysqlFamily::Mysql,
            signal: MysqlSignal::Port,
        });
    }
    None
}

/// 按镜像名认家族。认不出来返回 `None`（**什么都不做**，和 `image_preset` 同一条原则）。
///
/// 表刻意小：只认最常见的几个。命名带前缀的那种（`docker.1panel.live/library/mysql:8`）
/// 走 `model::image_last_segment` 取最后一段，所以自动命中。
/// （它和 `deploy` 那一族共用一份实现，见 `model/mod.rs` 的说明。）
pub fn family_from_image(image: &str) -> Option<MysqlFamily> {
    match image_last_segment(image).as_str() {
        "mysql" | "mysql-server" | "mysql-community-server" => Some(MysqlFamily::Mysql),
        "mariadb" | "mariadb-server" => Some(MysqlFamily::Mariadb),
        "percona" | "percona-server" => Some(MysqlFamily::Percona),
        _ => None,
    }
}

/// 按启动命令认家族（`mysqld` / `mariadbd`）。
pub fn family_from_command(command: &str) -> Option<MysqlFamily> {
    let lower = command.to_ascii_lowercase();
    if lower.contains("mariadbd") {
        Some(MysqlFamily::Mariadb)
    } else if lower.contains("mysqld") {
        // Percona 的进程也叫 mysqld，光看命令分不出来 —— 归 MySQL
        // （两者界面上的行为完全一致，分错也没有后果）。
        Some(MysqlFamily::Mysql)
    } else {
        None
    }
}

/// 从 `inspect` 的结果判断（**权威判定**，进页面时跑一次）。
///
/// 顺序：镜像引用 → 环境变量 → 命令 / entrypoint → 端口。
/// 都比列表那条启发式强：`inspect` 的字段是**完整**的（没有被截断的 `Command`）。
pub fn family_from_inspect(inspect: &ContainerInspect) -> Option<MysqlFamily> {
    if let Some(family) = inspect.image_ref().and_then(family_from_image) {
        return Some(family);
    }

    // 环境变量：官方入口脚本认的那几个键都是 `MYSQL_` / `MARIADB_` 打头的
    // （`MYSQL_ROOT_PASSWORD` / `MARIADB_ROOT_PASSWORD` / `MYSQL_DATABASE` ……），
    // 所以按**前缀**认就够了 —— 列一张键名表反而会漏掉新版本加的键。
    // mariadb 放前面：它的入口脚本两套名字都认，但用户建的是 mariadb。
    let env = inspect.env();
    let has_prefix = |prefix: &str| env.iter().any(|line| line.starts_with(prefix));
    if has_prefix("MARIADB_") {
        return Some(MysqlFamily::Mariadb);
    }
    if has_prefix("MYSQL_") {
        return Some(MysqlFamily::Mysql);
    }

    // 命令 / entrypoint：这里**没有截断**，所以 `mysqld` 是可靠的。
    let mut line = inspect.cmd().join(" ");
    line.push(' ');
    line.push_str(&inspect.entrypoint().join(" "));
    if let Some(family) = family_from_command(&line) {
        return Some(family);
    }

    // 端口（`inspect` 的 Ports 键形如 `3306/tcp`）。
    if inspect
        .ports()
        .iter()
        .any(|(key, _)| key.starts_with("3306/"))
    {
        return Some(MysqlFamily::Mysql);
    }

    None
}
