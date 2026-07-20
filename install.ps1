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
    $AssetUsed = ""
    foreach ($AssetName in $AssetNames) {
        $DownloadUrl = "https://github.com/$Repo/releases/latest/download/$AssetName"
        Write-Host "Downloading from $DownloadUrl..."
        try {
            Invoke-WebRequest -Uri $DownloadUrl -OutFile $TmpFile.FullName
            $Downloaded = $true
            $AssetUsed = $AssetName
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

    # --- Integrity: does this asset match the checksum published beside it? ---
    # Catches truncation and corruption. NOT an authenticity check — whoever could
    # replace the asset could replace checksums.txt too. See the provenance check below.
    $SumsFile = "$($TmpFile.FullName).sums"
    try {
        Invoke-WebRequest -Uri "https://github.com/$Repo/releases/latest/download/checksums.txt" -OutFile $SumsFile -ErrorAction Stop
        $Expected = $null
        foreach ($Line in Get-Content $SumsFile) {
            $Parts = $Line -split '\s+', 2
            if ($Parts.Count -eq 2 -and $Parts[1].Trim() -eq $AssetUsed) { $Expected = $Parts[0].Trim(); break }
        }
        if (-not $Expected) {
            Write-Host "Note: $AssetUsed not listed in checksums.txt; skipping checksum verification."
        } else {
            $Actual = (Get-FileHash -Path $TmpFile.FullName -Algorithm SHA256).Hash.ToLower()
            if ($Actual -ne $Expected.ToLower()) {
                Write-Error "Checksum mismatch for $AssetUsed.`n       expected $Expected`n       actual   $Actual`n       Refusing to install."
                exit 1
            }
            Write-Host "Checksum OK ($AssetUsed)."
        }
    } catch {
        # No checksums.txt on this release (older releases) — nothing to compare against.
    } finally {
        if (Test-Path $SumsFile) { Remove-Item -Path $SumsFile -Force }
    }

    # --- Authenticity: was this asset really built by this repo's workflow? ---
    # Three states, deliberately distinguished (see install.sh for the full rationale):
    #   no attestation published -> nothing to check; releases can predate provenance.
    #   published + verifies     -> good.
    #   published + fails        -> FATAL; something replaced a signed asset.
    # "Can't check" (no gh, or gh unauthenticated) is a warning, never a failure —
    # treating it as fatal deadlocks the very release that would publish the first
    # attestation. OTF_RELEASE_REQUIRE_ATTESTATION=1 makes every non-verified state fatal.
    $RequireAttestation = $env:OTF_RELEASE_REQUIRE_ATTESTATION -eq "1"
    function Resolve-AttestationGap($Message) {
        if ($RequireAttestation) {
            Write-Error "OTF_RELEASE_REQUIRE_ATTESTATION=1 and $Message"
            exit 1
        }
        Write-Host "Note: $Message"
    }

    $Digest = (Get-FileHash -Path $TmpFile.FullName -Algorithm SHA256).Hash.ToLower()
    $Attested = $false
    try {
        Invoke-WebRequest -Uri "https://api.github.com/repos/$Repo/attestations/sha256:$Digest" -UseBasicParsing -ErrorAction Stop | Out-Null
        $Attested = $true
    } catch { }

    $GhPresent = [bool](Get-Command gh -ErrorAction SilentlyContinue)
    $GhAuthed = $false
    if ($GhPresent) {
        & gh auth status 2>$null | Out-Null
        $GhAuthed = ($LASTEXITCODE -eq 0)
        # Clear it immediately. GitHub Actions appends `exit $LASTEXITCODE` to every pwsh step,
        # so a failed *probe* left lying around fails the whole step after a successful install.
        $global:LASTEXITCODE = 0
    }

    if (-not $Attested) {
        Resolve-AttestationGap "no build provenance is published for $AssetUsed; skipping verification."
    } elseif (-not $GhPresent) {
        Resolve-AttestationGap "provenance exists but the 'gh' CLI is not installed, so it cannot be verified."
    } elseif (-not $GhAuthed) {
        Resolve-AttestationGap "provenance exists but 'gh' is not authenticated (in GitHub Actions, set GH_TOKEN), so it cannot be verified."
    } else {
        & gh attestation verify $TmpFile.FullName --repo $Repo 2>$null | Out-Null
        $Verified = ($LASTEXITCODE -eq 0)
        $global:LASTEXITCODE = 0
        if ($Verified) {
            Write-Host "Provenance verified (built by $Repo)."
        } else {
            Write-Error "Build provenance FAILED verification for $AssetUsed. A signed attestation exists but does not match this download. Refusing to install. Run for details: gh attestation verify <file> --repo $Repo"
            exit 1
        }
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

# Reaching here means the install succeeded, so report success explicitly. GitHub Actions appends
# `exit $LASTEXITCODE` to every pwsh step, so any native command that ran along the way — a `gh`
# probe, say — would otherwise fail the step despite a working install. Setting the variable
# rather than calling `exit 0` keeps `irm | iex` from closing an interactive session.
$global:LASTEXITCODE = 0
