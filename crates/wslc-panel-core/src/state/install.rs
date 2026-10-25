//! 安装域：来源、在线清单、镜像源探测与安装进度。

use super::prelude::*;

/// 「添加实例」页里选中的**来源类型**。
///
/// 只记"用户选了哪一种"；带值的路径 / 名字在输入框里，
/// 拼成 [`wslc_core::model::install::InstallSpec`] 与执行计划是 `app.rs` 的事。
///
/// # 为什么是这三种（原来是五种）
///
/// 原先本地那三条是**分开的三个按钮**（tar / VHDX / `.wsl`），参考实现也这么分
/// （它的 `add.sources.rootfs` 与 `add.sources.vhdx` 是两项，而且**没有** `.wsl`）。
/// 但用户手上拿到的是**一个文件**，"它属于哪一类"不该由他来想 —— 所以合成
/// 一个「本地文件」，由后缀自动分岔（[`wslc_core::model::install::classify_local_file`]）。
///
/// ⚠️ **数据层仍然分三种**（`InstallSource` 的 `Tar` / `Vhdx` / `File`）：
/// 它们在 WSL 那边是三条不同的路（`--import` / `--import --vhd` /
/// `--install --from-file`），合并的只是**界面入口**。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InstallSourceKind {
    /// 本地文件：tar 归档 / `.vhdx` / `.wsl`，按后缀自动分岔。
    #[default]
    Local,
    /// 在线安装（`wsl --install -d`，微软商店或 `--web-download`）。
    Online,
    /// 从镜像站下载 rootfs 再导入（走 `curl.exe`）。
    Mirror,
}

impl InstallSourceKind {
    /// 全部可选值（决定下拉框里的顺序与索引）。
    ///
    /// 顺序：**本地在前，联网的两条在后**；联网那两条里「微软商店」排在
    /// 「镜像源」前面 —— 商店是 WSL 官方那条路，装出来就是官方发行版，
    /// 镜像源是商店/GitHub 都拉不动时的备选。
    pub const ALL: [InstallSourceKind; 3] = [
        InstallSourceKind::Local,
        InstallSourceKind::Online,
        InstallSourceKind::Mirror,
    ];

    /// 下拉框里的文案。
    ///
    /// ⚠️ **措辞是我们自己写的**：参考实现是 GPL-3.0-only、本仓库是 Apache-2.0
    /// （`AGENTS.md` §6），只对齐"有哪几类"，不抄它的字。
    pub fn label(self) -> &'static str {
        match self {
            Self::Local => "本地文件（tar / tar.gz / tar.xz / .vhdx / .wsl）",
            Self::Online => "微软商店 (Microsoft Store)",
            Self::Mirror => "在线发行版（镜像源）",
        }
    }

    /// 一句话说明（显示在来源按钮下面）。
    pub fn hint(self) -> &'static str {
        match self {
            Self::Local => {
                "手上是什么文件都行，按**后缀**自动分岔：tar 归档 → `wsl --import`（只是铺开\
                 文件系统）；`.vhdx` → `--import --vhd`（会拷贝一份，原始文件不动）；\
                 `.wsl` 包 → 交给 WSL 安装器（会做首次启动初始化、建默认用户）。\
                 不联网，最可靠。"
            }
            Self::Online => {
                "走 WSL 官方的在线安装（`wsl --install -d`）：默认从微软商店装，\
                 商店拉不动时可以切到 GitHub（`--web-download`）。"
            }
            Self::Mirror => {
                "从国内镜像站下载官方 rootfs 再导入 —— 商店和 GitHub 都拉不动时用这条，\
                 只用 `curl.exe`，不碰微软的服务器。**自动测速择优**，不用自己挑镜像站。"
            }
        }
    }

    /// 需不需要让用户填一个**文件路径**。
    pub fn needs_path(self) -> bool {
        matches!(self, Self::Local)
    }

    /// 路径输入框的标签。
    pub fn path_label(self) -> &'static str {
        match self {
            Self::Local => "本地文件路径",
            Self::Mirror | Self::Online => "",
        }
    }

    /// 路径输入框的占位提示。
    pub fn path_placeholder(self) -> &'static str {
        match self {
            Self::Local => r"D:\img\ubuntu-rootfs.tar（也可以是 ext4.vhdx 或 Ubuntu-24.04.wsl）",
            Self::Mirror | Self::Online => "",
        }
    }

    /// 文件选择器的类别名与后缀表。
    ///
    /// 返回 `None` 表示这条来源不需要选文件。
    ///
    /// 本地来源只有一条，所以这里的后缀表是**并集**：用户不用先想"我这是哪一类"。
    /// 复合后缀（`.tar.gz`）在 WinForms 的过滤器里写成两个通配就够
    /// （`*.tar.gz` 也能匹配，写上更直观）。
    pub fn file_filter(self) -> Option<(&'static str, &'static [&'static str])> {
        const LOCAL_FILES: &[&str] = &[
            "tar", "gz", "xz", "zst", "bz2", "lz4", "tgz", "txz", "vhdx", "vhd", "wsl",
        ];

        match self {
            Self::Local => Some(("安装文件", LOCAL_FILES)),
            Self::Mirror | Self::Online => None,
        }
    }

    /// 发行版名输入框的**标签**。
    ///
    /// 联网那两条路里它同时是"清单拉不到时手输的 id"，所以标签要长一些 ——
    /// 原先这个函数住在 `views.rs`，而它旁边的 `label()` / `hint()` /
    /// `path_label()` / `name_placeholder()` 全都在这里，漏了它一个。
    pub fn name_field_label(self) -> &'static str {
        if self.is_online() {
            "发行版名（留空 = 用下面选中的那个 id；清单拉不到时也可以直接填，如 Ubuntu-24.04）"
        } else {
            "发行版名"
        }
    }

    /// 发行版名输入框的占位提示。
    pub fn name_placeholder(self) -> &'static str {
        match self {
            Self::Online => "Ubuntu-24.04（选在线清单，或手输它的名字）",
            Self::Mirror => "Ubuntu-24.04",
            _ => "MyDistro",
        }
    }

    /// 安装目录是不是必填。
    ///
    /// 只有在线安装可以留空（WSL 有自己的默认位置）—— 而且"留空"还有个前提：
    /// 不改名。要改名的话重定位那一步必须知道装到哪儿，校验里单独管这件事。
    pub fn requires_install_dir(self) -> bool {
        !matches!(self, Self::Online)
    }

    /// 支不支持"装完启动"（只有在线安装有 `--no-launch`）。
    pub fn supports_launch(self) -> bool {
        matches!(self, Self::Online)
    }

    /// 要不要显示「镜像站」那一块（选发行版 + 自动测速状态 + 自定义 URL）。
    pub fn is_mirror(self) -> bool {
        matches!(self, Self::Mirror)
    }

    /// 要不要显示「在线清单」那一块。
    pub fn is_online(self) -> bool {
        matches!(self, Self::Online)
    }

    /// 要不要显示「本地文件」那一块（路径 + 浏览 + "识别为 …"）。
    pub fn is_local(self) -> bool {
        matches!(self, Self::Local)
    }
}

/// 在线可安装发行版的清单状态。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OnlineDistroState {
    /// 正在拉取。
    pub loading: bool,
    /// 拉取失败的原因（成功时为空串）。
    ///
    /// 刻意用 `String` 而不是 `Option`：界面要把它**原样**显示出来
    /// （"与服务器的连接被重置"这种话本身就是解释），空串即无错误。
    pub error: String,
    /// 清单。
    pub items: Vec<OnlineDistro>,
    /// 这份清单是哪儿来的。
    pub source: OnlineListSource,
    /// 当前选中的发行版 id。
    pub selected: Option<String>,
}

impl OnlineDistroState {
    /// 新建（空清单）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 按搜索词过滤后的清单（空词 = 全部）。
    pub fn filtered(&self, query: &str) -> Vec<&OnlineDistro> {
        self.items
            .iter()
            .filter(|item| online_matches(item, query))
            .collect()
    }

    /// 选中一个（同时记下 id）。
    pub fn select(&mut self, id: impl Into<String>) {
        self.selected = Some(id.into());
    }

    /// 丢掉当前清单（拉取失败时用）。
    pub fn clear(&mut self) {
        self.items.clear();
        self.selected = None;
    }

    /// 给一个"家族名"（比如 `Ubuntu`）找一个**带版本号的兄弟 id**（比如 `Ubuntu-26.04`）。
    ///
    /// # 为什么需要它（实测 2026-10-11，本机）
    ///
    /// `wsl -l -o` 把裸名字（`Ubuntu`）排在列表**第一位**，而它是清单里那条**旧的
    /// 商店包**（`PackageFamilyName = CanonicalGroupLimited.Ubuntu_79rhkp1fndgsc`，
    /// 而且清单里它的 `Amd64Url`/`Arm64Url` 都是空的）：
    ///
    /// | 命令 | 结果 |
    /// |---|---|
    /// | `wsl --install -d Ubuntu --no-launch` | **2~5 秒失败**：`Wsl/InstallDistro/WININET_E_CANNOT_CONNECT` |
    /// | `wsl --install -d Ubuntu-24.04 --no-launch` | 正常下载 |
    /// | `wsl --install -d Debian --no-launch` | 正常下载 |
    ///
    /// 同一分钟、同一台机器、同一个 `wsl.exe` —— 只有 id 不同。参考实现的列表第一项是
    /// 「Ubuntu 26.04 LTS」（传的是 `Ubuntu-26.04`），所以它一直没事；我们照搬
    /// `wsl -l -o` 的顺序，用户一上手就正好点在这个坏条目上。
    ///
    /// 规则：取 `<id>-…` 里版本号最大的那个（`Ubuntu-26.04` > `Ubuntu-24.04`）。
    /// 没有兄弟（`Debian`、`kali-linux`）就返回 `None`，绝不硬猜。
    ///
    /// # `taken`：**已经装过**的发行版名，必须排除
    ///
    /// 本机就是现成的例子：清单第一项是裸 `Ubuntu`，它的兄弟是 `Ubuntu-26.04`，
    /// 而那个**已经装在 `D:\linux\Ubuntu-26.04` 了**。换过去只会立刻撞上 WSL 的
    /// `Wsl/InstallDistro/ERROR_ALREADY_EXISTS`（实测 2026-10-12：对已安装的 id
    /// 跑 `wsl --install -d <id> --no-launch`，2 秒返回 -1，"当文件已存在时，
    /// 无法创建该文件"）。那次失败对用户毫无帮助 —— 不如**一开始就不换**
    /// （返回 `None`），让"这条来源依赖 wsl.exe 自己的网络，改用国内镜像源"
    /// 那句提示照常出现。
    pub fn sibling_id(&self, id: &str, taken: &[String]) -> Option<String> {
        let prefix = format!("{id}-");
        self.items
            .iter()
            .filter(|item| item.id.starts_with(&prefix) && item.id.len() > prefix.len())
            .filter(|item| {
                !taken
                    .iter()
                    .any(|existing| wslc_core::model::distro::same_name(existing, &item.id))
            })
            .max_by(|left, right| version_key(&left.id).cmp(&version_key(&right.id)))
            .map(|item| item.id.clone())
    }
}

/// 把一个 id 里的数字段抽出来做排序键（`Ubuntu-26.04` → `[26, 4]`）。
///
/// 只用于"哪个版本更新"这种比较：数字段按数值比、段数多的更大，
/// 没有数字的 id（`Ubuntu`、`kali-linux`）得到空键 —— 排在最后，不会被选成兄弟。
pub(crate) fn version_key(id: &str) -> Vec<u32> {
    id.split(|c: char| !c.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse::<u32>().ok())
        .collect()
}

/// 一个镜像候选的探测结果。
#[derive(Debug, Clone, PartialEq)]
pub struct MirrorProbeResult {
    /// 镜像站名。
    pub site: String,
    /// 完整 URL。
    pub url: String,
    /// HTTP 状态码（连接失败时 0）。
    pub code: u16,
    /// 总耗时（秒）—— 排序与显示都用它。
    pub secs: f64,
    /// 文件大小（只有 200 才有值）。
    pub bytes: Option<u64>,
}

impl MirrorProbeResult {
    /// 这一个可用吗。
    pub fn is_ok(&self) -> bool {
        self.code == 200
    }

    /// 一句话结果（界面直接显示）。
    pub fn summary(&self) -> String {
        if self.is_ok() {
            let size = self
                .bytes
                .map(mirrors::human_bytes)
                .unwrap_or_else(|| "大小未知".to_owned());
            format!("{size} · {:.2} 秒", self.secs)
        } else if self.code == 0 {
            format!("连不上（{:.1} 秒后放弃）", self.secs)
        } else {
            format!("HTTP {}（这个镜像上没有这个文件？）", self.code)
        }
    }
}

/// 已经选定的那个镜像。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorChoice {
    /// 镜像站名。
    pub site: String,
    /// 完整 URL。
    pub url: String,
    /// 版本代号（计划里显示用）。
    pub release: String,
    /// 打包格式（`tar.xz` / `wsl`……）—— 决定装法，见 `mirrors` 的模块说明。
    pub format: String,
    /// 预期大小（下载进度条的分母）；未知时 `None`。
    pub bytes: Option<u64>,
}

impl MirrorChoice {
    /// 是不是 `.wsl` 包（走 `--install --from-file` 而不是 `--import`）。
    pub fn is_bundle(&self) -> bool {
        wslc_core::model::install::InstallSource::mirror_is_bundle(&self.format, &self.url)
    }
}

/// 「在线发行版（镜像源）」那一块的状态。
///
/// 清单是**动态拉的**（`wslc_core::cmd::catalog`，两个 HTTP 请求走 `curl.exe`），
/// 所以这里存的是"拉回来的清单 + 探测结果"，而不是写死的表。
///
/// `Default` 直接派生：手写的那份每个字段都是"该类型的默认值"
/// （`Vec::new()` / `String::new()` / `None` / `false`），逐字段写一遍
/// 只是给了日后**加字段时漏写一个**的机会（clippy 的 `derivable_impls`）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MirrorState {
    /// 正在拉清单。
    pub loading: bool,
    /// 清单拉取失败的原因（成功时 `None`）。
    ///
    /// 和 `online.error` 一样是"要原样显示给用户看"的那种话
    /// （接口不通 / 解不开 / 架构不认识）。
    pub load_error: Option<String>,
    /// 清单（每个"发行版 + 版本"一条）。
    pub offers: Vec<Offer>,
    /// 清单来源说明（`"清单来自 wslui 接口（api1 → api2）"`）。
    pub origin: String,
    /// 清单的更新时间（接口给的）。
    pub updated: Option<String>,
    /// 选中的那一条（`"<name> <version>"`；空串 = 还没选，按第一条算）。
    pub distro_id: String,
    /// 正在探测。
    pub probing: bool,
    /// 每个候选的探测结果（探测完成后按耗时排序）。
    pub results: Vec<MirrorProbeResult>,
    /// 选定的那一个。
    pub chosen: Option<MirrorChoice>,
    /// 探测失败原因（比如"所有镜像上都拿不到"）。
    pub error: Option<String>,
}

impl MirrorState {
    /// 新建。
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前选中的那一条（没显式选过就是第一条）。
    pub fn selected_offer(&self) -> Option<&Offer> {
        if !self.distro_id.trim().is_empty() {
            if let Some(found) = self
                .offers
                .iter()
                .find(|offer| offer.id() == self.distro_id)
            {
                return Some(found);
            }
        }
        self.offers.first()
    }

    /// 换一条清单：先按参考实现的顺序排，再清掉上一次的探测结果，
    /// 并在原来选的那条没了时回落到第一条。
    pub fn set_offers(&mut self, mut offers: Vec<Offer>, origin: String, updated: Option<String>) {
        sort_offers_like_reference(&mut offers);

        // 原来选的那条还在新清单里吗（清单会更新，版本可能就没了）
        let keep = offers.iter().any(|offer| offer.id() == self.distro_id);
        self.offers = offers;
        self.origin = origin;
        self.updated = updated;
        self.load_error = None;
        if !keep {
            self.distro_id = self.offers.first().map(Offer::id).unwrap_or_default();
            self.reset_probe();
        }
    }

    /// 换一个发行版：清掉上一次的探测结果（否则会拿旧结果当新的）。
    pub fn select_distro(&mut self, id: impl Into<String>) {
        let id = id.into();
        if self.distro_id == id {
            return;
        }
        self.distro_id = id;
        self.reset_probe();
    }

    /// 把探测结果清空（换发行版 / 重新拉清单时用）。
    pub fn reset_probe(&mut self) {
        self.results.clear();
        self.chosen = None;
        self.error = None;
        self.probing = false;
    }

    /// 按耗时排序：可用的在前（快的更前），连不上的 / 报错的统一排到后面。
    ///
    /// 界面上那一列是"哪个镜像快"，而探测是按**清单顺序**发起的 ——
    /// 不排一下，最快的那条会散落在中间（`chosen` 有徽标，但用户想比较
    /// 其余几条时就只能自己找）。
    ///
    /// ⚠️ **不能只按 `secs` 排**：探测没跑成的候选（`code == 0` 且
    /// `secs == 0.0`，见 `app::Shell::probe_mirrors` 里那个 `unwrap_or`）
    /// 会因此**排在最前面**，看起来像"最快的"。
    /// 用 `total_cmp` 而不是 `partial_cmp().unwrap()`：`secs` 是 f64，
    /// 万一出现 NaN，`unwrap()` 会让界面崩在"探测"这个动作上。
    pub fn sort_results_by_speed(&mut self) {
        self.results.sort_by(|left, right| {
            right
                .is_ok()
                .cmp(&left.is_ok())
                .then_with(|| left.secs.total_cmp(&right.secs))
        });
    }

    /// 按**微软商店清单里的 id**去镜像清单里找同一个发行版。
    ///
    /// 用途：商店那条路装不上时（`wsl.exe` 自己的网络不通）一键换成镜像源，
    /// 并且**尽量落在同一个发行版**上，不让用户自己在清单里翻一遍。
    ///
    /// 三轮匹配（都是"只比字母数字、忽略大小写与分隔符"）：
    ///
    /// 1. `名字 + 版本` 拼起来对上 —— `Ubuntu-24.04` ↔ `Ubuntu` + `24.04`；
    /// 2. 只对上名字 —— 在线 id 常常不带版本（`Debian`、`Ubuntu`）；
    /// 3. 名字**前缀**对上 —— 两边的叫法有出入时兜底，实测这几对就是靠它：
    ///    `kali-linux` ↔ `Kali`、`FedoraLinux-44` ↔ `Fedora 44`、
    ///    `archlinux` ↔ `Arch Linux`、`OracleLinux_9_5` ↔ `Oracle Linux`。
    ///
    /// 第 3 轮故意做得"宽"（可能落到同系列的另一个版本上）——反正选完能在界面上
    /// 看到是哪个版本，比"什么都不选、让用户自己去翻"好。找不到返回 `None`。
    pub fn match_online_id(&self, online_id: &str) -> Option<String> {
        let want = normalize_id(online_id);
        if want.is_empty() {
            return None;
        }

        if let Some(offer) = self
            .offers
            .iter()
            .find(|offer| normalize_id(&format!("{}{}", offer.name, offer.version)) == want)
        {
            return Some(offer.id());
        }
        if let Some(offer) = self
            .offers
            .iter()
            .find(|offer| normalize_id(&offer.name) == want)
        {
            return Some(offer.id());
        }
        self.offers
            .iter()
            .filter_map(|offer| {
                let name = normalize_id(&offer.name);
                // 至少 4 个字符才敢做前缀匹配（`kali` 是 4，`or`/`os` 这种太容易误伤）
                let hit = name.len() >= 4 && (want.starts_with(&name) || name.starts_with(&want));
                hit.then(|| (name.len(), offer.id()))
            })
            .max_by_key(|(len, _)| *len)
            .map(|(_, id)| id)
    }
}

/// 只留字母数字、转小写 —— 用来比较"两个来源里的同一版发行版"。
///
/// `Ubuntu-24.04` / `Ubuntu 24.04` / `ubuntu24.04` 都归一成 `ubuntu2404`。
pub(crate) fn normalize_id(text: &str) -> String {
    text.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// 发行版的**常用度**排序键（越小越靠前）。
///
/// ⚠️ 这张表的顺序是照参考实现（`wsl-dashboard-ref` 的 `get_distro_sort_rank`）
/// 写的：用户明确要求"在线发行版的清单照 wsl-dashboard 来"，而它把
/// Ubuntu / Debian 排在最前、没列到的排最后。
///
/// 代码是我们自己写的：它是 **GPL-3.0-only**、本仓库 Apache-2.0
/// （`AGENTS.md` §6），能对齐的只有"哪几个发行版常用"这个**事实**。
pub(crate) fn distro_rank(name: &str) -> usize {
    let lower = name.to_lowercase();
    for (needle, rank) in [
        ("ubuntu", 1),
        ("debian", 2),
        ("kali", 3),
        ("arch", 4),
        ("fedora", 5),
        ("opensuse", 6),
        // 参考实现把 SLES 也单列一档（`sles` 是它的缩写）
        ("suse linux enterprise", 7),
        ("sles", 7),
        ("almalinux", 8),
        ("rocky", 9),
        ("oracle", 10),
        ("alpine", 11),
        ("openeuler", 12),
        ("mint", 13),
    ] {
        if lower.contains(needle) {
            return rank;
        }
    }
    999
}

/// 按参考实现的顺序排：先常用度，同族再按**版本倒序**（新的在前）。
///
/// ⚠️ 版本这里按**数值**比，而不是照参考实现那样按 "名字+版本" 字符串倒序：
/// 同一族里混着 `16.0` / `15.6` / `tumbleweed` 这种，字符串倒序会把
/// `tumbleweed` 排到 `16.0` 前面（`t` > `1`），而数值比把"没有数字的"排最后
/// —— 滚动版放最后才符合直觉。同族里数值键相同的（都是滚动版）再按名字排，
/// 保证顺序**确定**（否则每次刷新清单的行序都可能不一样）。
pub(crate) fn sort_offers_like_reference(offers: &mut [Offer]) {
    offers.sort_by(|left, right| {
        let (left_rank, right_rank) = (distro_rank(&left.name), distro_rank(&right.name));
        if left_rank != right_rank {
            return left_rank.cmp(&right_rank);
        }
        version_key(&right.version)
            .cmp(&version_key(&left.version))
            .then_with(|| left.version.cmp(&right.version))
    });
}

/// 计划里已经走完的一步。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinishedStep {
    /// 这一步在干什么。
    pub label: String,
    /// 成功了吗。
    pub ok: bool,
    /// 失败原因（成功时为空串）。
    pub detail: String,
}

/// 安装是怎么结束的（还在跑时为 `None`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallOutcome {
    /// 装好了（消息里带名字）。
    Success(String),
    /// 失败。
    Failed {
        /// 失败原因。
        detail: String,
        /// 卡在哪一步。
        step: Option<String>,
    },
    /// 被用户取消。
    Cancelled,
}

/// 一次安装的实时状态。
///
/// # 为什么它和 [`PullProgress`] / [`ExportProgress`] 都不一样
///
/// 安装是**多步**的（在线安装改名要 8 步），所以除了日志和进度，
/// 还要能画出"走到第几步、哪几步已经过了"。
/// 这里存的就是那份**纯数据**：`wslc_core::cmd::install` 发事件，
/// [`InstallProgress::apply`] 把它翻译成状态 —— 翻译逻辑是纯函数，能单测。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallProgress {
    /// 发行版名（用户想要的**最终**名字）。
    pub name: String,
    /// 装的时候**先**注册成的那些名字（在线安装的源 id，以及失败时换的那个兄弟 id）。
    ///
    /// `wsl -l -v` 上先是它们，重定位之后才变成 [`Self::name`]。
    /// [`AppState::busy_with`] 必须两个都认 —— 否则改名安装的中间态里，
    /// 列表上那一行（源名）不受任何互斥保护，而重定位正是
    /// 「导出 → 注销 → 导入」那段不可逆的操作。
    /// 来源见 `wslc_core::model::install::InstallPlan::source_names`。
    pub source_names: Vec<String>,
    /// 计划里的步骤总数。
    pub total: usize,
    /// 当前第几步（从 1 开始；0 = 还没开始）。
    pub index: usize,
    /// 当前这步在干什么。
    pub step_label: String,
    /// 当前这步对应的命令行（没有命令的步骤是空串）。
    pub step_line: String,
    /// 当前这步能不能被取消。
    ///
    /// 重定位（导出 → 注销 → 导入）那一步是 `false`：中途停下等于把刚装好的删了。
    /// 界面据此**不给**取消按钮，而不是让用户点了之后才发现没用。
    pub cancellable: bool,
    /// 已经走完的步骤。
    pub finished: Vec<FinishedStep>,
    /// 日志（环形截断）。
    pub log: Vec<String>,
    /// 最后一行输出（出错时它就是原因）。
    pub last_line: String,
    /// 已经产生多少字节（下载 / 导出）。
    pub have: u64,
    /// 预期总量；未知时 `None`。
    pub total_bytes: Option<u64>,
    /// **当前这一步**跑了多少秒（导出 / 下载那种有进度的步骤才有）。
    ///
    /// 和 [`InstallProgress::elapsed_secs`] 分开：那个是整场安装的用时
    /// （界面每轮更新），而速度要按**这一步**的时间算 ——
    /// 用整场时间去除下载量会把速度显示得偏小得离谱。
    pub step_secs: u64,
    /// 整场安装跑了多少秒。
    pub elapsed_secs: u64,
    /// 结束状态。
    pub outcome: Option<InstallOutcome>,
}

impl InstallProgress {
    /// 最多留多少行日志。
    ///
    /// 在线安装和镜像下载的输出都不少，全留着只会让内存一直涨；
    /// 用户真正要看的是**最后几行**和"走到第几步"。
    pub const MAX_LOG_LINES: usize = 2000;

    /// 新建一个进度记录。
    ///
    /// `source_names` 是"装的时候会先注册成哪些名字"（见 [`Self::source_names`]）——
    /// 由 `InstallPlan::source_names()` 算好传进来，这里不再猜。
    pub fn new(name: impl Into<String>, source_names: Vec<String>) -> Self {
        Self {
            name: name.into(),
            source_names,
            total: 0,
            index: 0,
            step_label: String::new(),
            step_line: String::new(),
            cancellable: true,
            finished: Vec::new(),
            log: Vec::new(),
            last_line: String::new(),
            have: 0,
            total_bytes: None,
            step_secs: 0,
            elapsed_secs: 0,
            outcome: None,
        }
    }

    /// 还在跑吗。
    pub fn is_running(&self) -> bool {
        self.outcome.is_none()
    }

    /// 已经走完几步。
    pub fn finished_count(&self) -> usize {
        self.finished.len()
    }

    /// 结束掉（成功/失败/取消）。
    ///
    /// 会往日志里补一行结局 —— 用户回看日志时，最后一行就是结论。
    /// 同一个结局重复设置不算变化（也**不会**重复写日志）。
    pub fn finish(&mut self, outcome: InstallOutcome) -> bool {
        if self.outcome.as_ref() == Some(&outcome) {
            return false;
        }
        let line = match &outcome {
            InstallOutcome::Success(message) => format!("✔ 完成：{message}"),
            InstallOutcome::Failed { detail, .. } => format!("✘ 失败：{detail}"),
            InstallOutcome::Cancelled => "✘ 已取消".to_owned(),
        };
        self.push_log(line);
        self.outcome = Some(outcome);
        true
    }

    /// 进度百分比（0~100）；拿不到总大小时 `None`。
    pub fn percent(&self) -> Option<f64> {
        let total = self.total_bytes?;
        if total == 0 {
            return None;
        }
        Some((self.have as f64 / total as f64 * 100.0).clamp(0.0, 100.0))
    }

    /// 平均速度（人话）；没在下载 / 时间还是 0 时为 `None`。
    pub fn speed(&self) -> Option<String> {
        if self.step_secs == 0 || self.have == 0 {
            return None;
        }
        Some(mirrors::human_speed(
            self.have as f64 / self.step_secs as f64,
        ))
    }

    /// 追加一行日志（并裁掉超上限的旧行）。
    fn push_log(&mut self, line: String) {
        self.log.push(line);
        if self.log.len() > Self::MAX_LOG_LINES {
            let excess = self.log.len() - Self::MAX_LOG_LINES;
            self.log.drain(..excess);
        }
    }

    /// 吃一个事件，返回**状态是否真的变了**（界面据此决定要不要重绘）。
    ///
    /// 进度事件每 250 ms 就来一个，而其中大多数只是重复的数字 ——
    /// 不比较就重绘等于让界面一直空转（导出那边是同样的处理）。
    pub fn apply(&mut self, event: &InstallEvent) -> bool {
        match event {
            InstallEvent::Step {
                index,
                total,
                label,
                line,
                cancellable,
            } => {
                self.index = *index;
                self.total = *total;
                self.step_label = label.clone();
                self.step_line = line.clone();
                self.cancellable = *cancellable;
                // 换步骤了：进度归零（那是上一步的产物大小）
                self.have = 0;
                self.total_bytes = None;
                self.step_secs = 0;
                self.push_log(format!("▶ 第 {index}/{total} 步：{label}"));
                if !line.is_empty() {
                    self.push_log(format!("  {line}"));
                }
                true
            }
            InstallEvent::Line(text) => {
                self.last_line = text.clone();
                self.push_log(text.clone());
                true
            }
            InstallEvent::Progress { have, total, secs } => {
                let changed =
                    self.have != *have || self.total_bytes != *total || self.step_secs != *secs;
                self.have = *have;
                self.total_bytes = *total;
                self.step_secs = *secs;
                changed
            }
            InstallEvent::StepDone { ok, detail, .. } => {
                self.finished.push(FinishedStep {
                    label: self.step_label.clone(),
                    ok: *ok,
                    detail: detail.clone(),
                });
                if *ok {
                    self.push_log(format!("✔ {}", self.step_label));
                } else {
                    self.push_log(format!("✘ {}：{detail}", self.step_label));
                }
                true
            }
        }
    }
}
