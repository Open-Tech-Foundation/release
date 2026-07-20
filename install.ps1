$ErrorActionPreference = "Stop"

$Repo = "Open-Tech-Foundation/release"
$BinName = "otf-release"

$Arch = (Get-WmiObject -Class Win32_Processor).Architecture
if ($Arch -eq 9) {
    $ArchName = "x64"
    $PublicArchName = "x86-64"
    $LegacyArchName = "x86_64"
} elseif ($Arch -eq 12) {
    $ArchName = "arm64"
    $PublicArchName = "arm64"
    $LegacyArchName = "aarch64"
} else {
    Write-Error "Unsupported architecture. Only x86_64 and ARM64 are supported."
    exit 1
}

$BareAssetNames = @(
    "${BinName}-windows-${PublicArchName}.exe",
    "windows-${ArchName}.exe",
    "win32-${ArchName}.exe",
    "otf-release-windows-${ArchName}.exe",
    "otf-release-windows-${LegacyArchName}.exe",
    "otf-release-win32-${ArchName}.exe",
    "otf-release-win32-${LegacyArchName}.exe"
)

# Releases ship the binary inside a .zip (the archive name drops the .exe). Older
# releases attached the raw .exe, so try archives first and fall back to the bare
# names — that keeps this script working against both old and new releases.
$AssetNames = @()
foreach ($Bare in $BareAssetNames) {
    $AssetNames += ($Bare -replace '\.exe$', '') + ".zip"
}
$AssetNames += $BareAssetNames

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
    $Downloaded = $false
    foreach ($AssetName in $AssetNames) {
        $DownloadUrl = "https://github.com/$Repo/releases/latest/download/$AssetName"
        Write-Host "Downloading from $DownloadUrl..."
        try {
            Invoke-WebRequest -Uri $DownloadUrl -OutFile $TmpFile.FullName
            $Downloaded = $true
            break
        } catch {
            if (Test-Path $TmpFile.FullName) {
                Clear-Content -Path $TmpFile.FullName
            }
        }
    }
    if (-not $Downloaded) {
        Write-Error "Download failed for all known Windows/$ArchName asset names."
        exit 1
    }

    # Verify we actually got an executable, not an HTML/JSON error page. This is
    # the guard that prevents installing garbage over a good binary.
    if ((Get-Item $TmpFile.FullName).Length -eq 0) {
        Write-Error "Downloaded file is empty; refusing to install."
        exit 1
    }

    function Read-Magic($Path) {
        $fs = [System.IO.File]::OpenRead($Path)
        try { return @($fs.ReadByte(), $fs.ReadByte()) } finally { $fs.Dispose() }
    }

    $b0, $b1 = Read-Magic $TmpFile.FullName

    # A .zip asset ("PK") holds the .exe under its own name; unpack it and carry on
    # with the extracted file, so the executable check below applies to the real binary.
    $ExtractDir = $null
    if ($b0 -eq 0x50 -and $b1 -eq 0x4B) {
        $ExtractDir = Join-Path ([System.IO.Path]::GetTempPath()) ([System.IO.Path]::GetRandomFileName())
        New-Item -ItemType Directory -Force -Path $ExtractDir | Out-Null
        $ZipCopy = "$($TmpFile.FullName).zip"
        Copy-Item -Path $TmpFile.FullName -Destination $ZipCopy -Force
        try {
            Expand-Archive -Path $ZipCopy -DestinationPath $ExtractDir -Force
        } catch {
            Write-Error "Downloaded archive could not be extracted."
            exit 1
        } finally {
            Remove-Item -Path $ZipCopy -Force -ErrorAction SilentlyContinue
        }
        $Extracted = Get-ChildItem -Path $ExtractDir -Recurse -Filter "${BinName}.exe" | Select-Object -First 1
        if (-not $Extracted) {
            Write-Error "Archive did not contain a ${BinName}.exe binary."
            exit 1
        }
        Move-Item -Path $Extracted.FullName -Destination $TmpFile.FullName -Force
        $b0, $b1 = Read-Magic $TmpFile.FullName
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
    if ($ExtractDir -and (Test-Path $ExtractDir)) { Remove-Item -Path $ExtractDir -Recurse -Force }
}

Write-Host "$BinName installed successfully to $DestPath"
Write-Host "Make sure $InstallDir is in your PATH."
