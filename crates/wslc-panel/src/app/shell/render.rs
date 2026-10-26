//! `impl Render for Shell` —— 整个窗口的渲染体。
//!
//! 它是这一摊里**唯一必须认识所有页面和所有浮层**的地方：
//! `views::xxx_overlay()` 的调用点、以及「后画的压在上面」那条层序，
//! 全在同一条 `.child(…)` 链子上。所以它单独一个文件，也刻意不拆。

use super::super::prelude::*;
use super::Shell;
// 提示条交给官方 `Notification`（`Root` 之上的 toast 层，见 `sync_dialog_stack` 旁边
// 那段交接）。`WindowExt` 提供 `push_notification` / `remove_notification`。
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::{Notification, NotificationType};

/// 提示条的 `Notification` id 标记。
///
/// 所有提示条共用一个 id —— `remove_notification::<ToastNote>` 于是能精确
/// 删掉"上一条提示"，把 `AppState::toast` 那个单槽语义原样保住
/// （`deploy/submit.rs` 依赖"后弹的盖掉先弹的"）。
struct ToastNote;

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 先把「窗口级对话框栈」对齐到当前状态（官方 `Dialog`）。
        //
        // 必须在借用 `&self.state` 之前调：它要 `&mut self`。
        // 只在状态**真的变了**时才动窗口（函数第一行那个 return），
        // 所以每帧调是安全的。
        self.sync_dialog_stack(window, cx);

        // 提示条：交给官方 `Notification`（窗口右上角的 toast 层，由 `Root` 渲染）。
        //
        // # 为什么不再自绘
        //
        // v0.20.1 及以前这里是一条**布局项**（主内容列最后一条 `.child(toast)`），
        // 理由是"浮层无论放哪儿都会压住某个控件"（v0.5.10 实机踩过提示条盖住
        // 「开始安装」）。官方 `Notification` 默认在右上角、可配 8 个 `Anchor`、
        // 悬停会暂停倒计时、自带关闭按钮 —— 这些收益值得换；位置偏好见
        // `docs/AUDIT-2026-10-26.md` §5.2（要在实机上挑一个不挡控件的 `Anchor`）。
        //
        // # 为什么先 remove 再 push
        //
        // 原来 `AppState::toast` 是**单槽**：后弹的盖掉先弹的。官方通知默认是
        // **堆叠**的，不 remove 就会同时看到"正在部署…"和"部署成功"两条。
        // `deploy/submit.rs` 里有一条注释明确依赖"后弹的盖掉先弹的"，所以
        // 这里用固定 id + `remove_notification` 把这个语义原样保住。
        if let Some(t) = self.state.take_toast() {
            window.remove_notification::<ToastNote>(cx);
            window.push_notification(
                Notification::new()
                    .id::<ToastNote>()
                    // 文案是 Markdown 风格写的，而通知画的是纯文本
                    // （AGENTS.md §5：渲染处清标记）。
                    .message(wslc_panel_core::util::strip_md(&t.text))
                    .with_type(match t.kind {
                        ToastKind::Info => NotificationType::Info,
                        ToastKind::Success => NotificationType::Success,
                        ToastKind::Error => NotificationType::Error,
                    }),
                cx,
            );
        }

        let entity = cx.entity();
        let state = &self.state;

        // 导航按 `Page::group()` 分组 —— 每组合成一个 `SidebarGroup`。
        //
        // v0.12.2 起用组件库的 `Sidebar`（见下方渲染处），所以这里是
        // `SidebarGroup` / `SidebarMenuItem` 而不是自己画的 `div`：
        // 分组标题、缩进、悬停态、选中态都由组件负责，`nav_item` 那个
        // 手写 hover/active 的版本已经不需要了。
        // ⚠️ 类型必须写全 `SidebarGroup<SidebarMenuItem>`：`SidebarGroup` 是
        // 泛型（`E: SidebarItem`），只写 `SidebarGroup` 会报 "missing generics"。
        let mut groups: Vec<SidebarGroup<SidebarMenuItem>> = Vec::new();
        let mut last_group = "";
        for page in Page::ALL {
            if page.group() != last_group {
                groups.push(SidebarGroup::new(page.group()));
                last_group = page.group();
            }
            let item_entity = entity.clone();
            let item = SidebarMenuItem::new(page.label())
                .active(state.page.highlights_nav(page))
                .on_click(move |_, window, cx| {
                    // `on_click` 给的是 `&mut App`（不是 `Context<Self>`），
                    // 所以走 `entity.update` 拿 `Context`。
                    //
                    // ⚠️ `window` 是必需的：「添加实例」页要现场创建输入框，
                    // 而 `InputState::new` 需要 `&mut Window`。
                    item_entity.update(cx, |shell, cx| shell.set_page(page, window, cx));
                });
            // 填进**最后一组**（`SidebarGroup` 不实现 Clone，所以取出来再放回）。
            if let Some(g) = groups.pop() {
                groups.push(g.child(item));
            }
        }

        // 副标题回答「数据有多新」，不再放会话 / 构建号 —— 那两样是低频
        // 诊断信息，已经挪到「应用设置 → 关于」。
        let refreshed = match self.last_refresh {
            Some(at) => wslc_panel_core::util::refresh_age_label(at.elapsed().as_secs()),
            None => "还没刷新".to_owned(),
        };
        let subtitle = format!(
            "最后刷新：{refreshed} · 自动刷新 {} 秒",
            state.prefs.refresh_secs
        );

        // 按钮只在**用户发起**的采集期间显示「刷新中…」。
        // 3 秒一次的自动刷新是后台行为：它照样跑，但按钮一直是「刷新」，
        // 用户随时可以点它手动刷新（见 `refresh_quiet` / `refresh_inner`）。
        let user_refreshing = state.busy && self.refresh_visible;

        // 侧边栏折叠按钮（v0.12.2）。
        //
        // ⚠️ 它必须放在**主区域**里，不能放进侧边栏自己：侧边栏收起之后
        // 那一块完全不渲染（`Offcanvas`），按钮跟着消失就再也点不回来了。
        let sidebar_toggle = {
            let entity = entity.clone();
            let collapsed = self.sidebar_collapsed;
            SidebarToggleButton::new()
                .collapsed(collapsed)
                .on_click(move |_, _, cx| {
                    entity.update(cx, |shell, cx| {
                        shell.sidebar_collapsed = !shell.sidebar_collapsed;
                        cx.notify();
                    });
                })
        };

        let refresh_button = {
            let entity = entity.clone();
            let label = refresh_label(state.busy, self.refresh_visible);
            // 刻意**不**用 `.disabled(...)`：禁用态的文字几乎看不清
            // （实机截图确认过）。`refresh()` 内部本来就有 `busy` 守卫，
            // 重复点击是无害的，文案也会变成"刷新中…"。
            Button::new("refresh")
                .label(label)
                .primary()
                .on_click(move |_, _, cx| {
                    entity.update(cx, |shell, cx| shell.refresh(cx));
                })
        };

        // 错误横幅：1 条直显；多条默认折叠成「第一条 + 还有 N 项」。
        //
        // 之前把**每个区段**的错误逐条铺出来，`wslc` 一时读不到时能红七行，
        // 把整页内容推下去 —— 用户第一眼看到的是七行报错，而不是"哪些数据还在"。
        let errors = &state.snapshot.errors;
        let error_banner: AnyElement = if errors.is_empty() {
            if !state.snapshot.has_data() {
                // 首屏 / 连不上 wslc 时的提示。比一片空白有用得多。
                //
                // 用 `user_refreshing` 而不是 `state.busy`：连不上 wslc 时
                // 后台每 3 秒就会空跑一轮，用 `busy` 的话这两句话会一直闪。
                let hint = if user_refreshing {
                    "正在读取 wslc 数据…"
                } else {
                    "没有读到任何数据。请确认已安装 WSL 3.0 以上版本；\
                     若 wslc.exe 不在默认路径，请设置环境变量 WSLC_PATH 指向它。"
                };
                div()
                    .w_full()
                    .text_sm()
                    .text_color(theme::text_dim())
                    .child(hint)
                    .into_any_element()
            } else {
                div().into_any_element()
            }
        } else {
            let count = errors.len();
            let expanded = self.errors_expanded;
            let toggle = |label: &'static str| {
                let entity = entity.clone();
                Button::new("errors-toggle")
                    .label(label)
                    .small()
                    .on_click(move |_, _, cx| {
                        entity.update(cx, |shell, cx| {
                            shell.errors_expanded = !shell.errors_expanded;
                            cx.notify();
                        });
                    })
            };

            let mut body = v_flex().w_full().gap_2();
            if count == 1 {
                body = body.child(
                    div()
                        .text_xs()
                        .text_color(theme::danger())
                        .child(errors[0].clone()),
                );
            } else if expanded {
                // 滚动容器必须先有 id（`overflow_y_scroll` 只对带 id 的元素可用）。
                body =
                    body.child(
                        v_flex()
                            .id("app-errors")
                            .w_full()
                            .max_h(px(160.))
                            .overflow_y_scroll()
                            .gap_1()
                            .children(errors.iter().map(|e| {
                                div().text_xs().text_color(theme::danger()).child(e.clone())
                            })),
                    );
                body = body.child(h_flex().w_full().justify_end().child(toggle("收起")));
            } else {
                body = body.child(
                    h_flex()
                        .w_full()
                        .justify_between()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .min_w_0()
                                .text_xs()
                                .text_color(theme::danger())
                                .overflow_hidden()
                                .truncate()
                                .child(errors[0].clone()),
                        )
                        .child(
                            h_flex()
                                .flex_none()
                                .gap_2()
                                .items_center()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme::text_dim())
                                        .child(format!("还有 {} 项", count - 1)),
                                )
                                .child(toggle("查看")),
                        ),
                );
            }

            v_flex()
                .w_full()
                .p_2()
                .rounded_md()
                .bg(theme::danger_soft())
                .border_1()
                .border_color(theme::danger_edge())
                .child(body)
                .into_any_element()
        };

        // 浮层全部在**官方 `Dialog` 的窗口级栈**里（`app::shell::dialogs` 的
        // `sync_dialog_stack` / `open_dialog_for`），不在这条 `.child()` 链子上。
        //
        // 这条链子原来挂着 19 个自绘浮层（`modal_scrim` + `modal_card`），层序靠
        // "谁写在后面"人工维持 —— v0.9.14 的"压缩磁盘的确认框在详情之下"就是
        // 那么来的。现在**层序 = 栈序**，链子上只剩页面本体和提示条。
        //
        // ⚠️ 加新浮层：在 `dialogs.rs` 里加一个 `DialogKey` 变体 +
        // `sync_dialog_stack` 里一个 `if` + `open_dialog_for` 里一个臂，
        // **不要**往这条链子上加。

        // 页面渲染要 `&Shell`（不只是 `&AppState`）——「添加实例」页有输入框，
        // 而输入框的 `InputState` 住在 `Shell` 里。
        // `cx` 也只有那一页用得上（要实时读输入框的值做命令预览）。
        //
        // 切页时淡入一下（120ms）：`with_animation` 的 id 用**页面标签** ——
        // 换页就换 id，于是重新播一次；同一页的后续重绘 id 不变，不会重播
        // （GPUI 按 id 记住播放进度，一次性动画结束后保持终值）。
        //
        // ⚠️ 动画得挂在 `div()` 上而不是 `AnyElement` 上：`AnyElement` 不实现
        // `Styled`（它是类型擦除后的元素），`.opacity()` 在那儿不存在。
        // 外面那层 `v_flex().w_full()` 负责把"整页宽"传下去 —— 动画包装层
        // 自己不带宽度，而页面内容大量用 `w_full`。
        // ⚠️ 「SQL 控制台」那一页要**撑满高度**（它自己分上下两块、各自滚动），
        // 其余页面是"内容多高就多高，外层滚动"。所以这两支不能合并：
        // 给普通页面加 `h_full` 会让短页面被拉满、长页面被压扁。
        let page_body: AnyElement = if state.page == Page::Sql {
            v_flex()
                .w_full()
                .h_full()
                .min_h_0()
                .child(
                    div()
                        .w_full()
                        .h_full()
                        .min_h_0()
                        .child(views::page(self, cx, &entity))
                        .with_animation(
                            state.page.label(),
                            Animation::new(Duration::from_millis(120)),
                            |el, delta| el.opacity(delta),
                        ),
                )
                .into_any_element()
        } else {
            v_flex()
                .w_full()
                .child(
                    div()
                        .w_full()
                        .child(views::page(self, cx, &entity))
                        .with_animation(
                            state.page.label(),
                            Animation::new(Duration::from_millis(120)),
                            |el, delta| el.opacity(delta),
                        ),
                )
                .into_any_element()
        };

        div()
            .relative()
            .size_full()
            .bg(theme::bg())
            .text_color(theme::text())
            .child(
                div()
                    .flex()
                    .flex_row()
                    .size_full()
                    // 侧边栏
                    // 侧边栏（v0.12.2 换成组件库的 `Sidebar`）。
                    //
                    // `SidebarCollapsible::Offcanvas`：收起时它**完全退出布局**，
                    // 主区域拿到全部宽度 —— 这就是用户要的"左边菜单能折叠"。
                    //
                    // 没用 `Icon` 模式（收成一条图标轨）：那个模式要靠图标才看得懂，
                    // 而我们这 7 项一直是纯文字；硬配图标得先确认每个 `IconName`
                    // 变体都存在，猜错就是编译错误（本地编不了，只能等 CI）。
                    .child(
                        Sidebar::new("app-sidebar")
                            .collapsible(SidebarCollapsible::Offcanvas)
                            .collapsed(self.sidebar_collapsed)
                            .w(px(216.))
                            .header(
                                SidebarHeader::new().child(
                                    h_flex()
                                        .w_full()
                                        .gap_2()
                                        .items_center()
                                        .child(
                                            div()
                                                .text_lg()
                                                .font_bold()
                                                .text_color(theme::primary())
                                                .child("wslc"),
                                        )
                                        .child(
                                            div()
                                                .text_sm()
                                                .text_color(theme::text_dim())
                                                .child("panel"),
                                        ),
                                ),
                            )
                            .children(groups),
                    )
                    // 主区域
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .overflow_hidden()
                            // SQL 页**不画应用级页头**。
                            //
                            // 它自带一整条工具栏（返回 / 库名 / 执行 …），再叠一层
                            // "SQL 控制台 + 最后刷新 + 自动刷新 30 秒 + 刷新按钮"
                            // 就是纯重复的 chrome —— 实测截图里顶上有四个按钮外加
                            // 两行标题，专业的 SQL 客户端（dbx / DBeaver）都不是这个样子。
                            //
                            // ⚠️ 折叠侧边栏的按钮因此**搬到 SQL 页自己的工具栏**里了：
                            // 侧边栏一收起那块就不渲染，按钮要是留在这条被隐藏的页头里，
                            // 就再也点不回来了。
                            .child(if state.page == Page::Sql {
                                div().into_any_element()
                            } else {
                                h_flex()
                                    .w_full()
                                    .flex_none()
                                    .justify_between()
                                    .px_6()
                                    .py_4()
                                    .border_b_1()
                                    .border_color(theme::border())
                                    .child(
                                        // 折叠按钮 + 标题绑在一起，否则
                                        // `justify_between` 会把标题推到中间
                                        // （三个孩子时它是"两边对齐 + 中间均分"）。
                                        h_flex()
                                            .gap_3()
                                            .items_center()
                                            .child(sidebar_toggle)
                                            .child(
                                                v_flex()
                                                    .gap_1()
                                                    .child(
                                                        div()
                                                            .text_lg()
                                                            .font_bold()
                                                            .child(state.page.label()),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(theme::text_dim())
                                                            .child(subtitle),
                                                    ),
                                            ),
                                    )
                                    .child(refresh_button)
                                    .into_any_element()
                            })
                            .child(
                                // ⚠️ 「SQL 控制台」是**整页工作区**，外层不能给它
                                // `p_5` 和 `overflow_y_scroll`：
                                //
                                // - `p_5` 会把左侧那棵树从内容区左边框推开 20px，
                                //   而用户明确要"左边的列表树左对齐"；
                                // - 外层滚动 + 内层滚动（树自己滚、表格自己虚拟滚）
                                //   会互相打架，而且 `DataTable` 拿不到确定高度时
                                //   算错可见行范围（表现为滚动条乱跳 / 底部空白）。
                                //
                                // 所以这一页自己管布局和滚动，外层只做容器。
                                //
                                // `overflow_y_scroll` 来自 `StatefulInteractiveElement`，
                                // 只对**带 id 的**元素可用（这正是它叫 "stateful" 的原因）。
                                // 不带 `.id()` 会报 "no method named overflow_y_scroll"。
                                if state.page == Page::Sql {
                                    v_flex()
                                        .id("app-body-sql")
                                        .w_full()
                                        .flex_1()
                                        .min_h_0()
                                        .overflow_hidden()
                                        .child(error_banner)
                                        .child(page_body)
                                        .into_any_element()
                                } else {
                                    v_flex()
                                        .id("app-body-scroll")
                                        .w_full()
                                        .flex_1()
                                        .min_h_0()
                                        .gap_4()
                                        .p_5()
                                        .overflow_y_scroll()
                                        .child(error_banner)
                                        .child(page_body)
                                        .into_any_element()
                                },
                            ), // 提示条放在主内容列**最后**：它是一条贴底的布局项，
                               // 出现时把上面那个 `flex_1` 的滚动区挤矮一点，
                               // 而不是盖在它上面（见 `toast` 的说明）。
                    ),
            )
    }
}
// ---------------------------------------------------------------------------
// 库 / 表树：节点 id 的编码与解码
// ---------------------------------------------------------------------------
