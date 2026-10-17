# 参考项目对比分析：`rcdesktop` / `wslcc` ↔ `wslc-panel`

> 本文是**一次性调研产出**，不是持续维护的计划文档。目的是把两个同期同类项目里
> 「我们没做、但他们做对了」的东西挑出来，并给出**适配 Rust + GPUI** 的落地方案。
>
> 三个项目都在做同一件事：**给 Windows 上的 `wslc.exe`（WSL 容器）套一个图形面板**。
> 所以可比性极高 —— 分歧点几乎都是"对同一个真实约束的不同取舍"。
>
> | 采集时间 | 参考代码版本 |
> |---|---|
> | 本次会话 | `rcdesktop@8e79809`（v0.4.0）、`wslcc@f21e599`（v1.8.1）、`wslc-panel@0.9.18` |
>
> 本地只读副本：`E:\cnb\_ref\rcdesktop`、`E:\cnb\_ref\wslcc`。
>
> ⚠️ **许可与抄写边界**：`rcdesktop` 是 **MIT**、`wslcc` 是 **MIT**、
> 本项目是 **Apache-2.0**（三者兼容，MIT 可以并入 Apache-2.0）。
> 但 `wslcc` 是 **C#**、`rcdesktop` 是 **Rust + Slint**，本项目是 **Rust + GPUI** ——
> **跨语言/跨 UI 框架，只能借鉴机制与决策，不能搬代码。**
> 这与 `AGENTS.md` §6 对 `wsl-dashboard`（GPL-3.0）的约束不同：那两个项目
> **许可上允许**引用代码，但**工程上不建议**（渲染层、异步模型、组件库都不一样）。

---

## 0. 结论先行

一句话：**`wslc-panel` 在"数据层的深度"（解析、真实夹具、MySQL/部署这类垂直功能）上明显领先，
但在"桌面应用的完整度"（进程模型、错误自愈、分发、可观测性）上落后于这两个项目。**

三个最要命的差距，按优先级：

| # | 差距 | 严重度 | 依据 |
|---|---|---|---|
| **1** | **`wslc` 调用完全并发，没有串行闸门、没有 sharing violation 重试** | 🔴 高 | `rcdesktop` README:27 明写"两次并发调用会 `ERROR_SHARING_VIOLATION (0x80070020)`"；本项目 `app.rs` 有 **45 处**独立后台调用点 |
| **2** | **没有任何 `wslc` 错误码识别 → 用户看到原始报错，无法自救** | 🔴 高 | `wslcc` ADR-0014 整篇在讲 `0x800701c0`（重解析点）怎么识别与指引；本项目全仓库 grep `800701c0`/`0x80070020`/`E_UNEXPECTED` **零命中** |
| **3** | **没有单实例守卫** | 🟠 中高 | `rcdesktop` `single_instance.rs`、`wslcc` `App.xaml.cs:97`；本项目可以开 N 个实例，**每一份都各自放大第 1 条的并发压力** |

以及三块"没有但成本不高、收益立刻可见"的：

- **自更新**（`wslcc` 有完整链路）
- **导航式安装包 + 自启/托盘**（两家都有，本项目只有裸 exe + zip）
- **ADR 制度 + 逐版本 release notes**（`wslcc` 有 15 篇 ADR + 17 篇 release notes，本项目只有 PLAN）

---

## 1. 三个项目速览

| | **wslc-panel**（本项目） | **rcdesktop** | **wslcc** |
|---|---|---|---|
| 语言 / UI | Rust + **GPUI**（`gpui-kit` 0.7.1） | Rust + **Slint** 1.9 | C# + **WinUI 3**（Windows App SDK 2.4） |
| 运行时 | 无（单文件 exe，D3D11） | 无（单文件 exe） | .NET 10 + Windows App Runtime |
| 代码量 | `*.rs` 49 个 / **2007 KB** | `*.rs` 33 个 / 197 KB + `*.slint` 20 个 / 138 KB | `*.cs` 74 个 / 464 KB + `*.xaml` 17 个 / 142 KB |
| 分层 | 3 crate（core / panel-core / panel） | 单 crate，`domain/`+`wslc/`+`ui/` | 3 项目（App / Core / Data） |
| 许可 | Apache-2.0 | MIT | MIT |
| 分发 | 裸 exe + zip（无安装包） | exe + zip + **Inno Setup**（x64+ARM64 单包） | zip ×2 + **Inno Setup** ×2（自包含/框架依赖） |
| 数据持久化 | JSON（`prefs.json` / 凭据 / 部署记录） | JSON（`settings.json`） | **SQLite**（6 张表） |
| 测试 | 真实夹具驱动 + 真机冒烟（**最扎实**） | mock client + 夹具 | 无测试项目（靠 PRD 验收标准） |

**三者最核心的分歧点**：**怎么对付 `wslc.exe` 这个"不喜欢并发"的子进程**。

---

## 2. 分层架构对比

### 2.1 本项目（3 crate）

```
wslc-core/        纯数据层，不依赖 UI（MSRV 1.75）
wslc-panel-core/  不依赖 GPUI 的那一半（edition 2024）
wslc-panel/       只有 UI（app.rs 364 KB + views.rs 330 KB）
```

拆 `wslc-panel-core` 的理由是**实测的 CI 时间**：`cargo test -p wslc-panel --bins`
要 763 秒（占一轮 CI 的 72%），搬出来之后整轮 **17.7 min → 1.7 min**。

> **这一条是三家里面最强的工程决策**，`rcdesktop` 和 `wslcc` 都没有等价物 ——
> `wslcc` 是 C#（编译快，没有这个问题），`rcdesktop` 只有 197 KB Rust，
> 测试跑起来本来就不贵。**本项目不要因为看它们就动摇这个拆法。**

### 2.2 `rcdesktop`（单 crate，靠目录分层）

```
src/domain/   纯数据模型（container / image / volume / session / system / compose / deploy）
src/wslc/     与 wslc.exe 有关的一切：client(trait) / queue(单车道) / parser / mock / stack
src/app.rs    桥梁：Tokio 异步事件 → Slint 事件循环（1192 行）
src/tray.rs   系统托盘
src/single_instance.rs  单实例
src/settings.rs 偏好 + 开机自启
src/rcompose.rs 外部 compose 工具的定位/安装/运行
ui/*.slint    声明式 UI（20 个文件，含 theme.slint 设计令牌）
```

值得注意的**关键设计**（`src/wslc/client.rs:10-38`）：

```rust
#[async_trait]
pub trait WslcClient: Send + Sync {
    /// Offline preview client: external tools (rcompose) must not touch real wslc.
    fn is_mock(&self) -> bool { false }
    async fn list_containers(&self, all: bool) -> Result<Vec<Container>, String>;
    // …20 个方法
}
```

**`WslcClient` 是一个 trait，`RealWslcClient` 和 `MockWslcClient` 是两个实现。**
这是本项目**完全没有**的一层抽象 —— 本项目的 `Wslc` 是一个具体 struct，
所有调用点直接依赖它。

### 2.3 `wslcc`（3 项目，单向依赖）

```
WSLCC.App    WinUI3 + MVVM（Pages / ViewModels / Controls / Mini / Services）
WSLCC.Core   Cli（WslcRunner / WslcCapabilities）+ Models + Services（20 个服务）
WSLCC.Data   SQLite（Entities / Repositories / WslcDatabase）
```

依赖方向 **App → Core → Data**，由 `WslcHost`（`WslcHost.cs:57-88`）做组合根：

```csharp
public WslcHost(string? databasePath = null) {
    Runner = new WslcRunner();
    Database = …;
    Capabilities = new WslcCapabilities(Runner);
    Images   = new WslcImageService(Runner, Audit, History, Capabilities);
    Containers = new WslcContainerService(Runner, Audit, History, Capabilities);
    …
}
public static WslcHost Default { get; } = new();
```

**每个服务都先声明接口（`IWslc*Service`）再实现** —— 20 个服务、20 个接口。
`docs/prd/06-non-functional-requirements.md:26` 把这条写成硬要求：
「服务接口化（`IWslc*Service`），便于替换实现与单元测试。」

> **对本项目的价值**：本项目 `wslc-core` 已经是"纯逻辑 + 可测"的，
> 但**没有接口层**。这不是缺陷（Rust 的 trait 是零成本抽象，但引入它
> 会让 45 个调用点全部泛型化，改造成本远超收益）。
> **真正值得抄的是 `rcdesktop` 那种"只抽象一个 trait、只为了 mock"的最小用法**，
> 见 §9.6。

---

## 3. 【重点】数据通道与并发模型

### 3.1 三个项目的做法

| | 本项目 | `rcdesktop` | `wslcc` |
|---|---|---|---|
| 调用方式 | `std::process::Command`，同步等子进程 | `tokio::process::Command`，async | `Process` + `WaitForExitAsync` |
| 编码 | 注入 `WSL_UTF8=1` + `decode.rs` 双路兜底 | `from_utf8_lossy` | `StandardOutputEncoding = UTF8` |
| 超时/取消 | ✅ 有（`Error::Timeout` / `Error::Cancelled`） | ✅ 有（`tokio::time::timeout`） | ✅ 有（`CancellationToken` + `KillProcessTree`） |
| 不弹黑框 | ✅ `CREATE_NO_WINDOW` | ✅ `CREATE_NO_WINDOW` | ✅ `CreateNoWindow = true` |
| **串行化** | ❌ **无** | ✅ **单车道队列** | ❌ 无（靠 UI 侧 `SemaphoreSlim` 局部串行） |
| **sharing violation 重试** | ❌ 无 | ✅ 退避 3 次 + 自动切 `cmd.exe` 模式 | ❌ 无 |

### 3.2 `rcdesktop` 的单车道队列（本项目最该看的一段）

`README.md:27` 把问题说得很直白：

> One detail worth knowing: wslc doesn't like being called concurrently.
> Two calls at the same time can fail with `ERROR_SHARING_VIOLATION` (0x80070020).
> The app runs every command through a single queue and retries with backoff when that happens.

`docs/superpowers/specs/…design.md:70-73` 给了机制：

> `wslc.exe` uses file-based locks for its session database. Multiple concurrent
> invocations fail with `ERROR_SHARING_VIOLATION`. To avoid this: all commands
> directed to `wslc.exe` are dispatched as messages to a dedicated Tokio worker
> task over a bounded MPSC channel. The worker executes exactly one command at a time.

实现只有 66 行（`src/wslc/queue.rs:22-66`）：

```rust
struct WslcRequest {
    args: Vec<String>,
    timeout: Duration,
    response: oneshot::Sender<Result<String, String>>,
}

#[derive(Clone)]
pub struct SingleLaneQueue { sender: mpsc::Sender<WslcRequest> }

impl SingleLaneQueue {
    pub fn new() -> Self {
        let (tx, mut rx) = mpsc::channel::<WslcRequest>(64);
        let bin_path = find_wslc_bin();
        let use_cmd_mode = Arc::new(AtomicBool::new(false));

        tokio::spawn(async move {
            while let Some(req) = rx.recv().await {
                let res = execute_serialized(bin, req.args, req.timeout, cmd_mode).await;
                let _ = req.response.send(res);
            }
        });
        Self { sender: tx }
    }
    // execute(): send + await oneshot → 调用方看起来就是个普通 async 调用
}
```

**要点**：调用方看到的是 `queue.execute(args, timeout).await` —— 一个普通异步调用。
串行化是**实现细节**，不是调用方的责任。**这是它能覆盖 20 个方法而不失控的原因。**

重试与模式回退（`queue.rs:84-163`）：

```rust
fn is_sharing_violation(output: &str) -> bool {
    let lower = output.to_lowercase();
    lower.contains("error_sharing_violation")
        || lower.contains("0x80070020")
        || lower.contains("used by another process")
        || lower.contains("outro processo")          // 葡萄牙语本地化（作者是巴西人）
}

// max_retries = 3；backoff 200ms → 400ms → 800ms
match tokio::time::timeout(timeout, run_future).await {
    Ok(Ok(stdout)) => return Ok(stdout),
    Ok(Err(err_msg)) => {
        if is_sharing_violation(&err_msg) {
            if !is_cmd { use_cmd.store(true, Ordering::Relaxed); }   // 永久切模式
            if attempt < max_retries { sleep(backoff); backoff *= 2; continue; }
        }
        return Err(err_msg);
    }
    Err(_) => return Err(format!("Command timed out after {}s", …)),
}
```

`cmd.exe` 回退模式的理由（`design.md:75-79`）：

> In GUI applications lacking an attached console window, `wslc.exe` can misbehave
> when accessing session handles. … **Cmd Fallback Mode:** Spawning through
> `cmd.exe /d /s /c "wslc.exe <args>"` to provide console handle inheritance.
> Auto-detection: If direct mode yields console-related sharing violations,
> the engine automatically switches to `cmd` mode and **caches this decision**.

**注意：`wslcc` 没有做这件事。** `WslcRunner.cs:23-49` 直接起进程，
唯一的串行化是 `WslcApiHost` 的 `SemaphoreSlim`（`WslcApiHost.cs:21`，且那条路径
`_session` 从未被业务调用，见 ADR-0003 后续）。

这是两家最重要的**判断分歧**，含义对本项目很直接：

> ⚠️ **`rcdesktop` 的 0x80070020 结论是它在自己机器上实测的，不是微软文档写的。**
> 本项目的 `AGENTS.md` §7.3 是一张很厚的"WSL 实测表"，**里面没有这一条** ——
> 说明本项目**从没撞到过**（或者撞到过但被当成偶发错误忽略了）。
> **所以上这条之前必须先复验**（复验方法见 §9.1），别照搬结论。

### 3.3 本项目的现状有多"散"

```powershell
# app.rs 里 background_executor() 的真实调用点数（排除 3 行注释）
> (Select-String -Path crates/wslc-panel/src/app.rs -Pattern 'background_executor\(\)' |
>    Where-Object { -not $_.Line.TrimStart().StartsWith('//') }).Count
45
```

这 45 处里，绝大多数会（在 `background_executor` 的 async 块里）调 `wslc.run(…)`
或 `wsl.run(…)`。典型形态：

- 3 秒定时刷新（`load_snapshot` → `wslc info` + `wslc list -a` + `wslc stats -a` + `wslc images` + `wslc network list` + `wslc volume list` + `wsl system list` …）
- 用户点的每个动作各起一个
- 数据库页 / 部署页各有自己的约 9 秒轮询

> ⚠️ **上面这句话要说准，否则会误判改造范围。**复核代码后的事实是：
>
> **同一个刷新路径内部是有协调的**（这套比 `rcdesktop` 的 `interval + await` 还细）：
> - `state.busy` 守卫：正在采时不开第二轮（`app.rs:5732-5744`）
> - `refresh_again` 合并：被挡下的请求**不丢弃**，本轮结束立刻补跑（`app.rs:5736/5805`）
> - `tick` 忙则跳过：自动刷新**不排队**（`app.rs:5823-5825`）
>
> **真正没有协调的是"子系统之间"**：`busy` 每个子系统各一份、互不通气 ——
> 快照刷新 / 部署页状态列（每条部署一次 `wslc exec`，`app.rs:5855-5868`）/
> 数据库页 / 日志与 exec 浮层 / 用户动作 / `wsl.conf` 保存（跑 `wsl.exe`）。
> 它们**本来就该**互不阻塞，所以"在每个调用点加协调"一定会漏。
>
> 而 `cli.rs` 里有超时、有取消、有双管道并发读，**就是没有闸门和重试**
> （grep `Mutex|Semaphore|SHARING` 只命中 `Arc<Mutex<Child>>` ——
> 那是持有子进程句柄以便 kill 用的，与串行化无关）。
>
> 📌 落地方案见 [`PLAN-v0.10.md`](PLAN-v0.10.md) §4。

### 3.4 建议（P0）

**做法**：在 `wslc-core` 里加一个进程内闸门，**只包 `Wslc`，不包 `Wsl`**。

```rust
// crates/wslc-core/src/cli.rs（示意）
// 为什么放在这一层而不是 app.rs：45 个调用点分散在 app.rs/views.rs，
// 在调用点加锁一定会漏；包在 run 里面则"想漏也漏不掉"。
static WSLC_GATE: Mutex<()> = Mutex::new(());

impl Wslc {
    pub fn run_checked(&self, args: &[&str]) -> Result<CommandOutput> {
        let _lane = WSLC_GATE.lock();          // ① 串行
        self.run_checked_raw(args)             // ② 原逻辑
    }
}
```

**必须同时考虑的四件事**：

1. **`Wslc` 有 45 个调用点，但 `Wsl`（发行版）要不要也进闸门？**
   `rcdesktop` 只对 `wslc` 做（它的 `find_wslc_bin` 只管 wslc）。
   `wsl.exe` 与 `wslc.exe` 共用 session 数据库吗？**未知，需实测**。
   保守做法：只锁 `Wslc`，`Wsl` 不动（这已经是收益最大的部分）。
2. **⚠️ 长任务会把闸门占死。** `wslc pull`（本项目超时 600 s 级别）、
   `wslc run`（要拉镜像）、`--manage --compact`（实测 10.2 s）会独占车道，
   期间 3 秒轮询全部排队 —— **界面会看起来"卡住了"**。
   `rcdesktop` 没这个问题是因为它的轮询间隔是 2.5 s 且**没有 `prune` 长任务并存**。
   → **建议做成两条车道**：`quick`（查询类，30 s 内）与 `slow`（写操作/拉取/压缩），
   `quick` 永远优先。或者给闸门加**超时获取**：排队超过 N 秒就返回
   `Error::Timeout` 而不是无限等。
3. **重试要能区分"可重试"与"不可重试"。** `rcdesktop` 只对 sharing violation 退避。
   本项目 `Error::NonZeroExit` 已经合并了 stdout/stderr（`error.rs:66-78`），
   加一个 `is_retryable()` 即可。
4. **复验脚本**（上这条之前先跑）：
   ```powershell
   # 并发起 8 个 wslc，看是否出现 0x80070020 / Sharing violation
   1..8 | ForEach-Object -Parallel { & wslc list --format json 2>&1 } | Select-String -Pattern '0x80070020|sharing'
   ```
   跑不通（没有 `-Parallel`）就用 8 个 `Start-Job`。**先拿到本机结论再改代码** ——
   这是本项目 `AGENTS.md` §5「实测优先于文档」的直接应用。

---

## 4. 【重点】错误码识别与"能自救的报错"

### 4.1 `wslcc` 的三类诊断（本项目全缺）

**① 重解析点 / RedirectionGuard → `0x800701c0`**（`docs/adr/0014-…md` 全篇）

背景（ADR-0014:7-14）：

> 部分用户把 `%LOCALAPPDATA%` 迁到其它盘，`%LOCALAPPDATA%\wslc` 因此变成 junction。
> wslc 2.9.13+ 启用 RedirectionGuard，enforce 时拒绝遍历不受信任的装入点，
> 读取 `settings.yaml` 或会话存储即失败：`错误代码：0x800701c0`。
> **实测 `CreateProcess` 的 mitigation 属性无法放宽父进程已 enforce 的策略** ——
> 所以应用侧**没有规避手段**。

决策（ADR-0014:18-24）：不做规避式修复，改成**启动自检 + 明确指引**：

```csharp
// WslcPathDiagnostics.cs:5-32
public static IReadOnlyList<string> DetectUntrustedReparsePoints(params string?[] paths) {
    foreach (var path in paths) {
        var info = new DirectoryInfo(full);
        if (info.Attributes.HasFlag(FileAttributes.ReparsePoint)) result.Add(full);
    }
}
```

并在错误出现时把命中路径列给用户（`ContainersViewModel.cs:307-328`）：

```csharp
private void CheckReparseWarning(string message) {
    var reparse = text.Contains("800701c0") || text.Contains("不受信任的装入点")
               || text.Contains("untrusted mount point");
    var paths = reparse ? WslcPathDiagnostics.DetectUntrustedReparsePoints(
                    WslcPathDiagnostics.SettingsDirectory, WslcHost.DefaultStoragePath) : [];
    …
    ReparseWarningText = L.GetFormat("ContainersPage.ReparseWarningWithPaths", string.Join(…, paths));
}
```

**本项目现状**：全仓库 grep `800701c0` / `reparse` / `junction` / `RedirectionGuard`
→ **零命中**。而这个场景**在本项目上一定也会发生** ——
因为本项目 `wslc-core/src/wslcconfig.rs` 与 `settings.rs` 都在读
`%LOCALAPPDATA%\wslc\settings.yaml`（`default_settings_path()`，`state.rs:748`）。

> ⚠️ 这一条的**用户画像很明确**：把 `LOCALAPPDATA` 挪到 D 盘的人
> （恰恰是最可能装 WSL 容器、且最可能用重量级面板的那批人）。
> 他们会看到一句无法理解的 HResult，然后去开 issue。

**② 代理 / TUN 网卡 → `E_UNEXPECTED`**（ADR-0003:7, `WslcNetworkDiagnostics.cs:8-34`）

> 本机实测（2026-09）API 的 Session `Start` 成功但 `GetImages` 等持续抛
> `COMException E_UNEXPECTED (0x8000FFFF)`；属特定环境（VPN/网络干扰，见 microsoft/WSL#41082）的不稳定。

检测方式是纯 IP 段判断（`198.18.0.0/15` 与 `100.64.0.0/10`）：

```csharp
if (IsInCidr(ip, 198, 18, 0, 0, 15) || IsInCidr(ip, 100, 64, 0, 0, 10))
    result.Add($"{ni.Name}（{addr.Address}）");
```

命中时才把"网络告警"挂到错误横幅上（`ContainersViewModel.cs:357-376`）。

**本项目现状**：`E_UNEXPECTED` 零命中。而本项目自己的
`docs/PLAN-v0.6.md` §3.4 与 README「已知限制」都在讲镜像站/registry 的连通性问题
（`1ms.run` 要 token、`daocloud` 要 tag 列表、直连 Docker Hub 超时）——
**是同一类问题的另一面，只是没有把它归因到"用户开了代理/VPN"。**

**③ 会话名必须先校验存在性**（ADR-0009:22）

> 实测 `wslc --session <不存在>` 会让**所有**操作报 `WSLC_E_SESSION_NOT_FOUND`。
> 因此启动与保存配置时先 `system session list` 校验，不存在则**回退到 wslc
> 默认会话并记录日志，绝不把无效会话名注入命令行**。

```csharp
// App.xaml.cs:152-167
private static async Task<string?> ResolveSessionNameAsync(string configuredName) {
    if (string.IsNullOrWhiteSpace(configuredName)) return null;
    var sessions = await WslcHost.Default.System.ListSessionsAsync();
    if (sessions.Any(s => s.Name.Equals(configuredName, StringComparison.OrdinalIgnoreCase)))
        return configuredName;
    WriteLog($"会话 '{configuredName}' 不存在，回退到 wslc 默认会话。");
    return null;
}
```

**本项目现状（⚠️ 这是一个"半成品"）**：`cli.rs` 里 `--session` **实现了**：

```rust
// cli.rs:160-193
/// 设置会话（对应全局选项 `--session <id>`）。
pub fn session(mut self, session: Option<u32>) -> Self { … }
/// 构造最终参数列表（含 `--session` 前缀）。
```

但**真实调用点有 0 个** —— grep 全仓库，`.session(` 只出现在
`cli.rs:1071 / 1089-1094` 的**测试**里。而 UI 侧 `state.rs:4004` 的
`session_label()` 只是把第一个活动会话的名字显示出来，**从不注入**。

→ **两个选择**：① 接上（配一个会话选择器 + 存在性校验，ADR-0009 的教训必须一起带上）；
② **删掉**，别留一层骗人的 API。**不要停在现在这个状态** ——
它会让下一个读代码的人以为"会话功能已经有了"。

### 4.2 `wslcc` 的解析健壮性：多候选字段名（ADR-0006）

> 预览版曾改变输出字段（如 2.9.9 起 `volume list --format json` 字段从两项扩展为
> Docker 十项），表明字段名不稳定。

决策（ADR-0006:11-14）：

- 逐行读、`{` 起始、`JsonDocument` 解析，**跳过非法行**，返回 `JsonElement` 流
- 字段读取用**多候选名称**：容器名 `Names`、ID `ID`/`Id`、镜像等依次尝试
- 值缺失时回退默认（`"-"`、`""`、`<none>`）
- 流式命令（pull/run/logs）**按行回调、不解析进度结构**，原样展示 —— 规避结构变化

调用方示例（`WslcRunner.cs:190-213`）：**只有当"一条都没解析出来、且有失败行"时才抛**：

```csharp
if (results.Count == 0 && failed > 0)
    throw new WslcCliException($"wslc 输出无法解析为 JSON（{failed} 行非 JSON）：{Truncate(output)}");
```

**本项目现状**：`jsonl.rs` 已经做到"空输出 / 坏行 / 字段缺失都不 panic"，
`tests/fixtures/` 是**真机抓的**（比 `wslcc` 强，`wslcc` 因为环境问题
"无法在本地捕获真实字段样本"，见 ADR-0006:7）。

**但有两点可以补**：

1. **错误识别的本地化覆盖**。`WslcRunner.cs:221-234` 同时认中英文与 HResult：
   ```csharp
   if (combined.Contains("灾难性故障")) return true;
   if (combined.Contains("Catastrophic", OrdinalIgnoreCase)) return true;
   if (combined.Contains("错误代码")) return true;      // ← 中文
   if (combined.Contains("Error code", OrdinalIgnoreCase)) return true;
   if (HresultPattern().IsMatch(combined)) return true;  // \bE_[A-Z0-9]+\b
   ```
   本项目只按**退出码**判成功（`error.rs:67-69`，`code == Some(0)`）。
   这在 wslc 上可能不够 —— 本项目自己已经踩过一次同类坑：
   `cmd/system.rs:65-71` 那条测试「`system session list --format json` 会报
   **当前命令的选项名称未被识别**」，说明 **wslc 会用非零退出码之外的形态报错**。
   → 值得核一遍：**有没有"退出码 0 但 stdout 里有 `错误代码：`"的情况**。
2. **`wslc events` 完全没接**。`storage.rs:5` 的注释已经写明
   「`wslc` 没有 `system df`（`system` 只有 `events` / `info` / `session`）」——
   也就是说**本项目知道它存在，但没做**。而 ADR-0011:12 说：

   > `events`（别名 `system events`）：新增事件流命令，支持 `--since`、`--until`
   > 与可重复 `--filter`（键：`type`、`container`、`image`、`network`、`event`）。

   ⚠️ **ADR-0011:41 记了一个必须一起知道的坑**：

   > **`events` 不带 `--until` 会永久流式输出、不退出。**`QueryAsync`（历史查询）
   > 必须注入 `--until`，否则调用永久阻塞。且 `until` 与事件落在同一整秒时会漏掉
   > 该事件（官方 E2E 亦用 `+1`），故查询用**当前秒 + 1** 作为上界。

   本项目有超时机制，就算写错也只会"30 秒后超时"而不是永久挂死 ——
   但**那 30 秒里闸门（如果上了 §3.4）会被占死**。两件事要一起做。

### 4.3 建议（P0/P1）

| 建议 | 优先级 | 落点 |
|---|---|---|
| 错误码 → 人话的映射表（`0x800701c0` / `0x80070020` / `E_UNEXPECTED` / `WSLC_E_SESSION_NOT_FOUND` / `0x8000FFFF`） | **P0** | `wslc-core/src/error.rs` 加 `Error::hint()`；UI 侧横幅显示原文 + 指引 |
| 启动时自检重解析点（`%LOCALAPPDATA%\wslc` 与 `storagePath` 是否 `FILE_ATTRIBUTE_REPARSE_POINT`） | **P0** | 新增 `wslc-core/src/diagnostics.rs`；设置页/概览页出 InfoBar。**纯 Win32 API，不需要起进程，无副作用** |
| TUN/代理网卡检测（`198.18.0.0/15`、`100.64.0.0/10`） | P1 | 同上（`GetAdaptersAddresses` 或复用 `ipconfig` 解析） |
| stats 轮询错误**去重**（相同错误只记一次、不刷屏） | P1 | `ContainersViewModel.cs:87-106` 的 `_lastStatsError`。本项目 3 秒一次刷新，wslc 挂掉时会疯狂刷新横幅 |
| 会话名注入前校验（或删掉半成品） | P1 | `cli.rs` + `state.rs:4004` |

---

## 5. 【可观测性】他们都有，我们都没有

| 能力 | `rcdesktop` | `wslcc` | 本项目 |
|---|---|---|---|
| 操作审计（谁、何时、做了什么、成败、耗时） | ❌ | ✅ `WslcAuditService.cs` + `audit_log` 表 | ❌ |
| 镜像拉取历史 | ❌ | ✅ `WslcHistoryService.cs` + `pull_history` 表 | ❌ |
| 容器状态快照 | ❌ | ✅ `container_snapshot` 表 | ❌ |
| 一键反馈（预填 issue + 复制诊断） | ❌ | ✅ `IssueReporter.cs` + `IssueReportBuilder.cs` | ❌ |
| 崩溃日志 | 控制台 | ✅ `%LOCALAPPDATA%\WSLCC\app-crash.log`（3 种未处理异常都挂） | ✅ `logs\wslc-panel.log` |
| 构建标识（哪个提交） | ❌ | ✅ 版本号（AssemblyVersion） | ✅ **提交号**（`main.rs:104-112`，比 `wslcc` 更强） |

### 5.1 `wslcc` 的一键反馈（成本低、直接减少维护负担）

`IssueReportBuilder.cs:21-26` 直接拼一个预填好的 GitHub issue URL：

```csharp
public static string BuildUrl(string context, string? detail, IssueReportContext info) {
    var title = $"[Bug] {Shorten(context, MaxTitleLength)}";        // 80
    var body = BuildIssueBody(context, detail, info);
    return $"{RepoUrl}/issues/new?title={Uri.EscapeDataString(title)}&body={Uri.EscapeDataString(body)}";
}
```

正文是自己会长出来的（`BuildIssueBody`）：问题描述 / 复现步骤占位 / 错误信息代码块 /
环境信息代码块 → 并**限制长度**（`MaxTitleLength=80`、`MaxContextLength=400`、
`MaxDetailLength=1500`、`MaxBodyLength=4000`，超长截断）。

环境块（`BuildEnvironment`，:93-105）：
```
WSLCC: 1.8.1
OS: …   .NET: …
wslc: 3.0.1.0
TUN 网卡: …
时间: 2026-…
```

**"复制诊断信息"**（`BuildDiagnostics`，:59-91）比 URL 更长 —— 多带
**近期操作记录**（最近 10 条审计）与**应用日志尾部**（30 行）。

UI 侧（`IssueReporter.cs:22-73`）挂在每个页面的 `InfoBar` 上：
主按钮「复制诊断信息」、次按钮「打开 GitHub」、关闭。

> **对本项目的启示非常大**：本项目 README 已经有一段很用心的「故障排查」
> （看日志文件、`RUST_LOG=debug`、窗口空白的三种原因）。但**用户还是要自己去
> 找日志文件、自己组织信息**。有了这条，「报 bug」变成一次点击。
> 而且本项目的构建号机制（`build_sha()`）**比 `wslcc` 更适合放进诊断块**
> （`wslcc` 只有版本号，同一版本的不同构建分不出来）。

### 5.2 建议（P1）

- `wslc-panel` 的「关于」页加两颗按钮：**「复制诊断信息」**（环境 + 构建提交号 +
  `wslc --version`/`info` 摘要 + 最近 N 行日志 + 自检结果）与**「在 GitHub 新建 issue」**
  （URL 预填，带长度上限）。
- 审计日志：**优先级低于反馈**。本项目是本地单用户工具，
  "谁改了什么"的价值不如"出错了怎么报"高。若要做，
  **别上 SQLite** —— 追加写 JSONL（`storage.rs` 已有算存储占用的能力）足够，
  且和现有 `deploys.rs` / `dbcreds.rs` 的持久化习惯一致。

---

## 6. 【分发】本项目只有裸 exe + zip

### 6.1 两家的做法

**`rcdesktop`**（`installer/rcdesktop.iss`，162 行，Inno Setup 6.3+）：

- **一个安装包同时装 x64 与 ARM64**（`rcdesktop.iss:35-44` 按架构选源目录），
  发布流程里跑 Node 矩阵构建两个目标（`release.yml:21-28`）
- `PrivilegesRequired=lowest` —— **per-user 安装，不要管理员**（:34）
- 可选任务：桌面图标（默认不勾）、**开机自启**（写 HKCU `…\Run`，:95-96）、
  **把 `rcompose` 加进 PATH**（:92-93 + `NeedsAddPath` 幂等判断）
- ⚠️ **`CloseApplications=no`**（:54）—— 因为「关窗口 = 收进托盘，
  Restart Manager 关不掉它」，所以改成自己 `taskkill`（:118-137）：
  ```pascal
  procedure StopRunningApp();
  begin
    Exec(ExpandStr('{sys}\taskkill.exe'), '/F /IM {#AppExe}', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
    Sleep(500);   // Give Windows a moment to release the executable
  end;
  function PrepareToInstall(var NeedsRestart: Boolean): String; begin StopRunningApp(); Result := ''; end;
  function InitializeUninstall(): Boolean; begin StopRunningApp(); Result := True; end;
  ```
- 卸载时清 Run 键 + 从 PATH 里摘掉自己（:142-162）
- 发布产物**带 `.sha256`**（`release.yml:63`）

**`wslcc`**（`scripts/installer.iss`，139 行）：同样的 per-user + Inno Setup，
但因为有两种发布形态（ADR-0013），安装器里多了**框架依赖前置检测**
（:60-138）：查 `commonpf\dotnet\shared\Microsoft.WindowsDesktop.App\10.*` 与
注册表 `Microsoft.WindowsAppRuntime.*`，缺了就弹窗并打开下载页。

**⚠️ 两边都有、而本项目没有的一句大实话**：`DefaultDirName={localappdata}\Programs\…`
—— **per-user 安装**。这对本项目特别合适，因为本项目本来就没有任何需要管理员的操作
（唯一的提权点是 `wsl --update`，见 §6.3）。

### 6.2 本项目现状

`release.yml` 只做三件事：`cargo build --release --locked` → 确认 exe 存在 →
`Compress-Archive` → `gh release create`。产物是**裸 exe + zip**。

- 用户要自己决定放哪、自己建快捷方式、自己研究怎么开机启动
- **没有 `sha256`**（`rcdesktop` 有）
- **只有 x64**（`rcdesktop` 有 `aarch64-pc-windows-msvc` + `windows-11-arm` runner）
- ⚠️ **`release.yml` 里没有 `cargo test`** —— `rcdesktop` 的
  `release.yml:38-39` 是 `cargo test --locked` **再** `cargo build`。
  本项目靠"main 上 CI 绿了才打标签"这个人为纪律兜着，
  但**发布流程本身不验证**。

### 6.3 建议（P1，但依赖 §7 的托盘）

| 建议 | 说明 | 前置依赖 |
|---|---|---|
| **Inno Setup 安装包** | per-user、可选桌面图标、可选开机自启、卸载清理；`CloseApplications=no` + `taskkill` 那一招**直接适用**（本项目是 GUI、将来会有托盘） | 无，可先做 |
| **产物 `.sha256`** | 两行 PowerShell（`Get-FileHash`），零风险 | 无 |
| **ARM64 构建** | `aarch64-pc-windows-msvc` + `windows-11-arm` runner；GPUI 在 ARM64 上能否跑 **需实测**（D3D11 走的是驱动） | 需实测 |
| **release.yml 加 `cargo test`** | 在 `cargo build` 之前跑 `cargo test --workspace --exclude wslc-panel --locked --profile ci`（和 `ci.yml` 的 core job 同一条命令） | 无 |

### 6.4 自更新（`wslcc` 独有）

`WslcUpdateService.cs` 是一条完整链路：

```csharp
private const string ReleasesApi = "https://api.github.com/repos/iwvw/wslcc/releases/latest";
// ① 检查：12 秒超时；解析 tag_name / html_url / assets[]；按 asset 名精确匹配
// ② 比较：Version.TryParse 双方，latest > current（不是字符串比）
// ③ 下载：10 分钟超时，ResponseHeadersRead 流式 + IProgress<double> 进度
// ④ 安装：Process.Start(dest, "/SILENT /SP- /NORESTART", UseShellExecute = true)
```

- 有编译期开关选对应安装包（`App.xaml.cs:26-30`，配合 ADR-0013 的双形态）
- **启动后延迟 4 秒自动检查**（`App.xaml.cs:182-195`），且可在设置里关掉
- 设置页的交互是**两段式**（`About.*` 文案可见）：
  「检查更新」→「发现新版本 v{0}」→「下载并更新」+ 下载百分比 + 
  「将下载 v{0} 安装包并启动安装程序，过程中应用会退出」

> **对本项目的可行性**：本项目**没有安装包**，所以自更新只有"下载新 exe →
> 替换自己"这一种形态 —— 而 Windows 上**运行中的 exe 不能覆盖自己**。
> `wslcc` 能用 `/SILENT` 是因为有 Inno Setup 安装器代劳。
> **所以顺序应该是：先做安装包（§6.3），再做自更新。**
> 没有安装包就想自更新，只能"下载到临时目录 → 起一个脚本 → 退出 → 脚本替换 → 重启"，
> 那是四条独立的失败路径。

---

## 7. 【进程模型】单实例 / 托盘 / 开机自启

### 7.1 单实例（本项目 P0 补，因为它放大 §3 的风险）

**`rcdesktop`** 只用了 45 行（`src/single_instance.rs`），**命名事件**方案：

```rust
const EVENT_NAME: &str = "Local\\RcDesktop.ShowWindow";

pub fn acquire() -> Option<Instance> {
    let event = unsafe { CreateEventW(null, 0, 0, name.as_ptr()) };   // 自动重置事件
    if event.is_null() {
        // Can't coordinate; behave as a normal standalone launch
        return Some(Instance { event: 0 });
    }
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe { SetEvent(event) };     // 通知第一个实例"把窗口亮出来"
        return None;
    }
    Some(Instance { event: event as usize })
}

impl Instance {
    /// Calls `on_show` (from a background thread) whenever another launch asks this instance to show its window.
    pub fn listen(&self, on_show: impl Fn() + Send + 'static) {
        std::thread::spawn(move || loop {
            if unsafe { WaitForSingleObject(event as _, INFINITE) } != 0 { break; }
            on_show();
        });
    }
}
```

**三个值得抄的细节**：
1. **`CreateEventW` 的 `ERROR_ALREADY_EXISTS` 同时充当"锁"和"唤醒信号"** ——
   没有额外的 IPC、没有本地 socket、没有窗口消息。
2. **拿不到句柄时降级为普通启动**（`if event.is_null() → Some(Instance{event:0})`）——
   守卫失败不该让用户打不开程序。
3. **mock 模式跳过单实例**（`main.rs:25-35`）——
   这样 `--mock` 的预览能和真身并排开着对比。**本项目如果上 mock（§9.6），这个细节直接适用。**

`wslcc` 用 WinAppSDK 的 `AppInstance.FindOrRegisterForKey` + `RedirectActivationToAsync`
（`App.xaml.cs:97-118`），同样在异常时"按普通启动处理"。

> **为什么本项目这条是 P0 而不是 P2**：现在开 3 个面板 = 3 份 3 秒轮询 =
> **9 倍**的 `wslc` 并发压力。如果要上 §3.4 的串行闸门，
> **闸门是进程内的 —— 它挡不住第二个进程**。所以**单实例和闸门必须一起上**，
> 否则闸门只能挡住一半的问题。

### 7.2 托盘（两家都有，本项目没有）

`rcdesktop/src/tray.rs`（150 行，用 `tray-icon` 0.19 crate）：

菜单（:23-35）：
```
Open RC Desktop
─────
☑ Start with Windows        ← CheckMenuItem，勾选态与注册表同步
  Restart WSL (wsl --shutdown)   ← 卡住时的逃生舱
─────
  Quit
```
行为：
- **左键单击 / 双击 = 亮窗口**（:80-84）；右键才弹菜单（`with_menu_on_left_click(false)`，:42）
- **tooltip 显示 `"RC Desktop\n3 of 5 containers running"`**（:105-112）
- 关窗口 = 收进托盘（`main.rs:59-64`）：
  ```rust
  main_window.window().on_close_requested(|| {
      if !(settings::get().close_to_tray && tray::is_installed()) {
          let _ = slint::quit_event_loop();
      }
      CloseRequestResponse::HideWindow
  });
  ```
  ⚠️ **`tray::is_installed()` 这个判断很重要** —— 托盘装失败时，
  关窗口必须真的退出，否则**窗口没了、托盘也没有 = 进程变成幽灵**。
- 图标从 exe 资源读，读不到就**程序化画一个 32×32 的**（:123-149）
- `Restart WSL` 直接 `wsl --shutdown`（`CREATE_NO_WINDOW`，不等待）

`wslcc` 用 `H.NotifyIcon.WinUI`，并且**左键唤出迷你面板**（ADR-0010:19）：

> 托盘左键默认切换迷你面板，可在设置中关闭以恢复为直接打开主窗口；
> 托盘右键菜单亦提供显隐项。持久化键 `app.trayMiniPanel`（默认开启）。

**对本项目（GPUI）的成本**：GPUI 没有托盘 API。需要
`tray-icon` crate（它自己起一个 Windows 消息循环线程，或用 `Shell_NotifyIcon` 手写）。
**与 GPUI 事件循环的桥接是主要不确定性** —— `rcdesktop` 的做法
（`slint::invoke_from_event_loop`）不能照搬，GPUI 要换成
`cx.update(…)` 从后台线程投递（本项目 `app.rs:539-546` 已经有一个
"后台线程往 `Arc<Mutex<Vec<Event>>>` 塞、界面每 250 ms 取"的成熟模式，
**这正是托盘事件该复用的形状**）。

### 7.3 开机自启

`rcdesktop/src/settings.rs:61-143` —— 纯 Win32 注册表，`HKCU\…\Run`，值名 `"RC Desktop"`，
命令是 `"<exe 路径>" --autostart`（:76-79）。

**最值得抄的是 `refresh()`**（:135-143）：

```rust
/// Points an existing entry at this executable again, e.g. after the
/// portable folder was moved.
pub fn refresh() {
    if let (Some(current), Ok(expected)) = (read(), command()) {
        if current != expected { let _ = set_enabled(true); }
    }
}
```

**为什么需要它**：本项目是**绿色单文件 exe**，用户会把 exe 挪来挪去
（下载文件夹 → D 盘 → 桌面）。不自愈的话，注册表里留着一条指向旧路径的自启项，
开机时报"找不到文件"，用户只会觉得"这软件有病"。
→ **本项目尤其需要这条，因为本项目比 `rcdesktop` 更"绿色"。**

配合 `--autostart` 参数 + `start_minimized` 偏好，实现"开机只进托盘不出窗口"
（`main.rs:69-72`）。

### 7.4 建议（P0/P1）

| 建议 | 优先级 | 说明 |
|---|---|---|
| **单实例守卫**（命名事件 + 降级） | **P0** | 与 §3.4 闸门同批做；45 行量级 |
| **托盘 + 关闭到托盘 + 逃生舱菜单** | P1 | 依赖 `tray-icon` 与 GPUI 的桥接验证；菜单至少要有「打开面板 / 重启 WSL / 退出」 |
| **开机自启 + `refresh()` 自愈** | P1 | 纯注册表，无 UI 依赖；可在托盘之前先做 |
| 托盘 tooltip 显示"N / M 容器运行中" | P2 | 复用现有快照数据 |

---

## 8. 【工程习惯】文档制度：`wslcc` 这一块是三家最成熟的

### 8.1 ADR（15 篇，固定四段式）

`docs/adr/README.md` 是一张索引表；每篇结构固定：
**状态 / 背景 / 决策 / 后果**（部分带「后续」「相关」「实测记录」）。

**为什么这个格式有效** —— 拿 ADR-0011 当例子，它把"决策"与"实测"分开写：

> ### 实测记录（2026-09-27）
> 在 2.9.13 上逐项实测新增能力，全部符合官方契约：
> - `image list --digests --format json` 返回真实 `sha256:` 摘要；不加 `--digests` 时 `Digest` 为 `<none>`。
> - `pull <带 tag 引用> --all-tags` 被拒绝（`tag 不能与 --all-tags/-a 一起使用`，`E_INVALIDARG`）。
> - `container cp -L` 跟随符号链接得到目标内容（13 字节），不加 `-L` 得到链接本身（0 字节）。
>
> ### 两处必须注意的坑（已在实现中规避）
> 1. **`events` 不带 `--until` 会永久流式输出、不退出。**
> 2. **2.9.13 启用 RedirectionGuard 后拒绝穿越重解析点。**

**这恰好就是本项目 `AGENTS.md` §7「踩过的坑」在写的同一种东西。**
差别是：本项目的坑**越堆越厚**（AGENTS.md 已经 39 KB），而 `wslcc` 把它
**按主题拆成 15 篇、每篇带状态、可被"推翻"**（ADR-0003 的「后续（2026-09-29 复核）」
小节就是这么用的：原文不改写，加一节写结论变了）。

还有两篇特别值得本项目对照：

- **ADR-0007（构建策略）**：`dotnet build` 在**中文 Windows** 上编译 WinUI 3 会撞
  `XamlCompiler` 的 `WMC9999`（NullReference）。结论是"**CI 用英文 runner**"，
  并明确写"本机仅需 .NET 10 SDK"。→ 本项目是"**本机没有 Rust 工具链**"，
  **同一种"工具链不在本机"的处境，两家的解法都是"把验证搬到 CI"**。
- **ADR-0013（双发布形态）**：同一版本同时出自包含版与框架依赖版，
  文件名区分（`-framework` 后缀），**自更新按编译期常量选对应安装包**。
  → 本项目不必照抄（GPUI 是单文件，没有运行时依赖，这正是 README 里
  "无需 WebView2 或任何运行时"的卖点），**但它示范了"多个发布产物如何不失控"**。

### 8.2 PRD（7 篇）+ 验收标准

`docs/prd/` 是：产品概述 / 用户故事 / 前端需求 / 后端需求 / 数据库设计 /
**非功能需求** / 验收标准。

`06-non-functional-requirements.md` 是**量化**的，不是形容词：

| 维度 | 条目（摘） |
|---|---|
| 性能 | 启动到可交互 **≤ 3 s**；单次列表刷新 **≤ 2 s**；长操作异步、UI 不冻结；数据库操作毫秒级 |
| 可靠性 | 任何 CLI/API 异常**不得崩溃**；重启后设置与历史完整可读；**wslc 不可用时界面仍可浏览数据库内容**；不假设网络可用 |
| 安全 | **不存储凭据**；审计不记敏感参数（只记键名）；数据写用户配置目录，**不写安装目录** |
| 可维护性 | 分层单向依赖；服务接口化；**编译器警告清零**；构建可脚本化 |
| 可观测性 | 未处理异常写 `app-crash.log`；写操作进审计；拉取历史可回溯；仪表盘展示数据通道 |
| 兼容性 | 输出字段变化用多候选容错；CLI 为主通道 |
| 本地化 | 界面简体中文；**错误消息保留 wslc 原始错误码**（便于对照官方 issue） |

> ⚠️ **对本项目的一个直接提醒**：「**wslc 不可用时界面仍可浏览数据库内容**」
> 这一条，本项目**部分不满足** —— 3 秒轮询失败时页面会显示红色横幅，
> 但**已经拿到的数据会保留**吗？需要核。而 `wslcc` 因为有 SQLite，
> 能拿"上一次的快照"兜底（`ContainersViewModel.cs:118-124`
> 从 `QueryLatestSnapshotsAsync()` 里取端口，主数据源失败时至少端口还在）。

### 8.3 Release notes（17 篇 + 模板）

`docs/release-notes-template.md` 给了**两套模板**：正式发布版（
新增 / 改进 / 修复 / 破坏性变更 / 安装 / 完整改动对比链接）与
小版本（变更 / 安装），并写了使用约定：

> - 标题只写版本号，如 `v1.7.2`，不加前缀或后缀。
> - 版本号与 git tag 一致。
> - **小版本内容少时，可省略「安装」之外的空节，只保留实际有内容的节。**
> - 正文用简体中文，条目用短句，**避免营销式措辞**。

**对照本项目**：`AGENTS.md` §4 自己承认了这件事：

> ⚠️ Release 的说明是**固定模板**（`release.yml` 里那段 `$notes`），**没有 changelog**。
> 想让下载页显示"本版变化"，得在发布之后 `PATCH /repos/{o}/{r}/releases/{id}` 把正文补上…
> **发布页是用户唯一会读的地方，值得补。**

→ **这是全部建议里"成本最低 / 收益最直接"的一条。**

### 8.4 建议（P1，成本极低）

| 建议 | 做法 |
|---|---|
| **`docs/adr/` 目录** | 把 `AGENTS.md` §7 的坑与各 `PLAN-v*.md` 里"被推翻的选型"提炼成 ADR：固定 `状态/背景/决策/后果/实测记录`。**不必一次写完** —— 每踩一个新坑写一篇，半年后自然成型。现成的候选：`--cd` 的坐标系统一（PLAN 里有实测）、`wslc settings.yaml` 保留注释、GPUI `#[test]` 宏遮蔽、弹窗层序、`--session` 半成品（§4.1） |
| **`docs/releases/vX.Y.Z.md`** | 打标签前写一篇，`release.yml` 读它当 `--notes-file`，缺了就退回现有固定模板（**渐进式，不阻塞发布**） |
| **`docs/prd/` 非功能需求** | 至少把 README「已知限制」升级成一张量化表（启动 ≤ N s、刷新 ≤ N s、wslc 挂了界面还能看什么） |

---

## 9. 【体验】逐个功能的对照与取舍

### 9.1 操作进度面板（分步 + 日志 + 三态）

`OperationProgressPanel.cs`（125 行）是三态齐备的：

```csharp
public void Report(string? line) {       // 流式：更新阶段文本 + 追加日志
    _phase.Text = text;
    AppendLog(text, emphasize: true);
}
public void Complete(string summary) { _bar.Visibility = Collapsed; AppendSummary(summary, isError: false); }
public void Fail(string message)     { _bar.Visibility = Collapsed; AppendSummary(message, isError: true); }
public void Cancelled(string message){ _bar.Visibility = Collapsed; _phase.Text = message; }
```

细节：
- `MaxLogEntries = 300`，超了从**头部**摘（:103-104）
- 每次追加都 `ScrollToEnd()`（:120-124）
- 日志 `IsTextSelectionEnabled = true`（用户能复制错误）
- 完成/失败插一条分隔线 + 摘要（成功用 `TextFillColorPrimaryBrush`，失败用 `SystemFillColorCriticalBrush`）

**本项目现状**：「添加实例」页的 `InstallProgress`（`state.rs:3009-3213`）
已经有分步 + 进度 + 取消 + `apply(event)`，**深度不比它差**。
差距在**别的地方没有**：本项目「部署管理」「备份/恢复」「拉取镜像」
用的是各自的 `PullProgress` / `DeployProgress` / `ExportProgress`
（`state.rs:2512 / 2285 / 2564`）——**四套并行的进度模型**。

> → **不建议抄它的实现，建议抄它的"统一"。** 本项目可以考虑把
> `PullProgress` / `DeployProgress` / `ExportProgress` 收敛到一个
> `OperationProgress { phase, lines, outcome }`，减少 4 份重复的"阶段 + 行 + 结束"逻辑。
> **属重构，不是新功能，优先级 P2** —— 但它是 `views.rs`/`app.rs` 膨胀的成因之一。

### 9.2 骨架屏

`SkeletonGrid.xaml.cs`：一个 `IsActive` 依赖属性 + 750 ms 往返的不透明度脉冲
（`1 → 0.45`，`AutoReverse`，`RepeatBehavior.Forever`），激活时显示、停用时归 1。

**本项目现状**：有入场淡入（`modal_scrim` 140 ms、切页 120 ms）与「加载中…」按钮态，
**没有骨架屏**。对 3 秒自动刷新的面板，骨架屏的价值不大（数据大部分时间在）。
**优先级 P2，且要小心**：GPUI 的 `with_animation` 需要"稳定且互不相同"的 id
（`AGENTS.md` §7.5），骨架屏每行一个 id 会让 id 数量跟着行数变 —— 容易踩坑。

### 9.3 容器图标识别（`wslcc` 独有，且对本项目有特别价值）

`ContainerIconService.cs`（291 行）：

- **缓存**：`%LOCALAPPDATA%\WSLCC\icons`，TTL **7 天**，外加 `ConcurrentDictionary` 内存层
- **6 个候选路径**（:16-24）：`/favicon.ico`、`/favicon.svg`、`/favicon.png`、
  `/apple-touch-icon.png`、`/apple-touch-icon-precomposed.png`、`/logo.png`
- **先读 HTML 里声明的图标**（`<link rel="icon">`），支持 `data:` URI
- 体积上限 `512 KB`，超了跳过（:89）
- `UserAgent: WSLCC/1.5`，超时 6 秒

`MiniPanelViewModel.cs:231-255` 展示了怎么用：只有 `ShouldResolveIcon()` 为真
（大概是有 Web URL 的容器）才去解析，失败就 `ApplyIcon(null)`（**有兜底，不会卡**）。

> **对本项目的价值密度很高**：本项目的「部署管理」页会**推断运行时**
> （nginx / Java / Node.js / Go）、**读端口**（`inspect`）、**推出 Web 根**。
> 也就是说 —— **本项目已经知道哪些容器是 Web 服务、监听哪个端口了。**
> 那么"给这一行显示它自己的 favicon"需要的只是
> `http://localhost:<port>/favicon.ico` 这一个请求。
> 这会让「部署管理」的列表从"一堆路径"变成"一眼认出是哪个站"。
>
> ⚠️ **但有两个本项目特有的顾虑**：
> ① 本项目是**绿色单文件 exe**，"在 `%LOCALAPPDATA%` 建图标缓存目录"
> 与现有习惯一致（`prefs.json` 就在那儿），可以；
> ② **发网络请求到 `localhost:<port>`** 是本项目**当前没有的行为**
> （现有网络请求只有 registry 的那些，且都是用户点击触发的）。
> → **必须由用户显式触发**（比如部署详情里的「获取图标」按钮），**不要在轮询里偷偷发。**
> 本项目的安全叙事是"不依赖任何运行时、只调本地 CLI"，不该被破坏。

### 9.4 迷你悬浮窗（`wslcc` ADR-0010）

380×540 DIP，工作区右下角、距边 12 DIP，托盘左键唤出。ADR-0010 里有四个
**非显然的决定**，每一个都值得单独看：

1. **独立窗口而不是主窗口变形** —— 职责与生命周期不同（浮层高频轻量、
   主窗低频完整），独立窗口状态机简单，动画只作用于浮层，可常驻预热。
2. **开关只由托盘驱动，不做失焦自动收起** ——
   > 失焦隐藏与托盘点击切换存在竞态（点击任务栏图标会先触发窗口失活），
   > 去掉自动隐藏可从根上消除，行为也更可预测。
3. **层级插到任务栏正下方** —— `SetWindowPos(hwnd, Shell_TrayWnd, …)`，
   实现"从任务栏后方滑出、且不被其它窗口遮盖"。**顺序要求：
   `PlaceBelowTaskbar` 必须在 `BringToForeground` 之后。**
   降级：找不到任务栏就纯 topmost；**副屏任务栏类名不同
   （`Shell_SecondaryTrayWnd`），当前仅按主显示器工作区定位**（诚实标注了限制）。
4. **滑入滑出动画** —— `Window` 无内置动画，用逐帧 `SetWindowPos`：
   > 单常驻工作线程 + **代际（generation）快照，新请求作废在途动画而非新建线程**；
   > 每帧 `SWP_NOZORDER` 保证 z 序稳定不闪烁；
   > **展开不淡入（淡入会掩盖位移，看起来像凭空出现）**。
5. **材质** —— 自建 `SystemBackdrop` 把 `IsInputActive` **永久置 true** 驱动
   `DesktopAcrylicController`：
   > 内置 `DesktopAcrylicBackdrop` 在窗口非前台时退化为纯色，浮层不可用。

**对本项目（GPUI）的评估**：**成本很高，不建议现在做。**
第 3、4、5 条都依赖 Win32 层窗口操作与 WinUI 的 backdrop 体系，
GPUI 的窗口抽象未必暴露 `SetWindowPos` 级别的控制；
而且本项目 **`app.rs` 364 KB / `views.rs` 330 KB**，
再加一个"常驻独立窗口 + 自己的状态机"会显著加重维护负担。

**但 ADR-0010 的"问题陈述"值得记住**（:8）：

> 容器启停是高频操作，但主窗口体积大、层级低，查看状态和启停需要先切回窗口，路径较长。

→ 本项目将来若要缩短这条路径，**更便宜的替代品有三个**：
① 托盘菜单直接列"运行中的容器"（`rcdesktop` 的 tooltip 已经是雏形）；
② 全局快捷键唤起主窗口并跳到「当前运行」页；
③ 托盘 tooltip 显示 `N / M 运行中`（零交互成本）。
**先做 ③，再想要不要做浮层。**

### 9.5 多语言

`wslcc` 的做法：`AppStrings.cs`（**965 行**，`<auto-generated> from Resources.resw`）
两个 `IReadOnlyDictionary<string,string>`：`Zh` 与 `En`；`AppL.cs` 是 28 行的取值层：

```csharp
private static readonly IReadOnlyDictionary<string, string> Table =
    CultureInfo.CurrentUICulture.Name.StartsWith("zh", …) ? AppStrings.Zh : AppStrings.En;

public static string Get(string key) =>
    Table.TryGetValue(key, out var value) && value.Length > 0 ? value : key;   // 缺 key 回退成 key 本身
public static string GetFormat(string key, params object?[] args) { try { … } catch { return key; } }
```

`GetFormat` 的 `catch { return key; }` 是个务实的决定：**格式串写错了也不会崩**。

**本项目现状**：全中文硬编码，而且**文案里有 Markdown**（`**加粗**`、反引号），
渲染处必须过 `util::strip_md`（`AGENTS.md` §5，v0.9.13 一次补了 15 处）。

> ⚠️ **这意味着本项目做 i18n 的成本比 `wslcc` 高**：
> 文案拆进字典后，**Markdown 标记要不要一起进字典**？
> 如果要，每种语言得各自维护 `**`；如果不要，`strip_md` 与文案的耦合就更紧。
> → **建议：以「提取到字典、但只有一份 zh 表」的形式先落地**（为将来留口子，
> 现在不翻译），把硬编码字符串换成 `t("key")`。**这是纯机械改动，且能防错别字复现。**
> **优先级 P2**，且**不要**在没有第二种语言需求时硬做翻译。

### 9.6 mock 后端（`rcdesktop` 独有）

`src/wslc/mock.rs`（428 行）实现同一个 `WslcClient` trait，内部用
`Arc<RwLock<Vec<Container>>>` 保存可变状态，`with_latency(1200ms)` 模拟真实延迟。

开关有两条（`config.rs:11` + `main.rs:20`）：
```rust
pub mock_mode: std::env::var("RCDESKTOP_MOCK").map(|v| v == "1").unwrap_or(false),
// 或命令行
let use_mock = config.mock_mode || args.iter().any(|a| a == "--mock" || a == "-m");
```

**它换来的测试能力**（`tests/integration_tests.rs` —— 55 行跑完整生命周期）：
session 健康 → 列表 → stop（断言 `Exited(0)`）→ logs → inspect → restart
（断言 `Running`）→ pull（断言镜像出现）→ 列卷 → rm（断言数量 -1）。

**对本项目的诚实评估**：

| | 本项目 | `rcdesktop` |
|---|---|---|
| 真机输出夹具 | ✅ **44 个真机抓的 fixture**（包括 `ms_distribution_info.json` 18 KB、`registry_tags_nginx.json` 23 KB） | ❌ 只有 `system_info` 一个真样本在测试里 |
| 端到端流程测试 | ⚠️ `wslc_smoke.rs`（真机冒烟，CI 上跑不了） | ✅ mock 驱动，CI 上能跑 |
| 无 WSL 环境下跑**界面** | ❌ | ✅ `--mock` |

→ **本项目的"解析正确性"比 `rcdesktop` 强得多**（真夹具 > 编的 mock），
**不要为此改测试策略**。mock 唯一真正补的是**"能跑起界面看布局"**。

⚠️ **但本项目当前根本用不上这一条**，因为：
1. 本机**没有 Rust 工具链**（连编译都做不到，更别说跑界面）；
2. CI 是 Windows runner，**但 GPUI 走 D3D11，GitHub runner 无 GPU**，
   `cargo test` 也跑不到 UI 路径（这正是本项目把纯逻辑搬进 `wslc-panel-core` 的原因）。

→ **结论：mock 对本项目是"将来某天有了本地工具链才有价值"的东西。
优先级 P2，且要先确认 `wslc-panel-core` 的 `AppState` 能不能接受一个
"假的 `Wslc`"** —— 目前 `AppState::new(wslc: Wslc, wsl: Wsl)`（`state.rs:3825`）
吃的是**具体类型**，要 mock 就得先把 `Wslc` 抽成 trait，
**那是一次 45 个调用点级别的改动**。**不要为了 mock 而做这个重构**；
真要做，等有第二个理由（比如想在 CI 里跑"快照数据 → 渲染树"的测试）时一起做。

---

## 10. 功能矩阵（用于决定"还缺什么"）

| 能力 | wslc-panel | rcdesktop | wslcc | 本项目该不该补 |
|---|---|---|---|---|
| 容器列表/启停/重启/删除 | ✅ | ✅ | ✅ | — |
| 日志（快照） | ✅ | ✅ | ✅ | — |
| 日志**跟随** | ⚠️ 开独立控制台 | ❌ | ✅ 流式 | P2 |
| `exec` / `attach` | ✅（非交互 exec + 独立终端） | ✅（Windows Terminal） | ✅ | — |
| 镜像拉取（含 tag 列表/体积） | ✅ **（两家都没有 tag 下拉框 + 体积）** | ⚠️ 简单 | ✅ | — |
| Compose 编排 | ❌ | ✅（rcompose + 自研） | ✅ **（自研 + 事件门控）** | P2（大工程） |
| 部署管理（nginx/Java/Node/Go） | ✅ **（两家都没有）** | ❌ | ❌ | — |
| MySQL 数据库管理 | ✅ **（两家都没有）** | ❌ | ❌ | — |
| **`wslc events`** | ❌ | ❌ | ✅ | **P1（官方已 GA）** |
| 镜像/容器/卷/网络 prune | ⚠️ 容器 + 卷 | ✅ 五种 | ✅ 四种 | P2（补镜像/网络） |
| **能力探测（版本门控）** | ❌ | ❌ | ✅ | **P1** |
| **单实例** | ❌ | ✅ | ✅ | **P0** |
| **托盘** | ❌ | ✅ | ✅（+迷你窗） | P1 |
| **开机自启** | ❌ | ✅（+路径自愈） | ✅ | P1 |
| **自更新** | ❌ | ❌ | ✅ | P1（依赖安装包） |
| **安装包** | ❌ | ✅（x64+ARM64 单包） | ✅（两种形态） | P1 |
| 产物 sha256 | ❌ | ✅ | ❌ | **P1（两行代码）** |
| 审计日志 | ❌ | ❌ | ✅ | P2 |
| 一键反馈 / 诊断 | ❌ | ❌ | ✅ | **P1（成本低）** |
| 容器图标 | ❌ | ❌ | ✅ | P2 |
| 多语言 | ❌（全中文） | ❌ | ✅（Zh/En） | P2 |
| ADR 文档 | ❌（有 PLAN） | ❌（有 plan/spec） | ✅ **15 篇** | **P1（成本低）** |
| Release notes | ❌（固定模板） | ⚠️ `generate_release_notes` | ✅ **17 篇 + 模板** | **P1（成本最低）** |
| PRD / 量化非功能需求 | ❌ | ❌ | ✅ **7 篇** | P2 |
| mock 后端 | ❌ | ✅ | ❌ | P2（前置成本高） |
| ARM64 | ❌ | ✅ | ✅ | P2（需实测 GPUI） |

---

## 11. 落地路线（按依赖关系排序）

> **三批之内不要并行铺开** —— 本项目 `app.rs` / `views.rs` 是 364 KB / 330 KB 的
> 单文件，并行改多个主题会在这个仓库反复踩"同文件冲突"（`AGENTS.md` §9 的理由之一）。

### 第 1 批：正确性与稳定性（不改 UI 布局）

1. **实测复验 `wslc` 并发是否真的撞 `0x80070020`**（§3.4 的复验脚本）—— **先做这个**，
   结论决定第 2 步的形态（单车道 / 双车道 / 不做）。
2. **`wslc-core` 加进程内闸门 + sharing violation 重试**（§3.4）—— 只锁 `Wslc`，
   带"取不到闸门就超时"的保护，避免长任务占死车道。
3. **单实例守卫**（§7.1）—— 与第 2 步同批，否则闸门只挡得住一个进程。
4. **错误码识别 + 重解析点自检 + 指引文案**（§4.1）—— 新增
   `wslc-core/src/diagnostics.rs`；`error.rs` 加 `hint()`；UI 出 InfoBar。
5. **stats 轮询错误去重**（§4.3）—— 小改，防刷屏。

> 这 5 条的产出都能**纯逻辑测试**（`wslc-panel-core` / `wslc-core`），
> 所以**在有 Rust 工具链之前也能靠 CI 验证**（这正是本项目分层拆法的红利）。

### 第 2 批：分发与可达性

6. **产物 `.sha256`** + **`release.yml` 里加 `cargo test`**（§6.3）—— 半天量级。
7. **`docs/releases/vX.Y.Z.md` + release.yml 读它**（§8.4）—— 成本最低、立刻可见。
8. **`docs/adr/` 起步**（§8.4）—— 先把 `--session` 半成品（§4.1）与并发闸门
   两个决定写成 ADR，形成范式。
9. **Inno Setup 安装包**（per-user、可选自启、`taskkill` 那一招）—— 依赖第 3 批的托盘
   才能把 `CloseApplications=no` 用对，但**可以先出"不带托盘"的版本**。
10. **一键反馈 / 复制诊断信息**（§5.1）—— 独立、无依赖。

### 第 3 批：桌面应用完整度

11. **托盘（先验证 `tray-icon` ↔ GPUI 的桥接）** → 12. **开机自启 + `refresh()` 自愈**
→ 13. **自更新**（必须在安装包之后）。
14. **`wslc events` 页**（记得 `--until` 必须注入，且上界 = 当前秒 + 1，见 §4.2）。
15. **能力探测 `WslcCapabilities`**（§4 引用的 ADR-0011 做法）——
    和 14 天然同批（`events` 本身就是 2.9.13+ 的能力，正好是第一个门控对象）。

### 不做的（明确记录理由）

- **迷你悬浮窗**（§9.4）：GPUI 侧成本过高，替代品更便宜。
- **SQLite**（§5.2）：本项目规模用 JSON 足够；`wslcc` 用它是因为有 6 张表。
- **容器图标**（§9.3）：除非做成用户显式触发，否则破坏"只调本地 CLI"的叙事。
- **双发布形态**（ADR-0013）：GPUI 单文件没有运行时依赖，这是卖点不是缺陷。
- **照搬 `WslcClient` trait / `IWslc*Service` 接口层**（§2.3）：
  本项目 `AppState::new(wslc: Wslc, wsl: Wsl)` 吃具体类型，改 trait 是
  45 个调用点级别的重构，**没有第二个理由之前不做**。

---

## 12. 需要在本机复验的清单

> 本机**没有 Rust 工具链**（`AGENTS.md` §1），所以下面这些**只能列出方法，不能在这里得出结论**。
> 建议按这个顺序做，前两条是第 1 批的**前置条件**。

| # | 待验 | 方法 | 影响 |
|---|---|---|---|
| 1 | `wslc` 并发是否真的报 `0x80070020` | 并排起 8 个 `wslc list --format json`，抓 stderr | 决定闸门形态 |
| 2 | `wsl.exe` 是否也需要串行化 | 同样方法压 `wsl -l -v`（与 `wslc` 混跑、单跑各一次） | 决定闸门范围 |
| 3 | 长任务占死闸门的实际影响 | 上一个闸门后，`wslc pull` 期间看 3 秒轮询的排队延迟 | 决定单车道 vs 双车道 |
| 4 | `%LOCALAPPDATA%\wslc` 在本机是否重解析点 | `(Get-Item $env:LOCALAPPDATA\wslc).Attributes` | 决定 `0x800701c0` 指引是否"本机也适用" |
| 5 | 是否存在"退出码 0 但 stdout 报错" | 故意传非法参数（如 `wslc list --bogus`），看退出码与输出 | 决定是否要像 `wslcc` 那样扫文本 |
| 6 | `wslc events` 在本机版本可用性与 `--until` 行为 | `wslc events --since 1h --until <now+1>` 是否退出 | `events` 页前提（ADR-0011:41） |
| 7 | `tray-icon` 能否与 GPUI 事件循环共存 | 最小 demo，不开窗口只装托盘 | 决定第 3 批可行性 |
| 8 | GPUI 在 ARM64 / 无独显环境的 D3D11 | 需 ARM64 机器或云 runner | 决定是否出 ARM64 包 |
| 9 | 3 秒轮询失败时，界面是否保留上一次数据 | 停掉 wslc 服务，观察各页 | PRD「wslc 不可用时仍可浏览」是否成立 |

---

## 13. 一句话总结

**这两个项目教给本项目的不是功能，而是"桌面程序的另一半"**：

- `rcdesktop` 教的是**进程边界**——把外部 CLI 当成一个**有状态、会抢锁、会吐本地化错误**
  的对端，用一个队列 + 一次 trait 抽象把它关进笼子（`queue.rs` / `client.rs`）；
- `wslcc` 教的是**用户边界**——把用户会遇到的**每一种失败**都变成一句能照着做的话
  （ADR-0014 的重解析点、ADR-0006 的多候选字段、ADR-0011 的 `--until`），
  并把这件事**制度化**（15 篇 ADR + 7 篇 PRD + 17 篇 release notes）。

本项目的**数据层已经比它们都深**（真夹具、MySQL、部署管理、镜像 tag + 体积），
**缺的是把这份深度安全地交付到用户手里** —— 所以第 1 批（并发 + 单实例 + 错误码）
比任何新功能都重要。
