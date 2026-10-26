//! 名字与路径的校验：应用名白名单、目标目录安全、容器内路径拼接。

use std::path::Path;

// 实现只有一份：`crate::util::shell_quote`。这里再导出，`deploy::shell_quote`
// 的调用点（`cmd::deploy` / `model::deploy::script`）和测试都不用改。
pub use crate::util::shell_quote;

/// 应用名白名单：`[A-Za-z0-9_-]`，长度 1..=64。
///
/// 和 v0.7 的库名白名单同一条原则：**不合法就拒绝**，把选择权交回用户，
/// 而不是我们猜他的意图去转义。它是目标目录名、也是本地存档的键。
///
/// 刻意**不允许点**：`..` 会变成上一级目录，那是路径穿越。
pub fn valid_app_name(name: &str) -> bool {
    let len = name.chars().count();
    (1..=64).contains(&len)
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// 允许作为目标目录的**前缀白名单**。
///
/// 见 [`safe_target`]：因为"部署"和"删除"都会往目标目录里**放 / 删文件**，
/// 而目标目录是用户填的。
const SAFE_TARGET_PREFIXES: &[&str] = &[
    "/opt/",
    "/srv/",
    "/app/",
    "/data/",
    "/www/",
    "/workspace/",
    "/home/",
    "/tmp/",
    "/var/www/",
    // nginx 官方镜像的站点目录（本机那个容器的实测值就是它）
    "/usr/share/nginx/",
    "/usr/local/",
];

/// 目标目录的**安全闸**：不合规的路径直接拒绝。
///
/// # 为什么必须有这个东西
///
/// [`prepare_target_script`] 与 [`remove_script`] 都会在目标目录里**删东西**
/// （现在删的是我们自己铺的那些名字，见 [`removals_script`]），而目标目录是
/// **用户填的**。填成 `/usr` 就等于往系统目录里塞文件、还可能在删的时候
/// 撞上系统文件 —— 容器照样会被搞坏。
///
/// # 为什么是白名单而不是黑名单
///
/// 黑名单要枚举所有危险路径（`/usr`、`/usr/share`、`/etc`、`/var`、
/// `/var/lib`……），漏一个就是灾难。
/// 白名单反过来：**只认识这几种**，其余一律拒绝，并把允许的写进提示里。
///
/// 这条刻意保守 —— 想加一个前缀就是 [`SAFE_TARGET_PREFIXES`] 里加一行。
/// 用户真要部署到别处，用「终端」自己拷。
///
/// 另外两条：必须是**绝对路径**（相对路径的含义取决于容器的 WORKDIR，
/// 太含糊），且不许出现 `..`（路径穿越）。
pub fn safe_target(target: &str) -> bool {
    let target = target.trim();
    if !target.starts_with('/') || target.contains("..") || target.contains('\0') {
        return false;
    }
    // 根目录本身、以及任何白名单前缀之外的东西都不行
    SAFE_TARGET_PREFIXES
        .iter()
        .any(|prefix| target.starts_with(prefix) && target.len() > prefix.len())
}

/// [`safe_target`] 的**加一条**版本：除了白名单，还放行**这个容器自己的站点目录**。
///
/// # 为什么需要它
///
/// nginx 形态的目标目录默认值是 `Shell::open_deploy_dialog` **从 `inspect`
/// 的挂载点推出来的**（那个容器的站点落在一个卷上，不能写死
/// `/usr/share/nginx/html` —— 用户自己挂了别的目录时，写死会把文件铺到
/// **没人读**的地方）。而用户完全可以把站点挂在白名单之外（`-v webdata:/site`）。
///
/// 那种情况下**面板自己预填的路径会被自己的安全闸拒掉** —— 用户看到的是
/// "只允许填在 … 底下"，而正确的那个就摆在输入框里、是我们填的。这是个死胡同。
///
/// 所以这里多放行 `web_root` 本身、以及它**底下**的路径。
///
/// ⚠️ 这**不是**把闸门放宽成"任意路径"：`web_root` 来自**容器自己的配置**
/// （`inspect` 的 `Mounts[].Destination`），是那个运行时真正对外提供内容的地方
/// —— 往它里面铺站点文件正是"部署"的语义。而白名单仍然是默认那一层：拿不到 `web_root`
/// 时（`None`）、或者它自己就是个怪路径时，一行都不多放。
///
/// ⚠️ 判"在它底下"要看**路径边界**：`/sitemap` 不在 `/site` 底下。
pub fn safe_target_with_site(target: &str, web_root: Option<&str>) -> bool {
    if safe_target(target) {
        return true;
    }
    let Some(root) = web_root.map(str::trim).filter(|root| !root.is_empty()) else {
        return false;
    };
    let root = root.trim_end_matches('/');
    // 站点目录自己也必须先像个正常路径。`root.len() < 2` 挡的是 `/` ——
    // 站点目录是根目录时，借它放行等于把闸门整个拆掉。
    if root.len() < 2 || !root.starts_with('/') || root.contains("..") || root.contains('\0') {
        return false;
    }
    let target = target.trim();
    if !target.starts_with('/') || target.contains("..") || target.contains('\0') {
        return false;
    }
    target == root || target.starts_with(&format!("{root}/"))
}

/// 目标目录不合规时给用户看的那一句（把允许的前缀列出来）。
pub fn safe_target_hint() -> String {
    format!(
        "目标目录只允许填在 {} 底下 —— 面板要往这个目录里**放文件**，\
         所以不能是系统目录。要放到别处请用「终端」自己拷。",
        SAFE_TARGET_PREFIXES.join("、")
    )
}

/// 取一个本地路径的**文件名**（最后一段），并且保证它不含路径分隔符。
///
/// 用在两处：给 Java / Go 拼**默认启动命令**（`java -jar app.jar` 要的是
/// 源文件的名字，不是应用名），以及存进本地存档的 `artifact` 字段。
///
/// ⚠️ 它**不**再参与拼 tar 的成员名了（v0.9 的搬运改成 `wslc container cp`，
/// 目录来源用的是 [`copy_members`]，成员名一律取本地路径本身）。
///
/// 返回 `None` 表示这个路径**没有可用的文件名**（以分隔符结尾、是根、或者是空的）。
pub fn artifact_name(source: &Path) -> Option<String> {
    let name = source.file_name()?.to_string_lossy().to_string();
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains('\0') {
        return None;
    }
    Some(name)
}

/// 在容器内的路径上拼一段（只处理 `/`，不碰 Windows 的分隔符）。
///
/// `dir` 是 `/` 或空时拼成 `/name`；否则去掉末尾多余的 `/` 再拼 ——
/// 免得出来 `<dir>//app.log` 这种能跑但难看的路径（而且它会在提示条里显示给用户）。
pub fn container_join(dir: &str, name: &str) -> String {
    let trimmed = dir.trim_end_matches('/');
    if trimmed.is_empty() {
        format!("/{name}")
    } else {
        format!("{trimmed}/{name}")
    }
}
