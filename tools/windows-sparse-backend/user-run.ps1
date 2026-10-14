param([string]$Owned, [string]$Username)
# Runs as the disposable non-admin account inside the native Windows job wrapper.
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
if (-not $IsWindows -or $env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted' -or
    [IO.Path]::GetDirectoryName($Owned) -ne [IO.Path]::GetFullPath($env:RUNNER_TEMP) -or
    [IO.Path]::GetFileName($Owned) -notmatch '^javelin-sparse-proof-[a-f0-9]{32}$') { throw 'Refusing outside owned disposable Windows fixture.' }
$Evidence = Join-Path $Owned 'user-results'
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$admin = [Security.Principal.WindowsPrincipal]::new($identity).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
$summary = [ordered]@{ identity = @{ name = $identity.Name; sid = $identity.User.Value; administrator = $admin };
    phases = [ordered]@{}; errors = @(); providersStopped = $false; registeredRoots = @() }
$providers = @{}
$generation = 0
$sparse = Join-Path $Owned 'sparse.exe'
$projfs = Join-Path $Owned 'projfs-provider.exe'
$bun = Join-Path $Owned 'user-tools/bun.exe'
$checks = Join-Path $Owned 'user-tools/checks.mjs'
$probe = Join-Path $Owned 'user-tools/probe.mjs'
$frozen = Join-Path $Owned 'frozen'
$env:PATH = "C:\Program Files\Git\usr\bin;C:\mingw64\bin;$env:PATH"
$userTemp = Join-Path $Owned 'user-temp'
New-Item -ItemType Directory -Path $userTemp | Out-Null
$env:TEMP = $userTemp; $env:TMP = $userTemp; $env:TMPDIR = $userTemp

function Start-Provider([string]$Name, [string]$Kind, [string]$Base, [string]$Snapshot = '-') {
    $script:generation++
    $root = Join-Path $Owned $Name
    $stop = Join-Path $Owned "$Name.stop"; $ready = Join-Path $Owned "$Name.ready"
    Remove-Item -LiteralPath $stop, $ready -Force -ErrorAction SilentlyContinue
    if ($Kind -eq 'cfapi') { $file = $sparse; $argv = @('serve', $Base, $Snapshot, $root, $stop, $ready) }
    else { $file = $projfs; $argv = @('serve', $Base, $root, $stop, $ready) }
    $log = Join-Path $Evidence "provider-$Name-$generation"
    $process = Start-Process -FilePath $file -ArgumentList ($argv | ForEach-Object { '"' + $_ + '"' }) -PassThru -WorkingDirectory $Owned `
        -RedirectStandardOutput "$log.stdout.txt" -RedirectStandardError "$log.stderr.txt"
    $script:providers[$Name] = @{ process = $process; log = "$log.stdout.txt"; kind = $Kind }
    $deadline = [DateTime]::UtcNow.AddSeconds(15)
    while (-not (Test-Path -LiteralPath $ready) -and -not $process.HasExited -and [DateTime]::UtcNow -lt $deadline) { Start-Sleep -Milliseconds 50 }
    if (-not (Test-Path -LiteralPath $ready)) { throw "Provider $Name did not start; inspect $log." }
    if ($Kind -eq 'cfapi' -and $summary.registeredRoots -notcontains $root) { $summary.registeredRoots += $root }
}
function Stop-Provider([string]$Name, [bool]$Kill = $false) {
    $process = $providers[$Name].process
    if (-not $process -or $process.HasExited) { return }
    if ($Kill) { $process.Kill() } else { Set-Content -LiteralPath (Join-Path $Owned "$Name.stop") -Value 'stop' }
    if (-not $process.WaitForExit(8000)) { $process.Kill(); [void]$process.WaitForExit(3000); throw "Provider $Name failed bounded stop." }
    @{ name = $Name; pid = $process.Id; killed = $Kill; exitCode = $process.ExitCode; at = [DateTime]::UtcNow.ToString('o') } |
        ConvertTo-Json -Compress | Add-Content -LiteralPath (Join-Path $Evidence 'provider-lifecycle.jsonl')
}
function Wait-Log([string]$Name, [string]$Pattern, [int]$Seconds = 20) {
    $deadline = [DateTime]::UtcNow.AddSeconds($Seconds)
    while ([DateTime]::UtcNow -lt $deadline) {
        if (Select-String -LiteralPath $providers[$Name].log -Pattern $Pattern -Quiet) { return }
        Start-Sleep -Milliseconds 50
    }
    throw "Provider $Name log never matched $Pattern."
}
function Wait-File([string]$Path, [int]$Seconds = 20) {
    $deadline = [DateTime]::UtcNow.AddSeconds($Seconds)
    while (-not (Test-Path -LiteralPath $Path) -and [DateTime]::UtcNow -lt $deadline) { Start-Sleep -Milliseconds 50 }
    if (-not (Test-Path -LiteralPath $Path)) { throw "Timed out waiting for $Path." }
}
function Check([string]$Phase) {
    & $sparse bounded 60000 $bun $checks $Owned $Evidence $Phase
    $summary.phases[$Phase] = $LASTEXITCODE
    return $LASTEXITCODE -eq 0
}
function Audit([string]$Name, [string]$Label) {
    $text = & $sparse audit (Join-Path $Owned $Name)
    $text | Set-Content -LiteralPath (Join-Path $Evidence "state-$Label-$Name.json")
    $summary.phases["audit-$Label-$Name"] = $LASTEXITCODE
}
function Step([string]$Name, [scriptblock]$Body) {
    try { & $Body } catch { $summary.errors += "${Name}: $_"; $summary.phases["step-$Name"] = 'error' }
}
try {
    & (Join-Path $env:SystemRoot 'System32/whoami.exe') /all | Set-Content -LiteralPath (Join-Path $Evidence 'ordinary-user-identity.txt')
    if ($admin -or $identity.Name -ne "$env:COMPUTERNAME\$Username") { throw 'Actual non-admin identity required; refusing admin substitute.' }
    [void](Check 'job-termination')
    [void](Check 'protect-base')
    Step 'rename-projfs' {
        Start-Provider 'pr' 'projfs' (Join-Path $frozen 'base-rename')
        try { [void](Check 'rename-projfs') } finally { Stop-Provider 'pr' }
    }
    Step 'rename-cfapi' {
        Start-Provider 'cr' 'cfapi' (Join-Path $frozen 'base-rename')
        try { [void](Check 'rename-cfapi') } finally { Stop-Provider 'cr' }
    }
    $native = Join-Path $frozen 'base-native'
    Start-Provider 'a' 'cfapi' $native
    Start-Provider 'b' 'cfapi' $native
    [void](Check 'native-ops')
    Step 'kill-during-hydration' {
        Set-Content -LiteralPath (Join-Path $Owned 'a.slow') -Value 'slow'
        $reader = Start-Process -FilePath $sparse -ArgumentList @('bounded', '60000', $bun, $checks, $Owned, $Evidence, 'read-slow') -PassThru `
            -RedirectStandardOutput (Join-Path $Evidence 'read-slow.stdout.txt') -RedirectStandardError (Join-Path $Evidence 'read-slow.stderr.txt')
        Wait-Log 'a' 'fetch-slow-wait'
        Stop-Provider 'a' $true
        if (-not $reader.WaitForExit(65000)) { throw 'Reader did not finish after provider kill.' }
        $summary.readerExitAfterProviderKill = $reader.ExitCode
        Remove-Item -LiteralPath (Join-Path $Owned 'a.slow')
        Audit 'a' 'after-hydration-kill'
        Start-Provider 'a' 'cfapi' $native
    }
    Step 'kill-during-write' {
        [void](Check 'closed-write')
        $holders = @()
        foreach ($item in @(@('notes.txt', 'overwrite', 'PARTIAL-WRITE'), @('README.txt.saving', 'create', 'NEW CONTENT PARTIAL'))) {
            $ready = Join-Path $Owned "hold-$($item[0]).ready"
            $holders += Start-Process -FilePath $sparse -ArgumentList @('hold', ('"' + (Join-Path (Join-Path $Owned 'a') $item[0]) + '"'), $item[1], ('"' + $item[2] + '"'), ('"' + $ready + '"')) -PassThru
            Wait-File $ready
        }
        # Kill writers with handles open and the provider together; nothing was closed or flushed by the writers.
        foreach ($holder in $holders) { $holder.Kill() }
        Stop-Provider 'a' $true
        foreach ($holder in $holders) { [void]$holder.WaitForExit(5000) }
        Audit 'a' 'after-write-kill'
        Start-Provider 'a' 'cfapi' $native
        [void](Check 'after-kills')
    }
    Stop-Provider 'a'; Stop-Provider 'b'
    Audit 'a' 'before-capture'; Audit 'b' 'final'
    Step 'capture-and-clones' {
        $staging = Join-Path $Owned 'staging/snap'
        $capture = & $sparse capture $native (Join-Path $Owned 'a') $staging
        $summary.phases['capture'] = $LASTEXITCODE
        $capture | Set-Content -LiteralPath (Join-Path $Evidence 'capture.json')
        if ($LASTEXITCODE -ne 0) { throw 'Quiescent capture failed.' }
        Copy-Item -LiteralPath (Join-Path $staging 'manifest.tsv') -Destination (Join-Path $Evidence 'snapshot-manifest.tsv')
        Set-Content -LiteralPath (Join-Path $Owned 'freeze.request') -Value 'snap'
        Wait-File (Join-Path $Owned 'freeze.done') 60
        [void](Check 'protect-snapshot')
        Start-Provider 'a' 'cfapi' $native
        [void](Check 'tree-a')
        Stop-Provider 'a'
        $snapshot = Join-Path $frozen 'snap'
        Start-Provider 'd' 'cfapi' $native $snapshot
        Start-Provider 'e' 'cfapi' $native $snapshot
        [void](Check 'clones')
        Stop-Provider 'd'; Stop-Provider 'e'
        Audit 'd' 'final'; Audit 'e' 'final'; Audit 'a' 'final'
    }
    Step 'common-probe' {
        $common = Join-Path $frozen 'base-common'
        foreach ($name in @('pa', 'pb', 'pc')) { Start-Provider $name 'cfapi' $common }
        @{ base = $common; layerA = (Join-Path $Owned 'pa'); layerB = (Join-Path $Owned 'pb'); clone = (Join-Path $Owned 'pc') } |
            ConvertTo-Json | Set-Content -LiteralPath (Join-Path $Evidence 'common-paths.json')
        & $sparse bounded 90000 $bun $probe --backend paths --paths (Join-Path $Evidence 'common-paths.json') --out (Join-Path $Evidence 'common-probe.json')
        $summary.phases['common-probe'] = $LASTEXITCODE
        foreach ($name in @('pa', 'pb', 'pc')) { Stop-Provider $name; Audit $name 'final' }
    }
} catch {
    $summary.errors += $_.ToString()
} finally {
    foreach ($name in @($providers.Keys)) { try { Stop-Provider $name } catch { $summary.errors += "stop ${name}: $_" } }
    $summary.providersStopped = @($providers.Values | Where-Object { -not $_.process.HasExited }).Count -eq 0
    $summary.unregister = [ordered]@{}
    foreach ($root in $summary.registeredRoots) { $summary.unregister[$root] = (& $sparse unregister $root) }
    $summary | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $Evidence 'user-summary.json')
}
exit 0
