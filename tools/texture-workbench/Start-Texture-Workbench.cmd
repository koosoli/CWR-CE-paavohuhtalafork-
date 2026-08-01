@echo off
setlocal
set "WORKBENCH_DIR=%~dp0"
powershell -NoExit -ExecutionPolicy Bypass -Command "Set-Location -LiteralPath '%WORKBENCH_DIR%'; Write-Host ''; Write-Host 'Arma CWA Texture Workbench' -ForegroundColor Green; Write-Host 'Original PBOs are never modified. Read README.md for the three-step quick start.'; Write-Host ''; .\PoseidonTools.exe texture --help"
