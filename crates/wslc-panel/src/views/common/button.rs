//! 按钮族：行内小按钮的唯一外壳 + 五个按语义命名的具名助手。
//!
//! `common.rs` 与 `common/` 目录并存，本模块由 `common.rs` 用
//! `pub(crate) use button::*;` 再导出 —— 调用点仍然写 `common::…`。
//!
//! 判断标准：**点它会跑一段动作吗？** 是 → 这里；
//! 只是把状态画出来的（徽标、链接、卡片）→ 别处。

use super::super::prelude::*;

/// **行内小按钮**的唯一实现：一行字、点下去跑一段动作。
///
/// # 为什么要有这一层
///
/// 下面那五个具名助手原先各写一份
/// `Button::new(id).label(label).small().on_click(…)` —— 五份一模一样的外壳，
/// 只有点击体不同。外壳一旦要改（换尺寸、加 tooltip、换成带图标的变体），
/// 就得记得改五处；漏一处的表现是"这一列的按钮和别处长得不一样"，
/// 而这种差异 CI 完全看不出来（`AGENTS.md` §8）。
///
/// 现在外壳只有这一份。五个具名助手保留，是因为**它们的名字说明语义**
/// （这个按钮会弹确认 / 会直接跑 / 只是开个浮层），调用点读起来比
/// `action_button(id, label, |shell, _, cx| …)` 清楚。
pub(crate) fn action_button(
    id: &str,
    label: &'static str,
    entity: &Entity<Shell>,
    run: impl Fn(&mut Shell, &mut Window, &mut Context<Shell>) + 'static,
) -> Button {
    let entity = entity.clone();
    Button::new(SharedString::from(id.to_owned()))
        .label(label)
        .small()
        .on_click(move |_, window, cx| {
            entity.update(cx, |shell, cx| run(shell, window, cx));
        })
}

/// 危险操作按钮（点击后弹出二次确认）。
pub(crate) fn danger_button(
    id: &str,
    label: &'static str,
    action: PendingAction,
    entity: &Entity<Shell>,
) -> impl IntoElement {
    action_button(id, label, entity, move |shell, _, cx| {
        shell.request(action.clone(), cx);
    })
}

/// 危险操作按钮（**发行版域**，点击后弹出二次确认）。
///
/// 和 [`danger_button`] 分开写，而不是合并成一个泛型函数：
/// 两个动作类型（[`PendingAction`] / [`DistroAction`]）没有任何共同方法，
/// 合并就得引入 trait 或枚举包装，调用点反而更长。
/// 重复的只是这一层薄包装 —— 按钮外壳已经收进 [`action_button`]。
pub(crate) fn danger_button_distro(
    id: &str,
    label: &'static str,
    action: DistroAction,
    entity: &Entity<Shell>,
) -> impl IntoElement {
    action_button(id, label, entity, move |shell, _, cx| {
        shell.request_distro(action.clone(), cx);
    })
}

/// 打开「单输入框提示弹窗」的按钮（移动位置 / 调整大小 / 设置默认用户）。
///
/// 和 [`danger_button_distro`] 的区别：那个弹**确认**，这个弹**输入框**。
pub(crate) fn prompt_button(
    id: &str,
    label: &'static str,
    kind: PromptKind,
    distro: &str,
    entity: &Entity<Shell>,
) -> AnyElement {
    let distro = distro.to_owned();
    action_button(id, label, entity, move |shell, window, cx| {
        // `window` 是必需的：`InputState::new` 要 `&mut Window`。
        shell.open_prompt(kind, distro.clone(), window, cx);
    })
    .into_any_element()
}

/// 即时操作按钮（启动 / 重启）。
///
/// 与 [`danger_button`] 的区别：不弹二次确认，点了就跑。
pub(crate) fn immediate_button(
    id: &str,
    label: &'static str,
    action: ImmediateAction,
    entity: &Entity<Shell>,
) -> impl IntoElement {
    action_button(id, label, entity, move |shell, _, cx| {
        shell.run_immediate(action.clone(), cx);
    })
}

/// 打开一个**面板内浮层**的按钮（日志 / exec）。
///
/// 和 [`immediate_button`] / [`danger_button`] 并列的第三种入口：
/// 那两个分别是"直接起进程"和"弹二次确认"，这一类只是把某个浮层打开
/// （浮层自己再去后台取数据）。
///
/// 需要 `&mut Window`：`exec` 浮层要建 `InputState`，而 `InputState::new`
/// 要 `&mut Window` —— 所以 window 必须**从点击回调里取**，不能提前存
/// （提前存的话 `Shell::new` 拿不到 window，见 `AGENTS.md` §7.5 的懒创建）。
pub(crate) fn overlay_button(
    id: &str,
    label: &'static str,
    open: impl Fn(&mut Shell, &mut Window, &mut Context<Shell>) + 'static,
    entity: &Entity<Shell>,
) -> AnyElement {
    action_button(id, label, entity, open).into_any_element()
}
