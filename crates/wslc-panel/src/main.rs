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

mod app;
mod state;
mod theme;
mod views;

use gpui_kit::WindowOptions;

fn main() {
    // 日志走 RUST_LOG，默认 info。
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    tracing::info!("wslc-panel 启动");

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx| {
            // 必须在打开任何窗口之前初始化组件层。
            gpui_kit::init(cx);

            match gpui_kit::open_window(WindowOptions::default(), cx, |_, cx| {
                cx.new(|cx| app::Shell::new(cx))
            }) {
                Ok(_) => tracing::info!("窗口已打开"),
                Err(e) => {
                    tracing::error!("打开窗口失败：{e}");
                    eprintln!("打开窗口失败：{e}");
                }
            }
        });
}
