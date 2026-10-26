//! 执行「添加实例」的**计划**：起进程、流式读输出、轮询进度、可取消。
//!
//! 计划本身是纯逻辑（[`crate::model::install`]），这里只负责"把它做出来"：
//!
//! - 每一步都发 [`InstallEvent`]，界面据此画步骤、日志、进度；
//! - 长步骤**不设固定超时**（在线安装、下载 rootfs 都可能几十分钟），
//!   靠 [`InstallCancel`] 取消 —— 与导出用的是同一套取舍；
//! - 进度不看子进程的输出，而是**轮询产物的大小**
//!   （下载的文件 / 导出的 tar）：那个数字真实且连续，
//!   而 `curl` 的进度条是 `\r` 刷新的、按行读会攒成一整行（见 [`crate::mirrors`]）。
//!
//! # 取消这件事的边界
//!
//! [`PlannedStep::cancellable`] 为 `false` 的步骤（重定位）**刻意不看取消标志**：
//! 那一步会先 `--unregister` 掉刚装好的发行版，中途停下等于把它删了。
//! 界面据此不给"取消"按钮，而不是让用户点了之后才发现没用。
//!
//! 另外，取消下载之后**半成品文件会被删掉** —— 留着只会占空间，
//! 而且用户多半以为它是个能用的 rootfs。

// `Path` 只有内联测试用（`same_dir` 的断言），正式构建里用不到 —— 否则
// `unused_imports` 会让 clippy 红。
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::cli::{self, StreamHandle, Wsl};
use crate::error::{Error, Result};
use crate::model::install::{InstallPlan, PlanProgram, PlannedStep};

// 五个子模块各自负责一段能力，它们统一用 `use super::*;` 拿本文件的作用域
// （事件类型、`emit` / `arg`、上面那几个 `use`）—— 所以这些只在这里写一份。
mod curl;
mod relocate;
mod step;
mod stream;

// 下载（curl）那一簇住在 curl.rs；主流程只用 run_curl_step。
use curl::run_curl_step;
// 「重定位」那一步（改名 / 挪盘）与「等注册」住在 relocate.rs。
// `ensure_relocated` / `wait_registered` / `is_busy` 主流程要用；`same_dir` 与
// `MOVE_*` 三个预算**只有测试**按名字用，所以单独 `#[cfg(test)]` 导入
// —— 否则非测试构建会报 unused_imports（clippy 阻塞）。
#[cfg(test)]
use relocate::{MOVE_FIRST_WAIT, MOVE_MAX_WAIT, MOVE_TOTAL_BUDGET, same_dir};
use relocate::{ensure_relocated, is_busy, wait_registered};
// 单步执行 + 三种重试住在 step.rs；主流程只用 run_wsl_step。
use step::run_wsl_step;
// 流式输出 / 取消 / 临时文件住在 stream.rs。
// `create_dir` / `remove_file` 由主流程直接分派，其余几个给别的子模块用。
use stream::{create_dir, finish_stream, line_sink, remove_file, sleep_cancellable};

/// 轮询子进程是否结束的间隔。
///
/// 250 ms 是"界面看起来是活的"和"别把 CPU 烧在这上面"之间的折中；
/// 导出那边用的是 500 ms，这里稍微快一点是因为安装的输出更密。
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// 下载用的程序。
///
/// 为什么是 `curl.exe` 而不是一个 HTTP 客户端 crate：见 [`crate::mirrors`] 的模块说明
/// （一句话：本仓库不能新增依赖，因为本机没有 cargo 而 CI 全部 `--locked`）。
const CURL: &str = "curl.exe";
/// 错误消息里用的程序名。
const CURL_LABEL: &str = "curl.exe";

// ---------------------------------------------------------------------------
// 事件与选项
// ---------------------------------------------------------------------------

/// 安装过程中发给界面的事件。
///
/// 故意做成**纯数据**：它要跨线程（后台执行器 → 界面），
/// 带上任何 GPUI 类型都会把这条通路堵死。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallEvent {
    /// 开始某一步。
    Step {
        /// 第几步（从 1 开始）。
        index: usize,
        /// 一共几步。
        total: usize,
        /// 这一步在干什么。
        label: String,
        /// 界面上那一行（可能是真命令行，见 [`PlannedStep::line`]）。
        line: String,
        /// 这一步能不能被取消。
        ///
        /// 界面据此决定**给不给**取消按钮：重定位那一步不接受取消
        /// （中途停下等于把刚装好的删了），让用户点了之后才发现没用更糟。
        cancellable: bool,
    },
    /// 子进程的一行输出。
    Line(String),
    /// 进度（下载 / 导出）。`total` 未知时界面只显示已完成的量。
    Progress {
        /// 已经产生多少字节。
        have: u64,
        /// 预期总量（探测时拿到的 `Content-Length`）；未知时为 `None`。
        total: Option<u64>,
        /// 这一步已经跑了多少秒。
        secs: u64,
    },
    /// 某一步结束。
    StepDone {
        /// 第几步（与 [`InstallEvent::Step`] 对应）。
        index: usize,
        /// 成功了吗。
        ok: bool,
        /// 失败原因（成功时为空串）。
        detail: String,
    },
}

/// 可克隆的取消标志。
///
/// 界面握一个、执行器握一个：用户点「取消」时把标志置上，
/// 执行器在轮询循环里看到它就杀掉当前子进程。
#[derive(Clone, Default)]
pub struct InstallCancel(Arc<AtomicBool>);

impl InstallCancel {
    /// 新建（未取消）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 请求取消。重复调用无害。
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// 已经被请求取消了吗。
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// 执行一次安装需要的运行时选项。
pub struct RunOptions {
    /// 取消标志（界面持有同一个）。
    pub cancel: InstallCancel,
    /// 事件回调。会在**多个线程**上被调用，实现里不该做重活。
    pub on_event: Arc<dyn Fn(InstallEvent) + Send + Sync>,
    /// 下载步骤的预期总字节数（界面探测镜像时拿到的）。
    ///
    /// 放在这里而不是塞进步骤参数：那是**显示用**的信息，
    /// 不该混进真正要执行的命令行里（否则"等效命令"就多出一个假参数）。
    pub expected_bytes: Option<u64>,
}

impl RunOptions {
    /// 新建（默认不取消、不带预期大小）。
    pub fn new(on_event: impl Fn(InstallEvent) + Send + Sync + 'static) -> Self {
        Self {
            cancel: InstallCancel::new(),
            on_event: Arc::new(on_event),
            expected_bytes: None,
        }
    }
}

/// 一次安装的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallSummary {
    /// 最终发行版名。
    pub name: String,
    /// 最终安装目录（可能为空）。
    pub install_dir: String,
    /// 成功了吗。
    pub ok: bool,
    /// 是被用户取消的吗。
    pub cancelled: bool,
    /// 一句话结果（成功或失败原因）。
    pub detail: String,
    /// 失败的步骤（成功时为 `None`）。
    pub failed_step: Option<String>,
}

// ---------------------------------------------------------------------------
// 执行
// ---------------------------------------------------------------------------

/// 按计划装一个发行版。**阻塞**，必须在后台执行器上跑。
pub fn run_plan(wsl: &Wsl, plan: &InstallPlan, opts: RunOptions) -> InstallSummary {
    let total = plan.steps.len();

    // 在线安装"换了 id 才装上"时，**真实注册的名字**是那个新 id（比如 `Ubuntu-26.04`），
    // 后面「等注册」「重定位」那两步必须按它来 —— 否则会去找一个不存在的发行版。
    // 只改这两步的第 1 个参数（"源名"），第 2 个参数是用户想要的名字，不能动：
    // 用户完全可能就把名字写成那个裸 id（默认就是），那一改就改错了。
    let mut effective_id: Option<String> = None;
    // 最终装出来的名字：默认是计划里的名字，真跑完「重定位」之后按那一步的目标名算。
    let mut final_name = plan.name.clone();

    for (index, step) in plan.steps.iter().enumerate() {
        let number = index + 1;

        let mut current = step.clone();
        if let Some(effective) = effective_id.as_deref() {
            current = retarget(&current, effective);
        }

        emit(
            &opts,
            InstallEvent::Step {
                index: number,
                total,
                label: current.label.clone(),
                line: current.line(),
                cancellable: current.cancellable,
            },
        );

        let outcome: Result<Option<String>> = match current.program {
            PlanProgram::Wsl => run_wsl_step(wsl, &current, &opts),
            PlanProgram::Curl => run_curl_step(&current, &opts).map(|()| None),
            PlanProgram::CreateDir => create_dir(&current).map(|()| None),
            PlanProgram::RemoveFile => remove_file(&current).map(|()| None),
            PlanProgram::WaitRegistered => wait_registered(wsl, &current, &opts).map(|()| None),
            PlanProgram::EnsureRelocated => {
                let target = current.args.get(1).cloned();
                ensure_relocated(wsl, &current, &opts).map(|()| {
                    // 这一步成功 = 发行版现在叫这个名字了
                    if let Some(target) = target {
                        final_name = target;
                    }
                    None
                })
            }
        };

        match outcome {
            Ok(swapped) => {
                if let Some(sibling) = swapped {
                    emit(
                        &opts,
                        InstallEvent::Line(format!(
                            "换上 {sibling} 装上了 —— 后面按这个名字继续（等注册 / 重定位）"
                        )),
                    );
                    final_name = sibling.clone();
                    effective_id = Some(sibling);
                }
                emit(
                    &opts,
                    InstallEvent::StepDone {
                        index: number,
                        ok: true,
                        detail: String::new(),
                    },
                )
            }
            // 「尽力而为」的收尾步骤（比如按 `.wslconfig` 开稀疏盘）失败**不算安装失败** ——
            // 发行版那一刻已经装好了，把整体判成失败会让人以为白装了。
            Err(e) if current.best_effort && !matches!(e, Error::Cancelled { .. }) => {
                let detail = e.to_string();
                emit(
                    &opts,
                    InstallEvent::Line(format!("这一步没做成，但它不影响已经装好的东西：{detail}")),
                );
                emit(
                    &opts,
                    InstallEvent::StepDone {
                        index: number,
                        ok: false,
                        detail: format!("（不影响安装）{detail}"),
                    },
                );
            }
            Err(e) => {
                let detail = e.to_string();
                emit(
                    &opts,
                    InstallEvent::StepDone {
                        index: number,
                        ok: false,
                        detail: detail.clone(),
                    },
                );
                return InstallSummary {
                    name: final_name,
                    install_dir: plan.install_dir.clone(),
                    ok: false,
                    cancelled: matches!(e, Error::Cancelled { .. }),
                    detail,
                    failed_step: Some(current.label.clone()),
                };
            }
        }
    }

    InstallSummary {
        name: final_name.clone(),
        install_dir: plan.install_dir.clone(),
        ok: true,
        cancelled: false,
        detail: format!("{final_name} 已安装"),
        failed_step: None,
    }
}

fn emit(opts: &RunOptions, event: InstallEvent) {
    (opts.on_event)(event);
}

/// 在线安装"换了 id 才装上"时，把后面那两步的**源名**改成真实注册的名字。
///
/// ⚠️ 只改第 1 个参数（`WaitRegistered` 的 `args[0]`、`EnsureRelocated` 的
/// `args[0] = 源名`）：
///
/// - `EnsureRelocated` 的 `args[1]` 是**用户想要的名字**，而它默认就等于那个裸 id
///   （`Ubuntu`）—— 一起改的话，用户要的 `Ubuntu` 会被改成 `Ubuntu-26.04`，
///   正好把重定位的目标名弄错；
/// - `--set-sparse` 那一步用的是最终名字，等重定位做完才对，不用动。
fn retarget(step: &PlannedStep, effective: &str) -> PlannedStep {
    let mut out = step.clone();
    if matches!(
        out.program,
        PlanProgram::WaitRegistered | PlanProgram::EnsureRelocated
    ) {
        if let Some(source) = out.args.first_mut() {
            *source = effective.to_owned();
        }
    }
    out
}

/// 取步骤里的第 `index` 个参数（计划是我们自己拼的，但执行器不该因此 panic）。
fn arg(step: &PlannedStep, index: usize) -> Result<&str> {
    step.args.get(index).map(String::as_str).ok_or_else(|| {
        Error::Install(format!(
            "内部错误：步骤「{}」缺少第 {} 个参数",
            step.label,
            index + 1
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    // 下面这几个**只有测试按名字用**（正式构建里没有调用点），所以在测试模块内部
    // 导入，不给顶层留一串 `#[cfg(test)]`：`failure_detail` 在 stream.rs、
    // `output_path` 在 curl.rs、参数改写与退避预算在 step.rs。
    use super::curl::output_path;
    use super::step::{
        BUSY_FIRST_WAIT, BUSY_MAX_WAIT, BUSY_RETRIES, args_with_id_replaced,
        args_with_source_toggled, worth_retrying_busy,
    };
    use super::stream::failure_detail;
    use crate::model::install::{InstallSource, InstallSpec, PlanContext};

    #[test]
    fn cancel_flag_is_shared_and_sticky() {
        let cancel = InstallCancel::new();
        assert!(!cancel.is_cancelled());
        let clone = cancel.clone();
        clone.cancel();
        // 界面和执行器握的是同一个标志
        assert!(cancel.is_cancelled());
        assert!(clone.is_cancelled());
    }

    #[test]
    fn output_path_is_read_from_the_curl_arguments() {
        let owned = |items: &[&str]| -> Vec<String> {
            items.iter().map(|item| (*item).to_owned()).collect()
        };
        let args = owned(&["-s", "-o", r"D:\tmp\a.tar.xz", "https://x"]);
        assert_eq!(
            output_path(&args).unwrap(),
            PathBuf::from(r"D:\tmp\a.tar.xz")
        );

        // 没有 -o / -o 后面没东西 → 明确的内部错误，而不是 panic
        assert!(matches!(
            output_path(&owned(&["-s", "https://x"])),
            Err(Error::Install(_))
        ));
        assert!(matches!(
            output_path(&owned(&["-o"])),
            Err(Error::Install(_))
        ));
    }

    #[test]
    fn same_dir_ignores_case_and_trailing_separators() {
        assert!(same_dir(Path::new(r"D:\wsl\X"), r"D:\wsl\X"));
        assert!(same_dir(Path::new(r"D:\wsl\X\"), r"D:\wsl\X"));
        assert!(same_dir(Path::new(r"D:\WSL\x"), r"d:\wsl\X"));
        assert!(!same_dir(Path::new(r"D:\wsl\X"), r"D:\wsl\Y"));
    }

    #[test]
    fn missing_arguments_are_errors_not_panics() {
        let step = PlannedStep {
            label: "x".to_owned(),
            program: PlanProgram::CreateDir,
            args: Vec::new(),
            cancellable: true,
            retry_other_source: false,
            best_effort: false,
            retry_other_id: None,
            retry_while_busy: false,
        };
        assert!(matches!(arg(&step, 0), Err(Error::Install(_))));
    }

    #[test]
    fn the_download_source_can_be_toggled_for_the_retry() {
        let owned = |items: &[&str]| -> Vec<String> {
            items.iter().map(|item| (*item).to_owned()).collect()
        };

        // 商店 → GitHub：插在 `-d <id>` 后面
        let store = owned(&[
            "--install",
            "-d",
            "Ubuntu-24.04",
            "--version",
            "2",
            "--no-launch",
        ]);
        assert_eq!(
            args_with_source_toggled(&store).unwrap(),
            owned(&[
                "--install",
                "-d",
                "Ubuntu-24.04",
                "--web-download",
                "--version",
                "2",
                "--no-launch"
            ])
        );

        // GitHub → 商店：去掉那个开关，其余原样
        let web = owned(&[
            "--install",
            "-d",
            "Ubuntu",
            "--web-download",
            "--version",
            "2",
        ]);
        assert_eq!(
            args_with_source_toggled(&web).unwrap(),
            owned(&["--install", "-d", "Ubuntu", "--version", "2"])
        );

        // 不是在装发行版（没有 `-d`）→ 不重试（返回 None），不许乱改命令
        assert_eq!(
            args_with_source_toggled(&owned(&["--list", "--online"])),
            None
        );
        assert_eq!(args_with_source_toggled(&[]), None);
    }

    #[test]
    fn the_id_can_be_swapped_for_the_retry() {
        let owned =
            |items: &[&str]| -> Vec<String> { items.iter().map(|s| (*s).to_owned()).collect() };

        let store = owned(&["--install", "-d", "Ubuntu", "--version", "2", "--no-launch"]);
        assert_eq!(
            args_with_id_replaced(&store, "Ubuntu-26.04").unwrap(),
            owned(&[
                "--install",
                "-d",
                "Ubuntu-26.04",
                "--version",
                "2",
                "--no-launch"
            ])
        );
        // 已经是它 → 不白跑一趟
        assert_eq!(args_with_id_replaced(&store, "Ubuntu"), None);
        // 不是安装命令 → 不乱改
        assert_eq!(
            args_with_id_replaced(&owned(&["--list", "--online"]), "X"),
            None
        );
        // `-d` 后面没有东西也不能 panic
        assert_eq!(
            args_with_id_replaced(&owned(&["--install", "-d"]), "X"),
            None
        );
    }

    #[test]
    fn a_swapped_id_retargets_the_wait_and_move_steps_only() {
        // 换了 id 才装上时，「等注册」要等真实名字
        let wait = PlannedStep {
            label: "等 Ubuntu 注册完成".to_owned(),
            program: PlanProgram::WaitRegistered,
            args: vec!["Ubuntu".to_owned()],
            cancellable: true,
            retry_other_source: false,
            best_effort: false,
            retry_other_id: None,
            retry_while_busy: false,
        };
        assert_eq!(retarget(&wait, "Ubuntu-26.04").args, vec!["Ubuntu-26.04"]);

        // 「重定位」只改源名：目标名是**用户要的名字**（默认就等于那个裸 id），
        // 一起改就把用户要的 `Ubuntu` 改错了
        let move_step = PlannedStep {
            label: "必要时把 Ubuntu 重定位成 Ubuntu".to_owned(),
            program: PlanProgram::EnsureRelocated,
            args: vec![
                "Ubuntu".to_owned(),
                "Ubuntu".to_owned(),
                r"D:\linux\Ubuntu".to_owned(),
                r"C:\Temp\wslc-panel\x.tar".to_owned(),
            ],
            cancellable: false,
            retry_other_source: false,
            best_effort: false,
            retry_other_id: None,
            retry_while_busy: false,
        };
        assert_eq!(
            retarget(&move_step, "Ubuntu-26.04").args,
            vec![
                "Ubuntu-26.04",
                "Ubuntu",
                r"D:\linux\Ubuntu",
                r"C:\Temp\wslc-panel\x.tar"
            ]
        );

        // 别的步骤（`--set-sparse` 用的是最终名字）一个字都不动
        let sparse = PlannedStep {
            label: "开稀疏盘".to_owned(),
            program: PlanProgram::Wsl,
            args: vec!["--manage".to_owned(), "Ubuntu".to_owned()],
            cancellable: true,
            retry_other_source: false,
            best_effort: true,
            retry_other_id: None,
            retry_while_busy: false,
        };
        assert_eq!(retarget(&sparse, "Ubuntu-26.04").args, sparse.args);
    }

    #[test]
    fn failure_detail_drops_the_useless_footer() {
        // 真机上 `--manage --move` 失败时，最后一行永远是这句套话，
        // 真正的原因（错误码）在它前面 —— 只留最后一行等于什么都没说
        let tail = vec![
            "无法与服务器建立连接".to_owned(),
            "错误代码：Wsl/InstallDistro/WININET_E_CANNOT_CONNECT".to_owned(),
            "如果此错误是意外错误，请考虑搜索现有问题或在 https://github.com/microsoft/WSL/issues 提交新问题。"
                .to_owned(),
        ];
        let detail = failure_detail(&tail);
        assert!(detail.contains("WININET_E_CANNOT_CONNECT"), "{detail}");
        assert!(!detail.contains("如果此错误是意外错误"), "{detail}");

        // 没有任何有用输出时也要给一句人话
        assert!(failure_detail(&[]).contains("没有给出任何有用的输出"));
        assert!(
            failure_detail(&["".to_owned(), "  ".to_owned()]).contains("没有给出任何有用的输出")
        );
    }

    #[test]
    fn the_move_retry_budget_covers_the_measured_sharing_violation_window() {
        // 实测（2026-10-12）：紧接着 `--install` 之后 `--move` 撞
        // `ERROR_SHARING_VIOLATION`，本机这一次的窗口是**约 60 秒**
        // （5 次 × 2 秒全失败，第 30 次约 60 秒才成功）。
        // 预算盖不住它，就会白退到"导出 → 注销 → 导入"那条要全量拷贝的路。
        assert!(
            MOVE_TOTAL_BUDGET >= Duration::from_secs(60),
            "重试预算 {MOVE_TOTAL_BUDGET:?} 盖不住实测的 60 秒窗口"
        );
        // 单次等待不能超过总预算，否则第一轮就超了 —— 等于一次都不重试
        assert!(MOVE_MAX_WAIT <= MOVE_TOTAL_BUDGET);
        // 第一次等待必须明显小于实测窗口，否则"其实已经好了"的那种情况也白等
        assert!(MOVE_FIRST_WAIT < MOVE_TOTAL_BUDGET / 4);
    }

    #[test]
    fn busy_failures_are_recognized_by_the_error_code_not_by_the_language() {
        // 实测（2026-10-12）：紧接着 `--install` 之后跑 `--move` 就是这个失败。
        // 中文系统上的文案是"另一个程序正在使用此文件"，英文系统上完全不同 ——
        // 所以判据必须是 WSL 自己给的错误码。
        let chinese = Error::NonZeroExit {
            program: "wsl",
            args: r"--manage X --move D:\wsl\X".to_owned(),
            code: -1,
            stderr: "另一个程序正在使用此文件，进程无法访问。 / \
                     错误代码： Wsl/Service/MoveDistro/ERROR_SHARING_VIOLATION"
                .to_owned(),
        };
        assert!(is_busy(&chinese));

        // 只给文案（某个 WSL 版本不打印错误码）也要认出来
        let text_only = Error::NonZeroExit {
            program: "wsl",
            args: String::new(),
            code: -1,
            stderr: "无法完成该操作，因为 VHD 当前正在使用中".to_owned(),
        };
        assert!(is_busy(&text_only));

        // ⚠️ 别的失败**不能**被当成"等一会儿就好"：路径写错、目标盘不存在这类
        // 等多久都不会好，早一点退到「导出 → 注销 → 导入」那条路反而更快。
        for other in [
            Error::NonZeroExit {
                program: "wsl",
                args: String::new(),
                code: -1,
                stderr: "系统找不到指定的路径。 / 错误代码： Wsl/ERROR_PATH_NOT_FOUND".to_owned(),
            },
            Error::Cancelled {
                program: "wsl",
                args: String::new(),
            },
            Error::Install("建不出安装目录 D:\\x：拒绝访问".to_owned()),
        ] {
            assert!(!is_busy(&other), "{other}");
        }
    }

    #[test]
    fn only_busy_failures_are_worth_waiting_for() {
        // 实测（2026-10-12）：紧接着 `--install` 之后跑 `--set-sparse` 就是这个失败
        let busy = Error::NonZeroExit {
            program: "wsl",
            args: "--manage X --set-sparse true --allow-unsafe".to_owned(),
            code: -1,
            stderr: "另一个程序正在使用此文件，进程无法访问。 / \
                     错误代码： Wsl/Service/ERROR_SHARING_VIOLATION"
                .to_owned(),
        };
        // 还有次数 + 是"忙" → 值得等
        assert!(worth_retrying_busy(&busy, 0));
        assert!(worth_retrying_busy(&busy, BUSY_RETRIES - 1));
        // 次数用尽 → 不再等（这条循环的出口之一）
        assert!(!worth_retrying_busy(&busy, BUSY_RETRIES));

        // 用户点了取消 → 别试了（"取消"和"再试一次"是相反的意思）
        assert!(!worth_retrying_busy(
            &Error::Cancelled {
                program: "wsl",
                args: String::new(),
            },
            0
        ));
        // 别的失败等多久都一样（比如 WSL 因为自己的策略拒绝开稀疏盘）
        assert!(!worth_retrying_busy(
            &Error::NonZeroExit {
                program: "wsl",
                args: String::new(),
                code: -1,
                stderr: "由于潜在的数据损坏，目前已禁用稀疏 VHD 支持。".to_owned(),
            },
            0
        ));

        // 预算要盖得住**实测**那个"刚装完 VHDX 还被占着"的窗口。
        //
        // 实测两次：`--move` 约 60 秒成功；`--set-sparse` 到 **75.3 秒**才成功
        // （第 6 次，正好压在当时的最后一次上）。所以这里按 120 秒钉住下限，
        // 免得日后有人"顺手"把预算调小。
        let mut total = Duration::ZERO;
        let mut wait = BUSY_FIRST_WAIT;
        for _ in 0..BUSY_RETRIES {
            total += wait;
            wait = (wait * 2).min(BUSY_MAX_WAIT);
        }
        assert!(
            total >= Duration::from_secs(120),
            "忙重试预算 {total:?} 盖不住实测的 75 秒窗口（`--set-sparse` 那次）"
        );
        // 单次等待也不能超过总预算，否则第一轮就超了 —— 等于一次都不重试
        assert!(BUSY_MAX_WAIT <= total);
    }

    #[test]
    fn a_best_effort_step_failing_does_not_fail_the_install() {
        // 场景：发行版已经装好，最后那条"按 .wslconfig 开稀疏盘"被 WSL 拒绝
        // （实测它默认就拒绝）。那一刻**不能**把整体判成失败 ——
        // 否则用户看到"安装失败"，去列表里一看发行版好好地在那儿。
        let plan = InstallPlan {
            name: "X".to_owned(),
            install_dir: String::new(),
            steps: vec![PlannedStep {
                label: "按 .wslconfig 把 X 的 VHD 设成稀疏".to_owned(),
                program: PlanProgram::Wsl,
                args: vec!["--manage".to_owned(), "X".to_owned()],
                cancellable: true,
                retry_other_source: false,
                best_effort: true,
                retry_other_id: None,
                retry_while_busy: false,
            }],
            notes: Vec::new(),
        };

        let events: Arc<Mutex<Vec<InstallEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let opts = RunOptions::new(move |event| {
            if let Ok(mut guard) = sink.lock() {
                guard.push(event);
            }
        });

        // 起一个不存在的可执行文件 → 这一步必然失败（而且失败得很快）
        let wsl = Wsl::with_program("definitely-not-a-real-binary");
        let summary = run_plan(&wsl, &plan, opts);

        assert!(summary.ok, "收尾步骤失败不该把安装判成失败：{summary:?}");
        let events = events.lock().unwrap();
        // 但界面上要说清"这一步没做成"（StepDone 里 ok=false）
        assert!(
            events
                .iter()
                .any(|e| matches!(e, InstallEvent::StepDone { ok: false, .. })),
            "{events:?}"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, InstallEvent::Line(text) if text.contains("不影响"))),
            "{events:?}"
        );
    }

    #[test]
    fn a_plan_with_only_cleanup_steps_runs_to_completion() {
        // 这条测试**不起任何进程**：`CreateDir` 与 `RemoveFile` 是纯文件系统操作，
        // 所以能真跑一遍 executor 的主循环（包括事件顺序），
        // 而"起 wsl.exe"那几条只能在真机上验 —— 见 §12 的手测清单。
        let dir =
            std::env::temp_dir().join(format!("wslc-panel-install-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("temp.txt");

        let plan = InstallPlan {
            name: "X".to_owned(),
            install_dir: dir.to_string_lossy().into_owned(),
            steps: vec![
                PlannedStep {
                    label: "创建安装目录".to_owned(),
                    program: PlanProgram::CreateDir,
                    args: vec![dir.to_string_lossy().into_owned()],
                    cancellable: true,
                    retry_other_source: false,
                    best_effort: false,
                    retry_other_id: None,
                    retry_while_busy: false,
                },
                PlannedStep {
                    label: "删掉临时文件（本来就不在，也算成功）".to_owned(),
                    program: PlanProgram::RemoveFile,
                    args: vec![file.to_string_lossy().into_owned()],
                    cancellable: true,
                    retry_other_source: false,
                    best_effort: false,
                    retry_other_id: None,
                    retry_while_busy: false,
                },
            ],
            notes: Vec::new(),
        };

        let events: Arc<Mutex<Vec<InstallEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let opts = RunOptions::new(move |event| {
            if let Ok(mut guard) = sink.lock() {
                guard.push(event);
            }
        });

        // 用一个必然不存在的可执行文件：这两步都不该碰它
        let wsl = Wsl::with_program("definitely-not-a-real-binary");
        let summary = run_plan(&wsl, &plan, opts);

        assert!(summary.ok, "{summary:?}");
        assert!(dir.exists(), "CreateDir 应该真的建出目录");
        let _ = std::fs::remove_dir_all(&dir);

        let events = events.lock().unwrap();
        let steps = events
            .iter()
            .filter(|e| matches!(e, InstallEvent::Step { .. }))
            .count();
        let done = events
            .iter()
            .filter(|e| matches!(e, InstallEvent::StepDone { ok: true, .. }))
            .count();
        assert_eq!(steps, 2, "{events:?}");
        assert_eq!(done, 2, "{events:?}");
        // 第一步的事件里要带上序号和总数（界面靠它画"第 1 / 2 步"）
        match &events[0] {
            InstallEvent::Step { index, total, .. } => {
                assert_eq!((*index, *total), (1, 2));
            }
            other => panic!("第一个事件应该是 Step：{other:?}"),
        }
    }

    #[test]
    fn a_failing_plan_stops_at_the_first_bad_step() {
        // 用"起一个不存在的可执行文件"造一次失败：`Wsl` 的 spawn 会返回
        // ExecutableNotFound，执行器应当**停在那里**并把原因交出来。
        let plan = InstallPlan {
            name: "X".to_owned(),
            install_dir: String::new(),
            steps: vec![
                PlannedStep {
                    label: "导入".to_owned(),
                    program: PlanProgram::Wsl,
                    args: vec!["--import".to_owned()],
                    cancellable: true,
                    retry_other_source: false,
                    best_effort: false,
                    retry_other_id: None,
                    retry_while_busy: false,
                },
                PlannedStep {
                    label: "不该被执行到".to_owned(),
                    program: PlanProgram::CreateDir,
                    args: vec![std::env::temp_dir().to_string_lossy().into_owned()],
                    cancellable: true,
                    retry_other_source: false,
                    best_effort: false,
                    retry_other_id: None,
                    retry_while_busy: false,
                },
            ],
            notes: Vec::new(),
        };

        let seen: Arc<Mutex<Vec<InstallEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let opts = RunOptions::new(move |event| {
            if let Ok(mut guard) = sink.lock() {
                guard.push(event);
            }
        });

        let wsl = Wsl::with_program("definitely-not-a-real-binary");
        let summary = run_plan(&wsl, &plan, opts);

        assert!(!summary.ok);
        assert!(!summary.cancelled);
        assert_eq!(summary.failed_step.as_deref(), Some("导入"));
        assert!(summary.detail.contains("找不到"), "{}", summary.detail);

        let events = seen.lock().unwrap();
        let steps = events
            .iter()
            .filter(|e| matches!(e, InstallEvent::Step { .. }))
            .count();
        assert_eq!(steps, 1, "失败之后不该继续跑下一步：{events:?}");
        assert!(
            events
                .iter()
                .any(|e| matches!(e, InstallEvent::StepDone { ok: false, .. }))
        );
    }

    #[test]
    fn an_already_cancelled_run_stops_before_the_first_step() {
        // WaitRegistered 是最容易"取消在第一步之前就生效"的一步：
        // 它每轮都先看标志。用它来钉住"取消不会假装成功"。
        let plan = InstallPlan {
            name: "X".to_owned(),
            install_dir: String::new(),
            steps: vec![PlannedStep {
                label: "等注册".to_owned(),
                program: PlanProgram::WaitRegistered,
                args: vec!["X".to_owned()],
                cancellable: true,
                retry_other_source: false,
                best_effort: false,
                retry_other_id: None,
                retry_while_busy: false,
            }],
            notes: Vec::new(),
        };

        let opts = RunOptions::new(|_| {});
        opts.cancel.cancel();

        let wsl = Wsl::with_program("definitely-not-a-real-binary");
        let summary = run_plan(&wsl, &plan, opts);
        assert!(!summary.ok);
        assert!(summary.cancelled, "{summary:?}");
    }

    #[test]
    fn a_plan_built_from_a_real_spec_has_the_documented_shape() {
        // 执行器依赖"计划里的参数位置"（`arg(step, 0)` 那些），
        // 所以这里把"计划 → 参数位置"这件事连起来钉一次：
        // 纯逻辑那侧的测试管参数内容，这条管它们的位置对得上。
        let ctx = PlanContext {
            default_dir: Some(r"D:\wsl".to_owned()),
            temp_dir: r"C:\Temp\wslc-panel".to_owned(),
            stamp: "1-2".to_owned(),
            wslconfig_sparse: false,
        };
        let spec = InstallSpec::new(
            "MyUbuntu",
            InstallSource::Online {
                id: "Ubuntu-24.04".to_owned(),
                launch: false,
                web_download: false,
                fallback_id: None,
            },
        );
        let plan = crate::model::install::plan(&spec, &ctx).unwrap();

        let relocate = plan
            .steps
            .iter()
            .find(|s| s.program == PlanProgram::EnsureRelocated)
            .expect("改名时必须有重定位那一步");
        assert_eq!(arg(relocate, 0).unwrap(), "Ubuntu-24.04");
        assert_eq!(arg(relocate, 1).unwrap(), "MyUbuntu");
        assert_eq!(arg(relocate, 2).unwrap(), r"D:\wsl\MyUbuntu");
        assert!(arg(relocate, 3).unwrap().ends_with(".tar"));

        let wait = plan
            .steps
            .iter()
            .find(|s| s.program == PlanProgram::WaitRegistered)
            .expect("在线安装要等注册");
        assert_eq!(arg(wait, 0).unwrap(), "Ubuntu-24.04");
    }
}
