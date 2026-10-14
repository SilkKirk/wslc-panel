# 用 GitHub API 把本地若干个文件推成一个提交（不需要 `git push`）。
#
# 为什么需要它：`github.com:443` 在这台机器上经常连不上，而 `api.github.com`
# 通常通（`AGENTS.md` §7.6）。这条路是**逐字节精确**的 —— 直接上传本地文件的
# 内容，不手抄、不重新格式化。
#
# 它做的事（对应 REST 的四步）：
#
#   POST /git/blobs   每个文件一个 blob（base64）
#   POST /git/trees   `base_tree` = 基线分支那棵树，只替换这几个文件
#   POST /git/commits 父提交 = 基线分支的 head
#   PATCH|POST /git/refs/heads/<branch>
#
# 用法：
#   # 直接更新 main（版本号之类的收尾提交）
#   pwsh -NoProfile -File scripts/gh-api-push.ps1 -Paths Cargo.toml,Cargo.lock,README.md -MessageFile msg.txt
#
#   # 在 main 头上开一个新分支（功能分支 / 开 PR 用）
#   pwsh -NoProfile -File scripts/gh-api-push.ps1 -Paths src/a.rs,src/b.rs -MessageFile msg.txt -Branch feat/xxx
#
#   # 先看看会动哪些文件
#   pwsh -NoProfile -File scripts/gh-api-push.ps1 -Paths ... -MessageFile msg.txt -DryRun
#
# ⚠️ `-Paths` 要**列具体文件**，别写 `.` —— 这个工作区里经常躺着 `.tmp-*`
# 临时文件（写提交信息、走 API 的脚本），漏进去就得事后专门提交删掉。
# 脚本会主动拦下 `.tmp` 开头的路径（真要传得显式加 `-AllowTemp`）。

param(
    [Parameter(Mandatory = $true)][string[]]$Paths,
    [Parameter(Mandatory = $true)][string]$MessageFile,
    [string]$Repo = 'SilkKirk/wslc-panel',
    [string]$Base = 'main',
    [string]$Branch = '',
    [switch]$AllowTemp,
    [switch]$DryRun
)

$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$api = "https://api.github.com/repos/$Repo"
if (-not $Branch) { $Branch = $Base }

# ⚠️ `pwsh -File script.ps1 -Paths a,b,c` 里的 `a,b,c` 会是**一个字符串**
# （`-File` 模式不按 PowerShell 语法解析参数），所以这里自己拆一次 ——
# 这样 `-File` 和 `-Command` 两种调用方式都对。
$Paths = @($Paths | ForEach-Object { $_ -split ',' } | ForEach-Object { $_.Trim() } | Where-Object { $_ })

# 保险一：别把临时文件提交上去（`AGENTS.md` §5 记着这次教训）
$temps = @($Paths | Where-Object { (Split-Path $_ -Leaf) -like '.tmp*' })
if ($temps.Count -gt 0 -and -not $AllowTemp) {
    throw ("这些是临时文件，不该提交：" + ($temps -join ', ') + '（真要传就加 -AllowTemp）')
}

$msg = [System.IO.File]::ReadAllText((Resolve-Path $MessageFile).Path)
if ([string]::IsNullOrWhiteSpace($msg)) { throw '提交信息是空的' }

$cred = "protocol=https`nhost=github.com`n" | git credential fill 2>$null
$tok = ($cred | Select-String '^password=').Line -replace '^password=', ''
if (-not $tok) { throw 'git credential 里没拿到 token（先确认本机 git 配过 GitHub 凭据）' }
$H = @{
    Authorization = "token $tok"
    Accept        = 'application/vnd.github+json'
    'User-Agent'  = 'wslc-panel-gh-api-push'
}
$json = 'application/json; charset=utf-8'
$utf8 = [System.Text.Encoding]::UTF8

function PostJson($uri, $obj) {
    $body = $utf8.GetBytes(($obj | ConvertTo-Json -Depth 8 -Compress))
    return Invoke-RestMethod -Headers $H -Method Post -Uri $uri -Body $body -ContentType $json
}

$baseSha = [string](Invoke-RestMethod -Headers $H -Uri "$api/git/ref/heads/$Base").object.sha
$baseTree = [string](Invoke-RestMethod -Headers $H -Uri "$api/git/commits/$baseSha").tree.sha
Write-Host "基线 $Base = $baseSha"
Write-Host "目标 $Branch（$($Paths.Count) 个文件）"

$tree = @()
foreach ($rel in $Paths) {
    $full = Join-Path $root ($rel -replace '/', '\')
    if (-not (Test-Path -LiteralPath $full)) { throw "本地没有这个文件：$rel" }
    if ($DryRun) {
        Write-Host "  会上传 $rel"
        continue
    }
    $b64 = [Convert]::ToBase64String([System.IO.File]::ReadAllBytes($full))
    $blob = PostJson "$api/git/blobs" @{ content = $b64; encoding = 'base64' }
    Write-Host "  blob $rel -> $($blob.sha)"
    $tree += @{ path = ($rel -replace '\\', '/'); mode = '100644'; type = 'blob'; sha = [string]$blob.sha }
}

if ($DryRun) {
    Write-Host '（DryRun，什么都没提交）'
    return
}

$newTree = PostJson "$api/git/trees" @{ base_tree = $baseTree; tree = $tree }
$commit = PostJson "$api/git/commits" @{ message = $msg; tree = [string]$newTree.sha; parents = @($baseSha) }
Write-Host "提交 -> $($commit.sha)"

# 保险二：推完自查一遍 —— 让 API 告诉我们这个提交到底动了哪些文件，
# 和 `-Paths` 对不上就说明哪里想错了（曾经手滑把 .tmp 提交进去过一次）。
if ($Branch -eq $Base) {
    $refBody = $utf8.GetBytes((@{ sha = [string]$commit.sha; force = $false } | ConvertTo-Json -Compress))
    $ref = Invoke-RestMethod -Headers $H -Method Patch -Uri "$api/git/refs/heads/$Branch" -Body $refBody -ContentType $json
} else {
    try {
        $ref = PostJson "$api/git/refs" @{ ref = "refs/heads/$Branch"; sha = [string]$commit.sha }
    } catch {
        # 分支已存在 → 强制挪过去（调用方自己保证这是想要的）
        $refBody = $utf8.GetBytes((@{ sha = [string]$commit.sha; force = $true } | ConvertTo-Json -Compress))
        $ref = Invoke-RestMethod -Headers $H -Method Patch -Uri "$api/git/refs/heads/$Branch" -Body $refBody -ContentType $json
    }
}
Write-Host "$Branch -> $($ref.object.sha)"

Start-Sleep -Seconds 3
$detail = Invoke-RestMethod -Headers $H -Uri "$api/commits/$($commit.sha)"
$changed = @($detail.files | ForEach-Object { $_.filename })
Write-Host ("实际改动：" + ($changed -join ', '))

$expected = @($Paths | ForEach-Object { $_ -replace '\\', '/' })
$unexpected = @($changed | Where-Object { $expected -notcontains $_ })
$missing = @($expected | Where-Object { $changed -notcontains $_ })
if ($unexpected.Count -gt 0) { Write-Warning ('多出来的文件：' + ($unexpected -join ', ')) }
if ($missing.Count -gt 0) { Write-Warning ('没进去的文件：' + ($missing -join ', ')) }
if ($unexpected.Count -eq 0 -and $missing.Count -eq 0) { Write-Host '自查通过：只动了指定的文件 ✓' }
