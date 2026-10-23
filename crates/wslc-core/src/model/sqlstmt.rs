//! SQL 脚本的**客户端切分**：把一段脚本拆成一条条可以逐条执行的语句。
//!
//! # 为什么必须切（这是整个 SQL 工作区的地基）
//!
//! 一次把整段脚本交给 `mysql -e` 时，**多个结果集是直接粘连的**，中间没有
//! 任何分隔符。实测（本机 wslc 3.0.3.0 / mysql 26.7.0，2026-10）：
//!
//! ```text
//! $ mysql -uroot -B -e "SELECT 1 AS a; SELECT 2 AS b"
//! a
//! 1
//! b
//! 2
//! ```
//!
//! 四行连着打出来 —— 分不清哪一行属于第一个结果集。所以"一次执行整段、
//! 再切结果"这条路走不通。
//!
//! 逐条执行就没有这个问题：每条语句的输出天然独立。
//! [`super::sqlresult::parse_batch_with_header`] 只用处理"一条语句的输出"。
//!
//! # 参考：dbx 也是这么做的
//!
//! `t8y2/dbx`（AGPL-3.0，**只读来理解机制，一行代码都没抄**）的
//! `apps/desktop/src/components/editor/useQueryEditorStatementBoundaries.ts`
//! 用 CodeMirror 在**客户端**切出"可执行语句的 range"，再逐条交给驱动执行。
//! 它的结构化数据来自驱动（`sqlx`），从不解析文本表格 —— 我们架构不同
//! （只能走 `wslc exec`），但"客户端切分 + 逐条执行"这一半是一样的。
//!
//! # 代价
//!
//! 每条语句一次 `wslc exec`，实测固定开销 **~70ms**（`SELECT 1` 0.07s）。
//! 3 条语句 0.26s。交互式查询完全够用，但**一次跑几十条会明显变慢** ——
//! 界面上要说明这件事（见 `views::db_console_card`）。
//!
//! # 不认识的东西（明确报错，不猜）
//!
//! - **`DELIMITER`**：它是**客户端命令**，不是 SQL，`mysql -e` 也不认。
//!   要支持得改成 stdin 喂数据 + 复杂状态机，这一版不做。
//! - **存储过程 / 触发器 / 函数定义**（`CREATE PROCEDURE ... BEGIN ... END`）：
//!   里面的 `;` 是真分隔符，但我们没有 `DELIMITER` 就切不对。
//!   遇到 `CREATE PROCEDURE` / `CREATE TRIGGER` / `CREATE FUNCTION` 时**整段当成一条**，
//!   让服务端去报错，而不是我们切错了给出一堆莫名其妙的语法错误。

/// 把一段脚本切成**可逐条执行**的语句。
///
/// 返回的每条都已去掉首尾空白；空语句（只有空白 / 只有注释）**不返回**。
///
/// # 不切的地方（每一条都是坑）
///
/// - `'...'` / `"..."` / `` `...` `` 里的 `;` 不是分隔符；
/// - `--` 行注释、`#` 行注释、`/* */` 块注释里的 `;` 不是分隔符；
/// - `\'` 这种**反斜杠转义**在 MySQL 的字符串里有效（`sql_mode` 没开
///   `NO_BACKSLASH_ESCAPES` 时），所以"引号里遇到 `\` 就跳过一个字符"；
/// - 反引号里的 `` ` `` 用**两个反引号**转义（`a``b`），不是反斜杠。
///
/// # 例子
///
/// ```
/// # use wslc_core::model::sqlstmt::split_statements;
/// assert_eq!(split_statements("SELECT 1; SELECT 2"), vec!["SELECT 1", "SELECT 2"]);
/// // 引号里的分号不切
/// assert_eq!(split_statements("SELECT 'a;b'"), vec!["SELECT 'a;b'"]);
/// // 注释里的分号不切
/// assert_eq!(split_statements("SELECT 1 -- a;b"), vec!["SELECT 1 -- a;b"]);
/// ```
pub fn split_statements(sql: &str) -> Vec<String> {
    // 存储过程 / 触发器 / 函数 / 事件：内部有大量真 `;`，没有 `DELIMITER`
    // 就切不对 —— 整段当一条，让服务端自己报错（见模块文档）。
    // ⚠️ 这个判断必须在切分**之前**，不要在循环里再处理一次（那样两处逻辑
    // 会互相打架：`start` 已经被某个 `;` 重置过了）。
    if starts_a_routine(sql) {
        let s = sql.trim();
        return if s.is_empty() {
            Vec::new()
        } else {
            vec![s.to_owned()]
        };
    }

    let bytes: Vec<char> = sql.chars().collect();
    let mut out: Vec<String> = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;

    while i < bytes.len() {
        let c = bytes[i];

        // --- 引号：一直吃到配对的收尾引号 ---
        if c == '\'' || c == '"' || c == '`' {
            i = skip_quoted(&bytes, i);
            continue;
        }

        // --- 行注释 `-- ` / `#` ---
        //
        // `--` 后面**必须**跟空白或行尾才算注释（MySQL 的规矩）：
        // `1--2` 是 `1 - (-2)`，不是注释。
        if c == '#' {
            i = skip_to_line_end(&bytes, i);
            continue;
        }
        if c == '-' && i + 1 < bytes.len() && bytes[i + 1] == '-' {
            let after = bytes.get(i + 2).copied();
            let is_comment = match after {
                None => true,
                Some(ch) => ch.is_whitespace(),
            };
            if is_comment {
                i = skip_to_line_end(&bytes, i);
                continue;
            }
        }

        // --- 块注释 `/* ... */`（`/*! ... */` 这种"可执行注释"也一起跳过，
        //     它里面的 `;` 同样不是分隔符）---
        if c == '/' && i + 1 < bytes.len() && bytes[i + 1] == '*' {
            i = skip_block_comment(&bytes, i);
            continue;
        }

        // --- 真正的分隔符 ---
        if c == ';' {
            push_trimmed(&mut out, &bytes[start..i]);
            i += 1;
            start = i;
            continue;
        }

        i += 1;
    }

    push_trimmed(&mut out, &bytes[start..]);
    out
}

/// 这条语句会返回结果集吗？（只用来决定"无输出"该怎么措辞，不参与执行）
///
/// ⚠️ 这是**启发式**，不是 SQL 解析器。判错的后果只有一句文案：
/// 它说"无输出"而其实有结果集时，界面上仍然会显示真实结果（因为判断依据
/// 永远是**实际输出**，不是这个函数）。所以这里可以粗略。
pub fn returns_rows(sql: &str) -> bool {
    let head = first_keyword(sql);
    matches!(
        head.as_str(),
        "SELECT"
            | "SHOW"
            | "DESCRIBE"
            | "DESC"
            | "EXPLAIN"
            | "WITH"
            | "TABLE"
            | "VALUES"
            | "CALL"
    )
}

/// 第一个关键字（大写）。跳过注释和括号。
fn first_keyword(sql: &str) -> String {
    let bytes: Vec<char> = sql.chars().collect();
    let mut i = 0usize;
    loop {
        while i < bytes.len() && bytes[i].is_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            return String::new();
        }
        // 跳过行注释
        if bytes[i] == '#' {
            i = skip_to_line_end(&bytes, i);
            continue;
        }
        if bytes[i] == '-' && i + 1 < bytes.len() && bytes[i + 1] == '-' {
            let after = bytes.get(i + 2).copied();
            if after.map(|c| c.is_whitespace()).unwrap_or(true) {
                i = skip_to_line_end(&bytes, i);
                continue;
            }
        }
        // 跳过块注释
        if bytes[i] == '/' && i + 1 < bytes.len() && bytes[i + 1] == '*' {
            i = skip_block_comment(&bytes, i);
            continue;
        }
        // 跳过左括号（`(SELECT ...) UNION (SELECT ...)` 这种）
        if bytes[i] == '(' {
            i += 1;
            continue;
        }
        break;
    }

    let mut word = String::new();
    while i < bytes.len() && (bytes[i].is_alphanumeric() || bytes[i] == '_') {
        word.extend(bytes[i].to_uppercase());
        i += 1;
    }
    word
}

/// 脚本是不是一条"例程定义"（存储过程 / 触发器 / 函数 / 事件）。
///
/// 这类语句内部有大量真 `;`，没有 `DELIMITER` 就切不对 —— 所以整段当一条，
/// 让服务端自己报错（它的报错比我们切错之后那堆语法错误有意义得多）。
fn starts_a_routine(sql: &str) -> bool {
    // 只看开头几个词，避免把 `SELECT 'CREATE PROCEDURE'` 误判成例程。
    let head: String = sql
        .chars()
        .take(200)
        .collect::<String>()
        .to_uppercase();
    let head = head.trim_start();
    ["CREATE PROCEDURE", "CREATE TRIGGER", "CREATE FUNCTION", "CREATE EVENT"]
        .iter()
        .any(|prefix| head.starts_with(prefix))
}

/// 从 `start`（一个引号字符）跳到配对收尾引号之后。
fn skip_quoted(bytes: &[char], start: usize) -> usize {
    let quote = bytes[start];
    // 反引号用**双写**转义（``` `` ```），其余用反斜杠转义。
    let is_backtick = quote == '`';
    let mut i = start + 1;
    while i < bytes.len() {
        let c = bytes[i];
        if !is_backtick && c == '\\' {
            // `\'` / `\"` / `\\` —— 跳过一个字符
            i += 2;
            continue;
        }
        if c == quote {
            if is_backtick && bytes.get(i + 1) == Some(&'`') {
                i += 2;
                continue;
            }
            return i + 1;
        }
        i += 1;
    }
    // 没闭合（用户还没打完）：吃到末尾，让服务端去报语法错。
    bytes.len()
}

fn skip_to_line_end(bytes: &[char], start: usize) -> usize {
    let mut i = start;
    while i < bytes.len() && bytes[i] != '\n' {
        i += 1;
    }
    i
}

fn skip_block_comment(bytes: &[char], start: usize) -> usize {
    let mut i = start + 2; // 跳过 `/*`
    while i + 1 < bytes.len() {
        if bytes[i] == '*' && bytes[i + 1] == '/' {
            return i + 2;
        }
        i += 1;
    }
    bytes.len()
}

/// 把一段切片 trim 后推进结果（空的不推）。
fn push_trimmed(out: &mut Vec<String>, slice: &[char]) {
    let s: String = slice.iter().collect();
    let t = s.trim();
    // 只剩注释也算空 —— 但别为了判断这个再写一遍状态机，
    // 交给"去掉注释后还有没有内容"这个简单检查。
    if !t.is_empty() && !is_only_comments(t) {
        out.push(t.to_owned());
    }
}

/// 整段是不是只有注释和空白。
fn is_only_comments(text: &str) -> bool {
    let bytes: Vec<char> = text.chars().collect();
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '#' {
            i = skip_to_line_end(&bytes, i);
            continue;
        }
        if c == '-' && i + 1 < bytes.len() && bytes[i + 1] == '-' {
            let after = bytes.get(i + 2).copied();
            if after.map(|ch| ch.is_whitespace()).unwrap_or(true) {
                i = skip_to_line_end(&bytes, i);
                continue;
            }
        }
        if c == '/' && i + 1 < bytes.len() && bytes[i + 1] == '*' {
            i = skip_block_comment(&bytes, i);
            continue;
        }
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(sql: &str) -> Vec<String> {
        split_statements(sql)
    }

    #[test]
    fn plain_statements_are_split_on_semicolons() {
        assert_eq!(split("SELECT 1; SELECT 2"), vec!["SELECT 1", "SELECT 2"]);
        // 尾部的 `;` 不产生空语句
        assert_eq!(split("SELECT 1;"), vec!["SELECT 1"]);
        // 连续分号 / 只有空白，都不产出空语句
        assert_eq!(split("SELECT 1;;;  ; SELECT 2"), vec!["SELECT 1", "SELECT 2"]);
        assert!(split("").is_empty());
        assert!(split("   \n\t ").is_empty());
        assert!(split(";;;").is_empty());
    }

    #[test]
    fn semicolons_inside_quotes_are_not_separators() {
        // 单引号
        assert_eq!(split("SELECT 'a;b'"), vec!["SELECT 'a;b'"]);
        // 双引号
        assert_eq!(split(r#"SELECT "a;b""#), vec![r#"SELECT "a;b""#]);
        // 反引号（标识符）
        assert_eq!(split("SELECT `a;b` FROM t"), vec!["SELECT `a;b` FROM t"]);
        // 引号里的 `;` 之后还有真分隔符
        assert_eq!(
            split("SELECT 'a;b'; SELECT 2"),
            vec!["SELECT 'a;b'", "SELECT 2"]
        );
    }

    #[test]
    fn escaped_quotes_inside_strings_do_not_end_the_string() {
        // `\'` 不结束字符串 —— 否则后面的 `;` 会被误当成分隔符
        assert_eq!(
            split(r"SELECT 'it\'s; fine' FROM t"),
            vec![r"SELECT 'it\'s; fine' FROM t"]
        );
        // `\\` 是转义的反斜杠，不转义后一个引号
        assert_eq!(
            split(r"SELECT 'a\\'; SELECT 2"),
            vec![r"SELECT 'a\\'", "SELECT 2"]
        );
        // 反引号用**双写**转义，不是反斜杠
        assert_eq!(
            split("SELECT `a``b;c` FROM t"),
            vec!["SELECT `a``b;c` FROM t"]
        );
    }

    #[test]
    fn semicolons_inside_comments_are_not_separators() {
        // `--` 行注释（后面必须有空白）
        assert_eq!(split("SELECT 1 -- a;b"), vec!["SELECT 1 -- a;b"]);
        // `#` 行注释
        assert_eq!(split("SELECT 1 # a;b"), vec!["SELECT 1 # a;b"]);
        // 块注释
        assert_eq!(split("SELECT /* a;b */ 1"), vec!["SELECT /* a;b */ 1"]);
        // `/*!40101 ... */` 这种可执行注释，里面的 `;` 同样不切
        assert_eq!(
            split("SELECT /*!40101 SET @x=1; */ 1"),
            vec!["SELECT /*!40101 SET @x=1; */ 1"]
        );
    }

    #[test]
    fn minus_minus_needs_whitespace_to_be_a_comment() {
        // `1--2` 是 `1 - (-2)`，不是注释 —— 所以它的 `;` 仍然是分隔符。
        // （MySQL 要求 `--` 后跟空白或行尾才是注释。）
        assert_eq!(split("SELECT 1--2; SELECT 3"), vec!["SELECT 1--2", "SELECT 3"]);
    }

    #[test]
    fn comment_only_fragments_are_dropped() {
        assert!(split("-- nothing here").is_empty());
        assert!(split("# nothing").is_empty());
        assert!(split("/* nothing */").is_empty());
        // 真语句后面的纯注释尾巴不产生新语句
        assert_eq!(split("SELECT 1; -- bye"), vec!["SELECT 1"]);
    }

    #[test]
    fn unclosed_quote_eats_the_rest_instead_of_panicking() {
        // 用户还没打完就点了执行：不 panic，把剩下的当一条交给服务端报错
        assert_eq!(split("SELECT 'abc"), vec!["SELECT 'abc"]);
        assert_eq!(split("SELECT `abc"), vec!["SELECT `abc"]);
        assert_eq!(split("SELECT /* abc"), vec!["SELECT /* abc"]);
    }

    #[test]
    fn routine_definitions_are_kept_as_one_statement() {
        // 没有 DELIMITER 就切不对，所以整段当一条（见模块文档）
        let sql = "CREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END";
        assert_eq!(split(sql), vec![sql]);
        // 大小写不敏感
        let sql2 = "create trigger t before insert on x for each row begin set @a=1; end";
        assert_eq!(split(sql2), vec![sql2]);
        // 但 `SELECT 'CREATE PROCEDURE'` 不是例程
        assert_eq!(
            split("SELECT 'CREATE PROCEDURE'; SELECT 2"),
            vec!["SELECT 'CREATE PROCEDURE'", "SELECT 2"]
        );
    }

    #[test]
    fn returns_rows_recognises_the_common_ones() {
        assert!(returns_rows("SELECT 1"));
        assert!(returns_rows("  select 1"));
        assert!(returns_rows("SHOW DATABASES"));
        assert!(returns_rows("DESCRIBE t"));
        assert!(returns_rows("EXPLAIN SELECT 1"));
        assert!(returns_rows("WITH x AS (SELECT 1) SELECT * FROM x"));
        // 注释在前也能认出来
        assert!(returns_rows("-- c\nSELECT 1"));
        assert!(returns_rows("/* c */ SELECT 1"));
        // 左括号开头（UNION 的第二个分支）
        assert!(returns_rows("(SELECT 1) UNION (SELECT 2)"));

        assert!(!returns_rows("INSERT INTO t VALUES (1)"));
        assert!(!returns_rows("UPDATE t SET a=1"));
        assert!(!returns_rows("DELETE FROM t"));
        assert!(!returns_rows("CREATE TABLE t (a INT)"));
        assert!(!returns_rows("SET @x=1"));
        assert!(!returns_rows(""));
    }

    #[test]
    fn multibyte_text_is_not_split_mid_character() {
        // 按 char 走而不是按字节，所以中文 / emoji 不会被切成半个
        assert_eq!(
            split("SELECT '中文；测试' ; SELECT '😀'"),
            vec!["SELECT '中文；测试'", "SELECT '😀'"]
        );
        // 全角分号 `；` 不是分隔符
        assert_eq!(split("SELECT 1； SELECT 2"), vec!["SELECT 1； SELECT 2"]);
    }
}
