//! 容器日志 / exec / 导出 / 单输入框提示 —— 四个容器域浮层。

use super::common::{
    ModalSize, form_field, kv, kv_block, log_text, modal_card, modal_footer, modal_scrim,
    modal_title,
};
use super::prelude::*;

/// 单输入框提示对话框的**内容**（标题 / 底部按钮由官方 `Dialog` 自己给）。
///
/// 弹窗**本身就是确认**：里面带着这个动作的代价说明和**实时**等效命令，
/// 提交后直接执行 —— 所以没有再叠一层二次确认。
/// 连续弹两个窗比一个信息充分的窗更烦人。
///
/// # 为什么不再自己画遮罩和卡片
///
/// 这个浮层是**第一个**迁到官方 `Dialog` 的（窗口级层栈，见
/// `Shell::open_prompt`）：遮罩、Esc、点遮罩关闭、层序都由组件负责。
/// 所以这里只产出"中间那块内容"，外壳在打开处拼。
///
/// 要 `cx` 才能从输入框里读数：底部那行命令是随打字变化的。
pub(crate) fn prompt_dialog_content(
    shell: &Shell,
    entity: &Entity<Shell>,
    cx: &App,
) -> Option<AnyElement> {
    let prompt = shell.prompt.as_ref()?;

    let kind = prompt.kind;
    let typed = prompt.input.read(cx).value().trim().to_owned();
    // 没填时给个占位符，别让预览行变成一条断掉的命令
    let shown = if typed.is_empty() {
        "<待填>".to_owned()
    } else {
        typed
    };
    // 等效命令**逐种拼**，不抽一个"flag"出来：导出的 `--export` 不是
    // `--manage` 的子命令、创建卷走的甚至是另一个可执行文件（`wslc`），
    // 硬塞进同一个模板反而要写一堆特例。
    let preview = match kind {
        PromptKind::MoveDistro => format!("wsl --manage {} --move {shown}", prompt.distro),
        PromptKind::ResizeDistro => format!("wsl --manage {} --resize {shown}", prompt.distro),
        PromptKind::SetDefaultUser => {
            format!("wsl --manage {} --set-default-user {shown}", prompt.distro)
        }
        PromptKind::ExportDistro => format!("wsl --export {} {shown}", prompt.distro),
        PromptKind::CreateVolume => format!("wslc volume create {shown}"),
    };

    // 「创建卷」不属于任何发行版（卷是全局的），所以那一行只在有主体时画。
    // 画一行空的"发行版："比不画更让人困惑。
    let subject: Option<AnyElement> = (!prompt.distro.is_empty()).then(|| {
        div()
            .text_xs()
            .text_color(theme::text_dim())
            .child(format!("发行版：{}", prompt.distro))
            .into_any_element()
    });

    // 只有"要填路径"的那两种才给「浏览…」——
    // 「调整大小」填的是 `50GB`，「设置默认用户」填的是用户名，
    // 给它们这个按钮只会让人以为该去选一个文件。
    let browse = kind.pick_target().map(|_| {
        let entity = entity.clone();
        Button::new("prompt-browse")
            .label("浏览…")
            .small()
            .on_click(move |_, window, cx| {
                entity.update(cx, |shell, cx| shell.browse_prompt_path(window, cx));
            })
            .into_any_element()
    });

    Some(
        v_flex()
            .w_full()
            .gap_3()
            .children(subject)
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_end()
                    // `flex_1 + min_w_0`：让输入框吃掉剩余宽度，
                    // 又在很窄的时候允许它收缩（不写 min_w_0 会顶出去）
                    .child(div().flex_1().min_w_0().child(form_field(
                        "prompt-input",
                        kind.label(),
                        &prompt.input,
                        cx,
                        true,
                    )))
                    .children(browse),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(crate::util::strip_md(kind.note())),
            )
            .child(
                v_flex()
                    .w_full()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::text_dim())
                            .child("等效命令"),
                    )
                    .child(
                        div()
                            .font_family("Consolas")
                            .text_xs()
                            .text_color(theme::text())
                            .child(preview),
                    ),
            )
            .into_any_element(),
    )
}

/// 「容器日志」浮层。
///
/// # 为什么这里没有「跟随」
///
/// `wslc logs --follow` **永不返回**。把它内嵌进来就等于一条永远不结束的
/// 后台任务，还得自己实现"停止跟随"（去杀子进程），而输出照样只能攒在
/// 一个文本块里。开一个真正的终端窗口既简单又更好用 —— 用户可以滚、
/// 可以 Ctrl+C 停。所以这里只做**快照**，跟随交给「在终端里跟随」，
/// 那个按钮用的是同一份行数 / 时间戳选项。
pub fn logs_overlay(state: &AppState, entity: &Entity<Shell>) -> AnyElement {
    let Some(logs) = state.logs.as_ref() else {
        return div().into_any_element();
    };

    // -- 行数档位 --
    //
    // 选中态用 `primary` 配色表示（项目里不用 `.disabled()`）——
    // 和「添加实例」页选来源那一排是同一套写法。
    let tail_buttons: Vec<AnyElement> = LogsState::TAILS
        .iter()
        .enumerate()
        .map(|(ix, tail)| {
            let tail = *tail;
            let entity = entity.clone();
            let mut button = Button::new(SharedString::from(format!("logs-tail-{ix}")))
                .label(LogsState::tail_label(tail))
                .small()
                .on_click(move |_, _, cx| {
                    entity.update(cx, |shell, cx| shell.set_logs_tail(tail, cx));
                });
            if logs.tail == tail {
                button = button.primary();
            }
            button.into_any_element()
        })
        .collect();

    // 「显示时间戳」是**布尔状态**（开着就一直显示），所以用官方 `Switch`。
    //
    // 早先它是一颗按钮、打开时变主色 —— 那是把"状态"画成了"动作"：
    // 用户看到的是一颗可以按的按钮，按下去之后除了颜色变了没有别的反馈。
    let timestamps = {
        let entity = entity.clone();
        Switch::new("logs-timestamps")
            .label("显示时间戳")
            .checked(logs.timestamps)
            .on_change(move |checked: &bool, _, cx| {
                let on = *checked;
                entity.update(cx, |shell, cx| shell.set_logs_timestamps(on, cx));
            })
            .into_any_element()
    };

    // 「刷新」在加载期间改文案而不是置灰：置灰需要一个我们没导入的 trait，
    // 而且"点了没反应"比"正在加载"更让人困惑。
    let refresh = {
        let entity = entity.clone();
        Button::new("logs-refresh")
            .label(if logs.loading {
                "加载中…"
            } else {
                "刷新"
            })
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.reload_logs(cx));
            })
            .into_any_element()
    };

    let follow = {
        let entity = entity.clone();
        let name = logs.container.clone();
        Button::new("logs-follow")
            .label("在终端里跟随")
            .small()
            .on_click(move |_, _, cx| {
                let name = name.clone();
                entity.update(cx, |shell, cx| {
                    shell.run_immediate(ImmediateAction::FollowContainerLogs(name), cx);
                });
            })
            .into_any_element()
    };

    let close = {
        let entity = entity.clone();
        Button::new("logs-close")
            .label("关闭")
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.close_logs(cx));
            })
            .into_any_element()
    };

    // -- 正文 --
    //
    // 失败时**保留**上一次的内容（`log_text` 拿到的还是旧的那份）：
    // 用户宁可看着旧日志，也不想面对一片空白。错误单独占一行。
    let body: AnyElement = match logs.error.as_ref() {
        Some(err) => v_flex()
            .w_full()
            .gap_2()
            .child(
                div()
                    .text_sm()
                    .text_color(theme::danger())
                    .child(format!("读取失败：{err}")),
            )
            .child(log_text(logs.text.as_deref(), "logs-body"))
            .into_any_element(),
        None => log_text(logs.text.as_deref(), "logs-body"),
    };

    // 日志为空时**解释一句** —— 空不等于"坏了"。
    //
    // ⚠️ 这条是 v0.9.8 实机反馈加的（"java 容器日志…看不到"）：那个容器的主进程是
    // 面板为「吊住」它而起的 `tail -f /dev/null`，它本身**不打印任何东西**，
    // 所以这里是空的、而且永远会是空的。而应用自己的输出**不在这里**：
    // 进程形态的部署把它重定向到了目标目录的 `app.log`（「部署管理」里那一行的
    // 「日志」）。不写这两句，用户只会以为日志功能坏了。
    let empty_hint: AnyElement =
        if logs.error.is_none() && logs.text.as_deref().is_none_or(|t| t.trim().is_empty()) {
            div()
                .w_full()
                .text_sm()
                .text_color(theme::text_dim())
                .child(crate::util::strip_md(
                    // 文案里带 `` `tail -f /dev/null` `` —— 不过 `strip_md` 就
                    // 把反引号一起画出来（这条是守卫，不是当下需要）。
                    "容器主进程没有往 stdout / stderr 写东西时就是这个样子 —— 比如面板为了\
                 「吊住」容器而起的 `tail -f /dev/null`，它本身不打印任何东西。\
                 部署的应用日志不在这一份里：那是应用自己的输出，在「部署管理」\
                 那一行的「日志」里看。",
                ))
                .into_any_element()
        } else {
            div().into_any_element()
        };

    modal_scrim(
        "logs",
        modal_card(ModalSize::Wide)
            .id("logs-card")
            .max_h(px(760.))
            .child(
                v_flex()
                    .gap_1()
                    .child(modal_title(format!("日志 —— {}", logs.container)))
                    .child(div().text_xs().text_color(theme::text_dim()).child(
                        "这是一份快照，不会自动更新（点「刷新」重新拉）。\
                                 要实时跟着看就点「在终端里跟随」，它会开一个独立窗口。",
                    )),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .flex_wrap()
                    .items_center()
                    .children(tail_buttons)
                    .child(timestamps)
                    .child(refresh)
                    .child(follow),
            )
            .child(body)
            .child(empty_hint)
            .child(h_flex().w_full().justify_end().child(close)),
    )
    .into_any_element()
}

/// 「容器 exec」浮层：跑一条命令，把输出显示出来。
///
/// # 这不是终端
///
/// `wslc exec` **不带 `-it`**，所以没有 TTY：`vi` / `top` / `less` 这类
/// 需要终端的程序在这里用不了（要么直接报错、要么立刻退出）。
/// 那是「打开终端」那个按钮的事 —— 两边的说明里都写清楚了，
/// 免得用户以为这里能顶替终端。
///
/// 命令走 `/bin/sh -c`，所以管道、重定向、`&&` 都能用（见
/// [`wslc_panel_core::state::exec_argv`] 的说明）。
pub fn exec_overlay(shell: &Shell, entity: &Entity<Shell>, cx: &App) -> AnyElement {
    let Some(exec) = shell.state.exec.as_ref() else {
        return div().into_any_element();
    };
    // 输入框是懒创建的：两个都在才算浮层是"开着的"。
    let Some(input) = shell.exec_input.as_ref() else {
        return div().into_any_element();
    };

    // 执行期间把「运行」**换掉**而不是置灰（项目里不用 `.disabled()`）——
    // 置灰的按钮会让人反复去点，一行"执行中…"说清了正在发生什么。
    let run_area: AnyElement = if exec.running {
        div()
            .text_sm()
            .text_color(theme::text_muted())
            .child("执行中…")
            .into_any_element()
    } else {
        let entity = entity.clone();
        Button::new("exec-run")
            .label("运行")
            .small()
            .primary()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.run_exec(cx));
            })
            .into_any_element()
    };

    let close = {
        let entity = entity.clone();
        Button::new("exec-close")
            .label("关闭")
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.close_exec(cx));
            })
            .into_any_element()
    };

    // -- 输出 --
    let output: AnyElement = match exec.error.as_ref() {
        Some(err) => v_flex()
            .w_full()
            .gap_2()
            .child(
                div()
                    .text_sm()
                    .text_color(theme::danger())
                    .child(format!("执行失败：{err}")),
            )
            .child(log_text(exec.output.as_deref(), "exec-output"))
            .into_any_element(),
        None => log_text(exec.output.as_deref(), "exec-output"),
    };

    // 回显这次输出是哪条命令的 —— 连着跑两条之后，没有这一行就分不清
    // 下面那块输出属于哪一次。
    let echo: Option<AnyElement> = (!exec.last_command.is_empty()).then(|| {
        h_flex()
            .w_full()
            .gap_2()
            .items_center()
            .child(
                div()
                    .text_xs()
                    .text_color(theme::text_dim())
                    .child("上一次执行"),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .font_family("Consolas")
                    .text_xs()
                    .text_color(theme::text_muted())
                    .overflow_hidden()
                    .truncate()
                    .child(exec.last_command.clone()),
            )
            .into_any_element()
    });

    modal_scrim(
        "exec",
        modal_card(ModalSize::Wide)
            .id("exec-card")
            .max_h(px(720.))
            .child(
                v_flex()
                    .gap_1()
                    .child(modal_title(format!("执行命令 —— {}", exec.container)))
                    .child(div().text_xs().text_color(theme::text_dim()).child(
                        "使用 /bin/sh 执行，支持管道和重定向，不提供 TTY；交互程序请用容器列表「更多 → 终端」。",
                    )),
            )
            .child(form_field("exec-command", "命令", input, cx, true))
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .flex_wrap()
                    .items_center()
                    .child(run_area),
            )
            .children(echo)
            .child(output)
            .child(h_flex().w_full().justify_end().child(close)),
    )
    .into_any_element()
}

/// 导出进度浮层。
///
/// # 为什么它和拉取镜像的浮层长得不一样
///
/// 拉取那边显示的是 **docker 的输出行**（每层一行，信息量在行里）。
/// 导出这边 `wsl --export` 几乎不打进度，但产物是个文件 ——
/// **文件大小是真实且连续增长的**，直接量它比解析输出有用得多，
/// 用户看到的也是"还要写多少"。
pub fn export_overlay(state: &AppState, entity: &Entity<Shell>) -> AnyElement {
    let Some(progress) = state.exporting.as_ref() else {
        return div().into_any_element();
    };

    let written = progress
        .written
        .map(format_bytes)
        .unwrap_or_else(|| "还没开始写入".to_owned());

    let last = if progress.last_line.trim().is_empty() {
        "（暂无输出）"
    } else {
        progress.last_line.trim()
    };

    let cancel = {
        let entity = entity.clone();
        Button::new("export-cancel")
            .label("取消导出")
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.cancel_export(cx));
            })
    };

    modal_scrim(
        "export",
        modal_card(ModalSize::Medium)
            .child(modal_title(format!("正在导出 {}", progress.name)))
            .child(
                v_flex()
                    .w_full()
                    .gap_2()
                    .child(kv_block("目标文件", progress.path.clone()).into_any_element())
                    .child(kv("已写入", written).into_any_element())
                    .child(
                        kv("已用时", format!("{} 秒", progress.elapsed_secs)).into_any_element(),
                    ),
            )
            .child(div().text_xs().text_color(theme::text_dim()).child(
                "wsl 不报告导出百分比，所以「已写入」是目标文件当前的大小 —— \
                     它是连续增长的，可以据此估还要多久。",
            ))
            .child(
                div()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(format!(
                        "导出期间请勿关机。取消会留下一个不完整的 tar，需要你自己删。\
                             最后一行输出：{last}"
                    )),
            )
            .child(h_flex().w_full().justify_end().child(cancel)),
    )
    .into_any_element()
}

// ---------------------------------------------------------------------------
// ⑧ 发行版配置（/etc/wsl.conf）
// ---------------------------------------------------------------------------
