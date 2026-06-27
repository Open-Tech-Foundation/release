$ErrorActionPreference = "Stop"

$Repo = "Open-Tech-Foundation/release"
$BinName = "otf-release"

$Arch = (Get-WmiObject -Class Win32_Processor).Architecture
if ($Arch -eq 9) {
    $ArchName = "x86_64"
} elseif ($Arch -eq 12) {
    $ArchName = "aarch64"
} else {
    Write-Host "Unsupported architecture. Only x86_64 and ARM64 are supported."
    exit 1
}

$AssetName = "${BinName}-windows-${ArchName}.exe"

Write-Host "Fetching latest version of $BinName..."
$ReleaseUrl = "https://api.github.com/repos/$Repo/releases/latest"
$ReleaseInfo = Invoke-RestMethod -Uri $ReleaseUrl
$Asset = $ReleaseInfo.assets | Where-Object { $_.name -eq $AssetName }

if (-not $Asset) {
    Write-Host "Error: Could not find release asset for Windows $ArchName."
    exit 1
}

$DownloadUrl = $Asset.browser_download_url
Write-Host "Downloading from $DownloadUrl..."

$InstallDir = Join-Path $env:USERPROFILE ".cargo\bin"
if (-not (Test-Path $InstallDir)) {
    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
}

$DestPath = Join-Path $InstallDir "${BinName}.exe"
Invoke-WebRequest -Uri $DownloadUrl -OutFile $DestPath

Write-Host "$BinName installed successfully to $DestPath"
Write-Host "Make sure $InstallDir is in your PATH."
