# PLAN v0.7 —— 容器里的 MySQL 数据库管理

> 这一版的主题是**"面板不只管容器，还管容器里的那套数据库"**。
> 起点是一个很具体的需求：认得出是 MySQL 的容器，在列表行上多一个「数据库」
> 按钮，点进去能建库、看库、管用户。
>
> 全部结论都是**本机实测**出来的（`wslc 3.0.3.0` + `mysql 26.7.0`），
> 不是照文档推的 —— 下面每条后面都写着怎么验的。

---

## 1. 一条实测表（方案的地基）

| # | 试的 | 结果 |
|---|---|---|
| 1 | `wslc exec --help` | 有 **`-e/--env`**、`-u/--user`、`-w/--workdir`、`-i/-t` |
| 2 | `wslc exec -e MYSQL_PWD=root mysql mysql -uroot -N -B -e "SHOW DATABASES"` | exit 0，4 行 TSV |
| 3 | 镜像里的客户端 | `/usr/bin/mysql`（`Ver 26.7.0`），另有 `/usr/bin/mysqlsh` |
| 4 | `wslc inspect mysql` 的 `Config.Env` | 有 `MYSQL_ROOT_PASSWORD=root` → **root 密码能直接读出来** |
| 5 | 同一个 inspect 的 `Config.ExposedPorts` | **是空的** → 端口只能看 `list` 的 `Labels` 元数据（那里有 `ContainerPort: 3306`） |
| 6 | `wslc list` 的 `Command` 字段 | 被**截断**成 20 字符（`"docker-entrypoint.s…"`）→ 想靠 `mysqld` 认人不可靠 |
| 7 | 密码错 | exit 1，`ERROR 1045 (28000): Access denied for user …` |
| 8 | 容器没运行就 exec | exit 1，`容器“…”未运行。错误代码： WSLC_E_CONTAINER_NOT_RUNNING` |
| 9 | 镜像里没客户端 | exit **126**，`OCI runtime exec failed: … exec: "mysql": executable file not found in $PATH` |
| 10 | `-B` 对一个含 tab 的值 | 输出 `a\tb`（字面反斜杠 + t）；反斜杠本身输出 `\\` → 解析要还原 |
| 11 | SQL 里带 `'` `"` `\` 空格 | 经 argv **原样到达**（我们不过 shell，所以也没有 shell 注入面） |
| 12 | stdout 不是 tty 时的默认输出 | **batch（TSV）** → "给人看的表格"必须显式 `--table` |
| 13 | `error.rs` 的 `NonZeroExit` Display | 会把 `args` 拼进消息 —— 而 args 里有 `-e MYSQL_PWD=<明文>` |

第 13 条是这一版**最要紧的一条**：不处理它，"任何一次 SQL 报错都会把密码
打到提示条和日志里"。对应的实现是 `cmd::mysql::sanitize`。

---

## 2. 技术路线：驱动容器里的客户端，不引 MySQL 协议库

被否掉的方案：加 `mysql` / `mysql_async` 依赖，从面板直接连 3306。

否掉的理由（按分量排）：

1. **本机没有 cargo**。加依赖就要改 `Cargo.lock`，而锁文件只能靠
   `gen-lock.yml` 重生成（`AGENTS.md` §3 的 `--locked` 那一节）——
   为这一个功能引入一整棵异步 IO + TLS 依赖树，代价和收益不成比例；
2. 自连**要求容器把 3306 发布到主机**。实测那个 mysql 容器确实发布了，
   但"没发布端口"是完全正常的用法（同网络里的别的容器连它就行）；
3. 还要自己处理 `caching_sha2_password`、TLS、字符集协商这些镜像里
   已经配好的东西。

走 `wslc exec` 的代价只有两条，而且两条都能给用户明确的下一步：

| 代价 | 表现 | 我们给的提示 |
|---|---|---|
| 容器必须在运行 | `WSLC_E_CONTAINER_NOT_RUNNING` | "先到「容器」页把它启动起来" |
| 镜像里得有客户端 | exit 126 + `executable file not found` | "换官方 mysql / mariadb 镜像，或用「终端」自己进去装" |

于是「数据库」按钮**只在运行中的 MySQL 容器上出现**（和「终端」按钮同一条
理由：停止的容器上 `wslc exec` 必然失败，那种"点了必然报错"的按钮不如不画）。

---

## 3. 「这个容器是不是 MySQL」：三层判定

| 层 | 时机 | 依据 | 代价 |
|---|---|---|---|
| **L1 启发式** | 每帧渲染（`container_row_actions`） | ①镜像名最后一段 ∈ {mysql, mariadb, percona, …}；②`Command` 里有 `mysqld`/`mariadbd`；③`Labels` 元数据里发布了 3306 | **0**（纯字符串） |
| **L2 权威** | 进页面那一次 | `wslc inspect`：`Config.Env` 有 `MYSQL_*`/`MARIADB_*`、`Cmd`/`Entrypoint` 含 `mysqld`、`Ports` 有 `3306/tcp` | 1 次 inspect |
| **L3 运行时** | 紧接着 L2 | `/bin/sh -c 'c=$(command -v mysql || command -v mariadb || command -v mysqlsh) …'` | 1 次 exec |

几个刻意的选择：

- **L1 只决定"按钮画不画"**：渲染期不能起进程。所以它必须是纯字符串，
  而且**认不出来就什么都不做**（不画按钮），和 `image_preset` 同一条原则；
- 三条依据的**可靠程度不一样**，所以 `MysqlSignal` 把依据也带出来了
  （`Image` / `Command` / `Port`）—— 界面文案和排障都用得上，而不是压成一个 bool；
- 依据 ③（端口）是**猜**的：一个恰好发布 3306 的非 MySQL 容器会被误判。
  代价可接受（表现为"多了一个按钮"，点进去 L2 会说清"看起来不是 MySQL"），
  但这条在代码注释里写明了是推测；
- 依据 ②（命令）**在官方镜像上基本命中不了** —— 实测 `Command` 被截断成
  `"docker-entrypoint.s…"`。留着它是因为用户显式把命令写成 `mysqld` 时它有效。

---

## 4. 落地形状

### 4.1 页面

`Page::Database` 是**新页面**，但**不在** `Page::ALL` 里 —— 和 `AddInstance`
同一个取舍：它不是侧边栏里并列的一站，而是"针对某一个容器"的页面，
只能从容器行上的按钮进，顶部有「← 返回容器」。

三个 tab：

| tab | 内容 | 动作 |
|---|---|---|
| **数据库** | 名称 / 字符集 / 排序规则 / 表数 / 占用（系统库带「系统」标记） | 新建（字符集、排序规则可留空）、删除（二次确认） |
| **用户** | `User` / `Host` / `plugin` / `Super_priv` | 新建（可顺带授权）、改密码、授权、删除（二次确认） |
| **SQL 控制台** | 原样执行一条语句，`--table` 形态输出 | 「上面两张表没做的功能」的逃生口 |

「用户」那一页必须有 `Host` 一列：实测那个容器的 `mysql.user` 里
`root@%` 和 `root@localhost` 是**两条独立记录**，不写 host 用户根本分不清
自己在改哪一个。

数据库的信息来自**一条** SQL（不是每个库问一次）：

```sql
SELECT s.SCHEMA_NAME, s.DEFAULT_CHARACTER_SET_NAME, s.DEFAULT_COLLATION_NAME,
       COALESCE((SELECT COUNT(*) FROM information_schema.TABLES t
                 WHERE t.TABLE_SCHEMA = s.SCHEMA_NAME), 0),
       COALESCE((SELECT SUM(t.DATA_LENGTH + t.INDEX_LENGTH) FROM information_schema.TABLES t
                 WHERE t.TABLE_SCHEMA = s.SCHEMA_NAME), 0)
FROM information_schema.SCHEMATA s ORDER BY s.SCHEMA_NAME
```

用户列表读 `mysql.user` —— ⚠️ **普通账号读不到**（`ERROR 1142`），
所以代码里两条查询**分开取**：用户列表失败不该把数据库列表一起判成失败，
而是单独给一句"当前账号权限不够"。

### 4.2 表单：连接卡在页面上，动作在弹窗里

- **连接卡（用户名 / 密码）在页面上**，因为它是"这一页现在连的是谁"——
  属于**页面状态**，用户扫一眼就该知道（和「添加实例」页的表单同一类）；
- **一次性动作（新建库 / 新建用户 / 改密码 / 授权）在弹窗里**，
  和「拉取镜像」同一条理由：`InputState::new` 要 `&mut Window`，
  只能懒创建（渲染时建会每帧重建，一个字都打不进去）。
- 四种动作**共用一个结构体**（`DbDialog`）：同时只可能开一个，
  而且它们共用"用户名 / 主机 / 密码 / 库名"那几格。
- **参数不合法不关弹窗**：标识符白名单没过时只给一条提示条 ——
  用户就在那一格里，改一个字比重打一遍强。
- 「改密码 / 授权」里的用户名 / 主机是**锁定**的（不在弹窗里给输入框）：
  那两样改了就是另一个账号了。

### 4.3 密码怎么走

```
连接卡（明文显示，只在内存里）
  → DbState.creds: MysqlCreds        （Debug 手写脱敏）
  → wslc exec -e MYSQL_PWD=<密码> …  （唯一的传输方式）
```

三条约束：

1. **不进命令行**：mysql 的 `-p<密码>` 会出现在容器内的进程列表里；
   `-e KEY=VALUE` 不会。所以 `client_args` 里**永不出现 `-p`**（有单测盯着）；
2. **不进错误消息**：`Error::NonZeroExit` 的 Display 会拼 `args`，
   而 args 里有 `MYSQL_PWD=…`。出口只有 `sanitize()`，它洗两遍 ——
   **按形状**（`MYSQL_PWD=` / `-p…`）洗参数，**按值**（拿密码原文替换）
   洗文本，因为 `CREATE USER … IDENTIFIED BY 'x'` 出错时 MySQL 会把
   **整条语句**回显在 stderr 里；
3. **不落盘**：`prefs.json` 里没有这个字段，关掉面板就没了。
   `MysqlCreds` 的 `Debug` 是手写的（`AppState` 是 `#[derive(Debug)]`，
   派生 Debug 会把明文密码打进任何一次调试打印）。

密码**唯一**会短暂暴露的地方是 Windows 侧 `wslc.exe` 的命令行
（`-e MYSQL_PWD=…`）。这一步省不掉：`cli.rs::build_command` 把子进程的
stdin 写死成 `Stdio::null()`，**没有** stdin 这条更隐蔽的路。这条写在
`cmd::mysql::exec_argv` 的文档注释里。

另外：`MYSQL_ROOT_PASSWORD` 写在容器环境变量里时**自动填好**
（实测容器就有），并说明来源；`MYSQL_RANDOM_ROOT_PASSWORD` 则明说
"密码在容器日志里，先去看「日志」"——我们读不到那个值，猜也没用。

### 4.4 切换容器要清密码

一个容易忽略但后果严重的点：从容器 A 的数据库页切到容器 B 时，
如果留着 A 的密码，而 B 恰好也有个 `root`（密码不同）—— 表现是
"B 连不上"或者更糟（连上了但那是 A 的账号意味着两个容器同密码）。
所以 `open_database_page` 在**换容器**时把连接卡清空重建。

---

## 5. 安全：标识符白名单，不做"转义后拼 SQL"

库名 / 用户名 / 字符集名来自用户输入，会直接进 SQL。处理方式是
**白名单**：`[A-Za-z0-9_$]`、长度 ≤ 64，不合法就**拒绝**（把选择权交回用户），
而不是我们猜他的意图去转义。校验通过之后**仍然**用反引号包起来 ——
`order` 这种保留字做库名是合法的，不包就变成语法错误。

密码不一样：它允许任意字符，所以走 `quote_literal`（`\` → `\\`、`'` → `\'`、
`"` → `\"`）。这一路有单测，包括"注入形状必须被拒"那几条。

---

## 6. 分阶段与验收

| 阶段 | 内容 | 怎么验 |
|---|---|---|
| P1 | 纯逻辑：识别 / 命令行 / 解析 / SQL 生成 / 错误分类 / 脱敏 | `core` job 单测（夹具是实机抓的） |
| P2 | 列表行按钮 + 页面框架 + 连接卡 + 数据库列表 | 本机那个真 mysql 容器上点通 |
| P3 | 新建 / 删除库、用户的新建 / 改密码 / 授权 / 删除 | 建一个 `wslc_probe` 再删掉；建一个用户用它连上 |
| P4 | SQL 控制台 + 文档 | 跑一条错 SQL，确认提示里**没有密码** |

⚠️ **CI 只证明"编译过 + 纯逻辑测试过"**（`AGENTS.md` §8）。下面这些必须
本机点一遍：

- 容器**停止**时列表行**不出现**「数据库」按钮；页面开着时把容器停掉 →
  报错文案是"先启动它"；
- `mysql` 首次初始化（entrypoint 在跑）期间点连接 → 提示"稍等"而不是
  甩一句英文 `ERROR 2002`；
- 用一个**没有客户端**的镜像冒充（比如 nginx）→ 提示"镜像里没有客户端"；
- 「操作」列加了第 5 个按钮（`ALL_COLUMNS` 的权重 250 → 330）之后
  **实机看宽度**会不会挤；
- 故意让 SQL 失败，翻提示条与日志，确认**密码一个字都没有**。

---

## 7. 这一版**没做**的事（都是刻意留的）

- **不自己连 3306**（见 §2）；
- **不做 SQL 自动补全 / 结果分页**：控制台的输出按原样展示，长结果靠滚动；
- **不做用户重命名**（`RENAME USER`）：改密码 + 授权覆盖了绝大多数需求，
  而"重命名"在界面上要额外说清"权限会跟着走"，这一版没到那一步；
- **不做导入导出 / 备份**：那是另一个量级的功能（要么走 `wslc cp` 落文件，
  要么在容器里跑 `mysqldump` 再想办法取出来），留给后面；
- **不做密码掩码**：组件库的 `InputState` 支不支持掩码**没核实过**
  （本机没有 cargo registry 源码，看不了实现），所以这一版连接卡上的
  密码是明文显示的 —— 这条**是推测的边界**，不是结论。
- **不做多容器的数据库页并存**：`AppState::db` 是 `Option<DbState>`，
  一次只服务一个容器（换容器即整体替换）。
