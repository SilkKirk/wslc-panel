//! SQL 控制台：执行语句、多结果切换、原始输出、导出 CSV。

use super::super::super::prelude::*;
use super::super::Shell;
use super::super::state::{DbStatement, SaveDecision};

impl Shell {
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
        if let Some(form) = self.forms.db_form.as_ref() {
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
        let Some(form) = self.forms.db_form.as_ref() else {
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

    /// 设置「原始输出」面板是否展开。
    ///
    /// 收目标值：界面上是受控 `Switch`，回调给的就是用户请求的新状态。
    pub fn set_sql_raw_open(&mut self, open: bool, cx: &mut Context<Self>) {
        self.sql_raw_open = open;
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
        if self.tasks.picking {
            self.state
                .notify(Toast::info("已经有一个选择框开着了，先处理它"));
            cx.notify();
            return;
        }

        // ⚠️ 这一段**整体**放在一个块里，让 `db`（对 `self.state` 的不可变借用）
        // 在块结束时立刻归还。外面还要 `self.state.notify(..)`（那要 `&mut`）
        // 和 `self.tasks.picking = true`（要 `&mut self`）—— 借用没结束就写，是 E0502。
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
                        wslc_panel_core::sqlview::to_csv(r),
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

        self.tasks.picking = true;
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
            // 项目里其它 `background_executor` 也都只在 `cx.spawn` 的
            // 顶层（例如 `refresh_inner`），没有"在 update 回调里 await"的先例。
            //
            // 返回值：`Ok(决定)` = 回调跑过了；`Err(_)` = 实体已销毁（窗口关了）。
            let decision = this.update_in(cx, |shell, _window, _cx| {
                shell.tasks.picking = false;
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
}
