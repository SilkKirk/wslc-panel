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
    self, AppRuntime, DeployKind, DeploySite, PortMap, ProbeResult, SourceKind,
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
    ///
    /// 判"发布了没有"用 [`deploy::port_is_published`]（纯函数、有单测）——
    /// 早先这里挂过一个 `publishes()` 方法，但界面另写了一份内联判据，
    /// 于是**被测的那份没人用、在用的那份没被测**（v0.9.13 改掉了）。
    pub ports: PortMap,
    /// 容器当前接入的网络名（反代预检用：和上游容器找共同用户网络）。
    ///
    /// 默认 `bridge` 上容器名解析不了，所以站点反代必须落在用户自定义网络上。
    pub networks: Vec<String>,
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
        networks: inspect
            .networks()
            .into_iter()
            .map(|(name, _, _, _)| name)
            .collect(),
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

/// 准备好目标目录：建出来，然后删掉**我们自己铺的那些名字**（[`deploy::removals_script`]）。
///
/// 返回 `true` = 目标目录里本来就有**不是我们铺的**东西。面板**没有**动它们
/// （逐名删除），只是值得跟用户说一句（[`deploy::target_not_empty_hint`]）。
///
/// `ours` = 这个目录是**本面板之前铺的**（本地存档里有记录）：
///
/// - `true`：什么都不说；
/// - `false`：目录非空时脚本打一个哨兵，我们把它转成一句**提醒** ——
///   ⚠️ **不是**错误、**不阻断部署**。
///
/// # 为什么不再是"拒绝部署"（v0.9.11 改）
///
/// v0.9.9 那层拒绝的初衷是保护**镜像自己要用的文件**（实测把应用部署到
/// `/opt/java`，而 `eclipse-temurin` 的 JDK 正好在 `/opt/java/openjdk`）。
/// 但 v0.9.10 起删除已经变成[逐名删除](deploy::removals_script)，那件事由它保证
/// —— 硬拦已经没有安全上的理由，只剩下"挡住用户自己的正常部署"这一个效果。
/// 实测：删掉旧记录 + 重建容器之后重新部署 `/opt/java`（安全的一次）被直接拒绝，
/// 用户只能来问为什么。所以现在**干完再说一句**。
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
) -> Result<bool> {
    let out = run_script(
        wslc,
        container,
        &deploy::prepare_target_script(target, ours, members),
    )?;
    Ok(out.contains(deploy::TARGET_NOT_EMPTY))
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
                // `io::Error::other`（1.74 稳定）比 `new(ErrorKind::Other, …)`
                // 短，而且 clippy 的 `io_other_error` 会盯着旧写法。
                // 本 crate 的 MSRV 是 1.85，用它没有兼容性代价。
                Error::Io(std::io::Error::other(format!(
                    "复制 {} 失败：{e}",
                    member.display()
                )))
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
        // 同上一处：用 `io::Error::other`（clippy 的 `io_other_error`）。
        Some(count) => Err(Error::Io(std::io::Error::other(format!(
            "文件没有真的传进去：目标目录 {target} 里只有 {count} 个条目，\
             而这次要传 {expected} 个。多半是容器没在运行 —— \
             `wslc container cp` 在这种情况下**不报错**（实测 exit 0），\
             所以面板只能靠数一遍发现。"
        )))),
        // 目录不在（`count_entries_script` 给的哨兵）。
        // ⚠️ 这条**不能**用 `io::Error::other`：`NotFound` 是有意义的类别
        // （调用方靠它区分"目录没建成"和"数量不对"）。
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

// ⚠️ 这里原本有一段 doc 注释（"只判活（不带文件数）…"），但它底下直接跟着
// 空行 + `read_log` 的文档 —— 于是 rustdoc 把那段话当成了 `read_log` 的说明，
// 而它讲的是**另一个不存在的函数**（`is_running`，v0.9.13 删掉的那个）。
// clippy 的 `empty_line_after_doc_comments` 报的就是这个。
//
// 那段话本身是有价值的（它解释了"为什么没有单独判活的入口"），所以改成
// 普通注释留在这里，而不是删掉。

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
///
/// 输出过一遍 [`deploy::filter_nginx_noise`]：端口 80 上接管 `default.conf`
/// 时必然出现 `conflicting server name "localhost" ... ignored`，exit code
/// 仍是 0 —— 不过滤的话每次重载都像报了个错。
pub fn reload_nginx(wslc: &Wslc, container: &str) -> Result<String> {
    let output = run_script(wslc, container, &deploy::reload_nginx_script())?;
    Ok(deploy::filter_nginx_noise(&output))
}

/// `read_resolv_conf` / 站点配置也走这条超时（都是几秒的事）。
pub fn read_resolv_conf(wslc: &Wslc, container: &str) -> Result<String> {
    run_script(wslc, container, "cat /etc/resolv.conf 2>/dev/null")
}

// ---------------------------------------------------------------------------
// nginx 站点配置：写 / 校验 / 回滚 / 删
// ---------------------------------------------------------------------------

/// 一次 `apply_site` 的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplySite {
    /// 非致命警告（配置已生效，但 `nginx -s reload` 失败/有噪音）。
    ///
    /// `nginx -s reload` 失败**不代表**读不到新配置：实测 reload 在 stop 之后
    /// 再 start 会读新 conf。所以这里只警告，`site` 仍然写进记录。
    pub warning: Option<String>,
}

/// 把一份 `server{}` 块写进容器、`nginx -t` 校验、reload；不通过就回滚。
///
/// 返回 `Ok` = 配置**已经生效**（可能带 reload 警告）；`Err` = `-t` 没过，
/// 文件已经恢复成写之前的样子 —— 调用方**不能**把新的 `site` 写进记录。
///
/// # 顺序（每一步都有实测理由）
///
/// 1. 读首行标记：不是 `# wslc-panel:site:<app>` 就拒绝覆盖（用户可能手写了
///    同名 conf）；
/// 2. 备份旧文件到 `/tmp`；
/// 3. 写新文件（本地临时文件 + `wslc container cp` —— `cp` 要求目标目录存在，
///    `/etc/nginx/conf.d/` 是镜像自带的）；
/// 4. `nginx -t`；失败 → 恢复备份、再 `-t` 一次（尽力），把**原来的错误**返回；
/// 5. `nginx -s reload`；失败只进 `warning`（见 [`ApplySite::warning`]）；
/// 6. 删备份。
pub fn apply_site(
    wslc: &Wslc,
    container: &str,
    app: &str,
    site: &DeploySite,
    root: &str,
    listen: u16,
    resolver: Option<&str>,
) -> Result<ApplySite> {
    let path = conf_path_for(app, site);
    if !is_our_conf(&path) {
        return Err(invalid(format!(
            "拒绝写这个路径（不是面板的 conf 目录）：{path}"
        )));
    }
    let marker = deploy::conf_marker(app);
    let first = read_first_line(wslc, container, &path)?;
    if !first.trim().is_empty() && first.trim() != marker.as_str() {
        return Err(invalid(format!(
            "{path} 已经存在，而且不是面板生成的（首行不是 `{marker}`）—— 拒绝覆盖"
        )));
    }

    let backup = format!("/tmp/wslc-panel-{app}.conf.bak");
    run_script(wslc, container, &backup_script(&path, &backup))?;

    let text = deploy::site_conf(app, site, root, listen, resolver);
    write_conf(wslc, container, &path, &text)?;

    if let Err(error) = run_script(wslc, container, &deploy::nginx_test_script()) {
        // 回滚：旧文件有就恢复，没有就删掉这次新写的
        let _ = run_script(wslc, container, &restore_script(&path, &backup));
        let _ = run_script(wslc, container, &deploy::nginx_test_script());
        let _ = run_script(wslc, container, &rm_script(&backup));
        return Err(error);
    }

    let warning = match run_script(wslc, container, &deploy::reload_nginx_script()) {
        Ok(output) => {
            let clean = deploy::filter_nginx_noise(&output);
            if clean.is_empty() {
                None
            } else {
                Some(clean)
            }
        }
        Err(error) => Some(format!("配置已写入，但重载失败：{error}")),
    };
    let _ = run_script(wslc, container, &rm_script(&backup));
    Ok(ApplySite { warning })
}

/// 删掉一个应用的 conf（站点关 / 删除部署）。
///
/// 返回 `Ok(None)` = 删干净了；`Ok(Some(warning))` = 文件已经删了，但
/// `nginx -t` / reload 有问题（**不能**因此把记录留下：文件已经不在了）；
/// `Err` = 文件不是面板的 / 删不掉，调用方要保留 `site` 记录。
pub fn remove_site(
    wslc: &Wslc,
    container: &str,
    app: &str,
    site: &DeploySite,
) -> Result<Option<String>> {
    let path = conf_path_for(app, site);
    if !is_our_conf(&path) {
        return Err(invalid(format!(
            "拒绝删除这个路径（不是面板的 conf 目录）：{path}"
        )));
    }
    let marker = deploy::conf_marker(app);
    let first = read_first_line(wslc, container, &path)?;
    if !first.trim().is_empty() && first.trim() != marker.as_str() {
        return Err(invalid(format!(
            "{path} 不是面板生成的（首行不是 `{marker}`）—— 拒绝删除"
        )));
    }
    run_script(wslc, container, &rm_script(&path))?;

    let mut warnings: Vec<String> = Vec::new();
    match run_script(wslc, container, &deploy::nginx_test_script()) {
        Ok(_) => {
            if let Err(error) = run_script(wslc, container, &deploy::reload_nginx_script()) {
                warnings.push(format!("配置已删，但重载失败：{error}"));
            }
        }
        Err(error) => warnings.push(format!("配置已删，但 nginx -t 失败：{error}")),
    }
    if warnings.is_empty() {
        Ok(None)
    } else {
        Ok(Some(warnings.join("；")))
    }
}

/// 记录里的 `conf` 优先（老档可能没写 → 按应用名重算）。
fn conf_path_for(app: &str, site: &DeploySite) -> String {
    let path = site.conf.trim();
    if path.is_empty() {
        deploy::conf_path(app)
    } else {
        path.to_owned()
    }
}

fn is_our_conf(path: &str) -> bool {
    path.starts_with("/etc/nginx/conf.d/00-wslc-panel-") && path.ends_with(".conf")
}

fn invalid(message: impl Into<String>) -> Error {
    Error::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        message.into(),
    ))
}

/// 读文件第一行；文件不存在时是空串（`head ... || true`）。
fn read_first_line(wslc: &Wslc, container: &str, path: &str) -> Result<String> {
    let script = format!(
        "head -n 1 {} 2>/dev/null || true",
        deploy::shell_quote(path)
    );
    let output = run_script(wslc, container, &script)?;
    Ok(output.lines().next().unwrap_or_default().to_owned())
}

fn backup_script(path: &str, backup: &str) -> String {
    format!(
        "if test -f {path}; then cp -f {path} {backup}; else rm -f {backup}; fi",
        path = deploy::shell_quote(path),
        backup = deploy::shell_quote(backup),
    )
}

fn restore_script(path: &str, backup: &str) -> String {
    format!(
        "if test -f {backup}; then cp -f {backup} {path}; else rm -f {path}; fi",
        path = deploy::shell_quote(path),
        backup = deploy::shell_quote(backup),
    )
}

fn rm_script(path: &str) -> String {
    format!("rm -f {}", deploy::shell_quote(path))
}

/// 把文本写成容器里的一个文件。
///
/// 走 `wslc container cp`（本仓库唯一的搬运方式）：先写本地临时文件，再
/// `cp` 到 `/etc/nginx/conf.d/` —— **目标目录必须存在**（镜像是有的），
/// 文件名保持 `00-wslc-panel-<app>.conf`（`cp` 会保留源文件的 basename）。
fn write_conf(wslc: &Wslc, container: &str, path: &str, text: &str) -> Result<()> {
    let name = path
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| invalid(format!("conf 路径取不出文件名：{path}")))?;
    let dir = std::env::temp_dir().join(format!("wslc-panel-site-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let local = dir.join(name);
    std::fs::write(&local, text)?;
    let args = deploy::cp_args(&local, container, "/etc/nginx/conf.d/");
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = wslc.run_with_timeout(&refs, QUICK_TIMEOUT);
    let _ = std::fs::remove_dir_all(&dir);
    result?.into_result()?;
    Ok(())
}

/// 读一个应用的站点 conf（只读浮层用）。
///
/// 记录里有 `conf` 就用它，没有就按应用名重算（老记录 / 手改过的存档）。
/// 文件不存在时 `cat` 会失败，错误原文交给界面 —— 用户至少知道"没有这份文件"，
/// 而不是看到一片空白。
pub fn read_site_conf(
    wslc: &Wslc,
    container: &str,
    app: &str,
    site: Option<&DeploySite>,
) -> Result<String> {
    // ⚠️ 只读：这里**不**校验首行标记 —— 文件可能刚被用户手改过，
    // 让他看到"现在实际是什么"比拒绝读更有用；写 / 删那两条才校验。
    let path = site
        .map(|site| site.conf.trim())
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| deploy::conf_path(app));
    run_script(
        wslc,
        container,
        &format!("cat {} 2>&1", deploy::shell_quote(&path)),
    )
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

// 这里**没有** `#[cfg(test)] mod tests`：这个模块里的东西全是"拼参数 → 起进程"，
// 纯逻辑都在 `model::deploy`（那边有 40 多条断言）。原先这里挂着两条
// `DeployProbe::publishes` 的测试 —— v0.9.13 把那个方法连同测试一起搬进了
// `model::deploy::port_is_published`：那个判据**界面真的在调**，
// 而 `publishes()` 只有测试在调（界面另写了一份内联的）。

