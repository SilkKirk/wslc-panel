//! 数据库备份 / 恢复：跑备份、列备份、恢复、删备份（含容器内那份）。
//!
//! # 实测过的路
//!
//! 备份 = 容器内 `mysqldump … > /tmp/wslc-backups/…` + `container cp` 拉回 Windows；
//! 恢复 = 把 **Windows 侧那份** `container cp` 进 `/tmp/wslc-restore/`
//! （目标写成**目录**）+ 容器内 `mysql … < …`。容器重建 / `/tmp` 被清都不再是障碍
//! （v0.8.2 改，完整实测矩阵见 `wslc_core::model::mysql::restore_container_path`）。

use super::super::super::prelude::*;
use super::super::Shell;

impl Shell {
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
}
