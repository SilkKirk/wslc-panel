# PLAN v0.5 —— 「添加实例」对齐参考实现

> 目标：把「添加实例」从"发一条命令、给个 toast"升级成**可选来源、装前校验、
> 过程可见、能取消**的安装流程。
>
> 参考实现：`E:\cnb\wsl-dashboard-ref`（GitHub/Gitee 上的 `owu/wsl-dashboard`）。
> ⚠️ 它是 **GPL-3.0-only**，本项目是 Apache-2.0：**只读它理解机制**，
> 代码/文案/图标一行都没进来（`AGENTS.md` §6）。本文里引用它的实现细节是为了
> 说明"为什么这么设计"，不是代码来源。

---

## 1. 现状（v0.4 结束时）

v0.3 的 P3（commit `0b97d17`）已经有「添加实例」：三种来源（tar 导入 /
`--install --from-file` / `--install -d`）、等效命令预览、浏览按钮、
启动与设为默认开关。缺的是**过程与安全**：

- 一次后台阻塞调用 + 前后各一个 toast —— 十几分钟里界面没有任何信息；
- 不能取消；
- 校验只有"名字不能为空 / 不能有 `\/` / 目录必须绝对路径"；
- 没有 VHDX 导入、没有镜像站、在线安装只能手输商店 id。

## 2. 参考实现是怎么做的

（`src/ui/views/add_view.slint` + `src/ui/handlers/distro/{install.rs,install_logic.rs}` +
`handlers/distro/mirror_install/*`）

1. **四种来源**：RootFS tar 导入、VHDX（`--import --vhd`）、Store
   （`wsl -l -o` 列表 → `--install -d <id> --no-launch`）、镜像站（下载 rootfs 再 `--import`）。
2. **装前校验与联动**：名字只留 `[A-Za-z0-9.]`、`-/_/空白` 折叠成 `-`、截断 25；
   从文件名猜发行版名（剥后缀、删 `rootfs`、遇平台词就停）；名字推出
   `默认安装目录\名字`；重名给随机后缀建议；安装目录非空报错。
3. **装中反馈**：流式终端面板（长步骤把 WSL 的原始输出藏起来只转点号）、
   状态行、成功/失败标记、右键复制输出。
4. **Store 装完重定位**：`--install -d <id> --no-launch`（先探 GitHub 通不通，
   通就加 `--web-download`）→ 轮询最多 15×2 秒确认注册 → 名字/位置要改时
   `--export` → `--unregister` → `--import` → 删临时文件 → 按 `.wslconfig` 决定 `--set-sparse`。
5. **镜像站**：清单来自**它自己的服务** `https://api1.wslui.com`（先取清单 URL，
   再取每个发行版的镜像列表），本地测速选最快，带 fallback 与下载进度。
6. 网络来源安装前弹一个"预计 3–10 分钟，继续？"的确认框。

## 3. 本机实测（决定方案的事实，2026-10-10）

| 事实 | 数据 |
|---|---|
| `curl.exe` | `C:\WINDOWS\system32\curl.exe`，curl 8.21.0 ✅ |
| `wsl -l -o` | **没代理时失败**（`raw.githubusercontent.com` 连接被重置，退出码 `-1`，原始输出存成 `tests/fixtures/wsl_list_online_failed.txt`）；**开着系统代理（dev-sidecar）时成功**，28 行 → `tests/fixtures/wsl_list_online.txt` |
| 系统代理与 `curl.exe` | 系统代理 `127.0.0.1:31181`（dev-sidecar）。`wsl.exe` 认它（所以 `-l -o` 能通），而 **`curl.exe` 不读 Windows 的代理设置**：直连 `raw.githubusercontent.com` 000、清华 200；手动 `--proxy` 指过去会撞它的 MITM 证书（`000`）。所以面板的网络请求都按"直连能通的目标"设计 |
| 在线清单兜底 | `cdn.jsdelivr.net/gh/microsoft/WSL@master/distributions/DistributionInfo.json` 200 / 18481 B ✅；`ghproxy.net/...` 也 200 ✅ |
| **镜像源接口**（照参考实现） | `GET api1.wslui.com/desktop/v1/helper/install` → 441 B（给出清单地址）；`GET api2.wslui.com/co-creation/api/online-distros` → 52497 B，**amd64 24 项 / arm64 20 项**，每条 2~13 个镜像，`update_time` 2026-10-09 ✅。两份响应都存成了 fixture |
| 清单里的 `format` | `tar.xz` / `tar.gz` / **`wsl`** —— Ubuntu 24.04 的 13 个来源里 **10 个是 `.wsl`**（清华/阿里/华为/网易/搜狐/火山/南大/华中科大/哈工大/北外），只有 3 个是 lxc 的 `rootfs.tar.xz` |
| **在线安装的 `--location` 陷阱** | 带 `--location` 必失败（`Wsl/InstallDistro/WININET_E_CANNOT_CONNECT`，商店与 `--web-download` 都一样）；**不带**则能连上开始下载。详见 §4.3 |
| Ubuntu rootfs | `mirrors.{tuna,ustc}.edu.cn/ubuntu-cloud-images/{noble,jammy}/current/{rel}-server-cloudimg-amd64-root.tar.xz` → 200（229 MB / 458 MB）✅ |
| 依赖 | 本仓库**没有 HTTP 客户端依赖**，且 CI 全部 `--locked`、本机没有 cargo → **不能新增依赖**（加了锁文件就与清单对不上，而本地没法重新生成） |

## 4. 我们怎么做

### 4.1 数据层：把"安装"从一条命令变成一份**计划**

```
crates/wslc-core/src/
├── model/install.rs   纯逻辑：名称/路径推导、装前检查、**执行计划**、两个在线列表的解析
├── mirrors.rs         镜像源的**清单解析**（wslui 接口的 JSON）+ 探测/下载参数 + 进度换算
├── cmd/catalog.rs     拉镜像源清单（两个 HTTP 请求走 curl.exe，阻塞）
├── cmd/install.rs     把计划执行出来：起进程、流式读输出、轮询进度、取消、重定位
└── cmd/distro.rs      只留"起 wsl.exe"的部分；`list_online` / `online_distros`（含兜底）
```

多步流程（在线安装改名最多 8 步）下，"一条命令"这个抽象撑不住；
硬套下去只能让**预览**和**执行**各写一份拼参数逻辑，而它们一旦走偏，
界面上那句"与实际执行完全一致"就是假话。所以：

- `plan(spec, ctx) -> InstallPlan`（纯函数）产出步骤列表；
- 界面逐条渲染当预览，`run_plan` 逐条执行 —— 两边读**同一份数据**。

### 4.2 五条来源的步骤

| 来源（界面上的名字） | 步骤 |
|---|---|
| 本地 rootfs 文件（tar / tar.gz / tar.xz） | 建目录 → `wsl --import <name> <dir> <tar> --version 2` |
| 导入 VHDX 虚拟磁盘 | 建目录 → `wsl --import <name> <dir> <file> --vhd --version 2` |
| 从 `.wsl` / 文件安装 | `wsl --install --from-file <file> --name <name> [--location <dir>]` |
| 微软商店 (Microsoft Store) | `--install -d <id> [--web-download] --version 2 [--no-launch]`（**不带 `--location`**，见 §4.3）→ 等注册 →（必要时、**不可取消**）`--manage --move <dir>`，要改名时 `--export` → `--unregister` → `--import` → 删中转 tar |
| 在线发行版（国内镜像源） | `curl -s -S -L --retry 2 -o <tmp> <url>` → 建目录 → `tar.*` 走 `--import`、`.wsl` 包走 `--install --from-file` → 删临时文件 |

收尾（任何来源）：`.wslconfig` 里开了 `[experimental] sparseVhd` → 补一条
`--manage <name> --set-sparse true`；用户勾了"设为默认" → 最后一条 `--set-default`。

> **v0.5.1 补记（来源名字 + 动态清单）**：这两条联网来源原来叫「在线安装」和
> 「镜像站下载」，名字上**看不出跟微软商店有关系** —— 用户要的就是"商店那条路"，
> 所以按参考实现的**分类**改名为「微软商店 (Microsoft Store)」与
> 「在线发行版（国内镜像源）」，并把商店排在镜像源前面（官方那条路优先）。
> 同时镜像源那份清单改成**照参考实现那样动态拉**（见 §3 的接口实测），
> 不再用写死的三行内置表。措辞与代码都是我们自己写的：参考实现是 GPL-3.0-only、
> 本仓库 Apache-2.0（`AGENTS.md` §6），能共用的只有"接口地址与 JSON 字段"这类事实。

### 4.3 在线安装：**绝不传 `--location`**

这是 v0.5.1 最重要的一条实测结论（同一台机器、同一天、只差一个参数）：

| 命令 | 结果 |
|---|---|
| `wsl --install -d Ubuntu --location D:\wsl --version 2 --no-launch` | **秒失败**：`Wsl/InstallDistro/WININET_E_CANNOT_CONNECT` |
| `wsl --install -d Ubuntu --web-download --location D:\wsl …` | 同样秒失败 |
| `wsl --install -d Ubuntu --no-launch` | **能连上**（25 秒还在下载，被我们掐掉） |

带上 `--location` 时 WSL 走的是"自己把整包下到指定目录"那条通道，用的是**不认系统代理**
的 WinINET（这台机器上系统代理是 dev-sidecar，`wsl -l -o` 能通说明清单那条通道认代理）；
不带时走商店/清单那条，能通。参考实现也从不传 `--location` —— 它装完再搬。

所以：

- 在线安装那条命令**永远不带 `--location`**，`<install_dir>` 由后面的
  `EnsureRelocated` 补正：同名 → `wsl --manage <name> --move <dir>`；
  要改名 → `--export` → `--unregister` → `--import`（那一步**不可取消**）；
- 代价是多一次搬运（同卷是改名，跨卷是真的拷一遍）；
- 收益是**装得上** —— 这条比"少拷一次"重要得多。

### 4.3 在线安装：**绝不传 `--location`**

这是 v0.5.1 最重要的一条实测结论（同一台机器、同一天、只差一个参数）：

| 命令 | 结果 |
|---|---|
| `wsl --install -d Ubuntu --location D:\wsl --version 2 --no-launch` | **秒失败**：`Wsl/InstallDistro/WININET_E_CANNOT_CONNECT` |
| `wsl --install -d Ubuntu --web-download --location D:\wsl …` | 同样秒失败 |
| `wsl --install -d Ubuntu --no-launch` | **能连上**（25 秒还在下载，被我们掐掉） |

带上 `--location` 时 WSL 走的是"自己把整包下到指定目录"那条通道，用的是**不认系统代理**
的 WinINET（这台机器上系统代理是 dev-sidecar，`wsl -l -o` 能通说明清单那条通道认代理）；
不带时走商店/清单那条，能通。参考实现也从不传 `--location` —— 它装完再搬。

所以：

- 在线安装那条命令**永远不带 `--location`**，`<install_dir>` 由后面的
  `EnsureRelocated` 补正：同名 → `wsl --manage <name> --move <dir>`；
  要改名 → `--export` → `--unregister` → `--import`（那一步**不可取消**）；
- 代价是多一次搬运（同卷是改名，跨卷是真的拷一遍）；
- 收益是**装得上** —— 这条比"少拷一次"重要得多。

> **v0.5.1 补记（两条通道互备）**：商店与 `--web-download` 走的是**不同的下载实现**，
> 一条挂了另一条可能通，所以在线安装那一步挂 `PlannedStep::retry_other_source`：
> 失败时自动换一边再试一次，并在日志里说明。
>
> 另外**删掉了**原来那个"探 GitHub 通不通来决定默认下载源"的后台探测：
> 它探的是**我们自己的 curl**（不读 Windows 系统代理），而真正决定成败的是
> **WSL 自己的网络通道** —— 两者在这台机器上结论正好相反（curl 探到 000，
> 而 WSL 能装上）。用错的信号去改默认值，只会把用户带偏；现在默认值固定是
> 微软商店，交给"失败自动换源"去兜。

### 4.4 界面

- 来源五个按钮（不下拉：本机没有 Rust 工具链，而 `Select` 组件的 API
  只有 CI 能验，为一个纯观感的改动不值得；按钮的措辞与参考实现的分类对齐）；
  来源相关的字段（文件路径 / 商店清单 / 镜像区）；
- **红字**：`preflight()` 在渲染时现算（纯函数），提交时用同一个函数；
  ⚠️ "安装目录非空"要碰文件系统 → 只在**提交那一刻**查（渲染每帧查会在网络盘上卡死）；
- **步骤预览**：`plan()` 的步骤列表 + notes（代价说明）；
- **装中**：第 N/M 步、已用时、下载百分比与速度、日志（等宽、可滚动、上限 2000 行）、
  取消按钮；重定位阶段换成"此阶段不可取消"的说明；
- **装完**：成功 → 提示 + 跳回实例列表 + 刷新；失败 → **留在页面上**（那里有日志）；
  取消 → 说明"已经装到一半的东西不会自动回收，去列表里看一眼"。

## 5. 与参考实现刻意不同的地方

| # | 不同 | 为什么 |
|---|---|---|
| 1 | **不删同名发行版**。参考实现在 Store 安装前会 `delete_distro(id)` 清理 | 那等于**悄悄 unregister 掉用户已有的数据**。我们改成重名直接报错并建议改名 |
| 2 | **不弹二次确认框**，改成按钮文案 + 常驻一行代价说明 | 用户已经在**专用安装页**填过表单了；本仓库既有的取舍是"信息充分的按钮优于连续弹窗"（见 `PromptKind` 的说明） |
| 3 | 在线清单**有兜底**（自己拉微软那份 JSON） | 本机在没代理时 `wsl -l -o` 就是坏的（有代理时能通），参考实现直接依赖它 |
| 4 | 镜像清单**和它用同一套接口**（wslui），但**解析与下载自己写** | 用户在用它、要的就是这份活的清单（24 个发行版、每个 2~13 个镜像）；接口地址与字段是事实，代码不能抄（GPL ↔ Apache，`AGENTS.md` §6）。同时保留「自定义下载地址」这条出口 |
| 5 | 名字默认填**清单 id**，不是友好名 | 否则每次在线安装都命中重定位（多拷几 GB）。用户想改名随时能改，界面会提示代价 |
| 6 | 保留「从 `.wsl` / 文件安装」（`--install --from-file`） | `.wsl` 是新格式，`--import` 吃不了；而且镜像源里**一半以上的来源就是 `.wsl`**（Ubuntu 24.04 的 13 个来源里 10 个），所以这条能力是必须的，不是锦上添花 |
| 7 | 导入失败时**保留**中转 tar 并告诉用户路径 | 那一刻源发行版已经 `--unregister` 了，tar 是唯一的数据副本 —— 参考实现无论如何都删掉 |
| 8 | 渲染时不做"目录非空"检查 | 每帧 `read_dir` 碰上网络盘会卡住界面；提交时查一次就够 |
| 9 | 镜像源按 `format` **分岔**（`tar.*` → `--import`；`wsl` → `--install --from-file`） | 参考实现一律 `--import`，挑到 `.wsl` 来源必然失败 —— 而它清单里 Ubuntu 的多数来源就是 `.wsl` |

## 6. 边界与失败模式

- **取消**：下载/导入阶段可取消 → 杀进程 + 删半成品（提示"可能留下不完整的发行版"）；
  重定位三连**不可取消**（位置在 `PlannedStep::cancellable == false`）——
  那一步取消等于把刚装好的删了。
- **中文路径**：参数是 Rust 字符串（`CreateProcessW`），编码安全；但 `curl.exe`
  的输出经 `decode::decode` 的启发式判定，**本机要实测一次中文安装目录**。
- **镜像文件改名**（镜像站会更新版本目录）：探测 404 时明确说"可能改名了，
  换一个版本或用自定义 URL"，不留一个转圈的空列表。
- **`curl.exe` 不可用**（被 EDR 拦 / 裁剪过的系统）：错误提示指向"从 tar 导入"
  这条不需要网络的来源。
- **在线安装可能改变 WSL 本身**（未装 WSL 时 `wsl --install` 会先装 WSL）：
  页面上说明这一点。

> **v0.5.2 补记（三个真机上撞出来的坑）**
>
> 1. **`--manage --move` 装完立刻跑会失败**：实测紧接着 `--install` 之后跑，
>    WSL 返回 `-1` 且只给一句套话；隔一会儿在同一个发行版上手工再跑 →
>    `操作成功完成`。所以现在**重试 5 次、每次隔 2 秒**，还不行就退到
>    「导出 → 注销 → 导入」这条不依赖 WSL 临时状态的路（参考实现一直在用它）。
> 2. **失败原因被那句套话盖住了**：`finish_stream` 原来只取**最后一行**输出，
>    而 `wsl.exe` 永远把"如果此错误是意外错误…"放在最后 —— 真正的原因
>    （`Wsl/InstallDistro/WININET_E_CANNOT_CONNECT`）在它前面。现在留最后 6 行、
>    滤掉那句套话再拼给用户看；这个坑不修，下次还是查不动。
> 3. **`--set-sparse true` 现在必须带 `--allow-unsafe`**：WSL 3.0.1.0 起默认
>    拒绝开稀疏盘（理由是"潜在的数据损坏"），报错里自己给出的命令就带这个开关
>    （实测输出）。同时把这一步标成 `best_effort`：那一刻发行版已经装好了，
>    它失败**不该**把整体判成"安装失败"（界面报失败、发行版却好好地在那儿，
>    是最让人摸不着头脑的一种）。

## 7. 测试与验证

- **纯逻辑单测**（CI 的 `core` 任务，不链接 GPUI）：
  名称/路径推导与边界（多字节文件名、截断后露出的 `-`）、`preflight` 的重名与
  目录非空、五条来源的**逐字节参数断言**、`wsl -l -o` 的真实失败输出（fixture）、
  微软清单的真实 18 KB JSON（fixture）、`curl` 探测输出的三种真实形态
  （200 / 404 / 连不上，fixture）、`pick_fastest`、进度百分比与限速、
  `.wslconfig` 的 `sparseVhd`、`InstallProgress::apply` 的事件翻译与日志截断、
  `MirrorState` 换发行版要清掉旧探测结果。
- **执行器**：`run_plan` 的主循环用**纯文件系统步骤**（`CreateDir` / `RemoveFile`）
  真跑一遍，断言事件顺序、失败即停、取消不假装成功；"起 wsl.exe"那几条只能真机验。
- **本机手测清单**（CI 查不出来，改这块必须点一遍）：
  1. 五条来源各跑一次（在线安装用自定义名 `MyUbuntu` 走**重定位**那条路，
     再对照注册表 `BasePath`；镜像站要等几百 MB 下载）；
  2. 名字非法 / 重名 / 目录非空 → 只出红字、**不发起任何进程**（任务管理器里看不到 `wsl.exe`）；
  3. 下载阶段与导入阶段各取消一次 → 临时文件是否清掉、提示是否正确；
  4. 日志滚动与行数上限；**中文安装目录**端到端一次；
  5. 整个安装过程中界面不卡（`AGENTS.md` §7.1 那类 bug）。

## 8. 被否掉的方案

| 方案 | 为什么否 |
|---|---|
| 引入 `reqwest`/`ureq` 做下载与在线清单 | 本仓库不能新增依赖：CI 全部 `--locked`，而本机没有 cargo 重新生成锁文件（`AGENTS.md` §1、§3）→ 用系统自带的 `curl.exe`，与 `picker.rs` 借 `powershell.exe` 同一思路 |
| 解析 `curl` 自己的进度条 | 它是 `\r` 刷新的，而流式读取按 `\n` 切行 → 会攒成一整行、到结束才吐出来。改成轮询**产物文件大小**（与导出的理由相同） |
| 逐行照搬参考实现 | GPL-3.0 → Apache-2.0 的许可冲突（`AGENTS.md` §6） |
| 用 `--name` 给 Store 安装改名 | `wsl.exe --help` 里 `--name` 只和 `--from-file` 一起出现；Store 那条路没有文档保证。重定位是能确定结果的做法 |
| 镜像清单也做"跟随上游"（拉参考项目那个 API） | 依赖别人的服务，且那一份清单的字段结构没有公开约定 |
| `--import-in-place`（就地注册已有 ext4 VHDX） | 它会直接吃用户磁盘上的原文件，风险等级和 `--import --vhd`（拷贝一份）不同，另立项 |

## 9. 本次不做

- i18n（全中文硬编码，`PLAN-v0.3.md` §5.3 已列出成本很高）；
- `--vhd-size` / `--fixed-vhd` 等在线安装的次要选项（本机没实测过，不写没验证的开关）；
- 安装队列 / 并发安装（同时只允许一个，界面明确拒绝第二个）。
