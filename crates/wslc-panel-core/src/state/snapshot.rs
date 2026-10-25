//! 数据快照：一次采集回来的一切，以及从它派生的总览数字。

use super::prelude::*;
use super::settings_storage_path;

/// 一次刷新得到的全部数据。
///
/// **每个区段独立记录错误**：某个命令失败不会让整页空白，
/// 用户仍然能看到其余可用信息（这是运维工具的基本要求）。
///
/// # ⚠️ 这里只放**界面真的会读**的字段
///
/// v0.9.16 清过一轮：`sessions`（`wslc system session list`）被删掉了 ——
/// 它唯一的消费者是「基本信息」页那张「活动会话」卡，而**同一份数据
/// `wslc info` 的 `Server.Sessions` 里已经有了**（见 [`AppState::session_label`]），
/// 也就是说那是个每 3 秒白起的 `wslc.exe` 子进程。
///
/// `storage` **留下来了**：它当时也差点被删（唯一读者是那张「存储」卡），
/// 但同一轮里基本信息页改成了一排圆环，其中「存储」那一环要的正是
/// "会话盘已用多少" —— 现在它是 [`Snapshot::storage`] 的读者。
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    /// `wslc info`
    pub info: Option<SystemInfo>,
    /// 运行中容器（列表 + 实时统计）
    pub running: Vec<ContainerSummary>,
    /// 全部容器
    pub all: Vec<ContainerSummary>,
    /// 镜像
    pub images: Vec<ImageListItem>,
    /// 网络
    pub networks: Vec<NetworkListItem>,
    /// 卷
    pub volumes: Vec<VolumeListItem>,
    /// WSL 实例（发行版）列表 —— 来自 `wsl.exe`，不是 `wslc.exe`。
    ///
    /// 已合并注册表信息（安装位置）与磁盘虚拟大小。
    pub distros: Vec<Distro>,
    /// 会话存储的位置与占用（自行计算，`wslc` 不提供）。
    ///
    /// 解析不出来时为 `None`（例如 `LOCALAPPDATA` 没定义）。
    pub storage: Option<StorageInfo>,
    /// WSL 虚拟机拿到几个 CPU 核。
    ///
    /// **在采集里读、不在渲染里读**：来自 `.wslconfig` 的 `[wsl2] processors`，
    /// 没配就退回 Windows 的 `NUMBER_OF_PROCESSORS` —— 读环境变量是 IO，
    /// 每帧都读不合适（和 `wslconfig` 那条同一个理由）。
    ///
    /// 它只用来把 `wslc stats` 的 CPU%（**100% ≈ 1 个核**）折算成整机占用率，
    /// 见 [`Snapshot::dashboard_stats`]。
    pub wsl_cpu_cores: Option<u32>,
    /// `%USERPROFILE%\.wslconfig` 的读取与静态检查结果。
    ///
    /// **在采集里读、不在渲染里读**：渲染每帧都可能发生，读文件不该在那儿做。
    /// 这个文件很小、极少变，跟着 3 秒的刷新一起读完全够。
    pub wslconfig: WslConfigInfo,
    /// 各区段的错误信息
    pub errors: Vec<String>,
    /// 本次刷新耗时（毫秒）
    pub elapsed_ms: u128,
}

/// 当前 Unix 时间戳（秒）。拿不到系统时间时给 0。
///
/// 只用来给临时文件名加一段"不会重复"的标记，精度不重要。
pub(crate) fn unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `.wslconfig` 的读取结果。
///
/// **只读** —— 这一版不做编辑（写文件）。为什么不提供"一键校验"见
/// [`wslc_core::wslconfig`] 的模块说明：实测 WSL 只在 **VM 启动时**
/// 报配置告警，主动触发就得先 `wsl --shutdown`，那会打断所有正在跑的发行版。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WslConfigInfo {
    /// 文件路径；`USERPROFILE` 取不到时为 `None`。
    pub path: Option<std::path::PathBuf>,
    /// 文件内容；`None` = **文件不存在**（没配过很正常，不是错误）。
    pub text: Option<String>,
    /// 读失败的原因（权限、编码……）。
    pub error: Option<String>,
    /// 静态检查发现的、放错文件的键（见 [`wslc_core::wslconfig::check`]）。
    pub misplaced: Vec<MisplacedKey>,
}

impl Snapshot {
    /// 是否至少有一个区段拿到了数据。
    pub fn has_data(&self) -> bool {
        self.info.is_some()
            || !self.all.is_empty()
            || !self.images.is_empty()
            || !self.networks.is_empty()
            || !self.distros.is_empty()
    }

    /// 基本信息页那一排圆环要的数字。
    ///
    /// **纯算术**（只读快照里的字段），所以渲染每帧算都行 —— 但它是纯逻辑，
    /// 断言必须留在能跑的地方：这个 crate 的测试 CI 会跑到，`views.rs` 的不会。
    ///
    /// # 口径（界面上每一环下面那行小字就是照这里写的）
    ///
    /// - **CPU**：各运行中容器的 `CPUPerc` **之和**。`wslc stats` 用的是
    ///   Docker 那条公式（WSL 源码里核过，见
    ///   [`wslc_core::wslconfig::processors`] 的说明），**100% ≈ 1 个核** ——
    ///   所以想变成"整机占用率"要除以核数，除以几由 `wsl_cpu_cores` 决定。
    ///   ⚠️ 只算容器：发行版自身、面板自己、Windows 上的其它进程都不在里面，
    ///   它**不是** Task Manager 里那个 CPU 占用。
    /// - **内存**：各容器的 `MemUsage` 分子之和 / 分母（分母是 WSL 的内存上限，
    ///   各容器报的是同一个数，所以取任意一个都行）。
    /// - **存储**：会话 VHD 在宿主机上实际占的字节 ÷ **所在盘总量** ——
    ///   镜像、容器可写层、卷都在这个 VHD 里（`wslc_core::storage` 的说明），
    ///   所以这一环回答的是"容器这一摊占了这块盘多少"。
    ///   ⚠️ 分母刻意**不是**"整块盘已用了多少"：那样读数是**这台电脑**的磁盘
    ///   占用率，和面板管的这一摊没关系（实机反馈就是这个）。
    pub fn dashboard_stats(&self) -> DashboardStats {
        let mut cpu_percent = 0.0f64;
        let mut mem_used = 0u64;
        let mut mem_limit: Option<u64> = None;

        for container in &self.running {
            // 没有 stats 的（`wslc stats` 那一轮漏了它）按"不计入"处理：
            // 把缺失当成 0 会让合计看起来比真实占用小，那不是"少算一点"，
            // 是**编了一个数**。
            let Some(stats) = container.stats.as_ref() else {
                continue;
            };

            if let Some(percent) = stats.cpu_percent().filter(|value| value.is_finite()) {
                cpu_percent += percent;
            }

            if let Some((used, limit)) = stats.mem_usage_parts() {
                if let Some(bytes) = parse_size(used) {
                    mem_used = mem_used.saturating_add(bytes.max(0.0) as u64);
                }
                // 上限取"任一个报出来的"：同一个虚拟机里的容器报的是同一个数。
                // 上限是 `0B`（这是 `wslc` 在容器刚起来时的样子）就当没有。
                if mem_limit.is_none() {
                    mem_limit = parse_size(limit)
                        .filter(|bytes| *bytes > 0.0)
                        .map(|b| b as u64);
                }
            }
        }

        DashboardStats {
            running: self.running.len(),
            total: self.all.len(),
            cpu_percent,
            cpu_cores: self.wsl_cpu_cores,
            mem_used,
            mem_limit,
            disk: self.storage.as_ref().and_then(|storage| storage.volume),
            vhd_bytes: self.storage.as_ref().and_then(|storage| storage.vhd_bytes),
        }
    }

    /// 镜像页顶部那句「N 条记录（M 个镜像）· 合计 X」。
    ///
    /// # 为什么必须按 ID 去重
    ///
    /// `wslc images` 对**同一个镜像**会给出多行（不同 `Repository` 引用指向
    /// 同一个 `ID`）—— 真机夹具里 `e2ac70e7319a` 就同时是
    /// `docker.1panel.live/library/hello-world` 和 `hello-world`。
    /// 直接 `sum(Size)` 会把它的体积算两遍；直接 `len()` 会把记录数当镜像数。
    ///
    /// ID 为空时退回 `reference()` 当键（理论上不该发生，但别让空串把
    /// 所有无 ID 的行并成一条）。
    pub fn image_totals(&self) -> (usize, u64) {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut bytes = 0u64;

        for image in &self.images {
            let key = if image.id.trim().is_empty() {
                image.reference()
            } else {
                image.id.clone()
            };
            if !seen.insert(key) {
                continue;
            }
            bytes = bytes.saturating_add(image.size_bytes().unwrap_or(0.0).max(0.0) as u64);
        }

        (seen.len(), bytes)
    }
}

/// 「基本信息」页那四个圆环的数字。
///
/// 由 [`Snapshot::dashboard_stats`] 算出来 —— 单独一个类型而不是一堆散字段，
/// 是为了让"这一排圆环读了哪些东西"一眼能看全：它们全部来自快照，**不发起
/// 任何采集**。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DashboardStats {
    /// 运行中的容器数。
    pub running: usize,
    /// 全部容器数（含已停止）。
    pub total: usize,
    /// 各运行中容器的 CPU% 之和（**100% ≈ 1 个核**）。
    pub cpu_percent: f64,
    /// 折算整机占用率要除的核数；拿不到时 `None`（那就只报原始合计）。
    pub cpu_cores: Option<u32>,
    /// 各运行中容器的内存占用之和（字节）。
    pub mem_used: u64,
    /// WSL 的内存上限（字节）；一个容器都没在跑时报不出来，为 `None`。
    pub mem_limit: Option<u64>,
    /// 会话存储所在卷的容量；拿不到时为 `None`。
    ///
    /// 它在这个页面上只当**分母**用（"会话 VHD 占了这块盘的多少"）——
    /// 见 [`DashboardStats::vhd_percent_of_disk`]。
    pub disk: Option<VolumeSpace>,
    /// 当前会话 VHD 的实际占用；文件还没建出来时为 `None`。
    ///
    /// 它是「存储」那一环的**分子**（镜像、容器可写层、卷都在这个 VHD 里）。
    pub vhd_bytes: Option<u64>,
}

impl DashboardStats {
    /// 容器这一摊折算成"整机占用率"（0.0–100.0）。
    ///
    /// 拿不到核数时返回 `None` —— 那时候**不能**拿原始合计当百分比用：
    /// 多核机器上 400% 的合计是四个核跑满，画成 400% 的环只会误导。
    pub fn cpu_whole_machine_percent(&self) -> Option<f64> {
        let cores = self.cpu_cores.filter(|cores| *cores > 0)?;
        let percent = self.cpu_percent / f64::from(cores);
        percent.is_finite().then(|| percent.clamp(0.0, 100.0))
    }

    /// 容器占用折算成"用了几个核"（环下面那行小字：`0.22 / 32 核`）。
    pub fn cpu_cores_used(&self) -> f64 {
        self.cpu_percent / 100.0
    }

    /// 容器内存占 WSL 上限的百分比（0.0–100.0）。
    ///
    /// 上限读不到（一个容器都没在跑）时返回 `None`。
    pub fn mem_percent(&self) -> Option<f64> {
        let limit = self.mem_limit.filter(|limit| *limit > 0)?;
        let percent = self.mem_used as f64 / limit as f64 * 100.0;
        percent.is_finite().then(|| percent.clamp(0.0, 100.0))
    }

    /// 会话 VHD 占**所在盘总量**的百分比（0.0–100.0）。
    ///
    /// # 分子是 VHD，不是"整块盘已用了多少"
    ///
    /// 这一环要回答的是"**容器这一摊**在这块盘里占了多少" —— 镜像、容器可写层、
    /// 卷都在那个 `storage.vhdx` 里（`wslc_core::storage` 的说明），所以分子用
    /// 它在宿主机上实际占的字节（`vhd_bytes`）。
    ///
    /// 换成"整块盘已用 / 总量"的话，环读数是**这台电脑**的磁盘占用率
    /// （本机 50%），和面板管的这一摊没关系 —— 用户就是这么反馈的。
    ///
    /// 分母拿不到（读不到卷容量）或 VHD 还没建出来时返回 `None`。
    pub fn vhd_percent_of_disk(&self) -> Option<f64> {
        let total = self.disk.filter(|disk| disk.total > 0)?.total;
        let vhd = self.vhd_bytes?;
        let percent = vhd as f64 / total as f64 * 100.0;
        percent.is_finite().then(|| percent.clamp(0.0, 100.0))
    }
}

/// 阻塞式采集一次完整快照。
///
/// 调用方必须在**后台线程/执行器**上调用（见 `app::Shell::refresh`）：
/// 内部会串行跑 6 条 `wslc` 命令 + 1 条 `wsl` 命令 + 1 条 `reg` 命令。
///
/// # 为什么 `--status` 不在这里
///
/// 它每轮都跑没必要（默认发行版/默认版本极少变），而且它的输出是**本地化**的。
/// 单独放到 [`load_distro_status`]，由调用方按 30 秒的节奏调。
pub fn load_snapshot(wslc: &Wslc, wsl: &Wsl) -> Snapshot {
    let started = std::time::Instant::now();
    let mut snap = Snapshot::default();

    match cmd::system::info(wslc) {
        Ok(info) => snap.info = Some(info),
        Err(e) => snap.errors.push(format!("wslc info：{e}")),
    }

    // ⚠️ 这里**曾经**还会跑一条 `wslc system session list` 并把结果放进
    // `Snapshot::sessions`。v0.9.16 去掉了：活动会话只被「基本信息」页那张
    // 被删掉的卡消费，而同一份数据 `wslc info` 的 `Server.Sessions` 里已经有了
    // —— 也就是说那是一个**每 3 秒白起的子进程**。

    // ⚠️ 一次拿两份（运行中 / 全部）：`stats` 每次约 2 秒，分别调
    // `running_summaries` + `all_summaries` 会让每一轮刷新白等两次。
    // 详见 `cmd::container::summaries` 的说明。
    match cmd::container::summaries(wslc) {
        Ok((running, all)) => {
            snap.running = running;
            snap.all = all;
        }
        Err(e) => snap.errors.push(format!("容器列表：{e}")),
    }

    match cmd::image::list(wslc) {
        Ok(v) => snap.images = v,
        Err(e) => snap.errors.push(format!("镜像列表：{e}")),
    }

    match cmd::network::list(wslc) {
        Ok(v) => snap.networks = v,
        Err(e) => snap.errors.push(format!("网络列表：{e}")),
    }

    match cmd::volume::list(wslc) {
        Ok(v) => snap.volumes = v,
        Err(e) => snap.errors.push(format!("卷列表：{e}")),
    }

    // WSL 实例：列表（3 秒级）+ 注册表与磁盘占用。
    //
    // 注册表走 `reg.exe`，实测只要 7~27 ms，所以**不必**降到 30 秒 ——
    // 真正贵的是多起一个 `wsl.exe` 进程，也就是 `--status`（见上）。
    match cmd::distro::snapshot(wsl) {
        Ok(list) => {
            snap.distros = list.distros;
            // 没认出来的行是"如实告知"，不是致命错误，但也不该被吞掉
            snap.errors.extend(list.warnings);
        }
        Err(e) => snap.errors.push(format!("WSL 实例列表：{e}")),
    }

    // 会话存储位置与占用：`storagePath` 来自 `settings.yaml`，
    // 会话名来自 `wslc info`。这一步纯文件系统 + 一次卷容量查询，
    // 不会失败到需要报错 —— 拿不到就是 `None`（界面上那一环显示 `—`）。
    //
    // ⚠️ 会话名从 `snap.info` 里取，**不用**那条 `wslc system session list`：
    // 两份数据是一样的（见 `Snapshot` 上面那段），而后者要多起一个进程。
    let configured = settings_storage_path(snap.info.as_ref());
    let session = snap
        .info
        .as_ref()
        .and_then(|info| info.server.sessions.first())
        .map(|session| session.name.clone());
    snap.storage = wslc_core::storage::inspect(configured.as_deref(), session.as_deref());

    // `.wslconfig`：纯文件读取（几毫秒），没有进程开销，所以每轮都读。
    snap.wslconfig = load_wslconfig();

    // CPU 核数：`.wslconfig` 的 `[wsl2] processors`，没配就退回机器核数。
    // 读它只为一件事 —— 把 `wslc stats` 的 CPU%（100% ≈ 1 个核）折算成
    // 整机占用率时不至于差好几倍，见 `Snapshot::dashboard_stats`。
    snap.wsl_cpu_cores = wsl_cpu_cores(snap.wslconfig.text.as_deref());

    snap.elapsed_ms = started.elapsed().as_millis();
    snap
}

/// WSL 虚拟机拿到几个 CPU 核。
///
/// 优先 `.wslconfig` 的 `[wsl2] processors`（用户限过核数就得按它算），
/// 没有就退回 Windows 的 `NUMBER_OF_PROCESSORS` —— 那正是 WSL 不限核时的行为。
///
/// 两个都拿不到时返回 `None`：界面上那时候**不报百分比**，只报原始合计
/// （见 [`DashboardStats::cpu_whole_machine_percent`]）。
pub(crate) fn wsl_cpu_cores(config: Option<&str>) -> Option<u32> {
    config
        .and_then(wslc_core::wslconfig::processors)
        .or_else(|| {
            std::env::var("NUMBER_OF_PROCESSORS")
                .ok()?
                .trim()
                .parse::<u32>()
                .ok()
                .filter(|count| *count > 0)
        })
}

/// 读 `%USERPROFILE%\.wslconfig` 并做静态检查。
///
/// 三种结果分得很清楚，因为界面上要说的话完全不同：
///
/// - 读到了 → 做检查，可能报出几条"放错文件的键"
/// - **文件不存在** → 正常状态（没配过），不是错误
/// - 读失败 → 真的出了问题（权限、编码），要如实说
pub(crate) fn load_wslconfig() -> WslConfigInfo {
    let path = wslc_core::config_path();
    match wslc_core::wslconfig::read() {
        Ok(Some(text)) => WslConfigInfo {
            misplaced: wslc_core::wslconfig::check(&text),
            path,
            text: Some(text),
            error: None,
        },
        Ok(None) => WslConfigInfo {
            path,
            text: None,
            error: None,
            misplaced: Vec::new(),
        },
        Err(e) => WslConfigInfo {
            path,
            text: None,
            error: Some(e.to_string()),
            misplaced: Vec::new(),
        },
    }
}

/// `wsl --status`（默认发行版 + 默认版本）。
///
/// **单独一个函数**，因为它的采集频率和列表不一样：
/// 调用方每 30 秒才调一次（见 `app::Shell` 的 `STATUS_EVERY_N_TICKS`）。
///
/// 失败时返回 `Err`，由调用方决定是覆盖还是保留上一次的值 ——
/// 通常应该**保留旧值**（一次瞬时失败不该让界面变成空白）。
pub fn load_distro_status(wsl: &Wsl) -> Result<WslStatus> {
    cmd::distro::status(wsl)
}
