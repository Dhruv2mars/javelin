# Called after feature/API/compiler gates. Changes only owned paths and one disposable account.
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted' -or -not $owned -or -not $token) {
    throw 'Refusing backend setup outside owned disposable CI.'
}
$username = "jvp$($token.Substring(0, 12))"
$credential = $null
$accountCreated = $false
$result.runtime = [ordered]@{ verdict = 'INCONCLUSIVE'; error = $null; account = $username; accountRemoved = $false }
try {
    $provider = Join-Path $owned 'provider.exe'
    $batch = Join-Path $owned 'build-provider.cmd'
    $source = Join-Path $PSScriptRoot 'provider.cpp'
    @('@echo off', "call `"$dev`" -no_logo -arch=x64", 'if errorlevel 1 exit /b 2',
        "cl /nologo /Bv /std:c++20 /W4 /WX /EHsc /D_WIN32_WINNT=0x0A00 /DNTDDI_VERSION=0x0A000008 `"$source`" /Fe:`"$provider`" /Fo:`"$(Join-Path $owned 'provider.obj')`" ProjectedFSLib.lib Ole32.lib") |
        Set-Content -LiteralPath $batch -Encoding ascii
    $compileProvider = Invoke-Bounded 'compiler-provider' (Join-Path $env:SystemRoot 'System32/cmd.exe') @('/d', '/c', $batch) 60000
    if ($compileProvider.exitCode -ne 0) { throw 'Native provider compile failed; this is not an unavailable platform verdict.' }
    $userTools = Join-Path $owned 'user-tools'
    $userEvidence = Join-Path $owned 'user-results'
    New-Item -ItemType Directory -Path $userTools, $userEvidence | Out-Null
    $bun = (Get-Command bun.exe -CommandType Application).Source
    Copy-Item -LiteralPath $bun -Destination (Join-Path $userTools 'bun.exe')
    foreach ($name in @('probe.mjs', 'atomic-writer.mjs')) {
        Copy-Item -LiteralPath (Join-Path $PSScriptRoot "../workspace-probe/$name") -Destination (Join-Path $userTools $name)
    }
    foreach ($name in @('user-run.ps1', 'checks.mjs')) {
        Copy-Item -LiteralPath (Join-Path $PSScriptRoot $name) -Destination (Join-Path $userTools $name)
    }
    $base = Join-Path $owned 'base'
    New-Item -ItemType Directory -Path $base | Out-Null
    $seed = Invoke-Bounded 'fixture-seed' (Join-Path $userTools 'bun.exe') @((Join-Path $userTools 'probe.mjs'), '--seed', $base)
    if ($seed.exitCode -ne 0) { throw 'Unchanged common fixture could not be seeded.' }
    $password = ConvertTo-SecureString ("Aa1!" + [guid]::NewGuid().ToString('N') + [guid]::NewGuid().ToString('N')) -AsPlainText -Force
    New-LocalUser -Name $username -Password $password -Description 'Disposable Javelin CI proof user' | Select-Object Name, SID
    $accountCreated = $true
    Add-LocalGroupMember -SID 'S-1-5-32-545' -Member $username
    $credential = [PSCredential]::new("$env:COMPUTERNAME\$username", $password)
    $permissions = Invoke-Bounded 'owned-user-acl' (Join-Path $env:SystemRoot 'System32/icacls.exe') @($owned, '/grant', "${username}:(OI)(CI)F")
    if ($permissions.exitCode -ne 0) { throw 'Could not grant proof user access to owned paths.' }
    $baseAcl = Invoke-Bounded 'base-readonly-acl' (Join-Path $env:SystemRoot 'System32/icacls.exe') @($base,
        '/inheritance:r', '/grant:r', "${username}:(OI)(CI)RX", '*S-1-5-32-544:(OI)(CI)F', '*S-1-5-18:(OI)(CI)F', '/T', '/L')
    if ($baseAcl.exitCode -ne 0) { throw 'Could not make backing dataset read-only to actual proof user.' }
    $pwsh = (Get-Process -Id $PID).Path
    # Start the ordinary-user script inside the native kill-on-close job wrapper.
    $argv = @('bounded', '120000', $pwsh, '-NoProfile', '-File', (Join-Path $userTools 'user-run.ps1'),
        '-Owned', $owned, '-Evidence', $userEvidence, '-Provider', $provider, '-Username', $username)
    $stdoutPath = Join-Path $evidence 'ordinary-user.stdout.txt'
    $stderrPath = Join-Path $evidence 'ordinary-user.stderr.txt'
    $child = Start-Process -FilePath $provider -ArgumentList ($argv | ForEach-Object { '"' + $_ + '"' }) -Credential $credential -WorkingDirectory $owned -RedirectStandardOutput $stdoutPath -RedirectStandardError $stderrPath -PassThru
    $result.runtime.wrapperPid = $child.Id
    if (-not $child.WaitForExit(135000)) { $child.Kill($true); [void]$child.WaitForExit(5000); throw 'Ordinary-user wrapper exceeded its independent Windows job budget.' }
    $result.runtime.exitCode = $child.ExitCode
    Get-Content -LiteralPath $stdoutPath
    Get-Content -LiteralPath $stderrPath
    Copy-Item -Path (Join-Path $userEvidence '*') -Destination $evidence
    $runtimeSummary = Join-Path $evidence 'runtime-summary.json'
    if (Test-Path -LiteralPath $runtimeSummary) { $result.runtime.results = Get-Content -Raw -LiteralPath $runtimeSummary | ConvertFrom-Json }
    $result.providerStarted = [bool]$result.runtime.results.providerStarted
    $result.rootsCreated = $result.runtime.results.rootsCreated
    $result.runtime.verdict = $result.runtime.results.verdict
} catch {
    $result.runtime.error = $_.ToString()
    $_ | Format-List * -Force
} finally {
    if (-not $accountCreated) { $result.runtime.accountRemoved = $true }
    if ($accountCreated) {
        try { Remove-LocalUser -Name $username; $result.runtime.accountRemoved = -not (Get-LocalUser -Name $username -ErrorAction SilentlyContinue) }
        catch { $result.runtime.error = "Account cleanup failed: $_" }
    }
    $result.runtime | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath (Join-Path $evidence 'backend.json')
}
