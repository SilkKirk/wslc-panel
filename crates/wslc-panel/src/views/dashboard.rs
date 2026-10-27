//! ① 基本信息（总览）页：环境卡片 + 四只圆环 + 总览数字。

use super::common::{card, cell_full};
use super::prelude::*;

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

/// 基本信息页：**这台机器上的 WSL 环境** + 一排总览圆环。
///
/// # 这一页回答什么
///
/// 1. **这套 WSL 是什么版本**（`wslc info`）—— 报问题时第一步要抄的；
/// 2. **容器这一摊现在占了多少**（CPU / 内存 / 存储 / 数量）—— 四只圆环。
///
/// # 为什么是一排圆环而不是一张每容器的表（v0.9.17）
///
/// v0.9.16 这里还是一张"运行中容器（实时）"的表，每行一个容器的
/// CPU / 内存 / 网络 / 进程数。实机反馈是**不需要看到每个容器** ——
/// 想知道的是"总共占了多少"，而且要 1Panel 那种一眼的总览。
///
/// 于是：表整个去掉（要看单个容器去「容器」页），换成四只环。
/// 这也顺手解掉了一个老问题：那一列 `docker.1panel.live/library...`
/// 之类的长镜像名在窄表里必然被截断，而环不需要那么宽。
///
/// # 四个环的口径
///
/// 数字全部来自 [`DashboardStats`]（**纯算术**，这一页自己不起任何进程），
/// 每一环下面那行小字就是口径本身，卡片底部还有一句总说明 ——
/// 口径写清楚是刻意的：`wslc stats` 的 CPU% 是 **100% ≈ 1 个核**，
/// 不折算的话 32 核机器上那几个小数会把人看糊涂。
pub(crate) fn dashboard_page(state: &AppState) -> impl IntoElement {
    let stats = state.snapshot.dashboard_stats();

    v_flex()
        .w_full()
        .gap_4()
        .child(environment_card(state))
        .child(status_card(state, &stats))
        .child(overview_tiles(state))
}

// ---------------------------------------------------------------------------
// 基本信息页的三块
// ---------------------------------------------------------------------------

/// 运行环境：`wslc info` 里**排障真正要用到**的那三项。
///
/// 横着排三格而不是竖着列三行：这一页的高度要留给下面那排圆环，
/// 而三项都很短（版本号），铺满一行正好。
///
/// 刻意**不显示** `Direct3D` / `DXCore`：它们是给 GPU 直通 / 计算负载看的，
/// 和"容器跑不跑得起来"没关系 —— 以前它们和版本号并排摆着，
/// 用户得自己判断哪个重要。
fn environment_card(state: &AppState) -> AnyElement {
    let Some(info) = state.snapshot.info.as_ref() else {
        // 首屏（或 `wslc info` 失败）时给一句话，而不是三个空框。
        // 失败原因由 `app/` 的 `error_banner` 统一说，这里只解释这块为什么空着。
        return card(
            "运行环境",
            div()
                .text_sm()
                .text_color(theme::text_dim())
                .child("还没读到 wslc 的环境信息。"),
        )
        .into_any_element();
    };

    card(
        "运行环境",
        h_flex()
            .w_full()
            .gap_4()
            .items_start()
            .child(env_cell("WSL 版本", info.client.version.clone()))
            .child(env_cell("内核版本", info.client.kernel_version.clone()))
            .child(env_cell("Windows", info.client.windows_version.clone())),
    )
    .into_any_element()
}

/// 「运行环境」里的一格：小标签 + 值。
///
/// 值比正文大一档（`text_lg` + 半粗）：这三项是**要抄下来贴给别人**的
/// （报问题的第一步就是报 WSL / 内核版本），大一点好读。
///
/// 仍然加 `truncate`：Windows 版本在别的机器上可能是
/// `10.0.26100.1742 (Win11 24H2)` 这种带括号的，不指望它一定放得下。
fn env_cell(label: &'static str, value: String) -> impl IntoElement {
    v_flex()
        .flex_1()
        .min_w_0()
        .gap_1()
        .child(div().text_xs().text_color(theme::text_dim()).child(label))
        .child(
            div()
                .text_lg()
                .font_semibold()
                .text_color(theme::text())
                .overflow_hidden()
                .truncate()
                .child(value),
        )
}

/// 圆环的**传参**直径。
///
/// ⚠️ `ProgressCircle` 实际画出来的是这个数的 **0.75 倍**
/// （组件库 `Size::Size(s) => this.size(s * 0.75)`）—— 写 170，屏幕上量到的是
/// 127.5px。这个数没在实机上校过，是照 1Panel 那排环的比例估的：
/// `AGENTS.md` §8 那条"实机看一眼"照旧成立（尤其是窗口窄到 880px 时）。
const GAUGE_DIAMETER: f32 = 170.0;

/// 一个圆环：环里是"大数字 + 标签"，环下面一行小字写口径。
///
/// `value` 是 0–100 的百分比；`None` 表示**这一项读不到** —— 那时候环画成空的、
/// 大数字写 `—`。不能把"读不到"画成 0%：两者在环上长得一模一样，
/// 而"占用 0"和"没读到"对用户是两件事。
fn gauge(
    id: &'static str,
    value: Option<f64>,
    color: Rgba,
    big: String,
    label: &'static str,
    caption: String,
) -> AnyElement {
    // `Hsla` 要显式标注：`Rgba -> Hsla` 是 `From` 实现出来的，
    // 而参数类型是 `impl Into<Hsla>`，`rgba.into()` 在那儿推不出目标类型
    // （theme.rs 里那几行也是这么写的）。
    let color: Hsla = color.into();

    v_flex()
        .flex_1()
        .min_w_0()
        .items_center()
        .gap_2()
        .child(
            ProgressCircle::new(id)
                // `.value()` 自己会 clamp 到 0–100，这里先夹一次是为了
                // 让"读不到"和"超出"在代码里就是同一个 0。
                .value(value.unwrap_or(0.0).clamp(0.0, 100.0) as f32)
                .color(color)
                // ⚠️ 必须用 `.with_size()`（`Sizable`），**不能**用 Styled 的
                // `.size()`：组件库 render 里是先 `refine_style`（应用 Styled）
                // 再按 `Size` 覆盖尺寸的，`.size()` 会被它盖掉。
                .with_size(px(GAUGE_DIAMETER))
                .child(
                    v_flex()
                        .items_center()
                        .gap_1()
                        .child(
                            div()
                                .text_lg()
                                .font_bold()
                                .text_color(theme::text())
                                .child(big),
                        )
                        .child(div().text_xs().text_color(theme::text_dim()).child(label)),
                ),
        )
        .child(
            // ⚠️ 这行小字要**居中**，不能只写个 `w_full` 就完事：这一格是
            // `flex_1` 的一整列，文本默认左对齐 —— 环在格子正中、字却贴着
            // 格子左边，两行对不上（实机反馈："下面的字有点歪了"）。
            // 用 `justify_center` 而不是 `text_center()`：前者本仓库到处在用，
            // 后者一次都没用过 —— 本机没工具链，不赌没验证过的 API（AGENTS.md §1）。
            // `overflow_hidden` 兜住"比格子还长"的那种小字（居中裁掉两头，不越界）。
            h_flex().w_full().justify_center().overflow_hidden().child(
                div()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(caption),
            ),
        )
        .into_any_element()
}

/// 用量 → 环的颜色。
///
/// 阈值判定（≥60% 黄、≥85% 红）在 `wslc-panel-core` 里带单测 ——
/// 这个文件里的断言 CI 不会跑。拿不到值时按"轻载"上色（不吓人）。
fn gauge_color(percent: Option<f64>) -> Rgba {
    match load_level(percent.unwrap_or(0.0)) {
        LoadLevel::Low => theme::primary(),
        LoadLevel::Medium => theme::warning(),
        LoadLevel::High => theme::danger(),
    }
}

/// 环里那个大数字：百分比两位小数；读不到就是破折号。
fn gauge_big(percent: Option<f64>) -> String {
    match percent {
        Some(value) => format!("{value:.2} %"),
        None => "—".to_owned(),
    }
}

/// 「状态」卡：四只圆环 —— 容器 CPU / 容器内存 / 会话盘 / 容器数量。
///
/// 顺序：CPU → 内存 → 容器 → 存储。前三只都是"容器这一摊"的（CPU / 内存 / 数量），
/// 存储单独摆在最后 —— 实机反馈就是把存储和容器调了个个儿。
fn status_card(state: &AppState, stats: &DashboardStats) -> AnyElement {
    let cpu_percent = stats.cpu_whole_machine_percent();
    let mem_percent = stats.mem_percent();
    let disk_percent = stats.vhd_percent_of_disk();
    let has_running = stats.running > 0;

    // 数量那一环：全部为 0 时没有"比例"可言，`None` 让环空着。
    //
    // 口径在 `DashboardStats::container_percent`（带测试）—— 和另外三个环
    // 同一个出处。原先这里手写了一遍 `running / total * 100`，而 `views/`
    // 不在任何测试目标里（见 `AGENTS.md` §2），写错了不会有信号。
    let count_percent = stats.container_percent();

    // CPU 环：拿不到核数时**照样报原始合计**（那一行小字会说明"拿不到核数"），
    // 但绝不把它当"整机占用率"—— 多核机器上 400% 是四个核跑满。
    let cpu_big = match (cpu_percent, has_running) {
        (Some(percent), _) => gauge_big(Some(percent)),
        (None, true) => format!("{:.2} %", stats.cpu_percent),
        (None, false) => gauge_big(None),
    };
    let cpu_caption = match (stats.cpu_cores, has_running) {
        (Some(cores), true) => format!("{:.2} / {cores} 核", stats.cpu_cores_used()),
        (None, true) => "各容器之和（拿不到核数）".to_owned(),
        _ => "没有正在运行的容器".to_owned(),
    };

    let mem_caption = match (has_running, stats.mem_limit) {
        (true, Some(limit)) => {
            format!("{} / {}", format_bytes(stats.mem_used), format_bytes(limit))
        }
        _ => "没有正在运行的容器".to_owned(),
    };

    // 存储那一环：分子是**会话 VHD** 的实际占用，分母是所在盘总量
    // （"容器这一摊占了这块盘多少"）。缺哪一半都不写数字 —— 一个比例只有
    // 分子或只有分母都是编的，不如写清楚缺什么。
    let disk_caption = match (stats.vhd_bytes, stats.disk) {
        (Some(vhd), Some(disk)) => {
            format!("{} / {}", format_bytes(vhd), format_bytes(disk.total))
        }
        (None, Some(disk)) => format!("VHD 还没建出来 / {}", format_bytes(disk.total)),
        (Some(vhd), None) => format!("{}（拿不到盘总量）", format_bytes(vhd)),
        (None, None) => "拿不到会话存储信息".to_owned(),
    };

    let (count_big, count_caption) = if stats.total > 0 {
        (
            format!("{} / {}", stats.running, stats.total),
            "运行中 / 全部".to_owned(),
        )
    } else {
        ("0 / 0".to_owned(), "还没有容器".to_owned())
    };

    // 底部那句总口径。**刻意写在这里而不是每一环里**：四条小字已经
    // 各说各的数字了，再各自加一句"这是什么"会把环淹掉。
    // v0.9.22 把原来那三行压成一句，腾出的位置给下面的「会话存储」——
    // 那一条回答的是"这些数字对应哪个文件、盘还剩多少"，比重复口径有用。
    let note = "口径：只算容器；CPU 按核数折算；存储是会话 VHD 占所在盘的比例。";

    let storage_line: AnyElement = match state.snapshot.storage.as_ref() {
        None => div()
            .w_full()
            .text_xs()
            .text_color(theme::text_dim())
            .child("会话存储：拿不到（LOCALAPPDATA / storagePath 未设置？）")
            .into_any_element(),
        Some(storage) => {
            let path = storage
                .vhd
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| storage.sessions_dir.display().to_string());
            let free = storage
                .volume
                .map(|volume| format_bytes(volume.free))
                .unwrap_or_else(|| "—".to_owned());
            let mut extra = String::new();
            if storage.has_other_sessions() {
                // `sessions_count` 数的是"找到几个 storage.vhdx"；当前会话的
                // VHD 还没建出来时（`vhd_bytes == None`），找到的全都是"其它"。
                let others = if storage.vhd_bytes.is_some() {
                    storage.sessions_count.saturating_sub(1)
                } else {
                    storage.sessions_count
                };
                if others > 0 {
                    extra = format!(
                        " · 另有 {others} 个会话占 {}",
                        format_bytes(storage.other_sessions_bytes())
                    );
                }
            }
            let used = storage
                .vhd_bytes
                .map(format_bytes)
                .unwrap_or_else(|| "—".to_owned());
            // 数字放在前面，长路径只进 tooltip，窄窗口也先读到盘剩余与会话占用。
            let summary = format!("盘剩余 {free} · 当前会话 {used}{extra}");
            let tip = format!("会话存储路径：{path}\n{summary}");
            cell_full("session-storage", summary, true, Some(tip.into()))
        }
    };

    card(
        "状态",
        v_flex()
            .w_full()
            .gap_3()
            .child(
                h_flex()
                    .w_full()
                    .gap_3()
                    .items_start()
                    .child(gauge(
                        "gauge-container-cpu",
                        cpu_percent,
                        gauge_color(cpu_percent),
                        cpu_big,
                        "CPU",
                        cpu_caption,
                    ))
                    .child(gauge(
                        "gauge-container-memory",
                        mem_percent,
                        gauge_color(mem_percent),
                        gauge_big(mem_percent),
                        "内存",
                        mem_caption,
                    ))
                    .child(gauge(
                        "gauge-container-count",
                        count_percent,
                        // 数量不是"用量"，别让它在 85% 时变红 ——
                        // 「4 个容器全在跑」是好事，不是告警。
                        theme::success(),
                        count_big,
                        "容器",
                        count_caption,
                    ))
                    .child(gauge(
                        "gauge-session-disk",
                        disk_percent,
                        gauge_color(disk_percent),
                        gauge_big(disk_percent),
                        "存储",
                        disk_caption,
                    )),
            )
            .child(storage_line)
            .child(div().text_xs().text_color(theme::text_dim()).child(note)),
    )
    .into_any_element()
}

/// 底部那排数字：镜像 / 网络 / 卷 / WSL 实例。
///
/// 容器的数量**不在这里** —— 它已经是上面那一环了，摆两遍只会让人
/// 犹豫该看哪个（`AGENTS.md` §5 同一条道理）。这一排回答的是"我这儿
/// 有多少东西"，想看清单一页一个，所以刻意**不可点**。
fn overview_tiles(state: &AppState) -> impl IntoElement {
    let snap = &state.snapshot;

    h_flex()
        .w_full()
        .gap_3()
        .child(stat_tile(
            "WSL 实例",
            snap.distros.len().to_string(),
            theme::text(),
        ))
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
        ))
}
