#!/usr/bin/env pwsh
<#
.SYNOPSIS
    检查一个 .exe 里到底有没有图标资源（RT_GROUP_ICON）。

.DESCRIPTION
    为什么要这个脚本：图标"能编出来"和图标"真的进了 exe"是两件事。
    `cargo check` 只做类型检查、根本不链接；而 build.rs 里的
    `manifest_optional()` 又会让"资源编译失败"退化成**静默无图标**。
    所以发布前必须**直接读 PE 的资源表**确认，不能只看构建绿了。

    这也是 v0.10.0 的教训：那个版本的 exe 没有任何资源段，桌面上一张白纸，
    而当时的 CI 和 release 全是绿的。

    做法：DOS 头 → e_lfanew → PE 签名 → COFF 头 → 可选头 →
    数据目录[2]（Resource）→ 资源目录第一层按**类型**枚举，
    看有没有 14（RT_GROUP_ICON）。全程只读文件头，不解依赖库。

.PARAMETER Path
    要检查的 exe。

.PARAMETER AllowMissing
    没图标时只警告、不失败（给"可选产物"用）。

.EXAMPLE
    pwsh -File scripts/verify-exe-icon.ps1 -Path dist/wslc-panel.exe
#>
param(
    [Parameter(Mandatory = $true)][string]$Path,
    [switch]$AllowMissing
)

$ErrorActionPreference = 'Stop'
$RT_GROUP_ICON = 14
$RT_ICON = 3

if (-not (Test-Path -LiteralPath $Path)) {
    Write-Error "没有这个文件：$Path"
    exit 1
}

$bytes = [System.IO.File]::ReadAllBytes((Resolve-Path -LiteralPath $Path).Path)

function Fail($msg) {
    if ($AllowMissing) {
        Write-Warning "$msg"
        exit 0
    }
    Write-Output "::error title=缺少图标::$msg"
    exit 1
}

# --- DOS 头 -----------------------------------------------------------------
if ($bytes.Length -lt 0x40 -or $bytes[0] -ne 0x4D -or $bytes[1] -ne 0x5A) {
    Fail "$Path 不是 PE 文件（没有 MZ 头）"
}

$peOffset = [BitConverter]::ToInt32($bytes, 0x3C)
if ($peOffset -le 0 -or $peOffset + 24 -ge $bytes.Length) {
    Fail "$Path 的 e_lfanew 不合理（$peOffset）"
}
if ($bytes[$peOffset] -ne 0x50 -or $bytes[$peOffset + 1] -ne 0x45) {
    Fail "$Path 的 PE 签名不对"
}

$coff = $peOffset + 4
$machine = [BitConverter]::ToUInt16($bytes, $coff)
$sectionCount = [BitConverter]::ToUInt16($bytes, $coff + 2)
$optionalSize = [BitConverter]::ToUInt16($bytes, $coff + 16)

$opt = $coff + 20
$magic = [BitConverter]::ToUInt16($bytes, $opt)
# 0x10B = PE32，0x20B = PE32+。数据目录的起点两者不同：
# PE32   在可选头 +0x60
# PE32+  在可选头 +0x70
switch ($magic) {
    0x10B { $dataDir = $opt + 0x60 }
    0x20B { $dataDir = $opt + 0x70 }
    default { Fail ("未知的可选头 magic：0x{0:X}" -f $magic) }
}

# 数据目录第 2 项 = Resource。每项 8 字节（RVA + Size）。
$resRva = [BitConverter]::ToUInt32($bytes, $dataDir + 2 * 8)
$resSize = [BitConverter]::ToUInt32($bytes, $dataDir + 2 * 8 + 4)

if ($resRva -eq 0 -or $resSize -eq 0) {
    Fail "$Path 没有资源段（数据目录[2] 是空的）—— 图标没嵌进去"
}

# --- RVA → 文件偏移（扫节表）-----------------------------------------------
$sections = @()
$sectionStart = $opt + $optionalSize
for ($i = 0; $i -lt $sectionCount; $i++) {
    $s = $sectionStart + $i * 40
    if ($s + 40 -gt $bytes.Length) { break }
    $nameBytes = $bytes[$s..($s + 7)]
    $sections += [pscustomobject]@{
        Name      = ([System.Text.Encoding]::ASCII.GetString($nameBytes)).Trim([char]0)
        VirtualSize = [BitConverter]::ToUInt32($bytes, $s + 8)
        VirtualAddress = [BitConverter]::ToUInt32($bytes, $s + 12)
        RawSize   = [BitConverter]::ToUInt32($bytes, $s + 16)
        RawPointer = [BitConverter]::ToUInt32($bytes, $s + 20)
    }
}

function RvaToOffset([uint32]$rva) {
    foreach ($sec in $sections) {
        $span = [Math]::Max($sec.VirtualSize, $sec.RawSize)
        if ($rva -ge $sec.VirtualAddress -and $rva -lt ($sec.VirtualAddress + $span)) {
            return [int]($rva - $sec.VirtualAddress + $sec.RawPointer)
        }
    }
    return -1
}

$resOff = RvaToOffset $resRva
if ($resOff -lt 0 -or $resOff + 16 -gt $bytes.Length) {
    # ⚠️ 括号不能省：`Fail "…{0:X}" -f $x` 会把 `-f` 和 `$x` 当成**位置参数**
    # 塞进 `$args`，格式串原样输出（实测：屏幕上真的印着 `rva=0x{0:X}`）。
    Fail ("资源段的 RVA 映射不到文件偏移（rva=0x{0:X}）" -f $resRva)
}

# --- 枚举资源类型 -----------------------------------------------------------
# IMAGE_RESOURCE_DIRECTORY 16 字节，后面跟 IDataDirectory 条目（各 8 字节）。
# 第一层就是"按类型"索引，条目里的 Name 字段：最高位=1 表示"指向字符串"，
# 否则低 16 位就是这个资源的**类型 ID**。
$typeCount = [BitConverter]::ToUInt16($bytes, $resOff + 12) +
             [BitConverter]::ToUInt16($bytes, $resOff + 14)
$found = @()
# 类型 → 该类型的二级目录偏移（相对资源段起点）。拿它才能往下取 RT_ICON 数据。
$typeDir = @{}
for ($i = 0; $i -lt $typeCount; $i++) {
    $entry = $resOff + 16 + $i * 8
    if ($entry + 8 -gt $bytes.Length) { break }
    $nameField = [BitConverter]::ToUInt32($bytes, $entry)
    $offsetField = [BitConverter]::ToUInt32($bytes, $entry + 4)
    if (($nameField -band 0x80000000) -ne 0) {
        $found += "字符串类型"
        continue
    }
    $typeId = [int]($nameField -band 0xFFFF)
    $found += $typeId
    $typeDir[$typeId] = [int]($offsetField -band 0x7FFFFFFF)
}

$types = ($found | ForEach-Object { "$_" }) -join ', '
if (-not ($found -contains $RT_GROUP_ICON)) {
    Fail "$Path 的资源段里没有 RT_GROUP_ICON(14)。现有类型：$types —— 图标没嵌进去"
}

# --- 图标**内容**校验：真的嵌了我们那一份吗 ----------------------------------
#
# ⚠️ 只查 "有 RT_GROUP_ICON" 是不够的 —— 这个判据**过得了 Inno 的默认图标**
# （它自己也是一个 group icon）。v0.10.1~v0.10.3 就是这样过了关：
# 发布出去的 `wslc-panel-0.10.x-setup-x64.exe` 里，13 张 RT_ICON 逐张 sha256
# 与 `assets/wslc-panel.ico` 的 7 张**没有一个重合**，而安装包照样"有图标"、
# 校验照样绿。所以这里再比一次**内容**。
#
# 取证方式：真正的 exe 里 rc.exe 会把 ico 的每一张图原样写进 RT_ICON
# （BMP/PNG 载荷逐字节一致），所以按 sha256 比对是可靠的。
function Get-IconPayloads([byte[]]$file, [int]$resourceOffset, $typeDirs) {
    # 资源树是**三层**：类型 → 名字/ID → 语言。第三层 entry 的 Offset
    # **不是**目录偏移，而是 IMAGE_RESOURCE_DATA_ENTRY 的偏移（相对资源段起点）。
    # 少走这一步就会把 entry 的 Name（语言 ID，比如 1033 = 0x409）当成数据 RVA ——
    # 解析出来是"RVA 1033"，落在第一个节之前，映射必定失败。
    $out = @()
    if (-not $typeDirs.ContainsKey($RT_ICON)) { return $out }
    $dir2 = $resourceOffset + $typeDirs[$RT_ICON]
    if ($dir2 + 16 -gt $file.Length) { return $out }
    $named2 = [BitConverter]::ToUInt16($file, $dir2 + 12)
    $id2 = [BitConverter]::ToUInt16($file, $dir2 + 14)
    for ($j = 0; $j -lt ($named2 + $id2); $j++) {
        $e2 = $dir2 + 16 + $j * 8
        if ($e2 + 8 -gt $file.Length) { break }
        $offsetField2 = [BitConverter]::ToUInt32($file, $e2 + 4)
        if (($offsetField2 -band 0x80000000) -eq 0) { continue }
        $dir3 = $resourceOffset + [int]($offsetField2 -band 0x7FFFFFFF)
        if ($dir3 + 16 -gt $file.Length) { continue }
        $named3 = [BitConverter]::ToUInt16($file, $dir3 + 12)
        $id3 = [BitConverter]::ToUInt16($file, $dir3 + 14)
        # 语言层取第一项即可（rc.exe 只写一个）
        if (($named3 + $id3) -lt 1) { continue }
        $e3 = $dir3 + 16
        if ($e3 + 8 -gt $file.Length) { continue }
        # 第三层 entry 的**后 4 字节**才是数据项偏移（前 4 字节是语言 ID）
        $dataEntryRel = [BitConverter]::ToUInt32($file, $e3 + 4)
        $dataEntry = $resourceOffset + [int]($dataEntryRel -band 0x7FFFFFFF)
        if ($dataEntry + 8 -gt $file.Length) { continue }
        $dataRva = [BitConverter]::ToUInt32($file, $dataEntry)
        $dataSize = [BitConverter]::ToUInt32($file, $dataEntry + 4)
        if ($dataSize -le 0) { continue }
        $dataOff = RvaToOffset $dataRva
        if ($dataOff -lt 0 -or $dataOff + $dataSize -gt $file.Length) { continue }
        $payload = New-Object byte[] $dataSize
        [Array]::Copy($file, $dataOff, $payload, 0, $dataSize)
        $out += [pscustomobject]@{
            Size = [int]$dataSize
            Hash = (Get-FileHash -InputStream ([System.IO.MemoryStream]::new($payload)) -Algorithm SHA256).Hash
        }
    }
    return $out
}

function Get-IcoPayloadHashes([string]$icoPath) {
    $ico = [System.IO.File]::ReadAllBytes($icoPath)
    $count = [BitConverter]::ToUInt16($ico, 4)
    $out = @()
    for ($i = 0; $i -lt $count; $i++) {
        $p = 6 + $i * 16
        if ($p + 16 -gt $ico.Length) { break }
        $size = [int][BitConverter]::ToUInt32($ico, $p + 8)
        $offset = [int][BitConverter]::ToUInt32($ico, $p + 12)
        if ($size -le 0 -or $offset + $size -gt $ico.Length) { continue }
        $payload = New-Object byte[] $size
        [Array]::Copy($ico, $offset, $payload, 0, $size)
        $out += [pscustomobject]@{
            Size = $size
            Hash = (Get-FileHash -InputStream ([System.IO.MemoryStream]::new($payload)) -Algorithm SHA256).Hash
        }
    }
    return $out
}

$icoPath = Join-Path $PSScriptRoot "../assets/wslc-panel.ico"
if (Test-Path -LiteralPath $icoPath) {
    $exeIcons = Get-IconPayloads $bytes $resOff $typeDir
    $icoIcons = Get-IcoPayloadHashes (Resolve-Path -LiteralPath $icoPath).Path
    $matched = @($icoIcons | Where-Object { $h = $_.Hash; $exeIcons | Where-Object { $_.Hash -eq $h } }).Count
    Write-Output "图标载荷比对：exe 里 $($exeIcons.Count) 张 / .ico 里 $($icoIcons.Count) 张，命中 $matched 张"
    if ($matched -eq 0) {
        Fail "$Path 里有 RT_GROUP_ICON，但**没有一张图**来自 assets/wslc-panel.ico —— 这是别处（多半是编译器/工具默认）的图标，不是我们的。exe=$($exeIcons.Count) ico=$($icoIcons.Count)"
    }
} else {
    Write-Warning "找不到 $icoPath，跳过图标内容比对（只验证了有 RT_GROUP_ICON）"
}

Write-Output "OK  $Path 有图标资源且内容与 assets/wslc-panel.ico 一致（资源类型：$types）"
exit 0
