# 多数据库改造方案（MySQL → 通用数据库管理）

> 目标：把现在"只认 MySQL"的数据库页，改成能管多种数据库的形态。
> 参考实现：[t8y2/dbx](https://github.com/t8y2/dbx)（Apache-2.0，Rust + Tauri）。
>
> **本文只研究机制，不抄代码。** dbx 的许可与本项目兼容（都是 Apache-2.0），
> 但仍然只读它的架构，代码自己写 —— 理由见 `AGENTS.md` §6 那条纪律。

---

## 1. 现状：为什么现在只会 MySQL

一句话：**"连接"这个概念在当前代码里不存在，存在的只有"容器"。**

| 位置 | 现状 |
|---|---|
| `model/mysql.rs` | `MysqlCreds` / `MysqlFamily`（Mysql·Mariadb·Percona）/ `SqlMode` / TSV 切分 |
| `cmd/mysql.rs` | `run_sql(wslc, container_name, client_bin, creds, sql, db, mode)` —— **每个函数都吃 `container_name`** |
| `model/schematree.rs` | 直接写死 `information_schema.TABLES` / `COLUMNS`（MySQL 的元数据表） |
| `model/sqlstmt.rs` | 语句切分（反引号 / `#` 注释 / `--` / `/* */`，都是 MySQL 方言） |
| `wslc-panel-core/state.rs` | `DbState` 挂在容器上：`provisional(container, image, family)` |
| `wslc-panel-core/sqlcomplete.rs` | `Dialect::{MySql, MariaDb}`，关键字写死在两个 `const` 数组里 |
| `views.rs` / `app.rs` | 数据库页只能**从容器行进入**，返回按钮写的是「← 数据库」 |

**所以"支持多数据库"不是加几个 match 分支，而是把"连接"从"容器"里拆出来。**

---

## 2. dbx 是怎么做的（读到的确切事实）

### 2.1 三层分离

```
dbx-types/models/connection.rs   235 KB   DatabaseType 枚举（100+ 变体）+ 每族连接参数
        ↓
dbx-driver-<engine>              6 个 crate 真正的协议实现
        ↓
plugins/dialects/*.yaml          38 个文件  方言 = 纯数据
```

**关键事实（逐个核对过）：**

| 层 | 事实 | 数字 |
|---|---|---|
| 协议驱动 | `dbx-driver-mysql` / `-postgres` / `-mongodb` / `-redis` / `-sqlserver` / `-elasticsearch` + `dbx-sqlite-worker` | **6 个引擎**，不是 100 个 |
| MySQL 驱动本体 | `dbx-driver-mysql/src/mysql.rs` | **456 KB 单文件** |
| 同族兼容垫片 | `dolt` / `doris` / `starrocks` / `tidb` / `oceanbase` / `manticoresearch` / `dolt`… | 每个 2–26 KB，复用同一驱动 |
| 方言 | `plugins/dialects/*.yaml`（access / clickhouse / dameng / duckdb / firebird / oracle / postgresql / sqlserver / sqlite / mysql / …） | **38 个 YAML** |
| 方言运行时 | `dialect_loader` + `hot_reload` + `DialectRegistry::global()`，`resolve(DialectKind)` 取 `DialectCapabilityDescriptor` | 可热加载 |
| 能力位 | `CAP_CREATE_TABLE` / `CAP_RENAME_COLUMN` / `CAP_AUTO_INCREMENT` / `CAP_INDEX_INCLUDE` / `CAP_TRANSACTIONAL_DDL` … | 30+ 个 |
| 共享管道 | `dbx-driver-support`：`ssh_config.rs` **51 KB**、`http_tunnel.rs` **19 KB**、`navicat_mysql_tunnel.rs` **32 KB**、`wkb.rs`（几何）、`ddl_scan.rs` | — |

### 2.2 值得学的三个想法

1. **方言是数据，不是代码。** 每加一个数据库，写一份 YAML（类型映射 + 引号风格 + 分页语法 +
   能力位），**不写 Rust**。这是它能"支持 100+"的真正原因 —— 引擎只有 6 个，方言有 38 份。
2. **能力位驱动 UI，而不是 `if kind == X`。** 界面上"改列名""加索引"这些按钮显不显示，
   查 `CAP_RENAME_COLUMN` 这类布尔位，而不是到处写数据库类型判断。
3. **同族复用同一个驱动，只加一个薄垫片。** TiDB / Doris / StarRocks / OceanBase 都走
   MySQL 协议，各自只加 2–26 KB 的差异处理。

### 2.3 不要照抄的部分（代价远大于收益）

| dbx 的做法 | 我们不抄的理由 |
|---|---|
| YAML 方言热加载 + 插件市场 | 我们是**单体小工具**，不是插件平台。38 个 YAML 意味着 38 份需要维护的方言表，而我们要支持的只有 5–6 种 |
| SSH 隧道 / HTTP 隧道 / Navicat 隧道（共 100 KB） | 那是"连远程服务器数据库"的需求。我们的场景是**本机 WSL 容器**，走不到隧道 |
| `wkb.rs`（几何类型）、`ddl_scan.rs`（DDL 反向解析） | 是给建表向导 / ER 图用的。我们不做建表向导 |
| 456 KB 的 `mysql.rs` | 里面是完整的预处理语句、二进制协议、批量 COPY、流式游标。我们是**面板**，跑的是人手动敲的 SQL |
| JDBC 插件运行时 | 需要 JVM。装不上也用不起 |

---

## 3. 三条路线

| | **A. exec-CLI 驱动**（扩展现有） | **B. 原生协议驱动**（学 dbx） | **C. 混合（推荐）** |
|---|---|---|---|
| 怎么连 | `wslc exec <容器> <客户端二进制> -e "<SQL>"` | 进程内 TCP 连 `容器IP:端口` / `localhost:映射端口` | 先用 A，把 B 接在**同一个 trait** 后面 |
| 新依赖 | **零** | `tokio-postgres` / `mysql_async` / `mongodb` / `rusqlite`（+ TLS + 运行时桥接） | 阶段 1–4 零依赖，阶段 5 再评估 |
| CI 影响 | 无（`ui` 仍 ~15 min 冷缓存） | `ui` 冷缓存显著变长；`Cargo.lock` 每次都要走 `gen-lock` | 前四阶段无影响 |
| 能连什么 | 容器 / 发行版**里有客户端**的库 | 任意可达的库（含远程） | 先覆盖容器，远程留到阶段 5 |
| 失败模式 | 镜像里没有 `psql` → 明确报"这个镜像里没有 psql" | 协议/认证/TLS 细节错 → 难查，且**本地无法验证** | A 的失败模式是好懂的那种 |
| 与项目定位 | **一致**：这个面板一直是 `wslc.exe` / `wsl.exe` 的 GUI（`AGENTS.md` §0） | 会把面板变成"通用 DB 客户端"，与 WSL 容器脱钩 | 保持定位，同时留了升级口 |

### 推荐 C：先把"架构改成多驱动"，第二个引擎用 exec 做

理由按重要性排：

1. **产品定位就是"容器面板"。** 数据库跑在 WSL 容器里，而官方镜像
   （`mysql` / `postgres` / `redis` / `mongo` / `mariadb`）**都自带客户端**。
   我们不需要实现协议就能干活。
2. **零新依赖 = 零 CI 代价。** 本机没有工具链（`AGENTS.md` §1），
   加 `tokio-postgres` 这类依赖意味着每次动 `Cargo.toml` 都要走 `gen-lock`，
   而且 `ui` 那个 15 分钟的 job 会更慢。**这是实打实的开发速度损失。**
3. **原生驱动有个绕不过去的成本：运行时桥接。** GPUI 的
   `background_executor` 不是 tokio；要靠"专用 tokio 线程 + channel"桥。
   多一层就多一类"卡界面"或"死锁"的 bug，而 §7.1 已经证明这类 bug 在这个项目里
   是最贵的一类。
4. **A 的失败模式清楚。** "镜像里没有 `psql`" 是一句话能说清的；协议层面的
   握手失败、认证插件不兼容、TLS 版本不匹配，是用户和我们都查不动的。

**但架构必须按 B 的形状设计**（trait + 连接模型 + 方言数据），
这样阶段 5 加原生驱动时不用推倒重来。

---

## 4. 目标架构

### 4.1 核心概念：连接 ≠ 容器

```rust
/// 一条数据库连接。
pub struct DbConn {
    pub id: String,              // 稳定 id，用于存偏好
    pub name: String,            // 显示名
    pub kind: DbKind,            // 引擎
    pub host: DbHost,            // 从哪儿连
    pub host_addr: String,       // 容器内部地址（127.0.0.1 或容器名）
    pub port: u16,
    pub user: String,
    pub password_ref: String,    // 存哪儿见 §6
    pub database: Option<String>,
    pub ssl: bool,
}

/// 从哪儿发起连接。**这是本项目与 dbx 最大的区别。**
pub enum DbHost {
    Container(String),   // wslc exec <name> <client> …
    Distro(String),      // wsl -d <name> -u …  <client> …
    // 阶段 5 才加：Native，进程内直连
}

pub enum DbKind { MySql, MariaDb, Postgres, Redis, Mongo, Sqlite }
```

`DbKind` 只有 **6 个**（不是 dbx 的 100+）。同族差异（Percona / TiDB / StarRocks）
用 `DbFlavor` 记号位表达，复用同一个驱动 —— 学 dbx 的"薄垫片"做法。

### 4.2 驱动 trait（放在 `wslc-core`，不依赖 UI）

```rust
pub trait SqlDriver: Send + Sync {
    fn kind(&self) -> DbKind;
    /// 从镜像名 / 端口 / 探测输出猜引擎（"从容器一键导入"用）
    fn detect(&self, hint: &ConnHint) -> Option<Confidence>;
    /// 连通性 + 版本
    fn probe(&self, host: &DbHost, conn: &DbConn) -> Result<ServerInfo>;
    /// 执行一段脚本，逐条返回结果集
    fn run_sql(&self, host:&DbHost, conn:&DbConn, sql:&str) -> Result<Vec<SqlResult>>;
    /// 库 / 表 / 列
    fn schema(&self, host:&DbHost, conn:&DbConn) -> Result<SchemaTree>;
    /// 这一族的方言（补全 / 生成 SQL / 分页语法）
    fn dialect(&self) -> &dyn Dialect;
}
```

两点刻意设计：

- **`run_sql` 只吃一段脚本**，切分由调用方（`sqlstmt`）按方言做 —— 因为
  `DELIMITER`、`$$` 这类规则**本身是方言**（PostgreSQL 的 `$$` 函数体、
  MySQL 的 `DELIMITER //`）。所以切分也必须进 `Dialect`。
- **`probe` 返回 `ServerInfo`**（版本 + 实际方言 + 能力位）。能力位决定界面显示什么，
  而不是在 `views.rs` 里写 `if kind == Postgres`。

### 4.3 方言：先做成 Rust 数据，不上 YAML

```rust
pub struct DialectData {
    pub kind: DbKind,
    pub quote: QuoteStyle,          // Backtick | DoubleQuote
    pub limit: LimitSyntax,         // LimitOffset | LimitOnly | FetchFirst | Top
    pub keywords: &'static [&'static str],
    pub comment_styles: &'static [&'static str],   // `--` / `#` / `/* */`
    pub capabilities: CapSet,       // 位标志，见下
}

bitflags::CapSet: CREATE_TABLE | RENAME_COLUMN | DROP_COLUMN | CREATE_INDEX
                | AUTO_INCREMENT | TRANSACTIONAL_DDL | SCHEMAS | MULTI_DATABASE
```

**为什么不上 YAML**（与 dbx 的关键分歧，写清代价）：

- dbx 要 38 种方言，**维护成本靠数据化摊薄**；我们只有 6 种，
  6 份 `const` 结构体的成本远低于"解析器 + 校验器 + 热加载 + 快照测试"；
- 我们**没有**建表向导 / ER 图 / 跨库迁移，能力位主要用来**隐藏按钮**，
  不需要 dbx 那种 30+ 位的表达力；
- YAML 得引入 `serde_yaml`（新依赖）+ 一个加载时机 + 一套"YAML 写错了怎么办"。

**将来真要数据化，`DialectData` 就是那份数据的形状** —— 换成从文件读只改一处。

---

## 5. 分阶段落地

每个阶段都能**单独发布**、都有可查的验收判据。阶段 1 是纯重构，用户看不到任何变化。

### 阶段 0（v0.13.0）抽出 trait，MySQL 变成"第一个实现"

**目标：行为零变化，架构换成多驱动形状。**

- 新增 `wslc-core/src/db/`：`conn.rs`（`DbConn`/`DbHost`/`DbKind`）、
  `driver.rs`（`SqlDriver`）、`dialect.rs`（`DialectData` + `Dialect`）。
- `cmd/mysql.rs` 改造成 `db/mysql_driver.rs`，实现 `SqlDriver`；
  内部逻辑**一行不改**（`run_sql` / `list_schema_tree` 等继续存在，只是被 trait 包一层）。
- `DbState` 增加 `conn: DbConn` 字段，`container`/`family` 降级成"导入来源"。
- `sqlcomplete.rs` 的 `Dialect::{MySql,MariaDb}` 改成读 `DialectData.keywords`。

**验收判据：**
- `core` job 576 + 241 条测试**一条不少**；
- `ui` job 绿，界面上看不到任何差异；
- `grep -r "MysqlFamily" crates/` 的命中数**只减不增**（除兼容层）。

**风险**：这一步动 `app.rs` / `views.rs` 里所有 `db.xxx` 的用法，是"大范围小改动"，
容易漏。缓解：先只加 trait 和 `conn` 字段，旧字段全部保留，下一阶段再删。

### 阶段 1（v0.13.1）连接与容器解耦

**目标：可以手工新建一条连接，不再只能"从容器进"。**

- 数据库页顶部加「新建连接」；表单字段按 `DbKind` 变（端口默认值 / 库名字段 / SSL 开关）。
- 「从容器导入」保留：拿容器的**镜像名 + 发布端口**猜 `DbKind`，
  这就是 `SqlDriver::detect` 的用武之地。
- 连接存在 `prefs.rs` 里（`Vec<DbConn>`）。

**验收判据：**
- 不经过容器、手填 `127.0.0.1:3306` 也能连上并跑 SQL；
- 从容器导入时**猜对了引擎**（列表里能看到识别结果）；
- 删除连接不影响容器本身（这是解耦的核心，要写测试断言）。

### 阶段 2（v0.14.0）第二个引擎：PostgreSQL

**这是真正的验收点 —— 抽象能不能撑住第二个实现。**

- `db/postgres_driver.rs`：`wslc exec <c> psql -A -t -F $'\t' -c "…"`。
  ⚠️ `psql` 的输出格式和 `mysql -B` **不一样**（`-A` 不对齐、`-t` 去表头、
  `-F` 指定分隔符，且**空值输出为空串**、要区分 NULL 得用 `-P null=…`）。
  这些必须在写代码前先用真容器抓一份夹具存进 `tests/fixtures/`（`AGENTS.md` §5
  "测试用真实抓下来的夹具"）。
- 元数据查询换 `pg_catalog`（`pg_namespace` / `pg_class` / `pg_attribute`）。
- 语句切分换规则（`$$` 函数体、`;` 在字符串里、`E'…'` 转义串）。

**验收判据：**
- 能连 `postgres:16` 容器、列出表结构、跑多语句、补全出表名列名；
- `core` job 里 PostgreSQL 的切分 / 解析 / 元数据全部有测试；
- **一行 `if kind == Postgres` 都不出现在 `views.rs` / `app.rs`** ——
  出现了就说明抽象漏了，回头补 trait。

### 阶段 3（v0.14.1）方言数据化收口

- 关键字表 / 引号风格 / 分页语法 / 注释样式全部收进 `DialectData`；
  `sqlcomplete` 与 `sqlstmt` 只读数据，不再 `match DbKind`。
- 界面按钮按能力位显隐（比如 PostgreSQL 没有 `SHOW TABLES`，就不给那个快捷项）。

**验收判据**：新增第三种引擎时，**不需要改 `wslc-panel` 里任何文件**（只有 `wslc-core` 加一个驱动模块 + 一份 `DialectData`）。这条能过，抽象才算成立。

### 阶段 4（v0.15.0）非 SQL：Redis 先行

- Redis 不是表结构，是 key 空间 —— 需要**另一套界面**（key 浏览 + 值查看 +
  命令控制台）。`SqlDriver` 不该硬套，另开 `KeyValueDriver` trait。
- 只做 `redis-cli` 能覆盖的部分；不做集群 / 哨兵。

**验收判据**：能连 `redis:7`、`SCAN` 出 key、看值、发任意命令。

### 阶段 5（可选，不排期）原生驱动，接远程库

只有在"要连 WSL 之外的数据库"这个需求**真的出现**时才做：

- `DbHost::Native` + 专用 tokio 运行时（单独的线程 + channel 桥到
  `background_executor`，见 §7.1 那条纪律）；
- 先只做 `tokio-postgres`（比 `sqlx` 轻），验证桥接没问题再评估 MySQL；
- 那时才需要动 `Cargo.lock`（走 `gen-lock.yml`）。

---

## 6. 具体改动清单（阶段 0 + 1）

| 文件 | 改动 |
|---|---|
| `wslc-core/src/db/mod.rs` | 新：模块出口 |
| `wslc-core/src/db/conn.rs` | 新：`DbConn` / `DbHost` / `DbKind` / `DbFlavor` |
| `wslc-core/src/db/dialect.rs` | 新：`DialectData` / `QuoteStyle` / `LimitSyntax` / `CapSet` |
| `wslc-core/src/db/driver.rs` | 新：`SqlDriver` trait + `ServerInfo` + `Confidence` |
| `wslc-core/src/db/mysql_driver.rs` | 由 `cmd/mysql.rs` 搬来，实现 trait |
| `wslc-core/src/model/schematree.rs` | `schema_tree_sql()` 从硬编码改成"由驱动给" |
| `wslc-core/src/model/sqlstmt.rs` | 切分规则参数化（`SplitRules`，MySQL 是其中一份） |
| `wslc-panel-core/src/state.rs` | `DbState` 加 `conn: DbConn`；`provisional()` 改签名 |
| `wslc-panel-core/src/prefs.rs` | 新：`connections: Vec<DbConn>`（**密码不存这里**，见下） |
| `wslc-panel-core/src/sqlcomplete.rs` | 关键字 / 引号读 `DialectData` |
| `crates/wslc-panel/src/views.rs` | 数据库页加连接列表；按钮按能力位显隐 |
| `crates/wslc-panel/src/app.rs` | `DbConn` 的新建 / 编辑 / 删除 / 导入动作 |

### 密码存哪儿（必须先定）

现在密码在**内存里的表单**（`DbPageForm.password`），不落盘 —— 这是对的。
多连接之后用户会想"存起来下次不用重填"，这就变成安全问题：

- **不建议**明文写进 `prefs.json`（那是 `%APPDATA%` 下的普通文件）；
- Windows 上正确做法是 DPAPI（`CryptProtectData`），但要新依赖 `windows` crate；
- **阶段 1 的保守选择：不存密码**，每次连的时候填（和现在的行为一致）。
  界面上明确写"密码不保存"。等用户真的抱怨了再上 DPAPI。

---

## 7. 代价与风险（说清楚，别到时候惊讶）

1. **`app.rs` / `views.rs` 会再一次大改。** 这两个文件已经 1 万行，
   而 `AGENTS.md` 记录的两次事故（删过头 9 个函数、写出 1071 个空行）
   都发生在这里。**本方案要求先按函数边界做增删，不许再手算行号区间。**
2. **`ui` job 是唯一验证通道，一轮 15 分钟。** 阶段 0 是"大范围小改动"，
   大概率要两三轮才全绿。要把阶段 0 切得更小（比如先只 `DbConn` + trait 定义，
   不接线），减少一次性的改动面积。
3. **每种引擎的输出格式都要抓真夹具。** MySQL 的 `-B` 我们已经踩明白了
   （空结果集和无结果集都是零字节）；`psql -A -t` 的 NULL、转义、
   多语句行为**完全不同**，不抓夹具就是瞎写。
4. **补全会变成"每引擎一套表"。** `sqlcomplete` 现在 30 个关键字 + 2 张数组；
   六种引擎就是六份。这是**必要的**重复（关键字本来就不同），
   但要防止"在补全里 match `DbKind`"—— 应该读 `DialectData`。
5. **不做的事**（明确排除，避免范围膨胀）：建表向导、ER 图、数据迁移、
   备份调度、SSH 隧道、远程库、集群/哨兵、几何类型、二进制大字段的编辑。

---

## 8. 一句话结论

**先把"连接"从"容器"里拆出来，用 trait 把 MySQL 包成第一个实现（行为不变），
再用 PostgreSQL 验证抽象是否成立；两个引擎都通了之后，加第三个引擎应当
只改 `wslc-core`。**

原生协议驱动（dbx 那条路）留成阶段 5 的可选升级项，理由是它会把面板从
"WSL 容器面板"变成"通用 DB 客户端"，并且带来本机无法验证的失败面。
