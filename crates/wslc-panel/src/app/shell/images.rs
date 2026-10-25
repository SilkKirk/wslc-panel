//! 镜像域：拉取对话框、tag 查询、拉取与取消。

use super::super::prelude::*;
use super::Shell;
use super::state::{DropdownItem, ImageForm};

impl Shell {
    /// 建「镜像」页的表单（幂等）：registry + 仓库名两个输入框 + tag 下拉框。
    ///
    /// **必须**在有 `&mut Window` 的地方调用 —— `InputState::new` 与
    /// `SelectState::new` 都要它。
    pub fn ensure_image_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.image_form.is_some() {
            return;
        }

        let host = cx.new(|cx| {
            InputState::new(window, cx).placeholder(wslc_core::registry::DEFAULT_REGISTRY)
        });
        // 预填内置那个：绝大多数用户根本不改它（`set_value` 也要 `&mut Window`，
        // 所以只能在这里做）。
        host.update(cx, |state, cx| {
            state.set_value(wslc_core::registry::DEFAULT_REGISTRY, window, cx)
        });
        let repository = cx.new(|cx| {
            InputState::new(window, cx).placeholder("nginx / library/nginx / grafana/grafana")
        });
        let tags_select = cx.new(|cx| {
            SelectState::new(Vec::<DropdownItem>::new(), None, window, cx).searchable(true)
        });

        // ⚠️ 这个订阅是**必须**的，不是优化：渲染整页的是 `Shell`，而下拉框的选中项
        // 改的是 `SelectState` 自己的状态 —— 不订阅父级就不会重绘，而且
        // "选了 tag 之后量体积 / 出现拉取按钮"整条链都断了。
        //
        // 订阅本身要存进 `Shell`：`Subscription` **一 drop 就退订**
        // （和 `select_subs` 同一个坑，见那个字段的说明）。
        let tags_sub = cx.subscribe(
            &tags_select,
            |shell, _emitter, event: &SelectEvent<Vec<DropdownItem>>, cx| {
                if let SelectEvent::Confirm(Some(tag)) = event {
                    shell.select_registry_tag(tag.clone(), cx);
                }
            },
        );
        let host_sub = cx.subscribe(&host, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        let repository_sub = cx.subscribe(&repository, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        self.image_subs = vec![tags_sub, host_sub, repository_sub];

        self.image_form = Some(ImageForm {
            host,
            repository,
            tags_select,
        });
    }

    /// 打开「拉取镜像」弹窗。
    ///
    /// 表单**只在第一次打开时建**（`InputState::new` / `SelectState::new` 都要
    /// `&mut Window`），之后一直留着 —— 第二次打开时上回的 registry / 仓库名 /
    /// 那一批 tag 都还在（重建会把它们清掉，用户得重新查一遍）。
    pub fn open_pull_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(progress) = &self.state.pulling {
            let image = progress.image.clone();
            self.state
                .notify(Toast::error(format!("{image} 正在拉取中，请等它结束")));
            cx.notify();
            return;
        }
        self.ensure_image_form(window, cx);
        self.state.set_section_open(UiSection::PullSource, false);
        // 下拉框里得有上一次查到的 tag（表单是留着的，但列表来自状态）。
        self.fill_tag_select(window, cx);
        self.pull_open = true;
        // 焦点给仓库名框：打开就能直接打字（registry 一般不用改）。
        if let Some(form) = self.image_form.as_ref() {
            let handle = form.repository.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }
        cx.notify();
    }

    /// 关闭「拉取镜像」弹窗。
    ///
    /// ⚠️ 拉取进行中**不允许关闭** —— 关掉会让人以为任务被取消了。
    /// 关不掉时给一句提示，别让这一下点击石沉大海（`AGENTS.md` §7.1 那条
    /// "点下去必须有变化"的同一条道理）。
    pub fn close_pull_dialog(&mut self, cx: &mut Context<Self>) {
        if self.state.pulling.is_some() {
            self.state
                .notify(Toast::info("正在拉取：要停止请点「取消拉取」"));
            cx.notify();
            return;
        }
        self.pull_open = false;
        cx.notify();
    }

    /// 从「拉取镜像」弹窗直接去建容器：**先关弹窗**，再开创建表单。
    ///
    /// 两个浮层叠在一起没有任何好处（而且遮罩只有一层），关掉之后用户回头
    /// 还能在「本地镜像」表里看见刚拉下来的那一条。
    pub fn use_pulled_image(&mut self, image: String, window: &mut Window, cx: &mut Context<Self>) {
        self.pull_open = false;
        self.open_create_dialog_prefilled(Some(image.as_str()), window, cx);
    }

    /// 把 tag 列表填进下拉框。
    ///
    /// ⚠️ `set_items` 会把下拉框自己的选中项清掉，而用户没做错什么 ——
    /// 所以换完清单要把"要拉的那个 tag"（[`RegistryState::effective_tag`]）
    /// **重新选上**（它还在新清单里的话）。
    ///
    /// ⚠️ 要 `&mut Window`，所以只能在 `update_in` 里调（见 [`Shell::query_registry`]）。
    pub(crate) fn fill_tag_select(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = self.image_form.as_ref() else {
            return;
        };
        let select = form.tags_select.clone();

        // ⚠️ 候选项由 `RegistryState::tag_items()` 给（纯逻辑，那边有测试）——
        // 它会把**列表里没有、但用户手打的**那个 tag 补在最前面。
        //
        // 为什么这条必须有：实机反馈"我想拉取 openjdk:8-jre-slim，结果啥东东都
        // 出来了" —— 用户打的 tag 不在这个 registry 返回的列表里（实测它的
        // `tags/list` 对 `library/openjdk` 只回 17282 个，既没有 `8-jre-slim`
        // 也没有 `latest`，而两者的 manifest 都是 200/能拉），于是
        // `set_selected_value` 找不到对应项 → 组件**自动选中第一项** `28-ea`
        // → "将拉取"跟着变成 `:28-ea`，**拉错镜像**。
        let items: Vec<DropdownItem> = self
            .state
            .registry
            .tag_items()
            .into_iter()
            .map(|tag| DropdownItem::new(tag.clone(), tag))
            .collect();
        let keep = self.state.registry.effective_tag().map(str::to_owned);

        select.update(cx, |state, cx| {
            state.set_items(items, window, cx);
            if let Some(tag) = keep {
                state.set_selected_value(&tag, window, cx);
            }
        });
    }

    /// 「镜像」页点「查询」：拉这个仓库的 tag 列表。
    ///
    /// ⚠️ 那个请求**必须在后台执行器上**（`AGENTS.md` §7.1）：实测 `node` 的
    /// tag 列表是 152 KB / 2.4 秒，放界面线程上就是"点了卡两秒"。
    ///
    /// ⚠️ 回来之后要**填下拉框**，而 `SelectState::set_items` 要 `&mut Window`
    /// —— 所以走 `spawn_in` + `update_in`（和 [`Shell::refresh_online_list`]
    /// 同一条路，见 `AGENTS.md` §7.2）。
    pub fn query_registry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 先把两个控件句柄取出来（`Entity` 克隆很便宜）：后面要 `&mut self`
        // 调别的方法，别让 `form` 的不可变借用一直挂着。
        let (host_input, repo_input) = {
            let Some(form) = self.image_form.as_ref() else {
                return;
            };
            (form.host.clone(), form.repository.clone())
        };

        let typed_host = host_input.read(cx).value().trim().to_owned();
        // 空着就当内置那个：用户清空 registry 框的意图多半是"用默认的"，
        // 而不是"给我一个非法地址的错误"。
        let host = {
            let normalized = wslc_core::registry::normalize_host(&typed_host);
            if normalized.is_empty() {
                wslc_core::registry::DEFAULT_REGISTRY.to_owned()
            } else {
                normalized
            }
        };
        // 收拾过就把结果写回框里（粘一整条 URL 是常态：`https://x/` 会变成 `x`）。
        if host != typed_host {
            let value = host.clone();
            host_input.update(cx, |state, cx| state.set_value(value, window, cx));
        }
        // ⚠️ 校验必须在**发请求之前**：`host` 会拼进 URL，再作为裸 argv 交给
        // `curl.exe`（判据只有 `registry::is_valid_host` 一处）。
        if !wslc_core::registry::is_valid_host(&host) {
            self.state.set_section_open(UiSection::PullSource, true);
            let handle = host_input.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
            self.state
                .notify(Toast::error(format!("registry 地址不合法：{host}")));
            cx.notify();
            return;
        }

        let raw = repo_input.read(cx).value().trim().to_owned();
        if raw.is_empty() {
            self.state
                .notify(Toast::error("先填一个仓库名，例如 nginx"));
            cx.notify();
            return;
        }
        // 规范化（短名补 `library/`、拆开粘贴进来的 URL、拆出 `:tag`）。
        // **失败就不发请求** —— 判据只有 `registry::split_input` 那一处
        // （那里也是安全边界）。
        let (repository, input_tag) = match registry::split_input(&raw) {
            Ok(parts) => parts,
            Err(e) => {
                self.state.notify(Toast::error(e));
                cx.notify();
                return;
            }
        };
        if self.state.registry.loading {
            return;
        }
        self.state
            .registry
            .begin_query(&host, &raw, &repository, input_tag.clone());
        cx.notify();

        let fetch_host = host.clone();
        let fetch_repository = repository.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    wslc_core::cmd::registry::fetch_tags(&fetch_host, &fetch_repository)
                })
                .await;
            let _ = this.update_in(cx, |shell, window, cx| {
                match result {
                    Ok(tags) => shell.state.registry.set_tags(&repository, tags),
                    Err(e) => shell.state.registry.set_error(e),
                }
                // 输入里自带 tag（用户粘的是完整引用，例如
                // `docker.m.daocloud.io/library/nginx:1.27`）→ 直接选中它。
                // **成功和失败两条路都要做**：别的 registry 的 tags/list 可能
                // 要认证（实测 daocloud 401），那时列表是空的，
                // 而这个 tag 就是唯一能拉的东西。
                if let Some(tag) = input_tag.clone() {
                    shell.select_registry_tag(tag, cx);
                }
                shell.fill_tag_select(window, cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// 选中一个 tag → 后台量它的**下载体积**。
    ///
    /// 只给**选中的那一个**量：一次要两个请求（index → 本机架构那份 manifest，
    /// 实测内容协商省不掉），列表里 1339 个 tag 全量一遍就是 1339 次请求，
    /// 直接撞限流。
    pub fn select_registry_tag(&mut self, tag: String, cx: &mut Context<Self>) {
        // 已经选中的 tag 再选一次 = 什么都不做。
        //
        // 两个理由：① [`Shell::fill_tag_select`] 换清单后会**重新选一次**，而组件
        // 也会为"选中变了"发事件 —— 不判重就会多量一次体积（两次请求，还可能撞
        // 限流）；② 那条路径下重选同一个 tag 会把刚量到的体积**清掉**
        //（`select_tag` 里 `size = None`），于是"量出来了又变回量不出来"。
        if self.state.registry.selected.as_deref() == Some(tag.as_str()) {
            return;
        }
        self.state.registry.select_tag(&tag);
        cx.notify();

        let host = self.state.registry.host.clone();
        let repository = self
            .state
            .registry
            .loaded
            .clone()
            .unwrap_or_else(|| self.state.registry.repository.clone());
        // ⚠️ `tag` 要**两份**：一份移进后台任务（`fetch_size` 要它），
        // 另一份留给结果回来时的核对。不克隆的话 `tag` 会被移进内层协程，
        // 外层那个 `Some(tag.as_str())` 就成了"借用已移动的值"（CI 实测报 E0382）。
        let fetch_tag = tag.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    wslc_core::cmd::registry::fetch_size(&host, &repository, &fetch_tag)
                })
                .await;
            let _ = this.update(cx, |shell, cx| {
                // 结果回来时用户可能已经点了别的 tag —— 那这一次就**丢掉**。
                // 把 nginx 的体积挂到 alpine 上是比"暂时没有体积"更糟的事。
                if shell.state.registry.selected.as_deref() != Some(tag.as_str()) {
                    return;
                }
                match result {
                    Ok(size) => shell.state.registry.set_size(size.bytes, size.layers),
                    Err(e) => shell.state.registry.set_size_error(e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 常用镜像的快捷入口：填进输入框并**立刻查**。
    ///
    /// 一键到位 —— 用户点 `nginx` 的意图就是"看看 nginx 有哪些 tag"，
    /// 再让他点一次「查询」是多余的。
    pub fn apply_registry_preset(
        &mut self,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // ⚠️ 先挡掉"正在查"，**再**动输入框：反过来的话输入框已经变成新仓库名，
        // 而结果列表还是上一个仓库的 —— 那正是 §3.2 要避免的串台，
        // 只不过这次是我们自己造成的。
        if self.state.registry.loading {
            self.state.notify(Toast::info("上一个查询还没回来"));
            cx.notify();
            return;
        }
        let Some(form) = self.image_form.as_ref() else {
            return;
        };
        form.repository
            .update(cx, |state, cx| state.set_value(name.to_owned(), window, cx));
        self.query_registry(window, cx);
    }

    /// 「镜像」页点「拉取」：把将要拉的那个引用拉下来。
    ///
    /// v0.6.2 起这是**唯一**的拉取入口（原先还有「拉取镜像」弹窗）——
    /// 引用由 [`RegistryState::selected_reference`] 从
    /// registry / 仓库名 / tag 三件东西拼出来。
    pub fn pull_registry_selection(&mut self, cx: &mut Context<Self>) {
        if self.state.pulling.is_some() {
            self.state
                .notify(Toast::info("已经有一个拉取在跑了，等它结束"));
            cx.notify();
            return;
        }
        // 输入可能刚在这一帧改变；提交时再次核对，不能拉取上次查询的旧目标。
        let inputs_match = self.image_form.as_ref().is_some_and(|form| {
            self.state.registry.matches_inputs(
                form.host.read(cx).value().trim(),
                form.repository.read(cx).value().trim(),
            )
        });
        if !inputs_match {
            self.state.notify(Toast::error("输入已修改，请先查询镜像"));
            cx.notify();
            return;
        }
        let Some(reference) = self.state.registry.selected_reference() else {
            self.state
                .notify(Toast::error("先选一个 tag，或者在仓库名后面写上 `:tag`"));
            cx.notify();
            return;
        };
        self.start_pull(reference, cx);
    }

    /// 发起一次**流式** `wslc pull`（不碰输入框、不弹窗）。
    ///
    /// v0.6.2 之后拉取只有**一个**入口（「镜像」页那个「拉取」按钮），
    /// 所以这里收的是**拼好的引用**：[`RegistryState::selected_reference`]
    /// 把 registry / 仓库名 / tag 三件东西拼成 `host/repo:tag`。
    /// 流式进度 + 取消 + 收尾提示 + 刷新**只有这一份实现**。
    pub fn start_pull(&mut self, reference: String, cx: &mut Context<Self>) {
        if self.state.pulling.is_some() {
            return;
        }

        // 读取线程不能直接碰 `AppState`，所以先把输出行塞进这个共享缓冲，
        // 由下面的异步任务定期搬进状态。
        //
        // 用 `Arc<Mutex<Vec<String>>>` 而不是 `mpsc::Sender`：
        // 回调要求 `Send + Sync`，而 `Sender` 的 `Sync` 实现随版本变化，
        // 共享缓冲没有这个不确定性。
        let buffer: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&buffer);

        let wslc = self.state.wslc.clone();
        let handle = match wslc_core::cmd::image::pull_streaming(&wslc, &reference, move |line| {
            if let Ok(mut pending) = sink.lock() {
                // 兜底防爆：读取线程可能远快于界面轮询。
                if pending.len() < 2000 {
                    pending.push(line.to_owned());
                }
            }
        }) {
            Ok(handle) => handle,
            Err(e) => {
                self.state
                    .notify(Toast::error(format!("无法启动拉取：{e}")));
                cx.notify();
                return;
            }
        };

        self.pull_cancel = Some(handle.cancel_token());
        self.state.pulling = Some(state::PullProgress::new(reference.clone()));
        self.state
            .notify(Toast::info(format!("开始拉取 {reference}")));
        cx.notify();

        cx.spawn(async move |this, cx| {
            let code = loop {
                // 1) 把这一轮攒下的输出搬进状态
                let batch: Vec<String> = {
                    let mut pending = buffer.lock().unwrap_or_else(|e| e.into_inner());
                    std::mem::take(&mut *pending)
                };
                if !batch.is_empty() {
                    let _ = this.update(cx, |shell, cx| {
                        if let Some(progress) = shell.state.pulling.as_mut() {
                            // 没有新增就不重绘
                            if progress.push_lines(batch) {
                                cx.notify();
                            }
                        }
                    });
                }

                // 2) 子进程结束了吗（非阻塞）
                match handle.try_wait() {
                    Ok(Some(code)) => break code,
                    Ok(None) => {}
                    Err(e) => {
                        tracing::warn!("检查拉取进程失败：{e}");
                        break -1;
                    }
                }

                cx.background_executor()
                    .timer(Duration::from_millis(200))
                    .await;
            };

            let cancelled = handle.was_cancelled();
            // 进程已退出，这里只是等读取线程把剩余输出读完。
            let _ = handle.finish();

            let _ = this.update(cx, |shell, cx| {
                // 最后一行留作失败时的原因说明
                let last_line = shell
                    .state
                    .pulling
                    .as_ref()
                    .and_then(|p| p.last_line())
                    .unwrap_or_default()
                    .to_owned();

                shell.state.pulling = None;
                shell.pull_cancel = None;

                if cancelled {
                    shell
                        .state
                        .notify(Toast::info(format!("{reference} 已取消")));
                } else if code == 0 {
                    shell
                        .state
                        .notify(Toast::success(format!("{reference} 拉取完成")));
                } else {
                    let detail = if last_line.is_empty() {
                        format!("退出码 {code}")
                    } else {
                        last_line
                    };
                    shell
                        .state
                        .notify(Toast::error(format!("{reference} 拉取失败：{detail}")));
                }

                // 取消不刷新（用户明确不想继续）；失败也刷一下，
                // 因为可能已经拉下来一部分层。
                if !cancelled {
                    shell.refresh(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 取消正在进行的拉取（kill 子进程）。
    pub fn cancel_pull(&mut self, cx: &mut Context<Self>) {
        match &self.pull_cancel {
            Some(token) => {
                token.cancel();
                tracing::info!("已请求取消拉取");
                self.state.notify(Toast::info("正在取消…"));
            }
            None => self.state.notify(Toast::error("当前没有正在进行的拉取")),
        }
        cx.notify();
    }
}
