//! 单步执行与"再试一次"：起一条 `wsl.exe` 命令，以及三档重试策略。
//!
//! 三种重试各自认一个标记（都是 [`PlannedStep`] 上的字段），见 [`run_wsl_step`]。
//! 单独一个文件，是因为"什么时候该重试、重试时改哪个参数"本身是一套策略，
//! 混在主循环里会被 `emit` 与结果分派淹没。
//!
//! 与 `install.rs` 同一模块树（`install.rs` + `install/` 目录并存），
//! 所以 `use super::*;` 就能拿到事件类型、`emit`、`arg`、`sleep_cancellable` 等。

use super::*;

/// 起一个 `wsl.exe` 步骤，边跑边读。
///
/// 三种"再试一次"都挂在这一层，各自认一个标记：
///
/// 1. [`PlannedStep::retry_while_busy`]（「按 .wslconfig 开稀疏盘」那一步）——
///    失败只是"VHD 还被 WSL 占着"时等一下再跑同一条命令；
/// 2. [`PlannedStep::retry_other_source`]（只有在线安装那一步）—— **换一个下载源**
///    （微软商店 ⇄ `--web-download`）：两条通道走的是不同的下载实现，
///    实测本机上一条秒失败、另一条能连上，换一次比让用户自己猜好；
/// 3. [`PlannedStep::retry_other_id`]—— 还不行就**换一个 id**
///    （裸 `Ubuntu` → `Ubuntu-26.04`）。换成功时返回
///    `Ok(Some(真正装上的那个 id))` —— 调用方要拿它去改后面步骤的参数。
pub(super) fn run_wsl_step(
    wsl: &Wsl,
    step: &PlannedStep,
    opts: &RunOptions,
) -> Result<Option<String>> {
    let mut first = run_wsl_once(wsl, &step.args, step.cancellable, opts);

    // ① 「资源还被占着」→ 等一下再跑同一条命令（见上面第 1 条）。
    if step.retry_while_busy {
        first = retry_step_while_busy(wsl, step, opts, first);
    }

    if first.is_ok() {
        return Ok(None);
    }

    // 用户**主动取消**时到此为止。
    //
    // "换源 / 换 id"都属于"再试一次"，而取消的意思正是"别试了"。
    // 不挡的话会依次发生：emit("换一个下载源再试一次") → 再 spawn 一次
    // （立刻又被 `finish_stream` 的取消检查杀掉）→ 换 id 再 spawn 一次 →
    // emit("这几条都没走通，可以改用国内镜像源")。结果是用户按下取消，
    // 看到的却是三条"网络不通"的建议 —— 而日志是安装失败后**唯一**的
    // 排查依据，等于主动往里灌错误结论。附带代价是多起两个进程。
    //
    // 匹配用 `&first`：按引用匹配不会移动它，下面还要 `first.map(...)` 还回去。
    if matches!(&first, Err(Error::Cancelled { .. })) {
        return first.map(|()| None);
    }

    if !step.retry_other_source && step.retry_other_id.is_none() {
        return first.map(|()| None);
    }

    let Some(args) = args_with_source_toggled(&step.args) else {
        return first.map(|()| None);
    };
    emit(
        opts,
        InstallEvent::Line("这一次没连上 —— 换一个下载源再试一次（微软商店 ⇄ GitHub）".to_owned()),
    );
    let retried = run_wsl_once(wsl, &args, step.cancellable, opts);
    if retried.is_ok() {
        return Ok(None);
    }

    // ③ 还不行就**换 id**：清单里排第一的裸家族名（`Ubuntu`）是旧的商店包，
    //    实测本机 2 秒就 `WININET_E_CANNOT_CONNECT`，而同族的 `Ubuntu-24.04`
    //    能照常下载。参考实现的列表第一项是「Ubuntu 26.04 LTS」，所以它没撞上。
    if let Some(sibling) = step.retry_other_id.as_deref() {
        if let Some(args) = args_with_id_replaced(&step.args, sibling) {
            emit(
                opts,
                InstallEvent::Line(format!(
                    "这两条还是不通 —— 换个带版本号的条目再试一次：{sibling}\
                     （清单里不带版本号的那个常常是旧的商店包，实测在这台机器上装不动）"
                )),
            );
            return match run_wsl_once(wsl, &args, step.cancellable, opts) {
                // 装成了，但**注册的名字是兄弟那个**：告诉调用方，
                // 后面「等注册」「重定位」要按它来（否则会找不到发行版）。
                Ok(()) => Ok(Some(sibling.to_owned())),
                Err(e) => Err(e),
            };
        }
    }

    // 都走不通 → 问题在 `wsl.exe` 自己的网络，不在我们选的源。
    // 指一条**不经过 wsl 网络**的路，别让用户在那儿猜。
    emit(
        opts,
        InstallEvent::Line(
            "这几条都没走通 —— 这条来源依赖 wsl.exe 自己的网络。\
             可以改用「在线发行版（镜像源）」：那是面板自己用 curl 从国内镜像站下载，\
             不经过 wsl 的网络。"
                .to_owned(),
        ),
    );
    retried.map(|()| None)
}

/// `retry_while_busy` 的步骤最多再试几次。
///
/// 7 次 + 3/6/12/24/30/30/30 秒的退避 ≈ 135 秒。
///
/// ⚠️ 这个数字是**量出来的，而且改过一次**：一开始按 `--move` 那次的窗口
/// （约 60 秒）定了 5 次 / 75 秒，结果真机上量「开稀疏盘」时是
/// **第 6 次（t≈75.3 秒）才成功** —— 正好压在最后一次上。
/// 那种"刚好够"的预算等于没有余量：再慢一点就会回到"静默不生效"。
/// 所以加厚到 135 秒（对实测的 75 秒有 80% 余量）。
///
/// 代价只是**最坏情况下**安装卡片在最后一步多停一会儿，而日志里会写明
/// "VHD 还被 WSL 占着，等 N 秒再试" —— 比"用户要的稀疏盘没生效"好得多。
pub(super) const BUSY_RETRIES: usize = 7;
/// 第一次等多久，之后翻倍。
pub(super) const BUSY_FIRST_WAIT: Duration = Duration::from_secs(3);
/// 单次等待的上限。
pub(super) const BUSY_MAX_WAIT: Duration = Duration::from_secs(30);

/// 这一次失败值不值得"等一会儿再试"。
///
/// 三个条件缺一不可：还有重试次数、不是用户取消、而且是"资源还被占着"。
/// 抽成纯函数是为了能测 —— 它是"要不要等"的**唯一**判据。
pub(super) fn worth_retrying_busy(error: &Error, attempts_done: usize) -> bool {
    attempts_done < BUSY_RETRIES && !matches!(error, Error::Cancelled { .. }) && is_busy(error)
}

/// 「资源还被占着」时等一下再跑同一条命令；返回**最后一次**的结果。
///
/// 只给标了 [`PlannedStep::retry_while_busy`] 的步骤用（目前只有「按 `.wslconfig`
/// 开稀疏盘」那一步）：它紧跟在 `--install` 后面跑，而那时 VHDX 还被 WSL 自己开着
/// —— 实测报 `Wsl/Service/ERROR_SHARING_VIOLATION`（退出码 -1），隔一会儿再跑
/// 就"操作成功完成"。
///
/// # 为什么必须重试
///
/// 那一步是 `best_effort`：失败**不会**把安装判成失败。不重试的话，用户在
/// `.wslconfig` 里写的 `[experimental] sparseVhd=true` 会**静默不生效** ——
/// 而界面上没有第二个地方能开它（v0.4 把发行版详情里那个开关拿掉了，
/// 见 `DistroAction::SetSparse` 的说明）。日志里有"这一步没做成"，
/// 但那不是用户该去读的地方。
fn retry_step_while_busy(
    wsl: &Wsl,
    step: &PlannedStep,
    opts: &RunOptions,
    mut last: Result<()>,
) -> Result<()> {
    let mut attempts_done = 0usize;
    let mut wait = BUSY_FIRST_WAIT;

    loop {
        // 先看这次的失败值不值得等（判据是上面那个纯函数，有单测）
        let worth = match &last {
            Ok(()) => false,
            Err(error) => worth_retrying_busy(error, attempts_done),
        };
        if !worth {
            return last;
        }

        attempts_done += 1;
        emit(
            opts,
            InstallEvent::Line(format!(
                "「{}」没成 —— VHD 还被 WSL 占着，等 {} 秒再试（第 {attempts_done}/{BUSY_RETRIES} 次）",
                step.label,
                wait.as_secs()
            )),
        );
        sleep_cancellable(wait, opts);
        wait = (wait * 2).min(BUSY_MAX_WAIT);
        last = run_wsl_once(wsl, &step.args, step.cancellable, opts);
    }
}

/// 跑一条 `wsl.exe` 命令（一步，不重试）。
fn run_wsl_once(wsl: &Wsl, args: &[String], cancellable: bool, opts: &RunOptions) -> Result<()> {
    let (sink, last) = line_sink(opts);
    let handle = {
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        wsl.spawn_streaming(&refs, sink)?
    };
    finish_stream(handle, "wsl", args, last, cancellable, None, opts)
}

/// 商店 ⇄ `--web-download`：有就去掉，没有就插在 `-d <id>` 后面。
///
/// 返回 `None` 表示这条命令根本不是在装某个发行版（没有 `-d`）——
/// 那种情况不该乱改参数。
pub(super) fn args_with_source_toggled(args: &[String]) -> Option<Vec<String>> {
    if let Some(at) = args.iter().position(|arg| arg == "--web-download") {
        let mut out = Vec::with_capacity(args.len() - 1);
        out.extend(args[..at].iter().cloned());
        out.extend(args[at + 1..].iter().cloned());
        return Some(out);
    }

    // 插在 `-d <id>` 后面：参数顺序对 wsl 无所谓，但这样日志里读起来和文档一致
    let at = args.iter().position(|arg| arg == "-d")?;
    let mut out = Vec::with_capacity(args.len() + 1);
    out.extend(args[..=at + 1].iter().cloned());
    out.push("--web-download".to_owned());
    out.extend(args.get(at + 2..).unwrap_or_default().iter().cloned());
    Some(out)
}

/// 把 `-d <id>` 后面那个名字换成另一个。
///
/// 返回 `None` 表示这条命令不是在装发行版（没有 `-d`）—— 那种情况不该乱改。
///
/// 用途见 [`PlannedStep::retry_other_id`]：裸 `Ubuntu` 在这台机器上装不动，
/// 换成 `Ubuntu-26.04` 就能下。
pub(super) fn args_with_id_replaced(args: &[String], id: &str) -> Option<Vec<String>> {
    let at = args.iter().position(|arg| arg == "-d")?;
    let mut out = args.to_vec();
    let slot = out.get_mut(at + 1)?;
    if slot == id {
        return None;
    }
    *slot = id.to_owned();
    Some(out)
}
