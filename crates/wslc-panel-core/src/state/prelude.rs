//! `state` 下所有子模块共用的导入。
//!
//! 这一层**完全不碰 GPUI**，只依赖 `wslc-core`。
//!
//! 拆成子模块之后，每个文件都得把这一串 `use` 写一遍 —— 集中在这里，
//! 新的共用导入也只加这一处。

pub(crate) use std::collections::BTreeSet;
pub(crate) use std::path::PathBuf;
pub(crate) use std::time::Duration;

pub(crate) use wslc_core::cmd::install::InstallEvent;
pub(crate) use wslc_core::mirrors::{self, Offer};
pub(crate) use wslc_core::model::install::{
    InstallSpec, OnlineDistro, PlanContext, Preflight, online_matches,
};
// 「在线清单来自哪儿」是**数据**（`wsl -l -o` 还是兜底 JSON），
// 定义在 `wslc-core` 里；这里转出去，界面按它显示那句说明。
pub use wslc_core::model::install::OnlineListSource;
// 多驱动层的连接类型（v0.13）。`DbState::conn` 用它 —— 现在只是容器 + 家族的
// 镜像，阶段 1 起它会成为"连接"的真正载体。
pub(crate) use wslc_core::db::{
    Credentials, DbConn, DbHost, DbKind, MySqlDriver, PostgresDriver, SqlDriver,
};
pub(crate) use wslc_core::model::wslconf;
pub(crate) use wslc_core::model::{
    AppRuntime, ContainerSummary, DbEntry, DbUser, DeployErrorKind, DeploySignal, Distro,
    ImageListItem, MysqlCreds, MysqlErrorKind, MysqlFamily, NetworkListItem, ProcessStatus,
    RootCredHint, SqlMode, SystemInfo, VolumeListItem, WslStatus, parse_size,
};
pub(crate) use wslc_core::settings::SettingsDoc;
// SQL 控制台的结构化结果。`SqlResult` 住在 `wslc-core` 的 `model/` 里
// （纯逻辑、能脱离 Windows 单测），这里只装它。
pub(crate) use wslc_core::model::sqlresult::SqlResult;
// 会话存储的位置与占用：基本信息页「存储」那一环要的是它算出来的盘容量
// （模板里那份 `StorageInfo` 自己带着 `VolumeSpace`）。
pub(crate) use wslc_core::storage::{StorageInfo, VolumeSpace};
pub(crate) use wslc_core::wslconfig::MisplacedKey;
pub(crate) use wslc_core::{Result, Wsl, Wslc, cmd, registry};
