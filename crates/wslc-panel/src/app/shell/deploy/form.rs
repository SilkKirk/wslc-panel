//! 「添加部署 / 重新部署」弹窗：打开、各开关、浏览目录、命令预览、读回表单值。

use super::super::super::prelude::*;
use super::super::Shell;
use super::super::state::{AfterPick, DeployField, DeployForm, DeployFormValues};

impl Shell {
    /// 打开「添加部署」（`editing = None`）或「重新部署」（`Some(应用名)`）弹窗。
    ///
    /// # 为什么"重新部署"也走这个弹窗，而不是一键重传
    ///
    /// 一键重传只能覆盖"我在同一个路径重新构建了产物"这一种情况。
    /// 用户想改目标目录 / 启动命令时，一键重传帮不上忙 —— 而他能走的另一条路
    /// 是**先删掉再添加**，那个「删掉」会把容器里的目录清空，代价大得多。
    /// 所以"重新部署"= 把这一条**预填**进同一个弹窗：直接点确认就是重传，
    /// 想改哪格就改哪格。动作还是只有**一个入口**。
    ///
    /// 应用名在重新部署时**锁死**（改了就是另一个部署了，那应该走「添加部署」）。
    pub fn open_deploy_dialog(
        &mut self,
        editing: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(deploy) = self.state.deploy.as_ref() else {
            return;
        };
        let runtime = deploy.runtime;
        let web_root = deploy.web_root.clone();
        let container = deploy.container.clone();
        let ports = deploy.ports.clone();
        let record = editing
            .as_ref()
            .and_then(|app| self.state.deploy_store.get(&container, app).cloned());

        // 多应用：新应用的名字 / 端口不能和已有的撞。应用名撞了会**覆盖**那一条
        // （`DeployStore::put` 按 `<容器>/<应用>` 索引），端口撞了两个进程会
        // 在容器里抢同一个 bind —— 两件都是"部署成功但原来那个不见了"的来源。
        let existing_names: Vec<String> = self
            .state
            .deploy_store
            .for_container(&container)
            .into_iter()
            .map(|record| record.app.clone())
            .collect();
        let existing_ports = self.state.deploy_store.binding_ports(&container);
        let published_ports = deploy_model::published_container_ports(&ports);

        // ⚠️ 默认应用名**不是**运行时键（`java` → `/opt/java`，正是 §15 那次
        // 把镜像自带 JDK 删掉的路径）。理由写在 `DEFAULT_APP_NAME` 那儿。
        // 多应用时再点一次「添加部署」，`suggest_app_name` 会给出 `app-2`。
        let app_value = record
            .as_ref()
            .map(|r| r.app.clone())
            .unwrap_or_else(|| deploy_model::suggest_app_name(&existing_names));
        let source_value = record
            .as_ref()
            .map(|r| r.source.clone())
            .unwrap_or_default();
        let target_value = record
            .as_ref()
            .map(|r| r.target.clone())
            .unwrap_or_else(|| {
                // nginx 的目标必须**跟着挂载点走**，而且**每个应用一个子目录**：
                // 所有应用铺进同一个 Web 根时，`removals_script` 的逐名删除会把
                // 另一个应用的同名文件删掉（见 `docs/PLAN-multi-app.md` §1.3）。
                deploy_model::default_app_target(runtime, &app_value, web_root.as_deref())
            });
        // 存档里有就用存档的；没有就从来源路径推（Java / Go 的默认启动命令
        // 要的是**源文件的名字**，不是应用名）
        let artifact = record
            .as_ref()
            .map(|r| r.artifact.clone())
            .filter(|name| !name.is_empty())
            .or_else(|| {
                if source_value.is_empty() {
                    None
                } else {
                    deploy_model::artifact_name(std::path::Path::new(&source_value))
                }
            });
        let command_value = record
            .as_ref()
            .map(|r| r.command.clone())
            .unwrap_or_else(|| {
                runtime
                    .default_start_command(&app_value, artifact.as_deref())
                    .unwrap_or_default()
            });
        let port_value = record
            .as_ref()
            .map(|r| r.port.to_string())
            .unwrap_or_else(|| {
                deploy_model::suggest_app_port(
                    &existing_ports,
                    &published_ports,
                    runtime.default_port(),
                )
                .to_string()
            });
        // 站点开的记录才有这一块；新应用默认**关**（先铺文件，再决定要不要
        // 接管端口）。老记录里 `site` 是 `None`，读出来也就是关。
        let record_site = record.as_ref().and_then(|r| r.site.clone());
        let site_enabled = record_site.is_some();
        let site_spa = record_site.as_ref().map(|s| s.spa).unwrap_or(true);
        let first_proxy = record_site.as_ref().and_then(|site| site.proxies.first());
        let proxy_prefix_value = first_proxy
            .map(|rule| rule.prefix.clone())
            .unwrap_or_default();
        let proxy_upstream_value = first_proxy
            .map(|rule| rule.upstream.clone())
            .unwrap_or_default();
        let proxy_strip = first_proxy.is_some_and(|rule| rule.strip_prefix);
        let server_name_value = record_site
            .as_ref()
            .map(|site| site.server_name.clone())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "localhost".to_owned());
        // 第一条进上面两格；其余塞进「更多反代」一格（预填成它能解析的写法）。
        let proxy_more_value = record_site
            .as_ref()
            .map(|site| {
                site.proxies
                    .iter()
                    .skip(1)
                    .map(deploy_model::format_proxy_rule)
                    .collect::<Vec<_>>()
                    .join("; ")
            })
            .unwrap_or_default();

        self.state
            .set_section_open(UiSection::DeployProxy, first_proxy.is_some());
        self.state
            .set_section_open(UiSection::DeployProxyMore, !proxy_more_value.is_empty());

        // `InputState::new` 要 `&mut Window` → 只能在这里（有 window）建。
        let app = cx.new(|cx| InputState::new(window, cx).placeholder("site"));
        let source = cx.new(|cx| {
            InputState::new(window, cx).placeholder(
                if runtime.source_kind() == deploy_model::SourceKind::Directory {
                    r"E:\build\site"
                } else {
                    r"E:\build\app.jar"
                },
            )
        });
        let target = cx.new(|cx| InputState::new(window, cx).placeholder("/opt/site"));
        let command = cx.new(|cx| InputState::new(window, cx).placeholder("java -jar app.jar"));
        let port = cx.new(|cx| InputState::new(window, cx).placeholder("8080"));
        let server_name = cx.new(|cx| InputState::new(window, cx).placeholder("localhost"));
        let proxy_prefix = cx.new(|cx| InputState::new(window, cx).placeholder("/api/"));
        let proxy_upstream =
            cx.new(|cx| InputState::new(window, cx).placeholder("http://api:8080"));
        let proxy_more = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("/auth/ => http://auth:9000; /ws/ => http://ws:9001 strip")
        });

        app.update(cx, |state, cx| {
            state.set_value(app_value, window, cx);
        });
        source.update(cx, |state, cx| {
            state.set_value(source_value, window, cx);
        });
        target.update(cx, |state, cx| {
            state.set_value(target_value, window, cx);
        });
        command.update(cx, |state, cx| {
            state.set_value(command_value, window, cx);
        });
        port.update(cx, |state, cx| {
            state.set_value(port_value, window, cx);
        });
        server_name.update(cx, |state, cx| {
            state.set_value(server_name_value, window, cx);
        });
        proxy_prefix.update(cx, |state, cx| {
            state.set_value(proxy_prefix_value, window, cx);
        });
        proxy_upstream.update(cx, |state, cx| {
            state.set_value(proxy_upstream_value, window, cx);
        });
        proxy_more.update(cx, |state, cx| {
            state.set_value(proxy_more_value, window, cx);
        });

        // 焦点给**来源**那一格：新增时最可能要做的事就是点「浏览…」，
        // 重新部署时用户多半直接点确认 —— 焦点在哪都不影响。
        let handle = source.read(cx).focus_handle(cx);
        window.focus(&handle, cx);

        let input_subs = [&proxy_prefix, &proxy_upstream, &proxy_more]
            .into_iter()
            .map(|input| {
                cx.subscribe(input, |_, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        cx.notify();
                    }
                })
            })
            .collect();
        self.deploy_form = Some(DeployForm {
            _input_subs: input_subs,
            editing,
            app,
            source,
            target,
            command,
            port,
            site_enabled,
            site_spa,
            server_name,
            proxy_prefix,
            proxy_upstream,
            proxy_strip,
            proxy_more,
            auto_connect: true,
        });
        cx.notify();
    }

    /// 关掉「添加部署」弹窗（取消 / 提交完都走它）。
    pub fn close_deploy_dialog(&mut self, cx: &mut Context<Self>) {
        self.deploy_form = None;
        cx.notify();
    }

    /// 「生成站点配置」勾选变化。
    pub fn set_deploy_site_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if let Some(form) = self.deploy_form.as_mut() {
            form.site_enabled = enabled;
        }
        cx.notify();
    }

    /// 「单页应用回退」勾选变化。
    pub fn set_deploy_site_spa(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if let Some(form) = self.deploy_form.as_mut() {
            form.site_spa = enabled;
        }
        cx.notify();
    }

    /// 「去掉前缀」勾选变化。
    pub fn set_deploy_proxy_strip(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if let Some(form) = self.deploy_form.as_mut() {
            form.proxy_strip = enabled;
        }
        cx.notify();
    }

    /// 「自动接通网络」勾选变化。
    pub fn set_deploy_auto_connect(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if let Some(form) = self.deploy_form.as_mut() {
            form.auto_connect = enabled;
        }
        cx.notify();
    }

    /// 「浏览…」：按运行时的来源类型弹文件 / 目录对话框。
    ///
    /// 复用 [`Shell::spawn_picker`]（它已经带好了 `picking` 守卫 ——
    /// `cmd::picker` 的文档要求"同时只弹一个"）和正确的
    /// `spawn_in` + `background_executor` 形状，不再写一份。
    ///
    /// 选完之后由 `AfterPick::DeploySource` 触发
    /// [`Shell::resync_deploy_command`]（Java / Go 的启动命令里嵌的是**源文件的
    /// 名字**，来源一换那格就不对了）。
    pub fn browse_deploy_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(form), Some(deploy)) = (self.deploy_form.as_ref(), self.state.deploy.as_ref())
        else {
            return;
        };
        let runtime = deploy.runtime;
        let input = form.source.clone();
        let folders = runtime.source_kind() == deploy_model::SourceKind::Directory;
        let label = runtime.source_label();
        let extensions = runtime.source_extensions();

        self.spawn_picker(
            &input,
            folders,
            label,
            extensions,
            "选择要部署的来源",
            AfterPick::DeploySource,
            window,
            cx,
        );
    }

    /// 来源选好之后，把**启动命令**按新来源重算一遍。
    ///
    /// # 为什么可以覆盖用户已经改过的内容
    ///
    /// Java / Go 的默认启动命令里嵌的是**源文件的名字**
    /// （`java -jar myapp-1.2.0.jar`、`./api`）—— 来源一换它就不对了，
    /// 留着只会得到 `Error: Unable to access jarfile`。而这两族的正常操作顺序
    /// 就是"先选来源、再调命令"，所以覆盖是符合预期的。
    ///
    /// nginx / Node 不动这一格：它们的命令（`npm start`）与来源文件名无关。
    pub(crate) fn resync_deploy_command(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(form), Some(deploy)) = (self.deploy_form.as_ref(), self.state.deploy.as_ref())
        else {
            return;
        };
        let runtime = deploy.runtime;
        if !matches!(runtime, AppRuntime::Java | AppRuntime::Go) {
            return;
        }
        let source = form.source.read(cx).value().trim().to_owned();
        if source.is_empty() {
            return;
        }
        let app = form.app.read(cx).value().trim().to_owned();
        let artifact = deploy_model::artifact_name(std::path::Path::new(&source));
        let Some(command) = runtime.default_start_command(&app, artifact.as_deref()) else {
            return;
        };
        let input = form.command.clone();
        input.update(cx, |state, cx| state.set_value(command, window, cx));
        cx.notify();
    }

    /// 把焦点送到「添加部署」弹窗里某一格（校验没过时用）。
    ///
    /// 单独一个方法而不是在 `submit_deploy` 里写个闭包：闭包要同时捕获
    /// `&mut self` 和 `cx`，借用关系绕，而且四个校验分支会各写一遍。
    ///
    /// ⚠️ `cx` 必须是 `&mut Context<Self>`（不能是 `&App`）：
    /// `Window::focus(&self, handle, cx)` 要的是 `&mut App`，而
    /// `InputState::focus_handle(cx)` 要的是 `&App` —— 两者差一层可变借用。
    /// 第一版这里写成了 `&App`，CI 报 `E0308: types differ in mutability`。
    /// 拿 `Context<Self>` 而不是 `&mut App`，是因为调用点本来就握着
    /// `&mut Context<Self>`（省掉一次降低），而它降到 `&App` / `&mut App`
    /// 都是自动的 —— 仓库里 `window.focus(&handle, cx)` 那些地方就是这么写的。
    pub(crate) fn focus_deploy_field(
        &mut self,
        which: DeployField,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 校验报错不能把焦点送到折叠区内部却不展开。
        if matches!(
            &which,
            DeployField::ProxyPrefix | DeployField::ProxyUpstream | DeployField::ProxyMore
        ) {
            self.state.set_section_open(UiSection::DeployProxy, true);
        }
        if matches!(&which, DeployField::ProxyMore) {
            self.state
                .set_section_open(UiSection::DeployProxyMore, true);
        }
        cx.notify();
        let Some(form) = self.deploy_form.as_ref() else {
            return;
        };
        let input = match which {
            DeployField::App => &form.app,
            DeployField::Source => &form.source,
            DeployField::Target => &form.target,
            DeployField::Command => &form.command,
            DeployField::Port => &form.port,
            DeployField::ProxyPrefix => &form.proxy_prefix,
            DeployField::ProxyUpstream => &form.proxy_upstream,
            DeployField::ServerName => &form.server_name,
            DeployField::ProxyMore => &form.proxy_more,
        };
        let handle = input.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
    }

    /// 读「添加部署」弹窗里**此刻**的值。
    ///
    /// 和 `read_db_form` 同一条理由：执行时用的就是用户眼睛看到的那个，
    /// 不存在"界面显示 A、实际用 B"的可能。
    pub(crate) fn read_deploy_form(&self, cx: &App) -> Option<DeployFormValues> {
        let form = self.deploy_form.as_ref()?;
        Some(DeployFormValues {
            app: form.app.read(cx).value().trim().to_owned(),
            source: form.source.read(cx).value().trim().to_owned(),
            target: form.target.read(cx).value().trim().to_owned(),
            command: form.command.read(cx).value().trim().to_owned(),
            port: form.port.read(cx).value().trim().parse::<u16>().ok(),
            site_enabled: form.site_enabled,
            site_spa: form.site_spa,
            server_name: form.server_name.read(cx).value().trim().to_owned(),
            proxy_prefix: form.proxy_prefix.read(cx).value().trim().to_owned(),
            proxy_upstream: form.proxy_upstream.read(cx).value().trim().to_owned(),
            proxy_strip: form.proxy_strip,
            proxy_more: form.proxy_more.read(cx).value().trim().to_owned(),
            auto_connect: form.auto_connect,
        })
    }
}
