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
    ├── app.rs        Shell（GPUI 实体）+ 所有动作 + 弹窗浮层（约 160 KB）
    ├── views.rs      渲染 + 全部弹窗浮层（约 180 KB / 5000 行，**很大**）
    └── theme.rs      颜色
```

> 这两个文件大是**已知**的：往里加东西之前先问一句"这能不能放
> `wslc-panel-core`"，判断标准就是下面那节（"这个文件 `use gpui` 吗"）。

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
2. 升版本号（**四个文件、六行**，少一处 CI 就会红）：
   - 根 `Cargo.toml` 的 `[workspace.package] version`
   - `crates/wslc-core/Cargo.toml` 里那个**显式**的 `version`（它还没改成
     `version.workspace = true`，是个小遗留）
   - **`Cargo.lock` 里三个 workspace 成员的 `version`** —— 锁文件记着成员版本，
     不改的话 `--locked` 直接报"锁文件与清单不一致"。本机没有 cargo 只能手改：
     先 `Select-String` 确认**只有那三处**（都在 `[[package]]` 的成员条目下），
     再整体替换。不放心就用 `gen-lock.yml` 重新生成。
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

⚠️ **用 REST API 建标签不会触发 `release.yml`**：`POST /git/refs` 只发 `create`
事件，而那个 workflow 监听的是 `push: tags`（`release.yml` 顶部就写着这件事）。
所以要么 `git push origin vX.Y.Z`，要么走 `release.yml` 的
`workflow_dispatch` + `release_tag=vX.Y.Z` —— 那条路径就是为"git push 不通"留的。

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

### 界面：同一个动作只给一个入口

列表行放**高频**动作，详情弹窗放**低频 / 危险**的 —— 同一个动作**不要两边都摆**。

理由不是"省地方"：两个入口背后是同一个 `ImmediateAction` / `PendingAction`，
行为完全一样，用户却要判断该点哪个。（v0.5.11 实机踩到：停止的容器在列表行和
详情里各有一份一模一样的「启动 / 日志 / 删除」。根因是一条注释写着
"照 1Panel：列表里不放操作按钮"，而本项目的列表行**一直有**按钮 ——
**注释和实现不一致时，后来的人会照着注释继续加重复**。）

推论：如果详情里某个状态下**一个动作都没有**（比如停止的容器），要写一句话
说明其余动作在哪儿，别让用户以为按钮没画出来。

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
- **"即时反馈"不只等于 toast**：浮层里的按钮换成「加载中…」/「执行中…」同样算
  （日志浮层、exec 浮层就是这么做的 —— 那儿不发 toast，因为结果本来就填在
  同一个浮层里）。关键是"从点击到有结果之间，界面上必须有变化"；
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
| **`wsl -d X` 会继承调用者的 cwd**（实测于 WSL 3.0.3.0） | 面板自己不设 cwd，所以"打开终端"落在哪儿 = **用户从哪个文件夹启动的面板**。实测：从 `C:\Users\76434\Downloads` 启动 → 终端落在 `/mnt/c/Users/76434/Downloads`；从 `E:\cnb\wslc-panel` 启动 → `/mnt/e/cnb/wslc-panel`。翻译不了的路径（UNC、未挂载的盘、从 WSL 里起 exe）回落 `~`。**要固定起点就得显式写 `--cd`** |
| **`--cd` 才是指定终端起点的开关**（实测于 WSL 3.0.3.0） | `wsl -d X --cd ~` → `/home/wt`；`--cd ~/tmp` 会把 `~` 展开；**`--cd` 指向不存在的目录不报错**，只在 stderr 留一行 `chdir(...) failed 2` 然后落在 `/`（所以写错的路径不会"点了没反应"，但也别指望它提示）；`--cd D:\wsl` 也会被翻译成 `/mnt/d/wsl`（我们还是只让用户填 Linux 路径 —— 两个坐标系之间猜最容易出错）。面板里由偏好 `terminal_dir` 决定，见 `wslc_panel_core::prefs::TerminalStartDir` |
| `wslc exec -i -t` **有 `-w/--workdir`** | 容器里的起点可以指定（`wslc exec --help`），但容器里**没有**"Linux home"这个概念 —— `~` 是谁由镜像决定，面板不往下传，见 `TerminalStartDir::container_workdir` |

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
- **模态遮罩必须挂 `.occlude()`** —— 一个只画底色的 `div` **不参与命中测试**
  （GPUI 只给注册过交互的元素建 hitbox），于是遮罩开着的时候点击会**穿到背后的
  页面上**。v0.5.10 时 8 个弹窗全有这个问题（实机：打开「创建容器」还能点到它
  背后那一列的按钮，而那个动作会重建表单、把用户打的字清掉）。
  现在遮罩统一走 `views::modal_scrim()`，行为只有一份，理由写在
  `theme::scrim()` 的文档里。

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
⚠️ **但这样建的标签不会触发 `release.yml`** —— `POST /git/refs` 只发 `create`
事件，而那个 workflow 监听的是 `push: tags`。要发版就用 `git push origin vX.Y.Z`，
或者走 `release.yml` 的 `workflow_dispatch` + `release_tag`（见 §4）。

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
  - 另有子代理报 `app.rs` 里的 `user_refreshing` 是死代码，实际它在
    `hint` 那里有用。

  核一遍的时间往往不比自己做少，所以"派出去"并不自动等于"省事"。
- 并行跑多个还会互相覆盖同一个文件、或在同一分支上抢提交（这个仓库的改动
  经常集中在 `app.rs` / `views.rs` 这两个大文件上）。

**所以默认自己做。** 只有同时满足这两条才派：

1. 能切成**互不依赖**的几块（不会改到同一批文件）；
2. 每一块的产出都能**明确验证**（有可查的判据，不是"读起来挺有道理"）。

派的时候一次不超过 2 个，提示词写到"别人拿着它就能干"的完整度。

> 用户随时可以要求更少（比如"只许派 1 个"）或者干脆不派 —— 按用户说的做，
> 这条上限只是默认值。
