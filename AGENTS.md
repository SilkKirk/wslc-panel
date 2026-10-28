# AGENTS.md —— 给在本仓库干活的编码代理

> 这份文件是**给 AI 代理看的**，不是给人看的入门文档（那个是 `README.md`）。
> 内容都是"不知道就会踩坑、而且踩了很难查"的那一类。

---

## 0. 一句话

`wslc-panel` 是 `wslc`（WSL **容器**）+ `wsl.exe`（WSL **发行版**）的图形面板。
Rust + [gpui-kit](https://github.com/longbridge/gpui-kit)（GPUI）。

> ⚠️ **两条硬约束，开工前先看**：
> **① 本机没有 Rust 工具链**（见 §1）；**② 一次最多派 2 个子代理**（见 §9）。

---

## 1. ⚠️ 最重要的一条：本机没有 Rust 工具链

**这台开发机上 `cargo` / `rustc` / `rustup` 都不存在。** 这不是"偶尔忘了装"，
是这个仓库的既定工作方式：

- **写下的每一行 Rust 都没在本地编译过** —— 但 **CI 会真的编译它、也真的跑测试**：
  `ui` job 跑 `cargo check`，`core` job 跑 `cargo test`（见 §3）。
  ⚠️ 别把这条读成"代码没验证过"：它说的是**本机没有工具链**，不是"没人验过"。
  真正没被覆盖的只有 §8 那几项（渲染 / 布局 / 真实交互）；
- **CI 是唯一的编译验证通道**，也是唯一能拿到 `rustfmt` diff 的地方；
- 所以：**改完就推，然后看 CI**。不要在本地"应该没问题"。

这条决定了后面很多约定（比如为什么要把纯逻辑拆出来、为什么测试要写得那么死）。

---

## 2. 仓库结构：四个 crate，拆法是有原因的

```
crates/
├── wslc-core/        数据层。不依赖任何 UI。MSRV 1.85，edition 2024
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
├── wslc-db/          原生（进程内）数据库驱动。同步 mysql / postgres；
│                     依赖 wslc-core，不依赖 GPUI（拆出去是为了不带那两棵驱动树）
│
├── wslc-panel-core/  面板里**不依赖 GPUI** 的那一半。edition 2024
│   ├── state/        AppState / Snapshot / 各域状态（按域分 10 个模块 + `tests/` 目录）
│   ├── prefs.rs      本程序自己的偏好（refresh_secs / theme）
│   ├── columns.rs    表格列定义
│   ├── presets.rs    预设值
│   └── util.rs       纯函数（format_bytes / human_secs / strip_md …）
│
└── wslc-panel/       只有 UI。edition 2024，rust-version 1.85
    ├── main.rs       入口、启动日志、应用主题
    ├── app/          Shell（GPUI 实体）：shell/ 下按域分子模块（+ `dialogs.rs` 对话框栈与 `dialogs/specs.rs`、`db/` 目录）
    ├── views/        渲染：按页面/浮层分模块（common 放共用小组件）
    └── theme.rs      颜色
```

> **两个"大文件"已经拆掉了**（2026-10-25）。原先 `app.rs` 11201 行、
> `views.rs` 10546 行、`state.rs` 7492 行，现在是三个目录：
>
> | 目录 | 拆法 | 判断"新代码放哪" |
> |---|---|---|
> | `crates/wslc-panel/src/views/` | 一个页面/一类浮层一个文件；共用小组件进 `common/`（`layout` / `table` / `form` / `button` 四个子模块，`common.rs` 与 `common/` 目录并存），共用导入进 `prelude.rs` | **被两个以上页面用到？** 是 → `common`；否 → 那个页面自己的模块 |
> | `crates/wslc-panel/src/app/shell/` | 一个功能域一个文件，`impl Shell` **整块**搬（不按方法拆） | 新动作按**它属于哪个页面**归域（db / deploy / containers …） |
> | `crates/wslc-panel-core/src/state/` | 一个域一个文件，`impl` 跟着它修饰的类型走 | 新状态类型跟着它的域走 |
>
> 两条硬约束（都踩过）：
>
> 1. **`Shell` 的结构体必须留在 `app/shell/mod.rs`** —— 域模块是它的**后代**
>    才能读那几个**私有**字段（`tasks` / `refresh` / `subs` / `dialog_ui` /
>    `errors_expanded`）。挪进 `core.rs` 就变成兄弟，`self.state` 都访问不到。
>    （2026-10-26 B8 收尾后 `Shell` 只剩 **9 个顶层字段**：`state`（`pub`）+
>    `forms` / `sql_ui` / `sidebar_collapsed`（`pub(crate)`，`views` 要读）+
>    上面那 5 个私有。）
> 2. **按域命名文件要避开 `prelude` 里已有的路径段** —— `state/wslconf.rs` 会和
>    `use wslc_core::model::wslconf;` 撞成 `E0659: ambiguous`（`registry` /
>    `db` / `install` 同理，所以它们叫 `*_state.rs`）。
>
> ~~还有一条：`wslc-panel/src/main.rs` 顶部有一层跨 crate 垫片……~~
> **2026-10-26 起垫片已全部删掉**（`state` / `prefs` / `util` / `columns` /
> `listview` / `presets` / `sqlview` / `update` / `deploys` 九个）。
> 现在引用一律写全路径 `wslc_panel_core::state::…` —— "这个类型属于哪个 crate"
> 在调用点就看得见。删掉的原因：垫片必须 `pub use`，于是 core 里那些再导出
> 被迫写成 `pub`（`pub(crate)` 跨 crate 看不见，实测报 29 个 `E0432`）。
> ⚠️ 新代码**不要**再加垫片。
>
> 往里加东西之前仍先问一句"这能不能放 `wslc-panel-core`"，判断标准就是下面
> 那节（"这个文件 `use gpui` 吗"）。

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

### 用脚本拆大文件时的五个坑（2026-10-26 实测，每个都多花了一轮 CI）

拆 `state/tests.rs`、`app/shell/db.rs`、`model/{deploy,install,mysql}.rs` 都是脚本按
`fn` / 小节边界切分、再按能力分组。本机没有编译器，所以错一处就是一轮 CI（约 3 分钟）：

1. **`mod` 和 `pub use` 要成对检查** —— 漏一个 `mod types;` 的表现是
   `unresolved import types`，看上去像文件写坏了，其实是声明漏了。
2. **子模块深了一层，`super::` 要多一层** —— `app/shell/db/conn.rs` 里
   `Shell` 在 `super::super`，`prelude` 在 `super::super::super`。
3. **`include_str!` 的相对路径跟着深一层** —— `state/tests` 引用
   `wslc-core/tests/fixtures` 时是 `../../../../`（少一层就是"文件不存在"）。
4. **原文件里的私有 `use` 会被子模块继承** —— 内联的 `mod tests { use super::*; }`
   能看到父模块的 `use std::path::Path`；搬到独立文件后这层继承**没了**，要自己补。
   反过来，脚本按"名字出现过"补 import 很容易加多：**PowerShell 的 `-match`
   默认不区分大小写**，`Path` 会命中注释里的 "path"，多出来的会被 clippy
   抓成 `unused import`（实测一次 8 条）。
5. **拆出来的文件末尾不能留空行**，分节横幅被搬走后也别留双空行 ——
   rustfmt 两条都会报。
6. **私有 helper 被搬走后"兄弟模块还在用"** —— 原来同模块白拿，拆开就没了。
   查法：把 `fn ` 开头的**非 `pub`** 逐个 grep 调用点；要跨模块用的提成
   `pub(crate)` 并在 `mod.rs` 再导出（实测踩过 `is_name_char` / `validate`）。
7. **"只有测试按名字用"的 helper** 要三件套：`#[allow(dead_code)]`（库构建里没人调）
   + `pub(crate) fn` + `#[cfg(test)] pub(crate) use`（不 `cfg` 的话库构建报
   unused import）。历史上 `civil_from_days` 就是这么处理的（2026-10-27 改用
   `chrono` 后已删）。
8. **rustfmt 会把 `pub(crate) use` 混排进 `pub use` 列表**，别手写"先列 pub use、
   最后补 priv"的顺序：按路径排序，带属性的项在前
   （`#[cfg(test)] pub(crate) use xxx::helper;` 在 `pub use xxx::*;` **之前**）。
9. **机械改字段路径（`self.x` → `self.group.x`）要一次扫四种写法**，只替换
   `self.x` / `shell.x` 会漏 —— 实测 B8 那一轮漏了 **7 处断链**（rustfmt 把长链
   写成 `shell` ⏎ `.schema_search` ⏎ …，字段在**行首**），编译期 7 条 `E0609`。
   改完按这三条自扫（比等 CI 快得多）：
   - `^\s*\.字段名$` → 断链漏网，补一行 `.group` 在它前面；
   - 该行有字段、**下一行以 `.` 开头** → 链续行，rustfmt 要求 `self` /
     `.group` / `.字段` / `.方法()` 各占一行；
   - 全仓 `Line.Length -gt 100`，以及 **`chain_width`（默认 60）** ——
     行宽没超、链宽超了照样红（实测两轮都栽在这上面）。
10. **按行号区间替换时，区间要从"正文第一行"开始**，别把上一行的
    `let x = if cond {` 一起吃进去 —— 实测 B5b 那轮有三处因此变成
    `busy_text(...) } else {`（语法错）。做法：先从文件里读出那段原文、
    `Contains` 断言过再替换，而不是隔着屏幕数行号。
11. **把调用换进 helper 之后，回头查一眼导入** —— 某个名字唯一的调用点被 helper
    吃掉之后，它在顶部的 `use` 就变成未使用的（clippy 阻塞）。实测 B5b 的
    `deploy.rs` / `database.rs` 都留了 `log_text`。
12. **链变短也会触发 rustfmt**（反方向同样要动）：`.child(sidebar_toggle)` 换成
    更短的 `.child(toggle)` 之后，整条链短到能合并，rustfmt 要求
    `h_flex().gap_3().items_center().child(toggle)` 挤成一行、内层整体左移 4 格。
    判据永远是**行宽 100 + `chain_width` 60 两个都要重算**，不是"我加长了所以要看"。

> 判据：`core` job 的**测试条数不变**（纯搬迁不该动断言），`lint` 绿了才算收工。

---

## 3. CI 怎么读

`.github/workflows/ci.yml`，三个 job：

| job | 名字 | 干什么 | 耗时 |
|---|---|---|---|
| `core` | 纯逻辑测试（wslc-core + wslc-panel-core，无需 GPU） | `cargo test --workspace --exclude wslc-panel --locked --profile ci` | ~1 分钟 |
| `ui` | wslc-panel 编译检查（GPUI） | `cargo check --workspace --all-targets --locked` + `cargo check -p wslc-panel --bins --locked` | ~15 分钟（冷缓存） |
| `lint` | 格式与 clippy | rustfmt + clippy（**阻塞**，2026-10-25 起） | ~2 分钟 |

要点：

- **`core` 先绿不代表能过** —— GPUI 的 API 假设（异步闭包、`ButtonVariants`、
  `Styled` 方法名、`spawn_in` / `update_in`……）只有 `ui` 那个 job 真正编译过才算数。
  **UI 改动要盯 `ui`。**
- **`core` 用 `--workspace --exclude wslc-panel`，不写死包名** ——
  写死的话，往 workspace 里加第 4 个 crate 时它的测试**永远不会跑**而 CI 照样绿
  （而 §2 恰恰鼓励把纯逻辑搬进 `wslc-panel-core`，也就是鼓励加新东西）。
  `--exclude` 是为了不把 GPUI 链接回来（那是 763 秒的来源）。
- **真机冒烟（`wslc_smoke.rs`）在 CI 上跳过**：`core` 步骤带了
  `WSL_PANEL_SKIP_SMOKE=1`。runner 上**有** `wslc.exe` 但容器引擎起不来，
  那几条命令会各自等到 `DEFAULT_TIMEOUT`（30 秒），Windows 上杀卡住的子进程
  又不保证立刻生效 —— **实测 2026-10-26 把 core 从 1 分钟拖到 10 分钟以上**，
  看起来像代码坏了。本机跑 `cargo test` 不设这个变量，照旧真机冒烟。
- **所有 cargo 步骤都带 `--locked`** —— 本机没有 cargo，改了 `Cargo.toml`
  之后**无法在本地重新生成锁文件**；不带 `--locked`，cargo 会自己解析一份新依赖集
  继续跑，于是 CI 全绿而仓库里那份 `Cargo.lock` 永远是旧的：构建不可复现，
  release 会用一份**没记录在仓库里**的依赖集出包。
- `push` **只监听 `main`**；功能分支走 `pull_request`。所以「推分支 + 开 PR」
  不会跑两遍（这是刻意改的，见 `ci.yml` 顶部注释）。
  ⚠️ **但本仓库现在不开功能分支了**（见下面「工作流」那条）——
  `ci.yml` 的 `pull_request` 触发留着不影响，只是不再走那条路。
- 同一个分支连推两次会**取消上一次**（`concurrency`）。
- 纯文档 PR 会跳过（`paths-ignore: **.md`）。
- `--profile ci`（见根 `Cargo.toml`）是给**真的会 codegen** 的步骤用的：
  不优化、不带调试信息。**`cargo check` 不要加它** —— 白搭一份冷缓存。

### 工作流：**直接在 `main` 上开发，不开分支、不开 worktree**

2026-10-23 按用户要求清理过一次：本地 10 个功能分支、远端 44 个全删了，
只留 `main`（清洗前每个分支的 sha 存在
`.git/branch-archive-20261023.txt`，要找回就 `git branch <name> <sha>`）。

**为什么这么定**：这个仓库的开发者只有一个人，而 CI 的 `push` 只跑 `main` ——
功能分支要么等 PR（`ui` 那个 job 15 分钟），要么白等一轮。分支反而制造了
"这个改动在哪个分支上"的额外记忆负担。实测代价是真实的：清理时发现
10 个本地分支里有 6 个的改动**早就通过 squash merge 进了 main**，
只是 sha 不同 —— 那种分支留着只会让人怀疑"是不是有东西没合"。

**所以：**

- 直接 `git switch main` + 改 + 推 `main`；
- **不要 `git worktree add`** —— 这个仓库同时只有一个检出目录
  （`E:\cnb\wslc-panel`），加 worktree 只会让"哪个目录是当前状态"变得含糊；
- 要实验性的东西就**先在 main 上做完再推**，或者干脆开个临时分支、当天删掉。

⚠️ **别用 `git add --renormalize`** —— 见 §7.6 那条实测（会把假差异变成真差异）。

> ⚠️ **「CI 绿」这个信号覆盖：类型检查过 + 纯逻辑测试跑过 + 格式与 clippy 过了。**
> 它**仍然不**包含：MSRV（CI 用浮动 `stable`，`rust-version` 那个数字无人校验）、
> 以及界面到底渲染成什么样（§8）。
> 别把它当全能凭证。
>
> （`lint` 从 2026-10-25 起是**阻塞**的 —— 见 §3 的表格。它曾经是
> `continue-on-error`，代价是 37 条 clippy 从 v0.10 挂到 v0.17 没人清。）

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

⚠️ **还有一个不是你的错的红叉，别去查代码。**
`.github/workflows/fmt.yml` 用 `GITHUB_TOKEN` 推的那条 `style: cargo fmt`
提交，会留下一条 **`pull_request` 事件、零 job、`conclusion=failure`** 的运行
（它的 check suite 里一个 check run 都没有）。看上去像"格式提交把 CI 弄挂了"，
其实只是"事件创建了、但 GitHub 不为 bot 的推送执行 job"。

已复现两次：`fix/review-followups` 的 `e2b44aa` → run `38206351029`、
`feat/container-logs-exec` 的 `8e83dcc` → run `38214971652`，两次都是 0 job。

判据（两条都满足就无视它，或者对那次运行点 "Delete run"）：

```powershell
(Invoke-RestMethod -Headers $H -Uri "$api/actions/runs/<run-id>/jobs").jobs.Count   # 0
(Invoke-RestMethod -Headers $H -Uri "$api/check-suites/<suite-id>/check-runs").total_count  # 0
```

**别把它当成"格式提交需要再验一次"** —— 同一个提交重跑（close + reopen PR，
或者手动 `workflow_dispatch`）就是绿的。

⚠️ **机器人推的提交不会触发 CI，所以拿到 fmt 提交之后要自己补一次"空提交"。**
2026-10-26 实测：本机没有 rustfmt，一批改动推上去 `lint` 红在格式上，于是
`workflow_dispatch` 跑 `.github/workflows/fmt.yml`（`commit=true`），它推回
`style: cargo fmt` —— 那个提交的 `push` **不产生任何 job**，等于 **fmt 之后的代码
没有经过任何编译验证**。正确收尾：

```powershell
git fetch origin main && git merge --ff-only origin/main
git commit --allow-empty -m "chore: 触发 CI 确认（fmt 提交由机器人推送，其 push 不会跑 job）"
git push origin main
```

（仓库历史里的 `chore: 触发 CI 确认` 都是这么来的。）

### 用 PowerShell 调 REST API 的三个坑（都踩过）

1. **`/git/commits/{sha}` 只吃完整 SHA** —— 短 SHA 返回 404，而且重试六次
   还是 404（不是网络问题，别去查网络）。而 `/commits/{ref}` 吃短的、也吃分支名。
   两个端点别混用。
2. **PowerShell 会把多行输出和 JSON 数组"套成一层数组"**：
   `git log -1 --format=%B` 的多行结果、`Invoke-RestMethod` 返回的数组，
   拿到的 `.Count` 会是 1、`.message` 会是**数组** —— 拼进请求体后 API 回你
   `422 ... 'properties/message', [...] is not a string`。
   对策：多行文本 `| Out-String` 再 `.TrimEnd()`，单值用 `[string]` 强转，
   数组先判 `$x.Count -eq 1 -and $x[0] -is [array]` 再展开。
3. **`git rev-parse 'v1.2.3^{commit}'` 在 PowerShell 里会被拆坏**
   （报 `unknown switch 'e'`）。要判"这个标签在不在 main 的祖先链上"，
   用 `git merge-base --is-ancestor <tag> origin/main`（参数别带 `^{}`），
   或者 `git log -1 --format=%H <tag>` 拿到提交号再比。

---

## 4. 发布流程

1. **先让 CI 在 main 上全绿**（尤其是 `ui`）。
2. 升版本号（**三个文件、六行**，少一处 CI 就会红）：
   - 根 `Cargo.toml` 的 `[workspace.package] version` —— 四个成员都继承它
     （`wslc-core` 从 v0.20.1 起也用 `version.workspace = true`，不再是手改项）
   - **`Cargo.lock` 里四个 workspace 成员的 `version`** —— 锁文件记着成员版本，
     不改的话 `--locked` 直接报"锁文件与清单不一致"。
     ⚠️ **成员是四个，不是三个**：v0.16 加了 `wslc-db`
     （`wslc-core` / `wslc-db` / `wslc-panel` / `wslc-panel-core`）。
     漏掉其中任何一个的后果是**所有带 `--locked` 的步骤全红**（包括 release），
     而现场只报"锁文件与清单不一致"，很难联想到是漏了一个 crate。
     本机没有 cargo 只能手改。
     ⚠️ **但别对这个版本号做全局替换**：`Select-String '0\.17\.0'` 实测命中**五**处，
     只有四处在成员条目下，另一处是**第三方 crate** 恰好也叫这个版本
     （0.17.0 这轮是 `glow`；0.8.0 那轮是 `xkbcommon` 和 `y4m`）——
     一起改掉就是改坏锁文件。所以按 `name = "wslc-…"` + 紧跟一行的
     `version` **成对**替换。不放心就用 `gen-lock.yml` 重新生成。
   - `README.md` 里那行启动日志样例（`版本 x.y.z，构建 <短 sha>`）
3. 推 main → **等 CI 全绿** → 打 `vX.Y.Z` 标签 → 推标签。
4. `release.yml` 由 `push: tags: ["v*"]` 触发，编译并发布三个资产：
   `wslc-panel.exe`（绿色版）、`wslc-panel-vX.Y.Z-windows-x64.zip`（归档）、
   `wslc-panel-X.Y.Z-setup-x64.exe`（Inno Setup 安装包，只在 `v` 标签上编）。

   ⚠️ **安装包的名字里没有 `v`**（`wslc-panel-0.22.3-setup-x64.exe`），
   而 zip 里有（`wslc-panel-v0.22.3-windows-x64.zip`）—— 实测 v0.22.1 /
   v0.22.2 / v0.22.3 三版都是这样（`scripts/build-installer.ps1` 从 tag
   取版本号时去了 `v`）。**`README` 早先写成 `vX.Y.Z-setup`，是错的**，
   用户按那个名字去发布页会找不到文件；已改。

   ⚠️ **`wslc-panel.exe` 这个文件名是契约**：发布页、README、以及更新提示里
   指向的都是它。改名等于让所有存量用户的"去发布页下载"落空。
   安装包和 zip 可以改名，它不能。

   **版本号仍然只有那六行要改**（本节第 2 条）—— 安装包版本由
   `scripts/build-installer.ps1` 从 tag 读，`.iss` 里**不写死**，
   所以它**不是**第 5 个要手改版本号的文件。

**标签必须落在 main 的祖先链上。** 如果 `git push` 不通、改用 API 建提交，
本地和远端的 sha 会分叉 —— 那时候 `git tag` 打出来的是**本地**那个提交，
标签就不在 main 上了（发生过一次）。建完标签核对一下：

```powershell
# tag 解引用到的 commit，树应该和 main 的一样
(Invoke-RestMethod -Headers $H -Uri "$api/git/tags/<tag-object-sha>").object.sha
```

⚠️ **用 REST API 建标签**（`POST /git/tags` + `POST /git/refs`）**会**触发
`release.yml` —— **实测（v0.9.2）**：建完标签 3 秒内自己起了一个
`event=push`、`head_sha` = 被打标签那个提交的 run（`38714331577`），
Release 与两个资产都是它建出来的。
（这里早先写的是"只发 `create` 事件、不会触发"—— 那条**是错的**，实测推翻。）

后果是**同一个标签上会并行跑两个 run**：push 那个 + 你手动
`workflow_dispatch` 那个（`38714331433`，晚 1 秒启动）。后者 `gh release view`
时 Release 还没出现，接着 `gh release create` 就撞
`422 ... Release.tag_name already exists` 而**把 job 跑红** ——
**发布本身是好的，红叉是假的**。
（`release.yml` 里已经补了"创建失败 → 退到覆盖资产"的兜底，见那段注释。）

**所以正确姿势是**：

1. 建完标签**先等一分钟**，看 `GET /actions/workflows/release.yml/runs`
   有没有自己起一个 `push` 事件的 run；
2. **有就别再 dispatch** —— 等它跑完即可；
3. 只有确实一个 run 都没起时，才走 `workflow_dispatch` +
   `release_tag=vX.Y.Z` 那条为"git push 不通"留的路径。

⚠️ Release 的说明是**固定模板**（`release.yml` 里那段 `$notes`），**没有 changelog**。
想让下载页显示"本版变化"，得在发布之后 `PATCH /repos/{o}/{r}/releases/{id}` 把正文补上
（重跑工作流会用模板覆盖它）。发布页是用户唯一会读的地方，值得补。

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

⚠️ **别用 `git add -A` / `git add .`** —— 这个工作区里经常躺着临时文件
（写提交信息的 `.tmp-msg*.txt`、走 API 的 `.tmp-*.ps1`、`.tmp-readme.md`），
`-A` 会把它们一起提交进去。**实测踩过一次**：`.tmp-msg7.txt` 就这么进了 main
（好在不影响构建与 release 产物 —— zip 里只有 exe + README + docs），
但仓库里凭空多一个文件，事后还得专门提交删掉。
提交时列具体路径，或者先 `git status --short` 看一眼再 `add`。

### 界面：同一个动作只给一个入口

列表行放**高频**动作，详情弹窗放**低频 / 危险**的 —— 同一个动作**不要两边都摆**。

理由不是"省地方"：两个入口背后是同一个 `ImmediateAction` / `PendingAction`，
行为完全一样，用户却要判断该点哪个。（v0.5.11 实机踩到：停止的容器在列表行和
详情里各有一份一模一样的「启动 / 日志 / 删除」。根因是一条注释写着
"照 1Panel：列表里不放操作按钮"，而本项目的列表行**一直有**按钮 ——
**注释和实现不一致时，后来的人会照着注释继续加重复**。）

推论：如果详情里某个状态下**一个动作都没有**（比如停止的容器），要写一句话
说明其余动作在哪儿，别让用户以为按钮没画出来。

### 界面：文案里的 Markdown 要在**渲染处**清掉

这个仓库的文案都是 Markdown 风格写的（`**加粗**`、反引号），而 `div` / `Button`
画的是**纯文本** —— 不过一遍 `util::strip_md`，屏幕上就真的显示 `**` 和反引号。

规矩：**凡是把字面量直接 `.child("…")` 到 `div` 上的地方，都过 `strip_md`**。
（提示条不用手写 —— `toast` 的渲染处已经统一清了；见 `views::common` 里那一处。）

⚠️ v0.9.13 审查时一次补了 **15 处** `strip_md` 调用（部署页 9 处 + 更早的版本留下的
6 处：数据库「连接」卡、备份浮层的说明、拉取弹窗的报错说明、设置页「终端」两段、
`image_facts_hint`）。`strip_md` 的意义是**守卫**而不是一次性修补 —— 新加文案时
看一眼渲染处。

⚠️ **又漏了 5 处**（v0.10.3 审查）：`plan.notes`（添加实例页的步骤说明，
`install.rs` 里那 7 条 notes 全带标记）、终端卡的**第一段**、`.wslconfig`
「放错文件的键」那句 `why`（在 `wslc-core/src/wslconfig.rs` 里）、
`image_env_hint` 的第二支、日志浮层的空状态提示。

这一轮暴露的是**机制问题**而不是手滑：这些文案散在三个 crate 里，
而 `wslc-panel` 的渲染**没有任何测试**（`views/` 不在任何测试目标里），
所以"哪一处又漏了"只能靠人肉扫。要根治就得让文案集中在
`wslc-panel-core`（那里有 `util::strip_md` 和它的测试），
或者加一条"所有会直接 `.child()` 的文案经 `strip_md` 后不含 `**`/反引号"的遍历断言。
在那之前，**改文案时按本节这条规矩逐个看渲染处**。

### 界面：列表行按钮超过 4 个就收进「更多」菜单

行内只留**这一类东西专属的入口**（比如「数据库」「部署管理」—— 只能从这一行进、
不在侧边栏里）+ **最高频的状态切换**（启动 / 停止），其余（含危险动作）收进
`Button::dropdown_menu(...)` 的下拉菜单里。

```rust
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};  // 扩展 trait，必须显式导入
Button::new(id).label("更多").small()
    .dropdown_menu(|menu, _window, _cx| {
        menu.item(PopupMenuItem::new("日志").on_click(move |_, _, cx| { … }))
    })
```

- `menu::init(cx)` 已经由 `gpui_component::init` 调过（我们 `main.rs` 调的
  `gpui_kit::init` 覆盖了它），**不用自己初始化**；
- 菜单项点完**自动关**（`PopupMenu` 调完 handler 自己 `dismiss`），回调里别手动关；
- 菜单走 popover 层（`deferred` + `POPUP_PRIORITY`），所以**表格的 `overflow_hidden`
  裁不掉它** —— 这是"行里能弹菜单"的前提；
- ⚠️ 行内少了按钮之后，**"其余动作在哪儿"的那几句文案必须跟着改**，
  否则用户按老习惯去行内找会以为功能没了。

⚠️ **v0.9.13→v0.9.14 的教训**：容器行铺到 7 个按钮时，我先把 `ALL_COLUMNS` 的操作列
从 330 加到 440 想"装下它们" —— 实机截图里那一列**还是两行**。
**"列太窄"有时候不是列的问题，是按钮太多。** 加第 5 个按钮之前先问一句
"它该不该进「更多」"。

### 组件选型：这几个**评估过、不换**（别再论证一遍）

"官方组件有就用"是默认方向，但下面四个官方组件核过源码之后**决定不用**。
每一条都有代价，写在这里免得下一轮重新走一遍：

| 官方组件 | 我们现在用什么 | 为什么不换 |
|---|---|---|
| `group_box::GroupBox` | `common::card()`（**22 处调用** + 1 处定义，审查时点的） | `title_style` / `content_style` 收的是 `StyleRefinement`，要构造它得先建个 `div`、链式设完再从 `style()` 取（很别扭）；而且 `GroupBox::render` 在内容外**多套一层**（`gap_2`），这 22 张卡的间距会整体变。颜色（`bg_card` / `border`）还得逐项覆盖。收益只剩"标题字号跟组件主题走" |
| `separator::Separator` | `border_b_1()`（**4 处**） | `horizontal()` 的实现是"容器高度 0 + 绝对定位的 1px 线"，替换掉 `border` 会让布局**少 1px**，且在滚动容器里贴不到正确位置。4 处 border 不值得 |
| `skeleton::Skeleton` | "正在读取…" / 官方 `Spinner` | 本项目的等待大多**有进度**（安装 / 导出 / 导入）；真正没进度的只有"读表结构、查 registry tag"两处，而它们已经有 `Spinner`（`db_loading`）。铺 Skeleton 要先为每块布局造形状，收益是"感知性能"，代价是每处都要维护假盒子 |
| `badge::Badge` | `Tag::custom`（`common::badge`） | `Badge` 是**数字 / 红点角标**（`.count()` / `.dot()`），不是文字胶囊。文字胶囊的官方组件就是 `Tag`，而 `common::badge` 用的正是 `Tag::custom` —— 已经是官方用法 |

> 判断标准不是"有没有官方组件"，而是"换过去之后**行为/观感/维护成本**里
> 至少一项明确变好"。只变"更像官方的写法"不算。
>
> ⚠️ 这张表里的**数字是会变的**（`card()` 从 17 涨到 22 就是加页面加出来的）。
> 数字只是"规模"的说明，**论据是后半列的代价**；重跑一遍数字发现对不上时，
> 先改数字、别删论据 —— 除非某个代价真的消失了，那才该重新论证。

### 计划文档

`docs/SPIKE.md` 是前期技术验证，`docs/wslc-schema.md` 是实测字段字典，
`docs/releases/` 是逐版本发布说明。**改行为时顺手更新它们。**

> 各阶段的 `docs/PLAN-*.md` / `docs/AUDIT-*.md` / `docs/COMPARISON-references.md`
> 已在 2026-10-27 删掉 —— 现状以本文件 + `README.md` 为准，历史事实从 git 里翻
> （`git log -- docs/`）。别再往这些路径加新引用。

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
- **"即时反馈"不只等于 toast**：浮层里的按钮换成「加载中…」/「执行中…」同样算
  （日志浮层、exec 浮层就是这么做的 —— 那儿不发 toast，因为结果本来就填在
  同一个浮层里）。关键是"从点击到有结果之间，界面上必须有变化"；
- 真的只是"把进程拉起来、不等待"的才可以同步：
  `spawn_in_new_console`（`CREATE_NEW_CONSOLE` + `spawn()`）、
  `explorer.exe`、`spawn_detached`。
- ⚠️ **`cx.spawn(…)` 的 future 是在界面线程上被 poll 的**，所以它**不是**后台
  执行器：在它里面直接调 `wslc.run(…)` / `wsl.exec(…)` / `std::fs::*` 一样会卡住
  界面。要**再套一层** `cx.background_executor().spawn(async move { … }).await`
  —— 项目里 40+ 处后台任务都是这个形状。
  **实测漏网过一次**：`Shell::delete_db_backup` 把"删文件 + 进容器 `rm -f`"直接
  写在 `cx.spawn` 的 async 块里，点「删除」会卡一下（v0.9.15 修）。
  查这个的字面判据：`wslc.run(` / `wsl.run(` **上面十几行**里有没有
  `background_executor`（全仓库现在只剩注释里的例子）。

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
| **`wsl -d X` 会继承调用者的 cwd**（实测于 WSL 3.0.3.0） | 面板自己不设 cwd，所以"打开终端"落在哪儿 = **用户从哪个文件夹启动的面板**。实测：从 `C:\Users\76434\Downloads` 启动 → 终端落在 `/mnt/c/Users/76434/Downloads`；从 `E:\cnb\wslc-panel` 启动 → `/mnt/e/cnb/wslc-panel`。翻译不了的路径（UNC、未挂载的盘、从 WSL 里起 exe）回落 `~`。**要固定起点就得显式写 `--cd`** |
| **`--cd` 才是指定终端起点的开关**（实测于 WSL 3.0.3.0） | `wsl -d X --cd ~` → `/home/wt`；`--cd ~/tmp` 会把 `~` 展开；**`--cd` 指向不存在的目录不报错**，只在 stderr 留一行 `chdir(...) failed 2` 然后落在 `/`（所以写错的路径不会"点了没反应"，但也别指望它提示）；`--cd D:\wsl` 也会被翻译成 `/mnt/d/wsl`（我们还是只让用户填 Linux 路径 —— 两个坐标系之间猜最容易出错）。面板里由偏好 `terminal_dir` 决定，见 `wslc_panel_core::prefs::TerminalStartDir` |
| `wslc exec -i -t` **有 `-w/--workdir`** | 容器里的起点可以指定（`wslc exec --help`），但容器里**没有**"Linux home"这个概念 —— `~` 是谁由镜像决定，面板不往下传，见 `TerminalStartDir::container_workdir` |

### 7.4 gpui-kit（0.8.0）没有文件选择器

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
  （`crates/wslc-panel/src/app/shell/render.rs` 的 `app-body-scroll` 那段注释也说了这件事。）
- `InputState::new` 要 `&mut Window` → 表单只能**懒创建**（点击时建）。
  渲染时建会每帧重建输入框，**字都打不进去**。
- 输入框的焦点在打开时用 `window.focus(&handle, cx)` 给过去，
  `handle` 来自公开的 `InputState::focus_handle(cx)`（`InputState::focus` 是 `pub(crate)`）。
- 全项目**不用 `.disabled()`**（不导入 `Disableable`）——
  要"不可用"就用条件不渲染那个按钮，或者给一句说明。
- **模态遮罩必须挂 `.occlude()`** —— 一个只画底色的 `div` **不参与命中测试**
  （GPUI 只给注册过交互的元素建 hitbox），于是遮罩开着的时候点击会**穿到背后的
  页面上**。v0.5.10 时 8 个弹窗全有这个问题（实机：打开「创建容器」还能点到它
  背后那一列的按钮，而那个动作会重建表单、把用户打的字清掉）。
  ⚠️ **v0.19 起浮层全走官方 `Dialog`**（窗口级栈），遮罩 / `.occlude()` /
  点遮罩关闭都是组件的事（`overlay` / `overlay_closable`）—— **项目里已经没有
  自绘模态**（`modal_scrim` / `modal_card` 已删）。这条只在你真要写一个**不进栈**的
  自定义浮层时才适用，那种情况先问一句"它为什么不该进栈"。
- 官方 `Dialog` 的遮罩是可点的（`overlay_closable`，默认 `true`）——
  **有提交动作的那几个都显式设了 `false`**
  （创建容器 / 数据库动作 / 添加部署 / 站点配置 / 拉取镜像 / 进度类）：
  误点遮罩丢掉填了一半的表单，比"关不掉"更烦。
  只读浮层（日志 / 详情 / exec / 备份列表 / 基础信息）保留 `true`，点外面关掉很自然。
- ✅ **浮层一律走官方 `Dialog`（窗口级栈），层序 = 栈序**（v0.19 起，**19/19 迁完**）。
  打开处调 `window.open_dialog(cx, |dialog, _, _| …)`，构造集中住在
  `app/shell/dialogs.rs` 与 `dialogs/specs.rs`；内容由 `views::*_body` 产出、外面包 `common::ShellView`
  （它 `observe` 了 `Shell`，否则内容会僵在打开那一刻）。

  **这一条根治了下面那个老坑**：`Dialog` 由 `Root` 统一渲染在窗口级的
  `active_dialogs` 里，"从详情里打开的确认框盖在详情之上"是**结构保证**，
  不再依赖谁记得把 `.child(…)` 排在后面。手写链子会忘，栈不会。

  ⚠️ **仍有两个必须知道的 `Dialog` 行为**（都是实机踩出来的，v0.19.0）：
  1. **builder 闭包只在"对话框层被重建"时重跑** —— 打字、后台任务推进都**不**
     触发它。所以任何"内容会变"的浮层（日志 / 进度 / exec 输出 / 输入预览），
     内容必须包在 `ShellView`（或 `PromptContent` 那种 observe 输入框的子实体）里，
     否则会僵在打开那一刻；
  2. **普通 `Dialog` 不会自动画 OK / Cancel** —— 那是 `AlertDialog` 的行为。
     `DialogButtonProps` 的 `render_ok` / `render_cancel` 只有 `AlertDialog` 调，
     `Dialog::render` 只看 `self.footer`，而 `show_cancel` 默认 `false`。
     底部按钮必须自己 `.footer(modal_footer([...]))`。
  3. ⚠️ **关闭时"清状态"必须自己写在每一条退出路径上。**
     `sync_dialog_stack` 是**按状态对齐**的：每次 `Shell::render` 都算一遍
     "现在应该开哪些对话框"，和 `dialog_stack` 比完补开 / 补关。
     所以**只把 `Dialog` 关掉、状态不清，下一帧就把它重新打开。**

     `Dialog` 的退出路径有**三条**，`overlay_closable` / `keyboard` 只管
     "允不允许"，**清状态仍然得自己写在回调里**：

     | 退出方式 | 要写在哪 |
     |---|---|
     | 底部按钮 | 按钮的 `.on_click`（`window.close_dialog(cx)` 之后 / 之前都行，但要成对） |
     | **Esc** | `.on_cancel(…)` —— 漏了就是"Esc 关不掉"，或者"关了又回来" |
     | **点遮罩** | 也走 `.on_cancel`（`overlay_closable(true)` 时才有这条路径） |

     ⚠️ **还有第四条（2026-10-26 补）：关掉窗口那一层，必须同时把
     `dialog_stack` 的记账去掉。**

     `window.close_dialog()` 只动窗口的 `active_dialogs`，而 `dialog_stack` 是我们
     自己的账 —— `sync_dialog_stack` 正是靠它对齐"该开哪些"。少了记账，下一帧
     sync 会认为那一层还开着，于是**再关一层**。

     三个关闭入口里**有两个不是我们关的**（读 `gpui-base` 0.8.0 的 `Cancel` 分支
     确认：`on_cancel` 返回 `true` 之后，由组件自己 `request_close` →
     `window.close_dialog`）：页脚按钮走 `close_top_dialog`、Esc / 点遮罩 /
     标题栏那个 ✕ 走 `on_cancel` 里的 `forget_top_dialog`。

     **v0.21.0 实机**（用户报的）：实例详情里点「压缩磁盘」→ 点「取消」→
     **详情跟着一起消失**；更糟的是账里留下一条幽灵记录，
     `desired == dialog_stack` 让 sync **永远早退** —— 详情再也点不出来，
     再开别的浮层还会走"非前缀 → 全关重建"把它顶回最上面。

     ⚠️ **没有别的兜底，别指望对齐时能发现。** 官方只暴露
     `has_active_dialog`（bool），**查不到层数**，所以"少记一层"sync 自己看不出来。
     （曾经加过一条"窗口里空了就清账"的自愈，但它只兜得住"全关光了"这一档，
     既验证不了、又容易掩盖以后真漏改的地方 —— 删掉了。）
     换句话说：**新加一条关闭路径 = 必须自己记一次账**，否则就是下一轮的这个 bug。

     **实测踩过（v0.20.0 → v0.20.1）**：`WslConf` 设了
     `overlay_closable(false)` 却**没有** `.on_cancel`（点遮罩、Esc 都关不掉，
     只剩「取消」一个出口）；`DeployRemoveConfirm` 设了
     `overlay_closable(true)` 也**没有** `.on_cancel`（点遮罩对话框关了、
     `confirm_remove` 还留着，下一帧又弹回来）。用户的描述是
     **"点其他位置也会弹出这个弹窗"** —— 点任何地方都会触发一次重绘，
     光看现象很难想到是"状态没清"。

     配套的两条经验：
     - 写新臂时可以拿这一段当清单：`dialog_stack` 里每个 `DialogKey`
       都要能回答"它被关掉时，是谁把对应状态清成 `None` 的"；
     - 只读浮层（日志 / 详情 / exec / 备份列表）**也**要 `.on_cancel` ——
       它们同样要清状态，只是清的那个字段不影响任务本身。

     ⚠️ **提交型浮层的页脚不能无条件关窗**（2026-10-28 审查发现的，四个臂里
     只有一个写对了）。判据是**状态清掉了没有**，不是"提交成功没有"（那一刻
     后台任务还没跑，拿不到结果）：

     ```rust
     let closed = entity.update(cx, |shell, cx| {
         shell.submit_deploy(window, cx);        // 校验失败时它**故意不清**状态
         shell.forms.deploy_form.is_none()
     });
     close_top_dialog_if_closed(&entity, closed, window, cx);
     ```

     提交型浮层在校验失败时**故意不关**（把焦点送回那一格），而窗口无条件
     关掉的话：下一帧 `sync_dialog_stack` 按状态把它原样打开 —— 用户看到
     "点了一下、弹窗闪一下"，刚被送过去的焦点也丢了。判据统一走
     `dialogs.rs` 的 `close_top_dialog_if_closed`，别再手写 `if closed`。

  **19 个浮层全部在栈里**，所以 `Shell::render` 末尾那条 `.child()` 链子上
  已经**不挂任何浮层**。加新浮层只有三处要动：

  1. `app/shell/dialogs.rs` 的 `DialogKey` 变体（枚举）**和** `DialogKey::ALL`
     那张表（**顺序 = 层序**，插在哪就是第几层）**和** `DialogKey::is_open`
     的臂（"这一帧该不该开"的**唯一判据**）；
  2. `dialogs/specs.rs` 的 `open_dialog_for` 臂（怎么开：标题 / 宽度 /
     `overlay_closable` / `keyboard` / `close` / footer）；
  3. 那个状态的**三条退出路径**（页脚按钮 / Esc / 点遮罩）都要清状态 + 记账
     （见上一节那张表）。

  ⚠️ **`sync_dialog_stack` 里没有 `if`** —— 它是"按 `DialogKey::ALL` +
  `is_open` 算一遍 desired，再和 `dialog_stack` 求最长公共前缀"的通用算法。
  （这一条原文写的是"`sync_dialog_stack` 的 `if`"，那是 v0.21 之前的样子：
  17 个手写顺序 `if`，层序只藏在书写顺序里。改成表之后，加浮层不需要动
  sync —— 但它**仍然要动 `ALL` 的顺序**，而这件事只有那张表能表达。）

  **不要**再往 render 的链子上挂东西。

  > 留下的历史（**别再犯**）：v0.9.14 及以前通用二次确认挂在链子的**最前面**，
  > 于是从详情里点「压缩磁盘」时，确认框连着遮罩一起被详情盖住 ——
  > 实机原话"压缩磁盘的对话确认在详情之下"。这就是"层序靠人工排序"的代价。
- **模态里放 `Select`（可搜索下拉框）**：v0.6.3 起「拉取镜像」弹窗里有 tag
  下拉框，这是本仓库**第一次**把 `Select` 放进模态。看实现是安全的
  （弹层走 `deferred` + `POPUP_PRIORITY`，遮罩只有 `BlockMouse`，见上一条），
  但 ⚠️ **这一条是推的，没有实机验证过**。
  真出问题（弹层被遮罩盖住 / 点不到）时的退路有两条：把下拉框换回
  "输入框 + 列表"，或者把表单从弹窗移回页面。
- **入场动画走 `with_animation`，id 必须"稳定 + 互不相同"**：切页有 120ms 淡入
  （浮层自己的入场动画由官方 `Dialog` 负责，我们不再手写遮罩动画）。
  id 是 GPUI 记住**播放进度**的钥匙 —— 同一个弹窗的多次重绘必须同 id
  （否则每帧重播），不同弹窗必须不同 id（否则两条动画共用一份进度）。
  切页淡入索性用页面标签当 id（换页即换 id → 重新播一次）。
  ⚠️ 动画要挂在 `div()` 上：`AnyElement` 不实现 `Styled`，`.opacity()` 在那儿
  不存在（`AnyElement` 只能当孩子，不能被继续修饰）。
- **自己画的 `div` 可点元素必须手写 hover/active**：组件库的 `Button` 自带
  （色值来自主题的 `button_*` / `button_primary_*`），但侧边栏导航项、
  容器名 / 发行版名链接是我们自己画的 —— 只挂 `cursor_pointer()` 的话鼠标移
  上去**毫无反应**（v0.6.5 实机反馈"没有点击的感觉"）。
  `.hover()` 来自 `InteractiveElement`、`.active()` 来自
  `StatefulInteractiveElement`（**所以元素必须先有 `.id()`**），两者签名都是
  `FnOnce(StyleRefinement) -> StyleRefinement` —— 和组件库里的写法一致。
  另外：组件库的 `Button` 只在 link/text 变体给 `cursor_pointer`，
  普通按钮悬停是**箭头**光标（刻意的，跟着 shadcn 那套），别当成 bug。
- **给"会被截断的文字"加 tooltip：用 gpui 原生的 `.tooltip(..)`**：
  `div().id(..).tooltip(|window, cx| Tooltip::new("…").build(window, cx))` 就行，
  **不用**借 `Button`。表格里会截断的单元格走
  `views::cell_muted_full(key, text)` / `cell_text_full(...)`，内部是
  `common::cell_text_with_tip`（`div` + `.id()` + 原生 tooltip）。
  ⚠️ 两条前提：① 元素**必须有 `.id()`**（`.tooltip` 挂在 `Stateful` 那一层）；
  ② `key` 必须**当帧唯一**（GPUI 的 hover 状态按元素 id 记），调用点传的是
  容器名 / 镜像引用 / 卷名 / 发行版名这类本来就唯一的标识。
  这条能成立还有个前提：我们用 `gpui_kit::open_window` 开窗，它内部包了
  `base::Root`（tooltip 浮层挂在 Root 的 window state 上）—— 换成裸
  `cx.open_window` 就**没有** tooltip 了。
  > 历史：v0.20.1 及以前这里写的是"tooltip 只能借组件库的控件，因为
  > `gpui::InteractiveElement` 没有 tooltip 方法"。那条在 0.8.0 **已经不成立**
  > —— gpui 原生就有 `.tooltip()`，`TooltipOverlay` 的文档还专门声明它不管这个 API。
  > （只查 `gpui_component` 的模块列表会漏掉它：`.tooltip()` 来自 gpui 本身。）
- **`Select` 的下拉菜单默认和触发器一样宽**（`menu_width` 默认 `Length::Auto`），
  所以长候选会被省略号截断 —— 要放得下就显式 `.menu_width(px(…))`。
  另外 `.placeholder()` 是"**还没选**"的文案，**空状态**另写 `.empty(…)`：
  v0.6.5 把空状态文案写进了 placeholder，结果展开明明有候选、
  触发器却写着"本地还没有镜像"（实机截图里一眼就看得出来）。
- **预填的值"留空"等于没填，必须在提交时拦一下**：v0.6.8 起「创建容器」按镜像
  预设预填環境变量（mysql → `MYSQL_ROOT_PASSWORD=`）与数据卷
  （`mysqldata:/var/lib/mysql`），但**只预填变量名** —— 密码这种值我们猜不出来，
  随便填一个反而让用户以为配好了。代价就是"看着填了、其实空着"，所以
  `confirm_create` 里必须检查（`model::prefilled_env_left_empty`）并把焦点送到
  那一格。**空值等于没填**这条对任何"我们代填"的东西都成立。
  ⚠️ 拦的范围要克制：只拦"我们预填的那个还留着空值"，用户把它删掉、或换成同一组
  里另一个合法选项（`MYSQL_ALLOW_EMPTY_PASSWORD=yes` / `POSTGRES_HOST_AUTH_METHOD=trust`）
  都放行 —— 他可能在复用一份**已经有数据**的卷，那种情况下这些变量本来就不需要。
  镜像预设表在 `model::image_preset`：**认不出来就什么都不做**（不预填、不拦），
  表里只有 mysql / mariadb / postgres / mongo / redis / nginx 这几个。

### 7.6 网络：`github.com` 经常不通 —— 先试 HTTP/1.1，再退到 API

- `github.com:443` 时常连不上（`Connection was reset` / 超时 **21 秒**起，
  重试几次就是一分钟）—— `git fetch` / `git push` 基本不可指望；
- `api.github.com` 通常通；
- **`gitee.com` 通**（参考项目就是从那儿 clone 的）；
- 系统代理（`127.0.0.1:31181`）可能**中途消失**，别写死依赖它。

#### ⚠️ 先试这条：`-c http.version=HTTP/1.1`

**实测（2026-10-23，v0.10.4 发布后对齐仓库时）：**

| 命令 | 结果 |
|---|---|
| `git fetch --tags --prune` | ❌ **427 秒**后 `RPC failed; curl 56 Recv failure: Connection was reset` → `fatal: early EOF` |
| `git fetch --depth=1 origin main` | ❌ **86 秒**后同样失败 |
| `git ls-remote origin` | ✅ **1.5 秒**（169 行，main + 全部标签） |
| `git -c http.version=HTTP/1.1 fetch --no-tags --depth=1 origin main` | ✅ **2.9 秒** |
| `git -c http.version=HTTP/1.1 fetch --tags --prune --unshallow origin` | ✅ **2.4 秒**（全历史 + 全标签 + 全分支） |

**判据**：`ls-remote` 通而 `fetch` 不通 —— 两者**走的不是同一条数据通道**：
`ls-remote` 只要 ref 广播（小），`fetch` 要 pack 传输（大）。
所以这不是"网络不通"，也不是仓库大（本仓库才 8.6 MB / 221 提交），
`--depth=1` 一样挂。

**根因**：git 默认用 **HTTP/2**，而链路上的中间设备会掐 h2 的数据流 ——
表现就是"ref 拿到了、对象传一半被重置"。强制 HTTP/1.1 后一律秒级。

**所以对齐仓库的正确顺序是**（别再一上来就跟 API 绕）：

```powershell
# 1) 先确认是哪种不通
git ls-remote origin refs/heads/main          # 通 → 是 h2 的问题，走第 2 步

# 2) 用 HTTP/1.1 拉全
git -c http.version=HTTP/1.1 fetch --tags --prune --unshallow origin
git reset --hard origin/main

# 3) 只有 HTTP/1.1 也失败时，才退到下面的 API 路径
```

⚠️ **`git reset --hard` 之前先确认工作区内容已经在远端**（`scripts/gh-api-pull.ps1`
的 DryRun 或逐文件 `hash-object` 比一遍）。本地 commits 与远端分叉时直接 reset
会丢提交；而"工作区内容一致、只是本地索引落后"那种情况才是可以安全 reset 的
（v0.10.4 那次就是这样）。

⚠️ **别用 `git add --renormalize`** 去"修"那些显示为 `M` 的文件。实测踩过：
本仓库 `.gitattributes` 是 `* text=auto eol=lf` + `*.ps1/*.iss/*.rc/*.isl eol=crlf`，
仓库里那些 blob **本来就是 CRLF**（属性生效前提交的）。`--renormalize` 会把它们
归一化成 LF 写进索引 —— **从"假差异"变成真差异**，还得 `reset --hard` 才回得来。
判"这些 M 是不是假的"用逐字节 + 归一化比对（见本节末尾那个脚本），别看 `git diff`。

#### 退路：`scripts/` 下这两个脚本（只走 `api.github.com`）

**别跟 `git` 较劲时用它们**（token 从 `git credential` 取，脚本里不存任何密钥）：

| 脚本 | 干什么 |
|---|---|
| `scripts/gh-api-pull.ps1` | 把远端分支的**内容**同步到本地工作区（`-DryRun` 先干跑） |
| `scripts/gh-api-push.ps1` | 把本地若干文件推成一个提交（`-Branch` 可开新分支，`-DryRun` 可预览） |

- **拉取**：`GET /git/ref/heads/<ref>` → `GET /git/trees/<tree>?recursive=1` 拿每个
  blob 的 sha；本地用 `git hash-object --no-filters`（**纯本地操作，不走网络**）算同名
  文件的 sha，**只下载对不上的那几个**，按字节写回。
  ⚠️ 它只同步**内容**、不动 git 历史：本地 `git log` 仍然落后，`git status` 会显示
  这些文件"被改过"。网络通了按上面那三步
  （`http.version=HTTP/1.1` + `fetch --unshallow` + `reset --hard`）彻底对齐。
- **推送**：`POST /git/blobs` → `POST /git/trees`（`base_tree` = 基线分支那棵树，
  只替换指定文件）→ `POST /git/commits`（父提交 = 基线 head）→
  `PATCH|POST /git/refs/heads/<branch>`。两道保险：**拒绝 `.tmp*` 路径**
  （§5 那次教训），以及提交后**自查**（用 `GET /commits/<sha>` 看实际改了哪些文件，
  和 `-Paths` 对不上会告警）。

⚠️ 手写这条路也得知其所以然（脚本就是照它写的）：

```
POST /git/blobs          { content: <base64 of the local file>, encoding: "base64" }
POST /git/trees          { base_tree: <基线分支的 tree>, tree: [{path, mode:"100644", type:"blob", sha}] }
POST /git/commits        { message, tree, parents: [<基线 head 的 sha>] }
PATCH /git/refs/heads/X  { sha: <新 commit>, force: true }
```

标签同理：`POST /git/tags` 建 tag 对象 → `POST /git/refs` 建 `refs/tags/vX.Y.Z`。
⚠️ **这样建的标签会触发 `release.yml`**（本节早先写"不会"，**实测推翻**，
细节与正确姿势见 §4 的那一段）。

⚠️ **用 API 建的提交和本地提交 sha 不一样**（作者/提交时间不同）——
对 sha 时认**远端**那个，别拿本地的比。

### 7.7 更新提示：只检查，不下载（本版的产品决定）

「应用设置 → 关于」里那张更新卡**只发一个 GET**：
`api.github.com/repos/SilkKirk/wslc-panel/releases/latest`。
查到新版本就提示一句 + 给一个「打开发布页」。

**它不下载、不校验、不替换 exe。** 两个理由：

1. **本机实测吞吐只有 11–15 KB/s**（30 MB 的 exe 要 40 分钟），
   在应用内做个半吊子下载器只会比"让用户用加速站"更糟；
2. 用户明确要的就是**提示**（"我自己找加速下载"）。

实现分层（改的时候别搞混）：

| 干什么 | 在哪儿 |
|---|---|
| 版本解析 / 比较、GitHub 响应解析 | `wslc-core/src/update.rs`（纯逻辑 + 真 fixture） |
| 起 `curl.exe` | `wslc-core/src/cmd/update.rs`（**阻塞**，必须走 `background_executor`） |
| 节流判据 / 提示判据 / 卡片文案 | `wslc-panel-core/src/update.rs`（纯逻辑，`core` job 能测） |
| 动作与状态迁移 | `crates/wslc-panel/src/app/shell/update.rs`（`check_update` / `apply_update_check`） |

⚠️ **自动检查失败必须静默**（只写 `warn` 日志，卡片回 `Idle`）——
这是个本地工具，每天开面板弹一句"检查更新失败"比不检查更糟。
只有**手动**点「检查更新」时才把错误显示出来。

⚠️ **24 小时节流不是优化，是配额保护**：未认证 GitHub API 是
**60 次/小时/IP**，而且**304 也算一次**（实测 remaining 52→51）。
共享出口（公司 NAT / 校园网）很容易被别人用光。

#### 将来真要"自更新"，地基已经测过了（别重新调研）

| 实验 | 结果 |
|---|---|
| 覆盖一个**正在运行**的 exe | ❌ 拒绝访问 |
| **改名**正在运行的 exe（`ping`/`cmd`/`powershell` 三份拷贝都试过） | ✅ 成功 |
| 改完名后把新文件放到**原路径** | ✅ 成功 |
| 边跑边删那个已改名的旧文件 | ❌ 拒绝访问；**进程退出后可删** |
| 改名后 `GetModuleFileName` / `process.execPath` / `MainModule.FileName` | ⚠️ **仍返回改名前的路径** |

最后一条最反直觉，也最容易写错：**替换之后 `current_exe()` 指着一个
已经不存在的路径**。所以要自更新，必须在**替换之前**把 exe 路径缓存下来
（启动时取一次），重启时不能用 `current_exe()`。

另外实测**新旧两个版本可以同时运行** —— 所以换完文件重启时要有交接
（新进程先等旧进程 PID 消失再开始轮询 `wslc`），否则会有一瞬间
两个面板各自 3 秒轮询 —— 那正是 `wslc` sharing violation 的成因。

完整方案（含三步替换、回滚、失败路径）当时记在 `docs/PLAN-install-update.md` 里（那份文档 2026-10-27 已删）。

---

## 8. 验证的边界：CI 查不出来的东西

CI 只能证明**编译过、纯逻辑测试过**。下面这些**必须在本机点一遍**：

- 窗口是不是真的渲染出来了、布局有没有错位；
- 文件选择器的对话框**弹没弹出来、是不是在最前面**（7.4 的代价）；
- 发行版「启动」之后**等一分钟以上**看是不是还 Running（7.3 那条）；
- 主题切换是不是立即重绘。

改到这类东西时，**在 PR 描述里明确写出"需要本机实测"**，别只说"CI 绿了"。

---

## 9. 子代理：一次最多 2 个

**硬上限：一次最多派 2 个子代理。** 这不是"建议别太多"，是用户定的规矩。

理由都是实际代价，不是"觉得应该"：

- **子代理看不到主对话。** 它不共享上下文，所以要在提示词里把"背景 + 目标 +
  验收标准"从头写全。写不全，它就拿着半个问题去猜，回来一份**看起来完整、
  实际不可用**的结论 —— 你还得先花时间判断它到底答了什么。
- **它的产出不能直接信，每条都要自己去代码里核。** 本仓库真实发生过两次：
  - 有子代理报"`columns.rs:84` 该按 clippy 改"，而那处 `!(total > 0.0)` 是
    **故意的** NaN 兜底（改成 `total <= 0.0` 对 NaN 也是 false，会算出一堆
    NaN 宽度）—— 照它改就是引入 bug；
  - 另有子代理报 `app/shell/render.rs` 里的 `user_refreshing` 是死代码，实际它在
    `hint` 那里有用。

  核一遍的时间往往不比自己做少，所以"派出去"并不自动等于"省事"。
- 并行跑多个还会互相覆盖同一个文件、或在同一分支上抢提交（这个仓库的改动
  经常集中在同一个域的几个文件上）。

**所以默认自己做。** 只有同时满足这两条才派：

1. 能切成**互不依赖**的几块（不会改到同一批文件）；
2. 每一块的产出都能**明确验证**（有可查的判据，不是"读起来挺有道理"）。

派的时候一次不超过 2 个，提示词写到"别人拿着它就能干"的完整度。

> 用户随时可以要求更少（比如"只许派 1 个"）或者干脆不派 —— 按用户说的做，
> 这条上限只是默认值。
