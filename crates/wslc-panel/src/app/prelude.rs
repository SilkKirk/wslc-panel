//! `app` 下所有子模块共用的导入。
//!
//! 理由同 `views::prelude`：这些名字本来就该只有一份，漏一个的表现是一大片
//! `no method named …` / `cannot find type …`。

pub(crate) use std::collections::BTreeSet;
pub(crate) use std::sync::{Arc, Mutex};
pub(crate) use std::time::{Duration, Instant};

// 注意：`primary()` / `danger()` 这些样式方法来自 trait `ButtonVariants`，
// 光导入 `Button` 是不够的 —— 这里用 glob 把 button 模块全带上。
pub(crate) use gpui_kit::component::button::*;
// `StyledExt` 提供 `font_bold` / `font_semibold` 等字重方法（由宏生成），
// 不导入 trait 就会报 "no method named font_bold"。
// 输入框与它的变化事件。
//
// `InputEvent::Change` 用来把「容器」页搜索框的输入**订阅回 `Shell`**：
// 渲染整页的是 `Shell`，输入框改的只是它自己的值 —— 不订阅父级就不会重绘，
// 过滤结果要等下一次自动刷新（默认 3 秒）才变，打字时像"搜索框坏了"。
// 这套写法在 v0.6 的 tag 过滤框上验证过（`cx.subscribe` + `matches!(event, InputEvent::Change)`）。
pub(crate) use gpui_kit::component::input::{EditorState, InputEvent, InputState, TabSize};
// 「微软商店」与「在线发行版」两张清单都走组件库的 `Select`（下拉框）——
// 参考实现也是一个下拉框（`CustomComboBox`），而不是我们原先那种
// "几十个按钮铺满一屏"的写法。
pub(crate) use gpui_kit::component::searchable_list::SearchableListItem;
pub(crate) use gpui_kit::component::select::{SelectEvent, SelectState};
// 左侧导航栏用组件库的 `Sidebar`（v0.12.2）：分组标题 / 悬停 / 选中 / 折叠
// 都由它负责。之前是自己画的 `div` 行，连 hover 都要手写
// （见 `AGENTS.md` §7.5 那条"自己画的可点元素必须手写 hover"）。
pub(crate) use gpui_kit::component::sidebar::{
    Sidebar, SidebarCollapsible, SidebarGroup, SidebarHeader, SidebarMenuItem, SidebarToggleButton,
};
// 同 views/：不导入 `Disableable`，全项目不再用 `.disabled()`。
pub(crate) use gpui_kit::component::{Sizable, StyledExt, h_flex, v_flex};
pub(crate) use gpui_kit::*;

pub(crate) use wslc_core::cmd::container::{PullPolicy, RunSpec, inspect as inspect_container};
pub(crate) use wslc_core::cmd::network as network_cmd;
// 部署管理页（v0.9）：真正的活都在 `cmd::deploy` 里（打包 / 上传 / 起停判活），
// 识别 / 脚本生成 / 输出解析是 `model::deploy` 的纯逻辑。
// ⚠️ `deploy` 这个名字两边都有（`cmd::deploy` 和 `model::deploy`），
// 所以一个用全路径、一个起别名 —— 否则读代码的人分不清在调哪一层。
pub(crate) use wslc_core::cmd::deploy as deploy_cmd;
pub(crate) use wslc_core::model::deploy as deploy_model;
pub(crate) use wslc_core::model::{AppRuntime, DeployKind, DeploySignal, DeploySite};
// 数据库管理页（v0.7.0）：真正的活都在 `cmd::mysql` 里（起进程 + 脱敏），
// 识别 / 拼参数 / SQL 生成是 `model::mysql` 的纯逻辑。
pub(crate) use wslc_core::cmd::mysql::{MysqlProbe, run_sql};
// 多驱动层（v0.14）：判断"这是哪个引擎"和按引擎取驱动。
pub(crate) use wslc_core::db::DbKind;
pub(crate) use wslc_core::model::{
    GrantKind, MysqlCreds, MysqlFamily, RootCredHint, SqlMode, mysql as mysql_model,
};
// 安装现在是**多步计划**（见 `wslc_core::model::install` 的模块说明）：
// `InstallSpec` 是用户填的东西，`plan()` 把它变成步骤，`run_plan` 把它做出来。
pub(crate) use wslc_core::cmd::distro::InstallSpec;
pub(crate) use wslc_core::cmd::install::{
    self, InstallCancel, InstallEvent, InstallSummary, RunOptions,
};
pub(crate) use wslc_core::mirrors;
pub(crate) use wslc_core::model::install::{
    self as install_model, InstallFieldErrors, InstallSource, derive_install_dir,
    suggest_name_from_file,
};
// 镜像仓库（Registry V2）的纯逻辑：引用规范化、URL 拼装、tag 排序、错误翻译。
pub(crate) use wslc_core::registry;
pub(crate) use wslc_core::settings::SettingKey;
// `Wsl` 是发行版（实例）的调用器，和容器的 `Wslc` 并列。
pub(crate) use wslc_core::{Wsl, Wslc};

pub(crate) use crate::theme;
pub(crate) use wslc_panel_core::state::{
    self, AppState, BackupState, ConfirmAction, DbFailure, DbState, DbTab, DeployFailure,
    DeployLogs, DeployPhase, DeployProgress, DeploySiteConf, DeployState, DetailInspect,
    DistroAction, ExecState, ExportProgress, ImmediateAction, ImportState, InstallOutcome,
    InstallProgress, InstallSourceKind, LogsState, MirrorChoice, MirrorProbeResult, Page,
    PendingAction, PromptKind, Toast, ToastKind, UiSection, create_command, exec_argv,
};
// `split_list` / `split_env_list` / `refresh_label` 是纯函数，
// 住在不依赖 GPUI 的 `wslc-panel-core` 里
// —— 这样它们的单测不必链接 GPUI（见那个 crate 的顶层说明）。
pub(crate) use crate::views;
pub(crate) use wslc_panel_core::util::{refresh_label, split_env_list, split_list};
