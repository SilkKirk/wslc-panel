# wslc-panel

[![CI](https://github.com/SilkKirk/wslc-panel/actions/workflows/ci.yml/badge.svg)](https://github.com/SilkKirk/wslc-panel/actions/workflows/ci.yml)
[![Release](https://github.com/SilkKirk/wslc-panel/actions/workflows/release.yml/badge.svg)](https://github.com/SilkKirk/wslc-panel/actions/workflows/release.yml)

**WSL 容器（wslc）管理面板** —— Rust + [GPUI](https://gpui.rs)（Zed 的 GPU 加速 UI 框架）。

面向 Windows 上 **WSL 3.x 的容器功能**（`wslc.exe`）：不依赖 Docker Desktop，
直接通过 WSL 自带的容器 CLI 管理容器、镜像、网络与卷。

```
┌──────────────┬──────────────────────────────────────────────────────┐
│ wslc  panel  │  基本信息                                            │
│              │                                                      │
│ 概览         │ ┌──────────────────────────────────────────────────┐ │
│  基本信息    │ │ 运行环境                                         │ │
│ WSL 实例     │ │ WSL 3.0.1.0  内核 6.18.40.1  Windows 10.0.26     │ │
│  实例列表    │ └──────────────────────────────────────────────────┘ │
│ 容器         │ ┌─────────┬──────────┬─────────┬─────────┬─────────┐ │
│  容器        │ │运行中 3 │全部 7    │镜像 12  │网络 3   │卷 2     │ │
│ 资源         │ └─────────┴──────────┴─────────┴─────────┴─────────┘ │
│  镜像        │ ┌──────────────────────────────────────────────────┐ │
│  网络        │ │ 运行中容器（实时）                               │ │
│  卷          │ │ 名称    镜像            CPU      内存            │ │
│ 设置         │ │ nginx   nginx:latest    0.5%     12MiB/15.5GiB   │ │
│  应用设置    │ │ mysql   mysql:8.0       12.4%    210MiB/15.5GiB  │ │
│  关于        │ └──────────────────────────────────────────────────┘ │
└──────────────┴──────────────────────────────────────────────────────┘
```

> 侧边栏就是上面这 8 页（`Page::ALL`）。**「添加实例」不在这里** —— 它从
> 「实例列表」页上的按钮进（同时列「实例列表」和「添加实例」是冗余的：
> 后者本来就是前者的一个动作）。「wlsc 配置」与「WSL 配置」也不是页面，
> 而是「应用设置」里的两个 tab。

---

## 功能

| 页面 | 内容 | 数据来源 |
|---|---|---|
| **基本信息** | **运行环境**（WSL / 内核 / Windows 版本）+ 容器 / 镜像 / 网络 / 卷统计 + **运行中容器的实时占用**（名称 / 镜像 / CPU / 内存 / 网络 I/O / 进程数，**按 CPU 占用从高到低排**，CPU 与内存带用量进度条，≥60% 变黄、≥85% 变红；点容器名进详情 —— 这一页不摆任何按钮）。⚠️ v0.9.16 去掉了「服务器」「活动会话」「存储」三张卡（全是面板自己的实现细节且几乎不变，详见 `docs/PLAN-v0.9.md` §22） | `wslc info`、`wslc list` / `wslc stats` |
| **当前运行** | 运行中容器 + **实时资源**（CPU%、内存、网络 I/O、块 I/O、PID） | `wslc list`、`wslc stats` |
| **全部容器** | 含已退出容器、状态徽标、端口映射。**行内最多 4 个动作**：**数据库**（仅认得出是 MySQL 且运行中）/ **部署管理**（仅认得出是 nginx / Java / Node.js / Go 且运行中）/ **启动**或**停止** / **更多** —— 另外那四个（**终端**（`exec -it`，仅运行中）/ **重启** / **日志** / **删除**）收在「更多」下拉菜单里，删除前有一条分隔线、点击弹二次确认；**工具栏**上是两颗**页面级**的按钮 —— **「停止全部」**（一键停掉所有正在运行的容器，二次确认，确认框里写明会影响几个；一个都没在跑时**不显示**）和 **「清理已停止」**（`prune`）；详情里强杀、**执行命令**（非交互 `exec`）、**`attach` 主进程** | `wslc list -a`、`wslc stats -a`、`wslc logs` / `exec` / `attach` |
| **数据库管理** | MySQL / MariaDB 容器专属（从容器行的「数据库」按钮进）：页头只有容器信息 + **「基础信息」**（点开才是**连接卡**：用户名 / 密码 —— 密码写在容器环境变量里时**自动读出**，只存内存不落盘）+ 「重新读取」；下面两个 tab：**数据库**（字符集 / 排序规则 / 表数 / 占用；**系统库不显示**，数量在右上角注明；+ 新建【默认同时创建用户并授权：**用户名手填**（旁边「用建议值」一键填 `<库名>_user`，不自动生成）、随机密码，账号记在本地、列表直接展示用户名 / 密码】）、**用户**（`mysql.user`：用户 / 主机 / 认证插件 / 超级权限 + 新建【可顺带授权】+ 改密码 + 授权 + 删除）。**库级动作都在数据库列表的每一行上**（四个）：**备份**（点开是浮层，右上**「立即备份」**生成第一份：`mysqldump` → Windows 侧存档 + 容器内留一份用于恢复；列表支持恢复【二次确认，覆盖现有数据】/ 打开位置 / 删除）/ **导入备份**（挑本机一个 `.sql` → 送进容器 → `mysql` 读它；目标库**就是这一行**，不给改）/ **SQL**（开控制台并**用这一行的库和账号**，`mysql -D <库>`，浮层显示"当前库"与报错原文）/ 删除（二次确认，本地账号一起清） | `wslc exec -e MYSQL_PWD=…` + 容器自带的 `mysql` 客户端 / `mysqldump`、`wslc inspect`、`wslc container cp` |
| **部署管理** | nginx / Java / Node.js / Go 容器专属（从容器行的「部署管理」按钮进）：**添加部署**（选本地来源 → 容器内目标目录 → 启动命令 → 端口；「浏览…」按运行时弹文件 / 目录对话框）+ **已部署列表**（应用名 / 形态 / 目标路径 / 状态 / 端口 / 操作）。**两种形态**：nginx 是**内容**形态（把静态文件铺进 Web 根就生效，默认目标**从 `inspect` 的挂载点推出来** —— 只认 `/usr/share/nginx` 底下的站点卷，`/etc/nginx/conf.d` 那种**配置**挂载不算，另给「重载 nginx」）；Java / Node.js / Go 是**进程**形态（面板在容器里起一个受 pidfile 管的 `nohup` 进程，可启动 / 停止 / 看日志 / 重新部署）。「重新部署」把那条记录**预填**回同一个弹窗（想改目标目录 / 启动命令就改，不必先删）。⚠️ 部署是**按名字替换**：删掉面板自己铺过的那几个（这次的 ∪ 上一次的 ∪ `app.log` / `app.pid`）再铺，**镜像自带的文件、你自己放的东西一个都不动**；目录里本来有别人的东西时会**事后提醒一句**（不拦）。目标目录只允许填在 `/opt` `/srv` `/app` `/var/www` `/usr/share/nginx` 这类位置（不允许系统目录）；从 Windows 传过去的文件**没有可执行位**，Go 这类要执行的会自动补 `chmod +x`。「部署过什么」记在本地面板、「现在跑没跑」**问容器**（在这一页上约 9 秒自动问一次） | `wslc container cp`（逐字节搬运，实测 sha256 一致）、`wslc exec`（准备目录 / 起停判活 / `chmod +x`）、`wslc inspect`（判运行时 + 推 Web 根 + 读端口） |
| **创建容器** | 弹窗表单：镜像引用（**右边有「从本地镜像里选」的可搜索下拉框** —— 候选是本地已有镜像，标签里带体积；从「镜像」页点「用这个镜像创建容器」进来时**已预填**）、容器名（**留空就按镜像命名**：`nginx`，同镜像的第二个叫 `nginx-2` —— 不让 `wslc` 起 `shimmering_karakoram` 那种随机名，否则列表里看不出是哪个镜像起的；下面那行提示会写出**具体**会叫什么）、端口 / 环境变量 / 卷挂载 / 网络 / 内存 / CPU、拉取策略（默认 `never`）；**按镜像预填三样**：① 读一次 `wslc image inspect`，按它声明的 `ExposedPorts` 预填端口（nginx → `8080:80`）；② 常见镜像（mysql / mariadb / postgres / mongo / redis / nginx）**首次启动必需的環境变量只预填变量名**（mysql → `MYSQL_ROOT_PASSWORD=`，值留给你填）；③ **建议的数据卷**（`mysqldata:/var/lib/mysql`）。碰到"预填了变量名却没填值"会在**提交时拦住**并把焦点送到那一格（空值等于没填 —— 否则容器起来就退出，原因还藏在日志里）。以上预填**只在那一格是空的时候**做，绝不覆盖手输的内容；并显示它声明的端口与启动命令；底部**实时等效命令预览**（留空时里面的 `--name` 就是上面那个名字） | `wslc run -d`、`wslc image inspect`、`wslc images`（快照） |
| **镜像** | **本地已有 + 拉取**：页面是「**拉取镜像…**」按钮 + 本地镜像表（同一 ID 多仓库引用会标注、可删除）。拉取在**弹窗**里：registry 是**可编辑**字段（默认内置 **`docker.1panel.live`** —— 实测唯一一个 tag 与 manifest 都免认证的加速站）+ 10 个常用官方镜像的快捷入口；仓库名认 `alpine` / `library/nginx` / `grafana/grafana` / `nginx:1.27`，**粘一整条浏览器 URL 也认**；**tag 是可搜索的下拉框**（整体喂进去 —— 组件库的列表是虚拟化的，`node` 那种 9170 个 tag 也不卡），按「**latest 优先 → 版本倒序**」排过（接口给的是字母序，`latest` 本来排在最后一个）；**选中某个 tag 才去量它的下载体积**（只量那一个 —— 一次要两个请求）；别的 registry 列不出 tag 也**照样能拉**（仓库名后面写 `:tag`，或粘完整引用）；拉完可直接「**用这个镜像创建容器**」（镜像名预填） | `curl.exe` → Registry V2（`/v2/<repo>/tags/list`、`/v2/<repo>/manifests/<ref>`）、`wslc images` / `wslc pull` |
| **网络 / 卷** | 网络（内置网络禁止删除）、卷（**创建命名卷** / 删除 / **清理未使用**） | `network list` / `volume list` / `volume create` / `volume prune` |
| **wlsc 配置** | 8 个配置项的当前生效值与默认值、快捷写入、原始 YAML 查看、**带时间戳备份保存**、调用系统编辑器 | 直接读写 `settings.yaml` |
| **实例列表** | WSL 发行版：状态、WSL 版本、是否默认、安装位置与 VHDX 虚拟大小；**行内**打开终端 / 启动（由本面板吊住保持运行）/ 终止 / 设为默认；**详情里**压缩磁盘、移动位置、调整大小、导出、打开安装位置、编辑 `/etc/wsl.conf`、删除 | `wsl --list --verbose` + 注册表 `Lxss` |
| **添加实例** | **三条来源**：**本地文件**（tar / tar.gz / tar.xz / `.vhdx` / `.wsl` 一个入口，**按后缀自动分岔**装法）/ **微软商店 (Microsoft Store)**（下拉框，可搜索）/ **在线发行版（镜像源）**（下拉框，清单照 `wsl-dashboard` 的常用度排序，选中即**自动测速择优**）。装前检查（名字、重名、目录）、实时的**执行步骤预览**、装中**日志与进度**、可取消（重定位阶段除外） | `wsl --import` / `wsl --import --vhd` / `wsl --install --from-file` / `wsl --install -d`（商店）/ `curl.exe` + wslui 清单（镜像源） |

> 「添加实例」只从**实例列表页上的按钮**进（侧边栏不列它）—— 那一页的输入框与
> 两个下拉框必须在点击时创建（`InputState::new` / `SelectState::new` 都要
> `&mut Window`），见 `app.rs` 的 `InstallForm`。
>
> 表格里放不下的长文本（镜像 / 镜像表的引用 / 安装位置 / 卷挂载点）会显示成省略号，
> **鼠标移上去弹出 tooltip 显示全文**（见 `views::cell_muted_full`）。

安全设计：**停止 / 停止全部 / 强杀 / 删除容器 / 删除镜像 / 清理容器 / 删除网络 / 删除卷 /
清理未使用卷 / 终止会话** 全部走二次确认弹窗，并显示不可撤销的提示。
（「终止会话」的确认框会写明：这个会话里**正在运行**的容器会一起停下，
但镜像/容器/卷在会话的 `storage.vhdx` 里、不会被删。）

### 自动刷新

默认 **3 秒**一次，在「应用设置」页 →「常规」tab 的**「刷新」**卡片里改
（预设 1/3/5/10/30 秒），改完立即生效并写入：

```
%LOCALAPPDATA%\wslc-panel\prefs.json
```

这是**应用自己的偏好**，刻意不写进 `wslc` 的 `settings.yaml` ——
那个文件归 `wslc` 所有，塞自定义键既可能被它重置，也会让人误以为改的是 `wslc` 的行为。

刷新间隔和耗时都不在界面上展示：耗时写进日志，间隔只在这一处设置。

同一个 `prefs.json` 里还有三个**界面**偏好（都在「应用设置」页里改）：

| 键 | 有什么用 | 默认 |
|---|---|---|
| `theme` | 跟随系统 / 深色 / 浅色（两套配色一起切） | `dark` |
| `install_dir` | 「新实例默认安装目录」—— 「添加实例」页拿它预填，并按发行版名推出子目录 | 没设过（要自己填） |
| `terminal_dir` | 「新终端从哪儿开始」：`home`（`wsl --cd ~`）/ `panel`（继承面板自己的工作目录）/ `{"custom": "/home/你/项目"}` | `home` |

> `install_dir` 是**旧文件里没有键**的那种：读不到就当作"没设过"，
> 而不是空字符串 —— 升级上来不会有任何行为变化。

> `terminal_dir` 相反：升级上来**会**有变化（老版本不给 `wsl --cd`，于是终端继承
> **面板自己**的工作目录 —— 也就是从哪个文件夹启动的面板，终端就从哪儿开始，
> 比如从「下载」里双击 exe 就落在 `/mnt/c/Users/你/Downloads`）。这是有意的：
> 那个落点本来就不是设计出来的。容器终端走 `wslc exec -w`，只对**绝对路径**生效
> （容器里没有「Linux home」这个概念）。

自动刷新是**后台**行为：顶部右上角的「刷新」按钮不会跟着变成「刷新中…」，
只有你自己点它（或执行启动 / 停止 / 删除等操作后补刷）时才显示进度 ——
否则 3 秒一次的话按钮会一直停在「刷新中…」上。想立刻看到最新数据，随时点它。

---

## 快速开始

**前置条件**：Windows 上装有 **WSL 3.0 以上版本**（提供 `wslc.exe`）。
若安装在非默认路径，设置环境变量 `WSLC_PATH` 指向 `wslc.exe`。

### 方式一：直接下载（推荐）

到 [Releases](https://github.com/SilkKirk/wslc-panel/releases) 下载 **`wslc-panel.exe`**
（单文件，直接运行，**不用解压**）。

`wslc-panel-vX.Y.Z-windows-x64.zip` 是同一个程序的压缩包，里面多带了 README 与 `docs/`，
要归档或离线分发时用它 —— 两者内容相同，不需要都下。

无需安装、不依赖 WebView2 或任何运行时。

### 方式二：从源码编译

> ⚠️ 需要 Rust 工具链 + MSVC 链接器（约数 GB）。本机当前**两者都没有**。

```powershell
# 0) 安装工具链（一次性）
winget install --id Rustlang.Rustup
winget install --id Microsoft.VisualStudio.2022.BuildTools `
  --override "--quiet --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
# 重开一个终端后确认：
rustc --version ; cargo --version

# 1) 先验证纯逻辑（不需要 GPU / 窗口，最快）
#    和 CI 的 core 任务同一条命令 —— 刻意**不写死包名**：写死的话，
#    以后往 workspace 里加第 4 个 crate 时它的测试永远不会跑，而 CI 照样绿。
cargo test --workspace --exclude wslc-panel

# 2) 编译并运行界面（首次会拉取并编译 GPUI，耗时较长）
cargo run -p wslc-panel
```

### 方式三：用 CI 验证（本机没有工具链时）

仓库里的 GitHub Actions 就是为此准备的 —— Windows runner 自带 Rust 与 MSVC：

| Workflow | 触发 | 作用 |
|---|---|---|
| [`ci.yml`](.github/workflows/ci.yml) | 推 `main` / 任意 PR（纯文档 PR 跳过） | 三个 job：**纯逻辑测试**（`--workspace --exclude wslc-panel`，以免把整棵 GPUI 链接回来）、**`wslc-panel` 编译检查**（`cargo check --workspace --all-targets` + `-p wslc-panel --bins`）、**格式与 clippy** |
| [`fmt.yml`](.github/workflows/fmt.yml) | 手动触发 | 跑 `cargo fmt --all` 并把结果提交回仓库 —— 本机没有 rustfmt 时**唯一**能格式化代码的入口 |
| [`release.yml`](.github/workflows/release.yml) | 推 `v*` 标签，或手动触发 | 编译 release、打包 zip、建 Release |
| [`gen-lock.yml`](.github/workflows/gen-lock.yml) | 推 `ci/gen-lock` 分支，或手动触发 | 生成 / 更新 `Cargo.lock` 并提交回分支 —— 本机没有 Rust 工具链时，这是唯一能更新锁文件的办法 |

> ⚠️ 功能分支的 push **不会**触发 `ci.yml`（`on.push.branches` 只写了 `main`）。
> 这是刻意的：否则「推分支 + 开 PR」会把同一套 job 跑两遍。
> **功能分支的结果要去 PR 里看**，别在本地干等。

> ⚠️ 三个 job 里只有前两个是真正的"门"：`lint` 是 `continue-on-error` + `exit 0`，
> **它绿不代表格式和 clippy 干净**（v0.5.10 时实测有 5 条 clippy 告警 +
> 147 处 rustfmt 差异，而那个 job 照样是绿的）。要拿真实结果得去读它的日志。

> ⚠️ 还有一个容易误判的红叉：`fmt.yml` 用 `GITHUB_TOKEN` 推的那条
> `style: cargo fmt` 提交，会留下一条**零 job 的 failure**（`pull_request` 事件，
> check suite 里一个 check run 都没有）—— 那不是代码问题，同一个提交重跑就是绿的。
> 判据见 [`AGENTS.md`](AGENTS.md) §3。

在 Actions 页手动触发 `release.yml`，就能在不打标签的情况下验证"exe 能不能产出"。

---

## 架构

```
crates/
├─ wslc-core/        纯逻辑，不依赖任何 UI 框架 → 可在无 GPU 环境下完整测试
│  ├─ cli.rs         Command 执行：注入 WSL_UTF8=1、CREATE_NO_WINDOW、超时、取消
│  ├─ decode.rs      UTF-8 / UTF-16LE 编码判定（wslc 默认吐 UTF-16LE）
│  ├─ jsonl.rs       JSON Lines 解析（--format json 是每行一个对象，不是数组）
│  ├─ model/         Container / Image / Network / Volume / Session / SystemInfo
│  ├─ cmd/           各子命令的类型化封装
│  ├─ settings.rs    settings.yaml 读写（**保留注释**的定点改写）
│  └─ tests/         fixtures 驱动的集成测试 + 真机冒烟测试
│
├─ wslc-panel-core/  同样不依赖 GPUI：面板里"不是界面"的那一半
│  ├─ state.rs       状态与数据采集
│  ├─ prefs.rs       应用自己的偏好（%LOCALAPPDATA%\wslc-panel\prefs.json）
│  ├─ columns.rs     各页表格的列定义（表头 + 宽度）
│  ├─ presets.rs     配置项的常用预设值
│  └─ util.rs        纯函数助手（切分、刷新按钮文案……）
│
└─ wslc-panel/       GPUI 应用
   ├─ main.rs        窗口与生命周期、日志（发布版写文件）+ 对 core 的 re-export 垫片
   ├─ app.rs         Shell：导航、异步刷新、所有动作与弹窗（唯一接触 GPUI 异步 API 的文件）
   ├─ views.rs       各页面渲染 + 全部弹窗浮层（约 180 KB，**很大**）
   └─ theme.rs       配色
```

**分层原则**：`wslc-core` 与 `wslc-panel-core` 都不知道 GPUI 的存在。这意味着

- 数据层与纯逻辑可以在没有 GPU / 没有窗口的环境里跑完整测试；
- 万一 GPUI 在本机有问题，**换渲染层不会造成数据层返工**；
- CI 也不必为了跑几个断言去编译并链接整棵 GPUI 依赖树 ——
  原先 `cargo test -p wslc-panel --bins` 要 **763 秒**，现在面板的测试
  全在 `wslc-panel-core` 里，十几秒跑完。

---

## 关于 GPUI 的选型

crates.io 上目前有三条线，本项目用的是第三条：

| 包 | 最新版 | 说明 |
|---|---|---|
| `gpui` | 0.2.2（2025-10-22） | zed-industries 官方发布，**已停更约一年** |
| `gpui-pre` | 0.3.8（2026-10-05） | Zed 主干快照 `zed@279fe07`，每周跟随更新 |
| `gpui-ce` | 0.2.2 | 0.3.x 已被 yank、由新所有者重发，**供应链可疑，不采用** |
| **`gpui-kit`** | **0.7.1（2026-10-05）** | **本项目使用**：伞形 crate，一个依赖即含 `gpui-pre` + `gpui-base` + `gpui-component`（60+ 组件）+ Lucide 图标 |

`gpui-kit` 是 `gpui-pre` 之上的官方推荐入口
（见其 README：「`gpui-kit` pins the matching GPUI release and re-exports GPUI,
base, component, and assets, so a Rust application lists a single dependency.」）。

---

## 文档

- [`AGENTS.md`](AGENTS.md) —— **给 AI 代理看的**仓库约定与踩坑记录（本机没有 Rust 工具链、
  CI 怎么读、发布流程、弹窗约定……）。人也能看：里面全是"不知道就会踩坑"的那类东西。
- [`docs/PLAN.md`](docs/PLAN.md) —— **当前**的实施方案、里程碑、风险对策
- [`docs/PLAN-v0.2.md`](docs/PLAN-v0.2.md) / [`PLAN-v0.3.md`](docs/PLAN-v0.3.md) /
  [`PLAN-v0.5.md`](docs/PLAN-v0.5.md) / [`PLAN-v0.6.md`](docs/PLAN-v0.6.md) /
  [`PLAN-v0.7.md`](docs/PLAN-v0.7.md) / [`PLAN-v0.8.md`](docs/PLAN-v0.8.md) /
  [`PLAN-v0.9.md`](docs/PLAN-v0.9.md) ——
  各阶段的**历史**计划。里面的"未做"说的是**当时**的状态，不是现状。
  （`PLAN-v0.9.md` 是「部署管理」那一版，§1 是**实测表**、§2 记了**两次被推翻的
  机制选型**、§10 是审查捞出来的问题 —— 包括一条"单测自己写错、断言恒假"。）
- [`docs/wslc-schema.md`](docs/wslc-schema.md) —— **实测采集**的 `wslc` 输出字段字典（踩坑记录）
- [`docs/SPIKE.md`](docs/SPIKE.md) —— M0 选型验证清单与回退方案

---

## 已知限制

- **wlsc 配置页目前不支持任意文本输入**：提供的是逐项展示 + 预设值快捷写入 +
  恢复默认 + 原始 YAML 查看 + 调用系统编辑器。完整的表单化自由编辑需要
  GPUI 的 `InputState` / `TextInput`，属于下一步。
- **卷的真实 JSON 字段名尚未实机校准**：采集时本机 0 个卷，`wslc` 输出 0 字节。
  未知字段会被兜进 `VolumeListItem::extra`，因此不会丢数据。
- **`exec` / `attach` 不开内嵌终端**：交互式程序不能内嵌进 GPUI 的消息循环，
  会开一个独立的 Windows 控制台窗口。容器详情里的「执行命令」是**非交互**的
  —— `wslc exec` 不带 `-it`，没有 TTY，`vi` / `top` / `less` 用不了；
  要终端就用同一排的「打开终端」。它跑的是 `/bin/sh -c <你写的整行>`，
  所以管道、重定向、`&&` 都能用（用 `/bin/sh` 而不是 bash：alpine 这类镜像里没有 bash）。
- **容器日志是「快照」，不做「跟随」**：`wslc logs --follow` 永不返回，
  内嵌就等于一条永远不结束的后台任务、还得自己实现"停止跟随"。
  所以日志浮层里给了一个「在终端里跟随」，开独立窗口（能滚、能 Ctrl+C）。
- **容器页的批量只有「停止全部」这一档**：`wslc stop` 收多个名字，所以工具栏上
  那一颗能一键停掉所有正在运行的容器；但**行首没有复选框**，不能只挑几个来
  停 / 删（`docs/PLAN.md` §4.3 里那个"批量"形状还没做）。
- **本机直连 Docker Hub 会超时**：`run` 表单默认 `--pull missing`，
  并提供"使用本地已有镜像"的输入方式；离线环境请用 `--pull never`。
- **「镜像」页默认只连内置的 `docker.1panel.live`**：实测它是唯一一个 tag 与
  manifest **都免认证**的加速站（`1ms.run` 的 manifest 要它自己域名下的 token，
  `daocloud` 连 tag 列表都要，而 `registry-1.docker.io` / `hub.docker.com`
  在这台机器上直接不可达）。要拉**别的 registry**：把「registry」那个框改成它的
  地址，并在仓库名后面写上 tag（`nginx:1.27`）—— **列不出 tag 也能拉**，
  只是没有下拉框可选。认证链路（`WWW-Authenticate` → token）**没写**，
  待补清单在 [`docs/PLAN-v0.6.md`](docs/PLAN-v0.6.md) §3.4。
- **镜像的「体积」有两个口径，别混**：「拉取区」显示的是「**下载约 N MB**」
  （registry manifest 里各层压缩后之和），而下面的「本地镜像」表里 `wslc images`
  的 `Size` 是**解压后**的落盘占用。同机 nginx 实测 60.9 MB vs 163 MB，差 2.7 倍 —— 两个数都对。
- **GPUI 文件里写测试模块要小心**：`app.rs` / `views.rs` 里有 `use gpui_kit::*;`，
  而 gpui 会再导出 `gpui_macros` 的 `test` **属性宏** —— 测试模块里写
  `use super::*;` 会把它继承进来、遮蔽 Rust 的 `#[test]`，报
  `recursion limit reached while expanding #[test]`（详见 [`docs/SPIKE.md`](docs/SPIKE.md)）。
  面板的测试现在都在 `wslc-panel-core` 里，那边没有 gpui，不存在这个坑。

---

## 故障排查

程序打不开、或者界面空白时，**先看日志文件**：

```
%LOCALAPPDATA%\wslc-panel\logs\wslc-panel.log
```

发布版是 Windows 子系统程序（**不会**弹出控制台窗口），所以这个文件是唯一能看到日志的地方。
正常启动时它长这样：

```text
INFO wslc_panel: wslc-panel 启动（版本 0.9.16，构建 1493886）
INFO gpui_windows::direct_write: Use Microsoft YaHei UI as UI font.
INFO gpui_windows::directx_devices: Using GPU: Intel(R) UHD Graphics
INFO gpui_windows::directx_devices: Created device with Direct3D 11.1 feature level.
INFO wslc_panel: 窗口尺寸：Some(Windowed(Bounds { ... size: Size { width: 1178px, height: 754px } }))
INFO wslc_panel: 窗口已打开
```

| 日志里看到 | 含义 / 处理 |
|---|---|
| 文件不存在 | 程序根本没启动（被杀软拦了？） |
| 只有「启动」没有「窗口已打开」 | 显卡/驱动问题。GPUI 走 D3D11，需要 D3D11 及以上；虚拟机 / 远程桌面下可能不可用 |
| `打开窗口失败：...` | 同上，看后面的具体错误 |
| 窗口已打开但界面空白 | 把环境变量 `RUST_LOG` 设成 `debug` 再跑一次，日志会详细得多 |

界面本身的问题（数据不显示、某个页面报错）会以红色横幅直接显示在页面顶部，不需要看日志。

**窗口尺寸**按主显示器的可用区域自适应（已排除任务栏）：期望 1280×820，
小屏上会自动缩小，并且任何情况下都不会超出屏幕。

---

## 许可证

Apache-2.0。GPUI（Zed Industries）与 GPUI Kit（Longbridge）同为 Apache-2.0。
