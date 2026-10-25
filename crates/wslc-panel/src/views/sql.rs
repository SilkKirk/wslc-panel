//! SQL 控制台（整页工作区）：左树 + 编辑器 + 结果表。

use super::common::{log_text, page_empty};
use super::prelude::*;

/// 「SQL 控制台」**整页**（v0.12）：左侧库 / 表树 + 右上编辑器 + 右下结果。
///
/// # 为什么从浮层改成整页
///
/// v0.9.2 起它是浮层（`db_console_overlay`）。实机用下来三个问题：
///
/// 1. 浮层盖住页面，**看着像弹窗**，而写 SQL 往往是个持续十分钟的事；
/// 2. 浮层里塞"树 + 编辑器 + 表格"三块，哪块都不够高（树 `max_h(440)`、
///    表格 `h(360)`，加起来就顶到 640 的天花板了）；
/// 3. 关掉就全没了，视觉上像"退出"，而用户只是想换个查询。
///
/// 用户的原话："别弹窗出来，直接跳到新页面，左边的列表树左对齐，右上面是
/// 一个输入的大框，右下显示数据"。所以这一页：
///
/// - **独占右侧区域**（不套 `app-body-scroll` 的 `p_5`，见 `app.rs` 里那段
///   按页分支），树贴着内容区的左边框；
/// - 编辑器在右上、结果在右下，两块都靠 `flex_1` 吃满剩下的高度。
///
/// # 高度是怎么分掉的
///
/// ```text
/// 页头（flex_none，一行）
/// ──────────────────────────────────────
/// 树 260px  │  编辑器（flex_none，编辑框固定 170px）
/// 全高      │  ─────────────────────────────
///           │  结果（flex_1，表格自己虚拟滚动）
/// ```
///
/// ⚠️ 编辑器给的是**固定** `h(170.)` 而不是 `flex_1`：`Editor::h()` 收的是
/// `DefiniteLength`（px / relative / auto），没有 flex 语义。而 `DataTable`
/// 恰恰相反 —— 它是虚拟滚动的，**必须**知道自己多高，所以那一边用
/// `flex_1 + min_h_0` 让布局算出一个确定值。
pub fn sql_console_page(
    shell: &Shell,
    form: &DbPageForm,
    cx: &App,
    entity: &Entity<Shell>,
) -> AnyElement {
    let Some(db) = shell.state.db.as_ref() else {
        return page_empty("还没有选中容器 —— 请到「容器」页点某一行的「数据库」");
    };
    // 页头：返回 + 库名徽标 + 刷新表结构。
    //
    // ⚠️ 这里**不重复写**"SQL 控制台"四个字：应用外壳（`app.rs` 的页头）
    // 已经在标题位置显示 `Page::Sql.label()` 了。同一屏出现两遍同一个标题
    // 是"同一份信息两个入口"的典型症状（`AGENTS.md` §5），所以这里只留
    // **这一页特有**的东西：从哪来（返回）、对哪个库、重读表结构。
    //
    // 不放「重新读取」（那会重跑整个数据库探测）：这一页要重读的只有表结构，
    // 给一个精确的按钮，而不是让用户拿一个大而全的刷新去猜它会重做什么。
    // 折叠侧边栏的按钮。
    //
    // ⚠️ 它原来在应用级页头里，而 SQL 页**不画**那一层（见 `app.rs` 的说明），
    // 所以搬到这里来 —— 侧边栏收起后那块不渲染，按钮必须留在**不会被收起**
    // 的地方，否则点没了就再也开不回来。
    let sidebar_toggle = {
        let entity = entity.clone();
        let collapsed = shell.sidebar_collapsed;
        SidebarToggleButton::new()
            .collapsed(collapsed)
            .on_click(move |_, _, cx| {
                entity.update(cx, |shell, cx| {
                    shell.sidebar_collapsed = !shell.sidebar_collapsed;
                    cx.notify();
                });
            })
    };
    // 返回按钮：专业 SQL 客户端是"一个连接下拉框"，我们是"从数据库页进来"，
    // 所以留一个返回。用 `text` 变体（无底色）—— 顶栏上的按钮越少越好。
    let back = {
        let entity = entity.clone();
        Button::new("sql-back-to-db")
            .label("← 数据库")
            .small()
            .on_click(move |_, window, cx| {
                entity.update(cx, |shell, cx| shell.close_db_console(window, cx));
            })
    };
    // 库名徽标：`dbx` 那种工作区都会把"当前连的是哪个库"钉在显眼处。
    //
    // 库名是**用户数据**，必须过 `strip_md` —— MySQL 允许库名里带 `**`，
    // 直接 `.child()` 上去会真的把星号画在屏幕上（`AGENTS.md` 那条规矩）。
    //
    // "未指定库"那种情况不加底色：它是个"没什么可显示"的状态，
    // 给它一块色块反而比一行淡字更抢眼。而 `theme` 里没有 `bg_subtle`
    // 这类更淡的卡片色，与其为它新造一个色值，不如就别画。
    let db_badge: AnyElement = match db.sql_database.as_deref() {
        Some(name) => div()
            .px_2()
            .py_0p5()
            .rounded_md()
            .bg(theme::primary_soft())
            .text_xs()
            .text_color(theme::primary())
            .child(crate::util::strip_md(name)),
        None => div()
            .text_xs()
            .text_color(theme::text_muted())
            .child("未指定库"),
    }
    .into_any_element();
    // 这一页实际用哪个账号。
    //
    // ⚠️ 它**不等于**连接卡上那个：卡是"管理账号"（用户填的，通常 root），
    // 而从某个库的行进来时，执行用的是**那个库自己的**账号
    // （`Shell::creds_for_row`）。所以必须写出来 —— 否则用户以为自己是 root 在查，
    // 实际是业务账号，遇上报错会对不上（实机反馈过）。
    let account = match db.sql_database.as_deref() {
        Some(name)
            if shell
                .state
                .db_credentials
                .get(&db.container, name)
                .is_some() =>
        {
            format!(
                "账号 {}（这个库的）",
                shell.creds_for_row(&db.container, name, cx).user
            )
        }
        _ => format!("账号 {}（连接卡）", shell.db_creds(cx).user),
    };
    // 树那一栏：收起时留一条**窄条**（32px）而不是完全消失 ——
    // 窄条上就是"展开"按钮，否则用户把树收起来之后找不到开回来的入口。
    // （专业 SQL 客户端也是这个做法：侧栏收起后留一条能点开的边。）
    let tree_column: AnyElement =
        if shell.tree_collapsed {
            let entity = entity.clone();
            v_flex()
                .w(px(32.))
                .h_full()
                .flex_none()
                .items_center()
                .p_1()
                .border_r_1()
                .border_color(theme::border())
                .child(Button::new("sql-expand-tree").label("»").small().on_click(
                    move |_, _, cx| {
                        entity.update(cx, |shell, cx| {
                            shell.tree_collapsed = false;
                            cx.notify();
                        });
                    },
                ))
                .into_any_element()
        } else {
            sql_tree_pane(db, shell, entity, cx)
        };
    v_flex()
        .w_full()
        .h_full()
        .min_h_0()
        .child(
            h_flex()
                .w_full()
                .flex_none()
                .gap_3()
                .items_center()
                .px_3()
                .py_2()
                .border_b_1()
                .border_color(theme::border())
                // 工具栏只留三样：侧边栏折叠、返回、当前库。
                // 「刷新表结构」搬进树头部的 `⟳`，「隐藏表结构」搬进树头部的 `«`。
                .child(sidebar_toggle)
                .child(back)
                .child(db_badge)
                .child(div().flex_1()),
        )
        .child(
            h_flex()
                .w_full()
                .flex_1()
                .min_h_0()
                .items_start()
                .child(tree_column)
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .min_h_0()
                        .child(sql_editor_pane(db, form, entity, &account, cx))
                        .child(sql_results_pane(db, shell, entity, cx)),
                ),
        )
        .into_any_element()
}
/// 编辑器上方那行"这次执行会落在哪个库"。
///
/// 从数据库列表行上的「SQL」按钮进来时就是那一行；不指定的话
/// `SELECT * FROM t` 会报 "No database selected"（实测过），
/// 所以这件事必须显式画出来，而不是让用户从报错里猜。
pub(crate) fn sql_target_db(db: &DbState, entity: &Entity<Shell>, account: &str) -> AnyElement {
    match db.sql_database.as_deref() {
        Some(name) => {
            let clear = {
                let entity = entity.clone();
                Button::new("db-sql-clear-db")
                    .label("改为不指定")
                    .small()
                    .on_click(move |_, _, cx| {
                        entity.update(cx, |shell, cx| shell.clear_sql_database(cx));
                    })
            };
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_dim())
                        .child("当前库"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text())
                        .child(crate::util::strip_md(name)),
                )
                .child(clear)
                // 账号和库同样是"这次执行落在哪儿"的一部分 —— 而且它**不等于**
                // 连接卡上那个（卡是管理账号，这里可能是这个库自己的账号）。
                .child(div().text_xs().text_color(theme::text_dim()).child("·"))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child(crate::util::strip_md(account)),
                )
                .into_any_element()
        }
        None => div()
            .text_xs()
            .text_color(theme::text_dim())
            .child(format!(
                "当前库：不指定（语句里要自己 USE <库>）· {account}"
            ))
            .into_any_element(),
    }
}
/// 右上：SQL 编辑器（多行 + 行号 + 语法高亮）+ 执行按钮。
pub(crate) fn sql_editor_pane(
    db: &DbState,
    form: &DbPageForm,
    entity: &Entity<Shell>,
    account: &str,
    _cx: &App,
) -> AnyElement {
    let run: AnyElement = if db.busy {
        // ⚠️ 点下去必须有变化（`AGENTS.md` §7.1）：从点击到结果出来这段时间
        // 界面上什么都不动的话，用户会以为按钮坏了然后点第二次。
        div()
            .text_sm()
            .text_color(theme::text_muted())
            .child("执行中…")
            .into_any_element()
    } else {
        let entity = entity.clone();
        Button::new("db-run-sql")
            .label("执行")
            .small()
            .primary()
            .on_click(move |_, window, cx| {
                entity.update(cx, |shell, cx| shell.run_db_console(window, cx));
            })
            .into_any_element()
    };
    // 回显这次输出是哪条语句的 —— 连着跑两条之后没有这一行就分不清了
    // （和 exec 浮层里那条"上一次执行"同一个理由）。
    let echo: AnyElement = if db.last_sql.is_empty() {
        div().into_any_element()
    } else {
        h_flex()
            .w_full()
            .gap_2()
            .items_center()
            .child(
                div()
                    .text_xs()
                    .text_color(theme::text_dim())
                    .child("上一次执行"),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .font_family("Consolas")
                    .text_xs()
                    .text_color(theme::text_muted())
                    .overflow_hidden()
                    .truncate()
                    // 多行语句在这里显示成一行：换行显示不了，
                    // 换成 `⏎` 让用户知道原语句是分行的。
                    .child(db.last_sql.replace('\n', " ⏎ ")),
            )
            .into_any_element()
    };
    v_flex()
        .w_full()
        .flex_none()
        .gap_2()
        .p_4()
        .border_b_1()
        .border_color(theme::border())
        .child(
            h_flex()
                .w_full()
                .gap_3()
                .items_center()
                // ⚠️ 顺序：**执行在左、当前库在右**（v0.12.4 按用户要求换过）。
                //
                // 原来的顺序是"当前库 …… 执行"，那个布局把最常用的动作
                // 推到了最右边。专业客户端（dbx / DBeaver）都是把"跑"
                // 放在编辑区左手边、紧挨着代码，连接/库的切换放在右边 ——
                // 眼睛从代码往下扫的第一站应该就是执行。
                .child(run)
                .child(div().flex_1())
                // 这里**不要**写 `&account`：参数类型是 `&str`，而 `String`
                // 会自动借成 `&str`；写成 `&account` 多一层引用
                // （clippy 的 `needless_borrow`）。`account` 在这句之后不再用到，
                // 所以直接传也不会被移动影响。
                .child(sql_target_db(db, entity, account)),
        )
        // SQL 编辑器本体：行号 / 缩进线 / SQL 语法高亮都来自 `Editor`
        // （`language("sql")` 是在 `ensure_db_form` 里设的）。
        //
        // ⚠️ 这里**不用** `form_field`：那个辅助函数是给单行 `InputState` 的
        // （它读 `.value()`、画 `Input`）。`Editor` 的 API 不一样，
        // 硬套会编不过 —— 而它也不会因为这个就"看起来像"，是直接报错。
        .child(div().w_full().child(Editor::new(&form.sql).h(px(170.))))
        .child(div().text_xs().text_color(theme::text_muted()).child(
            // ⚠️ 这句话说的是"多条语句是一条条单独跑的"，那是**用户看不出来**
            // 的行为（会以为是一次性提交），而且解释了为什么语句多会慢。
            //
            // 🔴 **不要**再写"每条约 70ms"。那是 exec 时代（每条起一个 `mysql`
            // 子进程）的实测数字；v0.16 起走原生连接，固定开销变成一次建连，
            // 那个数字已经不成立 —— 写着只会让人以为查询本身慢。
            "多条语句逐条执行",
        ))
        .child(echo)
        .into_any_element()
}
/// 左侧：库 / 表树（贴内容区左边框，全高）。
///
/// # 为什么用组件的 `Tree` 而不是自己画行
///
/// v0.12 这一版是**自己用 `Button` 一行行画**的，实机一看就丑：
/// `Button` 会把文字**居中**、给每一行套一个整行宽度的底色与内边距，
/// 于是 110 行看起来像 110 个色块，缩进也读不出来（`Button` 会把前导空格
/// 一起居中，`    sys_user` 和 `sys_user` 看起来只差一点点）。
///
/// `gpui-kit` 自带 `component::tree::Tree`：虚拟滚动、逐级展开、选中态、
/// 悬停态、右键菜单都是现成的，官方 `examples/editor` 就用它做文件树。
/// 它自己管**展开/收起**和**选中**，我们只负责把 `TreeItem` 喂进去、
/// 把每行画成什么样。
///
/// # 缩进是它给的
///
/// `entry.depth()` 是当前节点在树里的层级，`ListItem` 用
/// `pl(px(14.) * depth + px(10.))` 缩进 —— 和我们手写时的 4 个空格不同，
/// 它按"层"对齐，长表名和短表名不会看着参差不齐。
///
/// # 两种形态（见 `Shell::rebuild_schema_tree`）
///
/// - 没搜索：库是父节点、表是子节点，点库名展开；
/// - 有搜索：拍平成一层的「库.表」—— 否则用户搜到的那张表还藏在一个
///   收起的库节点里，等于白搜。
pub(crate) fn sql_tree_pane(
    db: &DbState,
    shell: &Shell,
    entity: &Entity<Shell>,
    _cx: &App,
) -> AnyElement {
    let pane = |inner: AnyElement| -> AnyElement {
        v_flex()
            .w(px(260.))
            .h_full()
            .flex_none()
            .min_h_0()
            .gap_2()
            .p_3()
            .border_r_1()
            .border_color(theme::border())
            .child(inner)
            .into_any_element()
    };

    // 树头一行：左边小字说明，右边两个**图标**按钮。
    //
    // 为什么改成图标：原来页头上有「刷新表结构」「隐藏表结构」两个带文字的
    // 按钮，加上应用级那层标题 / 刷新，一屏顶部有四五个按钮 —— 专业的 SQL
    // 客户端（dbx / DBeaver）顶栏是很少几个图标的。
    //
    // 用「⟳」「«」这两个字符而不是 `IconName`：`IconName` 的变体名得逐个
    // 确认存在（猜错就是编译错误，而本地编不了），这两个字符是 Unicode 里
    // 标准存在、任何字体都有的。
    let tree_header = |label: String| -> AnyElement {
        let refresh_entity = entity.clone();
        let collapse_entity = entity.clone();
        h_flex()
            .w_full()
            .items_center()
            .justify_between()
            .child(div().text_xs().text_color(theme::text_dim()).child(label))
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(Button::new("sql-reload-tree").label("⟳").small().on_click(
                        move |_, _, cx| {
                            refresh_entity.update(cx, |shell, cx| shell.load_schema_tree(cx));
                        },
                    ))
                    .child(
                        Button::new("sql-collapse-tree")
                            .label("«")
                            .small()
                            .on_click(move |_, _, cx| {
                                collapse_entity.update(cx, |shell, cx| {
                                    shell.tree_collapsed = true;
                                    cx.notify();
                                });
                            }),
                    ),
            )
            .into_any_element()
    };

    if db.schema_tree.is_empty() {
        return pane(
            v_flex()
                .w_full()
                .gap_2()
                // 读不到的时候**更**需要一个 ⟳ —— 那正是要重试的时候。
                .child(tree_header("表结构".to_owned()))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child("读不到表结构（账号可能没有 information_schema 权限）"),
                )
                .into_any_element(),
        );
    }

    let total = db.schema_tree.table_count();
    // 搜索框：110 张表必须能过滤，否则铺开没人看得下去。
    //
    // 过滤**不是**在这里做的 —— 关键字一变就由 `ensure_schema_search` 的
    // 订阅去 `rebuild_schema_tree` 重灌 items（见那里的注释）。
    let search: AnyElement = match shell.schema_search.as_ref() {
        Some(input) => div()
            .w_full()
            .child(Input::new(input).id("db-schema-search").w_full())
            .into_any_element(),
        None => div().into_any_element(),
    };

    // 树本体。`TreeState` 由 `Shell` 持有并负责重建（纯数据在
    // `DbState::schema_tree`，界面状态必须在 `Shell` 里）。
    let tree_body: AnyElement = match shell.schema_tree_state.as_ref() {
        Some(state) => {
            let entity = entity.clone();
            gpui_kit::component::tree::tree(
                state,
                // ⚠️ 最后一个参数是 `_cx`（不是 `cx`）：这个回调**不用** `Context`
                // —— 点击是塞进内层 `on_click` 的闭包里的，那儿的 `cx` 是另一个。
                // 写成 `cx` 会被 `unused_variables` 报（clippy 日志里一直有这条）。
                move |ix, entry, _selected, _window, _cx| {
                    let item = entry.item();
                    // 只取这三样：`TreeEntry` 每帧每行都会被调用，
                    // 克隆整棵子树（`item.clone()`）会把它的 children 一起复制。
                    let label = item.label.clone();
                    let id = item.id.clone();
                    let is_folder = entry.is_folder();
                    let depth = entry.depth();
                    let click_entity = entity.clone();

                    // 库（父节点）用半粗、表名用常规色 —— 一眼能分出层级。
                    // 两支而不是 `.when(...)`：项目里没验证过 `when` 存在，
                    // 而 `font_semibold` 是 `StyledExt` 里确认有的。
                    let label_el: AnyElement = if is_folder {
                        div()
                            .w_full()
                            .min_w_0()
                            // 长表名省略号收尾。`truncate` 要在有确定宽度的
                            // 父级里才生效（`w_full` + `min_w_0`）。
                            .truncate()
                            .text_sm()
                            .font_semibold()
                            .child(label)
                            .into_any_element()
                    } else {
                        div()
                            .w_full()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .child(label)
                            .into_any_element()
                    };

                    ListItem::new(ix)
                        .w_full()
                        .px_2()
                        .py_1()
                        // 缩进按"层"算，不按字符补空格 —— 长短表名才会对齐。
                        .pl(px(10.) + px(14.) * depth)
                        .child(label_el)
                        .on_click(move |_, window, cx| {
                            // 点库节点什么都不做：展开 / 收起是 `Tree` 自己的行为，
                            // 我们再toggle 一次会变成"点了没反应"（连开带关）。
                            if is_folder {
                                return;
                            }
                            let id = id.to_string();
                            click_entity.update(cx, |shell, cx| {
                                shell.on_schema_tree_click(&id, window, cx)
                            });
                        })
                },
            )
            .h_full()
            .into_any_element()
        }
        // 数据有了但 items 还没灌（极短的一瞬）：给一句话，别画空白。
        None => div()
            .w_full()
            .text_xs()
            .text_color(theme::text_muted())
            .child("正在准备表结构…")
            .into_any_element(),
    };

    pane(
        v_flex()
            .w_full()
            .h_full()
            .min_h_0()
            .gap_2()
            .child(tree_header(format!("表结构 · {total} 张表")))
            .child(search)
            .child(div().w_full().flex_1().min_h_0().child(tree_body))
            .into_any_element(),
    )
}

/// 右下：结果区（多语句 tab + 结果表格 + 页脚 + 导出 / 原始输出）。
///
/// # 三种形态
///
/// 1. **没有任何结果**（全是 DDL）→ 一句话，不画空表格；
/// 2. **有表格** → `DataTable` + 页脚；
/// 3. **结果集为空但有列名**（`SELECT ... WHERE 1=0`）→ 也画表格（空的），
///    因为列名本身是有效信息。
///
/// # 为什么表格是 `flex_1`
///
/// `DataTable` 是**虚拟滚动**的：它需要知道"我有多高"才能算出可见行范围。
/// 放在高度不定的容器里会算错（表现为滚动条乱跳 / 底部空白）。
/// 所以这一层用 `flex_1 + min_h_0`，让布局给出确定高度 ——
/// 父链上每一层都有确定高度（页面 `h_full` → 右侧列 `h_full` → 这里 `flex_1`）。
pub(crate) fn sql_results_pane(
    db: &DbState,
    shell: &Shell,
    entity: &Entity<Shell>,
    _cx: &App,
) -> AnyElement {
    // 统一的容器：吃满剩余高度 + 内边距。三种形态共用，免得各写一遍。
    let shell_area = |inner: AnyElement| -> AnyElement {
        v_flex()
            .w_full()
            .flex_1()
            .min_h_0()
            .p_4()
            .child(inner)
            .into_any_element()
    };
    // 报错优先：这时候没有结果表格可看。
    if let Some(error) = db.sql_error.as_ref() {
        return shell_area(
            v_flex()
                .w_full()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::danger())
                        .child("执行失败"),
                )
                // 原样保留换行：`run_statements` 会说清第几条出错、
                // 以及前几条已经生效了。
                .child(log_text(Some(error), "db-sql-error"))
                .into_any_element(),
        );
    }
    if !db.sql_ran {
        return shell_area(
            div()
                .text_sm()
                .text_color(theme::text_dim())
                .child("还没有执行任何语句")
                .into_any_element(),
        );
    }
    let Some(result) = db.sql_results.get(shell.sql_result_tab) else {
        return shell_area(
            div()
                .text_sm()
                .text_color(theme::text_dim())
                .child("执行成功，没有返回行")
                .into_any_element(),
        );
    };
    // 多语句：结果之间用一排小按钮切换。
    let tabs: AnyElement = if db.sql_results.len() > 1 {
        let buttons: Vec<AnyElement> = db
            .sql_results
            .iter()
            .enumerate()
            .map(|(ix, r)| {
                let entity = entity.clone();
                let active = ix == shell.sql_result_tab;
                let label = crate::sqlview::result_tab_label(ix, r);
                let mut b = Button::new(SharedString::from(format!("db-sql-tab-{ix}")))
                    .label(label)
                    .small();
                if active {
                    b = b.primary();
                }
                b.on_click(move |_, window, cx| {
                    entity.update(cx, |shell, cx| shell.switch_sql_result_tab(ix, window, cx));
                })
                .into_any_element()
            })
            .collect();
        h_flex()
            .w_full()
            .gap_1()
            .children(buttons)
            .into_any_element()
    } else {
        div().into_any_element()
    };
    // 表格本体。`shell.sql_table` 为 `None` 说明这个结果没有表格
    // （DDL / 无输出），画一句话就够。
    let grid: AnyElement = match shell.sql_table.as_ref() {
        Some(state) => div()
            .w_full()
            .flex_1()
            .min_h_0()
            .child(DataTable::new(state))
            .into_any_element(),
        None => div()
            .text_sm()
            .text_color(theme::text_dim())
            .child("执行成功，没有返回行")
            .into_any_element(),
    };
    // 页脚：行数 / 列数 / 耗时。文案规则在 `sqlview::footer_text`（有单测）。
    let footer = div()
        .text_xs()
        .text_color(theme::text_muted())
        .child(crate::sqlview::footer_text(result, db.sql_elapsed_ms));
    // 导出 CSV：只有真的有表格时才给按钮 —— 空结果导出一个只有表头的 CSV
    // 没有意义，只会让用户以为导出坏了。
    let export: AnyElement = if result.columns.is_empty() {
        div().into_any_element()
    } else {
        let entity = entity.clone();
        Button::new("db-sql-export")
            .label("导出 CSV")
            .small()
            .on_click(move |_, window, cx| {
                entity.update(cx, |shell, cx| shell.export_sql_result_csv(window, cx));
            })
            .into_any_element()
    };
    // 原始输出开关：排查列宽 / 字符集问题时，mysql 原样打了什么最有用。
    //
    // 用官方 `Switch` 而不是按钮：它控制的是"下面这块面板显不显示"，
    // 是个**状态**；按钮的"看原始输出"读起来像一次性动作，按完不知道还能不能再按。
    let raw_toggle: AnyElement = match db.sql_raw.as_ref() {
        Some(raw) if !raw.trim().is_empty() => {
            let entity = entity.clone();
            Switch::new("db-sql-raw-toggle")
                .label("原始输出")
                .checked(shell.sql_raw_open)
                .on_change(move |checked: &bool, _, cx| {
                    let open = *checked;
                    entity.update(cx, |shell, cx| shell.set_sql_raw_open(open, cx));
                })
                .into_any_element()
        }
        _ => div().into_any_element(),
    };
    let raw_panel: AnyElement = if shell.sql_raw_open {
        match db.sql_raw.as_deref() {
            Some(raw) => log_text(Some(raw), "db-sql-raw"),
            None => div().into_any_element(),
        }
    } else {
        div().into_any_element()
    };
    shell_area(
        v_flex()
            .w_full()
            .h_full()
            .min_h_0()
            .gap_2()
            .child(tabs)
            .child(grid)
            .child(
                h_flex()
                    .w_full()
                    .flex_none()
                    .justify_between()
                    .items_center()
                    .child(footer)
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(raw_toggle)
                            .child(export),
                    ),
            )
            .child(raw_panel)
            .into_any_element(),
    )
}
