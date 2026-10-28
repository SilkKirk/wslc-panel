# 全仓审查清单

> **为什么单独一个文件**：`AGENTS.md` 已经 65052 字节，而工作区指令预算是
> **65536** —— 只剩 484 字节。往那儿加任何一节都会把 §9 挤出预算，
> 表现为"子代理那节被截断"。所以这份清单住在这里，`AGENTS.md` §5 末尾
> 只留一行指针。

"按规范审一遍"是个没法执行的指令。下面这些是**能 grep 出来**的判据，
逐条跑比凭印象翻文件可靠得多。数字是 2026-10-28 全仓审查时的实测值，
**会随代码变化**；对不上时先确认是实现变了还是规矩破了。

---

## 一、能机械跑的（8 条）

| # | 规矩 | 判据 | 期望（2026-10-28） |
|---|---|---|---|
| 1 | §7.5 滚动容器要有 `.id()` | 每处 `overflow_y_scroll()` / `overflow_x_scroll()` 往上 8 行内有 `.id(` | 0 违规 |
| 2 | §7.5 不用 `.disabled()` | `\.disabled\(\|Disableable` | 命中**全是注释** |
| 3 | §7.5 没有自绘模态 | `modal_scrim\|modal_card` | 命中全是注释（3 处） |
| 4 | §7.5 浮层五处对齐 | `open_shell_dialog(` / `shell_view(` / `DialogKey::ALL` 元素 / `is_open` 臂 / `open_dialog_for` 臂 | **五个数都相等**（19） |
| 5 | §5 行内最多 4 个按钮 | `container_row_actions` 里 `actions.push` 次数 | ≤ 4（含「更多」= 5 次 push 里有一次是菜单） |
| 6 | §5 文案过 `strip_md` | `.child("…")` 字面量（**排除注释行**）里含 `**` 或反引号 | 0 |
| 7 | §2 没有假 API | `pub fn` / `pub struct` 在其他文件里零引用 | 0（类型推断 / 字段用法会误报，要人工看） |
| 8 | §5 导入要逐项 | `use super::…::common::\*` 之类 glob | 只允许 `prelude::*` |

### 判据 1 的现成实现

```powershell
$panel = (Get-ChildItem -Recurse -Filter *.rs crates\wslc-panel\src).FullName
foreach ($f in $panel) {
  $ls = [System.IO.File]::ReadAllLines($f)
  for ($i=0; $i -lt $ls.Count; $i++) {
    if ($ls[$i] -match 'overflow_[xy]_scroll\(\)') {
      $s = [Math]::Max(0, $i-8)
      if (($ls[$s..$i] -join "`n") -notmatch '\.id\(') {
        $f.Replace("$PWD\crates\wslc-panel\src\","") + ":" + ($i+1)
      }
    }
  }
}
```

（漏 `.id()` 其实是**编译错误**，`ui` job 会拦住 —— 这条只是双保险。）

### 判据 7 的两个已知误报

"零引用"的常见假阳性，**不要照着删**：

- **类型推断用法**：`let caps: X = f();` 之后 `caps.field` —— 名字只出现在
  定义处和字段访问处，粗扫会判成没人用；
- **只被测试按名字用**：那种要三件套（`#[allow(dead_code)]` + `pub(crate) fn`
  + `#[cfg(test)] pub(crate) use`），见 `AGENTS.md` §2 第 7 条。

真删除之前必须逐个确认"连测试都没提过"。

---

## 二、只能人工读的（跑不出来）

这一节才是审查真正花时间的地方。2026-10-28 那轮抓到**两个真 bug**，
都出在这里。

### 1. 注释与实现是否一致

**判据**：看到"**所以 X 时也要 Y**""**导出时要读这个**"这类句子，
就去 X 的代码路径上确认 Y 真的发生了。

实测抓到：

| 位置 | 注释说 | 实现 | 后果 |
|---|---|---|---|
| `sql_table.rs` 的 `order` 字段 | "导出的顺序该跟屏幕上一致 —— 所以导出时也要读这个" | `export_sql_result_csv` 读的是原始 `rows`，一次没碰过 `order` | 点列头排序后导出，得到一份**看起来没排过**的 CSV |
| `wslc-db/src/my.rs` 的 `classify` | "TLS 握手失败在 mysql crate 里也走 `IoError`" | TLS 判断写在最后的 `_ =>` 里，而 `IoError(_) => Connect` 先把它截走了 | TLS 失败被说成"连不上服务器"，用户去查网络 |

两条都是"注释是对的、实现没跟上" —— 而注释写得越肯定，后来的人越不会去核。

### 2. 同一件事在两处写法不同

**判据**：同一个判据出现两次以上时，**跨文件比对**写法。

实测抓到：`pg.rs` v0.17.0 修过 `contains("tls")` 的大小写问题
（注释里还留着"这是判据归一化而不是多加一个关键词"），
而 `my.rs` 一直是镜像版的全大写 `contains("TLS")` —— 改一处忘一处。
现在两边都先 `to_lowercase()`。

同类值得定期比对的有：

- `pg.rs` / `my.rs` 的错误归类（错误号 → `DbErrorKind`）；
- 各域的 `spawn_*` 包装（超时、取消、事后刷新是否一致）；
- `classify` / `explain_*` 这类"把错误翻成人话"的函数。

---

## 三、这轮修掉的（供回溯，不是待办）

| 提交 | 修了什么 |
|---|---|
| `c25594c` | 提交型浮层页脚无条件关窗（4 个臂里 3 个错）；`core.rs` 里 4 组只属于单域的助手各归其位；`explorer.exe` 4 份壳收成 1 份；`start/stop_deployment` 逐行同形的 60 行收成一个枚举；2 处 `db.busy` 静默 return |
| `6205643` | MySQL 的 TLS 错误归类（见上表） |
| `0836caf` | 2 个零调用点的 `pub fn`；README 里已不存在的「终止会话」 |
| `15818c8` | `state/mod.rs` 的 `//!` 块被空行截断 |
| `de36e0b` | CSV 导出没按屏幕排序（见上表，带 2 条新测试） |
| `19a25f1` | 2 处"选中变主色"按钮排补上"为什么不用 `RadioGroup`" |
| `63d826a` | `blocks.rs` 的 `common::*` 收成逐项（全仓唯一一处 glob） |
| `818152f` | `kv`/`kv_block` 与官方 `DescriptionList` 的分工 |
| `04edea8` | 2 处测试条数与实现对齐 |

---

## 四、仍然没有覆盖的

- **`views/` 的渲染结果**：`wslc-panel` 不在任何测试目标里
  （`cargo test --workspace --exclude wslc-panel`），所以布局错位、
  颜色不对、hover 没反应 —— 这类**只能在本机点一遍**（`AGENTS.md` §8）；
- **真实交互**：浮层的层序、焦点、Esc 键、点遮罩，CI 一个都测不到；
- **MSRV**：CI 用浮动 `stable`，`rust-version = "1.85"` 那个数字**无人校验**。
