//! 页面渲染。
//!
//! 全部页面都是纯函数：入参是 [`AppState`] 的只读引用 + `Entity<Shell>`，
//! 出参是元素。所有交互通过 `entity.update(...)` 回写到状态，
//! 因此这里没有可变状态，也没有异步逻辑 —— 渲染与数据彻底解耦。

// 注意：`primary()` / `danger()` 这些样式方法来自 trait `ButtonVariants`，
// 光导入 `Button` 是不够的 —— 这里用 glob 把 button 模块全带上。
use gpui_kit::component::button::*;
// `StyledExt` 提供 `font_bold` / `font_semibold` 等字重方法（由宏生成）。
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{Disableable, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::*;

use wslc_core::model::ContainerState;
use wslc_core::settings::{SETTING_KEYS, SettingKey, SettingKind};

use crate::app::Shell;
use crate::state::{AppState, Page, PendingAction, PullProgress};
use crate::theme;

// ---------------------------------------------------------------------------
// 通用小组件
// ---------------------------------------------------------------------------

/// 卡片：标题 + 内容。
fn card(title: &'static str, body: impl IntoElement) -> impl IntoElement {
    v_flex()
        .w_full()
        .gap_3()
        .p_4()
        .rounded_lg()
        .bg(theme::bg_card())
        .border_1()
        .border_color(theme::border())
        .child(
            div()
                .text_sm()
                .font_semibold()
                .text_color(theme::text_muted())
                .child(title),
        )
        .child(body)
}

/// 键值对行。
fn kv(label: &'static str, value: impl Into<SharedString>) -> impl IntoElement {
    h_flex()
        .w_full()
        .justify_between()
        .gap_4()
        .child(
            div()
                .flex_none()
                .text_sm()
                .text_color(theme::text_dim())
                .child(label),
        )
        .child(
            div()
                .text_sm()
                .text_color(theme::text())
                .overflow_hidden()
                .truncate()
                .child(value.into()),
        )
}

/// 顶部统计数字块。
fn stat_tile(label: &'static str, value: String, color: Rgba) -> impl IntoElement {
    v_flex()
        .flex_1()
        .gap_1()
        .p_3()
        .rounded_md()
        .bg(theme::bg_card())
        .border_1()
        .border_color(theme::border())
        .child(div().text_xs().text_color(theme::text_dim()).child(label))
        .child(div().text_xl().font_bold().text_color(color).child(value))
}

/// 状态徽标。
///
/// 底色由 [`theme::state_colors`] 统一给出（不透明的预置色），
/// 不用 alpha 叠加 —— 叠出来的观感会随背后的容器背景变化。
fn badge(text: impl Into<SharedString>, fg: Rgba, bg: Rgba) -> impl IntoElement {
    div()
        .flex_none()
        .px_2()
        .py_1()
        .rounded_md()
        .bg(bg)
        .text_xs()
        .font_semibold()
        .text_color(fg)
        .child(text.into())
}

/// 空状态占位。
fn empty_state(text: &'static str) -> impl IntoElement {
    v_flex()
        .w_full()
        .py_8()
        .items_center()
        .justify_center()
        .child(div().text_sm().text_color(theme::text_dim()).child(text))
}

/// 表格表头。
fn table_header(columns: &'static [(&'static str, f32)]) -> impl IntoElement {
    h_flex()
        .w_full()
        .gap_2()
        .px_3()
        .py_2()
        .border_b_1()
        .border_color(theme::border())
        .children(columns.iter().map(|(name, width)| {
            div()
                .w(px(*width))
                .flex_none()
                .text_xs()
                .font_semibold()
                .text_color(theme::text_dim())
                .child(*name)
        }))
}

/// 表格行容器。
fn table_row(columns: &'static [(&'static str, f32)], cells: Vec<AnyElement>) -> impl IntoElement {
    h_flex()
        .w_full()
        .gap_2()
        .px_3()
        .py_2()
        .border_b_1()
        .border_color(theme::border())
        .justify_start()
        .children(cells.into_iter().enumerate().map(|(i, cell)| {
            let width = columns.get(i).map(|c| c.1).unwrap_or(120.0);
            div().w(px(width)).flex_none().child(cell)
        }))
}

/// 单元格文字。
fn cell_text(text: impl Into<SharedString>) -> AnyElement {
    div()
        .text_sm()
        .text_color(theme::text())
        .overflow_hidden()
        .truncate()
        .child(text.into())
        .into_any_element()
}

/// 次要单元格文字。
fn cell_muted(text: impl Into<SharedString>) -> AnyElement {
    div()
        .text_sm()
        .text_color(theme::text_muted())
        .overflow_hidden()
        .truncate()
        .child(text.into())
        .into_any_element()
}

/// 状态徽标单元格。
fn cell_badge(state: ContainerState) -> AnyElement {
    let (fg, bg) = theme::state_colors(&state);
    badge(state.label().to_owned(), fg, bg).into_any_element()
}

/// 危险操作按钮（点击后弹出二次确认）。
fn danger_button(
    id: &str,
    label: &'static str,
    action: PendingAction,
    entity: &Entity<Shell>,
) -> impl IntoElement {
    let entity = entity.clone();
    Button::new(SharedString::from(id.to_owned()))
        .label(label)
        .small()
        .on_click(move |_, _, cx| {
            entity.update(cx, |shell, cx| {
                shell.request(action.clone(), cx);
            });
        })
}

// ---------------------------------------------------------------------------
// 页面分发
// ---------------------------------------------------------------------------

/// 渲染当前页面。
pub fn page(state: &AppState, entity: &Entity<Shell>) -> AnyElement {
    match state.page {
        Page::Dashboard => dashboard(state, entity).into_any_element(),
        Page::Running => running(state, entity).into_any_element(),
        Page::Containers => containers(state, entity).into_any_element(),
        Page::Images => images(state, entity).into_any_element(),
        Page::Networks => networks(state, entity).into_any_element(),
        Page::Volumes => volumes(state, entity).into_any_element(),
        Page::Config => config(state, entity),
    }
}

// ---------------------------------------------------------------------------
// ① 基本信息 / 总览
// ---------------------------------------------------------------------------

/// 总览页：`wslc info` + 统计 + 会话。
pub fn dashboard(state: &AppState, entity: &Entity<Shell>) -> impl IntoElement {
    let snap = &state.snapshot;

    let running = snap.all.iter().filter(|c| c.is_running()).count();
    let total = snap.all.len();

    let client = snap
        .info
        .as_ref()
        .map(|i| i.client.clone())
        .unwrap_or_default();
    let server = snap
        .info
        .as_ref()
        .map(|i| i.server.clone())
        .unwrap_or_default();

    // 存储那几行**不**直接展示 settings.yaml 里的原始值：
    // 出厂状态所有键都是注释（`# storagePath: default`），读到的是"未设置"，
    // 直接摆出来就变成"（默认：%LOCALAPPDATA%）"这种等于没说的东西。
    // 真实落盘位置由 `snap.storage` 提供，见 `storage_rows`。

    let session_rows: Vec<AnyElement> = if snap.sessions.is_empty() {
        vec![empty_state("没有活动的 wslc 会话").into_any_element()]
    } else {
        snap.sessions
            .iter()
            .map(|s| {
                h_flex()
                    .w_full()
                    .gap_3()
                    .py_1()
                    .child(badge(
                        format!("#{}", s.id),
                        theme::primary(),
                        theme::primary_soft(),
                    ))
                    .child(cell_text(s.display()))
                    .child(cell_muted(format!("创建者 PID {}", s.creator_pid)))
                    .into_any_element()
            })
            .collect()
    };

    v_flex()
        .w_full()
        .gap_4()
        .child(
            h_flex()
                .w_full()
                .gap_3()
                .child(stat_tile(
                    "运行中容器",
                    running.to_string(),
                    theme::success(),
                ))
                .child(stat_tile("全部容器", total.to_string(), theme::text()))
                .child(stat_tile(
                    "镜像",
                    snap.images.len().to_string(),
                    theme::text(),
                ))
                .child(stat_tile(
                    "网络",
                    snap.networks.len().to_string(),
                    theme::text(),
                ))
                .child(stat_tile(
                    "卷",
                    snap.volumes.len().to_string(),
                    theme::text(),
                )),
        )
        .child(
            h_flex()
                .w_full()
                .gap_4()
                .items_start()
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_4()
                        .child(card(
                            "客户端",
                            v_flex()
                                .w_full()
                                .gap_2()
                                .child(kv("WSL 版本", client.version.clone()))
                                .child(kv("内核版本", client.kernel_version.clone()))
                                .child(kv("Windows", client.windows_version.clone()))
                                .child(kv("Direct3D", client.direct3d_version.clone()))
                                .child(kv("DXCore", client.dxcore_version.clone())),
                        ))
                        .child(card(
                            "存储",
                            v_flex()
                                .w_full()
                                .gap_2()
                                .child(kv("配置文件", client.settings_file.clone()))
                                .children(storage_rows(state, entity)),
                        )),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_4()
                        .child(card(
                            "服务器",
                            v_flex()
                                .w_full()
                                .gap_2()
                                .child(kv("会话管理器", server.session_manager_version.clone()))
                                .child(kv("活动会话数", snap.sessions.len().to_string()))
                                .child(kv("当前会话", state.session_label())),
                        ))
                        .child(card(
                            "活动会话",
                            v_flex().w_full().gap_1().children(session_rows),
                        )),
                ),
        )
        // `wslc` 没有 `system df`，磁盘占用是我们在 Windows 侧自己算的。
        .child(disk_usage_card(state))
}
// ---------------------------------------------------------------------------
// 存储与磁盘占用（概览页用）
// ---------------------------------------------------------------------------

/// 字节 → 人类可读（1024 进制）。
fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

/// "存储"卡片里 `storagePath` 那几行。
///
/// 关键点：出厂状态下 `settings.yaml` 里所有键都是注释，
/// 读到的永远是"未设置"。所以这里显示的是**展开后的真实路径**，
/// 而不是"（默认：%LOCALAPPDATA%）"这种等于没说的字符串。
fn storage_rows(state: &AppState, entity: &Entity<Shell>) -> Vec<AnyElement> {
    let Some(storage) = state.snapshot.storage.as_ref() else {
        return vec![kv("storagePath", "无法确定（LOCALAPPDATA 未设置）").into_any_element()];
    };

    let configured_text = match &storage.configured {
        Some(value) => format!("{value}（来自 settings.yaml）"),
        None => format!(
            "未设置 → 用内置默认值 {}，展开后见下行",
            wslc_core::storage::DEFAULT_PLACEHOLDER
        ),
    };

    let vhd_text = match (&storage.vhd, storage.vhd_bytes) {
        (Some(path), Some(bytes)) => {
            format!("{}（{}）", path.display(), format_bytes(bytes))
        }
        (Some(path), None) => format!("{}（尚未创建）", path.display()),
        (None, _) => "（没有活动会话，无法定位 VHD）".to_owned(),
    };

    vec![
        kv("storagePath", configured_text).into_any_element(),
        kv("实际目录", storage.base.display().to_string()).into_any_element(),
        kv("会话磁盘", vhd_text).into_any_element(),
        reveal_storage_button(entity),
    ]
}

/// "打开所在文件夹"按钮。
///
/// 注意这是**只读**操作 —— v0.2 不允许改 `storagePath`：
/// 改了不会迁移已有容器/镜像，还会新建一个空会话，风险太大。
fn reveal_storage_button(entity: &Entity<Shell>) -> AnyElement {
    let entity = entity.clone();
    h_flex()
        .w_full()
        .justify_end()
        .child(
            Button::new("reveal-storage")
                .label("打开所在文件夹")
                .small()
                .on_click(move |_, _, cx| {
                    entity.update(cx, |shell, cx| shell.reveal_storage(cx));
                }),
        )
        .into_any_element()
}

/// 概览页的「磁盘占用」卡片。
///
/// 每一项都**如实标注来源与局限**。`wslc` 没有 `system df`：
/// 会话磁盘是文件系统实测的，镜像合计来自 `wslc images` 的 `Size`，
/// 而容器可写层与卷和镜像挤在同一个 VHD 里，Windows 侧分不出来 ——
/// 那就写"无法单独统计"，不编数字。
fn disk_usage_card(state: &AppState) -> AnyElement {
    let snap = &state.snapshot;

    let Some(storage) = snap.storage.as_ref() else {
        return card(
            "磁盘占用",
            v_flex()
                .w_full()
                .gap_2()
                .child(empty_state("无法确定存储位置（LOCALAPPDATA 未设置）")),
        )
        .into_any_element();
    };

    // `wslc images` 的 Size 是**每个镜像含共享层的总大小**，
    // 直接相加会重复计算共享层，所以文案里要说明。
    let image_total: f64 = snap.images.iter().filter_map(|i| i.size_bytes()).sum();

    let mut rows: Vec<AnyElement> = vec![
        kv(
            "会话磁盘",
            match storage.vhd_bytes {
                Some(bytes) => format_bytes(bytes),
                None => "尚未创建".to_owned(),
            },
        )
        .into_any_element(),
    ];

    if storage.has_other_sessions() {
        rows.push(
            kv(
                "其他会话",
                format!(
                    "{}（共 {} 个会话）",
                    format_bytes(storage.other_sessions_bytes()),
                    storage.sessions_count
                ),
            )
            .into_any_element(),
        );
    }

    rows.push(
        kv(
            "镜像合计",
            format!(
                "{}（{} 个镜像，含共享层重复计算）",
                format_bytes(image_total.max(0.0) as u64),
                snap.images.len()
            ),
        )
        .into_any_element(),
    );

    rows.push(kv("容器可写层", "无法单独统计（与镜像共用同一个 VHD）").into_any_element());
    rows.push(kv("卷", "无法单独统计（同上）").into_any_element());

    if let Some(volume) = storage.volume {
        rows.push(
            kv(
                "所在盘",
                format!(
                    "已用 {} / {}（剩余 {}，{:.1}%）",
                    format_bytes(volume.used()),
                    format_bytes(volume.total),
                    format_bytes(volume.free),
                    volume.used_percent()
                ),
            )
            .into_any_element(),
        );
    }

    card("磁盘占用", v_flex().w_full().gap_2().children(rows)).into_any_element()
}

// 这里曾经还有一张"刷新"卡片（上次耗时 / 自动刷新档位）。
// 刷新属于实现细节：耗时写日志，间隔在"设置"页里改，概览页不再展示。

// ---------------------------------------------------------------------------
// ② 当前运行 container
// ---------------------------------------------------------------------------

const RUNNING_COLUMNS: &[(&str, f32)] = &[
    ("名称", 150.0),
    ("状态", 90.0),
    ("镜像", 190.0),
    ("端口", 160.0),
    ("CPU", 70.0),
    ("内存", 140.0),
    ("网络 I/O", 120.0),
    ("块 I/O", 110.0),
    ("PID", 50.0),
    ("操作", 160.0),
];

/// 当前运行容器页：列表 + 实时统计。
pub fn running(state: &AppState, entity: &Entity<Shell>) -> impl IntoElement {
    let items = &state.snapshot.running;

    let rows: Vec<AnyElement> = items
        .iter()
        .enumerate()
        .map(|(ix, summary)| {
            let item = &summary.item;
            let stats = summary.stats.as_ref();

            let cpu = stats
                .map(|s| s.cpu_perc.clone())
                .unwrap_or_else(|| "-".into());
            let mem = stats
                .map(|s| s.mem_usage.clone())
                .unwrap_or_else(|| "-".into());
            let net = stats
                .map(|s| s.net_io.clone())
                .unwrap_or_else(|| "-".into());
            let block = stats
                .map(|s| s.block_io.clone())
                .unwrap_or_else(|| "-".into());
            let pids = stats
                .map(|s| s.pids.to_string())
                .unwrap_or_else(|| "-".into());

            let ports = item
                .port_mappings()
                .iter()
                .map(|p| p.display())
                .collect::<Vec<_>>()
                .join("、");
            let ports = if ports.is_empty() {
                "—".to_owned()
            } else {
                ports
            };

            let name = item.display_name().to_owned();

            table_row(
                RUNNING_COLUMNS,
                vec![
                    cell_text(name.clone()),
                    cell_badge(item.state_kind()),
                    cell_muted(item.image.clone()),
                    cell_text(ports),
                    cell_text(cpu),
                    cell_muted(mem),
                    cell_muted(net),
                    cell_muted(block),
                    cell_text(pids),
                    h_flex()
                        .gap_2()
                        .child(danger_button(
                            &format!("stop-{ix}"),
                            "停止",
                            PendingAction::StopContainer(name.clone()),
                            entity,
                        ))
                        .child(danger_button(
                            &format!("kill-{ix}"),
                            "强杀",
                            PendingAction::KillContainer(name),
                            entity,
                        ))
                        .into_any_element(),
                ],
            )
            .into_any_element()
        })
        .collect();

    v_flex()
        .w_full()
        .gap_3()
        .child(
            h_flex().w_full().justify_between().child(
                div()
                    .text_sm()
                    .text_color(theme::text_muted())
                    .child(format!("{} 个容器正在运行", items.len())),
            ),
        )
        .child(
            v_flex()
                .w_full()
                .rounded_lg()
                .bg(theme::bg_card())
                .border_1()
                .border_color(theme::border())
                .overflow_hidden()
                .child(table_header(RUNNING_COLUMNS))
                .child(if rows.is_empty() {
                    empty_state("当前没有运行中的容器").into_any_element()
                } else {
                    v_flex().w_full().children(rows).into_any_element()
                }),
        )
}

// ---------------------------------------------------------------------------
// ③ 全部 container
// ---------------------------------------------------------------------------

const ALL_COLUMNS: &[(&str, f32)] = &[
    ("名称", 160.0),
    ("状态", 200.0),
    ("镜像", 230.0),
    ("运行时长", 130.0),
    ("端口", 160.0),
    ("大小", 80.0),
    ("操作", 160.0),
];

/// 全部容器页（含已退出）。
pub fn containers(state: &AppState, entity: &Entity<Shell>) -> impl IntoElement {
    let items = &state.snapshot.all;

    let rows: Vec<AnyElement> = items
        .iter()
        .enumerate()
        .map(|(ix, summary)| {
            let item = &summary.item;
            let name = item.display_name().to_owned();
            let ports = item
                .port_mappings()
                .iter()
                .map(|p| p.display())
                .collect::<Vec<_>>()
                .join("、");
            let ports = if ports.is_empty() {
                "—".to_owned()
            } else {
                ports
            };

            table_row(
                ALL_COLUMNS,
                vec![
                    cell_text(name.clone()),
                    cell_badge(item.state_kind()),
                    cell_muted(item.image.clone()),
                    cell_muted(item.running_for.clone()),
                    cell_text(ports),
                    cell_muted(item.size.clone()),
                    h_flex()
                        .gap_2()
                        .child(danger_button(
                            &format!("rm-{ix}"),
                            "删除",
                            PendingAction::RemoveContainer(name.clone()),
                            entity,
                        ))
                        .child(danger_button(
                            &format!("stop-all-{ix}"),
                            "停止",
                            PendingAction::StopContainer(name),
                            entity,
                        ))
                        .into_any_element(),
                ],
            )
            .into_any_element()
        })
        .collect();

    let prune = {
        let entity = entity.clone();
        Button::new("prune")
            .label("清理已停止")
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| {
                    shell.request(PendingAction::PruneContainers, cx);
                });
            })
    };

    v_flex()
        .w_full()
        .gap_3()
        .child(
            h_flex()
                .w_full()
                .justify_between()
                .child(
                    div()
                        .text_sm()
                        .text_color(theme::text_muted())
                        .child(format!("共 {} 个容器", items.len())),
                )
                .child(prune),
        )
        .child(
            v_flex()
                .w_full()
                .rounded_lg()
                .bg(theme::bg_card())
                .border_1()
                .border_color(theme::border())
                .overflow_hidden()
                .child(table_header(ALL_COLUMNS))
                .child(if rows.is_empty() {
                    empty_state("没有任何容器").into_any_element()
                } else {
                    v_flex().w_full().children(rows).into_any_element()
                }),
        )
}

// ---------------------------------------------------------------------------
// ④ 镜像 / 网络 / 卷
// ---------------------------------------------------------------------------

const IMAGE_COLUMNS: &[(&str, f32)] = &[
    ("仓库:标签", 380.0),
    ("ID", 130.0),
    ("大小", 100.0),
    ("创建于", 200.0),
    ("操作", 120.0),
];

/// 镜像页。
pub fn images(state: &AppState, entity: &Entity<Shell>) -> impl IntoElement {
    let rows: Vec<AnyElement> = state
        .snapshot
        .images
        .iter()
        .enumerate()
        .map(|(ix, img)| {
            let reference = img.reference();
            table_row(
                IMAGE_COLUMNS,
                vec![
                    cell_text(reference.clone()),
                    cell_muted(img.short_id().to_owned()),
                    cell_muted(img.size.clone()),
                    cell_muted(img.created_since.clone()),
                    danger_button(
                        &format!("rmi-{ix}"),
                        "删除",
                        PendingAction::RemoveImage(reference),
                        entity,
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element()
        })
        .collect();

    v_flex()
        .w_full()
        .gap_3()
        .child(
            h_flex()
                .w_full()
                .justify_between()
                .child({
                    let entity = entity.clone();
                    let busy = state.pulling.is_some();
                    Button::new("pull-image")
                        .label("拉取镜像")
                        .small()
                        .disabled(busy)
                        .on_click(move |_, window, cx| {
                            // 注意：`InputState` 只能在有 `window` 的地方创建，
                            // 这里正好有 —— 所以弹窗是懒创建的。
                            entity.update(cx, |shell, cx| shell.open_pull_dialog(window, cx));
                        })
                })
                .child(div().text_sm().text_color(theme::text_muted()).child(
                    match &state.pulling {
                        Some(progress) => format!("正在拉取 {} …", progress.image),
                        None => format!(
                            "共 {} 条镜像记录（同一镜像可能对应多个仓库引用）",
                            state.snapshot.images.len()
                        ),
                    },
                )),
        )
        .child(
            v_flex()
                .w_full()
                .rounded_lg()
                .bg(theme::bg_card())
                .border_1()
                .border_color(theme::border())
                .overflow_hidden()
                .child(table_header(IMAGE_COLUMNS))
                .child(if rows.is_empty() {
                    empty_state("没有本地镜像").into_any_element()
                } else {
                    v_flex().w_full().children(rows).into_any_element()
                }),
        )
}

const NETWORK_COLUMNS: &[(&str, f32)] = &[
    ("名称", 200.0),
    ("ID", 140.0),
    ("驱动", 100.0),
    ("作用域", 100.0),
    ("IPv6", 80.0),
    ("操作", 220.0),
];

/// 网络页。
pub fn networks(state: &AppState, entity: &Entity<Shell>) -> impl IntoElement {
    let rows: Vec<AnyElement> = state
        .snapshot
        .networks
        .iter()
        .enumerate()
        .map(|(ix, net)| {
            let op: AnyElement = if net.is_builtin() {
                div()
                    .text_xs()
                    .text_color(theme::text_dim())
                    .child("内置网络，不可删除")
                    .into_any_element()
            } else {
                danger_button(
                    &format!("rmnet-{ix}"),
                    "删除",
                    PendingAction::RemoveNetwork(net.name.clone()),
                    entity,
                )
                .into_any_element()
            };

            table_row(
                NETWORK_COLUMNS,
                vec![
                    cell_text(net.name.clone()),
                    cell_muted(net.short_id().to_owned()),
                    cell_muted(net.driver.clone()),
                    cell_muted(net.scope.clone()),
                    cell_muted(if net.ipv6_enabled() {
                        "启用"
                    } else {
                        "关闭"
                    }),
                    op,
                ],
            )
            .into_any_element()
        })
        .collect();

    v_flex()
        .w_full()
        .gap_3()
        .child(
            div()
                .text_sm()
                .text_color(theme::text_muted())
                .child(format!("共 {} 个网络", state.snapshot.networks.len())),
        )
        .child(
            v_flex()
                .w_full()
                .rounded_lg()
                .bg(theme::bg_card())
                .border_1()
                .border_color(theme::border())
                .overflow_hidden()
                .child(table_header(NETWORK_COLUMNS))
                .child(if rows.is_empty() {
                    empty_state("没有网络").into_any_element()
                } else {
                    v_flex().w_full().children(rows).into_any_element()
                }),
        )
}

const VOLUME_COLUMNS: &[(&str, f32)] = &[
    ("名称", 240.0),
    ("驱动", 120.0),
    ("作用域", 120.0),
    ("挂载点", 320.0),
    ("操作", 120.0),
];

/// 卷页。
pub fn volumes(state: &AppState, entity: &Entity<Shell>) -> impl IntoElement {
    let rows: Vec<AnyElement> = state
        .snapshot
        .volumes
        .iter()
        .enumerate()
        .map(|(ix, vol)| {
            table_row(
                VOLUME_COLUMNS,
                vec![
                    cell_text(vol.display_name().to_owned()),
                    cell_muted(vol.driver.clone()),
                    cell_muted(vol.scope.clone()),
                    cell_muted(vol.mountpoint.clone()),
                    danger_button(
                        &format!("rmvol-{ix}"),
                        "删除",
                        PendingAction::RemoveVolume(vol.name.clone()),
                        entity,
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element()
        })
        .collect();

    v_flex()
        .w_full()
        .gap_3()
        .child(
            div()
                .text_sm()
                .text_color(theme::text_muted())
                .child(format!("共 {} 个卷", state.snapshot.volumes.len())),
        )
        .child(
            v_flex()
                .w_full()
                .rounded_lg()
                .bg(theme::bg_card())
                .border_1()
                .border_color(theme::border())
                .overflow_hidden()
                .child(table_header(VOLUME_COLUMNS))
                .child(if rows.is_empty() {
                    empty_state("没有卷（无卷时 wslc 输出为空）").into_any_element()
                } else {
                    v_flex().w_full().children(rows).into_any_element()
                }),
        )
}

// ---------------------------------------------------------------------------
// ⑤ wlsc 配置
// ---------------------------------------------------------------------------

/// 配置页。
///
/// 当前能力：逐项展示 + 预设值快捷写入 + 恢复默认 + 原始 YAML 查看 +
/// 带时间戳备份的保存 + 用系统编辑器打开。
///
/// 任意文本输入的完整表单需要 GPUI 的 `InputState` / `TextInput`，
/// 属于下一步（见 `docs/PLAN.md` M6）。
pub fn config(state: &AppState, entity: &Entity<Shell>) -> AnyElement {
    let Some(doc) = &state.settings else {
        let message = state
            .settings_error
            .clone()
            .unwrap_or_else(|| "尚未加载配置文件".to_owned());
        return v_flex()
            .w_full()
            .gap_4()
            .child(card(
                "wlsc 配置",
                v_flex()
                    .w_full()
                    .gap_3()
                    .child(div().text_sm().text_color(theme::danger()).child(message))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(open_in_editor_button(entity))
                            .child(reload_button(entity)),
                    ),
            ))
            // 即使 wslc 配置读不出来，"界面"偏好仍然可用。
            .child(interface_card(state, entity))
            .into_any_element();
    };

    let values = doc.values();
    let dirty = doc.is_dirty();

    let rows: Vec<AnyElement> = SETTING_KEYS
        .iter()
        .map(|key| setting_row(key, values.get(key), entity))
        .collect();

    let path = doc.path().display().to_string();
    let raw = doc.raw().to_owned();

    v_flex()
        .w_full()
        .gap_4()
        .child(card(
            "配置文件",
            v_flex()
                .w_full()
                .gap_3()
                .child(kv("路径", path))
                .child(kv(
                    "状态",
                    if dirty {
                        "有未保存的修改"
                    } else {
                        "已同步"
                    },
                ))
                .child(div().text_xs().text_color(theme::text_dim()).child(
                    "所有配置项的默认值在文件里都是注释状态，表示使用 wslc 内置默认值。\
                             保存前会自动做一次时间戳备份。",
                ))
                .child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .child(open_in_editor_button(entity))
                        .child(save_button(entity, dirty))
                        .child(reload_button(entity)),
                ),
        ))
        .child(card("配置项", v_flex().w_full().gap_2().children(rows)))
        // 应用自己的偏好放在最后，和上面那些 settings.yaml 的条目区分开。
        .child(interface_card(state, entity))
        .child(card(
            "原始 YAML",
            // 同 app.rs：滚动容器必须先有 id。
            v_flex()
                .id("settings-raw-yaml")
                .w_full()
                .max_h(px(420.))
                .overflow_y_scroll()
                .rounded_md()
                .bg(theme::bg())
                .p_3()
                .child(
                    div()
                        .font_family("Consolas")
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child(raw),
                ),
        ))
        .into_any_element()
}

/// 单个配置项的一行。
fn setting_row(
    key: &'static SettingKey,
    effective: Option<String>,
    entity: &Entity<Shell>,
) -> AnyElement {
    let is_default = effective.is_none();
    let shown = effective.unwrap_or_else(|| format!("默认（{}）", key.default));

    let mut actions = h_flex().flex_none().gap_2();

    // 枚举型：每个候选值一个按钮。
    if key.kind == SettingKind::Enum {
        for choice in key.choices.iter().copied() {
            let entity = entity.clone();
            actions = actions.child(
                Button::new(SharedString::from(format!("set-{}-{}", key.key, choice)))
                    .label(choice)
                    .small()
                    .on_click(move |_, _, cx| {
                        entity.update(cx, |shell, cx| {
                            shell.set_setting(key, Some(choice), cx);
                        });
                    }),
            );
        }
    }

    // 常用预设值。
    for preset in presets_for(key).iter().copied() {
        let entity = entity.clone();
        actions = actions.child(
            Button::new(SharedString::from(format!("preset-{}-{}", key.key, preset)))
                .label(preset)
                .small()
                .on_click(move |_, _, cx| {
                    entity.update(cx, |shell, cx| {
                        shell.set_setting(key, Some(preset), cx);
                    });
                }),
        );
    }

    // 非默认状态才显示"用默认"。
    if !is_default {
        let entity = entity.clone();
        actions = actions.child(
            Button::new(SharedString::from(format!("reset-{}", key.key)))
                .label("用默认")
                .small()
                .on_click(move |_, _, cx| {
                    entity.update(cx, |shell, cx| {
                        shell.set_setting(key, None, cx);
                    });
                }),
        );
    }

    let warning: AnyElement = if key.needs_warning {
        div()
            .text_xs()
            .text_color(theme::warning())
            .child("⚠ 修改此项不会迁移已有的容器与镜像，会在新路径下创建一个空会话。")
            .into_any_element()
    } else {
        div().into_any_element()
    };

    v_flex()
        .w_full()
        .gap_1()
        .py_2()
        .border_b_1()
        .border_color(theme::border())
        .child(
            h_flex()
                .w_full()
                .gap_3()
                .child(
                    div()
                        .w(px(150.))
                        .flex_none()
                        .text_sm()
                        .font_semibold()
                        .text_color(theme::text())
                        .child(key.label),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .text_color(if is_default {
                            theme::text_dim()
                        } else {
                            theme::primary()
                        })
                        .overflow_hidden()
                        .truncate()
                        .child(shown),
                )
                .child(actions),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme::text_dim())
                .child(format!("{}（{}）", key.help, key.key)),
        )
        .child(warning)
        .into_any_element()
}

/// 各配置项的常用预设值。
fn presets_for(key: &SettingKey) -> &'static [&'static str] {
    match (key.section, key.key) {
        (Some("session"), "cpuCount") => &["4", "8", "16"],
        (Some("session"), "memorySize") => &["2GB", "4GB", "8GB"],
        (Some("session"), "maxStorageSize") => &["100GB", "500GB", "1TB"],
        (Some("session"), "idleTimeout") => &["30", "60", "300"],
        (Some("session"), "hostLoopback") => &["host.wslc.internal", "none"],
        (Some("session"), "defaultBindingAddress") => &["127.0.0.1", "0.0.0.0"],
        _ => &[],
    }
}

fn open_in_editor_button(entity: &Entity<Shell>) -> impl IntoElement {
    let entity = entity.clone();
    Button::new("open-settings")
        .label("用系统编辑器打开")
        .primary()
        .on_click(move |_, _, cx| {
            entity.update(cx, |shell, cx| shell.open_settings_in_editor(cx));
        })
}

fn save_button(entity: &Entity<Shell>, dirty: bool) -> impl IntoElement {
    let entity = entity.clone();
    let label = if dirty {
        "备份并保存"
    } else {
        "已保存"
    };
    Button::new("save-settings")
        .label(label)
        .disabled(!dirty)
        .on_click(move |_, _, cx| {
            entity.update(cx, |shell, cx| shell.save_settings(cx));
        })
}

fn reload_button(entity: &Entity<Shell>) -> impl IntoElement {
    let entity = entity.clone();
    Button::new("reload-settings")
        .label("重新加载")
        .on_click(move |_, _, cx| {
            entity.update(cx, |shell, cx| {
                shell.reload_settings(cx);
            });
        })
}

/// "界面"卡片：**应用自己的偏好**，与 `wslc` 的配置无关。
///
/// 界面上不再到处显示刷新间隔 —— 只在这一处设置。
fn interface_card(state: &AppState, entity: &Entity<Shell>) -> AnyElement {
    let path_text = crate::prefs::Prefs::path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "（无法确定偏好文件位置）".to_owned());

    card(
        "界面",
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

/// 自动刷新间隔选择器（只出现在"设置"页）。
///
/// 这是**应用自己的偏好**，不是 `wslc` 的配置 —— 所以它和上面那些
/// `settings.yaml` 的条目在视觉上分开，用的是本地 `prefs` 而不是 `SettingsDoc`。
fn refresh_secs_picker(state: &AppState, entity: &Entity<Shell>) -> impl IntoElement {
    let current = state.prefs.refresh_secs;

    let buttons: Vec<AnyElement> = crate::prefs::REFRESH_PRESETS
        .iter()
        .map(|secs| {
            let secs = *secs;
            let entity = entity.clone();
            let mut button = Button::new(SharedString::from(format!("refresh-{secs}")))
                .label(format!("{secs} 秒"))
                .small()
                .on_click(move |_, _, cx| {
                    entity.update(cx, |shell, cx| {
                        shell.set_refresh_secs(secs, cx);
                    });
                });
            if current == secs {
                button = button.primary();
            }
            button.into_any_element()
        })
        .collect();

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
        .children(buttons)
        .children(extra)
}

/// 「拉取镜像」弹窗（覆盖层）。
///
/// 这是项目里**第一次**使用 GPUI 的输入控件（`InputState` + `Input`）。
/// 接入的四个关键点：
///
/// 1. **构造** —— `cx.new(|cx| InputState::new(window, cx).placeholder(..))`，
///    必须在有 `&mut Window` 的地方（见 `Shell::open_pull_dialog`）；
/// 2. **持有** —— `Shell::pull_input: Option<Entity<InputState>>`；
/// 3. **渲染** —— 就是这里的 `Input::new(input)`；
/// 4. **读值** —— `input.read(cx).value()`（见 `Shell::confirm_pull`；
///    注意 `value` 不带参数，见那里的注释）。
///
/// 焦点在打开弹窗时由 `window.focus(&handle, cx)` 交给输入框，
/// 用的是公开的 `InputState::focus_handle`（`InputState::focus` 是
/// `pub(crate)`，外部调不到）。
pub fn pull_dialog_overlay(
    input: &Entity<InputState>,
    state: &AppState,
    entity: &Entity<Shell>,
) -> AnyElement {
    // 弹窗有两种形态：没在拉取时是表单，拉取中是进度。
    let body: AnyElement = match &state.pulling {
        Some(progress) => pull_progress_body(progress, entity),
        None => pull_form_body(input, entity),
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
                .w(px(620.))
                .gap_4()
                .p_5()
                .rounded_lg()
                .bg(theme::bg_card())
                .border_1()
                .border_color(theme::border())
                .child(
                    div()
                        .text_lg()
                        .font_bold()
                        .text_color(theme::text())
                        .child("拉取镜像"),
                )
                .child(body),
        )
        .into_any_element()
}

/// 弹窗的**表单**形态（还没开始拉）。
fn pull_form_body(input: &Entity<InputState>, entity: &Entity<Shell>) -> AnyElement {
    let cancel = {
        let entity = entity.clone();
        Button::new("pull-cancel")
            .label("取消")
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.close_pull_dialog(cx));
            })
    };

    let confirm = {
        let entity = entity.clone();
        Button::new("pull-ok")
            .label("开始拉取")
            .primary()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.confirm_pull(cx));
            })
    };

    v_flex()
        .w_full()
        .gap_4()
        .child(
            div().text_sm().text_color(theme::text_muted()).child(
                "镜像引用，例如 nginx:latest 或 docker.1ms.run/library/nginx:latest",
            ),
        )
        .child(Input::new(input).id("pull-reference").w_full())
        .child(
            v_flex()
                .w_full()
                .gap_1()
                .rounded_md()
                .bg(theme::bg())
                .p_3()
                .child(div().text_xs().text_color(theme::warning()).child(
                    "实测本机直连 Docker Hub 会超时（registry-1.docker.io 不可达），\
                     建议填写镜像加速地址。",
                ))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_dim())
                        .child("拉取期间界面保持可用，随时可以取消。"),
                ),
        )
        .child(
            h_flex()
                .w_full()
                .justify_end()
                .gap_2()
                .child(cancel)
                .child(confirm),
        )
        .into_any_element()
}

/// 弹窗的**进度**形态（正在拉）。
///
/// 只渲染最后 [`PROGRESS_TAIL_LINES`] 行、且**不滚动** ——
/// 拉取的输出动辄上千行，滚动容器会把用户带到最老的那几行，
/// 而这里永远显示"刚刚发生了什么"。
fn pull_progress_body(progress: &PullProgress, entity: &Entity<Shell>) -> AnyElement {
    /// 进度区显示多少行。
    const PROGRESS_TAIL_LINES: usize = 12;

    let tail: Vec<AnyElement> = progress
        .lines
        .iter()
        .skip(progress.lines.len().saturating_sub(PROGRESS_TAIL_LINES))
        .map(|line| {
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child(line.clone())
                .into_any_element()
        })
        .collect();

    let body = if tail.is_empty() {
        vec![
            div()
                .text_xs()
                .text_color(theme::text_dim())
                .child("等待输出…")
                .into_any_element(),
        ]
    } else {
        tail
    };

    let abort = {
        let entity = entity.clone();
        Button::new("pull-abort")
            .label("取消拉取")
            .small()
            .danger()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.cancel_pull(cx));
            })
    };

    v_flex()
        .w_full()
        .gap_3()
        .child(
            div()
                .text_sm()
                .text_color(theme::text())
                .child(format!("正在拉取 {} ……", progress.image)),
        )
        .child(
            v_flex()
                .w_full()
                .gap_1()
                .rounded_md()
                .bg(theme::bg())
                .p_3()
                .children(body),
        )
        .child(
            h_flex()
                .w_full()
                .justify_between()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_dim())
                        .child(format!("已收到 {} 行输出", progress.lines.len())),
                )
                .child(abort),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    // ⚠️ 这里**故意不用 `use super::*;`** —— 这是个很难查的坑，记下来：
    //
    // `views` 模块里有 `use gpui_kit::*;`，而 gpui 在 gpui.rs 里无条件再导出了
    // gpui_macros 的 `test` **属性宏**：
    //
    //     pub use gpui_macros::{..., property_test, register_action, test, ...};
    //
    // `use super::*` 会把这个名字继承进来，于是本模块里的 `#[test]`
    // 解析到 **GPUI 的 test 宏**而不是 Rust 内建的那个，展开时自我递归。
    //
    // 症状极具迷惑性：报错是
    //     error: recursion limit reached while expanding `#[test]`
    // 而且提示的递归上限会随你调高而水涨船高（128 → 512 → 1024），
    // 因为问题不是"深度不够"，而是宏被同名遮蔽了。
    //
    // 正确做法（gpui-kit 的 lib.rs 注释里也写了）：测试模块**显式导入**需要的类型。
    use super::{
        ALL_COLUMNS, IMAGE_COLUMNS, NETWORK_COLUMNS, RUNNING_COLUMNS, VOLUME_COLUMNS, presets_for,
    };
    use wslc_core::settings::{SETTING_KEYS, SettingKind};

    /// 汇总所有表格的列定义，方便逐个检查。
    fn all_column_sets() -> Vec<&'static [(&'static str, f32)]> {
        vec![
            RUNNING_COLUMNS,
            ALL_COLUMNS,
            IMAGE_COLUMNS,
            NETWORK_COLUMNS,
            VOLUME_COLUMNS,
        ]
    }

    #[test]
    fn table_columns_have_names_and_positive_widths() {
        for columns in all_column_sets() {
            for &(name, width) in columns {
                assert!(!name.is_empty(), "列名不能为空");
                assert!(width > 0.0, "列宽必须为正数：{name}");
            }
        }
    }

    #[test]
    fn every_table_has_the_expected_column_count() {
        // 行内的 cell 数量少于列数只会留下空白，多出来则会被丢弃；
        // 这里把"必须一一对应"的约束固化下来，避免改表头时忘记改行。
        let counts: Vec<usize> = all_column_sets().iter().map(|c| c.len()).collect();
        assert_eq!(counts, vec![10, 7, 5, 6, 5]);
    }

    #[test]
    fn presets_are_non_empty_for_every_key() {
        for key in SETTING_KEYS {
            for preset in presets_for(key) {
                assert!(!preset.is_empty(), "{} 的预设值不能为空", key.key);
            }
        }
    }

    #[test]
    fn enum_setting_uses_choices_instead_of_presets() {
        let cred = SETTING_KEYS
            .iter()
            .find(|k| k.key == "credentialStore")
            .expect("应存在 credentialStore");
        assert_eq!(cred.kind, SettingKind::Enum);
        assert!(presets_for(cred).is_empty());

        // 注意：不要写成 `assert_eq!(cred.choices, &["wincred", "file"])`。
        // `&[&str]` 与 `&[&str; N]` 的比较会让编译器展开出一大堆引用/去 Sized 强制转换，
        // 在 `#[test]` 里表现为 "recursion limit reached while expanding #[test]"。
        // 统一转成 `Vec` 再比，类型简单、诊断也清楚。
        let choices: Vec<&str> = cred.choices.to_vec();
        assert_eq!(choices, vec!["wincred", "file"]);
    }

    #[test]
    fn numeric_settings_offer_presets() {
        let cpu = SETTING_KEYS.iter().find(|k| k.key == "cpuCount").unwrap();
        let cpu_presets: Vec<&str> = presets_for(cpu).to_vec();
        assert_eq!(cpu_presets, vec!["4", "8", "16"]);

        let idle = SETTING_KEYS
            .iter()
            .find(|k| k.key == "idleTimeout")
            .unwrap();
        let idle_presets: Vec<&str> = presets_for(idle).to_vec();
        assert_eq!(idle_presets, vec!["30", "60", "300"]);
    }
}
