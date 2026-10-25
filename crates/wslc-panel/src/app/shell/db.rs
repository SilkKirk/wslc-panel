//! 数据库域：连接、库 / 用户、SQL 控制台、备份与恢复。

use super::super::prelude::*;
use super::Shell;
use super::core::{PreparedDbSql, effective_db_creds, folder_id, leaf_id};
use super::state::{
    AfterPick, DbDialog, DbDialogKind, DbPageForm, DbStatement, DropdownItem, SaveDecision,
};

impl Shell {
    /// 建数据库页的三个输入框（幂等）。
    ///
    /// 幂等很要紧：`open_database_page` 每次进来都会调它，而重建输入框会让
    /// 用户刚打的字和刚填的密码全丢。
    pub(crate) fn ensure_db_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.db_form.is_some() {
            return;
        }
        let user = cx.new(|cx| InputState::new(window, cx).placeholder("root"));
        let password = cx.new(|cx| InputState::new(window, cx).placeholder("root 的密码"));
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

        self.db_form = Some(DbPageForm {
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
        let Some(form) = self.db_form.as_ref() else {
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
                Err(message) => Err(message),
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
        let Some(form) = self.db_form.as_ref() else {
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
        let Some(form) = self.db_form.as_ref() else {
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
    /// `pub(crate)`：`views.rs` 渲染时构造 `PendingAction`（删库 / 删用户）
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

    /// 打开某个容器的数据库管理页（从容器列表行的「数据库」按钮进）。
    ///
    /// `image` 只用于显示和"用镜像名兜底判家族"；**权威判定**在
    /// [`Shell::load_db`] 里用 `inspect` 做。
    pub fn open_database_page(
        &mut self,
        name: String,
        image: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_db_form(window, cx);

        // 同一个容器就**不重建状态**：切走再回来时不该把连接信息清掉。
        let same_container = self
            .state
            .db
            .as_ref()
            .is_some_and(|db| db.container == name);

        if !same_container {
            let family = mysql_model::family_from_image(&image).unwrap_or(MysqlFamily::Mysql);
            // v0.14：先按镜像名猜**引擎**。猜出 PostgreSQL 就走多驱动那条路，
            // 猜不出或猜成 MySQL 家族时**仍用原来的 `provisional`** ——
            // 它会保住 Percona 这类细分家族，而 `provisional_kind` 只知道
            // "MySql / MariaDb / Postgres" 三档，会把 Percona 显示成 MySQL。
            let kind = wslc_core::db::guess_kind(&wslc_core::db::ConnHint::new(&image))
                .map(|(k, _)| k)
                .unwrap_or_else(|| DbKind::from_family(family.label()));
            self.state.db = Some(if matches!(kind, DbKind::Postgres) {
                DbState::provisional_kind(name, image, kind)
            } else {
                DbState::provisional(name, image, family)
            });
            // 换了容器就要清密码：上一个容器的密码留在这儿会**连错库**
            // （两个容器恰好都有个 root 账号时，这个错误会非常难发现）。
            // 用户名也要跟着换成该引擎的默认值：PostgreSQL 没有 root 账号，
            // 留着 `root` 会让第一次连接直接报 role "root" does not exist。
            //
            // ⚠️ **连接串也要清**（v0.17 补）：它比密码更危险 ——
            // 一条 `mysql://root:pw@10.0.0.5:3306/shop` 里既有口令又有**主机**，
            // 留着它，切过去的那个容器页会静默地去连**上一台机器**，
            // 而页面上还写着"这一页是 <新容器>"。
            // 同一个理由（clear on switch）在这一段已经写过一次，这里漏了一处。
            if let Some(form) = self.db_form.as_ref() {
                form.password
                    .update(cx, |state, cx| state.set_value(String::new(), window, cx));
                // 状态里的 `dsn` 已经随新 `DbState` 一起变成 `None` 了（上面刚重建），
                // 所以这里只需清**输入框**，两者才不会再自相矛盾。
                form.dsn
                    .update(cx, |state, cx| state.set_value(String::new(), window, cx));
                let default_user = kind.default_user().to_owned();
                form.user
                    .update(cx, |state, cx| state.set_value(default_user, window, cx));
            }
            self.goto_page(Page::Database, cx);
            self.load_db(window, cx);
            return;
        }

        self.goto_page(Page::Database, cx);
        // 同一个容器：**不重建状态**（连接卡上填的账号要留着），
        // 但**没读到过**就再读一次 —— 否则上一轮读失败（密码错、容器刚起来
        // 还没就绪）之后，切走再回来只会看到同一张空页面：
        // 「重新读取」按钮在，但用户没理由知道要去点它。
        // 正在读的时候 `load_db` 自己会挡住（它第一句就是那条守卫）。
        if !self.state.db.as_ref().is_some_and(|db| db.loaded) {
            self.load_db(window, cx);
            return;
        }
        cx.notify();
    }

    /// 探测容器 + 读两张表（进页面、点「重新读取」都走这里）。
    ///
    /// 「重新读取」是**唯一**的刷新入口：连接卡上刚改的账号要靠它生效
    /// （不然用户改完密码还得猜"什么时候才算生效"）。
    pub fn load_db(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(db) = self.state.db.as_ref() else {
            return;
        };
        // 正在跑就不叠第二次（按钮那边也已经换成「读取中…」）。
        if db.busy {
            return;
        }
        // v0.14：PostgreSQL 走**独立**路径。
        //
        // 注：`kind` 是 `Copy`，取出来之后对 `self.state.db` 的借用就结束了，
        // 下面才能调 `&mut self` 的方法。
        let kind = db.conn.kind;
        if matches!(kind, DbKind::Postgres) {
            self.load_db_postgres(window, cx);
            return;
        }
        let container = db.container.clone();
        // v0.16：填了连接串就走原生连接（能连远程库）。
        // 连接串在状态里是 DsnText（Debug 手写脱敏，见那个类型）。sync move
        // 闭包里要的是 &str，所以在克隆点就取成原文 —— DsnText::as_str 引用
        // 的是 db，跨不过 move。
        let dsn = db.dsn.as_ref().map(|d| d.as_str().to_owned());
        // 用户此刻填在连接卡上的值。密码为空 = "还没填" → 用探测到的那个
        // （容器环境变量里的 `MYSQL_ROOT_PASSWORD` 就是给这种时候用的）。
        let (typed_user, typed_password) = self.read_db_form(cx);
        // 备份目录**先算出来**（纯字符串拼接），扫描本身放到后台执行器上 ——
        // `read_dir` + 每条一次 `metadata()`，在机械盘 / 网络盘上会明显卡一下，
        // 而下面那个 `update_in` 回调跑在**界面线程**上。
        let backup_dir = self.state.container_backup_dir(&container);

        if let Some(db) = self.state.db.as_mut() {
            db.busy = true;
            db.blocked = None;
            db.clear_error();
        }
        cx.notify();

        let wslc = self.state.wslc.clone();
        cx.spawn_in(window, async move |this, cx| {
            let target = container.clone();
            let wslc_probe = wslc.clone();
            let probe_dsn = dsn.clone();
            let probe =
                cx.background_executor()
                    .spawn(async move {
                        match probe_dsn.as_deref() {
                            // 有连接串 → 原生探测（不需要容器里的 mysql 客户端）。
                            Some(text) => crate::native_db::mysql_probe(text, &target)
                                .map_err(|e| e.to_string()),
                            // 没有 → 原来的 exec 那条路。
                            None => wslc_core::cmd::mysql::detect(&wslc_probe, &target)
                                .map_err(|e| e.to_string()),
                        }
                    })
                    .await;

            let probe = match probe {
                Ok(probe) => probe,
                Err(message) => {
                    // 探测就失败：页面只显示这一条（那些空表没有意义）
                    let _ = this.update_in(cx, |shell, _window, cx| {
                        let failure = DbFailure::from_text(message);
                        let summary = failure.summary();
                        if let Some(db) = shell.state.db.as_mut() {
                            if db.container == container {
                                db.busy = false;
                                db.loaded = false;
                                db.blocked = Some(failure);
                            }
                        }
                        // ⚠️ 提示条要 `&mut shell.state`，而上面那个 `db` 借用
                        // 必须**先结束**才能调它 —— 所以 summary 是 owned 的。
                        shell
                            .state
                            .notify(Toast::error(format!("读取失败：{summary}")));
                        cx.notify();
                    });
                    return;
                }
            };

            // 用哪个密码：**用户填了就用他的**，没填才用环境变量里那个
            let (user, password) = effective_db_creds(&probe, &typed_user, &typed_password);
            let creds = MysqlCreds {
                user: user.clone(),
                password: password.clone(),
                host: None,
                port: None,
            };
            let bin = probe.client.bin.clone();
            let lists = {
                let target = container.clone();
                let bin = bin.clone();
                let creds = creds.clone();
                let backup_dir = backup_dir.clone();
                let lists_dsn = dsn.clone();
                cx.background_executor()
                    .spawn(async move {
                        let (databases, users) = match lists_dsn.as_deref() {
                            // 原生：两条独立查询（用户列表读不到 `mysql.user`
                            // 是常态 —— 那时库列表不能跟着一起失败）。
                            Some(text) => (
                                crate::native_db::mysql_databases(text),
                                crate::native_db::mysql_users(text),
                            ),
                            None => (
                                wslc_core::cmd::mysql::list_databases(&wslc, &target, &bin, &creds)
                                    .map_err(|e| e.to_string()),
                                wslc_core::cmd::mysql::list_users(&wslc, &target, &bin, &creds)
                                    .map_err(|e| e.to_string()),
                            ),
                        };
                        // 顺手重扫备份目录（进页面时目录可能被手动动过）。
                        // 放在这个块里是**故意的**：扫描碰文件系统，而
                        // `update_in` 的回调在界面线程上 —— 见
                        // `AppState::rescan_backups` 的文档。
                        let backups = backup_dir
                            .as_deref()
                            .and_then(|dir| state::scan_backup_dir(dir).ok());
                        (databases, users, backups)
                    })
                    .await
            };

            let _ = this.update_in(cx, |shell, window, cx| {
                // 把这次实际用的账号同步到连接卡上：用户看得见"用的是哪个密码"
                // （从环境变量读出来的那个也要显示出来，否则他会以为没生效）
                if let Some(form) = shell.db_form.as_ref() {
                    form.user
                        .update(cx, |state, cx| state.set_value(user.clone(), window, cx));
                    form.password.update(cx, |state, cx| {
                        state.set_value(password.clone(), window, cx)
                    });
                }

                let databases_ok = lists.0.is_ok();
                // ⚠️ 这一整块借着 `shell.state.db`，而下面的提示条要
                // `&mut shell.state` —— 借用没结束就调 `notify` 是编译错误，
                // 所以先把要说的那句话算成 owned 的，出了块再发。
                // 备份列表的重扫同理：等 `db` 借用结束、出了块再做。
                //
                // ⚠️ 不要写成 `let mut rescan = false;` 然后在块里置 true ——
                // 那个初值永远不会被读到（块里必定先赋值），编译器会报
                // `unused_assignments`。下面直接让块**返回**它。
                let (failure_summary, rescan): (Option<String>, bool) = {
                    let Some(db) = shell.state.db.as_mut() else {
                        cx.notify();
                        return;
                    };
                    if db.container != container {
                        // 已经切到别的容器了，这份结果丢掉
                        return;
                    }
                    db.apply_probe(&probe);
                    db.creds = creds.clone();
                    db.busy = false;
                    db.loaded = true;
                    match lists.0 {
                        Ok(databases) => db.databases = databases,
                        Err(message) => db.fail(message),
                    }
                    match lists.1 {
                        Ok(users) => {
                            db.users = users;
                            // 这次读到了就清掉上一次的失败（换账号重读之后
                            // 那句"权限不够"不该继续挂着）。
                            db.users_error = None;
                        }
                        // 数据库列出来了、用户列表没列出来（多半是权限 ERROR 1142）：
                        // 存进 `users_error`，**只画在用户 tab 里** —— 整页红色横幅会让
                        // 人以为整个页面坏了，而「数据库」tab 明明能用（v0.16.0 实机反馈）。
                        Err(message) => {
                            if databases_ok {
                                db.fail_users(message);
                            }
                        }
                    }
                    // 块返回 `(提示条文案, 要不要重扫备份列表)` ——
                    // `rescan` 恒为 true（走到这里说明 probe 成功、列表已读），
                    // 但保持返回它的形状，将来若某个分支不该重扫就改一处。
                    (db.error.as_ref().map(DbFailure::summary), true)
                };
                // 备份列表：把后台算好的那份填进去（**不**在这里扫盘）。
                if rescan {
                    if let Some(entries) = lists.2 {
                        shell.state.apply_backups(&container, entries);
                    }
                }
                if let Some(summary) = failure_summary {
                    shell
                        .state
                        .notify(Toast::error(format!("读取失败：{summary}")));
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// PostgreSQL 的「读页面」路径（v0.14）。
    ///
    /// # 为什么是独立函数而不是 `load_db` 里的分支
    ///
    /// `load_db` 那条路有三件 MySQL 专有的事：`inspect` 探测（拿镜像 / 环境变量）、
    /// 从环境变量猜 root 口令（`effective_db_creds`）、客户端二进制探测。
    /// PostgreSQL 一件都不需要（官方镜像只有 `psql`，口令靠用户填）。
    ///
    /// 硬塞进同一段代码会把两边都搅浑，而且出问题时**分不清是谁的锅**。
    /// 分开写还有个好处：这条路一行都不碰 MySQL 的代码，随时能单独回退。
    ///
    /// # 少了什么（和 MySQL 那条路比）
    ///
    /// - 不读用户列表（PostgreSQL 的用户在 `pg_roles`，界面上还没做）
    /// - 不重扫备份目录（`mysqldump` 的备份对 PostgreSQL 不适用）
    ///
    /// 界面上那两块会按 [`DbState::is_mysql_family`] 隐藏。
    pub(crate) fn load_db_postgres(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(db) = self.state.db.as_ref() else {
            return;
        };
        let container = db.container.clone();
        let mut conn = db.conn.clone();
        // 当前库：PostgreSQL 的元数据是**按库隔离**的，必须带库名去查
        // （不指定时 `information_schema` 只返回连接默认库的内容）。
        conn.database = db.sql_database.clone();
        let driver = db.driver();
        let cred = state::to_credentials(&self.db_creds(cx));
        // v0.16：填了连接串就走原生连接（能连远程库）。
        // 连接串在状态里是 DsnText（Debug 手写脱敏，见那个类型）。sync move
        // 闭包里要的是 &str，所以在克隆点就取成原文 —— DsnText::as_str 引用
        // 的是 db，跨不过 move。
        let dsn = db.dsn.as_ref().map(|d| d.as_str().to_owned());

        if let Some(db) = self.state.db.as_mut() {
            db.busy = true;
            db.blocked = None;
            db.clear_error();
        }
        cx.notify();

        let wslc = self.state.wslc.clone();
        cx.spawn_in(window, async move |this, cx| {
            // 探测 + 列库，两件事一起在后台做完（各一次 `wslc exec`）。
            let probed = {
                let wslc = wslc.clone();
                let conn = conn.clone();
                let cred = cred.clone();
                let driver = driver.clone();
                let container = container.clone();
                let dsn = dsn.clone();
                cx.background_executor()
                    .spawn(async move {
                        // 有连接串 → 原生 TCP（能连远程库，容器里有没有 psql 都不影响）；
                        // 没有 → exec 那条路（在容器里跑 psql）。
                        if let Some(text) = dsn.as_deref() {
                            let spec = crate::native_db::parse(text)?;
                            let version = crate::native_db::server_version(&spec)?;
                            let names = crate::native_db::databases(&spec)?;
                            return Ok::<_, String>((
                                wslc_core::db::ServerInfo {
                                    kind: wslc_core::db::DbKind::Postgres,
                                    version: Some(version),
                                    client: Some("native".to_owned()),
                                },
                                names,
                            ));
                        }
                        let info = driver
                            .probe(&wslc, &conn, &cred)
                            .map_err(|e| e.to_string())?;
                        let names = wslc_core::cmd::postgres::list_databases(
                            &wslc,
                            &container,
                            &cred.user,
                            &cred.password,
                        )
                        .map_err(|e| e.to_string())?;
                        Ok::<_, String>((info, names))
                    })
                    .await
            };

            let _ = this.update_in(cx, |shell, _window, cx| {
                let (failure, databases) = match probed {
                    Ok((info, names)) => {
                        let entries: Vec<wslc_core::model::DbEntry> = names
                            .into_iter()
                            .map(|name| wslc_core::model::DbEntry {
                                name,
                                // PostgreSQL 没有 MySQL 那种"库默认字符集 / 排序规则"，
                                // 也没有现成的"表数量 / 占用"聚合 —— 全留空。
                                // ⚠️ 不是忘了填：界面上这几列对 PostgreSQL 要隐藏
                                // （按 `is_mysql_family` 判断），留空是诚实。
                                charset: String::new(),
                                collation: String::new(),
                                tables: 0,
                                bytes: 0,
                            })
                            .collect();
                        (None, Some((info, entries)))
                    }
                    Err(message) => (Some(DbFailure::from_text(message)), None),
                };
                // 提示条文案先算成 owned：下面 `db` 借用期间没法调 `notify`。
                let failure_summary = failure.as_ref().map(|f| f.summary());

                if let Some(db) = shell.state.db.as_mut() {
                    if db.container != container {
                        return;
                    }
                    db.busy = false;
                    match databases {
                        Some((info, entries)) => {
                            db.client_version = info.version;
                            db.loaded = true;
                            db.databases = entries;
                            // 用户列表：PostgreSQL 这条路径不读（见函数文档）。
                            db.users = Vec::new();
                        }
                        None => {
                            db.loaded = false;
                            db.blocked = failure;
                        }
                    }
                }
                if let Some(summary) = failure_summary {
                    shell
                        .state
                        .notify(Toast::error(format!("读取失败：{summary}")));
                }
                cx.notify();
            });
        })
        .detach();
    }
    /// 只重读两张表（表结构变更之后调）。
    ///
    /// 和 [`Shell::load_db`] 分开：那个要重新跑 `inspect` + 客户端探测（两个进程），
    /// 而"刚建完一个库"只需要把列表拿回来。
    ///
    /// ⚠️ 它刻意**不收 `&mut Window`**：这一步不需要往输入框里写东西，
    /// 于是确认弹窗那条路（`confirm_pending` 的异步块里没有 window）也能直接调它。
    pub fn reload_db_lists(&mut self, cx: &mut Context<Self>) {
        let Some(db) = self.state.db.as_ref() else {
            return;
        };
        let container = db.container.clone();
        let bin = db.client.clone();
        // v0.16：填了连接串就走原生连接。
        // 连接串在状态里是 DsnText（Debug 手写脱敏，见那个类型）。sync move
        // 闭包里要的是 &str，所以在克隆点就取成原文 —— DsnText::as_str 引用
        // 的是 db，跨不过 move。
        let dsn = db.dsn.as_ref().map(|d| d.as_str().to_owned());
        // 也走连接卡：用户可能刚在「基础信息」里改过密码再建库，这时
        // 重读列表必须用**新**的那份 —— 用 `db.creds`（上一次 load_db 的）
        // 会让"建库成功、列表却报 Access denied"。
        let creds = self.db_creds(cx);
        if let Some(db) = self.state.db.as_mut() {
            db.busy = true;
        }
        cx.notify();

        let wslc = self.state.wslc.clone();
        cx.spawn(async move |this, cx| {
            let lists = {
                let target = container.clone();
                let bin = bin.clone();
                let creds = creds.clone();
                let lists_dsn = dsn.clone();
                cx.background_executor()
                    .spawn(async move {
                        match lists_dsn.as_deref() {
                            // 有连接串 → 原生（否则"建完库刷新"会去读容器，
                            // 和页面正在显示的远程库对不上）。
                            Some(text) => (
                                crate::native_db::mysql_databases(text),
                                crate::native_db::mysql_users(text),
                            ),
                            None => (
                                wslc_core::cmd::mysql::list_databases(&wslc, &target, &bin, &creds)
                                    .map_err(|e| e.to_string()),
                                wslc_core::cmd::mysql::list_users(&wslc, &target, &bin, &creds)
                                    .map_err(|e| e.to_string()),
                            ),
                        }
                    })
                    .await
            };

            let _ = this.update(cx, |shell, cx| {
                let databases_ok = lists.0.is_ok();
                if let Some(db) = shell.state.db.as_mut() {
                    if db.container != container {
                        return;
                    }
                    db.busy = false;
                    db.loaded = true;
                    match lists.0 {
                        Ok(databases) => db.databases = databases,
                        Err(message) => db.fail(message),
                    }
                    match lists.1 {
                        Ok(users) => {
                            db.users = users;
                            // 这次读到了就清掉上一次的失败（换账号重读之后
                            // 那句"权限不够"不该继续挂着）。
                            db.users_error = None;
                        }
                        // 同上：用户列表的权限失败只画在用户 tab 里，
                        // 不要升级成整页错误（「数据库」tab 是好的）。
                        Err(message) => {
                            if databases_ok {
                                db.fail_users(message);
                            }
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 切换数据库页的 tab。
    pub fn set_db_tab(&mut self, tab: DbTab, cx: &mut Context<Self>) {
        if let Some(db) = self.state.db.as_mut() {
            db.tab = tab;
        }
        cx.notify();
    }

    /// 打开一个动作弹窗（新建库 / 新建用户 / 改密码 / 授权）。
    ///
    /// 输入框只能在这里建：`InputState::new` 要 `&mut Window`，而这个函数
    /// 是从**点击回调**里调的（那里正好有 window）。
    pub fn open_db_dialog(
        &mut self,
        kind: DbDialogKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 第二道入口检查（第一道是渲染层不画那些按钮）。
        //
        // 为什么两道都要：渲染层拦的是"看不见"，而**浮层是几分钟前就能开着的**
        // —— 用户完全可以开着「新建数据库」弹窗，再回「基础信息」粘一条连接串
        // 应用，然后回来点确定。那一刻按钮早就在屏幕上了，只有这里能拦住。
        //
        // 走到这里说明某条路绕过了渲染层的判据（或者渲染没更新），
        // 所以直接说清原因，别开一个填完必然失败的弹窗。
        //
        // ⚠️ 判据**先算成独立变量**再 notify：`write_blocked_reason` 要借着
        // `self.state.db`，而 `notify` 要 `&mut self.state` —— 借用没结束就是
        // 编译错误（这个仓库里踩过几次，见 `load_db` 里同类的注释）。
        // 返回的是 `Option<&'static str>`，所以算出来之后借用就结束了。
        let blocked = self
            .state
            .db
            .as_ref()
            .and_then(|db| db.write_blocked_reason());
        if let Some(reason) = blocked {
            self.state.notify(Toast::error(reason));
            cx.notify();
            return;
        }
        self.state.set_section_open(UiSection::DbEncoding, false);
        let database = cx.new(|cx| InputState::new(window, cx).placeholder("shop"));
        let charset = cx.new(|cx| {
            InputState::new(window, cx).placeholder("留空 = 服务器默认（一般为 utf8mb4）")
        });
        let collation = cx.new(|cx| InputState::new(window, cx).placeholder("留空 = 跟着字符集"));
        let user = cx.new(|cx| InputState::new(window, cx).placeholder("app"));
        let host = cx.new(|cx| InputState::new(window, cx).placeholder("%（任意主机）"));
        let password = cx.new(|cx| InputState::new(window, cx).placeholder("密码"));
        let grant_db = cx.new(|cx| InputState::new(window, cx).placeholder("留空 = 先不授权"));
        // 候选下拉框：带搜索（库多的机器上一屏放不下），**空列表**在
        // `fill_grant_db_select` 里补上 —— 这里先给空 `Vec`。
        let grant_db_select = cx.new(|cx| {
            SelectState::new(Vec::<DropdownItem>::new(), None, window, cx).searchable(true)
        });
        let db_user = cx.new(|cx| InputState::new(window, cx).placeholder("例如 shop_user"));
        let import_path = cx.new(|cx| {
            InputState::new(window, cx).placeholder(r"点右边的「选择文件…」，或直接粘一个路径")
        });

        // 预填：把行上带过来的用户名 / 主机填好（改密码 / 授权那两种）
        if let DbDialogKind::AlterPassword { user: u, host: h }
        | DbDialogKind::Grant { user: u, host: h } = &kind
        {
            user.update(cx, |state, cx| state.set_value(u.clone(), window, cx));
            host.update(cx, |state, cx| state.set_value(h.clone(), window, cx));
        } else {
            host.update(cx, |state, cx| state.set_value("%".to_owned(), window, cx));
        }
        // 「新建数据库」默认**同时建用户**（v0.8 的主路径），密码先给一个
        // 随机值（用户可见可改）。用户名保持手填，只有点击「用建议值」才填写，
        // 后续修改库名也不覆盖已经填写的用户名。
        let mut create_user = false;
        if kind == DbDialogKind::NewDatabase {
            create_user = true;
            password.update(cx, |state, cx| {
                state.set_value(mysql_model::generate_password(), window, cx)
            });
        }
        // 焦点给"最该填的那一格"
        let first = match &kind {
            DbDialogKind::NewDatabase => &database,
            DbDialogKind::NewUser => &user,
            DbDialogKind::AlterPassword { .. } => &password,
            // 授权：给**输入框**而不是上面那个下拉框。
            // 输入框在两种情况下都可用（下拉框的候选可能是空的），而且
            // 光标落进去就能直接打字；想用列表就点一下上面那格。
            DbDialogKind::Grant { .. } => &grant_db,
            // 导入：最该填的是**文件**（目标库已经由那一行定了）
            DbDialogKind::ImportSql { .. } => &import_path,
        };
        let handle = first.read(cx).focus_handle(cx);
        window.focus(&handle, cx);

        let mut input_subs: Vec<Subscription> = [&database, &charset, &collation]
            .into_iter()
            .map(|input| {
                cx.subscribe(input, |_, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        cx.notify();
                    }
                })
            })
            .collect();
        // 从下拉框里选了一个库 → 写进输入框。
        //
        // ⚠️ 这个订阅**不是可选的**：不订阅的话"从下拉框里选了一个库"什么都不会发生
        // （输入框不变，提交时读的还是空 —— 界面上看起来像"选了但没生效"）。
        // 和「创建容器」里 `image_select` 那条订阅同一个坑：`Subscription` 一 drop
        // 就退订，所以必须存进 `DbDialog`。
        //
        // 用 `subscribe_in` 而不是 `subscribe`：回调里要把值写回 `InputState`，
        // 而它的 `set_value` 要 `&mut Window`（`AGENTS.md` §7.2）。
        input_subs.push(cx.subscribe_in(
            &grant_db_select,
            window,
            |shell, _emitter, event: &SelectEvent<Vec<DropdownItem>>, window, cx| {
                // 和本项目另外四处 `Select` 订阅同一个写法：只有"选了一项"才动作，
                // 清空（`Confirm(None)`，下拉框开了 `cleanable` 才会有）不改输入框
                // —— 那会把用户手打的内容一起抹掉，而"清空"在下拉框上的语义本来
                // 就只是"我不从这儿选了"。
                if let SelectEvent::Confirm(Some(database)) = event {
                    shell.pick_grant_database(database.clone(), window, cx);
                }
            },
        ));
        // 反方向（输入框 → 下拉框）**刻意不做**，理由是一次重入：
        //
        // `pick_grant_database` 用 `InputState::set_value` 写值，而它会**同步**
        // 发 `InputEvent::Change`；如果这里再订阅 `grant_db` 的 Change 去
        // `grant_db_select.update(…)`，那条回调就在 `SelectState` 自己的
        // `Confirm` 事件栈里跑 —— 等于在 `select.update(…)` 内部再
        // `select.update(…)`，GPUI 会直接 panic（"already being updated"）。
        //
        // 代价是：用户手改了输入框之后，下拉框的显示会停在最后选中的那个库上。
        // 这是**能看见的**不一致，所以在界面上用文案把"以哪格为准"说清楚
        // （`views::grant_db_field` 把输入框标成最终生效的那一格），
        // 而不是让两格去追一个会崩的双向同步。

        self.db_dialog = Some(DbDialog {
            _input_subs: input_subs,
            kind,
            database,
            charset,
            collation,
            user,
            host,
            password,
            grant_db,
            grant_db_select,
            grant: GrantKind::All,
            create_user,
            db_user,
            import_path,
        });

        // 候选库（下拉框）要等 `db_dialog` 建好才填得进去。
        self.fill_grant_db_select(window, cx);
        cx.notify();
    }

    /// 把当前读到的**业务库**填进「授权到哪个库」的下拉框。
    ///
    /// 候选由 `DbState::grant_database_options()` 给（纯逻辑，带单测：
    /// 排除系统库 + 按名字排序）。一候选都没有时下拉框就是空的，展开由
    /// `Select::empty(…)` 说一句"还没读到库列表"（见 `views::grant_db_field`）
    /// —— 这一格**不会**因此不可用：旁边那个输入框照样能手打。
    ///
    /// `set_items` 会把下拉框的选中项清掉，所以填完要把输入框里**已经写着**的
    /// 那个库重新选上（用户可能是"先手打了库名，再想用下拉框换一个"）。
    pub(crate) fn fill_grant_db_select(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self.db_dialog.as_ref() else {
            return;
        };
        // 只有这两个动作画那一格（`views::db_dialog_overlay` 里两处
        // `grant_db_field`）。其余三种动作填了也没人看得见。
        //
        // 匹配**引用**（`&dialog.kind`）而不是 `dialog.kind`：`DbDialogKind`
        // 不是 `Copy`（里面带着 `String`），按值匹配在共享引用后面借用不过。
        if !matches!(
            &dialog.kind,
            DbDialogKind::NewUser | DbDialogKind::Grant { .. }
        ) {
            return;
        }
        let select = dialog.grant_db_select.clone();
        let current = dialog.grant_db.read(cx).value().trim().to_owned();
        let items: Vec<DropdownItem> = self
            .state
            .db
            .as_ref()
            .map(|db| db.grant_database_options())
            .unwrap_or_default()
            .into_iter()
            .map(|name| DropdownItem::new(name.clone(), name))
            .collect();

        select.update(cx, |state, cx| {
            state.set_items(items, window, cx);
            if !current.is_empty() {
                state.set_selected_value(&current, window, cx);
            }
        });
    }

    /// 「新建用户 / 授权」弹窗里从下拉框选了一个库：写进输入框。
    ///
    /// # 为什么只往这一个方向同步
    ///
    /// 输入框是**唯一的事实来源**（`submit_db_dialog` 只读它），所以这里只需
    /// 把值写进去。反方向（输入框 → 下拉框）**刻意不做**：`set_value` 会
    /// **同步**发 `InputEvent::Change`，那条回调是在 `SelectState` 自己的
    /// `Confirm` 事件栈里跑的 —— 再 `select.update(…)` 就是 GPUI 的
    /// "already being updated" panic。
    ///
    /// 代价要认：用户手改输入框之后，下拉框可能还显示着上一次选中的库。
    /// 所以 `views::grant_db_field` 在输入框下面写着"**以这里为准**"，
    /// 让"哪一格生效"在界面上是看得出来的，而不是靠用户猜。
    pub fn pick_grant_database(
        &mut self,
        database: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(input) = self.db_dialog.as_ref().map(|d| d.grant_db.clone()) else {
            return;
        };
        input.update(cx, |state, cx| state.set_value(database, window, cx));
        cx.notify();
    }

    /// 「导入 SQL」弹窗里点「选择文件…」。
    ///
    /// 复用面板唯一那个选择器（`cmd::picker` 起独立进程弹 Windows 原生对话框，
    /// 理由见 `AGENTS.md` §7.4）：`sql` 作为扩展名过滤，`AfterPick::Nothing`
    /// —— 选完只填输入框，导入要等用户点「开始导入」。
    pub fn pick_import_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(input) = self.db_dialog.as_ref().map(|d| d.import_path.clone()) else {
            return;
        };
        self.spawn_picker(
            &input,
            false,
            "SQL 文件",
            &["sql"],
            "选择要导入的 SQL 文件",
            AfterPick::Nothing,
            window,
            cx,
        );
    }

    /// 开始导入一份 `.sql`（长任务；状态在 `AppState::db.import`）。
    ///
    /// 目标库**来自弹窗的 kind**（`ImportSql { database }` —— 就是数据库列表
    /// 上被点的那一行），不给用户改的机会：这一条动作的语义就是"导入这个库的
    /// 数据"。它同时消掉了"库名填错"这类错误。
    ///
    /// # 校验都在**起进程之前**，而且失败**不关弹窗**
    ///
    /// 用户就在那一格上（`AGENTS.md` §7.5 的"空值等于没填"同理）：
    /// 文件必须真的在（选了之后被删 / 手打的路径写错，都要当场说）。
    pub fn start_db_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 先把要用的取成 owned（下面要改 self，不能再借着 db_dialog）
        let (path_text, database) = {
            let Some(dialog) = self.db_dialog.as_ref() else {
                return;
            };
            let DbDialogKind::ImportSql { database } = &dialog.kind else {
                return;
            };
            (
                dialog.import_path.read(cx).value().trim().to_owned(),
                database.clone(),
            )
        };

        if path_text.is_empty() {
            self.state.notify(Toast::error("先选一个 .sql 文件"));
            cx.notify();
            return;
        }
        let path = std::path::PathBuf::from(&path_text);
        if !path.is_file() {
            self.state
                .notify(Toast::error(format!("找不到这个文件：{path_text}")));
            cx.notify();
            return;
        }
        // 库名要进 `mysql -D …`（过一次 shell，`load_script` 会引起来），
        // 但先把"这个名字根本不是合法标识符"的情况拦在本地。
        // 它来自我们自己的库列表，正常永远过得去 —— 这一条是防御。
        if let Err(e) = mysql_model::validate_ident("库名", &database) {
            self.state.notify(Toast::error(e.to_string()));
            cx.notify();
            return;
        }

        // 原生连接下导入走不通：它要把文件 `container cp` 进去、再用容器里的
        // `mysql` 读它（见 `cmd::mysql::load_sql_file`）。
        //
        // ⚠️ 判据先算成独立变量：`db` 借着 `self.state`，而 notify 要
        // `&mut self.state`（借用冲突）。
        let blocked = self
            .state
            .db
            .as_ref()
            .and_then(|db| db.write_blocked_reason());
        if let Some(reason) = blocked {
            self.state.notify(Toast::error(reason));
            cx.notify();
            return;
        }
        let Some(db) = self.state.db.as_ref() else {
            return;
        };
        if db.busy || db.import.is_some() || db.backup.is_some() {
            // 有别的长任务在跑：让它先结束（按钮那边也会换成"…中"）
            return;
        }
        let container = db.container.clone();
        let bin = db.client.clone();
        // 按**行**取账号：这一行的库是 `database`，导入就该用它的账号
        //（见 [`Shell::creds_for_row`]）。「导入备份」是在同一个库上灌数据，
        // 和「备份」必须是同一个身份 —— 否则"备份能成、导入 Access denied"
        // 会变成一个只看浮层文案解释不了的现象。
        //
        // ⚠️ 必须在 `database` 被包成 `Option` **之前**取：下面那一步是为了
        // `ImportState` 的字段类型，而这里要的是裸库名。
        let creds = self.creds_for_row(&container, &database, cx);
        let database = Some(database);

        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("(名字读不出来)")
            .to_owned();
        if let Some(db) = self.state.db.as_mut() {
            db.import = Some(ImportState {
                file_name: file_name.clone(),
                database: database.clone(),
                dismissed: false,
            });
        }
        self.db_dialog = None;
        self.state.notify(Toast::info(format!(
            "正在导入 {file_name}{}…",
            database
                .as_deref()
                .map(|d| format!("到 {d}"))
                .unwrap_or_default()
        )));
        cx.notify();

        let wslc = self.state.wslc.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = {
                let wslc = wslc.clone();
                let container = container.clone();
                let bin = bin.clone();
                let creds = creds.clone();
                let path = path.clone();
                let database = database.clone();
                cx.background_executor()
                    .spawn(async move {
                        // ① 送进容器（目录目标 + 先 mkdir，实测见 cmd::mysql）
                        let container_path = wslc_core::cmd::mysql::push_dump_to_container(
                            &wslc, &container, &path,
                        )?;
                        // ② 让容器里的 mysql 读它
                        wslc_core::cmd::mysql::load_sql_file(
                            &wslc,
                            &container,
                            &bin,
                            &creds,
                            &container_path,
                            database.as_deref(),
                        )
                    })
                    .await
            };

            let _ = this.update_in(cx, |shell, _window, cx| {
                if let Some(db) = shell.state.db.as_mut() {
                    if db.container == container {
                        db.import = None;
                    }
                }
                match &result {
                    Ok(()) => {
                        shell.state.notify(Toast::success(format!(
                            "已导入 {file_name}{}",
                            database
                                .as_deref()
                                .map(|d| format!("到 {d}"))
                                .unwrap_or_default()
                        )));
                        // 表数 / 占用会变，重读一次让列表反映出来
                        shell.reload_db_lists(cx);
                    }
                    Err(e) => {
                        let failure = DbFailure::from_text(e.to_string());
                        shell
                            .state
                            .notify(Toast::error(format!("导入失败：{}", failure.summary())));
                        // 原文留在页面上（toast 只给一句"该怎么办"）
                        if let Some(db) = shell.state.db.as_mut() {
                            if db.container == container {
                                db.fail(e.to_string());
                            }
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 从数据库列表某一行开 SQL 控制台：**用这个库 + 这一行的账号**。
    ///
    /// 库名记在 `sql_database` 上（执行时作为 `mysql -D <库>` 传下去），
    /// 账号交给 `Shell::creds_for_row` 按行取 —— 它优先用面板给这个库记的账号，
    /// 没有才退回连接卡。
    ///
    /// ⚠️ **不要**把那一行的账号写进连接卡。卡是整页共享的「管理账号」
    /// （用户填的，通常 root）；写进去之后一退出控制台，「数据库管理」页的
    /// 读用户列表 / 新建库 / 备份就全都改用业务账号，报 `ERROR 1142`。
    /// 实机踩过两次：第一次只补了"退出时还原"，本质仍在借用同一张卡。
    /// 现在用户看到的是：卡上永远是 root，SQL 页头上写着"账号 X（这个库的）"。
    ///
    /// 那一行没有账号记录（不是面板建的库）时**只设库名**，账号保持用户当前
    /// 填的那个 —— 不猜、不编（凭据只对面板建的库有）。
    pub fn open_db_console_for(
        &mut self,
        database: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(db) = self.state.db.as_ref() else {
            return;
        };
        let container = db.container.clone();
        let credential = self
            .state
            .db_credentials
            .get(&container, &database)
            .cloned();

        // 🔴 **不要改连接卡。**
        //
        // 卡是整页共享的「管理账号」——它该一直是用户填的那个（通常是 root）。
        // 点某个库的「SQL」只是"用**这个库的**账号去看这个库"，那件事由
        // `db.sql_database` + `Shell::creds_for_row` 表达（执行时按行取账号），
        // 不需要也不应该把卡改掉。
        //
        // 实机踩过两次：早先这里会把那一行的账号写进卡，于是退出控制台之后
        // 「数据库管理」页（读用户列表 / 新建库 / 备份）全部改用业务账号在跑，
        // 报 `ERROR 1142`；第一版补丁只是"退出时还原"，本质仍在借用同一张卡。
        if let Some(db) = self.state.db.as_mut() {
            db.sql_database = Some(database.clone());
            db.clear_error();
        }
        // 顺序很重要：**先切页、再给焦点**。
        //
        // `set_page(Page::Sql)` 会调 `ensure_db_form` 把编辑器建出来，而它
        // **首次**创建时会把焦点抢到「密码」那格（见 `ensure_db_form` 末尾）。
        // 所以焦点必须在这之后给，否则用户一进来敲字会打到密码框里去。
        self.set_page(Page::Sql, window, cx);
        if let Some(form) = self.db_form.as_ref() {
            let handle = form.sql.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }
        let toast = match credential {
            Some(cred) => Toast::info(format!(
                "控制台已指向 {database}，用这个库自己的账号 {}（连接卡还是你的管理账号）",
                cred.user
            )),
            None => Toast::info(format!(
                "控制台已指向 {database}（这个库不是面板建的，账号沿用连接卡上那个）"
            )),
        };
        self.state.notify(toast);
        cx.notify();
    }

    /// 把控制台的"当前库"清掉（回到不指定 —— 由语句自己 `USE`）。
    pub fn clear_sql_database(&mut self, cx: &mut Context<Self>) {
        if let Some(db) = self.state.db.as_mut() {
            db.sql_database = None;
        }
        cx.notify();
    }

    /// 「新建数据库」弹窗里开关「同时创建用户」。
    pub fn set_db_create_user(&mut self, create_user: bool, cx: &mut Context<Self>) {
        if let Some(dialog) = self.db_dialog.as_mut() {
            dialog.create_user = create_user;
        }
        cx.notify();
    }

    /// 把「数据库用户名」填成建议值 `<库名>_user`（弹窗里那个「用建议值」按钮）。
    ///
    /// ⚠️ 刻意**不**做成"输入库名时自动联动" —— 实机反馈是"让手填，不要自动
    /// 生成"。这里只在用户**主动点**的时候写一次，而且写完就不再联动
    ///（后面改库名不会把它改掉）。
    ///
    /// 建议值取自**当前**库名，所以先填库名再点，值才符合直觉；库名还空着时
    /// 算出来是 `_user`（没有意义），这时只给一句提示。
    pub fn fill_db_username(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self.db_dialog.as_ref() else {
            return;
        };
        let database = dialog.database.read(cx).value().trim().to_owned();
        if database.is_empty() {
            self.state.notify(Toast::error(
                "先填库名，再点「用建议值」（建议值就是库名 + _user）",
            ));
            cx.notify();
            return;
        }
        let suggested = mysql_model::default_user_for(&database);
        let input = dialog.db_user.clone();
        input.update(cx, |state, cx| state.set_value(suggested, window, cx));
        cx.notify();
    }

    /// 关掉动作弹窗（不提交）。
    pub fn close_db_dialog(&mut self, cx: &mut Context<Self>) {
        self.db_dialog = None;
        cx.notify();
    }

    /// 把「导入中」浮层收起来（**不**取消导入）。
    ///
    /// 导入是分钟级的，而那个浮层带遮罩 —— 没有这个出口时用户会被锁在
    /// 屏幕前。收起来之后导入照跑，完成回调里照样发提示、照样重读列表。
    ///
    /// 为什么用 `dismissed` 而不是把 `db.import` 置 `None`：那个 `Option`
    /// 同时是**互斥锁**（`start_db_import` 第一句就查它）。置 `None` 会放开
    /// "再来一份导入"的口子，两份 `container cp` 抢同一个目录。
    pub fn dismiss_db_import(&mut self, cx: &mut Context<Self>) {
        if let Some(db) = self.state.db.as_mut() {
            if let Some(import) = db.import.as_mut() {
                import.dismissed = true;
            }
        }
        self.state.notify(Toast::info(
            "导入还在后台跑 —— 完成后会提示你，列表也会自动重读",
        ));
        cx.notify();
    }

    /// 选授权级别。
    pub fn set_db_grant(&mut self, grant: GrantKind, cx: &mut Context<Self>) {
        if let Some(dialog) = self.db_dialog.as_mut() {
            dialog.grant = grant;
        }
        cx.notify();
    }

    /// 提交动作弹窗。
    ///
    /// 这里最重要的一条：**参数不合法就在本地拦下，不关弹窗** ——
    /// 用户就在那一格里，改一个字比重打一遍强（`AGENTS.md` §7.5 的
    /// 「空值等于没填」也是这个思路）。
    ///
    /// 「建库同时建用户」（v0.8）返回第三样 `to_record`：账号要等 SQL
    /// **成功之后**才写进本地存档 —— 列表上不能出现一个 MySQL 里
    /// 根本不存在的账号。所有段**先都拼出来、都校验过**再合并成一条
    /// `-e`（`;` 分隔，实测能跑）—— 半途失败时一条都不会发出去，
    /// 不会留下"建了库没建用户"的半成品。
    pub fn submit_db_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 先把要用的东西取成 owned 的（下面要改 `self`，不能再借着它）
        // 闭包**返回 `Result<_, wslc_core::Error>`**：里面的 `?` 全是
        // `Error -> Error`，不需要 From 转换。`String` 的转换在**外面**做
        //（上一版把 `?` 放进 `Result<_, String>` 的闭包里，
        // `String: From<Error>` 不存在 —— CI 抓的）。
        // ⚠️ 类型要写**全路径** `wslc_core::Result`：app.rs 作用域里的裸
        // `Result` 来自 `gpui_kit::*`，它的默认错误类型是 anyhow —— 两个
        // "Result" 长得一样，是不同的类型（CI 抓的）。
        let prepared: wslc_core::Result<Option<PreparedDbSql>> = (|| {
            let Some(dialog) = self.db_dialog.as_ref() else {
                return Ok(None);
            };
            let text = |input: &Entity<InputState>| input.read(cx).value().trim().to_owned();

            match &dialog.kind {
                DbDialogKind::NewDatabase => {
                    let charset = text(&dialog.charset);
                    let collation = text(&dialog.collation);
                    let database = text(&dialog.database);
                    let mut parts: Vec<String> = Vec::new();
                    parts.push(mysql_model::create_database_sql(
                        &database,
                        (!charset.is_empty()).then_some(charset.as_str()),
                        (!collation.is_empty()).then_some(collation.as_str()),
                    )?);
                    // 用户名**由用户手填**（v0.9.3 起）。
                    // host 固定 `%`（要远程连的账号，默认锁 localhost 反而添堵）。
                    let mut to_record = None;
                    if dialog.create_user {
                        let user = text(&dialog.db_user);
                        // 留空就拦下来：这条路径下"没填用户名"= 建了一个连不上
                        // 的库，属于 §7.5 那种"看着填了、其实空着"的陷阱。
                        // 报错**不关弹窗**（外面的 Err 分支只发提示），
                        // 用户改一个字就能重试。
                        if user.is_empty() {
                            return Err(wslc_core::Error::InvalidArgument(
                                "请填数据库用户名（不知道填什么就点「用建议值」）".to_owned(),
                            ));
                        }
                        mysql_model::validate_ident("用户名", &user)?;
                        let password = text(&dialog.password);
                        parts.push(mysql_model::create_user_sql(&user, "%", &password)?);
                        parts.push(mysql_model::grant_sql(
                            &database,
                            &user,
                            "%",
                            GrantKind::All,
                        )?);
                        to_record = Some((database, user, password));
                    }
                    Ok(Some((parts.join("; "), dialog.kind.title(), to_record)))
                }
                DbDialogKind::NewUser => {
                    let grant_db = text(&dialog.grant_db);
                    let host = text(&dialog.host);
                    // ⚠️ 这里**必须**是 `then(|| …)` 而不是 `then_some(…)`：
                    // `then_some` 是**急切**求值 —— 它会在 `grant_db` 为空时
                    // 也构造 `("" , grant)`，而 `grant_db.as_str()` 是对
                    // `grant_db` 的借用，急着构造会把 `grant_db` 借到
                    // `create_user_statements` 之后（借用冲突）。
                    // clippy 的 `unnecessary_lazy_evaluations` 看不到这一层。
                    #[allow(clippy::unnecessary_lazy_evaluations)]
                    let grant = (!grant_db.is_empty()).then(|| (grant_db.as_str(), dialog.grant));
                    mysql_model::create_user_statements(
                        &text(&dialog.user),
                        &host,
                        &text(&dialog.password),
                        grant,
                    )
                    .map(|sql| Some((sql, dialog.kind.title(), None)))
                }
                DbDialogKind::AlterPassword { user, host } => {
                    mysql_model::alter_user_password_sql(user, host, &text(&dialog.password))
                        .map(|sql| Some((sql, dialog.kind.title(), None)))
                }
                DbDialogKind::Grant { user, host } => {
                    let grant_db = text(&dialog.grant_db);
                    if grant_db.is_empty() {
                        Err(wslc_core::Error::InvalidArgument(
                            "请填要授权给哪个数据库".to_owned(),
                        ))
                    } else {
                        mysql_model::grant_sql(&grant_db, user, host, dialog.grant)
                            .map(|sql| Some((sql, dialog.kind.title(), None)))
                    }
                }
                // 导入**不是**一条 SQL（它是"把文件送进容器让 mysql 读它"），
                // 所以确认按钮那边直接调 `start_db_import`，走不到这里。
                // 这一臂只是为了让 match 完备。
                DbDialogKind::ImportSql { .. } => Ok(None),
            }
        })();

        // 弹窗不在（理论上来不到）→ 什么都不做
        let (sql, title, to_record) = match prepared {
            Ok(None) => return,
            Ok(Some(prepared)) => prepared,
            Err(e) => {
                // 编码字段折叠后，校验错误仍应把用户带到可见的输入框。
                let message = e.to_string();
                let encoding_input = self.db_dialog.as_ref().and_then(|dialog| {
                    if message.contains("字符集") {
                        Some(dialog.charset.clone())
                    } else if message.contains("排序规则") {
                        Some(dialog.collation.clone())
                    } else {
                        None
                    }
                });
                if let Some(input) = encoding_input {
                    self.state.set_section_open(UiSection::DbEncoding, true);
                    let handle = input.read(cx).focus_handle(cx);
                    window.focus(&handle, cx);
                }
                // 不关弹窗，保留输入供用户直接修正。
                self.state.notify(Toast::error(message));
                cx.notify();
                return;
            }
        };

        let Some(db) = self.state.db.as_ref() else {
            return;
        };
        if db.busy {
            return;
        }
        let container = db.container.clone();
        let bin = db.client.clone();
        // 执行用的是**连接卡**上的账号（弹窗里的 user/host 是"被操作的对象"）。
        // 走 [`Shell::db_creds`] 而不是自己拼：那是全页唯一的取账号入口。
        let creds = self.db_creds(cx);

        self.db_dialog = None;
        self.run_db_statement(
            // 结构体字面量：每个值跟着名字走 —— 原先那 6 个 `String` 参数
            // 顺序写错了编译器也不会报（类型一样）。
            DbStatement {
                container,
                bin,
                creds,
                sql,
                title,
                to_record,
            },
            window,
            cx,
        );
    }

    /// SQL 控制台：执行输入框里那段脚本（**切分后逐条执行**）。
    ///
    /// # v0.11 起结果不再是"一段文本"
    ///
    /// 之前是 `--table` 的 ASCII 表格整段塞进 `log_text` —— 用户看不懂
    /// （实机反馈原话："现有的这个查询界面和数据展示很难看懂"）。现在：
    ///
    /// - 客户端切分（`model::sqlstmt`）→ 逐条执行（`cmd::mysql::run_statements`）；
    /// - 每条一个 `SqlResult`，界面上画成真正的表格（`views::db_console_card`）；
    /// - 同时留一份 `--table` 的原文进 `sql_raw`，作"看原始输出"的兜底。
    pub fn run_db_console(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = self.db_form.as_ref() else {
            return;
        };
        // ⚠️ `EditorState` 用 `text()`，不是 `InputState::value()`
        // （`text()` 返回的还不是 `String`，要 `.to_string()` 一次）。
        let sql = form.sql.read(cx).text().to_string();
        let sql = sql.trim().to_owned();
        if sql.is_empty() {
            self.state.notify(Toast::error("先写一条 SQL 再执行"));
            cx.notify();
            return;
        }
        let Some(db) = self.state.db.as_ref() else {
            return;
        };
        if db.busy {
            return;
        }
        let container = db.container.clone();
        let bin = db.client.clone();
        // 控制台**针对哪个库**：从数据库列表行上的「SQL」按钮进来时是那一行，
        // 否则是 `None`（由语句自己 `USE`）。不指定时 `SELECT * FROM t`
        // 会报 "No database selected"（实测过）。
        let database = db.sql_database.clone();
        // 账号也要跟着**那个库**走：从 `djxm` 行的「SQL」进来就该用 `djxm_user`，
        // 而不是"连接卡上碰巧是谁"（见 [`Shell::creds_for_row`]）。
        // `sql_database` 为 `None`（手动清掉了当前库）时按行的兜底规则取——
        // 那时用连接卡，因为没有一个具体的库可以查存档。
        let creds = match database.as_deref() {
            Some(name) => self.creds_for_row(&container, name, cx),
            None => self.db_creds(cx),
        };
        // v0.14：在 `db` 还是**不可变**借用的时候就取好驱动和连接。
        //
        // ⚠️ 必须放在下面那个 `self.state.db.as_mut()` **之前**：借用检查器
        // 看的是最后一次使用 —— 一旦在后面再碰 `db`，那段可变借用就会冲突（E0502）。
        //
        // `driver()` 返回 `Arc<dyn SqlDriver>`：要搬进 `background_executor`
        // 的闭包里，所以必须是静态生命周期 + Send + Sync。
        let mut conn = db.conn.clone();
        conn.database = database.clone();
        let driver = db.driver();
        let cred = state::to_credentials(&creds);
        // v0.16：用户填了连接串就走**原生连接**（能连远程库），否则走原来的路径。
        // 连接串在状态里是 DsnText（Debug 手写脱敏，见那个类型）。sync move
        // 闭包里要的是 &str，所以在克隆点就取成原文 —— DsnText::as_str 引用
        // 的是 db，跨不过 move。
        let dsn = db.dsn.as_ref().map(|d| d.as_str().to_owned());

        if let Some(db) = self.state.db.as_mut() {
            db.busy = true;
            db.clear_error();
            db.last_sql = sql.clone();
            db.sql_results.clear();
            db.sql_raw = None;
            db.sql_error = None;
            db.sql_ran = false;
        }
        cx.notify();

        let wslc = self.state.wslc.clone();
        cx.spawn_in(window, async move |this, cx| {
            let target = container.clone();
            let result = {
                let bin = bin.clone();
                let creds = creds.clone();
                let sql = sql.clone();
                let database = database.clone();
                let driver = driver.clone();
                let conn = conn.clone();
                let cred = cred.clone();
                let dsn = dsn.clone();
                cx.background_executor()
                    .spawn(async move {
                        let started = std::time::Instant::now();
                        // 两条引擎路径的差别只在这里：
                        //
                        // - MySQL：`run_statements` 逐条跑（结构化），
                        //   `run_console_sql` 再跑**整段** `--table` 留原文 ——
                        //   多花一次 exec 换"能看到 mysql 原样打了什么"。
                        // - PostgreSQL：`psql` 没有 `--table` 那种人读格式，
                        //   原文那一档**直接不给**（`raw = None`，界面上的
                        //   「看原始输出」按钮本来就会按 `sql_raw.is_some()` 隐藏）。
                        if matches!(conn.kind, DbKind::Postgres) {
                            // 有连接串 → 原生 TCP（能连远程库，容器里有没有 psql 都不影响）；
                            // 没有 → 退回 exec 那条路（在容器里跑 psql）。
                            let structured = match dsn.as_deref() {
                                Some(text) => crate::native_db::parse(text)
                                    .and_then(|spec| crate::native_db::run_sql(&spec, &sql)),
                                None => driver
                                    .run_script(&wslc, &conn, &cred, &sql)
                                    .map_err(|e| e.to_string()),
                            };
                            let elapsed = started.elapsed().as_millis() as u64;
                            return (structured, None, elapsed);
                        }
                        // MySQL / MariaDB 有连接串也走原生（同一个门面）。
                        // 原生这条路没有 `--table` 那种人读原文，所以 `raw = None`
                        // —— 和 PostgreSQL 同一个取舍。
                        if matches!(conn.kind, DbKind::MySql | DbKind::MariaDb) {
                            if let Some(text) = dsn.as_deref() {
                                let structured = crate::native_db::parse(text)
                                    .and_then(|spec| crate::native_db::run_sql(&spec, &sql));
                                let elapsed = started.elapsed().as_millis() as u64;
                                return (structured, None, elapsed);
                            }
                        }
                        let structured = wslc_core::cmd::mysql::run_statements(
                            &wslc,
                            &target,
                            &bin,
                            &creds,
                            &sql,
                            database.as_deref(),
                        );
                        let elapsed = started.elapsed().as_millis() as u64;
                        let raw = wslc_core::cmd::mysql::run_console_sql(
                            &wslc,
                            &target,
                            &bin,
                            &creds,
                            &sql,
                            database.as_deref(),
                        )
                        .ok();
                        (structured.map_err(|e| e.to_string()), raw, elapsed)
                    })
                    .await
            };

            let _ = this.update_in(cx, |shell, window, cx| {
                shell.apply_db_console_result(container, result, window, cx);
            });
        })
        .detach();
    }

    /// 把一次 SQL 执行的结果敲进状态。
    ///
    /// `result` 是 `(结构化结果, 原始输出, 耗时毫秒)`。拆成独立函数是为了
    /// 让 `spawn_in` 那个闭包只做"搬运"，判断都集中在有注释的这里。
    ///
    /// ⚠️ 要 `&mut Window`：结果集变了就得**重建** `TableState`
    /// （`TableState::new` 要 window，和 `InputState::new` 同一个道理）。
    pub(crate) fn apply_db_console_result(
        &mut self,
        container: String,
        result: (
            std::result::Result<Vec<wslc_core::model::sqlresult::SqlResult>, String>,
            Option<String>,
            u64,
        ),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (structured, raw, elapsed_ms) = result;
        let mut reload = false;
        // 重建表格要用的东西先算好（`sql_table` 是 `self` 的字段，
        // 而 `db` 借的是 `self.state`，两者同时可变借用会撞车）。
        let mut new_table: Option<wslc_core::model::sqlresult::SqlResult> = None;
        if let Some(db) = self.state.db.as_mut() {
            if db.container != container {
                return;
            }
            db.busy = false;
            db.sql_ran = true;
            db.sql_elapsed_ms = elapsed_ms;
            db.sql_raw = raw;
            match structured {
                Ok(results) => {
                    db.sql_results = results;
                    db.sql_error = None;
                    db.clear_error();
                    // 第一个**有表格**的结果作为默认 tab —— 全是 DDL 时停在 0。
                    let first_grid = db
                        .sql_results
                        .iter()
                        .position(|r| !r.columns.is_empty())
                        .unwrap_or(0);
                    new_table = db.sql_results.get(first_grid).cloned();
                    self.sql_result_tab = first_grid;
                    // 控制台可能刚改了库 / 表 —— 顺手把列表刷新一次。
                    // 失败时列表不会变，就不白跑一趟了。
                    reload = true;
                }
                Err(message) => {
                    // SQL 报错**不算**页面级错误：它就发生在控制台里，
                    // 显示在控制台下面比顶到页头更贴合上下文。
                    //
                    // ⚠️ 必须存进 `sql_error`（而不是像 v0.10 那样塞回输出）：
                    // 渲染那边靠它区分"红字错误"和"结果表格"。v0.10 曾经只置
                    // `sql_ran = true` 而让输出是 `None`，界面显示
                    // **"正在加载…"** 而真正的报错去了页头（实机截图抓到的：
                    // `show database` 拼错那次）。
                    db.sql_results.clear();
                    db.sql_error = Some(message);
                    // 页面级那条留给"读列表 / 连接"这类失败，
                    // 清了它免得同一条错误在屏幕上出现两遍。
                    db.clear_error();
                }
            }
        }
        // 表格整个换掉。`None`（报错 / 全是 DDL / 没有结果）时也清，
        // 免得旧结果还留在屏幕上。
        self.sql_table = new_table.map(|r| {
            cx.new(|cx| {
                gpui_kit::component::table::TableState::new(
                    crate::sql_table::SqlResultDelegate::new(r),
                    window,
                    cx,
                )
                .cell_selectable(true)
            })
        });
        if reload {
            self.reload_db_lists(cx);
        }
        cx.notify();
    }

    /// 切换结果 tab（多语句时）。
    ///
    /// 换 tab 要重建 `TableState` —— 每个结果的列和行都不一样，没法复用。
    pub fn switch_sql_result_tab(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(db) = self.state.db.as_ref() else {
            return;
        };
        let Some(result) = db.sql_results.get(index) else {
            return;
        };
        if result.columns.is_empty() {
            // 没有表格的结果（DDL / 无输出）不建表格 —— 界面画一句说明就够了
            self.sql_table = None;
        } else {
            let result = result.clone();
            self.sql_table = Some(cx.new(|cx| {
                gpui_kit::component::table::TableState::new(
                    crate::sql_table::SqlResultDelegate::new(result),
                    window,
                    cx,
                )
                .cell_selectable(true)
            }));
        }
        self.sql_result_tab = index;
        cx.notify();
    }

    /// 切换「看原始输出」的展开状态。
    pub fn toggle_sql_raw(&mut self, cx: &mut Context<Self>) {
        self.sql_raw_open = !self.sql_raw_open;
        cx.notify();
    }

    /// 把当前结果导出成 CSV（v0.11.3）。
    ///
    /// # 流程
    ///
    /// 弹保存对话框 → 用户在后台等 → 写文件（**原子写**，见 `fileio`）。
    ///
    /// # 为什么写盘也在后台
    ///
    /// 几万行的 CSV 是几 MB，写到机械盘 / 网络盘上要几百毫秒。
    /// `AGENTS.md` §7.1：界面线程上的任何阻塞都会让窗口"未响应"。
    /// 这里虽然不是起进程，但 `std::fs::write` 一样会卡。
    ///
    /// # 为什么复用 `picking`
    ///
    /// 那个布尔的作用是"已经有一个选择框开着"（见 `spawn_picker`）。
    /// 导出也弹选择框，两边共用同一个守卫才不会一次弹出两个对话框
    /// —— 任务栏里堆一排对话框是最糟的。
    pub fn export_sql_result_csv(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.picking {
            self.state
                .notify(Toast::info("已经有一个选择框开着了，先处理它"));
            cx.notify();
            return;
        }

        // ⚠️ 这一段**整体**放在一个块里，让 `db`（对 `self.state` 的不可变借用）
        // 在块结束时立刻归还。外面还要 `self.state.notify(..)`（那要 `&mut`）
        // 和 `self.picking = true`（要 `&mut self`）—— 借用没结束就写，是 E0502。
        //
        // 这个形状在 `open_db_backup_dialog` / `run_db_console` 里都用过：
        // 先把要用的东西算成 owned 的，出了块再用。
        let prepared: Option<(String, usize, String)> = {
            let Some(db) = self.state.db.as_ref() else {
                return;
            };
            let result = db.sql_results.get(self.sql_result_tab);
            // 没有结果、或者结果没有表格（DDL / 无输出）→ 不给导出。
            // 一个只有表头的 CSV 没有意义，只会让用户以为导出坏了。
            match result {
                Some(r) if !r.columns.is_empty() => {
                    // 建议文件名：`库-时间戳.csv`。表名我们不知道（结果可能是
                    // JOIN 出来的），所以用当前库名兜底。
                    let stamp = wslc_core::model::mysql::now_secs();
                    let base = db.sql_database.as_deref().unwrap_or("result").to_owned();
                    // ⚠️ 行数**现在**记下来，不要去数 CSV 字符串里有多少个换行 ——
                    // 单元格里的换行是被 `"` 包着的（RFC 4180），
                    // `lines().count()` 会把一个跨两行的单元格数成两行。
                    Some((
                        format!("{base}-{stamp}.csv"),
                        r.row_count(),
                        crate::sqlview::to_csv(r),
                    ))
                }
                _ => None,
            }
        };

        let Some((suggested, row_count, csv)) = prepared else {
            self.state.notify(Toast::error("这个结果没有表格可以导出"));
            cx.notify();
            return;
        };

        self.picking = true;
        cx.notify();

        let title = "导出 CSV".to_owned();
        cx.spawn_in(window, async move |this, cx| {
            // 第一步：选路径（后台起 powershell 等对话框）。
            let picked = cx
                .background_executor()
                .spawn(async move {
                    wslc_core::cmd::picker::pick_save_file(&title, &suggested, &["csv"])
                })
                .await;

            // ⚠️ `update_in` 的回调是**同步**的，里面不能再 await。
            // 所以"收下路径 → 交回一个待写的任务"这两步要分开：
            // 回调只负责把 `picking` 复位、把决定带出来；
            // 真正的写盘在**外面**（还在这个 async 块里）做。
            //
            // 项目里另一个 `background_executor` 也只出现在 `cx.spawn` 的
            // 顶层（见 `start_toast_reaper`），没有"在 update 回调里 await"的先例。
            //
            // 返回值：`Ok(决定)` = 回调跑过了；`Err(_)` = 实体已销毁（窗口关了）。
            let decision = this.update_in(cx, |shell, _window, _cx| {
                shell.picking = false;
                match picked {
                    Ok(Some(path)) => SaveDecision::Write(std::path::PathBuf::from(path)),
                    Ok(None) => SaveDecision::Cancelled,
                    Err(e) => SaveDecision::Failed(e.to_string()),
                }
            });

            let target = match decision {
                // 实体已销毁 / 用户取消 —— 都不用说话
                Err(_) => return,
                Ok(SaveDecision::Cancelled) => return,
                Ok(SaveDecision::Failed(e)) => {
                    let _ = this.update(cx, |shell, cx| {
                        shell
                            .state
                            .notify(Toast::error(format!("打开保存对话框失败：{e}")));
                        cx.notify();
                    });
                    return;
                }
                Ok(SaveDecision::Write(p)) => p,
            };

            // 第二步：写盘（后台 —— 几 MB 的 CSV 落到机械盘上要几百毫秒）。
            let body = csv;
            let write_to = target.clone();
            let write = cx
                .background_executor()
                .spawn(async move {
                    wslc_panel_core::fileio::write_atomic(&write_to, &body)
                        .map_err(|e| e.to_string())
                })
                .await;

            let _ = this.update(cx, |shell, cx| {
                match write {
                    Ok(()) => shell.state.notify(Toast::success(format!(
                        "已导出 {row_count} 行到 {}",
                        target.display()
                    ))),
                    Err(e) => shell.state.notify(Toast::error(format!("写文件失败：{e}"))),
                }
                cx.notify();
            });
        })
        .detach();
    }

    // -- SQL 控制台的库 / 表树（v0.11.2，v0.12.1 换成组件的 `Tree`）-----------

    /// 确保搜索框存在（`InputState::new` 要 `&mut Window`，所以懒创建）。
    pub(crate) fn ensure_schema_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.schema_search.is_some() {
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
        self.schema_search = Some(input);
        self.schema_search_sub = Some(sub);
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
        if self.schema_tree_built_for.as_ref() == Some(&fingerprint)
            && self.schema_tree_state.is_some()
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
        self.schema_tree_state = Some(state);
        self.schema_tree_built_for = Some(fingerprint);

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
        let mut parts = id.split('\u{1}');
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
                                .and_then(|spec| crate::native_db::schema(&spec)),
                            None => driver
                                .schema(&wslc, &conn, &cred)
                                .map_err(|e| e.to_string()),
                        }
                    } else if let Some(text) = dsn.as_deref() {
                        // 有连接串 → 原生（能读远程库的对象树）。
                        crate::native_db::parse(text)
                            .and_then(|spec| crate::native_db::schema(&spec))
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
                    shell.schema_tree_built_for = None;
                    let query = shell
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

    /// 点树里的表名 → 把 `SELECT * FROM \`库\`.\`表\` LIMIT 200` 填进编辑区。
    ///
    /// # 为什么是"填进去"而不是"直接执行"
    ///
    /// 用户点表是想看数据，但**先看一眼 SQL** 是更安全的默认：他可能想加
    /// `WHERE`，也可能只是点错了表。填进去之后按一下「执行」就行，
    /// 而"点了就立刻跑一个全表扫描"是不可撤销的（尤其是生产库）。
    pub fn pick_schema_table(
        &mut self,
        database: String,
        table: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let sql = wslc_core::model::schematree::select_from_table_sql(&database, &table);
        // 顺便把「当前库」设成这个表所在的库 —— 否则 `SELECT * FROM \`库\`.\`表\``
        // 虽然能跑，但下面再手写 `SELECT * FROM 别的表` 时会报
        // "No database selected"（实测过）。
        if let Some(db) = self.state.db.as_mut() {
            db.sql_database = Some(database);
        }
        if let Some(form) = self.db_form.as_ref() {
            form.sql
                .update(cx, |state, cx| state.set_value(sql, window, cx));
        }
        cx.notify();
    }

    /// 从数据库管理页回到容器列表。
    ///
    /// **不**清 `state.db`：下次再进来时连接信息与刚读到的两张表都还在
    /// （省掉一次探测 + 两次查询）。
    pub fn close_database_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_page(Page::Containers, window, cx);
        cx.notify();
    }

    // -- 部署管理（nginx / Java / Node.js / Go，v0.9）------------------------

    /// 在后台执行一条 DDL，成功后重读两张表。
    ///
    /// 和 [`Shell::run_db_console`] 的区别：那个把结果显示在控制台里，
    /// 这个只报一句成功 / 失败（表单提交的路径上没有"结果区"）。
    pub(crate) fn run_db_statement(
        &mut self,
        req: DbStatement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 拆回局部变量：下面那句 `async move` 会逐个把它们搬走，写成
        // `req.container` / `req.bin` 反而要在每个用的地方跟一遍 `req.`。
        let DbStatement {
            container,
            bin,
            creds,
            sql,
            title,
            to_record,
        } = req;
        if let Some(db) = self.state.db.as_mut() {
            db.busy = true;
            db.clear_error();
        }
        self.state.notify(Toast::info(format!("正在{title}…")));
        cx.notify();

        let wslc = self.state.wslc.clone();
        cx.spawn_in(window, async move |this, cx| {
            let target = container.clone();
            let result = {
                let bin = bin.clone();
                let creds = creds.clone();
                let sql = sql.clone();
                cx.background_executor()
                    .spawn(async move {
                        run_sql(&wslc, &target, &bin, &creds, &sql, None, SqlMode::Batch)
                            .map(|_| ())
                            .map_err(|e| e.to_string())
                    })
                    .await
            };

            let _ = this.update_in(cx, |shell, _window, cx| {
                let mut ok = false;
                if let Some(db) = shell.state.db.as_mut() {
                    if db.container == container {
                        db.busy = false;
                        match &result {
                            Ok(()) => {
                                db.clear_error();
                                ok = true;
                            }
                            Err(message) => db.fail(message.clone()),
                        }
                    }
                }
                match &result {
                    Ok(()) => {
                        // 成功才写档：列表上不能出现一个 MySQL 里不存在的账号。
                        // 写档失败只提示（最坏是重启后要重新录入），不回滚。
                        if let Some((database, user, password)) = to_record {
                            shell.state.db_credentials.put(
                                &container,
                                &database,
                                wslc_panel_core::dbcreds::DbCredential {
                                    user,
                                    password,
                                    host: "%".to_owned(),
                                },
                            );
                            if let Some(e) = shell.state.save_db_credentials() {
                                shell.state.notify(Toast::error(e));
                            }
                        }
                        shell.state.notify(Toast::success(format!("{title}成功")));
                    }
                    Err(message) => {
                        // 给用户看的是"该怎么办"（分类给的），原文留在页面上
                        let failure = DbFailure::from_text(message.clone());
                        shell
                            .state
                            .notify(Toast::error(format!("{title}失败：{}", failure.summary())));
                    }
                }
                if ok {
                    shell.reload_db_lists(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    // -- 数据库备份 / 恢复（v0.8）--------------------------------------------
    //
    // 实测过的路：
    // 备份 = 容器内 `mysqldump … > /tmp/wslc-backups/…` + `container cp` 拉回 Windows；
    // 恢复 = 把 **Windows 侧那份** `container cp` 进
    //        `/tmp/wslc-restore/`（目标写成**目录**）+ 容器内 `mysql … < …`。
    //        容器重建 / `/tmp` 被清都不再是障碍（v0.8.2 改，
    //        完整实测矩阵见 `wslc_core::model::mysql::restore_container_path`）。

    /// 备份一个数据库（长任务；状态在 `AppState::db.backup`）。
    pub fn start_db_backup(
        &mut self,
        database: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 原生连接下备份走不通：`mysqldump` 要跑在**容器里**，而原生连接
        // 可能连的是一台远程机器，`db.client` 那时是 `native` 哨兵。
        // 不拦的后果是 `exec: native: not found` —— 看上去像容器坏了。
        //
        // ⚠️ 判据先算成独立变量：`db` 借着 `self.state`，而 notify 要
        // `&mut self.state`（借用冲突）。
        let blocked = self
            .state
            .db
            .as_ref()
            .and_then(|db| db.write_blocked_reason());
        if let Some(reason) = blocked {
            self.state.notify(Toast::error(reason));
            cx.notify();
            return;
        }
        let Some(db) = self.state.db.as_ref() else {
            return;
        };
        if db.backup.is_some() {
            // 已经有一个在跑 —— 按钮那边也换成了「备份中…」
            return;
        }
        if db.busy || db.import.is_some() {
            // 读列表 / 导入没结束时不开新的长任务（进度浮层也挡着点击，
            // 这一条是防"从别的路径调进来"）
            //
            // ⚠️ 别静默返回：点了没反应会被当成"按钮坏了"（`AGENTS.md` §7.1）。
            // 这个分支原先一声不响 —— 而现在它是「立即备份」唯一可能的失败方式。
            self.state.notify(Toast::info(
                "正在读列表 / 导入中 —— 等它结束再点「立即备份」",
            ));
            cx.notify();
            return;
        }
        let Some(backup_dir) = self.state.backup_dir.clone() else {
            self.state
                .notify(Toast::error("LOCALAPPDATA 未设置，备份没地方放"));
            cx.notify();
            return;
        };
        let container = db.container.clone();
        let bin = db.client.clone();
        // 按**行**取账号：面板给这个库记过就用它自己的账号，没记过才用连接卡
        //（见 [`Shell::creds_for_row`]）。这一行是 `djxm`，备份就必须是
        // `djxm_user` 在 dump `djxm` —— 而不是"连接卡上碰巧是什么"。
        let creds = self.creds_for_row(&container, &database, cx);
        let paths = match wslc_core::cmd::mysql::backup_paths(
            &backup_dir,
            &container,
            &database,
            wslc_core::model::mysql::now_secs(),
        ) {
            Ok(paths) => paths,
            Err(e) => {
                self.state.notify(Toast::error(e.to_string()));
                cx.notify();
                return;
            }
        };

        if let Some(db) = self.state.db.as_mut() {
            db.backup = Some(BackupState {
                database: database.clone(),
                container_path: paths.container_path.clone(),
                windows_path: paths.windows_path.clone(),
                last_message: None,
                dismissed: false,
            });
        }
        self.state
            .notify(Toast::info(format!("正在备份数据库 {database}…")));
        cx.notify();

        // 备份目录先算出来：下面完成回调里要在**后台**重扫它。
        let backup_scan_dir = self.state.container_backup_dir(&container);
        let wslc = self.state.wslc.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = {
                let wslc = wslc.clone();
                let container = container.clone();
                let bin = bin.clone();
                let creds = creds.clone();
                let database = database.clone();
                let paths = paths.clone();
                // ⚠️ `BackgroundExecutor::spawn` 只吃 **future**（不是普通闭包）
                // —— `async move { 阻塞活 }` 把同步调用包进去，跟其它命令一个写法
                cx.background_executor()
                    .spawn(async move {
                        wslc_core::cmd::mysql::backup_database(
                            &wslc, &container, &bin, &creds, &database, &paths,
                        )
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                    })
                    .await
            };

            // 重扫备份目录也要在**后台**做（`read_dir` + 每条 metadata）：
            // 下面那个 `update_in` 跑在界面线程上。失败不致命 —— 列表保持原样。
            let rescanned = match backup_scan_dir {
                Some(dir) => {
                    cx.background_executor()
                        .spawn(async move { state::scan_backup_dir(&dir).ok() })
                        .await
                }
                None => None,
            };

            let _ = this.update_in(cx, |shell, _window, cx| {
                let finished = shell
                    .state
                    .db
                    .as_mut()
                    .filter(|db| db.container == container)
                    .map(|db| db.backup.take());
                match (&result, finished.flatten()) {
                    (Ok(()), Some(state)) => {
                        if let Some(entries) = rescanned {
                            shell.state.apply_backups(&container, entries);
                        }
                        shell.state.notify(Toast::success(format!(
                            "备份完成：{}（容器内留了一份用于恢复）",
                            state.windows_path.display()
                        )));
                    }
                    (Ok(()), None) => {
                        // 备份完成但页面已切走：文件是好的，一句话说明
                        shell
                            .state
                            .notify(Toast::info("备份完成（数据库页已切走）"));
                    }
                    (Err(message), _) => {
                        if let Some(db) = shell.state.db.as_mut() {
                            if db.container == container {
                                db.fail(message.clone());
                            }
                        }
                        let failure = DbFailure::from_text(message.clone());
                        shell
                            .state
                            .notify(Toast::error(format!("备份失败：{}", failure.summary())));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 关掉备份列表浮层。
    pub fn close_db_backup_dialog(&mut self, cx: &mut Context<Self>) {
        if let Some(db) = self.state.db.as_mut() {
            db.backup_dialog = None;
        }
        cx.notify();
    }

    /// 打开某个库的备份列表（列表是 Windows 侧目录扫出来的）。
    ///
    /// 顺手把面板记的账号**写进连接卡**：这一页所有动作都按行取账号
    ///（`Shell::creds_for_row`），而卡是没记账号时的兜底。从列表行进来的用户
    /// 根本没见过那张卡（它藏在页头的「基础信息」后面）。不写的话，
    /// **面板建的库**（有自己的账号，root 往往不对）会拿 root 去 dump，
    /// 报一句 Access denied，而用户没地方知道该填谁 —— 浮层里那行
    /// "备份用账号 X" 也因此显示了正确的值。
    /// 理由和 `open_db_console_for` 完全一样：卡上显示什么，执行就用什么。
    ///
    /// ⚠️ 写完卡还要顺手写 `db.creds`：那个字段不再是执行入口（见
    /// [`Shell::db_creds`]），但它仍然是"上一次连成功的那份"，留着是为了
    /// 将来做诊断时能和卡上的值对照 —— 不同步会让那份对照失真。
    pub fn open_db_backup_dialog(
        &mut self,
        database: String,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 浮层里的三个动作（立即备份 / 恢复 / 删除）在原生连接下全都走不通。
        // 让它打开只会得到"一个列着文件、但每个按钮都报同一句话"的浮层 ——
        // 不如在门口就说清。
        //
        // ⚠️ 判据先算成独立变量（`db` 的借用与 `notify` 的 `&mut self.state` 冲突）。
        let blocked = self
            .state
            .db
            .as_ref()
            .and_then(|db| db.write_blocked_reason());
        if let Some(reason) = blocked {
            self.state.notify(Toast::error(reason));
            cx.notify();
            return;
        }
        let Some(db) = self.state.db.as_ref() else {
            return;
        };
        let container = db.container.clone();

        // 同 `open_db_console_for`：**不改连接卡**。备份/导入/恢复用的是
        // `Shell::creds_for_row`（按这个库取账号），浮层里也会写明"备份用账号 X"。
        // 卡留给「管理账号」——它该一直是用户填的那个。
        if let Some(db) = self.state.db.as_mut() {
            db.backup_dialog = Some(database);
        }
        // 进浮层前重扫一次（可能在文件管理器里手动删过 / 拷进来过）
        self.state.rescan_backups(&container);
        cx.notify();
    }

    /// 恢复一份备份（**覆盖**该库现有数据 —— 走二次确认）。
    pub fn restore_db_backup(&mut self, index: usize, cx: &mut Context<Self>) {
        // 原生连接下恢复也走不通：它要 `container cp` + 容器里的 `mysql`。
        // ⚠️ 判据先算成独立变量（借用冲突，同 `start_db_backup`）。
        let blocked = self
            .state
            .db
            .as_ref()
            .and_then(|db| db.write_blocked_reason());
        if let Some(reason) = blocked {
            self.state.notify(Toast::error(reason));
            cx.notify();
            return;
        }
        let Some(db) = self.state.db.as_ref() else {
            return;
        };
        if db.backup.is_some() || db.busy {
            return;
        }
        let Some(entry) = db.backups.get(index) else {
            return;
        };
        // 账号按**这条备份所属的库**取（`Shell::creds_for_row`）：
        // 备份浮层点开时已经把自己钉在某个库上（`backup_dialog`），
        // 「恢复」就是"把这条备份灌回它的原库"，所以身份必须和「备份」
        // 完全一致 —— 备份用 `djxm_user`、恢复也必须用它。
        //
        // 这一行原先取的是 `db.creds`（只在 `load_db` 里赋一次值），
        // 于是"点某一行的「备份」把卡换成 shop_user，再在同一个浮层里点
        // 「恢复」"会用 **root** 去恢复，报 Access denied 而浮层上明明写着
        // 「备份用账号 shop_user」。现在两条路径都按行走，不再看卡。
        let creds = self.creds_for_row(&db.container, &entry.database, cx);
        let action = PendingAction::RestoreDatabase {
            container: db.container.clone(),
            database: entry.database.clone(),
            client: db.client.clone(),
            // 恢复的输入是 **Windows 侧那份**：执行时会把它 cp 进容器
            //（所以容器重建过也照样能恢复，见 `PendingAction::RestoreDatabase`）
            windows_path: entry.path.clone(),
            creds,
        };
        self.request(action, cx);
    }

    /// 请求删除一份备份文件（**先弹二次确认**）。
    ///
    /// 删除是不可撤销的：Windows 侧那份文件删了就是删了，容器内那份也一起
    /// `rm -f`，面板里没有回收站。它的兄弟动作「恢复」一直有确认（覆盖数据
    /// 至少还能再备份回来），而删除原先**没有** —— 这一条是补那个反着的配置。
    ///
    /// 真正的执行在 [`Shell::delete_db_backup_confirmed`]：那个函数只拿
    /// `PendingAction::DeleteDatabaseBackup` 里那几个字段，不再自己查索引
    /// （确认框开着的期间列表可能已经变了，按索引取会删错一条）。
    pub fn delete_db_backup(&mut self, index: usize, cx: &mut Context<Self>) {
        // 删除本身有一半是**容器内**的 `rm -f`。原生连接下那一步会去
        // `exec … rm`（`rm` 是真的，所以它**会执行成功**）—— 但里面的
        // 路径是按容器内约定拼的，远程库上根本没有那份文件。
        // 所以和备份 / 恢复同一条判据：先拦住，别留下"删了但没删干净"。
        //
        // ⚠️ 判据先算成独立变量（借用冲突，同 `start_db_backup`）。
        let blocked = self
            .state
            .db
            .as_ref()
            .and_then(|db| db.write_blocked_reason());
        if let Some(reason) = blocked {
            self.state.notify(Toast::error(reason));
            cx.notify();
            return;
        }
        let Some(db) = self.state.db.as_ref() else {
            return;
        };
        let Some(entry) = db.backups.get(index) else {
            return;
        };
        let action = PendingAction::DeleteDatabaseBackup {
            container: db.container.clone(),
            database: entry.database.clone(),
            windows_path: entry.path.clone(),
            container_path: entry.container_path.clone(),
        };
        self.request(action, cx);
    }

    /// 执行一份备份的删除（已经从二次确认里点过「永久删除」）。
    ///
    /// 参数是**确认时定下来的**路径，不再按索引回查：确认框开着的几秒里
    /// 用户可能在文件管理器里删过、或者又备份了一份（列表重排），
    /// 按索引取会删掉另一条。
    pub fn delete_db_backup_confirmed(
        &mut self,
        container: String,
        database: String,
        windows_path: std::path::PathBuf,
        container_path: String,
        cx: &mut Context<Self>,
    ) {
        let wslc = self.state.wslc.clone();
        // ⚠️ 这两件事都是**阻塞**的（删 Windows 侧文件 + 进容器 `rm -f`），
        // 所以整段丢进后台执行器。`cx.spawn(…)` 的 future 是在**界面线程**上被
        // poll 的，直接在它里面调 `wslc.run(…)` 就是 `AGENTS.md` §7.1 那个形状
        //（点一下窗口变灰，直到子进程结束）。仓库里其余后台任务都是这个写法：
        // `cx.background_executor().spawn(async move { … }).await`。
        //
        // 重扫备份目录也放在同一个后台块里：`read_dir` + 每条 metadata 会卡
        // 一下，而这个块下面的 `update` 回调在界面线程上。
        //
        // ⚠️ `container` / `database` 后面在 `update` 里还要用（刷列表、拼提示），
        // 而内层那两个 `async move` 会把它们搬走 —— 所以这里备好两份 clone。
        let exec_container = container.clone();
        let target_container = container.clone();
        let target_database = database.clone();
        let scan_dir = self.state.container_backup_dir(&container);
        cx.spawn(async move |this, cx| {
            let (file_result, container_result, rescanned) = cx
                .background_executor()
                .spawn(async move {
                    // Windows 侧那份是**主体**，删不掉要报错；容器内那份尽力删
                    //（容器停了 / 文件没了都不影响这次删除的结果）
                    let file_result =
                        std::fs::remove_file(&windows_path).map_err(|e| e.to_string());
                    let container_result = wslc
                        .run(&["exec", &exec_container, "rm", "-f", &container_path])
                        .map(|_| ())
                        .map_err(|e| e.to_string());
                    let rescanned = scan_dir
                        .as_deref()
                        .and_then(|dir| state::scan_backup_dir(dir).ok());
                    (file_result, container_result, rescanned)
                })
                .await;

            let _ = this.update(cx, |shell, cx| {
                match &file_result {
                    Ok(()) => {
                        let _ = container_result;
                        if let Some(entries) = rescanned {
                            shell.state.apply_backups(&target_container, entries);
                        }
                        shell.state.notify(Toast::success(format!(
                            "已删除 {target_database} 的备份（容器内那份尽力清理）"
                        )));
                    }
                    Err(e) => shell
                        .state
                        .notify(Toast::error(format!("删不掉备份文件：{e}"))),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 把「备份中」浮层收起来（**不**取消备份）。
    ///
    /// 这一版**没有**接取消令牌：`backup_database` 里是两条**顺序**的短命令
    /// （dump 一般几十秒内自己结束），接令牌要把它改成 `spawn_to_file` +
    /// 轮询那条路 —— 等"边 dump 边传"一起做。所以这里**不**假装能打断，
    /// 只把遮罩收掉（它挡住整页）。
    ///
    /// 和[`Shell::dismiss_db_import`] 同一个理由；区别是这个浮层底下没有
    /// 互斥锁要维护（`db.backup` 照样是 `Some`，所以「立即备份」仍然是
    /// "备份中…"、不会重复起一份）。
    pub fn cancel_db_backup(&mut self, cx: &mut Context<Self>) {
        if let Some(db) = self.state.db.as_mut() {
            if let Some(backup) = db.backup.as_mut() {
                backup.dismissed = true;
            }
        }
        self.state.notify(Toast::info(
            "备份还在跑（这一版不能中途打断）—— 完成后会提示你，列表也会自动重读",
        ));
        cx.notify();
    }

    /// 在文件管理器里打开备份目录（「打开位置」）。
    ///
    /// 同步 `explorer.exe`：它不等待、不捕获输出 —— `AGENTS.md` §7.1 允许。
    pub fn reveal_backup_dir(&mut self, dir: Option<std::path::PathBuf>, cx: &mut Context<Self>) {
        let Some(dir) = dir.filter(|d| d.is_dir()) else {
            self.state
                .notify(Toast::error("备份目录不存在（可能已被清空）"));
            cx.notify();
            return;
        };
        let toast = match std::process::Command::new("explorer.exe").arg(&dir).spawn() {
            Ok(_) => Toast::info(format!("已打开 {}", dir.display())),
            Err(e) => Toast::error(format!("打不开文件管理器：{e}")),
        };
        self.state.notify(toast);
        cx.notify();
    }

    // -- 在新终端窗口里打开的东西 --------------------------------------------

    /// 打开「基础信息」浮层（数据库页的连接信息）。
    pub fn open_db_info(&mut self, cx: &mut Context<Self>) {
        self.db_info_open = true;
        cx.notify();
    }

    /// 关掉「基础信息」浮层。
    pub fn close_db_info(&mut self, cx: &mut Context<Self>) {
        self.db_info_open = false;
        cx.notify();
    }

    /// 从 SQL 控制台回到「数据库管理」页。
    ///
    /// `sql_database` 留着 —— 下次点同一行的「SQL」还是那个库，省得重选。
    pub fn close_db_console(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_page(Page::Database, window, cx);
        cx.notify();
    }
}
