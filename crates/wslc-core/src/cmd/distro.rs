//! `wsl.exe` 子命令的类型化封装（WSL 发行版 / 实例）。
//!
//! # 与 `cmd::container` 等的区别
//!
//! `wsl.exe` **没有 `--format json`** —— 它的输出全是给人看的表格。
//! 所以这里只负责「拼参数 → 跑命令 → 把注册表信息合并进来」，
//! 真正的解析逻辑在 [`crate::model::distro`] 里（那样才能脱离 Windows 跑单测）。
//!
//! # 为什么还要读注册表
//!
//! `wsl` 命令**不报告**发行版装在哪个盘。唯一来源是
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Lxss\<GUID>` 下的
//! `BasePath` / `VhdFileName`（实测，见 `docs/PLAN-v0.3.md` §3.4）。
//!
//! 读取方式选了 `reg.exe query` 而不是注册表 API，理由见
//! [`crate::cli::run_helper`] 的文档（实测 7~27 ms，且输出恒为 UTF-8）。

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::cli::{self, StreamHandle, Wsl};
use crate::error::{Error, Result};
use crate::model::distro::{parse_distro_list, Distro, WslStatus};

/// 注册表根键：发行版元数据都在这里。
pub const LXSS_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Lxss";

/// `reg.exe` 的调用超时。
///
/// 实测 7~27 ms，给 10 秒足够宽松 —— 这是为了防止极端情况下卡住界面。
const REG_TIMEOUT: Duration = Duration::from_secs(10);

/// `wsl --list --verbose` 的结果。
///
/// 把「认出来的」和「没认出来的」分开返回：一行解析失败不该让整个列表变空
/// （那会让面板看起来像"一个发行版都没有"）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DistroList {
    /// 认出来的发行版。
    pub distros: Vec<Distro>,
    /// 没认出来的行（原因 + 原文），交给界面如实展示。
    pub warnings: Vec<String>,
}

/// 列出全部发行版（`wsl --list --verbose`）。
pub fn list(wsl: &Wsl) -> Result<DistroList> {
    let out = wsl.run_checked(&["--list", "--verbose"])?;
    let (distros, warnings) = parse_distro_list(&out.stdout);
    Ok(DistroList { distros, warnings })
}

/// 读取 `wsl --status`（默认发行版 + 默认版本）。
///
/// ⚠️ 这只是**兜底**：默认发行版的权威来源是列表里的 `*`。
/// 而且 `--status` 的输出是**本地化**的（中文系统是「默认分发:」），
/// 所以采集频率刻意放低（30 秒），见 `docs/PLAN-v0.3.md` §10。
pub fn status(wsl: &Wsl) -> Result<WslStatus> {
    let out = wsl.run_checked(&["--status"])?;
    Ok(WslStatus::parse(&out.stdout))
}

/// 默认发行版的名字（列表里的 `*`）。
pub fn default_distro(distros: &[Distro]) -> Option<&str> {
    distros
        .iter()
        .find(|d| d.is_default)
        .map(|d| d.name.as_str())
}

/// 采集一次完整的发行版快照：列表 + 注册表 + 磁盘占用。
///
/// **阻塞调用**，必须在后台执行器上跑（会串行起 `wsl.exe` 和 `reg.exe`）。
pub fn snapshot(wsl: &Wsl) -> Result<DistroList> {
    let mut list = list(wsl)?;
    enrich(&mut list.distros);
    Ok(list)
}

// ---------------------------------------------------------------------------
// 动作（P2）
// ---------------------------------------------------------------------------

/// 快速动作的超时（终止 / 设为默认）。
const QUICK_TIMEOUT: Duration = Duration::from_secs(60);

/// `--shutdown` 要等所有发行版里的进程退出，可能比单终止慢。
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(180);

/// 会动磁盘的动作（删除 / 压缩）。
///
/// 实测本机 18 GB 的 VHDX 上 `--compact` 只要 **10.2 秒**，
/// 但碎片多的盘可能到分钟级，所以给得宽松。
const DISK_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// **本项目只支持 WSL 2。**
///
/// 所有"新建发行版"的路径都**显式**传 `--version 2`，而不是依赖 WSL 的默认值 ——
/// 默认值（`wsl --set-default-version`）是可以被改成 1 的，
/// 那样建出来的发行版本程序管不了，用户还会以为是程序坏了。
pub const WSL_VERSION: u8 = 2;

/// 过滤掉 `wsl.exe` 打在 stderr 上、**与本操作无关**的配置告警。
///
/// 实测：进发行版的命令（`-d X -e ...` / `--manage`）会在 stderr 上打出
/// `%USERPROFILE%\.wslconfig` 的告警：
///
/// ```text
/// wsl: interop.appendWindowsPath:C:\Users\76434\.wslconfig 中的键"12"未知
/// wsl: user.default:C:\Users\76434\.wslconfig 中的键"15"未知
/// ```
///
/// ⚠️ **只在 VM 启动时报一次**（v0.3.7 实测更正）：`wsl --shutdown` 之后的
/// 第一条进发行版的命令会报，紧接着的第二、三条就静默了；`--status` /
/// `--version` / `-l -v` / `--terminate` / `--set-default` 则**从来不报**。
/// 也就是说它们来自 WSL2 虚拟机启动时读一次 `.wslconfig`，不是每条命令都校验。
///
/// 它们**不影响退出码**，但会把错误消息污染得看不出真正的原因 ——
/// 用户看到"操作失败：键12未知"只会更迷惑。
///
/// 想把这些告警**呈现给用户**的话，见 [`crate::wslconfig`]（那个模块只做
/// 不依赖 WSL 的静态检查，原因也写在它的模块说明里）。
fn strip_config_warnings(text: &str) -> String {
    text.lines()
        .filter(|line| !line.trim_start().starts_with("wsl: "))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

/// 跑一个发行版动作，成功返回 `Ok(())`，失败给出**干净**的错误。
fn run_action(wsl: &Wsl, args: &[&str], timeout: Duration) -> Result<()> {
    let out = wsl.run_with_timeout(args, timeout)?;
    if out.success() {
        return Ok(());
    }

    let detail = strip_config_warnings(&out.combined());
    Err(Error::NonZeroExit {
        program: "wsl",
        args: out.args.join(" "),
        // 实测 `wsl.exe` 用的是 -1，不是 1 —— 原样透传
        code: out.code.unwrap_or(-1),
        stderr: if detail.is_empty() {
            "（wsl 没有给出任何输出）".to_owned()
        } else {
            detail
        },
    })
}

/// 终止一个发行版（`wsl --terminate <name>`）。
///
/// ⚠️ 会丢掉发行版里**没有保存**的东西。调用方必须二次确认。
pub fn terminate(wsl: &Wsl, name: &str) -> Result<()> {
    run_action(wsl, &["--terminate", name], QUICK_TIMEOUT)
}

/// 关停**所有**发行版和 WSL2 轻量工具虚拟机（`wsl --shutdown`）。
///
/// 影响面比终止单个发行版大得多，调用方必须二次确认。
pub fn shutdown(wsl: &Wsl) -> Result<()> {
    run_action(wsl, &["--shutdown"], SHUTDOWN_TIMEOUT)
}

/// 把某个发行版设为默认（`wsl --set-default <name>`）。
///
/// 不破坏数据、可逆，所以**不需要**二次确认。
pub fn set_default(wsl: &Wsl, name: &str) -> Result<()> {
    run_action(wsl, &["--set-default", name], QUICK_TIMEOUT)
}

/// 注销发行版：**删除**它的根文件系统（`wsl --unregister <name>`）。
///
/// 不可撤销。调用方必须二次确认，并把 [`Distro::vhdx_bytes`] 一起展示出来，
/// 让用户知道自己要删掉多少东西。
pub fn unregister(wsl: &Wsl, name: &str) -> Result<()> {
    run_action(wsl, &["--unregister", name], DISK_TIMEOUT)
}

/// 压缩发行版的 VHDX，回收已释放的块（`wsl --manage <name> --compact`）。
///
/// 实测本机 18 GB 的盘耗时 **10.2 秒**、回收 20 MB —— 所以用
/// 「提示 + 完成后刷新」就够，不需要单独的进度弹窗。
pub fn compact(wsl: &Wsl, name: &str) -> Result<()> {
    run_action(wsl, &["--manage", name, "--compact"], DISK_TIMEOUT)
}

/// 在新控制台窗口里打开发行版的终端（`wsl -d <name>`）。
///
/// # 这就是「启动」该有的样子
///
/// 实测（WSL 3.0.1.0）：`wsl -d <name> -e true` 能让发行版变成 Running，
/// 但**约 20 秒后它会自己回到 Stopped**；连
/// `setsid nohup sleep 900 &` 这种真正的后台常驻进程也留不住它
/// （最后一个 `wsl.exe` 会话退出后，发行版就被回收了）。
///
/// 所以**不做**一个裸的「启动」按钮 —— 点完看着是"运行中"、
/// 20 秒后变回"已停止"，用户只会以为程序坏了。
/// 打开终端是诚实且有用的等价物：终端开着，发行版就一直是运行中。
pub fn open_terminal(wsl: &Wsl, name: &str) -> Result<()> {
    wsl.spawn_in_new_console(&["-d", name])
}

/// 「启动」用的哨兵命令。
///
/// `sleep infinity` 不会自己退出，所以**吊着它的那个 `wsl.exe` 也不会退出** ——
/// 这正是我们要的：WSL 认为有活动会话，发行版就一直运行。
const KEEP_ALIVE_ARGS: [&str; 3] = ["--", "sleep", "infinity"];

/// 启动发行版，并让它**一直保持运行**。
///
/// 返回**吊着它的那个进程**。调用方必须留着这个句柄 —— 它是
/// "这个发行版为什么还在跑"的唯一凭据（句柄一丢，界面上就说不清是谁在维持）。
///
/// # 为什么要这么绕
///
/// 实测（WSL 3.0.1.0，见 `docs/PLAN-v0.3.md`）：
///
/// | 做法 | 结果 |
/// |---|---|
/// | `wsl -d X -e true`（进程立刻退出） | 约 20 秒后 Stopped |
/// | `wsl -d X -e sh -c "setsid nohup sleep 900 &"` | **也**约 20 秒后 Stopped |
/// | `wsl -d X -- sleep infinity`（**吊住不放手**） | 一直 Running |
///
/// 第二条和第三条的差别是关键：**在发行版里造一个常驻进程没用** ——
/// 最后一个 `wsl.exe` 一退出，WSL 照样把整个发行版收掉（连"脱缰"的进程一起杀）。
/// 真正管用的是**从 Windows 这边吊住一个 `wsl.exe` 不放**。
///
/// 早先的版本只测了前两条，于是错误地得出"没有活动会话就留不住"，
/// 把它当成了硬约束 —— 其实活动会话是可以人为制造的。
///
/// # 代价（选了"面板关掉后继续跑"就得认）
///
/// 那个 `wsl.exe` 是**本程序**吊着的。本程序退出后它**不会**跟着退出
/// （Rust 的 `Child` 析构不杀进程），所以发行版会继续跑 —— 这正是要的行为，
/// 但也意味着任务管理器里会留下一个 `wsl.exe`。
/// 想停就点「终止」：`wsl --terminate` 会连 `sleep infinity` 一起杀，
/// 哨兵进程随之退出，界面上那个"保持运行中"的标记也会跟着消失。
pub fn start(wsl: &Wsl, name: &str) -> Result<std::process::Child> {
    // 先跑一条**会失败**的命令探一下。发行版名写错时直接 spawn 哨兵，
    // 只会得到一个"起来就退出"的进程，错误信息是空的；这一步能把
    // "不存在具有所提供名称的分发"这种话原样带出来。
    run_action(wsl, &["-d", name, "-e", "true"], QUICK_TIMEOUT)?;

    let mut args: Vec<&str> = vec!["-d", name];
    args.extend_from_slice(&KEEP_ALIVE_ARGS);
    wsl.spawn_sentinel(&args)
}

// ---------------------------------------------------------------------------
// 添加实例（P3）
// ---------------------------------------------------------------------------

/// 安装新发行版的超时。
///
/// 在线安装要下载几百 MB 到几 GB，从 tar 导入要铺开整个文件系统 ——
/// 按**量级**给，不按"感觉"给。
const INSTALL_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// 新发行版的**来源**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallSource {
    /// 本地 tar 文件 → `wsl --import <name> <dir> <file> --version 2`。
    ///
    /// 最可靠的一条路：**不联网也能用**。
    ///
    /// 版本**不给用户选**：本项目只支持 WSL 2（见 [`WSL_VERSION`]）。
    Tar {
        /// tar 文件路径。
        path: String,
    },
    /// 本地文件，交给 WSL 自己的安装器 → `wsl --install --from-file`。
    ///
    /// 和 [`InstallSource::Tar`] 的区别不只是参数：`--import` 只是把文件系统
    /// 铺开，而 `--install` 走的是 Store 安装器那套，会做首次启动初始化
    /// （建默认用户等）。同一个 tar，两条路的结果不一样。
    ///
    /// ⚠️ 这条**没有** `--version` 选项（实测 `wsl.exe --help`），
    /// 版本由安装器自己决定 —— 我们想显式指定也指定不了。
    File {
        /// 文件路径（RootFS 或 VHDX）。
        path: String,
    },
    /// 在线安装 → `wsl --install -d <name> --version 2`。
    ///
    /// ⚠️ 实测本机 `wsl --list --online` **不可用**
    /// （解析不了 `raw.githubusercontent.com`），所以发行版名只能**手输**，
    /// 不能做成一个"转圈等列表"的下拉框。
    Online {
        /// 装完是否立刻启动。
        ///
        /// 默认**不**启动：安装动辄十几分钟，装完自己弹一个终端出来很突兀。
        launch: bool,
    },
}

/// 「添加实例」的全部参数。
///
/// 和容器的 [`crate::cmd::container::RunSpec`] 一个套路：
/// [`InstallSpec::validate`] 先挡住明显错的输入，[`InstallSpec::to_args`]
/// 负责拼命令行 —— 界面据此做**等效命令预览**，用户随时知道我们要跑什么。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallSpec {
    /// 发行版名。
    pub name: String,
    /// 安装目录。留空表示交给 WSL 决定。
    pub install_dir: String,
    /// 来源。
    pub source: InstallSource,
    /// 装完是否设为默认。
    ///
    /// ⚠️ 这**不是**一个命令行选项 —— `--import` 和 `--install` 都不接受
    /// `--set-default`（实测 `wsl.exe --help`）。所以它对应的是安装成功后
    /// **再跑一条** `wsl --set-default <name>`，见 [`install`]。
    pub set_default: bool,
}

impl InstallSpec {
    /// 新建（只给名字和来源，其余字段用默认值）。
    pub fn new(name: impl Into<String>, source: InstallSource) -> Self {
        Self {
            name: name.into(),
            install_dir: String::new(),
            source,
            set_default: false,
        }
    }

    /// 校验；返回**可以直接给用户看**的错误。
    pub fn validate(&self) -> std::result::Result<(), String> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err("发行版名不能为空".to_owned());
        }
        // 发行版名会变成注册表键和安装目录的一部分；
        // 含路径分隔符会让它和安装路径混淆，直接挡掉。
        if name.contains(['\\', '/']) {
            return Err("发行版名里不能有 \\ 或 /".to_owned());
        }

        match &self.source {
            InstallSource::Tar { path } => {
                if path.trim().is_empty() {
                    return Err("tar 文件路径不能为空".to_owned());
                }
                if self.install_dir.trim().is_empty() {
                    return Err("从 tar 导入必须指定安装目录".to_owned());
                }
            }
            InstallSource::File { path } => {
                if path.trim().is_empty() {
                    return Err("文件路径不能为空".to_owned());
                }
            }
            // 在线安装没有额外必填项：名字上面已经校验过了
            InstallSource::Online { .. } => {}
        }

        let dir = self.install_dir.trim();
        if !dir.is_empty() && !is_absolute_windows_path(dir) {
            return Err(format!(
                "安装目录必须是绝对路径（如 D:\\wsl\\{name}）：{dir}"
            ));
        }

        Ok(())
    }

    /// 拼成 `wsl.exe` 的参数（**不含** `wsl` 本身）。
    ///
    /// 注意**不包含** `--set-default` —— 它不是安装命令的选项，
    /// 见 [`InstallSpec::preview_lines`]。
    pub fn to_args(&self) -> Vec<String> {
        let name = self.name.trim().to_owned();
        let dir = self.install_dir.trim();
        let mut args: Vec<String> = Vec::new();

        match &self.source {
            InstallSource::Tar { path } => {
                args.push("--import".to_owned());
                args.push(name);
                args.push(dir.to_owned());
                args.push(path.trim().to_owned());
                // **显式**指定版本，不吃 WSL 的默认值 ——
                // 默认值是能被用户改成 1 的，那样建出来的发行版本程序管不了。
                args.push("--version".to_owned());
                args.push(WSL_VERSION.to_string());
            }
            InstallSource::File { path } => {
                args.push("--install".to_owned());
                args.push("--from-file".to_owned());
                args.push(path.trim().to_owned());
                // `--name` 显式给：不给的话 WSL 会自己猜一个名字，
                // 而用户在表单里明确填了。
                args.push("--name".to_owned());
                args.push(name);
                if !dir.is_empty() {
                    args.push("--location".to_owned());
                    args.push(dir.to_owned());
                }
                // 这条**没有** `--version` 选项，只能听安装器的
            }
            InstallSource::Online { launch } => {
                args.push("--install".to_owned());
                args.push("-d".to_owned());
                args.push(name);
                if !dir.is_empty() {
                    args.push("--location".to_owned());
                    args.push(dir.to_owned());
                }
                args.push("--version".to_owned());
                args.push(WSL_VERSION.to_string());
                // 只有用户明确要求启动时才**不加** `--no-launch`。
                if !launch {
                    args.push("--no-launch".to_owned());
                }
            }
        }

        args
    }

    /// 等效命令预览。
    ///
    /// 返回的是**多行**：勾了"设为默认"时是两条命令 ——
    /// 界面上要如实展示，别让用户以为一条命令就搞定了。
    pub fn preview_lines(&self) -> Vec<String> {
        let mut lines = vec![format!("wsl {}", self.to_args().join(" "))];
        if self.set_default {
            lines.push(format!("wsl --set-default {}", self.name.trim()));
        }
        lines
    }
}

/// 是不是 Windows 绝对路径：`D:\...`、`D:/...` 或 UNC `\\server\share`。
///
/// 刻意**不用** `Path::is_absolute()`：它在非 Windows 上对 `D:\wsl` 返回
/// `false`，而这条校验的结论不该随编译平台变。
fn is_absolute_windows_path(text: &str) -> bool {
    let bytes = text.as_bytes();
    // 盘符形式：`D:\` / `D:/`
    if bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return matches!(bytes[2], b'\\' | b'/');
    }
    // UNC 形式：`\\server\share`
    text.starts_with("\\\\")
}

/// 按 [`InstallSpec`] 装一个新发行版。
///
/// 可能跑**一到两条**命令：先安装，需要的话再 `--set-default`
/// （`--import` / `--install` 都没有这个选项，只能分两步）。
///
/// **阻塞调用**，必须在后台执行器上跑 —— 在线安装可能要几十分钟。
pub fn install(wsl: &Wsl, spec: &InstallSpec) -> Result<String> {
    // 再校验一次：调用方（界面）本来就会先校验，但数据层不该依赖这件事。
    spec.validate().map_err(Error::InvalidArgument)?;

    let args = spec.to_args();
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run_action(wsl, &refs, INSTALL_TIMEOUT)?;

    let name = spec.name.trim();
    if spec.set_default {
        run_action(wsl, &["--set-default", name], QUICK_TIMEOUT)?;
        Ok(format!("{name} 已安装，并设为默认发行版"))
    } else {
        Ok(format!("{name} 已安装"))
    }
}

// ---------------------------------------------------------------------------
// `wsl --manage`（P4）
//
// 四个选项都挂在 `--manage <Distro>` 下面（实测 `wsl.exe --help`）：
//
//     --move <Location>            移动安装位置
//     --set-sparse <true|false>    开关稀疏 VHD
//     --resize <MemoryString>      调整磁盘大小
//     --set-default-user <Name>    设置默认用户
// ---------------------------------------------------------------------------

/// 校验并规范化一个"大小"字符串（`--resize` 用）。
///
/// WSL 的 `MemoryString` 形如 `50GB`：大小写不敏感，数字可以是小数。
/// 这里只做**格式**校验 —— 具体数值合不合理（比当前盘还小？）交给 WSL 判断，
/// 我们不猜。
///
/// 返回规范化后的写法（`50gb` → `50GB`），这样命令预览和日志里是统一的。
pub fn normalize_size(text: &str) -> std::result::Result<String, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("大小不能为空".to_owned());
    }

    // 数字部分：允许数字和小数点
    let split_at = text
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split_at);
    let unit = unit.trim().to_ascii_uppercase();

    if number.is_empty() || number.parse::<f64>().is_err() {
        return Err(format!("「{text}」里没有有效的数字"));
    }

    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if !UNITS.contains(&unit.as_str()) {
        return Err(format!(
            "单位只认 {}（例如 50GB），收到「{unit}」",
            UNITS.join(" / ")
        ));
    }

    Ok(format!("{number}{unit}"))
}

/// 移动发行版到新位置（`wsl --manage <name> --move <dir>`）。
///
/// ⚠️ 跨盘移动是**真的在拷数据**，18 GB 的盘可能要几分钟到几十分钟；
/// 同盘则是改路径，很快。
pub fn move_distro(wsl: &Wsl, name: &str, location: &str) -> Result<()> {
    let location = location.trim();
    if location.is_empty() {
        return Err(Error::InvalidArgument("目标位置不能为空".to_owned()));
    }
    if !is_absolute_windows_path(location) {
        return Err(Error::InvalidArgument(format!(
            "目标位置必须是绝对路径（如 D:\\wsl\\{name}）：{location}"
        )));
    }
    run_action(wsl, &["--manage", name, "--move", location], DISK_TIMEOUT)
}

/// 开关稀疏 VHD（`wsl --manage <name> --set-sparse <true|false>`）。
///
/// 开启后 WSL 会自动回收已释放的块 —— 相当于自动做压缩。
/// ⚠️ 切换这个标志**可能触发一次压缩**，所以给的是磁盘级超时。
pub fn set_sparse(wsl: &Wsl, name: &str, sparse: bool) -> Result<()> {
    let flag = if sparse { "true" } else { "false" };
    run_action(
        wsl,
        &["--manage", name, "--set-sparse", flag],
        DISK_TIMEOUT,
    )
}

/// 调整发行版磁盘大小（`wsl --manage <name> --resize <size>`）。
///
/// `size` 会先过 [`normalize_size`]。
pub fn resize(wsl: &Wsl, name: &str, size: &str) -> Result<()> {
    let size = normalize_size(size).map_err(Error::InvalidArgument)?;
    run_action(wsl, &["--manage", name, "--resize", &size], DISK_TIMEOUT)
}

/// 设置发行版的默认用户（`wsl --manage <name> --set-default-user <user>`）。
pub fn set_default_user(wsl: &Wsl, name: &str, user: &str) -> Result<()> {
    let user = user.trim();
    if user.is_empty() {
        return Err(Error::InvalidArgument("用户名不能为空".to_owned()));
    }
    run_action(
        wsl,
        &["--manage", name, "--set-default-user", user],
        QUICK_TIMEOUT,
    )
}

// ---------------------------------------------------------------------------
// `/etc/wsl.conf`（P4）
// ---------------------------------------------------------------------------

/// 发行版内 `wsl.conf` 的路径。
pub const WSL_CONF_PATH: &str = "/etc/wsl.conf";

/// 把一段文本包成 POSIX shell 的**单引号字面量**。
///
/// 单引号里除了 `'` 本身什么都不会被解释；`'` 的写法是 `'\''`
/// （关引号 → 反斜杠转义的单引号 → 再开引号）。
///
/// ⚠️ **凡是用户输入要进 shell 命令的地方都必须走这里。**
/// 表单里的「默认用户」是自由文本，不转义的话用户写一个
/// `; rm -rf /` 就直接执行了。
pub fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// 读发行版内的 `/etc/wsl.conf`。
///
/// **文件不存在时返回空串**，不是错误 —— 没配过 `wsl.conf` 是完全正常的
/// 状态，不该弹一条红色错误给用户。
///
/// 所以这里用一个**永远退出 0** 的脚本：`[ -f ... ] && cat ... || true`。
/// 比"跑 `cat`，失败后再去猜 stderr 里那句本地化的'没有那个文件'"稳得多。
///
/// 不用 `-u root`：`/etc/wsl.conf` 对任何用户都可读，少一次提权少一层意外。
pub fn read_wsl_conf(wsl: &Wsl, name: &str) -> Result<String> {
    let script = format!("[ -f {WSL_CONF_PATH} ] && cat {WSL_CONF_PATH} || true");
    let out = wsl.run_with_timeout(&["-d", name, "-e", "sh", "-c", &script], QUICK_TIMEOUT)?;

    if !out.success() {
        return Err(Error::NonZeroExit {
            program: "wsl",
            args: out.args.join(" "),
            code: out.code.unwrap_or(-1),
            stderr: {
                let detail = strip_config_warnings(&out.combined());
                if detail.is_empty() {
                    "（wsl 没有给出任何输出）".to_owned()
                } else {
                    detail
                }
            },
        });
    }

    // 只剥可能的前导 BOM：这个文件通常是我们自己写的，
    // 但用户也可能拿别的编辑器存过带 BOM 的版本。
    //
    // ⚠️ **不要**在这里 `trim_end()`。`WslConfDoc::is_dirty()` 是拿
    // `render()` 和原文比字节的，而 `render()` 会照着原文决定结尾换行 ——
    // 一旦把结尾换行（以及 CRLF 末尾那个 `\r`）在这里 trim 掉，两边就
    // **永远**不相等：用户打开 `/etc/wsl.conf`、什么都没改、点一次保存，
    // 也会整文件重写一遍（把我们的键规范化成 `key = value`、丢掉 BOM、
    // 还可能改掉行尾），`.bak` 里也白留一份。
    //
    // `cat` 出来的就是文件的字节，原样交给 `parse` 才是对的。
    Ok(out.stdout.trim_start_matches('\u{feff}').to_owned())
}

/// 写发行版内的 `/etc/wsl.conf`（**先备份**）。
///
/// 备份到 `/etc/wsl.conf.bak`。改坏了还能从那儿捞回来 ——
/// 这个文件写错会让发行版起不来，到时候一句"改坏了"没法交代。
pub fn write_wsl_conf(wsl: &Wsl, name: &str, text: &str) -> Result<()> {
    // 1) 备份。文件不存在时 `cp` 会失败，所以套一层 `[ -f ]`。
    let backup =
        format!("[ -f {WSL_CONF_PATH} ] && cp {WSL_CONF_PATH} {WSL_CONF_PATH}.bak || true");
    run_action(
        wsl,
        &["-d", name, "-u", "root", "-e", "sh", "-c", &backup],
        QUICK_TIMEOUT,
    )?;

    // 2) 写。heredoc 的定界符是**挑过的**（见 `model::wslconf::write_script`），
    //    而且带引号，所以 `$` / 反引号 / 反斜杠都不会被 shell 展开（实测）。
    let script = crate::model::wslconf::write_script(text);
    run_action(
        wsl,
        &["-d", name, "-u", "root", "-e", "sh", "-c", &script],
        QUICK_TIMEOUT,
    )
}

/// 发行版里有没有这个用户。
///
/// 保存 `[user] default` 之前用它挡一下 —— 写一个不存在的用户名会让
/// 发行版**下次启动直接失败**，而错误要到那时候才暴露，很难联想到这里。
pub fn user_exists(wsl: &Wsl, name: &str, user: &str) -> Result<bool> {
    let script = format!("id -u {}", shell_quote(user));
    let out = wsl.run_with_timeout(&["-d", name, "-e", "sh", "-c", &script], QUICK_TIMEOUT)?;
    Ok(out.success())
}

/// 发行版里这个路径存不存在（保存 `[boot] command` 前挡一下）。
pub fn path_exists(wsl: &Wsl, name: &str, path: &str) -> Result<bool> {
    // 先 `test -e`（按路径找），找不到再 `command -v`（按 PATH 找）——
    // `boot.command` 两种写法都常见。
    let script = format!(
        "test -e {p} || command -v {p} >/dev/null 2>&1",
        p = shell_quote(path)
    );
    let out = wsl.run_with_timeout(&["-d", name, "-e", "sh", "-c", &script], QUICK_TIMEOUT)?;
    Ok(out.success())
}

/// 从 `wsl --version` 的输出里抠出版本号（形如 `2.6.1.0`）。
///
/// ⚠️ 输出是**本地化**的 —— 中文系统上第一行是「WSL 版本: 2.6.1.0」，
/// 英文是「WSL version: 2.6.1.0」。所以不能按固定前缀切，只能扫每一行、
/// 取第一个形如 `数字(.数字)+` 的 token。
///
/// 纯函数，能单测。
pub fn parse_version(text: &str) -> Option<String> {
    for line in text.lines() {
        for token in line.split(|c: char| !(c.is_ascii_digit() || c == '.')) {
            // 至少要有一个点，而且每段都是非空数字 —— 否则 `2.` 或者
            // 日期里的 `2024` 都会被当成版本号
            if !token.contains('.') {
                continue;
            }
            let parts: Vec<&str> = token.split('.').collect();
            if parts.len() >= 2
                && parts
                    .iter()
                    .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
            {
                return Some(token.to_owned());
            }
        }
    }
    None
}

/// 拿 WSL 自己的版本号（拿不到时返回空串）。
///
/// 界面用它决定 `[boot]` / `[gpu]` / `[time]` 这几节显不显示 ——
/// 见 [`crate::model::wslconf::section_supported`]。
pub fn wsl_version(wsl: &Wsl) -> String {
    wsl.version()
        .ok()
        .and_then(|text| parse_version(&text))
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// 导出（P3 遗留）
// ---------------------------------------------------------------------------

/// **流式**导出发行版（`wsl --export <name> <file>`）。
///
/// # 为什么这个要流式，而 `--manage` 那批不用
///
/// 导出要跑很久（18 GB 的盘几分钟到几十分钟），用户必须能看见进度、
/// 也必须能取消。所以这里返回 [`StreamHandle`] 而不是跑完才返回。
///
/// # 进度怎么表达
///
/// `wsl --export` **不打百分比**，只偶尔打几行状态。所以界面的进度条
/// 不用它的输出，而是**轮询目标文件长到多大了** —— 那个数字是真实且连续的。
/// 这里的 `on_line` 只用来在出错时留下原因。
///
/// # 格式
///
/// 导出的是 **tar**（`wsl --export` 的默认格式），可以再用
/// [`crate::cmd::distro::InstallSource::Tar`] 导回来，也能喂给人家的机器。
/// 加 `--vhd` 会导出 VHDX，但那个格式只有 WSL 自己认，本程序不做。
///
/// 流式模式**没有内建超时** —— 免得把还在正常导出的任务误杀。
pub fn export_streaming(
    wsl: &Wsl,
    name: &str,
    path: &str,
    on_line: impl Fn(&str) + Send + Sync + 'static,
) -> Result<StreamHandle> {
    let name = name.trim();
    if name.is_empty() {
        return Err(Error::InvalidArgument("发行版名不能为空".to_owned()));
    }

    let path = path.trim();
    if path.is_empty() {
        return Err(Error::InvalidArgument("导出路径不能为空".to_owned()));
    }
    if !is_absolute_windows_path(path) {
        return Err(Error::InvalidArgument(format!(
            "导出路径必须是绝对路径（如 D:\\backup\\{name}.tar）：{path}"
        )));
    }
    // 以分隔符结尾的是**目录**，而 `--export` 要的是文件名。
    // 不挡的话 wsl 会报一句不好懂的错，而且可能先建出一个空文件。
    if path.ends_with(['\\', '/']) {
        return Err(Error::InvalidArgument(format!(
            "导出路径要写成文件名（如 D:\\backup\\{name}.tar），不能只给目录：{path}"
        )));
    }

    wsl.spawn_streaming(&["--export", name, path], on_line)
}

// ---------------------------------------------------------------------------
// 注册表
// ---------------------------------------------------------------------------

/// 注册表里的发行版元数据。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegistryEntry {
    /// 发行版名（`DistributionName`）。
    pub name: String,
    /// 安装位置（`BasePath`）。
    pub base_path: Option<PathBuf>,
    /// 根文件系统虚拟磁盘的**文件名**（`VhdFileName`）。
    ///
    /// 实测是 `ext4.vhdx`。注册表直接给了名字，就不用去猜 / 扫目录。
    pub vhd_file_name: Option<String>,
    /// 默认用户 UID（`DefaultUid`）。
    pub default_uid: Option<u32>,
    /// WSL 版本（`Version`）。
    pub version: Option<u8>,
}

/// 读取注册表里的全部发行版元数据。
///
/// # 失败时返回空列表，而不是错误
///
/// 注册表读不到（被组策略挡了、`reg.exe` 不在 `PATH` 里……）时，
/// 面板**仍然应该能显示发行版列表** —— 只是没有安装位置和磁盘占用。
/// 让整个页面因为一个辅助信息而报错是不划算的。
pub fn read_registry() -> Vec<RegistryEntry> {
    match cli::run_helper("reg.exe", "reg", &["query", LXSS_KEY, "/s"], REG_TIMEOUT) {
        Ok(out) if out.success() => parse_registry(&out.stdout),
        Ok(out) => {
            tracing::warn!(
                "reg query 退出码 {:?}：{}",
                out.code,
                out.combined().trim()
            );
            Vec::new()
        }
        Err(e) => {
            tracing::warn!("读取注册表失败：{e}");
            Vec::new()
        }
    }
}

/// 解析 `reg.exe query <Lxss> /s` 的输出。
///
/// # 实测格式（`docs/PLAN-v0.3.md` §3.4）
///
/// ```text
///
/// HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Lxss
///     DefaultVersion    REG_DWORD    0x2
///     DefaultDistribution    REG_SZ    {856a6800-...}
///
/// HKEY_CURRENT_USER\...\Lxss\{856a6800-...}
///     DistributionName    REG_SZ    Ubuntu-26.04
///     BasePath    REG_SZ    D:\linux\Ubuntu-26.04
///     VhdFileName    REG_SZ    ext4.vhdx
///     DefaultUid    REG_DWORD    0x0
///     Version    REG_DWORD    0x2
/// ```
///
/// 只有**最后一段是 GUID 的键**才算发行版条目 —— 根键最后一段是 `Lxss`。
/// 用这个区分，比匹配整条路径稳（路径前缀可能随版本变）。
pub fn parse_registry(stdout: &str) -> Vec<RegistryEntry> {
    let mut entries: Vec<RegistryEntry> = Vec::new();
    let mut current: Option<RegistryEntry> = None;

    // 收尾：一个条目只有在拿到 `DistributionName` 之后才算数
    // （注册表里可能有空的 GUID 键）。
    fn flush(current: &mut Option<RegistryEntry>, entries: &mut Vec<RegistryEntry>) {
        if let Some(entry) = current.take() {
            if !entry.name.is_empty() {
                entries.push(entry);
            }
        }
    }

    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // 键头
        if trimmed.starts_with("HKEY_") {
            flush(&mut current, &mut entries);
            if is_distro_key(trimmed) {
                current = Some(RegistryEntry::default());
            }
            continue;
        }

        let Some(entry) = current.as_mut() else {
            continue;
        };

        let Some((name, data)) = parse_registry_value(line) else {
            continue;
        };

        match name.as_str() {
            "DistributionName" => entry.name = data,
            "BasePath" => entry.base_path = Some(PathBuf::from(data)),
            "VhdFileName" => entry.vhd_file_name = Some(data),
            "DefaultUid" => entry.default_uid = parse_dword(&data),
            "Version" => {
                entry.version = parse_dword(&data).and_then(|v| u8::try_from(v).ok());
            }
            // 其余键（State / Flags / Flavor / ShortcutPath …）暂时用不上。
            // 不是错误，忽略即可。
            _ => {}
        }
    }

    flush(&mut current, &mut entries);
    entries
}

/// 键路径的最后一段是不是 GUID（`{...}`）。
fn is_distro_key(path: &str) -> bool {
    path.rsplit('\\')
        .next()
        .is_some_and(|last| last.starts_with('{') && last.ends_with('}'))
}

/// 拆一行注册表值。
///
/// ```text
/// "    DefaultVersion    REG_DWORD    0x2"  →  ("DefaultVersion", "0x2")
/// "    BasePath    REG_SZ    D:\my path"   →  ("BasePath", "D:\my path")
/// ```
///
/// 用「找 `REG_` 类型词」来切，**不按固定列宽** ——
/// 值名长度不同、列宽会变，而数据里可能有空格（路径）。
fn parse_registry_value(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim();

    // 第一次出现的 `REG_` 就是类型词：值名里不会含它（我们要的那几个都不含）。
    let (name, rest) = trimmed.split_once("REG_")?;
    let name = name.trim();
    if name.is_empty() {
        return None;
    }

    // rest 形如 "DWORD    0x2" / "SZ    D:\my path"
    let (_kind, data) = rest.split_once(char::is_whitespace)?;

    Some((name.to_owned(), data.trim().to_owned()))
}

/// 解析 `0x2` / `0xf` 这类十六进制 DWORD。
fn parse_dword(text: &str) -> Option<u32> {
    let hex = text.trim();
    let hex = hex
        .strip_prefix("0x")
        .or_else(|| hex.strip_prefix("0X"))
        .unwrap_or(hex);
    u32::from_str_radix(hex, 16).ok()
}

// ---------------------------------------------------------------------------
// 合并：注册表信息 + 磁盘占用 → 发行版列表
// ---------------------------------------------------------------------------

/// 把注册表信息与磁盘占用合并进发行版列表。
///
/// 名字匹配**大小写不敏感** —— 理论上两边一致，但没必要因为大小写差异丢数据。
pub fn enrich(distros: &mut [Distro]) {
    let entries = read_registry();

    for distro in distros.iter_mut() {
        let Some(entry) = entries
            .iter()
            .find(|e| e.name.eq_ignore_ascii_case(&distro.name))
        else {
            continue;
        };

        distro.base_path = entry.base_path.clone();
        distro.default_uid = entry.default_uid;
        // `-l -v` 没给出可用版本时，用注册表的补上
        if distro.version.is_none() {
            distro.version = entry.version;
        }

        let Some(base) = entry.base_path.as_deref() else {
            continue;
        };
        let Some(vhdx) = find_vhdx(base, entry.vhd_file_name.as_deref()) else {
            continue;
        };
        distro.vhdx_bytes = file_len(&vhdx);
        distro.vhdx_path = Some(vhdx);
    }
}

/// 找到发行版的虚拟磁盘文件。
///
/// 优先用注册表报的 `VhdFileName`（实测 `ext4.vhdx`）——
/// 这比"扫目录里最大的 `.vhdx`"可靠（用户可能往目录里放了别的东西）。
/// 注册表没报时才退回到扫 `.vhdx`，并取最大的那个。
fn find_vhdx(base: &Path, file_name: Option<&str>) -> Option<PathBuf> {
    if let Some(name) = file_name {
        let candidate = base.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    let mut best: Option<(PathBuf, u64)> = None;
    for entry in std::fs::read_dir(base).ok()?.flatten() {
        let path = entry.path();
        let is_vhdx = path
            .extension()
            .map(|ext| ext.to_string_lossy().eq_ignore_ascii_case("vhdx"))
            .unwrap_or(false);
        if !is_vhdx {
            continue;
        }

        let len = entry.metadata().map(|m| m.len()).unwrap_or(0);
        // 不用 `Option::is_none_or` —— 它在 1.82 才稳定，
        // 而本 crate 声明的 MSRV 是 1.75。
        let better = match &best {
            None => true,
            Some((_, best_len)) => len > *best_len,
        };
        if better {
            best = Some((path, len));
        }
    }

    best.map(|(path, _)| path)
}

/// 文件长度（字节）；读不到时 `None`。
fn file_len(path: &Path) -> Option<u64> {
    std::fs::metadata(path).ok().map(|m| m.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 本机实测的 `reg.exe query ... /s` 输出。
    const REAL_REGISTRY: &str = include_str!("../../tests/fixtures/lxss_query.txt");

    #[test]
    fn parses_the_real_registry_dump() {
        let entries = parse_registry(REAL_REGISTRY);

        assert_eq!(entries.len(), 1, "根键不该被当成发行版条目：{entries:?}");

        let e = &entries[0];
        assert_eq!(e.name, "Ubuntu-26.04");
        assert_eq!(e.base_path.as_deref(), Some(Path::new(r"D:\linux\Ubuntu-26.04")));
        assert_eq!(e.vhd_file_name.as_deref(), Some("ext4.vhdx"));
        assert_eq!(e.default_uid, Some(0));
        assert_eq!(e.version, Some(2));
    }

    #[test]
    fn root_key_is_not_treated_as_a_distro() {
        let entries = parse_registry(REAL_REGISTRY);
        assert!(
            !entries.iter().any(|e| e.name.is_empty()),
            "空名字的条目应被丢弃"
        );
    }

    #[test]
    fn parses_multiple_distros() {
        let text = "\
\r
HKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\CurrentVersion\\Lxss\r
    DefaultVersion    REG_DWORD    0x2\r
\r
HKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\CurrentVersion\\Lxss\\{aaa}\r
    DistributionName    REG_SZ    Debian\r
    BasePath    REG_SZ    C:\\wsl\\Debian\r
    DefaultUid    REG_DWORD    0x3e8\r
    Version    REG_DWORD    0x2\r
\r
HKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\CurrentVersion\\Lxss\\{bbb}\r
    DistributionName    REG_SZ    Ubuntu 24.04 LTS\r
    BasePath    REG_SZ    D:\\linux\\Ubuntu 24.04 LTS\r
    Version    REG_DWORD    0x1\r
";
        let entries = parse_registry(text);
        assert_eq!(entries.len(), 2, "{entries:?}");

        assert_eq!(entries[0].name, "Debian");
        assert_eq!(entries[0].default_uid, Some(1000), "0x3e8 = 1000");
        assert_eq!(entries[1].name, "Ubuntu 24.04 LTS");
        assert_eq!(
            entries[1].base_path.as_deref(),
            Some(Path::new(r"D:\linux\Ubuntu 24.04 LTS")),
            "路径里的空格不该把值截断"
        );
        assert_eq!(entries[1].version, Some(1));
    }

    #[test]
    fn distro_key_entries_without_a_name_are_dropped() {
        let text = "\
HKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\CurrentVersion\\Lxss\\{ccc}\r
    BasePath    REG_SZ    C:\\empty\r
";
        assert!(parse_registry(text).is_empty());
    }

    #[test]
    fn is_distro_key_requires_a_guid_last_segment() {
        assert!(is_distro_key(
            r"HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Lxss\{856a6800-7118-4a80-b013-a88c95dc788f}"
        ));
        assert!(!is_distro_key(
            r"HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Lxss"
        ));
        // 半截大括号不算
        assert!(!is_distro_key(r"HKEY_CURRENT_USER\Software\{abc"));
    }

    #[test]
    fn registry_value_parsing_handles_every_type_prefix() {
        assert_eq!(
            parse_registry_value("    DefaultVersion    REG_DWORD    0x2"),
            Some(("DefaultVersion".to_owned(), "0x2".to_owned()))
        );
        assert_eq!(
            parse_registry_value("    BasePath    REG_SZ    D:\\wsl"),
            Some(("BasePath".to_owned(), "D:\\wsl".to_owned()))
        );
        assert_eq!(
            parse_registry_value("    ShortcutPath    REG_EXPAND_SZ    %USERPROFILE%\\x.lnk"),
            Some((
                "ShortcutPath".to_owned(),
                "%USERPROFILE%\\x.lnk".to_owned()
            ))
        );
        // 键头 / 空行 / 没有类型词的行都不该被当成值
        assert_eq!(parse_registry_value("HKEY_CURRENT_USER\\Software"), None);
        assert_eq!(parse_registry_value(""), None);
        assert_eq!(parse_registry_value("    REG_SZ    x"), None);
    }

    #[test]
    fn dword_parsing_accepts_hex_with_and_without_prefix() {
        assert_eq!(parse_dword("0x2"), Some(2));
        assert_eq!(parse_dword("0xf"), Some(15));
        assert_eq!(parse_dword("0x0"), Some(0));
        assert_eq!(parse_dword("0X10"), Some(16));
        assert_eq!(parse_dword("  0x3e8  "), Some(1000));
        assert_eq!(parse_dword("not-a-number"), None);
    }

    #[test]
    fn empty_registry_output_yields_nothing() {
        assert!(parse_registry("").is_empty());
        assert!(parse_registry("\r\n\r\n").is_empty());
    }

    #[test]
    fn default_distro_picks_the_starred_entry() {
        let distros = vec![
            Distro::new("Debian", crate::model::distro::DistroState::Stopped, Some(2), false),
            Distro::new("Ubuntu", crate::model::distro::DistroState::Running, Some(2), true),
        ];
        assert_eq!(default_distro(&distros), Some("Ubuntu"));
        assert_eq!(default_distro(&[]), None);
    }

    // -- 动作 --------------------------------------------------------------

    #[test]
    fn this_project_only_targets_wsl_2() {
        // 这个常量是"只支持 WSL 2"这件事的**唯一**落点：
        // 所有新建发行版的路径都从这里取版本号。
        // 哪天要支持 WSL 1，改它之前先想清楚 `--manage` 那一批在 WSL 1 上还成不成立。
        assert_eq!(WSL_VERSION, 2);
    }

    #[test]
    fn config_warnings_are_stripped_from_error_text() {
        // 实测形态：两行 .wslconfig 告警 + 一行真正的错误
        let raw = concat!(
            "wsl: interop.appendWindowsPath:C:\\Users\\76434\\.wslconfig 中的键\"12\"未知\n",
            "wsl: user.default:C:\\Users\\76434\\.wslconfig 中的键\"15\"未知\n",
            "不存在具有所提供名称的分发。"
        );
        let cleaned = strip_config_warnings(raw);
        assert_eq!(cleaned, "不存在具有所提供名称的分发。");
        assert!(!cleaned.contains("wsl:"), "{cleaned}");
    }

    #[test]
    fn config_warning_filter_keeps_everything_else() {
        // 没有告警时不该改动任何东西
        assert_eq!(strip_config_warnings("操作成功完成。"), "操作成功完成。");
        // 只想去掉行首的 `wsl: ` 前缀行，正文里的 "wsl" 不受影响
        let text = "wsl: 某条告警\n真正的错误：wsl 拒绝了这个参数";
        assert_eq!(strip_config_warnings(text), "真正的错误：wsl 拒绝了这个参数");
        // 全被过滤掉时是空串，调用方据此给兜底文案
        assert!(strip_config_warnings("wsl: a\nwsl: b").is_empty());
        assert!(strip_config_warnings("").is_empty());
    }

    // -- 添加实例 ----------------------------------------------------------

    /// `Vec<String>` → `Vec<&str>`，方便断言。
    fn args_of(spec: &InstallSpec) -> Vec<String> {
        spec.to_args()
    }

    #[test]
    fn tar_import_args_are_exact() {
        let mut spec = InstallSpec::new(
            "Ubuntu",
            InstallSource::Tar {
                path: r"D:\img\ubuntu.tar".to_owned(),
            },
        );
        spec.install_dir = r"D:\wsl\Ubuntu".to_owned();

        assert_eq!(
            args_of(&spec),
            vec![
                "--import",
                "Ubuntu",
                r"D:\wsl\Ubuntu",
                r"D:\img\ubuntu.tar",
                "--version",
                "2"
            ]
        );
        assert!(spec.validate().is_ok());
    }

    #[test]
    fn new_distros_are_always_created_as_wsl_2() {
        // 显式传 `--version 2`，**不**吃 WSL 的默认值 ——
        // 那个值能被用户改成 1，而建出来的 WSL 1 发行版本程序管不了。
        let mut tar = InstallSpec::new(
            "Ubuntu",
            InstallSource::Tar {
                path: r"D:\img\ubuntu.tar".to_owned(),
            },
        );
        tar.install_dir = r"D:\wsl\Ubuntu".to_owned();
        let args = args_of(&tar);
        let at = args
            .iter()
            .position(|a| a == "--version")
            .expect("tar 导入应该带 --version");
        assert_eq!(args.get(at + 1).map(String::as_str), Some("2"), "{args:?}");

        let online = InstallSpec::new(
            "Ubuntu-24.04",
            InstallSource::Online { launch: false },
        );
        let args = args_of(&online);
        assert!(args.iter().any(|a| a == "--version"), "{args:?}");
        assert!(args.iter().any(|a| a == "2"), "{args:?}");

        // ⚠️ 从文件安装**确实**给不了版本（`--from-file` 没有这个选项），
        // 这一条要如实承认，不能假装我们也指定了。
        let file = InstallSpec::new(
            "X",
            InstallSource::File {
                path: r"D:\a.tar".to_owned(),
            },
        );
        let args = args_of(&file);
        assert!(!args.iter().any(|a| a == "--version"), "{args:?}");
    }

    #[test]
    fn online_install_adds_no_launch_unless_asked() {
        let quiet = InstallSpec::new(
            "Ubuntu-24.04",
            InstallSource::Online { launch: false },
        );
        assert_eq!(
            args_of(&quiet),
            vec![
                "--install",
                "-d",
                "Ubuntu-24.04",
                "--version",
                "2",
                "--no-launch"
            ]
        );
        // 在线安装**不需要**安装目录（WSL 有自己的默认位置）
        assert!(quiet.validate().is_ok());

        // 要求装完启动时，`--no-launch` 就不该出现
        let loud = InstallSpec::new("Ubuntu-24.04", InstallSource::Online { launch: true });
        let args = args_of(&loud);
        assert!(!args.iter().any(|a| a == "--no-launch"), "{args:?}");
        assert_eq!(
            args,
            vec!["--install", "-d", "Ubuntu-24.04", "--version", "2"]
        );
    }

    #[test]
    fn file_install_passes_name_and_optional_location() {
        let mut spec = InstallSpec::new(
            "MyDistro",
            InstallSource::File {
                path: r"D:\img\rootfs.tar.gz".to_owned(),
            },
        );
        assert_eq!(
            args_of(&spec),
            vec![
                "--install",
                "--from-file",
                r"D:\img\rootfs.tar.gz",
                "--name",
                "MyDistro"
            ]
        );

        // 给了安装目录就补上 --location
        spec.install_dir = r"E:\wsl\MyDistro".to_owned();
        let args = args_of(&spec);
        assert!(args.iter().any(|a| a == "--location"), "{args:?}");
        assert!(spec.validate().is_ok());
    }

    #[test]
    fn install_spec_rejects_bad_input() {
        let online = |name: &str| InstallSpec::new(name, InstallSource::Online { launch: false });

        // 名字空 / 全空白
        for bad in ["", "   "] {
            assert!(online(bad).validate().is_err(), "{bad:?} 应该被拒");
        }

        // 名字含路径分隔符
        for bad in [r"a\b", "a/b"] {
            let err = online(bad).validate().unwrap_err();
            assert!(err.contains('\\') || err.contains('/'), "{err}");
        }

        // tar 导入必须给安装目录
        let no_dir = InstallSpec::new(
            "X",
            InstallSource::Tar {
                path: r"D:\a.tar".to_owned(),
            },
        );
        assert!(no_dir.validate().unwrap_err().contains("安装目录"));

        // 安装目录必须是绝对路径
        let mut relative = InstallSpec::new(
            "X",
            InstallSource::Tar {
                path: r"D:\a.tar".to_owned(),
            },
        );
        relative.install_dir = r"wsl\X".to_owned();
        assert!(relative.validate().unwrap_err().contains("绝对路径"));

        // tar 文件路径空
        let mut no_path = InstallSpec::new(
            "X",
            InstallSource::Tar {
                path: "  ".to_owned(),
            },
        );
        no_path.install_dir = r"D:\wsl\X".to_owned();
        assert!(no_path.validate().is_err());

        // 从文件安装也要求路径非空
        let empty_file = InstallSpec::new(
            "X",
            InstallSource::File {
                path: String::new(),
            },
        );
        assert!(empty_file.validate().is_err());
    }

    #[test]
    fn absolute_path_check_does_not_depend_on_the_build_platform() {
        assert!(is_absolute_windows_path(r"D:\wsl"));
        assert!(is_absolute_windows_path("D:/wsl"));
        assert!(is_absolute_windows_path(r"\\server\share"));
        assert!(is_absolute_windows_path(r"c:\x"));

        assert!(!is_absolute_windows_path(r"wsl\X"));
        assert!(!is_absolute_windows_path("wsl"));
        assert!(!is_absolute_windows_path("D:"));
        assert!(!is_absolute_windows_path(r"D:relative"));
        assert!(!is_absolute_windows_path(""));
    }

    #[test]
    fn set_default_becomes_a_second_command_not_an_option() {
        // `--import` / `--install` 都不接受 `--set-default`（实测 --help），
        // 所以它必须是**第二条命令**，预览里也要如实显示两行。
        let mut spec = InstallSpec::new(
            "Ubuntu",
            InstallSource::Tar {
                path: r"D:\a.tar".to_owned(),
            },
        );
        spec.install_dir = r"D:\wsl\Ubuntu".to_owned();
        spec.set_default = true;

        let args = args_of(&spec);
        assert!(
            !args.iter().any(|a| a == "--set-default"),
            "安装命令里不该出现 --set-default：{args:?}"
        );

        let lines = spec.preview_lines();
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].starts_with("wsl --import"), "{lines:?}");
        assert_eq!(lines[1], "wsl --set-default Ubuntu");
    }

    #[test]
    fn preview_is_a_single_line_when_not_setting_default() {
        let spec = InstallSpec::new("X", InstallSource::Online { launch: false });
        let lines = spec.preview_lines();
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(lines[0], "wsl --install -d X --version 2 --no-launch");
    }

    #[test]
    fn install_rejects_an_invalid_spec_before_spawning() {
        // 用一个必然不存在的可执行文件：校验要是没挡住，
        // 拿到的会是 ExecutableNotFound 而不是 InvalidArgument。
        let wsl = Wsl::with_program("definitely-not-a-real-binary");
        let spec = InstallSpec::new("  ", InstallSource::Online { launch: false });
        assert!(matches!(
            install(&wsl, &spec),
            Err(Error::InvalidArgument(_))
        ));
    }

    // -- wsl --manage ------------------------------------------------------

    #[test]
    fn size_is_normalized_and_validated() {
        assert_eq!(normalize_size("50GB").unwrap(), "50GB");
        assert_eq!(normalize_size(" 50gb ").unwrap(), "50GB");
        assert_eq!(normalize_size("1.5TB").unwrap(), "1.5TB");
        assert_eq!(normalize_size("512MB").unwrap(), "512MB");
        // 数字和单位之间的空格会被吃掉，命令里就是紧凑写法
        assert_eq!(normalize_size("100 KB").unwrap(), "100KB");

        // 没有数字
        for bad in ["GB", "", "   "] {
            assert!(normalize_size(bad).is_err(), "{bad:?}");
        }

        // 单位不认识 —— 注意 `50`（没有单位）也要拒掉
        for bad in ["50", "50PB", "50GiB", "50g"] {
            let err = normalize_size(bad).unwrap_err();
            assert!(err.contains("单位只认"), "{bad} → {err}");
        }
    }

    #[test]
    fn move_requires_an_absolute_target() {
        // 用一个必然不存在的可执行文件：校验要是没挡住，
        // 拿到的会是 ExecutableNotFound 而不是 InvalidArgument。
        let wsl = Wsl::with_program("definitely-not-a-real-binary");

        assert!(matches!(
            move_distro(&wsl, "X", r"wsl\X"),
            Err(Error::InvalidArgument(_))
        ));
        assert!(matches!(
            move_distro(&wsl, "X", "   "),
            Err(Error::InvalidArgument(_))
        ));
    }

    #[test]
    fn resize_rejects_a_bad_size_before_spawning() {
        let wsl = Wsl::with_program("definitely-not-a-real-binary");
        assert!(matches!(
            resize(&wsl, "X", "50PB"),
            Err(Error::InvalidArgument(_))
        ));
    }

    #[test]
    fn set_default_user_rejects_an_empty_name() {
        let wsl = Wsl::with_program("definitely-not-a-real-binary");
        assert!(matches!(
            set_default_user(&wsl, "X", "  "),
            Err(Error::InvalidArgument(_))
        ));
    }

    // -- 导出 --------------------------------------------------------------

    #[test]
    fn export_rejects_bad_input_before_spawning() {
        // 用一个必然不存在的可执行文件：校验要是没挡住，
        // 拿到的会是 ExecutableNotFound 而不是 InvalidArgument。
        let wsl = Wsl::with_program("definitely-not-a-real-binary");
        // 不捕获任何东西的闭包是 `Copy`，可以重复用
        let noop = |_: &str| {};

        for bad in [r"backup\a.tar", "   ", r"D:\backup\", "D:/backup/"] {
            assert!(
                matches!(
                    export_streaming(&wsl, "Ubuntu", bad, noop),
                    Err(Error::InvalidArgument(_))
                ),
                "路径 {bad:?} 应该被本地挡下"
            );
        }

        // 发行版名不能为空
        assert!(matches!(
            export_streaming(&wsl, "  ", r"D:\backup\a.tar", noop),
            Err(Error::InvalidArgument(_))
        ));
    }

    #[test]
    fn export_walks_past_validation_and_tries_to_spawn() {
        // 校验通过后就该真的去起进程。这里用不存在的 exe，
        // 所以拿到 ExecutableNotFound 恰恰说明**已经走到起进程那一步了** ——
        // 这正是这条测试想钉住的边界。
        let wsl = Wsl::with_program("definitely-not-a-real-binary");
        let result = export_streaming(&wsl, "Ubuntu", r"D:\backup\ubuntu.tar", |_: &str| {});
        assert!(matches!(result, Err(Error::ExecutableNotFound { .. })));
    }

    // -- /etc/wsl.conf -----------------------------------------------------

    #[test]
    fn shell_quote_neutralizes_injection() {
        // 用户输入的默认用户名会进 shell 命令（`id -u <user>`），必须转义。
        assert_eq!(shell_quote("ubuntu"), "'ubuntu'");
        assert_eq!(shell_quote(""), "''");
        // 单引号本身：关引号 → 转义的单引号 → 再开引号
        assert_eq!(shell_quote("it's"), r"'it'\''s'");

        // 这些在单引号里都只是普通字符
        for nasty in ["; rm -rf /", "$(whoami)", "`id`", "a b", "a\"b", "a\\b"] {
            let quoted = shell_quote(nasty);
            assert!(quoted.starts_with('\'') && quoted.ends_with('\''), "{quoted}");
            // 关键：**不能**出现未被包裹的引号 —— 那才是能逃出去的地方
            assert!(
                !quoted[1..quoted.len() - 1].contains('\''),
                "内部还有裸引号：{quoted}"
            );
        }
    }

    #[test]
    fn shell_quote_survives_a_real_shell_when_available() {
        // 有 `sh` 就**真的**跑一遍 —— 比对照自己写的逆运算有说服力得多。
        // 没有就跳过：CI 的 windows runner 上不一定有 sh 在 PATH 里。
        if std::process::Command::new("sh")
            .arg("-c")
            .arg("exit 0")
            .output()
            .is_err()
        {
            eprintln!("没有 sh，跳过这条");
            return;
        }

        for original in [
            "ubuntu",
            "it's",
            "a b",
            "; rm -rf /tmp/nope",
            "$(id)",
            "`id`",
            "a\\b",
        ] {
            let script = format!("printf '%s' {}", shell_quote(original));
            let out = std::process::Command::new("sh")
                .arg("-c")
                .arg(&script)
                .output()
                .expect("跑 sh 失败");
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                original,
                "原文 {original:?} 过了一遍 shell 之后变了"
            );
        }
    }

    #[test]
    fn parses_the_version_out_of_a_localized_wsl_version_output() {
        // 中文系统（实测本机就是这个形态）
        let zh = "WSL 版本: 2.6.1.0\n内核版本: 6.6.87.2-1\nWSLg 版本: 1.0.66\n";
        assert_eq!(parse_version(zh).as_deref(), Some("2.6.1.0"));

        // 英文
        let en = "WSL version: 2.6.1.0\nKernel version: 6.6.87.2-1\n";
        assert_eq!(parse_version(en).as_deref(), Some("2.6.1.0"));

        // 拿不到
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("没有版本号的一行"), None);
        // 纯数字（没有点）不算版本号 —— 否则日期/构建号会被当版本
        assert_eq!(parse_version("构建 20241010"), None);
    }

    #[test]
    fn version_parsing_rejects_malformed_tokens() {
        // 结尾一个点：`["2", ""]` 里有空段，不算
        assert_eq!(parse_version("版本 2."), None);
        assert_eq!(parse_version("版本 .2"), None);
        // 段落里有合法 token 时要跳过不合法的，而不是整个放弃
        assert_eq!(parse_version("v. 然后 1.2.3"), Some("1.2.3".to_owned()));
    }
}
