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
//! 这是整个项目里**唯一**接触 GPUI 异步 API 的地方。
//!
//! # 为什么拆成目录
//!
//! 这个文件原先是一整份 **11 201 行** 的 `app.rs`：68 个顶层条目 +
//! 一个 **9 184 行、203 个方法** 的 `impl Shell`。它按页面的顺序往下排，
//! 但页面之间没有边界 —— 「部署页那几个方法」要靠行号记位置。
//!
//! 现在按**功能域**拆进 [`shell`]（见那个模块的说明）。
//! 拆法沿用 `views` 那条规矩：一个域一个文件；共用导入沉到 `prelude`。

mod prelude;
mod shell;

pub(crate) use shell::*;
