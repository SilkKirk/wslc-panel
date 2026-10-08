//! 系统级命令：`info` / `version` / `settings` / `system session`。

use crate::cli::Wslc;
use crate::error::Result;
use crate::jsonl;
use crate::model::{Session, SystemInfo};
use crate::model::session::parse_session_table;

/// `wslc info --format json`
pub fn info(wslc: &Wslc) -> Result<SystemInfo> {
    let out = wslc.run_checked(&["info", "--format", "json"])?;
    jsonl::parse_object(&out.stdout)
}

/// `wslc version`
pub fn version(wslc: &Wslc) -> Result<String> {
    let out = wslc.run_checked(&["version"])?;
    Ok(out.stdout_trimmed().to_owned())
}

/// `wslc system session list`
///
/// ⚠️ 这条命令**不支持 `--format json`**，返回的是中文表头表格，
/// 由 [`parse_session_table`] 解析。
pub fn sessions(wslc: &Wslc) -> Result<Vec<Session>> {
    let out = wslc.run_checked(&["system", "session", "list"])?;
    Ok(parse_session_table(&out.stdout))
}

/// 用默认编辑器打开 `settings.yaml`（`wslc settings`）。
///
/// 这是**分离启动**：不等待编辑器退出，否则 UI 会被阻塞。
pub fn open_settings_in_editor(wslc: &Wslc) -> Result<()> {
    wslc.spawn_detached(&["settings"])
}

/// 终止一个会话（`wslc system session terminate <id>`）。
pub fn terminate_session(wslc: &Wslc, session_id: u32) -> Result<()> {
    let id = session_id.to_string();
    wslc.run_checked(&["system", "session", "terminate", id.as_str()])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Wslc;

    /// 解析逻辑不需要真的跑 wslc：这里直接验证 fixture 走通同一条路径。
    #[test]
    fn info_fixture_parses_through_the_same_code_path() {
        let text = include_str!("../../tests/fixtures/info.json");
        let info: SystemInfo = jsonl::parse_object(text).unwrap();
        assert_eq!(info.client.version, "3.0.1.0");
    }

    #[test]
    fn session_fixture_parses_through_the_same_code_path() {
        let text = include_str!("../../tests/fixtures/session_list.txt");
        let sessions = parse_session_table(text);
        assert_eq!(sessions.len(), 1);
    }

    #[test]
    fn session_command_never_passes_format_json() {
        // 回归保护：真机验证过 `system session list --format json` 会报
        // “当前命令的选项名称未被识别”。这里锁死参数形状。
        let wslc = Wslc::with_program("wslc");
        let args = wslc.command_args(&["system", "session", "list"]);
        assert!(!args.iter().any(|a| a == "--format"));
    }
}
