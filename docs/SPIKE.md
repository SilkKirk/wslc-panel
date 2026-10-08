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

> 本机没有工具链，因此**改由 GitHub Actions 承担编译验证**（见 §5）。
> 每次 push 都会跑 `ci.yml`。
>
> **当前状态：CI 全绿**（[run 5](https://github.com/SilkKirk/wslc-panel/actions/runs/37761082677)）
> —— `cargo test -p wslc-core` 全部通过，`cargo check --workspace --all-targets`
> 编译通过（含整个 GPUI UI 层）。

| 假设 | 结果 | 备注 |
|---|---|---|
| A | ✅ **已验证** | CI run 1：`gpui-component` / `gpui-base` / `gpui-pre-platform` / `accesskit_windows` 全部 `Checking` 通过 —— GPUI 在 windows-latest 上能编译 |
| B | ✅ **已实机验证** | 实跑 release exe，日志：`gpui_windows::direct_write: Use Microsoft YaHei UI as UI font.` —— 中文走系统雅黑，不缺字 |
| C | ✅ **已实机验证** | 同一次运行：`Using GPU: Intel(R) UHD Graphics` / `Created device with Direct3D 11.1 feature level.` |
| D | ✅ **已实机验证** | 同一次运行：`wslc_panel: 窗口已打开`；随后把 `WindowOptions::default()` 换成显式的 1280×820 居中 |
| E | ✅ **已验证** | `Context::spawn` 签名是 `AsyncFnOnce(WeakEntity<T>, &mut AsyncApp) -> R`，与 `layer_shell.rs` / `testing.rs` / `example_editor.rs` 的写法**逐字一致**；`WeakEntity::update` 返回 `Result`，用 `let _ =` 接住 |
| F | ✅ **已验证** | `ButtonVariants`（`primary()`）需从 `button::*` 导入；`Sizable::small` / `Disableable::disabled` 见 gpui-component 源码 |
| G | ✅ **已验证** | 刻度表见 `gpui-pre-macros/src/styles.rs::box_style_suffixes`（含 `6`/`8`）与 `*_box_style_prefixes`（含 `px`/`py`/`gap`） |
| H | ✅ 已绕开 | 不用 `.opacity()`，改为 `theme.rs` 预置不透明色 + `hsla` 遮罩 |
| I | ✅ **已验证** | `div().id(...).on_click(...)` 在 CI run 2 没有再报错 |
| J | ✅ **已实机验证** | `.with_assets()` 编译通过；程序完整跑起来没有资源相关报错 |
| K | ⏳ 待补采样 | 卷的真实 JSON 字段名 |

### 实机运行的完整日志（第一次跑 release exe）

```text
INFO wslc_panel: wslc-panel 启动
INFO gpui_windows::direct_write: Use Microsoft YaHei UI as UI font.
INFO gpui_windows::directx_devices: Using GPU: Intel(R) UHD Graphics
INFO gpui_windows::directx_devices: Created device with Direct3D 11.1 feature level.
INFO wslc_panel: 窗口已打开
```

这一份日志一次性回答了三个问题：中文字体、D3D11 设备、窗口创建。

**同时暴露的缺陷（已修）**：release 版还带着控制台窗口 ——
默认是 console 子系统。已加
`#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]`，
并把发布版日志改写到
`%LOCALAPPDATA%\wslc-panel\logs\wslc-panel.log`（没有控制台之后，
文件是唯一能看到日志的地方）。debug 构建仍保留控制台，方便 `cargo run`。

### CI 找到的问题（已修）

**第一轮（编译错误，3 处）**

| 位置 | 错误 | 修法 |
|---|---|---|
| `model/image.rs:110` | `E0631` / `E0599`：`hello.iter()` 给出 `&&ImageListItem`，不能把 `ImageListItem::reference` 直接当函数传给 `map` | 改成闭包 `.map(\|i\| i.reference())` |
| `settings.rs:532`、`wslc_smoke.rs:183` | `E0599`：`SettingsValues::iter()` 这个**固有方法遮蔽了 `slice::iter`**，返回的是 `Vec` 而不是迭代器，于是 `.all()` 不存在 | 重命名为 `entries()`，并在文档注释里写明为什么不再叫 `iter` |

**第二轮（逻辑 bug，3 处 + UI 层 12 处编译错误）**

| 位置 | 问题 | 修法 |
|---|---|---|
| `jsonl.rs` | `parse_lines::<Row>("[]")` **返回 Ok** —— serde 允许结构体从序列反序列化，而模型字段全带 `#[serde(default)]`，于是 `[]` 会变成一条"全默认值"的垃圾记录而不是报错 | 显式拒绝 `[` 开头的行，记成 `LineError` |
| `model/container.rs` | `PortMapping::parse("8080->80/tcp")` 把 `8080` 当成了**主机 IP**（应为主机端口） | 宿主段无冒号时按端口解析；补了两条测试 |
| `settings.rs` | `set()` 把注释行的 `#` 误认成行尾注释，生成 `cpuCount: 4 # cpuCount: default` | 只在原本是生效行时才保留行尾注释 |
| `wslc-panel` × 9 | `no method named font_bold / font_semibold` —— 字重方法来自 `StyledExt` trait，未导入 | `use gpui_kit::component::StyledExt` |
| `wslc-panel` × 2 | `no method named overflow_y_scroll` —— 它属于 `StatefulInteractiveElement`，**只对带 `.id()` 的元素可用**；GPUI 的 overflow 宏只提供 `overflow_hidden` / `overflow_x_hidden` / `overflow_y_hidden` | 滚动容器加 `.id(...)` |
| `main.rs` | `no method named new found for &mut App` —— `cx.new` 来自 `AppContext` trait | `use gpui_kit::AppContext;` |
| `views.rs` | `error: recursion limit reached while expanding #[test]` | 见下面单独一节 —— 调大上限是**错的**修法；根因是 `#[test]` 被 GPUI 的同名宏遮蔽 |

> 这些都是"本机无法编译"才会拖到 CI 才暴露的典型问题：
> 自动解引用层级、固有方法遮蔽标准方法、serde 对序列的宽容、trait 不在作用域、
> 属性宏被同名遮蔽。

### 单独说一个坑：`#[test]` 被 GPUI 的同名宏遮蔽

这是整个项目里最迷惑人的一个错误，值得单独记下来。

`gpui-pre/src/gpui.rs` 里**无条件**再导出了 gpui_macros 的 `test` 属性宏：

```rust
pub use gpui_macros::{
    AppContext, IntoElement, Render, VisualContext, bench, property_test, register_action, test,
    ...
};
```

而 `views.rs` 模块里有 `use gpui_kit::*;`。测试模块一旦写成 `use super::*;`，
就会把父模块里那个 glob 导入的名字**一并继承进来**，于是模块内的 `#[test]`
解析到 **GPUI 的 test 宏**而不是 Rust 内建的那个，展开时自我递归。

症状极具误导性：

```
error: recursion limit reached while expanding `#[test]`
  = help: consider increasing the recursion limit by adding a
          `#![recursion_limit = "1024"]` attribute to your crate
```

把上限从默认 128 调到 256、再调到 512，**报错依旧**，只是提示值跟着涨到 1024
—— 因为问题不是"深度不够"，而是宏被同名遮蔽，调多大都会烧穿。

**正确修法**（gpui-kit 的 lib.rs 注释里其实写了：
*"Test modules should import their Kit types explicitly to avoid shadowing Rust's `#[test]`."*）：
测试模块**显式导入**需要的类型：

```rust
#[cfg(test)]
mod tests {
    // 不要写 `use super::*;`
    use super::{ALL_COLUMNS, RUNNING_COLUMNS, presets_for};
    use wslc_core::settings::{SETTING_KEYS, SettingKind};
    // ...
}
```

> **推论**：只要测试所在模块（或它的祖先）glob 导入了 `gpui_kit`，
> 就不能用 `use super::*;`。
> `wslc-core` 不受影响（不依赖 GPUI）；`wslc-panel` 的 `state.rs` 也不受影响
> （只导入 `wslc_core`）；只有 `views.rs` / `app.rs` 这类带 gpui glob 的模块要当心。

---

## 5. CI 作为编译验证通道

本机没有 Rust 工具链，所以把编译验证交给 GitHub Actions（Windows runner 自带 Rust + MSVC）：

- **`ci.yml`**
  - `core`：`cargo test -p wslc-core`（真机冒烟测试在没有 `wslc` 的 runner 上会自动跳过）
  - `ui`：`cargo check --workspace --all-targets` —— 这是 GPUI API 假设的唯一权威验证
  - `lint`：rustfmt / clippy，**只报告不阻塞**，输出写进 Step Summary
- **`release.yml`**：推 `v*` 标签或手动触发，编译 release 并打包 exe
  （打标签时同时建 Release；手动触发时只出 Artifact）
- **`fmt.yml`**：**手动触发**，跑 `cargo fmt --all` 并把结果提交回仓库。
  开发机上没有 rustfmt，这是唯一能拿到权威格式化结果的地方。

本地能在没有工具链的情况下做的两项静态检查（脚本在 `%TEMP%`，未入库）：

1. 括号/引号配平扫描（能正确处理注释、原始字符串、字符字面量 vs 生命周期）
2. 方法名交叉比对：把代码里所有 `.name(` 与 `gpui-pre` / `gpui-pre-macros` /
   `gpui-base` / `gpui-component` / `gpui-kit` 源码里的标识符比对，
   找出"调用了但依赖里不存在"的方法（拼错的方法名抓得到）

第 2 项已经抓到 `Button::primary()` 需要导入 trait `ButtonVariants` 这个问题。

---

## 6. 回退方案

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

---

## 7. v0.2 调研结论（存储与输入控件）

### 7.1 `storagePath` 显示不出来，是因为出厂全是注释

`settings.yaml` 把所有键都写成注释：

```yaml
  # storagePath: default
```

所以读到的永远是"未设置"。而 `wslc info` **不报**解析后的路径，只报
`Client.SettingsFile`。真实语义要读 settings.yaml 自己的注释：

> Base directory for the default session's storage; the session VHD is created at
> `<storagePath>\wslc\sessions\<session>\storage.vhdx`.
> default: `%LOCALAPPDATA%`

实测本机：

```
%LOCALAPPDATA%\wslc\sessions\wslc-cli-76434\storage.vhdx    612 MB
```

两个来源的会话名一致（`info.Server.Sessions[0].Name` 与
`system session list` 的显示名称都是 `wslc-cli-76434`），和磁盘目录名也对得上。

> ⚠️ 类型陷阱：`Session` 在不同地方字段名不同 ——
> `ServerInfo::sessions`（来自 `wslc info`）是 `name`，
> `Vec<Session>`（来自 `wslc system session list`）是 `display_name`。
> CI 抓过一次。

### 7.2 磁盘占用：`wslc` 没有 `system df`

`wslc system` 只有 `events` / `info` / `session`。逐项可得性：

| 指标 | 来源 | 结论 |
|---|---|---|
| 会话 VHD 占用 | 文件系统 | ✅ 精确 |
| 卷容量/可用 | `GetDiskFreeSpaceExW` | ✅ 精确 |
| 镜像合计 | `wslc images` 的 `Size` 求和 | ✅ 精确（含共享层重复计算） |
| 容器可写层 / 卷 | —— | ❌ 与镜像共用同一个 VHD，分不出来 |

拿卷容量的三条路都试过：

- `wslc` 没有对应命令；
- `fsutil volume diskfree` → **Error 5: Access is denied**（要管理员）；
- 起 PowerShell 查要 300ms+，刷新一次多一倍耗时；
- 最终用 `windows` crate 0.58（对齐 GPUI 依赖树里已有的版本，零额外编译成本）
  调 `GetDiskFreeSpaceExW`，微秒级。

### 7.3 GPUI `InputState` 的接入要点（第一次用输入控件）

API 全部在依赖源码里核对过：

| 事项 | 正确写法 | 出处 |
|---|---|---|
| 构造 | `cx.new(\|cx\| InputState::new(window, cx).placeholder(..))` | `gpui-component/src/list/list.rs:96` |
| 预制值 | `.default_value("..")` | `gpui-component/src/input/input.rs:1078` |
| 渲染 | `Input::new(&entity).id("..").w_full()` | `gpui-component/src/input/input.rs:213` |
| 读值 | `entity.read(cx).value()` —— **不带参数** | `gpui-base/src/input/base/state.rs:1257` |
| 焦点 | `let h = entity.read(cx).focus_handle(cx); window.focus(&h, cx);` | 两者都是公开 API |

两个坑：

1. **`value` 不带参数**。`gpui-component/src/input/state.rs:294` 里另有一个
   `value(&self, cx)` —— 那是**别的类型**的同名方法，照抄会报
   `E0061: this method takes 0 arguments`。CI 抓到了。
2. **`InputState::new` 要 `&mut Window`**，而 `Shell::new(cx)` 拿不到 window。
   所以只能**懒创建**：用户点按钮时（事件回调里有 `window`）才建。
   另外 `InputState::focus` 是 `pub(crate)`，外部只能用 `focus_handle` + `window.focus`。

### 7.4 分层没有破

`AppState` 依然**完全不碰 GPUI**。输入框是 GPUI 类型，所以放在
`Shell::pull_input: Option<Entity<InputState>>`，而"是否正在拉取"这种
纯数据状态（`AppState::pulling`）留在数据层。

这样做的好处已经在 CI 上体现：`cargo test -p wslc-core` 从始至终
不需要 GPU、不需要窗口，一直能跑。

### 7.5 流式执行与取消（`wslc pull` 的进度）

`run` 系方法会把输出**缓冲到进程结束**才返回，所以拉取只能显示"进行中"。
要实时进度就必须边跑边读。

`Wslc::spawn_streaming` 的设计要点：

| 决定 | 原因 |
|---|---|
| 两个读取线程（stdout / stderr 各一个） | 单线程读其中一个会死锁：管道缓冲写满后子进程卡住 |
| `read_until(b'\n')` 而不是 `BufRead::lines()` | 后者碰到非 UTF-8 直接报错中断 |
| `try_wait()` **非阻塞**轮询 | 调用方在 GPUI 异步执行器上，阻塞的 `wait()` 会占着线程池 |
| `finish()` 单独负责 join 读取线程 | 进程退出后读取线程还要排空管道里的剩余字节 |
| `CancelToken` 可克隆、与句柄分离 | 界面握令牌（点"取消"），执行任务握句柄，不用搬整个句柄 |
| 回调要求 `Send + Sync` | 会在两个线程上被调用 |

> ⚠️ **隐含前提：输出必须是 UTF-8**。按 `\n` 的字节切分时，
> UTF-16LE 的一行字节数是奇数，`decode` 的启发式判定会失效。
> `build_command` 注入的 `WSL_UTF8=1` 保证了这一点（实测过）。

**输出从读取线程搬到界面**：读取线程碰不到 `AppState`，所以用一个
`Arc<Mutex<Vec<String>>>` 当中转，异步任务每 200ms 搬一次。
刻意不用 `std::sync::mpsc::Sender` —— 回调要求 `Send + Sync`，
而 `Sender` 的 `Sync` 实现随 Rust 版本变化，共享缓冲没这个不确定性。

**测试**：4 个纯逻辑的行切分测试 + 2 个 Windows 端到端测试
（真起 `cmd` 收多行输出；起 `ping` 后 `kill`，断言 15 秒内结束且标记已取消）。
CI 上确认过它们**真的执行**了，不是被条件编译跳过。
`wslc-core` 目前 **167 个测试全绿**。

### 7.6 一个坑：API 推送不再触发 `push` 事件

用 REST API（`PATCH /git/refs/heads/main`）推送，前几次都能正常触发
`ci.yml`；某一次之后就不再产生 `push` 类型的 run 了
（三个 workflow 的 `state` 都是 `active`，不是被禁用）。

**应对**：推完显式派发一次

```powershell
POST /repos/{owner}/{repo}/actions/workflows/ci.yml/dispatches  {"ref":"main"}
```

反正 `ci.yml` 本来就带 `workflow_dispatch`，不影响正常用法。

### 7.7 组件库的主题必须**显式**切换

这是实机截图才发现的：`Input` 里明明打了字，但读不出来 ——
因为 **gpui-component 的默认主题是浅色**，它渲染出来的是白底浅灰字。

我们自己画的 `div` 由 `theme.rs` 定色，看起来是深色的，
于是很容易以为"整个界面都是深色的"。**但组件库的控件
（`Input` / `Button` / `Select` …）只认它自己的主题。**

```rust
gpui_kit::init(cx);
gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, cx);
```

出处：`gpui-component/src/theme/mod.rs:389`
（`pub fn change(mode: impl Into<ThemeMode>, _window: Option<&mut Window>, cx: &mut App)`），
签名里那个 `window` 参数**不读** —— 它内部会刷新所有窗口。

**连带发现**：`.disabled()` 的禁用态文字几乎看不清
（「拉取镜像」「刷新」「已保存」三个按钮都中招）。
与其去调组件库的禁用态配色，不如**干脆不用 `.disabled()`**：
不可用状态用"守卫 + 文案"表达，可读性反而更好 ——
比如"已保存"本来就是个**状态**，一行绿色文字比灰按钮更准确。

### 7.8 `cargo check --all-targets` **不会执行**测试

一个差点被漏掉的验证缺口：

`views.rs` / `app.rs` 里的单测属于 **bin 的 test target**。
`cargo check --workspace --all-targets` 只把它们**编译**一遍，
**不会运行**。而 `core` 任务跑的是 `cargo test -p wslc-core`，
覆盖不到 `wslc-panel`。

也就是说：`wslc-panel` 里那些测试（表格列宽、设置预设、输入切分……）
在加上这一步之前，**从来没有真正执行过**。

补上：

```yaml
- name: 跑 wslc-panel 自己的测试
  run: cargo test -p wslc-panel --bins -- --nocapture
```

它们都是纯逻辑，不创建窗口也不需要 GPU，所以能在 CI 上直接跑。
**今后往 `wslc-panel` 里加测试，记得它是靠这一步执行的。**
