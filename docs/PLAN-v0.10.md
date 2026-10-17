# PLAN v0.10 —— 稳定性与可诊断性

> 依据：[`COMPARISON-references.md`](COMPARISON-references.md)（对 `rcdesktop` / `wslcc` 的源码级对比）。
> **按用户决定，本版不做托盘 / 开机自启 / 自更新 / 安装包。** 那几项挪到 v0.11+，
> 且自更新必须先有安装包（Windows 上运行中的 exe 不能覆盖自己）。
>
> 本文只写**要改什么、改在哪个函数、怎么验**。所有"当前行为"都带 `文件:行号` 取证过。

---

## 0. 三条硬约束（决定了方案长什么样）

### 0.1 本机没有 Rust 工具链 → 每一条都必须能被 CI 验证

`AGENTS.md` §1。所以：

- **纯逻辑必须写进 `wslc-core` / `wslc-panel-core`**，由 `core` job 的
  `cargo test --workspace --exclude wslc-panel` 跑；
- **`wslc-panel` 里不写测试** —— 那条命令排除了它，写在那里根本不会执行；
- 本方案里 A（闸门）、C（错误分类）的核心都是**纯函数/纯状态机**，
  这是刻意的：**它们能在没有 GPU、没有 wslc 的机器上跑断言。**

### 0.2 不能新增 crate，最好连 `Cargo.lock` 都别动

`AGENTS.md` §3：所有 cargo 步骤带 `--locked`，而**本机无法重新生成锁文件**
（要改就走 `gen-lock.yml` 那条路，多一次来回）。

这直接决定了两处**模块位置**：

| 需要什么 | 放哪儿 | 为什么 |
|---|---|---|
| `CreateMutexW` / `MessageBoxW`（单实例） | **`wslc-core`** | 它**已经**依赖 `windows 0.58`（`crates/wslc-core/Cargo.toml:25-29`，为 `GetDiskFreeSpaceExW`）。**加 feature 不改 `Cargo.lock`**（锁文件只记包与依赖边，不记 feature）。 |
| 重解析点检测 | **`wslc-core`** | 只要 `GetFileAttributesW` + `FILE_ATTRIBUTE_REPARSE_POINT`，都在**已启用**的 `Win32_Storage_FileSystem` 里 → **零改动**。 |
| 单车道闸门 | **`wslc-core`** | 纯 `std`（`Mutex` + `Condvar`）→ **零依赖**。 |

> ⚠️ 反例：如果把单实例写进 `wslc-panel-core`（按 `AGENTS.md` §2 那条
> "这个文件 `use gpui` 吗" 的判据，它**不** use gpui，本该放那儿），
> 就要给 `wslc-panel-core` 加一个 `windows` 直接依赖 ——
> 那会改动 `Cargo.lock` 里该成员的 `dependencies` 列表，`--locked` 直接红。
> **判据在这里要让位于"能不能改锁文件"。** 这条理由要写进模块注释。

### 0.3 「实测优先」→ 先跑 Gate 0，再决定 A 的形态

`AGENTS.md` §5。`ERROR_SHARING_VIOLATION` 这条是 `rcdesktop` 在**它的机器**上实测的
（`_ref/rcdesktop/README.md:27`），而本项目的实测表（`AGENTS.md` §7.3）
**没有这一条** —— 说明我们没撞到过，或者撞到了但被当成偶发错误。
**下面 §2 的五个实验是 P0 的前置条件**，结论不同，A 的实现也不同。

---

## 1. 现状复核（先纠正两件事）

### 1.1 ✅ 快照刷新路径**已经**是自洽的 —— 别重复造

上一份对比文档里"45 处调用无任何协调"说得太粗。实际读代码之后：

| 机制 | 位置 | 效果 |
|---|---|---|
| `state.busy` 守卫 | `app.rs:5732-5744` | 正在采的时候**不会并发再开一轮** |
| `refresh_again` 补跑 | `app.rs:5736` / `5805-5807` | 被挡下的请求**不丢弃**，本轮结束立刻补跑 |
| `tick` 忙则跳过 | `app.rs:5823-5825` | 自动刷新**不排队**，不堆积 |
| 采集内部串行 | `app.rs:5760-5763` | `load_snapshot` 之后才跑 `--status`，注释写明"避免并发起两个 wsl.exe" |

**这套比 `rcdesktop` 的 `tokio::interval` + `await` 更细**（至少它保住了
"操作完成后补跑"）。**这些不要动。**

### 1.2 ❌ 真正的并发发生在**子系统之间**

`busy` 是**每个子系统各自一份**，彼此不通气：

| 子系统 | 自己的忙标志 | 会不会和快照撞上 |
|---|---|---|
| 快照刷新 | `AppState::busy`（`state.rs:3602` 起） | — |
| 部署页状态列（**每条部署一次 `wslc exec`**） | `DeployState::busy`（`app.rs:5855-5868`） | **会**：`tick_deploy_status` 完全不看 `state.busy` |
| 数据库页 | `DbState::busy`（`app.rs:3028` / `3174` / `5233`…） | **会** |
| 日志 / exec 浮层 | `LogsState` / `ExecState` | **会** |
| 用户点的动作（启动/停止/删除/创建） | `run_pending` 一类 | **会** |
| `wsl.conf` 保存 | `WslConfState::busy`（`app.rs:6617`） | **会**（且它跑的是 `wsl.exe`） |

**这就是 A 要解决的问题，而且只能在 `execute()` 里解** ——
在 6 个调用点各加一次协调一定会漏（这也正是它当年被拆成 6 份的原因：
它们**本来就该**互不阻塞）。

### 1.3 两个"半成品"，本次给出处置

| 现状 | 证据 | 本次处置 |
|---|---|---|
| `--session` **实现了但没有任何真实调用点** | `cli.rs:161` 定义；`.session(` 只出现在 `cli.rs:1071/1089-1094` 的测试里 | **D**：先诚实标注，不接线（详见 D 节的理由） |
| 副标题显示"会话 #N xxx"，但读写**走默认会话** | `app.rs:7681-7685` + `state.rs:4004-4011` | 同上 |

### 1.4 错误横幅现在是**一句话变七行**

`load_snapshot`（`state.rs:591-667`）对 7 个数据源各 `push` 一条错误；
`app.rs:7706-7722` 把 `snapshot.errors` **逐条**画成 `text_xs` 红字 ——
`wslc` 一挂，用户看到的是 7 行几乎同义的话，**没有一句告诉他为什么、怎么办**。
这是 C 要解决的。

---

## 2. Gate 0：先做这五个实验（1 小时内可完成）

> 用一台**装了 WSL 3.0+** 的机器。全部只读、不改配置。结果直接决定 A1/A2。

```powershell
# ① 并发是否真的报 sharing violation？（本项目当前就是并发形态）
1..8 | ForEach-Object -ThrottleLimit 8 -Parallel {
  & "$env:ProgramFiles\WSL\wslc.exe" list --format json 2>&1 |
    Select-String -Pattern '0x80070020|sharing|另一个程序|正由另一'
} | Format-List
# 没有 -Parallel 就退化成 8 个 Start-Job，或用
#   Start-Process wslc.exe -ArgumentList 'list','--format','json' -NoNewWindow -RedirectStandardError err$_.txt

# ② 长任务 + 短命令并存（模拟"拉镜像时 3 秒轮询继续跑"）
$pull = Start-Job { & "$env:ProgramFiles\WSL\wslc.exe" pull alpine:latest 2>&1 }
Start-Sleep 2
1..20 | ForEach-Object { & "$env:ProgramFiles\WSL\wslc.exe" list --format json 2>&1 | Select-String '0x80070020|sharing' }
Receive-Job $pull

# ③ wsl.exe 会不会和 wslc.exe 互撞
1..6 | ForEach-Object -ThrottleLimit 6 -Parallel {
  & "$env:ProgramFiles\WSL\wslc.exe" list --format json 2>&1 | Select-String '0x80070020'
  & "$env:SystemRoot\System32\wsl.exe" -l -v 2>&1 | Select-String '0x80070020'
}

# ④ 本机的 %LOCALAPPDATA%\wslc 是不是重解析点（决定 C3 的指引是否"本机也适用"）
(Get-Item "$env:LOCALAPPDATA\wslc" -Force).Attributes
# 关心是否含 ReparsePoint

# ⑤ 有没有"退出码 0 但 stdout 说错了"的形态
& "$env:ProgramFiles\WSL\wslc.exe" list --bogus; "exit=$LASTEXITCODE"
& "$env:ProgramFiles\WSL\wslc.exe" system session list --format json; "exit=$LASTEXITCODE"
```

**结果怎么用：**

| 实验 | 结论 | 影响 |
|---|---|---|
| ① 命中 sharing violation | 并发是真问题 | A 必须做 |
| ① 完全不命中 | 可能已在 WSL 3.0.x 修掉 | A 降级为"只做 C 的重试"，闸门先不做（**写进 ADR，免得以后重做同样的调研**） |
| ② 长任务期间短命令失败 | 长任务也要进闸门 | 选 **A1（全部串行）** |
| ② 短命令全部正常 | 长任务可以放行 | 选 **A2（只串行短命令）** |
| ③ 也命中 | `Wsl` 也要进闸门 | A 的范围扩到 `wsl.exe` |
| ⑤ 命中 | 需要像 `wslcc` 那样扫文本判错（ADR-0006） | 追加 C4 |

---

## 3. 变更清单总览

| 编号 | 改动 | 新增文件 | 修改文件 | 优先级 | 依赖 |
|---|---|---|---|---|---|
| **A** | `wslc` 单车道闸门 + sharing violation 重试 | `wslc-core/src/lane.rs` | `cli.rs`、`error.rs`、`lib.rs`、`app.rs`(tick) | **P0** | Gate 0 ①②③ |
| **B** | 单实例守卫 | `wslc-core/src/single_instance.rs` | `wslc-panel/src/main.rs`、`wslc-core/Cargo.toml`、`lib.rs` | **P0** | 无 |
| **C** | 错误归并 + 错误码 hint + 重解析点自检 | `wslc-core/src/diagnostics.rs` | `error.rs`、`state.rs`、`app.rs`、`lib.rs` | **P0/P1** | Gate 0 ④⑤ |
| **D** | 会话标签不再误导 | — | `app.rs`、`README.md`、`cli.rs`(注释) | P1 | 无 |
| **E** | 诊断信息导出 | — | `app.rs`、`state.rs`、`views.rs` | P1 | C |
| **F** | 发布流程杂项 | `docs/releases/*.md` | `release.yml` | P1 | 无 |

**不新增任何 crate，不改 `Cargo.lock`，不需要升版本号。**
（B 会在 `wslc-core/Cargo.toml` 里给**已有**的 `windows` 依赖加两个 feature ——
这不是依赖变更，锁文件不记录 feature。）

---

## 4. A —— 单车道闸门 + 重试

### 4.1 问题与落点

`execute()`（`cli.rs:652`）是**所有**阻塞式 `wslc`/`wsl` 调用的唯一咽喉：
`Wslc::run_owned`（:212）、`run_cancellable`（:226）、`run_with_timeout`（:243）
全都汇到它；`Wsl::` 那几个（:485/:493/:500）也走它。**这是唯一一个"想漏也漏不掉"的位置。**

### 4.2 新文件 `crates/wslc-core/src/lane.rs`

```rust
//! 进程内 `wslc` 单车道。
//!
//! # 为什么需要它
//!
//! `wslc.exe` 用**文件锁**管它的会话数据库，并发调用可能失败
//! （`rcdesktop` 实测 `ERROR_SHARING_VIOLATION` / `0x80070020`，
//! 见其 README 的 "One detail worth knowing"）。
//!
//! 本项目的问题不在**同一个**刷新路径里 —— 那条路径已经有 `busy` 守卫
//! （见 `app.rs::refresh_inner`）。问题在**子系统之间**：
//! 快照刷新、部署页状态列（每条部署一次 `wslc exec`）、数据库页、
//! 日志浮层、用户动作，各自持有一份互不通气的 `busy`。
//!
//! 它们本来就该互不阻塞，所以「在每个调用点加协调」一定会漏；
//! 唯一不漏的位置是 `cli::execute`，也就是这里。
//!
//! # 为什么不用票号做严格 FIFO
//!
//! 想过。票号要处理"超时弃号"，否则退出的号会让后面的号永久卡死
//! （`serving` 永远到不了），得再引一个 `BTreeSet` 收尸。
//! 而实际场景里同时最多 8 个等待者、每个只占 10~30 ms，
//! 朴素互斥不会真的饿死谁 —— **不值得为此引入一套回收逻辑**
//! （而且这部分代码本机没法编译，越简单越安全）。
//! 代价写清楚：**不保证严格先进先出。**

use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// 等多久就放弃。**只决定超时，不决定先后。**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    /// 查询类（`list` / `info` / `stats` / `inspect` / `version`）。
    /// 用户正盯着界面，等不到就该干脆报错。
    Quick,
    /// 写操作与长任务（`pull` / `run` / 备份）。可以多等一会儿。
    Bulk,
}

impl Priority {
    /// 默认等待预算。
    ///
    /// `Quick` 取 5 秒：一次 `wslc list` 实测在百毫秒级，
    /// 5 秒还没轮到说明前面压着长任务，再等下去界面只会更旧。
    /// `Bulk` 取 5 分钟：`pull` 本身就可能几分钟，不能比它短。
    pub fn budget(self) -> Duration {
        match self {
            Self::Quick => Duration::from_secs(5),
            Self::Bulk => Duration::from_secs(300),
        }
    }
}

struct Lane {
    busy: bool,
}

static LANE: Mutex<Lane> = Mutex::new(Lane { busy: false });
static FREE: Condvar = Condvar::new();

/// 车道被占用（等到预算上限也没轮到）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Busy {
    pub waited: Duration,
}

/// 占用凭证。**故意不 `pub`**：见 [`acquire`] 的说明。
pub(crate) struct Guard;

impl Drop for Guard {
    fn drop(&mut self) {
        let mut lane = LANE.lock().unwrap_or_else(|e| e.into_inner());
        lane.busy = false;
        drop(lane);
        // `notify_all` 不是 `notify_one`：唤醒哪一个由调度器决定，
        // 一个 Bulk 等待者和一个 Quick 等待者同时醒着才有可能被公平对待。
        FREE.notify_all();
    }
}

/// 排队进入车道。
///
/// ⚠️ **不要在持有 [`Guard`] 期间再调 `Wslc` 的阻塞方法** —— 会自己等自己。
/// 这不是靠纪律保证的，是靠**类型**：`Guard` 不对 crate 外暴露，
/// `cmd::*` 里拿不到它，所以嵌套在结构上不可能发生。
pub(crate) fn acquire(priority: Priority) -> Result<Guard, Busy> {
    let budget = priority.budget();
    let deadline = Instant::now() + budget;
    let mut lane = LANE.lock().unwrap_or_else(|e| e.into_inner());

    loop {
        if !lane.busy {
            lane.busy = true;
            return Ok(Guard);
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(Busy { waited: budget });
        }
        let (next, timeout) = FREE
            .wait_timeout(lane, deadline - now)
            .unwrap_or_else(|e| e.into_inner());
        lane = next;
        if timeout.timed_out() && lane.busy {
            // 下一轮会走到 deadline 分支；这里不 return，
            // 是因为 `wait_timeout` 可能提前返回（虚假唤醒）。
        }
    }
}

/// 车道现在忙吗？给自动刷新用 —— 忙就跳过这一轮，别排队。
pub fn is_busy() -> bool {
    let lane = LANE.lock().unwrap_or_else(|e| e.into_inner());
    lane.busy
}

// ---- 测试 -------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn guard_serializes_two_threads() {
        // 用 AtomicUsize 记"同时在车道里的最大数量"，必须是 1。
        let inside = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let (inside, peak) = (inside.clone(), peak.clone());
            handles.push(std::thread::spawn(move || {
                let _g = acquire(Priority::Quick).expect("8 个短任务不该超时");
                let now = inside.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(5));
                inside.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for h in handles { h.join().unwrap(); }
        assert_eq!(peak.load(Ordering::SeqCst), 1, "车道里同时只能有一个");
    }

    #[test]
    fn times_out_when_the_lane_is_held() {
        let _held = acquire(Priority::Quick).unwrap();
        let started = Instant::now();
        // 预算压到 50 ms，免得真等 5 秒
        let err = {
            // acquire() 用 Priority::budget()，测试里走内部函数更省时
            acquire_with_budget(Duration::from_millis(50)).unwrap_err()
        };
        assert!(err.waited >= Duration::from_millis(50));
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(is_busy(), "仍然是有人在里面");
    }

    #[test]
    fn dropping_the_guard_frees_the_lane() {
        {
            let _g = acquire(Priority::Quick).unwrap();
            assert!(is_busy());
        }
        assert!(!is_busy());
    }

    #[test]
    fn busy_flag_is_per_process_not_per_thread() {
        // 车道是全局的：一个线程持有，另一个线程看到的 `is_busy` 必须是 true。
        let _g = acquire(Priority::Quick).unwrap();
        let seen = std::thread::spawn(is_busy).join().unwrap();
        assert!(seen);
    }
}
```

测试需要一个可传预算的入口（`acquire_with_budget`，`pub(crate)`），
`acquire` 只是它加 `priority.budget()` 的壳。

### 4.3 改 `cli.rs`

```rust
// Wslc 的三个阻塞入口都改成走带闸门的版本（其余不动）
pub fn run_owned(&self, args: &[String]) -> Result<CommandOutput> {
    self.gated(args, self.timeout, None, lane::Priority::Quick)
}

fn gated(
    &self,
    args: &[String],
    timeout: Duration,
    cancel: Option<Arc<AtomicBool>>,
    priority: lane::Priority,
) -> Result<CommandOutput> {
    // 闸门出错要变成**用户能读的错误**，而不是悄悄失败。
    let _lane = lane::acquire(priority).map_err(|busy| Error::Busy {
        program: WSLC_LABEL,
        awaited: busy.waited,
    })?;
    execute(&self.program, WSLC_LABEL, WSLC_NOT_FOUND_HINT, args, timeout, cancel)
}
```

**优先级怎么定**（只改这几个，不碰 45 个调用点）：

| 方法 | 闸门 | 理由 |
|---|---|---|
| `run_owned` / `run_cancellable` / `run_with_timeout` | `Quick` | 全是查询与短写操作 |
| `spawn_streaming`（`pull` / `logs --follow`） | **A1: `Bulk` / A2: 不进闸门** | 时长不可控，见 §4.4 |
| `spawn_to_file`（`mysqldump` 大输出） | **A1: `Bulk` / A2: 不进闸门** | 同上 |
| `spawn_in_new_console`（`attach` / `exec -it`） | **一律不进闸门** | 它只是**启动**一个控制台然后立刻返回；如果进闸门，用户开着的终端会一直占着车道。它和别的调用撞了由重试兜底。 |
| `spawn_detached`（`settings`） | 不进闸门 | 起编辑器，同样立刻返回 |
| `Wsl::` 全体 | **看 Gate 0 ③** | 实测没撞就不动（少一层风险） |

### 4.4 A1 vs A2：长任务要不要占车道

| | **A1 全部串行**（`rcdesktop` 的做法） | **A2 只串行短命令** |
|---|---|---|
| 正确性 | 最好（理论上不会撞） | 长任务仍可能与短命令撞 → 靠重试 |
| 代价 | **`pull` 期间自动刷新被挡住**，界面数据停止更新几分钟 | 界面照常刷新 |
| 需要额外 UI | ✅ 「另一个操作正在进行，已暂停自动刷新」 | ❌ |
| 何时选它 | Gate 0 ② 命中 | Gate 0 ② 干净 |

**推荐 A2 起步**：它没有 UI 成本，且重试能兜住残余碰撞。
**只有 ② 明确命中时才升到 A1**，并把那条提示一起做（否则用户会以为面板卡死了）。

### 4.5 重试（A 的另一半，与 A1/A2 无关，**必做**）

在 `execute()` 里，拿到输出**之后**判一次：

```rust
// execute() 末尾，构造 CommandOutput 之前
Ok(CommandOutput { .. })
```

改成：

```rust
/// 识别 `wslc` 的"抢锁"类失败。
///
/// 三个判据都来自 `rcdesktop/src/wslc/queue.rs` 的 `is_sharing_violation`，
/// 外加中文系统上的说法（本品是中文界面，用户多在这个环境）。
fn is_sharing_violation(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains("0x80070020")
        || lower.contains("error_sharing_violation")
        || lower.contains("used by another process")
        || text.contains("另一个程序正在使用")
        || text.contains("正由另一进程使用")
}

const RETRY_DELAYS: [Duration; 3] = [
    Duration::from_millis(200),
    Duration::from_millis(400),
    Duration::from_millis(800),
];
```

重试放在**闸门之内**（已持有 `_lane`）：这样重试期间不会又和别人撞上，
也不会因为等待而放大延迟。

⚠️ **只对阻塞式 `execute` 做重试**。`spawn_streaming` / `spawn_to_file`
**不做** —— 它们的输出是边跑边消费的，重来一次会让"已经显示出来的进度"
出现第二遍，用户会以为拉了两次。这条要写进注释。

### 4.6 新错误变体（`error.rs`）

```rust
/// 车道被别的操作占着，等到预算上限也没轮到。
///
/// 和 [`Error::Timeout`] 的区别：那个是**起来了但没跑完**，
/// 这个是**根本没轮到它起**。用户该做的事也不同
/// （等 vs 改超时/换命令）。
#[error("{program} 正被另一个操作占用（已等待 {awaited:?}）。请稍后再试；\
         若正在拉取镜像或备份，等它结束即可。")]
Busy {
    program: &'static str,
    awaited: Duration,
},
```

### 4.7 改 `app.rs` 的 `tick`（两行）

```rust
pub fn tick(&mut self, cx: &mut Context<Self>) {
    self.tick_deploy_status(cx);
    // 车道被长任务占着时**跳过这一轮**：排队只会积压出一串过期请求，
    // 采回来也已经不是用户要看的东西了。
    if self.state.busy || wslc_core::lane::is_busy() {
        return;
    }
    self.refresh_quiet(cx);
}
```

### 4.8 风险与回滚

| 风险 | 应对 |
|---|---|
| 闸门写错 → **自己等自己**（死锁） | `Guard` 不对 crate 外暴露；`cmd::*` 拿不到它 → 结构上不可能嵌套 |
| `wait_timeout` 虚假唤醒导致提前返回 | 循环里只在 `deadline` 超时才 `return Err`；测试覆盖 |
| 界面在长任务期间"数据不更新" | A2 下不存在；A1 下加提示条 |
| 闸门让某条路径变慢，用户以为卡了 | `Error::Busy` 有明确文案；且 `Quick` 预算只有 5 秒 |
| CI 上行为与真机不同 | 闸门是纯逻辑，`core` job 直接跑单测；**真机上还要点一遍**（§8） |

**回滚**：A 是三个独立文件（`lane.rs` + `cli.rs` 的 `gated` + `error.rs` 一个变体），
`git revert` 一个提交即可，不影响其他改动。

---

## 5. B —— 单实例守卫

### 5.1 为什么还要做（用户排除了托盘，但没排除这条）

- 开 3 个面板 = 3 份 3 秒轮询 = **3 倍** `wslc` 并发；
- **闸门是进程内的，挡不住第二个进程** —— 不做这条，A 只解决一半。

### 5.2 新文件 `crates/wslc-core/src/single_instance.rs`

```rust
//! 单实例守卫：第二次启动不再开第二个面板。
//!
//! # 为什么在 `wslc-core` 而不是 `wslc-panel-core`
//!
//! 按 `AGENTS.md` §2 的判据（"这个文件 `use gpui` 吗"），它该在
//! `wslc-panel-core`。但那样要给那个 crate **新加一个 `windows` 直接依赖**，
//! 而 `Cargo.lock` 记录了成员的 `dependencies` 列表 —— 本机没有 cargo，
//! 改不了锁文件，`--locked` 会直接红。
//! `wslc-core` **已经**依赖 `windows 0.58`（为 `GetDiskFreeSpaceExW`），
//! 给它**加 feature** 不改锁文件。所以放这儿。
//!
//! # 为什么用命名互斥体 + 消息框，而不是"唤起已有窗口"
//!
//! 唤起窗口要拿到那个窗口的 `HWND`。GPUI 的窗口句柄是它自己的
//! `AnyWindowHandle`，是不是能换成 Win32 `HWND` **没有验证过**（本机没工具链）。
//! 有把握的只有两步：拿互斥体判"是不是第一个"，用 `MessageBoxW` 告诉用户
//! "已经有一个在跑了"。**先做有把握的部分**；"唤起窗口"留到能实机验证时再加
//! （见 §5.5）。

use std::time::Duration;

/// 名字带 `Local\` 前缀 = **只在当前登录会话内**唯一。
/// 两个用户各开一个面板是合理的，不该互相挡。
const MUTEX_NAME: &str = "Local\\wslc-panel.single-instance";

pub struct Instance {
    /// 互斥体句柄。进程退出时由系统回收，但 `Drop` 里也显式关一次。
    handle: isize,
}

impl Drop for Instance {
    fn drop(&mut self) {
        if self.handle != 0 {
            unsafe { windows::Win32::Foundation::CloseHandle(handle_of(self.handle)); }
        }
    }
}

/// `Some` = 这是第一个实例；`None` = 已经有一个在跑。
///
/// **拿不到句柄时按"第一个实例"处理**（`rcdesktop`
/// `single_instance.rs:19-22` 同款降级）：守卫失败不该让用户打不开程序。
pub fn acquire() -> Option<Instance> { … }

/// 告诉用户"已经有一个在跑了"。
///
/// 发布版是 `windows_subsystem = "windows"`，**没有控制台** ——
/// 不弹这个框，用户双击第二次会看到"什么都没发生"，然后去点第三次。
pub fn warn_already_running() { … }
```

实现要点（三条**容易写错**的）：

1. **`GetLastError()` 必须在 `CreateMutexW` 之后立刻读**，中间不能插任何别的
   Win32 调用（否则错误码被覆盖）；
2. `CreateMutexW` 返回**非空句柄**即使它已存在 —— "已存在"是靠
   `ERROR_ALREADY_EXISTS` 区分的，**不能靠句柄是否为空**；
3. 拿到 `ERROR_ALREADY_EXISTS` 后仍然要 **`CloseHandle`**（我们那次 open 也拿到了引用）。

### 5.3 `Cargo.toml`（只加 feature，不动版本）

```toml
[target.'cfg(windows)'.dependencies]
windows = { version = "0.58", features = [
    "Win32_Foundation",            # GetLastError / CloseHandle / ERROR_ALREADY_EXISTS
    "Win32_Storage_FileSystem",    # 已有：GetDiskFreeSpaceExW + 新增 GetFileAttributesW
    "Win32_System_Threading",      # CreateMutexW
    "Win32_Security",              # CreateMutexW 的 SECURITY_ATTRIBUTES 参数类型
    "Win32_UI_WindowsAndMessaging",# MessageBoxW
] }
```

> `Win32_Security` 是给 `CreateMutexW` 的 `*const SECURITY_ATTRIBUTES` 参数用的。
> 旁证：`rcdesktop` 为 `CreateEventW` 也显式列了 `Win32_Security`
> （`_ref/rcdesktop/Cargo.toml:26-32`）。**这条是推的，编译一次就清楚。**

### 5.4 调用点 `crates/wslc-panel/src/main.rs`

```rust
fn main() {
    init_tracing();   // 先初始化日志：第二个实例也要留下"我为什么退出"的记录

    // 单实例守卫放在**打开任何窗口之前** —— 放在之后会先闪一下窗口再退出。
    let Some(_instance) = wslc_core::single_instance::acquire() else {
        tracing::info!("已有一个 wslc-panel 在运行，本次启动退出");
        wslc_core::single_instance::warn_already_running();
        return;
    };

    tracing::info!("wslc-panel 启动（版本 {}，构建 {}）", …);
    gpui_kit::application()…;
}
```

### 5.5 明确不做（留档，免得以后重复讨论）

「第二次启动把已有窗口提到前台」需要 `HWND`。可行性取决于 GPUI 是否暴露它
（候选：`Window::window_handle()` + `raw-window-handle` 的 `HasWindowHandle`）。
**本机没有工具链，无法验证**，所以本版只做消息框。
真要做得先写一个最小 demo 验证这条路 —— 那是独立的一小步，别和 B 混在一起。

---

## 6. C —— 错误归并 + 错误码提示 + 重解析点自检

分三小块，**可以分别提交**。

### 6.1 C1：`Problem` 取代裸 `String`

`Snapshot::errors: Vec<String>`（`state.rs:403`）只有 3 个读者
（`app.rs:7706/5776`、`state.rs:4912` 一条断言），改动面很小。

```rust
// crates/wslc-panel-core/src/state.rs
/// 一个"出问题的数据源"。
///
/// 为什么不是一个 `String`：界面要分两行显示 ——
/// 一行是**原始错误**（给懂的人看，也方便贴进 issue），
/// 一行是**能照着做的一句话**（`hint`）。
/// 混成一个字符串就只能二选一。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// 哪一段失败了，如 `"容器列表"`。
    pub what: &'static str,
    /// 原始错误。
    pub detail: String,
    /// 能照着做的一句话；`None` = 这条错误我们认不出来。
    pub hint: Option<String>,
}

impl Problem {
    pub fn new(what: &'static str, detail: impl std::fmt::Display) -> Self {
        let detail = detail.to_string();
        let hint = wslc_core::diagnostics::classify(&detail);
        Self { what, detail, hint }
    }
}
```

`load_snapshot` 里 6 处 `snap.errors.push(format!("xx：{e}"))` 改成
`snap.problems.push(Problem::new("xx", e))`；`list.warnings`（`state.rs:638`）
不是错误，保持 `String`（另放一个 `warnings` 字段，或 `Problem { hint: None }`）。

**归并显示**（`app.rs:7706-7722`）：

```rust
// 同一个 hint 的多条错误合并成一句 —— wslc 一挂，7 个数据源会同时失败，
// 用户看到的却是 7 行字面不同、含义相同的话。
let (headline, rest) = state.snapshot.group_problems();
```

`group_problems()` 是纯函数（进 `panel-core`，**可测**）：

```rust
/// 把问题按 `hint` 归组。
/// 返回 (一句话总结, 剩余明细)。剩余明细最多 3 条，其余折成"等 N 项"。
pub fn group_problems(&self) -> (Option<String>, Vec<String>)
```

测试要覆盖：7 条同 hint → 1 句 + "共 7 项"；2 组不同 hint → 2 句；
0 条 → `(None, [])`；全无 hint → 退回逐条列（保持现状行为）。

### 6.2 C2：错误码 → 一句话（`wslc-core/src/diagnostics.rs`）

```rust
//! 把 `wslc` / `wsl` 的**已知故障**翻译成"用户能照着做的一句话"。
//!
//! # 为什么值得单独一个模块
//!
//! 这些字符串是**从真实报错里抄出来的**，不是猜的。散在各处会失传；
//! 集中在一处，加一条只要改一张表 + 一个测试。

/// 已知故障 → 给用户的下一步。
///
/// 认不出来就返回 `None` —— **绝不硬编一句泛泛的"请检查配置"**，
/// 那比不说更糟（用户会以为我们知道了什么）。
pub fn classify(text: &str) -> Option<String> {
    // ① 重解析点 / RedirectionGuard（wslcc ADR-0014 实测）
    if text.contains("0x800701c0") || text.contains("不受信任的装入点") {
        return Some(
            "wslc 的数据目录是一个 junction/符号链接（通常是把 \
             %LOCALAPPDATA% 挪到别的盘造成的），wslc 拒绝穿过它。\
             解决：把 settings.yaml 里的 storagePath 指到一个真实目录，\
             或把 %LOCALAPPDATA%\\wslc 恢复成普通目录。"
                .to_owned(),
        );
    }
    // ② 会话不存在（wslcc ADR-0009 实测：会让**所有**操作失败）
    if text.contains("WSLC_E_SESSION_NOT_FOUND") {
        return Some(
            "指定的会话不存在。面板只用默认会话，如果你刚在别处终止了会话，\
             刷新一次即可。".to_owned(),
        );
    }
    // ③ 代理 / TUN 干扰（wslcc ADR-0003 与 WslcNetworkDiagnostics 的做法）
    if text.contains("E_UNEXPECTED") || text.contains("灾难性故障") {
        return Some(
            "这是 wslc 报的 E_UNEXPECTED，实测常见于开了 VPN / 代理的情况。\
             可先关掉代理再试；若持续出现，请把日志一起报上来。"
                .to_owned(),
        );
    }
    // ④ 抢锁（A 会自己重试，到这里说明重试也没救）
    if crate::cli::is_sharing_violation(text) {
        return Some(
            "另一个进程正在使用 wslc（可能是另一个面板实例，或命令行里的 wslc）。\
             等它结束再试。".to_owned(),
        );
    }
    None
}

/// `%LOCALAPPDATA%\wslc` 与给定的其它路径里，哪些是**重解析点**。
///
/// 纯 Win32 `GetFileAttributesW`，**不起进程、不读文件内容**，
/// 所以可以在采集里顺手做（微秒级）。
///
/// 为什么值得提前查：上面的 ① 一旦发生，面板**所有**功能都不可用，
/// 而错误信息是一个 HResult。提前查出来就能在界面上直接给出解决办法。
pub fn reparse_points(paths: &[std::path::PathBuf]) -> Vec<std::path::PathBuf>
```

**测试**（`classify` 是纯函数，好测）：

```rust
#[test] fn classify_recognizes_reparse_guard() { … }
#[test] fn classify_recognizes_session_not_found() { … }
#[test] fn classify_recognizes_e_unexpected_and_vpn_hint() { … }
#[test] fn unknown_text_yields_none() {
    // 关键：认不出来时**不许**给一句泛泛的提示
    assert_eq!(classify("some random failure"), None);
}
```

`reparse_points` 用**普通目录**做负向断言（不需要权限）；
正向断言需要 junction，用 `cmd /c mklink /J` 建（**不需要管理员**），
标 `#[ignore]`，并说明"在 CI 上跑要建临时 junction，属于写盘操作，默认不跑"。

### 6.3 C3：把自检结果带进快照

`Snapshot` 加一个字段：

```rust
/// 启动自检：数据目录是不是重解析点。
///
/// 放在快照里而不是单独查一次：`GetFileAttributesW` 是微秒级，
/// 跟着 3 秒的刷新一起做完全够，而且**用户把目录改回来之后会自动消失**。
pub untrusted_paths: Vec<std::path::PathBuf>,
```

渲染：`app.rs` 的横幅区加一个**独立的警告块**（不是红色错误块 ——
它只是"你的某个设置会让功能不可用"，不是"刚才那次调用失败了"）：

```rust
if !state.snapshot.untrusted_paths.is_empty() {
    // 黄色警告条 + 一句话 + 命中的路径（最多 2 条，其余折成"等 N 个"）
}
```

### 6.4 C4（条件项，只在 Gate 0 ⑤ 命中时做）

像 `wslcc` 那样**同时看退出码和文本**：`CommandOutput::into_result()`
（`error.rs:86-97`）现在是 `code == Some(0)` 就 `Ok`。若 ⑤ 证明存在
"退出码 0 但 stdout 有报错"，则加：

```rust
pub fn into_result(self) -> Result<Self> {
    if self.success() && !self.looks_like_an_error() {
        Ok(self)
    } else { … }
}
```

判据抄 `WslcRunner.cs:221-234` 的四条：`灾难性故障` / `Catastrophic` /
`错误代码` / `Error code`，外加 `\bE_[A-Z0-9]+\b`。

⚠️ **不做这个之前先确认它不会误判**：`wslc logs` 的输出里可能**本来就**有
`error:` 开头的内容（容器自己的日志！）。所以判据只能是
"错误代码："这种 wslc **自己的**措辞，**不能**用 `error:` 开头。
这一条要写进注释和测试。

---

## 7. D / E / F

### 7.1 D —— 会话标签不再误导（P1，最小改动）

**现状**：`app.rs:7681-7685` 的副标题是 `会话 #1 wslc-cli-76434 · 构建 abc`，
而所有读写**走默认会话**（`.session(` 无真实调用点）。

**为什么本版不接线**（不选"加会话选择器"）：
接线需要 `AppState` 存会话 id、`load_snapshot` 用会话化过的 `Wslc`、
还有 ADR-0009 那条"名字不存在会让**所有**操作失败"的存在性校验 ——
这是一个带 UI 的功能，不是修 bug。**本版只消除"看着像在用、其实没用"的误导**：

```rust
// 有多个活动会话时才加那半句说明 —— 只有一个会话时它本来就是准的，
// 加说明只是噪音。
let subtitle = if count > 1 {
    format!("会话 {} · 构建 {} · 操作走默认会话", …, crate::short_build_sha())
} else {
    format!("会话 {} · 构建 {}", …)
};
```

顺带：
- `cli.rs:161` 的 `session()` 上加一段注释，写清**本仓库没有任何真实调用点**
  和"接的时候必须带存在性校验（否则所有操作报 `WSLC_E_SESSION_NOT_FOUND`）"；
- `README.md`「已知限制」加一条。

### 7.2 E —— 诊断信息导出（P1）

对着 `wslcc` 的 `IssueReportBuilder` 做**内容**，但落地方式要避开未验证的 API。

内容（生成纯文本，`panel-core` 里的纯函数 → **可测**）：

```
wslc-panel 0.9.x（构建 abc1234）
OS: Windows 10.0.26200 …  架构: x86_64
wslc: 3.0.1.0             wsl: 2.6.1.0
会话: #1 wslc-cli-76434
数据目录: C:\Users\x\AppData\Local\wslc（普通目录）
存储路径: …（来自 settings.yaml）
自检: 无重解析点
最近错误: 3 条（去重后 1 条）
  [容器列表] ……
--- 日志末尾 40 行 ---
……
```

导出方式：**写文件 + 打开所在文件夹**（`spawn_detached` 已有，`explorer.exe /select`），
文件名 `wslc-panel-diagnostics.txt`，落在 `%LOCALAPPDATA%\wslc-panel\`。

> ⚠️ 「复制到剪贴板」更顺手，但 GPUI 的剪贴板 API（`App::write_to_clipboard` /
> `ClipboardItem`）**本机无法验证签名**。所以**先做写文件**（一定能跑），
> 剪贴板等实机确认后再加一个按钮。这条别写成"顺便做了"。

入口：「关于」页一颗按钮 `复制/导出诊断信息`。

### 7.3 F —— 发布流程三件小事（P1，不用编译 Rust）

1. **`release.yml` 补 `cargo test`**：在「编译 release」之前插一步
   ```yaml
   - name: 纯逻辑测试（与 ci.yml 的 core job 同一条命令）
     run: cargo test --workspace --exclude wslc-panel --locked --profile ci
   ```
   `rcdesktop` 的 `release.yml:38-39` 就是先 `cargo test --locked` 再 `cargo build`。
   现在本项目是"靠 main 上 CI 绿了才打标签"这个人为纪律兜着 —— 发布流程本身不验证。

2. **产物带 `.sha256`**：两步 PowerShell
   ```powershell
   (Get-FileHash "dist/$env:BIN_NAME.exe" -Algorithm SHA256).Hash.ToLower() + "  $env:BIN_NAME.exe" |
     Out-File -Encoding ascii "dist/$env:BIN_NAME.exe.sha256"
   ```
   并加进 `upload-artifact` 与 `gh release upload`。

3. **release notes 落地**：新建 `docs/releases/vX.Y.Z.md`，
   `release.yml` 里 **有就读、没有就退回现在那段 `$notes`**（渐进式，不阻塞发布）。
   这件事 `AGENTS.md` §4 自己已经承认了（"发布页是用户唯一会读的地方，值得补"）。
   模板直接用 `wslcc` 那张表的结构（新增/改进/修复/破坏性变更/安装），
   但**去掉"安装"节**（本项目没有安装包，就是下载 exe 直接跑）。

---

## 8. 明确不在本版范围

| 项 | 理由 |
|---|---|
| 托盘 / 开机自启 / 自更新 / 安装包 | **用户明确排除**。注：自更新依赖安装包（Windows 上运行中的 exe 不能覆盖自己），将来做要按这个顺序。 |
| `wslc events` 页 + 能力探测 | **没有消费者就会变成第二个 `--session`**。要做得一起：`capabilities`（按 `wslc version` 门控，ADR-0011 的做法）与 `events`（必须注入 `--until`，上界 = 当前秒 + 1，否则永久流式）**同批**。 |
| mock 后端 | 前置成本是"把 `Wslc` 抽成 trait"——`AppState::new(wslc: Wslc, …)` 吃具体类型，那是 45 个调用点级别的重构。**没有第二个理由之前不做。** |
| SQLite / 审计日志 | 本地单用户工具，现有 JSON 持久化够用。一等公民是"报 bug 时能说清环境"，那是 E。 |
| 多语言 | 全中文是对的；真要做得先把散落在 `views.rs` 的文案提取成字典（还要处理 Markdown 与 `strip_md` 的耦合）。**没有第二种语言的需求就不做。** |
| 容器图标 | 会给本项目**引入当前没有的行为**（向 `localhost:<port>` 发 HTTP 请求）。与"只调本地 CLI"的叙事冲突。 |
| 迷你悬浮窗 | GPUI 侧依赖 Win32 窗口层序与按帧位移，成本远超收益。 |
| ARM64 构建 | GPUI 在 ARM64 上的 D3D11 需实机验证，无法在本机确认。 |

---

## 9. 实施顺序（每步一个提交，都可独立回滚）

| # | 内容 | 前置 | 验证方式 |
|---|---|---|---|
| 0 | **跑 Gate 0 的五个实验**，把结论贴进 issue/PR 描述 | — | 命令输出 |
| 1 | `diagnostics.rs`：`classify` + 单测（C2 的纯函数部分） | 0 | `core` job |
| 2 | `lane.rs` + 单测（**不接线**，先让测试绿） | 0 | `core` job |
| 3 | `cli.rs` 接闸门 + `Error::Busy` + 重试；`tick` 加 `is_busy` | 2 | `core` + `ui` job |
| 4 | `Problem` + `group_problems` + 横幅改渲染（C1） | 1 | `core` + `ui` job |
| 5 | `reparse_points` + `Snapshot::untrusted_paths` + 警告条（C3） | 1 | `core`（负向断言）+ 实机 |
| 6 | `single_instance.rs` + `main.rs` 调用点（B） | — | `ui` job + **实机双击两次** |
| 7 | D（会话标签）+ README/AGENTS 补文 | — | `ui` job |
| 8 | E（诊断导出，纯函数 + 按钮） | 4 | `core` + 实机 |
| 9 | F（release.yml 三步 + 一篇 notes） | — | 手动 dispatch 一次 release 看产物 |

**1~3 是核心**，做完就已经覆盖了本次分析里风险最高的部分。
4~9 可以按需插入，彼此不冲突（都在不同文件，除了 4/5/7 都碰 `app.rs` 的
横幅/副标题那一段 —— **别并行做**）。

---

## 10. 验收标准

> 勾选项都要能在**没有 GPU** 的 CI 上判定，或者明确标注"需实机"。

### A（闸门）
- [ ] `lane::tests` 4 条全绿（串行、超时、释放、跨线程可见）
- [ ] `cargo test --workspace --exclude wslc-panel --locked --profile ci` 通过
- [ ] 全仓库 `grep` 确认**没有**第二个 `lane::acquire` 调用点（只有 `cli.rs`）
- [ ] 【实机】3 秒轮询 + 手动点「停止全部」+ 打开部署页，界面不出现 `sharing violation`
- [ ] 【实机】自动刷新在 `wslc pull` 期间**不刷出红字**（A2）

### B（单实例）
- [ ] 【实机】双击第二次 → 弹「已经有一个在运行」，不出现第二个窗口
- [ ] 【实机】日志里第二次启动有 `已有一个 wslc-panel 在运行` 一行
- [ ] 拿不到互斥体时（模拟：同名对象被占）**仍然能启动**（降级路径）

### C（错误可读）
- [ ] `classify` 4 条单测全绿，含"认不出来返回 `None`"
- [ ] `group_problems` 覆盖：7 条同因 → 1 句 + 「共 7 项」；两组不同因 → 2 句；0 条 → 空
- [ ] 【实机】把 `%LOCALAPPDATA%\wslc` 换成 junction → 顶部出现黄色警告 + 一条可照做的指引（做完改回来要能自动消失）
- [ ] 【实机】`wslc` 不存在时，横幅不再是 7 行同义红字

### D/E/F
- [ ] 多个活动会话时副标题出现「操作走默认会话」；只有一个时不出现
- [ ] 【实机】关于页导出诊断文件，内容包含构建号、`wslc --version`、自检结果、日志尾部
- [ ] `release.yml` 在打标签时先跑 `cargo test`，产物里有 `.sha256`
- [ ] 存在 `docs/releases/vX.Y.Z.md` 时发布页正文取自它；不存在时退回模板

---

## 11. 需要同步更新的文档

- `docs/wslc-schema.md`：Gate 0 ①⑤ 的结论（**无论命中与否都要写** ——
  "没有命中"本身就是最有价值的记录，能防止下次有人重新调研一遍）
- `AGENTS.md` §7.3 的实测表：加一行 `wslc` 并发行为
- `AGENTS.md` §7.1：加一条"闸门在 `cli::execute` 里，不要在调用点另加锁"
- `README.md`：已知限制（会话标签的说明）、`--session` 的现状
- 新建 `docs/adr/0001-wslc-single-lane.md`：把 Gate 0 的结论 + A1/A2 的取舍写成
  第一篇 ADR（格式照 `wslcc` 的 `状态/背景/决策/后果/实测记录`）。
  **这比写进 PLAN 更耐久** —— PLAN 是"当时打算做什么"，ADR 是"为什么最终这么做"。
