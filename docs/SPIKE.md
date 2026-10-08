# M0 选型验证记录（SPIKE）

> 状态：**未完成** —— 本机没有 Rust 工具链，无法编译验证。
> 本文档列出**已经用实机数据确认的事实**与**仍需在装好工具链后验证的假设**，
> 以及精确的验证命令。请按 §3 的顺序执行并把结果填回 §4。

---

## 1. 已经确认的事实（有实机证据）

| # | 结论 | 证据 |
|---|---|---|
| 1 | `wslc` 默认输出 **UTF-16LE**；设 `WSL_UTF8=1` 后是 UTF-8 | `docs/wslc-schema.md` §0.1，字节级比对 |
| 2 | `--format json` 是 **JSON Lines**，不是数组 | `wslc images --format json` 实际输出 |
| 3 | 结果为空时输出 **0 字节**，不是 `[]` | `wslc list -a --format json`（0 容器时） |
| 4 | `stats.ID` 是 64 位，`list.ID` 默认 12 位 | 探针容器实测 |
| 5 | `inspect.Name` 带前导 `/`，`list.Names` 不带 | 探针容器实测 |
| 6 | 未启动容器的 `FinishedAt` 是零值 `0001-01-01T00:00:00Z` | 运行中容器的 inspect |
| 7 | `system session list` **不支持** `--format`，输出中文表头表格 | 实测报错 + 表格输出 |
| 8 | `container prune` 有 `-f/--force` | `wslc container prune --help` |
| 9 | `Labels` 里嵌着 JSON，含精确端口与 `VmPort` | 探针容器的 `Labels` 字段 |
| 10 | 同一个镜像会有多行（不同 `Repository` 指向同一 `ID`） | `wslc images --format json` |
| 11 | 本机**直连 Docker Hub 会超时**（`registry-1.docker.io` 不可达） | `wslc run alpine` 失败日志 |
| 12 | 本机 wslc = 3.0.1.0，内核 6.18.40.1-1 | `wslc info` |

这些结论已经固化进代码和测试 fixture，不需要再验证。

---

## 2. 仍未验证的假设（M0 的风险点）

| # | 假设 | 影响面 | 验证方式 |
|---|---|---|---|
| A | `gpui-kit` 0.7.1 能在本机编译 | 整个 UI 层 | §3.2 |
| B | **GPUI 能正常渲染中文**（字体会不会缺字/豆腐块） | 所有页面文案 | §3.2 目视 |
| C | 本机 GPU/D3D11 路径可用（`blade-graphics` 走 HLSL/D3D11） | 窗口能否出现 | §3.2 |
| D | `WindowOptions::default()` 能开出一个正常尺寸的窗口 | 首屏体验 | §3.2 |
| E | `cx.spawn(async move \|this, cx\| ...)` 的异步闭包语法与 `WeakEntity::update` 签名 | `app.rs`（唯一异步点） | §3.2 |
| F | `Button::new(SharedString)`、`.small()`、`.disabled()`、`.primary()` | 所有按钮 | §3.2 |
| G | GPUI `Styled` 的 `truncate()` / `overflow_y_scroll()` / `border_r_1()` 等方法名 | 表格与布局 | §3.2 |
| H | `Rgba::opacity(f32)` 用于半透明遮罩与徽标底色 | 徽标、确认弹窗 | §3.2 |
| I | `.id(&'static str)` + `.on_click()` 在 `div` 上可用（导航项） | 左侧导航 | §3.2 |
| J | `gpui_kit::assets::Assets` + `.with_assets()` 图标资源可用 | 图标（当前未强依赖） | §3.2 |
| K | 卷列表的真实 JSON 字段名（**采集时 0 个卷，schema 未采样**） | 卷页面 | 创建一个卷后再看 |

> 如果 A–J 里任何一条不成立，**只需改 UI 层**：`wslc-core` 不依赖 GPUI，
> 它的 60+ 个单元测试与 fixture 测试与渲染框架无关。

---

## 3. 验证步骤

### 3.1 先验证数据层（不需要 GPU / 窗口，最快，先做这个）

```powershell
cd E:\cnb\wslc-panel
cargo test -p wslc-core
```

预期：

- `jsonl` / `decode` / `model` / `settings` 的单元测试全绿；
- `tests/wslc_smoke.rs` 里的 fixture 测试全绿；
- 4 个 `smoke_*` 真机测试会因为本机有 `wslc` 而**真正执行**（不是跳过），
  并打印一行 `真机：运行中 N / 全部 N / 镜像 N / 网络 N / 卷 N`。

这一步能证明：UTF-16LE 解码、JSON Lines 解析、ID 前缀匹配、
中文表头表格解析、settings.yaml 注释保留全部正确。

### 3.2 再验证 UI 层

```powershell
cargo build -p wslc-panel
cargo run -p wslc-panel
```

逐项目视检查：

1. **窗口出现**（假设 A/C/D）——若黑屏或崩溃，看控制台日志。
2. **中文正常渲染**（假设 B）——侧边栏应显示「基本信息 / 当前运行 / 全部容器 /
   镜像 / 网络 / 卷 / wlsc 配置」，不是方块或空白。
3. **左侧导航可点击**（假设 I），点击后右侧标题跟随变化。
4. **「基本信息」页**出现 WSL 3.0.1.0、内核 6.18.40.1-1、
   `C:\Users\...\AppData\Local\wslc\settings.yaml`。
5. **数字统计**与命令行对齐：
   ```powershell
   $env:WSL_UTF8=1; wslc list -a; wslc images; wslc network list
   ```
6. **「wlsc 配置」页**能显示 8 个配置项、原始 YAML，
   点「8」按钮后「状态」变成「有未保存的修改」，
   点「备份并保存」后在 `%LOCALAPPDATA%\wslc\` 下出现
   `settings.yaml.bak-<时间戳>`，且 `settings.yaml` 里出现了
   `  cpuCount: 8` 而**其它注释一条都没丢**。
7. **危险操作**：真起一个容器，点「停止」→ 应弹出确认框而不是直接执行。
   ```powershell
   $env:WSL_UTF8=1
   wslc run -d --pull never --name spike-probe -p 18080:80 `
        docker.1ms.run/library/alpine:latest sleep 300
   ```
   验证完记得删除：`wslc remove -f spike-probe`
8. **超时与卡死保护**：若某条命令卡住，界面不应冻结（`busy` 只会禁用刷新按钮）。

### 3.3 卷 schema 补采样（假设 K）

```powershell
$env:WSL_UTF8=1
wslc volume create spike-vol
wslc volume list --format json     # ← 把这一行输出补进 docs/wslc-schema.md §7
wslc volume remove -f spike-vol
```

拿到真实字段后回来校准 `crates/wslc-core/src/model/volume.rs`
（那里已经把未知字段兜进 `extra`，所以即使字段名不同也不会丢数据）。

---

## 4. 验证结果

> 装好工具链并跑完 §3 后，把结果填在这里。

| 假设 | 结果 | 备注 |
|---|---|---|
| A | ⏳ 待验证 | |
| B | ⏳ 待验证 | |
| C | ⏳ 待验证 | |
| D | ⏳ 待验证 | |
| E | ⏳ 待验证 | |
| F | ⏳ 待验证 | |
| G | ⏳ 待验证 | |
| H | ⏳ 待验证 | |
| I | ⏳ 待验证 | |
| J | ⏳ 待验证 | |
| K | ⏳ 待验证 | |

---

## 5. 回退方案

如果 GPUI 在本机无法正常工作（A/B/C 失败），按以下顺序回退，
**每一次回退都只动 `crates/wslc-panel`，`wslc-core` 原样保留**：

1. `gpui-kit` → 官方 `gpui = "0.2.2"` + `gpui-macros = "0.2.2"`（去掉组件库，
   表格与按钮改用手写 `div`，`Button` 换成自绘的 `div().on_click()`）。
2. 若官方 `gpui` 也不行 → 换 `egui` / `iced`（渲染层整体替换，
   `state.rs` 的 `Snapshot` 与 `PendingAction` 可以直接复用）。
3. 若中文字体有问题 → 在 `theme.rs` 里统一指定 `font_family`，
   或改用 `gpui-component` 的主题字体配置。

**判定依据**：只要 `cargo test -p wslc-core` 全绿，
项目就保住了全部数据能力，UI 换实现不会造成返工。
