//! 官方 `Dialog` 的窗口级层栈：`DialogKey`、状态对齐、以及每个浮层的构造。
//!
//! # 为什么单独一个文件
//!
//! 这些浮层分属容器 / 发行版 / 数据库 / 部署四个域，但它们的**开合**是同一件事：
//! `Root` 只有一个窗口级栈（`WindowState::active_dialogs`），**层序 = 栈序**。
//! 由一处统一压栈，才不会出现"从详情里打开的确认框被详情盖住"那类层序 bug
//! （`AGENTS.md` §7.5）。
//!
//! 原先这堆代码住在 `distros.rs`（发行版域）：它是最早迁到 `Dialog` 的域，
//! 后来 15 个浮层陆续迁进来也顺手加在那儿，于是那个文件一半篇幅是**别的域**的
//! 东西。挪出来之后：
//!
//! - `distros.rs` 只剩发行版域自己的方法；
//! - 加一个新浮层动**两个文件**：本文件的 `DialogKey` 变体 + `sync_dialog_stack`
//!   的 `if`，以及 `dialogs/specs.rs` 的 `open_dialog_for` 臂；
//! - `Shell` 的结构体仍在 `app/shell/mod.rs` —— 本模块是它的后代，所以能读写
//!   那些私有字段（见 `mod.rs` 里"结构体必须留在这一层"的说明）。

use super::super::prelude::*;
use super::Shell;
use crate::views::{ShellView, modal_footer};

/// 已经压进**窗口级对话框栈**的浮层标识（官方 `Dialog`）。
///
/// # 为什么是一个枚举而不是"直接比状态"
///
/// 状态里那几样东西（`exporting` / `db_backup` …）各自独立，而对话框栈是**有序**的。
/// 用一个 `Vec<DialogKey>` 把"这一帧应该开着哪些、按什么顺序"表达出来，
/// 才能和实际栈做最长公共前缀的比对（官方 `ActiveDialog` 没有 id，见
/// [`Shell::sync_dialog_stack`]）。
///
/// 枚举里的顺序**就是** `DialogKey::ALL` 里的顺序（那个才是层序），
/// 枚举自身的排列只是为了好读 —— 改层序去改 `ALL`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DialogKey {
    /// 「数据库基础信息」对话框（连接卡）。
    DbInfo,
    /// 「正在导出」进度对话框。
    Export,
    /// 「正在备份数据库」进度对话框。
    DbBackupProgress,
    /// 「正在导入 SQL」进度对话框。
    DbImportProgress,
    /// 「容器日志」对话框。
    ContainerLogs,
    /// 「容器详情」对话框。
    ContainerDetail,
    /// 「发行版详情」对话框。
    DistroDetail,
    /// 「容器 exec」对话框。
    Exec,
    /// 「单输入框提示」对话框（移动 / 调整大小 / 默认用户 / 导出 / 创建卷）。
    Prompt,
    /// 「应用日志」对话框（部署管理页）。
    DeployLogs,
    /// 「站点配置」对话框（部署管理页）。
    DeploySiteConf,
    /// 「删除部署」确认框。
    DeployRemoveConfirm,
    /// 「备份列表」对话框（数据库管理页）。
    DbBackupList,
    /// 通用二次确认（危险操作）。
    Confirm,
    /// 「数据库动作」弹窗（新建库 / 建用户 / 改密 / 授权 / 导入）。
    DbAction,
    /// 「创建容器」弹窗。
    CreateContainer,
    /// 「拉取镜像」弹窗。
    PullImage,
    /// 「发行版配置」（/etc/wsl.conf）弹窗。
    WslConf,
    /// 「添加部署 / 重新部署」弹窗。
    DeployForm,
}

impl DialogKey {
    /// 全部浮层，**顺序 = 层序**（`Root` 的栈按 push 顺序叠，后 push 的在上面）。
    ///
    /// 这张表就是"z 序"的唯一来源。原先顺序藏在 `sync_dialog_stack` 的 17 个
    /// `if` 书写顺序里 —— 加浮层时插在哪、为什么插在那，读的人只能猜。
    ///
    /// ⚠️ **`Confirm` 必须在最后**：它是"从任何浮层里弹出来的二次确认"，
    /// 压在表单弹窗**下面**就是 v0.9.14 那个"压缩磁盘的确认框在详情之下"的同型 bug。
    pub(crate) const ALL: [DialogKey; 19] = [
        // 只读说明卡压在最底下：用户从页头点开后，还会接着打开备份列表 /
        // 数据库动作，那些必须盖在它上面。
        DialogKey::DbInfo,
        DialogKey::Export,
        DialogKey::DbBackupProgress,
        DialogKey::DbImportProgress,
        DialogKey::ContainerLogs,
        DialogKey::ContainerDetail,
        DialogKey::DistroDetail,
        DialogKey::Exec,
        DialogKey::Prompt,
        DialogKey::DeployLogs,
        DialogKey::DeploySiteConf,
        DialogKey::DeployRemoveConfirm,
        DialogKey::DbBackupList,
        DialogKey::DeployForm,
        DialogKey::WslConf,
        DialogKey::PullImage,
        DialogKey::CreateContainer,
        DialogKey::DbAction,
        DialogKey::Confirm,
    ];

    /// 这一帧这个浮层该开着吗 —— **唯一判据**（原先散在 17 个 `if` 里）。
    ///
    /// 每个臂都必须能回答"哪些状态同时成立才开"。尤其注意带"页面进得来"
    /// 门槛的那几个（`db_form.is_some()`）：少了前半句，没进数据库页时
    /// 也会弹出一个内容空空的浮层。
    pub(crate) fn is_open(self, shell: &Shell) -> bool {
        match self {
            DialogKey::DbInfo => {
                shell.dialog_ui.db_info_open
                    && shell.forms.db_form.is_some()
                    && shell.state.db.is_some()
            }
            DialogKey::Export => shell.state.exporting.is_some(),
            // 备份 / 导入进度：`dismissed` 时不进栈（用户点过「放到后台」），
            // 但任务照跑 —— 它**只影响浮层**，不影响任务本身。
            DialogKey::DbBackupProgress => shell
                .state
                .db
                .as_ref()
                .is_some_and(|db| db.backup.as_ref().is_some_and(|b| !b.dismissed)),
            DialogKey::DbImportProgress => shell
                .state
                .db
                .as_ref()
                .is_some_and(|db| db.import.as_ref().is_some_and(|i| !i.dismissed)),
            DialogKey::ContainerLogs => shell.state.logs.is_some(),
            DialogKey::ContainerDetail => shell.dialog_ui.detail.is_some(),
            DialogKey::DistroDetail => shell.dialog_ui.distro_detail.is_some(),
            DialogKey::Exec => shell.state.exec.is_some(),
            DialogKey::Prompt => shell.forms.prompt.is_some(),
            DialogKey::DeployLogs => shell
                .state
                .deploy
                .as_ref()
                .is_some_and(|d| d.logs.is_some()),
            DialogKey::DeploySiteConf => shell
                .state
                .deploy
                .as_ref()
                .is_some_and(|d| d.site_conf.is_some()),
            DialogKey::DeployRemoveConfirm => shell
                .state
                .deploy
                .as_ref()
                .is_some_and(|d| d.confirm_remove.is_some()),
            DialogKey::DbBackupList => {
                shell.forms.db_form.is_some()
                    && shell
                        .state
                        .db
                        .as_ref()
                        .is_some_and(|db| db.backup_dialog.is_some())
            }
            DialogKey::DeployForm => shell.forms.deploy_form.is_some(),
            DialogKey::WslConf => shell.forms.wslconf_dialog.is_some(),
            DialogKey::PullImage => shell.dialog_ui.pull_open && shell.forms.image_form.is_some(),
            DialogKey::CreateContainer => shell.forms.create_dialog.is_some(),
            DialogKey::DbAction => shell.forms.db_form.is_some() && shell.forms.db_dialog.is_some(),
            DialogKey::Confirm => shell.state.confirm.is_some(),
        }
    }
}

/// 浮层内容：绝大多数是 [`ShellView`]，少数几个（prompt）要自己的子实体。
///
/// `Dialog` 的 builder 可能重跑，所以这里存的是 `Entity`（可 clone），
/// 每次重建时再取元素。
enum DialogContent {
    Shell(Entity<ShellView>),
    Prompt(Entity<crate::views::PromptContent>),
}

impl DialogContent {
    fn element(&self) -> AnyElement {
        match self {
            Self::Shell(view) => view.clone().into_any_element(),
            Self::Prompt(view) => view.clone().into_any_element(),
        }
    }
}

/// 一个浮层外壳的"差异部分"。
///
/// 其余东西（遮罩、宽度单位、Esc、`content` 的接线、`footer` 的重建）都由
/// [`open_shell_dialog`] 统一处理 —— 19 个臂原先各写一遍，只有这五样不同。
struct DialogSpec {
    title: SharedString,
    width: f32,
    /// 点遮罩关不关（有提交动作的浮层要 `false`，见 `AGENTS.md` §7.5）。
    overlay_closable: bool,
    /// Esc 关不关。
    keyboard: bool,
    content: DialogContent,
}

/// 把「读 `Shell` 状态画一块」包成会随 `Shell` 重画的子实体。
///
/// ⚠️ 必须包：`Dialog` 的 builder 闭包只在"浮层被重建"时重跑，打字 / 后台任务
/// 推进都**不**触发它 —— 不包就会僵在打开那一刻（v0.19.0 的 bug）。
fn shell_view(
    entity: &Entity<Shell>,
    body: impl Fn(&Shell, &App) -> AnyElement + 'static,
    cx: &mut Context<Shell>,
) -> DialogContent {
    let view = entity.clone();
    let view = cx.new(move |cx| ShellView::new(view, body, cx));
    DialogContent::Shell(view)
}

/// 统一打开一个官方 `Dialog`，并把**三条退出路径**（按钮 / Esc / 点遮罩）的
/// "清状态"接好。
///
/// `close` 传方法引用（`Shell::close_logs` 这类）—— 它的签名统一是
/// `fn(&mut Shell, &mut Context<Shell>)`。进度类浮层不可关，传 `None`。
///
/// ⚠️ `footer` 是**闭包**而不是 `Vec`：`Dialog` 的 builder 可能重跑，而
/// `AnyElement` 是一次性的，不能复用。
///
/// ⚠️ 清状态必须写在回调里：`sync_dialog_stack` 是**按状态对齐**的，
/// 只关窗口不清状态，下一帧会把它重新打开（`AGENTS.md` §7.5）。
fn open_shell_dialog<F>(
    window: &mut Window,
    cx: &mut Context<Shell>,
    spec: DialogSpec,
    close: Option<fn(&mut Shell, &mut Context<Shell>)>,
    footer: F,
) where
    F: Fn() -> Vec<AnyElement> + 'static,
{
    use gpui_kit::component::WindowExt as _;

    let entity = cx.entity();
    let on_cancel = close.map(|close| {
        let entity = entity.clone();
        move |_: &ClickEvent, _: &mut Window, cx: &mut App| -> bool {
            entity.update(cx, |shell, cx| {
                close(shell, cx);
                // 记账：返回 `true` 之后**官方自己**会关掉这一层
                // （`gpui-base` 的 `Cancel` 分支：`if cancel(..) { … request_close .. }`，
                // 而 `request_close` 就是我们传的 `window.close_dialog`）。
                // 不记账的话下一帧 `sync_dialog_stack` 会以为它还开着，**再关一层**。
                shell.forget_top_dialog();
            });
            true
        }
    });

    // `close: None` = 这个浮层**根本不该能关**（两个进度类）。官方那三条关闭路径
    // （✕ / Esc / 点遮罩）都绕过我们的状态：关了不记账、也不清状态，于是下一帧
    // 被状态重新打开 —— 看起来就是"关了又回来"。所以连 ✕ 和 Esc 一起关掉，
    // 只留页脚那颗「放到后台」。
    let closable = on_cancel.is_some();

    window.open_dialog(cx, move |dialog, _window, _cx| {
        let mut dialog = dialog
            .title(spec.title.clone())
            .w(px(spec.width))
            .overlay(true)
            .overlay_closable(spec.overlay_closable)
            .keyboard(spec.keyboard && closable)
            .close_button(closable)
            .child(spec.content.element());
        if let Some(on_cancel) = on_cancel.clone() {
            dialog = dialog.on_cancel(on_cancel);
        }
        let items = footer();
        if !items.is_empty() {
            dialog = dialog.footer(modal_footer(items));
        }
        dialog
    });
}

/// 页脚按钮的收尾：**关掉栈顶那一层，并把账一起记掉**。
///
/// ⚠️ 两件事必须成对。`window.close_dialog()` 只动窗口的 `active_dialogs`，
/// 而 `dialog_stack` 是我们自己的账 —— 少了记账，下一帧 `sync_dialog_stack`
/// 会认为那一层还开着，于是**再关一层**。
///
/// 实机症状（v0.21.0）：实例详情里点「压缩磁盘」→ 看到确认框 → 点「取消」→
/// **详情跟着一起消失**；更糟的是账里留了一条幽灵记录，
/// `desired == dialog_stack` 让 sync 永远早退，详情**再也点不出来**。
fn close_top_dialog(entity: &Entity<Shell>, window: &mut Window, cx: &mut App) {
    use gpui_kit::component::WindowExt as _;
    window.close_dialog(cx);
    entity.update(cx, |shell, _cx| shell.forget_top_dialog());
}

mod specs;
