//! `impl Render for Shell` —— 整个窗口的渲染体。
//!
//! 它是这一摊里**唯一必须认识所有页面和所有浮层**的地方：
//! `views::xxx_overlay()` 的调用点、以及「后画的压在上面」那条层序，
//! 全在同一条 `.child(…)` 链子上。所以它单独一个文件，也刻意不拆。

use super::super::prelude::*;
use super::Shell;
// 通用二次确认已迁到官方 `Dialog`（`DialogKey::Confirm`），内容在 `core::confirm_body`。

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 先把「窗口级对话框栈」对齐到当前状态（官方 `Dialog`）。
        //
        // 必须在借用 `&self.state` 之前调：它要 `&mut self`。
        // 只在状态**真的变了**时才动窗口（函数第一行那个 return），
        // 所以每帧调是安全的。
        self.sync_dialog_stack(window, cx);

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
            Some(at) => crate::util::refresh_age_label(at.elapsed().as_secs()),
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
                            .id("error-list")
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

        // 提示条：主内容列里的一个**布局项**，不是浮层。
        //
        // 原先它是挂在根元素上的 `.absolute().bottom(20).left(0).right(0)`，
        // 横跨整个窗口宽度。v0.5.10 把它从右下角挪到底部居中，理由是右下角
        // 正好压着「开始安装」；但居中之后它变成压住**底部中间**的东西了 ——
        // 本机实测过，底部那一带的按钮会被它盖住。
        //
        // 浮层方案无论放哪儿都必然压住**某个**控件（这正是 v0.5.10 那次
        // 修复的成因），所以这里改成布局项：提示条出现时主内容区让出一条
        // 高度，谁都不会被盖住，也不需要再挑位置。
        // 代价是它出现 / 消失时内容会上下动一下 —— 比"有按钮点不到"好。
        let toast: AnyElement = match &state.toast {
            None => div().into_any_element(),
            Some(t) => {
                let entity = entity.clone();
                let color = match t.kind {
                    ToastKind::Info => theme::primary(),
                    ToastKind::Success => theme::success(),
                    ToastKind::Error => theme::danger(),
                };
                h_flex()
                    .w_full()
                    // `flex_none`：别让它被上面那个 `flex_1` 的滚动区挤扁。
                    .flex_none()
                    .justify_center()
                    .px_5()
                    .pb_4()
                    .child(
                        h_flex()
                            .max_w(px(560.))
                            .gap_3()
                            .px_4()
                            .py_3()
                            .rounded_md()
                            .bg(theme::bg_card())
                            .border_1()
                            .border_color(color)
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(color)
                                    .overflow_hidden()
                                    // 见 `util::strip_md`：提示条里也有写
                                    // `**不完整**` 这类标记的文案。
                                    .child(crate::util::strip_md(&t.text)),
                            )
                            .child(Button::new("toast-dismiss").label("关闭").small().on_click(
                                move |_, _, cx| {
                                    entity.update(cx, |shell, cx| shell.dismiss_toast(cx));
                                },
                            )),
                    )
                    .into_any_element()
            }
        };

        // NOTE: 通用二次确认走官方 Dialog 栈（DialogKey::Confirm）——
        // 它能从**任何**浮层里弹出来，窗口级栈保证它一定盖在最上面。

        // 「拉取镜像」弹窗。表单住在 `Shell::image_form` 里（不在 `state` 里），
        // 所以只有这里能画它。
        let pull_dialog: AnyElement = match (self.pull_open, self.image_form.as_ref()) {
            (true, Some(form)) => views::pull_dialog_overlay(form, state, &entity, cx),
            // `pull_open` 为真时表单一定在（`open_pull_dialog` 先建再置位）——
            // 真走到这里也只是什么都不画，不 panic。
            _ => div().into_any_element(),
        };

        // NOTE: 创建容器弹窗走官方 Dialog 栈（DialogKey::CreateContainer）。

        // NOTE: 容器详情 / 发行版详情都走官方 Dialog 栈
        // （`DialogKey::ContainerDetail` / `DistroDetail`，由 `sync_dialog_stack` 对齐）。
        // 它们的内容分别是 `views::container_detail_body` / `distro_detail_body`。

        // ⚠️ 「单输入框提示」（移动位置 / 调整大小 / 设置默认用户 / 导出 / 创建卷）
        // **不在这条链子上**：它已迁到官方 `Dialog`（窗口级层栈），
        // 由 `Shell::open_prompt` 里的 `window.open_dialog` 打开；
        // 内容由 `views::prompt_dialog_content` 提供。
        //
        // 迁它的理由不只是"用官方组件"：窗口级层栈让**层序 = 栈序**，
        // 不再依赖这条 `.child()` 链子的先后（见本文件末尾那段长注释）。

        // NOTE: export progress 走官方 Dialog 栈，见 sync_dialog_stack。

        // 发行版配置（/etc/wsl.conf）弹窗。要 `cx` 才能读输入框的值。
        let wslconf_dialog: AnyElement = views::wslconf_overlay(self, &entity, cx);

        // NOTE: 容器日志走官方 Dialog 栈（DialogKey::ContainerLogs）。

        // ⚠️ 「容器 exec」不在这条链子上 —— 它走官方 `Dialog` 的窗口级层栈
        // （`Shell::open_exec` 里的 `window.open_dialog`，内容是
        // `views::exec_dialog_body` 包在 `ShellView` 里）。

        // NOTE: 数据库动作弹窗走官方 Dialog 栈（DialogKey::DbAction），
        // 门槛（db_form 建好 + db_dialog 有值）在 sync_dialog_stack 里。

        // 「基础信息」（连接卡）—— 只有浮层开着、且确实在数据库页时才画。
        let db_info: AnyElement = match &state.db {
            Some(db) if self.db_info_open => match self.db_form.as_ref() {
                Some(form) => views::db_info_overlay(db, form, &entity, cx),
                None => div().into_any_element(),
            },
            _ => div().into_any_element(),
        };

        // ⚠️ v0.12 起 SQL 控制台是**整页**（`Page::Sql`），不再是浮层 ——
        // 所以这里没有它的位置了。浮层那一版（`db_console_overlay`）已删：
        // 它盖住页面、看着像弹窗，而且塞不下"树 + 编辑器 + 表格"三块。

        // 备份进度（跑着才有内容）与备份列表（点「备份」打开）。
        // NOTE: 备份进度走官方 Dialog 栈（DialogKey::DbBackupProgress）。
        // NOTE: 备份列表走官方 Dialog 栈（DialogKey::DbBackupList），
        // 门槛（db_form 建好 + 点了「备份」）在 sync_dialog_stack 里。
        // 导入进度（跑着才有内容）。
        // NOTE: 导入进度走官方 Dialog 栈（DialogKey::DbImportProgress）。

        // 部署页（v0.9）的三个浮层：
        // - 「添加部署 / 重新部署」要 `cx`（读那五格输入框的值）；
        // - 日志与删除确认都是**纯数据**（没有输入框），所以只要 `&Shell`。
        let deploy_dialog: AnyElement = views::deploy_dialog_overlay(self, &entity, cx);
        // NOTE: 部署日志 / 站点配置 / 删除确认都走官方 Dialog 栈
        // （DialogKey::DeployLogs / DeploySiteConf / DeployRemoveConfirm）。

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
                            )
                            // 提示条放在主内容列**最后**：它是一条贴底的布局项，
                            // 出现时把上面那个 `flex_1` 的滚动区挤矮一点，
                            // 而不是盖在它上面（见 `toast` 的说明）。
                            .child(toast),
                    ),
            )
            // 拉取弹窗排在创建弹窗**之前**：两者不会同时开着
            // （`use_pulled_image` 会先关掉拉取弹窗），但万一同时开着，
            // 后画的创建表单应该压在上面 —— 用户刚点的是它。
            .child(pull_dialog)
            // ⚠️ 「单输入框提示」不在这条链子上 —— 它走官方 `Dialog` 的窗口级层栈
            // （见上面 `Shell::open_prompt` 那段的说明）。
            .child(wslconf_dialog)
            // 日志和 exec 都可能是从容器详情里打开的，所以排在它之后。
            // 部署页的三个浮层：它们只在「部署管理」页上出现，
            // 不需要盖住数据库 / 备份那几层（那两组不会同时开着），
            // 所以插在数据库弹窗**之前**。
            .child(deploy_dialog)
            // 删除确认排在日志之后：它是最后点的那个动作，应该在最上面
            //（日志浮层与它可能同时开着 —— 用户看着日志觉得该删了）。
            // 数据库页的动作弹窗排在最后：它可能盖在任一页（含容器详情）上。
            // 「基础信息」排在动作弹窗**之后**（同时开着时它在上面 —— 但两者
            // 互斥：动作弹窗要按底下的按钮才会开，而浮层挡着那些按钮）。
            .child(db_info)
        // ⚠️ v0.12：SQL 控制台从浮层改成了**整页**（`Page::Sql`），
        // 所以这条链子上没有它的位置了。
        //
        // 留一条**踩过的坑**记在这里，因为这个坑很容易再踩：
        // v0.9.2 把它做成浮层时，只写了 `let db_console = …` 却没把它
        // `.child(…)` 进这棵树 —— 算出来的 `AnyElement` 当场被丢掉，
        // 于是点行上的「SQL」只有一条 toast、**界面永远不出现**
        //（实机原话："这个 sql 按钮点击也没弹出控制台输入 sql 的界面来"）。
        //
        // 教训：**这棵树里每算一个 `AnyElement`，都必须确认它真的被挂上去了。**
        // 漏挂不会编译失败、也不会运行时报错，只是"点了没反应"。
        // ℹ️ 通用二次确认（`DialogKey::Confirm`）**已经不在这条链子上**：
        // 它走官方 `Dialog` 的窗口级栈，层序 = 栈序 —— 从**任何**浮层里
        // 弹出来都一定盖在最上面。
        //
        // 这正是 v0.9.14 那条老坑的根治：当时它挂在这条链子的最前面，
        // 于是从详情里点「压缩磁盘」时，确认框连着遮罩一起被详情盖住 ——
        // 既看不到「确认」，也挡不住背后的按钮（实机原话："压缩磁盘的
        // 对话确认在详情之下"）。手写链子只能靠"记得排后面"，栈不会忘。
        //
        // ⚠️ 仍在链子上的浮层（`pull_dialog` / `create_dialog` / `deploy_dialog`
        // / `wslconf_dialog` / `db_dialog` 等）**还是按 `.child()` 的先后定层序**，
        // 加新浮层时仍要问那句：**它能不能从另一个浮层里被打开？**
    }
}
// ---------------------------------------------------------------------------
// 库 / 表树：节点 id 的编码与解码
// ---------------------------------------------------------------------------
