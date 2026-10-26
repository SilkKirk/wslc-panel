//! 失败分类，以及"下一步该做什么"。

/// 一次部署 / 运维动作失败的类型。
///
/// 分类只为了给**一句话的下一步** —— 原文照样留着（用户可能要贴给别人看）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeployErrorKind {
    /// 容器没在运行（一切都是 `wslc exec`，它必然失败）。
    ContainerNotRunning,
    /// 镜像里没有我们要用的工具（`/bin/sh`）。
    MissingTool,
    /// 没有可执行位 —— 就是那个 `666` 的坑。
    PermissionDenied,
    /// 路径不存在 / 建不出来 / 被别的东西占着。
    PathNotFound,
    /// 认不出来的。
    Other,
}

impl DeployErrorKind {
    /// 给用户的一句话下一步（`None` = 直接看原文就行）。
    pub fn hint(self) -> Option<&'static str> {
        match self {
            Self::ContainerNotRunning => {
                Some("容器没在运行 —— 先到「容器」页把它启动起来（这一页每个动作都要进容器）")
            }
            // ⚠️ v0.9 的搬运改成 `wslc container cp` 之后，要用到的只剩 `/bin/sh`
            //（准备目录 / 起停 / 判活 / 看日志全是 `sh -c` 一条脚本）。
            // 这句里早先写着"部署需要 /bin/sh **和 tar**"，而 tar 那条路
            // （往 stdin 灌 tar）已经被否掉了 —— 留着只会把人往一个
            // 已经不存在的问题上引。同族的 `BadArchive` / `NoHostTar`
            // 两个变体也一起删了：`classify_error` 造不出它们了。
            Self::MissingTool => Some(
                "镜像里缺我们要用的命令。整套动作只用容器里的 `/bin/sh`\
                 （准备目录、起停、判活、看日志都是一条 `sh -c` 脚本）——\
                 这个镜像精简到连它都没有的话，换一个完整点的镜像，\
                 或者用「终端」自己进去装",
            ),
            Self::PermissionDenied => Some(
                "文件没有可执行位（从 Windows 传过去的文件默认没有）。\
                 这是面板的 bug，请把它报出来",
            ),
            Self::PathNotFound => Some(
                "容器里的这个路径不存在、建不出来，或者被别的东西占着 —— 换一个目录试试（比如 /opt 底下）",
            ),
            Self::Other => None,
        }
    }

    /// 给用户看的一句话（没有建议时用原文兜底）。
    pub fn label(self) -> &'static str {
        match self {
            Self::ContainerNotRunning => "容器没在运行",
            Self::MissingTool => "镜像里缺命令",
            Self::PermissionDenied => "没有可执行位",
            Self::PathNotFound => "路径不可用",
            Self::Other => "操作失败",
        }
    }
}

/// 从一段报错文本 + 退出码分类。
///
/// 判据都是**实测抓下来的原文**（见模块文档那张表），顺序从"最确定"到"最泛"：
/// 错的关键词排在泛的前面，否则会被泛的吃掉。
pub fn classify_error(text: &str, code: Option<i32>) -> DeployErrorKind {
    let lower = text.to_ascii_lowercase();

    // 容器没运行：这条最要紧，因为它是"每个动作都会碰到"的那一个
    if text.contains("WSLC_E_CONTAINER_NOT_RUNNING")
        || text.contains("未运行")
        || lower.contains("is not running")
    {
        return DeployErrorKind::ContainerNotRunning;
    }

    // 没有可执行位（实测：`sh: 1: /opt/probe/runme: Permission denied` + exit 126）
    if lower.contains("permission denied") {
        return DeployErrorKind::PermissionDenied;
    }

    // 归档解不开那一条**随 tar 方案一起删了**（v0.9.13）：搬运现在只走
    // `wslc container cp`，它不产生 tar 报错，那两个关键词分支是死的。
    // 留着它的代价不是"多几行"—— 是下一个人会以为"有一条 tar 的路能用"。

    // 镜像里缺命令：exit 127，或者 OCI runtime 的 "executable file not found"
    // （后者是 `cmd::mysql` 那边实测到的原文）
    if lower.contains("not found")
        || lower.contains("executable file not found")
        || code == Some(127)
    {
        return DeployErrorKind::MissingTool;
    }

    // 路径不可用。判据全是**本机跑出来的原文**（wslc 3.0.3.0 + 一个 alpine 容器，
    // 2026-10-16 复核）：
    //
    // - `mkdir -p <已存在的文件>` → `mkdir: cannot create directory '/tmp/afile':
    //   File exists`（用户把目标目录填成某个文件名，是**最像手滑**的一种）；
    // - `mkdir -p <穿过一个文件的路径>` → 同上，但尾句是 `Not a directory`；
    // - `wslc container cp <文件> <容器>:/不存在的目录/` → exit 1 +
    //   `Could not find the file /tmp/nope-dir/ in container …` +
    //   `错误代码： ERROR_PATH_NOT_FOUND`
    //   （⚠️ 注意那是 `ERROR_PATH_NOT_FOUND`：下划线形态**匹配不到**
    //    上面那句 `lower.contains("not found")`，所以它落在这一支里是对的）；
    // - 早先实测的 `The system cannot find the path specified.`（Windows 侧）
    //   与 `No such file or directory`（容器里）继续留着。
    //
    // ⚠️ 这三条是 v0.9.13 补的：在补之前它们全都落到 `Other`，
    // 而 `Other` 的 `hint()` 是 `None` —— 用户只拿到一句引擎原文，
    // 明明这一支的"换个目录试试"就是对的。
    if lower.contains("no such file or directory")
        || lower.contains("cannot find the path")
        || lower.contains("directory nonexistent")
        || lower.contains("could not find the file")
        || lower.contains("error_path_not_found")
        || lower.contains("cannot create directory")
        || lower.contains("not a directory")
        || lower.contains("file exists")
    {
        return DeployErrorKind::PathNotFound;
    }

    DeployErrorKind::Other
}
