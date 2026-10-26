//! 失败分类：把客户端 / 服务端的原文归到能给出"下一步"的几类。

/// 子进程报错大致是哪一类。
///
/// 判据全部来自**实测抓下来的原文**（`tests/fixtures/mysql_error_*.txt`），
/// 不靠猜。分类的意义是给出**下一步**：光把
/// `OCI runtime exec failed: …` 甩给用户，他不知道该干什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MysqlErrorKind {
    /// 容器没在运行（`WSLC_E_CONTAINER_NOT_RUNNING`）。
    ContainerNotRunning,
    /// 镜像里没有 mysql 客户端（exit 126 + `executable file not found`）。
    ClientMissing,
    /// 用户名或密码不对（`ERROR 1045`）。
    AccessDenied,
    /// mysqld 还没起来 / 连不上（`ERROR 2002` / `2003`）——
    /// 第一次初始化要十几秒，这是**等一会儿就好**的情况。
    ServerNotReady,
    /// 权限不足（`ERROR 1142` 之类）—— 典型是普通账号去读 `mysql.user`。
    PermissionDenied,
    /// 库不存在（`ERROR 1049` / `1008`）。
    UnknownDatabase,
    /// 其它 SQL 错误（语法、约束……）。原文照给。
    SqlError,
    /// 认不出来。
    Unknown,
}

impl MysqlErrorKind {
    /// 给用户的一句话建议（`None` = 直接看原文就行）。
    pub fn hint(self) -> Option<&'static str> {
        match self {
            Self::ContainerNotRunning => {
                Some("容器没在运行 —— 先到「容器」页把它启动起来，再回来点「重新读取」。")
            }
            Self::ClientMissing => Some(
                "这个镜像里没有 mysql / mariadb 客户端，面板没法在里面执行 SQL。\
                 请换官方 mysql / mariadb 镜像，或者用「终端」自己进去装一个。",
            ),
            Self::AccessDenied => {
                Some("用户名或密码不对。密码可以在容器详情里核对，或者看容器日志里那行临时密码。")
            }
            Self::ServerNotReady => {
                Some("mysqld 还没准备好（第一次初始化要十几秒）。等几秒再点一次「重新读取」。")
            }
            Self::PermissionDenied => {
                Some("当前账号权限不够（比如读 mysql.user 需要高权限账号）。换个账号试试。")
            }
            Self::UnknownDatabase => Some("这个库不存在 —— 可能是名字打错了，或者已经被删掉了。"),
            Self::SqlError | Self::Unknown => None,
        }
    }

    /// 要不要在界面上显示成「错误」（而不是"稍等"）。
    ///
    /// `ServerNotReady` 是**暂时**的，界面上用提醒色更合适。
    pub fn is_transient(self) -> bool {
        matches!(self, Self::ServerNotReady)
    }
}

/// 认原生连接（`wslc-db`）那条路已经分好类的错误。
///
/// 输入形如 `"{DbErrorKind::label()}：{服务端原文}"`（见 `wslc_db::DbError::summary`）。
///
/// # 为什么靠标签而不靠类型
///
/// `wslc-db` 的 `DbErrorKind` 才是权威，但 `wslc-panel-core` / 本来不该依赖
/// `wslc-db`（那会把它带的 mysql + postgres 两棵依赖树拉进数据层）。
/// 所以在不新增依赖的前提下，标签是唯一可靠的信号 —— 而它确实可靠：
/// 这六个中文短语**只有**原生路径会写进错误文本。
///
/// ⚠️ 改那六个标签的措辞时**必须同步改这里**（`wslc-db/src/session.rs`
/// 的 `DbErrorKind::label`）。漏改的表现是"原生错误又退回到按 MySQL 猜"
/// —— 那正是这条要修的东西。
///
/// # 映射（注意 `Connect` 不能落到 `ServerNotReady`）
///
/// `ServerNotReady` 对应的建议是"mysqld 还没准备好，**等几秒再点**"，
/// 那只对**容器里刚起的 mysqld** 成立。原生连接可以连一台远程机器，
/// 等多久都没用 —— 所以它映射成 `Unknown`（只说原文，不给误导的建议）。
fn classify_native_text(text: &str) -> Option<MysqlErrorKind> {
    // 只认冒号前那一段，别让服务端原文里恰好出现别的标签（比如一句
    // 报错里引用了 `认证失败`）把分类带偏。
    let label = text.split(['：', ':']).next().unwrap_or("").trim();
    Some(match label {
        "认证失败" => MysqlErrorKind::AccessDenied,
        "库不可用" => MysqlErrorKind::UnknownDatabase,
        // 原生路径走到这里说明**连接层**就失败了。别给"等几秒"那条建议
        // （那是容器里 mysqld 的场景），见上面文档。
        "连不上服务器" => MysqlErrorKind::Unknown,
        "SQL 执行出错" => MysqlErrorKind::SqlError,
        // TLS 对用户来说也是"连不上"，但**原因完全不同**（证书 / 服务端配置）。
        // 归成 `Unknown`，让原文（含 "TLS handshake"）自己说话。
        "TLS 握手失败" => MysqlErrorKind::Unknown,
        "数据库错误" => MysqlErrorKind::Unknown,
        _ => return None,
    })
}

/// 按**报错原文**（和退出码）分类。
///
/// ⚠️ 退出码**不能**单独作为判据：实测三种情况分别是 1（密码错）、
/// 1（容器没运行）、**126**（没有客户端），而 126 只是"命令没找到"的约定，
/// 真正稳的是那些文本（`WSLC_E_CONTAINER_NOT_RUNNING` / `executable file not found`
/// 是英文的稳定标识，不会随系统语言变）。
pub fn classify_error(text: &str, code: Option<i32>) -> MysqlErrorKind {
    let lower = text.to_ascii_lowercase();

    if text.contains("WSLC_E_CONTAINER_NOT_RUNNING") {
        return MysqlErrorKind::ContainerNotRunning;
    }

    // ── 原生连接（`wslc-db`）的错：**已经分好类了**，别拿 MySQL 的规则重猜 ──
    //
    // 原生那条路（v0.16）在 `wslc-db` 里就按 `DbErrorKind` 分好了
    // （连不上 / 认证 / 库不可用 / SQL 错 / TLS），而它的 `Display` 是
    // `"{分类标签}：{服务端原文}"`。这六个中文标签**只有原生路径会产生**
    // （exec 路径的错都是 `wslc` 的英文 stderr），所以它们是可靠的路由标记。
    //
    // ⚠️ 为什么要在这里认，而不是"直接传类型"：`wslc-panel-core` 不依赖
    // `wslc-db`（那会把它带的两棵依赖树拉进数据层）—— 见 `DbState::dsn`
    // 那条注释。所以在**不新增依赖**的前提下，只能靠这段前缀。
    //
    // 不认的后果（实测过，不是推测）：
    // - PG 的 `连不上服务器：… Connection refused` 会命中下面的
    //   `connection refused` → `ServerNotReady` → 界面上建议
    //   **"mysqld 还没准备好（第一次初始化要十几秒），等几秒再点"** ——
    //   而用户连的是一台远程 PostgreSQL，等一辈子也不会好；
    // - PG 的 `认证失败：password authentication failed` 一个关键词都不命中
    //   → `Unknown` → **一句建议都没有**，用户不知道该去改口令。
    if let Some(kind) = classify_native_text(text) {
        return kind;
    }

    if lower.contains("executable file not found")
        || lower.contains("no such file or directory")
        || code == Some(126)
        || code == Some(127)
    {
        return MysqlErrorKind::ClientMissing;
    }
    if lower.contains("error 1045") || lower.contains("access denied") {
        return MysqlErrorKind::AccessDenied;
    }
    if lower.contains("error 2002")
        || lower.contains("error 2003")
        || lower.contains("can't connect")
        || lower.contains("connection refused")
    {
        return MysqlErrorKind::ServerNotReady;
    }
    if lower.contains("error 1142")
        || lower.contains("error 1227")
        || lower.contains("command denied")
    {
        return MysqlErrorKind::PermissionDenied;
    }
    if lower.contains("error 1049") || lower.contains("unknown database") {
        return MysqlErrorKind::UnknownDatabase;
    }
    if lower.contains("error ") {
        return MysqlErrorKind::SqlError;
    }
    MysqlErrorKind::Unknown
}
