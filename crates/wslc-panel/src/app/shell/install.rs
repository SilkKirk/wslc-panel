//! 添加实例域：来源切换、商店 / 镜像源清单、安装与日志。

use super::super::prelude::*;
use super::Shell;
use super::core::{dir_non_empty, lock};
use super::state::{AfterPick, DropdownItem, InstallRun};

impl Shell {
    /// 给「添加实例」页的**来源文件**输入框弹选择器。
    ///
    /// 选完做三件事（而不只是填个路径）：
    ///
    /// 1. 填路径；
    /// 2. 从**文件名**猜一个发行版名（`ubuntu-rootfs-amd64.tar.gz` → `ubuntu`）——
    ///    用户不必自己敲一遍；
    /// 3. 让安装目录跟着新名字走（见 [`Shell::resync_install_dir`]）。
    ///
    /// 后缀表按来源给：选"从 VHDX 导入"时不该看见一堆 `.tar`。
    pub fn browse_install_path(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((label, extensions)) = self.forms.install_form.as_ref().and_then(|form| {
            let (label, extensions) = form.source.file_filter()?;
            Some((label, extensions))
        }) else {
            return;
        };

        if self.tasks.picking {
            self.state
                .notify(Toast::info("已经有一个选择框开着了，先处理它"));
            cx.notify();
            return;
        }
        self.tasks.picking = true;
        cx.notify();

        cx.spawn_in(window, async move |this, cx| {
            let picked = cx
                .background_executor()
                .spawn(async move { wslc_core::cmd::picker::pick_file(label, extensions) })
                .await;

            let _ = this.update_in(cx, |shell, window, cx| {
                shell.tasks.picking = false;

                match picked {
                    Ok(Some(path)) => {
                        // 先只做"填输入框"这类需要 `&mut Window` 的事，
                        // 借用在下面那个块结束时自然放开，才能再 `&mut shell`。
                        let suggested = {
                            let Some(form) = shell.forms.install_form.as_ref() else {
                                cx.notify();
                                return;
                            };
                            form.source_path
                                .update(cx, |state, cx| state.set_value(path.clone(), window, cx));

                            let suggested = suggest_name_from_file(&path);
                            if !suggested.is_empty() {
                                form.name.update(cx, |state, cx| {
                                    state.set_value(suggested.clone(), window, cx)
                                });
                            }
                            suggested
                        };

                        if !suggested.is_empty() {
                            shell.resync_install_dir(window, cx);
                        }
                    }
                    // 用户点了取消 —— 这不是错误，什么都不做
                    Ok(None) => {}
                    Err(e) => shell
                        .state
                        .notify(Toast::error(format!("打开选择框失败：{e}"))),
                }
                cx.notify();
            });
        })
        .detach();
    }

    // -- 发行版配置（/etc/wsl.conf）-----------------------------------------

    /// 切换安装来源。
    ///
    /// 顺带把**新来源用不到的选项复位** —— 否则用户在"在线安装"里勾了
    /// "装完启动"，再切到"从 tar 导入"，那个勾还留着但界面上看不见，
    /// 一旦切回去又冒出来，很像 bug。
    pub fn set_install_source(&mut self, source: InstallSourceKind, cx: &mut Context<Self>) {
        let Some(form) = self.forms.install_form.as_mut() else {
            return;
        };
        if form.source == source {
            return;
        }

        form.source = source;
        // 换了来源，上一次的标红已经不对应新的输入了；下一次提交再重新算。
        form.show_errors = false;
        form.field_errors = InstallFieldErrors::default();
        if !source.supports_launch() {
            form.launch = false;
        }
        // 切到镜像源就顺手开始测速（清单已经在手、还没测过的话）——
        // 用户不用先点「刷新清单」，也不用等下一次进页面。
        if source.is_mirror()
            && self.state.mirrors.chosen.is_none()
            && !self.state.mirrors.probing
            && !self.state.mirrors.offers.is_empty()
        {
            self.probe_mirrors(cx);
        }
        cx.notify();
    }

    /// 「微软商店」装不上时的一键出口：换成镜像源，并尽量选中同一个发行版。
    ///
    /// # 为什么要有这个
    ///
    /// 商店那条路**依赖 `wsl.exe` 自己的网络**：实测这台机器上
    /// `wsl -l -o` 被 `raw.githubusercontent.com` 拦掉（500）时，商店与
    /// `--web-download` 会双双报 `Wsl/InstallDistro/WININET_E_CANNOT_CONNECT`，
    /// 而镜像源是**面板自己**用 `curl.exe` 从国内镜像站下载（同一时刻清华 200），
    /// 完全不经过 wsl 的网络。同一个发行版往往在镜像清单里也有
    /// （Ubuntu / Debian / Alpine / Arch / Fedora / Rocky / Kali / …），
    /// 所以直接帮他切过去、并选中对应的那一版，比让他自己去翻清单强。
    pub fn switch_to_mirror_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 在线清单里选过的那个 id（没选过就用输入框里的名字）
        let online_id = self
            .forms
            .install_form
            .as_ref()
            .map(|form| {
                form.online_id
                    .clone()
                    .unwrap_or_else(|| form.name.read(cx).value().trim().to_owned())
            })
            .unwrap_or_default();

        let matched = self.state.mirrors.match_online_id(&online_id);
        if let Some(offer) = matched.clone() {
            self.state.mirrors.select_distro(offer);
        }
        self.set_install_source(InstallSourceKind::Mirror, cx);
        // 下拉框也要跟着选中（否则它显示的还是上一条），然后**自动测速** ——
        // 和用户自己在下拉框里选一遍是同一条路。
        self.fill_mirror_select(window, cx);
        self.probe_mirrors(cx);

        match matched {
            Some(offer) => self.state.notify(Toast::success(format!(
                "已切到镜像源，并选中了「{offer}」—— 正在自动测速挑最快的镜像"
            ))),
            None => self.state.notify(Toast::info(
                "已切到镜像源 —— 在清单里挑一个版本，或填自定义下载地址",
            )),
        }
        cx.notify();
    }

    /// 设置「装完立即启动」。
    ///
    /// 收的是**目标值**而不是自己取反：`Switch` 是受控组件，回调里给的就是
    /// 用户请求的新状态。这里再 `!form.launch` 的话，渲染与点击之间只要有一次
    /// 时序差（比如连续快速切换），界面就会和状态对不上。
    pub fn set_install_launch(&mut self, on: bool, cx: &mut Context<Self>) {
        if let Some(form) = self.forms.install_form.as_mut() {
            form.launch = on;
            cx.notify();
        }
    }

    /// 设置「装完设为默认发行版」。理由同上：收目标值，不自己取反。
    pub fn set_install_default(&mut self, on: bool, cx: &mut Context<Self>) {
        if let Some(form) = self.forms.install_form.as_mut() {
            form.set_default = on;
            cx.notify();
        }
    }

    /// 设置在线安装的下载路径（`true` = 走 `--web-download`）。
    ///
    /// 同样收目标值：这个开关在界面上是一句话「从 GitHub 下载」，
    /// 语义是"要不要用 GitHub 那条路"，不是"切换一下"。
    pub fn set_web_download(&mut self, on: bool, cx: &mut Context<Self>) {
        if let Some(form) = self.forms.install_form.as_mut() {
            form.web_download = on;
            cx.notify();
        }
    }

    /// 把当前安装目录记成"新实例默认安装目录"。
    ///
    /// 刻意**不做成设置页里的输入框**：那需要给设置页也引入"懒创建输入框"
    /// 那一套机制（见 [`InstallForm`] 的说明）。用户在安装页填好一个路径、
    /// 顺手点一下"存为默认"，是更短的路径。
    pub fn remember_install_dir(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.forms.install_form.as_ref() else {
            return;
        };
        let dir = form.install_dir.read(cx).value().trim().to_owned();

        if dir.is_empty() {
            self.state
                .notify(Toast::error("先在「安装目录」里填一个绝对路径"));
            cx.notify();
            return;
        }
        if !install_model::is_absolute_windows_path(&dir) {
            self.state.notify(Toast::error(format!(
                "要记也得是绝对路径（如 D:\\wsl）：{dir}"
            )));
            cx.notify();
            return;
        }

        // ⚠️ `Prefs` 是**整体**写回磁盘的：这里必须带上其余字段，
        // 只填 install_dir 会把它们重置掉。用 `..` 而不是手抄字段 ——
        // 手抄的那份在加第 4 个字段时一定会漏（终端起始目录就是第 4 个）。
        let prefs = wslc_panel_core::prefs::Prefs {
            install_dir: Some(dir.clone()),
            ..self.state.prefs.clone()
        };
        self.state.prefs = prefs;
        match self.state.prefs.save() {
            Ok(()) => {
                tracing::info!("默认安装目录已记为 {dir}");
                self.state
                    .notify(Toast::success(format!("已记住：新实例默认装到 {dir}")));
            }
            Err(e) => {
                // 内存里的值仍然生效，只是重启后会丢
                self.state
                    .notify(Toast::error(format!("已记住 {dir}，但写盘失败：{e}")));
            }
        }
        cx.notify();
    }

    /// 选一个目录填进「安装目录」。
    ///
    /// 选完之后要把 [`InstallForm::last_derived_dir`] 清掉：那之后这个名字联动
    /// 就不能再动它了 —— 用户亲手选的目录被我们改掉是最恼人的一类 bug。
    ///
    /// ⚠️ 但**只能真的选到东西时**才清，所以这件事交给 `spawn_picker`
    /// 在选中的分支里做（见 [`AfterPick::InstallDirIsNowUserChosen`]）。
    /// 写在这一行之后的话，"点了取消"也会清 —— 用户什么都没选，
    /// 却永久失去了名字联动。
    pub fn browse_install_dir(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(input) = self
            .forms
            .install_form
            .as_ref()
            .map(|form| form.install_dir.clone())
        else {
            return;
        };
        self.spawn_picker(
            &input,
            true,
            "目录",
            &[],
            "选择安装目录",
            AfterPick::InstallDirIsNowUserChosen,
            window,
            cx,
        );
    }

    // -- 添加实例：在线清单与镜像站 ----------------------------------------

    /// 拉在线可安装发行版的清单（微软商店那条路）。
    ///
    /// 先问 `wsl --list --online`，拉不到就自己拉微软那份
    /// `DistributionInfo.json`（本机 `raw.githubusercontent.com` 不通，
    /// 所以兜底那条路是**常态**，见 `cmd::distro::online_distros`）。
    ///
    /// 拉回来要**填进下拉框**，而 `SelectState::set_items` 要 `&mut Window`
    /// —— 所以这里走 `spawn_in` + `update_in`（和文件选择器同一条路，
    /// 见 `AGENTS.md` §7.2）。
    pub fn refresh_online_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.online.loading {
            return;
        }
        self.state.online.loading = true;
        self.state.online.error.clear();
        cx.notify();

        let wsl = self.state.wsl.clone();
        cx.spawn_in(window, async move |this, cx| {
            let listing = cx
                .background_executor()
                .spawn(async move { wslc_core::cmd::distro::online_distros(&wsl) })
                .await;

            let _ = this.update_in(cx, |shell, window, cx| {
                shell.state.online.loading = false;
                shell.state.online.source = listing.source;
                if listing.items.is_empty() {
                    shell.state.online.clear();
                    shell.state.online.error = if listing.error.trim().is_empty() {
                        "没有拿到任何在线发行版".to_owned()
                    } else {
                        listing.error
                    };
                } else {
                    shell.state.online.error.clear();
                    shell.state.online.items = listing.items;
                }
                shell.fill_store_select(window, cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// 把在线清单填进「微软商店」的下拉框。
    ///
    /// 显示**友好名 + id**：`Ubuntu 24.04 LTS (Ubuntu-24.04)`。两个都要写 ——
    /// 用户以为自己装的是 "Ubuntu 24.04 LTS"，而装完列表里出现的是
    /// `Ubuntu-24.04`（那才是 `wsl --install -d` 认的名字），
    /// 提前写在选项里比事后解释强。
    pub(crate) fn fill_store_select(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = self.forms.install_form.as_ref() else {
            return;
        };
        let select = form.store_select.clone();
        // `set_items` 会把选中项清掉，而用户没做错什么 —— 换清单之后
        // 要把原来选的那个**重新选上**（它还在新清单里的话）。
        let keep = form.online_id.clone();

        let items: Vec<DropdownItem> = self
            .state
            .online
            .items
            .iter()
            .map(|item| {
                let label = if item.label == item.id {
                    item.id.clone()
                } else {
                    format!("{} ({})", item.label, item.id)
                };
                DropdownItem::new(label, item.id.clone())
            })
            .collect();

        select.update(cx, |state, cx| {
            state.set_items(items, window, cx);
            if let Some(id) = keep {
                state.set_selected_value(&id, window, cx);
            }
        });
    }

    /// 选中在线清单里的一项。
    ///
    /// # 名字默认填**清单里的 id**，而不是友好名
    ///
    /// 参考实现填的是友好名（`Ubuntu 24.04 LTS` → `Ubuntu-24-04-LTS`）。
    /// 那样每次在线安装都会走"导出 → 注销 → 导入"的重定位（多拷几个 GB），
    /// 因为它和 `wsl --install -d` 认的 id 对不上。
    /// 我们的默认值是 id（`Ubuntu-24.04`，本身就是合法名字），于是**不需要改名**；
    /// 位置由装完之后那一步 `--manage <id> --move <目录>` 补正
    /// （在线安装**永远不带 `--location`**，理由见 `model::install::plan` 里那张表）。
    /// 用户想改名随时能改，界面上也会提示那会多一次全量拷贝。
    pub fn pick_online_distro(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(form) = self.forms.install_form.as_mut() {
            // 只在"名字还是我们填的"时候动它：空着、或者还是上一次选中的那个 id。
            // 用户自己敲过名字就不该被覆盖（和 `last_derived_dir` 是同一条原则）。
            let current = form.name.read(cx).value().trim().to_owned();
            let ours = current.is_empty() || Some(current.as_str()) == form.online_id.as_deref();
            form.online_id = Some(id.clone());
            if ours {
                form.name
                    .update(cx, |state, cx| state.set_value(id.clone(), window, cx));
            }
        }
        self.state.online.select(id);
        self.resync_install_dir(window, cx);
        cx.notify();
    }

    /// 拉「在线发行版（镜像源）」的清单。
    ///
    /// 两个 HTTP 请求（先问清单地址、再取清单）走 `curl.exe`，**必须在后台**：
    /// 最长 25 秒 × 2，放界面上就是"点进去卡半分钟"（`AGENTS.md` §7.1）。
    ///
    /// 清单到手之后**自动开始测速**（见 [`Shell::probe_mirrors`]）：
    /// 参考实现是"选中即测速择优"，没有"探测"按钮 —— 而我们要保证
    /// 用户点「创建安装」时 URL 已经定了（预览里的命令必须就是真正要跑的）。
    pub fn refresh_mirror_catalog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.mirrors.loading {
            return;
        }
        self.state.mirrors.loading = true;
        self.state.mirrors.load_error = None;
        cx.notify();

        cx.spawn_in(window, async move |this, cx| {
            let fetched = cx
                .background_executor()
                .spawn(async { wslc_core::cmd::catalog::fetch() })
                .await;

            let _ = this.update_in(cx, |shell, window, cx| {
                shell.state.mirrors.loading = false;
                match fetched {
                    Ok(catalog) => {
                        let count = catalog.offers.len();
                        shell.state.mirrors.set_offers(
                            catalog.offers,
                            catalog.origin.label().to_owned(),
                            catalog.updated,
                        );
                        shell.fill_mirror_select(window, cx);
                        shell
                            .state
                            .notify(Toast::success(format!("拿到 {count} 个可装的发行版")));
                        shell.probe_mirrors(cx);
                    }
                    Err(e) => {
                        // 清单拉不到时**留着旧清单**：旧的还能用，总比空的强
                        shell.state.mirrors.load_error = Some(e.clone());
                        shell.state.notify(Toast::error(e));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 把镜像清单填进「在线发行版」的下拉框。
    ///
    /// 显示的是参考实现的写法：`Ubuntu 24.04 (12 镜像源)` —— **只给数量**，
    /// 不列镜像站名（装的时候会自动测速挑最快的，用户不需要先看一遍）。
    pub(crate) fn fill_mirror_select(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = self.forms.install_form.as_ref() else {
            return;
        };
        let select = form.mirror_select.clone();
        let keep = self.state.mirrors.selected_offer().map(|offer| offer.id());

        let items: Vec<DropdownItem> = self
            .state
            .mirrors
            .offers
            .iter()
            .map(|offer| DropdownItem::new(offer.menu_label(), offer.id()))
            .collect();

        select.update(cx, |state, cx| {
            state.set_items(items, window, cx);
            if let Some(id) = keep {
                state.set_selected_value(&id, window, cx);
            }
        });
    }

    /// 切换镜像源那块选中的发行版 —— 换完**立刻**重新测速。
    ///
    /// 参考实现就是"选中即测速择优"（它在下拉框旁边写着「自动测速择优」），
    /// 用户不用再点一次"探测"。测速是并发的（见 [`Shell::probe_mirrors`]），
    /// 最多也就几秒。
    pub fn select_mirror_distro(&mut self, id: String, cx: &mut Context<Self>) {
        self.state.mirrors.select_distro(id);
        self.probe_mirrors(cx);
        cx.notify();
    }

    /// 自动测速：**并发** HEAD 一遍候选，挑最快的那个。
    ///
    /// # 为什么并发
    ///
    /// 一条发行版最多十几个候选、每个 8 秒上限 —— 串行最坏是一分半，
    /// 而这是"选中发行版就自动开始"的动作（用户没得选，只能干等）。
    /// 参考实现也是并发（`futures_util::future::join_all`）。
    ///
    /// 这里用 `std::thread::scope`：探测本身是**阻塞**的 curl 调用，
    /// 线程是最自然的载体，而 scope 保证返回前全部 join 掉
    /// （不用管线程生命周期，也不需要线程池）。
    ///
    /// # 结果只用来挑一个
    ///
    /// 镜像站列表**不再显示**（用户要求"不需要那些什么清华源之类的"）：
    /// 界面只留一行状态，名字进安装日志 —— 出问题时那才是排查的地方。
    /// 成功也**不发提示条**：这个是自动触发的，换一次发行版弹一次太吵。
    pub fn probe_mirrors(&mut self, cx: &mut Context<Self>) {
        if self.state.mirrors.probing {
            return;
        }
        // 没有清单（还没拉 / 拉失败）时**安静地什么都不做**：
        // 这个函数现在是自动触发的，弹一句"先拉清单"只会莫名其妙。
        let Some(offer) = self.state.mirrors.selected_offer() else {
            return;
        };

        let candidates = offer.candidates();
        let release = offer.version.clone();

        self.state.mirrors.probing = true;
        self.state.mirrors.error = None;
        self.state.mirrors.results.clear();
        self.state.mirrors.chosen = None;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let probed = cx
                .background_executor()
                .spawn(async move {
                    let probes: Vec<Option<mirrors::Probe>> = std::thread::scope(|scope| {
                        let handles: Vec<_> = candidates
                            .iter()
                            .map(|candidate| {
                                scope.spawn(move || {
                                    // 纵深防御：`candidate.url` 来自远端清单，而它会作为
                                    // **裸 argv** 交给 curl（见 `mirrors::is_http_url`）。
                                    // `parse_catalog` 已经滤过一遍，界面这条探测路径是
                                    // 独立拼的，再挡一次。
                                    if !mirrors::is_http_url(&candidate.url) {
                                        return None;
                                    }
                                    let args = mirrors::probe_args(&candidate.url);
                                    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
                                    wslc_core::cli::run_helper_with_hint(
                                        "curl.exe",
                                        "curl.exe",
                                        wslc_core::cli::CURL_NOT_FOUND_HINT,
                                        &refs,
                                        Duration::from_secs(20),
                                    )
                                    .ok()
                                    .and_then(|out| mirrors::parse_probe(&out.stdout))
                                })
                            })
                            .collect();

                        handles
                            .into_iter()
                            .filter_map(|handle| handle.join().ok())
                            .collect()
                    });

                    candidates.into_iter().zip(probes).collect::<Vec<_>>()
                })
                .await;

            let _ = this.update(cx, |shell, cx| {
                shell.state.mirrors.probing = false;
                shell.state.mirrors.results = probed
                    .iter()
                    .map(|(candidate, probe)| MirrorProbeResult {
                        site: candidate.site.clone(),
                        url: candidate.url.clone(),
                        code: probe.as_ref().map(|p| p.code).unwrap_or(0),
                        secs: probe.as_ref().map(|p| p.secs).unwrap_or(0.0),
                        bytes: probe.as_ref().and_then(|p| p.bytes),
                    })
                    .collect();
                // 按耗时排（可用的在前）—— 探测是按清单顺序发起的，
                // 不排一下最快的那条会散落在中间
                shell.state.mirrors.sort_results_by_speed();

                match mirrors::pick_fastest(&probed) {
                    Some((candidate, probe)) => {
                        shell.state.mirrors.chosen = Some(MirrorChoice {
                            site: candidate.site.clone(),
                            url: candidate.url,
                            release: release.clone(),
                            format: candidate.format,
                            bytes: probe.bytes,
                        });
                    }
                    None => {
                        shell.state.mirrors.chosen = None;
                        shell.state.mirrors.error = Some(
                            "这个发行版在所有镜像上都拿不到（镜像站的文件会随版本改名）——\
                             换一个版本，或者用「自定义下载地址」"
                                .to_owned(),
                        );
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    // -- 添加实例：执行 ----------------------------------------------------

    /// 按表单执行安装。
    ///
    /// 安装可能跑十几分钟到几十分钟（在线下载 / 铺开文件系统），所以：
    ///
    /// 1. 装前检查在**提交这一刻**做（其中"目录非空"要碰文件系统，
    ///    不能放在渲染里）；
    /// 2. 真正干活的是 [`wslc_core::cmd::install::run_plan`]（**阻塞**），
    ///    跑在后台执行器上；
    /// 3. 界面每 250 ms 把事件搬进 `AppState::installing`，
    ///    所以日志和进度是**边跑边显示**的。
    pub fn confirm_install(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tasks.install_run.is_some() {
            self.state
                .notify(Toast::error("已经有一个安装在进行 —— 等它结束，或者先取消"));
            cx.notify();
            return;
        }

        // 先把 spec 取出来（借用在这一句结束），下面才好 `&mut self` 发提示条。
        let spec = self.forms.install_form.as_ref().map(|form| {
            form.to_spec(
                cx,
                &self.state.mirrors,
                &self.state.online,
                &self.state.distro_names(),
                self.state.prefs.install_dir.as_deref(),
            )
        });
        let Some(spec) = spec else {
            self.state.notify(Toast::error("表单尚未创建"));
            cx.notify();
            return;
        };

        let ctx = self.state.plan_context();
        let dir = spec.effective_install_dir(&ctx).unwrap_or_default();
        let check = self.state.preflight(&spec, dir_non_empty(&dir));
        if !check.ok() {
            // 不弹提示条、也不在页面上列错误清单：把"哪几块有问题"记下来，
            // 让 `add_instance` 把对应的输入框标红；焦点送到第一块，
            // 长表单里用户不用自己找。
            let fields = check.field_errors();
            let focus = self.forms.install_form.as_ref().and_then(|form| {
                if fields.name {
                    Some(form.name.clone())
                } else if fields.source {
                    match form.source {
                        InstallSourceKind::Local => Some(form.source_path.clone()),
                        // 在线清单没有可聚焦的输入框（下拉框），红框会画在
                        // 来源区块上；镜像源则把焦点送到"自定义地址"。
                        InstallSourceKind::Online => None,
                        InstallSourceKind::Mirror => Some(form.mirror_url.clone()),
                    }
                } else if fields.install_dir {
                    Some(form.install_dir.clone())
                } else {
                    None
                }
            });
            if let Some(form) = self.forms.install_form.as_mut() {
                form.show_errors = true;
                form.field_errors = fields;
            }
            if let Some(input) = focus {
                let handle = input.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
            }
            tracing::info!(
                "安装参数未通过：{}",
                check.error_text().unwrap_or_else(|| "参数有误".to_owned())
            );
            cx.notify();
            return;
        }
        if let Some(form) = self.forms.install_form.as_mut() {
            form.show_errors = false;
            form.field_errors = InstallFieldErrors::default();
        }
        for warning in &check.warnings {
            tracing::info!("安装提醒：{warning}");
        }

        let plan = match install_model::plan(&spec, &ctx) {
            Ok(plan) => plan,
            Err(e) => {
                self.state.notify(Toast::error(format!("参数有误：{e}")));
                cx.notify();
                return;
            }
        };

        // 要跑哪些步骤写进日志：出问题时能直接复制到终端复现
        // （界面上的预览和执行读的是**同一份计划**）。
        tracing::info!("安装 {}：{}", plan.name, plan.preview_lines().join("  →  "));

        let events: Arc<Mutex<Vec<InstallEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let done: Arc<Mutex<Option<InstallSummary>>> = Arc::new(Mutex::new(None));
        let cancel = InstallCancel::new();
        let expected_bytes = self.state.mirrors.chosen.as_ref().and_then(|c| c.bytes);
        let wsl = self.state.wsl.clone();

        // 事件队列与结果槽的**原始 Arc 交给 `install_run`**（界面每轮从那儿取），
        // 后台任务拿的是克隆 —— 两边看到的是同一份数据。
        let work_events = Arc::clone(&events);
        let work_done = Arc::clone(&done);
        let work_cancel = cancel.clone();

        // ⚠️ 两个名字都要记：改名安装的中间态里 `wsl -l -v` 上是**源名**
        // （比如 `Ubuntu-26.04`），而重定位（导出 → 注销 → 导入）恰好在那段时间里跑。
        // 只记最终名字的话，那一行不受任何互斥保护（见 `AppState::busy_with`）。
        self.state.installing = Some(InstallProgress::new(plan.name.clone(), plan.source_names()));
        self.tasks.install_run = Some(InstallRun {
            events: Arc::clone(&events),
            done: Arc::clone(&done),
            started: Instant::now(),
        });
        self.tasks.install_cancel = Some(cancel.clone());
        self.state.notify(Toast::info(format!(
            "正在安装 {}…（期间界面可以继续用，随时能取消）",
            plan.name
        )));
        cx.notify();

        cx.spawn(async move |this, cx| {
            // 阻塞活：后台执行器 + 流式事件。这是本仓库唯一允许起进程的地方。
            let work = cx.background_executor().spawn(async move {
                let mut opts = RunOptions::new(move |event| lock(&work_events).push(event));
                opts.cancel = work_cancel;
                opts.expected_bytes = expected_bytes;
                let summary = install::run_plan(&wsl, &plan, opts);
                *lock(&work_done) = Some(summary.clone());
                summary
            });

            // 边跑边搬事件。`absorb_install_events` 顺便看一眼执行结果有没有到 ——
            // `run_plan` 是**阻塞**函数，结果只能这样交回来。
            let summary = loop {
                let finished = this
                    .update(cx, |shell, cx| shell.absorb_install_events(cx))
                    .ok()
                    .flatten();
                if let Some(summary) = finished {
                    break summary;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
            };

            // 等它真正结束，免得 `Task` 在完成前被 drop 掉（那会取消任务）
            let _ = work.await;

            let _ = this.update(cx, |shell, cx| {
                shell.absorb_install_events(cx);
                shell.finish_install(summary, cx);
            });
        })
        .detach();
    }

    /// 取消正在进行的安装（kill 当前子进程）。
    ///
    /// ⚠️ 取消**不保证**什么都没发生：在线安装可能已经注册了一半，
    /// 重定位阶段（`--unregister` 之后）更是根本不给取消 ——
    /// 见 `wslc_core::cmd::install` 的模块说明。
    pub fn cancel_install(&mut self, cx: &mut Context<Self>) {
        match &self.tasks.install_cancel {
            Some(token) => {
                token.cancel();
                tracing::info!("已请求取消安装");
                self.state.notify(Toast::info("正在取消安装…"));
            }
            None => self.state.notify(Toast::error("当前没有正在进行的安装")),
        }
        cx.notify();
    }

    /// 把后台线程塞进来的事件搬进界面状态；返回**执行结果**（还在跑时是 `None`）。
    ///
    /// 只有**真的有变化**才 `cx.notify()`：进度事件每 250 ms 就来一个，
    /// 无脑重绘等于让界面一直空转（导出那边是同样的处理）。
    ///
    /// 结果也从这里回传（而不是另外留一个方法）：界面每轮反正要看一眼事件队列，
    /// 顺手看一眼结果槽不额外花什么，还能保证"最后一批事件"一定先被吃掉。
    pub(crate) fn absorb_install_events(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<InstallSummary> {
        let (events, started, finished) = {
            // 用 `?` 而不是 `let ... else { return None }`：两者等价，
            // 而这一个函数返回的就是 `Option`，`?` 更短也更难写错
            // （clippy 的 `question_mark` 报的就是这条）。
            let run = self.tasks.install_run.as_ref()?;
            let drained: Vec<InstallEvent> = std::mem::take(&mut *lock(&run.events));
            (drained, run.started, lock(&run.done).clone())
        };

        let Some(progress) = self.state.installing.as_mut() else {
            return finished;
        };

        let mut changed = false;
        for event in &events {
            changed |= progress.apply(event);
        }

        // 没有进度事件的步骤（导入 / 注销 / 安装本身）也要显示"跑了多久"
        let secs = started.elapsed().as_secs();
        if progress.elapsed_secs != secs {
            progress.elapsed_secs = secs;
            changed = true;
        }

        if changed {
            cx.notify();
        }
        finished
    }

    /// 安装收尾：写结局、发提示、刷新列表。
    ///
    /// 失败时**留在页面上** —— 那里有完整日志，用户要能读它；
    /// 成功才跳到实例列表，让他直接看到新装好的东西。
    pub(crate) fn finish_install(&mut self, summary: InstallSummary, cx: &mut Context<Self>) {
        self.tasks.install_run = None;
        self.tasks.install_cancel = None;

        let outcome = if summary.cancelled {
            InstallOutcome::Cancelled
        } else if summary.ok {
            InstallOutcome::Success(summary.detail.clone())
        } else {
            InstallOutcome::Failed {
                detail: summary.detail.clone(),
                step: summary.failed_step.clone(),
            }
        };
        if let Some(progress) = self.state.installing.as_mut() {
            progress.finish(outcome);
        }

        if summary.cancelled {
            self.state.notify(Toast::info(format!(
                "{} 的安装已取消。如果它其实已经装好了，去实例列表看一眼；不想要就删掉。",
                summary.name
            )));
        } else if summary.ok {
            self.state
                .notify(Toast::success(format!("{} 已安装", summary.name)));
            self.goto_page(Page::Instances, cx);
        } else {
            self.state
                .notify(Toast::error(format!("安装失败：{}", summary.detail)));
        }

        self.refresh(cx);
        cx.notify();
    }
}
