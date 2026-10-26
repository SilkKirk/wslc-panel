//! 一次性动作弹窗：建库 / 建用户 / 改密 / 授权 / 导入 SQL。

use super::super::super::prelude::*;
use super::super::Shell;
use super::super::core::PreparedDbSql;
use super::super::state::{AfterPick, DbDialog, DbDialogKind, DbStatement, DropdownItem};

impl Shell {
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

        self.forms.db_dialog = Some(DbDialog {
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
        let Some(dialog) = self.forms.db_dialog.as_ref() else {
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
        let Some(input) = self.forms.db_dialog.as_ref().map(|d| d.grant_db.clone()) else {
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
        let Some(input) = self.forms.db_dialog.as_ref().map(|d| d.import_path.clone()) else {
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
            let Some(dialog) = self.forms.db_dialog.as_ref() else {
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
        self.forms.db_dialog = None;
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

    /// 「新建数据库」弹窗里开关「同时创建用户」。
    pub fn set_db_create_user(&mut self, create_user: bool, cx: &mut Context<Self>) {
        if let Some(dialog) = self.forms.db_dialog.as_mut() {
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
        let Some(dialog) = self.forms.db_dialog.as_ref() else {
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
        self.forms.db_dialog = None;
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
        if let Some(dialog) = self.forms.db_dialog.as_mut() {
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
        // ⚠️ 类型要写**全路径** `wslc_core::Result`：app/ 作用域里的裸
        // `Result` 来自 `gpui_kit::*`，它的默认错误类型是 anyhow —— 两个
        // "Result" 长得一样，是不同的类型（CI 抓的）。
        let prepared: wslc_core::Result<Option<PreparedDbSql>> = (|| {
            let Some(dialog) = self.forms.db_dialog.as_ref() else {
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
                        // 的库，属于 `AGENTS.md` §7.5 那种"看着填了、其实空着"的陷阱。
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
                let encoding_input = self.forms.db_dialog.as_ref().and_then(|dialog| {
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

        self.forms.db_dialog = None;
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
}
