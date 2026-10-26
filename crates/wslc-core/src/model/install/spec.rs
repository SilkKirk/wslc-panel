//! 用户填的那份规格：`InstallSource` / `InstallSpec`。

use super::*;

/// 新发行版的**来源**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallSource {
    /// 本地 tar 文件 → `wsl --import <name> <dir> <file> --version 2`。
    ///
    /// 最可靠的一条路：**不联网也能用**，只是把文件系统铺开。
    Tar {
        /// tar 文件路径。
        path: String,
    },
    /// 本地 VHDX → `wsl --import <name> <dir> <file> --vhd --version 2`。
    ///
    /// `--vhd` 让 WSL 把给定的虚拟磁盘**拷**到安装位置（不是就地用）。
    /// 想就地注册已存在的 ext4 VHDX，那是另一个命令（`--import-in-place`），
    /// 本程序暂时不做 —— 它会动用户的原始文件，风险等级不同。
    Vhdx {
        /// vhdx 文件路径。
        path: String,
    },
    /// 本地文件，交给 WSL 自己的安装器 → `wsl --install --from-file`。
    ///
    /// 和 [`InstallSource::Tar`] 的区别不只是参数：`--import` 只是把文件系统
    /// 铺开，而 `--install` 走的是商店安装器那套，会做首次启动初始化
    /// （建默认用户等）。同一个 tar，两条路的结果不一样。
    ///
    /// ⚠️ 这条**没有** `--version` 选项（实测 `wsl.exe --help`），
    /// 版本由安装器自己决定 —— 我们想显式指定也指定不了。
    File {
        /// 安装文件路径（`.wsl` / `tar.gz` …）。
        path: String,
    },
    /// 在线安装（微软商店 / GitHub）。
    ///
    /// `id` **必须**是在线清单里的名字（`wsl -l -o` 那一列），
    /// 因为 `wsl --install -d` 只认它。用户想要别的名字时，
    /// WSL 装完的那个 `id` 会被重定位成 [`InstallSpec::name`]（计划里的
    /// `EnsureRelocated` 那一步）。
    Online {
        /// 在线清单里的发行版名（`wsl --install -d <id>`）。
        id: String,
        /// 装完是否立刻启动。
        ///
        /// 默认**不**启动：安装动辄十几分钟，装完自己弹一个终端出来很突兀。
        launch: bool,
        /// 走 `--web-download`（从网络下，而不是微软商店）。
        ///
        /// 参考实现会先探一下 GitHub 通不通再决定；本程序把探测结果当**默认值**，
        /// 但按钮在界面上，用户可以改。
        web_download: bool,
        /// 这条装不上时，改用哪个 id 再试一次。
        ///
        /// 界面上算好传进来（`OnlineDistroState::sibling_id`）：用户选了裸家族名
        /// （`Ubuntu`）时给它**带版本号的兄弟**（`Ubuntu-26.04`）。
        /// 起因见 [`crate::model::install::PlannedStep::retry_other_id`] 的说明 ——
        /// 实测本机 `-d Ubuntu` 必失败、`-d Ubuntu-24.04` 正常。
        fallback_id: Option<String>,
    },
    /// 镜像源：下载官方 rootfs / `.wsl` 包，再装成本地发行版。
    ///
    /// `url` 是**已经选定的**那一个（探测最快镜像的结果，或用户手填的）。
    /// 选哪一个是界面的活（要测速、要显示结果），计划里只记结论 ——
    /// 这样计划是确定的，预览才能和执行完全一致。
    ///
    /// # `format` 决定装法
    ///
    /// 清单里的来源有两种（实测 `wslui` 那份清单：Ubuntu 24.04 的 13 个来源里
    /// 10 个是 `.wsl`）：
    ///
    /// - `tar.*` → `wsl --import`（只是铺开文件系统）；
    /// - `wsl` → **新格式的 `.wsl` 包**，`--import` 吃不了，要走
    ///   `wsl --install --from-file`（会做首次启动初始化）。
    ///
    /// 参考实现一律按 `--import` 处理 —— 它挑到 `.wsl` 来源时必然失败。
    Mirror {
        /// 下载地址。
        url: String,
        /// 镜像站名（只用于显示，比如 `lxc-tuna` / `tsinghua`）。
        mirror: String,
        /// 发行版版本（只用于显示，比如 `24.04`）。
        release: String,
        /// 打包格式：`tar.xz` / `tar.gz` / `wsl`（空串按后缀猜）。
        format: String,
    },
}

impl InstallSource {
    /// 镜像源这条是不是 `.wsl` 包（装法与 tar 不同）。
    pub fn mirror_is_bundle(format: &str, url: &str) -> bool {
        format.eq_ignore_ascii_case("wsl") || url.to_lowercase().ends_with(".wsl")
    }
    /// 这个来源需不需要用户指定**安装目录**。
    ///
    /// 在线安装可以留空（WSL 有自己的默认位置）—— 但一旦要改名，
    /// 就必须有目录（重定位那一步的 `--import` 要它），校验里单独管这件事。
    pub fn requires_install_dir(&self) -> bool {
        matches!(
            self,
            Self::Tar { .. } | Self::Vhdx { .. } | Self::Mirror { .. }
        )
    }

    /// 这个来源需不需要一个**本地文件路径**。
    pub fn needs_path(&self) -> bool {
        matches!(
            self,
            Self::Tar { .. } | Self::Vhdx { .. } | Self::File { .. }
        )
    }

    /// 支不支持"装完立即启动"（只有在线安装有 `--no-launch`）。
    pub fn supports_launch(&self) -> bool {
        matches!(self, Self::Online { .. })
    }
}

/// 「添加实例」的全部参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallSpec {
    /// 用户想要的**最终**发行版名。
    pub name: String,
    /// 安装目录。留空表示"用默认目录 + 名字"（见 [`InstallSpec::effective_install_dir`]）。
    pub install_dir: String,
    /// 来源。
    pub source: InstallSource,
    /// 装完是否设为默认。
    ///
    /// ⚠️ 这**不是**一个命令行选项 —— `--import` 和 `--install` 都不接受
    /// `--set-default`（实测 `wsl.exe --help`）。所以它是计划里**最后一步**
    /// 单独的一条 `wsl --set-default`。
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

    /// 实际会用到的安装目录：用户填了就用它，没填就用"默认目录 + 名字"。
    pub fn effective_install_dir(&self, ctx: &PlanContext) -> Option<String> {
        let dir = self.install_dir.trim();
        if !dir.is_empty() {
            return Some(dir.to_owned());
        }
        derive_install_dir(ctx.default_dir.as_deref(), self.name.trim())
    }
}
