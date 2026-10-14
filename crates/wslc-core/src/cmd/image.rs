//! 镜像命令。

use crate::cli::{StreamHandle, Wslc};
use crate::error::{Error, Result};
use crate::jsonl;
use crate::model::{ImageFacts, ImageListItem};

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

/// **流式**拉取：每读到一行输出就回调一次。
///
/// 与 [`pull`] 的区别：`pull` 要等进程结束才返回，界面只能显示"进行中"；
/// 这个版本能实时显示 `Downloading` / `Pull complete` 之类的进度，
/// 而且返回的句柄可以**取消**（界面上那个"取消拉取"按钮）。
///
/// 流式模式**没有内建超时** —— 免得把还在正常下载的任务误杀。
/// 想要上限就自己计时后调 [`crate::cli::CancelToken::cancel`]。
pub fn pull_streaming(
    wslc: &Wslc,
    reference: &str,
    on_line: impl Fn(&str) + Send + Sync + 'static,
) -> Result<StreamHandle> {
    if reference.trim().is_empty() {
        return Err(Error::InvalidArgument("镜像引用不能为空".into()));
    }
    wslc.spawn_streaming(&["pull", reference], on_line)
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

/// 读一个**本地**镜像声明的端口与启动命令（`wslc image inspect --format json`）。
///
/// 用途：创建容器时把端口预填好、并写一句"这个镜像会跑什么"
/// （见 [`crate::model::suggest_port_mappings`]）。
///
/// 实测（WSL 3.0.3.0，两个真机 fixture 在 `tests/fixtures/image_inspect_*.json`）：
/// 本机镜像几十毫秒就回来；**镜像不在本地时** `wslc` 打「未找到映像 'x'。」
/// 并以退出码 1 结束 —— 所以调用方拿到的是一句能看的原因，不是空数据
/// （`[]` 那条路在 [`crate::model::parse_image_inspect`] 里也单独报错）。
///
/// ⚠️ **阻塞**：只能在后台执行器上调（`AGENTS.md` §7.1）。
pub fn inspect_facts(wslc: &Wslc, reference: &str) -> Result<ImageFacts> {
    if reference.trim().is_empty() {
        return Err(Error::InvalidArgument("镜像引用不能为空".into()));
    }
    let out = wslc.run_checked(&["image", "inspect", reference, "--format", "json"])?;
    crate::model::parse_image_inspect(&out.stdout).map_err(Error::InvalidArgument)
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
    fn inspect_rejects_empty_reference_before_spawning() {
        let wslc = Wslc::with_program("definitely-not-a-real-binary");
        assert!(matches!(
            inspect_facts(&wslc, "  "),
            Err(Error::InvalidArgument(_))
        ));
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
