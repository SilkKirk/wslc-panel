//! SQL 控制台的库 / 表树（官方 `Tree`）：搜索、重建、点击、刷新。

use super::super::super::prelude::*;
use super::super::Shell;

/// `TreeItem` 的 id 分隔符。
///
/// # 为什么不是 `.` 或 `::`
///
/// MySQL 的标识符几乎什么字符都能含（反引号里连空格和点都可以），
/// 所以拿 `.` 拼 `库.表` 再拆开会**拆错**：库名 `a.b` + 表名 `c` 和
/// 库名 `a` + 表名 `b.c` 拼出来一样。
///
/// `U+0001`（Start of Heading）是控制字符，不可能出现在标识符里 ——
/// 用它当分隔符，拆解就是无歧义的。
///
/// ⚠️ **拼和拆都在本文件**（[`leaf_id`] / [`folder_id`] 拼、
/// [`Shell::on_schema_tree_click`] 拆），必须用同一个常量：两处各写一个字面量
/// `'\u{1}'` 的话，改了一边就是"点了没反应"，而且不会有任何编译错误。
const TREE_ID_SEP: char = '\u{1}';

/// 表节点（叶子）的 id：`t\u{1}库\u{1}表`。
fn leaf_id(database: &str, table: &str) -> String {
    format!("t{TREE_ID_SEP}{database}{TREE_ID_SEP}{table}")
}

/// 库节点（父节点）的 id：`d\u{1}库`。
///
/// 只需要唯一 —— 点库节点时组件自己展开/收起，我们不解析它。
fn folder_id(database: &str) -> String {
    format!("d{TREE_ID_SEP}{database}")
}

impl Shell {
    /// 确保搜索框存在（`InputState::new` 要 `&mut Window`，所以懒创建）。
    pub(crate) fn ensure_schema_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.forms.schema_search.is_some() {
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("过滤库 / 表名"));
        // 订阅：打字时**重建树**（不是"只重绘"）。
        //
        // 过滤不能用"渲染时临时筛"那套了：`TreeState` 的 items 是**喂进去**的，
        // 组件自己不知道原始数据长什么样。所以关键字一变就得重新灌一份 items。
        //
        // ⚠️ 订阅**必须存进字段**，不能 `let _ = ...` —— drop 即退订，
        // 表现是"打字没反应"，而不会报错（`container_search_sub` 那个坑）。
        let sub = cx.subscribe(&input, |shell, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let query = input.read(cx).value().to_string();
                shell.rebuild_schema_tree(&query, cx);
                cx.notify();
            }
        });
        self.forms.schema_search = Some(input);
        self.subs.schema_search_sub = Some(sub);
    }

    /// 按当前数据 + 关键字**重建** `TreeState` 的 items。
    ///
    /// # 什么时候会重建
    ///
    /// - 读到新的表结构之后；
    /// - 搜索关键字变了；
    /// - 第一次进这一页。
    ///
    /// # 为什么关键字不变就跳过
    ///
    /// `TreeState` 重建会**丢掉用户已经展开的层级**（展开状态住在组件里）。
    /// 所以只有 `(关键字, 表总数)` 真的变了才重灌 —— 这个指纹在
    /// [`Shell::schema_tree_built_for`] 里。
    ///
    /// # 两种形态
    ///
    /// - **没搜索**：库是父节点、表是子节点（默认收起，点库名展开）；
    /// - **有搜索**：**拍平**成一层「库.表」。理由：树的父节点默认是收起的，
    ///   搜索时如果还挂着父节点，用户会看到一个收起的库名、得再点一下才见到
    ///   他刚搜的那张表 —— 那就不叫搜索了。拍平之后命中的表直接在眼前。
    pub(crate) fn rebuild_schema_tree(&mut self, query: &str, cx: &mut Context<Self>) {
        use gpui_kit::component::tree::TreeItem;

        let Some(db) = self.state.db.as_ref() else {
            return;
        };
        let total = db.schema_tree.table_count();
        let fingerprint = (query.trim().to_owned(), total);
        // 指纹没变就别动 —— 见函数文档（重建会丢展开状态）。
        if self.sql_ui.schema_tree_built_for.as_ref() == Some(&fingerprint)
            && self.sql_ui.schema_tree_state.is_some()
        {
            return;
        }

        let filtered = db.schema_tree.filter(query);
        let searching = !query.trim().is_empty();
        let mut items: Vec<TreeItem> = Vec::new();

        for node in &filtered.databases {
            if searching {
                // 拍平：每条都是叶子，id = `t\u{1}库\u{1}表`。
                for t in &node.tables {
                    items.push(TreeItem::new(
                        leaf_id(&node.name, &t.name),
                        format!("{}.{}", node.name, t.name),
                    ));
                }
            } else {
                let children: Vec<TreeItem> = node
                    .tables
                    .iter()
                    .map(|t| {
                        let label = match t.kind.tag() {
                            "" => t.name.clone(),
                            tag => format!("{}  {tag}", t.name),
                        };
                        TreeItem::new(leaf_id(&node.name, &t.name), label)
                    })
                    .collect();
                let label = if node.is_system() {
                    format!("{}（系统）", node.name)
                } else {
                    node.name.clone()
                };
                items.push(TreeItem::new(folder_id(&node.name), label).children(children));
            }
        }

        let state = cx.new(|cx| gpui_kit::component::tree::TreeState::new(cx));
        state.update(cx, |s, cx| s.set_items(items, cx));
        self.sql_ui.schema_tree_state = Some(state);
        self.sql_ui.schema_tree_built_for = Some(fingerprint);

        // 表结构变了 → 给补全换一份新快照。
        //
        // ⚠️ 漏了这一步的表现很具体：补全永远只有关键字、补不出表名和列名 ——
        // 因为第一次装提供者时树还是空的（`ensure_db_form` 比读表结构早）。
        self.install_sql_completion(cx);
    }

    /// 点树里的一行。
    ///
    /// `id` 的格式见 [`leaf_id`] / [`folder_id`]：`t\u{1}库\u{1}表` 是表，
    /// `d\u{1}库` 是库（点库由组件自己展开/收起，这里什么都不做）。
    pub fn on_schema_tree_click(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let mut parts = id.split(TREE_ID_SEP);
        let Some(kind) = parts.next() else {
            return;
        };
        if kind != "t" {
            // 库节点：展开 / 收起由 `Tree` 自己做，不用我们管。
            return;
        }
        let (Some(database), Some(table)) = (parts.next(), parts.next()) else {
            return;
        };
        self.pick_schema_table(database.to_owned(), table.to_owned(), window, cx);
    }

    /// 读整棵库 / 表树（进控制台时调一次）。
    ///
    /// 失败时**不清空**已有的树：用户可能已经有树了，一次读失败不该让它消失
    /// （比如网络抖了一下）。只在**从未读到过**时才发提示。
    pub fn load_schema_tree(&mut self, cx: &mut Context<Self>) {
        let Some(db) = self.state.db.as_ref() else {
            return;
        };
        let container = db.container.clone();
        let bin = db.client.clone();
        // 树是全库可见的（`information_schema`），但账号得有权限 ——
        // 按行的兜底规则取（有具体库时用那个库的账号，否则用连接卡）。
        let creds = match db.sql_database.as_deref() {
            Some(name) => self.creds_for_row(&container, name, cx),
            None => self.db_creds(cx),
        };
        let wslc = self.state.wslc.clone();
        // ⚠️ `container` 在后面 `update` 里还要用（判断"是不是还停在同一个容器"），
        // 而内层的 `async move` 会把它搬走 —— 所以备一份 clone。
        // 这个坑在 `delete_db_backup_confirmed` 里踩过一次（CI 的 E0382）。
        let exec_container = container.clone();
        // v0.14：PostgreSQL 走驱动。它的元数据查询**按库隔离**，所以要带上
        // 当前选中的库；MySQL 那条 `information_schema` 是全库可见的。
        let mut conn = db.conn.clone();
        conn.database = db.sql_database.clone();
        let driver = db.driver();
        let cred = state::to_credentials(&creds);
        // 连接串在状态里是 DsnText（Debug 手写脱敏，见那个类型）。sync move
        // 闭包里要的是 &str，所以在克隆点就取成原文 —— DsnText::as_str 引用
        // 的是 db，跨不过 move。
        let dsn = db.dsn.as_ref().map(|d| d.as_str().to_owned());
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    // 有连接串 → 原生 TCP；没有 → exec（容器里跑 psql）。
                    // 两条分支都要产出 `Result<SchemaTree, String>`，
                    // 所以各自的错误都在分支里转成 String（外层的 map_err 去掉了）。
                    if matches!(conn.kind, DbKind::Postgres) {
                        match dsn.as_deref() {
                            Some(text) => crate::native_db::parse(text)
                                .and_then(|spec| crate::native_db::schema(&spec))
                                .map_err(|e| e.to_string()),
                            None => driver
                                .schema(&wslc, &conn, &cred)
                                .map_err(|e| e.to_string()),
                        }
                    } else if let Some(text) = dsn.as_deref() {
                        // 有连接串 → 原生（能读远程库的对象树）。
                        crate::native_db::parse(text)
                            .and_then(|spec| crate::native_db::schema(&spec))
                            .map_err(|e| e.to_string())
                    } else {
                        wslc_core::cmd::mysql::list_schema_tree(
                            &wslc,
                            &exec_container,
                            &bin,
                            &creds,
                        )
                        .map_err(|e| e.to_string())
                    }
                })
                .await;
            let _ = this.update(cx, |shell, cx| {
                let mut ok = false;
                if let Some(db) = shell.state.db.as_mut() {
                    if db.container != container {
                        return;
                    }
                    match result {
                        Ok(tree) => {
                            db.schema_tree = tree;
                            ok = true;
                        }
                        Err(message) => {
                            // 之前读到过就不打扰（见函数文档）
                            if db.schema_tree.is_empty() {
                                shell
                                    .state
                                    .notify(Toast::info(format!("读不到表结构：{message}")));
                            }
                        }
                    }
                }
                if ok {
                    // 换了数据就得重灌 items。指纹先清掉，否则"表总数没变、
                    // 但表名变了"（改表名场景）会被误判成不变。
                    shell.sql_ui.schema_tree_built_for = None;
                    let query = shell
                        .forms
                        .schema_search
                        .as_ref()
                        .map(|i| i.read(cx).value().to_string())
                        .unwrap_or_default();
                    shell.rebuild_schema_tree(&query, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 点树里的表名 → 把 `SELECT * FROM <表> LIMIT 200` 填进编辑区。
    ///
    /// # 为什么是"填进去"而不是"直接执行"
    ///
    /// 用户点表是想看数据，但**先看一眼 SQL** 是更安全的默认：他可能想加
    /// `WHERE`，也可能只是点错了表。填进去之后按一下「执行」就行，
    /// 而"点了就立刻跑一个全表扫描"是不可撤销的（尤其是生产库）。
    ///
    /// # 引号风格跟着**当前引擎**走
    ///
    /// 原先这里调的是 `model::schematree::select_from_table_sql` —— 那是 MySQL 的
    /// 反引号版本。PostgreSQL 的标识符引用是双引号，反引号到那边是**语法错误**，
    /// 于是"点表"在 PostgreSQL 库上等于点了没反应（填进去的 SQL 跑不了）。
    ///
    /// 拼法现在只有一份（`DialectData::select_from_table_sql`），这里按
    /// `DbState::conn` 的引擎取方言 —— 别在这里再写 `if matches!(kind, …)`。
    pub fn pick_schema_table(
        &mut self,
        database: String,
        table: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(db) = self.state.db.as_ref() else {
            return;
        };
        // 先取方言再拼：`db` 是**不可变**借用，拼完就得放开，
        // 下面那句 `as_mut()` 要不回来（借用检查器看最后一次使用）。
        let dialect = wslc_core::db::dialect_for(db.conn.kind);
        let sql = dialect.select_from_table_sql(&database, &table, wslc_core::db::DEFAULT_LIMIT);
        // 顺便把「当前库」设成这个表所在的库 —— 否则 `SELECT * FROM 库.表`
        // 虽然能跑，但下面再手写 `SELECT * FROM 别的表` 时会报
        // "No database selected"（实测过）。
        if let Some(db) = self.state.db.as_mut() {
            db.sql_database = Some(database);
        }
        if let Some(form) = self.forms.db_form.as_ref() {
            form.sql
                .update(cx, |state, cx| state.set_value(sql, window, cx));
        }
        cx.notify();
    }
}
