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
| `wsl -l -o` | **失败**：`raw.githubusercontent.com` 连接被重置，退出码 `-1`（原始输出存成 `tests/fixtures/wsl_list_online_failed.txt`） |
| 在线清单兜底 | `cdn.jsdelivr.net/gh/microsoft/WSL@master/distributions/DistributionInfo.json` 200 / 18481 B ✅；`ghproxy.net/...` 也 200 ✅ |
| Ubuntu rootfs | `mirrors.{tuna,ustc}.edu.cn/ubuntu-cloud-images/{noble,jammy}/current/{rel}-server-cloudimg-amd64-root.tar.xz` → 200（229 MB / 458 MB）✅ |
| Alpine rootfs | `.../alpine/v3.21/releases/x86_64/alpine-minirootfs-3.21.0-x86_64.tar.gz` → TUNA / USTC / 阿里云均 200（3.5 MB）✅ |
| 不可用（因此**不进表**） | lxc-images 的 `default/rootfs.tar.xz`（TUNA/NJU/Tencent/SJTU 全 404）、Debian cloud rootfs（TUNA/USTC/NJU 404）、Kali（TUNA 的 release 目录里没有 WSL/rootfs 文件）、Huawei（假 200，返回 HTML 页面） |
| 依赖 | 本仓库**没有 HTTP 客户端依赖**，且 CI 全部 `--locked`、本机没有 cargo → **不能新增依赖**（加了锁文件就与清单对不上，而本地没法重新生成） |

## 4. 我们怎么做

### 4.1 数据层：把"安装"从一条命令变成一份**计划**

```
crates/wslc-core/src/
├── model/install.rs   纯逻辑：名称/路径推导、装前检查、**执行计划**、两个在线列表的解析
├── mirrors.rs         内置镜像表 + 探测/下载参数 + 进度换算（curl.exe）
├── cmd/install.rs     把计划执行出来：起进程、流式读输出、轮询进度、取消、重定位
└── cmd/distro.rs      只留"起 wsl.exe"的部分；`list_online` / `online_distros`（含兜底）
```

多步流程（在线安装改名最多 8 步）下，"一条命令"这个抽象撑不住；
硬套下去只能让**预览**和**执行**各写一份拼参数逻辑，而它们一旦走偏，
界面上那句"与实际执行完全一致"就是假话。所以：

- `plan(spec, ctx) -> InstallPlan`（纯函数）产出步骤列表；
- 界面逐条渲染当预览，`run_plan` 逐条执行 —— 两边读**同一份数据**。

### 4.2 五条来源的步骤

| 来源 | 步骤 |
|---|---|
| 从 tar 导入 | 建目录 → `wsl --import <name> <dir> <tar> --version 2` |
| 从 VHDX 导入 | 建目录 → `wsl --import <name> <dir> <file> --vhd --version 2` |
| 从文件安装 | `wsl --install --from-file <file> --name <name> [--location <dir>]` |
| 镜像站下载 | `curl -s -S -L --retry 2 -o <tmp> <url>` → 建目录 → `--import` → 删临时文件 |
| 在线安装 | `--install -d <id> [--web-download] [--location <dir>] --version 2 [--no-launch]` → 等注册 →（必要时、**不可取消**）`--export` → `--unregister` → `--import` → 删中转 tar |

收尾（任何来源）：`.wslconfig` 里开了 `[experimental] sparseVhd` → 补一条
`--manage <name> --set-sparse true`；用户勾了"设为默认" → 最后一条 `--set-default`。

### 4.3 在线安装的"快路径"

`--install -d` 只认清单里的 id。所以：

- **名字 == id**（默认就是它，选清单时把名字填成 id）→ 直接
  `--install -d <id> --location <dir>`，装完用**注册表 `BasePath` 核实**；
- `--location` 那次**失败**（微软文档列了它，但本机没法验证商店那条路是否真的接受）→
  **去掉 `--location` 自动重试一次**（`PlannedStep::location_fallback`），
  随后由重定位把名字与位置补正 —— 比"安装直接失败"好得多；
- 核实不通过（WSL 没听 `--location`）或**名字 != id** → 走重定位补齐。

参考实现**总是**重定位（哪怕名字一样），代价是每次多拷几个 GB。
我们把它拆成"快路径 + 核实兜底 + 去掉 `--location` 重试"：默认快，结果仍然确定。

### 4.4 界面

- 来源五个按钮；来源相关的字段（文件路径 / 在线清单 / 镜像区）；
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
| 3 | 在线清单**有兜底**（自己拉微软那份 JSON） | 本机 `wsl -l -o` 就是坏的，参考实现直接依赖它 |
| 4 | 镜像清单**不依赖第三方服务**，改成内置表 + 自定义 URL | 参考实现的清单来自它自己的 `api1.wslui.com`；别人的服务随时会变、会没。表里只放**本机 curl 验证过 200** 的条目 |
| 5 | 名字默认填**清单 id**，不是友好名 | 否则每次在线安装都命中重定位（多拷几 GB）。用户想改名随时能改，界面会提示代价 |
| 6 | 保留「从文件安装」（`--install --from-file`） | `.wsl` 是新格式，`--import` 吃不了；现成能力不该退 |
| 7 | 导入失败时**保留**中转 tar 并告诉用户路径 | 那一刻源发行版已经 `--unregister` 了，tar 是唯一的数据副本 —— 参考实现无论如何都删掉 |
| 8 | 渲染时不做"目录非空"检查 | 每帧 `read_dir` 碰上网络盘会卡住界面；提交时查一次就够 |

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
