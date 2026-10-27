//! SQL 生成与校验（标识符白名单 / 建库建用户 / 授权）。

use crate::error::{Error, Result};

/// 校验 MySQL **标识符**（库名 / 用户名 / 字符集名）。
///
/// # 为什么是白名单，而不是"转义后拼接"
///
/// 用户输入会直接进 SQL。转义要处理的是"哪种引号、反斜杠、`NO_BACKSLASH_ESCAPES`
/// 模式、多字节编码"，每一条都能写错；白名单只有一条规则：**只有
/// `[A-Za-z0-9_$]` 且不超过 64 字节才算合法**。不合法的**拒绝**（把选择权交回用户），
/// 而不是我们猜他的意图。
///
/// ⚠️ 通过校验之后**仍然**用反引号包起来（`ORDER` 这种保留字做库名是合法的，
/// 不包就变成语法错误）。
pub fn validate_ident(kind: &str, name: &str) -> Result<()> {
    let name = name.trim();
    if name.is_empty() {
        return Err(Error::InvalidArgument(format!("{kind}不能为空")));
    }
    if name.len() > 64 {
        return Err(Error::InvalidArgument(format!(
            "{kind}不能超过 64 个字符（MySQL 的限制），当前 {} 个",
            name.len()
        )));
    }
    if let Some(bad) = name
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || *c == '_' || *c == '$'))
    {
        return Err(Error::InvalidArgument(format!(
            "{kind}「{name}」里有不支持的字符 `{bad}` —— 只允许字母、数字、下划线和 $"
        )));
    }
    Ok(())
}

/// 校验 MySQL 的 **host** 部分（`'user'@'host'` 里那个 host）。
///
/// 比标识符宽松：可以是 `%`、`192.168.1.%`、`localhost`、域名。
pub fn validate_host(host: &str) -> Result<()> {
    let host = host.trim();
    if host.is_empty() {
        return Err(Error::InvalidArgument(
            "主机不能为空（本机请填 localhost）".to_owned(),
        ));
    }
    if host.len() > 255 {
        return Err(Error::InvalidArgument("主机名过长".to_owned()));
    }
    if let Some(bad) = host
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '%' | ':' | '*')))
    {
        return Err(Error::InvalidArgument(format!(
            "主机「{host}」里有不支持的字符 `{bad}`"
        )));
    }
    Ok(())
}

/// 字符集那一格的错误文本里一定含 `字符集` 这个词。
///
/// ⚠️ **这是界面依赖的一条字面契约**：`app::shell::db::dialog` 在用户点
/// 「创建库」校验失败时，靠 `message.contains(CHARSET_LABEL)` /
/// `COLLATION_LABEL` 决定把焦点送到「编码」折叠区里的哪一格
/// （那是唯一一处看起来"什么都做了、其实没聚焦到对的地方"的地方）。
///
/// 所以这两个词**别翻译、别改写**：改了之后那个 `contains` 静默失效，
/// 表现是"报错了，但没带你去改哪一格" —— 而它不会有任何编译错误。
///
/// 更好的做法是给 `Error` 加一个结构化的 `InvalidArgument { field, hint }`，
/// 但那要把 `InvalidArgument(String)` 这个变体在整个仓库里改一遍
/// （20+ 处构造点、以及 `Display` 的实现）。当前只有这一处读者，
/// 先把契约写在**定义处**，比留在调用点靠注释记着强。
pub const CHARSET_LABEL: &str = "字符集";

/// 排序规则那一格的错误文本里一定含 `排序规则` 这个词（理由同 [`CHARSET_LABEL`]）。
pub const COLLATION_LABEL: &str = "排序规则";

/// 校验字符集 / 排序规则名（`utf8mb4` / `utf8mb4_0900_ai_ci`）。
pub fn validate_charset(kind: &str, value: &str) -> Result<()> {
    let value = value.trim();
    if value.is_empty() {
        return Err(Error::InvalidArgument(format!("{kind}不能为空")));
    }
    if let Some(bad) = value
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || *c == '_'))
    {
        return Err(Error::InvalidArgument(format!(
            "{kind}「{value}」里有不支持的字符 `{bad}`"
        )));
    }
    Ok(())
}

/// 把一段文本变成 MySQL 的**单引号字符串字面量**（含两侧引号）。
///
/// 只做两件事：反斜杠 → `\\`、单引号 → `\'`；另外把双引号也转掉
/// （`"` 在 MySQL 里只有开了 `ANSI_QUOTES` 才有特殊含义，转掉它没有任何代价，
/// 而它正好会让**拼给 `wslc.exe` 的整条命令行**更容易被 Windows 的参数规则
/// 弄坏 —— 实际测过带引号的 SQL 能原样到达，见模块文档那张表）。
///
/// 用在**密码**上（密码允许任意字符，所以这里必须转义，不能像标识符那样白名单）。
pub fn quote_literal(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '"' => out.push_str("\\\""),
            _ => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// 反引号包起来的标识符（**必须先过 [`validate_ident`]**）。
fn ident(name: &str) -> String {
    format!("`{}`", name.trim())
}

/// `CREATE DATABASE`。
///
/// 字符集 / 排序规则留空就交给服务器的默认值（**不替用户猜**：
/// 猜一个 `utf8mb4` 看起来贴心，实际上是替他做了一个他看不见的决定）。
pub fn create_database_sql(
    name: &str,
    charset: Option<&str>,
    collation: Option<&str>,
) -> Result<String> {
    validate_ident("库名", name)?;
    let mut sql = format!("CREATE DATABASE {}", ident(name));
    if let Some(charset) = charset.map(str::trim).filter(|s| !s.is_empty()) {
        validate_charset(CHARSET_LABEL, charset)?;
        sql.push_str(&format!(" DEFAULT CHARACTER SET {charset}"));
    }
    if let Some(collation) = collation.map(str::trim).filter(|s| !s.is_empty()) {
        validate_charset(COLLATION_LABEL, collation)?;
        sql.push_str(&format!(" DEFAULT COLLATE {collation}"));
    }
    Ok(sql)
}

/// `DROP DATABASE`（**不加** `IF EXISTS`：用户是确认过才点的，
/// 库要是不在了，报一句 `ERROR 1008` 比"静默成功"诚实）。
pub fn drop_database_sql(name: &str) -> Result<String> {
    validate_ident("库名", name)?;
    Ok(format!("DROP DATABASE {}", ident(name)))
}

/// `CREATE USER`（`IF NOT EXISTS`：同名同 host 的账号已存在时报错没有任何价值）。
pub fn create_user_sql(user: &str, host: &str, password: &str) -> Result<String> {
    validate_ident("用户名", user)?;
    validate_host(host)?;
    Ok(format!(
        "CREATE USER IF NOT EXISTS '{}'@'{}' IDENTIFIED BY {}",
        user.trim(),
        host.trim(),
        quote_literal(password)
    ))
}

/// `ALTER USER … IDENTIFIED BY`（改密码）。
pub fn alter_user_password_sql(user: &str, host: &str, password: &str) -> Result<String> {
    validate_ident("用户名", user)?;
    validate_host(host)?;
    Ok(format!(
        "ALTER USER '{}'@'{}' IDENTIFIED BY {}",
        user.trim(),
        host.trim(),
        quote_literal(password)
    ))
}

/// 删除用户（`IF EXISTS`：删两次不该报错，用户要的结果是"它不在了"）。
pub fn drop_user_sql(user: &str, host: &str) -> Result<String> {
    validate_ident("用户名", user)?;
    validate_host(host)?;
    Ok(format!(
        "DROP USER IF EXISTS '{}'@'{}'",
        user.trim(),
        host.trim()
    ))
}

/// 授权级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantKind {
    /// 全部权限（等价于面板里最常见的那一下）。
    All,
    /// 只读。
    ReadOnly,
    /// 读写（不改结构、不授权）。
    ReadWrite,
}

impl GrantKind {
    /// 全部可选值（界面上按这个顺序排按钮）。
    pub const ALL: [GrantKind; 3] = [Self::All, Self::ReadOnly, Self::ReadWrite];

    /// 中文标签。
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "全部权限",
            Self::ReadOnly => "只读（SELECT）",
            Self::ReadWrite => "读写（增删改查）",
        }
    }

    /// 给 `GRANT` 用的权限串。
    pub fn privileges(self) -> &'static str {
        match self {
            Self::All => "ALL PRIVILEGES",
            Self::ReadOnly => "SELECT",
            Self::ReadWrite => "SELECT, INSERT, UPDATE, DELETE",
        }
    }
}

/// `GRANT <权限> ON <库>.* TO 'user'@'host'`。
///
/// MySQL 8 起 `GRANT` 会自己刷新权限表，**不需要** `FLUSH PRIVILEGES`。
pub fn grant_sql(database: &str, user: &str, host: &str, kind: GrantKind) -> Result<String> {
    validate_ident("库名", database)?;
    validate_ident("用户名", user)?;
    validate_host(host)?;
    Ok(format!(
        "GRANT {} ON {}.* TO '{}'@'{}'",
        kind.privileges(),
        ident(database),
        user.trim(),
        host.trim()
    ))
}

/// 建用户 + （可选）授权，合成**一条** `-e` 语句（用 `;` 分隔，实测能跑）。
///
/// 合成而不是跑两次：少一次 `wslc exec`（每次几百毫秒），而且**原子性更好** ——
/// 授权失败时不会留下一个"建好了但没权限"的账号让用户以为哪一步没生效
/// （MySQL 的 DDL 不回滚，但至少错误是一起报出来的）。
pub fn create_user_statements(
    user: &str,
    host: &str,
    password: &str,
    grant: Option<(&str, GrantKind)>,
) -> Result<String> {
    let mut sql = create_user_sql(user, host, password)?;
    if let Some((database, kind)) = grant {
        sql.push_str("; ");
        sql.push_str(&grant_sql(database, user, host, kind)?);
    }
    Ok(sql)
}
