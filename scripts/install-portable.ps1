[CmdletBinding()]
param(
    [string]$Source = (Split-Path -Parent $MyInvocation.MyCommand.Path),
    [string]$Destination = (Join-Path $env:LOCALAPPDATA "Programs\RuDock"),
    [switch]$NoShortcut,
    [switch]$NoRegistry,
    [switch]$Autostart,
    [switch]$Launch
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Resolve-FullPath([string]$Path) {
    [IO.Path]::GetFullPath($Path)
}

$Source = Resolve-FullPath $Source
$Destination = Resolve-FullPath $Destination
if (-not (Test-Path -LiteralPath (Join-Path $Source "wb.exe") -PathType Leaf)) {
    throw "RuDock package is missing wb.exe: $Source"
}
foreach ($required in @("wb-daemon.exe", "wb-panel.exe", "wb-hook-poc.exe", "wb-mcp.exe", "WebView2Loader.dll", "assets\panel-ui\index.html")) {
    if (-not (Test-Path -LiteralPath (Join-Path $Source $required) -PathType Leaf)) {
        throw "RuDock package is incomplete; missing $required"
    }
}

$sourceWithSlash = $Source.TrimEnd('\') + '\'
if ($Destination.StartsWith($sourceWithSlash, [StringComparison]::OrdinalIgnoreCase)) {
    throw "Destination cannot be inside the source package"
}

$parent = Split-Path -Parent $Destination
New-Item -ItemType Directory -Path $parent -Force | Out-Null
$stage = Join-Path $parent ".RuDock-install-$([Guid]::NewGuid().ToString('N'))"
$old = Join-Path $parent ".RuDock-old-$([Guid]::NewGuid().ToString('N'))"
$movedOld = $false
try {
    New-Item -ItemType Directory -Path $stage -Force | Out-Null
    Copy-Item -Path (Join-Path $Source "*") -Destination $stage -Recurse -Force
    if (Test-Path -LiteralPath $Destination) {
        $existingCli = Join-Path $Destination "wb.exe"
        if (Test-Path -LiteralPath $existingCli -PathType Leaf) {
            & $existingCli daemon stop --json *> $null
        }
        Move-Item -LiteralPath $Destination -Destination $old
        $movedOld = $true
    }
    Move-Item -LiteralPath $stage -Destination $Destination
    $stage = $null

    $uninstall = Join-Path $Destination "uninstall.ps1"
    $uninstallTemplate = Join-Path $Source "uninstall.ps1"
    if (-not (Test-Path -LiteralPath $uninstallTemplate -PathType Leaf)) {
        $uninstallTemplate = Join-Path $Source "uninstall-portable.ps1"
    }
    if (Test-Path -LiteralPath $uninstallTemplate -PathType Leaf) {
        Copy-Item -LiteralPath $uninstallTemplate -Destination $uninstall -Force
    }

    if (-not $NoShortcut) {
        $startMenu = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\RuDock.lnk"
        New-Item -ItemType Directory -Path (Split-Path -Parent $startMenu) -Force | Out-Null
        $shell = New-Object -ComObject WScript.Shell
        $shortcut = $shell.CreateShortcut($startMenu)
        $shortcut.TargetPath = Join-Path $env:WINDIR "System32\wscript.exe"
        $shortcut.Arguments = "`"$(Join-Path $Destination 'start-hidden.vbs')`""
        $shortcut.WorkingDirectory = $Destination
        $shortcut.IconLocation = "$(Join-Path $Destination 'wb-panel.exe'),0"
        $shortcut.Description = "RuDock Windows workbench"
        $shortcut.Save()
    }

    if (-not $NoRegistry) {
        $uninstallCommand = "powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"$uninstall`" -InstallDir `"$Destination`""
        $key = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\RuDock"
        New-Item -Path $key -Force | Out-Null
        $versionPath = Join-Path $Source "VERSION"
        $version = if (Test-Path -LiteralPath $versionPath) { (Get-Content -LiteralPath $versionPath -Raw).Trim() } else { "0.1.0" }
        New-ItemProperty -Path $key -Name DisplayName -Value "RuDock" -PropertyType String -Force | Out-Null
        New-ItemProperty -Path $key -Name DisplayVersion -Value $version -PropertyType String -Force | Out-Null
        New-ItemProperty -Path $key -Name Publisher -Value "CSYYYYXX" -PropertyType String -Force | Out-Null
        New-ItemProperty -Path $key -Name InstallLocation -Value $Destination -PropertyType String -Force | Out-Null
        New-ItemProperty -Path $key -Name UninstallString -Value $uninstallCommand -PropertyType String -Force | Out-Null
    }

    if ($Autostart) {
        & (Join-Path $Destination "wb.exe") settings autostart true --json *> $null
    }
    if ($Launch) {
        Start-Process -FilePath (Join-Path $Destination "wb.exe") -ArgumentList "daemon start" -WorkingDirectory $Destination -WindowStyle Hidden
    }

    [pscustomobject]@{
        installed = $true
        destination = $Destination
        shortcut = (-not $NoShortcut)
        autostart = [bool]$Autostart
    } | ConvertTo-Json -Compress
}
catch {
    if ((Test-Path -LiteralPath $Destination -PathType Container) -and $movedOld) {
        Remove-Item -LiteralPath $Destination -Recurse -Force
    }
    if ($movedOld -and (Test-Path -LiteralPath $old)) {
        Move-Item -LiteralPath $old -Destination $Destination
    }
    throw
}
finally {
    if ($stage -and (Test-Path -LiteralPath $stage)) {
        Remove-Item -LiteralPath $stage -Recurse -Force
    }
    if ($movedOld -and (Test-Path -LiteralPath $old)) {
        Remove-Item -LiteralPath $old -Recurse -Force
    }
}
