<#
.SYNOPSIS
    Configure + build with a CMake preset, setting the toolchain env first so it
    works from any shell.
.PARAMETER Preset         CMake preset (default win-x64-clang-rwdi).
.PARAMETER Target         Build only the given target(s) (cmake --target).
.PARAMETER Clean          Delete build/<preset> first for a fresh configure.
.PARAMETER ConfigureOnly  Stop after configure.
.PARAMETER BuildArgs      Extra args passed to `cmake --build` (e.g. --parallel 8).
.EXAMPLE
    .\scripts\Build.ps1
.EXAMPLE
    .\scripts\Build.ps1 -Preset win-x64-clang-dbg -Target PoseidonGame
#>
[CmdletBinding()]
param(
    [string]$Preset = 'win-x64-clang-rwdi',
    [string[]]$Target,
    [switch]$Clean,
    [switch]$ConfigureOnly,
    # Position=0 makes the other params named-only, so cmake passthrough flags
    # (e.g. --parallel) flow here instead of binding to -Preset.
    [Parameter(Position = 0, ValueFromRemainingArguments = $true)] $BuildArgs
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path $PSScriptRoot -Parent

# vcpkg toolchain (the base preset reads $env{VCPKG_ROOT}).
$vcpkgCmake = if ($env:VCPKG_ROOT) { Join-Path $env:VCPKG_ROOT 'scripts\buildsystems\vcpkg.cmake' } else { $null }
if (-not ($vcpkgCmake -and (Test-Path $vcpkgCmake)))
{
    $fallback = 'C:\dev\vcpkg'
    if (Test-Path (Join-Path $fallback 'scripts\buildsystems\vcpkg.cmake'))
    {
        $env:VCPKG_ROOT = $fallback
    }
    else
    {
        throw "VCPKG_ROOT is not set to a valid vcpkg checkout and none was found at $fallback. " +
              "Set VCPKG_ROOT to your vcpkg directory and retry."
    }
}

# LLVM tools (clang-format / clang-tidy) must be discoverable at configure time.
$llvmBin = 'C:\Program Files\LLVM\bin'
if ((Test-Path $llvmBin) -and (($env:Path -split ';') -notcontains $llvmBin))
{
    $env:Path = "$env:Path;$llvmBin"
}

Write-Host "Repo       : $repoRoot"
Write-Host "Preset     : $Preset"
Write-Host "VCPKG_ROOT : $env:VCPKG_ROOT"
Write-Host ""

$buildDir = Join-Path $repoRoot "build\$Preset"
if ($Clean -and (Test-Path $buildDir))
{
    Write-Host "Cleaning $buildDir ..."
    Remove-Item -Recurse -Force $buildDir
}

Push-Location $repoRoot
try
{
    # Configure
    cmake --preset $Preset
    if ($LASTEXITCODE -ne 0) { throw "Configure failed (exit $LASTEXITCODE)." }

    if ($ConfigureOnly)
    {
        Write-Host "Configure complete (ConfigureOnly)." -ForegroundColor Green
        return
    }

    # Build
    $cmakeBuild = @('--build', $buildDir)
    if ($Target)    { $cmakeBuild += @('--target') + $Target }
    if ($BuildArgs) { $cmakeBuild += $BuildArgs }

    cmake @cmakeBuild
    if ($LASTEXITCODE -ne 0) { throw "Build failed (exit $LASTEXITCODE)." }

    Write-Host ""
    Write-Host "Build succeeded: $buildDir" -ForegroundColor Green
    Write-Host "Run with: .\scripts\Start.ps1"
}
finally
{
    Pop-Location
}
