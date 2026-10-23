# 管理功能插件化方案

> 目标：把「数据库管理」「部署管理」这类**容器管理功能**从主程序里拆出去，
> 变成可以单独安装 / 卸载的插件；以后加新功能不必改主程序。
>
> 注意：这里说的是**我们自己功能的插件化**，不是接入第三方插件市场生态。
> 一切取舍都围绕"能不能真的装上去用"，而不是"能不能做成平台"。

---

## 1. 先说三个硬约束（它们决定了哪些方案根本不可能）

### ① 我们是 GPUI 原生 UI，没有 webview

绝大多数桌面软件的插件体系（VS Code / dbx / Obsidian）都靠 **插件自带 HTML/JS
界面**，宿主只提供 webview。我们没有 webview —— GPUI 是 Rust 里直接画。

**后果：插件不能自己带界面。** 界面只能由宿主渲染，插件最多提供"要显示什么"。
这条不能绕，除非整个 UI 层换技术栈（不现实）。

### ② 单文件静态 exe

`wslc-panel.exe` 是发布契约（更新提示、README、发布页都指向它）。
Rust 没有稳定的 ABI，所以：

- **不能**像 C 插件那样 dlopen 一个 `.dll` —— 每次升级编译器就可能崩，
  而且崩溃是内存错误级别，不是"报错"级别；
- 能做的是把插件编成**独立进程**，用文本协议通信。

### ③ 本机没有 Rust 工具链，CI 是唯一编译验证

`AGENTS.md` §1。任何"先写个插件加载器试试"的想法都要付 5–15 分钟一轮的
CI 代价，所以方案必须**分成能独立验证的小步**，不能一口气重构成插件架构。

---

## 2. 三种插件形态，选哪个

| | 编译期插件 | **进程外插件** | WASM 插件 |
|---|---|---|---|
| 怎么"装" | 重新编译 / 另一个 exe | 解压一个包到插件目录 | 下载一个 .wasm |
| 能带界面 | ❌ | ❌（宿主渲染） | ❌（宿主渲染） |
| 第三方能开发 | 要给我们提 PR | ✅ 任意语言/任意编译器 | ✅ 但要 WASM 工具链 |
| 崩溃影响 | 整个面板 | 只有那个插件 | 沙箱隔离 |
| 新依赖 | 零 | `serde_json`（已有） | `wasmtime`（重） |
| 工作量 | 小 | **中**（协议 + 渲染器 + 安装） | 大 |

**结论：分两层**

1. **内置功能用 crate 边界 + feature flag**（编译期）：
   `wslc-panel` 不再直接 `use` 数据库/部署的代码，而是通过一个
   `FeatureRegistry` 拿。**这是插件化的前提**，而且**不管最终装不装得上去都值得做**。
2. **想"不重新编译就装"的功能走进程外插件**：独立 exe + stdio JSON 协议，
   界面由宿主按**声明式页面描述**渲染。

WASM 现在不做：`wasmtime` 是重量级依赖，而它能带来的（沙箱）在"插件只有我们自己写"
的阶段没有价值。

---

## 3. 现状：为什么必须先拆边界

`AGENTS.md` §2 记着两个文件的大小问题，这一轮又长了一截：

| 文件 | 行数 | 里面有什么 |
|---|---|---|
| `crates/wslc-panel/src/app.rs` | **10 873** | 全部动作（数据库 / 部署 / 镜像 / 网络 / 卷 / 安装…）+ 全部浮层 |
| `crates/wslc-panel/src/views.rs` | **10 384** | 全部渲染 + 全部弹窗 |
| `crates/wslc-panel-core/src/state.rs` | **7 137** | `AppState` 里同时挂着 `db: Option<DbState>` / `deploy: Option<DeployState>` |

耦合点很具体：

- `Page` 枚举**写死**了所有页面（`Page::ALL`、`label()`、`group()`、`nav_id()`）；
- `views::page()` 是一个大 `match`，每个分支调各自那坨函数；
- `Shell::render` 的导航栏从 `Page::ALL` 生成 —— 想加一个页面必须改这三处；
- 数据库/部署的**动作**和**其它功能的动作**挤在同一份 `impl Shell` 里。

所以"插件化"的第一件事不是加载器，而是**把这四个耦合点变成注册表**。

---

## 4. 目标架构

### 4.1 两级扩展点

```rust
/// 一个"管理功能" = 一个页面 + 它自己的状态 + 它的动作。
pub trait Feature: Send + Sync {
    fn id(&self) -> FeatureId;                  // "db" / "deploy"
    fn page(&self) -> PageSpec;                 // 标题 / 分组 / 图标 / 排序
    /// 这个容器能不能用这个功能（决定列表行上显不显示入口）
    fn detect(&self, item: &ContainerListItem) -> Option<EntryHint>;
    /// 页面内容：**声明式**（见 4.2），不是 GPUI 元素
    fn render(&self, ctx: &FeatureCtx) -> PageBody;
    /// 用户点了某个动作
    fn action(&self, ctx: &mut FeatureCtx, action: ActionId) -> ActionOutcome;
}

/// 数据级扩展点（没有自己的页面）
pub trait Extension: Send + Sync {
    fn id(&self) -> ExtensionId;                // "db-engine.mysql" / "image-preset.php"
}
```

两级的分工：`Feature` 有页面，`Extension` 只是挂到别处（比如数据库引擎、
镜像预设、健康检查规则）。**数据库引擎天然就是 Extension** ——
v0.16 刚建的 `wslc-db` + `wslc-core::db::dialect` 已经是这个形状了。

### 4.2 主程序只认"声明式页面"

插件（无论内置还是进程外）返回的不是 GPUI 元素，而是一个**数据描述**：

```rust
pub enum Section {
    Heading(String),
    Text { tone: Tone, text: String },
    Card { title: String, rows: Vec<KeyValue> },
    Table(TableSpec),            // 列 + 行 + 每行可带的动作
    Form(FormSpec),              // 字段 + 提交按钮
    Actions(Vec<ActionSpec>),    // 按钮
    Empty { text: String },
}
```

宿主（`wslc-panel`）负责把 `PageBody` 渲染成 GPUI。
**这一步是插件能存在的前提**：只有数据跨进程，界面永远在宿主手里。

### 4.3 但要承认一个例外：数据库的 SQL 控制台

数据库页里那个「SQL 控制台」是**编辑器 + 树 + 结果表格**的组合，
用 `Section` 表达不了（也不该硬塞 —— 硬塞的结果是一个永远只支持
"标准控件"的抽象，最后还得开洞）。

所以留一条**内置特权通道**：

```rust
pub enum PageBody {
    Declarative(Vec<Section>),   // 插件能给的
    Builtin(BuiltinPageId),      // 只有内置 Feature 能用：渲染函数还在 wslc-panel 里
}
```

`Builtin("db.console")` 指向 `views::sql_console_page`（已有），
这样数据库插件可以"普通页面声明式 + 控制台走内置渲染"。

**这不是偷懒**：任何成熟的插件体系都有"特权 UI"（VS Code 的 webview、
浏览器扩展的 devtools panel）。区别只在于我们说清楚它是特权、只给内置用。

---

## 5. 阶段划分

每个阶段都能单独发布、都有可查的验收判据。

### 阶段 0（v0.17.0）抽边界，**行为零变化**

- `wslc-panel-core`：新增 `feature.rs`（`FeatureId` / `PageSpec` / `Feature` /
  `FeatureRegistry`），`Section` / `PageBody` 的数据类型。
- `wslc-panel`：`Page` 枚举改成注册表生成（`Page::ALL` → `registry.pages()`），
  导航栏、`views::page()` 的 match 都从注册表取。
- 数据库 / 部署各自实现 `Feature`，但**先只搬"页面元信息 + 入口检测"**，
  渲染函数仍留在 `views.rs`（通过 `Builtin` 通道）。

**验收判据：**
- `core` 633 + `wslc-panel-core` 241 条测试**一条不少**；
- 界面**完全看不出变化**（截图对比）；
- `grep -c "Page::Deploy\|Page::Database" crates/wslc-panel/src/views.rs`
  只在 `Builtin` 分发处出现，不再散落在逻辑里。

### 阶段 1（v0.17.1）编译期可选

- `crates/wslc-panel/Cargo.toml` 加 `[features] default = ["db", "deploy"]`；
  关掉某个 feature 时对应页面从注册表消失。
- 用途：给"精简版"出包（体积 + 启动时间），**也是插件化的技术验证** ——
  如果关掉一个 feature 还需要改别处，说明边界没抽干净。

**验收判据**：`cargo check -p wslc-panel --no-default-features --features db`
能过，且界面里没有部署页、也没有任何"部署"字样。

### 阶段 2（v0.18.0）声明式渲染器

- 宿主实现 `Section` → GPUI 的渲染器（heading/text/card/table/form/actions）。
- 把**部署管理**改成纯声明式（它的形态规整：卡片 + 表格 + 几个动作）。
  这是最好的试验品：**先证明"一个真实功能能完全用声明式表达"**。

**验收判据**：部署页关掉内置实现后，靠声明式数据画出来的界面和现在一致。

### 阶段 3（v0.19.0）进程外插件

- 目录：`%APPDATA%\wslc-panel\plugins\<id>\`，里面有 `manifest.json` + `bin\*.exe`。
- 协议：宿主 ↔ 插件用 **stdio JSON Lines**（一行一个请求/响应，
  和项目里 `jsonl.rs` 已有的解析风格一致）。
  请求只有四种：`describe`（拿 `PageSpec`）、`render`（拿 `PageBody`）、
  `action`（执行）、`shutdown`。
- 安装 = 把文件夹放进去（或 UI 里选一个 zip 解压）；卸载 = 删文件夹。
- 宿主启动时扫目录，读 `manifest.json` 的 `engines.host_api`，版本不匹配就不加载。

**验收判据**：把部署插件从内置改成外部 exe，功能不变；
插件崩溃（`panic!` / `exit 1`）时面板显示"插件 X 已停止"，**不闪退**。

### 阶段 4 才考虑的事

- 插件签名 / 校验和
- 从网络安装（现在只支持本地目录 / 手工放文件）
- 沙箱与权限（见下）

---

## 6. 安全：进程外插件等于完全控制

一个外部 exe 能做这台机器上任何事 —— 它能 `wslc rm -f`、能读 `~/.ssh`。
所以：

- **默认只从本地安装**（用户自己放文件 / 自己选 zip），不做自动下载；
- `manifest.json` 里声明 `permissions`（`exec.wslc` / `net` / `fs.read`），
  界面在**首次加载时**把权限列出来让用户确认，记在 `prefs` 里；
- 插件进程用**独立 stdio**，宿主做超时（比如 `render` 3 秒不返回就判死）；
- 不做"插件可以改面板自己的配置"这种能力 —— 需要的话由宿主代劳。

**不做签名**是因为：现在插件只有我们自己写。（真开放给第三方时必须补，
那是独立一块工作量。）

---

## 7. 为什么这条路对现在的我们值得

1. **`app.rs` / `views.rs` 各一万行**已经是实打实的维护成本 ——
   本会话里我在这两个文件上出过三次事故（删过头 9 个函数、写出 1071 个空行、
   引号拼坏一行）。**抽边界本身就是收益**，不必等插件真的能装。
2. **数据库那块已经自己长出边界了**：`wslc-core::db`（引擎/方言）、
   `wslc-db`（原生驱动）、`wslc-panel-core::sqlcomplete`（补全）。
   把它们登记成 Extension 只差一层注册表。
3. **部署管理形态规整**，是最适合先验证"声明式能表达真实功能"的样本。

---

## 8. 不做什么（明确排除，防范围膨胀）

- ❌ 换成 webview / Electron 那套（为了插件换整个 UI 技术栈，代价不成比例）
- ❌ dlopen 一个 Rust dylib（ABI 不稳定，崩了是内存错误）
- ❌ WASM 沙箱（`wasmtime` 太重，现阶段没有第三方插件需要隔离）
- ❌ 插件市场 / 自动更新 / 评分（那是产品运营，不是架构）
- ❌ 让插件自带任意界面（做不到，见 §1①）
- ❌ 热加载（改一个插件要重启面板，可接受）

---

## 9. 一句话

**先把「功能」变成「注册表里的条目 + 声明式的页面数据」，再谈「插件是内置的还是外部进程」**
—— 顺序反过来的话，会得到一个能加载插件、但每个插件都要往主程序里加代码的假插件体系。
