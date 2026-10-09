# wslc-panel v0.3 方案：接入 WSL 实例（发行版）管理

> **来源**：参考项目 [owu/wsl-dashboard](https://github.com/owu/wsl-dashboard)
> （Rust + Slint，Windows 上的 WSL 发行版管理面板）。
> 用户希望把它的**首页实例管理 / 添加实例 / 设置**三块搬到本项目。
>
> **本方案是"功能对齐 + 在本项目技术栈上重新实现"，不是代码搬运。**
> 理由见 §2（许可证）。

---

## 1. 结论先行：这不是"改页面"，是"新增一个平行领域"

| | 现在管的东西 | 要新增的东西 |
|---|---|---|
| 命令 | `wslc.exe` | `wsl.exe` |
| 对象 | WSL **容器**（container / image / network / volume） | WSL **发行版**（distro / 实例） |
| 本机版本 | WSL 3.0.1.0 | 同一个包，同一目录 |

`wsl.exe` 和 `wslc.exe` 就并排在同一个目录里（实测 `C:\Program Files\WSL\`），
**不是两个产品，是同一套 WSL 的两个 CLI 前端**。所以这不是引入新技术栈，
而是在现有骨架上接第二条命令行通道。

**已有的骨架可以直接复用**（这是本方案最大的红利，见 §6.2）：

- `cli.rs` 的子进程封装（`CREATE_NO_WINDOW` / 超时 / 取消 / 并发读管道）
- `decode.rs` 的编码判定 —— **实测 `wsl.exe` 与 `wslc.exe` 行为一致**（§3.1）
- `spawn_streaming` + `CancelToken` —— 导出/导入/安装都是长任务，正好用得上
- 确认弹窗 / Toast / 自动刷新 / `views.rs` 的表格与卡片助手函数

---

## 2. ⚠️ 许可证：只能"照着重写"，不能"拷贝代码"

| 项目 | 许可证 | 证据 |
|---|---|---|
| `owu/wsl-dashboard` | **GPL-3.0-only** | `LICENSE` 是 GPL-3.0 全文；每个源文件头都是 `SPDX-License-Identifier: GPL-3.0-only` |
| `wslc-panel` | **Apache-2.0** | `Cargo.toml` → `[workspace.package] license = "Apache-2.0"` |

**GPL-3.0 的代码不能以 Apache-2.0 分发。** 把 wsl-dashboard 的 `.rs` / `.slint`
文件（哪怕是片段）复制进来，本项目就必须整体改成 GPL-3.0 并附上完整源码与许可声明 ——
这会改变项目的分发条件，不是一个小决定。

**所以本方案的做法是：**

- ✅ 借鉴：**功能清单、交互流程、命令用法、边界情况的处理思路**（这些是想法，不受版权保护）
- ✅ 借鉴：它踩过的坑（例如哪些 `wsl.exe` 选项要提权）—— 我们自己实测复现
- ❌ 不复制：任何一行源代码、注释、i18n 文案、图标、截图
- ❌ 不复制：Slint 布局代码（我们本来也用不上，渲染层完全不同）

> 本方案文档里出现的 `src/ui/...` 路径，只是**用来定位"它做了什么"**，
> 不表示要去读它的实现细节。所有命令行为都以**本机实测**为准（§3）。

---

## 3. 实机采集到的事实（v0.3 的地基）

以下全部是本机 `WSL 3.0.1.0` 实测，不是查文档得来的。

### 3.1 输出编码：和 `wslc` 完全一样，`decode.rs` 不用改

```powershell
wsl.exe --version            # → UTF-16LE，中文显示为乱码
$env:WSL_UTF8=1; wsl.exe --version
# → WSL 版本: 3.0.1.0 / 内核版本: 6.18.40.1-1 / ...
```

结论：**`WSL_UTF8=1` 同样对 `wsl.exe` 生效**，且 `decode.rs` 的
「BOM → 严格 UTF-8 且无 NUL → 偶数长度当 UTF-16LE → lossy」判定
对 `wsl.exe` 同样安全。**这一层零改动。**

### 3.2 `wsl -l -v` 的格式（默认发行版用 `*` 标）

```
  NAME            STATE           VERSION
* Ubuntu-26.04    Stopped         2
```

**逐字节实测**（`WSL_UTF8=1`，stdout 恰好 80 字节，stderr 为空，退出码 0）：

```
20 20 4E 41 4D 45 20*12 53 54 41 54 45 20*11 56 45 52 53 49 4F 4E 0D 0A
2A 20 55 62 75 6E 74 75 2D 32 36 2E 30 34 20*4 53 74 6F 70 70 65 64 20*9 32 0D 0A
```

于是列偏移是**固定宽度**的：

| 列 | 表头行起点 | 数据行起点 |
|---|---|---|
| （默认标记 `* `） | —— | 0 |
| NAME | 2 | 2 |
| STATE | 18 | 18 |
| VERSION | 34 | 34 |

⚠️ 但**不能写死偏移**：NAME 列宽是 WSL 按最长名字算出来的，
发行版名字变长时列会变宽。**健壮的解析策略**：

1. 先 trim + 去掉空行；
2. 认 `*` 前缀（注意 `*` 后面跟一个空格）→ 默认发行版；
3. **在 token 里找已知的 STATE 枚举**（`Running`/`Stopped`/`Installing`/
   `Uninstalling`/`Converting`…），它左边整体是名字、右边是 VERSION；
4. 找不到已知 STATE 时**不要瞎猜** —— 跳过该行并记一条警告，
   总比把状态显示错强。

其他已确认的形态：

- `-l -q` 只给名字（纯列表，无表头）
- `-l --running` 只给运行中的
- `STATE` 是**英文枚举**，但 `--status` 是**中文**（§3.3）—— 别统一假设语言
- 行尾是 `\r\n`（不是 `\n`）

### 3.3 `wsl --status` 是**中文**的，和 `-l -v` 不一致

```
默认分发: Ubuntu-26.04
默认版本: 2
```

⚠️ **同一份输出里，列表页是英文枚举、status 页是中文**。
解析器不能统一假设语言 → **默认发行版以 `-l -v` 的 `*` 为准**，
`--status` 只作兜底（且解析要按 `:` 切、忽略键名文字）。

### 3.4 注册表是"发行版元数据"的权威来源

```
HKCU\Software\Microsoft\Windows\CurrentVersion\Lxss
  DefaultVersion      = 2
  DefaultDistribution = {856a6800-7118-4a80-b013-a88c95dc788f}
HKCU\...\Lxss\{856a6800-...}
  DistributionName = Ubuntu-26.04
  Version          = 2
  Flags            = 0xF
  BasePath         = D:\linux\Ubuntu-26.04
  DefaultUid       = 0
```

**`BasePath` 是唯一能拿到"装在哪个盘"的地方** —— `wsl` 命令不报这个。
磁盘占用 = `<BasePath>\ext4.vhdx` 的 `Length`（实测 18.32 GB）。

### 3.5 磁盘"实际占用"要用 `compact` 拿

`ext4.vhdx` 的 `Length` 是**虚拟大小**。真实可回收空间只能这样问：

```
compact /q D:\linux\Ubuntu-26.04\ext4.vhdx
→ 19,666,042,880 total bytes of data are stored in 19,666,042,880 bytes.
    ↑当前                              ↑压缩后
```

本机两者相等（不可回收）。UI 应显示 `虚拟 18.32 GB`，
并在「压缩」动作前预告可回收多少。

### 3.6 🔴 在线安装列表**在本机不可用**

```
wsl.exe --list --online
→ 未能从 "https://raw.githubusercontent.com/microsoft/WSL/master/
   distributions/DistributionInfo.json" 提取分发列表。无法解析服务器的名称或地址
   错误代码： Wsl/WININET_E_NAME_NOT_RESOLVED
```

这与项目已知的情况一致（PLAN-v0.2 §4.2：`registry-1.docker.io` 也不可达）。
**「添加实例」必须把"本地 tar 导入"作为一等公民**，在线安装只能是可选项，
且必须有超时 + 明确报错 + 允许手输发行版名，不能只有一个转圈的下拉框。

### 3.7 错误输出带**机器可读的错误码**，不要匹配中文

```
不存在具有所提供名称的分发。
错误代码： Wsl/Service/WSL_E_DISTRO_NOT_FOUND
```

- 提取 `Wsl/...` 作为错误分类依据（**免疫系统语言**）
- ⚠️ 退出码是 **`-1`，不是 1** —— 现有 `Error::NonZeroExit` 只看非 0，没问题，
  但任何"退出码 == 1"的判断都会写错

### 3.8 读 `/etc/wsl.conf` 会**启动**发行版

```powershell
wsl.exe -d Ubuntu-26.04 -u root -e cat /etc/wsl.conf   # 实测把 Stopped 的发行版拉起来了
```

有副作用、慢（秒级）。**绝不能放进 3 秒的自动刷新循环**，只能按需触发。

### 3.9 `.wslconfig` 的键**放错文件会被警告**

本机 `%USERPROFILE%\.wslconfig` 里有 `[interop] appendWindowsPath` 和 `[user] default`，
WSL 每次都警告：

```
wsl: interop.appendWindowsPath:C:\Users\76434\.wslconfig 中的键"12"未知
wsl: user.default:C:\Users\76434\.wslconfig 中的键"15"未知
```

这两个键属于**发行版内的** `/etc/wsl.conf`，不属于全局 `.wslconfig`。

👉 **机会点**：如果我们做 `.wslconfig` 编辑，就**做键合法性校验**
（区分"全局键"和"发行版键"）—— 这一点能做得比参考项目好。

---

## 4. 现状与差距

### 4.1 现在的侧边栏（`state.rs` → `Page`）

```
概览   基本信息
容器   容器
资源   镜像 / 网络 / 卷
设置   wlsc 配置
```

`Page::ALL` 长度 6，且有**两个测试**盯着它（`state.rs`）：

- `every_page_has_a_label_and_group` → `assert_eq!(Page::ALL.len(), 6)`
- `pages_are_grouped_in_navigation_order` → `assert_eq!(deduped, vec!["概览","容器","资源","设置"])`

⚠️ 加页面**必须同步改这两个断言**。历史上 commit `552a91c`
（"页面数量断言 7 → 6"）就是被这个测试抓出来的 —— 它是有效的，别绕过它。

### 4.2 新侧边栏

```
概览      基本信息
WSL 实例  实例列表        ← v0.3 新增（对应参考项目首页）
容器      容器
资源      镜像 / 网络 / 卷
设置      应用设置        ← v0.3 新增（把刷新间隔从「wlsc 配置」挪过来）
          wlsc 配置
```

`Page::ALL` → **8 项**（v0.3 只加这 2 个页面），
分组去重后 → `["概览","WSL 实例","容器","资源","设置"]`。

> 「添加实例」是 P3 的事，**v0.3 不加这个页面** —— 加一个空页面没有意义。
> 到 P3 时 `Page::ALL` 会变成 9 项，那时再改一次断言。

> **命名**：用「WSL 实例」而不是裸「实例」。因为 `wslc` 的 **session**
> 在中文语境里也常被叫"实例"，裸「实例」会和现有概念打架。

---

## 5. 目标功能清单（三块）

> 依据：参考项目的模块结构（`src/ui/views/*`、`src/ui/handlers/distro/*`）
> + 本机 `wsl.exe --help` 实测。**标「未确认」的是我还没实测/没读到的**。

### 5.1 实例管理（首页）

**列表**：每个发行版一张卡 / 一行，含

| 字段 | 来源 | 状态 |
|---|---|---|
| 名称 | `wsl -l -v` | ✅ 实测 |
| 状态徽标（运行中/已停止/安装中…） | `wsl -l -v` | ✅ 实测 |
| WSL 版本（1/2） | `wsl -l -v` | ✅ 实测 |
| 是否默认 | `wsl -l -v` 的 `*` | ✅ 实测 |
| 安装位置 | 注册表 `BasePath` | ✅ 实测 |
| 磁盘占用 | `BasePath\ext4.vhdx` | ✅ 实测 |
| 默认用户 | 注册表 `DefaultUid` / `--manage --set-default-user` | ✅ 实测 |
| 是否稀疏 VHD | 注册表 `Flags` 位 / `--manage --set-sparse` | 🟡 位含义未确认 |

**动作**（本机 `wsl.exe --help` 实测存在的）：

| 动作 | 命令 | 破坏性 | 备注 |
|---|---|---|---|
| 启动 | `wsl -d <name> -e true`（或 `--exec`） | 否 | 需确认最小启动方式 |
| 终止 | `wsl --terminate <name>` | 是（丢未保存数据） | 需二次确认 |
| 全部关停 | `wsl --shutdown` | 是 | 影响所有发行版 |
| 设为默认 | `wsl --set-default <name>` | 否 | |
| 改 WSL 版本 | `wsl --set-version <name> <1\|2>` | 慢/有风险 | 需强确认 |
| 删除 | `wsl --unregister <name>` | **不可撤销** | 删除整个 rootfs |
| 导出 | `wsl --export <name> <file> [--format tar\|tar.gz\|tar.xz\|vhd]` | 否 | 长任务 |
| 克隆 | 导出 + 导入 | 否 | 长任务 |
| 移动位置 | `wsl --manage <name> --move <dir>` | 有风险 | |
| 压缩 VHD | `wsl --manage <name> --compact` | 否 | 长任务，回收空间 |
| 稀疏 VHD | `wsl --manage <name> --set-sparse true\|false` | 否 | |
| 调整磁盘大小 | `wsl --manage <name> --resize <size>` | 有风险 | |
| 打开终端 | `wsl -d <name>` | 否 | 需 `CREATE_NEW_CONSOLE`（已有） |
| 在资源管理器打开 | `explorer.exe <BasePath>` | 否 | 已有同类实现（`reveal_storage`） |
| 编辑 `wsl.conf` | `wsl -d <name> -u root -e cat /etc/wsl.conf` | **会启动发行版** | 见 §3.8 |

> 参考项目 README 自述这一块是
> 「One-click **Start, Stop, Terminate, and Unregister**. Real-time status monitoring
> and detailed insights into **disk usage and file locations**」
> + 「Distro Management: **Set as default, migration (Move VHDX to other drives),
> and export/clone to `.tar` or `.tar.gz`**」
> + 「Quick Integration: Instant launch into **Terminal, VS Code, or File Explorer**
> with **customizable working directories and startup script hooks**」。

⚠️ 两点要注意：

1. **"Start" 和 "Stop" 在 `wsl.exe` 里没有对称的命令** ——
   只有 `--terminate`（终止）。参考项目的 "Start" 大概率就是"启动发行版"
   （`wsl -d <name> -e <cmd>` 或直接开终端），"Stop" 可能只是 `--terminate` 的别名。
   **它俩的确切语义标为未确认**，我们按 `wsl.exe` 实际能力实现即可，不必硬凑。
2. **"startup script hooks"（启动脚本钩子）** 是它自己的扩展概念，
   依赖它自己的配置体系。**建议不做** —— 不是 WSL 原生命令，收益低、维护成本高。

> 另有 **磁盘挂载**（`wsl --mount` / `--unmount`，在主机与 WSL 之间传文件）
> 是参考项目的一个独立功能页（`src/app/mount_disk/`）。
> **不在本次三块范围内**，但 `wsl.exe --help` 里确实有这两个命令，将来可单独立项。

### 5.2 添加实例

参考项目 README 自述支持**四种安装来源**：
「Install Linux distributions via **Microsoft Store, GitHub, local files (RootFS/VHDX),
or Online Mirrors**（with auto speed-test to pick the fastest mirror and built-in
RootFS download helper）」。
对应到本机实测存在的 `wsl.exe` 选项：

| 路径 | 我们的命令 | 本机可行性 |
|---|---|---|
| A. 在线安装（Store/CDN） | `wsl --install -d <name> [--no-launch] [--version <v>] [--location <dir>] [--web-download] [--fixed-vhd] [--vhd-size <size>] [--enable-wsl1]` | 🟡 **列表拉不到**（§3.6）。手输名字能否成功**未实测**（见 SPIKE-S8） |
| B. 从本地文件安装 | `wsl --install --from-file <path> [--name <n>] [--location <dir>]` | 🟢 可做（需文件选择器） |
| C. 从 tar 导入 | `wsl --import <name> <installDir> <file.tar> [--version <v>] [--vhd]` | 🟢 可做（需文件 + 目录选择器） |
| D. 就地导入 VHDX | `wsl --import-in-place <name> <file.vhdx>` | 🟢 可做（需文件选择器） |
| E. 镜像站下载 RootFS 再导入 | 自建下载器 + C | 🔴 本机无法验证（DNS 不通），**建议缓做** |

参考项目的 `mirror_install/` 本质是给 C 加一个"下载器 + 测速选最快镜像"，
解决国内拉不到官方 CDN 的问题。本机连 `raw.githubusercontent.com` 都解析不了，
所以 E 这条路径**在本机根本没法验证**（标未确认），不建议进 v0.3。

👉 建议顺序：**C（本地 tar 导入）→ B（从文件安装）→ D → A（手输名字）→ E（缓做）**。

**表单字段**（`--import` 为例）：发行版名、安装目录、tar 文件路径、
WSL 版本（1/2，默认跟随注册表 `DefaultVersion`=2）、是否设为默认。
底部同样给**等效命令预览**（复用 `views.rs::command_preview`）。

### 5.3 设置

参考项目 README 自述的设置项（**这是它的完整清单**）：

> 新实例默认安装目录 · 日志目录与日志级别（Error/Warn/Info/Debug/Trace）·
> UI 语言或跟随系统 · 深色模式开关 · 操作后是否自动关停 WSL ·
> 检查更新频率（每天/每周/每两周/每月）· 开机自启（含自动路径修复）·
> 启动时最小化到托盘 · 关闭按钮改为最小化到托盘 · **侧边栏功能页显隐自定义**

对照本项目的可行性：

| 设置项 | 我们怎么落 | 成本 |
|---|---|---|
| 自动刷新间隔 | 已有（`prefs.rs`），只需**从「wlsc 配置」页挪到「应用设置」页** | 极低 |
| 深/浅色主题 | `gpui_kit::component::Theme::change(ThemeMode::{Dark,Light}, ...)` 已用过 | 低 |
| 开机自启 | 写 `HKCU\...\CurrentVersion\Run` 一个键 | 低 |
| 新实例默认安装目录 | `prefs.rs` 加一个字段，供 §5.2 表单预填 | 低 |
| 日志目录 / 日志级别 | `main.rs` 的 `EnvFilter` + 已知日志路径，展示 + 可打开 | 低 |
| 配置文件位置展示 + 打开所在文件夹 | 已有 `reveal_storage` 同类实现可抄 | 低 |
| 关于 / 版本 / 构建号 | `build_sha()` 已有，界面已在显示 | 极低 |
| **侧边栏功能页显隐** | 改 `Shell::render` 的 nav 生成逻辑 + `prefs.rs` 存一个开关表 | 低 |
| 操作后自动关停 WSL | `wsl --shutdown`，需要定义"操作后"的时机 | 中 |
| 检查更新频率 | 参考项目走 GitHub Releases API（`src/api/client.rs` 用 `ureq`）。**本机无网络** → 价值存疑 | 中 |
| `.wslconfig` 编辑 | 需新写解析 + 键校验（§3.9，可做得比它好） | 中 |
| `/etc/wsl.conf` 编辑 | 需启动发行版（§3.8） | 中 |
| **界面语言（i18n）** | 本项目现在全中文硬编码；它有 50 种语言 + `assets/flags/*` | **很高，建议不做** |
| 系统托盘（启动最小化 / 关闭到托盘） | GPUI 无托盘 API，需引入 `tray-icon` | 高 |
| 网络端口转发 / HTTP 代理 | 参考项目独立模块 `src/network/`，还要建防火墙规则（要提权） | 高 |
| 任务计划 | 参考项目独立模块 `src/app/scheduler*.rs` | 高 |
| USB 设备（`usbipd-win`） | 参考项目独立模块 `src/usb/`，依赖第三方工具 | 高 |
| 磁盘挂载（`wsl --mount`） | 参考项目独立模块 `src/app/mount_disk/` | 中高 |

👉 **建议 v0.3 的设置页只做上表前 8 项**（极低 + 低成本那批），
把 i18n / 托盘 / 网络 / 任务计划 / USB / 磁盘挂载**明确列为"不做"**，避免范围失控。
这几项任何一个都够独立立项。

---

## 6. 架构设计

### 6.1 分层（沿用现有分层，不破坏它）

```
crates/wslc-core/                     ← 数据层，不依赖 UI，可单独 cargo test
  cli.rs        改：抽出通用 Runner，新增 Wsl
  decode.rs     不改（已够用）
  error.rs      改：去掉硬编码的 "wslc" 字样
  jsonl.rs      不改（wsl.exe 没有 JSON 输出，用不上但别删）
  model/
    distro.rs   新：Distro / DistroState / WslVersion / DistroDetail
  cmd/
    distro.rs   新：list / status / terminate / shutdown / set_default /
                     set_version / export / import / install / manage / unregister

crates/wslc-panel/                    ← UI 层
  state.rs      改：Snapshot 加 distros；Page 加 3 项；PendingAction 泛化
  app.rs        改：Shell 加 distro 相关弹窗与动作
  views.rs      改：加 instances / add_instance / app_settings 三个页面
  prefs.rs      改：加 theme / autostart 等字段
  theme.rs      不改（或加浅色配色）
```

> `views.rs` 已经 1951 行。加三个页面后会更长。
> **建议**：本次先加函数（保持"精准修改"），
> 若超过 ~2600 行再单独一个 commit 拆成 `views/` 目录 —— 不要把重构和新功能混在一起。

### 6.2 `wsl.exe` 封装怎么接（关键设计）

现在 `Wslc` 是 `{ program, session, timeout }`，`session` 是靠**前缀参数**
（`--session N`）实现的 —— 这正好说明它本质是个**通用的"带前缀参数的 CLI 调用器"**。

**推荐做法：抽出一个共享内部类型，`Wslc` 的公开 API 一个字都不改。**

```rust
// cli.rs
struct Runner { program: PathBuf, prefix: Vec<String>, timeout: Duration }

impl Runner { /* 把现有的 run / run_checked / spawn_streaming /
                spawn_in_new_console / spawn_detached / execute /
                build_command 全部搬进来 */ }

pub struct Wslc { inner: Runner }   // 行为与签名保持不变
pub struct Wsl  { inner: Runner }   // program = wsl.exe，prefix 恒为空
```

约束：
- `Wslc` 的**所有公开方法签名不能变** —— 现在有 30+ 处调用点 + CI 全量 `cargo check`
- `Wsl` 只加真正需要的：`run` / `run_checked` / `spawn_streaming` / `spawn_in_new_console`
- `resolve_program()` 复用同一份候选路径逻辑：实测两者同目录
  （`C:\Program Files\WSL\wsl.exe`），只是文件名不同。加一个 `WSL_PATH` 环境变量。

### 6.3 `error.rs` 要去掉硬编码的 "wslc"

现在所有错误消息都写死 `wslc`（`"找不到 wslc 可执行文件"`、`"wslc {args} 执行超时"`…）。
新增 `wsl.exe` 后这些文案会误导用户。

**最小改法**：给 `Error` 的变体加一个 `program: String` 字段（或用 `&'static str`），
`Display` 里插值。**不要**新造一个平行的 `DistroError` —— 那会让调用方要处理两套错误。

### 6.4 确认弹窗要泛化（一个小而必须的改动）

`state.rs` 现在是 `confirm: Option<PendingAction>`，
而 `PendingAction::execute(&self, wslc: &Wslc)` 只认 `Wslc`。

发行版动作（`--unregister` / `--shutdown` / `--set-version`）也需要二次确认。
**推荐**：把 `confirm` 的类型换成

```rust
enum ConfirmAction { Container(PendingAction), Distro(DistroAction) }
```

`PendingAction` **保持原样**（容器路径零改动），新增 `DistroAction`
（带 `title()` / `body()` / `confirm_label()` / `execute(&Wsl)`）。
这样两个域的确认文案、危险级别可以各自演进。

### 6.5 采集策略：必须分级，否则自动刷新会变慢

现在 `load_snapshot` 一轮**串行跑 7 条 `wslc` 命令**，间隔 3 秒。
再加 `wsl -l -v` + `wsl --status` 就是 9 条进程。

| 数据 | 频率 | 理由 |
|---|---|---|
| `wslc` 容器数据 | 3 秒（现状） | 用户主要看这个 |
| `wsl -l -v`（列表+状态+默认） | 3 秒 | 一条命令，能看到状态变化 |
| `wsl --status` | 30 秒 或 手动 | 变化极少，且是中文解析，没必要高频 |
| 每个发行版的 `vhdx` 大小 | 30 秒 或 手动 | 文件系统 stat，但发行版可能很多 |
| `/etc/wsl.conf` | **仅手动** | 会启动发行版（§3.8） |

⚠️ 另一个风险：**高频 `wsl.exe -l -v` 可能把 WSL 服务/工具 VM 一直拉活**。
上线前要实测"3 秒一次连续跑 5 分钟"的 CPU/内存表现（见 §9 SPIKE）。

---

## 7. 关键风险与对策

| # | 风险 | 影响 | 对策 |
|---|---|---|---|
| R1 | **GPL-3.0 代码污染** | 必须整体改许可 | §2：只对齐功能，代码全部自写，**不读它的实现细节** |
| R2 | **没有文件/目录选择器** | 导入/导出/安装全做不了 | 已核实 gpui-kit 不提供；见 §7.1 的 A/B/C 三选一 |
| R3 | `--install` / `--update` 可能需要 UAC | 提权失败或静默失败 | 见 §7.2 |
| R4 | `wsl --list --online` 本机不可用 | 「添加实例」在线路径不可用 | §3.6：本地导入为主，在线手输兜底 |
| R5 | `-l -v` 解析（名字含空格 / `*` / 语言混用） | 列表错乱 | 从右往左解析 + 中文/英文分别处理 + fixture 单测 |
| R6 | 长任务无进度反馈 | 用户以为卡死 | 复用 `spawn_streaming` + `CancelToken`（已有） |
| R7 | 高频 `wsl` 调用拖慢自动刷新 | 界面变卡 | §6.5 分级采集 |
| R8 | `Page::ALL` 的测试会失败 | CI 红 | §4.1：同步改两个断言 |
| R9 | 本机**没有 Rust 工具链** | 无法本地编译验证 | 只能靠 CI（见 §8 验证方式） |

### 7.1 🔴 文件/目录选择器（必须先解决）

`wsl --import` 要选 `.tar`，`--install --location` 要选目录，
`--export` 要选保存路径。**现有依赖里没有任何文件对话框能力。**

**已核实**：`gpui-kit` 0.7.1（即 `longbridge/gpui-kit`）**不带文件选择器**。
把它的组件目录列了一遍（`crates/component/src/`），有 `color_picker.rs`、
`dialog/`、`form/`、`setting/`、`table/`、`native_menu/`…… **但没有 file dialog / path picker**。
（所以 `/.refs` 里也不用再找了 —— 结论是没有。）

三个选项：

| 方案 | 成本 | 说明 |
|---|---|---|
| **A. 先不做选择器，手输路径** | 最低 | `InputState` 已有（v0.2 已接入）。功能可用，体验差，但**能立刻打通链路** |
| **B. 用 `windows 0.58` 调 `IFileOpenDialog`** | 中 | **零新依赖**（`wslc-core` 已依赖 `windows 0.58`）。但要写 unsafe COM，且需要 STA 线程 |
| **C. 引入 `rfd` crate** | 中 | 成熟、API 简单。代价：新依赖（会拉 `windows-sys`），且**与 GPUI 的事件循环集成需要验证** |

👉 **建议 A → C**：v0.3 先用 A 打通，v0.4 再上 C。
（B 看着省依赖，但 COM 的线程模型和 GPUI 的 executor 容易打架，性价比不高。）

#### 附带收获：gpui-kit 里**能直接用**的组件

既然列了目录，顺便记下对本次三块**直接有用**的现成组件
（省得又去手搓）：

| 组件 | 用在哪 |
|---|---|
| `crates/component/src/setting/` | **「应用设置」页**的分组与条目 —— 有现成的 setting 组件 |
| `crates/component/src/form/` | 「添加实例」表单 |
| `crates/component/src/table/` | 实例列表（也可继续用 `views.rs` 现有的 `table_header`/`table_row`） |
| `crates/component/src/dialog/` | 危险操作确认（不过现在 `app.rs` 的 `confirm_overlay` 已够用） |
| `crates/component/src/progress/` | 导出/导入/安装的进度条 |
| `crates/component/src/switch.rs` | 设置页的开关项（主题、自启…） |
| `crates/component/src/tag.rs` | 实例状态徽标 |
| `crates/component/src/virtual_list.rs` | 发行版很多时的列表虚拟化 |
| `crates/component/src/searchable_list/` | 在线发行版列表的搜索选择 |

> ⚠️ 这些组件的**实际 API 需要确认**（本项目是手写代码 + CI 编译验证的模式）。
> 用之前先解包到 `/.refs` 查签名，别照名字猜。

### 7.2 UAC 提权

- `cli.rs` 现在是 `std::process::Command`，**无法提权**。
- 需要提权的候选：`wsl --install`（启用可选组件）、`wsl --update`、`wsl --uninstall`。
- **大概率不需要提权的**：`--import` / `--export` / `--terminate` / `--set-default` / `--manage`。
  （WSL 3.x 已是 Store 应用，分发管理都在用户态。）

👉 **先实测再写代码**（§9 SPIKE-3）。如果确实要提权，用
`ShellExecuteW(None, "runas", ...)`，并且**要能处理用户点"否"**（返回码 1223）。

---

## 8. 分期实施顺序

| 期 | 内容 | 依赖 | 风险 |
|---|---|---|---|
| **P0** | 1. `cli.rs` 抽 `Runner` + 加 `Wsl`（`Wslc` API 不变）<br>2. `error.rs` 去掉硬编码 `wslc`<br>3. `model/distro.rs` + `cmd/distro.rs`（`list` / `status`）<br>4. `-l -v` 解析器 + fixture 单测<br>5. 注册表读取（`BasePath` / `Flags` / `DefaultUid`） | 无 | 低 |
| **P1** | 6. 「WSL 实例」页：列表 + 状态徽标 + 默认标记 + 磁盘占用（**只读**）<br>7. `Page` 加 **2** 项（实例列表 / 应用设置）+ 修 2 个断言<br>8. 「应用设置」页：刷新间隔搬过来 + 主题切换<br>9. 采集分级：`--status` + VHDX 大小 30 秒 | P0 | 低 |
| **P2** | 10. 生命周期：终止 / 关停全部 / 设默认 / 改版本（含 `ConfirmAction` 泛化）<br>11. 启动发行版<br>12. 打开终端 / 在资源管理器打开<br>13. 删除（`--unregister`，二次确认 + 显示将删除的 VHDX 大小）<br>14. **压缩 VHD**（`--manage --compact`，流式进度 + 取消 + 预计可回收量） | P1 | 中 |
| **P3** | 15. **添加实例**：本地 tar 导入（手输路径）<br>16. 从文件安装（`--install --from-file`）<br>17. 在线安装（手输名字 + 超时兜底）<br>18. 导出（流式进度 + 取消） | P2 | 中高（R2/R3/R4） |
| **P4** | 19. 文件/目录选择器（`rfd`，或 Win32 `IFileOpenDialog`）<br>20. 克隆 / 移动位置 / 稀疏 / 调整大小<br>21. `.wslconfig` 编辑（带键校验）<br>22. `wsl.conf` 编辑（按需启动） | P3 | 中 |
| **不做** | i18n（50 语言）· 系统托盘 · 网络端口转发 / HTTP 代理 · 任务计划 · USB（`usbipd-win`）· 磁盘挂载（`wsl --mount`） | —— | 建议各自单独立项 |

**建议先做 P0 + P1**（只读、零新依赖、立刻能看到东西），
同时把 §9 的 SPIKE 跑掉，再决定 P2/P3 的细节。

---

## 9. 动手前必须先做的 SPIKE（按项目惯例记进 `docs/SPIKE.md`）

| # | 要验证什么 | 怎么验 | 为什么重要 |
|---|---|---|---|
| S1 | `wsl.exe` 在 `Command` 下 + `WSL_UTF8=1` 的**原始字节** | 复用 `wslc-core` 的采集方式，dump 十六进制 | §3.1 只验证了 PowerShell，没验证 `Command`+`CREATE_NO_WINDOW` |
| S2 | `-l -v` 在**没有发行版** / **正在安装** / **运行中**时的输出 | 造场景或查 issue | 解析器要能扛住空列表 |
| S3 | `wsl --install` / `--import` **是否需要管理员** | 非提权 shell 里直接跑 | 决定要不要写 `runas`（R3） |
| S4 | **高频调用**的代价：3 秒一次 `wsl -l -v` 连跑 5 分钟 | 记 CPU / 内存 / 单次耗时 | 决定采集频率（R7） |
| S5 | `--export` / `--import` 的**进度输出格式**（stdout 还是 stderr？逐行？） | 用一个**小**发行版或小 tar 试 | 决定进度条怎么做（R6） |
| S6 | ~~`gpui-kit` 是否自带文件选择器~~ | **已核实：没有**（列过 `crates/component/src/`） | R2 只剩 A/B/C 三选一（§7.1） |
| S7 | `--export --format vhd` 产出的文件能否 `--import --vhd` 回来 | 小样本试 | 决定"克隆"怎么实现 |
| S8 | **本机无网络时，`wsl --install -d <name>` 手输名字能否成功** | 直接跑（会尝试联网，注意别卡住） | §5.2 路径 A 到底可不可用 |

**S1 / S3 / S4 是 P0 的前置**，其余可以随对应功能一起做。

### 验证方式（本机没有 Rust 工具链）

```powershell
# 本机跑不了 —— cargo / rustc 都未安装（已确认）
# 只能靠 CI：
#   .github/workflows/ci.yml 的 core / ui 两个 job
#     cargo test -p wslc-core
#     cargo check --workspace --all-targets
#     cargo test -p wslc-panel --bins
```

所以**每个阶段都要小步提交**，让 CI 尽早给出类型错误。
（`cargo check --all-targets` **不会执行**测试，`--bins` 那一步不能省 —— 见 `docs/SPIKE.md` §7.8。）

---

## 10. 已确认的决策（2026-10）

| # | 问题 | 决定 |
|---|---|---|
| 1 | 许可证路线 | ✅ **走"只对齐功能、代码全部自写"的干净实现**，不复制 GPL-3.0 代码 |
| 2 | 命名与位置 | ✅ 新区域叫 **「WSL 实例」**，放在「概览」下面 |
| 3 | 范围 | ✅ v0.3 **只做 P0 + P1**（只读实例列表 + 应用设置） |
| 4 | 文件选择器 | ✅ 接受**先手输路径**，选择器后续再加（P4） |
| 5 | 设置页范围 | ✅ **只做简单的几个**；中/高难度的（i18n、托盘、端口转发、任务计划、USB、磁盘挂载）**先不做** |
| 6 | 在线安装 | ✅ **做**（保留在 P3 路线图上） |
| 7 | 磁盘占用 | ✅ 显示**虚拟大小**，**并且做「压缩」动作**（`--manage --compact`） |
| 8 | 自动刷新 | ✅ `--status` / VHDX 大小降到 **30 秒**；**任何操作完成后立即刷新界面** |

### 由这些决定推导出的具体约束

- **§4.2 的侧边栏改成 8 项**（v0.3 只加 2 个页面，「添加实例」等 P3 再加）：
  ```
  概览      基本信息
  WSL 实例  实例列表        ← 新增
  容器      容器
  资源      镜像 / 网络 / 卷
  设置      应用设置        ← 新增
            wlsc 配置
  ```
  → `Page::ALL` 长度 **8**，分组去重后 `["概览","WSL 实例","容器","资源","设置"]`。
  **`state.rs` 里那两个断言必须同步改**（§4.1）。

- **「压缩」进 P2**（和删除、终止这些一起），因为它是长任务、要走流式进度 + 取消。
  列表上先显示虚拟大小；压缩按钮显示"预计可回收 N GB"（算法见 §3.5）。

- **采集分级**（§6.5）落地为：

  | 数据 | 频率 |
  |---|---|
  | `wslc` 容器数据 + `wsl -l -v` | 3 秒（跟随 `prefs.refresh_secs`） |
  | `wsl --status` + 各发行版 VHDX 大小 | 30 秒 |
  | `/etc/wsl.conf` | 仅手动 |

  **操作后立即刷新**这一点现有代码已经处理好了（`Shell::refresh` 的
  `refresh_again` 机制，见 commit `80b102d`）—— 新增的实例动作**必须走同一个入口**，
  不要另起一套刷新逻辑，否则会重演"点了没反应"的老问题。
