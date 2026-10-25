//! 数据库域：连接状态、库表用户、备份与导入。

use super::prelude::*;
use crate::util::truncate_chars;

/// 启动时读一次数据库账号存档。
///
/// 拆成自由函数是因为它**碰文件系统**：`AppState::new` 的构造表达式里
/// 塞 IO 会让那个函数没法一眼看完，而且测试也注入不进去。
pub(crate) fn db_credentials_at_startup() -> (crate::dbcreds::DbCredentialStore, Option<String>) {
    crate::dbcreds::DbCredentialStore::load(&crate::dbcreds::store_path(
        crate::prefs::app_dir().unwrap_or_default(),
    ))
}

/// 扫一个备份目录，按**时间倒序**返回（新的在上）。
///
/// 只认 `<库名>-<YYYYMMDD>-<HHMMSS>.sql` 这个形状（面板自己生成的）；
/// 不认识的文件**跳过** —— 目录里可能有用户手动拷进来的东西，报错只会烦人。
///
/// ⚠️ 碰文件系统 —— 只在**点击后**和**长任务完成回调**里调，
/// 绝不在渲染路径上（`AGENTS.md` §7.1）。
pub fn scan_backup_dir(dir: &std::path::Path) -> std::io::Result<Vec<BackupEntry>> {
    let mut out: Vec<BackupEntry> = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some((database, stamp)) = parse_backup_file_name(file_name) else {
            continue;
        };
        let bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
        out.push(BackupEntry {
            container_path: format!(
                "{}/{}",
                wslc_core::model::mysql::CONTAINER_BACKUP_DIR,
                file_name
            ),
            database,
            file_name: file_name.to_owned(),
            path,
            bytes,
            stamp,
        });
    }
    // 文件名里的时间戳是定宽的，字典序 == 时间序
    out.sort_by(|a, b| b.stamp.cmp(&a.stamp));
    Ok(out)
}

/// 「数据库管理」页的 tab。
///
/// ⚠️ **没有「SQL 控制台」这一项**（v0.9.2 去掉的）。实机反馈：
/// "上面不需要导入 sql 和 sql 控制台，都是在具体数据库 database 操作的" ——
/// SQL 是**针对某个库**的事，所以它现在挂在数据库列表每行的「SQL」按钮上、
/// 以一个浮层出现（`Shell::open_db_console_for` + `DbState.sql_database`），
/// 而不是一个"先切 tab、再想连哪个库"的入口。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DbTab {
    /// 数据库列表 + 新建 / 删除。
    #[default]
    Databases,
    /// MySQL 用户列表 + 新建 / 改密码 / 授权 / 删除。
    Users,
}

impl DbTab {
    /// 全部 tab（决定界面上的顺序）。
    pub const ALL: [DbTab; 2] = [Self::Databases, Self::Users];

    /// 标签。
    pub fn label(self) -> &'static str {
        match self {
            Self::Databases => "数据库",
            Self::Users => "用户",
        }
    }
}

/// 一次数据库操作失败：分类 + 原文。
///
/// 分开存是为了界面上能说**两句话**：一句"该怎么办"（分类给的），
/// 一句原文（排障要用，用户可能还想把它贴给别人看）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbFailure {
    /// 分类（决定给什么建议）。
    pub kind: MysqlErrorKind,
    /// 报错原文。⚠️ 到这里的文本**已经脱敏过**（出口在 `cmd::mysql::sanitize`）。
    pub detail: String,
}

impl DbFailure {
    /// 从一段报错文本分类。
    ///
    /// 退出码不传（`None`）：能走到这里的错误都已经被
    /// [`wslc_core::error::Error`] 格式化过，退出码也写在文本里了
    /// （`… 返回退出码 126：…`），而分类里真正稳的判据是那些**文本**。
    pub fn from_text(detail: impl Into<String>) -> Self {
        let detail = detail.into();
        Self {
            kind: wslc_core::model::mysql::classify_error(&detail, None),
            detail,
        }
    }

    /// 给用户的一句话建议（`None` = 直接看原文就行）。
    pub fn hint(&self) -> Option<&'static str> {
        self.kind.hint()
    }

    /// 提示条上那一句。
    ///
    /// 有建议就给建议；没有就给原文的**第一行**并截断 —— 提示条是一条单行横幅，
    /// 整段 `stderr` 塞进去会把界面撑变形。
    pub fn summary(&self) -> String {
        if let Some(hint) = self.hint() {
            return hint.to_owned();
        }
        let first = self.detail.lines().next().unwrap_or_default().trim();
        truncate_chars(first, 120)
    }
}

/// 原生连接时代替「容器里的客户端名」的那个**哨兵值**。
///
/// # 为什么需要一个哨兵而不只是 `Option`
///
/// `DbState.client` 被六条写路径直接塞进 `wslc exec <容器> <客户端>`，
/// 而那些路径在不走原生时必须拿到真的二进制名。给它改成
/// `Option<String>` 会把六处调用点全部改成 `let Some(bin) = … else …`，
/// 而收益只是"少一个不可能撞上的字符串" —— 容器里不会有一个叫
/// `native` 的可执行文件，撞上的概率为零。
///
/// 代价是**读它的地方必须知道这个值有特殊含义** —— 所以判据统一收在
/// [`DbState::is_native`] 里，不要在别处再比字符串。
pub const NATIVE_CLIENT: &str = "native";

/// 用户粘进来的连接串（`mysql://root:pw@10.0.0.5:3306/shop`）。
///
/// # 为什么是 newtype 而不是 `Option<String>`
///
/// `DbState` / `AppState` 都是 `#[derive(Debug)]`，而这个字符串**带口令**。
/// 如果它是裸 `String`，那么任何一次 `tracing::debug!("{:?}", state)`
/// 或者 `dbg!()` 都会把口令打进日志文件 —— 而日志是用户会贴到 issue 里的
/// 东西。这个仓库在密码这件事上一直是同一套做法
/// （[`MysqlCreds`] / `DbCredential` 都手写了脱敏 `Debug`），这里保持一致。
///
/// 用一个 newtype 而不是"给 `DbState` 手写整个 `Debug`"：后者要手写几十个
/// 字段，加一个字段忘了补就会**悄悄丢掉**诊断信息；newtype 只影响这一个字段。
///
/// [`MysqlCreds`]: wslc_core::model::MysqlCreds
#[derive(Clone, PartialEq, Eq)]
pub struct DsnText(String);

impl DsnText {
    pub fn new(text: impl Into<String>) -> Self {
        Self(text.into())
    }

    /// 原文。**只给真正要连接的地方用**（解析 / 拼 `ConnSpec`）。
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 是否为空（空串等于"没填"）。
    pub fn is_empty(&self) -> bool {
        self.0.trim().is_empty()
    }
}

impl std::fmt::Debug for DsnText {
    /// 口令段打 `***`，其余原样保留。
    ///
    /// 和 `MysqlCreds` 那种"整段打 `***`"不同：连接串里的**协议 / 主机 /
    /// 端口 / 库名 / 用户名**正是排障要看的东西（"我连的是哪台机器"），
    /// 遮掉它们等于让这条 Debug 失去价值。只遮口令那一段。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DsnText({})", redact_dsn_password(&self.0))
    }
}

/// 把 `scheme://user:password@host…` 里的 `password` 换成 `***`。
///
/// 判据刻意和 `wslc-db::redact_dsn` **一致**（那边是 `wslc-db` 里唯一
/// 有测试的那份实现，但 `wslc-panel-core` 不依赖那个 crate）：
///
/// - 先找 `://`，没有就原样返回（不像连接串的东西，没什么可遮的）；
/// - 再在**权限段**（`://` 到第一个 `/` 或 `?`）里找**最后一个** `@`；
/// - 那之后如果还有 `:`，`:` 到 `@` 之间就是口令。
///
/// ⚠️ 用**最后一个** `@`：口令里出现未编码的 `@` 很常见
/// （`mysql://root:p@ss@h/db`），按第一个切会把主机名当成口令的一部分。
pub(crate) fn redact_dsn_password(text: &str) -> String {
    let Some(scheme_end) = text.find("://") else {
        return text.to_owned();
    };
    let auth_start = scheme_end + 3;
    // 权限段结束：第一个 `/` 或 `?`（都没有就到末尾）
    let auth_end = text[auth_start..]
        .find(['/', '?'])
        .map(|i| auth_start + i)
        .unwrap_or(text.len());
    let auth = &text[auth_start..auth_end];
    let Some(at) = auth.rfind('@') else {
        // 没有 `@` = 没有 userinfo，也就没有口令
        return text.to_owned();
    };
    let userinfo = &auth[..at];
    let Some(colon) = userinfo.find(':') else {
        // `user@host` 形式：没有口令
        return text.to_owned();
    };
    // `user:` + `***` + `@host…`
    format!(
        "{}{}:***{}",
        &text[..auth_start],
        &userinfo[..colon],
        &text[auth_start + at..]
    )
}

/// 「数据库管理」页的状态。
///
/// 纯数据（一个 GPUI 类型都没有），所以住在 `AppState` 里、也能被单测；
/// 页面上那些输入框是 GPUI 的 `Entity<InputState>`，住在 `Shell` 里。
///
/// ⚠️ **密码在这里**（[`MysqlCreds::password`]）—— 它的 `Debug` 是手写脱敏的，
/// 所以这里派生 `Debug` 是安全的。除此之外密码**哪儿都不存**：
/// 不落盘（`prefs.json` 里没有这个字段）、不进日志、不进错误消息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbState {
    /// 目标容器名。
    pub container: String,
    /// 容器镜像引用（页面上显示，让用户确认点对了容器）。
    pub image: String,
    /// 家族（MySQL / MariaDB / Percona）。
    pub family: MysqlFamily,
    /// 容器里的客户端二进制名（探测得来）。
    ///
    /// ⚠️ 走原生连接时这里是 [`NATIVE_CLIENT`]，**不是**二进制名 ——
    /// 拿它去 `wslc exec` 会报"找不到命令"。用 [`DbState::is_native`] 判。
    pub client: String,
    /// 客户端版本那一行（`mysql  Ver 26.7.0 …`）。
    pub client_version: Option<String>,
    /// 用户粘进来的**连接串**（v0.16）。`Some` 时这一页走原生连接。
    ///
    /// # 为什么是可选的字符串而不是 `ConnSpec`
    ///
    /// `ConnSpec` 在 `wslc-db` 里，而 `wslc-panel-core` 不依赖那个 crate
    /// （它带 `postgres` 那棵树，没必要为了一个字段把整棵依赖拉进数据层）。
    /// 所以状态里存原文，**解析在 `wslc-panel` 里做** —— 解析失败要能
    /// 原样报给用户看（"端口不是合法数字：abc" 比 "连接失败" 有用得多）。
    ///
    /// ⚠️ 它**不落盘**：里面有口令。
    ///
    /// ⚠️ 类型是 [`DsnText`] 而不是 `String`：这个结构体派生了 `Debug`，
    /// 裸 `String` 会把口令打进日志（见那个 newtype 的文档）。
    ///
    /// ⚠️ 它也是 [`DbState::is_native`] 的两个判据之一 —— 填了它，那六条
    /// 只能走容器内客户端的写操作就会被拦住（见 `write_blocked_reason`）。
    pub dsn: Option<DsnText>,
    /// 多驱动层的连接（v0.13）。
    ///
    /// # 现在它是什么
    ///
    /// 一份**镜像**：由 `container` + `family` 推出来（`host` 就是那个容器、
    /// `kind` 由家族换算、`addr`/`port` 取引擎默认值）。
    ///
    /// ⚠️ 早先这里写的是"**还没有代码读它**"—— 那句话**已经过时**：
    /// `DbState::is_mysql_family` / `engine_label` 都读 `conn.kind`，
    /// 而 `db.driver()`（`wslc-panel`）也是从它选驱动的。
    /// 留着那句旧注释会让下一个人以为这是个空字段。
    ///
    /// # 为什么要有它
    ///
    /// 现在"数据库"这个概念在代码里不存在，存在的只有"容器"：`run_sql` 的
    /// 第一个参数就是容器名。要支持第二个引擎（PostgreSQL，v0.14），
    /// 就必须有一个"连接"的落点 —— 而这个落点不能是容器，否则永远只能
    /// "从容器行进入"。方案见 `docs/PLAN-multi-db.md`。
    ///
    /// ⚠️ 别急着把 `container` / `family` 删掉：界面上还在显示它们
    /// （"这是 MySQL 8.0 的容器"这种话对用户是有用的），而且
    /// `guess_kind` 的反向推理也需要镜像名。
    pub conn: DbConn,
    /// 连接凭据 —— **上一次连成功的那一份**。
    ///
    /// ⚠️ **它不是执行入口。** 所有动作（备份 / 恢复 / 导入 / SQL / 删库 / 删用户）
    /// 都必须用连接卡上**此刻**的值，唯一入口是 `Shell::db_creds(cx)`。
    ///
    /// 为什么：这个字段只在 `load_db` 里赋值一次，而「从列表行进备份浮层 /
    /// SQL 控制台」会把行上的账号写进连接卡 —— 于是同一页会出现两套账号。
    /// v0.10.3 之前的实现一半用这个字段、一半用连接卡，结果是"点某行的备份
    /// （卡换成 shop_user）再在同一个浮层点恢复"会用 **root** 去恢复，
    /// 报 Access denied 而浮层上明明写着 shop_user。
    ///
    /// 现在它的用途只剩"诊断 / 显示"（和卡上的值对照），写它的地方是
    /// `load_db` 与 `open_db_backup_dialog`（后者跟着卡一起更新）。
    pub creds: MysqlCreds,
    /// 这份凭据是怎么来的（"已从容器环境变量读出 root 密码"这类说明）。
    pub cred_note: Option<String>,
    /// 正在后台跑（探测 / 读列表 / 执行 SQL）。
    pub busy: bool,
    /// **根本性**的问题（不是 MySQL / 镜像里没客户端 / 容器没运行）。
    /// 有值时页面只显示它 —— 那些空表没有意义。
    pub blocked: Option<DbFailure>,
    /// 最近一次操作的失败。
    pub error: Option<DbFailure>,
    /// 数据库列表。
    pub databases: Vec<DbEntry>,
    /// MySQL 用户列表。
    pub users: Vec<DbUser>,
    /// 用户列表**自己**的失败（多半是权限：`ERROR 1142` 读不到 `mysql.user`）。
    ///
    /// # 为什么不复用 `error`
    ///
    /// 实机反馈（v0.16.0）：用普通账号进数据库页时，「用户」这一条查询被拒，
    /// 结果整页顶上挂着一条红色横幅 —— 而「数据库」tab 完全正常、表都列出来了。
    /// 那种画法会让人以为**整个页面坏了**，于是不敢用（明明能用）。
    ///
    /// 所以用户列表的失败只画在用户 tab 里：那是它自己的事。
    pub users_error: Option<DbFailure>,
    /// 成功读过一次没有？
    ///
    /// 用来区分「还没读」和「读到了，确实是空的」—— 两者在界面上是两句话。
    pub loaded: bool,
    /// 当前 tab。
    pub tab: DbTab,
    /// SQL 控制台：上一次执行的语句（回显用）。
    pub last_sql: String,
    /// SQL 控制台的结果：**每条语句一个**（v0.11 起）。
    ///
    /// # 为什么是 `Vec` 而不是一个 `Option<String>`
    ///
    /// 用户可能一次贴进多条语句（`SELECT ...; UPDATE ...; SELECT ...`）。
    /// 一次交给 `mysql -e` 的话多个结果集**直接粘连**（实测，见
    /// `wslc_core::model::sqlstmt` 的模块文档），分不清哪行属于哪条 ——
    /// 所以现在是**客户端切分 + 逐条执行**（`cmd::mysql::run_statements`），
    /// 每条产出一个 [`SqlResult`]，界面上按 tab 分开看。
    pub sql_results: Vec<SqlResult>,
    /// SQL 控制台的**原始** `--table` 输出（v0.11 之前唯一的形态）。
    ///
    /// 留着当"看原始输出"的兜底：排查列宽异常 / 字符集问题时，
    /// mysql 原样打了什么比任何解析结果都有说服力。
    pub sql_raw: Option<String>,
    /// 执行的语句**报错**时的原文（`None` = 没报错）。
    ///
    /// 和 `sql_raw` 分开：错误不是"结果"，它的渲染是红字 + 原样保留换行，
    /// 而结果表格是另一套。
    pub sql_error: Option<String>,
    /// 上一次执行一共花了多少毫秒（页脚那行"· 0.08 s"用）。
    ///
    /// 含 `wslc exec` 的固定开销（实测每条约 70ms）—— 所以它**不是**服务端
    /// 的查询耗时，是"用户从点下到看见结果"的时间。页脚里不说"查询耗时"，
    /// 只说数字加单位，免得被误读。
    pub sql_elapsed_ms: u64,
    /// SQL 控制台跑过没有（区分"还没跑过"和"跑完了没有输出"）。
    pub sql_ran: bool,
    /// SQL 控制台**针对哪个库**（`None` = 不指定，靠语句自己 `USE`）。
    ///
    /// 由数据库列表行上的「SQL」按钮设置（那一行是哪个库就是哪个），
    /// 也可以由控制台里的「不指定」按钮清掉。执行时作为 `mysql -D <库>` 传下去
    /// —— 不指定的话 `SELECT * FROM t` 会报 "No database selected"（实测过）。
    pub sql_database: Option<String>,
    /// SQL 控制台左侧的「库 / 表树」（v0.11.2）。
    ///
    /// # 什么时候读
    ///
    /// 打开控制台时读一次（一条 SQL 拿全，实测 118 行 / 0.08s）。
    /// **不**在每次执行 SQL 后重读 —— 那要多花一次 exec，而用户
    /// 大部分时候并不在改结构。改了 DDL 之后重开控制台就会刷新。
    ///
    /// 读失败（比如账号没权限看 `information_schema`）时是空树，
    /// 界面上就说一句"读不到表结构"，而不是画一个空白的树。
    pub schema_tree: wslc_core::model::schematree::SchemaTree,
    // ⚠️ 这里**不该**再挂一份账号存档的克隆。v0.8 挂过（`credentials`），
    // 而它从来没被同步过 —— 构造函数给的是空的，渲染却读它，于是
    // "建库同时建用户"明明成功（SQL 成功、`db-credentials.json` 也写了）、
    // 列表里那两列却永远是 `—`（实机截图抓到的）。
    // 现在渲染直接用 `AppState::db_credentials`（唯一数据源）。
    /// 正在备份的库（`None` = 空闲）。
    ///
    /// 备份是**长任务**（大库可能几分钟）：照 `ExportProgress` 的样子做 ——
    /// 状态在这里、取消令牌在 `Shell`、界面把按钮换成"备份中…"。
    pub backup: Option<BackupState>,
    /// 备份列表（**Windows 侧**目录里扫出来的）。
    ///
    /// 进页面 / 备份完成 / 删除后重扫。**纯本地目录扫描** —— 不碰容器、不起进程，
    /// 但会 `read_dir` + 每条一次 `metadata()`，所以扫描本身要在**后台执行器**上
    /// 做，再由 [`AppState::apply_backups`] 填进来；只有点击路径
    /// （`open_db_backup_dialog`）才可以同步调 [`AppState::rescan_backups`]。
    pub backups: Vec<BackupEntry>,
    /// 备份列表浮层开没开（开的对象是哪个库；`None` = 关着）。
    pub backup_dialog: Option<String>,
    /// 正在**导入**的 `.sql`（`None` = 空闲）。
    ///
    /// 和备份一样是长任务（大 dump 要几分钟），所以状态放这里、界面把它显示成
    /// 一个进度浮层 —— "点下去到有结果之间界面上必须有变化"（`AGENTS.md` §7.1）。
    ///
    /// ⚠️ 这个浮层**必须有关闭途径**（见 `views::db_import_progress`）：
    /// 导入跑在后台，关掉浮层不影响它继续跑；而没有关闭按钮的话，
    /// 一旦 `container cp` 卡住（容器停了 / 大文件），遮罩会挡住整页，
    /// 用户连"去检查容器"都做不了。
    pub import: Option<ImportState>,
}

/// 一次导入的状态（跑着的那一个）。
///
/// 「导入」= 用户挑一个本地 `.sql` → 送进容器 → `mysql` 读它。
/// 和**恢复**走的是同一条路（`cmd::mysql::load_sql_file`），区别只在
/// `database`：恢复是 `None`（dump 自带 `CREATE DATABASE`），
/// 导入多半要指定库（用户的 dump 常常没有 `USE`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportState {
    /// 文件名（显示用；完整路径太长，弹窗里已经看过一遍了）。
    pub file_name: String,
    /// 往哪个库导（`None` = 由文件里的语句决定）。
    pub database: Option<String>,
    /// 用户点过「知道了」把浮层收起来了。
    ///
    /// ⚠️ **收起来不等于取消**：导入还在后台跑（`container cp` + `mysql <`），
    /// 完成后照样发提示、照样重读列表。这个字段只决定**画不画那个遮罩**。
    ///
    /// 为什么必须有它：浮层带 `.occlude()`，没有关闭途径时一旦卡住
    /// （容器停了 / 几百 MB 的 dump），整页点不动，用户连"去容器页看看"
    /// 都做不了 —— 那是比"看不到进度"更糟的状态。
    ///
    /// 它放在 `ImportState` 里（而不是 `Option` 置 `None`）：`Option` 同时是
    /// **互斥锁**（`start_db_import` 第一句就查它），置 `None` 会放开"再来一份
    /// 导入"的口子。
    pub dismissed: bool,
}

/// 一次备份的状态（跑着的那一个）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupState {
    /// 在备份哪个库。
    pub database: String,
    /// dump 生成在**容器里**的路径（备份那一步的产物）。
    ///
    /// ⚠️ 恢复**不用**它（v0.8.2 起恢复从 Windows 存档重新送一份进去）——
    /// 留着是为了「删除备份」时把容器里这份也清掉。
    pub container_path: String,
    /// 备份文件在 **Windows** 侧的路径（完成后展示 / 打开位置）。
    pub windows_path: std::path::PathBuf,
    /// 最近一条 mysqldump 的 stderr（警告类消息，比如 GTID 那条）。
    ///
    /// 这一版是 `None`：输出直接落在容器内（见 `cmd::mysql` 的模块注释），
    /// 字段留着是给"边 dump 边传"那条路用的。
    pub last_message: Option<String>,
    /// 用户点过「放到后台」把浮层收起来了。
    ///
    /// 和 [`ImportState::dismissed`] 同一个理由：浮层带遮罩，而备份是分钟级的；
    /// 收起来不等于取消（备份没有取消能力，见 `Shell::cancel_db_backup`）。
    pub dismissed: bool,
}

/// 一条备份记录（**Windows 侧**目录里扫出来的文件）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupEntry {
    /// 备份的库（从文件名解出来：`djxm-20261015-035901.sql` → `djxm`）。
    pub database: String,
    /// 文件名（不含目录）。
    pub file_name: String,
    /// Windows 侧完整路径。
    pub path: std::path::PathBuf,
    /// 文件大小（字节）。
    pub bytes: u64,
    /// 备份时间（从文件名解出来的 `YYYYMMDD-HHMMSS`）。
    pub stamp: String,
    /// **容器内**那份备份的路径（备份时 dump 的产地）。
    ///
    /// 由文件名推出来（[`wslc_core::model::mysql::CONTAINER_BACKUP_DIR`] +
    /// 文件名），**不一定还在**（容器重建 / `/tmp` 被清就会消失）。
    /// 只用于「删除备份」时顺手清掉它 —— 恢复不依赖这个字段。
    pub container_path: String,
}

/// 从备份文件名解出 `(库名, 时间戳)`；不像的文件名返回 `None`（跳过）。
///
/// 目录里可能躺着用户手动拷进来的东西 —— 不认识的**跳过**而不是报错。
/// 把界面上的凭据（`MysqlCreds`）转成驱动认的形状（v0.14）。
///
/// 多驱动层只认"用户名 + 口令"两样（见 `db::driver::Credentials`），
/// 而连接卡上那份 `MysqlCreds` 还带着 `host`/`port`（走 socket 时都是 `None`）。
/// 转换点集中在这一个函数，将来连接卡换成通用形状时只改这里。
pub fn to_credentials(creds: &MysqlCreds) -> Credentials {
    Credentials::new(creds.user.clone(), creds.password.clone())
}
pub fn parse_backup_file_name(file_name: &str) -> Option<(String, String)> {
    let raw = file_name.strip_suffix(".sql")?;
    // 形如 `djxm-20261015-035901`：库自身也可能带 `-`，所以从**右边**找两段
    let (head, stamp2) = raw.rsplit_once('-')?;
    let (database, stamp1) = head.rsplit_once('-')?;
    if database.is_empty()
        || stamp1.len() != 8
        || stamp2.len() != 6
        || !stamp1.chars().all(|c| c.is_ascii_digit())
        || !stamp2.chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    Some((database.to_owned(), format!("{stamp1}-{stamp2}")))
}

impl DbState {
    /// 这个数据库该用哪个驱动（v0.14）。
    ///
    /// 返回 `Arc<dyn SqlDriver>` 而不是 `&dyn`：UI 那边要把它**搬进**
    /// `background_executor` 的闭包里（起进程必须离开界面线程，
    /// 见 `AGENTS.md` §7.1），所以需要 `'static + Send + Sync`。
    ///
    /// 每次调用新建一个（都是无状态的单元结构体，代价可以忽略）。
    /// MySQL 家族的驱动要带上**探测到的**客户端名 —— 镜像里可能叫
    /// `mariadb` 或 `mysql`，用错名字就是"命令不存在"。
    pub fn driver(&self) -> std::sync::Arc<dyn SqlDriver> {
        match self.conn.kind {
            DbKind::MySql | DbKind::MariaDb => std::sync::Arc::new(MySqlDriver::with_client(
                self.conn.kind,
                self.client.clone(),
            )),
            DbKind::Postgres => std::sync::Arc::new(PostgresDriver::new()),
        }
    }

    /// 是不是 MySQL 家族。
    ///
    /// 「数据库 / 用户 / 备份恢复」那几块**只有 MySQL 家族支持**（它们依赖
    /// `SHOW DATABASES`、`mysql.user`、`mysqldump`）。界面用这个判断
    /// 该不该画那些入口 —— 而不是画出来再让用户点进去看报错。
    pub fn is_mysql_family(&self) -> bool {
        !matches!(self.conn.kind, DbKind::Postgres)
    }

    /// 界面上"这是哪种库"的那句话。
    ///
    /// MySQL 家族走 `family.label()`（能显示 Percona 这种细分），
    /// PostgreSQL 走 `DbKind::label()`。
    ///
    /// ⚠️ **不要**直接用 `family.label()`：`kind == Postgres` 时那个字段
    /// 是占位值，会显示成 "MySQL"。
    pub fn engine_label(&self) -> &'static str {
        match self.conn.kind {
            DbKind::MySql | DbKind::MariaDb => self.family.label(),
            DbKind::Postgres => DbKind::Postgres.label(),
        }
    }
    /// 页面刚打开、还没探测完时的状态。
    ///
    /// `family` 来自**列表那条启发式**（镜像名 / 命令 / 3306 端口），
    /// 客户端名先用家族的兜底值 —— 这样页面立刻就能画出来，
    /// 而权威判定（`inspect` + 客户端探测）回来后由 [`Self::apply_probe`] 覆盖。
    pub fn provisional(
        container: impl Into<String>,
        image: impl Into<String>,
        family: MysqlFamily,
    ) -> Self {
        let kind = DbKind::from_family(family.label());
        let mut state = Self::provisional_kind(container, image, kind);
        // ⚠️ `from_family` 把 Percona 归到 `MySql`（协议相同），但界面上要显示
        // "Percona" 这个细分 —— 所以家族信息在这里补回去，别让抽象把显示变糙。
        state.family = family;
        state.client = family.fallback_client().to_owned();
        state
    }

    /// 按**引擎**建初始状态（v0.14，多驱动）。
    ///
    /// 和 [`Self::provisional`] 的区别：这个直接吃 `DbKind`，所以能表达
    /// PostgreSQL —— 而 `MysqlFamily` 表达不了。`provisional` 保留原签名
    /// 是为了不动现有调用点，它转调这个函数。
    ///
    /// ⚠️ `family` 字段在 `kind == Postgres` 时是**占位值**，没有意义。
    /// 要显示"这是哪种库"请用 [`Self::engine_label`]，它会按 kind 分流。
    pub fn provisional_kind(
        container: impl Into<String>,
        image: impl Into<String>,
        kind: DbKind,
    ) -> Self {
        // `conn` 要复用容器名，先落成 String（`impl Into<String>` 只能消费一次）。
        let container: String = container.into();
        let conn = DbConn::with_defaults(kind, DbHost::Container(container.clone()));
        let family = match kind {
            DbKind::MySql => MysqlFamily::Mysql,
            DbKind::MariaDb => MysqlFamily::Mariadb,
            // 占位：见上面那段注释。
            DbKind::Postgres => MysqlFamily::Mysql,
        };
        let client = match kind {
            DbKind::Postgres => kind.client_bin().to_owned(),
            _ => family.fallback_client().to_owned(),
        };
        Self {
            dsn: None,
            conn,
            container,
            image: image.into(),
            family,
            client,
            client_version: None,
            creds: MysqlCreds::root(""),
            cred_note: None,
            // ⚠️ **必须 false**。这里一开始写的是 `true`（想着"马上就要去探测，
            // 先算忙"），结果 `Shell::load_db` 的第一句就是"正在跑就不叠第二次"
            // —— 于是**第一次**加载被自己挡掉了，页面永远停在「正在读取…」，
            // 页头按钮永远是「读取中…」，实机（v0.8.0）就是这样。
            //
            // 「忙」的语义只能是"**已经有一个后台任务在跑**"：新建出来的状态
            // 没有任务在跑，忙不忙要由真正起任务的那个函数（`load_db`）来置。
            // 页面在 `load_db` 置位**之前**不会渲染一帧（同一个事件回调里），
            // 所以不需要靠这个标志来撑住"读取中"的样子。
            busy: false,
            blocked: None,
            error: None,
            databases: Vec::new(),
            users: Vec::new(),
            users_error: None,
            loaded: false,
            tab: DbTab::default(),
            last_sql: String::new(),
            sql_results: Vec::new(),
            sql_raw: None,
            sql_error: None,
            sql_elapsed_ms: 0,
            sql_ran: false,
            sql_database: None,
            schema_tree: wslc_core::model::schematree::SchemaTree::default(),
            backup: None,
            backups: Vec::new(),
            backup_dialog: None,
            import: None,
        }
    }

    /// 一次探测之后建初始状态。
    ///
    /// root 密码如果写在容器环境变量里，**直接填好** —— 那是用户自己在
    /// `-e MYSQL_ROOT_PASSWORD=…` 里给的，再让他手抄一遍没有意义。
    /// 其它情况（空密码 / 随机密码 / 什么都没有）只给一句说明。
    pub fn from_probe(probe: &wslc_core::cmd::mysql::MysqlProbe, image: impl Into<String>) -> Self {
        let mut state = Self::provisional(probe.container.clone(), image, probe.family);
        state.apply_probe(probe);
        state
    }

    /// 探测结果回来了：覆盖字段，但**绝不覆盖用户已经填好的密码**。
    ///
    /// 那条判断（"密码为空 且 用户名还是默认的 root"）是刻意的：
    /// 用户可能刚在连接卡上换成 `app` 账号并留空密码（那是这个账号真的没密码），
    /// 这时把 root 的密码塞进去会**连错账号**。
    pub fn apply_probe(&mut self, probe: &wslc_core::cmd::mysql::MysqlProbe) {
        self.family = probe.family;
        self.client = probe.client.bin.clone();
        self.client_version = probe.client.version.clone();
        if self.creds.password.is_empty() && self.creds.user == "root" {
            if let RootCredHint::Password(password) = &probe.root_hint {
                self.creds.password = password.clone();
            }
        }
        // 说明只写一次（用户自己改过账号之后就别再提环境变量了）
        if self.cred_note.is_none() {
            self.cred_note = probe.root_hint.note();
        }
        self.busy = false;
    }

    /// 这一页现在走的是**原生连接**吗（`wslc-db`，不经过容器里的客户端）。
    ///
    /// # 为什么看两处
    ///
    /// - `dsn.is_some()`：用户已经点了「应用连接串」，执行路径已经改了；
    /// - `client == NATIVE_CLIENT`：探测回来、`apply_probe` 写进去的那个哨兵。
    ///
    /// 只看 `client` 会漏掉"刚点应用、还没读回来"那一小段（那时 `client`
    /// 还是上一个容器的 `mysql`）；只看 `dsn` 则对"探测说原生、但 dsn 被
    /// 别处清掉"这种不一致不设防。两个都看，任何一边为真就算原生 ——
    /// 对这个判据来说，**宁可多拦一次，也别让一条命令去容器里找一个叫
    /// `native` 的可执行文件**。
    pub fn is_native(&self) -> bool {
        self.dsn.is_some() || self.client == NATIVE_CLIENT
    }

    /// 这一页**不能**做哪些写操作；能全做时返回 `None`。
    ///
    /// # 为什么需要这条判据
    ///
    /// 备份 / 恢复 / 导入 / 建库建用户 / 删库 / 删用户这六条，走的都是
    /// `wslc exec <容器> <客户端> …` —— 它们要的是**容器里的**
    /// `mysql` / `mysqldump`。而原生连接根本不经过容器（可以连一台
    /// 远程机器），`db.client` 那时是 [`NATIVE_CLIENT`] 这个哨兵值。
    ///
    /// 不拦的后果不是"报错"，而是**报一个指错方向的错**：用户点「备份」，
    /// 拿到的是 `exec: native: not found` 之类，看上去像容器坏了 ——
    /// 而真正的原因是这条路径压根没接原生驱动。
    ///
    /// ⚠️ 判据放这里而不是渲染层：渲染层决定"按钮画不画"，而**点下去的
    /// 那一刻**才是真正要判的地方（浮层可能是几分钟前打开的，那期间
    /// 用户可能刚应用了一条连接串）。
    pub fn write_blocked_reason(&self) -> Option<&'static str> {
        if self.is_native() {
            // ⚠️ 保持**一句话**（40 字级）。这段文案出现在两个 tab 的顶部，
            // 而它要说的其实只有"现在走原生了，这几条用不了，清空连接串即可"。
            //
            // 早先写的是 80+ 字的三行解释（还列举了 mysql / mysqldump、
            // 「SQL 控制台不受影响」）—— 那类细节该在用户**真的卡住**时才出现，
            // 不该每次进页面都铺一遍。正式软件（DataGrip / TablePlus）在这种
            // 位置都只有一行提示 + 一个动作入口。
            Some("原生连接不支持备份、导入、建库等操作，清空连接串即可恢复。")
        } else {
            None
        }
    }

    /// 记一次失败（分类 + 原文都留着）。
    pub fn fail(&mut self, error: impl std::fmt::Display) {
        self.error = Some(DbFailure::from_text(error.to_string()));
    }

    /// 记一条**用户列表自己**的失败（≠ 整页失败）。
    ///
    /// 加这个方法而不是让调用方直接赋值，是为了把"它绝不污染整页 `error`"
    /// 这条不变式写在一个**能跑测试**的地方 —— `wslc-panel` 不在任何测试目标里，
    /// 写在那边的断言永远不会执行（`AGENTS.md` §2）。
    ///
    /// 背景：v0.16.0 实机里，普通账号读 `mysql.user` 被拒（`ERROR 1142`），
    /// 结果整页顶上挂着红横幅，而「数据库」tab 明明好好地列着表。
    pub fn fail_users(&mut self, error: impl std::fmt::Display) {
        self.users_error = Some(DbFailure::from_text(error.to_string()));
    }

    /// 清掉上一次的错误（每次动作开始前调）。
    pub fn clear_error(&mut self) {
        self.error = None;
        self.users_error = None;
    }

    /// 页面能不能真的干活：探测通过、而且没有根本性问题。
    pub fn usable(&self) -> bool {
        self.blocked.is_none()
    }

    /// 用户 tab 实际要画哪些账号。
    ///
    /// **系统账号（`mysql.*` / `mariadb.sys`）和 root 永远不返回** ——
    /// 它们是安装时建的，日常管业务账号时只会占地方，root 的
    /// 「改密码 / 删除」误点代价也大。要看它们就用「SQL」控制台。
    ///
    /// 判定在 [`DbUser::is_hidden_by_default`] 里，只有一份。
    pub fn visible_users(&self) -> impl Iterator<Item = &DbUser> + '_ {
        self.users
            .iter()
            .filter(|user| !user.is_hidden_by_default())
    }

    /// 「授权到哪个库」那个下拉框里的候选（**业务库，按名字排序**）。
    ///
    /// # 为什么排除系统库
    ///
    /// 和「数据库」tab 不显示它们同一个理由，但这里更要紧：`GRANT ALL ON mysql.*`
    /// 差不多等于把一个账号提成 root（`mysql.user` 就在那个库里），而下拉框是
    /// **选一个就生效**的控件 —— 误选的代价太大。真要授给系统库，旁边那个输入框
    /// 还能手打（下拉框只是快捷方式，见 `views::grant_db_field`）。
    ///
    /// # 为什么排序
    ///
    /// 列表来自一条 `ORDER BY SCHEMA_NAME` 的 SQL，本来就是有序的；这里再排一次
    /// 是为了让"候选顺序"不依赖那条 SQL —— 将来换个查询、或者在别处填
    /// `databases`，下拉框的顺序都不该跟着变（下拉框的顺序变了，用户凭位置
    /// 点选就会点错）。
    pub fn grant_database_options(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .databases
            .iter()
            .filter(|entry| !entry.is_system())
            .map(|entry| entry.name.clone())
            .collect();
        names.sort();
        names
    }
}
