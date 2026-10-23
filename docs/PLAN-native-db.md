# 原生连接改造方案（exec-CLI → 进程内协议驱动）

> 这份文档**推翻**了 `PLAN-multi-db.md` §3 的路线选择。
> 原因不是原来的判断错了，而是**需求变了**：用户要的是「通用数据库工具 +
> 能连远程数据库」，而 exec-CLI 在结构上到不了远程库（没有容器可以 `exec`）。
>
> 参考实现：[t8y2/dbx](https://github.com/t8y2/dbx)（Apache-2.0，只读架构）。

---

## 1. 为什么必须换

| 能力 | exec-CLI（现状） | 原生驱动（目标） |
|---|---|---|
| 连容器里的库 | ✅ | ✅ |
| **连远程库（云数据库 / 另一台机器）** | ❌ **结构上不可能** | ✅ |
| 不依赖镜像里有客户端 | ❌（`psql` 不在 slim 镜像里就没戏） | ✅ |
| 备份/恢复/用户管理 | 靠 `mysqldump` / `mysql.user` | 靠协议，各引擎自己实现 |
| 连接池、超时、TLS | 只能靠命令行参数 | 可控 |
| 与「WSL 容器面板」的定位 | 紧耦合 | 解耦（但容器仍是主要入口之一） |

结论：**exec 那条路留作"快速看一眼容器里的库"的兜底，主路径换成原生驱动。**

---

## 2. 选型：同步驱动，不是 sqlx

| 候选 | 判断 |
|---|---|
| **`mysql` 28 + `postgres` 0.19（同步，纯 Rust）** | ✅ **选它** |
| `sqlx` 0.9 | ❌ 全异步，要桥 tokio；而且它最大的卖点是编译期校验宏，我们用不上 |
| `tokio-postgres` + `mysql_async` | ❌ 同样是异步，要桥 |

### 为什么"同步"是关键优势

这个面板的既有铁律（`AGENTS.md` §7.1）：**所有阻塞调用必须跑在
`background_executor` 上**。同步驱动天然符合这个模型：

```text
界面线程 ──spawn──▶ background_executor 线程 ──block_on 查询──▶ TCP
```

如果换成 `sqlx`/`tokio-postgres`，就得在 GPUI 后面塞一个 tokio runtime
并用 channel 桥接 —— 多一层就多一类"卡界面 / 死锁"的 bug，而这正是
本项目历史上最贵的一类缺陷。

### 版本与口径（实测于 crates.io）

| crate | 版本 | MSRV | 备注 |
|---|---|---|---|
| `mysql` | 28.0.4 | 未声明 | 特性里有 `minimal` / `rustls-tls` / `native-tls` |
| `postgres` | 0.19.14 | **1.85** | 和 `wslc-panel` 的 `rust-version` 一致 |

⚠️ `postgres` 的 MSRV 是 **1.85**，所以驱动**不能**放进 `wslc-core`
（那个 crate 的 MSRV 是 1.75，是既有承诺）。**新建 `wslc-db` crate**（1.85）。

---

## 3. 架构

```
wslc-db        ← 新 crate（edition 2024, rust-version 1.85）
  ├ conn.rs       ConnSpec：host / port / user / password / database / tls
  ├ value.rs      Cell 文本化（各驱动的 Value → Option<String>）
  ├ session.rs    trait Session { query_text(sql) -> Vec<TextResult> }
  ├ mysql_impl.rs
  └ pg_impl.rs
        ↑
wslc-panel-core ── 只依赖 wslc-db 的**类型**（ConnSpec / TextResult）
        ↑
wslc-panel      ── 界面；连接的打开/查询都在 background_executor 上
```

### 为什么结果只要「文本」

面板不打算做「按类型排序 / 编辑器里改二进制值」这类事（dbx 有，但那是
一个完整的数据库客户端，我们是一个面板）。所以：

- 每格都是 `Option<String>`（`None` = NULL）；
- 和既有决定一致：**NULL 与空串不区分**（列里用 `None`/`Some("")` 记着，
  界面上都画成空）。

这样两个驱动都能给出一致形状，**不用为每个引擎写一套类型映射**。

### 一个关键的 API 选择（PostgreSQL）

`postgres::Client::simple_query()` 返回的就是**文本**
（`SimpleQueryRow::get(i) -> Option<&str>`）＋ `CommandComplete(tag)`：

- 不需要类型解码 → 任意列类型都能显示；
- 一次调用可以跑多条语句 → 省掉"逐条"的固定开销（那是 exec 时代的妥协）；
- `CommandComplete` 正好当"这条是 DDL/INSERT，没有结果集"的判据。

MySQL 侧对应的是 `Conn::query_iter()` + 把 `mysql::Value` 自己格式化成文本。

---

## 4. 分阶段

### 阶段 A（v0.16.0）新建 `wslc-db` + 打通 PostgreSQL 原生连接

- `ConnSpec` + DSN 拼装（纯函数，有测试）
- `PgSession`：连接、`simple_query`、错误分类
- 界面上：连接表单支持 **host / port**（这就是"连远程库"的入口），
  并且 PostgreSQL 容器仍能从容器行一键带出参数
- 验收：本机连容器（`127.0.0.1:5433`）跑 SQL；**再连一个非本机地址**验证

### 阶段 B（v0.16.1）MySQL 原生连接

- `MySqlSession`：`Value` → 文本、错误码分类
- 与现有 `-B` 解析出来的结果对齐（同一张表在两种路径下显示一致）

### 阶段 C 连接管理（真正的"通用工具"）

- 连接列表持久化（`prefs.rs`），不再是"从容器进"的一次性页面
- 密码：**先不落盘**；后续用 Windows DPAPI
- 最近使用、复制连接、测试连接

### 阶段 D 补齐引擎

SQLite（`rusqlite`，同步，几乎零成本）→ Redis（协议简单，可手写 RESP）
→ 更多。方言表已经在 `wslc-core::db::dialect`，加引擎主要是加驱动。

### 阶段 E 可选：保留 exec 兜底

容器里 `exec` 一条命令看结果的路径保留，作为「原生连不上但容器活着」的
诊断手段。**不删**，但不再是主路径。

---

## 5. 代价（说清楚）

1. **CI 变慢。** `mysql` + `postgres` 会带来可观的新依赖树（TLS、加密、解析）。
   `ui` job 冷缓存从 ~15 分钟会更长；`core` job 也不小。
   缓解：驱动放 `wslc-db`，`wslc-panel-core` 只依赖它的类型，不让 GPUI 那棵树变胖。
2. **每次动 `Cargo.toml` 都要走 `ci/gen-lock`。** 本机没有 cargo，锁文件只能
   由 CI 生成（`gen-lock.yml` 用 `cargo metadata` 做最小更新）。
3. **本机无法编译验证。** 驱动 API 写错只能靠 CI 一轮一轮纠 —— 这是这个仓库
   的既定工作方式，不是这次引入的新问题。
4. **MySQL 8 的认证。** 默认 `caching_sha2_password` 在**非 localhost** 上
   需要 TLS 或服务端公钥交换。远程连 MySQL 8 很可能第一步就撞认证，
   所以 `mysql` 的 `rustls-tls` 特性必须开，且要能报出"需要 TLS"这类明确错误。
5. **备份/恢复要重写。** 现在的备份是 `mysqldump` 到容器里再拷出来，
   原生驱动之后得换成"流式读 + 本地写文件"，这是一整块新代码。

---

## 6. 与现有代码的关系

| 现状 | 怎么处理 |
|---|---|
| `wslc-core/src/db/`（DbKind / DbHost / DialectData） | **保留**。`DbHost` 增加 `Remote` 变体，`DbKind` 不变 |
| `cmd/mysql.rs`（exec 路径） | 保留，降级为兜底 |
| `cmd/postgres.rs`（exec 路径） | 同上；新驱动跑通后可以删，但先留着对照 |
| `model/postgres.rs`（CSV 解析） | 原生驱动用不上（`simple_query` 直接给文本），但**它的测试是有价值的**：用来验证"两条路径给的结果一致" |
| `sqlcomplete` / `dialect` | 完全复用，不依赖连接方式 |

---

## 7. 一句话

**把连接从"exec 一个容器"变成"打开一条 TCP"，其余（对象树、补全、结果表、
方言）一条都不用改** —— 这也是为什么上一版把方言和驱动抽象先做掉了：
那些工作在这一版里全部复用，不是白做。
