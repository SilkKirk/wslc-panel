# PLAN —— 打包安装与更新提示

> **依据**：`E:\cnb\wsl-dashboard-ref`（owu/wsl-dashboard，**GPL-3.0-only**）。
> ⚠️ 按 `AGENTS.md` §6：那个仓库**只能读来理解机制**，
> **一行代码、一句文案、一个图标都不能进本仓库**。本文的 `.iss`、Pascal、Rust
> 全部是照着"机制"重写的。
>
> **本版范围（用户定）**：更新**只做"发现新版本 → 提示"**，下载由用户自己在
> 发布页 / 加速站完成。**应用内不下载、不校验、不替换 exe、不重启。**
>
> 本文只写**要改什么、改在哪个函数、怎么验**。所有"当前行为"都取证过。

---

## 0. 结论先行

| 问题 | 决定 | 一句话理由 |
|---|---|---|
| 安装包 | **Inno Setup 6**，`PrivilegesRequired=lowest`，装到 `{localappdata}\Programs\wslc-panel` | 本项目没有任何需要管理员的操作；per-user 装完目录可写，将来要自更新也不用 UAC |
| 更新做到哪一步 | **只检查 + 提示**：关于页显示"有新版本 vX.Y.Z"，一个「打开发布页」按钮 | 用户自己找加速下载，省掉下载器/校验/替换/重启四件事 |
| 从哪儿查 | `https://api.github.com/repos/SilkKirk/wslc-panel/releases/latest`，`curl.exe` 拉 | 实测 200 / 2.1 s / 6.6 KB；**不需要改 `release.yml`**，不用新增资产 |
| 多久查一次 | 启动 8 秒后一次 + 手动按钮；**24 小时节流** | 未认证配额 **60 次/小时/IP**（实测），一天一次绰绰有余 |
| 怎么提示 | 一条 `Toast`（4 秒）+「关于」页卡片里**常驻**状态 | 不弹模态，不打扰；错过了也能在设置里看到 |
| 失败怎么办 | 自动检查**静默**（只写日志），手动检查才报错 | 这是个本地工具，不依赖网络；每天弹一句"检查更新失败"很烦 |
| 安装包和更新提示的关系 | **互不依赖**，可以分两批做、分两次发版 | 更新提示只读 JSON，安装包只写文件 |

---

## 1. 三条硬约束（决定了方案长什么样）

### 1.1 本机没有 Rust 工具链（`AGENTS.md` §1）

**每一行 Rust 都必须能被 CI 验证**：

- 纯逻辑（版本解析/比较、release JSON 解析、节流判据）放进
  **`wslc-core` / `wslc-panel-core`**，由 `core` job 的
  `cargo test --workspace --exclude wslc-panel` 跑；
- `wslc-panel` 里**不写测试**（那条命令排除了它，写了也不会执行）。

### 1.2 不新增 crate，不动 `Cargo.lock`（`AGENTS.md` §3）

所有 cargo 步骤带 `--locked`，本机改不了锁文件。因此：

| 本来想用 | 实际用 | 为什么 |
|---|---|---|
| `reqwest` / `ureq` | `curl.exe` | 仓库已有 4 处先例（`mirrors` / `catalog` / `registry` / `install`） |
| `semver` | 自己写 60 行 | `semver` 在锁文件里只是**传递依赖**，提成直接依赖会改成员的 `dependencies` 数组，`--locked` 直接红。而本项目只产 `X.Y.Z` 三段 |

> 因为不做下载校验，**`sha256` / `selfswap` 这两个模块都不用写了** ——
> 那是上一版方案里最大的一块。

### 1.3 实测优先（`AGENTS.md` §5）

下面每条都是**这次在本机跑的**。凡是"推的"，文中会标出来。

---

## 2. 实测台账（本机 Windows 10.0.26300 / curl 8.21.0 / 2026-10-21）

| # | 实验 | 结果 | 对方案的影响 |
|---|---|---|---|
| 1 | `api.github.com/repos/…/releases/latest` | ✅ 200 / **2.1 s** / 6.6 KB；同时段 `github.com:443` **两次连接超时** | 检查**只走 `api.github.com`**，不碰 `github.com` |
| 2 | 未认证配额 | `x-ratelimit-limit: 60`，实测 remaining 52；**304 也算一次**（52→51） | 一天一次；**不要**用 ETag 轮询（省流量不省配额） |
| 3 | 响应里有哪些字段可用 | `tag_name` / `published_at` / `body` / `html_url` / `assets[].digest` | 够拼出"版本 + 日期 + 发布页链接 + 变动摘要" |
| 4 | 历史 release 是否都能查到 | 67 个（含 v0.1.0），`assets[].digest` **全部**有 `sha256:…` | 将来真做自更新时校验值现成，不用另发文件 |
| 5 | `certutil -hashfile x SHA256` 的输出编码 | **GBK**（`b5 c4` = "的"）；`decode()` 会因"非 UTF-8 + 偶数长度"判成 UTF-16LE → **整段乱码**；`\| findstr` 过滤后是 66 字节纯 ASCII | 本版用不到；将来做校验时**别走 certutil** |
| 6 | **改名**一个正在运行的 exe | ✅ 成功（`ping`/`cmd`/`powershell` 各一份拷贝） | 将来做自更新的地基；本版不用 |
| 7 | 改名后 `GetModuleFileName` / `process.execPath` 返回什么 | **仍是改名前的路径** | 同上，将来必须在替换**前**缓存路径 |
| 8 | 本机到 release 资产的吞吐 | **~11–15 KB/s**（455 KB / 30 s） | 这是我**不在应用内做下载**的直接原因：30 MB 要 40 分钟，做个半吊子下载器只会更糟 |
| 9 | runner 有没有 Inno Setup | windows-2022 / 2025 / 2025-vs2026 镜像 readme **都有 InnoSetup 6.7.1** | 安装包能在 CI 里直接编，不用联网下载 |

> 第 8 条值得单独说：**"应用内下载"在这台机器上是不可用的**。用户自己也说了
> "我自己找加速下载" —— 这跟实测数字是一致的，不是妥协。

---

## 3. 安装包（Inno Setup）

> 这一节和"更新提示"**没有依赖关系**。想先只做提示，把 §3 整节留到下一批
> 完全没问题。

### 3.1 为什么是 per-user，而不是参考项目的 `admin`

参考项目 `PrivilegesRequired=admin` + `DefaultDirName={autopf}`，因为它要做
端口转发、防火墙规则、计划任务、磁盘挂载 —— 那些都要管理员。

**本项目一件都不需要**（全部通过用户态 CLI：`wslc` / `wsl` / `reg query`）。
而 `admin` 有两个直接代价：每次安装弹 UAC；**装在 `Program Files` 里的话，
将来想自更新必然失败**（目录不可写）。

per-user 装到 `{localappdata}\Programs\wslc-panel` 之后那个目录用户可写，
把"能自更新"这件事从"要提权"变成"默认可能"。

> 数据目录是 `%LOCALAPPDATA%\wslc-panel`，安装目录是
> `%LOCALAPPDATA%\Programs\wslc-panel`，**两者不重叠** —— 卸载/重装都不会碰
> 偏好、日志、部署记录和数据库账号。

### 3.2 `installer/wslc-panel.iss`（新增）

```ini
; wslc-panel 的安装包定义。
;
; 只用 Inno Setup 6.3+ 的语法（x64compatible）。CI 的 windows-latest 自带
; InnoSetup 6.7.1（实测镜像 readme），所以 workflow 里不用下载它。

#define AppName "wslc-panel"

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

; ⚠️ 这个 GUID 一旦发出去就不能再改：改了等于"另一个产品"，
; 升级装不上，旧版的卸载项也会留在"应用和功能"里。
#define AppId "{{8F3C2A61-5B7D-4E19-9A24-6C0D1E8B4F37}"

[Setup]
AppId={#AppId}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=SilkKirk
AppPublisherURL=https://github.com/SilkKirk/wslc-panel
AppSupportURL=https://github.com/SilkKirk/wslc-panel/issues
AppUpdatesURL=https://github.com/SilkKirk/wslc-panel/releases
DefaultDirName={localappdata}\Programs\wslc-panel
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
; per-user：不弹 UAC；装完目录用户可写（将来做自更新的前提，见 §3.1）
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\dist
OutputBaseFilename=wslc-panel-{#AppVersion}-setup-x64
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
UninstallDisplayIcon={app}\wslc-panel.exe
DirExistsWarning=no
; 我们自己在 [Code] 里停进程（见下），不走 Restart Manager：
; GPUI 窗口对 RM 的关闭请求行为没有验证过，而 taskkill 是确定的。
CloseApplications=no
RestartApplications=no
; VersionInfoVersion 必须是四段数字，三段版本后面补 .0
VersionInfoVersion={#AppVersion}.0
VersionInfoProductName={#AppName}
VersionInfoDescription=WSL 容器与发行版管理面板

[Languages]
; ChineseSimplified.isl 是 Inno 官方语言文件（已核对 issrc 的 Files/Languages）。
Name: "zh"; MessagesFile: "compiler:Languages\ChineseSimplified.isl"
Name: "en"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "创建桌面快捷方式"; Flags: unchecked

[Files]
Source: "{#ExePath}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\docs"; DestDir: "{app}\docs"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\wslc-panel.exe"
Name: "{group}\卸载 {#AppName}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\wslc-panel.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\wslc-panel.exe"; Description: "启动 {#AppName}"; Flags: nowait postinstall skipifsilent

[Code]
// ---------------------------------------------------------------------------
// 停掉正在运行的实例
// ---------------------------------------------------------------------------
//
// 为什么不是 CloseApplications=yes：那走 Restart Manager，它会先给窗口发关闭
// 请求再等。GPUI 的窗口收不收、多久收，本机没有工具链验证不了；而 taskkill
// 的结果是确定的。
//
// 刻意**不加 /T**：面板会为"启动"的发行版吊着 wsl.exe 哨兵（`AGENTS.md` §7.3）。
// 杀进程树会把哨兵一起带走，那些发行版会在 20 秒后被 WSL 回收 —— 用户只是
// 想装个新版本，不该顺带停掉他的发行版。哨兵留在那儿是既有行为。

function IsRunning(): Boolean;
var
  Code: Integer;
  Started: Boolean;
begin
  // tasklist 找不到时退出码仍是 0（实测），所以用 find 把"有没有这一行"
  // 翻译成退出码。find 判的是**文本**，不受系统语言影响。
  Started := Exec('cmd.exe',
    '/c tasklist /FI "IMAGENAME eq wslc-panel.exe" /NH | find /I "wslc-panel.exe"',
    '', SW_HIDE, ewWaitUntilTerminated, Code);

  // ⚠️ 分两步写，不要写成 `Started and (Code = 0)`：Inno 的 Pascal Script
  // 默认是完整布尔求值（不短路），Exec 失败时 Code 可能是未初始化的值。
  Result := False;
  if not Started then
    Exit;
  Result := (Code = 0);
end;

function StopRunning(): Boolean;
var
  Code, Waited: Integer;
begin
  if not IsRunning() then
  begin
    Result := True;
    Exit;
  end;

  Exec('cmd.exe', '/c taskkill /F /IM wslc-panel.exe', '', SW_HIDE,
    ewWaitUntilTerminated, Code);

  // 等它真的消失，最多 6 秒：进程退出和文件句柄释放之间有一小段窗口，
  // 不等的话 [Files] 复制会撞上"文件被占用"。
  Waited := 0;
  while (Waited < 6000) and IsRunning() do
  begin
    Sleep(250);
    Waited := Waited + 250;
  end;

  Result := not IsRunning();
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  Result := '';
  if not StopRunning() then
    Result := '无法关闭正在运行的 wslc-panel。请先手动退出它，再点"下一步"。';
end;

// ---------------------------------------------------------------------------
// 卸载
// ---------------------------------------------------------------------------

var
  RemoveData: Boolean;

function InitializeUninstall(): Boolean;
begin
  Result := StopRunning();
  if not Result then
  begin
    MsgBox('无法关闭正在运行的 wslc-panel，请先手动退出它再卸载。', mbError, MB_OK);
    Exit;
  end;

  RemoveData := False;

  // 静默卸载（/SILENT、winget、控制面板）时不弹框，默认**保留**数据。
  if UninstallSilent() then
  begin
    Result := True;
    Exit;
  end;

  // 数据目录里不止是偏好：db-credentials.json 存着用户数据库账号的**明文密码**，
  // 所以必须把"会删掉什么"说清楚，而不是一句"删除配置"。
  RemoveData := MsgBox(
    '是否同时删除面板数据？' + #13#10 + #13#10 +
    ExpandConstant('{localappdata}\wslc-panel') + #13#10 +
    '里面有：偏好设置、运行日志、部署记录、备份文件，以及' + #13#10 +
    '数据库账号（密码是明文保存的）。' + #13#10 + #13#10 +
    '选"否"则全部保留，以后重装还能接着用。',
    mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES;

  Result := True;
end;

procedure CurUninstallStepChanged(CurStep: TUninstallStep);
begin
  if CurStep = usPostUninstall then
  begin
    if RemoveData then
      DelTree(ExpandConstant('{localappdata}\wslc-panel'), True, True, True);
    // 安装目录本身由 Inno 按自己的记录删；这里兜底清掉运行期可能掉进去的东西。
    DelTree(ExpandConstant('{app}'), True, True, True);
  end;
end;
```

> `{#ExePath}` 由 `build-installer.ps1` 通过 `/DExePath=…` 注入 —— 不让 `.iss`
> 猜 `target\release` 在哪儿。

### 3.3 `scripts/build-installer.ps1`（新增）

```powershell
<#
.SYNOPSIS
  用 Inno Setup 编译安装包。CI 和本机（装了 Inno 之后）都能跑。

.NOTES
  版本号只有一处来源：调用方传进来（CI 传 tag，本机默认读根 Cargo.toml）。
  刻意**不**在 .iss 里写死版本 —— 那会变成第 5 个要手改版本号的文件，
  而 AGENTS.md §4 已经把这件事限定在"四个文件、六行"。
#>
param(
  [string]$Version,
  [string]$ExeSource,
  [string]$OutputDir = "dist"
)

$ErrorActionPreference = "Stop"

if (-not $Version) {
  $toml = Get-Content "$PSScriptRoot/../Cargo.toml" -Raw
  $Version = [regex]::Match($toml, '(?m)^version\s*=\s*"([^"]+)"').Groups[1].Value
}
$Version = $Version -replace '^v', ''

if (-not $ExeSource) { $ExeSource = "target/release/wslc-panel.exe" }
if (-not (Test-Path $ExeSource)) { throw "找不到 $ExeSource —— 先 cargo build --release" }

# ISCC：先找 PATH，再找两个标准安装位置。
# runner 镜像自带 6.7.1（实测 2022/2025/2025-vs2026 的 readme），但镜像会变；
# 找不到就给一句能照做的错误，而不是让 CI 报"命令不存在"。
$iscc = (Get-Command iscc.exe -ErrorAction SilentlyContinue).Source
if (-not $iscc) {
  foreach ($p in @("${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
                   "${env:ProgramFiles}\Inno Setup 6\ISCC.exe")) {
    if (Test-Path $p) { $iscc = $p; break }
  }
}
if (-not $iscc) {
  throw "找不到 ISCC.exe。装 Inno Setup 6（https://jrsoftware.org/isdl.php）后重试；CI 上说明镜像变了，需要在 release.yml 里加安装步骤。"
}

# Resolve-Path 给出的绝对路径才不会被 ISCC 当前工作目录影响。
$exePath = (Resolve-Path $ExeSource).Path
if (-not (Test-Path $OutputDir)) {
  New-Item -ItemType Directory -Path $OutputDir -Force | Out-Null
}
$outDir = (Resolve-Path $OutputDir).Path

Write-Host "ISCC : $iscc"
Write-Host "版本 : $Version"
Write-Host "源   : $exePath"

& $iscc "/DAppVersion=$Version" "/DExePath=$exePath" "/O$outDir" `
  "$PSScriptRoot/../installer/wslc-panel.iss"
if ($LASTEXITCODE -ne 0) { throw "ISCC 失败（退出码 $LASTEXITCODE）" }

$out = Join-Path $outDir "wslc-panel-$Version-setup-x64.exe"
if (-not (Test-Path $out)) { throw "ISCC 说成功，但没产出 $out" }
Write-Host "产物 : $out（$([math]::Round((Get-Item $out).Length/1MB,1)) MB）"
```

### 3.4 `release.yml` 只加三步

产物从 2 个变 3 个：多一个 `wslc-panel-<ver>-setup-x64.exe`（**不加**校验文件 ——
发布页上的 `sha256` 由 GitHub 自己在资产详情里给，实测每个 release 都有）。

```yaml
      - name: 纯逻辑测试（打标签前自己先验一遍）
        # 现在这条纪律是"main 上绿了才打标签"，靠人；发布流程本身不验证。
        run: cargo test --workspace --exclude wslc-panel --locked --profile ci

      - name: 编译安装包
        # 只在 vX.Y.Z 标签上编：手动 dispatch 的 tag 是 dev-<sha>，不是数字版本。
        if: startsWith(env.TAG, 'v')
        shell: pwsh
        run: ./scripts/build-installer.ps1 -Version "$env:TAG" -ExeSource "dist/$env:BIN_NAME.exe" -OutputDir dist
```

然后 `gh release create` / `gh release upload` 的资产列表加上
`dist/wslc-panel-$env:TAG-setup-x64.exe`，`upload-artifact` 的路径加 `dist/*.exe`
（已经是 `dist/*.exe` 的话不用改）。

> ⚠️ **`wslc-panel.exe` 这个文件名是更新的契约**：用户在发布页看到的就是它。
> 安装包可以改名，它不能。

---

## 4. 更新提示（本版核心，也是最简单的一块）

### 4.1 总览

```
启动 8 秒后（或点「检查更新」）
      │
      ├─ curl https://api.github.com/repos/SilkKirk/wslc-panel/releases/latest
      │    · 成功 → 解析 tag_name / published_at / html_url / body
      │    · 403  → "接口配额用完了，过会儿再试"
      │    · 404  → "仓库还没有 Release"
      │    · 6/28 → "连不上 GitHub"
      │
      ├─ version > 当前版本 && != 被跳过的版本 ?
      │
      是 → 关于页那行变成「有新版本 vX.Y.Z（2026-10-25 发布）」
      │    + 启动时那一次额外发一条 toast（只发一次）
      │
      用户点「打开发布页」→ explorer.exe 打开 html_url   ← 复用现成的 Shell::open_url
```

**应用内不做的事**：不下载、不校验、不替换、不重启。用户在发布页自己取
`wslc-panel.exe` 或安装包。

### 4.2 为什么直接打 API，不另发一份 `update.json`

| | 直接 API | 自己发 `update.json` |
|---|---|---|
| 要改 `release.yml` | **不用** | 要（多一个资产 + 一段生成逻辑） |
| 配额 | 60 次/小时/IP（一天一次 = 用掉 1 次） | 无 |
| 依赖 `github.com` | 否（`api.github.com` 实测通，`github.com` 同时段超时） | 是（资产在 `github.com`/`release-assets` 上） |
| 字段来源 | GitHub 给的（`tag_name`/`published_at`/`html_url`/`body`） | 自己拼 |

一天一次的频率下，配额根本不是问题。**少一个资产、少一段流水线逻辑**更值。

> 如果哪天一天要查很多次（比如加"每小时轮询"），再补 `update.json` ——
> 那时 `parse_feed` 加个分支就行，`Feed`/`Version` 这些类型不用动。

### 4.3 `crates/wslc-core/src/update.rs`（新增，纯逻辑）

```rust
//! 更新检查里**不碰网络**的那一半。
//!
//! 放 `wslc-core` 而不是 `wslc-panel-core`：它要配 fixture 单测（真实抓下来的
//! GitHub 响应），而 `core` job 跑的是 `--workspace --exclude wslc-panel`。
//! 和 `mirrors.rs` / `registry.rs` 同级 —— 都是"外部协议 + 纯解析"。

pub const REPO: &str = "SilkKirk/wslc-panel";

/// 版本号。**只认 `X.Y.Z`**，可选 `-pre` 后缀。
///
/// 为什么不引 `semver`：它在锁文件里只是传递依赖，提成直接依赖会改成员的
/// `dependencies` 数组，`--locked` 直接红（AGENTS.md §3）。
/// 而本项目实际只会产生 `0.9.25` 这种三段版本。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// 预发布后缀（`-rc1`）。`None` = 正式版。
    pub pre: Option<String>,
}

impl Version {
    /// `"v0.9.25"` / `"0.9.25"` / `"0.10.0-rc1"` 都能解；别的一律 `None`。
    pub fn parse(text: &str) -> Option<Self>;
}

impl std::fmt::Display for Version { /* "0.9.25" / "0.10.0-rc1" */ }

/// `latest` 是否比 `current` 新。
///
/// 规则（测试逐条钉住）：
/// - 数字段按**数值**比，不是字符串：`0.9.9 < 0.9.10`；
/// - 主版本优先：`0.10.0 > 0.9.99`；
/// - 预发布 < 同号正式版：`0.10.0-rc1 < 0.10.0`；
/// - 相等返回 `false`（相等不该提示）。
pub fn is_newer(latest: &Version, current: &Version) -> bool;

/// 一次"检查更新"拿到的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    /// 原始版本串（界面直接显示它，免得我们自己重新拼一遍）
    pub raw_version: String,
    pub tag: String,
    /// `published_at` 只取日期部分（`2026-10-19`）。
    pub published: Option<String>,
    /// `html_url` —— 用户点「打开发布页」就是去这儿。
    pub page_url: String,
    /// `body` 的**开头一小段**（我们截断，界面再 `strip_md`）。
    ///
    /// 为什么不整段：v0.9.25 的 body 就有 1 KB 多，整段塞进卡片会把
    /// 按钮挤出屏幕；完整内容在发布页上，那里才是该看的地方。
    pub notes_head: String,
}

/// 解析 GitHub `releases/latest` 的响应。
///
/// 只认数组/对象里的固定字段，缺了就 `Err` —— 不去猜、
/// 也不把 `draft` / `prerelease` 当成正常版本（`releases/latest` 本来就不返回它们，
/// 但显式判一次，免得将来换成 `/releases` 列表时静默把预发布版推给用户）。
pub fn parse_release(json: &str) -> Result<Release, String>;

/// 拉 release 的 curl 参数。
pub fn curl_args(url: &str) -> Vec<String>;

/// 请求地址。
pub fn api_url() -> String;   // https://api.github.com/repos/{REPO}/releases/latest

/// 把"这次检查为什么失败"翻译成一句能给用户看的话。
///
/// 认不出来就返回一句**朴素**的描述（带原始退出码），不编。
pub fn explain_failure(exit_code: Option<i32>, body_or_stderr: &str) -> String;
```

**`curl_args` 的具体内容**：

```rust
vec![
    "-sS".into(),                    // 静默，但保留错误输出
    "-L".into(),                     // 跟随重定向（API 一般不会跳，防代理）
    "--fail-with-body".into(),       // 4xx/5xx 时退出码非 0，但正文照给 —— 配额错误就在正文里
    "--proto".into(), "=https".into(),
    "-m".into(), "15".into(),        // 一次 6.6 KB，15 秒足够
    "-H".into(), "Accept: application/vnd.github+json".into(),
    "-H".into(), format!("User-Agent: wslc-panel/{}", env!("CARGO_PKG_VERSION")),
    url.into(),
]
```

> `User-Agent` 是 **GitHub 的硬性要求**（不带会被 403 拒），而且带上版本号
> 方便以后在日志里对账。`-m 15` 可以给死：这个请求只有 6.6 KB，和上面那种
> 30 MB 下载完全不是一类东西。

### 4.4 `crates/wslc-core/src/cmd/update.rs`（新增，起进程）

```rust
//! 更新的**阻塞**那一半：起一次 `curl.exe`。
//!
//! 只能在 `cx.background_executor()` 里调（AGENTS.md §7.1）。
//! 整个模块就一个函数 —— 本版不做下载，所以没有进度、没有取消、没有续传。

/// 拉一次 `releases/latest` 并解析。**阻塞**，实测 ~2 s。
pub fn check() -> Result<Release, String> {
    // 复用 cli::run_helper_with_hint（它会注入 CREATE_NO_WINDOW、超时、解码）
    // 和 mirrors::is_http_url（URL 会作为裸 argv 交给 curl，这是安全边界）。
}
```

### 4.5 `crates/wslc-panel-core`：偏好与节流判据

`prefs.rs` 新增三个字段（老的 `prefs.json` 靠 `serde(default)` 平滑升级）：

```rust
    /// 启动时自动检查更新（默认开）。
    ///
    /// 关掉之后**只有**「关于」页那颗按钮会发请求 —— 有些人就是不想让
    /// 一个本地工具往外连。
    #[serde(default = "default_true")]
    pub auto_check_update: bool,

    /// 上次检查更新的时间（Unix 秒），用来做 24 小时节流。
    #[serde(default)]
    pub last_update_check: Option<u64>,

    /// 用户点了「跳过这个版本」之后记住它，不再为它提示。
    #[serde(default)]
    pub skipped_version: Option<String>,
```

新增一个纯逻辑模块 `crates/wslc-panel-core/src/update.rs`：

```rust
/// 现在该不该自动检查？
///
/// 四条都要满足：开关开着、不是开发构建、离上次超过 24 小时、这次会话还没查过。
///
/// 为什么"不是开发构建"：`WSLC_PANEL_BUILD_SHA` 在本地构建时是 `"dev"`，
/// 那种构建的版本号跟发布版一样，提示更新纯属干扰。
pub fn should_auto_check(prefs: &Prefs, now_secs: u64, dev_build: bool) -> bool;

/// 这次的结果要不要提示？
///
/// 除了"版本更新"，还要排掉：用户点过"跳过这个版本"、用户当前版本其实是
/// 更新的开发版。
pub fn should_notify(current: &Version, release: &Release, skipped: Option<&str>) -> bool;

/// 卡片上那行状态文案（纯函数，好测）。
pub fn status_line(phase: &Phase) -> String;
```

UI 状态放在 `AppState`（全是纯数据，`views.rs` 要读）：

```rust
pub enum Phase {
    /// 还没查过（或这次会话关掉了自动检查）
    Idle,
    Checking,
    /// 查过了，就是最新
    UpToDate { checked_at: u64 },
    /// 有新版本
    Available { release: Release, dismissed: bool },
    /// 查失败（只在**手动**检查时给用户看）
    Failed { message: String },
}
```

### 4.6 界面：「应用设置 → 关于」加一张卡

`views::settings_about` 现在只收 `&AppState`；改成和 `settings_general` 一样
收 `&Shell`（要发起动作）。

```
┌ wslc-panel ─────────────────────────────────────────────┐
│ WSL 容器（wslc）和发行版（wsl.exe）的图形管理面板。      │
│ 版本 0.10.0 · 构建 1c8a994 · 会话 …                      │
├ 更新 ───────────────────────────────────────────────────┤
│ 最新版本 v0.10.1（2026-10-25 发布）                      │
│ [打开发布页]  [检查更新]  [启动时自动检查 ✓]             │
│ 已是最新（上次检查：3 小时前）                           │
└──────────────────────────────────────────────────────────┘
```

规矩（`AGENTS.md` §5）：

- **只有这一处动作入口**。启动时查到新版只发一条 `Toast::info`
  （"有新版本 v0.10.1 —— 设置 → 关于 里能打开发布页"），别处不再放按钮；
- `notes_head` 是 Markdown 片段，这里直接 `.child(...)` 到 `div` 上，
  **必须过 `util::strip_md`**；
- 不用 `.disabled()`：`Checking` 状态就不渲染「检查更新」，换成一行「正在检查…」；
- 不新开浮层，进度/结果都画在这张卡里；「打开发布页」直接复用
  `Shell::open_url`（`app.rs:2399`，它只接受 `http(s)` 并用 `explorer.exe` 打开，
  是"只把进程拉起来"的同步调用，`AGENTS.md` §7.1 允许）。

### 4.7 启动自动检查

```rust
// Shell::new 里
// 8 秒：躲开第一轮采集和窗口首帧。实测一轮采集要串行跑 7 条 wslc 命令，
// 这时候再插一个 2 秒的 HTTP 请求，用户看到的第一次卡顿就说不清是谁的锅。
cx.spawn(async move |this, cx| {
    cx.background_executor().timer(Duration::from_secs(8)).await;
    let _ = this.update(cx, |shell, cx| shell.maybe_auto_check(cx));
})
.detach();
```

- `maybe_auto_check` 先过 `should_auto_check`，不过就什么都不做；
- 查成功：更新 `prefs.last_update_check = now` 并落盘；有新版本且
  `should_notify` 为真 → `Toast::info` + 卡片切到 `Available`；
- 查失败：**只记日志**，卡片保持 `Idle`（不因为一次网络失败就显示"检查失败"）。

---

## 5. 测试（CI 能判的部分）

### 5.1 `core` job 自动跑（都在 `wslc-core` / `wslc-panel-core`）

| 文件 | 测试 | 断言要点 |
|---|---|---|
| `wslc-core/src/update.rs` | `parses_tag_with_and_without_v` | `v0.9.25` / `0.9.25` 等价 |
| | `compares_numerically_not_lexically` | `0.9.9 < 0.9.10` |
| | `major_wins` | `0.10.0 > 0.9.99` |
| | `prerelease_is_older` | `0.10.0-rc1 < 0.10.0` |
| | `equal_is_not_newer` | |
| | `rejects_garbage` | `""`、`"1.2"`、`"1.2.3.4"`、`"v"` → `None` |
| | `parses_real_release_response` | 用真抓的 6.6 KB fixture：版本、日期、`html_url`、body 截断长度 |
| | `missing_body_is_empty_not_err` | body 为 `null` 不影响解析 |
| | `prerelease_or_draft_is_rejected` | 显式判一次（将来换 `/releases` 列表时的护栏） |
| | `explains_rate_limit` | 正文里有 `rate limit` → 那句话 |
| | `explains_404_and_network` | |
| `wslc-panel-core/src/update.rs` | `should_auto_check_*` | 四条判据各一条：开关关、dev 构建、24h 内、正常 |
| | `should_notify_respects_skip` | 被跳过的版本不提示 |
| | `should_notify_ignores_equal` | |
| `wslc-panel-core/src/prefs.rs` | 三条 | 老文件升级后 `auto_check_update == true`、`normalized()` 不碰这三个字段、序列化回读一致 |

**fixture**（真抓的，不放编的）：`crates/wslc-core/tests/fixtures/github_release_latest.json`
—— 本次会话抓的 `v0.9.25` 响应（6.6 KB，未认证）。

### 5.2 `ui` job

`cargo check --workspace --all-targets --locked` 覆盖 `app.rs` / `views.rs`。
**这是唯一能证明 GPUI API 假设成立的地方**（`cx.background_executor().timer()`、
卡片的 `Button` 组合、`open_url` 的签名）。

### 5.3 必须在本机点的（`AGENTS.md` §8）

- 「关于」页那张卡：无更新 / 有新版本 / 检查中 / 失败 四个状态长什么样；
- 点「打开发布页」真的打开浏览器且落在正确的 tag 上；
- **断网**（拔代理）时手动检查 → 一句能看懂的话，不是 `curl: (6)`；
- 修改 `prefs.json` 里的 `last_update_check` 到"刚查过" → 重启后**不**自动查；
- 装 → 用 → 卸载 → 再装（快捷方式、卸载项、"应用和功能"里的名字/版本/图标）；
- 安装包在**旧版正在运行**时双击 → 应该自己把它关掉再装。

---

## 6. 实施顺序（每步一个提交，都可独立回滚）

| # | 内容 | 验证方式 |
|---|---|---|
| 1 | `wslc-core/src/update.rs`（`Version` / `is_newer` / `parse_release` / `explain_failure`）+ 真 fixture | `core` job，**不改任何现有行为** |
| 2 | `wslc-core/src/cmd/update.rs`（就一个 `check()`） | `core` job |
| 3 | `wslc-panel-core`：`prefs` 三个字段 + `update.rs`（节流/提示判据/文案）+ 测试 | `core` job |
| 4 | 「关于」页卡片 + 启动自动检查 + toast | `ui` job + **实机** |
| 5 | `installer/wslc-panel.iss` + `scripts/build-installer.ps1` + `release.yml` 两步 | 手动 `workflow_dispatch` 一次，看产物 |
| 6 | README / AGENTS / `COMPARISON-references.md` §6.4 的更正 | 人工 |

> 1–3 步纯新增，没人调用；4 步开始才改变用户看到的东西；5 步和 4 步互不依赖。

---

## 7. 验收标准

- [ ] `cargo test --workspace --exclude wslc-panel --locked --profile ci` 全绿
- [ ] `cargo check --workspace --all-targets --locked` 全绿
- [ ] `Cargo.toml` / `Cargo.lock` 的 diff 为空
- [ ] 全仓库 `grep -i` 确认没有 `sha256` / `selfswap` / `download` 这类**本版不做**的残留
- [ ] 【实机】四种状态都能看到；「打开发布页」落在正确的 tag 上
- [ ] 【实机】自动检查失败时界面**没有任何**变化（只有日志里一行 `warn`）
- [ ] 【实机】安装包三个产物齐全，卸载能问"要不要删数据"

---

## 8. 风险与回滚

| 风险 | 应对 |
|---|---|
| API 配额被别的程序/共享 IP 用光 | 一天一次，基本不可能；真撞上就显示"过会儿再试"，并保留「打开发布页」 |
| `api.github.com` 不可达 | 手动检查给能照做的话；自动检查静默 |
| 版本比较把预发布版当正式版 | `parse_release` 显式拒 `prerelease`/`draft`；测试钉住 |
| 提示太吵 | 24 小时节流 + 「跳过这个版本」+ 可整个关掉自动检查 |
| 安装包把用户的发行版弄停 | 停进程**不加 `/T`**（§3.2 的注释） |

**回滚**：1–3 步删文件即可；4 步 `git revert` 一个提交；5 步摘掉 `release.yml`
那两步、删 `installer/` 与 `scripts/build-installer.ps1`。

---

## 9. 明确不做（留档，免得重复讨论）

| 项 | 理由 |
|---|---|
| **应用内下载 / 校验 / 替换 / 重启** | 用户定：只提示。另外实测本机到 release 资产只有 **11–15 KB/s**，30 MB 要 40 分钟 —— 做个半吊子下载器不如让用户用加速站 |
| 自更新（换 exe） | 地基已经测过了，留档在 §2 第 6/7 条：**运行中的 exe 可以改名、不能覆盖**，改名后 `GetModuleFileName` **仍返回旧路径**。真要做时按这两条设计，别重新调研 |
| 单独的 `update.json` 资产 | 一天一次不需要绕配额；少一个资产、少一段流水线 |
| 后台静默自动安装 | 用户没点就换掉他的 exe，出事没人能解释 |
| 代码签名 | 没有证书。代价如实写在 README（浏览器下载会撞 SmartScreen 一次） |
| winget 提交 | 要先有稳定安装包 URL + sha256（本次就有），但 manifest 是另一个仓库的 PR 流程 |
| ARM64 / 托盘 / 开机自启 | `docs/PLAN-v0.10.md` §8 已排除 |
| 给 exe 加图标/版本资源 | 需要 `build.rs` + `embed-resource` 作为**直接**依赖 → 动 `Cargo.lock`。单独一批做（走 `gen-lock.yml`）；本版安装包先用默认图标 |

---

## 10. 要同步更新的文档

- `AGENTS.md` §4：发布产物多一个 `setup.exe`，并写明
  **`wslc-panel.exe` 的文件名是更新提示与下载页的契约，不能改**；
- `AGENTS.md` §7：新增一节"运行中的 exe 可以被改名，但不能被覆盖/删除；
  改名后 `GetModuleFileName` 仍返回旧路径" —— 这是将来做自更新时的地基，
  也是本版**为什么不做**自更新的证据（§2 第 6/7 条）；
- `docs/COMPARISON-references.md` §6.4：那句"自更新必须先有安装包"是**推的**；
  改成"不必（实测可改名替换），但本版按用户要求只做提示"；
- `README.md`：快速开始加"方式一：安装包"，故障排查加"更新提示从哪儿看、
  为什么它不自动下载"。
