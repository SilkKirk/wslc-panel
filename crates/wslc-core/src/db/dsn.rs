//! 连接串的**脱敏**（纯字符串，不解析、不连库）。
//!
//! 连接串里有明文口令，而界面上的那一行、`Debug` 输出、日志都会被截图 / 贴到
//! issue 里。这里只打掉口令段，保留协议 / 用户 / 主机 / 端口 / 库 ——
//! 用户要能确认"连的是哪台"。
//!
//! # 为什么放在 `wslc-core`
//!
//! `wslc-db` 的 `ConnSpec` 和面板侧的 `DsnText` 都要用同一套判据。早先
//! 两边各写一份（`wslc-db::redact_dsn` 与 `panel-core::redact_dsn_password`），
//! 对 `?query` / 未编码 `@` 的处理并不完全一致。`wslc-db` 和
//! `wslc-panel-core` 都依赖 `wslc-core`，所以唯一能共享的位置在这里。
//!
//! 认不出形状就**原样返回**：宁可显示不完全，也不要显示错。

/// 把 `scheme://user:password@host…` 里的 `password` 换成 `***`。
///
/// # 切法
///
/// 1. 先找 `://`，没有就原样返回（不像连接串的东西，没什么可遮的）；
/// 2. 权限段是 `://` 之后到第一个 `/` 或 `?` 之前 —— **不能**一路找到
///    query 里的 `@`；
/// 3. 权限段里找**最后一个** `@`：口令里出现未编码的 `@` 很常见
///    （`mysql://root:p@ss@h/db`），按第一个切会把主机名当成口令的一部分；
/// 4. `:` 到 `@` 之间就是口令。
pub fn redact_dsn(text: &str) -> String {
    let Some(scheme_end) = text.find("://") else {
        return text.to_owned();
    };
    let auth_start = scheme_end + 3;
    // 权限段结束：第一个 `/` 或 `?`（都没有就到末尾）
    let auth_end = text[auth_start..]
        .find(['/', '?'])
        .map(|i| auth_start + i)
        .unwrap_or(text.len());
    let auth = &text[auth_start..auth_end];
    let Some(at) = auth.rfind('@') else {
        // 没有 `@` = 没有 userinfo，也就没有口令
        return text.to_owned();
    };
    let userinfo = &auth[..at];
    let Some(colon) = userinfo.find(':') else {
        // `user@host` 形式：没有口令
        return text.to_owned();
    };
    // `user:` + `***` + `@host…`
    format!(
        "{}{}:***{}",
        &text[..auth_start],
        &userinfo[..colon],
        &text[auth_start + at..]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hides_the_password_but_keeps_the_rest() {
        assert_eq!(
            redact_dsn("postgres://alice:s3cret@db.example.com:5432/shop"),
            "postgres://alice:***@db.example.com:5432/shop"
        );
        // 没有口令段 → 原样（别把 `@` 后面的东西吃掉）
        assert_eq!(
            redact_dsn("postgres://alice@h:5432/db"),
            "postgres://alice@h:5432/db"
        );
        // 认不出形状 → 原样返回，不猜
        assert_eq!(redact_dsn("host=127.0.0.1"), "host=127.0.0.1");
        assert_eq!(redact_dsn(""), "");
    }

    #[test]
    fn an_unencoded_at_in_the_password_is_still_redacted() {
        // 取**最后一个** `@`：按第一个切会把 `ss@10.0.0.5` 当成主机名。
        assert_eq!(
            redact_dsn("mysql://root:p@ss@10.0.0.5:3306/db"),
            "mysql://root:***@10.0.0.5:3306/db"
        );
    }

    #[test]
    fn a_query_string_is_not_treated_as_part_of_the_password() {
        // `?` 之后不是 userinfo；早先按整段 rfind('@') 的写法在带 query 的
        // 连接串上会读错边界。
        assert_eq!(
            redact_dsn("postgres://u:p@h:5432/db?sslmode=require"),
            "postgres://u:***@h:5432/db?sslmode=require"
        );
    }

    #[test]
    fn a_colon_without_an_at_is_left_alone() {
        assert_eq!(redact_dsn("mysql://root:pw@h"), "mysql://root:***@h");
        assert_eq!(redact_dsn("postgres://h:5432/db"), "postgres://h:5432/db");
    }
}
