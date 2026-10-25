<#
.SYNOPSIS
  用 Inno Setup 编译 wslc-panel 的安装包。

.DESCRIPTION
  CI 和本机（装了 Inno Setup 之后）都能跑。版本号只有一处来源：
  调用方传进来（CI 传 tag，本机默认读根 Cargo.toml）——
  刻意不在 .iss 里写死，否则它会变成第 5 个要手改版本号的文件，
  而 AGENTS.md §4 已经把这件事限定在"四个文件、六行"。

.EXAMPLE
  ./scripts/build-installer.ps1 -Version v0.10.0 -ExeSource dist/wslc-panel.exe
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
  if (-not $Version) { throw "读不到根 Cargo.toml 里的 version" }
}
$Version = $Version -replace '^v', ''

if (-not $ExeSource) { $ExeSource = "target/release/wslc-panel.exe" }
if (-not (Test-Path $ExeSource)) {
  throw "找不到 $ExeSource —— 先 cargo build --release"
}

# ISCC：先找 PATH，再找两个标准安装位置。
# runner 镜像自带 6.7.1（实测 windows-2022 / 2025 / 2025-vs2026 的 readme），
# 但镜像会变；找不到时给一句能照做的错误，而不是让 CI 报"命令不存在"。
$iscc = (Get-Command iscc.exe -ErrorAction SilentlyContinue).Source
if (-not $iscc) {
  foreach ($p in @("${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
                   "${env:ProgramFiles}\Inno Setup 6\ISCC.exe")) {
    if (Test-Path $p) { $iscc = $p; break }
  }
}
if (-not $iscc) {
  throw "找不到 ISCC.exe。装 Inno Setup 6（https://jrsoftware.org/isdl.php）后重试；CI 上出现这条说明 runner 镜像变了，需要在 release.yml 里加安装步骤。"
}

# 绝对路径：ISCC 的当前工作目录不一定是仓库根，相对路径会飘。
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

$size = [math]::Round((Get-Item $out).Length / 1MB, 1)
Write-Host "产物 : $out（$size MB）"
