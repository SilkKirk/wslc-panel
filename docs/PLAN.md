# wslc-panel 实施方案

**WSL 容器（wslc）管理面板** · Rust + GPUI

| | |
|---|---|
| 项目路径 | `E:\cnb\wslc-panel` |
| 目标平台 | Windows（本机 WSL 3.0.1.0） |
| UI 框架 | GPUI —— Zed 的 GPU 加速 UI 框架 |
| 具体选型 | `gpui-kit` 0.7.1（伞形 crate：`gpui-pre` 0.3.8 + `gpui-base` + `gpui-component` 60+ 组件 + Lucide 图标） |
| 数据来源 | `wslc.exe` 子进程 + `--format json` |
| 许可证 | Apache-2.0（GPUI 与 GPUI Kit 均为 Apache-2.0） |

---

## 1. 环境现状（勘察实测）

| 项目 | 状态 |
|---|---|
| `wslc.exe` | ✅ `C:\Program Files\WSL\wslc.exe`，WSL **3.0.1.0** |
| 容器运行时 | ✅ 内核 6.18.40.1-1，Direct3D 1.611.1，会话管理器 3.0.1 |
| Rust 工具链 | ❌ **未安装**（无 cargo / rustc / rustup） |
| MSVC 链接器 | ❌ **未安装**（无 VS Build Tools / Windows SDK） |
| git / Node | ✅ 2.56.0 / v24.20.0 |

> 工具链由用户自行安装，见 [§7 编译验证步骤](#7-编译验证步骤)。

---

## 2. 为什么选 `gpui-kit`

crates.io 上目前有三条 GPUI 线：

| 包 | 最新版 | 发布时间 | 判断 |
|---|---|---|---|
| `gpui` | 0.2.2 | 2025-10-22 | zed-industries 官方，**已停更约一年** |
| `gpui-pre` | 0.3.8 | 2026-10-05 | Zed 主干快照 `zed@279fe07`，**每周跟随更新** |
| `gpui-ce` | 0.2.2 | 2026-08-28 | 0.3.x 已被 yank、由新所有者重发，**供应链可疑，不采用** |
| **`gpui-kit`** | **0.7.1** | **2026-10-05** | **本项目采用**：`gpui-pre` + `gpui-base` + `gpui-component` + 图标资源的统一入口 |

选 `gpui-kit` 的理由：

1. **它就是 Zed 的 GPUI** —— `gpui-kit` 依赖 `gpui-pre`，而 `gpui-pre` 的自我描述是
   *"Zed's GPU-accelerated UI framework (gpui-pre snapshot of zed@279fe07)"*，
   渲染路径与 Zed 完全一致，满足"用 zed 那个渲染"的要求；
   `gpui-kit` 的 README 明确写了它会把匹配版本的 GPUI、base、component、assets 一起再导出，
   所以应用只列**一个**依赖。
2. **配套组件库**（`gpui-component`，75+ 组件：Table / Input / Tabs / Sidebar / Dialog /
   Chart / Tree / Notification）直接对应本项目的表格、表单、弹窗需求，
   省掉约 2–3 人日的手写 UI 工作量。
3. 单独依赖 `gpui-pre` + `gpui-component` + `gpui-base` 三个包容易版本错配
   （`gpui-component` 对 `gpui-pre` 用的是 `=0.3.8` 精确版本约束），
   伞形 crate 从根上避免这个问题。

> **不采用 `gpui-ce`**：它的 0.3.2 / 0.3.3 已被 yank，随后 0.2.2 由新的所有者重新发布，
> 版本号与发布时间倒挂，供应链风险不可接受。

---

## 3. 架构

```
┌───────────────────────── wslc-panel（GPUI 应用） ─────────────────────────┐
│  main.rs          窗口与生命周期                                          │
│  app.rs           Shell：导航 · 异步刷新调度 · 危险操作确认弹窗            │
│                   （整个项目唯一接触 GPUI 异步 API 的文件）                │
│  views.rs         7 个页面渲染（纯函数，无可变状态）                       │
│  state.rs         AppState · Snapshot · PendingAction（不依赖 GPUI）      │
│  theme.rs         配色（只依赖 GPUI 的 rgb()）                            │
└───────────────────────────────────┬──────────────────────────────────────┘
                                    │ 单向依赖：core 从不依赖 UI
┌───────────────────────────────────▼──────────────────────────────────────┐
│  wslc-core（纯逻辑，可单测）                                               │
│   cli.rs        Command 构造/执行 · 注入 WSL_UTF8 · 超时 · 取消            │
│   decode.rs     UTF-8 / UTF-16LE 双解码                                   │
│   jsonl.rs      JSON Lines 逐行解析 + 容错                                 │
│   model/        Container · Image · Network · Volume · Session · SystemInfo│
│   cmd/          各子命令封装                                              │
│   settings.rs   settings.yaml 读写（保留注释）                             │
└───────────────────────────────────┬──────────────────────────────────────┘
                                    │ std::process::Command
                              wslc.exe（WSL 3.0.1.0）
```

**分层原则**：`wslc-core` 不依赖 GPUI，可以用 `cargo test -p wslc-core` 在没有 GPU / 没有窗口的
环境下跑完整测试。UI 层只做渲染与交互编排。

---

## 4. 页面清单

### 4.1 总览 / 基本信息
- **客户端**：WSL 版本、内核版本、Direct3D、DXCore、Windows 版本
- **服务器**：会话管理器版本、活动会话表（ID / 名称 / 创建者 PID）、当前会话选择器
- **存储**：`settings.yaml` 路径、`storagePath`、`sessions\<id>\storage.vhdx` 实际占用
- **统计**：运行中容器 / 全部容器 / 镜像 / 网络 / 卷 计数
- **快捷入口**：`wslc settings` 打开配置文件

### 4.2 当前运行 container
- 由 `wslc list` + `wslc stats --format json` 合并而成
- 列：ID、名称、镜像、状态、运行时长、端口、CPU%、内存（已用/上限）、网络 I/O、块 I/O、PID
- 顶部实时资源条，刷新间隔 1 / 3 / 5 秒可调，窗口失焦自动暂停
- 行内操作：停止 / 重启 / 强制终止 / 日志 / exec / attach
- 合并规则：`stats.ID`（64 位）与 `list.ID`（12 位）按**前缀**匹配

### 4.3 全部 container
- `wslc list -a`，含 exited / created
- 搜索（名称 / 镜像 / ID）、状态筛选、按列排序
- 批量：start / stop / restart / remove / `prune`
- **创建容器**：`wslc run` 表单化（镜像、名称、命令、端口、卷、环境变量、网络、
  重启策略、资源限制），带"等效命令预览"面板

### 4.4 wlsc 配置
- 表单字段：`session.cpuCount` / `memorySize` / `maxStorageSize` / `storagePath` /
  `defaultBindingAddress` / `hostLoopback` / `idleTimeout` / `credentialStore`
- 双模式：表单模式 ↔ 原始 YAML 模式
- 保存前自动备份为 `settings.yaml.bak-<时间戳>`
- 改 `storagePath` 时显式警告："不会迁移已有容器/镜像，将创建新空会话"

### 4.5 附加页
镜像、网络、卷、会话、实时事件流（`wslc events`）。

---

## 5. 关键技术风险与对策

| 风险 | 对策 | 状态 |
|---|---|---|
| `wslc` 默认输出 **UTF-16LE** | 子进程注入 `WSL_UTF8=1` + 兜底解码 | ✅ 已实测确认 |
| `--format json` 是 **JSON Lines** 不是数组 | 逐行 `serde_json::from_str` | ✅ 已实测确认 |
| 空结果输出 **0 字节**而非 `[]` | 空串直接返回空 `Vec` | ✅ 已实测确认 |
| `stats.ID` 是 64 位、`list.ID` 是 12 位 | 前缀匹配 | ✅ 已实测确认 |
| `inspect.Name` 带前导 `/`，`list.Names` 不带 | 统一规范化 | ✅ 已实测确认 |
| `inspect` 未启动容器的 `FinishedAt` 是零值时间 | 判零后显示 `-` | ✅ 已实测确认 |
| `system session list` 不支持 `--format` | 解析中文表头表格 | ✅ 已实测确认 |
| Docker Hub 直连超时 | `run` 表单默认 `--pull missing`，并提供"使用本地镜像"下拉 | ✅ 已实测确认 |
| `exec` / `attach` 交互式命令挂住 UI | 强制超时 + 独立线程 + 可取消；交互类开独立终端窗口，不内嵌 | 设计约束 |
| GPUI 中文字体渲染未验证 | **M0 首要验证项**（本机无工具链，尚未验证） | ⚠️ 待验证 |
| 危险操作（remove / prune / kill） | 二次确认弹窗 + 显示影响对象数 + 操作日志 | 设计约束 |

---

## 6. 里程碑

| 阶段 | 内容 | 预估 | 状态 |
|---|---|---|---|
| **M0** | 环境 + 选型验证：gpui 空窗口 + 中文渲染 | 0.5–1 天 | ⏳ 待用户装工具链 |
| **M1** | `wslc-core` 数据层 + fixtures 单测 | 1.5–2 天 | ✅ 代码已写 |
| **M2** | 应用骨架（窗口 / 导航 / 路由 / 主题 / 状态） | 1 天 | ✅ 代码已写 |
| **M3** | 总览页 | 1 天 | ✅ 代码已写 |
| **M4** | 当前运行容器（实时 stats） | 1.5 天 | ✅ 代码已写 |
| **M5** | 全部容器（筛选 / 批量 / 创建表单） | 2 天 | ✅ 代码已写 |
| **M6** | wlsc 配置页 | 1.5 天 | ✅ 代码已写 |
| **M7** | 镜像 / 网络 / 卷 / 会话 / 事件流 | 2 天 | 🟡 部分 |
| **M8** | 打磨（深浅色 / 快捷键 / 打包） | 1 天 | ⏳ 未开始 |

---

## 7. 编译验证步骤

工具链当前**未安装**。装好后按顺序执行：

```powershell
# 1) 安装 Rust（MSVC 工具链）
winget install --id Rustlang.Rustup
#    或 https://win.rustup.rs/x86_64

# 2) 安装 MSVC 链接器 + Windows SDK
winget install --id Microsoft.VisualStudio.2022.BuildTools `
  --override "--quiet --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"

# 3) 重新开一个终端，确认
rustc --version ; cargo --version

# 4) 只验证数据层（不需要 GPU / 窗口，最快）
cd E:\cnb\wslc-panel
cargo test -p wslc-core

# 5) 编译 UI（首次会拉取并编译 GPUI，耗时较长）
cargo build -p wslc-panel

# 6) 运行
cargo run -p wslc-panel
```

**M0 必须最先确认的一件事**：GPUI 能否在本机正常创建窗口并渲染中文。
若失败，回退顺序为：`gpui-pre` → 官方 `gpui` 0.2.2 → `egui`。
因为 UI 层只依赖 `wslc-core` 的数据模型，**回退不会影响数据层代码**。

---

## 8. 验收标准

- `cargo test -p wslc-core` 全绿，测试基于 `crates/wslc-core/tests/fixtures/` 里的**真实 wslc 输出**
  （空列表、字段缺失、UTF-16LE、超大输出、零值时间）
- `cargo clippy --all-targets -- -D warnings` 零告警
- `cargo fmt --check` 通过
- 冒烟清单：真实启停 / 删除容器，界面数值与命令行 `wslc list/stats/info` 逐项对齐
- 边界：`wslc.exe` 不存在、WSL 未启动、会话未创建、无权限、非 0 退出码 —— 均给出可读错误而非 panic

---

## 9. 参考

- 字段字典：[`wslc-schema.md`](./wslc-schema.md)（全部实测采集）
- 选型验证记录：[`SPIKE.md`](./SPIKE.md)
- GPUI 主页：<https://gpui.rs>
- GPUI Kit 组件库：<https://gpui-kit.com>
- wslc 设置文档：<https://aka.ms/wslc-settings>
