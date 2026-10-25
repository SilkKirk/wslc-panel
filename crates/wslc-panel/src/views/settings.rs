//! ⑤ 应用设置页：四个 tab 与其中的「界面 / 终端 / 主题 / 更新 / 关于」卡片。

use super::common::{card, cell_link, kv, radio_row};
use super::config::{config, wsl_config};
use super::prelude::*;

/// 应用设置页。
///
/// 这里管的是**本程序自己的偏好**
/// （`%LOCALAPPDATA%\wslc-panel\prefs.json`），
/// 和「wslc 配置」tab 管的 `wslc` 的 `settings.yaml` 是两回事 ——
/// 分开放是刻意的，见 `prefs.rs` 的模块说明。
///
/// v0.3 只做两项（都是"简单且立刻见效"的）：自动刷新间隔、界面主题。
/// 后来加了「终端」卡 —— 它不是纯数据：自定义路径那个输入框住在
/// `Shell` 里（`InputState::new` 要 `&mut Window`），所以这一页现在收
/// `&Shell` 而不是 `&AppState`（和「添加实例」页同一个理由）。
pub fn app_settings(shell: &Shell, entity: &Entity<Shell>) -> AnyElement {
    let state = &shell.state;
    let content: AnyElement = match state.settings_tab {
        SettingsTab::General => settings_general(shell, entity),
        // 「高级」和「WSL」就是把原先那两个独立页面搬进来当 tab ——
        // 内容一行没改，只是不再各占一个侧边栏入口。
        SettingsTab::Advanced => config(state, entity),
        SettingsTab::Wsl => wsl_config(state, entity),
        SettingsTab::About => settings_about(shell, entity),
    };

    v_flex()
        .w_full()
        .gap_4()
        .child(settings_tabs(state, entity))
        .child(content)
        .into_any_element()
}

/// 设置页的 tab 行。
///
/// 用 Button 而不是 gpui-kit 的 `TabBar`：项目现有的"选中态"就是这么做的
/// （见 [`theme_card`]），风格一致，而且是**已经编译通过**的形式。
/// 想换成 `TabBar` 的话，替换点只有这一个函数。
/// 「应用设置」的四个 tab（常规 / wslc 配置 / WSL 配置 / 关于）。
///
/// 和 [`db_tabs`] 同一个做法：官方 `TabBar` + `Tab`，不是拿 `Button` 拼。
pub(crate) fn settings_tabs(state: &AppState, entity: &Entity<Shell>) -> AnyElement {
    use gpui_kit::component::tab::{Tab, TabBar};

    let tabs: Vec<Tab> = SettingsTab::ALL
        .iter()
        .map(|tab| Tab::new().label(tab.label()))
        .collect();

    let selected = SettingsTab::ALL
        .iter()
        .position(|tab| *tab == state.settings_tab)
        .unwrap_or(0);

    let entity = entity.clone();
    TabBar::new("settings-tabs")
        .pill()
        .selected_index(selected)
        .children(tabs)
        .on_click(move |ix, _, cx| {
            if let Some(tab) = SettingsTab::ALL.get(*ix) {
                let tab = *tab;
                entity.update(cx, |shell, cx| shell.set_settings_tab(tab, cx));
            }
        })
        .into_any_element()
}

/// 「常规」tab：界面 + 主题 + 终端。
///
/// 主题 / 刷新 / 终端三张卡。
///
/// 「关于」以前是一个独立的侧边栏页面；v0.9.22 起它变成这一页的最后一个
/// tab（见 [`settings_about`]）—— 它只有只读信息，不值得占一个一级入口。
pub(crate) fn settings_general(shell: &Shell, entity: &Entity<Shell>) -> AnyElement {
    let state = &shell.state;
    v_flex()
        .w_full()
        .gap_4()
        .child(interface_card(state, entity))
        .child(terminal_card(shell, entity))
        .child(theme_card(state, entity))
        .into_any_element()
}

/// 「应用设置 → 关于」tab：应用介绍、版本、构建、会话、地址，以及**更新**。
///
/// 它从独立页面收进来（v0.9.22）：这里几乎没有开关，占一个侧边栏入口太贵；
/// 放进设置也不会让人误以为能改东西 —— 它已经是最后一个 tab 了。
///
/// v0.10 起这一页多了「更新」卡，所以它开始收 `&Shell`（要发起动作、
/// 要读 `Phase`）—— 和「常规」tab 同一个理由。
pub fn settings_about(shell: &Shell, entity: &Entity<Shell>) -> AnyElement {
    let state = &shell.state;
    const LINKS: &[(&str, &str)] = &[
        ("项目主页", "https://github.com/SilkKirk/wslc-panel"),
        ("问题反馈", "https://github.com/SilkKirk/wslc-panel/issues"),
        (
            "参考项目 owu/wsl-dashboard",
            "https://github.com/owu/wsl-dashboard",
        ),
        ("界面库 gpui-kit", "https://github.com/longbridge/gpui-kit"),
    ];

    let rows: Vec<AnyElement> = LINKS
        .iter()
        .map(|(label, url)| {
            v_flex()
                .w_full()
                .gap_1()
                .child(div().text_xs().text_color(theme::text_dim()).child(*label))
                // 以前这里是一个纯 `div` —— 地址看得见、点不动。
                // `cell_link` 是表格里那个"可点 URL"助手：默认浏览器打开 + tooltip。
                .child(cell_link(*label, *url, *url, entity))
                .into_any_element()
        })
        .collect();

    let page = v_flex()
        .w_full()
        .gap_4()
        .child(card(
            "wslc-panel",
            v_flex()
                .w_full()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .text_color(theme::text())
                        .child("WSL 容器（wslc）和发行版（wsl.exe）的图形管理面板。"),
                )
                .child(
                    // 四条键值对 → 官方 `DescriptionList`（`bordered(false)`：
                    // 它们已经在 `card()` 里了，再加一圈边框就是两层）。
                    //
                    // 用它而不是四个 `kv()` 的好处：**标签列宽自动对齐** ——
                    // `horizontal` 是 `DescriptionList` 的默认布局，它给所有
                    // label 同一个宽度（默认 120px）；而手写那版每行各自
                    // `justify_between()`，"版本" / "界面库" 长度不同，
                    // 值那一列会错开。
                    DescriptionList::new()
                        .bordered(false)
                        .label_width(px(90.))
                        .item("版本", env!("CARGO_PKG_VERSION"), 1)
                        .item("构建", crate::short_build_sha(), 1)
                        // 会话名从页头挪过来：报问题时它和构建号一样有用，但不该
                        // 每页都占着页头。
                        .item("会话", state.session_label(), 1)
                        // ⚠️ 版本号要跟着 `Cargo.toml` 的 `gpui-kit` 走 —— 忘了改
                        // 就会让「关于」页显示一个**用户会当真**的错版本
                        // （报 issue 时会照抄它）。v0.17.0 起是 0.8.0。
                        .item("界面库", "gpui-kit 0.8.0（GPUI）", 1),
                )
                .child(div().text_xs().text_color(theme::text_dim()).child(
                    "许可证 Apache-2.0。参考项目 owu/wsl-dashboard 是 GPL-3.0-only，\
                     只用来理解机制，没有代码进入本仓库。",
                )),
        ))
        .child(update_card(shell, entity))
        .child(card("地址", v_flex().w_full().gap_3().children(rows)));

    // ⚠️ 仅 debug 构建：官方 `Dialog` 的验证入口（见 [`dialog_spike_card`]）。
    // release 构建里这一整段不存在，所以发布产物不会有这个按钮。
    #[cfg(debug_assertions)]
    let page = page.child(dialog_spike_card());

    page.into_any_element()
}

/// 【SPIKE，仅 debug】官方 `Dialog` 能不能替掉手写的 18 个模态 —— 一个最小验证。
///
/// # 为什么要有它
///
/// 我们的 18 个模态是**自己拼**的：`modal_scrim`（遮罩 + `.occlude()` +
/// 140ms 淡入 + 稳定 id）、`modal_card`、以及 `Shell::render` 末尾那条
/// **手工层序链子**。那条链子已经踩过两次坑（`AGENTS.md` §7.5）。
///
/// 官方 `Dialog` 是**窗口级层栈**（读 `src/dialog/dialog.rs` 与 `window_ext.rs`
/// 确认）：
///
/// - `window.open_dialog(cx, |dialog, window, cx| …)` 把对话框压进
///   `WindowState::active_dialogs`，由 `Root` 统一渲染；
/// - `Dialog::render` 里用 `layer_ix` 做层叠偏移、`Anchored` + `snap_to_window`
///   定位、`.occlude()` 自带（不需要我们记得加）、`close_on_escape(keyboard)`、
///   `close_on_backdrop_press(overlay_closable)`、焦点句柄、入场动画全都有；
/// - **层序 = 栈序**，也就是说"从详情里弹出的确认框盖在详情**之上**"是**结构保证**的，
///   不需要人工维护渲染顺序 —— 那正是 §7.5 那条坑的根治办法。
///
/// # 它要求的两件事（迁移前必须想清楚）
///
/// 1. **浮层不再是 `Shell::render` 的孩子**：要用 `open_dialog` 把它压进窗口栈。
///    而 `open_dialog` 是**打开动作**，不是"每帧画一次" —— 所以
///    "状态说该显示" → "调一次 open_dialog"之间需要一个**只触发一次**的接线
///    （现在 18 个浮层全是"状态驱动的纯函数渲染"，这是最大的改动面）；
/// 2. 官方的 builder 闭包**可能被重跑**（源码注释原话："it may run again when the
///    dialog layer is rebuilt. Keep it cheap and create persistent entities before
///    opening, not inside the builder"）—— 所以 `InputState` 这类实体必须在**打开前**
///    建好。我们已经是懒创建 + 存在 `Shell` 上，这条天然满足。
///
/// # 这个按钮做什么
///
/// 点一下弹一个官方 `Dialog`，里面写清"该验证什么"。本机没有 Rust 工具链，
/// 所以**只能由人在实机上判断**；CI 只能证明这段代码编得过。
#[cfg(debug_assertions)]
fn dialog_spike_card() -> AnyElement {
    // 就近导入：只有这里用（放 prelude 会让 release 构建报 unused import）。
    use gpui_kit::component::WindowExt as _;

    let open = Button::new("dialog-spike-open")
        .label("打开官方 Dialog（spike）")
        .small()
        .on_click(move |_, window, cx| {
            window.open_dialog(cx, |dialog, _window, _cx| {
                dialog
                    .title("官方 Dialog（spike）")
                    .w(px(560.))
                    // 这三样我们现在**没有**：点遮罩关、Esc 关、以及栈序保证。
                    .overlay(true)
                    .overlay_closable(true)
                    .keyboard(true)
                    .child(
                        v_flex()
                            .w_full()
                            .gap_2()
                            .child(div().text_sm().text_color(theme::text()).child(
                                "这个对话框由 gpui-component 的窗口级栈渲染，不是 Shell 的孩子。",
                            ))
                            .child(div().text_xs().text_color(theme::text_dim()).child(
                                "该验证四件事：① 它盖在所有自绘浮层之上；② Esc 能关；\
                                 ③ 点遮罩能关；④ 从某个自绘浮层里再打开它，它仍在上层。",
                            )),
                    )
            });
        });

    card(
        "Dialog spike（仅 debug 构建）",
        v_flex()
            .w_full()
            .gap_2()
            .child(open)
            .child(div().text_xs().text_color(theme::text_dim()).child(
                "验证用：官方 Dialog 走窗口级栈。若四项都通过，18 个手写模态就可以迁过去 —— \
                 顺带删掉遮罩的 occlude、入场动画 id、以及 Shell::render 末尾那条手工层序链子。",
            )),
    )
    .into_any_element()
}

/// 「更新」卡。
///
/// # 只有一个动作入口
///
/// 「检查更新」「打开发布页」「跳过这个版本」全在这一张卡里（`AGENTS.md` §5
/// 的"同一个动作只给一个入口"）。启动时查到新版本只发一条 toast 指向这里，
/// 不在页头再摆一颗按钮。
///
/// # 为什么不做"下载并安装"
///
/// 用户要的是**提示**，下载他自己挑加速站（`docs/PLAN-install-update.md` §9）。
/// 而且实测本机到 release 资产只有 11–15 KB/s，30 MB 要 40 分钟 ——
/// 应用内下载只会更糟。
pub(crate) fn update_card(shell: &Shell, entity: &Entity<Shell>) -> AnyElement {
    use crate::update::Phase;

    let state = &shell.state;
    let phase = &state.update;
    let now = crate::util::now_secs();
    let status = crate::update::status_line(phase, now);
    let checking = matches!(phase, Phase::Checking);

    let mut body = v_flex().w_full().gap_3();

    // 状态行。检查中/有新版时用正文色，其余用次要色 —— 一眼能看出"有情况"。
    // 用 guard 而不是 `skipped: false` 字面量模式：`phase` 是引用，
    // 在引用上匹配字段字面量要绕一层解引用，guard 更直白也不会写错。
    let emphasise = checking || matches!(phase, Phase::Available { skipped, .. } if !*skipped);
    body = body.child(
        div()
            .text_sm()
            .text_color(if emphasise {
                theme::text()
            } else {
                theme::text_dim()
            })
            // 文案里没有 Markdown 标记，但这条规矩是守卫而不是一次修补：
            // 以后改文案的人可能顺手写上 `**…**`（见 `AGENTS.md` §5）。
            .child(crate::util::strip_md(&status)),
    );

    // 有新版时把发布说明的开头一小段露出来 —— 用户要据此决定"值不值得更新"。
    if let Phase::Available { release, .. } = phase {
        if !release.notes_head.is_empty() {
            body = body.child(
                div()
                    .text_xs()
                    .text_color(theme::text_dim())
                    .child(crate::util::strip_md(&release.notes_head)),
            );
        }
    }

    // 按钮行。
    let mut buttons: Vec<AnyElement> = Vec::new();

    if checking {
        // 检查中就不给按钮（全项目不用 `.disabled()`，见 `AGENTS.md` §7.5）。
        buttons.push(
            div()
                .text_xs()
                .text_color(theme::text_dim())
                .child("正在检查…")
                .into_any_element(),
        );
    } else {
        let mut check = Button::new("update-check")
            .label("检查更新")
            .small()
            .on_click({
                let entity = entity.clone();
                move |_, _, cx| {
                    entity.update(cx, |shell, cx| shell.check_update(true, cx));
                }
            });
        // 还没查过 / 查失败时，"检查更新"是主按钮。
        if matches!(phase, Phase::Idle | Phase::Failed { .. }) {
            check = check.primary();
        }
        buttons.push(check.into_any_element());

        if let Phase::Available { skipped, .. } = phase {
            let open = Button::new("update-open")
                .label("打开发布页")
                .small()
                .primary()
                .on_click({
                    let entity = entity.clone();
                    move |_, _, cx| {
                        entity.update(cx, |shell, cx| shell.open_release_page(cx));
                    }
                });
            buttons.push(open.into_any_element());

            if !*skipped {
                let skip = Button::new("update-skip")
                    .label("跳过这个版本")
                    .small()
                    .on_click({
                        let entity = entity.clone();
                        move |_, _, cx| {
                            entity.update(cx, |shell, cx| shell.skip_update_version(cx));
                        }
                    });
                buttons.push(skip.into_any_element());
            }
        }
    }

    // 「启动时自动检查」是个**布尔设置**，用官方 `Switch`。
    //
    // 早先它是一颗按钮、文案写"启动时自动检查：开 / 关"，注释的理由是
    // "这一格不值得为它引一个新组件的用法"。现在 `Switch` 已经在
    // 「添加实例」页用起来了，这条理由随之消失；而且按钮的两种文案读起来
    // 像两个**动作**（"关"是让人点它去关掉吗？），开关没有这个歧义。
    let auto_on = state.prefs.auto_check_update;
    let auto = Switch::new("update-auto-check")
        .label("启动时自动检查")
        .checked(auto_on)
        .on_change({
            let entity = entity.clone();
            move |checked: &bool, _, cx| {
                let next = *checked;
                entity.update(cx, |shell, cx| shell.set_auto_check_update(next, cx));
            }
        });
    buttons.push(auto.into_any_element());

    body = body
        .child(h_flex().w_full().gap_2().flex_wrap().children(buttons))
        .child(div().text_xs().text_color(theme::text_dim()).child(
            "只检查「有没有新版本」，不会自己下载或替换文件。\
             更新请到发布页取 wslc-panel.exe 或安装包（需要走加速的话就在浏览器里换源）。",
        ));

    card("更新", body).into_any_element()
}

/// 主题选择卡（跟随系统 / 深色 / 浅色）。
///
/// 切换本身在 [`Shell::set_theme`] 里做完（持久化 + 两套配色一起换）——
/// 这里只负责画按钮。以前那一步必须写在点击回调里（`Theme::change` 要
/// `&mut App`），现在 `Shell` 的方法里也能拿到，所以收成一处，
/// 免得"回调里切了组件库、状态里忘了切自绘配色"这类两处不同步的 bug。
pub(crate) fn theme_card(state: &AppState, entity: &Entity<Shell>) -> AnyElement {
    let current = state.prefs.theme;
    // 「跟随系统」要显示系统当前是什么主题 —— 用的是启动时读到的缓存值
    // （点按钮时会在后台再读一次，见 `Shell::set_theme`）。
    let system_label = match state.system_is_dark {
        Some(true) => "系统当前：深色",
        Some(false) => "系统当前：浅色",
        None => "系统当前：没读到（按深色算）",
    };

    // 互斥选项交给官方 `RadioGroup`：它对读屏软件是"一组单选"、自带方向键移动
    // + 空格选中（原先那排"选中就变主色"的按钮，这两样都没有）。
    let labels: Vec<SharedString> = crate::prefs::ThemePref::ALL
        .iter()
        .map(|theme| SharedString::from(theme.label()))
        .collect();
    let selected = crate::prefs::ThemePref::ALL
        .iter()
        .position(|theme| *theme == current);
    let picker = radio_row(
        "theme-picker",
        labels,
        selected,
        entity,
        |shell, ix, _, cx| {
            shell.set_theme(crate::prefs::ThemePref::ALL[ix], cx);
        },
    );
    card(
        "界面",
        v_flex()
            .w_full()
            .gap_3()
            .child(kv("主题", current.label()))
            .child(kv("当前配色", theme::mode().label()))
            .child(picker)
            .child(div().text_xs().text_color(theme::text_dim()).child(format!(
                "主题同时作用于自绘的界面和组件库的控件（输入框、按钮……）。{system_label}。"
            )))
            .child(div().text_xs().text_color(theme::text_dim()).child(
                "「跟随系统」读的是 Windows 的应用主题（AppsUseLightTheme 那个键，\
                 不是管任务栏的 SystemUsesLightTheme）；在系统里改完主题之后，\
                 再点一下「跟随系统」（或重启面板）就会跟上。",
            )),
    )
    .into_any_element()
}

// ---------------------------------------------------------------------------
// ⑥ wlsc 配置
// ---------------------------------------------------------------------------

/// "刷新"卡片：**应用自己的偏好**，与 `wslc` 的配置无关。
///
/// 界面上不再到处显示刷新间隔 —— 只在这一处设置。
///
/// 卡片标题刻意叫「刷新」而不是「界面」：主题卡也叫「界面」，
/// 两张同名卡片挨在一起会让人分不清哪个是哪个。
pub(crate) fn interface_card(state: &AppState, entity: &Entity<Shell>) -> AnyElement {
    let path_text = crate::prefs::Prefs::path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "（无法确定偏好文件位置）".to_owned());

    card(
        "刷新",
        v_flex()
            .w_full()
            .gap_3()
            .child(kv(
                "自动刷新间隔",
                format!("{} 秒", state.prefs.refresh_secs),
            ))
            .child(refresh_secs_picker(state, entity))
            .child(
                div()
                    .text_xs()
                    .text_color(theme::text_dim())
                    .child(format!("立即生效，并写入 {path_text}")),
            ),
    )
    .into_any_element()
}

/// 自动刷新间隔选择器（只出现在「应用设置」页）。
///
/// 这是**应用自己的偏好**，不是 `wslc` 的配置 —— 所以它和上面那些
/// `settings.yaml` 的条目在视觉上分开，用的是本地 `prefs` 而不是 `SettingsDoc`。
pub(crate) fn refresh_secs_picker(state: &AppState, entity: &Entity<Shell>) -> impl IntoElement {
    let current = state.prefs.refresh_secs;

    // 互斥选项 → 官方 `RadioGroup`（理由同主题卡）。
    let labels: Vec<SharedString> = crate::prefs::REFRESH_PRESETS
        .iter()
        .map(|secs| SharedString::from(format!("{secs} 秒")))
        .collect();
    let selected = crate::prefs::REFRESH_PRESETS
        .iter()
        .position(|secs| *secs == current);
    let picker = radio_row(
        "refresh-picker",
        labels,
        selected,
        entity,
        |shell, ix, _, cx| {
            shell.set_refresh_secs(crate::prefs::REFRESH_PRESETS[ix], cx);
        },
    );
    // 手动改过 prefs.json、值不在预设里时，补一个只读提示，
    // 免得用户看到"一个都没选中"而困惑。
    let extra = if crate::prefs::REFRESH_PRESETS.contains(&current) {
        None
    } else {
        Some(
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child(format!("当前：{current} 秒（不在预设中）"))
                .into_any_element(),
        )
    };

    h_flex()
        .w_full()
        .gap_2()
        .flex_wrap()
        .child(picker)
        .children(extra)
}

/// 「终端」卡：新终端的起始目录。
///
/// # 为什么有这一项
///
/// 不给 `--cd` 时 `wsl.exe` 会继承**调用它的那个进程**的工作目录，WSL 再把它
/// 翻译成 `/mnt/<盘>/...` —— 于是"打开终端"落在哪儿，取决于用户是从哪个文件夹
/// 启动的 `wslc-panel.exe`（本机实测：从 `C:\Users\76434\Downloads` 启动，
/// 终端就落在 `/mnt/c/Users/76434/Downloads`；路径翻译不了时回落 `~`）。
/// 默认因此改成显式的 `--cd ~`，这一卡让用户改。
///
/// # 输入框是懒创建的
///
/// 点过「自定义路径…」才会建（`InputState::new` 要 `&mut Window`，
/// 见 [`Shell::edit_terminal_dir`]）—— 渲染时现建会让用户一个字都打不进去。
pub(crate) fn terminal_card(shell: &Shell, entity: &Entity<Shell>) -> AnyElement {
    let current = &shell.state.prefs.terminal_dir;

    // 三个档位都是**互斥选项**（`Home` / `Panel` / 自定义），所以整组交给官方
    // `RadioGroup`；第三项被选中时顺带把输入框显示出来（`edit_terminal_dir`）。
    //
    // ⚠️ 「自定义」这一项有**副作用**（建 `InputState`），所以它不能是"只是选中"——
    // 这也是一处官方组件表达不了的地方：`RadioGroup` 只回报下标，副作用由
    // `pick` 自己按分支做。
    let labels: Vec<SharedString> = crate::prefs::TerminalStartDir::FIXED
        .iter()
        .map(|dir| SharedString::from(dir.label()))
        .chain(std::iter::once(SharedString::from("自定义路径…")))
        .collect();
    let selected = match current {
        crate::prefs::TerminalStartDir::Custom(_) => {
            Some(crate::prefs::TerminalStartDir::FIXED.len())
        }
        other => crate::prefs::TerminalStartDir::FIXED
            .iter()
            .position(|dir| dir == other),
    };
    let picker = radio_row(
        "terminal-dir-picker",
        labels,
        selected,
        entity,
        |shell, ix, window, cx| {
            match crate::prefs::TerminalStartDir::FIXED.get(ix) {
                Some(dir) => shell.set_terminal_dir(dir.clone(), cx),
                // 下标越界 = 最后那一项「自定义路径…」
                None => shell.edit_terminal_dir(window, cx),
            }
        },
    );
    let mut body = v_flex()
        .w_full()
        .gap_3()
        .child(kv("新终端的起始目录", current.describe()))
        .child(picker)
        .child(div().text_xs().text_color(theme::text_dim()).child(
            // 这一句里有 `wsl --cd` 的反引号 —— 同一张卡下面两段都过了
            // `strip_md`，只有这一段漏了（注释和实现不一致时后来人会照错的那份走）。
            crate::util::strip_md(
                "「打开终端」（实例列表）用 `wsl --cd` 指定起点；不给就是继承面板自己的工作目录，\
                 也就是从哪个文件夹启动的面板，终端就从哪儿开始。",
            ),
        ));

    // 输入框只在点过「自定义路径…」之后存在。它一直留着（不随页面离开销毁），
    // 所以这里只要 `Some` 就画。
    if let Some(input) = shell.terminal_dir_input.as_ref() {
        let apply = {
            let entity = entity.clone();
            Button::new("terminal-dir-apply")
                .label("应用")
                .small()
                .primary()
                .on_click(move |_, _, cx| {
                    entity.update(cx, |shell, cx| shell.apply_terminal_dir(cx));
                })
                .into_any_element()
        };

        body = body
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .child(Input::new(input).id("terminal-dir").w(px(360.)))
                    .child(apply),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme::text_dim())
                    .child(crate::util::strip_md(
                        "填发行版里的路径（`/home/wt/project` 或 `~`）—— Windows 路径不认，\
                     那是「面板所在目录」那一档的事。写错的路径 `wsl.exe` 不报错，只会落在 `/`。",
                    )),
            );
    }

    body = body.child(
        div()
            .text_xs()
            .text_color(theme::text_dim())
            .child(crate::util::strip_md(
                "容器的「打开终端」走 `wslc exec -w`，只对**绝对路径**生效：容器里没有\
             「Linux home」这个概念，`~` 交给镜像自己的工作目录决定。改动只影响之后新开的终端。",
            )),
    );

    card("终端", body).into_any_element()
}
