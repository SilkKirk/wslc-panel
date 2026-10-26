//! 「部署应用」的**纯逻辑**：nginx / Java / Node.js / Go 容器的识别、shell 脚本
//! 生成、`tar` 参数拼装、输出解析、错误分类。
//!
//! 这个模块里**一行进程都不起**（所以能脱离 Windows 单测）—— 真去跑命令的那部分
//! 在 [`crate::cmd::deploy`]。拆法和 `model/mysql.rs` ↔ `cmd/mysql.rs` 一样。
//!
//! # 文件怎么进容器：`wslc container cp`
//!
//! ⚠️ **`cp` 不在 `wslc --help` 的顶层命令表里**（那张表列了 `exec` / `run` /
//! `attach` / `export` / `import` … 但**没有 `cp`**），它只作为
//! `wslc container cp` 存在 —— `wslc cp` 会直接报「无法识别的命令」。
//! 我第一版方案就是照着顶层表下的结论（"`wslc` 没有 `cp`，只能往 stdin 里灌
//! tar"），直到有人拿 `wslc container cp --help` 的截图来问。
//! **结论：查 CLI 的能力要看子命令那一层，别只看顶层表。**
//!
//! 实测（`wslc 3.0.3.0` + 真实的 nginx 容器）：
//!
//! | 试的 | 结果 |
//! |---|---|
//! | `wslc container cp <文件> <容器>:/tmp/`（`/tmp` **已存在**） | exit 0，落在 `/tmp/<文件名>` |
//! | `wslc container cp <文件> <容器>:/tmp/不存在` | exit **1**，`ERROR_PATH_NOT_FOUND` |
//! | `wslc container cp <目录> <容器>:/tmp/已存在目录` | exit 0，**递归**复制（含隐藏文件），并且**多套一层目录名** |
//! | `-a` / `--archive` | 和不用它**结果一样**（帮助里写着"为 Docker CLI 兼容性而接受"） |
//! | 8 MB 二进制 | exit 0，sha256 与本地**完全一致** |
//! | 复制过去的文件权限 | **`666`** —— 没有可执行位（目录倒是 `777`） |
//! | `wslc container cp <文件> <**没运行**的容器>:/tmp/` | **exit 0、什么都不做、连一个字都不输出** ⚠️ |
//!
//! 最后一条是这一版最阴的一条：**`cp` 在容器没运行时静默成功**。
//! 所以流程里必须 ① **先**用 `exec` 准备目标目录（那一条会老老实实报
//! `WSLC_E_CONTAINER_NOT_RUNNING`），② 复制完**数一下文件**核对（见
//! `crate::cmd::deploy` 的 `verify_copy`）。光看 `cp` 的退出码会得到
//! "部署成功、容器里什么都没有"。
//!
//! 也因为是"多套一层目录名"，**目录来源要逐个顶层条目去 `cp`**
//! （见 [`copy_members`]）—— 这样落到目标里的才是目录的**内容**，
//! 而不是 `<目标>/<本地目录名>/…`。
//!
//! # 起进程：不能靠 `wslc exec -d`
//!
//! 实测 `wslc exec -d nginx sh -c "sleep 5"` **耗时 5.08 秒** —— `-d` 在 `exec`
//! 上不生效，它照样等命令结束。所以"启动应用"用的是**在容器里把它甩到后台**：
//! `nohup <命令> > <日志> 2>&1 & echo $! > <pidfile>`，实测 **0.17 秒**返回、
//! 进程在 exec 结束之后**还活着**。整套判活 / 停止 / 看日志都围着这个 pidfile 转。
//!
//! ⚠️ 那对大括号（`{ … }`）是**必须的**：`&` 的优先级最低，写成
//! `mkdir -p D && cd D && nohup C & echo $! > D/pid` 会把 `echo $! > pid` 甩到
//! 后台组**外面**去 —— 它和 `mkdir` 之间于是有了竞态。实测踩到过：
//! `cannot create …: Directory nonexistent`，而命令"看起来成功了"。
//!
//! # 文件怎么分
//!
//! | 文件 | 覆盖 |
//! |---|---|
//! | [`types`] | `AppRuntime` / `DeployKind` / `SourceKind` 与默认值 |
//! | [`detect`] | 识别（镜像名 / 启动命令 / inspect） |
//! | [`path`] | 应用名与目标路径的校验 |
//! | [`script`] | 在容器里执行的 shell 脚本生成 |
//! | [`parse`] | 那些脚本的输出解析 |
//! | [`naming`] | 建议名字 / 端口 / 目标目录，以及目标重叠判定 |
//! | [`site`] | nginx 站点配置（conf / 代理规则 / DNS / 网络） |
//! | [`port`] | 端口映射查询 |
//! | [`error`] | 失败分类与"下一步" |
//!
//! 对外路径**没有变**：`mod.rs` 把每个子模块 `pub use` 出来，所以
//! `wslc_core::model::deploy::start_script` 这类写法照旧。

mod detect;
mod error;
mod naming;
mod parse;
mod path;
mod port;
mod script;
mod site;
mod types;

#[cfg(test)]
mod tests;

pub use detect::*;
pub use error::*;
pub use naming::*;
pub use parse::*;
pub use path::*;
pub use port::*;
pub use script::*;
pub use site::*;
pub use types::*;
