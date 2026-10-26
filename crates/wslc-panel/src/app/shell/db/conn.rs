//! 连接：表单（懒得建/重填）、凭据来源（连接卡 / 面板记的账号 / 连接串）。

use super::super::super::prelude::*;
use super::super::Shell;
use super::super::state::DbPageForm;

impl Shell {
    /// 建数据库页的三个输入框（幂等）。
    ///
    /// 幂等很要紧：`open_database_page` 每次进来都会调它，而重建输入框会让
    /// 用户刚打的字和刚填的密码全丢。
    pub(crate) fn ensure_db_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.forms.db_form.is_some() {
            return;
        }
        let user = cx.new(|cx| InputState::new(window, cx).placeholder("root"));
        let password = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("root 的密码")
                .masked(true)
        });
        // 连接串（可选）：粘一条 `postgres://…` 就走原生连接。
        let dsn = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("postgres://user:pass@host:5432/db（留空＝在容器里执行）")
        });
        // SQL 编辑器（v0.12）：多行 + 行号 + SQL 语法高亮。
        //
        // 每一项都是有理由的，不是"顺手都打开"：
        // - `language("sql")`：靠 `tree-sitter-sql` 认关键字 / 字符串 / 注释，
        //   这是用户要的那个"像 dbx 一样的编辑器"的核心；
        // - `line_number(true)`：SQL 报错只说 "line 3"，没有行号就对不上；
        // - `indent_guides(true)`：多行语句缩进容易乱，参考线能看出层级；
        // - `soft_wrap(false)`：SQL 按行组织，自动折行会把 `;` 断到看不见的地方；
        // - `tab_size(2, soft)`：插空格不插制表符 —— 粘进别的工具时对齐不会散。
        let sql = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("sql")
                .line_number(true)
                .indent_guides(true)
                .soft_wrap(false)
                .tab_size(TabSize {
                    tab_size: 2,
                    hard_tabs: false,
                })
                .placeholder("SHOW DATABASES;")
        });

        // 用户名预填 root：绝大多数人只填密码那一格。
        user.update(cx, |state, cx| {
            state.set_value("root".to_owned(), window, cx)
        });
        // 焦点给密码（最可能要填的就是它）。
        let handle = password.read(cx).focus_handle(cx);
        window.focus(&handle, cx);

        self.forms.db_form = Some(DbPageForm {
            user,
            password,
            dsn,
            sql,
        });
        // 编辑器刚建好，把自动补全装上（表结构可能还没读到 ——
        // 那时只有关键字可补，读到之后 `rebuild_schema_tree` 会再装一次）。
        self.install_sql_completion(cx);
    }

    /// 应用连接卡里的「连接串」（v0.16）。
    ///
    /// 填了 → 这一页之后的操作走**原生连接**（`wslc-db`），能连远程库；
    /// 留空 → 回到"在容器里 exec 客户端"那条路。
    ///
    /// # 引擎必须和这一页对得上
    ///
    /// 把一个 PostgreSQL 连接串填进 MySQL 页，结果是"查询走 PG、库列表和
    /// 表结构却按 MySQL 的元数据去查" —— 那种半对半错的状态比直接拒绝难查得多。
    /// 所以引擎不匹配就明确报错，让用户去对应的页面。
    pub(crate) fn apply_db_dsn(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = self.forms.db_form.as_ref() else {
            return;
        };
        let text = form.dsn.read(cx).value().to_string().trim().to_owned();

        // 先算出结果，再动状态：这样 `db` 的可变借用不会跨越 `notify`
        // （借用没结束就调 `&mut self.state` 的方法是编译错误）。

        // 这一页的引擎（用来拦"把 PG 连接串填进 MySQL 页"这种事）。
        let page_kind = self.state.db.as_ref().map(|d| d.conn.kind);
        let outcome: std::result::Result<Option<String>, String> = if text.is_empty() {
            Ok(None)
        } else {
            match crate::native_db::parse(&text) {
                Ok(spec) => match page_kind {
                    // MySQL 与 MariaDB 协议兼容，算同一族。
                    Some(kind)
                        if !(kind == spec.kind
                            || (matches!(kind, DbKind::MySql | DbKind::MariaDb)
                                && matches!(spec.kind, DbKind::MySql | DbKind::MariaDb))) =>
                    {
                        Err(format!(
                            "这条连接串是 {}，但这一页是 {} —— 请到对应的数据库页里用",
                            spec.kind.label(),
                            kind.label(),
                        ))
                    }
                    _ => Ok(Some(text)),
                },
                Err(error) => Err(error.to_string()),
            }
        };

        match outcome {
            Ok(dsn) => {
                let switched = dsn.is_some();
                if let Some(db) = self.state.db.as_mut() {
                    // `DsnText` 是 newtype（`Debug` 手写脱敏）—— 状态里存的是它，
                    // 不是裸 `String`（见那个类型的文档：口令不能进日志）。
                    db.dsn = dsn.map(wslc_panel_core::state::DsnText::new);
                    // 树是从**上一个**连接读的，换了连接就必须重读。
                    db.schema_tree.databases.clear();
                }
                self.state.notify(Toast::info(if switched {
                    "已切到原生连接，正在重读…".to_owned()
                } else {
                    "已改回「在容器里执行」".to_owned()
                }));
                self.load_db(window, cx);
            }
            Err(message) => {
                self.state
                    .notify(Toast::error(format!("连接串用不了：{message}")));
            }
        }
    }

    /// 给 SQL 编辑器装上 **自动补全**（v0.12.4）。
    ///
    /// # 为什么要"装"而不是渲染时读
    ///
    /// `EditorState` 的补全提供者是它自己持有的一个 `Rc<dyn CompletionProvider>`
    /// （见 `gpui-kit` 的 `examples/editor`）。它拿不到 `Shell`，所以必须在
    /// 每次**元数据变化**之后，把一份新快照塞进去。
    ///
    /// # 什么时候调
    ///
    /// - 编辑器刚建好（`ensure_db_form`）；
    /// - 表结构读到 / 重读之后（`rebuild_schema_tree`）——
    ///   否则补全里永远是"只有关键字"，因为第一次装的时候树是空的。
    ///
    /// 成本：把 `SchemaTree` 克隆一份（110 表 + 800 列，几十 KB），
    /// 只在读表结构/进页面时发生，不在按键路径上。
    pub(crate) fn install_sql_completion(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.forms.db_form.as_ref() else {
            return;
        };
        let editor = form.sql.clone();
        let Some(db) = self.state.db.as_ref() else {
            return;
        };
        // 方言：MariaDB 有一批 MySQL 没有的关键字（`RETURNING` / `NEXT VALUE FOR`）。
        // Percona 走 MySQL —— 它是 MySQL 的兼容发行版。
        let dialect = match db.family {
            wslc_core::model::MysqlFamily::Mariadb => {
                wslc_panel_core::sqlcomplete::Dialect::MariaDb
            }
            _ => wslc_panel_core::sqlcomplete::Dialect::MySql,
        };
        let tree = Arc::new(db.schema_tree.clone());
        editor.update(cx, |state, _cx| {
            state.lsp_mut().completion_provider = Some(
                crate::sql_complete::SqlCompletionProvider::new(tree, dialect),
            );
        });
    }

    /// 读连接卡上**此刻**的值。
    ///
    /// 「执行时用连接卡上的值」是刻意的：那份值就是用户眼睛看到的那个，
    /// 不存在"界面显示 A、实际用 B"的可能（`DbState::creds` 只是它的镜像）。
    pub(crate) fn read_db_form(&self, cx: &App) -> (String, String) {
        let Some(form) = self.forms.db_form.as_ref() else {
            return ("root".to_owned(), String::new());
        };
        let user = form.user.read(cx).value().trim().to_owned();
        let user = if user.is_empty() {
            "root".to_owned()
        } else {
            user
        };
        // ⚠️ `value()` 返回的是 `SharedString`（gpui 的引用计数字符串），
        // 要自己转成 `String` —— `.trim()` 之后才是 `&str`，那种写法不用转。
        (user, form.password.read(cx).value().trim().to_owned())
    }

    /// 这一次动作要用的账号 —— **唯一入口**。
    ///
    /// # 为什么不能从 `DbState::creds` 取
    ///
    /// `DbState::creds` 全仓库只在 [`Shell::load_db`] 里赋值，而数据库页的
    /// 入口里有一半会**先把行上的账号写进连接卡**
    /// （[`Shell::open_db_backup_dialog`] / [`Shell::open_db_console_for`]）。
    /// 于是同一页上会出现两套账号，而且**看不出来**：
    ///
    /// | 动作 | 原来取的 |
    /// |---|---|
    /// | 备份 / 导入 / SQL 控制台 / 建库建用户 | 连接卡（用户看到的那个） |
    /// | 恢复备份 / 删库 / 删用户 | `DbState::creds`（进页面那一刻那个） |
    ///
    /// 复现路径是日常操作：进数据库页（`creds` = root）→ 点某一行的「备份」
    /// （连接卡被换成该库的 `shop_user`）→ 在浮层里点「恢复」→ 恢复用的是
    /// **root**，MySQL 回一句 `Access denied`，而浮层上明明写着
    /// 「备份用账号 shop_user」。
    ///
    /// 所以执行路径只认连接卡；`DbState::creds` 退回成"上一次连成功的那份"
    /// （只在诊断 / 显示里用）。
    ///
    /// `pub(crate)`：`views/` 渲染时构造 `PendingAction`（删库 / 删用户）
    /// 也要用它 —— 那两个按钮的账号必须和别处是同一个来源。
    pub(crate) fn db_creds(&self, cx: &App) -> MysqlCreds {
        let (user, password) = self.read_db_form(cx);
        MysqlCreds {
            user,
            password,
            host: None,
            port: None,
        }
    }

    /// **数据库列表那一行**该用哪个账号 —— 行内四个按钮 + 恢复共用这一个。
    ///
    /// # 规则（只有一条，按顺序取第一个有的）
    ///
    /// 1. 面板给 `(容器, 库)` 记过账号（`db-credentials.json`，建库时同时建的
    ///    那个用户）→ 用它。那是这个库**自己的**账号，备份 / 导入 / 恢复 / SQL
    ///    本来就该以它的身份做 —— 实测 `GRANT ALL ON <库>.*` 连 `DROP` 都给了，
    ///    所以连「删除这个库」它也干得了；
    /// 2. 没记过（不是面板建的库，或者存档被清过）→ 退回连接卡上那个
    ///    （用户看得见、能改的那份）。
    ///
    /// # 为什么不能用连接卡"看情况"
    ///
    /// 连接卡是**一个**值，而列表里有**多行**。用户在 `djxm` 那行点过「备份」
    /// 之后卡被换成 `djxm_user`，此时再去点 `wslc_probe` 那行的「删除」——
    /// 用卡就会拿 `djxm_user` 去删 `wslc_probe`，报一句别人看不懂的
    /// Access denied。所以**行内动作一律按行走**，卡只在"行上没有记录"时兜底。
    ///
    /// ⚠️ 唯一的例外是「用户」tab 的「删除」：删一个**账号**需要
    /// `DROP USER` 权限，而面板给某个库记的那个账号只有那个库的权限
    /// （`GRANT ALL ON <库>.*` 不含 `DROP USER`），所以那里仍然用连接卡上
    /// 的账号 —— 那一行的"对象"是账号本身，不是某个库。
    pub(crate) fn creds_for_row(&self, container: &str, database: &str, cx: &App) -> MysqlCreds {
        if let Some(cred) = self.state.db_credentials.get(container, database) {
            return MysqlCreds {
                user: cred.user.clone(),
                password: cred.password.clone(),
                host: None,
                port: None,
            };
        }
        self.db_creds(cx)
    }
}
