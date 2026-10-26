//! 部署管理域（`impl Shell`）：按**能力**分文件。
//!
//! 原先是一份 2130 行的 `deploy.rs`（36 个方法），其中 `submit_deploy` 一个函数就占了
//! 近 800 行（校验 → 打包上传 → 铺文件 → 接网络 → 起进程 → 逐阶段报告）。找"删除部署
//! 是怎么删的"得先翻过表单和提交两摊。
//!
//! | 文件 | 覆盖 |
//! |---|---|
//! | [`page`] | 页面生命周期（读存档 / 探状态 / 离开） |
//! | [`form`] | 添加部署弹窗（开关 / 浏览 / 命令预览 / 读值） |
//! | [`submit`] | 提交：校验 → 上传 → 铺文件 → 接网络 → 起进程 |
//! | [`lifecycle`] | 起停已部署的应用 |
//! | [`logs`] | 应用日志（打开 / 追 / 关） |
//! | [`site`] | nginx 站点配置与 reload |
//! | [`remove`] | 删除部署（请求 / 确认 / 真删） |
//! | [`progress`] | 阶段、进度写回、互斥提示 |
//!
//! 这些都是 `Shell` 的后代模块（`app/shell/deploy/mod.rs`），私有字段照样能读。

mod form;
mod lifecycle;
mod logs;
mod page;
mod progress;
mod remove;
mod site;
mod submit;
