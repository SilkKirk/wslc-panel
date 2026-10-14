# 用 GitHub API 把远端某个分支的**内容**同步到本地工作区。
#
# 为什么需要它：这台机器上 `github.com:443` 经常连不上（`git fetch` 一挂就是
# 几十秒 × 重试次数），但 `api.github.com` 通常通（`AGENTS.md` §7.6）。
# 于是"拉取"就绕开 git 协议，直接走 REST：
#
#   1. `GET /git/ref/heads/<ref>` → 远端 head 的 sha
#   2. `GET /git/commits/<sha>`   → 那棵 tree
#   3. `GET /git/trees/<tree>?recursive=1` → 每个文件的 blob sha
#   4. 本地用 `git hash-object` 算出同名文件的 blob sha（**纯本地操作**，不走网络）
#   5. 只把 sha 对不上的那几个文件下下来覆盖
#
# ⚠️ 它只同步**工作区的内容**，不动 git 历史（本地 `git log` 还会停在旧提交，
# `git status` 会显示这些文件"被改过"）。等网络恢复跑一次
# `git fetch --tags --prune && git reset --hard origin/main` 就彻底对齐了。
# 好处是：内容一致，且不会因为一次网络抖动卡住整个流程。
#
# 用法：
#   pwsh -NoProfile -File scripts/gh-api-pull.ps1
#   pwsh -NoProfile -File scripts/gh-api-pull.ps1 -Ref main
#   pwsh -NoProfile -File scripts/gh-api-pull.ps1 -DryRun   # 只报告，不写文件

param(
    [string]$Ref = 'main',
    [string]$Repo = 'SilkKirk/wslc-panel',
    [switch]$DryRun
)

$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$api = "https://api.github.com/repos/$Repo"

# token 从 `git credential` 里拿 —— 脚本本身不存任何密钥
$cred = "protocol=https`nhost=github.com`n" | git credential fill 2>$null
$tok = ($cred | Select-String '^password=').Line -replace '^password=', ''
if (-not $tok) { throw 'git credential 里没拿到 token（先确认本机 git 配过 GitHub 凭据）' }
$H = @{
    Authorization = "token $tok"
    Accept        = 'application/vnd.github+json'
    'User-Agent'  = 'wslc-panel-gh-api-pull'
}

$sha = [string](Invoke-RestMethod -Headers $H -Uri "$api/git/ref/heads/$Ref").object.sha
$tree = [string](Invoke-RestMethod -Headers $H -Uri "$api/git/commits/$sha").tree.sha
$items = @((Invoke-RestMethod -Headers $H -Uri "$api/git/trees/${tree}?recursive=1").tree)
Write-Host "远端 $Ref = $sha（$($items.Count) 项）"

$added = 0
$updated = 0
$same = 0

foreach ($item in $items) {
    if ($item.type -ne 'blob') { continue }

    $path = Join-Path $root ($item.path -replace '/', '\')
    $localSha = $null
    if (Test-Path -LiteralPath $path) {
        # `hash-object` 是纯本地操作（不碰网络）—— 用它判断"这份内容和远端一样吗"
        $localSha = (git -C $root hash-object -- $path).Trim()
    }
    if ($localSha -eq $item.sha) { $same++; continue }

    if ($DryRun) {
        if ($null -eq $localSha) { Write-Host "  会新增 $($item.path)" } else { Write-Host "  会更新 $($item.path)" }
        continue
    }

    $blob = Invoke-RestMethod -Headers $H -Uri "$api/git/blobs/$($item.sha)"
    $bytes = [Convert]::FromBase64String($blob.content)
    $dir = Split-Path -Parent $path
    if (-not (Test-Path -LiteralPath $dir)) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
    # 写**字节**：这样远端存的是 LF，本地也就是 LF（不会被 PowerShell 换成 CRLF）
    [System.IO.File]::WriteAllBytes($path, $bytes)

    if ($null -eq $localSha) {
        $added++
        Write-Host "  + $($item.path)"
    } else {
        $updated++
        Write-Host "  ~ $($item.path)"
    }
}

if ($DryRun) {
    Write-Host "（DryRun，什么都没写）"
} else {
    Write-Host "完成：新增 $added，更新 $updated，本来就一致 $same"
    Write-Host '提醒：本地 git 历史仍然落后 —— 网络恢复后跑 `git fetch --tags --prune && git reset --hard origin/main`'
}
