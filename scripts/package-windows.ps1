#Requires -Version 7.0
[CmdletBinding()]
param([switch]$SkipBuild)

$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
if (-not $IsWindows -or [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture -ne 'X64') {
    throw 'This release recipe requires Windows x64.'
}

function Invoke-Checked {
    param([string]$Program, [string[]]$Arguments)
    & $Program @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$Program failed with exit code $LASTEXITCODE." }
}

Push-Location $repo
try {
    $config = Get-Content -LiteralPath 'src-tauri\tauri.conf.json' -Raw | ConvertFrom-Json
    $version = $config.version
    if ($version -notmatch '^\d+\.\d+\.\d+$' -or $config.productName -ne 'NightBloom') {
        throw 'Update this recipe for the configured product/version before packaging.'
    }
    if (-not (Test-Path -LiteralPath 'LICENSE' -PathType Leaf)) {
        throw 'Choose and add the project LICENSE before distributing a release.'
    }
    if (-not $SkipBuild) {
        Invoke-Checked node @('scripts\third-party-notices.mjs')
        Push-Location 'frontend'
        try { Invoke-Checked npm.cmd @('run', 'desktop:bundle') }
        finally { Pop-Location }
    }

    $release = Join-Path $repo 'src-tauri\target\release'
    $installerName = "NightBloom_${version}_x64-setup.exe"
    $installer = Join-Path $release "bundle\nsis\$installerName"
    foreach ($file in @(
        $installer, (Join-Path $release 'astroimp-desktop.exe'),
        (Join-Path $repo 'frontend\dist\index.html'),
        (Join-Path $repo 'THIRD_PARTY_NOTICES.md')
    )) {
        if (-not (Test-Path -LiteralPath $file -PathType Leaf)) { throw "Required release file missing: $file" }
    }

    $output = Join-Path $repo "artifacts\$version"
    New-Item -ItemType Directory -Path $output -Force | Out-Null
    $scratch = [System.IO.Path]::GetFullPath((Join-Path ([System.IO.Path]::GetTempPath()) (
        'nightbloom-package-' + [guid]::NewGuid().ToString('N')
    )))
    $portable = Join-Path $scratch "NightBloom-$version"
    New-Item -ItemType Directory -Path $portable | Out-Null
    try {
        Copy-Item -LiteralPath (Join-Path $release 'astroimp-desktop.exe') -Destination $portable
        Copy-Item -LiteralPath (Join-Path $repo 'frontend\dist') -Destination (Join-Path $portable 'frontend') -Recurse
        Copy-Item -LiteralPath (Join-Path $repo 'docs\GETTING_STARTED.md') -Destination $portable
        Copy-Item -LiteralPath (Join-Path $repo 'LICENSE') -Destination (Join-Path $portable 'LICENSE.txt')
        Copy-Item -LiteralPath (Join-Path $repo 'THIRD_PARTY_NOTICES.md') -Destination $portable
        $commit = (& git rev-parse HEAD).Trim()
        if ($LASTEXITCODE -ne 0) { throw 'Cannot identify the packaged source commit.' }
        $changes = & git status --porcelain
        if ($LASTEXITCODE -ne 0) { throw 'Cannot check source-tree cleanliness.' }
        @{
            product = 'NightBloom'; version = $version; platform = 'windows-x64'
            commit = $commit; working_tree_dirty = [bool]$changes; signed = $false
        } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $portable 'BUILD-INFO.json') -Encoding utf8NoBOM
        @'
NightBloom - Windows desktop preview

Open astroimp-desktop.exe. Keep the frontend folder beside it.
See GETTING_STARTED.md for setup, optional Aladin, and troubleshooting.
Requires Microsoft Edge WebView2 Runtime. Python and Aladin are not required.
This preview is unsigned. No personal projects or library data are included.
'@ | Set-Content -LiteralPath (Join-Path $portable 'README.txt') -Encoding utf8NoBOM

        $zipName = "NightBloom_${version}_x64-portable.zip"
        Compress-Archive -LiteralPath $portable -DestinationPath (Join-Path $output $zipName) -Force
        Copy-Item -LiteralPath $installer -Destination (Join-Path $output $installerName) -Force
        Copy-Item -LiteralPath (Join-Path $portable 'BUILD-INFO.json') -Destination $output -Force
        @($installerName, $zipName) | ForEach-Object {
            $hash = Get-FileHash -LiteralPath (Join-Path $output $_) -Algorithm SHA256
            "$($hash.Hash.ToLowerInvariant())  $_"
        } | Set-Content -LiteralPath (Join-Path $output 'SHA256SUMS.txt') -Encoding ascii
        Write-Host "Release files: $output"
        Get-ChildItem -LiteralPath $output | Select-Object Name, Length
    }
    finally { Remove-Item -LiteralPath $scratch -Recurse -Force }
}
finally { Pop-Location }
