<#
.SYNOPSIS
    Copy the files the remaster needs but the retail game doesn't ship, from the
    Demo install into the retail install.
.PARAMETER Force
    Overwrite retail files that already exist (default: skip).
#>
[CmdletBinding()]
param([switch]$Force)

$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\CwaCommon.ps1"

$retail = Get-CwaRetailDir
$demo   = Get-CwaDemoDir
if (-not $retail) { throw "Retail install (Steam App $CwaRetailAppId) not found in registry." }
if (-not $demo)   { throw "Demo install (Steam App $CwaDemoAppId) not found in registry." }

Write-Host "Retail: $retail"
Write-Host "Demo:   $demo`n"

$items = @('fonts', 'dtaExt', 'AddOns\cwr_logo.pbo', 'BIN')

$copied = 0; $skipped = 0
foreach ($rel in $items)
{
    $src = Join-Path $demo $rel
    if (-not (Test-Path $src)) { Write-Warning "Demo is missing '$rel' - skipping."; continue }

    Write-Host "[$rel]"
    foreach ($file in Get-ChildItem -LiteralPath $src -Recurse -File)
    {
        $relPath = $file.FullName.Substring($demo.Length).TrimStart('\')
        $dest    = Join-Path $retail $relPath
        if ((Test-Path -LiteralPath $dest) -and -not $Force) { $skipped++; continue }

        $destDir = Split-Path $dest -Parent
        if (-not (Test-Path -LiteralPath $destDir)) { New-Item -ItemType Directory -Path $destDir -Force | Out-Null }
        Copy-Item -LiteralPath $file.FullName -Destination $dest -Force
        Write-Host "  + $relPath"
        $copied++
    }
}

Write-Host "`nDone. $copied copied, $skipped already present." -ForegroundColor Green
