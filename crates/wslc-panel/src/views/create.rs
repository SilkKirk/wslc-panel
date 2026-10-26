//! 「创建容器」弹窗，以及它依赖的那几个从镜像读来的提示块。

use super::common::{
    ModalSize, disclosure, form_field, modal_card, modal_scrim, modal_title, radio_row,
};
use super::prelude::*;

/// 「创建容器」弹窗里那条**从镜像读来的**信息（声明端口 / 启动命令）。
///
/// 它是**锦上添花**：读不到照样能建容器（端口自己填），所以失败时只给一句
/// 说明，不用红字 —— 红字是留给"要出事了"的（`AGENTS.md` 的界面约定）。
///
/// 数据来自 `wslc image inspect --format json`（`Shell::probe_image_facts`
/// 在后台读、读到了顺手把端口预填），解析在 `wslc-core`
/// （`model::parse_image_inspect`，配两个真机 fixture 单测）。
pub(crate) fn image_facts_hint(dialog: &CreateDialog) -> AnyElement {
    match &dialog.facts {
        // 用户自己敲的镜像引用：我们没去读，就什么都不说。
        FactsState::Idle => div().into_any_element(),
        FactsState::Loading => div()
            .text_xs()
            .text_color(theme::text_dim())
            .child("正在读镜像信息…")
            .into_any_element(),
        FactsState::Failed(error) => div()
            .text_xs()
            .text_color(theme::text_dim())
            .child(format!(
                "读不到镜像信息：{error}（镜像还不在本地时就是这样 —— 端口请自己填）"
            ))
            .into_any_element(),
        FactsState::Ready(facts) => {
            let ports = if facts.exposed_ports.is_empty() {
                "这个镜像没有声明端口；要访问它得自己写端口映射（例如 8080:80）。".to_owned()
            } else {
                let list = facts
                    .exposed_ports
                    .iter()
                    .map(|port| format!("{port}/tcp"))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "镜像声明监听 {list} —— 建议 {}（上面端口那格为空时已替你填好，可改）。",
                    wslc_core::model::suggest_port_mappings(&facts.exposed_ports)
                )
            };

            let mut body = v_flex()
                .w_full()
                .gap_1()
                .child(div().text_xs().text_color(theme::text_dim()).child(ports));
            if !facts.command.trim().is_empty() {
                // 说清"这是**镜像自带**的"：下面有一格同名的输入框
                // （`create-command`），两处都写"启动命令"用户会分不清哪个生效。
                //
                // ⚠️ 这一行**不能**读成"这个镜像跑得起来" —— 它拼的是
                // `Entrypoint + Cmd`。有 CMD 的（nginx / mysql）留空就是用它；
                // **只有 ENTRYPOINT 的**（`eclipse-temurin:*-jre-alpine` 就是，
                // 这里显示的是 `/__cacert_entrypoint.sh`）留空**起来就退**。
                // 所以两种情况给两句不同的话（v0.9.10 起 `ImageFacts::has_cmd`
                // 能把它们分开；`confirm_create` 也会拦住后者留空提交）。
                let text = if facts.has_cmd {
                    format!(
                        "镜像自带的启动命令：{}（「启动命令」那格留空时用它）",
                        facts.command
                    )
                } else {
                    format!(
                        "⚠️ 这个镜像**没有默认命令**（只有 ENTRYPOINT：{}）——\
                         「启动命令」那格**必须自己填**，留空的话容器起来就会立刻退出。\
                         想让它先待着等部署就填 `tail -f /dev/null`，\
                         想直接跑应用就填你的启动命令。",
                        facts.command
                    )
                };
                body = body.child(
                    div()
                        .text_xs()
                        .text_color(theme::text_dim())
                        // 这两句里都有 Markdown（`**没有默认命令**` /
                        // `**必须自己填**` / 反引号）→ 渲染处统一清掉
                        .child(crate::util::strip_md(&text)),
                );
            }
            body.into_any_element()
        }
    }
}

/// 「容器名」那格下面的提示：**留空时它会叫什么**。
///
/// 名字是按镜像推出来的（[`wslc_core::model::suggest_container_name`]）——
/// 不显示出来的话，用户得等建完才在列表里看到名字，而那时已经晚了
/// （实机反馈：随机名看不出是哪个镜像起的）。
///
/// 用的是和 `to_spec` 完全一样的输入，所以提示里的名字、下面等效命令里的
/// `--name`、以及真正执行的名字三者不会不一致。
/// 用户自己填了名字就不显示（那时候这行只会是噪音）。
pub(crate) fn auto_name_hint(dialog: &CreateDialog, state: &AppState, cx: &App) -> AnyElement {
    if !dialog.name.read(cx).value().trim().is_empty() {
        return div().into_any_element();
    }
    let image = dialog.image.read(cx).value().trim().to_owned();
    if image.is_empty() {
        return div().into_any_element();
    }
    let name = wslc_core::model::suggest_container_name(&image, &state.container_names());
    if name.is_empty() {
        return div().into_any_element();
    }

    div()
        .text_xs()
        .text_color(theme::text_dim())
        .child(format!("留空就按镜像命名：{name}"))
        .into_any_element()
}

/// 「环境变量」那行下面的提示：**这个镜像第一次启动必须有**哪个变量。
///
/// 只对预设表里认识的镜像显示（mysql / mariadb / postgres）—— 这类信息
/// `wslc image inspect` 不会告诉你，而少给一个变量的后果是**容器起来就退出**、
/// 原因只写在日志里（实机就是这么踩的：mysql 一直退，翻日志才看到缺
/// `MYSQL_ROOT_PASSWORD`）。
///
/// 这一行是**照着输入框实时算**的（渲染时读值），所以：
///
/// - 从下拉框选 / 从「镜像」页进来 → 变量已经被预填，这里说"把值补上"；
/// - **自己手打**镜像引用 → 我们不会预填（预填只挂在"知道镜像了"那两个钩子上，
///   见 `Shell::probe_image_facts`），这里就改说"请加上 `KEY=你的值`"。
///
/// 判断"预填在不在"用的是最朴素的字符串包含：这里只是一句提示，
/// 值不值得为它把 `env` 的解析规则再抄一遍。
pub(crate) fn image_env_hint(dialog: &CreateDialog, cx: &App) -> AnyElement {
    let image = dialog.image.read(cx).value().trim().to_owned();
    let Some(preset) = wslc_core::model::image_preset(&image) else {
        return div().into_any_element();
    };
    let Some(prefill) = preset.prefill_env else {
        return div().into_any_element();
    };

    let alternatives: Vec<&str> = preset
        .required_any_env
        .iter()
        .copied()
        .filter(|key| *key != prefill)
        .collect();
    let or_else = if alternatives.is_empty() {
        String::new()
    } else {
        format!("（也可以改用 {}）", alternatives.join(" / "))
    };

    let raw = dialog.env.read(cx).value().to_owned();
    let text = if raw.contains(&format!("{prefill}=")) {
        format!("这个镜像第一次启动必须给 {prefill}：变量名已经预填，把值补上{or_else}")
    } else {
        format!(
            "这个镜像第一次启动必须给 {prefill}，请在环境变量里加上 `{prefill}=你的值`{or_else}"
        )
    };

    div()
        .text_xs()
        .text_color(theme::text_dim())
        // 第二支文案里有 `{prefill}=你的值` 的反引号 —— 第一支没有，所以
        // 别只在其中一支加。统一在出口过一遍（`AGENTS.md` §5）。
        .child(crate::util::strip_md(&text))
        .into_any_element()
}

/// 拉取策略选择器。
///
/// 三选一是**互斥选项**，所以用官方 `RadioGroup`（`common::radio_row`）：
/// 原先那排"选中就变主色"的按钮对读屏软件是一堆独立动作，
/// 键盘上也拿不到方向键移动 + 空格选中。
pub(crate) fn pull_policy_row(current: &PullPolicy, entity: &Entity<Shell>) -> AnyElement {
    let options: [(&'static str, PullPolicy); 3] = [
        ("只用本地镜像", PullPolicy::Never),
        ("缺了就拉", PullPolicy::Missing),
        ("总是拉取", PullPolicy::Always),
    ];

    let labels: Vec<SharedString> = options
        .iter()
        .map(|(label, _)| SharedString::from(*label))
        .collect();
    let selected = options.iter().position(|(_, policy)| policy == current);
    // `move`：闭包要 `'static`，而 `options` 是本地数组。
    // 它是 `Copy`（`PullPolicy` 也是 `Copy`），所以整体搬进闭包不丢东西。
    let picker = radio_row(
        "create-pull-picker",
        labels,
        selected,
        entity,
        move |shell, ix, _, cx| {
            shell.set_create_pull(options[ix].1, cx);
        },
    );

    h_flex()
        .w_full()
        .gap_2()
        .items_center()
        .child(
            div()
                .flex_none()
                .text_xs()
                .text_color(theme::text_dim())
                .child("镜像拉取策略"),
        )
        .child(picker)
        .into_any_element()
}

/// 等效命令预览。
///
/// 直接调 `RunSpec::to_args()` 生成 —— 和真正执行时用的是**同一段代码**，
/// 不会出现"预览和执行不一致"。用户也可以直接复制到终端复现。
pub(crate) fn command_preview(spec: &RunSpec) -> AnyElement {
    let command = format!("wslc {}", spec.to_args().join(" "));

    v_flex()
        .w_full()
        .gap_1()
        .child(
            div()
                .text_xs()
                .text_color(theme::text_dim())
                .child("等效命令（与实际执行完全一致）"),
        )
        .child(
            div()
                .w_full()
                .rounded_md()
                .bg(theme::bg())
                .p_3()
                .font_family("Consolas")
                .text_xs()
                .text_color(theme::text_muted())
                .child(command),
        )
        .into_any_element()
}

/// 「创建容器」弹窗。
///
/// 表单字段直接对应 [`RunSpec`]，底部用 `to_args()` 实时预览等效命令。
///
/// **强制后台运行**（`-d`）：不带 `-d` 时 `wslc run` 会前台阻塞，
/// 而我们的子进程有超时，超时后会把刚建好的容器连带杀掉。
/// 需要前台交互的场景请用 `wslc` 自己的命令。
pub(crate) fn create_dialog_body(
    dialog: &CreateDialog,
    state: &AppState,
    entity: &Entity<Shell>,
    cx: &App,
) -> AnyElement {
    // ⚠️ 这里传了现有容器名：容器名留空时 `to_spec` 会按镜像推一个
    // （`nginx` / `nginx-2`），推出来的名字**同时出现在下面的等效命令里** ——
    // 预览和实际执行读同一个函数，所以不会不一致。
    let spec = dialog.to_spec(&state.container_names(), cx);
    let image_missing = spec.image.trim().is_empty();
    let command_required = matches!(&dialog.facts, FactsState::Ready(facts) if !facts.has_cmd);
    let mut advanced = v_flex().w_full().gap_3();
    if !command_required {
        advanced = advanced.child(form_field(
            "create-command",
            "启动命令（留空使用镜像默认命令）",
            &dialog.command,
            cx,
            true,
        ));
    }
    advanced = advanced
        .child(form_field(
            "create-network",
            "网络（默认 bridge）",
            &dialog.network,
            cx,
            true,
        ))
        .child(
            h_flex()
                .w_full()
                .gap_3()
                .child(form_field(
                    "create-memory",
                    "内存上限（如 512M）",
                    &dialog.memory,
                    cx,
                    false,
                ))
                .child(form_field(
                    "create-cpus",
                    "CPU 数（如 0.5）",
                    &dialog.cpus,
                    cx,
                    false,
                )),
        )
        .child(pull_policy_row(&dialog.pull, entity));
    if !matches!(&dialog.pull, PullPolicy::Never) {
        advanced = advanced.child(
            div()
                .text_xs()
                .text_color(theme::text_dim())
                .child("拉取镜像需要网络可达；也可先在镜像页拉取，再选择只用本地镜像。"),
        );
    }
    let pull_label = match &dialog.pull {
        PullPolicy::Never => "仅本地",
        PullPolicy::Missing => "缺失时拉取",
        PullPolicy::Always => "始终拉取",
    };
    let mut advanced_summary = vec![format!(
        "网络 {}",
        spec.network.as_deref().unwrap_or("bridge")
    )];
    if let Some(memory) = spec.memory.as_deref() {
        advanced_summary.push(format!("内存 {memory}"));
    }
    if let Some(cpus) = spec.cpus.as_deref() {
        advanced_summary.push(format!("CPU {cpus}"));
    }
    if !spec.command.is_empty() && !command_required {
        advanced_summary.push("自定义启动命令".to_owned());
    }
    advanced_summary.push(pull_label.to_owned());

    v_flex()
        .w_full()
        .gap_4()
            .child(
                // 左边是唯一的事实来源（可以手打、可以粘完整引用），
                // 右边那个下拉框只是**从本地已有的镜像里挑一个**的快捷方式 ——
                // 数据来自每 3 秒刷新的快照，不用手打一长串 registry 前缀
                // （手打时最容易漏的就是它，漏了 `wslc run` 会去 Docker Hub 找，
                // 而那台机器上不可达）。
                h_flex()
                    .w_full()
                    .gap_2()
                    .child(form_field(
                        "create-image",
                        "镜像引用（必填，例如 nginx:latest）",
                        &dialog.image,
                        cx,
                        false,
                    ))
                    .child(
                        v_flex()
                            .gap_1()
                            .w(px(300.))
                            .flex_none()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme::text_dim())
                                    .child("或从本地镜像里选（可搜索）"),
                            )
                            .child(
                                Select::new(&dialog.image_select)
                                    // 占位符说的是"还没选"，不是"没有镜像"。
                                    // v0.6.5 把**空状态**文案写成了占位符，于是
                                    // 展开明明有 3 个镜像、触发器却写着"本地还没有镜像"。
                                    .placeholder("选一个本地镜像…")
                                    // ⚠️ 菜单**默认和触发器一样宽**（`menu_width`
                                    // 默认 `Length::Auto`），而镜像引用比这 300px
                                    // 长得多 —— 不加宽就是每个候选都带省略号。
                                    .menu_width(px(460.))
                                    .empty(|_, _| {
                                        div()
                                            .px_3()
                                            .py_2()
                                            .text_sm()
                                            .text_color(theme::text_muted())
                                            .child("本地还没有镜像 —— 先去「镜像」页拉一个")
                                    })
                                    .w_full()
                                    .menu_max_h(px(320.)),
                            ),
                    ),
            )
            .child(image_facts_hint(dialog))
            .child(form_field(
                "create-name",
                "容器名（留空按镜像命名）",
                &dialog.name,
                cx,
                true,
            ))
            .child(auto_name_hint(dialog, state, cx))
            .children(command_required.then(|| {
                form_field(
                    "create-command",
                    "启动命令（镜像没有默认命令，必填）",
                    &dialog.command,
                    cx,
                    true,
                )
            }))
            .child(
                h_flex()
                    .w_full()
                    .gap_3()
                    .child(form_field(
                        "create-ports",
                        "端口映射（逗号或换行分隔，如 8080:80）",
                        &dialog.ports,
                        cx,
                        false,
                    ))
                    .child(form_field(
                        "create-env",
                        "环境变量 KEY=VALUE（每行一个；值里可以有逗号）",
                        &dialog.env,
                        cx,
                        false,
                    )),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme::text_dim())
                    .child("端口映射需在创建时设置，创建后不能直接修改。"),
            )
            .child(image_env_hint(dialog, cx))
            .child(form_field(
                "create-volumes",
                "卷挂载（卷名:/容器内路径）",
                &dialog.volumes,
                cx,
                true,
            ))
            .child(disclosure(
                UiSection::CreateAdvanced,
                format!("高级设置 · {}", advanced_summary.join(" · ")),
                state,
                entity,
                advanced,
            ))
            .child(disclosure(
                UiSection::CreateCommandPreview,
                "查看等效命令",
                state,
                entity,
                command_preview(&spec),
            ))
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .text_color(if image_missing {
                                theme::warning()
                            } else {
                                theme::text_dim()
                            })
                            .child(if image_missing {
                                "还差一个镜像引用"
                            } else {
                                ""
                            }),
                    )
                    ),
        .into_any_element()
}
