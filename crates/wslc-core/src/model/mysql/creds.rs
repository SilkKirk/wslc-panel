//! 凭据：`MysqlCreds`（`Debug` 手动脱敏）、`RootCredHint`、从容器环境变量读 root 线索。

/// 连接用的凭据（用户名 / 密码 / 可选的主机端口）。
///
/// # ⚠️ 密码绝不能进日志
///
/// `Debug` 是**手写**的：`AppState` 是 `#[derive(Debug)]`，而 `DbState` 住在
/// 它里面 —— 派生出来的 `Debug` 会在任何一次调试打印（`tracing`、panic 消息、
/// `assert_eq!` 失败输出）里把明文密码打出来。这里打成 `"***"`。
///
/// 同理，密码**不进任何命令行参数**：它只出现在 [`exec_env`] 里
/// （`wslc exec -e MYSQL_PWD=…`），而错误消息里的参数串一律先过
/// [`redact_args`]。
#[derive(Clone, PartialEq, Eq)]
pub struct MysqlCreds {
    /// 用户名（面板默认填 `root`）。
    pub user: String,
    /// 密码；空串是**合法**值（`MYSQL_ALLOW_EMPTY_PASSWORD=yes` 那种）。
    pub password: String,
    /// 主机；`None` = 让客户端自己决定（容器里跑就是 unix socket）。
    pub host: Option<String>,
    /// 端口；`None` = 客户端默认。
    pub port: Option<u16>,
}

impl std::fmt::Debug for MysqlCreds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MysqlCreds")
            .field("user", &self.user)
            // 只说"有没有"，不说是什么
            .field(
                "password",
                &if self.password.is_empty() {
                    "<empty>"
                } else {
                    "***"
                },
            )
            .field("host", &self.host)
            .field("port", &self.port)
            .finish()
    }
}

impl MysqlCreds {
    /// 用 root + 密码建一份。
    pub fn root(password: impl Into<String>) -> Self {
        Self {
            user: "root".to_owned(),
            password: password.into(),
            host: None,
            port: None,
        }
    }

    /// 用户名 / 主机对（错误提示里用它说清"是谁连不上"）。
    pub fn user_label(&self) -> String {
        format!(
            "{}@{}",
            self.user,
            self.host.as_deref().unwrap_or("localhost")
        )
    }
}

/// 从容器环境变量里读出来的 root 凭据线索。
///
/// 这是"用户不用手打密码"的来源：`inspect` 的 `Config.Env` 里就有
/// `MYSQL_ROOT_PASSWORD`（**实测**本机那个 mysql 容器就是 `MYSQL_ROOT_PASSWORD=root`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootCredHint {
    /// 环境变量里直接写着 root 密码。
    Password(String),
    /// 镜像允许空密码（`MYSQL_ALLOW_EMPTY_PASSWORD=yes` 之类）。
    Empty,
    /// 镜像生成了随机密码，写在容器日志里 —— 我们读不到，只能让用户自己去看日志。
    Random,
    /// 什么都没有（用户得自己填）。
    Unknown,
}

impl RootCredHint {
    /// 给用户看的一句话（`None` = 不用说什么）。
    pub fn note(&self) -> Option<String> {
        match self {
            Self::Password(_) => Some(
                "已从容器环境变量（MYSQL_ROOT_PASSWORD）读出 root 密码，直接点「连接」即可。\
                 密码只存在内存里，关掉面板就没了。"
                    .to_owned(),
            ),
            Self::Empty => {
                Some("容器允许 root 空密码（MYSQL_ALLOW_EMPTY_PASSWORD），密码留空即可。".to_owned())
            }
            Self::Random => Some(
                "这个容器用的是随机 root 密码（MYSQL_RANDOM_ROOT_PASSWORD）—— 它只写在\
                 容器日志里。请先到「日志」里找 \u{201c}GENERATED ROOT PASSWORD\u{201d} 那一行，再填到这里。"
                    .to_owned(),
            ),
            Self::Unknown => None,
        }
    }
}

/// 从 `Config.Env`（形如 `KEY=VALUE` 的字符串数组）里找 root 凭据线索。
///
/// 优先级：**明确的密码** > 空密码 > 随机密码 —— 都设了的话，
/// 入口脚本认的是明确的那个（实测的容器就是 `MYSQL_ROOT_PASSWORD=root`）。
pub fn root_credential_from_env(env: &[String]) -> RootCredHint {
    let value = |key: &str| -> Option<String> {
        env.iter()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
            .map(|v| v.trim().to_owned())
    };
    let truthy = |key: &str| -> bool {
        value(key).is_some_and(|v| {
            let v = v.to_ascii_lowercase();
            matches!(v.as_str(), "yes" | "y" | "true" | "1" | "on")
        })
    };

    for key in ["MYSQL_ROOT_PASSWORD", "MARIADB_ROOT_PASSWORD"] {
        if let Some(v) = value(key) {
            return if v.is_empty() {
                RootCredHint::Empty
            } else {
                RootCredHint::Password(v)
            };
        }
    }
    for key in [
        "MYSQL_ALLOW_EMPTY_PASSWORD",
        "MARIADB_ALLOW_EMPTY_ROOT_PASSWORD",
    ] {
        if truthy(key) {
            return RootCredHint::Empty;
        }
    }
    for key in ["MYSQL_RANDOM_ROOT_PASSWORD", "MARIADB_RANDOM_ROOT_PASSWORD"] {
        if truthy(key) {
            return RootCredHint::Random;
        }
    }
    RootCredHint::Unknown
}
