# PLAN v0.8 —— 建库同时建用户 + 数据库手动备份 / 恢复

> ⚠️ 这份文档**是补写的**：v0.8.0 的提交信息里引用了它，但当时没有真的建。
> 现在补上，内容包括当时的设计、**实测依据**，以及两处**事后的更正**
> （§4、§5）—— 更正比原文更重要，所以没有藏起来。

范围由产品决定拍定（用户在 v0.7 的数据库页截图上逐条划掉了不要的东西）：

**做**：① 建库同时建用户（用户名 / 密码直接显示在库列表里）；② 手动备份 / 恢复。

**不做**：库的描述、建库时间（MySQL 本来就不存）、分页、搜索框、类型下拉、
「已授权用户」列、定时备份、本地 notes 存档。

---

## 1. 起点与既有约束

v0.7.0 已经做了数据库管理页（连接卡 / 库列表 / 用户列表 / SQL 控制台）。
v0.8 在它上面加两件事。两条硬约束贯穿全篇：

- **不引入 MySQL 协议库**：所有 SQL 都靠**容器自带的 `mysql` 客户端**、
  经 `wslc exec` 执行（`Cargo.lock` 不动）；
- **纯逻辑放 `wslc-core` / `wslc-panel-core`**：本机没有 Rust 工具链，
  只有 `core` 那个 CI job 会跑测试（十几秒），放错地方就等于没测。

## 2. 建库同时建用户

### 2.1 决定

| 项 | 定下来的做法 | 为什么 |
|---|---|---|
| 用户名 | `<库名>_user`（`djxm` → `djxm_user`） | 建库的人几乎总要一个连它的账号；规则写在弹窗里，跟着库名变 |
| 主机 | 固定 `%` | 面板的用途是"把库连起来用"，默认锁 `localhost` 只会添堵 |
| 密码 | 预填 **16 位随机**，可改 | 见 §2.2 |
| SQL 形态 | 建库 + 建用户 + 授权合成**一条** `-e`（`;` 分隔） | 实测 `;` 分隔多条语句能跑；**先都拼好、都校验过再发**，所以不会留下"建了库没建用户"的半成品 |
| 账号存哪 | 本地 `db-credentials.json`，键 = `容器名/库名` | MySQL 的密码是**单向哈希**，建完就查不回来；要"在列表里显示密码"就必须自己记 |
| 写盘时机 | SQL **成功之后** | 列表里不能出现一个 MySQL 里根本不存在的账号 |
| 删库 | 同步删掉本地那条记录 | 库都没了，那条账号已经失去意义 |
| 展示 | 库列表加「用户名 / 密码」两列 | 别处建的库显示 `—`（不猜、不编） |

### 2.2 ⚠️ 密码在磁盘上是**明文**（产品决定的代价）

`db-credentials.json` 里存的是明文。这是"**要在列表里展示密码**"这个产品
决定的直接代价 —— 1Panel 同样这么做。写在这里是为了别让后来的人以为
它是疏忽：

- 文件落在 `%LOCALAPPDATA%\wslc-panel\`，权限跟着用户的 profile；
- **日志里绝不出现**：`DbCredential` 的 `Debug` 是**手写脱敏**的
  （`password: "***"`），所以 `DbState` / `AppState` 派生的 `Debug` 一路带不出来；
- 错误消息同样：`Error::NonZeroExit` 的 `Display` 会带上 `args`，而
  `wslc exec -e MYSQL_PWD=<明文>` 就在里面 —— 所以所有 mysql 相关错误
  都必须先过 `cmd::mysql::sanitize`。

### 2.3 随机密码的随机源是**推的边界**

没有 rand 依赖（不为这一期引入新 crate），用的是 `std` 自带的熵：
纳秒时间 + 进程内递增计数器 + 栈地址（ASLR），过 `DefaultHasher` 后用 LCG
摊开。**不是密码学安全的随机** —— 威胁模型是"别让两个库同密码"，
不是"对抗能读内存的攻击者"。这条在 `model::mysql::generate_password`
的文档注释里写明了。

字符集刻意**不含**需要 SQL 转义的字符（`'` `"` `\`），也不含 `0/O`、`1/l/I`
—— 密码是要抄到别的机器上用的。

## 3. 手动备份 / 恢复

### 3.1 定下来的做法

- **备份**：容器内 `sh -c 'mkdir -p /tmp/wslc-backups && mysqldump --databases
  <库> --single-transaction --quick --set-gtid-purged=OFF > /tmp/wslc-backups/<库>-<时间>.sql'`，
  然后 `wslc container cp <容器>:<路径> <Windows 存档目录>`；
- **Windows 存档**：`%LOCALAPPDATA%\wslc-panel\backups\<容器>\`（**路径里带
  容器名**，两个容器的备份互不混）；
- **恢复**：见 §5（这里改过一次）；
- **文件名**：`<库名>-<YYYYMMDD>-<HHMMSS>.sql`（定宽、字典序 == 时间序，
  所以列表倒序不用解析时间）；同一秒同库撞名就报"刚备份过"，不自动加序号。

`--databases` 让 dump 里带 `CREATE DATABASE`，所以恢复是"回原库"；
`--set-gtid-purged=OFF` 是因为实测不加会往 stderr 打一条 GTID 警告。

### 3.2 `spawn_to_file`：为"边 dump 边传"留的地基，这一期**没用上**

`cli.rs` 加了 `Wslc::spawn_to_file`（stdout 落**本地**文件、stderr 单独逐行
回调），带端到端单测。它解决的问题是：`run` 会把全部输出缓冲到内存
（大库 dump 会撑爆），`spawn_streaming` 又把 stdout / stderr 混进同一个回调
（备份文件里混进一行报错就整个坏了）。

⚠️ 但这一期的备份**没有**用它 —— dump 的 stdout 直接落在**容器内**
（sh 的重定向做的），Windows 侧只是事后 `cp` 回来。所以界面上没有实时进度，
只有"正在备份…"。这条如实写在 `cmd::mysql::backup_database` 的注释里。

## 4. ⚠️ 更正之一：数据库管理页**从 v0.7.0 起就没能用过**

**症状**（用户在 v0.8.0 上截图）：页面永远停在「正在读取…」，页头按钮永远是
「读取中…」，两张表永远是空的，新建库 / 备份按钮都不可能出现。

**根因**：`DbState::provisional` 把 `busy` 置成 `true`（想法是"马上就要去探测，
先显示读取中"），而 `Shell::load_db` 的第一句是"正在跑就不叠第二次"：

```rust
if db.busy { return; }   // ← 第一次加载被自己挡掉
```

于是**一次探测都没跑起来**。`git show v0.7.0:…state.rs` 里同样是
`busy: true` + 同一句守卫 —— **这个页面从上线起就是坏的**，v0.8 的建库 /
备份全建在它上面。

**为什么 CI 没拦住**：类型全对、`core` 全绿、`ui` 全绿。这个 bug 只表现为
"界面永远转圈"，只有实机点一下才看得见。

**教训**（这条是这一版最值钱的东西）：*页面上的反证要看*。那张截图里
页头客户端那行只有一个兜底的名字 `mysql`、**没有版本号** —— 那正是
"权威探测没跑过"的物证。我当时把它当成了"加载中的正常状态"，没去看。

**修法与防线**：

- `busy` 的语义收窄成"**已经有一个后台任务在跑**"，置位的责任挪到真正起任务
  的函数（`load_db` 进去就置 `true`，且每条完成路径都会清）；
- 回归测试 `a_freshly_opened_db_page_can_be_loaded` 把"新建出来的状态必须是
  可加载的"钉死 —— 这类 bug 断言是唯一能提前拦住它的地方；
- 顺带修掉两个同类死角：读失败后**重新进页面会自动重试**；`ls` 缺失文件
  的退出码非零要翻成"不存在"（否则精心写的解释永远不会出现）。

## 5. ⚠️ 更正之二：「本地→容器是坏的」是**我测错了**

v0.8.0 / 0.8.1 的恢复**只认容器内 `/tmp` 那份**，理由写在代码注释里：
`wslc container cp` 的"本地→容器"方向实测不可用。**那个结论是错的。**

用户拿 `wslc container cp --help` 指出 `cp` 有三种形式（本地→容器、
容器→本地、**标准输入→容器**）。重测（容器 Up，2026-10-15）：

| 试的 | 结果 |
|---|---|
| `cp <本地文件> mysql:/tmp/probe.txt`（**目标写成文件名**） | ❌ `ERROR_PATH_NOT_FOUND`（我 v0.8.0 只试了这一种） |
| `cp <本地文件> mysql:/tmp/`（**目标写成已存在的目录**） | ✅ 落在 `/tmp/<本地文件名>` |
| `cp <本地文件> mysql:/tmp`（目录、无尾斜杠） | ✅ |
| `cp <本地文件> mysql:/不存在的目录/` | ❌ 同一个错 —— 目录要先 `mkdir -p` |
| 4 MB 随机二进制（sha256 对照） | ✅ **逐字节一致** |
| 本地路径含空格 / 同名覆盖 | ✅ / ✅ |
| `cp - mysql:/tmp/`（stdin 形式，经 `cmd /c "… < file"`） | ❌ `unexpected EOF` —— 没找到正确喂法，未继续查 |

**教训更深一层**：`wslc exec -i`（喂 stdin）本来就是支持的，
`docs/wslc-schema.md` §10 的命令清单里也**早就列着 `cp`** ——
也就是说"没有办法把文件送回容器"这个结论，与仓库里已有的实测记录相矛盾，
我没去核对。**"这条路不通"必须多试几种参数形式**（尤其"目标是文件名"
和"目标是目录"是两套语义）。

**改法**：恢复改成**以 Windows 侧存档为准** ——
`mkdir -p /tmp/wslc-restore` → `container cp <Windows dump> <容器>:/tmp/wslc-restore/`
→ `sh -c 'mysql … < /tmp/wslc-restore/<文件名>'`。于是
"容器重建过 / `/tmp` 被清过"不再是障碍（这个限制本来就不需要存在）。

端到端实测（新配方）：造库 → dump → cp 回 Windows → **删掉容器内那份并
DROP 整个库** → 只用 Windows 侧文件恢复 → 两张表和中文数据都回来了。

## 6. 落地位置（改动地图）

| 位置 | 内容 |
|---|---|
| `wslc-core/src/model/mysql.rs` | `generate_password` / `default_user_for` / `mysqldump_args` / `restore_script` / `backup_file_name` / `CONTAINER_BACKUP_DIR` / `CONTAINER_RESTORE_DIR` / `restore_container_path` / `WINDOWS_BACKUP_SUBDIR`（都是纯函数，带测试） |
| `wslc-core/src/cmd/mysql.rs` | `BackupPaths` / `backup_paths` / `backup_database` / `push_dump_to_container` / `restore_database` |
| `wslc-core/src/cli.rs` | `spawn_to_file` + `FileStreamHandle`（§3.2） |
| `wslc-panel-core/src/dbcreds.rs` | `DbCredential`（手写脱敏 `Debug`）/ `DbCredentialStore` / `load` / `save`（临时文件 + rename） |
| `wslc-panel-core/src/state.rs` | `DbState` 的凭据 / 备份字段、`BackupState`、`BackupEntry`、`parse_backup_file_name`、`scan_backup_dir`、`PendingAction::RestoreDatabase` |
| `wslc-panel-core/src/columns.rs` | 库列表 8 列（加了用户名 / 密码）、`BACKUP_COLUMNS` |
| `wslc-panel/src/app.rs` | 建库弹窗的"同时创建用户"、`run_db_statement` 成功后写凭据、备份 / 恢复 / 删除备份 / 打开位置 |
| `wslc-panel/src/views.rs` | 库列表两个新列 + 「备份」按钮、`db_backup_progress`、`db_backup_dialog_overlay` |

## 7. 没有验证到的（诚实清单）

- 界面渲染 / 布局（8 列的宽度、两个按钮挤不挤）—— **本机没有 Rust 工具链，
  也没法跑 GUI**，只能实机看；
- `cp -`（stdin 形式）没喂通，所以没采用；
- 恢复只能"回原库"（dump 是 `--databases` 形式）—— 改库名恢复是另一个功能；
- 备份没有实时进度（§3.2）。
