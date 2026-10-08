//! 会话模型。
//!
//! ⚠️ `wslc system session list` **不支持 `--format json`**，只输出表格，
//! 且表头是**中文**（实测）：
//!
//! ```text
//! ID   创建者 PID   显示名称
//! 1    21684     wslc-cli-76434
//! ```
//!
//! 因此这里是**表格解析**：先按表头关键字定位列，再按"2 个以上连续空格"
//! 切分数据行。定位失败时回退到位置顺序（0/1/2），以兼容英文表头或列序变化。

use serde::{Deserialize, Serialize};

/// 一个活动的 wslc 会话。
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Session {
    /// 会话 ID（对应全局选项 `--session <id>`）。
    pub id: u32,
    /// 创建者进程 PID。
    pub creator_pid: u32,
    /// 显示名，如 `wslc-cli-76434`。
    pub display_name: String,
}

impl Session {
    /// 展示名，空时回退到 `会话 #<id>`。
    pub fn display(&self) -> String {
        if self.display_name.is_empty() {
            format!("会话 #{}", self.id)
        } else {
            self.display_name.clone()
        }
    }
}

/// 解析 `wslc system session list` 的表格输出。
///
/// 无法识别的行会被跳过（不报错），因此 `--verbose` 之类的额外列不会导致失败。
pub fn parse_session_table(text: &str) -> Vec<Session> {
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());

    let Some(header_line) = lines.next() else {
        return Vec::new();
    };
    // 某些版本可能加 BOM 或前导空格。
    let header_line = header_line.trim_start_matches('\u{feff}');
    let headers = split_columns(header_line);

    let id_col = find_column(&headers, &["id", "会话"]).unwrap_or(0);
    let pid_col = find_column(&headers, &["pid", "创建者"]).unwrap_or(1);
    let name_col = find_column(&headers, &["显示名", "名称", "name"]).unwrap_or(2);

    let mut out = Vec::new();
    for line in lines {
        let cols = split_columns(line);
        if cols.is_empty() {
            continue;
        }
        let get = |idx: usize| cols.get(idx).map(String::as_str).unwrap_or("");
        // ID 解析不出来就认为这行不是数据行（例如分隔线或提示文本）。
        let Ok(id) = get(id_col).trim().parse::<u32>() else {
            continue;
        };
        out.push(Session {
            id,
            creator_pid: get(pid_col).trim().parse::<u32>().unwrap_or(0),
            display_name: get(name_col).trim().to_owned(),
        });
    }
    out
}

/// 按"2 个及以上连续空格"切分列；单个空格视为列内空格。
fn split_columns(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut pending_ws = 0usize;

    for ch in line.chars() {
        if ch == ' ' || ch == '\t' {
            pending_ws += 1;
            continue;
        }
        if pending_ws >= 2 && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        } else if pending_ws == 1 && !cur.is_empty() {
            cur.push(' ');
        }
        pending_ws = 0;
        cur.push(ch);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// 在表头里找到包含任一候选关键字的列下标（大小写不敏感）。
fn find_column(headers: &[String], candidates: &[&str]) -> Option<usize> {
    headers.iter().position(|h| {
        let lower = h.to_ascii_lowercase();
        candidates.iter().any(|c| {
            let c = c.to_ascii_lowercase();
            h.contains(c.as_str()) || lower.contains(&c)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../tests/fixtures/session_list.txt");

    #[test]
    fn parses_real_session_table() {
        let sessions = parse_session_table(FIXTURE);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, 1);
        assert_eq!(sessions[0].creator_pid, 21684);
        assert_eq!(sessions[0].display_name, "wslc-cli-76434");
        assert_eq!(sessions[0].display(), "wslc-cli-76434");
    }

    #[test]
    fn splits_chinese_header_correctly() {
        let cols = split_columns("ID   创建者 PID   显示名称");
        assert_eq!(cols, vec!["ID", "创建者 PID", "显示名称"]);
    }

    #[test]
    fn splits_data_row_correctly() {
        let cols = split_columns("1    21684     wslc-cli-76434");
        assert_eq!(cols, vec!["1", "21684", "wslc-cli-76434"]);
    }

    #[test]
    fn single_space_stays_inside_a_column() {
        let cols = split_columns("a b    c");
        assert_eq!(cols, vec!["a b", "c"]);
    }

    #[test]
    fn handles_multiple_sessions() {
        let text = "ID   创建者 PID   显示名称\n1    100    first\n2    200    second session\n";
        let sessions = parse_session_table(text);
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[1].display_name, "second session");
    }

    #[test]
    fn handles_english_header_variant() {
        let text = "ID   CREATOR PID   NAME\n7    42    my-session\n";
        let sessions = parse_session_table(text);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, 7);
        assert_eq!(sessions[0].creator_pid, 42);
        assert_eq!(sessions[0].display_name, "my-session");
    }

    #[test]
    fn empty_output_yields_no_sessions() {
        assert!(parse_session_table("").is_empty());
        assert!(parse_session_table("ID   创建者 PID   显示名称\n").is_empty());
    }

    #[test]
    fn non_numeric_rows_are_skipped() {
        let text = "ID   创建者 PID   显示名称\n（无活动会话）\n1  2  ok\n";
        let sessions = parse_session_table(text);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].display_name, "ok");
    }

    #[test]
    fn display_falls_back_to_id() {
        let s = Session {
            id: 3,
            ..Default::default()
        };
        assert_eq!(s.display(), "会话 #3");
    }
}
