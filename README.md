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
│ 概览         │  ┌────────┬────────┬──────┬──────┬─────┐             │
│  基本信息    │  │运行中 3│全部 7  │镜像 12│网络 3│卷 2 │             │
│              │  └────────┴────────┴──────┴──────┴─────┘             │
│ 容器         │  ┌───────────────────┬───────────────────────┐       │
│  当前运行    │  │ 客户端            │ 服务器                │       │
│  全部容器    │  │ WSL      3.0.1.0  │ 会话管理器    3.0.1   │       │
│              │  │ 内核     6.18.40  │ 活动会话表            │       │
│ 资源         │  │ Windows  10.0.26  │                       │       │
│  镜像        │  └───────────────────┴───────────────────────┘       │
│  网络        │                                                      │
│  卷          │                                                      │
│              │                                                      │
│ 设置         │                                                      │
│  wlsc 配置   │                                                      │
└──────────────┴──────────────────────────────────────────────────────┘
```

---

## 功能

| 页面 | 内容 | 数据来源 |
|---|---|---|
| **基本信息** | WSL/内核/Windows/Direct3D 版本、活动会话、`settings.yaml` 路径与 `storagePath`、容器/镜像/网络/卷统计 | `wslc info`、`wslc system session list` |
| **当前运行** | 运行中容器 + **实时资源**（CPU%、内存、网络 I/O、块 I/O、PID），刷新间隔 1s/3s/10s 可调 | `wslc list`、`wslc stats` |
| **全部容器** | 含已退出容器、状态徽标、端口映射、批量停止/删除/`prune` | `wslc list -a`、`wslc stats -a` |
| **镜像 / 网络 / 卷** | 镜像（同一 ID 多仓库引用会标注）、网络（内置网络禁止删除）、卷 | `wslc images` / `network list` / `volume list` |
| **wlsc 配置** | 8 个配置项的当前生效值与默认值、快捷写入、原始 YAML 查看、**带时间戳备份保存**、调用系统编辑器 | 直接读写 `settings.yaml` |

安全设计：**停止 / 强杀 / 删除容器 / 删除镜像 / 清理 / 删除网络 / 删除卷** 全部走二次确认弹窗，
并显示不可撤销的提示。

---

## 快速开始

**前置条件**：Windows 上装有 **WSL 3.0 以上版本**（提供 `wslc.exe`）。
若安装在非默认路径，设置环境变量 `WSLC_PATH` 指向 `wslc.exe`。

### 方式一：直接下载（推荐）

到 [Releases](https://github.com/SilkKirk/wslc-panel/releases) 下载
`wslc-panel-vX.Y.Z-windows-x64.zip`，解压后直接运行 `wslc-panel.exe`。

单文件、无需安装、不依赖 WebView2 或任何运行时。

### 方式二：从源码编译

> ⚠️ 需要 Rust 工具链 + MSVC 链接器（约数 GB）。本机当前**两者都没有**。

```powershell
# 0) 安装工具链（一次性）
winget install --id Rustlang.Rustup
winget install --id Microsoft.VisualStudio.2022.BuildTools `
  --override "--quiet --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
# 重开一个终端后确认：
rustc --version ; cargo --version

# 1) 先验证数据层（不需要 GPU / 窗口，最快）
cargo test -p wslc-core

# 2) 编译并运行界面（首次会拉取并编译 GPUI，耗时较长）
cargo run -p wslc-panel
```

### 方式三：用 CI 验证（本机没有工具链时）

仓库里的 GitHub Actions 就是为此准备的 —— Windows runner 自带 Rust 与 MSVC：

| Workflow | 触发 | 作用 |
|---|---|---|
| [`ci.yml`](.github/workflows/ci.yml) | 任意 push / PR | `wslc-core` 测试 + 整个 workspace 的 `cargo check` |
| [`release.yml`](.github/workflows/release.yml) | 推 `v*` 标签，或手动触发 | 编译 release、打包 zip、建 Release |

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
└─ wslc-panel/       GPUI 应用
   ├─ main.rs        窗口与生命周期
   ├─ app.rs         Shell：导航、异步刷新、确认弹窗（唯一接触 GPUI 异步 API 的文件）
   ├─ views.rs       各页面渲染（纯函数）
   ├─ state.rs       状态与数据采集（不依赖 GPUI）
   └─ theme.rs       配色
```

**分层原则**：`wslc-core` 不知道 GPUI 的存在。这意味着

- 数据层可以在没有 GPU / 没有窗口的环境里跑完整测试；
- 万一 GPUI 在本机有问题，**换渲染层不会造成数据层返工**。

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

- [`docs/PLAN.md`](docs/PLAN.md) —— 实施方案、里程碑、风险对策
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
  会开一个独立的 Windows 控制台窗口。
- **本机直连 Docker Hub 会超时**：`run` 表单默认 `--pull missing`，
  并提供"使用本地已有镜像"的输入方式；离线环境请用 `--pull never`。

---

## 许可证

Apache-2.0。GPUI（Zed Industries）与 GPUI Kit（Longbridge）同为 Apache-2.0。
