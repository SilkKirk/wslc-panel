//! 部署域：运行时识别、阶段文案、失败建议。

use super::super::prelude::*;
use super::super::*;

#[test]
fn deploy_page_is_not_in_the_sidebar_and_does_not_need_a_window() {
    // 「部署管理」和「数据库管理」都服务**某一个**容器，只能从容器列表行上的
    // 按钮进，所以**不在** `Page::ALL` 里。
    assert!(!Page::ALL.contains(&Page::Deploy));
    // 和 `Containers` 同组，侧边栏里不会多出一个组标题
    assert_eq!(Page::Deploy.group(), "容器");
    assert_eq!(Page::Deploy.label(), "部署管理");
    // ⚠️ **不需要 window** —— 这一页一个输入框都没有。
    // 「添加部署」那五个框是点按钮那一刻才建的（`open_deploy_dialog` 有
    // window），页面本身进来就能画。这和 v0.6.3 的「拉取镜像」是同一个形状。
    //
    // 写成 `true` 不是"更保险"：`set_page` 里会给它一个**永远走不到**的分支。
    assert!(
        !Page::Deploy.needs_window_to_enter(),
        "部署页没有常驻输入框 —— 表单在弹窗里，是点的时候建的"
    );
}

#[test]
fn a_freshly_opened_deploy_page_can_be_loaded() {
    // ⚠️ 和 `a_freshly_opened_db_page_can_be_loaded` 是同一条教训：
    // `provisional` 里把 `busy` 置 true 会让 `load_deploy` 的第一句守卫
    // 把**第一次**加载挡掉，页面永远停在「正在读取…」。
    // v0.8 在数据库页上真的踩过一次，这里把它钉死。
    let state = DeployState::provisional(
        "nginx",
        "docker.1panel.live/library/nginx:latest",
        AppRuntime::Nginx,
        DeploySignal::Image,
    );
    assert!(
        !state.busy,
        "新建的状态不能是 busy：`load_deploy` 会因为这一条直接返回"
    );
    assert!(!state.loaded);
    // 列表此时是空的（存档还没读进来），但页面已经能画了
    assert!(state.entries.is_empty());
    assert!(state.progress.is_none());
}

/// 造一行"已部署列表"（测试用）。
fn deploy_entry(app: &str, runtime_key: &str) -> DeployEntry {
    DeployEntry {
        record: crate::deploys::DeployRecord {
            app: app.to_owned(),
            runtime: runtime_key.to_owned(),
            source: format!(r"E:\build\{app}"),
            target: format!("/opt/{app}"),
            command: format!("./{app}"),
            artifact: app.to_owned(),
            port: 8080,
            members: vec![app.to_owned()],
            site: None,
        },
        runtime: AppRuntime::from_key(runtime_key),
        process: None,
        files: None,
        probed: false,
        probe_error: None,
    }
}

#[test]
fn deploy_entries_resolve_the_runtime_key_from_the_archive() {
    let mut state = DeployState::provisional(
        "nginx",
        "nginx:latest",
        AppRuntime::Nginx,
        DeploySignal::Image,
    );
    let records = vec![
        deploy_entry("site", "nginx").record,
        deploy_entry("api", "go").record,
    ];
    state.set_records(&records);

    assert_eq!(state.entries.len(), 2);
    // 运行时从存档里的**稳定键**还原（不是从显示名，也不是靠猜）
    assert_eq!(state.entries[0].runtime, Some(AppRuntime::Nginx));
    assert_eq!(state.entries[1].runtime, Some(AppRuntime::Go));
    // `set_records` 只填记录 —— 现状要问容器，所以这时**还没问过**
    assert!(state.entries.iter().all(|e| !e.probed));
    assert!(state.entries.iter().all(|e| e.files.is_none()));

    assert_eq!(state.index_of("api"), Some(1));
    assert_eq!(state.index_of("nope"), None);
}

#[test]
fn an_unreadable_runtime_key_is_reported_not_guessed() {
    // 存档被手改坏（或者更新过的版本写了别的键）时，**不猜默认值**
    //（猜错会把文件铺到别的地方），而是如实说"认不出来"。
    let mut state = DeployState::provisional(
        "nginx",
        "nginx:latest",
        AppRuntime::Nginx,
        DeploySignal::Image,
    );
    state.set_records(&[deploy_entry("weird", "rust").record]);

    assert_eq!(
        state.entries.len(),
        1,
        "认不出来也要留在列表里，不能悄悄藏起来"
    );
    assert_eq!(state.entries[0].runtime, None);
    assert_eq!(state.entries[0].status_label(), "类型认不出来");
    assert!(!state.entries[0].is_process());
}

#[test]
fn deploy_status_labels_tell_the_three_shapes_apart() {
    // 还没问过容器
    let mut entry = deploy_entry("api", "go");
    assert_eq!(entry.status_label(), "读取中…");

    // 问过了：进程跑着
    entry.probed = true;
    entry.process = Some(ProcessStatus::Running);
    entry.files = Some(4);
    assert_eq!(entry.status_label(), "运行中");
    assert!(entry.is_process());

    // 进程停了，但目标目录里还有东西 → 「已停止」
    entry.process = Some(ProcessStatus::Stopped);
    assert_eq!(entry.status_label(), "已停止");

    // 进程停了、目标目录也没了 → 「目标不存在」
    // （容器被删掉重建之后就会是这一种，见 `crate::deploys` 的说明）
    entry.files = None;
    assert_eq!(entry.status_label(), "目标不存在");

    // 内容形态（nginx）没有属于我们的进程，看文件在不在
    let mut content = deploy_entry("site", "nginx");
    content.probed = true;
    content.files = Some(12);
    assert_eq!(content.status_label(), "已部署");
    assert!(!content.is_process(), "nginx 是内容形态，没有启动/停止");

    content.files = Some(0);
    assert_eq!(content.status_label(), "目标目录是空的");

    content.files = None;
    assert_eq!(content.status_label(), "目标不存在");
}

#[test]
fn deploy_state_takes_the_authoritative_runtime_from_the_probe() {
    // 列表那条启发式先给一个（可能是按命令猜的），inspect 回来后覆盖
    let mut state = DeployState::provisional(
        "web",
        "my-company/web:1",
        AppRuntime::Node,
        DeploySignal::Command,
    );
    let probe = wslc_core::cmd::deploy::DeployProbe {
        container: "web".to_owned(),
        image: "docker.1panel.live/library/nginx:latest".to_owned(),
        runtime: AppRuntime::Nginx,
        web_root: Some("/usr/share/nginx/html".to_owned()),
        ports: vec![(
            "80/tcp".to_owned(),
            vec![("127.0.0.1".to_owned(), "80".to_owned())],
        )],
        networks: vec!["bridge".to_owned()],
    };
    state.apply_probe(&probe);

    assert_eq!(state.runtime, AppRuntime::Nginx);
    assert_eq!(state.image, "docker.1panel.live/library/nginx:latest");
    // 权威判定来自 inspect（字段完整、没被截断）→ 依据记成镜像名那一档
    assert_eq!(state.signal, DeploySignal::Image);
    // Web 根是从挂载点推出来的（内容形态的默认目标目录用它）
    assert_eq!(state.web_root.as_deref(), Some("/usr/share/nginx/html"));
    // 判端口发布用的是**唯一那份**纯函数（界面和这里调的是同一个）
    assert!(wslc_core::model::deploy::port_is_published(
        &probe.ports,
        80
    ));
    assert!(!wslc_core::model::deploy::port_is_published(
        &probe.ports,
        8080
    ));
    assert!(
        !state.busy,
        "探测回来必须把忙清掉，否则按钮永远是「读取中…」"
    );
}

#[test]
fn every_deploy_phase_says_what_it_is_doing() {
    // ⚠️ 「停止」曾经复用 `Starting`，于是那一行写着"正在启动…" ——
    // 按钮和文案对不上，是实机一眼能看出来、CI 看不出来的那类问题。
    assert_eq!(DeployPhase::Starting.label(), "正在启动…");
    assert_eq!(DeployPhase::Stopping.label(), "正在停止…");
    assert_ne!(DeployPhase::Stopping.label(), DeployPhase::Starting.label());
    assert_eq!(DeployPhase::Uploading.label(), "正在上传…");
    assert_eq!(DeployPhase::Configuring.label(), "正在写入站点配置…");
    assert_eq!(DeployPhase::Removing.label(), "正在删除…");
}

#[test]
fn deploy_failure_always_gives_a_next_step() {
    // 实测抓下来的原文（容器没运行）
    let failure = DeployFailure::from_text(
        "容器\"x\"未运行。\n错误代码： WSLC_E_CONTAINER_NOT_RUNNING".to_owned(),
    );
    assert_eq!(failure.kind, DeployErrorKind::ContainerNotRunning);
    assert!(
        failure.summary().contains("启动"),
        "提示条要说清下一步：{}",
        failure.summary()
    );
    // 分类认不出来时不能空着 —— 给原文第一行
    let other = DeployFailure::from_text("没法归类的一句话\n第二行".to_owned());
    assert_eq!(other.summary(), "没法归类的一句话");
}
