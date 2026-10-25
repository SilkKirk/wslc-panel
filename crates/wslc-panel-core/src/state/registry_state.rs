//! 镜像仓库（Registry V2）查询状态、容器详情巡检结果。

use super::prelude::*;

/// 「镜像」页**远端那一半**的状态（v0.6 建，v0.6.2 并进「镜像」页）。
///
/// 全是纯数据 —— registry / 仓库名两个输入框和 tag 下拉框是
/// `Entity<InputState>` / `Entity<SelectState<_>>`，留在 `wslc-panel` 的 `Shell` 里
/// （和「添加实例」页同一个分工）。
///
/// # 为什么要分开记 `repository` 与 `loaded`
///
/// 用户从 `nginx` 换到 `redis` 时，网络那 1~2 秒里**结果还是 nginx 的**。
/// 如果界面只认一个仓库名，那段时间就会把 nginx 的 tag 挂在 redis 的标题下 ——
/// 而这恰恰是用户最容易误判成"redis 有这些 tag"的时刻（串台）。
/// 所以：`repository` 是"正在查谁"，`loaded` 是"列表里的东西是谁的"，
/// 两者不等时界面显示"正在查 X"，不显示旧结果（[`RegistryState::begin_query`] 会清）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryState {
    /// registry 地址（**用户可编辑**，见 [`registry::is_valid_host`]）。
    ///
    /// v0.6.2 之前这是个常量：合并两页时它必须变成字段，否则"从别的 registry 拉"
    /// 这个能力就没了（当初正是为了它才留着两个拉取入口，见
    /// `docs/PLAN-v0.6.md` §3.6）。
    pub host: String,
    /// 用户输进框里的原文（用来显示"由 … 解析为 …"）。
    pub input: String,
    /// 正在查的仓库名（**规范化之后**的，`alpine` → `library/alpine`）。
    pub repository: String,
    /// 上一次查成功、且列表里正显示的那个仓库名。
    pub loaded: Option<String>,
    /// tag 列表（已排序：`latest` 优先、版本倒序）——
    /// 整个列表都会喂给 tag 下拉框：它的列表是**虚拟化**的，
    /// 所以实测里 `node` 那种 9170 个 tag 也不会每帧建 9170 个元素
    /// （v0.6.2 之前是"本地过滤 + 只渲染前 200 行"）。
    pub tags: Vec<String>,
    /// 正在查 tag。
    pub loading: bool,
    /// 查 tag 失败的原因（能给用户看的一句话）。
    pub error: Option<String>,
    /// 选中的 tag（下拉框点出来的）。
    pub selected: Option<String>,
    /// **用户输入里自带的** tag（`nginx:1.27` 的 `1.27`）。
    ///
    /// 为什么要有它：tags 列表拉不到时（别的 registry 的 `tags/list` 可能要认证，
    /// 实测 daocloud 就 401）下拉框里一个 tag 都没有，但用户其实已经把 tag
    /// 写在输入里了 —— 那就该能拉。它是 [`RegistryState::effective_tag`] 的兜底，
    /// 也是"按完整引用拉取"这条能力在合并之后唯一的落点。
    pub input_tag: Option<String>,
    /// 选中 tag 的下载体积（字节，**压缩后**）。
    pub size: Option<u64>,
    /// 选中 tag 的层数。
    pub layers: Option<usize>,
    /// 正在查体积。
    pub sizing: bool,
    /// 查体积失败的原因。
    pub size_error: Option<String>,
}

impl Default for RegistryState {
    fn default() -> Self {
        Self::new()
    }
}

impl RegistryState {
    /// 初始状态：registry 是内置那个（用户可改），其余全是空的。
    pub fn new() -> Self {
        Self {
            host: registry::DEFAULT_REGISTRY.to_owned(),
            input: String::new(),
            repository: String::new(),
            loaded: None,
            tags: Vec::new(),
            loading: false,
            error: None,
            selected: None,
            input_tag: None,
            size: None,
            layers: None,
            sizing: false,
            size_error: None,
        }
    }

    /// 开始查一个仓库：**先把上一次的结果清干净**。
    ///
    /// ⚠️ 这一步不能省，理由见类型文档（换仓库时串台）。
    ///
    /// `input_tag` 是输入里自带的 tag（没有就 `None`）——
    /// 查询回来之后由调用方拿它选中（见 [`RegistryState::effective_tag`]）。
    pub fn begin_query(
        &mut self,
        host: &str,
        input: &str,
        repository: &str,
        input_tag: Option<String>,
    ) {
        self.host = host.to_owned();
        self.input = input.to_owned();
        self.repository = repository.to_owned();
        self.clear_results();
        self.input_tag = input_tag;
        self.loading = true;
    }

    /// 编辑新输入后不再复用旧 tag 或旧拉取目标。
    pub fn matches_inputs(&self, host: &str, input: &str) -> bool {
        let host = registry::normalize_host(host);
        let host = if host.is_empty() {
            registry::DEFAULT_REGISTRY
        } else {
            host.as_str()
        };
        self.host == host && self.input.trim() == input.trim()
    }

    /// 清掉"上一次查询的结果"，但保留 registry 与用户输入。
    pub fn clear_results(&mut self) {
        self.tags.clear();
        self.loaded = None;
        self.error = None;
        self.selected = None;
        self.input_tag = None;
        self.size = None;
        self.layers = None;
        self.size_error = None;
        self.sizing = false;
    }

    /// 查 tag 成功。
    pub fn set_tags(&mut self, repository: &str, tags: Vec<String>) {
        self.repository = repository.to_owned();
        self.loaded = Some(repository.to_owned());
        self.loading = false;
        self.error = None;
        self.tags = tags;
        // 换了仓库，选中的那个 tag 就不属于新列表了。
        self.selected = None;
        self.size = None;
        self.layers = None;
        self.size_error = None;
        self.sizing = false;
    }

    /// 查 tag 失败。结果一并清掉 —— 报错的同时还留着一列旧 tag，
    /// 用户会以为那些是"这次查到的"。
    pub fn set_error(&mut self, message: String) {
        self.loading = false;
        self.sizing = false;
        self.error = Some(message);
        self.tags.clear();
        self.loaded = None;
        self.selected = None;
        self.size = None;
        self.layers = None;
        self.size_error = None;
    }

    /// 选中一个 tag。体积要**重新查**，所以先把旧的那个清掉。
    pub fn select_tag(&mut self, tag: &str) {
        self.selected = Some(tag.to_owned());
        self.size = None;
        self.layers = None;
        self.size_error = None;
        self.sizing = true;
    }

    /// 体积到手。
    pub fn set_size(&mut self, bytes: u64, layers: usize) {
        self.size = Some(bytes);
        self.layers = Some(layers);
        self.sizing = false;
        self.size_error = None;
    }

    /// 体积查失败（tag 本身还是选中的，只是量不出来）。
    pub fn set_size_error(&mut self, message: String) {
        self.sizing = false;
        self.size = None;
        self.layers = None;
        self.size_error = Some(message);
    }

    /// 要拉的那个 tag：**下拉框选的优先，其次用输入里自带的那个**。
    ///
    /// 兜底的用途见 [`RegistryState::input_tag`]：别的 registry 的 tag 列表
    /// 可能拉不到（要认证），那时用户写在输入里的 `:tag` 是唯一的凭据。
    pub fn effective_tag(&self) -> Option<&str> {
        self.selected.as_deref().or(self.input_tag.as_deref())
    }

    /// 下拉框里该列出哪些 tag：**列表 + （列表里缺的）用户手打的那个**。
    ///
    /// # 为什么手打的要补进去（v0.9.4 的实机 bug）
    ///
    /// 反馈原文："我想拉取 openjdk:8-jre-slim，结果啥东东都出来了"。现象是下拉框
    /// 显示 `28-ea`、"将拉取"也变成 `:28-ea` —— **拉错镜像**。
    ///
    /// 根因：用户打的 `8-jre-slim` 不在 registry 返回的列表里，于是界面那边
    /// `set_selected_value` 找不到对应项 → 下拉框没有任何选中 → 组件**自己选中
    /// 第一项**。而"列表里没有"不等于"拉不了"：**实测（2026-10-15，
    /// docker.1panel.live / library/openjdk）** 它的 `tags/list` 只回了 17282 个，
    /// 里面既没有 `8-jre-slim` 也没有 `latest`，可这两个 tag 的 `manifests/`
    /// 都是 **200**（能拉）。列表不全不是用户的错，更不该因此把要拉的东西换掉。
    ///
    /// 手打的那个放**最前面**：它是用户明确要的，而列表可能有两万项，
    /// 排在后面等于找不到。
    pub fn tag_items(&self) -> Vec<String> {
        let mut items: Vec<String> = self.tags.clone();
        if let Some(tag) = self.effective_tag() {
            if !self.tags.iter().any(|t| t == tag) {
                items.insert(0, tag.to_owned());
            }
        }
        items
    }

    /// 将要拉取的完整镜像引用（`docker.1panel.live/library/alpine:latest`）。
    ///
    /// 没有 tag（既没选、输入里也没写）或还没查过仓库就是 `None` ——
    /// 「拉取」按钮据此决定渲不渲染。
    ///
    /// ⚠️ 必须带 registry 前缀：不带的话 `wslc pull` 会去 Docker Hub 找，
    /// 而那台机器上 `registry-1.docker.io` 不可达（实测 000）。
    pub fn selected_reference(&self) -> Option<String> {
        let tag = self.effective_tag()?;
        let repository = self.loaded.as_deref().unwrap_or(self.repository.as_str());
        if repository.is_empty() || self.host.is_empty() {
            return None;
        }
        Some(registry::pull_reference(&self.host, repository, tag))
    }

    /// 一句话说明"这个仓库名是怎么来的"—— 原文与规范化结果**不同**时才给。
    pub fn normalization_note(&self) -> Option<String> {
        let input = self.input.trim();
        if input.is_empty() || input == self.repository {
            return None;
        }
        Some(format!("由 {input} 解析为 {}", self.repository))
    }

    /// 体积说人话：`下载约 3.9 MB（1 层）`。
    ///
    /// ⚠️ 措辞必须是"**下载约**"：这是 manifest 累加的**压缩后**大小，
    /// 而「镜像」页里 `wslc images` 的 `Size` 是解压后占用的（同机 nginx 实测
    /// 60.9 MB vs 163 MB，差 2.7 倍）。写成"大小"用户一对就会当 bug 报。
    pub fn size_label(&self) -> Option<String> {
        let bytes = self.size?;
        let layers = self.layers.unwrap_or(0);
        Some(format!(
            "下载约 {}（{layers} 层）",
            mirrors::human_bytes(bytes)
        ))
    }
}

/// 容器详情里那一份 `wslc inspect`。
///
/// # 为什么详情要单独拉一次 inspect
///
/// 详情里要显示的三样东西**列表那一份数据里没有**：
///
/// | 要显示的 | 列表（`wslc list -a`）有什么 | `inspect` 有什么 |
/// |---|---|---|
/// | 挂载 | `Mounts` 只有卷名（`mysqldata`），`Labels.V1.Volumes` 实测**是空的** | `Mounts[]` 的 `Source` + `Destination` |
/// | 启动命令 | `Command` 被 wslc **截断到 20 字符**（`"docker-entrypoint.s…"`） | `Config.Entrypoint` + `Config.Cmd` 全量 |
/// | 环境变量 | 完全没有 | `Config.Env` 全量 |
///
/// 而 `inspect` 是一次几百毫秒的子进程（`AGENTS.md` §7.1：不许放界面线程），
/// 所以**打开详情时在后台拉一次**，拉回来之前界面显示"正在读取…"。
///
/// `container` 用来防串台：用户可能点了 A、没等回来又点 B。
/// 写回之前比一次名字，不一致就丢掉 —— 否则 A 的挂载会画在 B 的详情里。
#[derive(Debug, Clone)]
pub struct DetailInspect {
    /// 这份数据属于哪个容器。
    pub container: String,
    /// 拉回来的结果；`None` = 还在读（或读失败了，看 `error`）。
    pub inspect: Option<wslc_core::model::ContainerInspect>,
    /// 读失败的原因（人话，界面上原样显示）。
    pub error: Option<String>,
}

impl DetailInspect {
    /// 开始读某个容器的详情。
    pub fn loading(container: impl Into<String>) -> Self {
        Self {
            container: container.into(),
            inspect: None,
            error: None,
        }
    }
}
