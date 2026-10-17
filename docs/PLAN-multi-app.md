# PLAN —— 一个容器跑多个应用（多应用部署）

> **触发问题**：「nginx 和 Java 等几个的部署管理，我都是想一个容器部署多个应用，行不行？」
>
> **结论：行，而且数据层已经支持了一半。** 存档是按 `<容器>/<应用>` 索引的，
> 列表、启动/停止、日志、删除都是**按应用**的（见 §1.1）。真正卡住的是三件事：
>
> 1. **三个默认值每一项都撞车**：新应用的应用名永远是 `app`、端口永远是运行时的默认值、
>    nginx 的目标目录永远是**同一个 Web 根**；
> 2. **nginx 没有按应用隔离目录**：所有应用铺进同一个 Web 根，而删除/准备是"逐名删除"——
>    两个应用都有 `index.html` 时，删 A 会删掉 B（§1.3，这是**数据风险**，不是体验问题）；
> 3. **没有端口唯一性校验**：同一容器里的应用共享网络命名空间，两个应用写同一个端口，
>    后启动的那个必然 `Address already in use`。
>
> 修复模型很朴素：**一个应用 = 一个名字 + 一个目录 + 一个端口**；
> nginx 再加**每应用一份 `server` 块**（`listen <端口>`）。
> 端口必须在建容器时发布（多 `-p` 已支持，**实测**）。
>
> ⚠️ 本文**修订** [`PLAN-nginx-site.md`](PLAN-nginx-site.md) 的三处：§5.2 的
> `resolver` 在多应用下必须放进各自的 `server`（放文件顶层会重复，**实测**）；
> §5.5 的「一个容器一个站点」改成「一个**端口**一个站点」；
> nginx 的默认目标从 Web 根改成 `<web_root>/<app>`。
>
> **依赖关系**：nginx 的「站点开」模式（每应用一个端口/conf）建立在
> `PLAN-nginx-site.md` 的 `site` 字段 + conf 生成之上；进程型（Java/Node/Go）的
> 多应用**不依赖**它，可以先做。两个计划都落地之后，同一个容器里可以
> Java 多应用 / nginx 多应用 / nginx 站点开+站点关混用。

---

## 0. 先说「行不行」

| 运行时 | 今天能多应用吗 | 卡在哪 | 本方案之后 |
|---|---|---|---|
| **Java** | 手工改四格**能跑**（名字/目标/端口/命令） | 应用名/端口默认撞；改了名字目标不会跟着改；面板不知道应用实际绑哪个端口 | 名字/端口/目标自动不撞；启动命令给提示 |
| **Node.js** | 同上 | 同上；`npm start` 默认 3000 | 同上（P1：默认带 `PORT=`） |
| **Go** | 同上 | 同上 | 同上 |
| **Nginx** | **不安全** | 目标默认是同一个 Web 根；没有配置层；逐名删除会串台 | 每应用一个子目录 + 一个端口 + 一份 server 块 |
| **同一容器混运行时**（nginx + Java） | ❌ | `runtime` 是**容器级**的（`detect` 按镜像认定） | 不做（§7） |
| **外部访问** | 取决于建容器 | 端口发布只在 `create` 时给；**多 `-p` 已支持** | 表单按已发布端口分配；没发布就明说"只能容器内访问" |

> 「手工能跑」的意思是：现在也可以在「添加部署」里把应用名改成 `app2`、
> 目标改成 `/opt/app2`、端口改成 8081、启动命令改成绑 8081，然后部署。
> **但面板不会拦你填错**，第二个应用默认会把第一个覆盖掉（应用名都是 `app`）；
> 改了应用名之后**目标也不会自动跟着变**（还是 `/opt/app`）——所以那四格都得自己改。
> 所以这不是"支持"，只是"没挡"。

---

## 1. 现状取证

### 1.1 已经按应用索引的部分（这些不要重写）

| 东西 | 位置 | 说明 |
|---|---|---|
| 存档键 | `DeployStore::put` / `for_container`（`deploys.rs:137` / `:156`） | `"<容器>/<应用>"`，同一容器本来就能有多条 |
| 列表 | `DeployState::entries` / `views::deploy_table` | 每行一条记录，本来就画得出来 |
| 启停/日志/删除 | `Shell::deploy_action` / `start_deployment` / `stop_deployment` | 全是按应用名找记录 |
| 状态探测 | `refresh_deploy_status_inner`（`app.rs:3989`） | 每条部署一次 `exec`（N 条就是 N 次，P1 再合并） |

**结论：不需要改存档结构（除了 `site`），也不需要新增"应用列表"这个概念。**

### 1.2 三个默认值撞车

`Shell::open_deploy_dialog`（`app.rs:4112`）新建一条时：

| 字段 | 现在的默认 | 后果 |
|---|---|---|
| 应用名 | `DEFAULT_APP_NAME = "app"`（`deploy.rs:271`） | 第二次「添加部署」还是 `app` → `store.put` **覆盖**第一条 |
| 端口 | `runtime.default_port()`（`app.rs:4171`） | Java/Go 都 8080、Node 都 3000 → 端口冲突 |
| 目标目录 | nginx = `web_root`（`app.rs:4138`）；进程型 `/opt/<app>` | nginx **所有应用同一个目录**；进程型只要应用名不撞就没事 |

### 1.3 nginx 的共享根 + 逐名删除 = 会删别人的文件

`model::deploy::prepare_target_script` 在复制**之前**先执行
[`removals_script`](crates/wslc-core/src/model/deploy.rs#L719)：

```text
rm -rf <目标>/<这次要铺的名字 ∪ 上次铺过的名字 ∪ app.log ∪ app.pid>
```

- 目标目录是**用户填的**，旧默认是 Web 根；
- 两个应用都往 Web 根铺 `index.html` 时，铺 B 会在复制前把 A 的 `index.html` 删掉；
- 删 A 时也会把 B 的 `index.html` 删掉（A 的 `members` 里有这个名字）。

**这不是"可能会"，是当前默认值下必然发生。** 所以 nginx 的多应用不能只改默认值，
必须**每个应用一个子目录**（`<web_root>/<app>`），让 `removals_script` 只在应用自己的目录里动手。

### 1.4 端口没有唯一性校验

`submit_deploy` 只校验应用名格式、来源、目标白名单、启动命令；**端口只 parse 成 `u16`**，
不查有没有被同一容器的另一条记录用掉。容器里所有应用共享一个网络命名空间，
所以两个进程型应用写同一个端口时，后启动的那个会在 `app.log` 里
`bind: address already in use`，而面板的 `start` 仍然返回成功（`nohup ... &` 的语义），
用户只看到"部署成功、状态已停止"。

### 1.5 运行时是容器级

`cmd::deploy::detect` 用 `inspect` 的镜像/环境/命令认**容器**属于 nginx / Java / Node / Go；
「添加部署」的表单默认值全来自这个 `runtime`。所以**同一个容器里混运行时**（nginx + Java）
今天做不到，也不在本文范围（§7）。

---

## 2. 实测（`wslc 3.0.3.0` + 官方 `nginx:1.31.6`，2026-10-17，本机）

> 跑完把容器/网络/临时文件都删了。

| 试的 | 结果 |
|---|---|
| 一个 nginx 容器，两个 `server` 块，分别 `listen 8080` / `8081`，**同一个 `server_name localhost`** | `nginx -t` **通过**（`server_name` 只在同一 `listen` 上才冲突） |
| 同上，两个 root 各自放 `index.html` | `http://localhost:18080/` → A，`http://localhost:18081/` → B |
| 第二个应用开 SPA 回退，请求 `/some/spa/route` | 返回 B 的 `index.html` ✅ |
| 第二个应用的 `/api/` 反代到一个**没在跑**的后端 | `nginx -t` / reload **通过**；请求 502（变量 + resolver 方案，见 `PLAN-nginx-site.md` §3） |
| `wslc run -p 18080:8080 -p 18081:8081` | 两个映射都生效；`inspect.Ports` 两个键 `8080/tcp`、`8081/tcp` |
| `inspect.Ports` 里的 `HostIp` | **`127.0.0.1`**（不是 `0.0.0.0`）—— URL 要把 `127.0.0.1` 也算成 `localhost` |
| 两个应用的 conf **各自在文件顶层写 `resolver`** | ❌ `[emerg] "resolver" directive is duplicate`，`nginx -t` **失败** |
| 两个应用的 conf **各自把 `resolver` 放进 `server {}`** | ✅ `nginx -t` 通过 |
| 建容器表单的端口映射 | 已支持逗号/换行分隔（`views.rs:7032` + `split_list`）；`wslc run` 多个 `-p` 实测可用 |

**最关键的一条：`nginx -t` 对"两个应用、两个端口、同一个 `server_name`"是接受的；
但对"两个文件都在 http 级写 `resolver`"是拒绝的。** 所以每应用的模板必须把
`resolver` 放进 `server`，不能用 `PLAN-nginx-site.md` 里那份顶层写法。

---

## 3. 目标模型

```text
一个应用 = 应用名（容器内唯一）
         + 目标目录（容器内唯一；父子重叠只在"老根 + 新子目录"这个受控方向上允许）
         + 端口（会 bind 的应用之间唯一，最好已发布到主机；nginx 站点关不 bind）
         + 运行时（今天 = 容器运行时）
         [+ nginx 的一份 server 块（listen = 端口，root = 目标目录）]
```

### 3.1 nginx 的多应用 = 每应用一个端口

- 每个应用：`root = <web_root>/<app>`，`listen = 应用端口`；
- 文件：`/etc/nginx/conf.d/00-wslc-panel-<app>.conf`，**站点开的**每个应用一份；
- **不同端口**上 `server_name localhost` 可以重复（实测），所以不需要 `server_name` 区分；
- **端口 80** 是特例：`default.conf` 已经在 80 上，要接管就靠 `00-` 字典序 + `localhost`
  抢先成为默认 server（实测；会有一条 `conflicting server name` 警告，exit 0）；
- 两个**站点开**的应用都 `listen 80` → **拒绝**（两个 `00-` 块会互相吞掉，只有一个能生效）。
- 访问：`http://localhost:<主机端口>/`（主机端口从 `PortMap` 反查）；
  没发布 → 只能容器内访问 `http://127.0.0.1:<端口>/`。

> 为什么不让所有应用共享一个端口、按路径分（`/app1/`、`/app2/`）？
> 纯静态站可以，但每个 SPA 的 `base` 要改成 `/app1/`，每个应用的 `/api` 前缀会互相打架；
> 这是 P1（§7），P0 用"一应用一端口"——它对 nginx / Java / Node / Go **是同一种模型**，
> 而且实测成立。

### 3.2 nginx 的两种模式（同一个容器里可以混用）

| | **站点开**（多应用推荐） | **站点关**（只铺文件） |
|---|---|---|
| 目标 | `<web_root>/<app>` | `<web_root>/<app>` |
| 端口 | 就是这一格的端口（`listen <port>`） | **不占端口**；由容器的默认站点在 80 上提供 |
| 文件怎么访问 | `http://localhost:<该端口的宿主端口>/`，`root` = 应用子目录 | `http://localhost:<80 的宿主端口>/<app>/`，由 `default.conf` 的 `root <web_root>` 提供 |
| SPA / 反代 | 面板生成 `try_files` 和 `location /api/` | **没有**；SPA 的 `base` 要自己写成 `/<app>/`，反代要自己改 conf |
| 端口这一格 | 是 listen，必须唯一 | 只是信息（默认站点的端口是 80） |

**因此：**

- 前端要打后端 → **必须站点开**（否则没有 `location /api/` 可配）；
- 纯静态、没有反代、能接受 `/<app>/` 前缀 → 站点关也行（文件已经隔离在子目录里）；
- 站点关的应用，端口列要显示"由默认站点提供（/<app>/）"，URL 用 **80 的宿主端口 + /<app>/**，
  不能拿 `record.port` 去算（那一格对站点关没有意义）；
- 站点开/关是**每条部署**的勾选，同一个容器里可以 A 开、B 关。

> ⚠️ **站点开 + 端口 80 = 接管 `default.conf`**，而默认站点正是**所有站点关的应用**
> 的宿主。接管之后，那些应用的 `/<app>/` 会一起消失（新的 server 只 `root` 自己那个子目录）。
> 所以同一个容器里既有站点关应用、又想让某个应用站点开 80 时，**别这么做** ——
> 给站点开的应用换一个非 80 端口（那个端口要已发布）。P0 不试图在同一份 conf 里
> 给多个子目录都加 `location`。

### 3.3 进程型（Java / Node / Go）的多应用

面板能做的是：**分配**一个不冲突的名字/目录/端口，并在启动命令旁边说清
"应用自己也要绑这个端口"。

面板**不能**做的是：把端口注入到任意应用里（Java 的 `--server.port`、Go 的 flag、
Node 的 `PORT` 各不相同）。所以：

- `port` 这一格对进程型仍然是**信息性**的（它不会出现在启动命令里）；
- 表单按运行时给一句提示（§4.2）；
- 端口唯一性/发布状态由面板校验；应用实际绑没绑，只能看 `app.log`；
- ⚠️ **容器自己的主进程**（镜像的 `CMD` / `ENTRYPOINT`）也可能在监听某个端口 ——
  面板看不到它（`inspect` 只给发布映射，不给监听表）。如果分配到的端口正好被主进程占着，
  应用会 `bind: address already in use`；换一个端口或看 `app.log`。

---

## 4. 方案

### 4.1 分配：纯函数（全部进 `wslc-core` / `wslc-panel-core`，带单测）

```rust
// wslc-core/src/model/deploy.rs
/// 新应用的默认名：app / app-2 / app-3 ……
pub fn suggest_app_name(existing: &[String]) -> String;

/// 新应用的默认端口：优先用**已发布**且没被记录占用的容器端口；
/// 都占了就退回 default（运行时默认）往上找第一个没占的；不保证已发布。
pub fn suggest_app_port(existing: &[u16], published: &[u16], default: u16) -> u16;

/// `PortMap` → 已发布的**容器 TCP 端口**，升序去重（空 bindings 不算发布；
/// `/udp` 不算 —— 应用监听的是 TCP）。
pub fn published_container_ports(ports: &PortMap) -> Vec<u16>;

/// 主机可访问的 URL；`127.0.0.1` / `0.0.0.0` / `::` / `::1` / 空 → `localhost`。
pub fn app_url(ports: &PortMap, container_port: u16) -> Option<String>;

/// 新应用的默认目标目录：nginx = `<web_root>/<app>`；进程型 = `/opt/<app>`。
pub fn default_app_target(runtime: AppRuntime, app: &str, web_root: Option<&str>) -> String;

/// 两个目录的方向关系（**必须分方向**，见 §4.3 的规则）。
pub enum TargetOverlap {
    Same,
    /// 新目标在已有目标**里面**（`/root/app2` vs `/root`）—— 迁移时要允许。
    NewInsideExisting,
    /// 新目标是已有目标的**父目录**（`/root` vs `/root/app2`）—— 危险。
    ExistingInsideNew,
}
pub fn target_overlap(new_target: &str, existing_target: &str) -> Option<TargetOverlap>;
```

```rust
// wslc-panel-core/src/deploys.rs（要看记录，只能在这一层）
impl DeployStore {
    /// 同一容器里别的应用已经占了这个端口吗？
    ///
    /// 只算**真的会 bind 的记录**：进程型全部算；nginx 只算 `site.is_some()` 的
    /// （站点关的记录由默认站点在 80 上提供，`record.port` 只是信息，不占端口）。
    pub fn port_conflict(&self, container: &str, app: &str, port: u16) -> Option<&str>;

    /// 同一容器里**真的会 bind** 的端口（给 `suggest_app_port` 用）。
    /// 进程型全部算；nginx 只算 `site.is_some()` 的。
    pub fn binding_ports(&self, container: &str) -> Vec<u16>;

    /// **新建**时能不能用这个目标；`None` = 可以。
    ///
    /// 规则见 §4.3：相同 / 新目标是父目录 → 拦；新目标在旧目录里面 →
    /// 只有当"子目录的第一段"不在旧应用的 `members` 里才放行
    /// （否则旧应用的逐名删除会把它带走）。
    pub fn target_blocker(
        &self,
        container: &str,
        app: &str,
        target: &str,
    ) -> Option<TargetBlock>;
}

/// `target_blocker` 给出的三种"不能这么做"。
pub enum TargetBlock {
    /// 同一个目录。
    Same(String),
    /// 新目标是已有目标的父目录。
    CoversExisting(String),
    /// 新目标在旧目录里，而"第一段"正是旧应用铺过的名字 —— 会被它的逐名删除带走。
    InsideMember { existing: String, name: String },
}
```

测试至少覆盖：

- `suggest_app_name`：空 → `app`；有 `app` → `app-2`；有 `app`/`app-2` → `app-3`；
- `suggest_app_port`：published `[80, 8080]` + used `[]` → `80`；used `[80]` → `8080`；
  published 全占 → 从 default 往上找；0 永远不返回；
- `published_container_ports`：`80/tcp` 有 bindings、`8080/tcp` 空 bindings → 只出 `[80]`；
  `8080/udp` 有 bindings → 不算；
- `app_url`：`127.0.0.1:18080` → `http://localhost:18080`；`0.0.0.0` → `localhost`；
  空 bindings → `None`；
- `default_app_target`：nginx + `/data/www` + `api` → `/data/www/api`；
  nginx 无 web_root → `/usr/share/nginx/html/api`；Java + `api` → `/opt/api`；
- `port_conflict`：同应用忽略；别的应用同端口 → `Some`；
  nginx **站点关**的记录同端口 → `None`（它不 bind）；nginx 站点开的 → `Some`；
- `binding_ports`：进程型 + nginx 站点开都在；nginx 站点关不在；
- `target_overlap`：`/opt/a/b` vs `/opt/a` → `NewInsideExisting`；
  `/opt/a` vs `/opt/a/b` → `ExistingInsideNew`；`/opt/a` vs `/opt/a` → `Same`；
  `/opt/a` vs `/opt/ab` → `None`；尾部 `/` 要归一化；
- `target_blocker`：老应用目标 `/root`、`members=["index.html"]`：
  - 新目标 `/root/app2` → 放行（迁移路径）；
  - 新目标 `/root/assets` 且 `members` 里有 `assets` → 拦；
  - 新目标 `/root` → 拦；新目标 `/`（父目录）→ 拦。

### 4.2 表单默认值（`open_deploy_dialog`）

新建一条时：

| 字段 | 新默认 |
|---|---|
| 应用名 | `record.app`（重新部署）或 `suggest_app_name(现有名字)` |
| 端口 | `record.port`（重新部署）或 `suggest_app_port(会 bind 的现有端口, published_container_ports, runtime.default_port())` |
| 目标 | `record.target`（重新部署）或 `default_app_target(runtime, 应用名, web_root)` |

- nginx 的**目标目录从"Web 根"改成"Web 根/应用名"**：这是多应用隔离的核心，
  也是防止 `removals_script` 串台的关键；
- ⚠️ `web_root` 只从 `/usr/share/nginx` 底下的挂载点推（`nginx_web_root` 的现有规则）；
  站点挂在 `/data/www`、`/www` 这类地方时推不出来，默认会回落到
  `/usr/share/nginx/html/<app>` —— 表单要提示"按你的挂载点改"，
  别让用户以为面板知道真实的 Web 根；
- 端口那一格下面加一行**已发布端口**（和列表里 `主机->容器` 的记法一致）：
  `容器已发布：主机 8080 → 容器 80、主机 8081 → 容器 8080；当前可用：80、8080`
  （从 `deploy.ports` 算；没发布的端口标灰/文字说明）；
- 进程型的端口格下面按运行时给一句：
  - Java：`应用自己要绑这个端口（Spring Boot：--server.port=<端口>）`；
  - Node：`很多应用读 PORT（可以写 PORT=<端口> npm start）`；
  - Go：`看你的 flag / 环境变量`。
- nginx 的站点勾选框决定端口这一格的含义：**勾上 = listen**（唯一、参与冲突检查）；
  **不勾 = 信息**（默认站点在 80 提供，URL 用 80 + `/<app>/`）。这一句要写在表单里，
  否则用户会以为不勾站点时端口也生效。

> 应用名改了之后目标目录**不会自动跟着改**（它可能已经被用户手动改过）。
> 这不会造成冲突（名字唯一时目标也唯一），只是目录名和用户名不一致；P1 再做一个
> "目标还是默认值就跟着应用名走"的小逻辑。
>
> ⚠️ **行为变化**：新 nginx 应用的默认目标从 Web 根改成 `<web_root>/<app>`，
> 单应用用户会看到站点在 `/<app>/`（勾上「生成站点配置」才在 `/`、独占一个端口）。
> 旧记录不动。想保留"第一条就在根"的旧行为也可以：第一条用 Web 根、从第二条开始
> 用子目录 —— 代价是第一条仍然共享根（隔离只对新记录生效）。**默认按统一子目录**，
> 因为这才是多应用下不会串台的形状。

### 4.3 校验（`submit_deploy`，同步阶段，**不合法不关弹窗**）

按顺序：

1. **应用名格式**（现有 `valid_app_name`）；
2. **新建时应用名已存在** → 报错：
   `已经有叫 <app> 的部署 —— 想覆盖请点那一行的「重新部署」，或者换个名字`。
   ⚠️ 这是多应用最容易误操作的一步：现在同名会**静默覆盖**。
3. **端口**：必须是 `1..=65535`；空 → 报错（**不要**像现在这样静默回落到
   `runtime.default_port()` —— 多应用下那必然撞）；
   `port_conflict` → 报错
   `端口 8080 已经被「api」用了 —— 同一个容器里的应用不能共用一个端口`。
   - **nginx 站点关时跳过这条**：那种记录不 bind 任何端口（文件由默认站点在 80 上提供），
     `record.port` 只是信息；否则一个站点关的应用会白白占掉一个端口号。
   - 进程型全部要查（面板不知道应用实际绑没绑，只能按"它应该绑"来算）。
   - ⚠️ 现在 `DeployField` 只有 App / Source / Target / Command，**没有 Port** ——
     端口校验失败时没法把焦点送回去；顺手加一个变体。
4. **目标目录**（用 `safe_target_with_site` 那层之后）：
   - 新建 + **相同目录** → 报错；
   - 新建 + **新目标是已有目标的父目录** → 报错（新应用的逐名删除可能带走子目录）；
   - 新建 + **新目标在已有目标里面** → **默认放行**（这正是"老记录在 Web 根、
     新应用铺到 Web 根/app2"的迁移路径），**但**如果子目录的第一段出现在
     旧应用的 `members` 里 → 报错（旧应用的 `removals_script` 会删它）。
     例：旧目标 `/root`、`members=["index.html"]` → `/root/app2` 放行；
     `members` 里有 `assets` 时 `/root/assets` 被拦；
   - 重新部署：
     - **目标没变** + 与别人重叠 → 只警告（老记录已经在共享目录里，拦住就永远改不了），
       警告里点名对方应用和"会动到它的同名文件"；
     - **目标变了** → 按"新建"的规则查（不能让人手动改到一个会吃掉别人的父目录）；
5. **端口没发布到主机** → **警告不拦**：
   `8081 没发布到主机 —— 站点只能在容器/其它容器里访问；要外部访问得重建容器并加 -p 8081:8081`；
6. **nginx + 端口 80** → 提示"会接管默认站点，**站点关的应用的 `/<app>/` 也会一起消失**"
   （这是预期行为，不拦，但文案要说全）；
7. **进程型 + 端口**：不校验应用真的绑了它（做不到），只给提示。

> 端口唯一性是**跨运行时**的：同一容器的网络命名空间只有一个，nginx 应用和
> （理论上）别的进程应用也不能共用端口。所以 `port_conflict` 不按运行时过滤，
> 但要排除 **nginx 站点关**（它不 bind 任何端口，见 §3.2）。

### 4.4 nginx：每应用一份 conf

模板（**`resolver` 在 `server` 里**，这是多应用下唯一能过 `nginx -t` 的写法）：

```nginx
# wslc-panel:site:api —— 由面板生成，重新部署会整份覆盖
server {
    listen 8080;
    server_name localhost;
    resolver 127.0.0.11 ipv6=off valid=5s;   # 只有反代规则才需要

    root "/usr/share/nginx/html/api";
    index index.html;

    location / {
        try_files $uri $uri/ /index.html;
    }

    location /api/ {
        set $wslc_upstream_1 "http://backend:3000";
        proxy_pass $wslc_upstream_1$request_uri;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
    }
}
```

- 文件：`/etc/nginx/conf.d/00-wslc-panel-<app>.conf`；
- `listen` = `DeployRecord.port`（不另存）；
- `root` = `DeployRecord.target`（每个应用自己的子目录）；
- 去前缀：`location ~ ^<prefix>(.*)$ { ... proxy_pass $u/$1$is_args$args; }`
  （`PLAN-nginx-site.md` §3 实测唯一能用的写法）；
- **端口 80**：`00-` 前缀 + `localhost` 会接管 `default.conf`；两个应用都 80 时**拒绝**；
- **端口 ≠ 80**：它是该端口上唯一的 `server`，不存在 `server_name` 冲突
  （实测两个端口上同名的 `localhost` 可以共存）；
- 写入/校验/回滚沿用 `PLAN-nginx-site.md` §5.3：
  标记检查 → 备份 → `wslc container cp` → `nginx -t` → reload；
  `-t` 失败回滚并给 `emerg` 原文；reload 失败保留文件、下次生效；
- 删除一个应用：只删它自己的 conf + reload，别的应用不动；
- `filter_nginx_noise` 照旧过滤 `conflicting server name localhost ... ignored`
  （只有端口 80 的应用会有）。

**和 `PLAN-nginx-site.md` 的差异（必须改）：**

| 旧写法 | 多应用下的问题 | 改成 |
|---|---|---|
| `resolver` 写在 conf 文件顶层（http 级） | 第二个应用再写一次 → `"resolver" directive is duplicate`，**`nginx -t` 直接失败**（实测） | 写进每个 `server {}` |
| 「一个容器只允许一个站点」 | 多应用就是多个站点 | 「一个**端口**只允许一个站点」；不同端口各一份 |
| nginx 目标默认 = `web_root` | 所有应用共用目录，逐名删除串台 | `<web_root>/<app>` |
| `site_conflict()`（看 `site`） | 只看 `site` 会漏掉端口冲突 | `port_conflict()`（看所有**会 bind** 的记录） |

### 4.5 存档 / 删除 / 迁移

- **存档结构不变**：`DeployRecord` 仍是一条应用；`site: Option<DeploySite>` 仍按应用存；
  `listen` 不另存，用 `record.port`。
- **删除一个应用**：停它自己的进程 → 删它自己目录里的成员 → 删它自己的 conf → reload。
  多应用下互不影响（因为目录和 conf 都按应用隔离）。
- **删除时目录与别的应用重叠**：确认浮层里**点名**共用目录的应用，并写清
  "会删掉这几个名字；对方的同名文件也会被删"——这是给老记录（共用 Web 根）的逃生口，
  新记录不会被允许走到这一步。
- **老记录（目标 = Web 根）不自动迁移**：面板没法知道哪些文件属于谁。
  页面加一句提示/一个警告条；用户要做的是：
  1. 用「重新部署」把它的目标改成 `<web_root>/<app>`（文件会**复制**过去）；
  2. 手动删掉旧目录里的文件（或用「终端」）。
  面板**不做**移动，也不假装修好了。
- **重新部署改了目标目录**：现在 `submit_deploy` 的 `stop_first` 用的是**表单里的新目标**
  去停旧进程 —— 改了目录就停不掉旧进程（pidfile 在旧目录）。
  要改成用**记录里的旧目标**停；这是多应用/改目录场景的必修项。
- **重新部署把站点从"开"改成"关"**：不能只是"这次不写 conf" —— 旧的
  `/etc/nginx/conf.d/00-wslc-panel-<app>.conf` 还在，server 块继续生效。
  必须 `remove_site` 删掉它 + reload，记录里 `site = None`。
  （反过来"关→开"就是正常的写入；"开→开但换了端口"是覆盖同一份文件。）
  - ⚠️ 删 conf 失败时**保留旧的 `site`**（`Some ⟺ 文件在` 这条不变），
    报错让用户重试/手动删；不能记录说没有、容器里还留着。
- **`ours_target`**：目标唯一之后，它只在"同一条应用重新部署"时为真；老共享根仍然按老逻辑。
- **老共享根下的两个计数会失真**（P1）：
  - `verify_copy` 数的是"目标目录顶层条目数"，别的应用的文件会把它垫高 →
    可能漏报"这次其实没传进去"。改成**逐个成员 `test -e`** 更准；
  - 内容形态的状态列数的是 `find -type f`（含子目录），老根下会把别的应用的文件算进来。
  这两条只影响**老共享根**；新记录一应用一子目录，不受影响。

### 4.6 界面

1. **列表**：本来就一行一条；「端口」列改成
   - **站点开**（或进程型）已发布：`容器 80 · 主机 8080`（悬停看 `http://localhost:8080/`）；
   - **站点开**没发布：`8080（没发布）`（悬停看"只能容器内访问"）；
   - **nginx 站点关**：`由默认站点提供`（悬停看 `http://localhost:<80 的宿主端口>/<app>/`）；
     不要拿 `record.port` 去算 —— 站点关时它不是一个 listen 端口（见 §3.2）。
   - 端口列宽度 110 可能放不下 URL，**不硬塞**，全文走 tooltip（既有的 `cell_muted_full`）。
2. **页头**：加一行小字 `已发布端口：主机 8080 → 容器 80、主机 8081 → 容器 8080`
   （用户添加应用时知道还有哪些可用）。
3. **添加/重新部署弹窗**：§4.2 的默认值 + 已发布端口提示 + 进程型端口提示；
   nginx 的站点区（SPA/反代）保持 §4.4。
4. **共用目录警告条**（P1）：如果同一个容器的记录里有目标重叠，页面顶部说一句
   `有 2 条部署共用目录 —— 删除/更新可能互相同名文件`。
5. **建容器弹窗**的端口映射下面加一句：
   `一个应用一个容器内端口；想放多个应用就多写几个，例如 8080:80, 8081:8080`
   （host 端口不能重复，容器内端口也要互不相同）。
6. **不新增列**：应用名/形态/目标/状态/端口/操作这 6 列够用；多应用靠行数体现。

### 4.7 进程型的启动/日志

- 面板的 `start` 是 `nohup ... &`，**端口冲突不会让 `start` 返回失败**：
  进程立刻退出，状态列变成「已停止」，原因在 `app.log`。
- P0：端口唯一性 + 表单提示，挡住最常见的冲突；
- P1：打开「日志」时如果文本里有 `address already in use` / `EADDRINUSE`，
  在浮层顶部加一句 `端口被占用 —— 这个容器里另一个应用或容器主进程可能已经在用 <端口>`；
  纯函数 `classify_app_log(text) -> Option<&'static str>` 可测。
- P1：Node 的新建默认命令改成 `PORT=<端口> npm start`（应用不读 `PORT` 时无害）；
  只对 Node 做，不给 Java/Go 猜。

### 4.8 状态轮询（P1）

`refresh_deploy_status_inner` 现在是**一条部署一次 `exec`**；多应用下 N 条就是 N 次。
P1 可以按容器合成一条脚本（一个 `exec` 里跑 N 段，按哨兵切分），
把 9 秒一轮的成本从 O(N) 降到 O(1)。这不影响正确性，先不做。

---

## 5. 代码落点

| 文件 | 改什么 |
|---|---|
| `wslc-core/src/model/deploy.rs` | `suggest_app_name` / `suggest_app_port` / `published_container_ports` / `app_url` / `default_app_target`；`conf_path` / `site_conf` 把 `resolver` 放进 `server`；`filter_nginx_noise`；单测 |
| `wslc-core/src/cmd/deploy.rs` | `DeployProbe` 已有 `ports`；`apply_site` / `remove_site` 改成"每应用一份"（签名不变，`site.conf` 按应用） |
| `wslc-panel-core/src/deploys.rs` | `port_conflict` / `binding_ports` / `target_blocker`；`DeployRecord` 的 `site`（上一版） |
| `wslc-panel-core/src/state.rs` | 无结构变化；`DeployState` 已经有 `ports`、`entries` |
| `wslc-panel/src/app.rs` | `open_deploy_dialog` 三个默认值；`submit_deploy` 的校验顺序 + 用旧目标停进程；`confirm_remove_deployment` 的共用目录警告 |
| `wslc-panel/src/views.rs` | 端口列 tooltip、页头已发布端口、弹窗提示、建容器提示、共用目录警告条 |
| `docs/PLAN-nginx-site.md` | 顶部标记"多应用见本文"；`resolver` 模板改成 server 内；一容器一站 → 一端口一站 |

**不要把分配逻辑写进 `views.rs` / `app.rs`**：应用名/端口/目录的推荐值、
冲突判断、URL 计算全是纯逻辑，进 `wslc-core` / `wslc-panel-core`，CI 才测得到。

---

## 6. 验收

### 6.1 CI 能判定的

- [ ] `cargo test --workspace --exclude wslc-panel --locked --profile ci` 全绿
      （§4.1 的全部断言）
- [ ] `cargo check --workspace --all-targets --locked`（`ui` job）
- [ ] `cargo fmt --check`（`lint`）

### 6.2 必须实机的

**nginx（已实测过核心，这次点面板）**

- [ ] 建容器时发布两个端口（`-p 18080:8080 -p 18081:8081`；这两个是**容器内**的 8080/8081）
- [ ] 添加应用 A：**勾上站点**，目标默认是 `<web_root>/app`，端口默认 8080；
      部署 → `http://localhost:18080/` 是 A
- [ ] 再点「添加部署」：应用名默认 `app-2`、端口默认 8081、目标 `<web_root>/app-2`；
      **勾上站点**，部署 → `http://localhost:18081/` 是 B，A 不受影响
- [ ] 再添加一个**站点关**的 C：目标 `<web_root>/app-3`，端口格随便填；
      确认端口列显示"由默认站点提供（/app-3/）"，且**不会**因为端口和 A/B 一样被拦
      （这个容器没发布 80，所以 C 从 Windows 打不开 —— 这是预期，不是 bug）
- [ ] A 与 B 各自有 `index.html`；删 B → A 的文件和站点都还在
- [ ] B 开 SPA、`/api/` 反代；刷新子路由不 404；`/api` 到后端
- [ ] 重新部署 B 时**取消勾选站点** → 它的 conf 消失、端口不再监听、
      记录里不再有 `site`；A 不受影响
- [ ] 两个应用都填同一个端口 → 被拦，提示点名另一个应用
- [ ] 两个应用都填同一个目标目录 → 被拦（新建）
- [ ] 老记录（目标 = `<web_root>`）还在时，新建应用铺到 `<web_root>/app2`
      **能过**（迁移路径）；把目标改成 `<web_root>` 本身 → 被拦
- [ ] 老记录的 `members` 里有某个目录名时，新应用不能铺到 `<web_root>/<那个名字>`
- [ ] 另建一个容器（`-p 18082:80`），应用勾站点、端口 80 → `default.conf` 不再服务 `/`，
      `nginx -t` 只有那条预期 warning；`http://localhost:18082/` 是这个应用
- [ ] 两个应用的 conf 都有反代 → `nginx -t` 通过（证明 `resolver` 在 server 内）

**进程型（至少一个运行时）**

- [ ] 同一个 Java 容器里放两个应用（不同目录/端口，启动命令各自绑端口）：
      两个都「运行中」，日志各自独立，停一个另一个不受影响
- [ ] 故意让两个应用写同一个端口 → 提交时被拦；绕过校验后启动，状态变「已停止」，
      `app.log` 里有 `address already in use`
- [ ] 端口没发布 → 端口列说"没发布"，应用仍能在容器内跑

**通用**

- [ ] 「添加部署」时应用名留默认 —— 第二次自动是 `app-2`，不会覆盖第一条
- [ ] 重新部署改目标目录：旧进程用**旧目录**停掉（不会留下孤儿进程）
- [ ] 老记录（目标 = Web 根）删除时，确认框点名共用目录的另一个应用

---

## 7. 边界与不做

| 不做 | 理由 |
|---|---|
| 同一容器混运行时（nginx + Java） | `runtime` 是 `detect` 按镜像认定的；改成应用级要重做「通用镜像 + 运行时选择」，是另一个功能 |
| 路径式多应用（一个端口 `/app1/`、`/app2/`） | 每个 SPA 的 `base` 和 `/api` 前缀会互相打架；P1，且要先有"应用级 base path"的概念 |
| 自动迁移老记录（Web 根 → 子目录） | 面板不知道哪些文件属于哪个应用；只能提示 + 手工 |
| 把端口注入 Java/Go 的启动命令 | 每个框架的写法不同；猜错就是"部署成功但起不来" |
| 自动给容器加端口发布 | `-p` 只能建容器时给；面板能做的只是**报告**和**提示重建** |
| 一个应用多副本 / 端口池 | 这是编排，不是这个面板的定位 |
| 自动杀 `kill -9` | 现有约定：只发 `TERM`；多应用不改变这条 |

---

## 8. 风险与兜底

| 风险 | 兜底 |
|---|---|
| **老记录共用 Web 根**，删除/更新会互删同名文件 | 新建时拒绝重叠目标；重新部署只警告；删除确认点名对方；页面提示条（P1） |
| `removals_script` 删除的是"这次要铺的名字" | 目标唯一后它只在自己目录里动手；老记录不自动迁移 |
| 两个应用同端口，后启动的静默失败 | `port_conflict` 在提交时拦；P1 日志分类 |
| 进程型应用实际没绑面板分配的端口 | 表单按运行时提示；`port` 保持"信息性"；日志里看 `bind` 错误 |
| nginx `resolver` 重复 | **放进每个 `server`**（实测唯一可行）；`site_conf` 单测断言它在 `server` 内 |
| 一个应用的 conf 坏了，**下一次任何一个应用**的部署/重载都会 `-t` 失败 | `emerg` 原文里带文件名；按文件名修那一个应用。这是"共享一个 nginx 进程"的固有代价 |
| 两个 nginx 应用都 80 | `port_conflict` 拦；提示"一个端口一个站点" |
| 站点开 80 接管 `default.conf`，**站点关的应用一起不可访问** | 提交时文案说全；同一容器里既要站点关应用又要站点开 80 时，建议换非 80 端口 |
| 端口没发布导致"部署成功但外部打不开" | 提交时警告 + 端口列如实显示；提示重建容器加 `-p` |
| 站点关的 URL 假定"默认站点 listen 80 + root `<web_root>`"（官方镜像的形状） | 自定义镜像的默认 listen/root 可能不是它 —— 端口列仍按 80 算会不准。P1 从 `nginx -T` 读真实值；P0 在文案里写"按官方镜像的默认站点估算" |
| 多 JVM 共享容器内存/CPU | 建容器时的 memory/cpus 是**整容器**的；页面上说一句 |
| 状态轮询 O(N) 次 exec | P1 合并；v0.10 的闸门会串行化，不会并发炸 |
| 本机没有 Rust 工具链 | 分配/冲突/URL 全进 `wslc-core` / `wslc-panel-core`，`core` job 跑断言；界面靠 `ui` + 实机 |

---

## 9. 实施顺序（每步一个提交）

| # | 内容 | 验 |
|---|---|---|
| 1 | `suggest_app_name` / `suggest_app_port` / `published_container_ports` / `app_url` / `default_app_target` + 单测 | `core` |
| 2 | `DeployStore::port_conflict` / `binding_ports` / `target_blocker` + 单测 | `core` |
| 3 | `open_deploy_dialog` 三个默认值 + 弹窗提示（先不校验） | `ui` |
| 4 | `submit_deploy` 校验：应用名/端口/目标；端口冲突拦、目标按 §4.3 的方向规则拦（新建或改了目标）/ 只警告（目标没变且共享） | `ui` + 实机 |
| 5 | `submit_deploy` 用**旧目标**停进程（改目录场景） | `ui` + 实机 |
| 6 | nginx 每应用 conf（`resolver` 进 server）+ `PLAN-nginx-site.md` 的修订 | `ui` + 实机 |
| 7 | 端口列 URL / 页头已发布端口 / 建容器提示 / 删除共用目录警告 | `ui` + 实机 |
| 8 | P1：日志端口冲突提示、共用目录警告条、状态合并、Node `PORT` 默认 | — |

1~2 纯逻辑，不需要实机；3 以后每步按 §6.2 点对应项。

---

## 10. 一句话总结

**多应用本身不需要新架构** —— 存档、列表、启停、删除本来就是按应用的。
要补的是**三个"分配"**（名字、目录、端口）、**两个"别删错"**（目标重叠、共用目录）、
和 **nginx 的每应用 conf**（`listen` 用应用端口、`root` 用应用子目录、
`resolver` 放进 `server`）。做完之后，一个容器里放 N 个同运行时应用是同一套逻辑，
nginx 只是多一份 `server` 块。
