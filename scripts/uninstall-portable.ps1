[CmdletBinding()]
param(
    [string]$InstallDir = (Split-Path -Parent $MyInvocation.MyCommand.Path),
    [switch]$PurgeData,
    [switch]$KeepFiles,
    [switch]$NoRuntimeChanges
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest
$InstallDir = [IO.Path]::GetFullPath($InstallDir).TrimEnd('\')
$cli = Join-Path $InstallDir "wb.exe"
if (-not $NoRuntimeChanges -and (Test-Path -LiteralPath $cli -PathType Leaf)) {
    & $cli settings win false --json *> $null
    & $cli settings autostart false --json *> $null
    & $cli daemon stop --json *> $null
}

$startMenu = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\RuDock.lnk"
if (Test-Path -LiteralPath $startMenu) {
    Remove-Item -LiteralPath $startMenu -Force
}
$key = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\RuDock"
if (Test-Path -LiteralPath $key) {
    Remove-Item -LiteralPath $key -Recurse -Force
}

if ($PurgeData) {
    foreach ($data in @(
        (Join-Path $env:APPDATA "WB"),
        (Join-Path $env:LOCALAPPDATA "WB")
    )) {
        if (Test-Path -LiteralPath $data) {
            Remove-Item -LiteralPath $data -Recurse -Force
        }
    }
}

if (-not $KeepFiles -and (Test-Path -LiteralPath $InstallDir)) {
    $escaped = $InstallDir.Replace("'", "''")
    Start-Process -FilePath powershell.exe -WindowStyle Hidden -ArgumentList @(
        "-NoProfile",
        "-ExecutionPolicy", "Bypass",
        "-Command", "Start-Sleep -Milliseconds 500; Remove-Item -LiteralPath '$escaped' -Recurse -Force -ErrorAction SilentlyContinue"
    )
}

[pscustomobject]@{
    uninstalled = $true
    data_purged = [bool]$PurgeData
    install_dir = $InstallDir
} | ConvertTo-Json -Compress
