//! 卷命令。

use crate::cli::Wslc;
use crate::error::{Error, Result};
use crate::jsonl;
use crate::model::VolumeListItem;

/// `wslc volume list --format json`
///
/// ⚠️ 无卷时 `wslc` 输出 **0 字节**，这里返回空 `Vec`。
/// 采集环境里一个卷都没有，因此字段名尚未实机校准
/// （见 `model/volume.rs` 顶部说明）。
pub fn list(wslc: &Wslc) -> Result<Vec<VolumeListItem>> {
    let out = wslc.run_checked(&["volume", "list", "--format", "json"])?;
    jsonl::parse_lines(&out.stdout)
}

/// 创建卷。
pub fn create(wslc: &Wslc, name: &str) -> Result<String> {
    if name.trim().is_empty() {
        return Err(Error::InvalidArgument("卷名不能为空".into()));
    }
    let out = wslc.run_checked(&["volume", "create", name])?;
    Ok(out.stdout_trimmed().to_owned())
}

/// 删除卷。
pub fn remove(wslc: &Wslc, names: &[String], force: bool) -> Result<()> {
    if names.is_empty() {
        return Err(Error::InvalidArgument(
            "volume remove 至少需要一个卷".into(),
        ));
    }
    let mut args: Vec<String> = vec!["volume".into(), "remove".into()];
    if force {
        args.push("-f".into());
    }
    args.extend(names.iter().cloned());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    wslc.run_checked(&refs)?;
    Ok(())
}

/// 清理未使用的卷（`-f` 跳过确认，因为 stdin 是 null）。
pub fn prune(wslc: &Wslc) -> Result<String> {
    let out = wslc.run_checked(&["volume", "prune", "-f"])?;
    Ok(out.stdout_trimmed().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_output_yields_empty_list() {
        // 这正是本机实测的情况：0 个卷 → 0 字节输出。
        let items: Vec<VolumeListItem> = jsonl::parse_lines("").unwrap();
        assert!(items.is_empty());
    }

    #[test]
    fn create_rejects_blank_name_before_spawning() {
        let wslc = Wslc::with_program("definitely-not-a-real-binary");
        assert!(matches!(create(&wslc, " "), Err(Error::InvalidArgument(_))));
    }

    #[test]
    fn remove_rejects_empty_list_before_spawning() {
        let wslc = Wslc::with_program("definitely-not-a-real-binary");
        assert!(matches!(
            remove(&wslc, &[], false),
            Err(Error::InvalidArgument(_))
        ));
    }
}
