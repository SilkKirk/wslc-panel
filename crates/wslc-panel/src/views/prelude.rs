//! `views` 下所有子模块共用的导入。
//!
//! # 为什么要有它
//!
//! 拆成子模块之后，每个文件都得把这一整串写一遍 —— 而它们本来就该**只有一份**：
//! 三套按钮样式（`ButtonVariants`）、`Styled` 家族、`gpui_kit::*` 那些 trait，
//! 漏一个的表现是一大片 `no method named …`。集中在这里之后，子模块只要
//! `use super::prelude::*;`，并且**新的共用导入也只加这一处**。
//!
//! `pub(crate) use` 而不是 `use`：子模块靠 glob 拿到这些名字，私有导入不会外传。

// 注意：`primary()` / `danger()` 这些样式方法来自 trait `ButtonVariants`，
// 光导入 `Button` 是不够的 —— 这里用 glob 把 button 模块全带上。
pub(crate) use gpui_kit::component::button::*;
// 官方折叠区：`Accordion`（`common::disclosure` 与 wslconf 的分节折叠都用它）。
//
// 为什么不再用 `Collapsible`：那只是个"展开/收起"的容器，折叠头要自己画
// （我们原先用 `Button` + `Icon` + `rotate`，还照抄了 `Accordion` 的 trigger 写法）。
// `Accordion` 自带那个 trigger、弹簧动画、以及"收起时卸载内容"。
//
// ⚠️ 只导 `Accordion`，**不导 `AccordionItem`**：单项是通过
// `Accordion::item(|item| …)` 的回调参数拿到的，类型名本身用不到
// （导入它会被 clippy 判成 unused import，而 `lint` 是阻塞的）。
pub(crate) use gpui_kit::component::accordion::Accordion;
// `Editor` 是 SQL 控制台那个多行编辑器（v0.12）。和多行 `Textarea` 的区别：
// `Editor` 带语法高亮（靠 `tree-sitter-sql` feature）+ 行号 + 缩进线。
pub(crate) use gpui_kit::component::input::{Editor, Input, InputState};
// `ListItem` 是树控件里"一行"的类型（`Tree` 的回调必须返回它）。
// 它比 `Button` 合适得多：`Button` 会把文字**居中**、给整行套底色和内边距，
// 画出来像一排色块（v0.12 实机就是这个效果，用户说"太丑了"）；
// `ListItem` 是给列表 / 树用的，悬停与选中态由组件自己管。
pub(crate) use gpui_kit::component::list::ListItem;
// 折叠应用级侧边栏的按钮。SQL 页不画应用级页头（见 `app.rs`），
// 所以那个按钮必须由这一页自己提供。
pub(crate) use gpui_kit::component::sidebar::SidebarToggleButton;
// 「微软商店」与「在线发行版」两张清单都是**下拉框**（参考实现也是
// `CustomComboBox`，不是我们原先那种按钮列表）。
pub(crate) use gpui_kit::component::select::Select;
// 列表行那颗「更多」用的下拉菜单（v0.9.14）。
//
// ⚠️ 它来自 gpui-kit 的**扩展 trait**：`Button::dropdown_menu(…)`（和
// `ButtonVariants` 一样，光导入 `Button` 是不够的），菜单项是 `PopupMenuItem`。
// 走的是组件的 popover 层（`deferred` + `POPUP_PRIORITY`），所以**不会被表格的
// `overflow_hidden` 裁掉** —— 那是这一类弹出能用的前提（见 `AGENTS.md` §7.5）。
pub(crate) use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
// 基本信息页那四只圆环（v0.9.17）。
//
// 为什么用它而不是自己拿 `div` 拼：圆环要画弧（`canvas` + `PathBuilder`），
// 那条路本仓库一次都没走过；组件库里现成有 `ProgressCircle`（已核对 0.7.1 的
// 源码：`progress/progress_circle.rs`），直接用它就不必去赌没验证过的 API。
// ⚠️ 它的大小只能用 `Sizable::with_size()` 设，别用 Styled 的 `.size()`
// —— 见 `views::gauge` 里那段说明。
pub(crate) use gpui_kit::component::progress::ProgressCircle;
// SQL 结果表格（v0.11）：`DataTable` 是 gpui-kit 自带的虚拟滚动表格，
// 支持列宽拖拽 / 排序 / 单元格选择。delegate 在 `crate::sql_table`。
pub(crate) use gpui_kit::component::table::DataTable;
// 官方**静态**表格组件族：9 张表用它替代自己拼的 `div` 表格。
//
// 和上面的 `DataTable` 是两个东西（读 `table.rs` 的模块文档确认）：
// `Table` 是"简单、无状态、可组合"的静态表格，**没有**虚拟滚动和列管理；
// `DataTable` 才是虚拟滚动 + 列宽拖拽那个。我们的列表只有几十行、行内还带按钮，
// 要的正是前者；虚拟滚动只留给 SQL 结果表。
//
// 为什么"行"必须保持 `TableRow` 这个具体类型，不能 `into_any_element()`：
// `TableBody::children` 的约束是 `ChildElement`（= `Sizable + IntoElement`），
// 而 `AnyElement` 不实现 `Sizable`。装箱之后就塞不回表格了。
//
// （这一条也解释了为什么 `rows` 到处写成 `Vec<TableRow>` 而不是 `Vec<AnyElement>`。
// 显式 `use` 会遮蔽 `gpui_kit::*` 里的同名 glob，所以这几个短名字不会打架。）
pub(crate) use gpui_kit::component::table::{
    Table, TableBody, TableCell, TableHead, TableHeader, TableRow,
};
// 官方表单格：`Field` 是"标签 + 控件"的那层壳，用在 `common::form_field`。
// `Form`（多个 Field 的容器，带 `label_width` / `columns`）暂时没用到 ——
// 我们的表单是**每个弹窗自己排布局**（还混着 `h_flex` 成对排列），
// 强行套一个 `Form` 会把每个弹窗的布局重排一遍，收益只在"标签列宽统一"。
// 将来真要统一标签列宽时，从这里开始。
pub(crate) use gpui_kit::component::form::Field;
// 官方单选组：一组互斥选项用它（`common::radio_row`）。
// 原先那些"一排按钮、选中那颗改 `.primary()`"的写法语义是错的（对读屏软件来说是
// 一堆独立动作，不是"这组选了哪个"），键盘上也拿不到方向键移动 / 空格选中。
pub(crate) use gpui_kit::component::radio::{Radio, RadioGroup};
// ⚠️ `WindowExt`（`window.open_dialog` / `close_dialog`）**不放这里** ——
// 目前只有 debug 构建的 Dialog spike 用它，放进 prelude 会让 release 构建
// 报一条 unused import。谁用谁就近导入。
// 注意：这里**不导入** `Disableable` —— 全项目不再使用 `.disabled()`：
// gpui-component 的禁用态文字几乎看不清，改用"守卫 + 文案"表达不可用状态。
pub(crate) use gpui_kit::component::description_list::DescriptionList;
pub(crate) use gpui_kit::component::{Sizable, StyledExt, h_flex, v_flex};
pub(crate) use gpui_kit::*;
// 弹窗的入场动画要一个时长（`Animation::new(Duration)`）。
pub(crate) use std::time::Duration;

pub(crate) use wslc_core::cmd::container::{PullPolicy, RunSpec};
// 安装的**计划**是纯逻辑（在 `wslc-core` 里）：页面直接把它的步骤列表画出来，
// 和执行时读的是同一份数据 —— 预览不会和执行走偏。
pub(crate) use wslc_core::mirrors;
pub(crate) use wslc_core::model::install as install_model;
// MySQL 容器的识别是**纯逻辑**（镜像名 / 命令 / 3306 端口），
// 所以列表行每帧都能白拿这个判断 —— 见 `container_row_actions`。
pub(crate) use wslc_core::model::mysql as mysql_model;
// 「部署应用」的识别也是纯逻辑（镜像名 / 命令），同一个理由 ——
// 列表行每帧都要判断"这一行该不该画「部署管理」按钮"。
pub(crate) use wslc_core::model::deploy as deploy_model;
pub(crate) use wslc_core::model::{
    ContainerListItem, ContainerState, ContainerSummary, DbUser, Distro, DistroState, GrantKind,
    ImageListItem, VolumeListItem,
};
// 镜像仓库（Registry V2）的纯逻辑：常用镜像清单、引用/主机名校验、体积累加。
pub(crate) use wslc_core::registry;
// `/etc/wsl.conf` 的字段表与保序文档模型（纯逻辑，在 `wslc-core` 里）。
pub(crate) use wslc_core::model::wslconf;
pub(crate) use wslc_core::settings::{SETTING_KEYS, SettingKey, SettingKind};

// 显式导入而不靠 `gpui_kit::*`：勾选框与开关都是这一层专用的组件，
// 写明来源比"碰巧 glob 里有"可靠。
pub(crate) use gpui_kit::component::checkbox::Checkbox;
// 布尔选项统一用它 —— `Switch` 是组件库给"开 / 关一个设置"的控件，
// 自带开关动画、焦点框与键盘可达性（空格 / 回车切换）。
// 和 `Button` 的差别是**语义**：按钮表示"执行一个动作"，开关表示"某个状态是开还是关"。
pub(crate) use gpui_kit::component::switch::Switch;

// `FactsState` 是「创建容器」弹窗里那条"从镜像读来的信息"的状态机
// （`ImageForm` 则是「拉取镜像」弹窗的表单）—— 两者的纯数据部分都在
// `wslc-core` / `wslc-panel-core`，这里只是画出来。
pub(crate) use crate::app::{
    CreateDialog, DbDialog, DbDialogKind, DbPageForm, FactsState, ImageForm, InstallForm, Shell,
    WslConfDialog,
};
// 列宽定义与配置项预设值都是纯数据，住在不依赖 GPUI 的 `wslc-panel-core` 里
// —— 这样它们的单测不必链接 GPUI（见那个 crate 的顶层说明）。
pub(crate) use crate::columns::{
    ALL_COLUMNS, BACKUP_COLUMNS, DB_COLUMNS, DB_USER_COLUMNS, DEPLOY_COLUMNS, DISTRO_COLUMNS,
    IMAGE_COLUMNS, NETWORK_COLUMNS, VOLUME_COLUMNS,
};
// 容器列表的筛选 / 排序状态与纯逻辑（在 `wslc-panel-core`，带单测）。
pub(crate) use crate::listview::{ContainerScope, ContainerSort, ContainerView};
pub(crate) use crate::presets::presets_for;
pub(crate) use crate::state::{
    AppState, DashboardStats, DbFailure, DbState, DbTab, DeployEntry, DeployLogs, DeploySiteConf,
    DeployState, DistroAction, ImmediateAction, InstallOutcome, InstallProgress, InstallSourceKind,
    LogsState, OnlineListSource, Page, PendingAction, PromptKind, PullProgress, SettingsTab,
    UiSection, WslConfState,
};
pub(crate) use crate::theme;
// 基本信息页那排圆环用的纯逻辑：用量分档（≥60% 黄、≥85% 红）。
// 它在 `wslc-panel-core` 里（带单测），因为这个文件里的断言 CI 不会跑。
pub(crate) use crate::util::{LoadLevel, format_bytes, load_level};
