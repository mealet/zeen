<#
.SYNOPSIS
Installs zeen from a GitHub release.

.DESCRIPTION
Copies bin, share and lib into a prefix (default %LOCALAPPDATA%\Programs\zeen),
adds bin to the user PATH and verifies the installation with zeen --version.

.EXAMPLE
install.ps1

.EXAMPLE
install.ps1 -Version v0.1.0 -Prefix C:\tools\zeen
#>
[CmdletBinding()]
param(
    [string]$Version = "",
    [string]$Prefix = "",
    [switch]$System
)

$PrevEAP = $ErrorActionPreference

try {
    [Net.ServicePointManager]::SecurityProtocol =
        [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
} catch {}

$Repo = if ($env:ZEEN_REPO) { $env:ZEEN_REPO } else { 'mealet/zeen' }
$ApiUrl = if ($env:ZEEN_API_URL) { $env:ZEEN_API_URL } else { 'https://api.github.com' }

if (-not $Prefix) {
    if ($System) {
        $Prefix = Join-Path $env:ProgramFiles 'zeen'
    } elseif ($env:ZEEN_PREFIX) {
        $Prefix = $env:ZEEN_PREFIX
    } else {
        $Prefix = Join-Path $env:LOCALAPPDATA 'Programs\zeen'
    }
}

$Headers = @{ Accept = 'application/vnd.github+json' }
if ($env:GITHUB_TOKEN) {
    $Headers['Authorization'] = "Bearer $($env:GITHUB_TOKEN)"
}

if ($Version -and -not $Version.StartsWith('v')) { $Version = "v$Version" }

$ReleaseUrl = if ($Version) {
    "$ApiUrl/repos/$Repo/releases/tags/$Version"
} else {
    "$ApiUrl/repos/$Repo/releases/latest"
}

try {
    $release = Invoke-RestMethod -Headers $Headers -Uri $ReleaseUrl
} catch {
    throw "unable to read the release from ${ReleaseUrl}: $($_.Exception.Message)"
}

$Tag = $release.tag_name
if (-not $Tag) { throw 'no release tag found' }

$Asset = $release.assets |
    Where-Object { $_.name -like '*-windows-x86_64.zip' -or $_.name -like '*-windows-x86_64.tar.gz' } |
    Select-Object -First 1
if (-not $Asset) { throw "release $Tag carries no build for windows-x86_64" }

$Url = $Asset.browser_download_url
$Name = $Asset.name

Write-Host "release: $Tag"
Write-Host "asset:   $Name"

$Tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("zeen-" + [System.IO.Path]::GetRandomFileName())
New-Item -ItemType Directory -Path $Tmp -Force | Out-Null

try {
    $ErrorActionPreference = 'Stop'
    $ZipPath = Join-Path $Tmp $Name
    try {
        Invoke-WebRequest -Uri $Url -OutFile $ZipPath -UseBasicParsing
    } catch {
        throw "download failed for ${Name}: $($_.Exception.Message)"
    }

    Expand-Archive -Path $ZipPath -DestinationPath $Tmp -Force

    $Root = Get-ChildItem -Path $Tmp -Directory |
        Where-Object { Test-Path (Join-Path $_.FullName 'bin\zeen.exe') } |
        Select-Object -First 1
    if (-not $Root) { throw 'the archive does not contain bin\zeen.exe' }

    $BinDir = Join-Path $Prefix 'bin'
    $ShareDir = Join-Path $Prefix 'share\zeen'
    $StdDir = Join-Path $ShareDir 'std'
    $LibDir = Join-Path $Prefix 'lib\zeen'

    New-Item -ItemType Directory -Path $BinDir -Force | Out-Null
    if (Test-Path $StdDir) { Remove-Item -Recurse -Force $StdDir }
    New-Item -ItemType Directory -Path $ShareDir -Force | Out-Null

    Copy-Item -Path (Join-Path $Root.FullName 'bin\zeen.exe') -Destination $BinDir -Force
    Copy-Item -Path (Join-Path $Root.FullName 'share\zeen\std') -Destination $StdDir -Recurse -Force

    if (Test-Path (Join-Path $Root.FullName 'lib\zeen')) {
        New-Item -ItemType Directory -Path $LibDir -Force | Out-Null
        Copy-Item -Path (Join-Path $Root.FullName 'lib\zeen\*') -Destination $LibDir -Recurse -Force
    }

    $UserPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $Entries = @()
    if ($UserPath) { $Entries = $UserPath -split ';' | Where-Object { $_ } }
    if ($Entries -notcontains $BinDir) {
        $NewPath = ($Entries + $BinDir) -join ';'
        [Environment]::SetEnvironmentVariable('Path', $NewPath, 'User')
        Write-Host "added $BinDir to your user PATH"
    } else {
        Write-Host "$BinDir is already on your user PATH"
    }

    & (Join-Path $BinDir 'zeen.exe') --version

    Write-Host "installed zeen into $Prefix"
    Write-Host 'open a new terminal for the PATH change to take effect'
} finally {
    $ErrorActionPreference = $PrevEAP
    if (Test-Path $Tmp) { Remove-Item -Recurse -Force $Tmp -ErrorAction SilentlyContinue }
}
