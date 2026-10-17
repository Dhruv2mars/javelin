# Runs only as a bounded child of run.ps1 on a disposable hosted runner.
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted' -or
    $env:RUNNER_OS -ne 'Windows' -or -not $env:WINDOWS_PROOF_EVIDENCE) {
    throw 'Refusing feature inspection/setup outside disposable hosted Windows proof.'
}
$result = [ordered]@{ command = 'Get-WindowsOptionalFeature -Online -FeatureName Client-ProjFS';
    before = $null; enable = $null; after = $null; ready = $false; error = $null }
try {
    $feature = Get-WindowsOptionalFeature -Online -FeatureName Client-ProjFS
    $feature | Format-List *
    $result.before = [string]$feature.State
    if ($feature.State -eq 'Disabled') {
        $result.enableCommand = 'Enable-WindowsOptionalFeature -Online -FeatureName Client-ProjFS -NoRestart'
        $enabled = Enable-WindowsOptionalFeature -Online -FeatureName Client-ProjFS -NoRestart
        $enabled | Format-List *
        $result.enable = @{ RestartNeeded = [bool]$enabled.RestartNeeded; Online = $enabled.Online }
        if ($enabled.RestartNeeded) { throw 'RestartNeeded=True; reboot is not authorized.' }
    } elseif ($feature.State -ne 'Enabled') {
        throw "Unsupported feature state $($feature.State); no reboot or workaround attempted."
    }
    $after = Get-WindowsOptionalFeature -Online -FeatureName Client-ProjFS
    $after | Format-List *
    $result.after = [string]$after.State
    $result.ready = $after.State -eq 'Enabled'
} catch {
    $result.error = $_.ToString()
    $_ | Format-List * -Force
} finally {
    $result | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath (Join-Path $env:WINDOWS_PROOF_EVIDENCE 'feature.json')
}
if (-not $result.ready) { exit 3 }
