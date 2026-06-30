$ErrorActionPreference = "Stop"

$Repo = "Open-Tech-Foundation/release"
$BinName = "otf-release"

$Arch = (Get-WmiObject -Class Win32_Processor).Architecture
if ($Arch -eq 9) {
    $ArchName = "x86_64"
} elseif ($Arch -eq 12) {
    $ArchName = "aarch64"
} else {
    Write-Error "Unsupported architecture. Only x86_64 and ARM64 are supported."
    exit 1
}

$AssetName = "${BinName}-windows-${ArchName}.exe"

Write-Host "Fetching latest version of $BinName..."
$ReleaseUrl = "https://api.github.com/repos/$Repo/releases/latest"
try {
    $ReleaseInfo = Invoke-RestMethod -Uri $ReleaseUrl
} catch {
    Write-Error "Failed to query the GitHub API ($ReleaseUrl). You may be rate limited; wait a bit and retry. $_"
    exit 1
}
$Asset = $ReleaseInfo.assets | Where-Object { $_.name -eq $AssetName }

if (-not $Asset) {
    Write-Error "Could not find release asset for Windows $ArchName."
    exit 1
}

$DownloadUrl = $Asset.browser_download_url
Write-Host "Downloading from $DownloadUrl..."

$InstallDir = Join-Path $env:USERPROFILE ".cargo\bin"
if (-not (Test-Path $InstallDir)) {
    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
}

$DestPath = Join-Path $InstallDir "${BinName}.exe"

# Download to a temp file first. Nothing touches the installed binary until the
# download has been fetched AND verified, so a failed/bogus response can never
# clobber a working install.
$TmpFile = New-TemporaryFile
try {
    Invoke-WebRequest -Uri $DownloadUrl -OutFile $TmpFile.FullName

    # Verify we actually got an executable, not an HTML/JSON error page. This is
    # the guard that prevents installing garbage over a good binary.
    if ((Get-Item $TmpFile.FullName).Length -eq 0) {
        Write-Error "Downloaded file is empty; refusing to install."
        exit 1
    }

    $fs = [System.IO.File]::OpenRead($TmpFile.FullName)
    try {
        $b0 = $fs.ReadByte()
        $b1 = $fs.ReadByte()
    } finally {
        $fs.Dispose()
    }

    # Windows PE executables start with "MZ" (0x4D 0x5A).
    if (-not ($b0 -eq 0x4D -and $b1 -eq 0x5A)) {
        $hint = ""
        if ($b0 -eq 0x7B) { $hint = " Got a JSON/API response instead (likely a rate-limit or error page)." }
        elseif ($b0 -eq 0x3C) { $hint = " Got an HTML page instead." }
        Write-Error "Downloaded file is not a Windows executable.$hint Refusing to install; your existing $BinName is left untouched."
        exit 1
    }

    Move-Item -Path $TmpFile.FullName -Destination $DestPath -Force
} finally {
    if (Test-Path $TmpFile.FullName) { Remove-Item -Path $TmpFile.FullName -Force }
}

Write-Host "$BinName installed successfully to $DestPath"
Write-Host "Make sure $InstallDir is in your PATH."
