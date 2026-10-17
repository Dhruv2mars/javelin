param([string]$Owned, [string]$Evidence, [string]$Provider, [string]$Username)
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
if (-not $IsWindows -or $env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted' -or
    [IO.Path]::GetDirectoryName($Owned) -ne [IO.Path]::GetFullPath($env:RUNNER_TEMP) -or
    [IO.Path]::GetFileName($Owned) -notmatch '^javelin-windows-proof-[a-f0-9]{32}$' -or
    $Evidence -ne (Join-Path $Owned 'user-results')) { throw 'Refusing outside owned disposable Windows fixture.' }
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
$admin = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
$summary = [ordered]@{ verdict = 'INCONCLUSIVE'; identity = @{ name = $identity.Name; sid = $identity.User.Value; administrator = $admin };
    providerStarted = $false; rootsCreated = 0; phases = @(); error = $null; stoppedProviders = $false }
$providers = @{}
$generation = 0
$base = Join-Path $Owned 'base'
$bun = Join-Path $Owned 'user-tools/bun.exe'
$probe = Join-Path $Owned 'user-tools/probe.mjs'
$checks = Join-Path $Owned 'user-tools/checks.mjs'
$env:PATH = "C:\Program Files\Git\usr\bin;C:\mingw64\bin;$env:PATH"
$userTemp = Join-Path $Owned 'user-temp'
New-Item -ItemType Directory -Path $userTemp | Out-Null
$env:TEMP = $userTemp
$env:TMP = $userTemp
$env:TMPDIR = $userTemp
$summary.tempDirectory = $userTemp

function Start-Provider([string]$Name, [string]$Snapshot = '') {
    $script:generation++
    $root = Join-Path $Owned $Name
    $stop = Join-Path $Owned "$Name.stop"
    $ready = Join-Path $Owned "$Name.ready"
    Remove-Item -LiteralPath $stop, $ready -Force -ErrorAction SilentlyContinue
    $providerArguments = @('serve', $base, $root, $stop, $ready)
    if ($Snapshot) { $providerArguments += $Snapshot }
    $process = Start-Process -FilePath $Provider -ArgumentList ($providerArguments | ForEach-Object { '"' + $_ + '"' }) -PassThru -WorkingDirectory $Owned -RedirectStandardOutput (Join-Path $Evidence "provider-$Name-$generation.stdout.txt") -RedirectStandardError (Join-Path $Evidence "provider-$Name-$generation.stderr.txt")
    $script:providers[$Name] = $process
    $deadline = [DateTime]::UtcNow.AddSeconds(10)
    while (-not (Test-Path -LiteralPath $ready) -and -not $process.HasExited -and [DateTime]::UtcNow -lt $deadline) { Start-Sleep -Milliseconds 50 }
    if (-not (Test-Path -LiteralPath $ready)) { throw "Provider $Name did not start; inspect its raw log." }
    $summary.providerStarted = $true
    $summary.rootsCreated = @($providers.Keys).Count
    return (Get-Content -Raw -LiteralPath $ready)
}
function Stop-Provider([string]$Name, [bool]$Kill = $false) {
    $process = $providers[$Name]
    if (-not $process -or $process.HasExited) { return }
    if ($Kill) { $process.Kill() } else { Set-Content -LiteralPath (Join-Path $Owned "$Name.stop") -Value 'stop' }
    if (-not $process.WaitForExit(5000)) { $process.Kill(); [void]$process.WaitForExit(2000); throw "Provider $Name failed bounded stop." }
    @{ name = $Name; pid = $process.Id; killed = $Kill; exited = $process.HasExited; exitCode = $process.ExitCode } |
        ConvertTo-Json | Add-Content -LiteralPath (Join-Path $Evidence 'provider-lifecycle.jsonl')
}
function Audit([string]$Name, [string]$Phase) {
    $text = & $Provider audit (Join-Path $Owned $Name) $base
    if ($LASTEXITCODE -ne 0) { throw "Audit $Name failed." }
    $text | Set-Content -LiteralPath (Join-Path $Evidence "state-$Phase-$Name.json")
}
function Check([string]$Phase) {
    & $Provider bounded 30000 $bun $checks $Owned $Evidence $Phase
    $code = $LASTEXITCODE
    $summary.phases += @{ id = $Phase; exitCode = $code }
    if ($code -ne 0) { throw "Native Windows check $Phase failed; inspect raw phase result." }
}
try {
    & (Join-Path $env:SystemRoot 'System32/whoami.exe') /all | Set-Content -LiteralPath (Join-Path $Evidence 'ordinary-user-identity.txt')
    if ($admin -or $identity.Name -ne "$env:COMPUTERNAME\$Username") { throw 'Actual non-admin identity required; refusing admin substitute.' }
    Check 'job-termination'
    Check 'base'
    $ids = @{}
    foreach ($name in @('a', 'b', 'c')) { $ids[$name] = Start-Provider $name; Stop-Provider $name; Audit $name 'initial' }
    $ids | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $Evidence 'initial-instances.json')
    [void](Start-Provider 'a')
    Check 'metadata'
    Stop-Provider 'a'; Audit 'a' 'placeholder'
    [void](Start-Provider 'a')
    Check 'hydrate'
    Stop-Provider 'a'; Audit 'a' 'hydrated'
    foreach ($name in @('a', 'b', 'c')) { [void](Start-Provider $name) }
    Check 'build'
    Stop-Provider 'a'; Audit 'a' 'after-build'
    [void](Start-Provider 'a'); Check 'build-clean'
    @{ base = $base; layerA = (Join-Path $Owned 'a'); layerB = (Join-Path $Owned 'b'); clone = (Join-Path $Owned 'c') } |
        ConvertTo-Json | Set-Content -LiteralPath (Join-Path $Evidence 'paths.json')
    & $Provider bounded 70000 $bun $probe --backend paths --paths (Join-Path $Evidence 'paths.json') --out (Join-Path $Evidence 'common-probe.json')
    $summary.commonProbeExit = $LASTEXITCODE
    try { Check 'directory-rename' } catch { $summary.directoryRenameError = $_.ToString() }
    Check 'mutate'
    foreach ($name in @('a', 'b', 'c')) { Stop-Provider $name; Audit $name 'after-probe' }
    Audit 'a' 'after-mutate'
    [void](Start-Provider 'a')
    [void](Start-Provider 'b')
    Stop-Provider 'a'
    try {
        Check 'clone-snapshot'
        [void](Start-Provider 'd' (Join-Path $Owned 'snapshot'))
        [void](Start-Provider 'a')
        Check 'clone-verify'
        Stop-Provider 'd'; Audit 'd' 'clone'
    } catch { $summary.cloneError = $_.ToString() }
    if ($providers['a'].HasExited) { [void](Start-Provider 'a') }
    Check 'recovery-before'
    Stop-Provider 'a' $true
    Audit 'a' 'killed-before'
    $reopened = Start-Provider 'a'
    if ($reopened -ne $ids.a) { throw 'Reopen changed persisted ProjFS instance identity.' }
    Check 'recovery-reopen-before'
    Check 'recovery-after'
    Stop-Provider 'a' $true
    [void](Start-Provider 'a')
    Check 'recovery-reopen-after'
    foreach ($name in @('a', 'b')) { Stop-Provider $name; Audit $name 'final' }
    Check 'final'
    if (@($summary.phases | Where-Object { $_.exitCode -ne 0 }).Count -eq 0) {
        $summary.verdict = 'VERIFIED' # Bounded native phases only; common/backend verdict stays separate.
    }
} catch {
    $summary.error = $_.ToString()
    $_ | Format-List * -Force
} finally {
    foreach ($name in @($providers.Keys)) {
        try { Stop-Provider $name } catch { $summary.error = "Provider cleanup failed: $_" }
    }
    $summary.stoppedProviders = @($providers.Values | Where-Object { -not $_.HasExited }).Count -eq 0
    $summary | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath (Join-Path $Evidence 'runtime-summary.json')
}
if ($summary.verdict -ne 'VERIFIED' -or -not $summary.stoppedProviders) { exit 3 }
