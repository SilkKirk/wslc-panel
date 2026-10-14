# PLAN v0.6 —— 「镜像仓库」：在线浏览 → 拉取 → 启动

> 需求原话：「需要一个镜像仓库的功能，可以在线查看镜像仓库，然后拉取启动」。
>
> 三个已经定下来的选择（用户 2026-10-13 确认）：
> 1. **只做公共加速站 + 内置常用官方镜像**，私有仓库（Harbor / CNB / ACR）留到后面的阶段；
> 2. **新增一个独立的「镜像仓库」页**（不是把「镜像」页那个输入框改掉）；
> 3. **内置 registry 只用 `docker.1panel.live`**，`docker.1ms.run` 不用。
>    → 直接后果：这一版**没有 token / 401 挑战那套代码**（见 [§3.4](#34-认证这一版一行都没有)），
>    因为它是实测下来唯一连 manifest 都免认证的站（§2.1）。
>
> 第 2 条与 `AGENTS.md` §5「同一个动作只给一个入口」有冲突。
> **最初打算把「镜像」页那个按钮删掉**，真做的时候发现那会砍掉一个能力
> （新页只连内置 registry，而用户现有的 nginx 是从 daocloud 拉的）——
> 于是改成"两个入口都留、把意图在措辞上分开"。
> 经过与理由都在 [§3.6](#36-两个拉取入口没有删改成把意图分开)。

---

## 1. 现状：缺的是「浏览」，不是「拉取」

拉取这条链**已经全有了**，而且是本项目里做得比较厚的一块：

| 已有的东西 | 在哪 |
|---|---|
| 流式拉取（每行回调、可取消、无内建超时） | `wslc_core::cmd::image::pull_streaming` |
| 进度状态（200 行上限、`push_lines` 返回"是否真有新增"） | `wslc_panel_core::state::PullProgress` |
| 拉取浮层（输入框 → 进度 → 日志滚动 → 取消） | `views::pull_dialog_overlay` |
| 拉取动作（200 ms 轮询 + 取消令牌 + 成功/失败 toast） | `app::Shell::start_pull`（约 `app.rs:1000`） |
| 建容器表单（镜像/名称/端口/环境变量/卷/网络/资源限制 + 等效命令预览） | `app::CreateDialog` + `views::create_dialog_overlay` |

缺的是**在这条链之前的那一步**：用户得自己知道镜像的**全名**。
现在「镜像」页那个拉取框是一个纯输入框 —— 名字打错一个字母，
轻则立刻 `NAME_UNKNOWN`，重则挂在 `pull` 那条 600 秒超时上（`cmd/image.rs` 的 `pull`）。

再加一条本项目早就记着的约束（`cmd/image.rs` 的注释、`PLAN.md` §5 都写了）：
**本机直连 Docker Hub 会超时**。这一版要先确认它现在是否还成立，以及
"既然直连不通，浏览该从哪儿拿数据"。

---

## 2. 本机实测（2026-10-13，全部是这一版方案的事实依据）

### 2.1 谁通、谁不通

用系统自带 `curl.exe`（8.21.0）逐个打，`-m 8`：

| 目标 | 结果 |
|---|---|
| `registry-1.docker.io/v2/` | **000**（8.0 秒超时） |
| `auth.docker.io/token?...` | **000** |
| `hub.docker.com/v2/repositories/library/alpine/tags` | **000** |
| `hub.docker.com/v2/search/repositories/?query=nginx` | **000** |
| `docker.1ms.run/v2/library/alpine/tags/list` | **200**（2.1 s，2067 B） |
| `docker.1panel.live/v2/library/alpine/tags/list` | **200**（2.7 s，2067 B） |
| `docker.m.daocloud.io/v2/library/alpine/tags/list` | **401**（0.3 s，要认证） |

**结论一：Docker Hub 那条路整条是死的。** 三个域名全 000 —— 不只是 registry，
连**搜索接口和标签接口**都不通。所以"在线查看镜像仓库"**不能照 Docker Hub 的 API 做**，
市面上那些"镜像搜索"界面依赖的 `hub.docker.com/v2/search`、`.../tags`、星数、官方标识，
在这台机器上一个都拿不到。

**结论二（这一版的关键）：用户已经在用的加速站，自己就是一个完整的 Registry V2 代理。**
`docker.1ms.run` / `docker.1panel.live` 的 `/v2/<repo>/tags/list` 免认证可读。
于是**浏览和拉取可以走同一个 host** —— 界面上看到的 tag，就是 `wslc pull` 马上能拉的那个。
这比"从别人的服务拉一份静态清单"强在：不存在"清单里有、但拉不到"这种状态。

### 2.2 Registry V2 到底能给什么、不能给什么

| 能力 | 实测 | 说明 |
|---|---|---|
| 列某个仓库的 tag | ✅ `GET /v2/<repo>/tags/list` | 一次拿全量。alpine 2067 B / 1.3 s；nginx 23757 B / 1.7 s；redis 17955 B |
| 短名要不要自己补 `library/` | ⚠️ **各站不一致，必须本地补** | 见 §2.3 |
| 分页 | ✅ `?n=5` 生效，响应头带 `Link: </v2/...?last=...&n=5>; rel="next"` | 但我们**不用**它（见 §3.5） |
| 算镜像体积 | ✅ 可以，但要走两次 | index → 挑 amd64/linux → manifest → 累加 `layers[].size` + `config.size` |
| 列"全部仓库" | ❌ `/v2/_catalog` → **401** | 加速站不开放。所以**列不出**所有仓库，只能按名字查 |
| 搜索 | ❌ | 没有这个接口（Docker Hub 的搜索在 `hub.docker.com`，实测 000） |
| 星数 / 官方标识 / 最近更新时间 | ❌ | 同上，只有 `hub.docker.com` 有 |
| 限流 | ⚠️ 两台都有 | `1ms.run`：`Ratelimit-Limit: 1000;w=3600`；`1panel.live`：`x-uri-ratelimit-limit: 100` |

体积的实测样本（`docker.1panel.live`，累加压缩层）：

| 仓库 | 层数 | 累加（**压缩后**，即下载量） |
|---|---|---|
| `library/alpine:latest` | 1 | **3.7 MB** |
| `library/nginx:latest` | 7 | **60.9 MB** |

> **试过内容协商，省不掉那一次请求**（2026-10-13 实测）：
> 只接受 `application/vnd.docker.distribution.manifest.v2+json`、
> 只接受 OCI manifest、两者都收 —— 三种 `Accept` 拿回来的是**同一个**
> 9218 B 的 OCI image index。所以"index → 再要一次"是必然的两跳，不是没优化。

### 2.3 跨站不一致 —— 要在**我们自己的纯函数**里抹平

> ⚠️ 第 2、3 条在"只用 `docker.1panel.live`"的前提下**用不到**。
> 留着它们有两个理由：它们正是"**为什么选这一家**"的证据，
> 同时是将来要加第二个 registry 时的**待补清单**。

1. **短名补 `library/`（这一条现在就要做）**：
   - `docker.1ms.run/v2/alpine/tags/list` → **200**，而且返回体里 `name` 已经被它规范化成
     `"library/alpine"`；
   - `docker.1panel.live/v2/alpine/tags/list` → **404 `NAME_UNKNOWN`**。
   → 同一个界面，在两台站上一台能用一台不能。**所以规范化必须本地做**，不能指望 registry。
2. **manifest 的认证要求不同**：
   - `1panel.live`：`GET /v2/library/alpine/manifests/latest` → **200**，直接给
     OCI image index（9218 B，带 `docker-content-digest`）。**完全免认证。**
   - `1ms.run`：同一个请求 → **401**，响应头
     `WWW-Authenticate: Bearer realm="https://docker.1ms.run/openapi/v1/auth/token",
     service="docker.1ms.run", scope="repository:library/alpine:pull"`。
     照着 realm 取一次 token 再带 `Authorization` 重试 → **200**。
     → 注意：**它 `tags/list` 免认证、`manifests` 要认证**。
     所以认证不能做成"进这个 registry 先登录一次"，得做成
     **任何一次请求收到 401 就现场走一遍挑战流程**。
   - `daocloud`：连 `tags/list` 都 401，realm 在**另一个域名**
     （`https://m.daocloud.io/auth/token`）。
3. **token 接口的字段名不是 `token`**：`1ms.run` 的 token 响应是
   `{"access_token":"eyJ...", ...}`（1775 B）。
   只认 `token` 字段 → 拿到"有 token 却还是 401"这种查不动的现象。两个字段都要认。

顺带：错误体是标准的 V2 信封，**可以直接拿来给用户看**：

```json
{"errors":[{"code":"NAME_UNKNOWN","message":"repository name not known to registry",
            "detail":{"name":"library/no-such-image-xyz"}}]}
```
```json
{"errors":[{"code":"DENIED","message":"requested access to the resource is denied"}]}
```

### 2.4 端到端真拉了一次

```
wslc pull docker.1panel.live/library/hello-world:latest
  latest: Pulling from library/hello-world
  4f55086f7dd0: Pulling fs layer / Downloading / Verifying Checksum /
                 Download complete / Extracting / Pull complete
  Digest: sha256:5e23090353324d887c48ad5e5c56d294eab81588df9605b07d1afe895f9cc8f8
  Status: Downloaded newer image for docker.1panel.live/library/hello-world:latest
```

**退出码 0，7.0 秒**，拉完 `wslc images` 里就是
`docker.1panel.live/library/hello-world:latest`（`Size: 10.1kB`）。

两个结论：
- 加速站确实提供 blob，**拉取这条链不用改一行**；
- 输出的行形态（`Downloading` / `Pull complete`）正是现在 `PullProgress` 吃的那些行 ——
  新页复用现有进度浮层即可。

### 2.5 ⚠️ 体积的口径：两个数都对，但差 2.7 倍

| 数字 | 来源 | nginx 实测 |
|---|---|---|
| **压缩后**（下载要下多少） | registry manifest 累加 `layers[].size` | **60.9 MB** |
| **解压后**（占本地磁盘多少） | `wslc images` 的 `Size` 字段 | **163 MB**（同一台机器上那个 daocloud 的 nginx） |

同一台机器、同一个镜像，**2.7 倍**。如果界面上把 manifest 算出来的数直接写成"大小"，
用户拿它跟「镜像」页里的 `Size` 一对，就会当成 bug 报上来。
**必须在两个地方各自标清口径**（新页写"下载约 61 MB"，本地镜像页写"占用 163 MB"）。

---

## 3. 方案

### 3.1 分层与落点（按 `AGENTS.md` §2 / §5）

```
crates/wslc-core/src/
├── registry.rs          纯函数（可脱离 Windows 单测）
│                        · 引用规范化：alpine → library/alpine
│                        · URL 拼装：tags/list、manifests/<ref>
│                        · tags/list 解析（含 tags 为 null / 空）
│                        · V2 错误信封解析（NAME_UNKNOWN → 中文一句话）
│                        · WWW-Authenticate 挑战解析
│                        · token 响应解析（token / access_token 两个字段）
│                        · manifest 选择（index → amd64/linux）+ 体积累加
│                        · tag 排序（latest 优先 → 版本倒序）
├── mirrors.rs          不动（那是 WSL **发行版**的镜像站：tar.xz / .wsl 包）
└── cmd/registry.rs     起 curl.exe 的那一半：拉 tag、拉 manifest、走认证
                        （不能新增依赖，理由同 mirrors.rs，见 §5）

crates/wslc-panel-core/src/state.rs
  RegistryState        · 当前 registry / 仓库名 / tag 列表 / 过滤词 / loading / error

crates/wslc-panel/src/
  views.rs             registry_page（新页）
  app.rs               Shell::refresh_tags / open_registry_page / pull_from_registry
```

**为什么不塞进 `mirrors.rs`**：`mirrors` 管的是"某个镜像站上有哪份 rootfs 文件"
（清单是 wslui 的接口、下的是 tar/`.wsl` 包），这一版管的是"某个 registry 里某个仓库
有哪些 tag"（Registry V2 协议）。两者唯一相似的地方是都借 `curl.exe` ——
那部分已经是现成的 `cli::run_helper_with_hint` / `cli::spawn_streaming`。

### 3.2 数据流

```
选 registry（内置三条）
   ↓
输仓库名（或点内置常用镜像）
   ↓  规范化：alpine → library/alpine（本地做，见 §2.3）
GET <host>/v2/<repo>/tags/list        ← 401 就现场走一遍挑战
   ↓  解析 + 排序（latest 优先、版本倒序）
tag 列表（可本地过滤）
   ↓  选中某个 tag → GET manifests/<tag> → 挑 amd64/linux → 累加体积
「拉取」→ wslc pull <host>/<repo>:<tag>   ← 复用 pull_streaming + PullProgress
   ↓
「用这个镜像创建容器」→ 打开 CreateDialog（预填镜像名，pull 策略 = Never）
```

### 3.3 内置的 registry 与常用镜像

`RegistrySource { host }` —— 这一版**只有一个**：`docker.1panel.live`（用户选定）。
它是实测下来唯一一个 tag 与 manifest **都免认证**的（§2.1），所以整条链路
一次 401 都不会遇到。

常用镜像清单内置一批作为**入口**（界面上是一排小按钮，点一下就填进输入框）：

```
nginx  redis  mysql  postgres  mongo  alpine  ubuntu  node  python  hello-world
```

> 每一项都在 `docker.1panel.live` 上实测过 200（2026-10-13）——
> 不是从文档抄的，**界面上的快捷按钮点下去 404 是最难解释的一种**。
>
> 刻意只放 10 个：`PLAN-v0.5.md` §4.4 记着"几十个按钮铺满一屏 + 外面再挂一个搜索框"
> 是已经被否掉的做法（那次的结论是改用下拉框）。这里 10 个排成两行是可接受的，
> 再多就该换成下拉框了。

> **为什么需要这份内置清单**：因为**没有搜索接口**（§2.2）。
> 一上来给一个空输入框，等于把"该输什么"整个推给用户 ——
> 而这恰恰是这一版要解决的问题（§1）。

### 3.4 认证：这一版一行都没有

因为选定的 `docker.1panel.live` 对 tag 和 manifest 都免认证（§2.1 实测）。

⚠️ **别把"没有认证代码"读成"registry 不需要认证"** ——
同一张表里另外三家都要（`1ms.run` 要它自己域名下的 Bearer token、
`daocloud` 的 realm 在另一个域名、Docker Hub 直接不可达）。
真加第二家时，要补的东西是：

```
如果 401:
  从响应头解析 WWW-Authenticate: Bearer realm=..,service=..,scope=..
  GET <realm>?service=..&scope=..   → 取 token
     ⚠️ 字段名是 access_token 而不是 token（实测 1ms.run 的响应是 1775 字节）
  带上 Authorization: Bearer <token> 重试**原请求**
```
- 认证要挂在**每一次请求**上，不能做成"进这个 registry 先登录一次" ——
  实测 `1ms.run` 的 `tags/list` 免认证、`manifests` 要认证，两者不一致；
- token 只在内存里缓存，key 是 `(host, repo)`（scope 是按仓库发的），**不落盘**；
- 401 重试只做**一轮**（token 也拿不到就直接报错，不要循环）。

私有仓库（`wslc registry login` 的凭据 → `Authorization: Basic`）留到 P4。

### 3.5 界面（新页 `Page::Registry`）

> ⚠️ **下面记的是 v0.6 当时的做法。** v0.6.2 按 §3.6/§3.7 合并回一页，
> 其中两条被推翻了：registry **改成了可编辑字段**（不再是"只显示它是谁"），
> tag **改成可搜索下拉框**（不再有 200 行上限与本地过滤）。
> 这两处现在的样子见 §3.7。

三段：

1. **registry**：这一版只有一个，所以**不放选择器**，只在页面上显示它是谁
   （一行小字：`docker.1panel.live`）。出问题时那句话要带上它 ——
   用户得知道"是这家站的问题"，而不是"这个应用坏了"。
2. **仓库**：输入框 + 内置常用镜像的快捷块；输入框失焦/回车时做**规范化预览**
   （`alpine` → `library/alpine`），让用户看见我们打算发什么过去。
3. **tag 列表**：
   - **一次拿全量，本地过滤** —— 不按 `Link` 分页。理由是实测数据：
     nginx 的 1339 个 tag 一共 23.7 KB / 1.7 s，一次拿完比"翻页 + 增量状态"简单得多，
     而且本地过滤是即时的（翻页过滤要等网络）；
   - **排序是必须的**：接口给的是**字母序**。实测 alpine 的 224 个 tag 里，
     前 12 个是 `2.6, 2.7, 20190228, 20190408, …`，`latest` 排在**第 224 位（最后一个）**，
     `3.1x` 系列的 127 个挤在中间。照原样显示 = 把最没用的排最前面。
     排序规则：`latest` 第一 → 版本号倒序（纯函数，有单测）。
   - **最多渲染 200 行**（`registry::MAX_VISIBLE_TAGS`）：实测 `node` 有 **9170** 个 tag、
     `jenkins/jenkins` 7324 个，全渲染等于每帧建几千个元素。截断时要说清
     "共匹配 N 个，只显示前 200 个 —— 继续输入可以缩小范围"，否则用户以为就这么多；
   - 选中某个 tag 才去要 manifest 算体积（**不要**为列表里每个 tag 都请求一次 ——
     1339 个 tag 就是 1339 次请求，直接撞限流）；
   - 体积如实写成「**下载约 61 MB**」，不写"大小"（§2.5）；
   - 顶部一行：这份 tag 列表来自哪个 registry、什么时候拉的。

其余按仓库既有约定：

- 网络动作一律 `cx.background_executor().spawn(...)`，**点下去先出"正在加载 tag…"**
  （`AGENTS.md` §7.1：没有它，用户会以为按钮坏了然后点第二次）；
- `Select` 是本项目用过的组件（v0.5.13 的两个下拉框），但它的展开/搜索/键盘行为
  **CI 一条都验不到**（`AGENTS.md` §8）—— 这一版尽量用按钮 + 输入框 + 列表，少引入组件；
- 滚动容器记得 `.id(...)`（漏了是编译错误，`ui` job 会拦）；
- 新页要进 `Page::ALL`，而 `state.rs` 里有一条断言 `Page::ALL.len() == 8` 的测试，
  **改了要一起改**（那个断言在 v0.4 加 About 页时已经抓过一次）。

### 3.6 两个拉取入口：**没有删，改成把意图分开**

原计划（本文档第一版）是把「镜像」页右上角那个「拉取镜像」按钮改成**跳转**，
理由写得很足：用户选了"新增独立页"，于是它和「镜像仓库」页的「拉取」成了
同一个动作的两个入口，而 `AGENTS.md` §5 与 `PLAN.md` §4.2 记着一条**实机教训**
（v0.5.11：停止的容器在列表行和详情里各有一份一模一样的「启动 / 日志 / 删除」）。

**真做的时候发现照原计划会砍掉一个能力**：

- 「镜像仓库」页的浏览只连内置的 `docker.1panel.live`（§3.3）——
  它是实测唯一一个 tag 与 manifest **都免认证**的站；
- 而用户**现有的 nginx 就是从 `docker.m.daocloud.io` 拉的**（`wslc images` 实测）；
- 也就是说 `docker.m.daocloud.io/library/nginx:latest` 这种引用，
  新页**根本查不出来**（daocloud 的 `tags/list` 就要 401）。
  把弹窗删掉 = 从此只能从 1panel.live 拉镜像，**这是一次能力回退**。

所以改成：**两个入口都留，但把"用户的意图"在措辞上分开**。

| 入口 | 用户的处境 | 能做什么 |
|---|---|---|
| 「镜像」页 → **「按引用拉取…」** | 我已经知道完整引用（可以是**别的 registry**） | 直接填引用就拉 |
| 「镜像仓库」页 | 我不知道有哪些 tag | 库名 → tag 列表（排序 / 过滤）→ 体积 → 拉 |

这样"该点哪个"是有客观答案的（知道要什么 vs 要先看有什么），
而不是两个行为完全一样的按钮让人猜 —— 那条教训要避的是**后者**。
弹窗里也写了这句话，并把"不知道有哪些 tag 就用「镜像仓库」页"直接写出来。

> ⚠️ **这是个待用户拍板的取舍**：如果认为"只留一个入口"更重要，
> 正确的合并方向是**让「镜像仓库」页的 registry 变成可编辑字段**
> （默认 `docker.1panel.live`），然后才删掉弹窗 ——
> 那样不会丢能力。反过来先删弹窗就是回退。已写进 PR 描述等确认。

### 3.7 v0.6.2 修订：按 §3.6 那条路合并回一页（用户拍板）

用户确认走 §3.6 里写的那个方向。落地的样子：

| 项 | v0.6 | v0.6.2 |
|---|---|---|
| 页面 | 「镜像」+「镜像仓库」两页（`Page::ALL` 9 页） | **一页「镜像」**（8 页）：拉取区 + 本地镜像表 |
| 拉取入口 | 「按引用拉取…」弹窗 + 「镜像仓库」页的「拉取」 | **只有一个**「拉取」（`AGENTS.md` §5 重新成立） |
| registry | 常量 `DEFAULT_REGISTRY`，页面上只显示 | **可编辑输入框**（默认仍是内置那个） |
| tag 选择 | 本地过滤 + 只渲染前 200 行（`MAX_VISIBLE_TAGS`） | **可搜索下拉框**，整个列表喂进去 |
| 别的 registry | 靠弹窗"粘完整引用" | 改 registry 框 + 在仓库名后写 `:tag` |

关键依据（都是查过/实测过的，不是推的）：

- **下拉框的列表是虚拟化的**：`gpui-kit` 的 `crates/component/src/list/list.rs`
  用 `v_virtual_list`（`select.rs` 的弹层就是 `List::new(...)`）——
  所以 9170 个 tag 不会变成"每帧建 9170 个元素"，这正是能删掉 200 行上限的原因。
  代价：`registry::MAX_VISIBLE_TAGS` / `filter_tags` / `RegistryState::visible_tags`
  一整套"本地过滤 + 截断"没了（留着就是死代码 + 会误导人的注释）。
- **registry 变成输入框 ⇒ 多了一条安全边界**：`host` 会拼进 URL 再作为**裸 argv**
  交给 `curl.exe`，所以新增 `registry::is_valid_host` + `normalize_host`
  （实测形状：`localhost` / `192.168.1.10:5000` 要认，`host:port` / `a..b` /
  `docker.1panel.live/v2` 要拒），并在 `fetch_tags` / `fetch_size` 里各判一次。
- **`input_tag` 是这次合并的关键补丁**：别的 registry 的 `tags/list` 可能要认证
  （实测 daocloud 401），那时下拉框里一个 tag 都没有 —— 而用户已经在仓库名里
  写了 `:1.27`。所以那个 tag 要存进状态（`RegistryState::input_tag`），
  作为 `effective_tag()` 的兜底，否则"按完整引用拉取"这个能力还是丢了。

### 3.8 v0.6.3 修订：表单回到弹窗 + 把"用起来"那几步接上（用户拍板）

用户看完 v0.6.2 的实机截图后提了两件事：**拉取应该是一个按钮 + 弹窗**，
以及**把"拉取 → 建容器 → 进去干活"这条链捋顺**。落地三处：

| 项 | v0.6.2 | v0.6.3 |
|---|---|---|
| 「镜像」页 | 拉取表单铺满整页 + 本地镜像表 | **一个「拉取镜像…」按钮 + 本地镜像表**，表单回到弹窗 |
| 「打开终端」 | 藏在容器详情里（点名字 → 找按钮） | **容器列表行内**（运行中才给），详情不再重复它 |
| 创建容器 | 端口全靠手打 | **按镜像声明的 `ExposedPorts` 预填**，并显示声明的端口与启动命令 |

**为什么表单回弹窗**：v0.6.2 合并时把表单铺在页面上，结果这一页被表单占满，
而"本地有什么镜像"反倒成了次要内容 —— 后者才是经常要看的。
入口仍然只有一个（`AGENTS.md` §5）：点按钮才建表单、才可能拉取。

**新增的那次 `wslc image inspect`**（实测 WSL 3.0.3.0，两份真机 fixture
在 `crates/wslc-core/tests/fixtures/image_inspect_*.json`）：

- 输出形状有**两处不一致**，都得认：带 `--format json` 是**单行数组**、
  不带是**缩进对象**（两种都收，取第一个）；`Cmd` / `Entrypoint` 可能是
  字符串数组，也可能是单个字符串。解析在
  `model::parse_image_inspect`（纯函数 + fixture 单测）。
- 我们只取两项：`Config.ExposedPorts`（实测 nginx 是 `{"80/tcp":{}}`，
  只收 tcp）与 `Entrypoint` + `Cmd`（nginx 是
  `/docker-entrypoint.sh nginx -g daemon off;`）。
- 端口建议是纯函数（`model::suggest_port_mappings`）：`< 1024` 的映射到
  `8000 + 端口`（80 → 8080、443 → 8443），其余保持原样，撞了就往后挪一格。
- **镜像不在本地时** `wslc` 打「未找到映像 'x'。」、stdout 是 `[]`、退出码 1 ——
  所以这条路拿到的是一句能看的原因，界面只说一句"端口请自己填"，不打断创建。
- 预填**只在用户还没填那格时**做（和「添加实例」的 `last_derived_dir` 同一条
  原则：绝不覆盖用户手输的东西）。

> ⚠️ 待实机确认：**模态弹窗里放可搜索下拉框**在本仓库是第一次
> （之前的 `Select` 都在页面上）。看实现是安全的 —— 弹层走
> `deferred` + `POPUP_PRIORITY`，遮罩只有 `BlockMouse`、没有点击处理器，
> 不会和"点外面关菜单"打架 —— 但**这是推的，没有实机验证过**。

---

## 4. 分阶段

| 阶段 | 内容 | 验收 |
|---|---|---|
| **P1** | `registry.rs` 纯函数 + `cmd/registry.rs` + fixture 单测 | ✅ **已完成**：`core` job 绿 |
| **P2** | 新页 `Page::Registry`：仓库 → tag 列表 → 拉取 | ✅ **已完成**：`ui` job 报过 1 个 E0382（已修） |
| **P3** | 拉完「用这个镜像创建容器」预填；两个入口的意图分开（§3.6） | ✅ **已完成** |
| **P4**（可选） | 私有仓库：`wslc registry login` 凭据 + registry 可编辑 | **一半已完成**：registry 可编辑在 **v0.6.2** 做了（§3.7）；凭据 / 认证链路仍未做，另外立项 |

### 4.1 实施记录（2026-10-13）

**一条 CI 反馈就拿到了全部信息。** P2（约 900 行 UI 代码，全部手写、从未编译过）
推上去之后，`ui` job 只报了**一个**错误：

```
error[E0382]: borrow of moved value: `tag`
    --> crates\wslc-panel\src\app.rs:1090
```

`select_registry_tag` 里 `tag` 被移进后台协程（`fetch_size` 要它），
而外层"结果回来时选中的还是不是它"那句核对又要用它。
`query_registry` 里当初就为同一件事克隆了一份（`fetch_repository`），这里漏了。

**这条值得记下来的是它的反面**：borrowck 在类型检查**之后**跑，
所以"只报一个错"意味着 `views.rs` 那 300 行页面代码、
`cx.subscribe` 那套本项目**第一次用**的 API、以及全部样式方法名，
类型检查**一次全过**。其中 `cx.subscribe` + `InputEvent::Change` 不是猜的：
本机 `%TEMP%\gpui-src\` 里有 gpui-component 0.7.1 的源码副本，
`src/input/search.rs`（组件库自己的 `SearchBox`）与
`tests/input/constraints.rs` 都给了同样的用法 —— **先读依赖源码再写**，
比"猜一个 API 烧一轮 CI"便宜得多。

P1 要存的 fixture（**都是 2026-10-13 从 `docker.1panel.live` 真抓的原文**，
不手写 —— 编的夹具会"正好符合我的解析器"，那就测不出东西）：

```
crates/wslc-core/tests/fixtures/
├── registry_tags_alpine.json              2067 B / 224 个 tag（latest 在最后一个）
├── registry_tags_nginx.json              23757 B / 1339 个 tag（压"上千 tag"这条路径）
├── registry_index_alpine.json             9218 B / 16 条，其中 8 条是 attestation
│                                          （amd64/linux 只有一条：d56c381f…）
├── registry_manifest_alpine_amd64.json    1022 B / 单架构 manifest（611 + 3849738）
├── registry_hello_world.json             11062 B / ⚠️ **amd64 有两条**：一条 linux、
│                                          两条 windows —— 钉住"只看架构会挑错"
├── registry_error_name_unknown.json        133 B / 404 的 NAME_UNKNOWN 信封
└── registry_error_denied.json               86 B / 403 的 DENIED 信封
```

---

## 5. 被否掉的方案

| 方案 | 为什么否 |
|---|---|
| 打 `hub.docker.com` 的官方 API（搜索 / 标签 / 星数 / 官方标识） | **实测三个域名全 000**（同一分钟，只有加速站通）。要的东西它一个都给不了 |
| 照「在线发行版」那样，再拉一份别人维护的镜像清单 | 那是 WSL **发行版**的清单（wslui 接口，下的是 tar / `.wsl` 包），跟容器镜像是两回事；而且它给不了**可浏览的 tag 列表**，"在线查看镜像仓库"这个需求落不了地 |
| 引入 HTTP 客户端 crate（`reqwest` / `ureq`） | 不能新增依赖：CI 全部 `--locked`，而本机没有 cargo，锁文件在本地没法重新生成（`AGENTS.md` §1、§3）→ 继续用系统自带的 `curl.exe`，与 `mirrors.rs` / `picker.rs` 同一个思路 |
| 拿 `wslc image inspect` 或本地 `wslc images` 的 `Size` 当"下载量" | 口径不同（解压后 vs 压缩后，实测 2.7 倍，§2.5）；而且**本地没有的镜像根本查不到** —— 而这一版恰恰是在拉之前就要显示 |
| 同时内置三家 registry（含要 token 的两家） | 用户选定只用 `docker.1panel.live`；而它是唯一免认证的那家，于是整条认证链路都不用写。**留了明确的待补清单**（§3.4），不是"反正用不上" |
| 用 `/v2/_catalog` 列"全部仓库" | 实测 **401**，加速站不开放 |
| 按 `Link` 头做真正的分页 | 全量一次才 23.7 KB / 1.7 s，而分页要引入"翻页游标 + 增量状态"，还得处理"过滤词跨页"的问题。为了省 20 KB 换来一类新状态，不划算 |
| 让用户直接输 `host/repo:tag` 当"仓库" | 把 URL 拼装交给用户 = 多一个写错的自由度，而"看到的 tag 就是能拉的"这个保证也就不成立了 |
| 为列表里每个 tag 都请求一次 manifest（显示全部体积） | nginx 有 1339 个 tag → 1339 次请求，`1ms.run` 的限流是 1000/小时（§2.2） |

---

## 6. 验证边界

**CI（`core` job）能验的**：引用规范化（`alpine` → `library/alpine`；粘贴的
URL / `host/repo` / `repo:tag` 都要认；`..` / 空段 / 非法字符 / 以 `-` 开头 / 超长全部拒收）、
tags/list 解析（含 `tags: null`、空数组、非 JSON）、V2 错误信封翻译、
**manifest 挑本机架构**（alpine 的 16 条里挑出 amd64/linux 那一条、且不能挑到
attestation；hello-world 里**不能挑到 windows 那份**）、体积累加、
tag 排序（`latest` 优先、版本倒序、**日期 tag 不算版本**、非版本 tag 不 panic）、
本地过滤与截断。

**CI 查不到、必须本机点一遍的**（`AGENTS.md` §8）：

1. tag 列表滚动 + 过滤随手打字；**换仓库时旧结果要清掉**（别串台）；
2. 拉取过程中界面不卡（`AGENTS.md` §7.1 那类 bug），取消按钮有效；
3. 拉完打开「创建容器」，镜像名**确实预填了**、`pull` 策略是 `Never`；
4. 「镜像」页那个「按引用拉取…」**仍然拉得动别的 registry 的引用**
   （拿 `docker.m.daocloud.io/library/nginx:latest` 试一次）；
5. tag 过滤框**边打字边筛**（这是这一版唯一用 `cx.subscribe` 的地方，
   CI 只验了它能编译，验不了它真的会重绘）。

**还没验、要在本机现场看的两条**：

- `1panel.live` 的 `x-uri-ratelimit-limit: 100` 是分钟级还是小时级**没有确认**。
  频繁切仓库会不会撞 429，得实测一次，并把"被限流"做成一句人话
  （而不是让用户看见一个 429）；
- 那 10 个常用镜像的**快捷按钮**在真机上排出来是什么样（10 个 `flex_wrap`
  的小按钮：会不会换行换得很难看、会不会把「查询」挤下去）——
  这是纯布局问题，CI 一个字都验不到（`AGENTS.md` §8）。

---

## 7. 顺带发现（与这一版无关，但值得记一笔）

**本机的 WSL 已经是 `wslc 3.0.3.0` 了**，而 `docs/wslc-schema.md` 与
`PLAN.md` 里那些"实测于 WSL 3.0.1.0"的结论全都标的是 3.0.1.0。

其中几条**已知会随版本变**的：`--set-sparse` 需要 `--allow-unsafe`
（v0.5.2 补记里记的就是"3.0.1.0 起默认拒绝"）、`--manage --move` 的被占用窗口
（v0.5.13 实测约 60 秒）。这些窗口长度和默认行为在 3.0.3.0 上是否还一样，
**没人验过**，而界面上那些重试预算（94 秒 / 135 秒）是按 3.0.1.0 的实测定的。

不是这一版要修的东西，但下一轮动安装流程时应当先复核一遍。
