//! 执行计划：类型（`PlanContext` / `PlannedStep` / `InstallPlan`）与 `plan()`。

use std::path::PathBuf;

use super::*;

/// 计划阶段需要知道的**环境**（都不是用户填的，所以单独一个结构）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanContext {
    /// 「默认安装目录」偏好。
    pub default_dir: Option<String>,
    /// 临时目录（镜像站下载、在线安装重定位时的中转 tar 都放这儿）。
    pub temp_dir: String,
    /// 临时文件名里的一段唯一标记（进程 id + 时间戳）。
    ///
    /// 计划里要写出**具体的**临时文件路径，用户才看得见"会往哪写"；
    /// 但也因此不能让两次安装撞同一个文件名。
    pub stamp: String,
    /// `.wslconfig` 里是否要求新发行版用稀疏 VHD（`[experimental] sparseVhd=true`）。
    ///
    /// 开着的话，装完补一条 `wsl --manage <name> --set-sparse true` ——
    /// 那是用户自己在 WSL 配置里要求的，不是我们自作主张（参考实现也这么做）。
    pub wslconfig_sparse: bool,
}

impl Default for PlanContext {
    fn default() -> Self {
        let stamp = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
        );
        Self {
            default_dir: None,
            temp_dir: std::env::temp_dir()
                .join("wslc-panel")
                .to_string_lossy()
                .into_owned(),
            stamp,
            wslconfig_sparse: false,
        }
    }
}

/// 计划里一步要用什么去执行。
///
/// 前四个是"起一个进程"；后两个是**动作**（执行器自己实现，不是命令行）。
/// 把它们放进同一个枚举，是为了让"预览"能按**真实顺序**列出全部步骤 ——
/// 只列命令的话，用户会以为中间那些"等待/重定位"不存在。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanProgram {
    /// 起 `wsl.exe`，`args` 是它的参数。
    Wsl,
    /// 起 `curl.exe`，`args` 是它的参数。
    Curl,
    /// `std::fs::create_dir_all(args[0])`。
    CreateDir,
    /// `std::fs::remove_file(args[0])`（文件不存在也算成功）。
    RemoveFile,
    /// 等发行版注册完成：轮询 `wsl -l -q` 直到 `args[0]` 出现。
    WaitRegistered,
    /// **必要时**重定位：`args = [源名, 目标名, 目标目录, 中转 tar 路径]`。
    ///
    /// 装完发现"名字/位置已经正好了"就什么也不做；否则
    /// `--export` → `--unregister` → `--import`。这一段**不可取消**：
    /// 走到 `--unregister` 之后被取消，等于把刚装好的东西删了。
    EnsureRelocated,
}

/// 计划里的一步。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedStep {
    /// 这一步在干什么（给人看的一句话）。
    pub label: String,
    /// 用什么执行。
    pub program: PlanProgram,
    /// 参数（含义随 `program` 变，见 [`PlanProgram`]）。
    pub args: Vec<String>,
    /// 能不能被用户取消。
    ///
    /// `false` 的只有重定位那一步 —— 它中途被打断会**丢数据**。
    /// 界面据此不给"取消"按钮，而不是让用户点了之后才发现没用。
    pub cancellable: bool,
    /// 失败时要不要**换一个下载源**（商店 ⇄ `--web-download`）重试一次。
    ///
    /// 只给在线安装那一步用。两条通道走的是**不同的下载实现**，
    /// 所以一条挂了另一条可能通：实测（2026-10-10）本机
    /// `--location` 那条通道秒报 `Wsl/InstallDistro/WININET_E_CANNOT_CONNECT`，
    /// 而参考实现那条（不带 `--location`）能连上。
    ///
    /// 用户看到的失败信息是"无法与服务器建立连接"，但**换一条路可能就好了** ——
    /// 与其让他自己去猜，不如自动换一次。
    pub retry_other_source: bool,
    /// 这一步失败**不算安装失败**（只记一条警告）。
    ///
    /// 给"收尾"步骤用：那一刻发行版已经装好了，比如"按 `.wslconfig` 开稀疏盘"
    /// —— WSL 完全可能因为自己的策略拒绝它（实测它默认就拒绝），
    /// 把整体判成失败会让人以为白装了（界面报"安装失败"、其实好好的）。
    pub best_effort: bool,
    /// 失败时换成**另一个 id** 重试（`-d` 后面那个名字）。
    ///
    /// 只给在线安装那一步用，而且只在"用户选的是裸家族名"时才给。
    /// 起因是实测（2026-10-11，本机）：`wsl -l -o` 排在**第一位**的裸 `Ubuntu`
    /// 是清单里那条旧的商店包，`wsl --install -d Ubuntu` 2 秒就
    /// `Wsl/InstallDistro/WININET_E_CANNOT_CONNECT`，而同一分钟的
    /// `-d Ubuntu-24.04` / `-d Debian` 能照常下载 —— 参考实现的列表第一项是
    /// 「Ubuntu 26.04 LTS」（`Ubuntu-26.04`），所以它一直没撞上。
    ///
    /// 换成功之后，**真实注册的名字**是那个新 id，执行器会把后面
    /// 「等注册」「重定位」两步的参数一起改过来（见 `cmd::install::run_plan`）。
    pub retry_other_id: Option<String>,
    /// 失败原因是"资源还被占着"时，**等一下再跑同一条命令**。
    ///
    /// 只给「按 `.wslconfig` 开稀疏盘」那一步用（`--set-sparse`）。
    ///
    /// # 为什么非重试不可
    ///
    /// 那一步是 [`PlannedStep::best_effort`]：失败**不会**把安装判成失败。
    /// 于是紧接着 `--install` 跑（VHDX 还被 WSL 自己开着）时，用户明明在
    /// `.wslconfig` 里写了 `[experimental] sparseVhd=true`，**结果静默没生效**
    /// —— 而界面上没有第二个地方能开它（v0.4 把详情里那个开关拿掉了，
    /// 见 `DistroAction::SetSparse` 的说明）。
    ///
    /// 本机实测（2026-10-12）：紧接着 `--install` 之后跑它就是
    /// `Wsl/Service/ERROR_SHARING_VIOLATION`（退出码 -1）；同一个发行版隔一会儿
    /// 手工再跑就"操作成功完成"。所以这里等一等就能拿到用户要的结果。
    ///
    /// ⚠️ 重试只认**错误码**（见 `cmd::install::is_busy`），而且用户点了取消
    /// 就不再试 —— 别的失败（比如 WSL 因为自己的策略拒绝开稀疏盘）等多久都一样。
    pub retry_while_busy: bool,
}

impl PlannedStep {
    pub(crate) fn new(label: impl Into<String>, program: PlanProgram, args: Vec<String>) -> Self {
        Self {
            label: label.into(),
            program,
            args,
            cancellable: true,
            retry_other_source: false,
            best_effort: false,
            retry_other_id: None,
            retry_while_busy: false,
        }
    }

    fn uncancellable(mut self) -> Self {
        self.cancellable = false;
        self
    }

    /// 「资源还被占着」时等一下再跑（见字段说明）。
    fn retry_while_busy(mut self) -> Self {
        self.retry_while_busy = true;
        self
    }

    /// 失败时换一个下载源再试一次（只给在线安装那一步用，见字段说明）。
    fn with_source_fallback(mut self) -> Self {
        self.retry_other_source = true;
        self
    }

    /// 失败时换成另一个 id 再试一次（见字段说明）。
    fn with_id_fallback(mut self, id: impl Into<String>) -> Self {
        self.retry_other_id = Some(id.into());
        self
    }

    /// 失败**不算安装失败**（收尾步骤用，见字段说明）。
    fn best_effort(mut self) -> Self {
        self.best_effort = true;
        self
    }

    /// 界面上那一行怎么写。
    ///
    /// `Wsl` / `Curl` 给**真命令行**（用户能直接复制到终端复现）；
    /// 其余几种不是一条命令，就如实给 label —— 编一条假的 `mkdir`
    /// 只会让人以为我们真的跑了 `mkdir`。
    pub fn line(&self) -> String {
        match self.program {
            PlanProgram::Wsl => {
                format!("wsl {}", crate::util::format_args_for_display(&self.args))
            }
            PlanProgram::Curl => {
                format!("curl {}", crate::util::format_args_for_display(&self.args))
            }
            _ => self.label.clone(),
        }
    }

    /// 这一步会不会起一个外部进程。
    pub fn spawns_process(&self) -> bool {
        matches!(self.program, PlanProgram::Wsl | PlanProgram::Curl)
    }
}

/// 装一个发行版的**完整计划**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallPlan {
    /// 最终发行版名。
    pub name: String,
    /// 最终安装目录（可能为空：在线安装且不改名时可以交给 WSL 决定）。
    pub install_dir: String,
    /// 按顺序执行的步骤。
    pub steps: Vec<PlannedStep>,
    /// 给用户看的额外说明（代价、前提）。不参与执行。
    pub notes: Vec<String>,
}

impl InstallPlan {
    /// 预览：带序号的步骤列表。
    pub fn preview_lines(&self) -> Vec<String> {
        self.steps
            .iter()
            .enumerate()
            .map(|(idx, step)| format!("{}. {}", idx + 1, step.line()))
            .collect()
    }

    /// 会真正起进程的那几步（日志里用得上）。
    pub fn process_steps(&self) -> impl Iterator<Item = &PlannedStep> {
        self.steps.iter().filter(|s| s.spawns_process())
    }

    /// 这个计划在**重定位之前**可能注册成的名字（只有在线安装会给出非空结果）。
    ///
    /// 在线安装先装成清单里的 `id`，失败时还可能换成兄弟 `id`
    /// （见 [`PlannedStep::retry_other_id`]），重定位之后才变成
    /// [`InstallPlan::name`]。所以这段时间里 `wsl -l -v` 上是**源名**。
    ///
    /// # 为什么界面需要它
    ///
    /// 安装期间界面上记的是最终名字（`InstallProgress::name`），而
    /// `AppState::busy_with` 拿它去查"这个发行版是不是正在被安装"。
    /// 改名安装时两边不一致 → **查不到** → 重定位那一步
    /// （`--export` → `--unregister` → `--import`）全程都能被别的按钮
    /// （终止 / 删除 / 打开终端）打断，而它的说明写着"中途停下等于把刚装好的删了"
    /// （见 `cmd::install` 的模块说明）。本机实测过：改名安装的中间态里，
    /// 列表上确实会先出现源名那一行。
    ///
    /// 从**步骤**里读、而不是从 [`InstallSpec`] 再算一遍：计划是唯一的事实来源，
    /// 而"换 id 重试"那一手挂在步骤上 —— 重算就会漏掉它，于是
    /// "换了兄弟 id 才装上"那条路又失去保护。
    pub fn source_names(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();

        for step in &self.steps {
            if step.program != PlanProgram::Wsl {
                continue;
            }
            // `--install -d <id>`：`-d` 后面那个才是会被注册的名字。
            let Some(at) = step.args.iter().position(|arg| arg == "-d") else {
                continue;
            };

            // 与最终名字相同的不必重复记（`busy_with` 本来就会查最终名字）；
            // 判据用 `same_name`，和 `busy_with` 那边保持一致。
            let mut push = |name: &str| {
                let name = name.trim();
                if name.is_empty()
                    || crate::model::distro::same_name(name, &self.name)
                    || out.iter().any(|seen| seen == name)
                {
                    return;
                }
                out.push(name.to_owned());
            };

            if let Some(id) = step.args.get(at + 1) {
                push(id);
            }
            if let Some(sibling) = step.retry_other_id.as_deref() {
                push(sibling);
            }
        }

        out
    }
}
/// 按 [`InstallSpec`] 拼出执行计划。
///
/// 校验不过时返回**能直接给用户看**的错误（和 [`preflight`] 用的是同一套规则）。
pub fn plan(spec: &InstallSpec, ctx: &PlanContext) -> Result<InstallPlan, String> {
    let errors = validate(spec, ctx);
    if !errors.is_empty() {
        return Err(errors.join("；"));
    }

    let name = spec.name.trim().to_owned();
    let dir = spec.effective_install_dir(ctx);
    let mut steps: Vec<PlannedStep> = Vec::new();
    let mut notes: Vec<String> = Vec::new();

    match &spec.source {
        InstallSource::Tar { path } => {
            let path = path.trim();
            let dir = dir.clone().unwrap_or_default();
            steps.push(create_dir_step("安装目录", dir.as_str()));
            steps.push(PlannedStep::new(
                format!("把 {path} 展开成发行版 {name}"),
                PlanProgram::Wsl,
                own(&[
                    "--import",
                    name.as_str(),
                    dir.as_str(),
                    path,
                    "--version",
                    "2",
                ]),
            ));
            notes.push(
                "`--import` 只是把文件系统铺开，**不会**做首次启动初始化（不建默认用户）。\
                 想要那一步请用「从 .wsl / 文件安装」。"
                    .to_owned(),
            );
        }
        InstallSource::Vhdx { path } => {
            let path = path.trim();
            let dir = dir.clone().unwrap_or_default();
            steps.push(create_dir_step("安装目录", dir.as_str()));
            steps.push(PlannedStep::new(
                format!("把 {path} 作为虚拟磁盘导入并拷贝到 {dir}"),
                PlanProgram::Wsl,
                own(&[
                    "--import",
                    name.as_str(),
                    dir.as_str(),
                    path,
                    "--vhd",
                    "--version",
                    "2",
                ]),
            ));
            notes.push(
                "`--vhd` 会把给定的磁盘**拷贝**一份到安装目录（原始文件不动）。\
                 几百 MB 到几 GB 的盘会有一次全量拷贝。"
                    .to_owned(),
            );
        }
        InstallSource::File { path } => {
            let path = path.trim();
            let mut args = own(&["--install", "--from-file", path, "--name", name.as_str()]);
            if let Some(dir) = dir.as_deref() {
                args.push("--location".to_owned());
                args.push(dir.to_owned());
            }
            steps.push(PlannedStep::new(
                format!("交给 WSL 安装器从 {path} 安装（会做首次启动初始化）"),
                PlanProgram::Wsl,
                args,
            ));
            notes.push(
                "这条路径**没有** `--version` 选项（实测 `wsl.exe --help`），\
                 版本由安装器自己决定 —— 我们指定不了。"
                    .to_owned(),
            );
        }
        InstallSource::Online {
            id,
            launch,
            web_download,
            fallback_id,
        } => {
            let id = id.trim().to_owned();
            let same_name = id == name;
            let target_dir = dir.clone();
            let fallback_id = fallback_id
                .as_deref()
                .map(str::trim)
                .filter(|candidate| !candidate.is_empty() && *candidate != id)
                .map(str::to_owned);

            // 装完要不要**再动一次**（改名 或 挪位置）——先算出来，因为
            // 它反过来决定要不要 `--no-launch`（见下）。
            let needs_relocate = !same_name || target_dir.is_some();

            let mut args = own(&["--install", "-d", id.as_str()]);
            if *web_download {
                args.push("--web-download".to_owned());
            }
            // ⚠️ **不传 `--location`** —— 这是实测出来的（2026-10-10，本机）：
            //
            // | 命令 | 结果 |
            // |---|---|
            // | `wsl --install -d Ubuntu --location D:\wsl --version 2 --no-launch` | 秒失败：`Wsl/InstallDistro/WININET_E_CANNOT_CONNECT` |
            // | `wsl --install -d Ubuntu --web-download --location D:\wsl …` | 同样秒失败 |
            // | `wsl --install -d Ubuntu --no-launch` | **能连上**（25 秒还在下载） |
            //
            // 带上 `--location` 时 WSL 走的是"自己把整包下到指定目录"那条通道，
            // 用的是不认系统代理的 WinINET；不带时走商店/清单那条，能通。
            // 参考实现也从不传 `--location` —— 它装完再搬（我们也一样，
            // 见下面那一步 `EnsureRelocated`，同名的走 `--manage --move`）。
            //
            // 代价：多一次搬运（同一个卷上是改名，跨卷是真的拷一遍）。
            // 好处：这条路能装上，而"直接装到目标目录"在这台机器上装不上。
            args.push("--version".to_owned());
            args.push(WSL_VERSION.to_string());
            // ⚠️ 要重定位时**必须** `--no-launch`，即使用户勾了"装完立即启动"。
            //
            // 原因（本机实测，2026-10-12）：`--install` 之后那个发行版的 VHDX
            // 还被 WSL 自己占着 —— 紧接着跑 `--manage <name> --move` 会
            // `Wsl/Service/MoveDistro/ERROR_SHARING_VIOLATION`，跑 `--export`
            // 同样可能撞上。而**启动它只会让这件事更糟**：运行中的发行版
            // 本来就搬不动，于是必然退到"导出 → 注销 → 导入"那条要全量拷贝的路。
            // 顺带一提，新发行版被启动时会进 OOBE（建默认用户），而我们的
            // stdin 是 `Stdio::null()`（见 `cli::build_command`）—— 用户根本
            // 看不到那个交互界面。
            //
            // 用户勾的那个开关**没有被无声吞掉**：预览里的说明会讲清这一次为什么
            // 不启动（见下面那段 `notes`）。
            if !launch || needs_relocate {
                args.push("--no-launch".to_owned());
            }
            let install_step =
                PlannedStep::new(format!("从在线源安装 {id}"), PlanProgram::Wsl, args)
                    // 商店 ⇄ GitHub 自动换一次：两条通道在不同网络环境下会各挂一条。
                    .with_source_fallback();
            // 裸名字（`Ubuntu`）→ 带版本号的兄弟（`Ubuntu-26.04`）再试一次。
            // 实测（2026-10-11，本机）：`wsl -l -o` 排在第一位的裸 `Ubuntu` 是清单里
            // 那条**旧的商店包**，`wsl --install -d Ubuntu` 2 秒就
            // `WININET_E_CANNOT_CONNECT`，而 `-d Ubuntu-24.04` / `-d Debian` 照常下载。
            // 参考实现的列表第一项是「Ubuntu 26.04 LTS」，所以它一直没撞上这个坑。
            let install_step = match fallback_id.clone() {
                Some(sibling) => install_step.with_id_fallback(sibling),
                None => install_step,
            };
            steps.push(install_step);

            steps.push(PlannedStep::new(
                format!("等 {id} 注册完成"),
                PlanProgram::WaitRegistered,
                vec![id.clone()],
            ));

            if needs_relocate {
                let target_dir = target_dir.unwrap_or_default();
                let temp_tar = format!(
                    "{}\\wslc-panel-{}-{}.tar",
                    ctx.temp_dir.trim_end_matches(['\\', '/']),
                    sanitize_name(&id),
                    ctx.stamp
                );
                steps.push(
                    PlannedStep::new(
                        format!("必要时把 {id} 重定位成 {name}（装到 {target_dir}）"),
                        PlanProgram::EnsureRelocated,
                        // `target_dir` 下面那句 note 还要用，所以这里 clone 一份
                        vec![
                            id.clone(),
                            name.clone(),
                            target_dir.clone(),
                            temp_tar.clone(),
                        ],
                    )
                    .uncancellable(),
                );
                if !same_name {
                    notes.push(format!(
                        "在线清单里没有「{name}」这个名字，所以会绕一下：先装成「{id}」，\
                         再 `--export` → `--unregister` → `--import` 改成「{name}」。\
                         这一段走完之后**不能取消**（取消等于把刚装好的删掉），\
                         中途会用到 {} 作中转。",
                        temp_tar
                    ));
                } else {
                    notes.push(format!(
                        "装的时候**不带 `--location`**（带上它 WSL 会改走一条不认系统代理的\
                         下载通道，实测在这台机器上直接报 `WININET_E_CANNOT_CONNECT`），\
                         所以装完会用注册表核实它落在哪儿，需要的话用 `--manage {id} --move` \
                         搬到 {target_dir}。"
                    ));
                }
                if *launch {
                    notes.push(format!(
                        "你勾了「装完立即启动」，但这一次要先{}，所以命令里仍然带了 \
                         `--no-launch`：运行中的发行版搬不动（实测紧接着安装之后 \
                         `--move` 会报 `ERROR_SHARING_VIOLATION`）。装完在实例列表里点\
                         「打开终端」就能进去。",
                        if same_name { "挪位置" } else { "改名" }
                    ));
                }
            }

            if let Some(sibling) = fallback_id.as_deref() {
                notes.push(format!(
                    "「{id}」在清单里是个**不带版本号的名字**（实测这类条目在这台机器上\
                     装不动：`wsl --install -d {id}` 两秒就报 `WININET_E_CANNOT_CONNECT`），\
                     所以上面那条命令失败时会自动换成「{sibling}」再试一次，\
                     装好之后的名字仍然是你要的「{name}」。"
                ));
            }
        }
        InstallSource::Mirror {
            url,
            mirror,
            release,
            format,
        } => {
            let url = url.trim();
            let dir = dir.clone().unwrap_or_default();
            let bundle = InstallSource::mirror_is_bundle(format, url);
            let extension = if bundle {
                ".wsl"
            } else if format.eq_ignore_ascii_case("tar.gz")
                || url.to_lowercase().ends_with(".tar.gz")
            {
                ".tar.gz"
            } else {
                ".tar.xz"
            };
            let temp_dir = ctx.temp_dir.trim_end_matches(['\\', '/']);
            let temp_file = format!(
                "{temp_dir}\\wslc-panel-rootfs-{}-{}{}",
                sanitize_name(release),
                ctx.stamp,
                extension
            );

            // ⚠️ **先建临时目录，再下载**。`curl -o` 不会自己建目录：父目录不存在时
            // 它给的是 `curl: (23) client returned ERROR on write of N bytes`
            // （本机实测，退出码 23），而那句话里**一个字都没提目录** ——
            // 用户只会当成网络问题去查。
            //
            // 而 `%TEMP%\wslc-panel` 平时**并不存在**：全仓库只有在线安装的
            // 重定位会顺手 `create_dir_all` 它（见 `cmd::install::ensure_relocated`），
            // 而镜像源这条路的第一步就是下载。更麻烦的是 `%TEMP%` 会被
            // Storage Sense / 磁盘清理回收 —— 所以这不是"只有第一次才会踩"，
            // 而是"某天早上突然又踩一次"（本机实测：目录不在时 curl 退出码 23，
            // 建出来之后同一条命令 200 / 19 MB）。
            //
            // 放在计划里而不是执行器里：预览和执行读的是同一份计划（见模块说明），
            // 执行器里偷偷多做一个动作就等于界面上那句预览在说谎。
            steps.push(create_dir_step("临时目录", temp_dir));

            steps.push(PlannedStep::new(
                format!(
                    "从 {mirror} 下载{}（{release}）",
                    if bundle { " .wsl 包" } else { " rootfs" }
                ),
                PlanProgram::Curl,
                crate::mirrors::download_args(url, temp_file.as_str()),
            ));

            if bundle {
                // `.wsl` 包 → 交给 WSL 自己的安装器（`--from-file`）。
                // `--name` 直接给了最终名字，所以**不需要**改名重定位；
                // 但 `--location` 不一定被采纳，所以位置仍然要核实一次。
                let mut args = own(&[
                    "--install",
                    "--from-file",
                    temp_file.as_str(),
                    "--name",
                    name.as_str(),
                ]);
                if !dir.is_empty() {
                    args.push("--location".to_owned());
                    args.push(dir.clone());
                }
                steps.push(PlannedStep::new(
                    format!("把下载的 .wsl 包安装成发行版 {name}"),
                    PlanProgram::Wsl,
                    args,
                ));
                if !dir.is_empty() {
                    // ⚠️ 中转 tar 的路径**必须**是个真路径。
                    //
                    // 这里原来传的是 `String::new()`：`--manage --move` 一失败
                    // 紧接着 `--install` 之后跑
                    // `--move` 会失败（返回 -1）"，所以这条回退路径是**设计上
                    // 会出现**的），就退到「导出 → 注销 → 导入」，
                    // 而那时跑的是 `wsl --export <name> ""` ——
                    // 只会回一句看不懂的参数错误，整条退路等于废掉
                    // （数据不会丢：export 在 unregister 之前就失败了）。
                    //
                    // 前缀与上面下载的临时文件（`wslc-panel-rootfs-…`）错开，
                    // 免得重定位把还没用完的下载产物覆盖掉。
                    let temp_tar = format!(
                        "{temp_dir}\\wslc-panel-relocate-{}-{}.tar",
                        sanitize_name(&name),
                        ctx.stamp
                    );
                    steps.push(
                        PlannedStep::new(
                            format!("必要时把 {name} 挪到 {dir}"),
                            PlanProgram::EnsureRelocated,
                            vec![name.clone(), name.clone(), dir.clone(), temp_tar],
                        )
                        .uncancellable(),
                    );
                }
                notes.push(format!(
                    "下载的是镜像站上的官方 **.wsl 包**（{url}）——\
                     它和 tar 不一样：走 WSL 自己的安装器，会做首次启动初始化（建默认用户）。\
                     装完会把临时文件删掉。"
                ));
            } else {
                steps.push(create_dir_step("安装目录", dir.as_str()));
                steps.push(PlannedStep::new(
                    format!("把下载的 rootfs 展开成发行版 {name}"),
                    PlanProgram::Wsl,
                    own(&[
                        "--import",
                        name.as_str(),
                        dir.as_str(),
                        temp_file.as_str(),
                        "--version",
                        "2",
                    ]),
                ));
                notes.push(format!(
                    "下载的是镜像站上的官方 rootfs（{url}）；\
                     `--import` 只是铺开文件系统，**不做**首次启动初始化。\
                     装完会把临时文件删掉。"
                ));
            }

            steps.push(PlannedStep::new(
                format!("删掉临时文件 {temp_file}"),
                PlanProgram::RemoveFile,
                vec![temp_file.clone()],
            ));
        }
    }

    // 收尾：用户自己在 `.wslconfig` 里要求了稀疏 VHD，就照着做。
    if ctx.wslconfig_sparse {
        // ⚠️ 必须带 `--allow-unsafe`：WSL 3.0.1.0 现在**默认拒绝**开稀疏盘
        // （理由是"潜在的数据损坏"），报错里自己给出了该加的开关：
        // `wsl.exe --manage <分发> --set-sparse true --allow-unsafe`。
        // 用户的 `.wslconfig` 明确要了稀疏盘，所以照着它给的命令来。
        //
        // 这一步是 `best_effort`：WSL 哪天又改主意不给开，也**不能**把
        // "已经装好的发行版"判成安装失败（这个坑也踩过 —— 界面报"安装失败"，
        // 其实发行版好好地在那儿）。
        //
        // ⚠️ 但它同时是 `retry_while_busy`：**紧接着 `--install` 跑的时候
        // VHDX 还被 WSL 自己开着**（实测 2026-10-12：`Wsl/Service/
        // ERROR_SHARING_VIOLATION`，退出码 -1）。那一步是 `best_effort`，
        // 所以失败只会记一条日志 —— 而用户明明在 `.wslconfig` 里要了稀疏盘，
        // 结果是**静默不生效**（界面上没有第二个地方能开它）。
        // 等一下再跑就能成，所以这里给它重试。
        steps.push(
            PlannedStep::new(
                format!("按 .wslconfig 把 {name} 的 VHD 设成稀疏（自动回收空间）"),
                PlanProgram::Wsl,
                own(&[
                    "--manage",
                    name.as_str(),
                    "--set-sparse",
                    "true",
                    "--allow-unsafe",
                ]),
            )
            .best_effort()
            .retry_while_busy(),
        );
        notes.push(
            "你的 `%USERPROFILE%\\.wslconfig` 里开了 `[experimental] sparseVhd`，\
             所以装完会补一条 `--set-sparse true --allow-unsafe`\
             （WSL 目前默认拒绝开稀疏盘，要显式加 `--allow-unsafe`）。\
             这一步没做成也不影响装好的发行版 —— 刚装完那一刻磁盘可能还被 WSL \
             占着，它会等一会儿重试几次。"
                .to_owned(),
        );
    }

    // `--import` / `--install` 都不接受 `--set-default`，所以它是最后**单独**一条。
    if spec.set_default {
        steps.push(PlannedStep::new(
            format!("把 {name} 设为默认发行版"),
            PlanProgram::Wsl,
            own(&["--set-default", name.as_str()]),
        ));
    }

    Ok(InstallPlan {
        name,
        install_dir: dir.unwrap_or_default(),
        steps,
        notes,
    })
}

/// 建目录的步骤。
///
/// `label` 说明建的是**哪个**目录（`"安装目录"` / `"临时目录"`）：计划里两处都要建，
/// 而预览那一行必须能分辨它们 —— 原先这里写死了"创建安装目录"，临时目录那一步
/// 会看着像在重复建安装目录。
fn create_dir_step(label: &str, dir: &str) -> PlannedStep {
    PlannedStep::new(
        format!("创建{label} {dir}"),
        PlanProgram::CreateDir,
        vec![dir.to_owned()],
    )
}

/// `&[&str]` → `Vec<String>`（拼参数时到处都要，起个短名字）。
fn own(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

/// 临时目录的默认位置（`%TEMP%\wslc-panel`），供界面事先展示。
pub fn default_temp_dir() -> PathBuf {
    std::env::temp_dir().join("wslc-panel")
}
