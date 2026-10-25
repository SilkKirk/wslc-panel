//! 容器详情 / 发行版详情两个浮层。

use super::common::{
    ModalSize, danger_button, danger_button_distro, immediate_button, kv, kv_block, modal_card,
    modal_scrim, overlay_button, prompt_button,
};
use super::prelude::*;

/// 容器详情弹窗。
///
/// 挂载、端口这些占地方的信息在这儿看全；动作只放**列表行里没有**的那几个
/// （强杀 / 执行命令 / attach 主进程），高频动作留在列表行。
///
/// ⚠️ 这里原先写着"列表里不放操作按钮，操作按钮集中在这儿" —— 那是照 1Panel
/// 写的，但本项目的列表行一直有操作按钮（启动 / 重启 / 停止 / 日志 / 删除），
/// 于是详情里又摆了一份一模一样的，实机上看起来就是"同一排按钮出现两次"。
/// 现在按"不重复"来分：列表放高频的，详情放低频或危险的。
///
/// ⚠️ v0.9.14 起列表行自己又分了两层（行内 4 个 + 「更多」菜单），
/// 这份"不重复"的账要按 `container_row_actions` / `container_more_menu`
/// 那两个函数来算 —— 终端也在那一行的「更多」里，所以详情里照样没有它。
///
/// 挂载用 `kv_block`（换行不截断）：`E:\code → /etc/nginx/conf.d/`
/// 这种长路径用 `kv` 会被截成 `E:\...\con...`，等于没显示。
pub fn container_detail_overlay(
    name: &str,
    state: &AppState,
    entity: &Entity<Shell>,
) -> AnyElement {
    let Some(summary) = state
        .snapshot
        .all
        .iter()
        .find(|c| c.item.display_name() == name)
    else {
        // 容器刚被删掉 —— 弹窗自己消失，别留一个空壳
        return div().into_any_element();
    };

    let item = &summary.item;
    let running = item.is_running();

    // -- 动作 --------------------------------------------------------------
    //
    // ⚠️ **列表行里已经有的动作，这里一个都不重复。**
    //
    // 容器列表那一行给的是高频动作：**启动 / 停止**在行内，
    // **终端 / 重启 / 日志 / 删除** 在那一行的「更多」菜单里（v0.9.14 起）。
    // 详情里再摆一遍没有意义 —— 同一个动作两个入口，用户还得判断该点哪个，
    // 而两个入口背后是同一个 `ImmediateAction` / `PendingAction`，行为完全一样。
    // （这一点是**实机看出来的**：发布版里停止的容器在列表行和详情里各有一份
    // 一模一样的「启动 / 日志 / 删除」。）
    //
    // ⚠️ v0.6.3：「打开终端」从详情**挪到了列表行**。理由是实测的使用路径 ——
    // 建完容器想进去干活时，得"点名字开详情 → 找按钮"两次点击，而它其实是
    // 运行中的容器最高频的动作。挪上去之后这里就**不再重复**它了
    // （`AGENTS.md` §5 那条规矩）。
    //
    // 详情留给**列表里没有、而且低频或危险**的那几个：强杀（列表里只有优雅停止）、
    // 执行命令（非交互）、attach 主进程。
    //
    // 所以**停止的容器在详情里一个动作都没有** —— 那是对的，它能做的几件事
    // 列表行里全有。下面给一句话说明，免得看起来像按钮坏了。
    //
    // 发行版详情是同一条规矩（那边不重复 打开终端 / 启动 / 终止 / 设为默认）。
    let mut actions: Vec<AnyElement> = Vec::new();
    if running {
        actions.push(
            danger_button(
                "detail-kill",
                "强杀",
                PendingAction::KillContainer(name.to_owned()),
                entity,
            )
            .into_any_element(),
        );
        actions.push(overlay_button(
            "detail-exec",
            "执行命令",
            {
                let target = name.to_owned();
                move |shell, window, cx| shell.open_exec(target.clone(), window, cx)
            },
            entity,
        ));
        // `attach` 是"开一个新终端窗口"——同步调用，不等它关闭
        // （用户可能在里面待几个小时，见 `AGENTS.md` §7.1）。
        actions.push(
            immediate_button(
                "detail-attach",
                "attach 主进程",
                ImmediateAction::AttachContainer(name.to_owned()),
                entity,
            )
            .into_any_element(),
        );
    }

    // 一行说明：其余动作为什么不在这儿。
    // 没有它，用户会以为详情里少了按钮（或者按钮没画出来）。
    //
    // ⚠️ v0.9.14 起列表行里多了一层「更多」菜单 —— 这句话必须**指到菜单**
    // （终端 / 重启 / 日志 / 删除 都在那儿），否则用户按老习惯去行内找，
    // 找了一圈只看到"启动/停止"，会以为功能被删了。
    let hint = if running {
        "终端 / 重启 / 日志 / 删除 在列表那一行的「更多」菜单里（停止在行内）"
    } else {
        "这个容器已经停止 —— 启动在行内，日志 / 删除 在那一行的「更多」菜单里"
    };

    let close = {
        let entity = entity.clone();
        Button::new("detail-close")
            .label("关闭")
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.close_detail(cx));
            })
    };

    // 端口在这里显示**完整形式**（含绑定地址），和列表里的精简形式区分开：
    // 列表看的是"映射了多少"，详情看的是"到底绑在哪"。
    let ports_text = {
        let ports = item.port_mappings();
        if ports.is_empty() {
            "—".to_owned()
        } else {
            ports
                .iter()
                .map(|p| p.display())
                .collect::<Vec<_>>()
                .join("  ")
        }
    };

    // -- inspect 那一份（挂载 / 启动命令 / 环境变量**只有它有**）----------------
    //
    // ⚠️ 这三样在列表那一份数据里都没有：`list` 的 `Command` 被 wslc **截到 20 字符**
    //（`"docker-entrypoint.s…"`）、挂载只有卷名（`mysqldata`，没有容器内路径），
    // 环境变量则完全没有。所以打开详情时后台拉一次 inspect
    //（`Shell::open_detail` → `DetailInspect`）。
    let detail = state
        .detail_inspect
        .as_ref()
        .filter(|d| d.container == name);
    let inspect = detail.and_then(|d| d.inspect.as_ref());
    let inspect_error = detail.and_then(|d| d.error.as_deref());

    // 一项"只有 inspect 才知道"的信息。**三种状态分开画**，别都塌成 `—`：
    //   还没拉回来 → "正在读取…"（塌成 `—` 的话用户会以为这容器就没有）
    //   拉失败     → 把原因说出来（inspect 失败多半是容器刚被删掉）
    //   拉到了但空 → `—`（这才真的是"没有"）
    let inspect_field = |label: &'static str, value: Option<String>| -> AnyElement {
        if let Some(text) = value {
            return kv_block(label, text).into_any_element();
        }
        if inspect.is_some() {
            return kv(label, "—").into_any_element();
        }
        match inspect_error {
            Some(error) => kv_block(label, format!("读不到：{error}")).into_any_element(),
            None => kv(label, "正在读取…").into_any_element(),
        }
    };

    // 启动命令：`Entrypoint` + `Cmd` 接起来 —— 运行时就是这么拼的。
    // 只有 `Entrypoint` 的镜像（`-jre-alpine` 那类）在这里一眼看得出来。
    let command_line = inspect.and_then(|i| i.command_line());
    // 挂载：**优先 inspect**（列表 / labels 那条路实测是空的，
    // 见 `ContainerInspect::mounts_summary` 的表格）；拿不到才回落到旧的摘要。
    let mounts = inspect
        .and_then(|i| i.mounts_summary())
        .or_else(|| item.mounts_summary());
    // 环境变量：一行一条（`kv_block` 换行显示，卡片自己有 `overflow_y_scroll`）。
    //
    // ⚠️ 值**原样显示**，包括 `MYSQL_ROOT_PASSWORD=…` 这类：和「数据库管理」页
    // 把密码画在列表里一致，也和 Docker Desktop 的 inspect 一致 —— 这是用户
    // 自己机器上的屏幕，来查 env 的人要的就是值本身。
    // 要改成"密码类键打码、点一下才显示"的话，只改这一处的取值即可。
    let env = inspect
        .map(|i| i.env())
        .filter(|lines| !lines.is_empty())
        .map(|lines| lines.join("\n"));

    let networks_text = if item.networks.trim().is_empty() {
        "—".to_owned()
    } else {
        item.networks.clone()
    };
    let size_text = if item.size.trim().is_empty() {
        "—".to_owned()
    } else {
        item.size.clone()
    };

    let mut rows: Vec<AnyElement> = vec![
        kv("ID", item.id().to_owned()).into_any_element(),
        kv("镜像", item.image.clone()).into_any_element(),
        kv("状态", item.status.clone()).into_any_element(),
        // ⚠️ 这一格是 `RunningFor`，实测是**创建至今**（三个容器都对得上 `CreatedAt`），
        // 不是运行时长 —— 真正的 uptime 就在上一行 `Status` 里（`Up 2 hours`）。
        // 原先标成「运行时长」，于是"状态 Up 2 hours"和"运行时长 45 hours ago"
        // 并排出现、看着自相矛盾（v0.9.7 实机反馈）。
        kv("已创建", item.running_for.clone()).into_any_element(),
        // `RunningFor` 是相对时间，给一个绝对时间对照 —— 排查"它到底哪会儿建的"时
        // 相对时间不够用。两个标签分开写，别让用户以为是一个东西。
        kv("创建时间", item.created_at.clone()).into_any_element(),
        kv("网络", networks_text).into_any_element(),
        // `Size` 是 wslc 列表给的容器大小（不是镜像大小）。标签写「容器大小」
        // 是为了不和上面的镜像/下面的挂载混淆；不承诺它精确等于可写层。
        kv("容器大小", size_text).into_any_element(),
        kv_block("端口", ports_text).into_any_element(),
        inspect_field("挂载", mounts),
        inspect_field("启动命令", command_line),
        inspect_field("环境变量", env),
    ];

    // 健康检查为空 = 这个镜像没定义 HEALTHCHECK；非空就原样报出来
    // （`healthy` 在列表里被刻意隐去，详情里要能看到事实）。
    if !item.health_status.trim().is_empty() {
        rows.insert(
            3,
            kv("健康检查", item.health_status.clone()).into_any_element(),
        );
    }

    if let Some(stats) = summary.stats.as_ref() {
        rows.push(kv("CPU", stats.cpu_perc.clone()).into_any_element());
        rows.push(kv("内存", stats.mem_usage.clone()).into_any_element());
        rows.push(kv("网络 I/O", stats.net_io.clone()).into_any_element());
        rows.push(kv("PID", stats.pids.to_string()).into_any_element());
    }

    modal_scrim(
        "container-detail",
        modal_card(ModalSize::Medium)
            .id("detail-card")
            .max_h(px(860.))
            .overflow_y_scroll()
            .child(
                div()
                    .text_lg()
                    .font_bold()
                    .text_color(theme::text())
                    .child(name.to_owned()),
            )
            .child(v_flex().w_full().gap_2().children(rows))
            .child(
                v_flex()
                    .w_full()
                    .gap_2()
                    // 先说明"其余动作在列表里"，再是这一页独有的那几个。
                    // 顺序反过来的话，停止的容器那一条会只剩右下角一个「关闭」，
                    // 看起来像界面坏了。
                    .child(div().text_xs().text_color(theme::text_dim()).child(hint))
                    .child(
                        h_flex()
                            .w_full()
                            .justify_between()
                            .items_center()
                            .child(h_flex().gap_2().flex_wrap().children(actions))
                            .child(close),
                    ),
            ),
    )
    .into_any_element()
}

/// 发行版详情弹窗。
///
/// **低频或危险**的动作在这儿：压缩 / 移动 / 调整大小 / 导出 / 删除，
/// 外加只读的「打开安装位置」「编辑配置」。高频动作（打开终端 / 启动 /
/// 终止 / 设为默认）只在列表行里 —— 同一件事给两个入口，用户还得判断点哪个。
/// 和容器详情同一套取舍。
///
/// 「安装位置」「虚拟磁盘」用 `kv_block`（换行不截断）：
/// `D:\linux\Ubuntu-26.04` 这种路径在列表里被截成 `D:\linux\Ubu...`，
/// 等于没显示，而这里正是要看清它。
///
/// 另外这里**替列表行承担了一件事**：如果这个发行版正被本面板吊着
/// （`state.kept_alive`），在「状态」下面加一句说明 —— 那个标记原先画在
/// 列表行的状态列里，会溢到隔壁「版本」列上（见 [`distro_row`]）。
pub fn distro_detail_overlay(name: &str, state: &AppState, entity: &Entity<Shell>) -> AnyElement {
    let Some(distro) = state.snapshot.distros.iter().find(|d| d.name == name) else {
        // 发行版刚被删掉 —— 弹窗自己消失，别留一个空壳
        return div().into_any_element();
    };

    let is_default = state.default_distro() == Some(distro.name.as_str());
    let running = distro.state.is_running();
    // 过渡态（安装 / 卸载 / 转换中）下 `wsl` 会拒绝写操作，
    // 所以那几种状态下干脆不显示写操作按钮。
    let transitional = distro.state.is_transitional();
    // 本项目**只支持 WSL 2**。正常的机器上这永远是 false；
    // 但如果真有个 WSL 1 发行版，它的磁盘操作（VHDX 那套）全都不成立，
    // 与其让用户点了看报错，不如不给按钮 + 明说原因。
    let is_wsl1 = distro.version == Some(1);
    // 「它现在是不是被**本面板**吊着的」（[`KeepAlive`](crate::app)）。
    //
    // 这件事原先标在列表行的状态列里，实机发现那个徽标会盖到隔壁「版本」列上
    // （见 `distro_row`）。它是低频信息 —— 只在点了「启动」之后才成立 ——
    // 所以搬到这儿来，列表行只留状态徽标。
    let kept = state.kept_alive.iter().any(|n| n == name);

    // -- 动作 --------------------------------------------------------------

    // ⚠️ **列表行里已经有的动作，这里一个都不重复。**
    //
    // 发行版列表那一行给的是：打开终端 / 启动 / 终止 / 设为默认。
    // 详情留给"低频、危险、需要看清代价"的操作：压缩 / 移动 / 调整大小 /
    // 导出 / 删除，外加只读的「打开安装位置」和「编辑配置」。
    //
    // 「设为默认」原先**两边都有** —— 而这一行注释当时只列了
    // 「打开终端 / 启动 / 终止」，把它漏了。实机看出来之后删掉：
    // 它的条件（`!is_default && !transitional`）和列表行那一份完全一样，
    // 删掉不丢任何能力。
    // 详情页本身就不挤，低频动作直接摆出来；只有容器列表行才需要「更多」。
    let mut actions: Vec<AnyElement> = Vec::new();
    if !transitional {
        if !running && !is_wsl1 {
            actions.push(
                danger_button_distro(
                    "distro-detail-compact",
                    "压缩磁盘",
                    DistroAction::Compact(name.to_owned()),
                    entity,
                )
                .into_any_element(),
            );
        }
        actions.push(prompt_button(
            "distro-detail-move",
            "移动位置",
            PromptKind::MoveDistro,
            name,
            entity,
        ));
        if !is_wsl1 {
            actions.push(prompt_button(
                "distro-detail-resize",
                "调整大小",
                PromptKind::ResizeDistro,
                name,
                entity,
            ));
        }
        actions.push(prompt_button(
            "distro-detail-export",
            "导出",
            PromptKind::ExportDistro,
            name,
            entity,
        ));
        actions.push(
            danger_button_distro(
                "distro-detail-unregister",
                "删除",
                DistroAction::Unregister {
                    name: name.to_owned(),
                    vhdx_bytes: distro.vhdx_bytes,
                },
                entity,
            )
            .into_any_element(),
        );
    }

    // 「打开安装位置」是**只读**操作，所以和其他动作分开摆到左边。
    let reveal = {
        let entity = entity.clone();
        let target = name.to_owned();
        Button::new("distro-detail-reveal")
            .label("打开安装位置")
            .small()
            .on_click(move |_, _, cx| {
                let target = target.clone();
                entity.update(cx, |shell, cx| shell.reveal_distro_path(target, cx));
            })
    };

    // 「编辑配置」也是这一组：它读/写的是发行版**里面**的 `/etc/wsl.conf`，
    // 不改发行版本身的注册信息。
    //
    // 注意：这个文件在 `.wslconfig`（`%USERPROFILE%`，整机一份）里**不存在**
    // —— 见 `wslc_core::wslconfig` 的说明。所以入口摆在发行版详情里，
    // 而不是「WSL 配置」页（那一页管的是前者）。
    let edit_conf = {
        let entity = entity.clone();
        let target = name.to_owned();
        Button::new("distro-detail-wslconf")
            .label("编辑配置")
            .tooltip("编辑发行版内的 /etc/wsl.conf")
            .small()
            .on_click(move |_, window, cx| {
                let target = target.clone();
                entity.update(cx, |shell, cx| shell.open_wslconf(target, window, cx));
            })
    };

    let close = {
        let entity = entity.clone();
        Button::new("distro-detail-close")
            .label("关闭")
            .small()
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| shell.close_distro_detail(cx));
            })
    };

    // -- 信息 --------------------------------------------------------------

    let location = distro
        .base_path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "—".to_owned());
    let vhdx = distro
        .vhdx_path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "—".to_owned());
    let disk = distro
        .vhdx_bytes
        .map(format_bytes)
        .unwrap_or_else(|| "—".to_owned());
    let uid = distro
        .default_uid
        .map(|u| u.to_string())
        .unwrap_or_else(|| "—".to_owned());

    let mut rows: Vec<AnyElement> = vec![
        kv("状态", distro.state.label()).into_any_element(),
        kv("WSL 版本", distro.version_label()).into_any_element(),
        kv("默认发行版", if is_default { "是" } else { "否" }).into_any_element(),
        kv("默认用户 UID", uid).into_any_element(),
        kv("磁盘（虚拟）", disk).into_any_element(),
        kv_block("安装位置", location).into_any_element(),
        kv_block("虚拟磁盘", vhdx).into_any_element(),
    ];

    if is_wsl1 {
        rows.push(
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child(
                    "⚠️ 这是 WSL 1 发行版。本程序只支持 WSL 2，\
                     所以压缩 / 稀疏 / 调整大小这些磁盘操作对它不适用，已经隐藏。",
                )
                .into_any_element(),
        );
    }

    if kept {
        // 插在「状态」下面而不是追到末尾：它解释的正是上面那个「运行中」——
        // 为什么一个没人用的发行版会一直跑着。文案里要写清**关掉面板之后**
        // 会怎样，那是用户拿不到答案、只能猜的那一半。
        rows.insert(
            1,
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child(
                    "它现在由本面板吊住（点了「启动」）。关掉本面板它不会跟着退出，\
                     发行版会继续跑；要让它停下来，用「终止」。",
                )
                .into_any_element(),
        );
    }

    let mut buttons: Vec<AnyElement> =
        vec![reveal.into_any_element(), edit_conf.into_any_element()];
    buttons.extend(actions);

    modal_scrim(
        "distro-detail",
        modal_card(ModalSize::Wide)
            .id("distro-detail-card")
            .max_h(px(860.))
            .overflow_y_scroll()
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_lg()
                            .font_bold()
                            .text_color(theme::text())
                            .child(name.to_owned()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::text_dim())
                            .child("磁盘维护、导出与删除都在这里；删除不可撤销。"),
                    ),
            )
            .child(v_flex().w_full().gap_2().children(rows))
            .child(
                v_flex()
                    .w_full()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .font_semibold()
                            .text_color(theme::text_dim())
                            .child("操作"),
                    )
                    .child(h_flex().w_full().gap_2().flex_wrap().children(buttons)),
            )
            .child(h_flex().w_full().justify_end().child(close)),
    )
    .into_any_element()
}
