//! 应用外壳：窗口内容、左侧导航、异步刷新调度、危险操作确认。
//!
//! # 关于异步
//!
//! 所有 `wslc` 调用都是阻塞的（子进程 + 管道读取），**绝不能**跑在 UI 线程上。
//! 这里用 GPUI 的标准模式：
//!
//! ```text
//! cx.spawn(async move |this, cx| {
//!     let data = cx.background_executor().spawn(async move { 阻塞采集() }).await;
//!     this.update(cx, |state, cx| { 写回状态; cx.notify(); });
//! })
//! ```
//!
//! 这是整个项目里**唯一**接触 GPUI 异步 API 的地方，
//! 因此如果上游 API 有变动，只需要改这一个文件。

use std::time::Duration;

// 注意：`primary()` / `danger()` 这些样式方法来自 trait `ButtonVariants`，
// 光导入 `Button` 是不够的 —— 这里用 glob 把 button 模块全带上。
use gpui_kit::component::button::*;
// `StyledExt` 提供 `font_bold` / `font_semibold` 等字重方法（由宏生成），
// 不导入 trait 就会报 "no method named font_bold"。
use gpui_kit::component::{Disableable, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::*;

use wslc_core::Wslc;
use wslc_core::settings::SettingKey;

use crate::state::{self, AppState, Page, PendingAction, RefreshInterval, Toast, ToastKind};
use crate::theme;
use crate::views;

/// 应用外壳。
pub struct Shell {
    /// 全部状态。
    pub state: AppState,
}

impl Shell {
    /// 创建外壳：立刻触发一次采集、加载配置、启动自动刷新。
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut shell = Self {
            state: AppState::new(Wslc::new()),
        };
        shell.refresh(cx);
        shell.start_auto_refresh(cx);
        shell
    }

    // -- 数据刷新 ----------------------------------------------------------

    /// 后台采集一次完整快照。
    ///
    /// 重复调用会被忽略（`busy` 保护），避免自动刷新和手动刷新叠加。
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.state.busy {
            return;
        }
        self.state.busy = true;
        cx.notify();

        let wslc = self.state.wslc.clone();
        cx.spawn(async move |this, cx| {
            let snapshot = cx
                .background_executor()
                .spawn(async move { state::load_snapshot(&wslc) })
                .await;

            let _ = this.update(cx, |shell, cx| {
                shell.state.busy = false;
                shell.state.snapshot = snapshot;
                // 首次拿到 `wslc info` 之后才能确定 settings.yaml 的真实位置。
                // 只加载一次，避免把用户没保存的编辑覆盖掉。
                if shell.state.settings.is_none() {
                    shell.load_settings(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 按固定间隔自动刷新；间隔由 `state.interval` 决定，暂停时不刷新。
    fn start_auto_refresh(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| loop {
            let interval = match this.update(cx, |shell, _| shell.state.interval) {
                Ok(interval) => interval,
                // 实体已销毁 → 退出循环，避免泄漏。
                Err(_) => break,
            };

            // 暂停时也要周期性醒来，才能感知"用户把自动刷新打开了"。
            let wait = interval.duration().unwrap_or(Duration::from_secs(2));
            cx.background_executor().timer(wait).await;

            if interval.duration().is_none() {
                continue;
            }

            if this
                .update(cx, |shell, cx| shell.refresh(cx))
                .is_err()
            {
                break;
            }
        })
        .detach();
    }

    // -- 配置 --------------------------------------------------------------

    /// 加载 `settings.yaml`（优先使用 `wslc info` 报告的路径）。
    pub fn load_settings(&mut self, cx: &mut Context<Self>) {
        let info = self.state.snapshot.info.clone();
        match state::load_settings(info.as_ref()) {
            Ok(doc) => {
                self.state.settings = Some(doc);
                self.state.settings_error = None;
            }
            Err(e) => {
                self.state.settings = None;
                self.state.settings_error = Some(e);
            }
        }
        cx.notify();
    }

    /// 用户主动重新加载（会丢弃未保存的修改）。
    pub fn reload_settings(&mut self, cx: &mut Context<Self>) {
        self.load_settings(cx);
        self.state.notify(Toast::info("已重新加载 settings.yaml"));
        cx.notify();
    }

    /// 修改一项配置（只改内存中的文档，需要保存才落盘）。
    pub fn set_setting(
        &mut self,
        key: &'static SettingKey,
        value: Option<&'static str>,
        cx: &mut Context<Self>,
    ) {
        let Some(doc) = self.state.settings.as_mut() else {
            self.state.notify(Toast::error("配置文件尚未加载"));
            cx.notify();
            return;
        };

        if doc.set(key.section, key.key, value) {
            let shown = value.unwrap_or("默认值");
            self.state
                .notify(Toast::info(format!("{} → {shown}（记得保存）", key.label)));
        }
        cx.notify();
    }

    /// 备份并保存 `settings.yaml`。
    pub fn save_settings(&mut self, cx: &mut Context<Self>) {
        let result = match self.state.settings.as_mut() {
            Some(doc) => doc.save_with_backup().map_err(|e| e.to_string()),
            None => {
                self.state.notify(Toast::error("配置文件尚未加载"));
                cx.notify();
                return;
            }
        };

        let toast = match result {
            Ok(Some(backup)) => Toast::success(format!(
                "已保存，备份：{}",
                backup
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            )),
            Ok(None) => Toast::success("已保存"),
            Err(e) => Toast::error(format!("保存失败：{e}")),
        };
        self.state.notify(toast);
        cx.notify();
    }

    /// 用系统默认编辑器打开配置文件（`wslc settings`）。
    pub fn open_settings_in_editor(&mut self, cx: &mut Context<Self>) {
        let wslc = self.state.wslc.clone();
        let toast = match wslc_core::cmd::system::open_settings_in_editor(&wslc) {
            Ok(()) => Toast::success("已用系统默认编辑器打开 settings.yaml"),
            Err(e) => Toast::error(format!("打开失败：{e}")),
        };
        self.state.notify(toast);
        cx.notify();
    }

    // -- 危险操作确认 ------------------------------------------------------

    /// 请求执行一个危险操作（先弹确认框）。
    pub fn request(&mut self, action: PendingAction, cx: &mut Context<Self>) {
        self.state.request_confirm(action);
        cx.notify();
    }

    /// 取消确认。
    pub fn cancel_pending(&mut self, cx: &mut Context<Self>) {
        self.state.cancel_confirm();
        cx.notify();
    }

    /// 确认并执行。
    pub fn confirm_pending(&mut self, cx: &mut Context<Self>) {
        let Some(action) = self.state.confirm.take() else {
            return;
        };

        let wslc = self.state.wslc.clone();
        let toast = match action.execute(&wslc) {
            Ok(message) => Toast::success(message),
            Err(e) => Toast::error(format!("{}失败：{e}", action.title())),
        };
        self.state.notify(toast);
        cx.notify();
        // 立即刷新，让列表反映最新状态。
        self.refresh(cx);
    }

    // -- 界面状态 ----------------------------------------------------------

    /// 切换页面。
    pub fn set_page(&mut self, page: Page, cx: &mut Context<Self>) {
        if self.state.page != page {
            self.state.page = page;
            cx.notify();
        }
    }

    /// 切换自动刷新间隔。
    pub fn set_interval(&mut self, interval: RefreshInterval, cx: &mut Context<Self>) {
        self.state.interval = interval;
        cx.notify();
    }

    /// 关闭提示条。
    pub fn dismiss_toast(&mut self, cx: &mut Context<Self>) {
        self.state.toast = None;
        cx.notify();
    }
}

// ---------------------------------------------------------------------------
// 渲染
// ---------------------------------------------------------------------------

impl Render for Shell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let state = &self.state;

        // 导航按 `Page::group()` 分组，组名变化时插一条小标题。
        let mut nav: Vec<AnyElement> = Vec::new();
        let mut last_group = "";
        for page in Page::ALL {
            let group = page.group();
            if group != last_group {
                if !last_group.is_empty() {
                    nav.push(div().h(px(6.)).into_any_element());
                }
                nav.push(
                    div()
                        .px_3()
                        .pt_2()
                        .pb_1()
                        .text_xs()
                        .font_semibold()
                        .text_color(theme::text_dim())
                        .child(group)
                        .into_any_element(),
                );
                last_group = group;
            }
            nav.push(nav_item(page, state.page, &entity));
        }

        let subtitle = format!(
            "会话 {} · 刷新 {} ms",
            state.session_label(),
            state.snapshot.elapsed_ms
        );

        let refresh_button = {
            let entity = entity.clone();
            let busy = state.busy;
            let label = if busy { "刷新中…" } else { "刷新" };
            Button::new("refresh")
                .label(label)
                .primary()
                .disabled(busy)
                .on_click(move |_, _, cx| {
                    entity.update(cx, |shell, cx| shell.refresh(cx));
                })
        };

        let error_banner: AnyElement = if !state.snapshot.errors.is_empty() {
            v_flex()
                .w_full()
                .gap_1()
                .p_3()
                .rounded_md()
                .bg(theme::danger_soft())
                .border_1()
                .border_color(theme::danger_edge())
                .children(state.snapshot.errors.iter().map(|e| {
                    div()
                        .text_xs()
                        .text_color(theme::danger())
                        .child(e.clone())
                }))
                .into_any_element()
        } else if !state.snapshot.has_data() {
            // 首屏 / 连不上 wslc 时的提示。比一片空白有用得多。
            let hint = if state.busy {
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
        };

        let toast: AnyElement = match &state.toast {
            None => div().into_any_element(),
            Some(t) => {
                let entity = entity.clone();
                let color = match t.kind {
                    ToastKind::Info => theme::primary(),
                    ToastKind::Success => theme::success(),
                    ToastKind::Error => theme::danger(),
                };
                div()
                    .absolute()
                    .bottom(px(20.))
                    .right(px(20.))
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
                                    .child(t.text.clone()),
                            )
                            .child(
                                Button::new("toast-dismiss")
                                    .label("关闭")
                                    .small()
                                    .on_click(move |_, _, cx| {
                                        entity.update(cx, |shell, cx| shell.dismiss_toast(cx));
                                    }),
                            ),
                    )
                    .into_any_element()
            }
        };

        let confirm: AnyElement = match &state.confirm {
            None => div().into_any_element(),
            Some(action) => confirm_overlay(action, &entity),
        };

        let page_body = views::page(state, &entity);

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
                    .child(
                        v_flex()
                            .w(px(216.))
                            .h_full()
                            .flex_none()
                            .gap_1()
                            .p_3()
                            .bg(theme::bg_sidebar())
                            .border_r_1()
                            .border_color(theme::border())
                            .child(
                                h_flex()
                                    .w_full()
                                    .gap_2()
                                    .px_3()
                                    .py_3()
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
                            )
                            .children(nav),
                    )
                    // 主区域
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .overflow_hidden()
                            .child(
                                h_flex()
                                    .w_full()
                                    .flex_none()
                                    .justify_between()
                                    .px_6()
                                    .py_4()
                                    .border_b_1()
                                    .border_color(theme::border())
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
                                    )
                                    .child(refresh_button),
                            )
                            .child(
                                // `overflow_y_scroll` 来自 `StatefulInteractiveElement`，
                                // 只对**带 id 的**元素可用（这正是它叫 "stateful" 的原因）。
                                // 不带 `.id()` 会报 "no method named overflow_y_scroll"。
                                v_flex()
                                    .id("app-body-scroll")
                                    .w_full()
                                    .flex_1()
                                    .min_h_0()
                                    .gap_4()
                                    .p_5()
                                    .overflow_y_scroll()
                                    .child(error_banner)
                                    .child(page_body),
                            ),
                    ),
            )
            .child(toast)
            .child(confirm)
    }
}

/// 左侧导航项。
fn nav_item(page: Page, current: Page, entity: &Entity<Shell>) -> AnyElement {
    let entity = entity.clone();
    let active = page == current;

    let mut item = h_flex()
        .w_full()
        .id(nav_id(page))
        .gap_2()
        .px_3()
        .py_2()
        .rounded_md()
        .cursor_pointer()
        .child(div().text_sm().child(page.label()))
        .on_click(move |_, _, cx| {
            entity.update(cx, |shell, cx| shell.set_page(page, cx));
        });

    item = if active {
        item.bg(theme::bg_selected()).text_color(theme::primary())
    } else {
        item.text_color(theme::text_muted())
    };

    item.into_any_element()
}

/// 导航项的稳定 ID。
///
/// 用 `&'static str` 而不是格式化出来的 `String`：
/// GPUI 的交互元素要求 ID 稳定，静态字符串最不容易出错。
fn nav_id(page: Page) -> &'static str {
    match page {
        Page::Dashboard => "nav-dashboard",
        Page::Running => "nav-running",
        Page::Containers => "nav-containers",
        Page::Images => "nav-images",
        Page::Networks => "nav-networks",
        Page::Volumes => "nav-volumes",
        Page::Config => "nav-config",
    }
}

/// 危险操作的确认浮层。
fn confirm_overlay(action: &PendingAction, entity: &Entity<Shell>) -> AnyElement {
    let cancel = {
        let entity = entity.clone();
        Button::new("confirm-cancel")
            .label("取消")
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.cancel_pending(cx));
            })
    };

    let confirm = {
        let entity = entity.clone();
        Button::new("confirm-ok")
            .label(action.confirm_label())
            .primary()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.confirm_pending(cx));
            })
    };

    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(theme::scrim())
        .child(
            v_flex()
                .w(px(460.))
                .gap_4()
                .p_5()
                .rounded_lg()
                .bg(theme::bg_card())
                .border_1()
                .border_color(theme::danger().opacity(0.5))
                .child(
                    div()
                        .text_lg()
                        .font_bold()
                        .text_color(theme::danger())
                        .child(action.title()),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(theme::text_muted())
                        .child(action.body()),
                )
                .child(
                    h_flex()
                        .w_full()
                        .justify_end()
                        .gap_2()
                        .child(cancel)
                        .child(confirm),
                ),
        )
        .into_any_element()
}
