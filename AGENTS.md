# AGENTS.md —— 给在本仓库干活的编码代理

> 这份文件是**给 AI 代理看的**，不是给人看的入门文档（那个是 `README.md`）。
> 内容都是"不知道就会踩坑、而且踩了很难查"的那一类。

---

## 0. 一句话

`wslc-panel` 是 `wslc`（WSL **容器**）+ `wsl.exe`（WSL **发行版**）的图形面板。
Rust + [gpui-kit](https://github.com/longbridge/gpui-kit)（GPUI）。

---

## 1. ⚠️ 最重要的一条：本机没有 Rust 工具链

**这台开发机上 `cargo` / `rustc` / `rustup` 都不存在。** 这不是"偶尔忘了装"，
是这个仓库的既定工作方式：

- **写下的每一行 Rust 都没编译过**；
- **CI 是唯一的编译验证通道**，也是唯一能拿到 `rustfmt` diff 的地方；
- 所以：**改完就推，然后看 CI**。不要在本地"应该没问题"。

这条决定了后面很多约定（比如为什么要把纯逻辑拆出来、为什么测试要写得那么死）。

---

## 2. 仓库结构：三个 crate，拆法是有原因的

```
crates/
├── wslc-core/        数据层。不依赖任何 UI。MSRV 1.75，edition 2021
│   ├── cli.rs        Wslc / Wsl 两个调用器 + 超时 / 取消 / 流式
│   ├── decode.rs     UTF-16LE ↔ UTF-8（wsl.exe 的编码坑）
│   ├── jsonl.rs      JSON Lines 解析（空输出 / 坏行 / 字段缺失都不 panic）
│   ├── settings.rs   wslc 的 settings.yaml（**保留注释**）
│   ├── storage.rs    会话存储占用（自己算，wslc 不提供）
│   ├── wslconfig.rs  %USERPROFILE%\.wslconfig 的读取与静态检查
│   ├── model/        纯数据模型（Container / Distro / WslStatus……）
│   └── cmd/          各子命令的类型化封装
│       ├── container.rs / image.rs / network.rs / volume.rs / system.rs  ← wslc.exe
│       ├── distro.rs                                                    ← wsl.exe
│       └── picker.rs  文件/目录选择器（借独立进程弹 Windows 原生对话框）
│
├── wslc-panel-core/  面板里**不依赖 GPUI** 的那一半。edition 2024
│   ├── prefs.rs      本程序自己的偏好（refresh_secs / theme）
│   ├── state.rs      AppState / Snapshot / 各种动作枚举
│   ├── columns.rs    表格列定义
│   ├── presets.rs    预设值
│   └── util.rs       纯函数（format_bytes 之类）
│
└── wslc-panel/       只有 UI。edition 2024，rust-version 1.85
    ├── main.rs       入口、启动日志、应用主题
    ├── app.rs        Shell（GPUI 实体）+ 所有动作 + 弹窗浮层
    ├── views.rs      渲染（**很大会很大**，约 114 KB）
    └── theme.rs      颜色
```

### 为什么把 `wslc-panel-core` 拆出来

因为 **CI 时间**。原先面板的单测跑的是 `cargo test -p wslc-panel --bins`，
那一步**必须把整棵 GPUI 依赖树 codegen 并链接一遍**才能跑几个字符串切分和
列宽的断言 —— 实测 **763 秒**，占掉一整轮 CI 的 72%。而 `cargo check` 产出的
只是元数据，对那一步一点用都没有，缓存再热也得从头再来。

那些测试全是纯逻辑，搬进不依赖 GPUI 的 `wslc-panel-core` 之后，
由 `core` 任务里的 `cargo test --workspace --exclude wslc-panel` 跑，**十几秒完事**。

> **一轮 CI：17.7 分钟 → 1.7 分钟。测试一条没少。**

**所以：往 `wslc-panel-core` 里加东西是默认选择。** 只有真的需要
`Entity` / `Context` / `Window` / `Element` 的东西才留在 `wslc-panel`。
判断标准很简单：**这个文件 `use gpui` 吗？** 不 use 就该搬过去。

---

## 3. CI 怎么读

`.github/workflows/ci.yml`，三个 job：

| job | 名字 | 干什么 | 耗时 |
|---|---|---|---|
| `core` | 纯逻辑测试（wslc-core + wslc-panel-core，无需 GPU） | `cargo test --workspace --exclude wslc-panel --locked --profile ci` | ~1 分钟 |
| `ui` | wslc-panel 编译检查（GPUI） | `cargo check --workspace --all-targets --locked` + `cargo check -p wslc-panel --bins --locked` | ~15 分钟（冷缓存） |
| `lint` | 格式与 clippy（只报告，不阻塞） | rustfmt + clippy，`continue-on-error: true` | — |

要点：

- **`core` 先绿不代表能过** —— GPUI 的 API 假设（异步闭包、`ButtonVariants`、
  `Styled` 方法名、`spawn_in` / `update_in`……）只有 `ui` 那个 job 真正编译过才算数。
  **UI 改动要盯 `ui`。**
- **`core` 用 `--workspace --exclude wslc-panel`，不写死包名** ——
  写死的话，往 workspace 里加第 4 个 crate 时它的测试**永远不会跑**而 CI 照样绿
  （而 §2 恰恰鼓励把纯逻辑搬进 `wslc-panel-core`，也就是鼓励加新东西）。
  `--exclude` 是为了不把 GPUI 链接回来（那是 763 秒的来源）。
- **所有 cargo 步骤都带 `--locked`** —— 本机没有 cargo，改了 `Cargo.toml`
  之后**无法在本地重新生成锁文件**；不带 `--locked`，cargo 会自己解析一份新依赖集
  继续跑，于是 CI 全绿而仓库里那份 `Cargo.lock` 永远是旧的：构建不可复现，
  release 会用一份**没记录在仓库里**的依赖集出包。
- `push` **只监听 `main`**；功能分支走 `pull_request`。所以「推分支 + 开 PR」
  不会跑两遍（这是刻意改的，见 `ci.yml` 顶部注释）。
- 同一个分支连推两次会**取消上一次**（`concurrency`）。
- 纯文档 PR 会跳过（`paths-ignore: **.md`）。
- `--profile ci`（见根 `Cargo.toml`）是给**真的会 codegen** 的步骤用的：
  不优化、不带调试信息。**`cargo check` 不要加它** —— 白搭一份冷缓存。

> ⚠️ **「CI 绿」这个信号只覆盖：类型检查过 + 纯逻辑测试跑过。仅此而已。**
> 它**不**包含：格式（`lint` 永远不阻塞，见下）、clippy、MSRV（CI 用浮动
> `stable`，`rust-version = "1.75"` 无人校验）、以及界面到底渲染成什么样（§8）。
> 别把它当全能凭证。

### 拿 CI 结果的可靠姿势

`github.com:443` 在这台机器上**经常连不上**，但 `api.github.com` 通常通。
用 REST API 读，别指望 `gh`：

```powershell
$cred = "protocol=https`nhost=github.com`n" | git credential fill 2>$null
$tok  = ($cred | Select-String '^password=').Line -replace '^password=',''
$H    = @{ Authorization = "token $tok"; Accept = "application/vnd.github+json"; "User-Agent" = "dsh" }
$api  = "https://api.github.com/repos/SilkKirk/wslc-panel"

# 某个提交的检查
Invoke-RestMethod -Headers $H -Uri "$api/commits/<sha>/check-runs"

# 某个 job 的完整日志（拿错误用）
(Invoke-WebRequest -Headers $H -UseBasicParsing -Uri "$api/actions/jobs/<job_id>/logs").Content
```

⚠️ 轮询"CI 跑完了没"有两个**相反**的坑，都得防：

1. **空列表也满足"没有未完成的"** —— 会让循环立刻退出、拿着空结果往下走。
   所以必须同时要求 `check_runs.Count -gt 0`。（这个 bug 真的发生过一次。）
2. **但纯文档 PR（`**.md` / `docs/**`）一条 check run 都不会产生** ——
   `paths-ignore` 把它们整个跳过了。对这类 PR，`Count -gt 0` 会让循环
   **永远等下去**（表现为"卡住"，比第 1 条更难查）。正确姿势：先看这个 PR
   改了哪些文件；零 check run 就直接判成"预期跳过"，别再等。

另外，**只看"没有未完成的"不够，还要看 `conclusion`**：被 `concurrency`
取消掉的运行、以及被跳过的 job，状态都是 `completed` —— 不检查 `conclusion`
就会把"取消"读成"绿了"。收工条件应该是
`status == 'completed'` **且** `conclusion in ('success','neutral','skipped')`。

---

## 4. 发布流程

1. **先让 CI 在 main 上全绿**（尤其是 `ui`）。
2. 升版本号：
   - 根 `Cargo.toml` 的 `[workspace.package] version`
   - `crates/wslc-core/Cargo.toml` 里那个**显式**的 `version`（它还没改成
     `version.workspace = true`，是个小遗留）
   - `README.md` 里那行启动日志样例（`版本 x.y.z，构建 <短 sha>`）
3. 推 main → **等 CI 全绿** → 打 `vX.Y.Z` 标签 → 推标签。
4. `release.yml` 由 `push: tags: ["v*"]` 触发，编译并发布
   `wslc-panel.exe` + `wslc-panel-vX.Y.Z-windows-x64.zip`。

**标签必须落在 main 的祖先链上。** 如果 `git push` 不通、改用 API 建提交，
本地和远端的 sha 会分叉 —— 那时候 `git tag` 打出来的是**本地**那个提交，
标签就不在 main 上了（发生过一次）。建完标签核对一下：

```powershell
# tag 解引用到的 commit，树应该和 main 的一样
(Invoke-RestMethod -Headers $H -Uri "$api/git/tags/<tag-object-sha>").object.sha
```

---

## 5. 代码约定

### 注释用中文，而且解释**为什么**

不是"这行在干什么"，是"**为什么这么干、不这么干会怎样**"。
每条非显然的决定后面都该有代价、实测数据或者被否掉的替代方案。
看看 `cmd/distro.rs` 里 `start()` 的注释就明白这个密度了。

### 实测优先于文档

这个项目最核心的价值观：**能测就测，别信文档也别信直觉。**
几乎所有关键决定后面都跟着一句"实测……"。看到"实测"两个字，
就知道那一条是跑出来的，不是推的。

反过来说：**如果一条结论是推出来的，要写明它是推的**，
并说清什么情况下会不成立。

### 解析逻辑放 `model/`，别放 `cmd/`

`cmd/` 只负责"拼参数 → 跑命令 → 把结果交出去"。真正的解析放 `model/`，
那样才能**脱离 Windows 跑单测**。`model/distro.rs` 就是这么拆的。

### 测试用**真实抓下来的夹具**

`crates/wslc-core/tests/fixtures/` 里全是真机抓的输出（`distro_list.txt`、
`lxss_query.txt`、`container_list.jsonl`……），不是编的。
编的夹具会"正好符合我的解析器"，那就测不出东西。

纯函数（尤其是拼命令、拼脚本、解析输出的）**必须有测试** ——
这类东西错了只表现为"界面上什么也没发生"，是最难查的一类问题。

### 提交信息

中文，conventional commits 风格：`feat:` / `fix:` / `perf:` / `chore:` / `docs:` / `refactor:`。
正文写清**为什么**，以及被否掉的替代方案。

### 计划文档

`docs/PLAN-*.md` 是分阶段计划，`docs/SPIKE.md` 是前期技术验证，
`docs/wslc-schema.md` 是字段字典。**改行为时顺手更新它们**，
它们是这个项目"当时为什么这么决定"的唯一记录。

---

## 6. 参考项目（只读，**绝对不能抄**）

```
E:\cnb\wsl-dashboard-ref     ← 已 clone 的参考实现
https://gitee.com/bye/wsl-dashboard
https://github.com/owu/wsl-dashboard
```

> ⚠️ **它是 GPL-3.0-only，本项目是 Apache-2.0。**
>
> 只能**读它来理解机制**（"它这个功能是怎么做到的"），
> **一行代码、一句文案、一个图标都不能进这个仓库**。
> 引用它的代码到 PR 描述或注释里说明问题是可以的，但要标明来源和许可。

它值得看的地方：`src/wsl/ops/` 下各个操作的实现思路。
它踩过的坑往往我们也会踩。

**注意：它自己的代码也有 bug。** 比如"吊住 wsl.exe 让发行版保持运行"
那段，它 `spawn()` 之后把 `Child` 直接丢掉了，从不 kill ——
于是关掉 GUI 之后那个 `wsl.exe` 会一直留着。我们**照抄了它的行为**
（这是产品选择），但**留了句柄**，所以能判断哨兵还活着、也能在界面上标出来。

---

## 7. 踩过的坑（都是实测的，别再踩一遍）

### 7.1 ⚠️ 别把阻塞调用放在界面线程上

**这是这个项目最严重的一类 bug，犯过一次。**

`wsl.exe` / `wslc` 的调用都是**同步等子进程**的。直接在事件回调里调它们，
界面线程就卡住了 —— Windows 大约 **5 秒**收不到窗口消息就给它挂上「未响应」。

症状：点「启动」→ 窗口变灰 → 过一会儿才弹提示 → 恢复。
压缩、移动这类**分钟级**操作能把界面挂十分钟。

**规矩：**

- 所有会起进程的东西一律走 `cx.background_executor().spawn(...)`；
- 参考 `Shell::spawn_distro_action` / `Shell::spawn_container_action`；
- **点下去先发一条「正在…」** —— 没有它，从点击到结果出来这段时间界面上
  什么都没发生，用户会以为按钮坏了然后去点第二次；
- 真的只是"把进程拉起来、不等待"的才可以同步：
  `spawn_in_new_console`（`CREATE_NEW_CONSOLE` + `spawn()`）、
  `explorer.exe`、`spawn_detached`。

### 7.2 异步里要 `&mut Window` → `spawn_in` + `update_in`

`InputState::set_value(value, window, cx)` **需要一个 `&mut Window`**，
而 `cx.spawn` 给的回调里没有 window。

```rust
cx.spawn_in(window, async move |this, cx| {
    let picked = cx.background_executor().spawn(async move { 阻塞活 }).await;
    let _ = this.update_in(cx, |shell, window, cx| {
        // 这里有 window 了
        input.update(cx, |state, cx| state.set_value(path, window, cx));
    });
})
.detach();
```

这是 gpui-kit 自己组件里在用的写法（`crates/component/src/root.rs`、`list.rs`）。

### 7.3 WSL 的行为（全部实测于 WSL 3.0.1.0）

| 事实 | 说明 |
|---|---|
| **必须设 `WSL_UTF8=1`** | 否则 `wsl.exe` 输出 UTF-16LE。`decode.rs` 两边都处理，但设了更省事 |
| `wsl.exe -d <不存在>` 退出码是 **-1**，不是 1 | 别假设非零就是 1 |
| `wsl -l -v` 输出是 `\r\n`，列从 2 / 18 / 34 开始 | `*` 标默认发行版；**表头是英文，而 `wsl --status` 是中文**（本地化的） |
| **`wsl -d X -e true` 约 20 秒后自己 Stopped** | 最后一个活动会话结束就回收 |
| **`wsl -d X -e sh -c "setsid nohup sleep 900 &"` 也留不住** | 在发行版里造常驻进程**没用** |
| **`wsl -d X -- sleep infinity`（吊住 `wsl.exe` 不放）→ 一直 Running** | **这才是"启动"的正确做法**。见 `cmd::distro::start` |
| `--import` / `--install` **都没有 `--set-default`** | 「设为默认」必须装完**再跑一条**命令 |
| `--install --from-file` **没有 `--version`** | 这条路径指定不了 WSL 版本 |
| `wsl --list --online` 在这台机器上**不可用** | 解析不了 `raw.githubusercontent.com`；在线安装只能手输发行版名 |
| `.wslconfig` 的"键未知"告警**只在 VM 启动时报** | `--shutdown` 后第一条进发行版的命令报，之后静默；`--status`/`-l -v`/`--terminate` 从来不报 |
| `--manage --compact` 在 18 GB 的盘上 **10.2 秒** | 所以不需要进度条，提示 + 完成刷新就够 |
| 注册表 `HKCU\...\Lxss\{GUID}` 是安装位置的唯一来源 | `wsl` 命令不报告装在哪个盘。用 `reg.exe query /s`：**7~27 ms**，且输出**恒为 UTF-8**（哪怕系统 ACP=936） |
| 读 `/etc/wsl.conf` 会**启动发行版** | 有副作用，界面上要说清 |

### 7.4 gpui-kit（0.7.1）没有文件选择器

`crates/component/src/` 里没有 picker。所以 `cmd/picker.rs` 走的是
**起一个 `powershell.exe` 让另一个进程弹 Windows 原生对话框**这条路。

选它不选 `rfd` 的理由：**独立进程在结构上不可能阻塞界面线程**
（见 7.1）。代价是慢几百毫秒、不是严格模态、EDR 可能敏感 —— 都写在
`picker.rs` 的模块注释里了。要换 `rfd` 的话替换点只有两个函数。

### 7.5 弹窗 / 浮层的约定

- 滚动容器**必须先有 `.id(...)`** —— 但这条**不是**运行时 panic：
  `overflow_y_scroll` 来自 `StatefulInteractiveElement`，只对**带 id 的**元素
  可用，漏了 `.id()` 是**编译错误**（`no method named overflow_y_scroll`）。
  也就是说 `ui` job 会直接拦住，这类缺陷溜不进 main —— 不用把它当"会崩"来防。
  （`crates/wslc-panel/src/app.rs` 的 `app-body-scroll` 那段注释也说了这件事。）
- `InputState::new` 要 `&mut Window` → 表单只能**懒创建**（点击时建）。
  渲染时建会每帧重建输入框，**字都打不进去**。
- 输入框的焦点在打开时用 `window.focus(&handle, cx)` 给过去，
  `handle` 来自公开的 `InputState::focus_handle(cx)`（`InputState::focus` 是 `pub(crate)`）。
- 全项目**不用 `.disabled()`**（不导入 `Disableable`）——
  要"不可用"就用条件不渲染那个按钮，或者给一句说明。

### 7.6 网络：`github.com` 经常不通

- `github.com:443` 时常连不上（`Connection was reset` / 超时）；
- `api.github.com` 通常通；
- **`gitee.com` 通**（参考项目就是从那儿 clone 的）；
- 系统代理（`127.0.0.1:31181`）可能**中途消失**，别写死依赖它。

**`git push` 不通时的退路**：用 Git Data API 建提交，**逐字节精确**，
不要手抄文件内容：

```
POST /git/blobs          { content: <base64 of the local file>, encoding: "base64" }
POST /git/trees          { base_tree: <main 的 tree>, tree: [{path, mode:"100644", type:"blob", sha}] }
POST /git/commits        { message, tree, parents: [<main 的 sha>] }
PATCH /git/refs/heads/X  { sha: <新 commit>, force: true }
```

标签同理：`POST /git/tags` 建 tag 对象 → `POST /git/refs` 建 `refs/tags/vX.Y.Z`。
**用 API 建的提交和本地提交 sha 会不一样** —— 事后记得
`git fetch --tags --prune && git reset --hard origin/main` 对齐本地。

---

## 8. 验证的边界：CI 查不出来的东西

CI 只能证明**编译过、纯逻辑测试过**。下面这些**必须在本机点一遍**：

- 窗口是不是真的渲染出来了、布局有没有错位；
- 文件选择器的对话框**弹没弹出来、是不是在最前面**（7.4 的代价）；
- 发行版「启动」之后**等一分钟以上**看是不是还 Running（7.3 那条）；
- 主题切换是不是立即重绘。

改到这类东西时，**在 PR 描述里明确写出"需要本机实测"**，别只说"CI 绿了"。
