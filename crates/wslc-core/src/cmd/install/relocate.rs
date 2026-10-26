//! 「重定位」那一步：把刚装好的发行版弄成用户要的**名字 + 位置**。
//!
//! 单独一个文件，因为它是安装流程里唯一**不可取消**的一段（`--unregister`
//! 之后数据只在那个中转 tar 里），逻辑和三段式的说明都集中在这里，
//! 主流程（`install.rs` 的 `run_plan`）读起来就不会被它淹没。
//!
//! 与 `install.rs` 同一模块树（`install.rs` + `install/` 目录并存）。

use std::path::Path;

use super::*;

/// 把刚装好的发行版弄成"用户要的名字 + 用户要的位置"。
///
/// 三种情况，各有各的做法（都比"一律导出再导入"省事）：
///
/// | 情况 | 做法 |
/// |---|---|
/// | 名字已经对、位置也对 | 什么也不做 |
/// | 名字对、位置不对 | `wsl --manage <name> --move <dir>`（WSL 自己搬，比导来导去稳妥） |
/// | 名字不对 | `--export` → `--unregister` → `--import`（WSL 没有"改名"命令） |
///
/// ⚠️ 第三步里 `--unregister` 之后数据只在那个中转 tar 里了。
/// 所以：导入失败时**保留** tar 并告诉用户它在哪（这一点和参考实现不同 ——
/// 它无论如何都删掉了）。
pub(super) fn ensure_relocated(wsl: &Wsl, step: &PlannedStep, opts: &RunOptions) -> Result<()> {
    let from = arg(step, 0)?.to_owned();
    let to = arg(step, 1)?.to_owned();
    let dir = arg(step, 2)?.to_owned();
    let temp_tar = arg(step, 3)?.to_owned();

    // 注册表读不到时**不猜**：宁可少做一步并说明，也不要在没有依据的情况下
    // 去导出/注销用户的东西。
    let entries = crate::cmd::distro::read_registry();
    let base_of = |name: &str| {
        entries
            .iter()
            // 大小写不敏感：注册表里存的是 `Ubuntu`，而用户/清单给的可能是
            // `ubuntu` —— 这里用 `==` 会读不到位置，于是"已经装好了"被判成
            // "读不到注册表，这里就不动了"。见 `model::distro::same_name`。
            .find(|entry| crate::model::distro::same_name(&entry.name, name))
            .and_then(|entry| entry.base_path.clone())
    };

    // ⚠️ 这里刻意是**字符串**比较，不是 `same_name`。
    //
    // 它问的是"要不要改名"，而只差大小写对用户来说就是一次改名请求
    // （他明确要 `MyUbuntu` 这个名字）。WSL 那边名字匹配虽然不区分大小写，
    // 但把它当成"同一个名字、只是挪个位置"会静默忽略用户改的写法。
    // 重名/查找那几处必须忽略大小写（`same_name`），这里必须不忽略 ——
    // 两个判据不一样，别顺手统一。
    if from == to {
        let Some(current) = base_of(&to) else {
            emit(
                opts,
                InstallEvent::Line(format!(
                    "读不到注册表，没法核实 {to} 到底装在哪儿 —— 这里就不动了。\
                     位置不对的话，可以在实例详情里用「移动」。"
                )),
            );
            return Ok(());
        };
        if dir.trim().is_empty() || same_dir(&current, &dir) {
            emit(
                opts,
                InstallEvent::Line(format!("{to} 已经装好了，不需要重定位")),
            );
            return Ok(());
        }

        emit(
            opts,
            InstallEvent::Line(format!(
                "{to} 现在在 {}，要挪到 {dir}（先用 `wsl --manage --move`）",
                current.display()
            )),
        );
        // ① 先试 `--manage --move`：快，而且不需要中转文件。
        //
        // ⚠️ 它会因为"装完还没稳定"而失败（实测：紧接着 `--install` 之后跑，
        // 返回 -1；隔一会儿手工再跑一次就 `操作成功完成`）。所以这里**重试几次**。
        if move_with_retries(wsl, &to, &dir, opts).is_ok() {
            return Ok(());
        }
        emit(
            opts,
            InstallEvent::Line(
                "`--manage --move` 没能成功（等过几次）—— 改用「导出 → 注销 → 导入」\
                 这条更慢但更稳的路"
                    .to_owned(),
            ),
        );
        // ② 落到下面那条通用路径（导出 → 注销 → 导入）。
        //    它虽然慢（要拷一遍），但**不依赖 WSL 的临时状态**，是参考实现一直在用、
        //    而且我们自己验证过的路。
    }

    // 名字要改（或者 `--move` 没成功）：WSL 没有改名命令，只能导出再导入。
    emit(
        opts,
        InstallEvent::Line(format!("准备把 {from} 导出到 {temp_tar}")),
    );
    if let Some(parent) = Path::new(&temp_tar).parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| Error::Install(format!("建不出临时目录 {}：{e}", parent.display())))?;
    }

    let export_progress = ProgressSource {
        path: PathBuf::from(&temp_tar),
        total: None,
        started: Instant::now(),
    };
    run_wsl_args(
        wsl,
        &["--export", from.as_str(), temp_tar.as_str()],
        false,
        Some(&export_progress),
        opts,
    )?;

    emit(opts, InstallEvent::Line(format!("注销 {from}")));
    run_wsl_args(wsl, &["--unregister", from.as_str()], false, None, opts)?;

    std::fs::create_dir_all(&dir)
        .map_err(|e| Error::Install(format!("建不出安装目录 {dir}：{e}")))?;

    emit(
        opts,
        InstallEvent::Line(format!("把中转 tar 导入成 {to}（装到 {dir}）")),
    );
    let version = crate::model::install::WSL_VERSION.to_string();
    let import = run_wsl_args(
        wsl,
        &[
            "--import",
            to.as_str(),
            dir.as_str(),
            temp_tar.as_str(),
            "--version",
            version.as_str(),
        ],
        false,
        None,
        opts,
    );

    match import {
        Ok(()) => {
            match std::fs::remove_file(&temp_tar) {
                Ok(()) => emit(
                    opts,
                    InstallEvent::Line(format!("已删掉中转文件 {temp_tar}")),
                ),
                Err(e) => emit(
                    opts,
                    InstallEvent::Line(format!("中转文件 {temp_tar} 删不掉（{e}），要自己清理")),
                ),
            }
            Ok(())
        }
        Err(e) => {
            // ⚠️ 这时候 `from` 已经被注销，**数据只在这个 tar 里**。
            // 删掉它等于把刚装好的东西扔了，所以保留并说清位置。
            emit(
                opts,
                InstallEvent::Line(format!(
                    "导入失败，但{from}已经注销了 —— 数据只在中转文件里，所以**故意保留**它：\
                     {temp_tar}。可以手动 `wsl --import {to} <目录> \"{temp_tar}\" --version 2` 再试。"
                )),
            );
            Err(e)
        }
    }
}

/// `--manage --move` 重试的**总预算**。
///
/// 实测（2026-10-12）：紧接着 `--install` 之后跑 `--move` 会撞
/// `Wsl/Service/MoveDistro/ERROR_SHARING_VIOLATION`（刚装完那一刻 VHDX
/// 还被 WSL 自己开着），本机这一次的窗口是 **约 60 秒** ——
/// 连续 5 次 × 2 秒全部失败，第 30 次（约 60 秒后）才成功。
/// 所以预算取 60 秒 + 50% 余量。
///
/// ⚠️ 低于实测窗口的代价不是"报个错"，而是**退到「导出 → 注销 → 导入」**
/// 那条要把整个根文件系统拷一遍的路（几个 GB、几分钟）。
/// 等待是便宜的那一边。
pub(super) const MOVE_TOTAL_BUDGET: Duration = Duration::from_secs(94);
/// 第一次等多久，之后翻倍。
pub(super) const MOVE_FIRST_WAIT: Duration = Duration::from_secs(2);
/// 单次等待的上限。
pub(super) const MOVE_MAX_WAIT: Duration = Duration::from_secs(32);

/// `wsl --manage <name> --move <dir>`，失败就等一下再试。
///
/// # 为什么要重试（2026-10-12 重新实测了一遍）
///
/// 紧接着 `--install` 之后跑 `--move`，WSL 返回 `-1`，只给一句套话加错误码：
///
/// ```text
/// 另一个程序正在使用此文件，进程无法访问。
/// 错误代码： Wsl/Service/MoveDistro/ERROR_SHARING_VIOLATION
/// ```
///
/// 也就是**刚装完那一刻 VHDX 还被 WSL 自己开着**（隔一会儿在同一个发行版上
/// 手工再跑就"操作成功完成"）。
///
/// 等待用**指数退避**（2/4/8/16/32/32 秒，合计约 94 秒，见 [`MOVE_TOTAL_BUDGET`]）：
/// 这个冲突的典型时长是几十秒，前面几次快速试探能命中"其实已经好了"，
/// 后面的长等待不浪费轮次。
///
/// ⚠️ 只有**像是"还被占着"**的失败才值得等（见 [`is_busy`]）：路径写错、
/// 目标盘不存在这类错误等多久都不会好，早一点退到那条通用路径反而更快。
fn move_with_retries(wsl: &Wsl, name: &str, dir: &str, opts: &RunOptions) -> Result<()> {
    let started = Instant::now();
    let mut wait = MOVE_FIRST_WAIT;
    let mut attempt = 1usize;

    // 第一次**单独**做：这样 `last` 一上来就是真的错误值，不必先塞一个
    // "等会儿就被覆盖"的 `None`（clippy 的 `unused_assignments` 报的正是那个初值：
    // 循环体至少跑一次，而每条能读到它的路径上它都已经被赋过值了）。
    let mut last = match run_wsl_args(wsl, &["--manage", name, "--move", dir], false, None, opts) {
        Ok(()) => return Ok(()),
        Err(e) => e,
    };

    loop {
        // 不是"还被占着"，或者再等就超预算了 → 交回调用方走通用路径
        if !is_busy(&last) || started.elapsed() + wait > MOVE_TOTAL_BUDGET {
            return Err(last);
        }

        emit(
            opts,
            InstallEvent::Line(format!(
                "第 {attempt} 次搬没成功（{last}），等 {} 秒再试",
                wait.as_secs()
            )),
        );
        sleep_cancellable(wait, opts);
        wait = (wait * 2).min(MOVE_MAX_WAIT);
        attempt += 1;

        match run_wsl_args(wsl, &["--manage", name, "--move", dir], false, None, opts) {
            Ok(()) => {
                emit(
                    opts,
                    InstallEvent::Line(format!(
                        "第 {attempt} 次搬成功了（一共等了约 {} 秒）",
                        started.elapsed().as_secs()
                    )),
                );
                return Ok(());
            }
            Err(e) => last = e,
        }
    }
}

/// 这个失败是不是"资源还被占着"（等一下再试就有意义的那种）。
///
/// 判据用 WSL 自己给的**错误码**，而不是本地化文案：中文是
/// "另一个程序正在使用此文件"，英文是 "The process cannot access the file
/// because it is being used by another process"，而
/// `ERROR_SHARING_VIOLATION` 在两种语言里都在。
/// 文案那一条是兜底（万一某个 WSL 版本不打印错误码）。
pub(super) fn is_busy(error: &Error) -> bool {
    let text = error.to_string();
    text.contains("ERROR_SHARING_VIOLATION") || text.contains("正在使用")
}

/// 跑一条 `wsl.exe` 命令（内部用，参数是拼好的）。
fn run_wsl_args(
    wsl: &Wsl,
    args: &[&str],
    cancellable: bool,
    progress: Option<&ProgressSource>,
    opts: &RunOptions,
) -> Result<()> {
    let owned: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
    let (sink, last) = line_sink(opts);
    let handle = wsl.spawn_streaming(args, sink)?;
    finish_stream(handle, "wsl", &owned, last, cancellable, progress, opts)
}

/// 两个目录是不是同一个（忽略结尾分隔符与大小写）。
///
/// Windows 路径不区分大小写，而用户在表单里可能写成 `d:\wsl\X`。
pub(super) fn same_dir(left: &Path, right: &str) -> bool {
    let normalize = |text: &str| text.trim_end_matches(['\\', '/']).to_ascii_lowercase();
    normalize(&left.to_string_lossy()) == normalize(right)
}
