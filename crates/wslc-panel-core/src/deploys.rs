//! 部署记录的**本地存档**（`deployments.json`）。
//!
//! # 为什么记录存在面板这边，而不是容器里的一个清单文件
//!
//! 这是个真的岔路，两条都想过：
//!
//! | 放容器里 | 放本地 |
//! |---|---|
//! | 记录跟着容器走 | 容器停着**也能看** |
//! | 要在容器里维护一个清单 + 解析它 | 读一次几毫秒，能挂在页面加载路径上 |
//! | **容器一停就什么都看不到** | 容器被删掉后记录会指向空目录（状态列会说「目标不存在」） |
//!
//! 选本地是因为本机那三个容器**平时就是 Exited 状态**（实测）——
//! "停着就看不到自己部署过什么"是个真问题，而另一种的代价只是"状态列会说实话"。
//!
//! ⚠️ 但**运行状态绝不从这份存档里读**。存档记的只是"我们部署过什么"，
//! "它现在跑没跑""目标目录还在不在"**一律问容器**（`model::deploy` 那几条脚本）。
//! 分工是「**本地记部署过什么，容器答现在怎么样**」，不是二选一。
//!
//! # 存什么、存哪、兜底
//!
//! - 文件：`%LOCALAPPDATA%\wslc-panel\deployments.json`（和 `prefs.json`
//!   同一个目录，见 [`crate::prefs`]）；
//! - 结构：`{ "<容器名>/<应用名>": { … } }` —— **键里带容器名**：
//!   两个容器各有一个 `site` 时互不串台（和 `dbcreds` 同一条理由）；
//! - 解析失败（手改坏了 / 半截写入）**不 panic、不覆盖**：返回空档 + 一句原因，
//!   写入走 [`crate::fileio`] 的原子写（临时文件 → `.bak` → 主文件），
//!   半途断电最多丢掉这一次的更新，旧档还能恢复。
//!
//! 这里**没有密码**，所以 `Debug` 可以派生（对比 `dbcreds::DbCredential`，
//! 那个的 `Debug` 是手写脱敏的）。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use wslc_core::model::deploy::{TargetOverlap, target_overlap};
use wslc_core::model::DeploySite;

/// 一条部署记录。
///
/// 字段刻意都是**扁平的字符串**（运行时存的是 `"nginx"` 这种稳定键，不是
/// `AppRuntime` 的 Rust 变体名）：这样这个文件是**能手改、能看懂**的，
/// 而且以后改了枚举的写法也不会把老档读废。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployRecord {
    /// 应用名（也是目标目录名）。由 `model::deploy::valid_app_name` 的白名单把关。
    pub app: String,
    /// 运行时键：`nginx` / `java` / `node` / `go`（见 `AppRuntime::key`）。
    pub runtime: String,
    /// **本地**来源路径（用户在 Windows 上选的那个文件 / 目录）。
    pub source: String,
    /// 容器内的目标目录。
    pub target: String,
    /// 启动命令（**原样**交给容器里的 `/bin/sh -c`）。内容形态（nginx）是空串。
    pub command: String,
    /// 解包之后要 `chmod +x` 的**文件名**；空串 = 不需要。
    ///
    /// 存下来而不是每次从 `source` 重算：源文件被改名 / 删掉之后就重算不出来了，
    /// 而"停止 / 重新部署"这些动作**不应该**因为本地源没了就做不了。
    pub artifact: String,
    /// 应用监听的端口（**信息性** —— 已存在的容器改不了端口发布，
    /// 见 `cmd::deploy` 的说明）。
    pub port: u16,
    /// 上一次**真的铺进目标目录的名字**（文件来源是文件名，目录来源是顶层条目名）。
    ///
    /// 「重新部署」和「删除」靠它决定删哪些 —— 删的**只有这些**（外加
    /// `app.log` / `app.pid`），**不再整目录清空**。见
    /// `model::deploy::removals_script` 那段：v0.9.8 用 `find -delete` 清目录，
    /// 把 `eclipse-temurin` 镜像自带的 JDK（正好在默认目标目录 `/opt/java`
    /// 底下）删掉过一次，应用从此起不来。
    ///
    /// `#[serde(default)]`：v0.9.10 之前的存档里没有这一项，读出来是空表
    /// （那种老记录只能删日志 / pidfile —— 比"猜它铺了什么"安全）。
    #[serde(default)]
    pub members: Vec<String>,
    /// nginx 站点配置。`Some` ⟺ 容器里**确实有**面板生成的那份 conf 文件。
    ///
    /// `#[serde(default)]`：v0.9.19 之前的存档里没有这一项；`skip_serializing_if`
    /// 让 `None` 不进 JSON（老档看起来还是一样干净）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site: Option<DeploySite>,
}

/// 全部记录，按 `"<容器名>/<应用名>"` 索引。
///
/// `BTreeMap`：序列化出来是**按键排序**的（用户手改时看得懂，diff 也稳定）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployStore {
    #[serde(flatten)]
    entries: BTreeMap<String, DeployRecord>,
}

/// `"<容器>/<应用>"` 形式的键。
///
/// 单独一个函数：容器名 / 应用名里理论上可以有 `/`（我们不希望，但解析层的原则是
/// 不 panic）—— 拼的时候 trim 一下，读的时候按**前缀**筛。
pub fn key(container: &str, app: &str) -> String {
    format!("{}/{}", container.trim(), app.trim())
}

impl DeployStore {
    /// 读档。文件不存在 → 空档（第一次运行就是这么进来的）；
    /// 解析失败 → 空档 + `Some(原因)`（调用方**必须**把它显示出来）。
    pub fn load(path: &Path) -> (Self, Option<String>) {
        // 主文件缺失时从 `.bak` 恢复（见 `fileio` 的崩溃点表）。
        let text = match crate::fileio::read_text(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Self::default(), None),
            Err(e) => return (Self::default(), Some(format!("读不到部署存档：{e}"))),
        };
        match serde_json::from_str::<DeployStore>(&text) {
            Ok(store) => (store, None),
            Err(e) => (
                Self::default(),
                Some(format!(
                    "部署存档解析失败（{}）—— 里面的记录要重新部署一次：{e}",
                    path.display()
                )),
            ),
        }
    }

    /// 写档（原子写，见 [`crate::fileio`] 的模块注释）。
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let text = serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_owned());
        crate::fileio::write_atomic(path, &text)
    }

    /// 取一条记录。
    pub fn get(&self, container: &str, app: &str) -> Option<&DeployRecord> {
        self.entries.get(&key(container, app))
    }

    /// 记 / 更新一条（同名即覆盖 —— "重新部署"就是这条路）。
    pub fn put(
        &mut self,
        container: &str,
        record: DeployRecord,
    ) -> Option<DeployRecord> {
        let app = record.app.clone();
        self.entries.insert(key(container, &app), record)
    }

    /// 删掉一条。
    pub fn remove(&mut self, container: &str, app: &str) -> Option<DeployRecord> {
        self.entries.remove(&key(container, app))
    }

    /// 某个容器的**全部**记录，按应用名排序。
    ///
    /// 按**前缀**筛而不是"按 `/` 拆"：容器名里如果真有 `/`，拆法会把名字截断。
    /// 前缀带上那个 `/` 就不会把 `web` 和 `web2` 混起来。
    pub fn for_container(&self, container: &str) -> Vec<&DeployRecord> {
        let prefix = format!("{}/", container.trim());
        self.entries
            .iter()
            .filter(|(k, _)| k.starts_with(&prefix))
            .map(|(_, v)| v)
            .collect()
    }

    /// 条数（测试与"共几条"的展示用）。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    // -----------------------------------------------------------------------
    // 多应用：端口 / 目录冲突（v0.9.19）
    // -----------------------------------------------------------------------

    /// 新端口能不能用；`Some(应用名)` = 已经被别的应用占了。
    ///
    /// 只算**真的会 bind** 的记录（见 [`record_binds_port`]）。
    pub fn port_conflict(&self, container: &str, app: &str, port: u16) -> Option<&str> {
        self.for_container(container)
            .into_iter()
            .find(|record| {
                record.app.as_str() != app.trim()
                    && record_binds_port(*record)
                    && record.port == port
            })
            .map(|record| record.app.as_str())
    }

    /// 同一容器里**真的会 bind** 的端口（去重、升序）。
    pub fn binding_ports(&self, container: &str) -> Vec<u16> {
        let mut out: Vec<u16> = self
            .for_container(container)
            .into_iter()
            .filter(|record| record_binds_port(*record) && record.port > 0)
            .map(|record| record.port)
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// **新建**应用时能不能用这个目标目录；`None` = 可以。
    ///
    /// 规则（方向很重要，见 `docs/PLAN-multi-app.md` §4.3）：
    /// - 相同 / 新目标是已有目标的父目录 → 拦；
    /// - 新目标在旧目录里面 → 只有当"子目录第一段"不在旧应用的
    ///   [`DeployRecord::members`] 里才放行（否则旧应用的逐名删除会带走它）。
    pub fn target_blocker(
        &self,
        container: &str,
        app: &str,
        target: &str,
    ) -> Option<TargetBlock> {
        let target = normalize_target(target);
        for record in self.for_container(container) {
            if record.app.as_str() == app.trim() {
                continue;
            }
            let existing = normalize_target(&record.target);
            match target_overlap(target, existing) {
                Some(TargetOverlap::Same) => {
                    return Some(TargetBlock::Same(record.app.clone()));
                }
                Some(TargetOverlap::ExistingInsideNew) => {
                    return Some(TargetBlock::CoversExisting(record.app.clone()));
                }
                Some(TargetOverlap::NewInsideExisting) => {
                    if let Some(name) = first_segment_under(target, existing) {
                        if record.members.iter().any(|member| member.trim() == name) {
                            return Some(TargetBlock::InsideMember {
                                existing: record.app.clone(),
                                name: name.to_owned(),
                            });
                        }
                    }
                }
                None => {}
            }
        }
        None
    }
}

/// 一条记录会不会真的在容器里 bind 一个端口。
///
/// - 进程型（`java` / `node` / `go`）会；
/// - nginx 只有**站点开**（`site: Some`）才会 `listen <port>`；站点关的记录
///   由容器的默认站点提供，`port` 只是信息；
/// - 认不出来的运行时键按"会 bind"算（保守：宁可多拦一个端口，
///   不要让两个应用真的撞上）。
fn record_binds_port(record: &DeployRecord) -> bool {
    record.runtime.trim() != "nginx" || record.site.is_some()
}

/// 去掉尾部 `/`；空串（或全 `/`）归一成 `/`。
fn normalize_target(path: &str) -> &str {
    let trimmed = path.trim().trim_end_matches('/');
    if trimmed.is_empty() { "/" } else { trimmed }
}

/// `path` 在 `parent` 底下的**第一段**。
fn first_segment_under<'a>(path: &'a str, parent: &str) -> Option<&'a str> {
    let rest = if parent == "/" {
        path.strip_prefix('/')?
    } else {
        path.strip_prefix(parent)?.strip_prefix('/')?
    };
    rest.split('/').next().filter(|name| !name.is_empty())
}

/// `target_blocker` 给出的三种"不能这么做"。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetBlock {
    /// 同一个目录。
    Same(String),
    /// 新目标是已有目标的父目录。
    CoversExisting(String),
    /// 新目标在旧目录里，而"第一段"正是旧应用铺过的名字。
    InsideMember { existing: String, name: String },
}

impl TargetBlock {
    /// 给用户看的一句话（`target` 是用户填的那个目录）。
    pub fn message(&self, target: &str) -> String {
        match self {
            Self::Same(app) => format!(
                "目标目录 {target} 已经被「{app}」用了 —— 一个应用一个目录；\
                 换个目录（比如 /opt/<你的应用名> 或 Web 根下的子目录）"
            ),
            Self::CoversExisting(app) => format!(
                "这个目录是「{app}」的父目录 —— 新应用铺进去可能把它的子目录当自己的\
                 文件删掉；换一个更具体的目录"
            ),
            Self::InsideMember { existing, name } => format!(
                "「{existing}」铺过「{name}」—— 新应用铺到 {target} 会被它的\
                 重新部署删掉；换个应用名/目录"
            ),
        }
    }
}

/// 默认的存档路径：`<数据目录>/deployments.json`。
///
/// `data_dir` 传 `prefs.json` 所在的目录（`%LOCALAPPDATA%\wslc-panel`），
/// 保持"面板自己的东西都在一处"。
pub fn store_path(data_dir: impl AsRef<Path>) -> PathBuf {
    data_dir.as_ref().join("deployments.json")
}

/// 一次部署的**删除列表**：这一次要铺的 ∪ **上一次铺过的**（去重）。
///
/// # 为什么必须有"上一次"这一半（v0.9.13 修）
///
/// [`crate::deploys::DeployRecord::members`] 的文档、以及
/// `model::deploy::removals_script` 的文档都写着"删除列表有三个来源：
/// 这次的成员、**存档里上一次的成员**、外加 `app.log` / `app.pid`"。
/// 而 `Shell::submit_deploy` 早先只传了**这次的**成员 —— 于是源目录里
/// **改名 / 删掉**的文件会永远留在目标目录里（"我明明改了，页面上还是旧的"
/// 正是 `docs/PLAN-v0.9.md` §2.3 当初要避免的那一类），而且记录随后被覆盖，
/// 连「删除」也再也收拾不到它。
///
/// 顺序：**这次的在前**（它们马上要被写进去），上一次剩下的跟在后面。
/// 去重按 trim 之后的名字比，空名字直接丢掉（`removals_script` 那边还会再
/// 拦一次带 `/`、`..` 的，这里不做路径判断）。
///
/// ⚠️ 纯函数、住在这里（而不是 `app.rs`）：`app.rs` 没有 `#[cfg(test)]`，
/// 而 CI 的测试任务**不跑 `wslc-panel`** —— 写在那儿等于没有断言。
pub fn merge_members(previous: &[String], current: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(previous.len() + current.len());
    for name in current.iter().chain(previous.iter()) {
        let name = name.trim();
        if name.is_empty() || out.iter().any(|kept| kept.as_str() == name) {
            continue;
        }
        out.push(name.to_owned());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ⚠️ 回归钉（v0.9.13）：**上一次铺过、这一次不铺了**的名字必须留在删除列表里。
    ///
    /// 现场的形状：源目录里 `b.html` 改名成了 `c.html`，用户点「重新部署」。
    /// 少了这一半，`b.html` 会永远留在容器里（nginx 照旧把它发给访客）。
    #[test]
    fn merge_members_keeps_the_names_we_laid_down_last_time() {
        let previous = vec!["a.html".to_owned(), "b.html".to_owned()];
        let current = vec!["a.html".to_owned(), "c.html".to_owned()];
        assert_eq!(
            merge_members(&previous, &current),
            vec!["a.html", "c.html", "b.html"],
            "这次的在前、上一次剩下的跟在后面，重复的只留一份"
        );
    }

    #[test]
    fn merge_members_handles_the_empty_shapes() {
        let one = vec!["x".to_owned()];
        assert_eq!(merge_members(&[], &one), one, "第一次部署：只有这次的");
        assert_eq!(merge_members(&one, &[]), one, "这次没铺东西（理论上到不了）");
        assert!(merge_members(&[], &[]).is_empty());
        // 老存档里没有 `members`（`#[serde(default)]` → 空表）：那就是只有这次的
        assert_eq!(merge_members(&[], &["x".to_owned()]), vec!["x"]);
    }

    #[test]
    fn merge_members_trims_and_drops_empty_names() {
        let previous = vec!["  ".to_owned(), " a ".to_owned(), String::new()];
        let current = vec!["a".to_owned()];
        assert_eq!(merge_members(&previous, &current), vec!["a"]);
    }

    fn record(app: &str, target: &str) -> DeployRecord {
        DeployRecord {
            app: app.to_owned(),
            runtime: "go".to_owned(),
            source: format!(r"E:\build\{app}"),
            target: target.to_owned(),
            command: format!("./{app}"),
            artifact: app.to_owned(),
            port: 8080,
            members: vec![app.to_owned()],
            site: None,
        }
    }

    /// 多应用测试用的记录：运行时 / 端口 / 目录 / 铺过的名字都能指定。
    fn record_of(
        app: &str,
        runtime: &str,
        port: u16,
        target: &str,
        members: &[&str],
    ) -> DeployRecord {
        DeployRecord {
            app: app.to_owned(),
            runtime: runtime.to_owned(),
            source: format!(r"E:\build\{app}"),
            target: target.to_owned(),
            command: String::new(),
            artifact: String::new(),
            port,
            members: members.iter().map(|name| (*name).to_owned()).collect(),
            site: None,
        }
    }

    #[test]
    fn keys_are_scoped_by_container() {
        let mut store = DeployStore::default();
        store.put("web", record("site", "/opt/site"));
        store.put("web2", record("site", "/srv/site"));
        // 两个容器各有一个 `site` —— 互不串台
        assert_eq!(store.get("web", "site").unwrap().target, "/opt/site");
        assert_eq!(store.get("web2", "site").unwrap().target, "/srv/site");
        assert_eq!(store.len(), 2);
        assert_eq!(key(" web ", " site "), "web/site");
    }

    #[test]
    fn for_container_does_not_confuse_similarly_named_containers() {
        // ⚠️ 这条是"按前缀筛"而不是"按 `/` 拆"的理由：`web` 不该看见 `web2` 的记录
        let mut store = DeployStore::default();
        store.put("web", record("a", "/opt/a"));
        store.put("web", record("b", "/opt/b"));
        store.put("web2", record("c", "/opt/c"));

        let web = store.for_container("web");
        assert_eq!(web.len(), 2);
        assert!(web.iter().all(|r| r.target != "/opt/c"), "{web:?}");

        assert_eq!(store.for_container("web2").len(), 1);
        assert!(store.for_container("nope").is_empty());
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!("wslc-deploys-{}", std::process::id()));
        let path = store_path(&dir);
        let _ = std::fs::remove_file(&path);

        let mut store = DeployStore::default();
        store.put("nginx", record("site", "/usr/share/nginx/html"));
        store.save(&path).expect("应能写档");

        let (loaded, error) = DeployStore::load(&path);
        assert!(error.is_none(), "{error:?}");
        assert_eq!(
            loaded.get("nginx", "site").unwrap().target,
            "/usr/share/nginx/html"
        );
        // 每种字段都要真的往返一次 —— 少存一个字段的表现是"重新部署时命令变空了"
        assert_eq!(loaded.get("nginx", "site").unwrap().artifact, "site");
        assert_eq!(loaded.get("nginx", "site").unwrap().port, 8080);

        std::fs::remove_file(&path).ok();
        std::fs::remove_dir(&dir).ok();
    }

    #[test]
    fn the_archive_is_hand_editable_json() {
        // 存档是给用户看得懂的：键是 `<容器>/<应用>`，运行时是 `"go"` 这种稳定键，
        // **不是** Rust 的变体名（`Go`）也不是显示名（`Node.js`）
        let mut store = DeployStore::default();
        store.put("nginx", record("site", "/opt/site"));
        let text = serde_json::to_string(&store).unwrap();
        assert!(text.contains(r#""nginx/site""#), "{text}");
        assert!(text.contains(r#""runtime":"go""#), "{text}");
    }

    #[test]
    fn missing_file_is_an_empty_store_not_an_error() {
        let (store, error) = DeployStore::load(Path::new(
            r"Z:\definitely\not\a\real\path\deployments.json",
        ));
        assert!(store.is_empty());
        assert!(error.is_none(), "文件不存在不算错：{error:?}");
    }

    #[test]
    fn corrupt_file_degrades_to_empty_and_reports() {
        let dir = std::env::temp_dir().join(format!("wslc-deploys-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = store_path(&dir);
        std::fs::write(&path, "{ 不是 json").unwrap();

        let (store, error) = DeployStore::load(&path);
        assert!(store.is_empty());
        // ⚠️ 这个 `Some` **必须**被界面显示出来（`AppState::deploy_store_error`
        // → `views::deploy_page` 的提示条）。这条断言只保证"错误算出来了"，
        // 保证不了"画出来了" —— 后者在 CI 里没有信号（`wslc-panel` 不在测试目标里）。
        assert!(error.is_some(), "损坏要能被说出来");

        std::fs::remove_file(&path).ok();
        std::fs::remove_dir(&dir).ok();
    }

    #[test]
    fn put_with_the_same_name_replaces_instead_of_duplicating() {
        // "重新部署"就是这条路：同名覆盖，不能攒出两条
        let mut store = DeployStore::default();
        store.put("web", record("site", "/opt/old"));
        store.put("web", record("site", "/opt/new"));
        assert_eq!(store.len(), 1);
        assert_eq!(store.get("web", "site").unwrap().target, "/opt/new");
    }

    #[test]
    fn remove_drops_exactly_one_entry() {
        let mut store = DeployStore::default();
        store.put("web", record("site", "/opt/site"));
        store.put("web", record("api", "/opt/api"));
        assert_eq!(store.remove("web", "site").unwrap().app, "site");
        assert!(store.get("web", "site").is_none());
        assert!(store.get("web", "api").is_some());
        assert_eq!(store.remove("web", "site"), None);
    }

    // -----------------------------------------------------------------------
    // 多应用：端口 / 目录冲突（v0.9.19）
    // -----------------------------------------------------------------------

    #[test]
    fn nginx_records_own_a_port_only_with_a_site() {
        // nginx 的站点关形态不 bind 端口（靠容器里的默认站点）；
        // 站点开（DeploySite）才会 `listen <port>`，这时它算真的占端口。
        let mut store = DeployStore::default();
        let mut site = record_of("site", "nginx", 80, "/srv/site", &["index.html"]);
        store.put("web", site.clone());
        store.put(
            "web",
            record_of("api", "java", 8080, "/opt/api", &["app.jar"]),
        );
        assert_eq!(store.binding_ports("web"), vec![8080]);
        assert_eq!(store.port_conflict("web", "new", 80), None);
        assert_eq!(store.port_conflict("web", "new", 8080), Some("api"));

        site.site = Some(wslc_core::model::DeploySite {
            server_name: "localhost".to_owned(),
            spa: true,
            proxies: Vec::new(),
            conf: wslc_core::model::deploy::conf_path("site"),
        });
        store.put("web", site);
        assert_eq!(store.binding_ports("web"), vec![80, 8080]);
        assert_eq!(store.port_conflict("web", "new", 80), Some("site"));
        // 同一个应用重新部署不算冲突
        assert_eq!(store.port_conflict("web", "api", 8080), None);
    }

    #[test]
    fn target_blocker_allows_the_old_root_to_migrate() {
        // 老记录铺在 Web 根、新应用铺到它的子目录 —— 这是**迁移路径**，必须放行
        let mut store = DeployStore::default();
        store.put(
            "web",
            record_of("site", "nginx", 80, "/root", &["index.html"]),
        );
        assert!(store.target_blocker("web", "app-2", "/root/app-2").is_none());
        assert!(store.target_blocker("web", "app-2", "/root/app-2/").is_none());
        assert!(store.target_blocker("web", "app-2", "/root-other").is_none());
    }

    #[test]
    fn target_blocker_rejects_same_parent_and_owned_names() {
        let mut same = DeployStore::default();
        same.put(
            "web",
            record_of("site", "nginx", 80, "/root", &["index.html"]),
        );
        assert_eq!(
            same.target_blocker("web", "app", "/root/"),
            Some(TargetBlock::Same("site".to_owned()))
        );
        assert_eq!(
            same.target_blocker("web", "app", "/"),
            Some(TargetBlock::CoversExisting("site".to_owned()))
        );

        let mut owned = DeployStore::default();
        owned.put(
            "web",
            record_of("assets", "nginx", 80, "/root", &["assets"]),
        );
        assert_eq!(
            owned.target_blocker("web", "app", "/root/assets"),
            Some(TargetBlock::InsideMember {
                existing: "assets".to_owned(),
                name: "assets".to_owned(),
            })
        );
    }

    #[test]
    fn target_block_messages_name_the_other_app() {
        let block = TargetBlock::InsideMember {
            existing: "site".to_owned(),
            name: "assets".to_owned(),
        };
        let message = block.message("/root/assets");
        assert!(message.contains("site"), "{message}");
        assert!(message.contains("assets"), "{message}");
        assert!(message.contains("/root/assets"), "{message}");
    }
}
