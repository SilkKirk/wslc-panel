//! 表单族：折叠区、单选组、tab 排、表单字段（标签 + 输入框）。
//!
//! `common.rs` 与 `common/` 目录并存，本模块由 `common.rs` 用
//! `pub(crate) use form::*;` 再导出 —— 调用点仍然写 `common::…`。
//!
//! 判断标准：**它收的是"用户填/选的东西"吗？** 是 → 这里；
//! 只是把已经定好的东西摆出来的（卡片、键值对、空状态）→ `layout.rs`。

use super::super::prelude::*;

/// 展开状态只控制展示，折叠不会清空表单实体里的参数。
///
/// # 直接用官方 `Accordion`，不再自己拼折叠头
///
/// 早先这里是 `Collapsible` + 手画的 trigger（`Button` + `Icon` + `rotate`），
/// 注释里写着"和 `Accordion` 的 trigger 完全一致（读 `accordion.rs` 确认过）"——
/// **既然在模仿它，就该直接用它**。官方 `AccordionItem` 自带：
///
/// - `ChevronDown` + `rotate(0.5)`（展开转 180°，和我们手画的一模一样）；
/// - 面板用**弹簧动画**展开（`spring` + `MotionReveal`），并且**收起时卸载内容**
///   （"a settled closed panel unmounts, so its content costs no layout or paint"）；
/// - 触发的键盘可达性与焦点框。
///
/// # 三处必须显式盖掉官方默认（都是读源码得出的）
///
/// 1. `.bordered(false)`：官方 `Accordion` 默认**有**边框 + 圆角 + 裁切
///    （把整组画成一张卡片）。我们的折叠区**本来就在卡片里**，再套一圈就是两层；
/// 2. `.h_auto()`：`Accordion::render` 里写死了 `.size_full()`（`w_full` + `h_full`），
///    在 `v_flex` 卡片里 `h_full` 会把折叠区撑到整卡高度。
///    能盖掉是因为 `refine_style(&self.style)` 在 render 末尾；
/// 3. `.bg(theme::bg_card())`：官方 `AccordionItem` 会铺 `tokens.accordion` 底色，
///    我们在卡片里，得用卡片底色（`AGENTS.md` §5：颜色归我们，形态归组件）。
///
/// # 为什么回调要拿 `&[usize]` 做幂等判断，而不是无脑 toggle
///
/// 官方 `Accordion::on_toggle_click` 挂在**容器**上（`BaseAccordion.on_click`），
/// 而单项的 `on_toggle_click` 会被 `Accordion::render` 覆盖成它内部的记账。
/// 容器级回调在"点内容区"时也可能被触发 —— 所以这里**不能**无脑 `toggle_section`，
/// 而是先比对官方报的展开状态和我们的状态，**只在真的不一致时才翻**
/// （`open.is_empty()` = 收起了）。这样即使回调多触发一次，界面也不会自己乱折叠。
pub(crate) fn disclosure(
    section: UiSection,
    title: impl Into<String>,
    state: &AppState,
    entity: &Entity<Shell>,
    body: impl IntoElement,
) -> AnyElement {
    let open = state.section_open(section);
    let title = title.into();
    let entity = entity.clone();
    Accordion::new(SharedString::from(format!("section-{section:?}")))
        .bordered(false)
        .h_auto()
        .item(move |item| {
            item.title(title)
                .open(open)
                .bg(theme::bg_card())
                .child(body)
        })
        .on_toggle_click(move |open_ix, _, cx| {
            let now_open = !open_ix.is_empty();
            entity.update(cx, |shell, cx| {
                if shell.state.section_open(section) != now_open {
                    shell.state.toggle_section(section);
                    cx.notify();
                }
            });
        })
        .into_any_element()
}

/// 一组**互斥选项** —— 官方 `RadioGroup`（横向），选中项由 `selected` 指出。
///
/// # 为什么不再用"一排按钮 + 选中那颗改成 `.primary()`"
///
/// 那是本项目原先的写法（9 处）。问题是**语义错了**：一排按钮对读屏软件来说是
/// 一堆各自独立的"动作"，而不是"这组里选了哪个"；键盘上也拿不到
/// "方向键在组内移动、空格选中"那套官方行为。官方 `Radio` 自带这些。
///
/// # 从 `Radio` 源码（0.8.0）核出来的两条
///
/// 1. `RadioGroup::new` 的默认 style 带 **`.flex_1()`** —— 放进 `v_flex` 会被
///    竖向拉长（和 `Field` 是同一个坑），所以这里显式 `.flex_none()` 盖掉
///    （`refine_style` 在 render 末尾，能盖住）；
/// 2. 横向布局在 render 里是 `h_flex().w_full().flex_wrap()` + **固定 `gap_3()`**，
///    改不了。所以它适合"独占一行的一组选项"，**不适合塞进紧凑工具栏**
///    （日志浮层那排"行数 / 时间戳 / 刷新"就只能继续用按钮）。
///
/// 每项的 id 由官方按**位置**给（`radio.id = ix.into()`），组 id 是我们传的这个 ——
/// GPUI 的元素 id 是**路径**，所以同页多组之间不会撞（已按源码确认）。
///
/// `pick` 收的是"选中的下标"，由调用页映射回自己的值：
/// 组件的回调本来就只给 `&usize`，硬要泛型化成一个 `T` 反而要在调用点写更多。
pub(crate) fn radio_row(
    id: &'static str,
    labels: Vec<SharedString>,
    selected: Option<usize>,
    entity: &Entity<Shell>,
    pick: impl Fn(&mut Shell, usize, &mut Window, &mut Context<Shell>) + 'static,
) -> AnyElement {
    let entity = entity.clone();
    RadioGroup::horizontal(id)
        .flex_none()
        .children(labels.into_iter().map(Radio::from))
        .selected_index(selected)
        .on_change(move |ix, window, cx| {
            let ix = *ix;
            entity.update(cx, |shell, cx| pick(shell, ix, window, cx));
        })
        .into_any_element()
}

/// 一排**互斥的 tab** —— 官方 `TabBar` + `Tab`，选中项由 `selected` 指出。
///
/// # 为什么收成一处
///
/// 「应用设置」的四个 tab、「数据库管理」的两个 tab、SQL 的多结果 tab 原先
/// 各写一份接线：建 `Vec<Tab>` → 算选中下标 → `TabBar::new(id).pill()…on_click`。
/// 三份逐字相同，只有"哪个枚举"和"选完写回哪"不同 —— 而**同一类控件在一个
/// 应用里必须长得一样、行为一样**。
///
/// # 为什么不再用"一排按钮 + 选中那颗 `.primary()`"
///
/// 那是本项目的旧写法（v0.18 前 9 处）。它把**状态**画成了**动作**：一排按钮
/// 对读屏软件是一堆各自独立的动作，键盘上也拿不到 tab 该有的左右箭头移动。
/// 组件库本来就有 `TabBar`（下划线 / 胶囊等形态由 `TabVariant` 管）。
///
/// ⚠️ `on_click` 回调给的是**下标**（`&usize`），由调用页映射回自己的值 ——
/// 组件的 API 就是这样，硬泛型化成一个 `T` 反而让调用点更长。
///
/// ⚠️ 它**不适合塞进紧凑工具栏**：横向布局在 `RadioGroup` 那边有同样的限制，
/// 而这排 tab 自带间距与滚动，塞进一行按钮中间会把那一行撑坏。工具栏里的
/// 行数档位（日志浮层）因此仍是按钮。
pub(crate) fn tab_bar(
    id: &'static str,
    labels: Vec<SharedString>,
    selected: usize,
    entity: &Entity<Shell>,
    pick: impl Fn(&mut Shell, usize, &mut Window, &mut Context<Shell>) + 'static,
) -> AnyElement {
    use gpui_kit::component::tab::{Tab, TabBar};

    let entity = entity.clone();
    TabBar::new(id)
        .pill()
        .selected_index(selected)
        .children(labels.into_iter().map(|label| Tab::new().label(label)))
        .on_click(move |ix, window, cx| {
            let ix = *ix;
            entity.update(cx, |shell, cx| pick(shell, ix, window, cx));
        })
        .into_any_element()
}

/// 「创建容器」弹窗的**表单字段**：标签 + 输入框。
///
/// `full` 决定宽度：直接放进 `v_flex` 的用 `w_full`，
/// 放进 `h_flex` 成对排列的用 `flex_1`。
/// （在 `v_flex` 里写 `flex_1` 会把字段**竖向**拉长，是个容易踩的坑 ——
/// 官方 `Field::render` 第一行就是 `.flex_1()`，所以**两种宽度都必须显式覆盖**，
/// 见 [`form_field_with_error`]。）
pub(crate) fn form_field(
    id: &'static str,
    label: &'static str,
    input: &Entity<InputState>,
    cx: &App,
    full: bool,
) -> AnyElement {
    form_field_impl(id, label, input, cx, full, false, false)
}

/// 密码 / 口令字段：和 [`form_field`] 同形，但输入框默认遮住并在右侧给
/// 一个「显示 / 隐藏」按钮。
///
/// 用官方 `Input` 的 `content_type(Password)` + `mask_toggle()`；对应的
/// `InputState` 创建时还要 `.masked(true)`（两边都要，缺一个都可能只是
/// "看起来像密码"而没有真正的遮罩行为）。
///
/// ⚠️ 不要拿它包 DSN 那种"整段都要读"的字段：把 `postgres://…` 整段遮住
/// 反而没法核对主机 / 库名。DSN 的脱敏在展示那一行走 `redact_dsn`。
pub(crate) fn form_field_secret(
    id: &'static str,
    label: &'static str,
    input: &Entity<InputState>,
    cx: &App,
    full: bool,
) -> AnyElement {
    form_field_impl(id, label, input, cx, full, false, true)
}

/// 多行纯文本字段（官方 `Textarea`）：标签 + 可折行的输入区。
///
/// 只给"每行一条 / 可以粘多行"的字段用：环境变量、卷挂载、反代规则。
/// 单行字段仍用 [`form_field`] —— `Input` 的密码遮罩 / 清除按钮 / 前后缀
/// 都在那一层，混用会把简单字段也弄成多行。
pub(crate) fn textarea_field(
    id: &'static str,
    label: &'static str,
    input: &Entity<TextareaState>,
    cx: &App,
    full: bool,
) -> AnyElement {
    let empty = input.read(cx).value().trim().is_empty();
    let label_color = if empty {
        theme::text_muted()
    } else {
        theme::text_dim()
    };

    let field = Field::new()
        .label_fn(move |_, _| div().text_xs().text_color(label_color).child(label))
        .child(Textarea::new(input).id(id).w_full());

    if full {
        field.flex_none().w_full().into_any_element()
    } else {
        field.flex_1().min_w_0().into_any_element()
    }
}

/// [`form_field`] 的错误态版本：`invalid` 时标签和输入框边框一起变红。
///
/// 只给「添加实例」用：那一页点「开始安装」校验失败后，用红框指出哪一格
/// 要改，而不是在页面底部再堆一段和步骤预览重复的错误清单。
///
/// # 为什么外壳改用官方 `Field`
///
/// 原先这里是手写的 `v_flex().gap_1().child(标签 div).child(输入框)` —— 52 处调用点
/// 拼出来的形状完全一样。官方 `form` 模块给的正是这个壳（`Form` / `Field`），
/// 所以壳交给它，我们只保留"颜色"和"错误态"这两件它管不了的事
/// （`AGENTS.md` §5：颜色归我们，形态归组件）。
///
/// 读 `form/field.rs`（0.8.0）确认过的四件事：
///
/// 1. `Field::render` **第一行是 `.flex_1()`** —— 直接放进 `v_flex` 会把字段竖向
///    拉长（就是上面那条注释说的坑）。所以下面两种宽度都**显式覆盖**：
///    `full` → `.flex_none().w_full()`，否则 → `.flex_1().min_w_0()`。
///    `refine_style(&self.style)` 在 render 的最后才执行，我们的能盖掉它的默认值；
/// 2. 标签默认是 `text_sm + font_medium` + 主题色。我们这一套设计是
///    **`text_xs` + 变暗表示"必填未填"**，所以走 `label_fn` 自己画 ——
///    外面那层 `wrap_label` 的字号继承会被内层的 `.text_xs()` 覆盖；
/// 3. `description(..)` 渲染成 `text_xs + muted_foreground`，正好是我们要的"提示"
///    档位，将来要加说明不必再手写；
/// 4. **`Field` 没有 error 态**（只有 `required` 的红色星号）—— 所以"标签变红"
///    仍然由我们通过 `label_fn` 的颜色表达，"边框变红"仍然打在 `Input` 上。
///
/// ⚠️ 一处已知瑕疵：`Field` 内部会给自己的包裹层 `.id(self.id)`，而 `self.id`
/// 只有在**经由 `Form` 的 enumerate** 构建时才被喂进 `ix`（`Field::props`）。
/// 我们这里是**单独**建 `Field`（为了保留每个弹窗自己的布局），所以 id 恒为 0 ——
/// 多个字段共用一个元素 id。这些包裹层不挂任何 hover / 点击 / 滚动处理，
/// 共享的只是"没有状态"的那份状态，所以现在看不出问题；
/// 哪天有字段需要在 `Field` 外壳上做悬停效果，就得改成由 `Form` 统一建。
pub(crate) fn form_field_with_error(
    id: &'static str,
    label: &'static str,
    input: &Entity<InputState>,
    cx: &App,
    full: bool,
    invalid: bool,
) -> AnyElement {
    form_field_impl(id, label, input, cx, full, invalid, false)
}

/// [`form_field`] / [`form_field_secret`] / [`form_field_with_error`] 的唯一实现。
///
/// 三个公开函数只是"要不要错误态 / 要不要密码遮罩"的两个布尔组合；
/// 外壳和宽度覆盖逻辑只写这一份。
fn form_field_impl(
    id: &'static str,
    label: &'static str,
    input: &Entity<InputState>,
    cx: &App,
    full: bool,
    invalid: bool,
    secret: bool,
) -> AnyElement {
    // 取值只为在为空时把标签调暗一点，让"必填未填"看得见。
    let empty = input.read(cx).value().trim().is_empty();
    let label_color = if invalid {
        theme::danger()
    } else if empty {
        theme::text_muted()
    } else {
        theme::text_dim()
    };

    let mut input_el = Input::new(input).id(id).w_full();
    if secret {
        input_el = input_el
            .content_type(InputContentType::Password)
            .mask_toggle();
    }
    if invalid {
        input_el = input_el.border_color(theme::danger());
    }

    let field = Field::new()
        // 标签自己画：颜色要说"必填未填"，字号要压到 `text_xs`。
        .label_fn(move |_, _| div().text_xs().text_color(label_color).child(label))
        .child(input_el);

    if full {
        field.flex_none().w_full().into_any_element()
    } else {
        field.flex_1().min_w_0().into_any_element()
    }
}
