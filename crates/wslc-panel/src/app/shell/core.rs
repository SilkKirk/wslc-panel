//! `Shell` 的核心：构造、自动刷新、页面切换、二次确认、提示条。
//!
//! 这些是**其他每个域都会用到**的那一层，所以单独一个文件 ——
//! 想看「一次点击到底走了哪几步」，先读这里。

use super::super::prelude::*;
use super::Shell;
use super::state::{DropdownItem, InstallForm};

/// 取锁；中毒（别的线程 panic 过）时照常用里面的值。
///
/// 这几个锁保护的都只是"事件队列"，中毒不影响数据本身的正确性 ——
/// 在这里 `unwrap()` 只会把后台线程的一次 panic 变成界面的一次 crash。
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// 安装目录里已经有东西了吗。
///
/// ⚠️ 只在**提交那一刻**调用：渲染每帧都可能发生，而 `read_dir` 碰上网络盘
/// 或者掉线的移动硬盘能卡很久 —— 那种卡顿看起来就像界面死了。
pub(crate) fn dir_non_empty(dir: &str) -> bool {
    let dir = dir.trim();
    if dir.is_empty() {
        return false;
    }
    match std::fs::read_dir(dir) {
        Ok(mut entries) => entries.next().is_some(),
        // 目录不存在 / 没权限 → 当作"不是非空"。
        // 真有问题的话，后面的 `create_dir_all` / `wsl` 会给一句更准确的错。
        Err(_) => false,
    }
}

/// 这次连数据库要用哪个账号。
///
/// 规则只有一条：**用户填了就用他的**，没填才用探测到的那个
/// （容器环境变量里的 `MYSQL_ROOT_PASSWORD`）。
///
/// 为什么"空密码"要特殊对待：空密码是 MySQL 的**合法配置**
/// （`MYSQL_ALLOW_EMPTY_PASSWORD=yes`），而"还没填"和"就是要空密码"在界面上
/// 长得一模一样。这里选"环境变量优先" —— 那种容器里环境变量写的本来就是
/// 空密码（`RootCredHint::Empty`），两条路殊途同归。
pub(crate) fn effective_db_creds(
    probe: &MysqlProbe,
    typed_user: &str,
    typed_password: &str,
) -> (String, String) {
    let user = if typed_user.is_empty() {
        "root".to_owned()
    } else {
        typed_user.to_owned()
    };
    let password = if typed_password.is_empty() {
        match &probe.root_hint {
            RootCredHint::Password(password) => password.clone(),
            _ => String::new(),
        }
    } else {
        typed_password.to_owned()
    };
    (user, password)
}

/// 本程序自己的版本号（`Cargo.toml` 的 `version`），解析成可比较的形式。
///
/// 解析不出来时退回 `0.0.0`：那会让"远端比 0.0.0 新"永远成立 ——
/// 也就是"有新版就提示"，这是**故意选的方向**。反过来（退回一个很大的版本）
/// 会让更新提示彻底静默，而那种 bug 没人会发现。
pub(crate) fn current_version() -> wslc_core::update::Version {
    wslc_core::update::Version::parse(env!("CARGO_PKG_VERSION")).unwrap_or(
        wslc_core::update::Version {
            major: 0,
            minor: 0,
            patch: 0,
            pre: None,
        },
    )
}

/// `wsl --status` 的采集周期（秒）。
///
/// 它每轮都跑没必要：默认发行版 / 默认版本极少变，而且它的输出是**本地化**的
/// （中文系统是「默认分发:」），解析成本比列表高。
///
/// 列表（`wsl --list --verbose`）仍然跟随自动刷新 —— 状态变化要看得到。
pub(crate) const DISTRO_STATUS_SECS: u64 = 30;

/// 数据库动作弹窗「校验通过之后要执行的东西」。
///
/// 形状是 `(SQL, 标题, 要记进本地存档的账号)`：
///
/// - 第 1 样是拼好的 SQL（多段用 `; ` 连接，实测能跑）；
/// - 第 2 样是提示条里的动词（"新建数据库" / "改密码"……）；
/// - 第 3 样**只在「建库同时建用户」时有值**：`(库, 用户, 密码)`。
///   它必须等 SQL **执行成功**之后才写进本地存档 —— 列表上不能出现一个
///   MySQL 里根本不存在的账号。
///
/// # 为什么要一个别名
///
/// 这个三元组原先直接写在 `submit_db_dialog` 的 `let prepared:` 里，
/// 那行的类型长这样：
///
/// ```text
/// wslc_core::Result<Option<(String, &'static str, Option<(String, String, String)>)>>
/// ```
///
/// clippy 的 `type_complexity` 会报（它确实很难一眼看懂）。起个名字之后
/// 那个 `let` 只剩下"这是个 Result"，而**具体形状在这里有文档**。
pub(crate) type PreparedDbSql = (String, &'static str, Option<(String, String, String)>);

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
pub(crate) const TREE_ID_SEP: char = '\u{1}';

/// 表节点（叶子）的 id：`t\u{1}库\u{1}表`。
pub(crate) fn leaf_id(database: &str, table: &str) -> String {
    format!("t{TREE_ID_SEP}{database}{TREE_ID_SEP}{table}")
}

/// 库节点（父节点）的 id：`d\u{1}库`。
///
/// 只需要唯一 —— 点库节点时组件自己展开/收起，我们不解析它。
pub(crate) fn folder_id(database: &str) -> String {
    format!("d{TREE_ID_SEP}{database}")
}

/// 危险操作的确认浮层。
///
/// 容器域和发行版域共用这一个浮层 —— 文案由 [`ConfirmAction`] 自己给，
/// 所以这里不需要知道是哪个域。
/// 通用二次确认对话框的**内容**（标题 / 底部按钮由 `Dialog` 负责）。
///
/// 文案里的 Markdown 必须过 `strip_md`：这一条是"不可撤销"那类确认框，
/// 留着字面星号比别处更糟（`util::strip_md` 的注释里有完整理由）。
pub(crate) fn confirm_body(action: &ConfirmAction) -> AnyElement {
    div()
        .text_sm()
        .text_color(theme::text_muted())
        .child(wslc_panel_core::util::strip_md(&action.body()))
        .into_any_element()
}

impl Shell {
    /// 创建外壳：立刻触发一次采集、加载配置、启动自动刷新。
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut shell = Self {
            state: AppState::new(Wslc::new(), Wsl::new()),
            last_refresh: None,
            errors_expanded: false,
            container_search: None,
            container_search_sub: None,
            pull_cancel: None,
            export_cancel: None,
            picking: false,
            keep_alive: Vec::new(),
            wslconf_dialog: None,
            terminal_dir_input: None,
            create_dialog: None,
            exec_input: None,
            detail: None,
            distro_detail: None,
            install_form: None,
            select_subs: Vec::new(),
            image_form: None,
            image_subs: Vec::new(),
            pull_open: false,
            create_subs: Vec::new(),
            prompt: None,
            dialog_stack: Vec::new(),
            install_run: None,
            install_cancel: None,
            refresh_again: false,
            refresh_visible: false,
            status_tick: 0,
            deploy_tick: 0,
            db_form: None,
            sql_table: None,
            sql_result_tab: 0,
            sql_raw_open: false,
            sidebar_collapsed: false,
            tree_collapsed: false,
            schema_tree_state: None,
            schema_tree_built_for: None,
            schema_search: None,
            schema_search_sub: None,
            db_dialog: None,
            db_info_open: false,
            deploy_form: None,
        };
        shell.refresh(cx);
        shell.start_auto_refresh(cx);
        shell.start_toast_reaper(cx);
        // 更新检查单独排一条时间线：延迟 8 秒，只查一次（见那个方法）。
        shell.start_auto_check(cx);
        shell
    }

    /// 建「容器」页的搜索框（幂等）。
    ///
    /// **必须**在有 `&mut Window` 的地方调用（`InputState::new` 要它），
    /// 所以入口是 `set_page`，不是 render。
    ///
    /// 订阅 `InputEvent::Change` 不是优化，是**必须**：渲染整页的是 `Shell`，
    /// 输入框改的是 `InputState` 自己的值 —— 不订阅父级不会重绘，过滤结果
    /// 要等到下一次自动刷新才变，打字时看起来就像"搜索框坏了"。
    /// （v0.6 的 tag 过滤框就是这套写法，见 `InputEvent` 的导入注释。）
    pub(crate) fn ensure_container_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.container_search.is_some() {
            return;
        }

        let input =
            cx.new(|cx| InputState::new(window, cx).placeholder("搜索名称 / 镜像 / ID / 端口"));
        let sub = cx.subscribe(&input, |_shell, _input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });

        self.container_search = Some(input);
        self.container_search_sub = Some(sub);
        cx.notify();
    }

    /// 建「添加实例」表单（幂等）。
    ///
    /// **必须**在有 `&mut Window` 的地方调用 —— `InputState::new` 要它。
    ///
    /// 安装目录预填「默认安装目录」偏好（见 `prefs.rs`）：它同时是"由名字推路径"
    /// 的基准，用户不用每装一个都重新敲一遍盘符。
    pub(crate) fn ensure_install_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.install_form.is_some() {
            return;
        }

        let default_source = InstallSourceKind::default();
        let default_dir = self.state.prefs.install_dir.clone().unwrap_or_default();

        // 两张清单都是**下拉框**（组件库的 `Select`）。它们和输入框一样要
        // `&mut Window`，所以在这里一起建；条数不多（商店 ~24、镜像源 24），
        // 一开始给空列表，拉回来再 `set_items` 填。
        //
        // 商店那张带搜索（`searchable`）：微软的清单里 24 个发行版的名字
        // 有好几对很像（Ubuntu 26.04 / 24.04 / 22.04 / 20.04），
        // 原先是列表外面单独一个搜索框，现在收进下拉框里（参考实现没有搜索，
        // 但它的清单是 `wsl -l -o` 的顺序、我们的更长）。
        let store_select = cx.new(|cx| {
            SelectState::new(Vec::<DropdownItem>::new(), None, window, cx).searchable(true)
        });
        let mirror_select =
            cx.new(|cx| SelectState::new(Vec::<DropdownItem>::new(), None, window, cx));

        // 选中即生效：商店那条把 id 记下来并填进名字框；
        // 镜像源那条**立刻开始自动测速**（参考实现就是"选中即测速择优"，
        // 没有"探测"按钮）。
        //
        // ⚠️ `subscribe_in` 返回的 `Subscription` **一丢就退订** ——
        // 必须存进 `Shell`（组件库自己也是这么做的：`vec![cx.subscribe_in(...)]`）。
        let store_sub = cx.subscribe_in(
            &store_select,
            window,
            |shell, _emitter, event: &SelectEvent<Vec<DropdownItem>>, window, cx| {
                if let SelectEvent::Confirm(Some(id)) = event {
                    shell.pick_online_distro(id.clone(), window, cx);
                }
            },
        );
        let mirror_sub = cx.subscribe_in(
            &mirror_select,
            window,
            |shell, _emitter, event: &SelectEvent<Vec<DropdownItem>>, _window, cx| {
                if let SelectEvent::Confirm(Some(id)) = event {
                    shell.select_mirror_distro(id.clone(), cx);
                }
            },
        );
        self.select_subs = vec![store_sub, mirror_sub];

        self.install_form = Some(InstallForm {
            name: cx.new(|cx| {
                InputState::new(window, cx).placeholder(default_source.name_placeholder())
            }),
            install_dir: cx.new(|cx| InputState::new(window, cx).placeholder(r"D:\wsl\MyDistro")),
            source_path: cx.new(|cx| {
                InputState::new(window, cx).placeholder(default_source.path_placeholder())
            }),
            mirror_url: cx.new(|cx| {
                InputState::new(window, cx).placeholder("留空 = 用自动测速挑出来的那个地址")
            }),
            store_select,
            mirror_select,
            online_id: None,
            source: default_source,
            launch: false,
            set_default: false,
            // 默认走**微软商店**（实测本机这条路能连上；两条通道都不带 `--location`，
            // 理由见 `model::install::plan` 里那张表）。开关在界面上，用户可以切；
            // 真失败了执行器还会自动换一次（见 `PlannedStep::retry_other_source`）。
            web_download: false,
            last_derived_dir: if default_dir.is_empty() {
                None
            } else {
                Some(default_dir.clone())
            },
            show_errors: false,
            field_errors: InstallFieldErrors::default(),
        });

        // 预填默认安装目录本身（"名字还没填"时它就是基准）
        if !default_dir.is_empty() {
            if let Some(form) = self.install_form.as_ref() {
                form.install_dir
                    .update(cx, |state, cx| state.set_value(default_dir, window, cx));
            }
        }
    }

    /// 按名字刷新安装目录（**只在用户没自己改过时**）。
    ///
    /// 判断依据是 [`InstallForm::last_derived_dir`]：输入框的值还等于我们上次推的
    /// （或者干脆是空的），就说明它是我们填的，可以跟着名字走；
    /// 否则一律不动 —— 用户手输了路径却被我们改掉，是最恼人的一类 bug。
    pub(crate) fn resync_install_dir(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let default_dir = self.state.prefs.install_dir.clone();
        let Some(form) = self.install_form.as_mut() else {
            return;
        };

        let name = form.name.read(cx).value().trim().to_owned();
        let current = form.install_dir.read(cx).value().trim().to_owned();
        let untouched =
            current.is_empty() || Some(current.as_str()) == form.last_derived_dir.as_deref();
        if !untouched {
            return;
        }

        let derived = derive_install_dir(default_dir.as_deref(), &name)
            .or_else(|| default_dir.clone().filter(|_| name.is_empty()));
        if let Some(dir) = derived {
            form.install_dir
                .update(cx, |state, cx| state.set_value(dir.clone(), window, cx));
            form.last_derived_dir = Some(dir);
        }
    }

    // -- 单输入框提示弹窗（移动位置 / 调整大小 / 设置默认用户）---------------

    /// 隔多少轮刷新采一次 `wsl --status`。
    ///
    /// **按秒换算而不是写死轮数**：刷新间隔是用户可以改的（1~600 秒），
    /// 写死"每 10 轮"在间隔改成 30 秒时就变成 5 分钟一次了。
    pub(crate) fn status_every_n_ticks(&self) -> u32 {
        let secs = self.state.prefs.refresh_secs.max(1);
        u32::try_from(DISTRO_STATUS_SECS.div_ceil(secs))
            .unwrap_or(u32::MAX)
            .max(1)
    }

    // -- 镜像：远端浏览 / 拉取（v0.6 建，v0.6.2 并进「镜像」页） ---------------

    /// 用系统默认浏览器打开一个 URL。
    ///
    /// # 为什么是 `explorer.exe`
    ///
    /// 它把 URL 交给 Windows 的默认协议处理器（也就是默认浏览器），
    /// 本身不等待、不捕获输出 —— `AGENTS.md` §7.1 允许这种"只把进程拉起来"
    /// 的同步调用（和打开资源管理器同一个理由）。
    ///
    /// ⚠️ 只接受 `http://` / `https://`：这个入参来自容器 inspect 的
    /// `Ports`，理论上是我们自己拼的，但把任意字符串交给
    /// `explorer.exe` 等于多开一个协议处理器入口，没必要。
    pub fn open_url(&mut self, url: String, cx: &mut Context<Self>) {
        let url = url.trim().to_owned();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            self.state
                .notify(Toast::error(format!("不是可打开的 URL：{url}")));
            cx.notify();
            return;
        }
        match std::process::Command::new("explorer.exe").arg(&url).spawn() {
            Ok(_) => {
                tracing::info!("已用默认浏览器打开 {url}");
                self.state
                    .notify(Toast::info(format!("已在浏览器打开 {url}")));
            }
            Err(e) => {
                tracing::warn!("打开浏览器失败：{e}");
                self.state
                    .notify(Toast::error(format!("打开浏览器失败：{e}")));
            }
        }
        cx.notify();
    }

    // -- 更新提示 ----------------------------------------------------------
    //
    // 这一节只做两件事：**查**（一次 HTTP GET）和**显示**（卡片 + 一条提示）。
    // 刻意不做下载/校验/替换 exe —— 用户自己在发布页取文件，理由见
    // `docs/PLAN-install-update.md` §9。

    /// 后台采集一次完整快照，并让顶部按钮显示「刷新中…」。
    ///
    /// 点按钮、以及操作（启动/停止/创建/删除）完成后的补刷都走这里。
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.refresh_inner(cx, true);
    }

    /// 后台采集一次完整快照，但**不动**顶部按钮的文案（静默刷新）。
    ///
    /// 3 秒一次的自动刷新走这条：它是后台行为，按钮该一直写着「刷新」；
    /// 用户想立刻看到最新数据时自己点按钮（那条走 [`Shell::refresh`]）。
    pub(crate) fn refresh_quiet(&mut self, cx: &mut Context<Self>) {
        self.refresh_inner(cx, false);
    }

    /// 采集的公共实现。`visible` 表示这一轮要不要点亮顶部按钮。
    ///
    /// 已经在采的时候**不会并发再开一轮**，但也不会丢掉这次请求：
    /// 记在 `refresh_again` 上，本轮结束后立刻补跑。
    ///
    /// 这一点很关键 —— 操作（启动/停止/创建/删除）完成后都会调它，
    /// 如果正好撞上 3 秒的自动刷新就把这次请求丢掉，
    /// 界面要等到下一个周期才变，用户会以为操作没生效。
    pub(crate) fn refresh_inner(&mut self, cx: &mut Context<Self>, visible: bool) {
        // 先顺手清掉已经死掉的哨兵（用户点了「终止」、或在别处 `wsl --shutdown`）。
        // 放在最前面：`busy` 那条提前返回也不该让界面上的那份"谁在吊着它"
        // 的信息过期（发行版详情弹窗里那句说明读的就是 `state.kept_alive`）。
        self.sync_keep_alive(cx);

        if self.state.busy {
            // **不能丢**：正在跑的那轮采到的是操作**之前**的数据。
            // 丢掉这次请求，界面就要等下一个周期才变 ——
            // 用户看到的就是"点了停止，状态还是运行中"。
            self.refresh_again = true;
            // 用户点的按钮正好撞上后台那轮：立刻点亮按钮，免得"点了没反应"。
            // 补跑的那轮会一直亮到采完（见下面处理 `refresh_again` 的顺序）。
            if visible && !self.refresh_visible {
                self.refresh_visible = true;
                cx.notify();
            }
            return;
        }
        self.refresh_again = false;
        self.state.busy = true;
        self.refresh_visible = visible;
        cx.notify();

        // `wsl --status` 降频到 30 秒一次；其余（含发行版列表）跟随本轮刷新。
        let with_status = self.status_tick % self.status_every_n_ticks() == 0;
        self.status_tick = self.status_tick.wrapping_add(1);

        let wslc = self.state.wslc.clone();
        let wsl = self.state.wsl.clone();
        cx.spawn(async move |this, cx| {
            let (snapshot, status) = cx
                .background_executor()
                .spawn(async move {
                    let snapshot = state::load_snapshot(&wslc, &wsl);
                    // 同一轮里串行跑，避免并发起两个 wsl.exe
                    let status = with_status.then(|| state::load_distro_status(&wsl));
                    (snapshot, status)
                })
                .await;

            let _ = this.update(cx, |shell, cx| {
                shell.state.busy = false;
                shell.refresh_visible = false;
                // 耗时只写日志，不在界面上显示。
                tracing::debug!(
                    "采集完成：{} ms，{} 个容器，{} 个发行版，{} 处错误",
                    snapshot.elapsed_ms,
                    snapshot.all.len(),
                    snapshot.distros.len(),
                    snapshot.errors.len()
                );
                shell.state.snapshot = snapshot;
                // 页头「最后刷新」用：自动 / 手动刷新都算一次。
                shell.last_refresh = Some(Instant::now());
                // 没有错误了就收起横幅；否则上一次点开的「查看」会让下一次
                // 新错误**自动展开**，看起来像突然弹了一屏红字。
                if shell.state.snapshot.errors.is_empty() {
                    shell.errors_expanded = false;
                }

                // 30 秒一次的那部分：失败时**保留旧值**。
                // 一次瞬时失败不该让界面变成空白（那看起来像"数据丢了"）。
                if let Some(result) = status {
                    match result {
                        Ok(s) => {
                            shell.state.distro_status = Some(s);
                            shell.state.distro_status_error = None;
                        }
                        Err(e) => {
                            tracing::warn!("wsl --status 失败：{e}");
                            shell.state.distro_status_error = Some(e.to_string());
                        }
                    }
                }

                // 首次拿到 `wslc info` 之后才能确定 settings.yaml 的真实位置。
                // 只加载一次，避免把用户没保存的编辑覆盖掉。
                if shell.state.settings.is_none() {
                    shell.load_settings(cx);
                }

                // 本轮采集期间被挡下的刷新请求 → 立刻补跑。
                //
                // 放在 `cx.notify()` **之前**：补跑会把 `busy` 重新置起来，
                // 先通知的话会闪一帧「刷新」再变回「刷新中…」。
                if std::mem::take(&mut shell.refresh_again) {
                    shell.refresh(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 定时刷新（自动刷新用）。
    ///
    /// 与 [`Shell::refresh`] 的区别有两点：
    /// 1. **忙的时候直接跳过、不排队** —— 下一个周期自然会再来一次，堆着没有意义；
    /// 2. **静默** —— 不动顶部按钮的文案。3 秒一次的后台刷新不该让按钮
    ///    一直停在「刷新中…」上；用户想立刻刷新就自己点按钮。
    pub fn tick(&mut self, cx: &mut Context<Self>) {
        // 部署页那一列也要自己刷新 —— 见 [`Shell::tick_deploy_status`]。
        self.tick_deploy_status(cx);
        if self.state.busy {
            return;
        }
        self.refresh_quiet(cx);
    }

    /// 「部署管理」页的状态列**慢档**轮询（这一页不参与快照那套刷新）。
    ///
    /// # 为什么需要它（v0.9.13 补）
    ///
    /// 那一列的状态是**问容器**得来的（`probe_script`：pidfile + `/proc` 判活、
    /// 数文件），而这套快照刷新（`refresh_quiet` → `load_snapshot`）**不碰它**。
    /// 于是：坐着不动的时候，应用自己崩了、或者在「终端」里被 `kill` 掉了，
    /// 界面上会**一直显示「运行中」** —— 而页面顶部那句说明恰恰写着
    /// "「现在跑没跑」是**问容器**得来的"。这就是"两个地方对同一件事有两种说法"。
    ///
    /// # 为什么比快照慢一档
    ///
    /// 这个轮询**每一条部署一次 `wslc exec`**（8 条 = 8 个子进程），
    /// 而快照那一套是几条命令拿全量。跟着 3 秒一轮太重，所以走
    /// [`Self::deploy_status_every_n_ticks`]（默认约 9 秒）。
    ///
    /// ⚠️ 重探**不清旧值**（[`Shell::refresh_deploy_status_quiet`]）：
    /// 否则每 9 秒整列都会闪一下「读取中…」—— 那比"数据晚 9 秒"更难受。
    pub(crate) fn tick_deploy_status(&mut self, cx: &mut Context<Self>) {
        // 只有站在这一页上才值得问：别的页面根本看不到这一列。
        if self.state.page != Page::Deploy {
            return;
        }
        // 一条部署动作正在跑 / 正在读 → 别插一脚（进度文案和这一列会互相踩）。
        // 先把三个旗标取出来，`self.state.deploy` 的借用就在这里结束 ——
        // 下面要 `&mut self`（借用检查器看得见这个顺序）。
        let (action_running, busy, loaded) = match self.state.deploy.as_ref() {
            Some(deploy) => (deploy.progress.is_some(), deploy.busy, deploy.loaded),
            None => return,
        };
        if action_running || busy || !loaded {
            return;
        }
        self.deploy_tick = self.deploy_tick.wrapping_add(1);
        if self.deploy_tick % Self::deploy_status_every_n_ticks() != 0 {
            return;
        }
        self.refresh_deploy_status_quiet(cx);
    }

    /// 部署页状态列隔几轮问一次（一轮 = `prefs.refresh_secs`，默认 3 秒）。
    pub(crate) fn deploy_status_every_n_ticks() -> u32 {
        3
    }

    /// 按固定间隔自动刷新。
    ///
    /// 间隔由偏好（`prefs.refresh_secs`，默认 3 秒）决定，用户可以在"设置"页改；
    /// 每轮都重新读一次，所以改完立即生效，不需要重启。
    pub(crate) fn start_auto_refresh(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                let interval = match this.update(cx, |shell, _| shell.state.refresh_interval()) {
                    Ok(interval) => interval,
                    // 实体已销毁 → 退出循环，避免泄漏。
                    Err(_) => break,
                };

                cx.background_executor().timer(interval).await;

                if this.update(cx, |shell, cx| shell.tick(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    // -- 配置 --------------------------------------------------------------

    /// 请求执行一个**容器域**危险操作（先弹确认框）。
    pub fn request(&mut self, action: PendingAction, cx: &mut Context<Self>) {
        self.state.request_confirm(action);
        cx.notify();
    }

    /// 请求执行一个**发行版域**危险操作（先弹确认框）。
    pub fn request_distro(&mut self, action: DistroAction, cx: &mut Context<Self>) {
        self.state.request_distro_confirm(action);
        cx.notify();
    }

    /// 取消确认。
    pub fn cancel_pending(&mut self, cx: &mut Context<Self>) {
        self.state.cancel_confirm();
        cx.notify();
    }

    /// 这个危险操作会不会和正在跑的长任务打架？会的话给出拒绝的理由。
    ///
    /// 放在 [`Shell::confirm_pending`] 里而不是渲染层：渲染层决定"按钮画不画"
    /// 会随刷新闪来闪去（安装开始/结束都在变），而**点下去的那一刻**才是
    /// 真正要判的地方 —— 弹窗可能是几分钟前打开的，那期间安装可能已经开始了。
    pub(crate) fn blocking_reason(&self, action: &ConfirmAction) -> Option<String> {
        match action {
            // 「关停全部」压的是 `wsl --shutdown`，作用在所有发行版上，
            // 会把安装 / 导出正在跑的 `wsl.exe` 一起掐掉。
            // 名义上"关停发行版"不该影响正在安装的那个（它还没注册完），
            // 但它掐的是整个 WSL 服务 —— 实测里重定位那一步被打断
            // 等于把刚装好的删掉（见 `cmd::install` 的模块说明）。
            ConfirmAction::Distro(DistroAction::ShutdownAll) => self
                .state
                .long_task()
                .map(|what| format!("{what} —— 这时「关停全部」会把它一起掐掉，等它结束再动")),
            // 动某一个发行版：只在这个动作碰的正是那个正在被安装/导出的
            // 发行版时才拦（别的一律放行 —— 安装要几十分钟，把整页操作
            // 全锁住比这个 bug 更烦人）。`distro_name()` 对 `ShutdownAll`
            // 返回 `None`，上面那条已经处理过它了。
            ConfirmAction::Distro(other) => other.distro_name().and_then(|name| {
                self.state.busy_with(name).map(|what| {
                    format!("{name} 正在{what} —— 这时对它做别的写操作会把那一步弄坏，等它结束再动")
                })
            }),
            // 容器域和发行版安装互不相干：一个走 `wslc.exe`、一个走 `wsl.exe`，
            // 各自的发行版/会话也不重叠。拦下来只会平白挡住正常的容器操作。
            //
            // 「终止会话」也归在这里，理由同上：它终止的是 **wslc 的容器会话**，
            // 而安装 / 导出动的是 `wsl.exe` 注册的**发行版**（新实例的 `--import`
            // 会新建一个自己的发行版），两者不共享进程也不共享文件。
            // 唯一的交集是它们都要占 CPU/IO —— 那不该成为"禁止操作"的理由。
            ConfirmAction::Container(_) => None,
        }
    }

    /// 确认并执行。
    ///
    /// 两个域共用这一个入口：[`ConfirmAction::execute`] 内部按域分派到
    /// 各自的调用器（`wslc` / `wsl`）。
    ///
    /// ⚠️ **必须异步**：`wslc` / `wsl` 的调用都是同步等子进程的，
    /// 放在界面线程上会把窗口卡成"未响应"。容器那边的停止/删除还只是秒级，
    /// 发行版的压缩、移动是**分钟级** —— 同步跑的话界面能挂十分钟。
    pub fn confirm_pending(&mut self, cx: &mut Context<Self>) {
        let Some(action) = self.state.confirm.take() else {
            return;
        };

        // 确认框**无论如何都要关掉**（上面已经 take 走了）——
        // 拒绝执行时留一个"点了没反应又关不掉"的弹窗是最糟的结果。
        if let Some(reason) = self.blocking_reason(&action) {
            self.state.notify(Toast::error(reason));
            cx.notify();
            return;
        }

        // 发行版被删掉之后它的详情弹窗就没有对象了 —— 现在先判断好，
        // 待会儿 `action` 要移进异步块。
        let close_detail = match &action {
            ConfirmAction::Distro(DistroAction::Unregister { name, .. }) => {
                self.distro_detail.as_deref() == Some(name.as_str())
            }
            _ => false,
        };

        // 「删除备份」**分流**到它自己的执行函数。
        //
        // 它要做三件事（删本地文件 → 进容器 rm → 重扫目录写回 `AppState`），
        // 而 `PendingAction::execute` 的形状是"跑一条 `wslc` 命令 → 返回字符串"
        // —— 最后那件它表达不了（`&self`）。所以在这里把字段拆出来、交给
        // `delete_db_backup_confirmed`，别让它掉进下面那个通用分支。
        //
        // 确认框已经 `take()` 走了（上面），所以这里 return 不会留下残影。
        //
        // ⚠️ clippy 的 `redundant_pattern_matching` 会建议把这段改成
        // `matches!(…)` —— **不能改**：这里要**绑定四个字段**
        // （container / database / windows_path / container_path），
        // `matches!` 只能判真假、拿不到值。这是它的误报。
        #[allow(clippy::redundant_pattern_matching)]
        if let ConfirmAction::Container(PendingAction::DeleteDatabaseBackup {
            container,
            database,
            windows_path,
            container_path,
        }) = &action
        {
            let (container, database) = (container.clone(), database.clone());
            let (windows_path, container_path) = (windows_path.clone(), container_path.clone());
            self.delete_db_backup_confirmed(container, database, windows_path, container_path, cx);
            return;
        }

        // 删库 / 删用户之后要重读数据库页那两张表 —— 容器的列表刷新管不到它们。
        // 这里先算好（`action` 马上要移进异步块）。
        // 删库还要**同时清掉本地存档里的账号**：库都没了，列表上不能再出现
        // 一个连不上的用户名 / 密码（就算哪天重建同名库，密码也对不上了）。
        let dropped_database = match &action {
            ConfirmAction::Container(PendingAction::DropDatabase {
                container,
                database,
                ..
            }) => Some((container.clone(), database.clone())),
            _ => None,
        };
        let reload_db = matches!(
            action,
            ConfirmAction::Container(
                PendingAction::DropDatabase { .. } | PendingAction::DropUser { .. }
            )
        );

        // 点下去先给个回应。慢动作（压缩/移动）要等很久，
        // 没有这条的话用户只会觉得按钮坏了，然后去点第二次。
        let title = action.title();
        self.state.notify(Toast::info(format!("正在{title}…")));
        cx.notify();

        let wslc = self.state.wslc.clone();
        let wsl = self.state.wsl.clone();

        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { action.execute(&wslc, &wsl) })
                .await;

            let _ = this.update(cx, |shell, cx| {
                // `matches!(&result, Ok(_))` 就是 `.is_ok()`（clippy 的
                // `redundant_pattern_matching`）。用 `.is_ok()` 还有个好处：
                // 它不借用整个 `result`，下面的 `match result` 才能直接移动。
                let ok = result.is_ok();
                match result {
                    Ok(message) => shell.state.notify(Toast::success(message)),
                    Err(e) => shell
                        .state
                        .notify(Toast::error(format!("{title}失败：{e}"))),
                }

                // 删库成功：本地存档里的账号一起清（见 `dropped_database` 的注释）
                if ok {
                    if let Some((container, database)) = dropped_database {
                        shell.state.db_credentials.remove(&container, &database);
                        if let Some(e) = shell.state.save_db_credentials() {
                            shell.state.notify(Toast::error(e));
                        }
                    }
                }

                if close_detail {
                    shell.distro_detail = None;
                }

                // 删库 / 删用户：重读数据库页那两张表（成功失败都读一次 ——
                // 失败了也要让用户看到"它到底还在不在"）。
                if reload_db {
                    shell.reload_db_lists(cx);
                }

                // 立即刷新，让列表反映最新状态。
                shell.refresh(cx);
                cx.notify();
            });
        })
        .detach();
    }

    // -- 界面状态 ----------------------------------------------------------

    /// 切换页面（导航点击走这里）。
    ///
    /// `window` 是给**带控件的页面**准备的：「添加实例」有输入框和两个下拉框，
    /// 而 `InputState::new` / `SelectState::new` 都要 `&mut Window` —— 所以必须
    /// **在点击时**把表单建好。在渲染时建会每帧重建一次控件，字都打不进去。
    ///
    /// ⚠️ 「镜像」页**不需要** window：v0.6.3 把拉取表单挪进了弹窗，
    /// 它是在**点「拉取镜像…」那一刻**建的（`SelectState` 同理）。
    ///
    /// v0.9.22 起「容器」页也需要 window：列表上方的搜索框是 `InputState`，
    /// 只能在进入页面时建一次（见 [`Shell::ensure_container_search`]）。
    pub fn set_page(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        if page.needs_window_to_enter() {
            // 「添加实例」那一套：表单 + 两份清单的预取。
            //
            // ⚠️ 必须**按页判断**，不能只看 `needs_window_to_enter()`：
            // 「数据库管理」也是要 window 的页（连接卡有输入框），
            // 但它不需要装发行版的那份表单，更不该顺手去拉商店 / 镜像源清单。
            if page == Page::AddInstance {
                self.ensure_install_form(window, cx);

                // 进页面就把两份清单在后台拉起来：用户切到「微软商店」或
                // 「在线发行版（镜像源）」时列表**已经在了**，不用先点一次「刷新」。
                // 两个请求都很小（商店清单十几 KB、镜像清单 50 KB），
                // 而且各自的函数里会挡住重复请求。**不在这里同步拉**：那会卡住界面。
                if self.state.online.items.is_empty() && !self.state.online.loading {
                    self.refresh_online_list(window, cx);
                }
                if self.state.mirrors.offers.is_empty() && !self.state.mirrors.loading {
                    self.refresh_mirror_catalog(window, cx);
                } else if self.state.mirrors.chosen.is_none() && !self.state.mirrors.probing {
                    // 清单已经在手（上次进来拉过）、但还没测速过 —— 补一次。
                    // 否则"换个发行版页"回来会发现 URL 还是空的（校验会挡住提交）。
                    self.probe_mirrors(cx);
                }
            }
            // 「数据库管理」：连接卡那三个输入框（懒创建，幂等）
            if page == Page::Database {
                self.ensure_db_form(window, cx);
            }
            // 「SQL 控制台」（v0.12）：整页的工作区。它复用 `DbPageForm`
            // （编辑器那一格住在里面），外加树自己的搜索框。
            if page == Page::Sql {
                self.ensure_db_form(window, cx);
                self.ensure_schema_search(window, cx);
                // 树只在**还没读过**时才自动读一次。
                //
                // 每次进页面都读一遍会白花一次 `wslc exec`（约 70ms），
                // 而用户大部分时候只是在几条查询之间来回。要刷新表结构
                // 就点页面上的「刷新表结构」——那是明确的动作。
                let need_tree = self
                    .state
                    .db
                    .as_ref()
                    .is_some_and(|db| db.schema_tree.is_empty());
                if need_tree {
                    self.load_schema_tree(cx);
                } else {
                    // 数据已经有了，但树控件的 items 可能还没灌过（第一次进这一页，
                    // 或者上次是在"有搜索关键字"的状态下离开的）—— 补一次。
                    let query = self
                        .schema_search
                        .as_ref()
                        .map(|i| i.read(cx).value().to_string())
                        .unwrap_or_default();
                    self.rebuild_schema_tree(&query, cx);
                }
            }
            // 「容器」：搜索框是 `InputState`，只能在进入页面这一刻建。
            if page == Page::Containers {
                self.ensure_container_search(window, cx);
            }
        }
        self.goto_page(page, cx);
    }

    /// 不碰表单的页面切换 —— 给**没有 window** 的场合用。
    ///
    /// 目前只有"容器创建完成后跳到「容器」页"这一处。
    pub(crate) fn goto_page(&mut self, page: Page, cx: &mut Context<Self>) {
        if self.state.page != page {
            self.state.page = page;
            cx.notify();
        }
    }

    // -- 提示条 ------------------------------------------------------------

    /// 让提示条**自己消失**的小循环（每 500ms 看一眼）。
    ///
    /// # 为什么是一个常驻循环，而不是"弹提示时起一个定时器"
    ///
    /// `AppState::notify` 住在 `wslc-panel-core` 里，那一层**没有执行器**、
    /// 起不了定时器。换成"在每个 notify 的调用点各起一个"的话：全仓库 60 多处
    /// 都得改，而且旧定时器还得有办法认出"现在这条提示已经不是我那条了"
    /// （提示条是单条的 `Option<Toast>`，后弹的会覆盖先弹的）。
    ///
    /// 一个循环只在一个地方，而且天然覆盖"以后新加的 notify"。
    ///
    /// # 代价
    ///
    /// 每 500ms 醒一次、读一个 `Option<Toast>`（外加一次 `Instant::elapsed`）。
    /// **只有真的收掉时才 `cx.notify()`** —— 否则界面会每半秒重绘一遍，
    /// 而 GPUI 每一帧都要跑布局 + 光栅化（`Cargo.toml` 里那段
    /// `profile.dev` 的注释说的就是这件事）。
    ///
    /// 500ms 这个粒度：提示条本来就活 4~8 秒，晚半秒收掉没人看得出来；
    /// 反过来做成一帧一查没有意义。
    pub(crate) fn start_toast_reaper(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(500))
                    .await;
                let alive = this.update(cx, |shell, cx| {
                    if shell.state.clear_expired_toast() {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    // 实体已销毁 → 退出循环，避免泄漏（和 `start_auto_refresh`
                    // 同一个写法）。
                    break;
                }
            }
        })
        .detach();
    }

    /// 关闭提示条。
    pub fn dismiss_toast(&mut self, cx: &mut Context<Self>) {
        self.state.toast = None;
        cx.notify();
    }
}
