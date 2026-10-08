//! wslc-panel —— WSL 容器（wslc）管理面板。
//!
//! 渲染使用 **GPUI**（Zed 的 GPU 加速 UI 框架），通过 `gpui-kit` 伞形 crate 引入，
//! 同时获得 `gpui-base`（行为与基础设施）与 `gpui-component`（60+ 样式化组件）。
//!
//! 数据来自 `wslc.exe` 子进程，全部解析逻辑在 `wslc-core` 里，与界面完全解耦。
//!
//! ```text
//! main.rs   窗口与生命周期
//! app.rs    Shell：导航、异步刷新、确认弹窗（唯一接触 GPUI 异步 API 的文件）
//! views.rs  各页面渲染（纯函数）
//! state.rs  状态与数据采集（不依赖 GPUI）
//! theme.rs  配色
//! ```

// 发布版不给它配控制台窗口 —— 这是个 GUI 程序，双击运行时多弹一个黑框很突兀
// （第一次实机运行就暴露了这个问题）。
// debug 构建保留控制台，方便 `cargo run` 时直接看日志。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// 注意：这里**没有** `#![recursion_limit]`。
// 如果遇到 `error: recursion limit reached while expanding #[test]`，
// 不要靠调大这个上限去解决 —— 真正的原因是 `use super::*;` 把 gpui 再导出的
// `test` 属性宏继承进了测试模块，遮蔽了 Rust 内建的 `#[test]`。
// 完整说明见 views.rs 的测试模块。

mod app;
mod prefs;
mod state;
mod theme;
mod views;

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

// `cx.new(...)` 来自 `AppContext` trait，不导入就没有这个方法。
//
// 注意这里**没有** `PlatformDisplay`：`primary_display()` 返回的是
// `Rc<dyn PlatformDisplay>`，在 trait object 上调 `visible_bounds()`
// 不需要把 trait 带进作用域（带了反而是 unused import）。
use gpui_kit::{App, AppContext, WindowBounds, WindowOptions, px, size};

/// 期望的初始窗口尺寸（大屏上就用它）。
const DESIRED_WINDOW: (f32, f32) = (1280.0, 820.0);

/// 再小也不小于这个 —— 除非显示器的可用区域本身就比它还小。
const MIN_WINDOW: (f32, f32) = (880.0, 620.0);

/// 初始窗口占显示器可用区域的比例（留点边距，不让窗口贴边）。
const WINDOW_RATIO: f32 = 0.92;

fn main() {
    init_tracing();

    tracing::info!("wslc-panel 启动（版本 {}）", env!("CARGO_PKG_VERSION"));

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx| {
            // 必须在打开任何窗口之前初始化组件层。
            gpui_kit::init(cx);

            let options = initial_window_options(cx);
            tracing::info!("窗口尺寸：{:?}", options.window_bounds);

            // `cx.new(app::Shell::new)` 而不是 `cx.new(|cx| Shell::new(cx))`：
            // 后者是 clippy 的 redundant_closure。
            match gpui_kit::open_window(options, cx, |_, cx| cx.new(app::Shell::new)) {
                Ok(_) => tracing::info!("窗口已打开"),
                Err(e) => tracing::error!("打开窗口失败：{e}"),
            }
        });
}

/// 按**主显示器的可用区域**算初始窗口尺寸，而不是写死像素。
///
/// 第一版写的是固定的 1280×820，结果在小屏上开出来的窗口比桌面还大
/// （实机跑一次就发现了）。这里改成：
///
/// 1. 取显示器 `visible_bounds()`（已排除任务栏/停靠栏）；
/// 2. 按 [`WINDOW_RATIO`] 缩放；
/// 3. 夹到 `[MIN_WINDOW, DESIRED_WINDOW]`；
/// 4. **最后再和可用区域取 min** —— 保证任何情况下都不会超出屏幕。
///
/// 拿不到显示器信息时退回 [`DESIRED_WINDOW`]。
fn initial_window_options(cx: &App) -> WindowOptions {
    let window_size = cx
        .primary_display()
        .map(|display| {
            let usable = display.visible_bounds().size;
            size(
                px(fit_to_display(
                    usable.width.as_f32(),
                    DESIRED_WINDOW.0,
                    MIN_WINDOW.0,
                )),
                px(fit_to_display(
                    usable.height.as_f32(),
                    DESIRED_WINDOW.1,
                    MIN_WINDOW.1,
                )),
            )
        })
        .unwrap_or_else(|| size(px(DESIRED_WINDOW.0), px(DESIRED_WINDOW.1)));

    WindowOptions {
        window_bounds: Some(WindowBounds::centered(window_size, cx)),
        ..Default::default()
    }
}

/// 让窗口尺寸适配可用区域。
fn fit_to_display(usable: f32, desired: f32, min: f32) -> f32 {
    (usable * WINDOW_RATIO).clamp(min, desired).min(usable)
}

// ---------------------------------------------------------------------------
// 日志
// ---------------------------------------------------------------------------

/// 日志文件路径：`%LOCALAPPDATA%\wslc-panel\logs\wslc-panel.log`。
///
/// 发布版没有控制台，这个文件是**唯一**能看到日志的地方，
/// 所以出问题时要让用户先看这里。
fn log_file_path() -> Option<PathBuf> {
    Some(crate::prefs::app_dir()?.join("logs").join("wslc-panel.log"))
}

/// 日志出口：追加写日志文件，debug 构建下**同时**镜像到 stderr。
///
/// 自己实现而不用 `tracing-appender`：只要"追加写"这一件事，
/// 不想为此多一个依赖（它还会起后台线程，对桌面程序是多余的）。
///
/// 这里刻意**不做 `cfg(debug_assertions)` 分支**：早先的写法是
/// "debug 写控制台 / 发布写文件"两条独立路径，结果发布分支里的
/// `FileWriter`、`log_file_path` 在 debug 构建下成了死代码，
/// CI 的 `cargo check`（dev profile）也就**根本不会类型检查它们**。
/// 现在只有一条路径，两种构建都会编译到。
#[derive(Clone)]
struct LogWriter {
    path: Arc<PathBuf>,
    /// debug 构建下把同样的内容再写一份到 stderr，`cargo run` 时直接可见。
    mirror_stderr: bool,
}

impl std::io::Write for LogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.mirror_stderr {
            // 尽力而为：没有控制台时写 stderr 会失败，但不该因此中断日志。
            let _ = std::io::stderr().write_all(buf);
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.path.as_path())?;
        file.write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogWriter {
    type Writer = LogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// 初始化日志。
///
/// 永远写 `%LOCALAPPDATA%\wslc-panel\logs\wslc-panel.log`；
/// debug 构建额外镜像到 stderr。拿不到 `LOCALAPPDATA` 时退回只写 stderr。
fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    let Some(path) = log_file_path() else {
        tracing_subscriber::fmt().with_env_filter(filter).init();
        return;
    };

    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        // 文件里不要 ANSI 颜色转义。代价是 debug 的控制台也没有颜色，
        // 换来的是同一份字节同时进文件和终端。
        .with_ansi(false)
        .with_writer(LogWriter {
            path: Arc::new(path),
            mirror_stderr: cfg!(debug_assertions),
        })
        .init();
}
