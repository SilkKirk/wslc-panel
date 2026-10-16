# PLAN v0.9 —— 容器里的应用部署管理

> 这一版的主题和 v0.7 的「数据库管理」是同一个路子：**面板不只管容器，还管
> 容器里跑的那套东西**。起点是一个很具体的需求 —— 认得出是 nginx / Java /
> Node.js / Go 的容器，在列表行上多一个「部署管理」按钮，点进去能**添加部署**、
> **看已部署列表**、并且能把它启动 / 停止 / 看日志。
>
> 全部机制结论都是**本机实测**出来的（`wslc 3.0.3.0` + 真实的 nginx 容器
> `docker.1panel.live/library/nginx:1.31.6`），下面 §1 那张表就是地基。
> 凡是没有实测、只能推的，正文里会写明"**这是推的**"。

---

## 1. 一条实测表（方案的地基）

| # | 试的 | 结果 |
|---|---|---|
| 1 | `wslc --help` 的**顶层命令表** | 里面**没有 `cp`** —— 和文件搬运有关的只有 `export`（整机导出 tar）和 `build` / `import` / `save` / `load`（都是**镜像级**的）。⚠️ **我照着这张表下过结论"没有 cp"，那是错的** —— 见第 24 条 |
| 2 | `wslc exec --help` | **有 `-i/--interactive`**（原文："附加到 Stdin 并保持其打开状态"）、`-e/--env`、`-u/--user`、`-w/--workdir`、`-d/--detach` |
| 3 | `'hello' \| wslc exec -i nginx sh -c 'cat > /tmp/probe.txt'` | exit 0，读回来内容原样 |
| 4 | 容器里 `ls /mnt` | **是空的** —— 容器看不见 Windows 的盘。所以"把宿主目录挂进去"这条路对**已存在的**容器不成立 |
| 5 | Windows 侧 `tar` 1 MB 随机二进制 + 嵌套目录 → 重定向进 `wslc exec -i … tar -xf -` | exit 0，**sha256 与本地完全一致**（`68363f11…`），三层目录结构保留 |
| 6 | `tar -cf - . \| wslc exec -i …`（**直接管道**，不经临时文件） | 同样 exit 0、sha256 一致 —— 流式那条路**也是通的** |
| 7 | `wslc exec -d nginx sh -c "sleep 5"` | **耗时 5.08 秒** —— `-d` 在 `exec` 上**不生效**，它照样等命令结束 |
| 8 | `wslc exec nginx sh -c 'nohup sleep 300 >/tmp/n.log 2>&1 & echo ok'` | **0.17 秒返回**，2 秒后扫 `/proc/*/cmdline` 那个 `sleep 300` **还活着** |
| 9 | 起进程 → 写 pidfile → `kill -0 $(cat pid)` 判活 → `kill` 停 | 三步全通：写入 pid → `RUNNING` → `STOPPED` |
| 10 | `mkdir -p D && cd D && nohup C & echo $! > D/pid` | **失败**：`&` 优先级最低，`echo $! > pid` 被甩到后台组**之外**，于是 pidfile 的写入早于 `mkdir`（实测报 `cannot create …: Directory nonexistent`）。必须写成 `mkdir -p D && { cd D && nohup C & echo $! > D/pid; }` |
| 11 | nginx 镜像（`nginx:1.31.6` / debian）里的工具 | `tar` `/usr/bin/tar`、`base64`、`gzip`、`tail`、`mkdir`、`timeout` **都有**；**`ps` 和 `pkill` 没有** |
| 12 | 没有 `ps` 时怎么看进程 | 读 `/proc/[0-9]*/cmdline` 可用 —— 它不依赖镜像里装了什么包 |
| 13 | 那个 nginx 容器的 `inspect` | `Mounts` 里有 `{Type: volume, Name: webdata, Destination: /usr/share/nginx/html}`；`Ports` 里有 `80/tcp → 127.0.0.1:80` |
| 14 | `wslc exec nginx nginx -s reload` | exit 0，stderr 一行 `signal process started` |
| 15 | 容器**没运行**时 `wslc exec -i … < nul` | exit 1，`容器"…"未运行。` + `错误代码： WSLC_E_CONTAINER_NOT_RUNNING` |
| 16 | Windows 侧 tar 在哪 | `C:\WINDOWS\system32\tar.exe` —— **系统自带**，不用额外装 |
| 17 | `wslc list` 的 `Command` 字段 | 又被截断成 20 字符（nginx 那行是 `"/docker-entrypoint.…"`）→ **靠命令认不出 nginx**（和 v0.7 §1 里 mysql 那条是同一个坑） |
| 18 | Windows 侧 `tar` 打包 → 容器里解开，看**权限位** | 全是 **`666`**（`ls -l` 给 `-rw-rw-rw-`）。Windows 的 tar **不保留可执行位** —— 所以任何"要被执行的文件"（Go 二进制、shell 脚本）解出来**跑不起来**：实测 `Permission denied`、**exit 126** |
| 19 | 在解包那条命令尾巴上补一句 `chmod +x`，然后把整套脚本**逐条**跑一遍：解包 → 启动 → 判活 → 看日志 → 停止 → 再判活 | **全部通过**：权限变 `777`；pid `470`；`__RUNNING__`；日志里 stdout 和 stderr **两行都在**；`__STOPPED__` |
| 20 | 那个 nginx 容器的 Web 根**是不是挂载点** | **是**。`/proc/mounts` 里有 `/dev/sdc /usr/share/nginx/html ext4`，`stat` 给的设备号 2080 和根目录的 35 **不是一个**。也就是说站点内容落在一个**卷**上 |
| 21 | 对那个挂载点 `rm -rf /usr/share/nginx/html` | **exit 1**，`rm: cannot remove '/usr/share/nginx/html': Device or resource busy`。它**确实清空了内容**，但退出码是 1 |
| 22 | 换成 `mkdir -p X && find X -mindepth 1 -delete && tar -xf - -C X`（X 就是那个挂载点） | **exit 0**；隐藏文件（`.hidden`）也清掉了；512 KB 二进制的 sha256 与本地一致；没有多套一层目录 |
| 23 | 那条 `find` 会不会**递归**删子目录 | 会。实测 `a/b/c.txt`、符号链接、顶层文件全部清掉，`X` 自己留着，exit 0 |
| 24 | `wslc container cp --help`（**子命令那一层**） | **有 `cp`**！"在容器和本地文件系统之间复制文件"，支持本地→容器、容器→本地、以及 `-` 标准输入。⚠️ `wslc cp`（不带 `container`）报「无法识别的命令」；而**顶层 `wslc --help` 的表里根本没列它** —— 这就是我第 1 条误判的来源 |
| 25 | `wslc container cp <文件> <容器>:/tmp/`（`/tmp` **已存在**） | exit 0，落在 `/tmp/<文件名>` |
| 26 | `wslc container cp <文件> <容器>:/tmp/不存在的名字` | exit **1**，`ERROR_PATH_NOT_FOUND` —— 它**不会替我们建目录** |
| 27 | `wslc container cp <目录> <容器>:/tmp/已存在目录` | exit 0，**递归**复制（隐藏文件 `.hidden` 也在），但**多套一层目录名**（落到 `/tmp/d1/site/…`）。`-a/--archive` 和不加它**结果一样**（帮助里写着"为 Docker CLI 兼容性而接受"） |
| 28 | 8 MB 二进制经 `cp` 过去 | exit 0，**sha256 与本地完全一致**；文件权限 **`666`**（目录倒是 `777`） |
| 29 | ⚠️ `wslc container cp <文件> <**没运行**的容器>:/tmp/` | **exit 0、什么都不做、一个字都不输出** |

几条定调子的：

- 第 **1 / 24** 条是这一版**最该记住的教训**：查一个 CLI 的能力**要看子命令那一层**，
  别只看顶层表 —— 顶层表漏了 `cp`，而我据此写了半屏"没有 cp 所以只能灌 tar"的
  论证，方向整个跑偏；
- 第 **4** 条决定了"搬文件"不能靠宿主目录挂载（已存在的容器加不了挂载）；
- 第 **7** 条决定了**不能**用 `wslc exec -d` 起应用 —— 它会挂住调用方；
- 第 **10** 条是个纯粹的坑，但它值得单独列出来：它错得很安静（命令"成功"了，
  只是 pidfile 没写上），是我自己在实测里踩到的；
- 第 **13 / 20** 条合起来解释了为什么 nginx 是**内容形态**：站点目录挂在卷上，
  而 nginx 本来就是容器的主进程。

第 **18** 条是这一版最值钱的一条：不处理它，Go 那条路（以及任何"部署一个可执行
文件"的用法）会稳定地失败在 `Permission denied` 上，而**部署本身是"成功"的**
—— 文件确实在、大小也对，只有"跑"这一步不行。这正是 `AGENTS.md` §5 说的
"错了只表现为界面上什么也没发生"那一类。第 19 条是它的解药，而且**整条
生命周期是实测跑通的**，不是推的。第 28 条说明换成 `cp` 之后**这个坑还在**
（`cp` 给的是 `666`），所以 `chmod +x` 那一步留着了。

第 **20~23** 条是**方案定稿之后才测出来的**，它们直接推翻了我第一版写的脚本
（`rm -rf <目标> && mkdir -p <目标> && tar …`）：`rm -rf` 在挂载点上返回 1，
于是 `&&` 链**断在第一步**。而"先删目录再 `mv` 新的上去"那条路在挂载点上
**也是错的**：挂载点删不掉，`mv` 会把目录**套进**挂载点里。最后落到
`find -mindepth 1 -delete`（只清内容、不碰挂载点本身）。

第 **29** 条是最后一批测出来的，它改的是**错误处理的形状**：`cp` 在容器没运行时
静默成功，所以流程必须 ① 先跑一条 `exec`（那条会老实报
`WSLC_E_CONTAINER_NOT_RUNNING`），② 复制完**数一遍**核对。光看 `cp` 的退出码
会得到"部署成功、容器里空空如也"。

---

## 2. 机制选型：文件怎么进容器

### 2.1 先说结论 + 一次方向性误判

**结论：用 `wslc container cp`**（§1 第 24~28 条）。

⚠️ 这一节值得把它**误判的过程**也留下来，因为那是个方法论上的坑：

- 我查能力的方式是 `wslc --help`。那张**顶层命令表**里有 `exec` / `run` /
  `attach` / `export` / `import` / `save` / `load` / `logs` …… 但**没有 `cp`**。
  于是方案的第一版写了半屏"`wslc` 没有 `cp`，所以唯一的通道是往
  `exec -i` 的 stdin 里灌 tar"，并且**已经照着那条路写了一整轮代码和实测**。
- 真相是 **`cp` 在子命令那一层**：`wslc container cp` 好用得很
  （`wslc container --help` 里列着它），而且 `wslc cp` 不带 `container` 时
  确实报「无法识别的命令」—— 顶层表和子命令表**不一致**，我信了前者。

**教训：查一个 CLI 的能力，要下到子命令那一层去看。** 顶层表是"常用命令的
宣传单"，不是能力清单。（这一条已经写进 `model::deploy` 的模块文档，
免得下一个人再照着顶层表下结论。）

### 2.2 被否掉的方案

| 方案 | 为什么否掉 |
|---|---|
| **往 stdin 灌 tar**（`wslc exec -i … tar -xf -`） | 它是误判之下的产物。**实测完全可行**（§1 第 5、6 条：1 MB 二进制 sha256 一致），但它要三样东西：Windows 侧一个 `tar.exe`、一个**等于载荷大小的临时文件**、以及给 `cli.rs` 加一条"stdin 接本地文件"的路。而 `wslc container cp` 一条命令就干完了同一件事 —— **没有理由自己搭一条更复杂、更难查的路** |
| `-v/--mount` 把宿主目录挂进容器 | **只能在 `run` / `create` 时给**。已存在的容器加不了挂载，而本功能服务的正是"已经建好的容器"（实测：`-v` 只在 `run` 那一组选项里） |
| 走 WSL 发行版的文件系统（容器卷落盘的位置） | 要先知道 `wslc` 把卷放在哪个发行版的哪个路径下，再想办法从那儿写 —— 那是**没实测过**的假设，而且 `wslc` 没承诺过那个位置稳定 |
| `wslc export` → 改 → `wslc import` | 那是**重建镜像**，不是"往运行中的容器里放文件"。用户的应用一改就要重建镜像，代价完全不成比例 |

### 2.3 选定的形状

**两步**（两种部署形态共用）：

```
① wslc exec <容器> sh -c 'mkdir -p <目标> && find <目标> -mindepth 1 -delete'
② wslc container cp -q <本地条目> <容器>:<目标>     ← 每个顶层条目一次
③ wslc exec <容器> sh -c 'chmod +x <目标>/<文件名>'  ← 只有要执行的文件（Go）
```

每一步都有实测支撑，而且**顺序不能换**：

| 步 | 为什么非它不可 |
|---|---|
| ① `mkdir -p` + 清内容 | ① `cp` **要求目标目录已存在**（实测不存在的路径报 `ERROR_PATH_NOT_FOUND`，它不替我们建）；② 这一步走 `exec`，它会**报出"容器没在运行"** —— 而 `cp` 在那种情况下**静默成功**；③ `find -mindepth 1 -delete` 而不是 `rm -rf`，见下 |
| ② 逐个顶层条目 `cp` | `cp` 复制**目录**时会把**目录名一起带过去**（实测 `cp <本地>\site nginx:/tmp/d1` → `/tmp/d1/site/…`）。整目录复制会让目标里平白多一层 —— 而 nginx 的 Web 根多一层就等于**站点打不开**。逐个顶层条目复制，落到目标里才是目录的**内容**（见 `model::deploy::copy_members`）。**文件**来源就复制它自己 |
| ② 之后**数一遍**核对 | `cp` 在容器没运行时 exit 0 且什么都不做。只有数一下目标目录的**顶层条目**才发现。判据刻意用条目数而不是文件数：一个只有空子目录的**合法**来源文件数就是 0，拿文件数判会误报 |
| ③ `chmod +x` | `cp` 过去的文件权限是 **`666`**（实测；目录倒是 `777`）。不补这一步，Go 二进制稳定失败在 `Permission denied`（exit 126），而部署**看起来是成功的** |

⚠️ 只有 **Go**（`AppRuntime::needs_exec_bit`）才拼第 ③ 步。给所有部署都无脑
`chmod +x` 是错的：Java 的 jar 不需要、Node 的源码不需要，而 `chmod -R +x`
会把数据文件也标成可执行，那是在**改用户的文件语义**。

**为什么是 `find -mindepth 1 -delete` 而不是 `rm -rf`**：见 §1 第 20~23 条 ——
目标是**卷挂载点**时 `rm -rf` 返回 1，`&&` 链会断在第一步。这一条不能改成
`rm -rf`，有回归单测盯着。

**为什么是"先清空再铺"**：部署要**可预期** —— 结果目录应当等于源目录，
而不是"等于上一次部署 + 这次部署的并集"。残留的旧 `index.html` / 旧 jar
是最难查的一类问题（"我明明改了，页面上还是旧的"）。代价是**目标目录里原有的
东西会没**，所以「添加部署」里把这条写在用户点得下去的地方（见 §6.4），
而且路径要先过 §2.4 那道闸。

### 2.4 目标目录的安全闸（被清空的是**用户填的路径**）

`find <目标> -mindepth 1 -delete` 会把**那一层之下的一切**删掉，而目标目录是
用户填的。填成 `/usr` 就是 `find /usr -mindepth 1 -delete` —— 容器当场报废。

所以加一道**白名单**（`model::deploy::safe_target`）：只允许
`/opt/`、`/srv/`、`/app/`、`/data/`、`/www/`、`/workspace/`、`/home/`、`/tmp/`、
`/var/www/`、`/usr/share/nginx/`、`/usr/local/` 底下，且必须是绝对路径、不含 `..`。

- **为什么是白名单不是黑名单**：黑名单要枚举所有危险路径（`/usr`、`/usr/share`、
  `/etc`、`/var`、`/var/lib`……），漏一个就是灾难；而这个删除的危险面是
  "这一层的一切"，光看深度判不出来。白名单反过来：只认识这几种，其余一律拒绝，
  并把允许的写进提示里。想加一个前缀就是表里加一行。
- 这条**刻意保守**：用户真要部署到别处，用「终端」自己拷。
- 它同时保护了「删除部署」（那个也走 `find -delete`）。

### 2.5 这条路承认的代价（写清楚，不藏）

- **一个目录里有 N 个顶层条目就是 N 次 `wslc` 调用**（每次一个子进程）。
  一个静态站点的顶层通常只有 3~10 个条目，可以接受；但**顶层有几百个文件**的
  目录会明显变慢。对比 tar 那条路是"一次打包 + 一次传输"——
  这是选 `cp` 换来的代价，而换到的是"不用自己拼 tar 管道、不占临时磁盘、
  用的是 wslc 自己的命令"。
  ⚠️ 这条**没有在几百个文件的目录上实测过**（只测到 3 个顶层条目），
  所以它是**推的**：真遇到慢的场景，退路是回到 tar 那条（它已经验证可行）。
- **不做增量**：每次都是清空重铺。见 §2.3 的"为什么先清空"。

---

## 3. 「这个容器能部署什么」：两层判定

| 层 | 时机 | 依据 | 代价 |
|---|---|---|---|
| **L1 启发式** | 每帧渲染（`container_row_actions`） | ①**镜像名**最后一段匹配运行时表（见下）；②启动命令里有 `nginx` / `java` / `node` / `npm` … | **0**（纯字符串） |
| **L2 权威** | 进页面那一次 | `wslc inspect`：镜像引用 → `Config.Env` → `Cmd` / `Entrypoint`；再加上从 `Mounts` 里推出 Web 根 | 1 次 inspect |

### 3.1 镜像名表

和 `model::image_preset` 同一条原则：**认不出来就什么都不做**（不画按钮），
表刻意小，只认最常见的官方镜像名。带 registry 前缀的走"取最后一段"，所以
`docker.1panel.live/library/nginx:latest` → `nginx` **自动命中**（这正是本机那个
nginx 容器的情况，实测）。

| 运行时 | 镜像最后一段认这些 | 默认端口 |
|---|---|---|
| **Nginx** | `nginx`、`openresty`、`tengine` | 80 |
| **Java** | `openjdk`、`temurin`、`eclipse-temurin`、`amazoncorretto`、`corretto`、`zulu`、`liberica`、`maven`、`gradle`、`tomcat`、`jetty`、`wildfly` | 8080 |
| **Node.js** | `node`、`nodejs`、`iojs` | 3000 |
| **Go** | `golang`、`go` | 8080 |

### 3.2 为什么**没有**端口依据（和 MySQL 不一样）

`model::mysql` 有一条"发布了 3306 就当它是 MySQL"的**猜测**依据，能这么做是因为
3306 是 IANA 专门分给 MySQL 的。**这里不成立**：80 和 8080 谁都在用
（nginx、Java、Go、Node 自己、还有无数别的东西），拿端口猜只会把按钮画到一堆
不相干的容器上。所以 v0.9 **刻意不给端口依据** —— 这是"被否掉的方案"，不是漏了。

### 3.3 命令依据为什么还留着

`wslc list` 的 `Command` 被截断成 20 字符（§1 第 17 条），所以官方镜像在这条上
**基本命中不了**。留着它的唯一场景是：用户显式把命令写成 `nginx` / `java -jar …`
时它有效，而那种情况下它是**最准**的一条。判定结果里把依据一起带出来
（`DeploySignal`），界面文案和排障都用得上，而不是压成一个 `bool` —— 和
`MysqlSignal` 同一个取舍。

---

## 4. 两种部署形态：Content 与 Process

这是这一版最重要的一个设计决定，因为**nginx 不能按 Java 那样起**。

| 形态 | 运行时 | 部署是什么 | 怎么"运行" |
|---|---|---|---|
| **Content（内容）** | Nginx | 把静态站点铺进 Web 根目录 | **不自己起进程** —— 复用容器里那个 nginx。动作是「重新上传」+「重载 nginx」 |
| **Process（进程）** | Java / Node.js / Go | 把 jar / 源码 / 二进制铺进目标目录（Go 还要补 `chmod +x`，见 §2.2），再把它跑起来 | 面板起一个 `nohup` 进程，用 pidfile 管着 |

**为什么 nginx 要单独一支**：容器里那个 nginx 主进程已经占着 80 端口了，
我们再 `nohup nginx` 起一个只会**端口冲突**（退一步说，就算起了第二个也监听不到
同一个端口）。所以 Content 形态下"部署"这个动作的产物是**文件**，不是进程 ——
nginx 每个请求都从磁盘读，铺完就生效。

Content 形态的 Web 根是**从 `Mounts` 里推**的：取 `Destination` 在
`/usr/share/nginx` 底下、且 `Type` 是 `volume` 或 `bind` 的那个挂载点；本机那个
容器的实测值就是 `webdata → /usr/share/nginx/html`。推不出来就回落
`/usr/share/nginx/html`（官方镜像的默认值），**并且把这个值填进表单让用户能改**。

> ⚠️ **更正（v0.9.13）**：这一条早先还写着"`/etc/nginx` 底下的也算"
>（理由是"有的镜像把 `conf.d` 单独挂出来"）。**那是错的**：`/etc/nginx/conf.d`
> 挂的是**配置**，不是站点 —— 拿它当 Web 根会让「添加部署」把配置目录预填进
> 「容器内目标目录」，文件铺进去没有任何效果，而源目录里若有个 `default.conf`
> 还会把容器里同名的配置**删掉再覆盖**。现在只认 `/usr/share/nginx`，
> 见 §19.6。

⚠️ 这一条（从 `Mounts` 推 Web 根）是**推的**：本机只有一个 nginx 容器，我验的是
"这条路能读到那个挂载点"，没验过"各种 nginx 变体的挂载点都长这样"。所以界面上
那个字段是**可编辑**的，而且默认值旁边写着它是从哪儿来的。

---

## 5. 监督：pidfile + `/proc`，只用 POSIX sh 内建

容器里**没有 `ps`、没有 `pkill`**（§1 第 11 条），所以整套监督只能用
sh 内建 + `/proc`。四条脚本（全部由 `model::deploy` 里的**纯函数**生成，有单测）：

**启动**
```sh
mkdir -p <目录> && cd <目录> && { nohup <启动命令> > <日志> 2>&1 & echo $! > <pid>; }
```
⚠️ 那对大括号是**必须的**，理由见 §1 第 10 条（`&` 的优先级最低，不分组就
会把 `echo $! > pid` 甩到后台组外面去，而且它和 `mkdir` 之间还有竞态）。

**判活**
```sh
test -f <pid> && kill -0 $(cat <pid>) 2>/dev/null && echo __RUNNING__ || echo __STOPPED__
```
输出用 `__RUNNING__` / `__STOPPED__` 这种**带哨兵的定值**，不去猜中文/英文输出。
Content 形态另有一条**数文件**的脚本（`find <目录> -type f | wc -l`），
用来区分"还没部署过"和"部署过了"。

**停止**
```sh
kill $(cat <pid>) 2>/dev/null; rm -f <pid>
```
v0.9 只发 `TERM`，**不做 `kill -9`**（没有 `ps`，要确认"它是不是真的没退"得再扫
一遍 `/proc`，收益不抵复杂度）。界面上写清这一点。

**日志**
```sh
tail -n 200 <日志>
```
`tail` 在实测的镜像里有（§1 第 11 条）；启动时本来就是
`nohup <命令> > <日志> 2>&1`，所以 stdout / stderr 都在这个文件里。

⚠️ **`/proc` 那条路最终没有用到**。实测它可用（没有 `ps` / `pkill` 时能靠
`/proc/*/cmdline` 看进程），方案里本来还打算用它做**内容形态的**"nginx 主进程
在不在"。后来砍掉了，理由是：

- 官方 nginx 镜像的**主进程就是 nginx**（`Cmd` 是 `nginx -g "daemon off;"`，
  实测），所以"容器在运行"和"nginx 在跑"在这个镜像上是**同一件事** ——
  再扫一遍 `/proc` 是多余的一次 exec；
- 而"扫 `/proc` 找 `nginx: master`"这条**是推的**：我验了"能读到 `cmdline`"，
  没验过"各种 nginx 变体的 master 进程名都一样"（`openresty` / `tengine` /
  alpine 版都可能不同）。**为一个推出来的判据多起一次进程，不划算。**

所以内容形态的状态就看**文件数**（部署过没有），"服务在不在"由容器自己的
运行状态回答（列表行上就写着）。

---

## 6. 落地形状

### 6.1 页面

`Page::Deploy` 是**新页面**，但**不在** `Page::ALL` 里 —— 和 `Database` /
`AddInstance` 同一个取舍：它不是侧边栏里并列的一站，而是"针对某一个容器"的页面，
只能从容器行上的按钮进，顶部有「← 返回容器」。

⚠️ 它**不**让 `Page::needs_window_to_enter` 为真：这一页上**没有常驻输入框**，
「添加部署」的五个框是点按钮那一刻才建的（见 §10.2 —— 这一点我一开始写错了，
是审查时改回来的）。

页面结构：

```
[← 返回容器]        [重载 nginx]  [重新读取]  [添加部署…]
容器 nginx · docker.1panel.live/library/nginx:latest
Nginx · 按镜像名判断

（表格）应用名 | 形态 | 目标路径 | 状态 | 端口 | 操作
        site  | 内容 | /usr/share/nginx/html | 已部署 · 12 个文件 | 80（已发布） | 重新上传 删除
        api   | 进程 | /opt/api | 运行中 · 9 个文件 | 8080（没发布到主机…） | 停止 日志 重新部署 删除
```

- **状态列**的取值：`运行中` / `已停止` / `目标不存在` / `已部署`（内容形态，
  按文件数）/ `目标目录是空的` / `读取中…` / `读不到状态` / `类型认不出来`。
  判据来自 §5 那两条脚本，**不是**缓存的猜测；
  ⚠️ `读不到状态`（问容器失败了）和 `目标不存在`（真没了）是**两句话** ——
  混成一句会让人以为**文件丢了**；
- **操作列**：进程形态给「启动 / 停止 / 日志 / 重新部署 / 删除」，
  内容形态给「重新上传 / 删除」。按状态只画**当前有意义**的那几个
  （停止时画「启动」，运行时画「停止」—— 不留一个点了必然报错的按钮）；
- **读取失败**（容器没运行 / 不是这类容器）走一张 `blocked` 卡片，和
  `db_blocked_card` 同一形状，把原文放在可滚动的 `log_text` 里；
- **空列表**：一句"这个容器上还没有部署过任何应用" + 运行时自己的说明
  （`AppRuntime::note`）+ 「添加部署…」按钮；
- 存档读坏了（`deploy_store_error`）在列表上方给一条提示条 ——
  见 §10.4 那条"别再造一个没人读的 error 字段"。

### 6.2 列表行按钮

```
「部署管理」只有**运行中**且 L1 认得出运行时**的容器才画。
```

理由和「终端」「数据库」完全一致：这一页的**每一个动作**都要 `wslc exec`，
容器没跑就只剩 `WSLC_E_CONTAINER_NOT_RUNNING`。

> 被考虑过的另一种做法：把按钮画在**所有**这类容器上，因为"已部署列表"来自
> **本地存档**、容器停着也能看。否掉它的理由：那样点进去是一页**全是死按钮**的
> 东西，而「启动」按钮就在同一个列表行上、离这个按钮两格远 —— 先启动再进来
> 的路径并不绕。这条取舍写在这里，免得以后有人以为是漏了。

⚠️ 画上去之后「操作」列最多会到 **7 个按钮**（终端 / 数据库 / 部署管理 /
重启 / 停止 / 日志 / 删除）。`ALL_COLUMNS` 的「操作」权重 v0.7.0 是 330，
这一版要再看一眼 —— 数字是**权重**不是像素，但**实机得看会不会挤**
（`AGENTS.md` §8）。

### 6.3 已部署列表存在哪：本地存档

照 `dbcreds.rs` 的样子，一份 `deployments.json`，
键是 `"<容器名>/<应用名>"`，`%LOCALAPPDATA%\wslc-panel\` 下，
临时文件 + 重命名写入，解析失败**降级成空档 + 一句提示**、不 panic 不覆盖。

**为什么是本地而不是容器内的清单文件**（这是个真的岔路）：

- 容器里的清单要读就得 `exec`，**容器停下就什么都看不到**；而本机实测那两个
  容器**本来就是 Exited 状态**（用户日常就是让它们停着），"停着就看不到自己
  部署过什么"是个真问题；
- 本地存档读一次只要几毫秒，能挂在页面的加载路径上；
- 反过来，**运行状态**（跑没跑、目标目录还在不在）**必须**问容器 —— 那是事实。
  所以是"**本地记'部署过什么'，容器答'现在怎么样'**"这条分工，不是二选一。

⚠️ 代价写在明面上：**容器被 `wslc remove` 掉之后再建一个同名的**，本地存档里
那些记录会指向一个空目录 —— 表现是状态列说「目标不存在」。这是诚实的表现，
不加自动清理（自动清理会误删用户还没看的记录）。

`DeployRecord` 的字段：应用名、运行时、形态、源（本地路径 + 文件/目录）、
目标目录、启动命令、日志文件名、端口（信息性）、创建时间。

### 6.4 添加部署：一个弹窗，两步

一次性动作走弹窗，和「拉取镜像」「新建数据库」同一条理由
（`InputState::new` 要 `&mut Window`，只能懒创建）。

字段：

| 字段 | 说明 |
|---|---|
| 应用名 | 目标目录名 / 存档键。走**白名单** `[A-Za-z0-9_-]`，≤ 64 |
| 来源 | 「浏览…」按钮 → `cmd::picker::pick_directory` / `pick_file`。**按运行时预填类型**：Nginx/Node 是目录，Java 是文件（`.jar`），Go 是文件 |
| 目标目录 | 预填见下；**可编辑** |
| 启动命令 | 按运行时预填（见下）；Content 形态没有这一格 |
| 端口 | 预填运行时默认端口；**信息性**（见 §6.5） |

预填值（尽量"用户不用改就能点下去"）：

| 运行时 | 来源 | 目标目录 | 启动命令 | 解包后要 `chmod +x` |
|---|---|---|---|---|
| Nginx | 目录 | `Mounts` 推出来的 Web 根，否则 `/usr/share/nginx/html` | ——（Content） | 否 |
| Java | 文件 `.jar` | `/opt/<应用名>` | `java -jar <源文件名>` | 否 |
| Node.js | 目录 | `/opt/<应用名>` | `npm start` | 否 |
| Go | 文件 | `/opt/<应用名>` | `./<源文件名>` | **是**（§1 第 18 条） |

⚠️ Java / Go 的启动命令里要的是**源文件的名字**（`java -jar myapp-1.2.0.jar`），
不是应用名 —— 所以预填这格要等用户选完来源，**选之前**先填一个
`java -jar <应用名>.jar` 的占位形状。这条不做的话，用户不改那一格就会
得到 `Error: Unable to access jarfile`。

⚠️ Node 的 `npm start` 是**推的**：它要求源目录里有 `package.json` 且
`scripts.start` 存在。没有的话 `npm start` 会报错，而报错会落在应用日志里
（用户点「日志」能看到）。这里**不**去猜"到底该 `node server.js` 还是
`npm start`" —— 猜错的表现是"部署成功了但起不来"，比明确报错更难查。

「部署」按钮点下去的流程（**长任务，全程在后台执行器里**）：

1. **停旧进程**（只有"重新部署 + 进程形态"）—— 必须在清空目标目录**之前**：
   pidfile 就在那个目录里，目录一清旧进程就再也停不掉，而新的启动会再起一个
   → 端口冲突；
2. **准备目标目录**（`mkdir -p` + `find -mindepth 1 -delete`）—— 顺带报出
   "容器没在运行"；
3. **逐个顶层条目 `wslc container cp`**；
4. **核对**（数目标目录的顶层条目）—— 给 `cp` 的"静默成功"兜底；
5. **补 `chmod +x`**（只有 Go 这类要执行的文件）；
6. **起进程**（进程形态）；内容形态到这一步就完了（nginx 每个请求都从磁盘读）；
7. 写本地存档 → 刷新列表。

**中间态**要在界面上看得见：按 `AGENTS.md` §7.1，从点击到有结果之间必须有变化。
这一版的形状是 `DeployProgress { app, phase }` 挂在 `AppState` 上，
弹窗按钮换成「正在上传… / 正在启动…」并**不可重复点击**（有 `progress` 就是守卫）。

⚠️ 阶段**只分三档**（`Uploading` / `Starting` / `Removing`），刻意不把第 2~5 步
各拆一档：它们的耗时完全由体积决定，而界面上它们**都是"在搬东西"** ——
拆开只会让按钮文案闪来闪去，换不到信息。

### 6.5 端口：只能"报告"，不能"发布"

`wslc` 只在 `run` / `create` 时接受 `-p/--publish`，**已存在的容器改不了端口**。
所以这一版：

- 「端口」那一列展示 `inspect.Ports` 里**已经发布**的映射（本机那个 nginx 实测是
  `80/tcp → 127.0.0.1:80`）；
- 如果记录里的端口**没有**被发布，就在那一格画一个提示（"容器没把这个端口发布到
  主机，外部访问不了；端口要在建容器时给，已建好的容器改不了"）——
  **把下一步明确告诉用户**，而不是画一个点了没用的「发布」按钮。

### 6.6 列定义

`wslc-panel-core::columns` 里加 `DEPLOY_COLUMNS`，并同步
`every_table_has_the_expected_column_count` 那个断言（它把每个表的列数钉死了）。

| 列 | 权重 | 说明 |
|---|---|---|
| 应用名 | 200 | |
| 形态 | 90 | 内容 / 进程 |
| 目标路径 | 340 | 长，且是要认的东西 → 用 `cell_muted_full` 带 tooltip |
| 状态 | 170 | |
| 端口 | 110 | |
| 操作 | 330 | 最多 3 个按钮 |

---

## 7. 安全：路径进 shell 必须先引号

目标目录、应用名、pidfile / 日志路径都是**用户输入**，而它们要拼进
`sh -c` 的脚本文本里。这不是"参数被当成选项"那个问题，是**真的 shell 注入**：
一个名字叫 `a; rm -rf /` 的应用会真的去执行那半截。

处理方式是两层：

1. **应用名走白名单** `[A-Za-z0-9_-]`、长度 ≤ 64，不合法**拒绝**并把焦点送回那一格
   （和 v0.7 的库名白名单同一条原则：把选择权交回用户，而不是我们猜他的意图）；
2. **所有进脚本的路径一律过 `shell_quote()`** —— 单引号包起来，内部的 `'` 按
   POSIX 的 `'\''` 写法逃逸，有单测盯着（含 `'; rm -rf /`、`$(…)`、
   反引号、换行、空串这几种形状）。

**启动命令是唯一的例外**，它是**原样**进脚本的。这是刻意的，理由和
`state::exec_argv` 那段一模一样：用户在这一格里写的就是他要执行的**命令行**
（`java -jar x.jar --spring.profiles.active=prod` 这种），把它当数据引号起来
就没法用了。这一条要在界面上说出来（"这行会原样交给容器里的 `/bin/sh -c`"），
不能让用户以为它是参数。

---

## 8. 分阶段与验收

| 阶段 | 内容 | 怎么验 |
|---|---|---|
| **P1 纯逻辑** | 识别表 / 五条脚本的生成 / `shell_quote` / `safe_target` / `cp` 参数 / `inspect` 里推 Web 根 / 状态解析 / 错误分类 / 本地存档读写 | `core` job 单测。夹具用**实测抓下来的**：那个 nginx 容器的 `inspect` JSON、未运行时的报错原文 |
| **P2 进程层** | `cmd::deploy`：准备目录 → 逐个 `container cp` → 核对 → `chmod` → 起停判活看日志 | `ui` job 编译 + 本机拿真 nginx 容器手跑（脚本由 P1 的单测保证文本正确） |
| **P3 界面** | 列表行按钮 + 页面 + 添加部署弹窗 + 存档接线 | 本机点通（见下） |
| **P4 文档** | 本文件、`README.md` 的功能表、版本号 0.8.0 → 0.9.0（四个文件六行 + `Cargo.lock` 三处） | 照 `AGENTS.md` §4 |

⚠️ **CI 只证明"编译过 + 纯逻辑测试过"**（`AGENTS.md` §8）。下面这些**必须本机点一遍**：

- 容器**停止**时列表行**不出现**「部署管理」按钮；页面开着时把容器停掉 →
  报错文案是"先启动它"；
- **Nginx（Content）**：把本机一个真目录部署进那个 nginx 容器，`curl
  http://127.0.0.1:80/` 能看到新内容；再点「重新上传」换一版，内容跟着变；
  ⚠️ 这条同时验的是**"目标目录是卷挂载点"**那个坑（§1 第 20~23 条）；
- **Java / Node.js / Go（Process）**：**机制**这一层大部分已经实测跑通了
  （§1 第 19 条：同一批起停脚本在真容器里走完了"启动 → 判活 → 日志 → 停止"；
  第 28 条验了 `cp` 的字节完整性与权限位）。没验的是**界面到机制那一层**
  —— Rust 拼出来的脚本字符串、后台任务、状态刷新这三者接起来对不对。
  所以要在 PR 里明确标成**"Shell 机制实测过，界面链路未本机实测"**，
  最先该点的是：「部署 → 启动 → 状态变运行中 → 日志有输出 → 停止 → 状态变已停止」
  这一整条。本机只有 nginx / mysql / hello-world 三个镜像，造 Java / Node / Go
  的真容器要联网拉镜像，不在这一版的验证预算里；
- 部署一个**可执行的**文件（Go 二进制或 shell 脚本）到那个 nginx 容器上，
  确认它能真的跑起来 —— **这条专门验 `chmod +x`**（§1 第 18 / 28 条那个坑）；
- **故意把容器停掉再点「部署」**，确认界面报的是"容器没在运行"而不是
  "部署成功" —— **这条专门验 `cp` 的静默成功**（§1 第 29 条）；
- 「操作」列加到第 7 个按钮之后**实机看宽度**会不会挤（`ALL_COLUMNS` 的 330）；
- 部署一个**带空格和中文**的目录名，确认内容完整（这条验的是 `shell_quote`）；
- 部署一个**顶层条目很多**的目录（比如 50 个文件），量一下要多久 ——
  §2.5 那条"每个顶层条目一次 `wslc` 调用"的代价**还没有实测数据**。

### 8.1 已经实测过的：**对着 Rust 会生成的那串脚本，逐字跑了一遍**

这一条值得单独写出来，因为它是**CI 永远查不到**的那一层：
`model::deploy` 里那 12 个"生成 shell 脚本"的函数，CI 只能证明**它们能编译**
和**拼出来的字符串等于单测里写死的那个**。而"那串字符串在真容器里跑得通吗"
只能真跑。

做法：按 `shell_quote` / `container_join` 的规则**逐字抄出** Rust 会生成的那串，
拿真 nginx 容器当靶子跑一遍（`/opt/wslc-panel-probe`）。

| 生成函数 | 实测结果 |
|---|---|
| `prepare_target_script` | exit 0 |
| `cp_args`（**逐个顶层条目**：`.hidden` / `index.html` / `runme` / `sub`） | 4/4 全部 exit 0 —— 含**隐藏文件** |
| `count_entries_script`（`verify_copy` 的判据） | 输出 `4`，与发出去的数量相等 → 核对**通过** |
| `chmod_exec_script` | `runme` 变 **`777`**，而 `index.html` 仍是 **`666`** —— 只给那一个文件补了可执行位（没有 `chmod -R`） |
| `start_script` | exit 0、**0.16 秒**返回（脚本本身不打印 pid，pid 写进了 pidfile） |
| `probe_script`（进程形态） | 输出**正好两行**：第 1 行 `__WSLC_RUNNING__`、第 2 行 `6` → **验证了 `parse_probe_output` 的按位置解析**（这正是它跟 `parse_process_status` 不能混用的原因） |
| `tail_script` | `started-by-panel`（stdout）**和** `warn-line`（stderr）**都在** → 验证了 `nohup … > log 2>&1` |
| `stop_script` | 返回 `__WSLC_STOPPED__`，紧接着 `probe_script` 第 1 行也变成 `__WSLC_STOPPED__` → `kill` 真的生效了 |
| `remove_script` | 内容清空、**目录本身留着**，`count_entries_script` 给 `0` |
| `tail_script`（日志已随目录被清掉） | 给 `__WSLC_NO_LOG__` → 验证了 `render_log_output` 的哨兵翻译 |
| `count_entries_script`（目录**不存在**） | 给 `__WSLC_NO_DIR__` → `parse_file_count` 返回 `None` → 走 `verify_copy` 的"没建成"那条 |
| `reload_nginx_script` | exit 0，`signal process started` 出现在 **stderr** → **这就是必须写 `2>&1` 的证据**（不并过来用户什么也看不到） |

另外两条**当时**测的（后来那个容器被别的会话启动了，没法重跑，所以记的是当次的结论）：

- 容器**没运行**时 `wslc exec` → exit 1 + `WSLC_E_CONTAINER_NOT_RUNNING`
  （`classify_error` 认的就是这串）；
- 同样情况下 `wslc container cp` → **exit 0、无输出、什么都没做** ——
  `verify_copy` 存在的**唯一**理由。

⚠️ 仍然**没有**实测的：Rust 那一层把上面这些串起来之后的行为
（后台任务、状态写回、界面刷新），以及 Java / Node / Go 三种真容器。
见上面那份"必须本机点一遍"的清单。

---

## 9. 这一版**没做**的事（都是刻意留的）

- **不做端口发布 / 容器重建**：已存在的容器改不了 `-p`（§6.5）。要发布端口的
  正确做法是重建容器，那是另一件事；
- **不做从容器取文件回 Windows**：⚠️ 理由是**"这一版不需要"**，不是"做不到"——
  `wslc container cp <容器>:<路径> <本地路径>` 是**实测可用**的
  （§1 第 24 条，方向反过来而已）。用户要看容器里的东西，用「终端」；
- **不做环境变量 / 多文件源 / 部署历史版本回滚**：v1 一次一个源；
- **不做 `kill -9` 强制停止**（§5）；
- **不做 Node 的包安装**：`npm install` 是网络 + 分钟级操作，塞进「部署」会把
  "部署"变成一个可能失败在十个地方的复合动作。用户可以在「终端」里装，
  或者自己把 `npm install && npm start` 写进启动命令；
- **不做多容器的部署页并存**：`AppState::deploy` 是 `Option<DeployState>`，
  一次只服务一个容器（换容器即整体替换）—— 和 `AppState::db` 同一个取舍；
- **不做断点续传 / 并行复制**：一个目录的顶层条目是**串行**一个个 `cp` 的
  （§2.5）。并行要自己管子进程池和错误汇总，收益在"顶层条目很多"时才明显，
  而那种场景还没有实测数据。

---

## 10. 自审：改动点清单（对着代码逐条查出来的）

方案写完之后做了一次**只读的穷尽审计**（"在 `Page` 里加一个变体会牵动哪些地方"），
下面是审计结论 + 我据此定下来的改动清单。行号是审计当时的。

### 10.1 `Page::Deploy` 要碰的 5 处穷尽匹配

`Page` 没有 `#[non_exhaustive]`，所以漏一处就是编译错误（这是好事 —— 编译器
会替我们盯住）。全仓库**一共只有 5 处**：

| # | 位置 | 函数 | 改法 |
|---|---|---|---|
| 1 | `state.rs:123` | `Page::label()` | 加 `Deploy => "部署管理"` |
| 2 | `state.rs:139` | `Page::group()` | 并进 `Containers \| Database` 那一支（"容器"组） |
| 3 | `state.rs:167` | `needs_window_to_enter()` | ⚠️ **不改** —— 见 §10.2 |
| 4 | `views.rs:490` | `views::page()` 分发 | 加 `Page::Deploy => deploy_page(...)` |
| 5 | `app.rs:5990` | `nav_id()` | 加 `Deploy => "nav-deploy"` |

**不进 `Page::ALL`**，所以这几处**不用**动：
`state.rs:110` 的 `[Page; 8]` 类型、`state.rs:3272` 的
`assert_eq!(Page::ALL.len(), 8)`、`app.rs:5591` 的侧边栏循环。
（`AddInstance` / `Database` 就是这么待着的，照着来。）

⚠️ 但 `nav_id()` 的注释（`app.rs:5986`）现在写着"这个 match 只覆盖 `Page::ALL`
里那 8 页" —— 它**已经不准确**了（`AddInstance` / `Database` 也在里面）。
加第三页时顺手把这句改对，否则下一个人会照着它做出错误判断
（`AGENTS.md` §5 那条"注释和实现不一致时，后来的人会照着注释继续加重复"）。

### 10.2 ⚠️ 我改错了一处，又改回来了：`needs_window_to_enter`

清单第一版把它算成本次改动之一，理由是"添加部署的表单要 window 才能建"。
**那是把"弹窗要 window"和"进页面要 window"混成了一件事。**

`needs_window_to_enter` 问的是"**进这一页**是不是必须有 window"，也就是
"这一页上有没有**常驻**的输入框"。部署页上**一个都没有** —— 「添加部署」那五个框
是**点按钮那一刻**才建的（`open_deploy_dialog` 有 window），
这正是 v0.6.3「拉取镜像」的形状（那次也是把表单挪进弹窗，于是「镜像」页
从这一列里退出去了）。

写成 `true` 不是"更保险"，它会带来两样坏东西：

1. `set_page` 里多一个**永远走不到**的分支（部署页不从侧边栏进，
   `set_page(Page::Deploy, …)` 没人调用）—— 而 v0.4 删掉
   `Page::Config` / `Page::WslConfig` 清的就是这种垃圾；
2. 测试 `only_the_pages_with_inputs_need_a_window_to_enter`
   （`state.rs:3631`）会把这条断言当成真的，然后下一个人以为
   "部署页有常驻输入框"，去页面里找那五个框 —— 找不到。

所以：**`needs_window_to_enter` 不动**，`set_page` 也不加分支。
新页面的断言写成 `!Page::Deploy.needs_window_to_enter()`，
理由（"表单在弹窗里"）写在断言旁边。这条是**审查**捞出来的，不是写方案时想到的
—— 记在这里，因为"多写一个进去更保险"这个直觉下次还会冒出来。

### 10.3 `columns.rs`

- 加 `DEPLOY_COLUMNS`（6 列，见 §6.6）；
- 加进 `all_column_sets()`（`:161`）—— 顺序决定了那个断言向量；
- 更新 `:196` 的 `assert_eq!(counts, vec![6, 5, 6, 5, 7, 8, 5, 4])`
  → 末尾追加一个 `6`；
- 更新 `:156` 的 `use super::{…}` 清单；
- 更新 `views.rs:52` 的 `use crate::columns::{…}` 清单。

### 10.4 本地存档

照 `dbcreds` 的样子加 `wslc-panel-core/src/deploys.rs`，并在
`wslc-panel-core/src/lib.rs:26` 的模块清单里登记。
启动读盘照 `state.rs:859` 的 `db_credentials_at_startup()` 那个自由函数的样子
（碰文件系统的放自由函数、纯数据放 `AppState`）。

⚠️ 审计顺手发现一件事：`AppState::db_credentials_error`（`state.rs:2959`）
**只有定义和初始化两处，全仓库没有任何读取点** —— 读档坏了的原因算出来了却没人
显示。这是 v0.8 留下的一个洞。v0.9 **不修它**（不在这一版的射程里），但新写的
`deploy_store_error` **必须真的在界面上显示出来**，不能把一个刚发现的毛病
再复制一份。这条记在这里，免得下次又忘。

### 10.5 `wslc-core` 那边

- `model/deploy.rs` 新建 + `model/mod.rs` 登记（已做）；
- `model/container.rs` 加 `ContainerMount` + `ContainerInspect::mounts()`
  —— 审计确认**原来没有**（只有一个 `mount_count()`），已做；
- `image_last_segment` 从 `mysql.rs` 提到 `model/mod.rs`（两族都要用，各存一份
  必然漂移），已做；
- `cmd/deploy.rs` 新建 + `cmd/mod.rs` 登记，**只走 `wslc container cp`
  和 `wslc exec`**，不需要 `cli.rs` 加任何新方法（`run_with_timeout` 就够）；
- ⚠️ `cli.rs:348`-`:351` 那段注释（"**导入**（从本地喂 SQL 进容器）没有这条路"）
  **保持原样** —— 它是**对的**，而且在我们改用 `container cp` 之后更加对：
  那条 stdin 的路我们**没有加**（第一版加过，改成 `cp` 之后又撤掉了 ——
  留着就是一段没人用、也没实测过的公共 API）。
  ⚠️ 这条本身就是"方案变过之后要回头清理"的例子：**撤掉的代码，和写下的代码
  一样要交代**。

### 10.6 界面

- 容器行按钮：`views.rs:907` 那个 `if let Some(_candidate) = mysql_container_kind`
  旁边再加一段（判定函数是 `model::deploy::container_app_kind`）；
- 页面渲染：照 `views::database`（`views.rs:1090`）的三形态骨架；
- 弹窗：照 `db_dialog_overlay`（`views.rs:1737`），在 `render` 里
  **声明区**（`app.rs:5734`–`:5795`）和**挂载区**（`app.rs:5918`–`:5939`）
  各加一处 —— 两段是一一对应的，只改一处就画不出来；
- 挂载位置：部署弹窗**不需要**盖住数据库那套浮层（两者不同时开），
  所以插在 `exec_dialog`（`:5933`）之后、`db_dialog`（`:5935`）之前；
- 「浏览…」**复用** `Shell::spawn_picker`（`app.rs:4197`）—— 它已经带好了
  `picking` 守卫（`app.rs:694`，picker 的文档要求"同时只弹一个"）和
  `spawn_in` + `background_executor` 的正确形状，不要再写一份。

### 10.7 审计**没有**发现的东西（这些是"不用改"的证据，不是漏了）

- `Page` 没有 `Display` / `From` / `Default` 实现；
- `main.rs` / `theme.rs` / `prefs.rs` 里没有任何 `Page` 引用；
- `views.rs` 里除了 `:490` 没有别的 `Page` 匹配；
- `app.rs` 里没有 `#[cfg(test)]` —— `Page` 的测试全在 `state.rs`，
  所以新页面的单测也要加在 `state.rs`（**不在 `app.rs`**）。

### 10.8 第二轮审查：**代码写完之后**的静态审查，捞出 7 条

写完之后又做了一轮**只读的静态审查**（"按本仓库别处有没有这么用过"去核每一个
API 调用、逐条手算每一个新断言的字符串）。它捞出 7 条真问题，全部已修：

| # | 问题 | 性质 | 修法 |
|---|---|---|---|
| 1 | `state.rs` 的测试辅助函数 `deploy_entry` 少了 `probe_error` 字段 | **一定是编译错误**（结构体字面量必须给全字段）→ `core` 与 `ui`（`--all-targets` 会编 `#[cfg(test)]`）**两边都红** | 补 `probe_error: None` |
| 2 | `views.rs` 用了 3 次 `.text_base()` | 全仓库**史无前例**（别处是 `text_sm` / `text_xs` / `text_lg` / `text_xl`）→ 很可能是 `no method named text_base` | 换成 `.text_lg()` |
| 3 | `injection_shapes_are_neutralized` 的第三条断言**恒假** | 新单测自己写错 —— 它在**没剥外层引号**的串上找 `'\''`，于是九个用例全断言失败。⚠️ **单测写错比不写还坏**：它让"红"变成噪音 | 改成"剥掉外层引号后，内部单引号数 == 3 × `'\''` 出现次数" |
| 4 | `mysql.rs` 的文档链接 `[`image_last_segment`]` 悬空（函数已挪走） | 只是 rustdoc warning | 改成纯文字 |
| 5 | 动作的异步回写**没判"用户已经切到别的容器了"** | 状态串台（A 的错误横幅挂到 B 的页面、A 的回调清掉 B 的"正在…"） | 加 `Shell::deploy_page_is(container)`，**页面上的东西**都判；存档与列表刷新**不判**（它们按容器索引） |
| 6 | **空目录来源会被当成"部署成功"** | `copy_members` 对空目录返回空表 → `verify_copy` 的 `count >= 0` 恒真 → 弹"已部署"而目标目录是空的。而 `copy_members` 的文档**恰好要求调用方拦掉这件事** | `copy_sources` 里 `members.is_empty()` 直接报错 |
| 7 | 读存档与写存档对"`LOCALAPPDATA` 没设置"处理不一致 | 读降级到**当前工作目录**（`unwrap_or_default()`），写明确拒绝 | 两边一致：都不做，并且把原因交给 `deploy_store_error` 显示出来 |

另外顺手**删掉**了两处"没有调用方"的东西（审查提到、但都不报 warning 因为都是
`pub`）：`DeployState::usable()` 和 `model::deploy::exec_i_args()`（后者只剩它
自己的单测在用）。`AGENTS.md` 的原则是**不要留没人用的公共 API** ——
它只会让下一个人以为"这条路能用"。

> 第 1、3 条是这一轮最值钱的两条：它们都是**"CI 会红、但红的原因跟逻辑无关"**
> 的那一类。第 3 条尤其值得记：**新写的单测自己也要被审**，
> 一条恒假的断言会把一整条 CI 变成噪音，然后所有人开始无视红线。

### 10.9 这一版**踩到的流程坑**（留个记录）

方案执行到一半发现**同一个工作目录里有另一个会话在改同一批文件**
（v0.8 补文档 + "恢复备份改走 `container cp`"，见 `git status` 里
`cmd/mysql.rs` / `docs/PLAN-v0.8.md` 那些**不是本次改动**的条目）。
两批改动混在 `state.rs` / `model/mysql.rs` 里，**按文件拆不开**。

记在这里是因为它有个可复用的教训：**多会话并行改同一个工作目录时，
"只提交我的部分"这件事在文件级做不到** —— 除非两边约好各自只碰不同的文件。
`AGENTS.md` §9 那条"一次最多派 2 个子代理"、以及"改动经常集中在
`app.rs` / `views.rs` 这两个大文件上"的提醒，说的正是这个成本。

> ⚠️ **事后更正（结论反了）**：上面那段判断错了，错在我把"本地 `git status`
> 显示这些文件被改过"直接读成了"有人没提交"。
>
> 真相是 **本地 git 历史落后**：本机 `github.com` 不通，`scripts/gh-api-pull.ps1`
> 只同步**内容**、不动历史（`AGENTS.md` §7.6）—— 那些改动**早就是远端 main 上的
> `4ebe1dd`（#53）**了。所以"拆不开"这个结论根本不成立：
>
> - 逐个文件比过（本地 vs `GET /contents/<path>?ref=main`）：`cli.rs` /
>   `cmd/mysql.rs` / `docs/wslc-schema.md` / `docs/PLAN-v0.8.md` 与 main
>   **逐字节相同**，**一个字节都不在这次提交里**；
> - 于是这个 PR 的 diff 只有本次的部署改动（用 `POST /git/trees` 的 `base_tree`
>   打在 main 那棵树上，见 `AGENTS.md` §7.6）。
>
> **教训（比原来那条更有用）**：`git status` 说"这些文件被改过"，
> **不能推断出"有人没提交"** —— 在"内容用 API 同步、历史不跟着走"的仓库里，
> 它更可能只是**本地历史落后**。判据要看**远端**：`git log` 比不过，
> 就逐文件比 blob（或看远端有没有对应的 commit）。
> 我一度按"别人的活混在我这边"去写提交信息，那条说明是错的、已撤。

### 10.10 CI 第一轮抓到的三处（"本机没工具链"的真实代价）

推上去第一轮：`core` **478 过 / 1 挂**，`ui` **2 处编译错误**。三处都与逻辑无关，
全是"本机一行都没编译过"的直接后果：

| # | 报错 | 根因 | 修法 |
|---|---|---|---|
| 1 | `E0308: types differ in mutability`（`app.rs:3789`） | `focus_deploy_field` 的 `cx` 写成了 `&App`，而 `Window::focus` 要 `&mut App`（`InputState::focus_handle` 要 `&App` —— 两者差一层可变借用） | 参数改成 `&mut Context<Self>`（调用点本来就握着它，一次降低都不用） |
| 2 | `E0382: borrow of moved value: container`（`app.rs:3536`） | `refresh_deploy_status` 里 `container` 被内层 `async move` 搬走了，可回写时还要拿它判"这一页还是那个容器吗" | 和另外 6 处同一个写法：进内层之前先 `clone` 一份 |
| 3 | 单测 `start_script_neutralizes_an_injected_directory` **断言恒假** | `!script.contains("; rm -rf /; echo '")` —— 引号转义**必然**把 `'\''` 里的那个 `'` 插在这串后面，所以它**一定**包含 | 改成逐字钉死引号后的整体形式 + 数 `'\''` 转义 + 命令与大括号分组仍在 |

⚠️ 第 3 条和 §10.8 的第 3 条是**同一类错误**，而且这次是**我自己**在同一版里
第二次犯：**"某段坏字符串没有原样出现"这种断言，在"我们要给它加引号"的场景里
几乎总是恒假的** —— 加引号这个动作本身就会把引号插进去。要判的是
"**引号之后**它只是数据"，判据是"除了 `'\''` 没有裸引号"或者"整体形式逐字相等"。

> 这三条正好说明 `AGENTS.md` §1 那句话的分量：CI 是**唯一**能回答"能不能编译"
> 的地方。而且它顺带抓出了一条恒假断言 —— 那种"红了但没有信息量"的测试比不写
> 还坏：它训练人无视红线。

### 10.11 第四处：自审发现的**死胡同**（CI 查不出来的那一类）

`ui` 绿了也查不出这个：nginx 形态的目标目录默认值是
`Shell::open_deploy_dialog` **从 `inspect` 的挂载点推出来的**
（见 §3：那个容器的站点落在一个卷上，写死 `/usr/share/nginx/html` 会在
"用户自己挂了别的目录"时把文件铺到**没人读**的地方）。

但 `safe_target` 只认一张**静态白名单**。于是用户把站点挂在白名单之外
（`-v webdata:/site`）时，**面板会拒掉自己预填的那个路径** —— 他看到的提示是
"只允许填在 `/opt/`、`/srv/`… 底下"，而正确的那个就摆在他眼前的输入框里、
是我们填进去的。这是个**没法自己走出来的死胡同**。

修法是新增 `safe_target_with_site(target, web_root)`：白名单之外**再放行这个
容器自己的站点目录**（以及它底下的路径，判前缀时看**路径边界**：
`/sitemap` 不在 `/site` 底下）。

⚠️ 这不是把闸门放宽：`web_root` 来自**容器自己的配置**（`inspect` 的
`Mounts[].Destination`），是那个运行时真正对外提供内容的地方 —— 清空它正是
"部署"的语义。而拿不到它（`None`）、或者它自己就是个怪路径（`/`、带 `..`、
相对路径）时，**一行都不多放**。这两条都有单测钉着。

> 记这一条是因为它代表一类 CI **结构上**查不出的缺陷：**类型全对、测试全绿、
> 逻辑自洽，但用户走进去了出不来。** 这类只能靠"把用户会怎么走一遍"想一遍 ——
> 具体到这次，就是问一句"我们预填的值，我们自己会接受吗？"

---

## 11. 顺带修的：「创建容器」缺**启动命令**那一格

### 11.1 用户报的现象

界面上一个容器点「启动」→ 提示成功 → 还是「已退出」，反复点都一样。
（实机：`eclipse-temurin:8-jre-alpine`，用户管它叫"本地的 jre8"。）

### 11.2 实测（全部在真机上跑过）

```
$ wslc image inspect docker.1panel.live/library/eclipse-temurin:8-jre-alpine
Cmd          :                      ← 空
Entrypoint   : {/__cacert_entrypoint.sh}
ExposedPorts :                      ← 空

$ wslc start eclipse-temurin
eclipse-temurin                     ← **启动命令本身是成功的**

$ wslc inspect eclipse-temurin
ExitCode   : 0
StartedAt  : 2026-10-15T10:19:14.767116477Z
FinishedAt : 2026-10-15T10:19:14.76824708Z   ← 相隔 1.1 ms
logs: （空）
```

**不是启动失败，是启动成功之后立刻正常退出。** 那个镜像（`-jre-alpine`，
JRE-only）只有 `ENTRYPOINT`、没有 `CMD`，而 entrypoint 的末行是 `exec "$@"`：

```sh
exec "$@"
```

不带参数时 `exec` 在 alpine 的 ash 里没东西可执行、直接返回 0 —— 容器于是
"跑完了"。镜像本身完全正常：`wslc run --rm … java -version` 实测打出
`openjdk version "1.8.0_504" (Temurin)`。

### 11.3 为什么面板用户走不出来（这才是缺陷本身）

面板的「创建容器」弹窗**没有启动命令这一格**：

- `CreateDialog` 只有 image / name / ports / env / volumes / network / memory / cpus / pull；
- `to_spec()` 从不给 `RunSpec::command` 赋值 —— 全仓库 `spec.command` 只在
  `cmd/container.rs` 的**测试**里出现过；
- 弹窗里那句「启动命令：…」是 `wslc image inspect` 读来的**只读**提示
  （`views::image_facts_hint`）。

于是**对于没有 `CMD` 的镜像，面板建出来的容器必然是"起来就退"的**，跟怎么点
没关系。而唯一能填命令的地方（「添加部署」那格）只能作用于**已存在**的容器 ——
这个容器 1 ms 就退，来不及 `exec` 进去。**死胡同，和 §10.11 同一类。**

### 11.4 改法

1. `CreateDialog` 加 `command` 一格，`to_spec` 写进 `RunSpec::command`；
2. 换算走**新的纯函数** `wslc_panel_core::state::create_command`，
   而不是把逻辑摆在 `app.rs` 里（理由同 §2：那样单测不必链接 GPUI）；
3. 视图里加一行 `form_field("create-command", …)`。

`create_command` 上钉着一条**唯一的陷阱**，两条单测专门守它：

| 输入 | 结果 | 为什么 |
|---|---|---|
| 空 / 只有空白 | **空 `Vec`** | 一条命令都不给，镜像自己的 `ENTRYPOINT` / `CMD` 才有机会生效 |
| 非空 | `["/bin/sh", "-c", <原文>]` | 同 `exec_argv`：管道 / 重定向 / `&&` 不能静默失效 |

⚠️ 空的时候**不能**返回 `["/bin/sh","-c",""]` —— 那同样算"用户给了命令"，
会把镜像的 `CMD` **顶掉**：mysql / nginx 那类镜像本来能跑，用户在那格敲了个
空格反而起不来了。这条是从"留空"到"什么都不做"的关键一步，所以它有自己的单测。

顺带把 `image_facts_hint` 的措辞改了（「镜像自带的启动命令：X（「启动命令」
那格留空时用它）」）：弹窗里现在有**两处**都叫"启动命令"，不写清哪个生效，
用户分不出来。

### 11.5 这一版**没做**的（留给下一版）

- **没有在弹窗里告警"这个镜像没有 `CMD`、留空会起来就退"**。现在只是把
  `Entrypoint + Cmd` 原样显示出来（对那个 temurin 镜像显示的是
  `/__cacert_entrypoint.sh`），用户仍然可能读成"这镜像有命令、能跑"。
  真的要做，就得区分"只有 `ENTRYPOINT`"和"有 `CMD`"两种情况 ——
  `ImageFacts.command` 现在把两者拼成了一个字符串，信息已经丢了，
  所以这需要**先改 `parse_image_inspect`**（`wslc-core`，配 fixture 单测），
  不是加一句话的事。
- 弹窗多一行之后**是否会出现滚动条**，属于 §8 的"必须本机点一遍"那一类
  （卡片有 `max_h(880)` + `overflow_y_scroll` 兜底，理论上只是多滚一点）。

---

## 12. 数据库页两个"点了没反应"（v0.9.5 实机报过来）

### 12.1 用户报的现象

数据库列表某一行的「备份」点开只有一句"还没有备份"+「关闭」——
**没有任何"现在就备份"的按钮**，所以那个列表永远不会有第一份；
另一个库的「SQL」点下去只有一条"控制台已指向 djxm…"的提示，
**输入 SQL 的浮层不出现**。

> ⚠️ 这一节的定位是**读源码**得到的：本机没有 Rust 工具链（`AGENTS.md` §1），
> 没法实机复现，也没有加临时日志跑一遍。下面每条都给了可自己复核的判据，
> 没有一条是"看起来像"。

### 12.2 原因一：创建备份的入口**从 v0.8 起就不存在**

`Shell::start_db_backup`（`app.rs`）全仓库**只有定义、没有一处调用**。
判据是**改动前**那棵树（用户跑的就是它：`main` = `1b2c1f2`）：`app.rs` 里
`start_db_backup` 只命中 1 行 —— `pub fn start_db_backup(` 本身；`views.rs` 里 0 行。
（本地 git 落后于远端、`1b2c1f2` 没取回来，这一条是按 `api.github.com` 拉回的
`main` 源码数出来的，见 §7.6。）

`git log -S start_db_backup` 也只命中引入它的那一个提交（`5556233`，v0.8 的
备份功能）—— 也就是说**从来没有人给它接过按钮**，不是后来被删掉的。

而列表行上的「备份」按钮打开的是 `db_backup_dialog_overlay`，那个浮层里
只有「恢复 / 打开位置 / 删除」和一个「关闭」。于是从 v0.8 起，"备份"是**死的**：
能看历史备份、能恢复、能删，但**永远造不出第一份**。

⚠️ 这个死代码 `cargo check` **查不出来**（它编译得过，只是没人调）——
和 §7.5 那些"漏 `.id()`"不同，那类编译错误 CI 会拦，这类不会。

### 12.3 原因二：SQL 控制台浮层**算了但没挂进渲染树**

v0.9.2 把控制台从 tab 改成浮层时，`Render for Shell` 里只写了

```rust
let db_console: AnyElement = match &state.db { … views::db_console_overlay(…) … };
```

后面那串 `.child(…)` 里**没有 `.child(db_console)`** —— 算出来的 `AnyElement`
当场被丢掉（`unused variable` 警告而已，而 `lint` job 不阻塞，见 §3）。
`open_db_console_for` 该做的都做了（`db_console_open = true` + 把账号写进连接卡 +
一条 toast），所以用户看到的是"提示有了、浮层没有"。

### 12.4 改法

1. `Render for Shell` 里补 `.child(db_console)`（排在 `db_info` 之后：两者都从
   数据库页的按钮打开，遮罩互斥，不需要更细的层级）；
2. 备份浮层加「立即备份」（右上角）—— **创建入口只此一处**：行上的「备份」
   是进这个浮层，同一动作不给两个入口（§5）。备份跑着时它换成「备份中…」
   （项目里不用 `.disabled()`），空列表文案也改成指路；
3. `open_db_backup_dialog` 顺手把**面板记的账号**写进连接卡 —— 备份用的是
   连接卡上的账号，而从行上进来的用户没见过那张卡（它藏在「基础信息」后面）。
   不写的话，**面板建的库**（有自己的账号，root 往往不对）会拿 root 去 dump，
   报一句 `Access denied`，而用户没地方知道该填谁。浮层里新加的那行
   「备份用账号 X」显示的就是这次真正会用的账号（密码为空时额外点出来）；
4. 「SQL」打开时把焦点给「语句」那一格（§7.5：输入框的焦点要在打开时
   `window.focus` 给过去，否则敲的字落在别处）；
5. `start_db_backup` 里"正在读列表 / 导入中"那条分支原先**静默返回** ——
   改成一句 toast（§7.1：点了没反应会被当成按钮坏了）。它现在是「立即备份」
   唯一可能的失败方式，不能一声不响。

### 12.5 这一版**没做**的

- 备份仍然**不能中断**（`cancel_db_backup` 只是说明情况），要真取消得把
  `backup_database` 改成 `spawn_to_file` + 轮询那条路；
- 备份仍然直接落在 `<数据目录>/backups/<容器>/`，**没有"另存为"** ——
  要拿出去只能点「打开位置」再去拷。用户提的是"备份**导出**"，如果他要的是
  "存到我指定的目录"，那是下一版的事（`cmd::picker` 有现成的目录选择器）；
- ⚠️ **必须在本机点一遍**（§8）：浮层是不是真的画出来了、SQL 输入框能不能
  直接打字、备份能不能跑完并在列表里多出一行。CI 只证明编译过。

---

## 13. v0.9.6 的「立即备份」：第一次真点下去就炸，还牵出一个会**弄坏数据**的隐患

### 13.1 现象（用户截图，构建 `9458905`）

装上 v0.9.6 点「立即备份」，红色提示条写着：

```text
备份失败：wslc exec -e MYSQL_PWD=* mysql /bin/sh -c mkdir -p
/tmp/wslc-backups && mysql -u *_user -N -B --databases * --sing...
```

（`*` 是脱敏：密码 `djxm` 出现在用户名和库名里，按值一起被洗掉了。）

### 13.2 根因一：把**客户端**当成了 dump 程序

`mysqldump_args(client_bin, …)` 原来把传进来的 `client_bin` 直接当 argv[0]，
而调用链传的是**探测到的客户端**（`db.client` = `mysql`，那是执行 SQL 用的）。
于是容器里真跑起来的是"**mysql 客户端 + mysqldump 的参数**"。

**实测（本机容器，mysql 26.7.0，同一条 sh 脚本、同一个库）**：

| 程序名 | 结果 |
|---|---|
| `mysql -u djxm_user -N -B --databases djxm …`（v0.9.6 的形状） | `mysql: [ERROR] unknown option '--databases'.` **exit 2** |
| `mysqldump -u djxm_user -N -B --databases djxm …` | **exit 0**，110 张表 / 2,318,588 字节 |

顺带解释了现场那两个 **0 字节**文件（`/tmp/wslc-backups/djxm-*.sql`）：
`>` 在程序失败**之前**就把文件建出来了。

**为什么一直没人发现**（两条一起才成立）：

1. 这个函数只有一个调用方，而"备份"按钮从 v0.8 到 v0.9.6 **根本不存在**（§12）
   —— 那段代码从来没被真跑过；
2. 单测传的是 `"mysqldump"`，而且**从不检查 argv[0]**（只查 `-u` / `--databases`
   这些"形状"），于是"调用方传了客户端"这件事在测试里完全不可见。

### 13.3 根因二（更严重）：恢复 / 导入会把非 ASCII 数据**双重编码**

查这个 bug 时顺手量了字符集，发现容器里（没有 `LANG`）mysql 客户端协商出来的
是 **latin1**：`character_set_client` / `character_set_results` 全是 latin1。
而面板恢复 / 导入时既没给 `--default-character-set`，dump 里的 `SET NAMES utf8mb4`
又救不回来 —— 中文会被当成 latin1 再转一次存进库。

**实测（临时库往返：建库 → 塞 `中文测试` → dump → 删库 → 恢复 → 比 HEX）**：

| 恢复时用的客户端 | 恢复后 HEX | 结论 |
|---|---|---|
| `mysql -uroot < dump`（v0.9.6 的形状） | `C3A4C2B8C2ADC3A6E28093E280A1…` | ❌ 双重编码 |
| 同一份 dump，但 dump **去掉 `-N`**（自带 `SET NAMES utf8mb4`） | `C3A4C2B8C2ADC3A6E28093E280A1…` | ❌ 一样坏 |
| 同一份 `-N` dump + 客户端 `--default-character-set=utf8mb4` | `E4B8ADE69687E6B58BE8AF95` | ✅ 与原始一致 |

⚠️ **中间那行是这条结论里最值钱的一格**：它推翻了"把 `-N` 去掉就好了"这个
看起来最顺手的修法 —— 客户端自己的字符集不对时，dump 里那句 `SET NAMES`
**改不了客户端怎么解释它读到的字节**。所以 `-N` 保留，修在客户端参数上。

### 13.4 改法

1. 新的纯函数 `model::mysql::dump_bin(client_bin)`：**收客户端名、吐 dump 工具名**
   （`mysql` → `mysqldump`、`mariadb` → `mariadb-dump`，保留目录；
   `mysqldump` / `mariadb-dump` 幂等原样返回）。`mysqldump_args` 内部调它 ——
   调用方**没有机会**再传错；
2. `client_args` 与 `load_script` 都在客户端后面写死
   `--default-character-set=utf8mb4`：前者覆盖 SQL 控制台（顺便修了"控制台里
   `SELECT` 中文显示成 `?`"）与 dump，后者覆盖**恢复 / 导入**（那条才是数据安全）；
3. 测试改成**钉住事故**：`mysqldump_args("mysql", …)` 断言 `args[0] == "mysqldump"`
   且 `!= "mysql"`；`client_args` / `load_script` 的期望串里都带上了字符集开关。

### 13.5 端到端实测（本机容器，2026-10-16）

跑的是**面板修复后的确切 argv**（不是手写的等价命令）：

```text
① wslc exec -e MYSQL_PWD=<pw> mysql /bin/sh -c \
     "mkdir -p /tmp/wslc-backups && mysqldump --default-character-set=utf8mb4 \
      -u djxm_user -N -B --databases djxm --single-transaction --quick \
      --set-gtid-purged=OFF > /tmp/wslc-backups/djxm-repro.sql"
   → exit 0、2,318,588 字节、110 个 CREATE TABLE、sha256 0824bfdc6f81…eed6
② wslc container cp -q mysql:/tmp/wslc-backups/djxm-repro.sql <Windows 完整文件路径>
   → exit 0，Windows 侧 2,318,588 字节，sha256 同样 0824bfdc6f81…eed6（**逐字节一致**）
```

中文往返（另一轮临时库）：dump 新形状 exit 0 / stderr 空，恢复后 HEX 与原始
`E4B8ADE69687E6B58BE8AF95` **完全一致**。

⚠️ `mysqldump` 会因为**权限不够但无关紧要**的两件事往 stderr 打 `Error:` 字样
（实测：`column_masking_policy` 少 SELECT、tablespaces 少 PROCESS），
但**退出码是 0、dump 是完整的** —— 所以 [`backup_database`] 只认退出码，
不拿 stderr 判失败（这条也写进函数文档了）。

### 13.6 这一版**没做**的

- stderr 里那两条 mysqldump 警告**没有显示给用户**：`BackupState::last_message`
  这个字段从 v0.8 就留着（注释写着"最近一条 mysqldump 的 stderr"），
  但一直是 `None` —— 要显示得把 `backup_database` 的 stderr 一路带回界面；
- 备份仍然**不能中途打断**、仍然**没有"另存为"**（同 §12.5）；
- ⚠️ 仍然**必须在本机点一遍**（§8）：这次只能在命令行把 argv 形状跑通
  ——「点按钮 → 进度浮层 → 列表多一行 → 点恢复」这条界面路径没人走过。
  恢复那一步尤其要小心：它是**覆盖**操作。

---

## 14. 容器详情看不见挂载 / 启动命令 / 环境变量（v0.9.7 实机反馈）

### 14.1 现象

点容器名打开详情，只有 ID / 镜像 / 状态 / 运行时长 / 端口 / CPU / 内存 /
网络 I/O / PID。其中 `挂载` 是 `—`（而那个 mysql 容器明明挂着 `mysqldata`），
**启动命令和环境变量两行根本没有**。另外"状态 Up 2 hours"和
"运行时长 45 hours ago"并排出现，看着自相矛盾。

### 14.2 根因一：这三样信息**列表那一份数据里就没有**

| 详情要显示的 | `wslc list -a` 给的 | `wslc inspect` 给的 |
|---|---|---|
| 挂载 | `Mounts` = `mysqldata`（**只有卷名**）；`Labels` 里 `V1.Volumes` 实测是 **`[]`** | `Mounts[]` = `{Type: volume, Name: mysqldata, Destination: /var/lib/mysql}` |
| 启动命令 | `Command` 被 wslc **截断到 20 字符**（`"docker-entrypoint.s…"`） | `Config.Entrypoint` + `Config.Cmd` 全量 |
| 环境变量 | 没有这个字段 | `Config.Env` 全量 |

而详情弹窗**只读列表快照**（`state.snapshot`）。所以：

- `挂载` 走的是 `ContainerListItem::mounts_summary()`（解析 `Labels` 里的
  `com.microsoft.wsl.container.metadata` → `V1.Volumes`），而那个数组在本机
  **恒为空** —— 也就是说那一行**从来都是 `—`**，不是这个容器特殊；
- 启动命令 / 环境变量**压根没有渲染代码**，不是"读失败"。

### 14.3 根因二：「运行时长」那一格其实是 `RunningFor` = 创建至今

三个容器都对得上 `CreatedAt`：mysql 创建于 `2026-10-14 16:56 +0800` →
`RunningFor` = "45 hours ago"；eclipse-temurin 创建于 `2026-10-15 18:42` →
"19 hours ago"。真正的 uptime 在**上一行** `Status` 里（`Up 2 hours`）。
wslc 的 `RunningFor` 和 `docker ps` 同义（按 `Created` 算），所以标签写
「运行时长」是错的。

### 14.4 改法

1. `Shell::open_detail` 打开详情时**后台拉一次 `wslc inspect`**
   （§7.1：几百毫秒的子进程不能放界面线程），结果放 `AppState::detail_inspect`。
   写回之前比一次容器名 —— 用户点了 A 又点 B、或者已经关掉详情时，
   A 的数据**不能**画到 B 的详情里（否则挂载会串台）；
2. 新增两行：`启动命令`（`Entrypoint` + `Cmd` 接起来，和运行时一致）、
   `环境变量`（一行一条）；`挂载` 改成**优先 inspect**、拿不到才回落到旧摘要；
3. 三种状态**分开画**：还没拉回来 → `正在读取…`；拉失败 → `读不到：<原因>`；
   拉到了但空 → `—`。全塌成一个 `—` 的话，"还没读完"看起来就是"没有"；
4. 「运行时长」改成「已创建」（值本来就是创建至今）；
5. `ContainerInspect::command_line()` / `mounts_summary()` 是**纯函数**，
   各带单测（`core` job 里真跑）。

⚠️ 环境变量的值**原样显示**（包括 `MYSQL_ROOT_PASSWORD=…`）：和「数据库管理」页
把密码画在列表里、以及 Docker Desktop 的 inspect 一致 —— 这是用户自己机器上的
屏幕，来查 env 的人要的就是值本身。想改成"密码类键打码、点一下才显示"的话，
只改 `container_detail_overlay` 里那一处取值。

### 14.5 实测（本机 WSL 3.0.3.0，2026-10-16）

```text
$ wslc list -a --format json      （mysql 那条）
  CreatedAt   : 2026-10-14 16:56:17 +0800 GMT+8
  RunningFor  : 45 hours ago        ← 创建至今，不是 uptime
  Status      : Up 2 hours          ← 真正的 uptime
  Mounts      : 'mysqldata'         ← 只有卷名
  LocalVolumes: 1
  Labels      : com.microsoft.wsl.container.metadata={"V1":{…,"Volumes":[]}}  ← 空的
  Command     : '"docker-entrypoint.s…"'   ← 截断到 20 字符

$ wslc inspect mysql
  Config.Cmd        : ["mysqld"]
  Config.Entrypoint : ["docker-entrypoint.sh"]
  Config.Env        : ["MYSQL_ROOT_PASSWORD=…", "PATH=…", "GOSU_VERSION=1.19", …]
  Mounts            : [{Type: volume, Name: mysqldata, Destination: /var/lib/mysql}]
```

### 14.6 这一版**没做**的

- 「工作目录」/「运行用户」没加：`inspect` 里都有（`Config.WorkingDir` /
  `Config.User`，各加一行即可），这次只做用户点名的那三样；
- `ContainerListItem::mounts_summary()`（labels 那条路）**留着当兜底**，但实测
  在本机恒空。哪天确认它在别的 WSL 版本上也没数据，可以直接删掉；
- 详情里的 inspect **只在打开时拉一次**：容器被 启动 / 停止 之后这里不重新拉
  （状态、CPU 那些行来自列表快照，会自己刷新；`Entrypoint` / `Env` 本来也不会变）；
- ⚠️ 仍需**本机点一遍**（§8）：那三行画出来什么样、`正在读取…`会不会一闪而过、
  环境变量多的容器滚动条正不正常。

---

## 15. 「部署 java 应用」把镜像的 JDK 删了，而界面还说"运行中"（v0.9.8 实机）

### 15.1 现象（两张截图）

1. 「部署管理」里那行 `java` 显示**运行中 · 3 个文件**，点「日志」只有一行：
   `nohup: failed to run command 'java': No such file or directory`；
2. 容器的「日志」浮层是 **（没有输出）**。
   用户的描述是"java 容器日志和部署的 java 应用日志都看不到"。

### 15.2 根因一：**部署把目标目录清空时，连镜像自带的 JDK 一起删了**

部署（进程形态）的语义是"**先清空目标目录**再铺"（§`prepare_target_script`），
而那个应用的默认目标目录是 `/opt/<应用名>` —— 应用名正好叫 `java`，
于是目标目录是 **`/opt/java`**。而 `eclipse-temurin:8-jre-alpine` 的 JDK
**正好在 `/opt/java/openjdk`**：

```text
$ wslc exec eclipse-temurin /bin/sh -c 'ls -la /opt/java; command -v java'
total 113176
-rw-r--r-- 1 root root        63 Oct 16 06:17 app.log
-rw-r--r-- 1 root root         3 Oct 16 06:17 app.pid
-rw-rw-rw- 1 root root 115864603 Sep  3 06:12 ruoyi-admin.jar
                             ← 没有 openjdk/ 了；`command -v java` 也没有
$ wslc inspect eclipse-temurin        （镜像的 ENV 还在，但指向的东西已经没了）
  PATH=/opt/java/openjdk/bin:…        JAVA_HOME=/opt/java/openjdk
```

`/opt/java/openjdk/bin/java` 不存在 → `nohup java …` 必然失败。

⚠️ **白名单挡不住这种事**：`safe_target` 只知道"`/opt` 底下允许"，它**不可能知道
哪个镜像在 `/opt` 底下放了什么**（temurin 的 JDK、tomcat 的 `/usr/local/tomcat`…）。
这一条是"用户填的路径 + 我们的清空语义"撞上"镜像自己的布局"，跟路径白名单是两回事。

### 15.3 根因二：判活把**僵尸**当成活着，于是状态一直是"运行中"

```text
$ wslc exec eclipse-temurin /bin/sh -c 'ps -o pid,ppid,args'
  PID  PPID COMMAND
    1     0 tail -f /dev/null      ← 容器主进程（面板为了"吊住"它起的）
   29     1 [nohup]                ← **僵尸**：nohup 起不来 java 就退了
$ kill -0 29 && echo 活着
活着                               ← `kill -0` 对僵尸也返回 0
```

`status_script` 原本就是 `test -f pidfile && kill -0 $(cat pidfile) && echo R || echo S`
—— 僵尸让这条链**一路走到底**，界面于是常年显示"运行中"，而进程早就死了。
（PID 1 是 `tail -f /dev/null`，它**不回收子进程**，所以那个僵尸会一直挂着。）

### 15.4 根因三：容器日志确实**是空的**，但界面没解释

那个容器的主进程是 `tail -f /dev/null`（面板为了吊住它而起的，§11 那次留下的），
它本身不打印任何东西 —— 所以「日志」里"（没有输出）"是**正确**的。
而应用自己的输出**不在这里**：进程形态的部署把它重定向到了目标目录的 `app.log`。
两件事界面都没说，用户只能猜"日志功能坏了"。

### 15.5 改法

1. **清空之前先确认这个目录是我们的**：`prepare_target_script(target, ours)`
   - `ours = true`（本地存档里有这个目录的记录 = 我们自己铺的）→ 照旧清空重铺；
   - `ours = false` → **只允许它不存在或为空**；非空就 echo
     `__WSLC_TARGET_NOT_EMPTY__` 并**什么都不做**，`cmd::deploy::prepare_target`
     把它转成一句人话（`target_not_empty_hint`：先说结论、再说为什么、最后给出
     "换个目录 / 先去终端清空"两条出路）。
   - ⚠️ 判断与清空必须在**同一条脚本**里（不能先由界面查一次再清）：中间会有竞态，
     而且多一次 `exec` 往返。哨兵走 stdout 且脚本 `exit 0` —— 非零退出码会被
     `run_script` 先变成"参数 + stderr"的原始错误，调用方就读不到哨兵了。
   - 界面侧：`ours_target = 正在重新部署 || 存档里有同目标目录的记录`（`app.rs`）。
2. **判活认僵尸**：`status_script` 多读一眼 `/proc/<pid>/stat` 的第三个字段，
   `Z` 就当死。用纯 shell 剥 `(comm)`（`${s#*) }`）而不是 `awk`：精简镜像不一定有 awk，
   而 `comm` 里**可能带空格**，按空格切会切错。
3. **空日志给一句解释**（`logs_overlay`）：说明"主进程不写 stdout/stderr 时就是这个样子
   （比如 `tail -f /dev/null`）"，并指出应用日志在「部署管理」里。

### 15.6 实测（本机容器，2026-10-16；不是推的）

判活（`eclipse-temurin` 里那个 pid=29 的僵尸就是现场）：

| 场景 | 旧脚本 | 新脚本 |
|---|---|---|
| pidfile 里的 pid 是**僵尸**（现场那个 29） | `__WSLC_RUNNING__` ❌ | `__WSLC_STOPPED__` ✅ |
| pidfile 里是**真活的**进程（`nohup sleep 300`） | — | `__WSLC_RUNNING__` ✅ |
| 没有 pidfile | — | `__WSLC_STOPPED__` ✅ |

清目录守卫（用 `/tmp/probe-nonempty/openjdk/readme` 合成一个"像 `/opt/java` 那样
非空的陌生目录"，**不拿用户真实目录做实验**）：

| 场景 | 结果 |
|---|---|
| 非空 + `ours=false` | `__WSLC_TARGET_NOT_EMPTY__`，**目录内容原样**（`openjdk` 还在） ✅ |
| 空目录 + `ours=false` | 放行（无输出，然后 mkdir + 清） ✅ |
| 不存在的目录 + `ours=false` | 放行并建出来 ✅ |
| `ours=true` | 照旧清空 ✅ |

用户的 `/opt/java` 全程原样（`app.log` / `app.pid` / `ruoyi-admin.jar`）。

### 15.7 用户那个容器怎么救（已经坏了的部分，代码救不回来）

`/opt/java/openjdk` 已经被删，那个容器里的 `java` **回不来了**。两条路：

1. **重建容器**（干净）：面板里删掉 `eclipse-temurin` → 「创建容器」用同一个镜像,
   **「启动命令」填 `tail -f /dev/null`**（§11 那一格，这个镜像没有 `CMD`，
   不填就起来就退）→ 再「部署管理」重新部署。源 jar（`E:\code\ruoyi-admin.jar`）
   还在，**目标目录改成 `/opt/app`**（别再用 `/opt/java`）；
2. **只把 JDK 搬回去**：起一个临时容器（`wslc run -d --name jdkprobe <镜像> /bin/sh -c "sleep 600"`）
   → `wslc container cp jdkprobe:/opt/java/openjdk <本地目录>` →
   `wslc container cp <本地目录>/openjdk eclipse-temurin:/opt/java/` → 删临时容器。
   ⚠️ 这条**没实测过**（`container cp` 搬**目录**出容器的语义没验），写在这里只是备选。

### 15.8 这一版**没做**的

- **首次部署到"非空的陌生目录"会被拒**（哪怕那目录只是镜像自带的默认站点）：
  要用它就得先去「终端」自己清空。这是刻意的保守 —— 比起"悄悄删掉 JDK"，
  多一次手动清空是划算的；真嫌麻烦，可以让面板在弹窗里给一个"我知道我在干什么"的勾选；
- **僵尸不会被回收**：容器 PID 1 不回收子进程是容器自己的事，面板只能"别把它当活着"；
- 「运行中」这一列**不再撒谎**，但"启动失败"和"正常停止"在界面上仍是同一句
  「已停止」—— 想说清区别得再存一个"上一次是启动失败"的状态（这次没做，日志里有原因）；
- ⚠️ 仍需**本机点一遍**（§8）：守卫拒绝时那句话在提示条 / 弹窗里显示成什么样、
  空日志那句解释排版正常、部署成功的老路没被弄坏。

---

## 16. 「部署」不再清空目标目录 + 镜像没 `CMD` 时拦住留空提交（v0.9.10）

### 16.1 那个洞：容器重建之后，v0.9.9 的守卫就不管用了

v0.9.9 的判断是"本地存档里有这个目录的记录 → 允许清空重铺"，而记录**按容器名**索引
—— 容器删掉重建之后记录还在，于是同名的「重新部署」（目标被预填成 `/opt/java`）
会把**新容器**里的 JDK 再删一遍。同一个坑的第二只脚。

### 16.2 改法：从"整目录清空"改成"逐名删除"

`removals_script(target, members)` 只删：

- 这次要铺进去的成员名（`cmd::deploy::member_names`，复制**之前**就能算出来）；
- 存档里记着的**上一次**铺过的名字（`DeployRecord::members`，`#[serde(default)]`，
  老存档读出来是空表 → 那就只删日志 / pidfile）；
- 加 `app.log` / `app.pid`（名字是面板定的）。

名字里带 `/`、`\`、`..` 或空串时跳过。**其余一律不碰。**
`prepare_target_script` 与 `remove_script` 都换成这个。

代价（刻意的取舍，写在这里免得以后有人当成 bug）：结果目录**不再等于源目录**，
目录里不是面板放的东西会留下。

### 16.3 镜像自己没 `CMD` 时，不再让"留空"提交

`ImageFacts` 加了 `has_cmd`（`command` 把 `Entrypoint + Cmd` 拼成一句话之后信息就丢了，
这一位是拼之前单独取的；单字符串 / 空数组 / 只有 Entrypoint 三种形状都有单测）。
`confirm_create` 在"确定读到没 Cmd + 那一格空着"时拦住并把焦点送过去；
`image_facts_hint` 对这两种镜像给**两句不同的话**。

现场就是这个镜像：`eclipse-temurin:8-jre-alpine` 的 `Cmd` 是 `null`，
不填命令建出来的容器 `StartedAt 07:49:57.5535` → `FinishedAt 07:49:57.5552`
（**1.7 ms**、`ExitCode 0`），而界面上只看到"点了启动还是已退出"。

### 16.4 实测

合成一个和现场同形状的目录（`openjdk/` + `ruoyi-admin.jar` / `app.log` / `app.pid`）：

```text
铺之前:        app.log  app.pid  openjdk  ruoyi-admin.jar
旧脚本之后:    ''                                  ← openjdk 没了（§15 那次事故）
新脚本之后:    'openjdk '                           ← 我们三个没了，java 还在
删除部署之后:  'openjdk '                           ← 同上
```

---

## 17. 「不是面板铺的目录就拒绝部署」这层拦是错的（v0.9.11）

### 17.1 现象（v0.9.10 刚上线就实测到）

用户删掉了旧记录、重建容器（这次命令和端口都对了），重新部署时报：

```text
参数错误：目标目录 /opt/java 里已经有东西，而它不是面板铺的 —— 面板不会清空它。
```

### 17.2 为什么

`ours_target` 靠"存档里有没有『这个容器 + 这个目标目录』的记录"判断。用户把记录删了
（`deployments.json` 里只剩 `nginx/nginx`），于是面板认为这是"第一次往这个目录铺"，
而 `/opt/java` 非空（`openjdk/`） —— v0.9.9 那层**拒绝**就触发了。

⚠️ 关键：**这层拒绝已经没有任何安全作用了**。§16 之后删除是**逐名**的，
`openjdk/` 根本不在删除列表里。它挡住的只是用户自己的正常部署。
（而且它依赖的是**可能过期的本地状态** —— 存档记录；真正可靠的判据是**当场的目录内容**，
那是底层那层在做的事。）

### 17.3 改法：把"拒绝"降级成"干完说一句"

- `prepare_target_script`：那句探针里的 **`exit 0` 去掉** —— 目录非空时照样 echo
  `__WSLC_TARGET_NOT_EMPTY__`，但脚本**继续** `mkdir -p` + 逐名删除；
- `cmd::deploy::prepare_target` 返回 `bool`（`true` = 目录里本来有不是我们铺的东西），
  **不再**返回 `Error::InvalidArgument`；
- `app.rs`：部署**成功之后** `Toast::info` 说一句；`target_not_empty_hint` 也改成
  提醒语气（说清"面板只替换自己那几个文件，没动它们"，并给出"换目录 / 去终端自己清"
  两条选择）；
- `ours` 这个名字保留，但它现在只决定"说不说那句话"。

### 17.4 实测（就在用户那个正在跑的容器上）

```text
$ ls -A /opt/java                      →  openjdk
$ <新脚本原样跑一遍>                    →  __WSLC_TARGET_NOT_EMPTY__ ，退出码 0
$ ls -A /opt/java ; command -v java     →  openjdk ；/opt/java/openjdk/bin/java
```

也就是：**提醒照说、部署照走、`openjdk` 一根毛都没动。**

### 17.5 教训（写给下次）

**别用"拒绝"去表达"提醒"。** 底层已经保证安全之后，上层再拦一次只会挡住正常路径；
而且这层拦必须依赖某个本地状态（这里是存档记录），那个状态**随时可能是过期的**
（容器重建、用户删记录、换机器……）。要拦就得拦在**当场能验的事实**上，
只提醒就别把它做成错误。

### 17.6 这一版没做的

- 目录里"上一次铺的、这一次不铺了"的旧文件，只有存档里记着 `members` 才收拾得到
  —— 记录被删掉的那种会漏（少删，安全，但要用户自己去终端清）；
- ⚠️ 仍需**本机点一遍**（§8）：那条提醒在提示条里显示成什么样、
  以及"部署成功 + 提醒"两条 toast 的先后顺序读起来顺不顺。

---

## 18. 容器页没有「全部停止」的按钮（v0.9.12）

### 18.1 现象（用户一句话报过来的）

> 容器没有全部停止的按钮

对着界面核了一遍，属实：**「容器」页的工具栏上只有「创建容器」和「清理已停止」**，
要把正在跑的容器全停下只能一行一行点「停止」—— 8 个在跑就点 8 次、弹 8 次确认框。
而隔壁「实例列表」页早就有一颗页面级的**「关停全部」**（`DistroAction::ShutdownAll`）。
也就是说：**同一个面板里，两个域对"全停"这件事的处理是不对称的** ——
用户会先看到实例页那一颗，再回到容器页找同一颗，然后找不到。

### 18.2 为什么会有这个洞

不是漏写，是**从来没人写下过它**。

`README.md` 功能表里那句"批量停止/删除/`prune`"、`docs/PLAN.md` §4.3 的
"批量：start / stop / restart / remove / prune"（M5 那一行还标着 ✅）——
都是**计划里的形状**，真正落地的只有 `prune`（「清理已停止」），
**批量删除也从来没有过**。这正好撞上 `AGENTS.md` §5 那条"注释和实现不一致时，
后来的人会照着注释继续加"：README 里那句"批量停止"就是为什么这个洞
能一路活到今天都没人觉得奇怪。

底层并不缺能力：`cmd::container::stop(wslc, &[names])` 收的本来就是**切片**。
缺的只有界面这一颗按钮，和一条"全停"的确认文案。

### 18.3 改法：照「关停全部」对称地做一遍

- **`PendingAction::StopAllContainers { running }`**（`wslc-panel-core::state`）：
  - 和 `PruneContainers` 一样**执行时现查** `wslc list`
    （`cmd::container::list_running`），**不带着名单走** —— 用户从确认框弹出来
    到点下去之间可能又起了容器，带快照只会漏停；
  - `running` **只用于文案**（确认框一开就盖住了工具栏上那行"N 个在运行"）。
    `docs/PLAN.md` 风险表里那条"危险操作要**显示影响对象数**"说的就是这件事；
    它不参与任何判断，两者不一致时以执行结果为准；
  - 执行时若一个都没在跑 → `Ok("没有正在运行的容器")`，**不报错**：直接调 `stop`
    会撞上 `run_names` 那句"至少需要一个容器"，而"确认框开着的时候容器已经被
    别的操作停了"不是错误，是用户想要的结果已经达成；
  - 摘要（`stopped_all_message`）最多列 5 个名字、其余用"等"带过 ——
    提示条不是日志，二十个容器名会把整条撑成好几行。
- **`views::containers()`**：工具栏右侧加「停止全部」，**一个都没在跑时不画**
  （点了什么也不会发生，让用户去确认一个没有效果的动作只会让人怀疑自己看错了）；
  同一行那句"共 N 个容器"顺带补成"共 N 个容器，M 个在运行"——
  和实例页那句话同一个形状。
  ⚠️ 那个 M 取的是 `snapshot.running.len()`（= `wslc list` 不带 `-a` 的原文），
  **不是** `all` 里数 `is_running()`：执行侧问的就是同一条命令，两边必须同判据，
  否则会出现"确认框写着 3 个、实际停了 4 个"。`is_running()` 还把
  `state == "running"` 当唯一标准，而 `wslc list` 会不会列其它状态没逐个实测过
  （`wslc` 连 `pause` 子命令都没有），所以不去猜。
- **执行路径一行没动**：`Shell::confirm_pending` 本来就是容器/发行版两个域共用的
  （点击 → 一条"正在…"提示 → `background_executor` 里跑 → toast + 刷新列表）。
  `blocking_reason` 对容器域返回 `None`，也不用改。

### 18.4 实测（本机 wslc 3.0.3.0，2026-10-16）

面板会发的那条命令是 `wslc stop <名字...>`（`run_names` 拼出来的形状），
所以**对着真机逐条跑了一遍**（跑完把两个容器都停回去了，`list -a` 与测前一致）：

```text
$ wslc list --format json             # 不带 -a
（空）
$ wslc start redis ; wslc start nginx
$ wslc list --format json
redis | running
nginx | running
$ wslc stop nginx redis                ← 面板发的就是这一条
nginx                                  ← 每个名字一行，顺序与传入一致
redis
exit=0
$ wslc list --format json
（空 —— 两个都停了）
$ wslc list -a --format json
eclipse-temurin | exited   redis | exited   mysql | exited   nginx | exited
```

另外两条边界（都是无副作用的探针）：

```text
$ wslc container stop no-such-a no-such-b
找不到容器 'no-such-a'。  WSLC_E_CONTAINER_NOT_FOUND
找不到容器 'no-such-b'。  WSLC_E_CONTAINER_NOT_FOUND
exit=1                                  ← 逐个报错、把整个名单走完才退出
$ wslc container stop <两个本来就 exited 的容器>
exit=0                                  ← 停一个"本来就停着"的不算失败
```

结论（也写进代码注释了）：**"列表和执行之间容器自己退了"不会变成失败**；
真会失败的只剩"那一瞬间它被删了"这种很窄的竞态 —— 那种情况下其余容器照样停了，
错误信息里带着是哪个名字，所以不拆成 N 条命令（那只是多 N 次进程开销）。

⚠️ 上面测的是**命令层**。下面这些仍然必须**本机点一遍**（`AGENTS.md` §8）：

- 一个都没在跑时，工具栏上**不该**出现「停止全部」；
- 点下去之后，确认框里那个数字和工具栏上那句一致；
- 多个容器一起停时界面不卡（`wslc stop` 是**串行**的 —— 从上面那份实测看它是
  逐个处理，8 个可能要几秒到十几秒；走的是 `background_executor`，不是界面线程）。

### 18.5 这一版没做的

- **只做"全停"，没做"全启"**：实例页也只有「关停全部」、没有「启动全部」
  （那条取舍写在 `cmd::distro::start` 的说明里 —— 启动是"吊住 `wsl.exe`"，
  面板得替每个发行版持一个句柄）。容器这边"全启"要回答的是另外两个问题：
  要不要连带启动、启动失败的那几个怎么报 —— 不是这一版该顺手塞的。
- **行首复选框 + 任意挑选**仍然没有（`docs/PLAN.md` §4.3 那个形状）。
  这一版做的是"全部"这一档，**不是多选**；真要挑还得先有选择态，
  而且得决定它和搜索 / 筛选怎么交互。
- 批量**删除**同理没做 —— 那是不可撤销的，和"停止"不是一个危险级别。

---

## 19. 一轮只读审查之后的修正（v0.9.13）

这一轮不是新功能，是**把 v0.9 这一版交出去之后回头做的一次只读审查**：
一行一行读实现，逐条对着文档里"当初为什么这么定"的说法核，再把**能验的
假设拿到真机上验**（`AGENTS.md` §8 那类 CI 查不出来的东西）。

> ⚠️ 这一轮**不动版本号**：`AGENTS.md` §4 的版本号是**发布**那一步的事，
> 而这些改动还没合并到 main。下面用 v0.9.13 只是为了给这一轮起个名字。

### 19.1 审查是怎么做的（以及一个临时探针容器）

- **只读**读代码（`app.rs` / `views.rs` / `model::deploy` / `cmd::deploy` /
  `deploys.rs` / `columns.rs`），逐条对着 §1 那张实测表和 §12~§18 的说法核；
- **拿到真机上跑**：本机 `wslc 3.0.3.0` 在，四个容器都在（都是 Exited）。
  为了验 alpine / busybox 上的那条路，临时起了**一个**探针容器
  （`wslc run -d --pull never --name wslc-panel-review-probe
  docker.1panel.live/library/eclipse-temurin:17-jre-alpine /bin/sh -c "tail -f /dev/null"`），
  验完 `stop` + `remove`，**原有的四个容器状态没动**。

### 19.2 「重新部署」不删上一次铺进去的名字（**最要紧的一条**）

`removals_script` 的文档写着删除列表有三个来源：**这次的成员**、**存档里上一次
的成员**（`DeployRecord::members`）、外加 `app.log` / `app.pid`。
`deploys.rs` 的字段文档也写着"「重新部署」和「删除」靠它决定删哪些"。

而 `Shell::submit_deploy` 只传了**这次的**：`prepare_target(…, &member_names)`。
于是源目录里**改名 / 删掉**的文件永远留在目标目录里 —— 正是 §2.3 里
"为什么先清空再铺"那段要避免的那一类（"我明明改了，页面上还是旧的"），
而且记录随后被 `members: member_names` 覆盖，连「删除」也再收拾不到它。
**改过目标目录的重新部署更糟**：旧目录整批文件成了孤儿。

改法：新增纯函数 `wslc_panel_core::deploys::merge_members(previous, current)`
（这次的在前、上一次剩下的跟在后面、去重），`submit_deploy` 用
**存档里那条记录的 `members`** 当"上一次"。三条理由：

1. 它必须来自**存档**，不能靠"读一眼目录"—— 面板不可能知道目录里哪些是自己
   上次铺的；
2. **失败时存的是并集**（`merge_members` 的结果），成功时存的是**这一次的**：
   复制可能只做了一半，而我们已经把上一次的名字删掉了，存并集才能让之后的
   「删除」把它们都收拾干净；反过来成功时存并集的话，那些"上一次有、这一次
   没有"的名字会一代代累积，记录越读越假；
3. 纯函数住在 `wslc-panel-core`（**不是 `app.rs`**）：`app.rs` 没有
   `#[cfg(test)]`，而 CI 的测试任务**不跑 `wslc-panel`** —— 写在那儿等于没有断言。

### 19.3 一个 `progress` 槽 + "按应用算的忙碌"

界面上的忙碌标记是**按应用名**算的（`views::deploy_row_actions` 比
`p.app == app`），而执行侧的守卫是**按页面**的（`deploy.progress.is_some()`）。
两者不一致，于是 A 在跑的时候：

- B 那一行**照样画着按钮**，点下去 `start_deployment` / `confirm_remove_deployment`
  **静默 `return`** —— 一句话都不说（`AGENTS.md` §7.1：点了没反应会被当成按钮坏了）；
- `stop_deployment` 连守卫都**没有**：它会覆盖 A 的 `progress`，而 A 的回调回来时
  只判容器、不判应用，一句 `set_deploy_progress(None)` 就把 **B 的**「正在…」
  清掉 —— B 的按钮提前复活、能再点一次；
- 停止用的是 `DeployPhase::Starting`，所以那一行写着**"正在启动…"**。

三处改法：补 `DeployPhase::Stopping`（"正在停止…"）；`stop_deployment` 补同一个
守卫；守卫拦下来时统一发一句 `deploy_busy_notice`（**不静默**）；清进度统一走
新的 `clear_deploy_progress_for(app)` —— **谁的动作谁才能清自己那一格**。

### 19.4 一批界面文案没走 `strip_md`（屏幕上真的显示 `**`）

`util::strip_md` 的设计意图写得很清楚（"与其在每条文案里手工删标记，
不如在渲染处统一清掉 —— 于是它是个守卫"），但 v0.9 新加的**9 处全是裸的**：
部署页的说明卡、"共 N 条部署"那行、弹窗里四段说明、日志浮层那句、删除确认的
正文与警告。另外顺手查出**6 处这次之前就漏着**的（同一个渲染处规矩：
数据库「连接」卡、备份浮层的说明、拉取弹窗的报错说明、设置页「终端」那两段、
`image_facts_hint` 里"没有默认命令"那句）。一共补了 **15 处** `strip_md` 调用。

⚠️ 顺带修了一句**本身就过期**的话：删除确认里"被清掉的是**卷里的数据**"
—— v0.9.10 起删的只有我们自己铺过的名字，那句读起来像"整个目录被清空"。
现在说的是"删的就是**卷里**的那几个文件，卷还在，文件不会回来"。

> 教训（值得单独记）：**"渲染处统一清掉"这条守卫要真的在每个渲染处生效**。
> 一条不过，用户看到的就是 `**静态文件**` 这种字面量 —— 而 CI 一个字都不会说。

### 19.5 部署页的状态列**不参与自动刷新**

`tick → refresh_quiet → refresh_inner` 只采容器 / 镜像 / 网络 / 卷 / 发行版快照，
**不碰 `state.deploy`**。于是坐在这一页不动时：应用自己崩了、或者在「终端」里
被 `kill` 掉了，界面会**一直显示「运行中」** —— 而页面顶上那句说明恰恰写着
"「现在跑没跑」是**问容器**得来的"。两个地方对同一件事有两种说法。

改法：`tick` 里加 `tick_deploy_status`，只在**这一页**、没有动作在跑、
而且已经 `loaded` 时触发；**慢档**（`deploy_status_every_n_ticks = 3`，
按 `prefs.refresh_secs`＝3 秒算约 9 秒一轮）—— 因为那里**每一条部署都是一次
`wslc exec`**，跟着快照 3 秒一轮太重。

⚠️ 慢档重探**保留旧值**（`refresh_deploy_status_quiet` 不清 `probed` / `process` /
`files`）：不然每 9 秒整列都会闪一下「读取中…」，比"数据晚几秒"难受得多。
另外给 `refresh_deploy_status_inner` 补了"已经在问就不叠第二次"的守卫
（慢档轮询和动作回调完全可能撞上）。

### 19.6 nginx 的 Web 根不再认 `/etc/nginx`（§4 那条更正）

早先推 Web 根时会退到 `/etc/nginx` 底下的挂载点。而 `-v ./conf.d:/etc/nginx/conf.d`
是**极常见**的挂法，那种容器的站点内容往往烤在镜像里、根本没有挂载 ——
于是面板把**配置目录**预填成"容器内目标目录"：铺进去没有任何效果（nginx 不把
`.html` 当配置读），而源目录里若有个 `default.conf`，`removals_script` 会先删掉
容器里那个**同名配置**再覆盖它。现在只认 `/usr/share/nginx` 底下（且不是它自己），
拿不到就回落 `/usr/share/nginx/html`。回归钉是
`config_mounts_are_never_treated_as_the_web_root`。

### 19.7 默认应用名从"运行时键"改成 `app`

`open_deploy_dialog` 早先把 `AppRuntime::key()` 当应用名初值 → Java 容器预填出
`java` → 默认目标目录正好是 **`/opt/java`**，即 §15 那次"把镜像自带的 JDK 删掉"
的现场路径。逐名删除之后面板已经不会删它了，但把用户往那个路径上引没有任何好处
（而 §15.7 给用户的自救建议恰恰是"改成 `/opt/app`"）。现在是
`model::deploy::DEFAULT_APP_NAME = "app"`，理由写在那个常量上。

### 19.8 `classify_error` 认不出 `mkdir` / `cp` 真正的报错

本机实测（探针容器）三种形状的原文：

```text
mkdir -p <已存在的文件>        → mkdir: cannot create directory '/tmp/afile': File exists
mkdir -p <穿过一个文件的路径>   → mkdir: cannot create directory '/tmp/afile': Not a directory
container cp … /tmp/nope-dir/  → exit 1「Could not find the file …」+ ERROR_PATH_NOT_FOUND
```

这三条**一条都不命中**原来的关键词（`"not found"` 匹配不到 `not_found`
下划线形态，`"cannot find the path"` 匹配不到 `Could not find the file`），
全部落到 `Other` —— 而 `Other` 的 `hint()` 是 `None`：用户只拿到一句引擎原文，
尽管 `PathNotFound` 那句"换一个目录试试"就是对的。现在它们进 `PathNotFound`
（判据就是上面这三行原文，另有单测钉住）。

### 19.9 死代码与"测的不是跑的那份"

- `cmd::deploy::is_running` **没有调用方**（界面每一行问的是 `probe_state`：
  一条脚本同时拿"跑没跑 + 目录里几个文件"）→ 删掉，并在原处留一句说明；
- `DeployProbe::publishes` **只有测试在调**，而界面另写了一份内联判据 →
  两边的判据合成 `model::deploy::port_is_published`（纯函数 + 单测），
  界面和 `DeployState` 都调它。**"被测的那份没人用、在用的那份没被测"
  是这仓库最忌讳的形状**；
- **tar 时代的遗留**随方案一起清掉：`DeployErrorKind::BadArchive` 已经造不出来
  （`classify_error` 里那三条 tar 关键词是死的）、`NoHostTar` **从第一天就
  不可达**，两条 hint 还分别写着"看原文里的 tar 报错"和
  "这台机器上没有 tar.exe" —— 而搬运早就改成 `wslc container cp` 了。
  `MissingTool` 的 hint 里那句"部署需要 /bin/sh **和 tar**"也删了。

### 19.10 容器页「操作」列的权重 330 → 440

「部署管理」之后这一格最多 **7 个**按钮（终端 / 数据库 / 部署管理 / 重启 /
停止 / 日志 / 删除，共 17 个汉字），而 330 那一版是按**"最多 5 个按钮"**估的
（注释里就写着 5）。`columns.rs` 在 `DB_COLUMNS` 那段刚记过一次**实机被裁**的
教训（4 个按钮 / 11 个字在 ≈273px 里被裁，最后给到 360/1390 ≈ 26%），
还写下规矩"加按钮时顺手把这个数往上调"。按同一套口径量：17 个字 ≈ 380px，
留余量 → 权重 ≈ 410~440，取 **440**。

⚠️ 这个数**是算出来的（推的）**，`AGENTS.md` §8 那条"实机看一眼会不会挤"
仍然成立 —— 只是 330 那一版连算都没算。

> ⚠️ **后来被 §20 推翻了**（紧接着的 v0.9.14）：实机截图里那一列还是两行 ——
> 因为问题不是"列不够宽"，而是**7 个按钮根本不该全铺出来**。
> 现在行内只留 4 个 + 「更多」菜单，权重收回到 **340**。

### 19.11 文档与杂项

- **README 的「部署管理」行**还在写"部署会**先清空目标目录**再铺"和
  "删会把容器里的目录清空" —— v0.9.10 起就不是了。已按现在的语义重写
  （按名字替换 + 别人东西不动 + 事后提醒 + 状态自动刷新）；
- **`nav_id` 的注释**（`app.rs`）写着"这个 match 只覆盖 `Page::ALL` 里那 8 页"，
  从 `AddInstance` 起就不准了（现在有 11 支）—— §10.1 里当初就点名要改，没改；
- **`.github/workflows/release.yml` 结尾的换行**被上一次编辑吃掉了（老版本有），
  每次改它都会带一行 `\ No newline at end of file` 噪音 → 补回；
- `wslc-panel-core::state` 里 `AppState` 那句文档注释行尾多了个 `///`
  （`/// 应用的完整状态。///`）→ 删掉。

### 19.12 本机实测：复核了什么、**推翻了**哪两个怀疑

审查里有两处我本来准备报成 bug，先在真机上验了 —— 结论是**代码没问题**，
所以没有改（记在这里，免得下一个人再去查一遍）：

1. **`count_entries_script` 用了 `find -mindepth 1 -maxdepth 1`**（GNU 扩展），
   我怀疑 alpine 的 busybox find 不支持。实测（busybox **1.37.0** / alpine 3.24）：
   `find --help` 里列着 `-mindepth`，`find /tmp/probe -mindepth 1 -maxdepth 1 | wc -l`
   给 **3**（= 真实顶层条目数）、rc=0 → **可用**；
2. **僵尸判活那套** `/proc/<pid>/stat` + `${s##*) }`：在同一镜像里复现了 §15 的
   现场（`nohup /nonexistent-xyz &` → pid 46 是 `[nohup] Z`、PPID=1），实测
   `kill -0 46` → **rc=0**（老脚本会说 RUNNING），新脚本给
   **`__WSLC_STOPPED__`** → 在被测的那个 shell 上**真的生效**。

顺带把搬运那条链也复核了一遍（全部与 §1 一致）：`cp --help` 里 **`-q` 真实存在**；
目录 cp **确实多套一层**（`/tmp/probe2/site/…`）、逐个顶层条目**落平**
（`/tmp/probe3/index.html`、`sub/x.txt`）；单文件权限 **`-rw-rw-rw-`（666）**、
目录 777；cp 到不存在的目录 **exit 1**。整套起停判活（prepare → start → probe →
tail → stop → remove）在 alpine 上跑通，`ours=false` 时哨兵照打、外来目录
（合成的 `openjdk/`）**一个字节没动**。

### 19.13 这一轮**没做**的 / 仍需本机点一遍

- **孤儿文件只在我能算出来的时候收拾**：目标目录被改过（换过一次路径）时，
  **旧目录**里那一批文件面板再也找不到 —— 记录里只留了新目标。要处理得存
  "历史目标目录"，那是另一件事（记录结构要动）；
- **`progress` 仍然是一个槽**：这一轮做到的是"A 在跑时点 B 一定有反馈、
  也不会串台"，**不是**"两个部署能并行"。并行要管子进程池和错误汇总，
  收益只在"一次铺好几个应用"时才明显；
- ⚠️ **必须在本机点一遍**（`AGENTS.md` §8，CI 只证明编译过 + 纯逻辑测试过）：
  1. 源目录**改名一个文件**后点「重新部署」→ 进容器 `ls` 看旧名字**没了**
     （这是 19.2 的唯一判据）；
  2. A 应用部署中，点 B 那一行的「启动 / 停止 / 删除」→ 出的是**"已经有一个
     部署动作在跑"**，而且 A 那一行的「正在…」**不会**被清掉；
  3. 点「停止」时那一行写的是**"正在停止…"**；
  4. 部署页的说明文字里**不再出现** `**` 或反引号（一行一行看）；
  5. 在这一页坐着不动 **≥9 秒** → 状态列自己更新（把容器/app 在别处停掉试）；
     而且**不该**整列闪「读取中…」；
  6. 容器行「操作」列在 1707 逻辑宽下**没被裁** —— v0.9.14 起这一格只剩 4 个按钮
     （见 §20），不再是 §19.10 那个 440 权重的形状了；
  7. `-v ./conf.d:/etc/nginx/conf.d` 的 nginx 容器：目标目录预填应该是
     **`/usr/share/nginx/html`**，不是那个配置目录。

---

## 20. 容器行操作收进「更多」菜单（v0.9.14）

### 20.1 起因：实机截图里那一列被挤成了两行

v0.9.13 发出去之后，用户在容器页截了一张图：`[终端][部署管理][重启][停止]` 一行、
`[日志][删除]` 又一行 —— **7 个按钮铺满两行**。而 §19.10 刚刚把这一列的权重从
330 提到 440，理由是"要装下这 7 个"。

用户的要求很直接：**行内只留 `数据库` / `部署管理` / `启动-停止` + 一个「更多」**，
其余的点「更多」弹出来（他给的参考图就是那种 `终端 日志 更多` + 弹出菜单的形状）。

> 这一条把 §19.10 的 440 **反向修正**回 340。教训是：
> **"列太窄装不下"有时候不是列的问题，是按钮太多** —— 加宽列只治了症状，
> 实机截图里那一列照样两行。§19.10 那段自己也写着"仍需实机看一眼"，这一眼
> 看出来的结论是"别把 7 个按钮全铺出来"。

### 20.2 现在长什么样（行内最多 4 个）

| 容器状态 | 行内 | 「更多」菜单里 |
|---|---|---|
| 运行中 | 数据库（仅 MySQL 家族）· 部署管理（仅 nginx / Java / Node / Go）· 停止 · 更多 | 终端 · 重启 · 日志 · ─── · 删除 |
| 已停止 | 启动 · 更多 | 日志 · ─── · 删除 |

每一档的取舍：

- **「数据库」「部署管理」必须留在行内**：它们是**只能从这一行进**的两个页面
  （都不在侧边栏里），藏进菜单等于把唯一入口埋起来；
- **「启动 / 停止」留在行内**：这一页最常点的动作，也是唯一"看着状态点"的动作；
- **「终端 / 重启 / 日志 / 删除」进菜单**：低频或危险 —— 删除有二次确认、
  日志是排查时才看、终端只有运行中才有意义；
- **菜单里只画当前有意义的**：停止的容器不画「终端」（`wslc exec` 在它上面必然
  失败，和 §6.2 那条"停止的容器不画「数据库」/「部署管理」"是同一个原则）；
- 删除放最后、前面一条**分隔线**：它是唯一不可撤销的那个。0.7.1 的
  `PopupMenuItem` 没有危险态配色，所以这里**不靠颜色**，靠分隔线 + 二次确认。

### 20.3 用的是 gpui-kit 0.7.1 自带的下拉菜单

```rust
Button::new(id).label("更多").small()
    .dropdown_menu(|menu, _window, _cx| {
        menu.item(PopupMenuItem::new("终端").on_click(move |_, _, cx| { … }))
    })
```

来自 `gpui_kit::component::menu` —— 它是个**扩展 trait**（`DropdownMenu`），
和 `ButtonVariants` 一样**必须显式导入**，光导入 `Button` 是不够的。
三条前提都是**读 0.7.1 的源码**确认的（本机没有 Rust 工具链，跑不起来界面）：

1. **不用自己初始化**：`gpui_component::init` 里已经调了 `menu::init(cx)`
   （`src/lib.rs:145`），而我们 `main.rs` 调的是 `gpui_kit::init(cx)`；
2. **点完自动关**：`PopupMenu` 先调 item 的 handler、然后自己
   `dismiss(&Cancel, …)`（`popup_menu.rs` 里那段 `handler(…); … self.dismiss(…)`），
   所以回调里**不要**手动关菜单；
3. **不会被表格的 `overflow_hidden` 裁掉**：菜单走组件的 popover 层
   （`deferred` + `POPUP_PRIORITY`），和 `Select` 的下拉是同一套 ——
   这也是"表格行里能弹菜单"的前提（`AGENTS.md` §7.5）。

⚠️ **需要实机确认的三件事**（CI 只证明类型对）：菜单会不会被表格容器裁掉 / 点不到、
锚点位置与期望是否一致、行内那 4 个按钮在 1707 逻辑宽下排不排得下。

### 20.4 顺带改掉的文案（这一步不能省）

行内少了一半按钮，"其余动作在哪儿"的那几句话**必须跟着改** —— 否则用户按老习惯
去行内找「终端」，找一圈只看到"启动/停止"，会以为功能被删了
（`AGENTS.md` §5：过期文案会让后来的人照着它继续错）。改了三处 + README：

- 容器详情弹窗的动作区说明与那句 `hint`（终端 / 重启 / 日志 / 删除 都在「更多」里）；
- 数据库页探测失败卡里那句"想自己进去看，用「容器」页那一行的「终端」"；
- 部署页"看起来不是这类容器"那句里的「终端」；
- README 的「全部容器」行（改成"行内最多 4 个动作 + 「更多」菜单"）。


