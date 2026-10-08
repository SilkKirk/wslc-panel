//! 镜像命令。

use crate::cli::Wslc;
use crate::error::{Error, Result};
use crate::jsonl;
use crate::model::ImageListItem;

/// `wslc images --format json`
pub fn list(wslc: &Wslc) -> Result<Vec<ImageListItem>> {
    let out = wslc.run_checked(&["images", "--format", "json"])?;
    jsonl::parse_lines(&out.stdout)
}

/// 本地已有的镜像引用（`Repository:Tag`），供创建容器表单做下拉候选。
///
/// 已去重并排序，悬空镜像（`<none>:<none>`）会被排除。
pub fn local_references(wslc: &Wslc) -> Result<Vec<String>> {
    let mut refs: Vec<String> = list(wslc)?
        .into_iter()
        .filter(|i| !i.is_dangling())
        .map(|i| i.reference())
        .collect();
    refs.sort();
    refs.dedup();
    Ok(refs)
}

/// 拉取镜像。
///
/// ⚠️ 实测本机**直连 Docker Hub 会超时**（`registry-1.docker.io` 不可达），
/// 因此调用方应允许用户配置镜像加速地址，并把超时设长。
pub fn pull(wslc: &Wslc, reference: &str) -> Result<String> {
    if reference.trim().is_empty() {
        return Err(Error::InvalidArgument("镜像引用不能为空".into()));
    }
    let out = wslc
        .run_with_timeout(&["pull", reference], std::time::Duration::from_secs(600))?
        .into_result()?;
    Ok(out.stdout_trimmed().to_owned())
}

/// 删除镜像。
pub fn remove(wslc: &Wslc, references: &[String], force: bool) -> Result<Vec<String>> {
    if references.is_empty() {
        return Err(Error::InvalidArgument("rmi 至少需要一个镜像".into()));
    }
    let mut args: Vec<String> = vec!["rmi".into()];
    if force {
        args.push("-f".into());
    }
    args.extend(references.iter().cloned());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = wslc.run_checked(&refs)?;
    Ok(out
        .stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect())
}

/// 给镜像打标签。
pub fn tag(wslc: &Wslc, source: &str, target: &str) -> Result<()> {
    wslc.run_checked(&["tag", source, target])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fixture_through_the_same_path() {
        let items: Vec<ImageListItem> =
            jsonl::parse_lines(include_str!("../../tests/fixtures/images.jsonl")).unwrap();
        assert_eq!(items.len(), 3);
    }

    #[test]
    fn local_references_filters_dangling_and_dedups() {
        // 用 fixture 直接跑筛选逻辑（与 local_references 内部一致）。
        let items: Vec<ImageListItem> =
            jsonl::parse_lines(include_str!("../../tests/fixtures/images.jsonl")).unwrap();
        let mut refs: Vec<String> = items
            .into_iter()
            .filter(|i| !i.is_dangling())
            .map(|i| i.reference())
            .collect();
        refs.sort();
        refs.dedup();

        assert_eq!(
            refs,
            vec![
                "docker.1ms.run/library/alpine:latest".to_owned(),
                "docker.1panel.live/library/hello-world:latest".to_owned(),
                "hello-world:latest".to_owned(),
            ]
        );
    }

    #[test]
    fn pull_rejects_empty_reference_before_spawning() {
        let wslc = Wslc::with_program("definitely-not-a-real-binary");
        assert!(matches!(pull(&wslc, "  "), Err(Error::InvalidArgument(_))));
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
