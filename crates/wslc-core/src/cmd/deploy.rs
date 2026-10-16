//! 「部署应用」里**会起进程**的那一半：准备目录 → 复制文件 → 起停判活看日志。
//!
//! 纯逻辑（脚本生成、参数拼装、输出解析、识别）都在 [`crate::model::deploy`]，
//! 这里只负责"拼参数 → 跑命令 → 把结果交出去"。拆法和
//! `model/mysql.rs` ↔ `cmd/mysql.rs` 一样。
//!
//! # 四条实测出来的硬约束（改动前先读一遍）
//!
//! 1. **搬运走 `wslc container cp`**。⚠️ 它**不在 `wslc --help` 的顶层命令表里**
//!    （顶层表没有 `cp`，`wslc cp` 直接报「无法识别的命令」）—— 查 wslc 的能力
//!    要看**子命令**那一层。两条硬要求：目标目录**必须先存在**
//!    （否则 `ERROR_PATH_NOT_FOUND`），以及目录来源会**多套一层目录名**
//!    （所以要逐个顶层条目复制，见 [`crate::model::deploy::copy_members`]）；
//! 2. ⚠️ **`cp` 在容器没运行时静默成功**：exit 0、不输出、什么都不做。
//!    所以 ① 先用 `exec` 准备目标目录（那条会老实报
//!    `WSLC_E_CONTAINER_NOT_RUNNING`），② 复制完**核对**一遍
//!    （[`verify_copy`]）—— 光看 `cp` 的退出码会得到"部署成功、容器里空的"；
//! 3. **`wslc exec -d` 不能用**：实测 `wslc exec -d nginx sh -c "sleep 5"`
//!    **耗时 5.08 秒** —— `-d` 在 `exec` 上不生效，它照样等命令结束。
//!    所以"启动应用"靠容器内的 `nohup … &`（实测 0.17 秒返回、进程活着）；
//! 4. **`cp` 过去的文件没有可执行位**（实测 `666`，目录倒是 `777`），
//!    所以可执行文件要补一句 `chmod +x`，否则稳定失败在
//!    `Permission denied`（exit 126）。

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::cli::Wslc;
use crate::error::{Error, Result};
use crate::model::deploy::{
    self, AppRuntime, DeployKind, ProbeResult, ProcessStatus, SourceKind,
};

/// 复制文件的超时。
///
/// 实测单个 8 MB 文件是亚秒级，但要按**体积**给余量：一个几 GB 的镜像层式
/// 产物在慢盘上可能几分钟。超时的后果是子进程被杀掉、复制到一半的东西留在
/// 容器里 —— 下一次部署会按存档里的成员名把它替换掉，所以不是不可恢复，
/// 但宁松不宁紧。
const COPY_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// 短命令（准备目录 / 判活 / chmod / 停止 / 看日志 / 重载）的超时。
///
/// 这些都不该超过几秒；给 60 秒是留足余量，同时保证界面不会永远停在"读取中"。
const QUICK_TIMEOUT: Duration = Duration::from_secs(60);

/// 看日志时默认取最后多少行。
pub const LOG_TAIL_LINES: usize = 200;

// ---------------------------------------------------------------------------
// 探测
// ---------------------------------------------------------------------------

/// 进页面时探测一次的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployProbe {
    /// 容器名（`inspect` 回来的那个，去掉了前导 `/`）。
    pub container: String,
    /// 镜像引用（页面上显示，让用户确认点对了容器）。
    pub image: String,
    /// 权威判定的运行时。
    pub runtime: AppRuntime,
    /// 从挂载点推出来的 nginx Web 根（只有 nginx 有意义）。
    pub web_root: Option<String>,
    /// 容器**已经发布**的端口：`"80/tcp" -> [(host_ip, host_port)]`。
    ///
    /// 只用来**报告**：已存在的容器改不了端口发布（`-p` 只在 `run` / `create`
    /// 时给），所以界面上只能告诉用户"这个端口没发布到主机"，
    /// 不能画一个点了没用的「发布」按钮。
    pub ports: Vec<(String, Vec<(String, String)>)>,
}

impl DeployProbe {
    /// 这个端口发布到主机了吗（按容器端口号匹配 `"80/tcp"` 这种键）。
    pub fn publishes(&self, port: u16) -> bool {
        let prefix = format!("{port}/");
        self.ports
            .iter()
            .any(|(key, bindings)| key.starts_with(&prefix) && !bindings.is_empty())
    }
}

/// 探测：`inspect` 一次，判运行时 + 推 Web 根 + 读端口映射。
///
/// 判定失败（不是这几类容器）返回 [`Error::Io`] 带一句人话 ——
/// 这一步是进页面的门，失败了页面只显示那张说明卡。
pub fn detect(wslc: &Wslc, container: &str) -> Result<DeployProbe> {
    let inspect = crate::cmd::container::inspect(wslc, container, false)?;

    let runtime = deploy::runtime_from_inspect(&inspect).ok_or_else(|| {
        Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "「{container}」看起来不是 nginx / Java / Node.js / Go 容器 —— \
                 面板认不出该往哪儿部署。（依据是镜像引用、环境变量和启动命令，\
                 都读不到线索。）"
            ),
        ))
    })?;

    Ok(DeployProbe {
        container: inspect.name().unwrap_or(container).to_owned(),
        image: inspect.image_ref().unwrap_or_default().to_owned(),
        runtime,
        web_root: deploy::nginx_web_root(&inspect),
        ports: inspect.ports(),
    })
}

// ---------------------------------------------------------------------------
// 准备 + 复制 + 核对
// ---------------------------------------------------------------------------

/// 一次部署会往目标目录里放哪几个**名字**（不读文件内容）。
///
/// # 为什么要单独算一遍
///
/// [`prepare_target`] 现在只删**我们自己铺的名字**（[`deploy::removals_script`]），
/// 而这一步在**复制之前**跑 —— 所以得独立于 [`copy_sources`] 把名字算出来。
/// 同一份列表也进本地存档（`DeployRecord::members`），下次「重新部署 / 删除」
/// 靠它把"上一次有、这一次没有"的旧文件收拾掉。
///
/// 文件来源就是文件名；目录来源是它的**顶层条目名**（和 `copy_members` 同一套语义）。
/// 目录为空时返回空表 —— 调用方（`copy_sources`）会自己拦掉那种情况。
pub fn member_names(source: &Path, kind: SourceKind) -> Result<Vec<String>> {
    let paths = match kind {
        SourceKind::File => vec![source.to_path_buf()],
        SourceKind::Directory => directory_entries(source)?,
    };
    Ok(paths
        .iter()
        .filter_map(|path| deploy::artifact_name(path))
        .collect())
}

/// 准备好目标目录：建出来，然后删掉**我们自己铺的那些名字**（不碰别人的文件）。
///
/// `ours` = 这个目录是**本面板之前铺的**（本地存档里有记录）：
///
/// - `true`：按 `members` 收拾一遍再铺 —— 「重新部署」要的是"结果目录等于源目录"；
/// - `false`：**只允许它不存在或为空**。非空就返回一句人话的错误，
///   **一个字节都不清**。⚠️ 这一条是 v0.9.9 加的真实事故补丁：目标目录
///   很可能是**镜像自己要用的地方** —— 实测把应用（名字就叫 `java`）部署到
///   默认的 `/opt/java`，而 `eclipse-temurin:8-jre-alpine` 的 JDK 正是
///   `/opt/java/openjdk`。白名单（[`deploy::safe_target`]）挡不住这种事：
///   它只知道"`/opt` 底下允许"。
///
/// ⚠️ 真正保证"不删别人的东西"的是 [`deploy::removals_script`] 的**逐名删除**
/// （v0.9.10 起不再 `find -delete`）—— 所以 `ours` 判断失手也不会损坏数据，
/// 它只是替用户拦一下"你可能选错目录了"。
///
/// ⚠️ 这个判断必须和动手删在**同一条脚本**里（不能先由界面查一次再删）：
/// 两次 `exec` 之间目标目录可能被别的东西写进去，而且多一次往返。
///
/// ⚠️ 必须在复制**之前**跑，两条理由：
///
/// 1. 它（通过 `exec`）会**报出"容器没在运行"** —— 而 `cp` 在那种情况下
///    **静默成功**（实测 exit 0、不输出、什么都不做）；
/// 2. `cp` **要求目标目录已经存在**（实测不存在的路径报
///    `ERROR_PATH_NOT_FOUND`），它不会替我们建。
pub fn prepare_target(
    wslc: &Wslc,
    container: &str,
    target: &str,
    ours: bool,
    members: &[String],
) -> Result<()> {
    let out = run_script(
        wslc,
        container,
        &deploy::prepare_target_script(target, ours, members),
    )?;
    if out.contains(deploy::TARGET_NOT_EMPTY) {
        // 哨兵走的是 stdout（脚本特意 `exit 0`），所以由我们转成一句人话 ——
        // 原样露给用户只会是一串 `__WSLC_…__`
        return Err(Error::InvalidArgument(deploy::target_not_empty_hint(target)));
    }
    Ok(())
}

/// 列出目录来源的**顶层条目**（给 `deploy::copy_members` 用）。
///
/// 只读一层：`wslc container cp` 自己会递归（实测），我们只需要把顶层那几个
/// 名字交出去 —— 整目录交给它的话目标里会多一层目录名。
///
/// 返回的条目**排除**目录自身（`read_dir` 本来就不含 `.` / `..`）。
fn directory_entries(dir: &Path) -> Result<Vec<PathBuf>> {
    let reader = std::fs::read_dir(dir).map_err(|e| {
        Error::Io(std::io::Error::new(
            e.kind(),
            format!("读不了本地目录 {}：{e}", dir.display()),
        ))
    })?;
    let mut out: Vec<PathBuf> = Vec::new();
    for entry in reader {
        let entry = entry.map_err(Error::Io)?;
        out.push(entry.path());
    }
    // 排序只为**可预期**（错误的归属、日志、以及"先传哪个"都一样）——
    // `read_dir` 的顺序是文件系统给的，不稳定。
    out.sort();
    Ok(out)
}

/// 把本地来源复制进容器的目标目录，返回**实际复制过去的那些条目**。
///
/// 文件来源是它自己；目录来源是它的顶层条目。⚠️ **空目录会返回 `Err`** ——
/// 见下面那段。
pub fn copy_sources(
    wslc: &Wslc,
    container: &str,
    source: &Path,
    kind: SourceKind,
    target: &str,
) -> Result<Vec<PathBuf>> {
    let entries = match kind {
        SourceKind::File => Vec::new(),
        SourceKind::Directory => directory_entries(source)?,
    };
    let members = deploy::copy_members(source, kind, &entries);

    // ⚠️ 空目录**必须在这里拦掉**：`copy_members` 对空目录返回空表，
    // 于是后面 `verify_copy` 的 `count >= 0` **恒真** —— 界面会弹
    // "已部署 xxx → /opt/xxx"，而目标目录里什么都没有。
    // （这正是 `model::deploy::copy_members` 的文档里要求调用方做的事。）
    if members.is_empty() {
        return Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "这个目录是空的，没有东西可以部署：{}",
                source.display()
            ),
        )));
    }

    for member in &members {
        let args = deploy::cp_args(member, container, target);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        // ⚠️ 失败**要说是哪一个**：一个目录里 30 个条目，只说"复制失败"
        // 用户根本不知道该去看哪个。
        wslc.run_with_timeout(&refs, COPY_TIMEOUT)?
            .into_result()
            .map_err(|e| {
                Error::Io(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("复制 {} 失败：{e}", member.display()),
                ))
            })?;
    }
    Ok(members)
}

/// 复制完**核对**：目标目录里真的有东西吗。
///
/// # 为什么非核不可
///
/// 实测：容器**没在运行**时 `wslc container cp` 返回 **exit 0**、什么都不做、
/// 一个字都不输出。所以"`cp` 没报错"根本不能当作成功 ——
/// 那条路上界面上会显示"部署成功"，而容器里空空如也。
///
/// 判据用**顶层条目数**（`find -mindepth 1 -maxdepth 1`）而不是文件数：
/// 一个只有空子目录的合法来源，文件数本来就是 0，拿它判会误报失败。
///
/// 返回容器里实际的顶层条目数。
pub fn verify_copy(
    wslc: &Wslc,
    container: &str,
    target: &str,
    expected: usize,
) -> Result<u64> {
    let script = deploy::count_entries_script(target);
    let output = run_script(wslc, container, &script)?;
    let actual = deploy::parse_file_count(&output);

    match actual {
        Some(count) if count >= expected as u64 => Ok(count),
        Some(count) => Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!(
                "文件没有真的传进去：目标目录 {target} 里只有 {count} 个条目，\
                 而这次要传 {expected} 个。多半是容器没在运行 —— \
                 `wslc container cp` 在这种情况下**不报错**（实测 exit 0），\
                 所以面板只能靠数一遍发现。"
            ),
        ))),
        // 目录不在（`count_entries_script` 给的哨兵）
        None => Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("目标目录 {target} 没建成 —— 容器可能刚被停掉"),
        ))),
    }
}

/// 给一个可执行文件补 `+x`（见模块文档第 4 条）。
pub fn chmod_exec(wslc: &Wslc, container: &str, path: &str) -> Result<()> {
    run_script(wslc, container, &deploy::chmod_exec_script(path)).map(|_| ())
}

// ---------------------------------------------------------------------------
// 起停判活看日志
// ---------------------------------------------------------------------------

/// 起一个受 pidfile 管的进程。
///
/// 实测：0.17 秒返回，进程在 exec 结束之后**还活着**。
pub fn start(wslc: &Wslc, container: &str, dir: &str, command: &str) -> Result<()> {
    run_script(wslc, container, &deploy::start_script(dir, command)).map(|_| ())
}

/// 发 `TERM` 并删掉 pidfile。
///
/// ⚠️ 容器里**没有 `ps`、没有 `pkill`**（实测），所以确认"真的没退"要自己扫
/// `/proc` —— v0.9 **不做 `kill -9`**，界面上写清了这一点。
pub fn stop(wslc: &Wslc, container: &str, dir: &str) -> Result<()> {
    let script = deploy::stop_script(&deploy::container_join(dir, deploy::PID_FILE));
    run_script(wslc, container, &script).map(|_| ())
}

/// 判活 + 数文件（一次 exec 两件事）。
pub fn probe_state(
    wslc: &Wslc,
    container: &str,
    kind: DeployKind,
    dir: &str,
) -> Result<ProbeResult> {
    let script = deploy::probe_script(kind, dir);
    let output = run_script(wslc, container, &script)?;
    Ok(deploy::parse_probe_output(kind, &output))
}

/// 只判活（不带文件数）。
pub fn is_running(wslc: &Wslc, container: &str, dir: &str) -> Result<ProcessStatus> {
    let script = deploy::status_script(&deploy::container_join(dir, deploy::PID_FILE));
    let output = run_script(wslc, container, &script)?;
    deploy::parse_process_status(&output).ok_or_else(|| {
        Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("判活脚本的输出看不懂：{}", output.trim()),
        ))
    })
}

/// 读应用日志的最后 [`LOG_TAIL_LINES`] 行。
pub fn read_log(wslc: &Wslc, container: &str, dir: &str) -> Result<String> {
    let script = deploy::tail_script(
        &deploy::container_join(dir, deploy::LOG_FILE),
        LOG_TAIL_LINES,
    );
    let output = run_script(wslc, container, &script)?;
    Ok(deploy::render_log_output(&output))
}

/// 重载容器里的 nginx（内容形态特有的动作）。
pub fn reload_nginx(wslc: &Wslc, container: &str) -> Result<String> {
    run_script(wslc, container, &deploy::reload_nginx_script())
}

/// 删除一次部署（「删除」按钮）。
///
/// `members` 是**上一次真的铺进去的名字**（`DeployRecord::members`）——
/// 删除只删这些 + `app.log` / `app.pid`，**不碰目录里别人的东西**
/// （见 [`deploy::removals_script`]：整目录清空在 v0.9.8 删掉过一个 JDK）。
/// 老存档里没有这个字段时传空表：那就只删日志 / pidfile。
///
/// ⚠️ 不可撤销，而且目标是卷挂载点时会删掉卷里的数据 —— 调用方必须先二次确认。
pub fn remove_target(
    wslc: &Wslc,
    container: &str,
    target: &str,
    members: &[String],
) -> Result<()> {
    run_script(wslc, container, &deploy::remove_script(target, members)).map(|_| ())
}

/// 跑一条容器内脚本，返回 stdout；非零退出码转成错误。
///
/// stdout / stderr 分开走 `CommandOutput`：**判活和数数看的是 stdout**
/// （哨兵在那里），而 `wslc` 的错误消息在 stderr。`combined()` 那条路会把
/// 两者混起来，哨兵就淹了。
fn run_script(wslc: &Wslc, container: &str, script: &str) -> Result<String> {
    let args = deploy::exec_args(container, script);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = wslc.run_with_timeout(&refs, QUICK_TIMEOUT)?;
    // `into_result` 会在非零退出码时把 stderr 拼进错误 —— 那正是要给用户看的
    let output = output.into_result()?;
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publishing_is_matched_by_container_port() {
        let probe = DeployProbe {
            container: "nginx".to_owned(),
            image: "docker.1panel.live/library/nginx:latest".to_owned(),
            runtime: AppRuntime::Nginx,
            web_root: Some("/usr/share/nginx/html".to_owned()),
            // 本机那个真容器的实测值
            ports: vec![(
                "80/tcp".to_owned(),
                vec![("127.0.0.1".to_owned(), "80".to_owned())],
            )],
        };
        assert!(probe.publishes(80));
        assert!(!probe.publishes(8080), "没发布的端口要如实说不");
        // ⚠️ `8080` 不能被 `80/tcp` 匹配上（前缀要带那个 `/`）
        assert!(!probe.publishes(8));
    }

    #[test]
    fn a_port_key_with_no_bindings_counts_as_not_published() {
        let probe = DeployProbe {
            container: "web".to_owned(),
            image: "nginx".to_owned(),
            runtime: AppRuntime::Nginx,
            web_root: None,
            ports: vec![("80/tcp".to_owned(), Vec::new())],
        };
        assert!(
            !probe.publishes(80),
            "有键但没有实际绑定（HostIp/HostPort 缺失）不算发布出去"
        );
    }
}
