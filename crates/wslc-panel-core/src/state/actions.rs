//! 待确认 / 待执行的动作枚举（容器域与发行版域各一套），以及提示条。

use super::prelude::*;
use crate::util::format_bytes;

/// 需要用户二次确认的危险操作（**`wslc.exe` 这一域**）。
///
/// 容器 / 镜像 / 网络 / 卷 / 会话都属于 `wslc`；`wsl.exe` 那一域见
/// [`DistroAction`]。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingAction {
    /// 停止容器。
    StopContainer(String),
    /// **停止所有正在运行的容器**（「容器」页工具栏上那颗「停止全部」）。
    ///
    /// 为什么不是 `StopAllContainers(Vec<String>)`：那样带走的是一份
    /// **点按钮那一刻**的快照，而用户从确认框弹出来到点「全部停止」之间
    /// 可能又起了别的容器。执行时现查 `list_running` 拿到的才是"现在正在跑的"，
    /// 这和 [`PendingAction::PruneContainers`] 是一致的 —— 那个也是执行时
    /// 才去问 `wslc`，不带着名单走。
    ///
    /// `running` **只用于文案**：确认框一开就盖住了工具栏上那行"N 个在运行"，
    /// 影响面得在正文里再写一遍（和 `DistroAction::Unregister` 带 `vhdx_bytes`
    /// 是同一条理由：代价要在点之前说清）。它**不参与任何判断** ——
    /// 真要停哪些以 `execute` 当场查到的那份为准，两者不一致时以执行结果为准。
    StopAllContainers {
        /// 请求时快照里正在运行的容器数，仅供确认文案。
        running: usize,
    },
    /// 强制终止容器。
    KillContainer(String),
    /// 删除容器。
    RemoveContainer(String),
    /// 清理所有已停止的容器。
    PruneContainers,
    /// 删除镜像。
    RemoveImage(String),
    /// 删除网络。
    RemoveNetwork(String),
    /// 删除卷。
    RemoveVolume(String),
    /// 清理所有**没有被任何容器使用**的卷。
    PruneVolumes,
    /// 删除一个**数据库**（数据库管理页）。
    ///
    /// 为什么要带凭据：`PendingAction::execute` 只拿到 `&Wslc`，
    /// 而 `DROP DATABASE` 得先连上 mysqld —— 连接信息必须随动作一起过来。
    /// （`MysqlCreds` 的 `Debug` 是脱敏的，所以进 `AppState::confirm` 是安全的。）
    DropDatabase {
        /// 目标容器。
        container: String,
        /// 库名。
        database: String,
        /// 容器里的客户端二进制名（探测得来，别在这里再猜一次）。
        client: String,
        /// 连接凭据。
        creds: MysqlCreds,
    },
    /// 删除一个 **MySQL 用户**。
    ///
    /// `'user'@'host'` 才是完整身份（实测 `root@%` 和 `root@localhost` 是两条
    /// 独立的记录），所以两个都要带着。
    DropUser {
        /// 目标容器。
        container: String,
        /// 用户名。
        user: String,
        /// 主机。
        host: String,
        /// 容器里的客户端二进制名。
        client: String,
        /// 连接凭据。
        creds: MysqlCreds,
    },
    /// **恢复**一份备份（v0.8）—— 覆盖该库的现有数据，所以走二次确认。
    ///
    /// dump 从 **Windows 侧存档**送进容器再恢复（`push_dump_to_container`
    /// 的注释里有那三步的实测依据）。所以容器重建过、`/tmp` 被清过都**不影响**
    /// —— 只要那份 Windows 文件还在，恢复就能进行。
    RestoreDatabase {
        /// 目标容器。
        container: String,
        /// 要恢复的库（dump 是 `--databases` 出来的，恢复回**原名**）。
        database: String,
        /// 容器里的客户端二进制名。
        client: String,
        /// **Windows 侧**那份 dump 的路径（恢复的输入）。
        windows_path: std::path::PathBuf,
        /// 连接凭据。
        creds: MysqlCreds,
    },
    /// **删除**一份备份文件（v0.10.4 补的二次确认）。
    ///
    /// # 为什么它必须有确认，而「恢复」本来就有
    ///
    /// 原先正好反着：**恢复**（覆盖数据、但旧库还能再备份一份）有确认，
    /// **删除**（Windows 侧文件 + 容器内 `rm -f`，一起没，没有任何地方能找回来）
    /// 反而直接执行。用户在备份浮层里连着点两下就够了 —— 那份备份不可再生。
    ///
    /// 它不能像别的动作那样塞进 `PendingAction::execute`：删一份备份要做
    /// "删本地文件 + 进容器 rm + 重扫目录"三件事，而 `execute` 的形状是
    /// "跑一条 `wslc` 命令 → 返回字符串"。所以这里只带**够定位那一条**的字段，
    /// 真正的执行仍在 `Shell::delete_db_backup_confirmed` 里。
    DeleteDatabaseBackup {
        /// 目标容器（容器内那份也要删）。
        container: String,
        /// 这份备份属于哪个库（只用于文案）。
        database: String,
        /// **Windows 侧**那份文件的路径（主要产物，删不掉要报错）。
        windows_path: std::path::PathBuf,
        /// **容器内**那份的路径（尽力删，删不掉不影响结果）。
        container_path: String,
    },
}

impl PendingAction {
    /// 确认弹窗标题。
    pub fn title(&self) -> &'static str {
        match self {
            PendingAction::StopContainer(_) => "停止容器",
            PendingAction::StopAllContainers { .. } => "停止所有容器",
            PendingAction::KillContainer(_) => "强制终止容器",
            PendingAction::RemoveContainer(_) => "删除容器",
            PendingAction::PruneContainers => "清理已停止的容器",
            PendingAction::RemoveImage(_) => "删除镜像",
            PendingAction::RemoveNetwork(_) => "删除网络",
            PendingAction::RemoveVolume(_) => "删除卷",
            PendingAction::PruneVolumes => "清理未使用的卷",
            PendingAction::DropDatabase { .. } => "删除数据库",
            PendingAction::DropUser { .. } => "删除 MySQL 用户",
            PendingAction::RestoreDatabase { .. } => "恢复备份",
            PendingAction::DeleteDatabaseBackup { .. } => "删除备份文件",
        }
    }

    /// 确认弹窗正文。
    pub fn body(&self) -> String {
        match self {
            PendingAction::StopContainer(name) => {
                format!("将向容器 {name} 发送停止信号。容器内的未保存数据会丢失。")
            }
            PendingAction::StopAllContainers { running } => format!(
                "将向**所有正在运行的容器**（{running} 个）发送停止信号。\
                 这些容器里未保存的数据会丢失；已经停着的容器不受影响。"
            ),
            PendingAction::KillContainer(name) => {
                format!("将立即杀死容器 {name}（SIGKILL），不做优雅退出。")
            }
            PendingAction::RemoveContainer(name) => {
                format!("将删除容器 {name} 及其可写层。此操作不可撤销。")
            }
            PendingAction::PruneContainers => {
                "将删除所有**已停止**的容器及其可写层。此操作不可撤销。".to_owned()
            }
            PendingAction::RemoveImage(reference) => {
                format!("将删除镜像 {reference}。若仍有容器引用它，删除会失败。")
            }
            PendingAction::RemoveNetwork(name) => {
                format!("将删除网络 {name}。仍有容器连接时会失败。")
            }
            PendingAction::RemoveVolume(name) => {
                format!("将删除卷 {name} 及其中的数据。此操作不可撤销。")
            }
            PendingAction::PruneVolumes => {
                "将删除所有**没有被任何容器使用**的卷及其中的数据。此操作不可撤销。\
                 正在被使用的卷不受影响；一个都没有时什么也不会发生。"
                    .to_owned()
            }
            PendingAction::DropDatabase { database, .. } => format!(
                "将执行 `DROP DATABASE {database}` —— 这个库里**所有表和数据**都没了。\
                 此操作不可撤销，面板里也没有回收站。\
                 （挂着的卷不受影响：库是 mysqld 管的，删库不删卷。）"
            ),
            PendingAction::DropUser { user, host, .. } => format!(
                "将执行 `DROP USER '{user}'@'{host}'` —— 这个账号会被删掉，\
                 已经授出去的权限一并消失。已经建立的连接不会立刻断开，\
                 但之后它就连不上了。此操作不可撤销。"
            ),
            PendingAction::RestoreDatabase {
                database,
                windows_path,
                ..
            } => format!(
                "将把备份 `{}` 恢复进数据库 **{database}** ——\
                 这个库**现在的全部数据会被覆盖**（dump 是 `--databases` 形式，\
                 恢复只能回原库）。此操作不可撤销。",
                windows_path.display()
            ),
            PendingAction::DeleteDatabaseBackup {
                container,
                database,
                windows_path,
                container_path,
            } => format!(
                "将**永久删除**这份备份：\n\
                 {}\n\
                 （容器 {container} 里的 `{container_path}` 也一起删。）\n\n\
                 数据库 {database} 本身不受影响，但这份存档再也没有第二份 —— \
                 面板里没有回收站，删了就得重新备份。",
                windows_path.display()
            ),
        }
    }

    /// 按钮文案。
    pub fn confirm_label(&self) -> &'static str {
        match self {
            PendingAction::StopContainer(_) => "停止",
            PendingAction::StopAllContainers { .. } => "全部停止",
            PendingAction::KillContainer(_) => "强制终止",
            PendingAction::RemoveContainer(_) | PendingAction::PruneContainers => "删除",
            PendingAction::RemoveImage(_) => "删除镜像",
            PendingAction::RemoveNetwork(_) => "删除网络",
            PendingAction::RemoveVolume(_) => "删除卷",
            PendingAction::PruneVolumes => "清理",
            PendingAction::DropDatabase { .. } => "删除数据库",
            PendingAction::DropUser { .. } => "删除用户",
            PendingAction::RestoreDatabase { .. } => "恢复",
            PendingAction::DeleteDatabaseBackup { .. } => "永久删除",
        }
    }

    /// 执行操作，返回给用户看的结果摘要。
    pub fn execute(&self, wslc: &Wslc) -> Result<String> {
        match self {
            PendingAction::StopContainer(name) => {
                let done = cmd::container::stop(wslc, std::slice::from_ref(name))?;
                Ok(format!("已停止：{}", done.join("、")))
            }
            // 名单现查，不用界面上那份快照（理由写在枚举上那段注释里）。
            //
            // 一个都没在跑时**不能**照样调 `stop`：`cmd::container::stop` 收到空名单
            // 会返回 `InvalidArgument`（`run_names` 里那道检查），于是"确认框开着
            // 的时候容器已经被别的操作停掉了"会变成一条报错 —— 那不是错误，
            // 是用户想要的结果已经达成了。
            //
            // 一条命令停多个，而不是在这里自己循环。实测（本机 WSL 3.0.3.0）：
            //
            // - `wslc stop nginx redis` → 两行 stdout（名字，顺序与传入一致）、退出码 0
            //   —— 正好是 `run_names` 收集的那份，所以摘要里的名字是准的；
            // - 停一个**已经停着**的容器 → 退出码 0（没有"它没在跑"这种报错）；
            // - 只有"名字不存在"才失败，而且是**逐个**报错、把整个名单走完再以 1 退出
            //   （`wslc container stop no-such-a no-such-b` → 两条
            //   `WSLC_E_CONTAINER_NOT_FOUND`）。
            //
            // 也就是说"列表和执行之间容器自己退了"不会变成失败；真会失败的只剩
            // "那一瞬间它被删掉了"这种很窄的竞态 —— 那种情况下其余容器照样停了，
            // 错误信息里带着是哪个名字。拆成 N 条命令换不来什么，只多 N 次进程开销。
            PendingAction::StopAllContainers { .. } => {
                let running = cmd::container::list_running(wslc)?;
                if running.is_empty() {
                    return Ok("没有正在运行的容器".to_owned());
                }
                let names: Vec<String> = running
                    .iter()
                    .map(|c| c.display_name().to_owned())
                    .collect();
                let done = cmd::container::stop(wslc, &names)?;
                Ok(stopped_all_message(&done))
            }
            PendingAction::KillContainer(name) => {
                let done = cmd::container::kill(wslc, std::slice::from_ref(name))?;
                Ok(format!("已终止：{}", done.join("、")))
            }
            PendingAction::RemoveContainer(name) => {
                let done = cmd::container::remove(wslc, std::slice::from_ref(name), true)?;
                Ok(format!("已删除：{}", done.join("、")))
            }
            PendingAction::PruneContainers => {
                let out = cmd::container::prune(wslc, None)?;
                Ok(if out.is_empty() {
                    "没有需要清理的容器".to_owned()
                } else {
                    out
                })
            }
            PendingAction::RemoveImage(reference) => {
                let done = cmd::image::remove(wslc, std::slice::from_ref(reference), false)?;
                Ok(format!("已删除镜像：{}", done.join("、")))
            }
            PendingAction::RemoveNetwork(name) => {
                cmd::network::remove(wslc, std::slice::from_ref(name))?;
                Ok(format!("已删除网络：{name}"))
            }
            PendingAction::RemoveVolume(name) => {
                cmd::volume::remove(wslc, std::slice::from_ref(name), true)?;
                Ok(format!("已删除卷：{name}"))
            }
            PendingAction::PruneVolumes => {
                let out = cmd::volume::prune(wslc)?;
                Ok(if out.is_empty() {
                    "没有需要清理的卷".to_owned()
                } else {
                    out
                })
            }
            // 数据库管理页的两条危险动作：走容器里的 mysql 客户端执行 DDL。
            // `run_sql` 内部已经把错误脱敏（`cmd::mysql::sanitize`），
            // 所以密码不会跟着错误冒到提示条上。
            PendingAction::DropDatabase {
                container,
                database,
                client,
                creds,
            } => {
                let sql = wslc_core::model::mysql::drop_database_sql(database)?;
                cmd::mysql::run_sql(wslc, container, client, creds, &sql, None, SqlMode::Batch)?;
                Ok(format!("已删除数据库：{database}"))
            }
            PendingAction::DropUser {
                container,
                user,
                host,
                client,
                creds,
            } => {
                let sql = wslc_core::model::mysql::drop_user_sql(user, host)?;
                cmd::mysql::run_sql(wslc, container, client, creds, &sql, None, SqlMode::Batch)?;
                Ok(format!("已删除用户：'{user}'@'{host}'"))
            }
            // 恢复：把 Windows 侧那份 dump 送进容器，再从容器内重定向进 mysql。
            //
            // 这里**不**假设容器里还有一份旧的 dump（实测：容器重建 / WSL 重启后
            // `/tmp` 里那份就没了）。送进去之前先看本地文件在不在 —— 用户可能
            // 在文件管理器里把它删了，那时要给一句人话，而不是 cp 的引擎报错。
            PendingAction::RestoreDatabase {
                container,
                database,
                client,
                windows_path,
                creds,
            } => {
                if !windows_path.is_file() {
                    return Err(wslc_core::Error::Unsupported(format!(
                        "备份文件不在了：{}。它可能被手动删掉了 —— 备份列表里\
                         会跟着消失，重新备份一份即可。",
                        windows_path.display()
                    )));
                }
                let container_path =
                    cmd::mysql::push_dump_to_container(wslc, container, windows_path)?;
                // `None` = 由 dump 自己的语句决定往哪灌（备份是 `--databases` 出来的）
                cmd::mysql::load_sql_file(wslc, container, client, creds, &container_path, None)?;
                Ok(format!("已恢复数据库：{database}（现有数据已被备份覆盖）"))
            }
            // 删除备份**不在这里执行**。
            //
            // 理由：它要做三件事（删本地文件 → 进容器 `rm -f` → 重扫目录
            // 并写回 `AppState`），而 `execute` 的形状是"跑一条 `wslc` 命令 →
            // 返回字符串"——最后一件事它表达不了（`&self`，拿不到 `AppState`）。
            // 这个分支只是为了 match 完备：`views::confirm_overlay` 会把这一类
            // 分流给 `Shell::delete_db_backup_confirmed`，走不到这里。
            //
            // 不用 `unreachable!()`：真要漏进来，宁可给一句能读的错误，
            // 也不要在用户的机器上 panic（和 `submit_prompt` 那几个分支同理）。
            PendingAction::DeleteDatabaseBackup { .. } => Err(wslc_core::Error::InvalidArgument(
                "删除备份不走这条同步路径（见 Shell::delete_db_backup_confirmed）".to_owned(),
            )),
        }
    }
}

// 自动刷新间隔不再是界面上的档位开关，而是 `prefs.rs` 里的持久化偏好。
// 界面只负责在"设置"页里改它。

/// 「停止全部」的结果摘要（成功提示条上那一行）。
///
/// 单独一个纯函数是为了**能测**：这句话只出现在提示条上，写错了的表现是
/// "用户不知道刚才到底停掉了哪个/几个"，而这类缺陷在 CI 里除了编译之外
/// 没有任何信号（`AGENTS.md` §8）。
///
/// 名字超过 [`STOP_ALL_NAMES_IN_SUMMARY`] 个就截断：提示条不是日志，
/// 二十个容器名会把整条撑成好几行，把后面真正的错误信息挤出去。
pub(crate) fn stopped_all_message(done: &[String]) -> String {
    if done.is_empty() {
        // `wslc stop` 一行都没打印。正常不该发生（它会把停掉的容器名打出来），
        // 但报"已停止 0 个容器"读起来像成功了却什么都没发生 —— 如实说。
        return "停止命令已执行，但 wslc 没有报告任何容器".to_owned();
    }
    let shown = done
        .iter()
        .take(STOP_ALL_NAMES_IN_SUMMARY)
        .cloned()
        .collect::<Vec<_>>()
        .join("、");
    if done.len() > STOP_ALL_NAMES_IN_SUMMARY {
        format!("已停止 {} 个容器：{shown} 等", done.len())
    } else {
        format!("已停止 {} 个容器：{shown}", done.len())
    }
}

/// 摘要里最多列几个容器名（余下的用"等"带过）。
pub(crate) const STOP_ALL_NAMES_IN_SUMMARY: usize = 5;

/// 需要二次确认的 **WSL 发行版**动作。
///
/// 与 [`PendingAction`]（容器）并列，刻意**不合并成一个枚举**：
/// 两者的执行器不同（`Wsl` vs `Wslc`）、危险级别与文案也不同，
/// 硬塞进一个类型只会让 `execute` 长出两个参数、每个 match 都要写全两套。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DistroAction {
    /// 终止发行版（丢未保存数据）。
    Terminate(String),
    /// 关停全部发行版。
    ShutdownAll,
    /// 注销（删除）发行版及其根文件系统。
    Unregister {
        /// 发行版名。
        name: String,
        /// 将要删掉的虚拟磁盘大小（字节）；读不到时为 `None`。
        ///
        /// 带上它是为了让用户在点"删除"之前**知道要删掉多少东西**。
        vhdx_bytes: Option<u64>,
    },
    /// 压缩发行版的虚拟磁盘。
    Compact(String),
    /// 开关稀疏 VHD（开启后 WSL 会自动回收已释放的空间）。
    ///
    /// 单独一个动作而不是"切换"：界面不显示当前状态
    /// （注册表的 `Flags` 里哪一位是稀疏**没有实测确认**），
    /// 所以给两个明确的动作，让用户自己说清要开还是要关。
    ///
    /// ⚠️ **目前没有界面入口**：v0.4 按需求把发行版详情里那个稀疏开关拿掉了
    /// （理由见 `views::distro_detail_overlay` 里的说明）。日常要开就走
    /// `.wslconfig` 的 `[experimental] sparseVhd=true`（「应用设置 → WSL」），
    /// 装新实例时由安装计划里那一步补上。这个变体和
    /// [`crate::cmd::distro::set_sparse`] 留下来的是**能力**，不是"还有入口"——
    /// 别照着一句过期的注释去界面上找那个按钮。
    SetSparse {
        /// 发行版名。
        name: String,
        /// `true` = 开启稀疏。
        sparse: bool,
    },
}

impl DistroAction {
    /// 这个动作作用在哪个发行版上。
    ///
    /// `ShutdownAll` 返回 `None`：它压的是 `wsl --shutdown`，作用在**所有**
    /// 发行版上，比"动某一个"更粗暴 —— 调用方要单独处理它
    /// （见 [`AppState::long_task`]），别把它当成"碰不到特定的那个"而放行。
    pub fn distro_name(&self) -> Option<&str> {
        match self {
            Self::ShutdownAll => None,
            Self::Terminate(name) | Self::Compact(name) => Some(name),
            Self::Unregister { name, .. } | Self::SetSparse { name, .. } => Some(name),
        }
    }

    /// 确认弹窗标题。
    pub fn title(&self) -> &'static str {
        match self {
            DistroAction::Terminate(_) => "终止发行版",
            DistroAction::ShutdownAll => "关停所有发行版",
            DistroAction::Unregister { .. } => "删除发行版",
            DistroAction::Compact(_) => "压缩虚拟磁盘",
            DistroAction::SetSparse { sparse, .. } => {
                if *sparse {
                    "开启稀疏磁盘"
                } else {
                    "关闭稀疏磁盘"
                }
            }
        }
    }

    /// 确认弹窗正文。
    pub fn body(&self) -> String {
        match self {
            DistroAction::Terminate(name) => format!(
                "将立即终止发行版 {name}。里面**没有保存**的东西会丢失，\
                 正在跑的服务会中断。"
            ),
            DistroAction::ShutdownAll => "将关停**所有**发行版和 WSL2 轻量工具虚拟机。\
                 所有发行版里没保存的东西都会丢失。"
                .to_owned(),
            DistroAction::Unregister { name, vhdx_bytes } => {
                let size = match vhdx_bytes {
                    Some(bytes) => format!("（虚拟磁盘约 {}）", format_bytes(*bytes)),
                    None => String::new(),
                };
                format!(
                    "将注销发行版 {name} 并**删除它的根文件系统**{size}。\
                     此操作不可撤销，里面的数据无法找回。"
                )
            }
            DistroAction::Compact(name) => format!(
                "将压缩 {name} 的虚拟磁盘，回收已释放的空间。\
                 这个动作**不会删除任何数据**，但发行版必须处于已停止状态；\
                 大磁盘可能要几分钟。"
            ),
            DistroAction::SetSparse { name, sparse } => {
                if *sparse {
                    format!(
                        "将把 {name} 的虚拟磁盘标记为**稀疏**：WSL 之后会自动回收\
                         已释放的块，相当于持续做压缩。不会删除任何数据。"
                    )
                } else {
                    format!(
                        "将关闭 {name} 的稀疏标志。磁盘不会再自动回收空间，\
                         之后只能手动压缩。不会删除任何数据。"
                    )
                }
            }
        }
    }

    /// 按钮文案。
    pub fn confirm_label(&self) -> &'static str {
        match self {
            DistroAction::Terminate(_) => "终止",
            DistroAction::ShutdownAll => "全部关停",
            DistroAction::Unregister { .. } => "删除",
            DistroAction::Compact(_) => "压缩",
            DistroAction::SetSparse { sparse, .. } => {
                if *sparse {
                    "开启"
                } else {
                    "关闭"
                }
            }
        }
    }

    /// 执行操作，返回给用户看的结果摘要。
    pub fn execute(&self, wsl: &Wsl) -> Result<String> {
        match self {
            DistroAction::Terminate(name) => {
                cmd::distro::terminate(wsl, name)?;
                Ok(format!("已终止：{name}"))
            }
            DistroAction::ShutdownAll => {
                cmd::distro::shutdown(wsl)?;
                Ok("已关停所有发行版".to_owned())
            }
            DistroAction::Unregister { name, .. } => {
                cmd::distro::unregister(wsl, name)?;
                Ok(format!("已删除发行版：{name}"))
            }
            DistroAction::Compact(name) => {
                cmd::distro::compact(wsl, name)?;
                Ok(format!("{name} 的虚拟磁盘已压缩"))
            }
            DistroAction::SetSparse { name, sparse } => {
                cmd::distro::set_sparse(wsl, name, *sparse)?;
                Ok(format!(
                    "{name} 的稀疏磁盘已{}",
                    if *sparse { "开启" } else { "关闭" }
                ))
            }
        }
    }
}

/// 确认弹窗里可能出现的动作 —— 两个域的**统一入口**。
///
/// `AppState::confirm` 只存这一个类型，弹窗和"确认"按钮就不必各写两份。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmAction {
    /// 容器 / 镜像 / 网络 / 卷。
    Container(PendingAction),
    /// WSL 发行版。
    Distro(DistroAction),
}

impl ConfirmAction {
    /// 确认弹窗标题。
    pub fn title(&self) -> &'static str {
        match self {
            ConfirmAction::Container(action) => action.title(),
            ConfirmAction::Distro(action) => action.title(),
        }
    }

    /// 确认弹窗正文。
    pub fn body(&self) -> String {
        match self {
            ConfirmAction::Container(action) => action.body(),
            ConfirmAction::Distro(action) => action.body(),
        }
    }

    /// 按钮文案。
    pub fn confirm_label(&self) -> &'static str {
        match self {
            ConfirmAction::Container(action) => action.confirm_label(),
            ConfirmAction::Distro(action) => action.confirm_label(),
        }
    }

    /// 执行。两个域各用各的调用器。
    pub fn execute(&self, wslc: &Wslc, wsl: &Wsl) -> Result<String> {
        match self {
            ConfirmAction::Container(action) => action.execute(wslc),
            ConfirmAction::Distro(action) => action.execute(wsl),
        }
    }
}

/// 卷名能不能直接用。
///
/// # 只挡一类：以 `-` 开头的名字
///
/// 卷名会被当成 `wslc volume create` 的**裸 argv**。`wslc volume create --help`
/// 里的 `--help` 会被解析成选项而不是卷名 —— 用户以为建了个卷，
/// 实际什么都没建（或者某个开关被静默生效）。这类"看着成功其实没做"
/// 是最难查的，所以在**本地**先挡掉，不白起一个进程。
///
/// # 别的规则**故意**不自己编
///
/// 字符集、长度、大小写这些 `wslc` 自己的规则没在真机上测过
/// （采集环境里一个卷都没有，见 `model/volume.rs` 的说明）。
/// 自己编一套只会把合法名字挡在外面 —— 那是比"让 wslc 报错"更糟的结果：
/// 用户找不到任何理由。
pub fn validate_volume_name(name: &str) -> std::result::Result<(), &'static str> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("卷名不能为空");
    }
    if trimmed.starts_with('-') {
        return Err("卷名不能以 - 开头（会被 wslc 当成选项，而不是名字）");
    }
    Ok(())
}

/// 提示条的类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    /// 普通信息。
    Info,
    /// 操作成功。
    Success,
    /// 操作失败。
    Error,
}

/// 一次性提示条。
///
/// # 它现在是「一次性槽」，不再是「会自己消失的状态」
///
/// v0.20.2 起展示交给官方 `Notification`（`Root` 管的 toast 层），
/// `AppState` 这边只负责**把消息交给界面**：`notify()` 覆盖式写入，
/// `Shell::render` 取走并 `push_notification`。
///
/// 所以这里**没有** `shown_at` / `lifetime()` 了：什么时候消失由组件决定
/// （它的倒计时在鼠标悬停时还会暂停，见官方文档）。原先自绘那套
/// "错误 8 秒、其余 4 秒"的差异化随之取消 —— 官方只给了 `autohide(bool)`、
/// 没有"每条多少秒"，强行模拟要么改组件、要么自己起定时器（那正是被删掉的东西）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    /// 展示文本。
    pub text: String,
    /// 类型（决定图标 / 颜色）。
    pub kind: ToastKind,
}

impl Toast {
    /// 成功提示。
    pub fn success(text: impl Into<String>) -> Self {
        Self::new(text, ToastKind::Success)
    }
    /// 失败提示。
    pub fn error(text: impl Into<String>) -> Self {
        Self::new(text, ToastKind::Error)
    }
    /// 普通提示。
    pub fn info(text: impl Into<String>) -> Self {
        Self::new(text, ToastKind::Info)
    }

    fn new(text: impl Into<String>, kind: ToastKind) -> Self {
        Self {
            text: text.into(),
            kind,
        }
    }
}

/// **不需要二次确认**的即时操作。
///
/// 与 [`PendingAction`] / [`DistroAction`] 的区别：这些不破坏数据、可逆，
/// 而且是运维里最高频的动作 —— 每次都弹确认反而碍事。
/// 破坏性的停止/强杀/删除仍然走那两个。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImmediateAction {
    /// 启动一个已停止的容器。
    StartContainer(String),
    /// 重启一个运行中的容器。
    RestartContainer(String),
    /// 启动一个发行版，并**让它一直保持运行**。
    ///
    /// 做法是从 Windows 这边吊住一个 `wsl.exe` 不放（WSL 认为有活动会话，
    /// 就不会回收发行版）。细节和实测见 [`wslc_core::cmd::distro::start`]。
    ///
    /// ⚠️ 吊着它的那个进程属于**本面板**。面板关掉后它不会跟着退出，
    /// 发行版会继续跑 —— 所以界面上要写明"它由本面板吊住"。
    /// 那句话在**发行版详情弹窗**里（列表行的状态列装不下它，
    /// 曾经硬塞进去，结果溢到隔壁列上）。
    StartDistro(String),
    /// 打开发行版的终端（`wsl -d <name>`，会开一个新的控制台窗口）。
    ///
    /// 和 [`ImmediateAction::StartDistro`] 的区别：那个由**面板**吊着，
    /// 这个由**用户自己的终端窗口**吊着 —— 终端一关，发行版约 20 秒后就停了。
    OpenDistroTerminal(String),
    /// 把发行版设为默认。
    SetDefaultDistro(String),
    /// 在新终端窗口里打开容器的交互式 shell（`wslc exec -it <容器> /bin/sh`）。
    ///
    /// 交互式程序不能内嵌进 GPUI，也不该被捕获输出 —— 和
    /// [`ImmediateAction::OpenDistroTerminal`] 是同一个理由。
    OpenContainerShell(String),
    /// 在新终端窗口里 attach 到容器主进程（`wslc attach <容器>`）。
    AttachContainer(String),
    /// 在新终端窗口里跟随容器日志（`wslc logs --follow`）。
    ///
    /// 不带行数 / 时间戳参数：那两项从 `AppState::logs` 里现读
    /// （按钮就在日志浮层上，用户刚选的就是他要的）。
    FollowContainerLogs(String),
    /// 启动一条部署（v0.9）—— 面板在容器里起一个受 pidfile 管的进程。
    ///
    /// 参数是**应用名**（不是容器名）：具体跑在哪个容器、哪个目录、哪条命令
    /// 由 `AppState::deploy` + 本地存档决定（`Shell::deploy_action` 去取）。
    /// 刻意不把容器名 / 命令塞进这个变体：那会让"界面上显示的那一条"和
    /// "实际执行的那一条"有两个真相来源。
    StartDeployment(String),
    /// 停止一条部署（只发 `TERM`，不做 `kill -9`）。
    StopDeployment(String),
}
