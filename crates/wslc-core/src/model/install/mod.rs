//! 「添加实例」的纯逻辑：名称与路径推导、装前检查、**执行计划**。
//!
//! # 为什么要有"计划"这一层
//!
//! v0.3 那版把安装当成"一两条命令"（`InstallSpec::to_args` + `preview_lines`）。
//! 对齐参考实现之后，在线安装最多要 8 步（装 → 等注册 → 导出 → 注销 → 导入 →
//! 删临时文件 → 开稀疏 → 设默认），镜像站还要先下载。**"一条命令"这个抽象已经不成立了**；
//! 硬套下去只能让"预览"和"执行"各写一份拼参数逻辑 —— 而它们一旦走偏，
//! 界面上那句"与实际执行完全一致"就成了假话。
//!
//! 所以这里产出的是一个**步骤列表**：界面逐条渲染当预览，执行器逐条执行，
//! 两边读的是同一份数据。（执行在 [`crate::cmd::install`]。）
//!
//! # 为什么这些函数必须是纯的
//!
//! 本机没有 Rust 工具链、CI 里 `core` 任务才是唯一能跑测试的地方，而它**不连 GPUI**。
//! 所以名称推导、重名判断、参数拼装、两个在线列表的解析全都不起进程、不碰文件系统、
//! 不看 `cfg(windows)` —— 只吃字符串。这样它们才能被真正验证（`AGENTS.md` §1、§2）。
//!
//! 只有一处例外：[`PlanContext::default`] 会用 `%TEMP%` 和进程 id 拼一个临时目录，
//! 那也是为了"计划里能写出临时文件的完整路径"（用户要能看见它）。
//!
//! # 文件怎么分
//!
//! | 文件 | 覆盖 |
//! |---|---|
//! | [`name`] | 名称推导 / 合法性 / 本地文件后缀识别 |
//! | [`path`] | Windows 绝对路径与默认安装目录 |
//! | [`catalog`] | 在线清单（`wsl -l -o` 与商店 JSON） |
//! | [`spec`] | 用户填的那份规格（`InstallSource` / `InstallSpec`） |
//! | [`preflight`] | 装前检查（`Preflight` / `InstallFieldErrors`） |
//! | [`plan`] | 执行计划（类型 + `plan()`） |
//!
//! 对外路径没有变：`mod.rs` 把子模块 `pub use` 出来，所以
//! `wslc_core::model::install::plan` 这类写法照旧。

/// **本项目只支持 WSL 2。**
///
/// 所有"新建发行版"的路径都**显式**传 `--version 2`，而不是依赖 WSL 的默认值 ——
/// 默认值（`wsl --set-default-version`）是可以被用户改成 1 的，
/// 那样建出来的发行版本程序管不了，用户还会以为是程序坏了。
///
/// ⚠️ 例外：`--install --from-file`（[`InstallSource::File`]）**没有**这个选项
/// （实测 `wsl.exe --help`），版本由安装器自己决定 —— 这一条只能如实承认。
pub const WSL_VERSION: u8 = 2;

/// 发行版名长度上限。
///
/// 参考实现也是 25：超长的名字在 `wsl -l -v` 的表格里会把列挤歪，
/// 而且它同时是注册表键名和默认安装目录名的一部分。
pub const MAX_NAME_LEN: usize = 25;

mod catalog;
mod name;
mod path;
mod plan;
mod preflight;
mod spec;

#[cfg(test)]
mod tests;

pub use catalog::*;
pub use name::*;
pub(crate) use name::{is_name_char, name_syntax_errors};
pub use path::*;
pub use plan::*;
pub(crate) use preflight::validate;
pub use preflight::*;
pub use spec::*;
