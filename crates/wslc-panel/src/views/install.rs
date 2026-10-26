//! ④b 添加实例页：商店 / 镜像源两条来源、安装日志。

use super::common::{card, empty_state, form_field, form_field_with_error, radio_row};
use super::prelude::*;
// 安装步骤条用官方 `Stepper`（见 [`step_stepper`] 的说明）。只有这一页用它，
// 所以不放 `views::prelude` —— 那会让别的页面多一个用不上的短名字。
use gpui_kit::component::stepper::{Stepper, StepperItem};

/// 「添加实例」页。
///
/// # 五条来源共用一套表单
///
/// 靠 [`InstallSourceKind`] 切换 —— 它们只是参数不同，没必要做成五个页面。
/// 这里**保留**了参考实现没有的「从 .wsl / 文件安装」（`.wsl` 是新格式，`--import` 吃不了）。
///
/// # 表单是懒创建的
///
/// `InputState::new` 需要 `&mut Window`，所以表单在**点击实例列表上那个
/// 「添加实例」按钮时**才建（见 `Shell::ensure_install_form`）。
/// 正常路径下进得来就一定有表单；万一没有（比如程序内部跳过来），
/// 给一句提示而不是 panic。
///
/// # 校验与预览都走纯函数
///
/// 校验（[`AppState::preflight`]）和步骤预览（`install_model::plan`）都是
/// **同两个纯函数**：提交时用的也是它们，所以"挡住的/要跑的"不会走偏。
///
/// 但**渲染时不再把校验错误铺在页面上**：平时不显示红字，只有点了
/// 「开始安装」且没过时，[`Shell::confirm_install`] 才把
/// `InstallForm::field_errors` 记下来；这一页据此把对应输入框
/// 标红、把焦点送过去。参数不合法时连步骤预览都不画 —— 以前底部那张
/// 「装前检查」卡和预览里的同一句错误，现在都没有了。
///
/// ⚠️ 渲染时**不查**"安装目录非空"（那要碰文件系统，每帧查一次会在网络盘上卡住）；
/// 那条检查在提交那一刻做，见 `Shell::confirm_install`。
pub fn add_instance(shell: &Shell, cx: &App, entity: &Entity<Shell>) -> AnyElement {
    let state = &shell.state;
    let Some(form) = shell.install_form.as_ref() else {
        return card(
            "添加实例",
            v_flex()
                .w_full()
                .gap_2()
                // 一句就够：用户要知道的只有"回上一页再点一次"。
                // 原来的第二行（"输入框必须在点击时创建"）是实现细节，
                // 对用户没有任何操作价值。
                .child(empty_state("回到「实例列表」，再点一次「添加实例」")),
        )
        .into_any_element();
    };

    let source = form.source;
    let spec = form.to_spec(
        cx,
        &state.mirrors,
        &state.online,
        // 已经装过的发行版名：镜像源的"裸名字换兄弟"要拿它排除掉
        // （见 `OnlineDistroState::sibling_id`）
        &state.distro_names(),
        state.prefs.install_dir.as_deref(),
    );
    let check = state.preflight(&spec, false);
    // 只有"已经点过开始安装且没过"才标红；平时不常驻任何错误提示。
    let show_errors = form.show_errors;
    let field_errors = form.field_errors;
    let plan = install_model::plan(&spec, &state.plan_context());
    let running = state
        .installing
        .as_ref()
        .map(InstallProgress::is_running)
        .unwrap_or(false);

    // -- 来源三选一 --
    //
    // **互斥选项** → 官方 `RadioGroup`（`common::radio_row`）：原先那排
    // "选中就变主色"的按钮对读屏软件是一堆独立动作，键盘上也拿不到
    // 方向键移动 + 空格选中。顺带不再需要"用 `{:?}` 拼稳定 id"那套讲究 ——
    // 组里的 id 由组件按**位置**给。
    let labels: Vec<SharedString> = InstallSourceKind::ALL
        .iter()
        .map(|kind| SharedString::from(kind.label()))
        .collect();
    let selected = InstallSourceKind::ALL
        .iter()
        .position(|kind| *kind == source);
    let source_picker = radio_row(
        "install-source-picker",
        labels,
        selected,
        entity,
        |shell, ix, _, cx| {
            shell.set_install_source(InstallSourceKind::ALL[ix], cx);
        },
    );
    // -- 输入框 --
    let mut fields: Vec<AnyElement> = vec![form_field_with_error(
        "install-name",
        source.name_field_label(),
        &form.name,
        cx,
        true,
        show_errors && field_errors.name,
    )];

    if let Some((label, _)) = source.file_filter() {
        // 「浏览…」和输入框并排。`flex_1 + min_w_0` 让输入框吃掉剩余宽度，
        // 又在窗口很窄时允许它收缩（不写 `min_w_0` 会把它顶出去）。
        let browse_file = {
            let entity = entity.clone();
            Button::new("install-browse-file")
                .label("浏览…")
                .small()
                .on_click(move |_, window, cx| {
                    entity.update(cx, |shell, cx| shell.browse_install_path(window, cx));
                })
        };

        fields.push(
            h_flex()
                .w_full()
                .gap_2()
                .items_end()
                .child(div().flex_1().min_w_0().child(form_field_with_error(
                    "install-path",
                    label,
                    &form.source_path,
                    cx,
                    true,
                    show_errors && field_errors.source,
                )))
                .child(browse_file)
                .into_any_element(),
        );

        // 「识别为 …」：本地来源**只有一个入口**，装法由后缀决定 ——
        // 用户必须能看出我们猜成了什么（猜错了他会看到一条 wsl 的报错，
        // 而那条报错不会告诉他"是你文件后缀不对"）。
        //
        // 纯函数、不碰磁盘：渲染时现算（读文件头要碰磁盘，那条检查只能放在
        // 提交那一刻，见 `add_instance` 的说明）。
        let path = form.source_path.read(cx).value().trim().to_owned();
        let (hint, color) = match install_model::classify_local_file(&path) {
            Some(kind) => (format!("识别为：{}", kind.label()), theme::text_dim()),
            None if path.is_empty() => (
                "按**后缀**自动判断装法：tar / tar.gz / tar.xz → `--import`；\
                 .vhdx → `--import --vhd`；.wsl → WSL 安装器。"
                    .to_owned(),
                theme::text_dim(),
            ),
            // 认不出来不硬猜，但也不能什么都不说 —— 按 tar 处理并讲清楚
            None => (
                "没认出这个后缀，会按 **tar 归档**处理（`wsl --import`）。\
                 如果它其实是 `.vhdx` 或 `.wsl`，改一下文件名后缀即可。"
                    .to_owned(),
                theme::warning(),
            ),
        };
        fields.push(
            div()
                .text_xs()
                .text_color(color)
                .child(crate::util::strip_md(&hint))
                .into_any_element(),
        );
    }

    // 安装目录：+ 浏览 + 存为默认
    let browse_dir = {
        let entity = entity.clone();
        Button::new("install-browse-dir")
            .label("浏览…")
            .small()
            .on_click(move |_, window, cx| {
                entity.update(cx, |shell, cx| shell.browse_install_dir(window, cx));
            })
    };
    let remember_dir = {
        let entity = entity.clone();
        Button::new("install-remember-dir")
            .label("存为默认")
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.remember_install_dir(cx));
            })
    };

    fields.push(
        v_flex()
            .w_full()
            .gap_1()
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_end()
                    .child(div().flex_1().min_w_0().child(form_field_with_error(
                        "install-dir",
                        if source.requires_install_dir() {
                            "安装目录（必填）"
                        } else {
                            "安装目录（可留空，交给 WSL 决定）"
                        },
                        &form.install_dir,
                        cx,
                        true,
                        show_errors && field_errors.install_dir,
                    )))
                    .child(browse_dir)
                    .child(remember_dir),
            )
            .child(
                div().text_xs().text_color(theme::text_dim()).child(
                    "留空时会用「默认安装目录 + 发行版名」；点「存为默认」把当前这个路径记住。",
                ),
            )
            .into_any_element(),
    );

    // -- 来源专属区块 --
    let source_block: AnyElement = if source.is_online() {
        online_block(state, form, entity)
    } else if source.is_mirror() {
        mirror_block(state, form, entity, cx)
    } else {
        div().into_any_element()
    };
    // 在线 / 镜像的"来源"不是一个普通输入框，没法给 Input 改边框；
    // 外面套一圈红框，效果一样，又不用把错误文案再写一遍。
    let source_block = if show_errors && field_errors.source && !source.needs_path() {
        div()
            .w_full()
            .rounded_lg()
            .border_1()
            .border_color(theme::danger())
            .p_2()
            .child(source_block)
            .into_any_element()
    } else {
        source_block
    };

    // -- 选项 --
    let mut options: Vec<AnyElement> = Vec::new();

    // 这里**没有** WSL 版本选择器：本项目只支持 WSL 2，
    // 装出来的固定是 WSL 2（`wslc_core::model::install::WSL_VERSION`）。
    // 给一个只有一个选项的下拉框不如不给。

    // 布尔选项一律用官方 `Switch`（组件库自带开/关动画、焦点框与键盘可达性）。
    //
    // 早先这里是**按钮**，注释写着"全项目都是这个路子（复选框样式在深色主题下
    // 对比度很差）"。前一半已经不成立（`Checkbox` 在部署页长期在用，没再收到
    // 对比度反馈），后一半也不再是理由 —— `Switch` 是另一套控件，不是复选框。
    // 更重要的是**一致性**：同一批"要不要做某事"的选项，有的画成按钮、有的画成
    // 复选框，用户得先分辨"这个按钮是按一下就做、还是按一下切换状态"。
    if source.supports_launch() {
        // ⚠️ 不要用 `let entity = entity.clone()` 这种同名遮蔽：这个块里要用两次，
        // 第二次会因为"已经从第一次的遮蔽变量里移走了"而编译不过（真的踩到了）。
        let launch_entity = entity.clone();
        options.push(
            Switch::new("toggle-launch")
                .label("装完立即启动")
                .checked(form.launch)
                .on_change(move |checked: &bool, _, cx| {
                    // `Switch` 是**受控**组件：回调给的就是用户请求的新值，
                    // 由我们写回状态再 notify（和 `Checkbox` 同一套约定）。
                    let on = *checked;
                    launch_entity.update(cx, |shell, cx| shell.set_install_launch(on, cx));
                })
                .into_any_element(),
        );

        let web_entity = entity.clone();
        options.push(
            Switch::new("toggle-web-download")
                // 标签说明**打开**代表什么；两个来源的差别在下面那段说明里。
                .label("从 GitHub 下载（--web-download）")
                .checked(form.web_download)
                .on_change(move |checked: &bool, _, cx| {
                    let on = *checked;
                    web_entity.update(cx, |shell, cx| shell.set_web_download(on, cx));
                })
                .into_any_element(),
        );
        // 这条对"装不装得上"影响最大，所以把两条通道的差别说清：
        // 微软商店走的是商店/清单那条通道，`--web-download` 是 WSL 自己去
        // GitHub 下整包 —— 实测本机上前者能连上、后者直连不通（要代理）。
        // 真失败了执行器还会自动换一次（见 `PlannedStep::retry_other_source`）。
        options.push(
            div()
                .text_xs()
                .text_color(theme::text_dim())
                .child(if form.web_download {
                    "GitHub 那条要能连上 raw.githubusercontent.com（被墙时要走代理）；\
                     连不上会自动切回微软商店再试一次。"
                } else {
                    "微软商店是 WSL 官方那条路（本机实测能连上）；\
                     商店也拉不动时，改用「在线发行版（镜像源）」——那条完全不依赖 WSL 的网络。"
                })
                .into_any_element(),
        );
    }

    {
        let entity = entity.clone();
        options.push(
            Switch::new("toggle-default")
                .label("装完设为默认发行版")
                .checked(form.set_default)
                .on_change(move |checked: &bool, _, cx| {
                    let on = *checked;
                    entity.update(cx, |shell, cx| shell.set_install_default(on, cx));
                })
                .into_any_element(),
        );
    }

    // -- 步骤预览（只在参数通过时显示）--
    //
    // 以前这里还有一张「装前检查」卡，把同样的错误再列一遍；参数不合法时
    // 预览卡里还会再写一次 `参数还不对：…`。三处红字说的是同一件事，
    // 所以现在：**不合法就不画预览**，等点了「开始安装」再把对应输入框标红。
    let preview_body: Option<AnyElement> = if show_errors && field_errors.any() {
        // 上一次提交没过：连步骤都先不画，等改完再点一次。
        // （`plan()` 不知道"目录非空"那条，所以不能只看它是不是 Ok。）
        None
    } else {
        match &plan {
            Ok(plan) => {
                let steps: Vec<StepperItem> = plan
                    .steps
                    .iter()
                    .map(|step| step_item(step.label.clone(), Some(step.line()), None))
                    .collect();
                let mut body = v_flex()
                    .w_full()
                    .gap_2()
                    .child(step_stepper("install-steps-preview", steps, 0))
                    .children(plan.notes.iter().map(|note| {
                        div()
                            .text_xs()
                            .text_color(theme::text_dim())
                            // ⚠️ notes 全是 Markdown 写的（`**不做**首次启动初始化`
                            // / `--import`），而 `div` 画的是纯文本 —— 不过
                            // `strip_md` 屏幕上就是字面星号和反引号。
                            // 这一处是 v0.9.13 那轮补 15 处时**唯一漏下的**。
                            .child(crate::util::strip_md(note))
                    }));
                // 提醒不是错误、也不挡提交；只在参数已经能跑时附在步骤下面，
                // 避免和"点开始安装才校验"的红框混在一起。
                if !check.warnings.is_empty() {
                    body = body.children(check.warnings.iter().map(|warning| {
                        div()
                            .text_xs()
                            .text_color(theme::text_dim())
                            .child(format!("! {}", crate::util::strip_md(warning)))
                    }));
                }
                Some(body.into_any_element())
            }
            Err(_) => None,
        }
    };

    // -- 提交 / 取消 --
    //
    // ⚠️ 重定位那一步（导出 → 注销 → 导入）**不给**取消按钮：
    // 它的 `cancellable` 是 false，中途停下等于把刚装好的发行版删掉。
    // 让用户点了之后才发现没用，比"这里没有按钮 + 一句说明"糟得多。
    let cancellable = state
        .installing
        .as_ref()
        .map(|progress| progress.cancellable)
        .unwrap_or(true);

    let action: AnyElement = if running && cancellable {
        let entity_cancel = entity.clone();
        h_flex()
            .w_full()
            .items_center()
            .gap_3()
            .child(
                Button::new("cancel-install")
                    .label("取消安装")
                    .small()
                    .danger()
                    .on_click(move |_, _, cx| {
                        entity_cancel.update(cx, |shell, cx| shell.cancel_install(cx));
                    }),
            )
            .child(elapsed_label(state))
            .into_any_element()
    } else if running {
        h_flex()
            .w_full()
            .items_center()
            .gap_3()
            .child(
                div()
                    .text_xs()
                    .text_color(theme::warning())
                    .child("正在改名/挪位置 —— 这一步不能取消（中途停下会把刚装好的东西删掉）"),
            )
            .child(elapsed_label(state))
            .into_any_element()
    } else {
        // 取消过 / 装完之后按钮要能再点一次：文案跟着状态走。
        let label = if state.installing.is_some() {
            "再装一个"
        } else {
            "开始安装"
        };
        let entity = entity.clone();
        h_flex()
            .w_full()
            .justify_end()
            .child(Button::new("do-install").label(label).primary().on_click(
                move |_, window, cx| {
                    entity.update(cx, |shell, cx| shell.confirm_install(window, cx));
                },
            ))
            .into_any_element()
    };

    let mut page = v_flex()
        .w_full()
        .gap_4()
        .child(card(
            "安装来源",
            v_flex().w_full().gap_3().child(source_picker).child(
                div()
                    .text_xs()
                    .text_color(theme::text_dim())
                    .child(crate::util::strip_md(source.hint())),
            ),
        ))
        .child(card(
            "参数",
            v_flex()
                .w_full()
                .gap_3()
                .children(fields)
                .child(source_block),
        ))
        .child(card("选项", v_flex().w_full().gap_3().children(options)));

    if let Some(preview) = preview_body {
        page = page.child(card("要执行的步骤", preview));
    }
    page = page.child(action);

    // 装过 / 正在装 → 显示日志（装完之后留在页面上，用户要能读它）
    if state.installing.is_some() {
        page = page.child(install_log_card(state, source, entity));
    }

    page.into_any_element()
}

/// 「微软商店」那一块：刷新 + **下拉框** + 来源说明。
///
/// 参考实现也是一个下拉框（`CustomComboBox`）—— 原先我们那种"几十个按钮
/// 铺满一屏 + 外面再挂一个搜索框"既占地方、又要用户自己找。
/// 搜索收进下拉框里（`SelectState::searchable`）。
///
/// 不需要 `cx`：下拉框的选中由 `Shell` 那边订阅，这里只负责画
/// （不像镜像源那块还要读自定义 URL 输入框）。
pub(crate) fn online_block(
    state: &AppState,
    form: &InstallForm,
    entity: &Entity<Shell>,
) -> AnyElement {
    let refresh = {
        let entity = entity.clone();
        Button::new("online-refresh")
            .label(if state.online.loading {
                "正在拉取…"
            } else {
                "刷新商店清单"
            })
            .small()
            .on_click(move |_, window, cx| {
                entity.update(cx, |shell, cx| shell.refresh_online_list(window, cx));
            })
    };

    // 下拉框：显示 `Ubuntu 24.04 LTS (Ubuntu-24.04)`，取到的是 id。
    // 选中由 `Shell` 那边订阅（`SelectEvent::Confirm`），这里只负责画。
    let picker = Select::new(&form.store_select)
        .placeholder("选一个发行版（可以打字搜索）")
        .w_full()
        .menu_max_h(px(320.));

    // 状态：正在拉 / 失败原因（原文）/ 来源说明
    let status: AnyElement =
        if state.online.loading {
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child("正在拉取在线清单…")
                .into_any_element()
        } else if !state.online.error.trim().is_empty() {
            v_flex()
                .w_full()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::warning())
                        .child(format!("清单有问题：{}", state.online.error)),
                )
                .child(div().text_xs().text_color(theme::text_dim()).child(
                    "拉不到清单也能装：直接在「发行版名」里填在线 id（例如 Ubuntu-24.04）。",
                ))
                .into_any_element()
        } else if !state.online.items.is_empty() {
            let mut lines: Vec<AnyElement> = vec![
                div()
                    .text_xs()
                    .text_color(theme::text_dim())
                    .child(format!(
                        "共 {} 个可装。{}",
                        state.online.items.len(),
                        state.online.source.label()
                    ))
                    .into_any_element(),
            ];
            // ⚠️ 清单是**我们**用 curl 从 CDN 兜底拉到的（不是 wsl 自己拉的）时，
            // 说明 `wsl.exe` 连不上它要的那份 manifest —— 而安装那一步正是走它。
            // 实测（2026-10-10）这种状态下商店与 `--web-download` 会**双双**报
            // `Wsl/InstallDistro/WININET_E_CANNOT_CONNECT`。与其让用户装到一半才失败，
            // 不如现在就说清"这条来源现在多半装不上、该换哪条"。
            //
            // 注意这句话**会过期**（实测同一小时内 `wsl -l -o` 从成功变成 500 又变回来），
            // 所以它必须带上"怎么刷新"的出口，而不是一句判决。
            match state.online.source {
                OnlineListSource::FallbackJson => lines.push(
                    div()
                        .text_xs()
                        .text_color(theme::warning())
                        .child(
                            "注意：这份清单是面板自己从 CDN 拉的，说明 wsl 自己拉不到它要的那份 \
                         manifest —— 现在用「微软商店」装多半会连不上。\
                         先试「在线发行版（镜像源）」（那条不经过 wsl 的网络）；\
                         如果你刚把代理修好，点一下上面的「刷新商店清单」再看这行还在不在。",
                        )
                        .into_any_element(),
                ),
                OnlineListSource::Wsl => lines.push(
                    div()
                        .text_xs()
                        .text_color(theme::success())
                        .child("wsl 自己拉到了清单 —— 这条来源现在是通的。")
                        .into_any_element(),
                ),
                OnlineListSource::Unknown => {}
            }
            // 选中了"不带版本号的家族名"（`Ubuntu`）时提醒一句：实测这类条目在这台机器上
            // **装不动**（`wsl --install -d Ubuntu` 两秒就 `WININET_E_CANNOT_CONNECT`），
            // 装的时候会自动换成带版本的那条（见 `OnlineDistroState::sibling_id`）。
            //
            // ⚠️ 兄弟**已经装过**时不能提这句：那时 `sibling_id` 会给 `None`
            // （换了也只会撞 `ERROR_ALREADY_EXISTS`），说"会自动改用 X"就是假话。
            if let Some(picked) = state.online.selected.as_deref() {
                if let Some(sibling) = state.online.sibling_id(picked, &state.distro_names()) {
                    lines.push(
                        div()
                            .text_xs()
                            .text_color(theme::warning())
                            .child(format!(
                                "「{picked}」不带版本号 —— 实测这类条目在本机装不动\
                             （2 秒就报连接失败），装的时候会自动改用「{sibling}」再试一次。"
                            ))
                            .into_any_element(),
                    );
                }
            }
            v_flex().w_full().gap_1().children(lines).into_any_element()
        } else {
            div()
                .text_xs()
                .text_color(theme::text_dim())
                .child("还没拉过清单 —— 点上面的按钮，或者直接在「发行版名」里手输 id。")
                .into_any_element()
        };

    v_flex()
        .w_full()
        .gap_2()
        .child(
            h_flex()
                .w_full()
                .gap_2()
                .items_center()
                .child(refresh)
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child("选择发行版（微软商店 / Microsoft Store）"),
                ),
        )
        .child(picker)
        .child(status)
        .into_any_element()
}

/// 「在线发行版（镜像源）」那一块：清单（动态拉）+ **下拉框** + 自动测速状态 + 自定义 URL。
///
/// 清单来自 `wslc_core::cmd::catalog`（wslui 的公开接口，见 `mirrors.rs` 的说明），
/// 顺序照参考实现排（Ubuntu / Debian 在最前，见 `sort_offers_like_reference`）。
///
/// # 为什么没有"镜像站列表"了
///
/// 参考实现是**自动测速择优**：用户只选发行版，面板自己并发测一遍、挑最快的那个。
/// 原先我们让用户先点「探测最快镜像」、再对着十几行"清华 TUNA 0.16 秒 / 南大 …"
/// 自己挑 —— 那是把**面板该做的事**推给了用户（用户原话："不需要那些什么清华源
/// 之类的"）。现在测速在**选中发行版时自动开始**，界面上只留一行状态；
/// 镜像站名字进安装日志（出问题时那才是排查的地方）。
pub(crate) fn mirror_block(
    state: &AppState,
    form: &InstallForm,
    entity: &Entity<Shell>,
    cx: &App,
) -> AnyElement {
    let refresh = {
        let entity = entity.clone();
        Button::new("mirror-refresh")
            .label(if state.mirrors.loading {
                "正在拉取…"
            } else {
                "刷新清单"
            })
            .small()
            .on_click(move |_, window, cx| {
                entity.update(cx, |shell, cx| shell.refresh_mirror_catalog(window, cx));
            })
    };

    // 下拉框：显示 `Ubuntu 24.04 (12 镜像源)`，取到的是清单里的 id。
    let picker = Select::new(&form.mirror_select)
        .placeholder("选一个发行版（自动测速择优）")
        .w_full()
        .menu_max_h(px(320.));

    // 状态行：正在拉 / 拉失败（原话）/ 清单概览
    let status: AnyElement = if state.mirrors.loading {
        div()
            .text_xs()
            .text_color(theme::text_muted())
            .child("正在拉取发行版清单…")
            .into_any_element()
    } else if let Some(error) = state.mirrors.load_error.as_ref() {
        div()
            .text_xs()
            .text_color(theme::warning())
            .child(format!("清单没拉到：{error}"))
            .into_any_element()
    } else if !state.mirrors.offers.is_empty() {
        div()
            .text_xs()
            .text_color(theme::text_dim())
            .child(format!(
                "共 {} 个可装版本。{}{}",
                state.mirrors.offers.len(),
                state.mirrors.origin,
                state
                    .mirrors
                    .updated
                    .as_ref()
                    .map(|time| format!("（清单更新于 {time}）"))
                    .unwrap_or_default()
            ))
            .into_any_element()
    } else {
        div()
            .text_xs()
            .text_color(theme::text_dim())
            .child("还没拉到清单 —— 点「刷新清单」，或者直接在下面填一个 rootfs URL。")
            .into_any_element()
    };

    // 测速状态：正在测 / 已选好 / 全都拿不到。
    //
    // ⚠️ **不写镜像站名字**（用户要求"不需要那些什么清华源之类的"）：
    // 只给"选好了 + 多大"，名字在安装日志里（`从 <site> 下载 …` 那一步）。
    let probe_line: AnyElement = if state.mirrors.probing {
        div()
            .text_xs()
            .text_color(theme::text_muted())
            .child("正在自动测速，挑最快的镜像…")
            .into_any_element()
    } else if let Some(error) = state.mirrors.error.as_ref() {
        div()
            .text_xs()
            .text_color(theme::warning())
            .child(error.clone())
            .into_any_element()
    } else if let Some(choice) = state.mirrors.chosen.as_ref() {
        div()
            .text_xs()
            .text_color(theme::success())
            .child(format!(
                "已自动测速选好镜像（{} 个来源里最快的那个{}）",
                state.mirrors.results.len(),
                choice
                    .bytes
                    .map(|bytes| format!("，{}", mirrors::human_bytes(bytes)))
                    .unwrap_or_default()
            ))
            .into_any_element()
    } else {
        div()
            .text_xs()
            .text_color(theme::text_dim())
            .child("选中发行版后会自动测速挑最快的镜像。也可以在下面直接填 URL。")
            .into_any_element()
    };

    v_flex()
        .w_full()
        .gap_2()
        .child(
            h_flex()
                .w_full()
                .gap_2()
                .items_center()
                .child(refresh)
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child("选择发行版（在线发行版 · 镜像源）"),
                ),
        )
        .child(status)
        .child(picker)
        .child(probe_line)
        .child(form_field(
            "install-mirror-url",
            "自定义下载地址（留空 = 用自动测速挑出来的；.wsl 包和 tar 都支持）",
            &form.mirror_url,
            cx,
            true,
        ))
        .into_any_element()
}

/// 一句"已经跑了多久"。
pub(crate) fn elapsed_label(state: &AppState) -> AnyElement {
    let text = state
        .installing
        .as_ref()
        .map(|progress| {
            let mut parts = vec![format!(
                "已用 {}",
                crate::util::human_secs(progress.elapsed_secs)
            )];
            if let Some(percent) = progress.percent() {
                parts.push(format!("{percent:.1}%"));
            }
            if let Some(speed) = progress.speed() {
                parts.push(speed);
            }
            parts.join(" · ")
        })
        .unwrap_or_default();

    div()
        .text_xs()
        .text_color(theme::text_muted())
        .child(text)
        .into_any_element()
}

/// 安装步骤条里的**一步**：标题（可带 ✔/✘）+ 可选的一行命令。
fn step_item(
    label: impl Into<SharedString>,
    line: Option<String>,
    mark: Option<bool>,
) -> StepperItem {
    let mut text = v_flex().gap_1().child(
        h_flex()
            .gap_2()
            .items_center()
            .children(mark.map(|ok| {
                let (glyph, color) = if ok {
                    ("✔", theme::success())
                } else {
                    ("✘", theme::danger())
                };
                div().flex_none().text_xs().text_color(color).child(glyph)
            }))
            .child(
                div()
                    .text_sm()
                    .text_color(theme::text())
                    .child(label.into()),
            ),
    );
    if let Some(line) = line {
        text = text.child(
            div()
                .font_family("Consolas")
                .text_xs()
                .text_color(theme::text_muted())
                .child(line),
        );
    }
    StepperItem::new().child(text)
}

/// 「要执行的步骤」与「安装过程」共用的**步骤条**（官方 `Stepper`）。
///
/// # 为什么两处共用一个控件
///
/// 预览（`plan.steps`）和进行中（`progress.finished`）说的是同一件事：
/// 「这一串步骤，走到第几步」。原先前者是等宽命令行列表、后者是带 ✔/✘ 的
/// 灰字列表 —— 同一件事两种画法，用户从"预览"到"开始装"要重新认一遍布局。
///
/// # 数据来源为什么不能合并
///
/// 进行中必须读 `progress.finished`（**执行器的账**），不能读 `plan.steps`：
/// 安装期间参数框仍然可编辑，`plan` 会跟着变，用它对账会把 ✔ 标到错的步骤上。
///
/// # 为什么竖排
///
/// 每一步都可能带一行命令行（`wsl --install …`）；横排会被压成省略号，
/// 而那一行正是用户要复制去复现的。
///
/// ⚠️ `id` 由调用方给，而且两处**必须不同** —— 预览和进度可能同屏
/// （安装期间预览卡还在），GPUI 的元素 id 撞了会让两处共用一个状态。
fn step_stepper(id: &'static str, items: Vec<StepperItem>, selected: usize) -> AnyElement {
    Stepper::new(id)
        .vertical()
        .w_full()
        .selected_index(selected)
        .items(items)
        .into_any_element()
}

/// 安装日志卡片：步骤 + 进度 + 输出 + 结局。
///
/// 装完之后**留在页面上**（不自动清空）：失败了用户要读日志，
/// 成功了也能对一下"到底跑了哪些步骤"。下次点「再装一个」时被替换掉。
pub(crate) fn install_log_card(
    state: &AppState,
    source: InstallSourceKind,
    entity: &Entity<Shell>,
) -> AnyElement {
    let Some(progress) = state.installing.as_ref() else {
        return div().into_any_element();
    };

    let head = h_flex()
        .w_full()
        .gap_2()
        .items_center()
        .justify_between()
        .child(
            div()
                .text_sm()
                .text_color(theme::text())
                .child(if progress.total == 0 {
                    format!("正在准备安装 {}…", progress.name)
                } else {
                    format!(
                        "第 {} / {} 步：{}",
                        progress.index, progress.total, progress.step_label
                    )
                }),
        )
        .child(elapsed_label(state));

    // 步骤条：已走完的带 ✔/✘，正在走的是"当前项"。
    //
    // 数据来源是 `progress.finished`（**执行器的账**）而不是 `plan.steps` ——
    // 安装期间参数框仍然可编辑，`plan` 会跟着变（见 [`step_stepper`] 的说明）。
    let mut steps: Vec<StepperItem> = progress
        .finished
        .iter()
        .map(|step| step_item(step.label.clone(), None, Some(step.ok)))
        .collect();
    if progress.is_running() {
        steps.push(step_item(
            progress.step_label.clone(),
            Some(progress.step_line.clone()).filter(|line| !line.trim().is_empty()),
            None,
        ));
    }
    let steps = step_stepper("install-steps-progress", steps, progress.finished.len());

    let outcome: AnyElement = match &progress.outcome {
        None => div().into_any_element(),
        Some(InstallOutcome::Success(message)) => div()
            .text_sm()
            .text_color(theme::success())
            .child(format!("✔ {message}"))
            .into_any_element(),
        Some(InstallOutcome::Failed { detail, step }) => div()
            .text_sm()
            .text_color(theme::danger())
            .child(format!(
                "✘ 安装失败{}{detail}",
                step.as_ref()
                    .map(|step| format!("（{step}）"))
                    .unwrap_or_default()
            ))
            .into_any_element(),
        Some(InstallOutcome::Cancelled) => div()
            .text_sm()
            .text_color(theme::warning())
            .child("已取消 —— 已经装到一半的东西不会自动回收，去实例列表看一眼")
            .into_any_element(),
    };

    // 微软商店那条路失败时的**一键出口**：换成镜像源，并尽量选中同一个发行版。
    //
    // 为什么值得单独做个按钮：那条路依赖 `wsl.exe` 自己的网络（实测：它的清单
    // 拉不到时商店与 `--web-download` 会双双报 `WININET_E_CANNOT_CONNECT`），
    // 而镜像源是**我们自己去国内镜像站下载**、不经过 wsl 的网络 —— 同一个发行版
    // 往往在那边也有（Ubuntu/Debian/Alpine/Arch/Fedora/…）。让用户自己去
    // 清单里翻一遍、再手动对齐版本，是没必要付出的代价。
    let fallback: AnyElement = if source.is_online()
        && matches!(progress.outcome, Some(InstallOutcome::Failed { .. }))
    {
        let entity = entity.clone();
        h_flex()
            .w_full()
            .items_center()
            .gap_3()
            .child(
                Button::new("install-switch-to-mirror")
                    .label("改用「在线发行版（镜像源）」再试")
                    .small()
                    .primary()
                    .on_click(move |_, window, cx| {
                        entity.update(cx, |shell, cx| shell.switch_to_mirror_source(window, cx));
                    }),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme::text_dim())
                    .child("那条是面板自己从国内镜像站下载，不依赖 wsl 的网络。"),
            )
            .into_any_element()
    } else {
        div().into_any_element()
    };

    // ⚠️ 滚动容器必须带 `.id(...)`（见 online_block 的注释）。
    //
    // 只画**最后** 400 行：日志上限是 2000 行，一帧往界面里塞 2000 个元素
    // 是白白的开销，而用户真正要看的就是尾部（拉取那边的日志上限也是 200 行）。
    const SHOWN: usize = 400;
    let shown_from = progress.log.len().saturating_sub(SHOWN);
    let tail = &progress.log[shown_from..];

    let log: AnyElement = if tail.is_empty() {
        div().into_any_element()
    } else {
        v_flex()
            .id("install-log")
            .w_full()
            .max_h(px(260.))
            .overflow_y_scroll()
            .gap_1()
            .children(tail.iter().map(|line| {
                div()
                    .font_family("Consolas")
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(line.clone())
            }))
            .into_any_element()
    };

    let log_note = if shown_from > 0 {
        format!(
            "已收到 {} 行输出（只显示最后 {SHOWN} 行）",
            progress.log.len()
        )
    } else {
        format!("已收到 {} 行输出", progress.log.len())
    };

    card(
        "安装过程",
        v_flex()
            .w_full()
            .gap_2()
            .child(head)
            .child(steps)
            .child(outcome)
            .child(fallback)
            .child(log)
            .child(
                div()
                    .text_xs()
                    .text_color(theme::text_dim())
                    .child(log_note),
            ),
    )
    .into_any_element()
}

// ---------------------------------------------------------------------------
// ⑤ 应用设置
// ---------------------------------------------------------------------------
