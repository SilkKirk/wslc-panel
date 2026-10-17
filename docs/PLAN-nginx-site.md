# PLAN —— 给 nginx 部署补上「站点 / 反向代理」

> ⚠️ **多应用（一个容器跑多个应用）见 [`PLAN-multi-app.md`](PLAN-multi-app.md)。**
> 本文是"单应用"的原始方案；多应用下它有两处**必须改**：
> §5.2 的 `resolver` 要放进各自的 `server {}`（放在文件顶层时，第二个应用再写一次
> 会 `"resolver" directive is duplicate`，**实测 `nginx -t` 直接失败**）；
> §5.5 的「一个容器一个站点」改成「一个**端口**一个站点」；
> 另外 nginx 的默认目标目录改成 `<web_root>/<app>`（本文写的是单应用时的 Web 根）。
> 下面保留原文是为了记录当时的取舍，不要照抄这三处。

> **触发问题**：「nginx 的部署管理是不是有点问题？nginx 部署的前端怎么反代到后端？」
>
> **结论：不是操作错了，是功能缺一层。** 现在的「部署管理」对 nginx 只做一件事：
> 把静态文件铺进 Web 根（重载还得用户自己点页头的「重载 nginx」）。它**没有**
> 任何 nginx 配置层，而反代要的正好是配置层。并且面板建的容器默认在 `bridge` 上，
> **容器名解析不通** —— 就算手工写 `proxy_pass http://api:8080` 也会
> `host not found in upstream`。
>
> 本文的「当前行为」都带文件/函数（行号会漂）；「能跑」的结论都标了实测环境与日期
> （`wslc 3.0.3.0`，2026-10-17，本机）。与 `PLAN-v0.10.md`（稳定性）**独立**，可单独排期。

---

## 0. 一句话方案

给 nginx 的「内容」部署加一个可选的 **站点（site）**：

- 面板生成一份 `server {}` 块，写上 **SPA 回退** 和 **一条反代规则**；
- 写进 `/etc/nginx/conf.d/00-wslc-panel-<app>.conf`；
- **`nginx -t` 过了才 reload**，过不了就把旧文件放回去、把 `emerg` 原文给用户；
- 反代上游用**变量 + `resolver`**，不用静态 `proxy_pass`（后端容器停着时
  配置校验 / 重载 / nginx 重启都不会被一个解析不了的名字拖死 —— **实测**）；
- 上游是另一个容器时，面板先检查两个容器有没有**共同用户网络**，
  没有就一键 `wslc network connect`（运行中也能接，实测）。

不做的：改端口发布（做不到）、HTTPS、多规则/负载均衡（P1）、自由编辑整份配置。

### 0.1 备选（如果你只想要更小的改动）

| 方案 | 做什么 | 代价 |
|---|---|---|
| **A（本文，推荐）** | 结构化站点（SPA + 一条反代）+ 网络预检 + `-t` 回滚 | 改动面最大；但用户不需要懂 nginx |
| **B（最小）** | 部署弹窗多一格「nginx 配置片段」，原样写进 conf.d；`nginx -t` 不过就回滚 | 用户要自己写 `location`；**默认 bridge 仍然解析不了名字**，还得手工接网络；片段写错时只能看 `emerg` |
| **C（零代码）** | 照本文附录手工做 | 面板重新部署不管它，也不校验；换机器/换容器要重来 |

B 里「多行输入」还有个现成约束：本仓库目前没有多行 `InputState` 的用法
（全仓库 grep 不到 `multi_line` / textarea），要么先验证 gpui-kit 0.7.1 支持，
要么退回「一行一条规则」的列表 —— 这会把 B 的"最小"优势吃掉一半。

---

## 1. 现状：部署管理只做了「铺文件」

| 能力 | 现状 | 证据 |
|---|---|---|
| 拷贝静态文件进 Web 根 | ✅ | `app.rs::submit_deploy` 的第 ②~⑤ 步 |
| 判断容器是不是 nginx | ✅ | `model::deploy::runtime_from_inspect` |
| 生成 / 修改 nginx 配置 | ❌ **完全没有** | `proxy_pass` / `nginx -t` 在生产代码里零命中；`conf.d` 只出现在注释与测试里 |
| 校验配置 | ❌ | `model::deploy::reload_nginx_script()` 只有 `nginx -s reload 2>&1` |
| 失败回滚 | ❌ | 没有备份、没有 `-t` |
| 容器间按名字访问 | ❌（默认网络） | 见 §3 |
| 端口发布 | ❌（只能报告） | `cmd::deploy::DeployProbe.ports` / `views::deploy_port_cell` |

### 1.1 一个容易误会的点

`AppRuntime::Nginx` 的 `kind()` 是 `Content`（`model/deploy.rs:130`）：它**不是**在说
「nginx 只能放静态文件」，而是在说「这个运行时没有我们另起的进程」。
`note()` 那句「铺进 Web 根就生效」也只描述了**文件**这一半，没描述**配置**那一半。

也就是说：**当前实现把「站点」缩成了「目录」。** 前端要打后端，缺的就是
「目录之外的那份配置」。

---

## 2. 用户实际撞到的三堵墙

### 2.1 第一堵：没有地方写 `location /api/ { proxy_pass ... }`

- 部署页的表单只有：应用名 / 来源 / 目标目录 / 启动命令（进程形态）/ 端口。
  见 `views.rs::deploy_dialog_overlay`（v0.9 起的 5 个输入框）。
- 用户唯一的出路是「更多 → 终端」自己进去写 conf，但：
  - 面板下一次「重新部署」不会碰它（好在不会覆盖），可用户也不知道面板**不**管它；
  - 写坏了 `reload` 只会拿到一句 `nginx: [emerg] ...`，页面上没有回滚。

### 2.2 第二堵：默认 bridge 不解析容器名

面板「创建容器」的网络那一格默认留空 → `wslc run` 用默认 `bridge`
（`app.rs:2327` 建 `InputState` 时的 placeholder 是「留空则用 bridge」，
`views.rs:7058-7060` 把它画出来；`app.rs:516-518` 空串就不加 `--network`）。

默认 bridge 上，容器的 `/etc/hosts` 只有**自己的容器 ID**，没有别的容器名。
所以即使手写了：

```nginx
location /api/ { proxy_pass http://api:8080; }
```

`nginx -t` 会直接：

```text
[emerg] host not found in upstream "api" ...
```

### 2.3 第三堵：端口发布在建容器时就定死了

`-p` 只在 `run` / `create` 时给（`cmd::container::RunSpec`），已存在的容器改不了。
这不是 bug，但界面必须**如实**说：`views::deploy_port_cell` 已经这么做了。

真正的问题是：**这一条会把「站点能不能从 Windows 打开」变成建容器时的选择**。
所以站点表单里的 listen 端口要默认取「容器已发布的那个容器端口」，
没发布时要明确告诉用户「站点只能在容器/其它容器里访问」。

---

## 3. 实测记录（wslc 3.0.3.0，2026-10-17）

> 全部用官方 `docker.1panel.live/library/nginx:latest`（1.31.6）在本机跑的，
> 跑完把容器和网络都删了。

| 试的 | 结果 |
|---|---|
| 同一**用户自定义**网络，`getent hosts probe-web` | `172.18.0.2 probe-web`，`rc=0` |
| 同上，从另一个容器 `curl http://probe-web/` | `HTTP/1.1 200 OK` |
| **默认 bridge**，`getent hosts probe-dflt-a` | **`rc=2`（解析不了）**；`/etc/hosts` 只有 `172.17.0.3 <容器 ID>` |
| `host.docker.internal` | **`rc=2`（不解析）** —— 别拿它当"宿主机"用 |
| 运行中的容器 `wslc network connect <net> <c>` | 立刻能按名字解析；`disconnect` 立刻失效 |
| 官方镜像 `/etc/nginx/conf.d/default.conf` | `listen 80; server_name localhost; root "/usr/share/nginx/html";` |
| 放一个 `aa-wslc-panel-site.conf` 进 conf.d（`00-` 序更前） | 它成为 80 的**默认 server**：`Host: localhost` / `Host: probe-web` / 直接 IP **都命中它** |
| 同上 `nginx -t` | 成功，但有一句 `conflicting server name "localhost" ... ignored`（**exit 0**，可忽略） |
| 坏配置 `listen abc;` + `nginx -t` | **exit 1**，`[emerg] "listen" directive is not allowed here in ...:1` |
| 坏配置 + `nginx -s reload` | **exit 1**，同一句 `emerg`；**旧配置继续服务** |
| `proxy_pass http://p4-api:80`（静态名字）+ 后端**停着** | `nginx -t` **exit 1**：`host not found in upstream "p4-api"` |
| 同上，后端**停着**时 `nginx -s reload` | 也会失败；也就是说**后端一停，重载 nginx 就不能做了**。重启 nginx 会走同一段配置解析，所以也起不来（**这一句是推的**：`-t` 实测失败，启动没单独试） |
| `set $u http://p4-api:80; proxy_pass $u$request_uri;` + 后端**停着** | `nginx -t` **exit 0**、reload **exit 0**；请求返回 **502**；后端起来后 **3 秒内自动恢复** |
| 同上，后端运行时请求 `/api/foo?x=1` | 后端收到 **`/api/foo?x=1`**（`$request_uri` 原样） |
| 去前缀：`location ~ ^/api/(.*)$ { set $u http://p4-api:80; proxy_pass $u/$1$is_args$args; }` | 后端收到 **`/foo?x=1`** ✅ |
| 去前缀的错误写法：`rewrite ... break;` + `proxy_pass $变量;`（或 `$变量$uri`） | **500 Internal Server Error** —— 别用 |
| 去前缀的另一种错误写法：`set $u http://p4-api:80/; proxy_pass $u;` | 后缀**丢了**（后端只收到 `/`）—— 别用 |
| `/etc/resolv.conf` | `nameserver 127.0.0.11`（Docker/wslc 的嵌入式 DNS）—— 变量方案要用的 `resolver` 就是它 |
| 后端**停着**时它的名字 | 解析不了（`rc=2`）—— 所以静态 `proxy_pass` 才会失败 |
| `/var/log/nginx/error.log` | 是 **`-> /dev/stderr` 的符号链接**；`tail` 它会**永久挂住**（这次实测把一条命令挂到超时）。要读 nginx 自身日志用 `wslc logs <容器>`，别 `tail` 那个文件 |
| `wslc container cp` 到 `/etc/nginx/conf.d/` | 成功，文件权限 `666`（nginx 只读，够用） |

**两个关键结论：**

1. **必须用变量 + resolver**。静态 `proxy_pass http://api:8080` 会把
   「后端停着」升级成「nginx 校验/重载/重启全失败」，这在 WSL 面板里是常态
   （用户随手停容器）。
2. **`00-` 前缀不是装饰，是承重的**：它让我们的 server 块成为 80 的默认 server，
   从而真正接管 `localhost:8080` 这类访问。代价是那句 `conflicting server name`
   警告（exit 0），不致命。

---

## 4. 目标行为（用户视角）

部署一个前端目录、**勾上**「生成站点配置」（**默认不勾**：不勾的行为和现在完全一样；
这是刻意的 —— 勾上会接管 80 端口，不能对"只想丢几个文件进去"的人静默发生）：

| 字段 | 默认 | 说明 |
|---|---|---|
| 端口（容器内） | 容器**已发布**的那个容器端口；一个都没发布时用 80 | **就是现有那一格「端口」** —— nginx 形态下它同时是站点的 `listen`；没发布时页面提示"外部打不开" |
| 单页应用（SPA） | 开 | `try_files $uri $uri/ /index.html`，刷新子路由不 404 |
| 反代路径前缀 | 空（= 不加反代） | 例如 `/api/`，必须以 `/` 结尾 |
| 上游 | 空 | 例如 `http://api:8080`；只填 `api:8080` 也能认 |
| 去掉路径前缀 | 关 | 关：后端收到 `/api/foo`；开：后端收到 `/foo` |
| 自动接通网络 | 开 | 上游是另一个容器且没有共同网络时，面板替你 `network connect` |

提交后：

1. 面板先在后台做**预检**（上游主机存在吗、两个容器共网吗、能不能连上——最后一个只警告）；
2. 再走原来的「停旧进程 → 准备目录 → 复制 → 核对 → chmod」；
3. 然后写 conf → `nginx -t` → `nginx -s reload`；
4. 存档里多一条 `site`；页面上显示**访问地址**（从 `inspect` 的端口映射算）。

失败时：

- 配置写坏 → 旧 conf 放回去、**不 reload**、错误横幅里给 `emerg` 原文；
- reload 失败（比如容器里 nginx 根本没跑）→ **保留**新 conf（下次重载/重启生效），
  但明说"已写入、没重载成功"；
- 复制都没成功 → 不碰 conf，存档保留**旧**的 `site`。
- 删除时 conf 删掉了但 reload 失败 → 记录照样删，提示"文件与配置已删，
  但重载失败（原因）—— 下次重载/重启生效"。

> **前提（应用侧）**：前端要请求**同源路径**（`/api/...`），不能把 API 基址
> 写死成 `http://localhost:3000` —— 那样请求根本不经过 nginx，反代配了也不参与，
> 还会撞 CORS。这条要写进表单的说明里。

删除部署 = 删文件 + **删掉这份 conf** + reload；只删**我们自己的**那个文件名，
别的 `.conf` 一个都不碰。

---

## 5. 方案

### 5.1 数据模型（新增，向后兼容）

放在 `crates/wslc-core/src/model/deploy.rs`（纯数据 + 纯逻辑；`wslc-panel-core`
的 `deploys.rs` 只是存档壳，不该反过来定义模型）：

```rust
/// 一条站点配置（只有 nginx 内容形态会有）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploySite {
    /// `server_name`。P0 恒为 `localhost`（见 §3 的"00- 抢默认 server"）。
    #[serde(default = "default_server_name")]
    pub server_name: String,
    /// SPA 回退。
    #[serde(default)]
    pub spa: bool,
    /// 反代规则（P0 只有 0 或 1 条，结构上留成 Vec）。
    #[serde(default)]
    pub proxies: Vec<ProxyRule>,
    /// 容器内的配置文件名。空串 = 用 `conf_path(app)` 重算。
    #[serde(default)]
    pub conf: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyRule {
    /// 例如 `/api/`（必须以 `/` 结尾）。
    pub prefix: String,
    /// 已规范化，例如 `http://api:8080`。
    pub upstream: String,
    /// 转发时去掉 `prefix`。
    #[serde(default)]
    pub strip_prefix: bool,
}
```

> **`listen` 不单独存**：nginx 的 `listen` 就用 `DeployRecord.port`
> —— 那一格本来就是"这个应用监听哪个端口"，而且端口列 / 发布状态已经靠它了。
> 再存一份只会出现"两格不一样、不知道信哪个"。

`DeployRecord` 加一个字段（**必须有 `serde(default)`**，老存档才能读）：

```rust
pub struct DeployRecord {
    // …原有字段…
    /// 站点配置。`Some` ⟺ 容器里**确实有**我们那份 conf 文件。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site: Option<wslc_core::model::DeploySite>,
}
```

> **`Some` ⟺ 文件在**：`nginx -t` 失败会回滚文件，所以这时候存档里不写 `site`；
> reload 失败但文件在，就写。这样「删除」不会去删一个不存在的文件，
> 也不会漏删一个存在的文件。

### 5.2 生成的配置（已实测）

不开反代时只要 SPA：

```nginx
# wslc-panel:site:site —— 由面板生成，重新部署会整份覆盖
server {
    listen 80;
    server_name localhost;
    root "/usr/share/nginx/html";
    index index.html;

    location / {
        try_files $uri $uri/ /index.html;
    }
}
```

开反代（**保留**前缀，后端自己带 `/api`）：

```nginx
# wslc-panel:site:site —— 由面板生成，重新部署会整份覆盖
server {
    listen 80;
    server_name localhost;
    root "/usr/share/nginx/html";
    index index.html;

    # ⚠️ resolver 必须放在 server 里，不能放文件顶层：
    # 多应用时每个文件都写一次 http 级的 resolver 会 "directive is duplicate"（实测）
    resolver 127.0.0.11 ipv6=off valid=5s;

    location / {
        try_files $uri $uri/ /index.html;
    }

    location /api/ {
        set $wslc_upstream_1 "http://api:8080";
        proxy_pass $wslc_upstream_1$request_uri;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
    }
}
```

开反代并**去掉**前缀（后端是 `/foo`，不是 `/api/foo`）：

```nginx
    # 去前缀用**正则 location**（这是实测唯一能用的写法，见 §3）
    location ~ ^/api/(.*)$ {
        set $wslc_upstream_1 "http://api:8080";
        proxy_pass $wslc_upstream_1/$1$is_args$args;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
    }
```

要点：

- `resolver` 只在**有反代规则、且上游不是 IP** 时加；地址取容器
  `/etc/resolv.conf` 的第一个 `nameserver`，拿不到时回退 `127.0.0.11`
  （本机实测值）。**位置在 `server {}` 里**，不能放文件顶层（多应用会重复，见
  [`PLAN-multi-app.md`](PLAN-multi-app.md) §2）。
- 前缀字符集限死 `[A-Za-z0-9/_-]` 且必须以 `/` 结尾 —— 于是正则不用转义，
  也堵住配置注入。
- **只匹配 `<前缀>` 开头的请求**：`/api`（不带尾斜杠）本身**不会**命中
  `/api/` 这个 location（nginx 的语义，两种写法都一样）。表单里写清这一点；
  真要处理裸 `/api` 就再加一条 `location = /api { return 308 /api/; }`（P1）。
- 上游校验：`http(s)://host:port` 或 `host:port`（补 `http://`）；
  host 只允许 `[A-Za-z0-9._-]`，port 1~65535。**不允许**空格、`;`、`{}`、`$`、
  换行 —— 这些能直接拼出第二条配置。
- `root` 要**加双引号**（`root "/mnt/e/my site";`）：目标目录是从挂载点推出来的，
  完全可能带空格；顺手把 `\` 和 `"` 转义掉。带空格的 root 要有单测。
- `try_files` 的最后一项 `/index.html` 在文件不存在时返回 404，不会循环。

### 5.3 写入 / 校验 / 回滚

`cmd::deploy::apply_site(wslc, container, site, root) -> Result<ApplySite>`：

```rust
pub struct ApplySite {
    /// 最终有没有把新 conf 留在 conf.d 里（= `nginx -t` 过没过）。
    pub applied: bool,
    /// 给界面的额外一句话（reload 失败、名字解析警告被吞掉等）。
    pub warning: Option<String>,
}
```

步骤（**每一步的失败都要能回到"用户原来的站点还在"**）：

1. **标记检查**：`head -n 1 <conf>`。文件存在、首行不是
   `# wslc-panel:site:<app>` → **拒绝覆盖**，报"这个文件不是面板生成的"。
   这是「只动我们自己的东西」那条原则在配置文件上的落地。
2. **备份**：`if test -f <conf>; then cp -f <conf> /tmp/wslc-panel-<app>.conf.bak; fi`
3. **写新文件**：本地临时**目录**里写一个**同名**的 `.conf`（`wslc container cp`
   落的是文件名），再 `wslc container cp` 进 `/etc/nginx/conf.d/`；
   写完删本地临时目录。
4. **`nginx -t 2>&1`**（exit code 1 → 错误里带 `emerg` 原文）：
   - 失败：`cp -f <bak> <conf>`（没有 bak 就 `rm -f <conf>`），再 `nginx -t` 确认，
     **不 reload**，返回 `applied = false` + 原文；
   - 成功：继续。
5. **`nginx -s reload 2>&1`**：
   - 失败：**不回滚**（文件是合法的；失败通常是容器里 nginx 没跑），
     返回 `applied = true, warning = Some(原文)`；
   - 成功：`rm -f <bak>`，返回 `applied = true`。

> ⚠️ **不要 `tail /var/log/nginx/error.log`**：官方镜像里它是 `-> /dev/stderr`
> 的符号链接，`tail` 会**永久挂住**（本机实测把一条命令挂到 120 秒超时）。
> 要看 nginx 自己的日志用 `wslc logs <容器>`；要看配置错误看 `nginx -t` 的 stderr。

`cmd::deploy::remove_site(wslc, container, conf)`：同样的标记检查 → `rm -f` →
`nginx -t` → reload。`conf` 只接受
`/etc/nginx/conf.d/00-wslc-panel-*.conf` 这个形状（**不信存档里的任意路径**，
存档是能手改的）。

> ⚠️ `conflicting server name "localhost" ... ignored` 是**预期警告**（§3），
> 但它每次 `-t` / reload 都会出现在输出里，而页头的「重载 nginx」现在会把
> 输出原样弹成 toast（`app.rs::reload_nginx_in_container`）。加一个纯函数
> `filter_nginx_noise(text) -> String` 把这行过滤掉（`signal process started`
> 留着，它证明信号真的发出去了），`apply_site` 的 warning 和那个 toast 都用它。
> 这条要带单测：只删这一种已知噪音，别把真正的 `emerg` 删掉。

### 5.4 网络可达性

**预检**（`submit_deploy` 的后台任务开头）：

1. 解析每条规则的上游 host；是 `localhost` / IP → 不检查网络；
2. 否则按**容器名**在 `snapshot.containers` 里找：
   - 找到 → `inspect` 那个容器，取它的网络；
   - **找不到 → 不拦**，按"外部地址"处理（可能是别的机器 / 外网域名），
     只给一句提醒：容器里的 `resolver` 只认容器网络和它自己的上游 DNS，
     解析不了时请求会 502；真名打错时也一样。**不猜、不改地址**；
3. `shared_dns_network(a, b)`：两边网络名的交集，**排除** `bridge` / `host` / `none`
   （实测默认 bridge 不解析名字）；
4. 没有交集：
   - 「自动接通网络」勾着 → 挑一个**能用的用户自定义网络**：
     优先 nginx 容器已有的非内置网络，其次后端容器已有的，
     都没有就 `network ensure wslc-panel` 建一个；
     然后把**不在这个网络上**的那个容器 `network connect` 进去；
     **不断开任何东西**；
   - 没勾 → 报错，并把两条命令原样写出来，让用户自己接。
   - ⚠️ 后端容器**没在运行**时 `network connect` 会失败（运行中才能接，实测）：
     这时**在预检就停**，提示"后端容器 xxx 没在运行 —— 先启动它，再接一次"。
     不能带着"名字能解析但其实没接上"的状态往下走。

`cmd::network` 现在只有 `list` / `attachable_names` / `remove`，要补：

```rust
pub fn create(wslc: &Wslc, name: &str) -> Result<()>;
pub fn ensure(wslc: &Wslc, name: &str) -> Result<()>;   // 有就跳过
pub fn connect(wslc: &Wslc, network: &str, container: &str) -> Result<()>;
pub fn exists(wslc: &Wslc, name: &str) -> Result<bool>;
```

> `attachable_names()` 现在把 `bridge` 也算进"可连接"（`network.rs:14-21`）——
> 对我们的用途要**再排除 `bridge`**，否则会把两个容器接到一个不解析名字的网络上，
> 用户看到"接上了"却还是 `host not found`。

**可达性探测**（best-effort，只产生**警告**，不拦部署）：

在 nginx 容器里 `curl -sS -o /dev/null -w '%{http_code}' --max-time 3 <upstream>/`：
- 拿到任何 HTTP 码（含 404）→ 通；
- exit 6/7（解析/拒绝）→ 警告"名字能解析/网络已接通，但连不上 —— 后端可能没启动，
  或只监听了 127.0.0.1"；
- exit 127（没有 curl）→ 跳过，不吭声。
  （官方 nginx 镜像有 `/usr/bin/curl`，实测；别的镜像没有就跳过。）

### 5.5 记录 / 删除 / 重新部署

- **重新部署**：表单从 `record.site` 预填；同名应用覆盖**同一份** conf（先备份）。
- **删除**：原来的「停进程 → 删目标目录里我们铺的名字」之后，加一步
  「`remove_site` → `rm` 那份 conf → `nginx -t` → reload」。
  删除确认浮层要写出 conf 路径。
  - ⚠️ 文件删掉了、`remove_site` 却失败（比如容器刚停）时：**记录照样删掉**，
    提示「文件已删，但站点配置没删掉（原因）—— 用「终端」删掉 `<路径>` 再重载」。
    留一条指向空目录的记录比这更糟。
- **删除记录**（运行时键认不出来的那一行）：**不动** conf —— 它现在写着
  "容器里一个字都不动"，保持一致。
- **一个端口只允许一个站点**（多应用见 [`PLAN-multi-app.md`](PLAN-multi-app.md)）：
  同一个 `listen` 上两个 `server_name localhost` 的块，只有一个能生效（`00-` 字典序在前的），
  另一个会被 `conflicting server name ... ignored` 吞掉，页面看着"配了但不生效"。
  **不同端口**上同名的 `localhost` 可以共存（实测），所以多应用 = 每应用一个端口。
  提交前用 `DeployStore::port_conflict` 检查同一容器的其它记录：
  「这个端口已经给『xxx』了 —— 一个应用一个端口」。
  （端口 80 仍然特殊：它和 `default.conf` 抢默认 server，见 §3。）
- **旧版本读新存档**：`serde` 默认忽略未知字段，旧版本重新部署会**丢掉** `site`。
  README / 升版说明里写一句：升上来之后别再用旧版本改同一个容器的部署。

### 5.6 界面

1. **弹窗**（`views.rs::deploy_dialog_overlay`，只 nginx 显示）：
   - 一个 `Checkbox`「生成站点配置（反代 / SPA）」；
   - 勾上才**渲染**下面这些（项目不用 `.disabled()`）：
     SPA / 反代前缀 / 上游 / 去前缀 / 自动接通网络；
   - 现有的「端口（信息性）」那一格在 nginx 形态下改叫「端口 / 站点 listen」，
     并默认取**容器已发布的容器端口**（`published_container_port`）；
   - 文案：一句"这会接管容器的这个端口；不勾就和现在一样只铺文件"；
   - 弹窗加 `.max_h(px(720)).overflow_y_scroll().id(...)`
     （滚动容器**必须有 `.id()`**，这是编译期约束）。
2. **端口列**（`deploy_port_cell`）：已发布时显示
   `80 → http://localhost:8080`（`site_url(ports, record.port)` 算），没发布时保留
   现在那句「没发布到主机」。
3. **目标目录列**的 tooltip 追加站点摘要（不新增列）：
   `站点 80 · SPA · /api/ → http://api:8080`。
4. **行内「站点配置」**按钮（只有 `record.site.is_some()` 的行有）：
   只读浮层，`cat` 容器里那份 conf（看到的是**实际生效的文件**，不是我们存的字符串）。
   ⚠️ 浮层顺序：它排在删除确认**之前**（确认框永远在最后）。
5. **删除确认**：多一行 `站点配置：/etc/nginx/conf.d/00-wslc-panel-site.conf（会一起删掉）`。
6. **nginx 页面的空状态**加一句：`要让前端调到后端，添加部署时勾上「生成站点配置」。`
7. `DeployPhase` 加一档 `Configuring`（"正在写入站点配置…"），
   因为 `-t` + reload 可能是秒级，不能让它看起来卡住。
8. **同容器里还有别的部署时**，勾上站点要在表单里说一句：
   `这个容器里还有 N 条部署：站点会把 80 端口指向这一条的目录，别的目录不再由默认站点提供。`
   （有些用户是把多个静态包铺在同一个 Web 根的不同子目录里 —— 那种仍然可用，
   但要知道 `/` 现在归这一条。）

### 5.7 `submit_deploy` 的新顺序

```text
0. 同步校验（应用名 / 来源 / 目标 / 端口 / 站点字段 / 上游 / 前缀 / 一容器一站）
1. 【后台】网络预检 + 自动接通
2. 【后台】可达性探测（只警告）
3. 停旧进程（进程形态、重新部署）
4. 准备目标目录
5. 逐个顶层条目复制
6. 核对
7. chmod +x（只有 Go）
8. 【后台】apply_site：写 conf → nginx -t → reload（内容形态 + 勾了站点）
9. 启动（进程形态）
10. 写存档（成功 / 失败两条路都要正确带 `site`）
```

为什么网络预检在**复制之前**：几百 MB 复制完再报"名字解析不了"太亏；
而 `network connect` 是可逆、无破坏的（不加不减端口，只多一个网络端点）。

**预检失败时表单必须还在。** `submit_deploy` 现在第一件事就是
`self.deploy_form = None`（`app.rs:4569`）；如果在后台才做网络预检，
失败时用户得把整张站点表单重填一遍（而这一格刚填了前缀、上游、端口）。
所以顺序要改成：

1. 同步校验 + 格式校验 → 失败：不关表单、焦点送回那一格（和现在一样）；
2. **表单保持打开**，后台跑网络预检（`inspect` 不能上界面线程）；
3. 预检过了 → 取走值、关表单、继续长任务；没过 → 清掉"预检中…"、
   在弹窗里显示原因，**用户改一格就能再点一次**。

这一步会改变 `submit_deploy` 的形状（从"同步校验 → 立刻 spawn"变成
"同步校验 → spawn 预检 → 再 spawn 长任务"），要在实现时单独一个提交。

为什么 conf 在**复制之后**：`root` 指向刚铺好的目录；先写配置会在复制失败时
留下一个指向空目录的站点。

`DeployPhase::Uploading` 盖住 3~7；8 用 `Configuring`；9 用 `Starting`。

存档里的 `site` 怎么取（**别写成"永远等于表单"**）：

| 走到哪一步失败 | 存档里写什么 |
|---|---|
| 预检 / 复制 / 核对 / chmod 失败 | **旧**的 `site`（重新部署前的那份；新部署就是 `None`）—— 容器里的 conf 没被碰 |
| `apply_site` 返回 `applied = false`（`-t` 没过，已回滚） | **旧**的 `site` |
| `apply_site` 返回 `applied = true`（含 reload 失败） | **新**的 `site`（文件确实在 conf.d 里） |

---

## 6. 代码落点（按改动量排）

| 文件 | 改什么 |
|---|---|
| `wslc-core/src/model/deploy.rs` | `DeploySite` / `ProxyRule`；`conf_path`、`site_conf`、`normalize_upstream`、`valid_proxy_prefix`、`upstream_host`、`site_url`、`published_container_port`、`first_nameserver`、`shared_dns_network`、`filter_nginx_noise`；**全部带单测**；在 `model/mod.rs` re-export |
| `wslc-core/src/cmd/deploy.rs` | `DeployProbe.networks`；`read_resolv_conf`、`read_conf_first_line`、`apply_site`、`remove_site`、`read_site_conf`、`probe_http` |
| `wslc-core/src/cmd/network.rs` | `create` / `ensure` / `connect` / `exists`（`attachable_names` 的 `bridge` 语义另说） |
| `wslc-panel-core/src/deploys.rs` | `DeployRecord.site`（`serde(default)`）+ 往返 / 老档兼容测试；`port_conflict`（它要看记录，只能住这一层 —— `wslc-core` 不能反向依赖 `wslc-panel-core`） |
| `wslc-panel-core/src/state.rs` | `DeployState.networks`；`DeployForm` 相关的纯值类型；`DeployConf` 浮层状态 |
| `wslc-panel/src/app.rs` | `DeployForm` 新字段；`open_deploy_dialog` 预填；`submit_deploy` 新顺序；`confirm_remove_deployment` 加 `remove_site`；`open/close_site_conf`；`reload_nginx_in_container` 用 `filter_nginx_noise` |
| `wslc-panel/src/views.rs` | 弹窗站点区、端口列、行内按钮、只读浮层、删除文案、空状态 |
| `docs/PLAN-v0.9.md` / `AGENTS.md` §7.3 | 把 §3 的实测补进去（尤其是"默认 bridge 不解析"和"静态 upstream 会拖死重载"） |

**纯逻辑不要写进 `wslc-panel`**（CI 不跑那个 crate 的测试）；**`cmd::deploy`
保持零测试**（它只拼参数 + 起进程），所有判据都在 `model`。

### 6.1 关键纯函数与测试点

```rust
// wslc-core/src/model/deploy.rs（纯逻辑，wslc-core 的单测跑）
pub fn conf_path(app: &str) -> String   // /etc/nginx/conf.d/00-wslc-panel-<app>.conf
pub fn site_conf(site: &DeploySite, root: &str, listen: u16, resolver: Option<&str>) -> String
pub fn normalize_upstream(raw: &str) -> Result<String, String>
pub fn valid_proxy_prefix(raw: &str) -> Result<String, String>
pub fn upstream_host(upstream: &str) -> &str
pub fn site_url(ports: &PortMap, listen: u16) -> Option<String>
pub fn published_container_port(ports: &PortMap) -> Option<u16>
pub fn first_nameserver(resolv_conf: &str) -> Option<String>
pub fn shared_dns_network(a: &[String], b: &[String]) -> Option<String>
pub fn filter_nginx_noise(text: &str) -> String

// wslc-panel-core/src/deploys.rs（要看存档记录，只能在这一层）
impl DeployStore {
    /// 同一容器里别的应用已经占了这个端口吗？（多应用见 PLAN-multi-app.md）
    pub fn port_conflict(&self, container: &str, app: &str, port: u16) -> Option<&str>
}
```

测试至少覆盖：

- `site_conf` 的**逐行断言**：`listen` / `server_name` / `root`（含带空格的路径要加引号）/
  `try_files` / `proxy_pass $wslc_upstream_1$request_uri` / 去前缀那条正则；
- `filter_nginx_noise`：去掉 `conflicting server name ... ignored`，
  保留 `signal process started` 和 `[emerg] ...`；
- 注入：`normalize_upstream("http://a:1; } server { listen 9;")` → `Err`；
  `valid_proxy_prefix("/api;")` → `Err`；`/api`（不带尾斜杠）→ `Err`；
- `shared_dns_network`：`["bridge"]` vs `["bridge"]` → `None`；
  `["bridge","web"]` vs `["web"]` → `Some("web")`；
- `site_url`：`[("80/tcp", [("0.0.0.0","8080")])]` → `http://localhost:8080`；
  空 bindings → `None`；
- `DeployRecord` 老 JSON（没有 `site`）能读、`site=None`；带 `site` 能往返；
- `DeployStore::port_conflict`：别的应用同端口 → 拦；端口不同 → 放行；
  同一个应用重新部署 → 不拦。

---

## 7. 边界与不做

| 不做 | 理由 |
|---|---|
| 改端口发布 / 重建容器 | `-p` 只能在 `create` 时给；重建容器是另一个危险动作，不在这一版 |
| HTTPS / 证书 | 需要证书来源、ACME、80/443 双端口；独立一版 |
| 多条反代 / upstream 负载均衡 / 自定义 header | P1；先把"一条能跑通"做对 |
| 自由编辑整份 conf | 会把 `-t` 的失败面放大到"任意 nginx 语法"，用户拿到的是 `emerg` 而不是我们的校验；P2 |
| 把后端起在 nginx 容器里 | 部署页是**按容器**的，一个容器一个运行时；那是「终端」的事 |
| `host.docker.internal` | 本机实测**不解析**（rc=2），不能当"宿主机"卖点 |
| 自动 `disconnect` 网络 | 只加不减；断开可能影响用户别的用途 |

---

## 8. 风险与兜底

| 风险 | 兜底 |
|---|---|
| `00-` 抢 80 的默认 server（`default.conf` 不再服务 `/`） | 这就是「站点」的语义；删除站点即恢复。UI 上写明"会接管 80 端口" |
| `conflicting server name "localhost"` 警告 | exit 0，不拦；注释里写明这是预期的，别把它当错误弹给用户 |
| `conf.d` 是只读 bind mount | `cp` 会 `ERROR_PATH_NOT_FOUND` / 拒绝；错误里带路径，并提示用「终端」改挂载 |
| 镜像把 `conf.d` 放在别处（少数 openresty / tengine 变体） | P0 固定 `/etc/nginx/conf.d`，`cp` 失败会把路径原样报出来；P1 再给一个可改的「配置目录」（默认值不变） |
| 用户自己有一份同名的 `00-wslc-panel-*.conf` | 首行标记检查 → 拒绝覆盖 |
| `conf.d` 里已经有排在我们前面的 `00-*.conf`（别的工具写的） | 那个才是默认 server，我们的站点对 `localhost` 不生效，且 nginx 只会给 warning。P1 在写入前 `ls conf.d/*.conf` 比一遍文件名；P0 至少在文档里写明"`00-wslc-panel-<app>.conf` 要是字典序第一个" |
| 用户手改过我们生成的 conf | 重新部署整份覆盖（注释里已声明）；想保留就别改面板管的文件 |
| `nginx -t` 被**用户自己**的坏配置拖失败 | 原样显示 `emerg`（含文件名）；我们的文件回滚，用户的文件不动 |
| `network connect` 改变容器路由 | 只新增网络端点；优先用已有网络；不断开任何东西；操作前在文案里说清 |
| 后端只监听 `127.0.0.1` | 可达性探测给警告；文案直接点名这个可能 |
| reload 失败但 conf 是合法的 | 保留 conf（下次重载/重启生效），提示"已写入、没重载成功" |
| 旧版本读新存档丢 `site` | 升版说明写清；不降级改部署 |
| 本机没有 Rust 工具链 | 纯逻辑全部进 `wslc-core` / `wslc-panel-core`；`core` job 跑断言；`ui` job 编 |
| 界面没有实机验证 | 这一版必须实机点：部署 → 刷新子路由 → 停后端 → 重载 → 删站点（见 §9） |

---

## 9. 验证

### 9.1 CI 能判定的

- [ ] `cargo test --workspace --exclude wslc-panel --locked --profile ci` 全绿（§6.1 的断言）
- [ ] `cargo check --workspace --all-targets --locked`（`ui` job）过
- [ ] `cargo fmt --check`（`lint` job，非阻塞）

### 9.2 必须实机的

准备：一个 nginx 容器（`-p 8080:80`）、一个后端容器（Node / Java 均可），
**先都放在默认 bridge 上**（故意不共享网络）。

- [ ] 部署前端（勾站点、`/api/` → `http://<后端>:<port>`、自动接通网络）：
      toast 里能看到"已把两个容器接到网络 …"；`wslc network inspect` 里两个容器都在
- [ ] `http://localhost:8080/` 是前端首页
- [ ] `http://localhost:8080/orders/42`（SPA 子路由）刷新不 404
- [ ] `http://localhost:8080/api/<真实接口>` 通，后端日志里能看到请求
- [ ] **停掉后端** → `/api` 返回 502；`nginx -t` / 「重载 nginx」**仍然成功**；
      启动后端 → 5 秒内恢复（`valid=5s`）
- [ ] 取消「自动接通网络」、两个容器仍不共网 → 部署**在预检就失败**，
      提示里有 `wslc network connect` 两条命令
- [ ] 故意填坏上游（`http://api:1; } server {`）→ 表单拦下，不发命令
- [ ] 手动把 conf 改成非法（比如删掉 `}`）→ 点「重新部署」：
      旧站点仍然能访问，横幅里是 `emerg` 原文
- [ ] 删除部署 → conf 文件没了、`default.conf` 的页面回来、reload 成功
- [ ] 同一容器再添加第二个站点 → 被拦下，文案说"一个容器一个站点"
- [ ] nginx 容器的 80 **没**发布到主机 → 端口列说"外部访问不了"，
      站点在容器内（从后端容器 curl）仍然工作
- [ ] `wslc logs <nginx 容器>` 里**没有** `host not found in upstream`
      （变量方案的证明）

---

## 10. 实施顺序（每步一个提交，都能独立回滚）

| # | 内容 | 验 |
|---|---|---|
| 1 | `DeploySite` / `ProxyRule` + 全部纯函数 + 单测（**不接线**） | `core` job |
| 2 | `cmd::network` 的 `create` / `ensure` / `connect` / `exists` | `core` job |
| 3 | `DeployRecord.site`（serde 兼容测试） | `core` job |
| 4 | `cmd::deploy`：`networks` / `read_resolv_conf` / `apply_site` / `remove_site` | `core` + `ui` |
| 5 | `DeployForm` 站点字段 + 弹窗渲染（先不接提交） | `ui` job |
| 6 | `submit_deploy`：预检 + 写 conf + 存档 + 失败语义 | `ui` job |
| 7 | 删除流程 + 「站点配置」只读浮层 + 端口列 | `ui` job |
| 8 | 文案 / 空状态 / `AGENTS.md` / `PLAN-v0.9.md` | — |

1~3 是纯逻辑，**不需要实机**；4 以后每步都要在 §9.2 里点一遍对应的那几项。

---

## 11. 要同步更新的文档

- `AGENTS.md` §7.3 的实测表：加「默认 bridge 不解析容器名」「静态 upstream 会让
  `nginx -t`/reload 在后端停止时失败」「`/var/log/nginx/*.log` 是 `/dev/stderr`，
  别 `tail`」；
- `docs/PLAN-v0.9.md` 的部署那一节：把「部署 = 铺文件」改成「部署 = 铺文件 +
  （可选）站点配置」；
- `README.md`「已知限制」：端口发布只能在建容器时给；旧版本读新存档会丢 `site`；
- 本文末尾的「手工做法」在功能上线后可以删掉，或者改成"面板内部就是这么做的"。

---

## 附：今天就能用的手工做法（功能上线前）

```powershell
$wslc = "$env:ProgramFiles\WSL\wslc.exe"
$img = "docker.1panel.live/library/nginx:latest"

# 0) 先建网络（--network 要求它已经存在）
& $wslc network create webnet

# 1) nginx 容器建的时候就发布 80（已建好的补不了）
& $wslc create --name web -p 8080:80 --network webnet $img
& $wslc start web

# 2) 后端容器（示例叫 api，已经存在/正在运行）也接进同一个网络
#    默认 bridge 不解析名字，所以这一步不能省
& $wslc network connect webnet api

# 3) 在 Windows 上写 00-site.conf（LF 换行，不要 BOM），拷进去
& $wslc container cp .\00-site.conf web:/etc/nginx/conf.d/

# 4) 先校验，再重载
& $wslc exec web nginx -t
& $wslc exec web nginx -s reload
```

配置内容照 §5.2 抄。**关键：上游用变量 + `resolver`**；直接写
`proxy_pass http://api:8080` 的话，后端一停，`nginx -t` / 重载 / 重启 nginx
就全都失败（§3 实测）。
