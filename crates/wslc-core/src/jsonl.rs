//! JSON Lines 解析。
//!
//! `wslc` 的 `--format json` 输出的是**每行一个独立 JSON 对象**，
//! 不是 JSON 数组；而 `wslc inspect` 输出的**是**标准 JSON 数组。
//! 本模块把这两种形态统一成 `Vec<T>`。
//!
//! 空结果（0 条记录）时 `wslc` 输出 **0 字节**，而不是 `[]`。

use serde::de::DeserializeOwned;

use crate::error::{Error, Result};

/// 单行解析失败的记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineError {
    /// 行号（从 1 开始）。
    pub line_no: usize,
    /// 原始行内容（已截断，避免日志爆炸）。
    pub raw: String,
    /// 错误信息。
    pub message: String,
}

/// 严格模式：全部分析失败时返回错误，部分失败时丢弃坏行并记日志。
pub fn parse_lines<T: DeserializeOwned>(text: &str) -> Result<Vec<T>> {
    let (items, errors) = parse_lines_lenient::<T>(text);
    for e in &errors {
        tracing::warn!(
            line = e.line_no,
            raw = %e.raw,
            "跳过无法解析的 wslc 输出行：{}",
            e.message
        );
    }
    if items.is_empty() && !errors.is_empty() {
        return Err(Error::Parse(format!(
            "共 {} 行全部解析失败；首个错误（第 {} 行）：{}",
            errors.len(),
            errors[0].line_no,
            errors[0].message
        )));
    }
    Ok(items)
}

/// 宽松模式：返回成功解析的条目与失败明细，由调用方决定如何处理。
pub fn parse_lines_lenient<T: DeserializeOwned>(text: &str) -> (Vec<T>, Vec<LineError>) {
    let mut items = Vec::new();
    let mut errors = Vec::new();

    for (idx, raw_line) in text.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }

        // JSON Lines 的每一行必须是一个**对象**。
        //
        // 这里必须显式拒绝数组：serde 允许"结构体从序列反序列化"，
        // 而本项目的模型字段全是 `#[serde(default)]`，
        // 于是 `[]` 会被悄悄解析成一条"所有字段都是默认值"的垃圾记录，
        // 而不是报错。CI 上就是这么发现 `parse_lines::<Row>("[]")` 返回 Ok 的。
        if line.starts_with('[') {
            errors.push(LineError {
                line_no: idx + 1,
                raw: truncate(line, 200),
                message: "JSON Lines 的每一行必须是对象，不能是数组".to_owned(),
            });
            continue;
        }

        match serde_json::from_str::<T>(line) {
            Ok(v) => items.push(v),
            Err(err) => errors.push(LineError {
                line_no: idx + 1,
                raw: truncate(line, 200),
                message: err.to_string(),
            }),
        }
    }

    (items, errors)
}

/// 兼容模式：先按 JSON 数组解析，失败再按 JSON Lines 解析。
///
/// 用于 `wslc inspect` —— 它输出的是数组，但未来版本可能改，
/// 或者被用户包装脚本改成 JSONL。
pub fn parse_array_or_lines<T: DeserializeOwned>(text: &str) -> Result<Vec<T>> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    if trimmed.starts_with('[') {
        match serde_json::from_str::<Vec<T>>(trimmed) {
            Ok(v) => return Ok(v),
            Err(err) => {
                tracing::debug!("按 JSON 数组解析失败，回退 JSON Lines：{err}");
            }
        }
    }

    parse_lines(trimmed)
}

/// 解析单个 JSON 对象（用于 `wslc info --format json`）。
pub fn parse_object<T: DeserializeOwned>(text: &str) -> Result<T> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(Error::Parse("输出为空，期望一个 JSON 对象".into()));
    }
    serde_json::from_str(trimmed)
        .map_err(|e| Error::Parse(format!("解析 JSON 对象失败：{e}；原始输出前 200 字符：{}", truncate(trimmed, 200))))
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max_chars).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq)]
    struct Row {
        #[serde(rename = "ID", default)]
        id: String,
        #[serde(rename = "Name", default)]
        name: String,
    }

    #[test]
    fn parses_json_lines() {
        let text = "{\"ID\":\"a\",\"Name\":\"one\"}\n{\"ID\":\"b\",\"Name\":\"two\"}\n";
        let rows: Vec<Row> = parse_lines(text).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "a");
        assert_eq!(rows[1].name, "two");
    }

    #[test]
    fn empty_output_yields_empty_vec() {
        // 真实场景：0 个容器时 wslc 输出 0 字节。
        assert!(parse_lines::<Row>("").unwrap().is_empty());
        assert!(parse_lines::<Row>("\n\n  \n").unwrap().is_empty());
    }

    #[test]
    fn a_bare_array_is_not_valid_json_lines() {
        // `[]` 是合法的 JSON，但**不是**合法的 JSON Lines 行。
        //
        // 这条测试是 CI 抓出来的：模型字段全带 `#[serde(default)]`，
        // 而 serde 允许结构体从序列反序列化，所以 `[]` 会被解析成
        // 一条全默认值的垃圾记录而**不报错**。修法是显式拒绝 `[` 开头的行。
        assert!(parse_lines::<Row>("[]").is_err());
        assert!(parse_lines::<Row>("[{\"ID\":\"a\"}]").is_err());
        // 但走数组解析入口时 `[]` 应得到空列表。
        assert!(parse_array_or_lines::<Row>("[]").unwrap().is_empty());
        // 正常的对象行仍然是逐行解析。
        assert_eq!(parse_lines::<Row>("{\"ID\":\"a\"}").unwrap().len(), 1);
    }

    #[test]
    fn skips_blank_lines_and_tolerates_trailing_newline() {
        let text = "\n{\"ID\":\"a\"}\n\n\n{\"ID\":\"b\"}\n\n";
        let rows: Vec<Row> = parse_lines(text).unwrap();
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn missing_fields_use_defaults() {
        let rows: Vec<Row> = parse_lines("{\"ID\":\"a\"}").unwrap();
        assert_eq!(rows[0].name, "");
    }

    #[test]
    fn partial_failure_keeps_good_rows() {
        let text = "{\"ID\":\"a\"}\nnot json at all\n{\"ID\":\"b\"}\n";
        let rows: Vec<Row> = parse_lines(text).unwrap();
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn total_failure_is_an_error() {
        let err = parse_lines::<Row>("garbage\nmore garbage\n").unwrap_err();
        assert!(matches!(err, Error::Parse(_)));
    }

    #[test]
    fn lenient_reports_line_numbers() {
        let text = "{\"ID\":\"a\"}\nbroken\n";
        let (items, errors) = parse_lines_lenient::<Row>(text);
        assert_eq!(items.len(), 1);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].line_no, 2);
        assert_eq!(errors[0].raw, "broken");
    }

    #[test]
    fn parses_json_array_for_inspect() {
        let text = "[{\"ID\":\"a\"},{\"ID\":\"b\"}]";
        let rows: Vec<Row> = parse_array_or_lines(text).unwrap();
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn array_parser_falls_back_to_lines() {
        let text = "{\"ID\":\"a\"}\n{\"ID\":\"b\"}\n";
        let rows: Vec<Row> = parse_array_or_lines(text).unwrap();
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn array_parser_handles_empty() {
        let rows: Vec<Row> = parse_array_or_lines("").unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn parses_single_object() {
        let row: Row = parse_object("{\"ID\":\"a\",\"Name\":\"n\"}").unwrap();
        assert_eq!(row.id, "a");
        assert_eq!(row.name, "n");
    }

    #[test]
    fn object_parser_rejects_empty() {
        assert!(parse_object::<Row>("   ").is_err());
    }

    #[test]
    fn truncate_is_char_safe() {
        let s = "中".repeat(300);
        let t = truncate(&s, 200);
        assert_eq!(t.chars().count(), 201); // 200 + 省略号
    }
}
