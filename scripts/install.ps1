# Install javelin and javelin-node on Windows x86_64 from the npm registry. EXPERIMENTAL:
# the Windows build and this script have never run on Windows.
#
#   irm https://javelin-codes.vercel.app/install.ps1 | iex
#   $env:JAVELIN_VERSION = '0.1.0'; irm https://javelin-codes.vercel.app/install.ps1 | iex
#
# It reads @dhruv2mars/javelin-win32-x64 from the registry, downloads its tarball, and
# checks it against the registry's `dist.integrity` (SHA-512) before it extracts or writes
# anything. Each binary is copied beside its destination and swapped into place. It changes
# nothing else: no PATH edits and no elevation. Windows PowerShell 5.1 or PowerShell 7, and
# the tar.exe that ships with Windows 10 and later.

param(
    [string]$Version = $(if ($env:JAVELIN_VERSION) { $env:JAVELIN_VERSION } else { 'latest' }),
    [string]$InstallDir = $(if ($env:JAVELIN_INSTALL_DIR) { $env:JAVELIN_INSTALL_DIR } else { Join-Path $HOME '.javelin\bin' }),
    [string]$Registry = $(if ($env:JAVELIN_NPM_REGISTRY) { $env:JAVELIN_NPM_REGISTRY } else { 'https://registry.npmjs.org' })
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

function Fail([string]$Message) { throw "install.ps1: $Message" }

if (([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture) -ne 'X64') {
    Fail 'no Javelin build for this architecture (Windows x86_64 only)'
}
$Registry = $Registry.TrimEnd('/')
if (-not ($Registry -match '^https://' -or $Registry -match '^http://(127\.0\.0\.1|localhost)([:/]|$)')) {
    Fail "refusing $Registry; use an https registry"
}
if ($Version -notmatch '^[0-9A-Za-z.+-]+$') { Fail "not a version or tag: $Version" }
$package = '@dhruv2mars/javelin-win32-x64'

$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ('javelin-install-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    try { $manifest = Invoke-RestMethod -UseBasicParsing -Uri "$Registry/$package/$Version" }
    catch { Fail "could not read $package@$Version from $Registry" }
    $resolved = [string]$manifest.version
    if ($resolved -notmatch '^\d+\.\d+\.\d+(-[0-9A-Za-z.]+)?$') { Fail "not a release version: $resolved" }
    $integrity = [string]$manifest.dist.integrity
    if (-not $integrity.StartsWith('sha512-')) { Fail "$package@$resolved has no SHA-512 integrity" }

    $name = "javelin-win32-x64-$resolved.tgz"
    $archive = Join-Path $tmp $name
    try { Invoke-WebRequest -UseBasicParsing -Uri "$Registry/$package/-/$name" -OutFile $archive }
    catch { Fail "could not download $name" }
    $expected = ([System.BitConverter]::ToString([Convert]::FromBase64String($integrity.Substring(7)))).Replace('-', '').ToLower()
    $actual = (Get-FileHash -Algorithm SHA512 -LiteralPath $archive).Hash.ToLower()
    if ($expected -ne $actual) { Fail "integrity mismatch for $package@$resolved (registry $integrity); nothing was installed" }

    $extract = Join-Path $tmp 'extract'
    New-Item -ItemType Directory -Path $extract | Out-Null
    & tar.exe -xzf $archive -C $extract
    if ($LASTEXITCODE -ne 0) { Fail "could not extract $name" }
    if ((Get-Content -Raw -LiteralPath (Join-Path $extract 'package\package.json') | ConvertFrom-Json).name -ne $package) {
        Fail "the tarball is not $package"
    }

    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    $InstallDir = (Resolve-Path -LiteralPath $InstallDir).Path
    foreach ($bin in @('javelin.exe', 'javelin-node.exe')) {
        $source = Join-Path $extract "package\bin\$bin"
        if (-not (Test-Path -LiteralPath $source -PathType Leaf)) { Fail "$package@$resolved has no bin\$bin" }
        $dest = Join-Path $InstallDir $bin
        $staged = Join-Path $InstallDir ($bin + '.' + [guid]::NewGuid().ToString('N') + '.new')
        Copy-Item -LiteralPath $source -Destination $staged
        # Move-Item -Force deletes the old file before moving; File.Replace swaps it in one step.
        if (Test-Path -LiteralPath $dest -PathType Leaf) {
            [System.IO.File]::Replace($staged, $dest, [NullString]::Value)
        } else {
            Move-Item -LiteralPath $staged -Destination $dest
        }
    }

    Write-Output "Installed javelin and javelin-node $resolved ($package) to $InstallDir"
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
