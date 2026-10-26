//! 日志 / 拉取进度的中间状态。

use super::super::*;

// -- 日志浮层的状态 ----------------------------------------------------
#[test]
fn pull_progress_keeps_only_the_last_lines() {
    let mut p = PullProgress::new("alpine:latest");
    assert_eq!(p.image, "alpine:latest");
    assert!(p.last_line().is_none());

    assert!(p.push_lines((0..10).map(|i| format!("line {i}"))));
    assert_eq!(p.lines.len(), 10);
    assert_eq!(p.last_line(), Some("line 9"));

    // 超过上限后丢掉最老的，保留最新的
    assert!(p.push_lines((0..PullProgress::MAX_LINES + 20).map(|i| format!("extra {i}"))));
    assert_eq!(p.lines.len(), PullProgress::MAX_LINES);
    assert_eq!(p.last_line(), Some("extra 219"));

    // 空输入不算新增（调用方据此跳过重绘）
    assert!(!p.push_lines(Vec::new()));
    assert_eq!(p.lines.len(), PullProgress::MAX_LINES);
}

#[test]
fn logs_state_maps_its_controls_to_log_options() {
    let mut logs = LogsState::new("web");
    // 默认给一个有限的档位 —— "全部"在大容器上可能是几百 MB
    assert_eq!(logs.tail, Some(200));
    assert_eq!(logs.options().tail, Some(200));
    assert!(!logs.options().timestamps);
    assert!(!logs.options().details, "详细默认不开");

    logs.tail = None;
    logs.timestamps = true;
    let opts = logs.options();
    assert_eq!(opts.tail, None, "None 是「全部」");
    assert!(opts.timestamps);
}

#[test]
fn log_tail_labels_cover_every_preset() {
    assert_eq!(LogsState::tail_label(Some(200)), "200 行");
    assert_eq!(LogsState::tail_label(None), "全部");
    // 档位表里那个"全部"必须真的是 None，不然界面上点它没反应
    assert_eq!(LogsState::TAILS[3], None);
}

#[test]
fn a_stale_log_response_is_dropped() {
    // 连点「1000 行」→「全部」会有两个请求在飞，先发的可能后回。
    // 只认最后一次发起的那个 —— 否则界面会显示上一次选择的结果。
    let mut logs = LogsState::new("web");

    let first = logs.begin_load();
    assert!(logs.loading);
    let second = logs.begin_load();
    assert_ne!(first, second, "每轮票据必须不同");

    // 旧的那个后到了：必须被丢掉
    assert!(!logs.finish_load(first, Ok("旧结果".into())));
    assert!(logs.loading, "还在等新的那个，不能因为旧响应到了就收工");
    assert_eq!(logs.text, None, "旧结果不能覆盖");

    // 新的那个到了：收下
    assert!(logs.finish_load(second, Ok("新结果".into())));
    assert!(!logs.loading);
    assert_eq!(logs.text.as_deref(), Some("新结果"));
}

#[test]
fn a_failed_log_load_keeps_the_previous_text() {
    // 拉失败时**不该**把上一次的内容擦掉：用户宁可看着旧日志，
    // 也不想面对一片空白（而且错误信息会单独显示）。
    let mut logs = LogsState::new("web");
    let t = logs.begin_load();
    assert!(logs.finish_load(t, Ok("上一次的内容".into())));

    let t2 = logs.begin_load();
    assert!(logs.finish_load(t2, Err("连不上".into())));
    assert_eq!(logs.text.as_deref(), Some("上一次的内容"));
    assert_eq!(logs.error.as_deref(), Some("连不上"));
    assert!(!logs.loading);
}

#[test]
fn begin_load_clears_the_previous_error() {
    // 重试时上一条错误必须先消失，否则界面会一直挂着它
    let mut logs = LogsState::new("web");
    let t = logs.begin_load();
    logs.finish_load(t, Err("第一次失败".into()));
    assert!(logs.error.is_some());

    logs.begin_load();
    assert_eq!(logs.error, None);
}
