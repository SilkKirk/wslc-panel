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
| B | ⏳ 待运行验证 | 需要真实窗口，只能本机 `cargo run` 后目视 |
| C | 🟡 编译期通过 | GPU/D3D11 是运行期行为，CI 无显示设备 |
| D | ⏳ 待运行验证 | `WindowOptions::default()` 编译通过，运行效果待看 |
| E | ✅ **已验证** | `Context::spawn` 签名是 `AsyncFnOnce(WeakEntity<T>, &mut AsyncApp) -> R`，与 `layer_shell.rs` / `testing.rs` / `example_editor.rs` 的写法**逐字一致**；`WeakEntity::update` 返回 `Result`，用 `let _ =` 接住 |
| F | ✅ **已验证** | `ButtonVariants`（`primary()`）需从 `button::*` 导入；`Sizable::small` / `Disableable::disabled` 见 gpui-component 源码 |
| G | ✅ **已验证** | 刻度表见 `gpui-pre-macros/src/styles.rs::box_style_suffixes`（含 `6`/`8`）与 `*_box_style_prefixes`（含 `px`/`py`/`gap`） |
| H | ✅ 已绕开 | 不用 `.opacity()`，改为 `theme.rs` 预置不透明色 + `hsla` 遮罩 |
| I | ✅ **已验证** | `div().id(...).on_click(...)` 在 CI run 2 没有再报错 |
| J | 🟡 编译期通过 | `gpui_kit::assets::Assets` + `.with_assets()` 已过编译 |
| K | ⏳ 待补采样 | 卷的真实 JSON 字段名 |

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
