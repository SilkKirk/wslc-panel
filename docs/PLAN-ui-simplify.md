# PLAN —— 界面第一批：已有信息上屏 + 去掉噪声

> 状态：**已实施（待 CI 编译 + 实机验证）**。实施记录见文末 §14。
> 范围：只做「第一批」。**不改**概览页那 4 个环；**不合并**网络/卷为「资源」tab。
> 前提：**不动采集层** —— 不新增 `wslc` 命令、不改 `load_snapshot` 的采集顺序、
> 不引入新 crate。本批只消费 `Snapshot` / model 里**已经有**的字段。

---

## 0. 一句话

不新增任何采集，把已经躺在数据模型里、但界面上没画的字段画出来；
同时把四类噪声收掉：**常驻错误横幅、长解释文案、逐行红色删除、重复概览**。

---

## 1. 本批明确不做什么

| 不做 | 为什么 |
|---|---|
| 4 个圆环改成指标条 | 用户已确认保留圆环 |
| 网络/卷合并成「资源」tab | 用户已确认不合并 |
| 卷/网络 `inspect`、创建网络、镜像/网络 `prune` | 要新采集，留给 P1 |
| 会话切换器（`--session`） | 要改调用器与全局状态，留给 P1 |
| 批量选择 / 批量启停 | 交互与状态都更重，留给 P1 |
| 日志搜索/下载、`wslc events`、托盘/自启/自更新 | 不在本批 |
| 880px 下容器行操作列的裁切 | 需要横向滚动/折叠列，单独一批；本批只保证镜像/网络/卷的新列在窄窗下不更差 |

---

## 2. 变更清单

| 编号 | 改动 | 主要文件 | 要新采集？ |
|---|---|---|---|
| A1 | 容器页：搜索 + 状态筛选 + 排序 | `wslc-panel-core/src/listview.rs`（新）、`state.rs`、`app.rs`、`views.rs` | 否 |
| A2 | 镜像/网络/卷：删除收进「更多」；实例页去掉重复概览 | `views.rs`、`columns.rs` | 否 |
| A3 | 已有信息上屏：镜像使用数/总占用、卷 InUse、网络 IPv4/内部、概览存储行、容器详情 | `views.rs`、`state.rs`、`columns.rs` | 否 |
| A4 | 错误横幅折叠 + 页头改「最后刷新」 | `app.rs`、`util.rs` | 否 |
| A5 | 子页面高亮父导航项 | `state.rs`、`app.rs` | 否 |
| A6 | 「关于」并入设置 + 链接可点 | `state.rs`、`app.rs`、`views.rs`、`README.md` | 否 |

---

## 3. A1 容器页：搜索 + 状态筛选 + 排序

### 3.1 纯逻辑放 `wslc-panel-core`

新建 `crates/wslc-panel-core/src/listview.rs`，并在 `lib.rs` 里 `pub mod listview;`。
`wslc-panel/src/main.rs` 的 re-export 垫片补一行：

```rust
mod listview {
    pub use wslc_panel_core::listview::*;
}
```

类型与函数：

```rust
/// 容器列表的状态范围。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ContainerScope {
    #[default]
    All,
    Running,
    /// 未运行（含 created / paused / exited / dead）—— 叫「未运行」而不是
    /// 「已停止」，因为 `paused` 不是停止。
    NotRunning,
}

impl ContainerScope {
    pub const ALL: [ContainerScope; 3] = [Self::All, Self::Running, Self::NotRunning];
    pub fn label(self) -> &'static str; // 全部 / 运行中 / 未运行
}

/// 排序方式。默认保留 `wslc list` 的原序（不改变现有行为）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ContainerSort {
    #[default]
    Default,
    Name,
    State,
}

impl ContainerSort {
    pub const ALL: [ContainerSort; 3] = [Self::Default, Self::Name, Self::State];
    pub fn label(self) -> &'static str; // 默认 / 名称 / 状态
}

/// 页面上的筛选状态（查询词不在这里，它住在 `InputState`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ContainerView {
    pub scope: ContainerScope,
    pub sort: ContainerSort,
}

/// 过滤 + 排序。返回引用，不复制 `ContainerSummary`。
pub fn visible_containers<'a>(
    items: &'a [ContainerSummary],
    query: &str,
    view: ContainerView,
) -> Vec<&'a ContainerSummary>;

/// `(全部, 运行中, 未运行)`，给筛选按钮上的计数用。
pub fn scope_counts(items: &[ContainerSummary]) -> (usize, usize, usize);
```

匹配规则（写进文档，测试钉死）：

- `query.trim()` 为空 → 不过滤；大小写不敏感；
- 匹配字段：**容器名、镜像引用、ID、端口映射、网络名**（`display_name` /
  `image` / `id` / `ports` / `networks`）。不匹配启动命令 —— 它经常是一长串
  `tail -f /dev/null`，匹配进来只会制造噪声；
- 状态范围用 `ContainerSummary::item.is_running()`，和列表行按钮用的是同一个判断；
- `Default` 排序**不改变原序**（`wslc list` 给什么就是什么）；
- `Name` 按 `display_name()` 的 ASCII 小写比较；
- `State` 运行中在前，同状态按名称（空名回退 ID）。

`AppState` 增加一个字段：

```rust
pub container_view: crate::listview::ContainerView,
```

`AppState::new` 里用 `ContainerView::default()`。它是**页面状态**（不是偏好），
不写 `prefs.json`，重启后回到「全部 + 默认排序」。

### 3.2 搜索框的生命周期

`InputState::new` 要 `&mut Window`，所以搜索框必须在**进入页面那一刻**建一次，
不能在 render 里建（见 `AGENTS.md` §7.5）。

`Shell` 增加：

```rust
pub(crate) container_search: Option<Entity<InputState>>,
/// 输入框变化的订阅。**必须存着** —— `Subscription` 一 drop 就退订，
/// 表现是"打字时列表不跟着变"，不会报错。
container_search_sub: Option<Subscription>,
```

新增：

```rust
fn ensure_container_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    if self.container_search.is_some() {
        return;
    }
    let input = cx.new(|cx| {
        InputState::new(window, cx).placeholder("搜索名称 / 镜像 / ID / 端口")
    });
    // ⚠️ 必须订阅 `InputEvent::Change`：渲染整页的是 `Shell`，输入框改的是
    // `InputState` 自己的值 —— 不订阅父级不会重绘，过滤结果要等下一次自动刷新
    // 才变，打字时看起来就像"搜索框坏了"。
    // 这不是猜的 API：v0.6 的 tag 过滤框用过同一套（`cx.subscribe` +
    // `matches!(event, InputEvent::Change)`），组件库自己的 `SearchBox` 也是这么写的。
    let sub = cx.subscribe(&input, |_shell, _input, event: &InputEvent, cx| {
        if matches!(event, InputEvent::Change) {
            cx.notify();
        }
    });
    self.container_search = Some(input);
    self.container_search_sub = Some(sub);
    cx.notify();
}
```

`Page::needs_window_to_enter` 增加 `Page::Containers`：

```rust
pub fn needs_window_to_enter(self) -> bool {
    matches!(
        self,
        Page::AddInstance | Page::Database | Page::Containers
    )
}
```

`Shell::new` 里新字段初始化为 `container_search: None` / `container_search_sub: None`；
`errors_expanded: false`、`last_refresh: None` 在 §6 一起加。

`Shell::set_page` 的 `if page.needs_window_to_enter()` 块里补：

```rust
if page == Page::Containers {
    self.ensure_container_search(window, cx);
}
```

#### 三个「没有 window 也要回容器页」的路径

`goto_page` 不建输入框。本批把这三处改成有 window 的 `set_page`，
否则从详情/部署页回来时搜索框会缺席：

| 位置 | 现状 | 改法 |
|---|---|---|
| `confirm_create` 创建成功 | `cx.spawn` + `goto_page(Page::Containers)` | 改 `cx.spawn_in(window, …)` + `this.update_in(cx, |shell, window, cx| …)` + `shell.set_page(Page::Containers, window, cx)` |
| `close_database_page` | `goto_page(Page::Containers)` | 签名加 `window: &mut Window`，改 `set_page`；`views.rs` 的调用点（返回按钮）回调里本来就有 window |
| `close_deploy_page` | 同上 | 同上 |

改完用一条 grep 自查：**`goto_page(Page::Containers` 必须为 0 处**。
`goto_page` 的注释同步改成"只给没有输入框的页面用"。

#### 渲染侧

`views::page` 里这一支改成：

```rust
Page::Containers => containers(shell, cx, entity).into_any_element(),
```

`views::containers` 签名改成 `(&Shell, &App, &Entity<Shell>)`；
查询词从 `shell.container_search.as_ref().map(|i| i.read(cx).value())` 读。
如果它意外为 `None`（不该发生），工具栏只画筛选/排序，不 panic。

### 3.3 工具栏

两行，避免在 880px 下挤成一团：

```text
┌ 创建容器 ─────────────────────────────────────────────────────────────┐
│ [创建容器] [搜索框 240px] [清空]          [停止全部] [清理已停止]        │
│ [全部 12] [运行中 3] [未运行 9]   [排序：默认]   显示 3 / 共 12          │
└──────────────────────────────────────────────────────────────────────┘
```

- 「清空」只在查询词非空时出现；点击走 `InputState::set_value("", window, cx)`；
- 筛选按钮选中态用 `.primary()`（和设置页 tab / 主题按钮同一套写法）；
- 「排序」用 `Button::dropdown_menu`，菜单项 `ContainerSort::ALL`；
- 删掉原来那行「共 N 个容器，M 个在运行」—— 计数已经在筛选按钮上；
- 「停止全部」仍然只在 `running > 0` 时显示，语义不变（它执行时现查
  `list_running`，不受筛选影响）；
- 空结果：

  ```text
  没有匹配的容器
  换个关键词，或点「全部」；也可以清空筛选。
  [清空筛选]
  ```

  「清空筛选」同时把 scope 复位到 `All`、sort 复位到 `Default`、搜索框清空。

### 3.4 状态与健康上屏

列表「状态」格现在只有 `State` 徽标。改成两行：

```text
[运行中] [不健康]        ← 第二枚只在 health 非空且不是 healthy 时画
Up 2 hours               ← `Status` 字段，text_xs + muted，截断
```

- `health_status` 的取值实测可能是空串 / `healthy` / `unhealthy` / `starting`；
  `healthy` **不画**（那是默认态，画出来全是噪声）；
- `unhealthy` 用 danger 色，`starting` / 其它未知值用 warning 色；
- 第二行截断即可，完整 `Status` 在详情里已经有（`kv("状态", item.status)`）；
- 「状态」列权重保持 120，不为了这一格加宽整表（宽窗下够，窄窗下本来就不够）。

容器详情补三行（都是 `ContainerListItem` 已有字段）：

| 标签 | 字段 |
|---|---|
| 创建时间 | `item.created_at`（绝对时间，和现有「已创建」的相对时间并列） |
| 网络 | `item.networks`（空则 `—`） |
| 容器大小 | `item.size`（wslc 的 `Size` 字段；标注"容器"是为了不和镜像大小混淆，不承诺它精确等于可写层） |

健康检查非空时再加一行「健康检查」。`healthy` 也显示在详情里（详情是查事实的地方，
不需要像列表那样去噪）。

### 3.5 测试

`listview.rs` 里至少这些：

1. `empty_query_returns_all_in_original_order`；
2. `query_matches_name_image_id_ports_network_case_insensitively`；
3. `query_is_trimmed`；
4. `scope_counts_use_is_running`（含 created / paused 计入「未运行」）；
5. `sort_by_name_is_case_insensitive`；
6. `sort_by_state_puts_running_first`；
7. `sort_default_keeps_input_order`；
8. `empty_input_gives_empty_output`。

`state.rs` 的 `only_the_pages_with_inputs_need_a_window_to_enter` 要改：
现在它断言 `Page::ALL` 里没有任何一页需要 window；加上 Containers 之后改成
逐页断言，并显式检查 `Page::AddInstance`。

---

## 4. A2 低频动作与重复概览

### 4.1 镜像 / 网络 / 卷：行内只留「更多」

统一形状（和 `container_more_menu` 一样，菜单项点完自动关）：

| 页面 | 行内 | 「更多」菜单 |
|---|---|---|
| 镜像 | `更多` | 用这个镜像创建容器 / 分隔线 / 删除 |
| 网络（非内置） | `更多` | 删除 |
| 网络（内置） | `内置网络，不可删除`（`cell_muted_full`，悬停看全文） | — |
| 卷 | `更多` | 删除 |

镜像的「用这个镜像创建容器」走现成的 `Shell::use_pulled_image(reference, window, cx)`：
它会关掉拉取弹窗、按镜像名预填创建表单。这是**列表里原本缺的高频动作**，
顺手补上；不新增采集。

这样镜像/网络（非内置）/卷三页的操作列都只有一个「更多」，窄窗下不会再被两个按钮挤爆。
`danger_button` 在这三页不再使用；删除仍然走 `PendingAction` 的二次确认，
危险提示由确认框承担，不靠行内红按钮。

⚠️ 镜像菜单里的「用这个镜像创建容器」要用 `PopupMenuItem::on_click` 的
**window** 参数：`move |_, window, cx| … use_pulled_image(reference, window, cx)`。
忽略 window 拿不到 `Shell::use_pulled_image`（它要 `&mut Window` 才能建表单）。

### 4.2 实例页去掉「实例概览」卡

删掉 `views::distro_summary_card` 及其调用，把信息压成工具栏下面**一行**：

```text
共 3 个发行版 · 2 个在运行 · 磁盘合计 45.2 GB（虚拟）· 默认 Ubuntu-26.04
（1 个实例没读到大小）
```

- 磁盘合计仍然只加**读到了大小**的那些，未知数量如实写出来；
- 「默认发行版」从卡片挪到这一行；
- 这样「运行中 / 全部 / 磁盘合计」不再在卡片、工具栏、表格里各出现一次。

`distro_summary_card` 删除后，`views.rs` 里没有别处引用（改完 grep 一下）。

---

## 5. A3 已有信息上屏

### 5.1 镜像：使用数 + 总占用

`IMAGE_COLUMNS` 从「仓库:标签 / ID / 大小 / 创建于 / 操作」改成：

```rust
pub const IMAGE_COLUMNS: &[(&str, f32)] = &[
    ("仓库:标签", 380.0),
    ("大小", 110.0),
    ("使用中", 110.0),
    ("创建于", 180.0),
    ("操作", 120.0),
];
```

- ID 不进列（低频；本批不额外提供入口，要看 ID 用命令行 `wslc images`）；
- 「使用中」：先看 `img.containers.trim()` —— 空串显示 `—`（字段缺失不等于 0）；
  否则用 `img.container_count()`：`0` → 「未使用」，`N` → 「N 个容器」；
- 同一 ID 多仓库引用会重复显示使用数，这是如实的（每个引用都是可操作的一行）。

`Snapshot` 增加纯方法：

```rust
/// `(不同镜像数, 合计字节数)`。**按 ID 去重** —— `wslc images` 对同一个镜像
/// 会给出多行（不同 Repository 引用），直接 `sum` 会把体积算两遍。
/// ID 为空时退回 reference 当键。
pub fn image_totals(&self) -> (usize, u64);
```

（实现时 `state.rs` 要补 `use std::collections::BTreeSet;` —— 现在没有这个导入。）

镜像页工具栏那句状态改成：

```text
共 6 条记录（5 个镜像）· 合计 1.2 GB
```

拉取中仍然优先显示「正在拉取 X …」。

测试：用 `crates/wslc-core/tests/fixtures/images.jsonl`（里面有两条
`e2ac70e7319a`），断言不同镜像数为 2、合计只算一次该 ID 的体积。

### 5.2 卷：使用中

`VOLUME_COLUMNS` 从「名称 / 驱动 / 作用域 / 挂载点 / 操作」改成：

```rust
pub const VOLUME_COLUMNS: &[(&str, f32)] = &[
    ("名称", 240.0),
    ("使用中", 90.0),
    ("驱动", 100.0),
    ("挂载点", 320.0),
    ("操作", 120.0),
];
```

- 读 `VolumeListItem::in_use()`（`extra` 里的 `InUse`）：
  `Some(true)` → 「使用中」、`Some(false)` → 「未使用」、`None` → `—`；
- ⚠️ 这个字段**没有实机夹具**（采集环境没有卷，`volume.rs` 顶部写了它是按
  Docker 惯例推的）。所以 `None` 必须显示 `—`，**不能**用它决定"能不能删"；
  删除/清理仍然交给 `wslc` 自己判断；
- 「作用域」列去掉（实测都是 `local`，不提供信息）。

「清理未使用卷」的确认框可以顺带写数量，但**本批不做**（要改
`PendingAction::PruneVolumes` 的形状）；先只把每行的 InUse 画出来。

### 5.3 网络：IPv4 / 内部

`NETWORK_COLUMNS` 从「名称 / ID / 驱动 / 作用域 / IPv6 / 操作」改成：

```rust
pub const NETWORK_COLUMNS: &[(&str, f32)] = &[
    ("名称", 220.0),
    ("驱动", 100.0),
    ("IPv4", 90.0),
    ("IPv6", 80.0),
    ("内部", 80.0),
    ("操作", 140.0),
];
```

- `ipv4_enabled()` / `is_internal()` 已有且夹具里有 `"true"` / `"false"`；
- ID 和作用域（恒为 `local`）不进列；子网/网关/连接容器数要 `inspect`，留给 P1；
- 内置网络的操作格用 `cell_muted_full`（截断 + tooltip 全文），避免 10 个汉字
  在窄列里被直接裁掉。

### 5.4 概览页：会话存储一行

**不动 4 个环**，在环下面那行小字旁边/下方补一条：

```text
会话存储：C:\Users\…\wslc\sessions\1\storage.vhdx · 盘剩余 120.3 GB · 另有 2 个会话占 8.2 GB
```

数据全部来自 `Snapshot::storage`（`StorageInfo`）：

- 路径：`vhd`，没有就退回 `sessions_dir` / `base`，再没有就写「VHD 还没建出来」；
- 剩余：`volume.free`（`format_bytes`）；
- 其它会话：`has_other_sessions()` 时写 `sessions_count - 1` 和
  `other_sessions_bytes()`；
- 来源（`origin.label()`）放进 tooltip，不占正文；
- 用 `cell_muted_full("session-storage", text)` 画：一行、截断、悬停看全文。

同时把现在那段三行口径**压成一句**：

```text
口径：只算容器；CPU 按核数折算；存储是会话 VHD 占所在盘的比例。
```

### 5.5 容器详情补字段

见 §3.4。只加 `ContainerListItem` 已有字段，不新增任何采集。

---

## 6. A4 错误横幅折叠 + 页头最后刷新

### 6.1 错误横幅

`Shell` 增加 `errors_expanded: bool`（默认 `false`）。

`app.rs` 的 `error_banner` 改成：

| 情况 | 画什么 |
|---|---|
| 0 条 | 什么都不画 |
| 1 条 | 直接显示这一条（不折叠，折叠反而多一次点击） |
| N 条，未展开 | 第一条 + `还有 N-1 项` + `[查看]` |
| N 条，已展开 | 全部 + `[收起]`；列表 `max_h(px(160))` + `overflow_y_scroll()`，**必须有 `.id()`** |

- 切换只改 `errors_expanded`，不碰快照；
- 成功的那一轮把 `errors_expanded` 复位成 `false`：否则下一次再出错时，
  横幅会**自动展开**（用户上次点过「查看」），看起来像突然弹了一屏红字；
- 恢复成功后 `snapshot.errors` 为空，横幅自然消失，不需要手动清；
- 本批**不拆 warning/error**。`load_snapshot` 现在把发行版的解析 warning
  也塞进 `errors`（`state.rs` 的 `snap.errors.extend(list.warnings)`），
  视觉上确实偏红；拆成 `Snapshot.warnings` 要动采集装配，放到下一批。

### 6.2 页头

`Shell` 增加 `last_refresh: Option<Instant>`，在 `refresh_inner` 把快照写回
`Shell` 的那一处（`shell.state.busy = false;` 附近）设置
`shell.last_refresh = Some(Instant::now());`。自动刷新和手动刷新都算。

页头副标题从：

```text
会话 #1 wslc-cli-76434 · 构建 1a2b3c4
```

改成：

```text
最后刷新：3 秒前 · 自动刷新 3 秒
```

- 相对时间用 `util.rs` 的新纯函数：

  ```rust
  /// `0/1 秒` → 「刚刚」，其余复用 `human_secs` 再加「前」。
  pub fn refresh_age_label(secs: u64) -> String;
  ```

  测试边界：0 / 1 / 2 / 59 / 60 / 3661；
- `last_refresh` 为 `None` 时写「还没刷新」；
- 会话名与构建号移到「关于」（A6），不再常驻页头。
- 这个标签只在重绘时更新；自动刷新默认 3 秒，会带着它一起更新。
  不额外加 1 秒定时器（那是又一个常驻唤醒）。

---

## 7. A5 子页面高亮父导航项

`Page` 增加两个纯方法（`state.rs`，带测试）：

```rust
impl Page {
    /// 这一页在侧边栏里应该高亮哪一项（自己就是导航项时返回自己）。
    pub fn nav_parent(self) -> Option<Page> {
        match self {
            Page::AddInstance => Some(Page::Instances),
            Page::Database | Page::Deploy => Some(Page::Containers),
            _ => None,
        }
    }

    /// `nav` 这一项在当前页面上是否算选中。
    pub fn highlights_nav(self, nav: Page) -> bool {
        self == nav || self.nav_parent() == Some(nav)
    }
}
```

`app.rs` 的 `nav_item` 把 `active = page == current` 改成
`active = current.highlights_nav(page)`。

效果：进「数据库管理 / 部署管理」时侧边栏「容器」亮着；进「添加实例」时
「实例列表」亮着。页面标题仍然是子页自己的名字，不会误导。

测试：

- `add_instance_highlights_instances`；
- `database_and_deploy_highlight_containers`；
- `dashboard_highlights_itself_only`；
- `normal_pages_do_not_highlight_others`。

---

## 8. A6 「关于」并入设置 + 链接可点

### 8.1 从页面变成设置 tab

- `Page` 删除 `About` 变体；`Page::ALL` 从 8 项变 7 项；
- `Page::label` / `Page::group` 删掉对应分支；
- `app.rs` 的 `nav_id` 删掉 `Page::About` 分支（`Page` 穷尽，编译器会提醒）；
- `views::page` 删掉 `Page::About` 分支；
- `SettingsTab` 增加 `About`，`ALL` 变 4 项，`label()` 返回「关于」；
- `views::app_settings` 的分发增加 `SettingsTab::About => settings_about(state, entity)`；
- `views::about` 改名 `settings_about`（避免"它还是一个页面"的错觉），
  签名改成 `(&AppState, &Entity<Shell>)`。

设置 tab 的标签顺手改清楚：

| 变体 | 现在 | 改成 |
|---|---|---|
| `General` | 常规 | 常规 |
| `Advanced` | 高级 | **wslc 配置** |
| `Wsl` | WSL | **WSL 配置** |
| `About` | — | 关于 |

（`Advanced` 这个变体名是历史包袱，本批只改显示文案，不改枚举名；改枚举名
会牵动 `settings-tab-{:?}` 的 id 和测试，收益不大。）

### 8.2 链接可点

`about` 里那四条地址现在是纯文本 `div`，点不动。改成 `views::cell_link`：

```rust
cell_link(label, url, url, entity)
```

`cell_link` 内部走 `Shell::open_url`（默认浏览器），已经是现成的，且带 tooltip。

### 8.3 内容

「关于」tab 保留：版本、构建、界面库、许可证说明、四条地址；
**新增**「会话」（`state.session_label()`）—— 它从页头挪过来；
如果 `snapshot.info` 在，可再加 WSL / 内核版本各一行（也可以不加，
概览页已经有；保持这一页只读、短）。

### 8.4 文档与测试

- `state.rs` 的 `assert_eq!(Page::ALL.len(), 8)` 改 7，注释同步；
- `pages_are_grouped_in_navigation_order` 的去重分组不变（仍是 5 组），不用改；
- `README.md` 的侧边栏示意图去掉「关于」，把「8 页」改「7 页」，并注明
  「关于」是「应用设置」里的一个 tab；
- `README.md` 现在功能表里还有「当前运行」页（`README.md:43`），但代码里
  早已没有这一页 —— 本批顺手改成「容器页 + 状态筛选」，和实际一致。
- 补一个 `SettingsTab` 的小测试：`ALL.len() == 4`、每个 `label()` 非空。
  这条测试的目的是逼着以后加 tab 时同步 `settings_tabs` 的渲染顺序。

---

## 9. 列定义汇总

| 表 | 旧列 | 新列 | 列数 |
|---|---|---|---|
| 容器 | 名称 / 镜像 / 状态 / 资源 / 端口 / 操作 | 不变（状态格内部改两行） | 6 → 6 |
| 镜像 | 仓库:标签 / ID / 大小 / 创建于 / 操作 | 仓库:标签 / 大小 / 使用中 / 创建于 / 操作 | 5 → 5 |
| 网络 | 名称 / ID / 驱动 / 作用域 / IPv6 / 操作 | 名称 / 驱动 / IPv4 / IPv6 / 内部 / 操作 | 6 → 6 |
| 卷 | 名称 / 驱动 / 作用域 / 挂载点 / 操作 | 名称 / 使用中 / 驱动 / 挂载点 / 操作 | 5 → 5 |
| 实例 | 名称 / 状态 / 版本 / 默认 / 安装位置 / 磁盘（虚拟） / 操作 | 不变 | 7 → 7 |

**列数全部不变**，所以 `columns.rs` 的
`every_table_has_the_expected_column_count` 那个 `[6,5,6,5,7,8,5,4,6]`
**不用改**；但 `*_row` 里 cell 的**内容**要逐一对上，改完把该测试的注释同步
（它现在写的是「镜像 5 列（含 ID）」这类旧描述）。

`columns.rs` 顶部的注释要更新：
- `IMAGE_COLUMNS` 说明为什么 ID 不进列、使用数为什么值得一列；
- `NETWORK_COLUMNS` 说明为什么去掉作用域（恒 `local`）；
- `VOLUME_COLUMNS` 说明 `InUse` 尚未实机校准，`None` 显示 `—`。

---

## 10. 提交顺序（每步可独立回滚）

1. `feat(core): 容器列表过滤/排序、镜像合计、导航父项（纯逻辑 + 测试）`
   - `listview.rs`、`lib.rs`、`main.rs` 垫片、`state.rs`（字段/方法）、`util.rs`
     （`refresh_age_label`）
   - **不碰 `columns.rs` 和 `views.rs`**：这一步只加能力，界面行为不变
2. `feat(ui): 容器页搜索、状态筛选、排序、状态/健康上屏`
   - `app.rs`（搜索框、订阅、`set_page`、三条回容器路径）、`views.rs`
3. `feat(ui): 镜像/卷/网络上屏使用关系，删除收进「更多」`
   - `views.rs`、`columns.rs`（列定义和行内容必须在同一个提交里改，
     否则表头会和单元格错位）
4. `feat(ui): 概览页会话存储行、容器详情补字段`
   - `views.rs`
5. `refactor(ui): 错误横幅折叠、页头最后刷新、子页父项高亮`
   - `app.rs`、`views.rs`
6. `refactor(ui): 实例页去概览卡、关于并入设置、链接可点`
   - `views.rs`、`state.rs`、`app.rs`
7. `docs: README 与 PLAN-ui-simplify 同步`

第 1 步只加纯逻辑，不改界面行为；第 2 步开始用它们。列定义和对应行内容
永远在同一个提交里改 —— 分开改会让表头和单元格错位，而这种错位 CI 看不出来
（列数没变，测试照样绿）。

---

## 11. 验收

### CI 能判定

- `core` job：`listview` 的 8 个测试、`image_totals` 去重测试、
  `refresh_age_label` 边界、`Page::nav_parent` / `highlights_nav`、
  `Page::ALL.len() == 7`、`needs_window` 的新断言全部通过；
- `ui` job：`cargo check --workspace --all-targets --locked` 通过
  （尤其是 `InputEvent` 导入、`spawn_in`/`update_in`、`Page` 穷尽 match）；
- `lint` 只报告，不阻塞。

### 必须本机点一遍（AGENTS §8）

- [ ] 880×620 最小窗口：容器页工具栏不挤爆；镜像/网络/卷三个新操作列
      （只有「更多」）不被裁；
- [ ] 容器搜索：打字时列表**立即**变（订阅生效，不是等 3 秒自动刷新）；
- [ ] 搜索匹配名称/镜像/ID/端口/网络；「清空」能清掉；
- [ ] 筛选按钮计数正确（含 paused/created 归到「未运行」）；
- [ ] 排序：默认 / 名称 / 状态；切页再回来筛选状态还在；
- [ ] 状态格：`Up 2 hours` 第二行显示；`unhealthy` 橙/红；`healthy` 不画；
- [ ] 镜像「使用中」：0 显示未使用、空字段显示 `—`；总占用不重复计同一 ID；
- [ ] 卷 InUse：有值/无值都试；`—` 不误导；
- [ ] 网络 IPv4/内部；内置网络操作格悬停能看全文；
- [ ] 概览页存储行：路径、剩余空间、其它会话；口径只剩一句；
- [ ] 错误横幅：1 条 / 多条折叠 / 展开滚动 / 收起；
- [ ] 页头「最后刷新」在 3 秒后变化；
- [ ] 进数据库/部署页时「容器」高亮，进添加实例时「实例列表」高亮；
- [ ] 关于：四个链接能开浏览器；会话/构建在；设置 tab 四个不折行；
- [ ] 从创建容器成功、数据库返回、部署返回三条路径回到容器页，
      **搜索框都在**；
- [ ] 镜像「更多 → 用这个镜像创建容器」预填正确；删除走二次确认。

### 回归

- [ ] 「停止全部」的影响面计数**不受筛选影响**（它执行时现查 `list_running`）；
- [ ] 清理已停止 / 删除卷 / 删除网络 / 删除镜像的确认框仍能弹出、层序仍在最后；
- [ ] 容器详情里新增的三行在 `inspect` 还没回来时不会显示成空（它们来自
      `list`，不需要 inspect）；
- [ ] `Page::needs_window_to_enter` 变了之后，`set_page` 以外没有
      `goto_page(Page::Containers)` 残留。

---

## 12. 风险与回退

| 风险 | 表现 | 对策 |
|---|---|---|
| 搜索框订阅丢了 | 打字不刷新，像坏了 | `Subscription` 存 `Shell` 字段；创建后只建一次 |
| `Page::Containers` 加进 `needs_window` 后，某条 async 路径直接 `goto_page` | 搜索框缺席 | §3.2 的三处改完 + grep 自查；渲染侧允许 `None` 不 panic |
| `spawn_in` 改写 `confirm_create` 引入异步生命周期错误 | 编译失败 | 照 `app.rs` 里已有 14 处 `spawn_in` 的形状写；`update_in` 里只做状态写回 |
| 卷 `InUse` 字段未实机校准 | 显示成「未使用」但其实在用 | 空/缺字段一律 `—`；绝不据此禁用删除 |
| 镜像 `Containers` 字段为空 | 误显示「未使用」 | 空串显示 `—`，只有解析出 0 才写「未使用」 |
| 删掉 `Page::About` 牵动较多 match | 编译错误/测试红 | 编译器会穷尽检查；测试只改 `ALL.len` 与注释 |
| 880px 下容器操作列仍被裁 | 已知问题，本批不修 | 单列一批做横向滚动/折叠；本批保证新列不更差 |
| 错误横幅折叠后用户看不到第二条 | 以为只错了一项 | 明确写「还有 N 项」+「查看」；1 条时不折叠 |

---

## 13. 文档同步

- `README.md`：
  - 侧边栏示意图与「8 页」→「7 页」；
  - 功能表：容器页加搜索/筛选/排序、状态+健康；镜像加使用数/总占用；
    卷加使用中；网络加 IPv4/内部；概览加会话存储行；设置加「关于」tab；
  - 删掉 `README.md:43` 那条不存在的「当前运行」页，改成「容器页 + 状态筛选」；
  - 自动刷新段落加一句：页头显示「最后刷新 N 秒前」。
- `docs/PLAN-ui-simplify.md`：本文。
- `docs/PLAN-v0.10.md`：不涉及，不改。
- `AGENTS.md`：本批没有新的全局约定，不改。

---

## 14. 实施记录（v0.9.22）

本批已按上面的顺序落地。和方案有两处小偏差，都是实现时为了少引入未验证 API：

1. **会话存储的「来源」没进 tooltip**。方案想让 `origin.label()` 悬停可见，
   但 `cell_muted_full` 的 tooltip 就是正文本身，没有独立的 tip 参数；
   为了不新增一个只有一处用的 helper，正文只写路径 / 剩余 / 其它会话，
   来源不显示。要补的话，给 `cell_full` 加一个 `tip: Option<SharedString>`
   参数即可（改动点只有 `cell_full` 和它的两个包装函数）。
2. **镜像的「创建容器」菜单项用了 `PopupMenuItem::on_click` 的 window 参数**。
   实机 API 已核对（`F: Fn(&ClickEvent, &mut Window, &mut App)`，见
   gpui-kit `menu_item.rs`），不是猜的；`Menu` 的点击回调本来就有 window。

另外，`Page::Containers` 进入 `needs_window_to_enter` 之后，
`confirm_create` 的成功路径从 `cx.spawn` 改成了 `cx.spawn_in`，
否则创建完回到容器页时搜索框会缺席（`goto_page` 拿不到 window）。

## 15. 表单与设置精简（2026-10-18）

用户要求其他界面继续保持简洁，本批先落实创建容器、部署表单、两个配置页和实例详情；概览四环、网络/卷独立页面及现有列表数据保持。

- **创建容器**：镜像、名称、端口、环境变量、卷默认可见；可选启动命令、网络、资源限制、拉取策略放进“高级设置”，折叠时显示当前摘要。已确定镜像没有 CMD 时，启动命令直接显示为必填。等效命令单独展开；保留端口发布约束和必需变量校验，校验可能涉及高级字段时自动展开。
- **部署**：字段帮助只显示当前运行时的内容，保留文件覆盖范围、命令执行语义和端口发布限制。nginx 启用独立站点后先显示 SPA 与域名，反代和更多规则按需展开。重新部署已有规则自动展开；折叠不会清空参数，校验报错先展开再聚焦。
- **wslc 配置**：候选值与恢复默认收进每项“修改”菜单；自定义值保留页面唯一的系统编辑器入口。保留存储路径迁移警告、未保存状态、备份保存；原始 YAML 按需展开。
- **WSL 配置**：默认保留文件、内容和静态检查结果。检查范围与生效方式折叠；没有文件或读取失败时不宣称检查通过，发现错位键时完整显示问题行及应放置位置。
- **实例详情**：保留打开安装位置和编辑配置；压缩、移动、扩容、导出、删除收进“更多操作”。沿用原有 WSL1/运行态/过渡态限制与危险操作确认。
- **数据库**：恢复此前只构造、未挂到页头的“基础信息”按钮，连接信息继续按需打开。

折叠统一使用当前依赖 gpui-component 0.7.1 的 `Collapsible`，六个区域的展开状态只存在内存中，不保存偏好、不改变表单参数。新增状态测试覆盖默认折叠、区域独立、重复设置和开关切换。创建输入与反代输入订阅 `InputEvent::Change`，使摘要及已展开预览即时更新。

验证：提交后以 CI 的 core 与 ui 为编译/纯逻辑验证；实机仍需检查展开/收起、已有反代回填、校验焦点、实例菜单及确认层级，以及窄窗口布局。本批未调整拉取镜像、新建数据库或执行命令弹窗，这些留待下一批。

