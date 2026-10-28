# Install javelin and javelin-node on Windows x86_64.
#
#   irm https://javelin-codes.vercel.app/install.ps1 | iex
#   $env:JAVELIN_VERSION = 'v0.1.0'; irm https://javelin-codes.vercel.app/install.ps1 | iex
#
# The installer reads the release host's `latest` file (or uses -Version), downloads that
# version's SHA256SUMS and the archive for Windows, and checks the archive's SHA-256 before
# it extracts or writes anything. Each binary is copied beside its destination and swapped
# into place. It changes nothing else: no PATH edits and no elevation. Windows PowerShell
# 5.1 or PowerShell 7.
#
# Release layout (docs/releases.md): BASE/latest holds a version such as v0.1.0, and
# BASE/VERSION/ holds SHA256SUMS, manifest.json and javelin-VERSION-TARGET.zip.

param(
    [string]$Version = $(if ($env:JAVELIN_VERSION) { $env:JAVELIN_VERSION } else { 'latest' }),
    [string]$InstallDir = $(if ($env:JAVELIN_INSTALL_DIR) { $env:JAVELIN_INSTALL_DIR } else { Join-Path $HOME '.javelin\bin' }),
    [string]$ReleaseBase = $env:JAVELIN_RELEASE_BASE,
    [string]$Archive,
    [string]$Sums
)

# The release host. tools/release/release.ts stamps it when it stages a release;
# JAVELIN_RELEASE_BASE or -ReleaseBase overrides it.
$DefaultReleaseBase = ''

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

function Fail([string]$Message) { throw "install.ps1: $Message" }

if (-not $ReleaseBase) { $ReleaseBase = $DefaultReleaseBase }
if (([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture) -ne 'X64') {
    Fail 'no Javelin build for this architecture (Windows x86_64 only)'
}
$target = 'x86_64-pc-windows-gnu'

if (($Archive -and -not $Sums) -or ($Sums -and -not $Archive)) {
    Fail '-Archive and -Sums must be given together'
}

$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ('javelin-install-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    # Copy BASE/$Path to $tmp/$Name. Plain http is accepted only for a loopback host.
    function Get-ReleaseFile([string]$Path, [string]$Name) {
        $source = $ReleaseBase.TrimEnd('/') + '/' + $Path
        $dest = Join-Path $tmp $Name
        if ($ReleaseBase -match '^https://' -or $ReleaseBase -match '^http://(127\.0\.0\.1|localhost)([:/]|$)') {
            try { Invoke-WebRequest -UseBasicParsing -Uri $source -OutFile $dest }
            catch { Fail "could not download $source" }
        } elseif ($ReleaseBase -match '^http://') {
            Fail "refusing plain http for $ReleaseBase; use https"
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
        if (-not $ReleaseBase) { Fail 'no release host is set yet; pass -ReleaseBase or set JAVELIN_RELEASE_BASE' }
        if ($Version -eq 'latest') {
            $Version = (Get-Content -LiteralPath (Get-ReleaseFile 'latest' 'latest') -Raw).Trim()
        }
        # A version names a directory on the host: v, then letters, digits, dots, dashes, pluses.
        if ($Version -notmatch '^v[0-9A-Za-z.+-]+$') { Fail "not a release version: $Version" }
        $name = "javelin-$Version-$target.zip"
        $sumsPath = Get-ReleaseFile "$Version/SHA256SUMS" 'SHA256SUMS'
        $archivePath = Get-ReleaseFile "$Version/$name" $name
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
    $binaries = @{}
    foreach ($bin in @('javelin.exe', 'javelin-node.exe')) {
        $found = Get-ChildItem -LiteralPath $extract -Recurse -File -Filter $bin |
            Where-Object { $_.Directory.Name -eq 'bin' } | Select-Object -First 1
        if (-not $found) { Fail "archive $name has no bin\$bin" }
        $binaries[$bin] = $found.FullName
    }

    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    $InstallDir = (Resolve-Path -LiteralPath $InstallDir).Path
    foreach ($bin in @('javelin.exe', 'javelin-node.exe')) {
        $dest = Join-Path $InstallDir $bin
        $staged = Join-Path $InstallDir ($bin + '.' + [guid]::NewGuid().ToString('N') + '.new')
        Copy-Item -LiteralPath $binaries[$bin] -Destination $staged
        # Move-Item -Force deletes the old file before moving; File.Replace swaps it in one step.
        if (Test-Path -LiteralPath $dest -PathType Leaf) {
            [System.IO.File]::Replace($staged, $dest, [NullString]::Value)
        } else {
            Move-Item -LiteralPath $staged -Destination $dest
        }
    }

    Write-Output "Installed javelin and javelin-node ($name) to $InstallDir"
    if (($env:Path -split ';') -contains $InstallDir) {
        Write-Output 'Run: javelin --version'
    } else {
        Write-Output ''
        Write-Output "$InstallDir is not on your PATH. To add it for your user:"
        Write-Output "  [Environment]::SetEnvironmentVariable('Path', [Environment]::GetEnvironmentVariable('Path', 'User') + ';$InstallDir', 'User')"
        Write-Output 'then open a new terminal and run: javelin --version'
    }
} finally {
    Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
}
