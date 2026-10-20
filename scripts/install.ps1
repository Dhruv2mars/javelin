# Install the Javelin CLI from a release archive on Windows x86_64.
#
# The archive's SHA-256 is checked against SHA256SUMS before anything is extracted or
# written. The binary is copied beside its destination and renamed into place, so
# re-running with a newer release upgrades in place. Nothing else is changed: no PATH
# edits and no privileges. Run it with Windows PowerShell 5.1 or PowerShell 7.
#
#   powershell -File install.ps1                      latest release
#   powershell -File install.ps1 -Version v0.1.0      pinned release
#   powershell -File install.ps1 -Archive A -Sums S   a local archive and its SHA256SUMS
[CmdletBinding()]
param(
    [string]$Version = 'latest',
    [string]$BinDir = (Join-Path $env:LOCALAPPDATA 'Programs\Javelin\bin'),
    [string]$ReleaseBase = 'https://github.com/Dhruv2mars/javelin/releases',
    [string]$Archive,
    [string]$Sums
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

function Fail([string]$Message) { throw "install.ps1: $Message" }

if (([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture) -ne 'X64') {
    Fail 'unsupported architecture; see docs/install.md'
}
$triple = 'x86_64-pc-windows-msvc'

if (($Archive -and -not $Sums) -or ($Sums -and -not $Archive)) {
    Fail '-Archive and -Sums must be given together'
}

$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ('javelin-install-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    # Copy or download one release file into $tmp. $ReleaseBase is a URL or a local directory
    # laid out like GitHub's: BASE/download/TAG/FILE and BASE/latest/download/FILE.
    function Get-ReleaseFile([string]$Name) {
        if ($Version -eq 'latest') { $source = "$ReleaseBase/latest/download/$Name" }
        else { $source = "$ReleaseBase/download/$Version/$Name" }
        $dest = Join-Path $tmp $Name
        if ($ReleaseBase -match '^https?://') {
            try { Invoke-WebRequest -UseBasicParsing -Uri $source -OutFile $dest }
            catch { Fail "could not download $source" }
        } else {
            if (-not (Test-Path -LiteralPath $source -PathType Leaf)) { Fail "could not read $source" }
            Copy-Item -LiteralPath $source -Destination $dest
        }
        return $dest
    }

    if ($Archive) {
        foreach ($path in @($Archive, $Sums)) {
            if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { Fail "no such file: $path" }
        }
        $name = Split-Path -Leaf $Archive
        $archivePath = Join-Path $tmp $name
        $sumsPath = Join-Path $tmp 'SHA256SUMS'
        Copy-Item -LiteralPath $Archive -Destination $archivePath
        Copy-Item -LiteralPath $Sums -Destination $sumsPath
    } else {
        $sumsPath = Get-ReleaseFile 'SHA256SUMS'
        # Exactly one archive per platform is listed in a release's SHA256SUMS.
        $names = @(Get-Content -LiteralPath $sumsPath | ForEach-Object {
            if ($_ -match '^[0-9a-fA-F]{64}\s+\*?(javelin-.+-' + [regex]::Escape($triple) + '\.zip)$') { $Matches[1] }
        })
        if ($names.Count -ne 1) { Fail "release has no single archive for $triple" }
        $name = $names[0]
        $archivePath = Get-ReleaseFile $name
    }

    $expected = @(Get-Content -LiteralPath $sumsPath | ForEach-Object {
        if ($_ -match '^([0-9a-fA-F]{64})\s+\*?(.+)$' -and $Matches[2] -eq $name) { $Matches[1].ToLower() }
    })
    if ($expected.Count -ne 1) { Fail "SHA256SUMS has no single entry for $name" }
    $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $archivePath).Hash.ToLower()
    if ($expected[0] -ne $actual) {
        Fail "checksum mismatch for $name (expected $($expected[0]), got $actual); nothing was installed"
    }

    $extract = Join-Path $tmp 'extract'
    Expand-Archive -LiteralPath $archivePath -DestinationPath $extract
    $binary = Get-ChildItem -LiteralPath $extract -Recurse -File -Filter 'javelin.exe' |
        Where-Object { $_.Directory.Name -eq 'bin' } | Select-Object -First 1
    if (-not $binary) { Fail "archive $name has no bin\javelin.exe" }

    New-Item -ItemType Directory -Path $BinDir -Force | Out-Null
    $BinDir = (Resolve-Path -LiteralPath $BinDir).Path
    $dest = Join-Path $BinDir 'javelin.exe'
    $staged = Join-Path $BinDir ('javelin.exe.' + [guid]::NewGuid().ToString('N') + '.new')
    Copy-Item -LiteralPath $binary.FullName -Destination $staged
    # Move-Item -Force deletes the old file before moving; File.Replace swaps it in one step.
    if (Test-Path -LiteralPath $dest -PathType Leaf) {
        [System.IO.File]::Replace($staged, $dest, [NullString]::Value)
    } else {
        Move-Item -LiteralPath $staged -Destination $dest
    }

    Write-Output "Installed javelin ($name) to $(Join-Path $BinDir 'javelin.exe')"
    if (-not (($env:Path -split ';') -contains $BinDir)) {
        Write-Output "Add $BinDir to your PATH to run javelin."
    }
} finally {
    Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
}
