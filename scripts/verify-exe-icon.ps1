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
    Fail "资源段的 RVA 映射不到文件偏移（rva=0x{0:X}）" -f $resRva
}

# --- 枚举资源类型 -----------------------------------------------------------
# IMAGE_RESOURCE_DIRECTORY 16 字节，后面跟 IDataDirectory 条目（各 8 字节）。
# 第一层就是"按类型"索引，条目里的 Name 字段：最高位=1 表示"指向字符串"，
# 否则低 16 位就是这个资源的**类型 ID**。
$typeCount = [BitConverter]::ToUInt16($bytes, $resOff + 12) +
             [BitConverter]::ToUInt16($bytes, $resOff + 14)
$found = @()
for ($i = 0; $i -lt $typeCount; $i++) {
    $entry = $resOff + 16 + $i * 8
    if ($entry + 8 -gt $bytes.Length) { break }
    $nameField = [BitConverter]::ToUInt32($bytes, $entry)
    if (($nameField -band 0x80000000) -ne 0) {
        $found += "字符串类型"
    } else {
        $found += ([int]($nameField -band 0xFFFF))
    }
}

$types = ($found | ForEach-Object { "$_" }) -join ', '
if ($found -contains $RT_GROUP_ICON) {
    $iconCount = 0
    if ($found -contains 3) { $iconCount = $found.Count }  # RT_ICON=3
    Write-Output "OK  $Path 有图标资源（RT_GROUP_ICON=14；资源类型：$types）"
    exit 0
}

Fail "$Path 的资源段里没有 RT_GROUP_ICON(14)。现有类型：$types —— 图标没嵌进去"
