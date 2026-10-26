//! 「数据库管理」页的生命周期：进来时探测、读取两张表、切 tab、离开时收尾。

use super::super::super::prelude::*;
use super::super::Shell;
use super::super::core::effective_db_creds;

impl Shell {
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
            if let Some(form) = self.forms.db_form.as_ref() {
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
                                crate::native_db::mysql_databases(text).map_err(|e| e.to_string()),
                                crate::native_db::mysql_users(text).map_err(|e| e.to_string()),
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
                if let Some(form) = shell.forms.db_form.as_ref() {
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
                            let spec = crate::native_db::parse(text).map_err(|e| e.to_string())?;
                            let version = crate::native_db::server_version(&spec)
                                .map_err(|e| e.to_string())?;
                            let names =
                                crate::native_db::databases(&spec).map_err(|e| e.to_string())?;
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
                                crate::native_db::mysql_databases(text).map_err(|e| e.to_string()),
                                crate::native_db::mysql_users(text).map_err(|e| e.to_string()),
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

    /// 从数据库管理页回到容器列表。
    ///
    /// **不**清 `state.db`：下次再进来时连接信息与刚读到的两张表都还在
    /// （省掉一次探测 + 两次查询）。
    pub fn close_database_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_page(Page::Containers, window, cx);
        cx.notify();
    }

    /// 打开「基础信息」浮层（数据库页的连接信息）。
    pub fn open_db_info(&mut self, cx: &mut Context<Self>) {
        self.dialog_ui.db_info_open = true;
        cx.notify();
    }

    /// 关掉「基础信息」浮层。
    pub fn close_db_info(&mut self, cx: &mut Context<Self>) {
        self.dialog_ui.db_info_open = false;
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
