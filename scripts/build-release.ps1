[CmdletBinding()]
param(
    [string]$Version,
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$RepoRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$Utf8NoBom = New-Object Text.UTF8Encoding($false)

function Get-WorkspaceVersion {
    $cargoToml = [IO.File]::ReadAllText((Join-Path $RepoRoot "Cargo.toml"))
    $section = [regex]::Match(
        $cargoToml,
        '(?ms)^\[workspace\.package\]\s*(.*?)(?=^\[|\z)'
    )
    if (-not $section.Success) {
        throw "Cargo.toml is missing [workspace.package]"
    }
    $value = [regex]::Match($section.Groups[1].Value, '(?m)^version\s*=\s*"([^"]+)"')
    if (-not $value.Success) {
        throw "Cargo.toml is missing workspace.package.version"
    }
    return $value.Groups[1].Value
}

function Assert-ChildPath {
    param([string]$Parent, [string]$Child)

    $parentFull = [IO.Path]::GetFullPath($Parent).TrimEnd('\', '/')
    $childFull = [IO.Path]::GetFullPath($Child)
    if (-not $childFull.StartsWith($parentFull + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to modify a path outside $parentFull`: $childFull"
    }
}

function Find-Cargo {
    $command = Get-Command cargo -ErrorAction SilentlyContinue
    if ($null -ne $command) {
        return $command.Source
    }
    $fallback = Join-Path $env:USERPROFILE ".cargo\bin\cargo.exe"
    if (Test-Path -LiteralPath $fallback -PathType Leaf) {
        return $fallback
    }
    throw "cargo.exe was not found. Install the Rust GNU toolchain first."
}

$workspaceVersion = Get-WorkspaceVersion
if ([string]::IsNullOrWhiteSpace($Version)) {
    $Version = $workspaceVersion
}
if ($Version -ne $workspaceVersion) {
    throw "Requested version $Version does not match Cargo.toml version $workspaceVersion"
}
if ($Version -notmatch '^\d+\.\d+\.\d+([+-][0-9A-Za-z.-]+)?$') {
    throw "Unsupported release version: $Version"
}

$packageName = "RuDock-$Version-windows-x64"
$distRoot = [IO.Path]::GetFullPath((Join-Path $RepoRoot "dist"))
$stageDir = [IO.Path]::GetFullPath((Join-Path $distRoot $packageName))
$archivePath = [IO.Path]::GetFullPath((Join-Path $distRoot "$packageName.zip"))
$archiveHashPath = "$archivePath.sha256"
Assert-ChildPath -Parent $RepoRoot -Child $distRoot
Assert-ChildPath -Parent $distRoot -Child $stageDir
Assert-ChildPath -Parent $distRoot -Child $archivePath

Push-Location $RepoRoot
try {
    if (-not $SkipBuild) {
        $mingwBin = Join-Path $RepoRoot ".toolchain\mingw64\bin"
        if (Test-Path -LiteralPath $mingwBin -PathType Container) {
            $env:Path = "$mingwBin;$env:Path"
        }
        $cargo = Find-Cargo
        & $cargo build --workspace --release --locked
        if ($LASTEXITCODE -ne 0) {
            throw "cargo build failed with exit code $LASTEXITCODE"
        }
    }

    New-Item -ItemType Directory -Path $distRoot -Force | Out-Null
    if (Test-Path -LiteralPath $stageDir) {
        Remove-Item -LiteralPath $stageDir -Recurse -Force
    }
    New-Item -ItemType Directory -Path $stageDir | Out-Null

    $binaries = @("wb.exe", "wb-daemon.exe", "wb-panel.exe", "wb-hook-poc.exe", "wb-mcp.exe")
    foreach ($binary in $binaries) {
        $source = Join-Path $RepoRoot "target\release\$binary"
        if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
            throw "Release binary is missing: $source"
        }
        Copy-Item -LiteralPath $source -Destination $stageDir
    }

    $loader = Join-Path $RepoRoot ".wv2-sdk\runtimes\win-x64\native\WebView2Loader.dll"
    if (-not (Test-Path -LiteralPath $loader -PathType Leaf)) {
        throw "WebView2Loader.dll is missing: $loader"
    }
    Copy-Item -LiteralPath $loader -Destination $stageDir

    $panelAssetDir = Join-Path $stageDir "assets\panel-ui"
    New-Item -ItemType Directory -Path $panelAssetDir -Force | Out-Null
    Copy-Item -LiteralPath (Join-Path $RepoRoot "assets\panel-ui\index.html") -Destination $panelAssetDir

    $docsAssetDir = Join-Path $stageDir "docs-assets"
    New-Item -ItemType Directory -Path $docsAssetDir -Force | Out-Null
    foreach ($asset in @(
        "v8-final.png",
        "desktop-widgets.png",
        "ai-spotlight-glow.png",
        "m5-plugins-installed-uninstall.png",
        "m5-market-page.png"
    )) {
        Copy-Item -LiteralPath (Join-Path $RepoRoot "docs-assets\$asset") -Destination $docsAssetDir
    }

    $pluginDest = Join-Path $stageDir "plugins"
    New-Item -ItemType Directory -Path $pluginDest -Force | Out-Null
    Copy-Item -Path (Join-Path $RepoRoot "plugins\*") -Destination $pluginDest -Recurse

    foreach ($document in @("README.md", "AGENT_INTEGRATION.md", "LICENSE")) {
        $source = Join-Path $RepoRoot $document
        if (Test-Path -LiteralPath $source -PathType Leaf) {
            Copy-Item -LiteralPath $source -Destination $stageDir
        }
    }
    Copy-Item -LiteralPath (Join-Path $RepoRoot "scripts\install-portable.ps1") -Destination (Join-Path $stageDir "install.ps1")
    Copy-Item -LiteralPath (Join-Path $RepoRoot "scripts\uninstall-portable.ps1") -Destination (Join-Path $stageDir "uninstall.ps1")
    Copy-Item -LiteralPath (Join-Path $RepoRoot "scripts\start-hidden.vbs") -Destination (Join-Path $stageDir "start-hidden.vbs")
    [IO.File]::WriteAllText((Join-Path $stageDir "VERSION"), "$Version`r`n", $Utf8NoBom)

    $manifestLines = Get-ChildItem -LiteralPath $stageDir -File -Recurse |
        Sort-Object FullName |
        ForEach-Object {
            $relative = $_.FullName.Substring($stageDir.Length + 1).Replace('\', '/')
            $hash = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
            "$hash  $relative"
        }
    [IO.File]::WriteAllLines((Join-Path $stageDir "SHA256SUMS.txt"), $manifestLines, $Utf8NoBom)

    $versionOutput = (& (Join-Path $stageDir "wb.exe") --version | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or $versionOutput -notmatch [regex]::Escape($Version)) {
        throw "Staged wb.exe failed its version smoke test: $versionOutput"
    }

    foreach ($output in @($archivePath, $archiveHashPath)) {
        if (Test-Path -LiteralPath $output) {
            Remove-Item -LiteralPath $output -Force
        }
    }
    Compress-Archive -LiteralPath $stageDir -DestinationPath $archivePath -CompressionLevel Optimal

    $verifyRoot = Join-Path $distRoot ".verify-$([Guid]::NewGuid().ToString('N'))"
    Assert-ChildPath -Parent $distRoot -Child $verifyRoot
    try {
        Expand-Archive -LiteralPath $archivePath -DestinationPath $verifyRoot
        $expanded = Join-Path $verifyRoot $packageName
        foreach ($required in @(
            "wb.exe",
            "wb-daemon.exe",
            "wb-panel.exe",
            "wb-hook-poc.exe",
            "wb-mcp.exe",
            "WebView2Loader.dll",
            "assets\panel-ui\index.html",
            "docs-assets\v8-final.png",
            "plugins\stopwatch\plugin.json",
            "README.md",
            "LICENSE",
            "install.ps1",
            "uninstall.ps1",
            "start-hidden.vbs",
            "SHA256SUMS.txt"
        )) {
            $path = Join-Path $expanded $required
            if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
                throw "Portable archive is missing $required"
            }
        }
    }
    finally {
        if (Test-Path -LiteralPath $verifyRoot) {
            Remove-Item -LiteralPath $verifyRoot -Recurse -Force
        }
    }

    $archiveHash = (Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash.ToLowerInvariant()
    [IO.File]::WriteAllText(
        $archiveHashPath,
        "$archiveHash  $([IO.Path]::GetFileName($archivePath))`r`n",
        $Utf8NoBom
    )

    Write-Host "Portable release ready: $archivePath"
    Write-Host "SHA-256: $archiveHash"
}
finally {
    Pop-Location
}
