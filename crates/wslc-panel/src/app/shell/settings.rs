//! 应用设置：偏好读写、主题、终端起点。

use super::super::prelude::*;
use super::Shell;
use super::core::reveal_with_explorer;

impl Shell {
    /// 加载 `settings.yaml`（优先使用 `wslc info` 报告的路径）。
    pub fn load_settings(&mut self, cx: &mut Context<Self>) {
        let info = self.state.snapshot.info.clone();
        match state::load_settings(info.as_ref()) {
            Ok(doc) => {
                self.state.settings = Some(doc);
                self.state.settings_error = None;
            }
            Err(e) => {
                self.state.settings = None;
                self.state.settings_error = Some(e);
            }
        }
        cx.notify();
    }

    /// 用户主动重新加载（会丢弃未保存的修改）。
    pub fn reload_settings(&mut self, cx: &mut Context<Self>) {
        self.load_settings(cx);
        self.state.notify(Toast::info("已重新加载 settings.yaml"));
        cx.notify();
    }

    /// 修改一项配置（只改内存中的文档，需要保存才落盘）。
    pub fn set_setting(
        &mut self,
        key: &'static SettingKey,
        value: Option<&'static str>,
        cx: &mut Context<Self>,
    ) {
        let Some(doc) = self.state.settings.as_mut() else {
            self.state.notify(Toast::error("配置文件尚未加载"));
            cx.notify();
            return;
        };

        if doc.set(key.section, key.key, value) {
            let shown = value.unwrap_or("默认值");
            self.state
                .notify(Toast::info(format!("{} → {shown}（记得保存）", key.label)));
        }
        cx.notify();
    }

    /// 备份并保存 `settings.yaml`。
    pub fn save_settings(&mut self, cx: &mut Context<Self>) {
        let result = match self.state.settings.as_mut() {
            Some(doc) => doc.save_with_backup().map_err(|e| e.to_string()),
            None => {
                self.state.notify(Toast::error("配置文件尚未加载"));
                cx.notify();
                return;
            }
        };

        let toast = match result {
            Ok(Some(backup)) => Toast::success(format!(
                "已保存，备份：{}",
                backup
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            )),
            Ok(None) => Toast::success("已保存"),
            Err(e) => Toast::error(format!("保存失败：{e}")),
        };
        self.state.notify(toast);
        cx.notify();
    }

    /// 用系统默认编辑器打开配置文件（`wslc settings`）。
    pub fn open_settings_in_editor(&mut self, cx: &mut Context<Self>) {
        let wslc = self.state.wslc.clone();
        let toast = match wslc_core::cmd::system::open_settings_in_editor(&wslc) {
            Ok(()) => Toast::success("已用系统默认编辑器打开 settings.yaml"),
            Err(e) => Toast::error(format!("打开失败：{e}")),
        };
        self.state.notify(toast);
        cx.notify();
    }

    /// 用系统默认程序打开 `%USERPROFILE%\.wslconfig`。
    ///
    /// # 为什么走 `explorer.exe`
    ///
    /// 它按**文件关联**打开，用户装了 VS Code / Notepad++ 就会用那个 ——
    /// 比我们写死 `notepad.exe` 尊重用户的习惯。项目里"在资源管理器里打开"
    /// 也是这个路子。
    ///
    /// **只 `spawn` 不等待**：等编辑器关掉会把界面挂住（这正是 v0.3.4 修的那类问题）。
    pub fn open_wslconfig(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.state.snapshot.wslconfig.path.clone() else {
            self.state.notify(Toast::error(
                "找不到 .wslconfig 的位置（USERPROFILE 没定义？）",
            ));
            cx.notify();
            return;
        };

        // 文件不存在时**先建一个空的**：不然 explorer 会弹一个系统级的
        // "找不到文件"框，看起来像我们的错。建个空文件正是"从头配一份"的起点。
        let created = !path.exists();
        if created {
            if let Err(e) = std::fs::write(&path, "") {
                self.state
                    .notify(Toast::error(format!("创建 .wslconfig 失败：{e}")));
                cx.notify();
                return;
            }
        }

        let toast = match reveal_with_explorer(&path, "系统默认程序") {
            Ok(()) if created => Toast::success("已创建空的 .wslconfig 并用系统默认程序打开"),
            Ok(()) => Toast::success("已用系统默认程序打开 .wslconfig"),
            Err(e) => Toast::error(format!("打开失败：{e}")),
        };
        self.state.notify(toast);
        cx.notify();
    }

    // -- 危险操作确认 ------------------------------------------------------

    /// 切换「应用设置」页里的 tab。
    pub fn set_settings_tab(&mut self, tab: state::SettingsTab, cx: &mut Context<Self>) {
        self.state.settings_tab = tab;
        cx.notify();
    }
    /// 设置自动刷新间隔（秒），并立即写入偏好文件。
    ///
    /// 自动刷新循环每轮都会重新读 `state.prefs`，所以改完下一轮就生效，
    /// 不需要重启，也不需要通知循环。
    pub fn set_refresh_secs(&mut self, secs: u64, cx: &mut Context<Self>) {
        // ⚠️ `Prefs` 是要整体写回磁盘的：用 `..` 带上其余字段
        // （以前是手抄 refresh_secs/theme/install_dir —— 加第 4 个字段
        // 「终端起始目录」时那三行就会漏掉它，用户设过的终端起点被一次
        // 刷新间隔改动悄悄抹掉）。
        let prefs = wslc_panel_core::prefs::Prefs {
            refresh_secs: secs,
            ..self.state.prefs.clone()
        }
        .normalized();
        if self.state.prefs == prefs {
            return;
        }
        let secs = prefs.refresh_secs;
        self.state.prefs = prefs;

        match self.state.prefs.save() {
            Ok(()) => {
                tracing::info!("自动刷新间隔已改为 {secs} 秒");
                self.state
                    .notify(Toast::success(format!("刷新间隔已改为 {secs} 秒")));
            }
            Err(e) => {
                tracing::warn!("保存偏好失败：{e}");
                // 内存里的值仍然生效，只是重启后会丢。
                self.state
                    .notify(Toast::error(format!("已改为 {secs} 秒，但保存失败：{e}")));
            }
        }
        cx.notify();
    }

    // -- 新终端的起始目录 ----------------------------------------------------

    /// 设置「新终端的起始目录」，并立即写入偏好文件。
    ///
    /// 只影响**之后打开**的终端：已经在跑的那些窗口的 cwd 是它们自己的事，
    /// 这里改不了（界面上也是这么写的）。
    pub fn set_terminal_dir(
        &mut self,
        dir: wslc_panel_core::prefs::TerminalStartDir,
        cx: &mut Context<Self>,
    ) {
        let prefs = wslc_panel_core::prefs::Prefs {
            terminal_dir: dir,
            ..self.state.prefs.clone()
        };
        if self.state.prefs == prefs {
            return;
        }
        let described = prefs.terminal_dir.describe();
        self.state.prefs = prefs;

        match self.state.prefs.save() {
            Ok(()) => {
                tracing::info!("新终端的起始目录已改为 {described}");
                self.state
                    .notify(Toast::success(format!("新终端将从「{described}」开始")));
            }
            Err(e) => {
                tracing::warn!("保存偏好失败：{e}");
                // 内存里的值仍然生效，只是重启后会丢。
                self.state.notify(Toast::error(format!(
                    "已改为「{described}」，但保存失败：{e}"
                )));
            }
        }
        cx.notify();
    }

    /// 打开「自定义起始目录」的输入框（幂等）。
    ///
    /// **必须**在有 `&mut Window` 的地方调用 —— `InputState::new` 要它
    /// （`AGENTS.md` §7.5：渲染时现建输入框会让用户一个字都打不进去）。
    ///
    /// 建完之后**一直留着**，再把焦点给过去：用户点完按钮就能直接敲。
    pub fn edit_terminal_dir(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.forms.terminal_dir_input.is_none() {
            // 预填当前的自定义路径（不是自定义档位时留空，走占位符提示格式）。
            let current = match &self.state.prefs.terminal_dir {
                wslc_panel_core::prefs::TerminalStartDir::Custom(dir) => dir.clone(),
                _ => String::new(),
            };
            let input =
                cx.new(|cx| InputState::new(window, cx).placeholder("/home/wt/project 或 ~"));
            // `set_value` 也要 `&mut Window`，所以只能在这儿做。
            input.update(cx, |state, cx| state.set_value(current, window, cx));
            self.forms.terminal_dir_input = Some(input);
        }

        // `InputState::focus` 是 `pub(crate)`，外部只能走 `focus_handle`。
        if let Some(input) = self.forms.terminal_dir_input.clone() {
            let handle = input.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }
        cx.notify();
    }

    /// 把输入框里的路径应用成「自定义起始目录」。
    ///
    /// 校验是纯函数（[`wslc_panel_core::prefs::parse_custom_dir`]），所以"报错的红字"
    /// 和"真的存下去的值"用的是同一套规则。校验不过就**只提示、不改偏好** ——
    /// 把非法值存进去，用户下次点「打开终端」会得到一个落点随机的窗口
    /// （`wsl.exe` 对不存在的 `--cd` 不报错，只落在 `/`）。
    pub fn apply_terminal_dir(&mut self, cx: &mut Context<Self>) {
        let Some(input) = self.forms.terminal_dir_input.clone() else {
            return;
        };
        let typed = input.read(cx).value().trim().to_owned();
        match wslc_panel_core::prefs::parse_custom_dir(&typed) {
            Ok(dir) => {
                self.set_terminal_dir(wslc_panel_core::prefs::TerminalStartDir::Custom(dir), cx)
            }
            Err(e) => {
                self.state.notify(Toast::error(e));
                cx.notify();
            }
        }
    }

    /// 切主题：**持久化 + 两套配色一起换**。
    ///
    /// 两套配色指：
    ///
    /// 1. gpui-component 自己的主题 —— `Input` / `Button` / `Select` 这些控件
    ///    只认它（不切的话输入框还是白底浅灰字，实机看就是"字打了但读不出来"）；
    /// 2. 我们自己画的那些 `div` —— 走 [`crate::theme::set_mode`] 那个全局。
    ///
    /// 「跟随系统」用**缓存**的系统主题立刻切一下（点下去立刻有反应），
    /// 同时在后台再读一次：用户很可能刚在系统设置里改过主题才来点这个按钮。
    /// **不在点击回调里同步读**（起 `reg.exe`，几十毫秒，但那是界面线程 ——
    /// `AGENTS.md` §7.1）。
    pub fn set_theme(&mut self, theme: wslc_panel_core::prefs::ThemePref, cx: &mut Context<Self>) {
        let changed = self.state.prefs.theme != theme;
        self.state.prefs.theme = theme;

        if let Some(is_dark) = theme.fixed_is_dark() {
            self.apply_theme(is_dark, cx);
            if changed {
                self.persist_theme(theme, cx);
            }
            return;
        }

        // 跟随系统
        let cached = self.state.system_is_dark.unwrap_or(true);
        self.apply_theme(theme.resolve_is_dark(cached), cx);
        if changed {
            self.persist_theme(theme, cx);
        }

        cx.spawn(async move |this, cx| {
            let probed = cx
                .background_executor()
                .spawn(async { wslc_core::cmd::win_theme::system_is_dark() })
                .await;
            let _ = this.update(cx, |shell, cx| {
                shell.state.system_is_dark = probed;
                // 探到的和缓存不一致 → 现在就跟上（用户多半刚改过系统主题）
                if let Some(is_dark) = probed {
                    if shell.state.prefs.theme.fixed_is_dark().is_none() {
                        shell.apply_theme(is_dark, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 真正落地一次主题切换（两套配色）。
    pub(crate) fn apply_theme(&mut self, is_dark: bool, cx: &mut Context<Self>) {
        // `theme::apply` 既设我们自己的全局模式，又给出组件库要的那个模式 ——
        // 两套配色只能一起切，写在一处才不会漏。
        crate::theme::apply(is_dark, cx);
        cx.notify();
    }

    /// 写回偏好文件（+ 提示）。
    pub(crate) fn persist_theme(
        &mut self,
        theme: wslc_panel_core::prefs::ThemePref,
        cx: &mut Context<Self>,
    ) {
        match self.state.prefs.save() {
            Ok(()) => {
                tracing::info!("主题已改为{}", theme.label());
                self.state
                    .notify(Toast::success(format!("主题已改为{}", theme.label())));
            }
            Err(e) => {
                tracing::warn!("保存偏好失败：{e}");
                // 内存里的值仍然生效，只是重启后会丢。
                self.state.notify(Toast::error(format!(
                    "已改为{}，但保存失败：{e}",
                    theme.label()
                )));
            }
        }
        cx.notify();
    }
}
