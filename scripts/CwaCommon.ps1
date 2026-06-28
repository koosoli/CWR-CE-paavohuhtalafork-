# Locate the retail + Demo installs of Arma: Cold War Assault via the registry.
# Dot-source from other scripts; sets no StrictMode/preferences.

$script:CwaRetailAppId = 65790     # Arma: Cold War Assault (retail)
$script:CwaDemoAppId   = 4819000   # Arma: Cold War Assault Remastered Demo

function Get-CwaInstall
{
    # Resolve a Steam game's install dir by AppId: the "Steam App <id>" uninstall
    # key first, then a libraryfolders.vdf / appmanifest scan. $null if not found.
    param([Parameter(Mandatory)][int]$AppId)

    # 1) Uninstall key (native + WOW6432Node hives).
    $uninstallKeys = @(
        "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\Steam App $AppId"
        "HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\Steam App $AppId"
    )
    foreach ($key in $uninstallKeys)
    {
        $loc = (Get-ItemProperty -Path $key -Name InstallLocation -ErrorAction SilentlyContinue).InstallLocation
        if ($loc -and (Test-Path $loc)) { return (Resolve-Path $loc).Path }
    }

    # 2) Fallback: Steam library scan.
    $steam = (Get-ItemProperty 'HKCU:\Software\Valve\Steam' -Name SteamPath -ErrorAction SilentlyContinue).SteamPath
    if (-not $steam)
    {
        $steam = (Get-ItemProperty 'HKLM:\SOFTWARE\WOW6432Node\Valve\Steam' -Name InstallPath -ErrorAction SilentlyContinue).InstallPath
    }
    if ($steam)
    {
        $steam = ($steam -replace '/', '\').TrimEnd('\')
        $vdf = Join-Path $steam 'steamapps\libraryfolders.vdf'
        $libs = @($steam)
        if (Test-Path $vdf)
        {
            $libs += Select-String -Path $vdf -Pattern '"path"\s+"([^"]+)"' -AllMatches |
                ForEach-Object { $_.Matches } |
                ForEach-Object { $_.Groups[1].Value -replace '\\\\', '\' }
        }
        foreach ($lib in ($libs | Select-Object -Unique))
        {
            $manifest = Join-Path $lib "steamapps\appmanifest_$AppId.acf"
            if (Test-Path $manifest)
            {
                $m = Select-String -Path $manifest -Pattern '"installdir"\s+"([^"]+)"'
                if ($m)
                {
                    $dir  = $m.Matches[0].Groups[1].Value
                    $full = Join-Path $lib "steamapps\common\$dir"
                    if (Test-Path $full) { return (Resolve-Path $full).Path }
                }
            }
        }
    }

    return $null
}

function Get-CwaRetailDir { Get-CwaInstall -AppId $script:CwaRetailAppId }
function Get-CwaDemoDir   { Get-CwaInstall -AppId $script:CwaDemoAppId }
