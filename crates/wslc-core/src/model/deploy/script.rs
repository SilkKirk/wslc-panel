//! 生成在容器里执行的 shell 脚本（准备目录 / 复制 / 起停 / 判活 / 清理）。

use std::path::{Path, PathBuf};

use super::*;

/// 进程形态的日志文件名（放在目标目录里）。
pub const LOG_FILE: &str = "app.log";
/// 进程形态的 pidfile 名（放在目标目录里）。
pub const PID_FILE: &str = "app.pid";

/// 判活脚本输出的两个哨兵值。
///
/// 用**定值**而不是去猜容器里的语言环境：`wslc` 的输出可能是中文，
/// 镜像里的 `sh` 可能是 dash / busybox，唯一稳定的东西是我们自己 echo 出来的。
pub const STATUS_RUNNING: &str = "__WSLC_RUNNING__";
/// 见 [`STATUS_RUNNING`]。
pub const STATUS_STOPPED: &str = "__WSLC_STOPPED__";
/// 「目标目录不存在」的哨兵（数文件那条脚本用）。
pub const NO_DIR: &str = "__WSLC_NO_DIR__";
/// 「还没有日志文件」的哨兵。
pub const NO_LOG: &str = "__WSLC_NO_LOG__";
/// 「目标目录里已经有东西，而它不是我们铺的」的哨兵。
///
/// 见 [`prepare_target_script`]：这一条是 v0.9.8 加的，代价是一次**真实的灾难**
/// （部署到 `/opt/java` 把镜像自带的 JDK 删了）。
pub const TARGET_NOT_EMPTY: &str = "__WSLC_TARGET_NOT_EMPTY__";

/// 目标目录非空、且不是我们铺的时的**提醒**（给用户看的那一句）。
///
/// ⚠️ 这是一句**提醒**，不是错误：面板**照常部署**（只替换自己铺的那些名字，
/// 见 [`removals_script`]），只是告诉用户"这个目录里本来有别的东西，我没动它们"。
///
/// # 为什么从"拦"改成"提醒"（v0.9.11 改）
///
/// v0.9.9 是**拒绝部署**的（`exit 0` + 哨兵）。当时它还是数据安全的最后一道防线
/// —— 再往前一版是整目录 `find -delete` 清空。而 v0.9.10 改成逐名删除之后，
/// **它拦的已经不是危险了**：`/opt/java` 里那个 `openjdk/` 根本不在删除列表里。
///
/// 代价实测：用户删掉旧记录、重建容器之后重新部署，目标目录被预填成 `/opt/java`
/// —— 面板直接拒绝，而那次部署**本来是安全的**，`java` 也不会丢。
/// 挡住的只是用户自己。所以现在：**干完再说一句**。
///
/// ⚠️ **第一行要能独立成立**：提示条（toast）只取第一行并截断到 120 字符。
pub fn target_not_empty_hint(target: &str) -> String {
    format!(
        "目标目录 {target} 里本来就有别的东西（不是面板铺的）—— 面板只替换自己那几个文件，\
         没动它们。\n\
         那里面可能是**镜像自己要用的文件**：实测 `eclipse-temurin` 的 JDK 就在 \
         `/opt/java/openjdk`（面板不会删它）。想把目录清干净，用「终端」自己来；\
         或者换个目标目录（比如 `/opt/<你的应用名>`、`/app/...`）。"
    )
}

/// 拼"**只删我们自己铺的那些名字**"的片段（不清空整个目录）。
///
/// # ⚠️ 为什么不再 `find <目标> -mindepth 1 -delete`（v0.9.10 改）
///
/// 整目录清空造成过一次**真实的数据损坏**：应用名就叫 `java`，默认目标目录
/// `/opt/java` 正好是 `eclipse-temurin:8-jre-alpine` 放 JDK 的地方
/// （`/opt/java/openjdk`）—— 一个 `find -delete` 就把容器里的 `java` 删了，
/// 应用从此起不来。白名单（[`safe_target`]）挡不住这种事：它只知道
/// "`/opt` 底下允许"，**不知道镜像在 `/opt` 底下放了什么**。
///
/// v0.9.9 加过一层"不是我们铺的就拒绝"，但那层依赖**本地存档里有没有记录**
/// —— 而记录是**按容器名**索引的，容器删掉重建之后记录还在，
/// 于是"重新部署"又会把新容器里的 JDK 删一遍（同一个坑的第二只脚）。
///
/// 所以现在改成逐个删**我们自己写进去的名字**：
///
/// - `members`：这次要铺进去的成员名（`cmd::deploy::member_names` 算得出来），
///   外加调用方从存档里带上的**上一次**铺过的名字（`DeployRecord::members`），
///   后者把"上一次有、这一次没有"的旧文件也收拾掉；
/// - 外加进程形态的 `app.log` / `app.pid` —— 那两个也是我们写的；
/// - **其余一律不碰**：镜像自带的文件、用户自己放的东西都留着。
///
/// 代价要说清（这是刻意的取舍）：结果目录**不再等于源目录**，
/// 目录里不是我们放的东⻄会留下。比起"悄悄删掉一个 JDK"，
/// 多留几个文件是划算的。
///
/// 名字里出现 `/`、`\`、`..` 或空串时**跳过**：成员名来自本地文件名，
/// 不该带路径；真带了说明上游错了，宁可少删也不能删到目标目录外面去。
pub fn removals_script(target: &str, members: &[String]) -> String {
    let mut names: Vec<String> = Vec::new();
    for member in members {
        let member = member.trim();
        if !is_safe_member(member) {
            continue;
        }
        names.push(shell_quote(&container_join(target, member)));
    }
    // 日志与 pidfile 无条件带上（它们的名字是面板定的，不是一个"成员"）
    for fixed in [LOG_FILE, PID_FILE] {
        names.push(shell_quote(&container_join(target, fixed)));
    }
    format!("rm -rf {}", names.join(" "))
}

/// 成员名能不能安全地拼成 `<目标>/<名字>`。
fn is_safe_member(name: &str) -> bool {
    !name.is_empty() && !name.contains('/') && !name.contains('\\') && !name.contains("..")
}

/// 准备目标目录：建出来，然后删掉**我们自己铺的那些名字**（[`removals_script`]）。
///
/// # ⚠️ 为什么不能写 `rm -rf <目标>`
///
/// nginx 那种容器的 Web 根是一个**卷挂载点**。本机实测
/// （`wslc inspect nginx`）：`/usr/share/nginx/html` 挂在 `/dev/sdc` 上
/// （`/proc/mounts` 里查得到，设备号 2080，和根目录的 35 不是一个）。
/// 而对挂载点 `rm -rf` 会**返回 1**：
///
/// ```text
/// rm: cannot remove '/usr/share/nginx/html': Device or resource busy
/// ```
///
/// 它**确实清空了内容**，但退出码是 1 —— 于是 `&&` 链在那里就断了，
/// 后面的步骤根本不会跑。表现是"点了部署，界面上说失败了，什么都没有"。
///
/// 所以只删**里面的名字**、不碰目录本身（[`removals_script`]）。
///
/// # `ours`：只决定"要不要提醒一句"，**不决定拦不拦**（v0.9.11 改）
///
/// | `ours` | 行为 |
/// |---|---|
/// | `true`（本地存档里有这个目录的记录） | 直接按 [`removals_script`] 收拾 + 铺，什么都不说 |
/// | `false`（第一次往这个目录铺） | 目录非空就先 echo [`TARGET_NOT_EMPTY`]（**提醒**），然后**照常铺** |
///
/// ⚠️ v0.9.9 那种"非空就拒绝"已经去掉了：逐名删除之后它拦的**不是危险**，
/// 而是用户自己的正常部署（实测：删掉旧记录 + 重建容器之后重新部署
/// `/opt/java` 被硬拦，而那次部署本来完全安全）。理由写在
/// [`target_not_empty_hint`] 那段。**哨兵后面没有 `exit`** —— 它只是给调用方
/// 一个"要不要说一句"的信号，脚本一定往下走。
///
/// ⚠️ 这一步必须在 `cp` **之前**跑，两条理由：
///
/// 1. 它（通过 `exec`）会**报出"容器没在运行"** —— 而 `cp` 在那种情况下
///    **静默成功**（实测 exit 0、不输出、什么都不做）；
/// 2. `cp` **要求目标目录已经存在**（实测不存在的路径报
///    `ERROR_PATH_NOT_FOUND`），它不会替我们建。
pub fn prepare_target_script(target: &str, ours: bool, members: &[String]) -> String {
    let target_q = shell_quote(target);
    let notice = if ours {
        String::new()
    } else {
        // `ls -A` 认隐藏文件（`.hidden` 也要算"已经有东西"），
        // 目录不存在时它自己报错但我们不看退出码（`2>/dev/null`）——
        // 那种情况本来就没什么可提醒的。
        //
        // ⚠️ **不要加 `exit`**：这一句只是"要不要说一句"的信号，脚本必须往下走
        // （v0.9.9 那里是 `exit 0`，等于拒绝部署 —— 见上面那段为什么改掉）。
        // 也**不要**用非零退出码表达"目录非空"：那会被 `run_script` 变成
        // "参数 + stderr" 的原始错误，调用方既读不到哨兵、又拿到一句看不懂的失败。
        format!(
            "if test -d {target_q} && test -n \"$(ls -A {target_q} 2>/dev/null)\"; then \
             echo {TARGET_NOT_EMPTY}; fi; "
        )
    };
    let removals = removals_script(target, members);
    format!("{notice}mkdir -p {target_q} && {removals}")
}

/// `chmod +x <路径>`。
///
/// 见模块文档：`cp` 过去的文件权限是 **`666`** —— 任何"要被执行的文件"
/// （Go 二进制、shell 脚本）不补这一步就会稳定失败在 `Permission denied`
/// （exit 126），而部署本身看起来是**成功**的（文件在、大小对）。
pub fn chmod_exec_script(path: &str) -> String {
    format!("chmod +x {}", shell_quote(path))
}

/// 一次部署要 `cp` 哪些本地路径。
///
/// - **文件来源**：就是它自己 → 落到 `<目标>/<文件名>`；
/// - **目录来源**：它的**每一个顶层条目** → 落到 `<目标>/<条目名>`。
///
/// # ⚠️ 为什么目录要逐个顶层条目去 cp
///
/// `wslc container cp` 复制目录时**会把目录名一起带过去**（实测：
/// `cp <本地>\site nginx:/tmp/d1` 的结果是 `/tmp/d1/site/…`）。
/// 直接整目录复制的话目标里会平白多一层，而 nginx 的 Web 根多一层就等于
/// **站点打不开**（`/usr/share/nginx/html/site/index.html` 不是 nginx 认的入口）。
///
/// 逐个顶层条目复制，落到目标里的才是目录的**内容**。
///
/// `entries` 由调用方给（要读磁盘，不能在这个纯逻辑模块里做）——
/// 只传**顶层**那一层，且必须**非空**（空目录部署出来就是空的，调用方要能说出
/// 这件事而不是当作成功）。
pub fn copy_members(source: &Path, kind: SourceKind, entries: &[PathBuf]) -> Vec<PathBuf> {
    match kind {
        SourceKind::File => vec![source.to_path_buf()],
        SourceKind::Directory => entries.to_vec(),
    }
}

/// 拼 `wslc container cp` 的参数。
///
/// 形状：`container cp -q <本地路径> <容器>:<目标目录>`。
///
/// - 位置参数在**开关后面**（`wslc` 的位置参数解析遇到第一个非选项就只认
///   位置参数了，和 `exec` / `logs` 那两条不变量同理）；
/// - `-q` 抑制进度输出：实测非 tty 时本来就没有输出，加它是为了**防止**
///   将来版本往 stdout 里写进度条（那会把调用方的输出解析搅乱）；
/// - ⚠️ 目标**必须是已存在的目录**（实测不存在的路径报
///   `ERROR_PATH_NOT_FOUND`）→ 调用方先跑 [`prepare_target_script`]。
pub fn cp_args(source: &Path, container: &str, target_dir: &str) -> Vec<String> {
    vec![
        "container".to_owned(),
        "cp".to_owned(),
        "-q".to_owned(),
        source.to_string_lossy().to_string(),
        format!("{container}:{target_dir}"),
    ]
}

/// 启动脚本：把应用甩到后台，并把 pid 写进 pidfile。
///
/// ⚠️ `command` 是**原样**进脚本的（**不做** [`shell_quote`]）。这是刻意的，
/// 理由和 `wslc_panel_core::state::exec_argv` 那段一样：用户在这一格里写的就是
/// 他要执行的**命令行**（`java -jar x.jar --spring.profiles.active=prod`），
/// 把它当数据引号起来就没法用了。界面上必须把这件事说出来。
///
/// 路径（`dir` / 日志 / pidfile）则**一律**引号 —— 那些是数据。
pub fn start_script(dir: &str, command: &str) -> String {
    let dir_q = shell_quote(dir);
    let log_q = shell_quote(&container_join(dir, LOG_FILE));
    let pid_q = shell_quote(&container_join(dir, PID_FILE));
    // ⚠️ 大括号不能省：`&` 的优先级最低（见模块文档那条实测）。
    // `{{` / `}}` 是 `format!` 的转义，出来是单个花括号。
    format!(
        "mkdir -p {dir_q} && cd {dir_q} && {{ nohup {command} > {log_q} 2>&1 & echo $! > {pid_q}; }}"
    )
}

/// 判活脚本：pidfile 在、那个 pid 还活着、**而且它不是僵尸** → `__WSLC_RUNNING__`。
///
/// # ⚠️ 为什么要多看一眼 `/proc/<pid>/stat`（v0.9.8 加）
///
/// `kill -0` 对**僵尸进程**也返回 0 —— 而"启动失败"恰好会留下一个僵尸：
///
/// ```text
/// $ nohup java -jar app.jar > app.log 2>&1 & echo $! > app.pid
/// nohup: failed to run command 'java': No such file or directory
/// $ ps -o pid,ppid,args
///   PID  PPID COMMAND
///     1     0 tail -f /dev/null     ← PID 1 是面板吊容器用的 tail，它**不回收子进程**
///    29     1 [nohup]               ← 僵尸：`kill -0 29` 仍然是成功
/// ```
///
/// 于是 pidfile 里那个 pid "还活着"，界面上一直显示**运行中**（实机报过来的就是
/// 这个："java 容器日志和部署的 java 应用日志都看不到"，其实进程早就死了）。
///
/// 修法：读 `/proc/<pid>/stat` 的**第三个字段**（进程状态），`Z` 就当死。
/// 这里不用 `awk`：镜像里不一定有它（busybox 有、精简镜像没有），
/// 而纯 shell 剥掉 `comm` 那一段只需要 `${s##*) }` 一步 ——
/// ⚠️ 必须用 `##`（**最长**匹配）而不是 `#`：`comm` 自己可能带 `)` 甚至 `) `，
/// 短的匹配会停在 comm 中间，剥出来的"状态"就成了 comm 的残渣（实测这一条是推的，
/// 但代价只有两个字符）。
///
/// `test -f X && kill -0 … && … && echo R || echo S` 的结合性正是我们要的
/// （链上任何一环失败都会走到 `echo S`）。
pub fn status_script(pid_path: &str) -> String {
    let pid_q = shell_quote(pid_path);
    format!(
        "test -f {pid_q} && p=$(cat {pid_q}) && kill -0 \"$p\" 2>/dev/null && \
         s=$(cat /proc/\"$p\"/stat 2>/dev/null) && s=${{s##*) }} && st=${{s%% *}} && \
         [ -n \"$st\" ] && [ \"$st\" != Z ] && echo {STATUS_RUNNING} || echo {STATUS_STOPPED}"
    )
}

/// 停止脚本：发 `TERM` 然后删掉 pidfile。
///
/// ⚠️ v0.9 **不做 `kill -9`**。容器里**没有 `ps`、没有 `pkill`**（实测），
/// 要确认"它是不是真的没退"得自己扫 `/proc`，收益不抵复杂度。
/// 界面上把"只发 TERM"这件事写出来。
///
/// 用 `if test -f` 包着 `kill`：pidfile 不在时 `kill $(cat …)` 会因为**没有参数**
/// 而报一句用法错误（那行会进 stderr，被用户当成"停止失败"）。
pub fn stop_script(pid_path: &str) -> String {
    let pid_q = shell_quote(pid_path);
    format!(
        "if test -f {pid_q}; then kill $(cat {pid_q}) 2>/dev/null; fi; rm -f {pid_q}; echo {STATUS_STOPPED}"
    )
}

/// 看日志：最后 `lines` 行；没有日志文件时输出哨兵 [`NO_LOG`]。
///
/// `|| echo NO_LOG` 有个已知的副作用：`test -f` 过了但 `tail` 自己失败时也会
/// 打哨兵。那属于"文件在但读不了"的怪情况，按"没有日志"处理可以接受。
pub fn tail_script(log_path: &str, lines: usize) -> String {
    let log_q = shell_quote(log_path);
    format!("test -f {log_q} && tail -n {lines} {log_q} || echo {NO_LOG}")
}

/// 数目标目录里的文件数（内容形态用它区分"没部署过"和"部署过了"）。
pub fn count_files_script(dir: &str) -> String {
    let dir_q = shell_quote(dir);
    format!("if test -d {dir_q}; then find {dir_q} -type f | wc -l; else echo {NO_DIR}; fi")
}

/// 数目标目录的**顶层条目**数。
///
/// 给"复制真的发生了吗"那条核对用（见 `crate::cmd::deploy::verify_copy`）。
///
/// ⚠️ 判据刻意用**顶层条目**而不是文件数：一个只有空子目录的**合法**来源，
/// 文件数本来就是 0，拿文件数判会把它误报成失败。而顶层条目数正好等于
/// 我们要复制过去的那些名字的个数。
///
/// 解析仍走 [`parse_file_count`]（它就是"解析 `wc -l` 的输出或 `NO_DIR` 哨兵"，
/// 与数的是文件还是条目无关）。
pub fn count_entries_script(target: &str) -> String {
    let target_q = shell_quote(target);
    format!(
        "if test -d {target_q}; then find {target_q} -mindepth 1 -maxdepth 1 | wc -l; else echo {NO_DIR}; fi"
    )
}

/// 重载 nginx（铺完配置 / 内容之后想让它立刻生效时用）。
///
/// `2>&1` 是必须的：nginx 把 `signal process started` 那句写在 **stderr**，
/// 不并过来用户在提示条上就什么也看不到。
pub fn reload_nginx_script() -> String {
    "nginx -s reload 2>&1".to_owned()
}

/// 一次 exec 同时拿到"跑没跑"和"目录里有多少文件"。
///
/// # 为什么要合成一条
///
/// 列表里**每一条**部署都要问一次状态，分两次 exec 就是两倍往返。
/// 两条脚本用 `;` 串起来，顺序固定（**进程状态在前、文件数在后**），
/// 解析按位置取 —— 见 [`parse_probe_output`]。
///
/// 内容形态没有进程可问（nginx 是容器自己的主进程，见 [`AppRuntime::kind`]），
/// 所以只数文件。
pub fn probe_script(kind: DeployKind, dir: &str) -> String {
    let count = count_files_script(dir);
    match kind {
        DeployKind::Content => count,
        DeployKind::Process => {
            let status = status_script(&container_join(dir, PID_FILE));
            format!("{status}; {count}")
        }
    }
}

/// 一次探测拿到的两件事。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeResult {
    /// 进程形态：跑没跑。内容形态恒为 `None`（没有属于我们的进程可问）。
    pub process: Option<ProcessStatus>,
    /// 目标目录里的文件数；目录不存在 → `None`（**不是** `Some(0)` ——
    /// "没部署过"和"部署了一个空目录"在界面上是两句话）。
    pub files: Option<u64>,
}

/// 解析 [`probe_script`] 的输出。
///
/// ⚠️ **按位置取，不能都取最后一行**：进程那条在前、文件数在后。
/// [`parse_process_status`] 和 [`parse_file_count`] 各自都找"最后一个非空行"
/// （它们服务的是单条脚本），拿它们直接套合并输出会双双落空。
pub fn parse_probe_output(kind: DeployKind, stdout: &str) -> ProbeResult {
    let lines: Vec<&str> = stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    match kind {
        DeployKind::Content => ProbeResult {
            process: None,
            files: lines.last().and_then(|l| parse_file_count(l)),
        },
        DeployKind::Process => ProbeResult {
            process: lines.first().and_then(|l| parse_process_status(l)),
            files: lines.last().and_then(|l| parse_file_count(l)),
        },
    }
}

/// 删除一次部署（「删除」按钮用）：只删**我们自己铺的那些名字**。
///
/// ⚠️ 不碰目录本身（理由见 [`prepare_target_script`]：目标可能是卷挂载点，
/// 对挂载点 `rm -rf` 返回 1），也**不碰别人的文件** —— 只删 `members`
/// （调用方从 `DeployRecord::members` 取，也就是上一次真的铺了什么）
/// 加上 `app.log` / `app.pid`。见 [`removals_script`] 那段"为什么要改"：
/// 整目录清空在 v0.9.8 把 `eclipse-temurin` 的 JDK 删掉过一次。
///
/// 用 `if test -d` 包着是因为目录可能已经不在（容器重建过）——
/// 那种情况**不算失败**，用户要的结果（里面没我们的东西了）已经成立了。
///
/// ⚠️ 这一步**不可撤销**，而且目标是卷挂载点时会删掉卷里的数据 ——
/// 所以界面上必须是二次确认，而且要把完整路径显示出来。
pub fn remove_script(target: &str, members: &[String]) -> String {
    let target_q = shell_quote(target);
    let removals = removals_script(target, members);
    format!("if test -d {target_q}; then {removals}; fi; echo {STATUS_STOPPED}")
}

// ---------------------------------------------------------------------------
// 参数拼装
// ---------------------------------------------------------------------------

/// 拼"在运行中的容器里执行一条命令"的参数（非交互）。
///
/// 形状 `exec <容器> <命令> …`：位置参数在开关**后面**（`wslc` 的位置参数解析
/// 遇到第一个非选项就只认位置参数了，和 `logs_args` 那条不变量同理）。
pub fn exec_args(container: &str, script: &str) -> Vec<String> {
    vec![
        "exec".to_owned(),
        container.to_owned(),
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        script.to_owned(),
    ]
}

// 这里**有意没有** `exec_i_args`（"拼 `exec -i` 的参数"）。
//
// 它不是没想过：`-i` 在容器名前面、实测 `wslc exec -i nginx sh -c '…'`
// 能收到管道里的字节。但传文件已经改用 `wslc container cp`（见 `cp_args`），
// 而"往容器里灌一段文本"这条路也没做 —— 因为
// `crate::cli::build_command` 写死 `Stdio::null()`，子进程的 stdin 是关的，
// 要用它得先给 `cli.rs` 加一条新路。
//
// **没有调用方就不要留在公共 API 里**：它只会让下一个人以为"这条路能用"。
// 真要用时再加，`exec_args` 就在上面、照着写即可。
