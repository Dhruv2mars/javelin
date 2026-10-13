# Bounded prerequisite gate and native Windows fixture. Hosted runner only.
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted' -or
    $env:RUNNER_OS -ne 'Windows' -or -not $IsWindows) {
    throw 'Refusing outside disposable GitHub-hosted Windows. No host changes made.'
}
$token = [guid]::NewGuid().ToString('N')
$tempParent = [IO.Path]::GetFullPath($env:RUNNER_TEMP)
$owned = Join-Path $tempParent "javelin-windows-proof-$token"
$evidence = Join-Path $tempParent "javelin-windows-evidence-$token"
$transcriptStarted = $false
$commands = [Collections.Generic.List[object]]::new()
$result = [ordered]@{ schema = 1; sourceSHA = $env:SOURCE_SHA; runId = $env:GITHUB_RUN_ID;
    runAttempt = $env:GITHUB_RUN_ATTEMPT; backend = 'INCONCLUSIVE'; available = $false;
    blockers = @(); sources = [ordered]@{}; rootsCreated = 0; providerStarted = $false }

function Invoke-Bounded([string]$Name, [string]$File, [string[]]$Arguments, [int]$Milliseconds = 30000) {
    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = $File
    $info.UseShellExecute = $false
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    foreach ($argument in $Arguments) { $info.ArgumentList.Add($argument) }
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $info
    $entry = [ordered]@{ name = $Name; executable = $File; argv = $Arguments;
        timeoutMs = $Milliseconds; pid = $null; exitCode = $null; timedOut = $false; error = $null;
        exitConfirmed = $false; treeTermination = 'not needed' }
    Write-Host "COMMAND $Name $File $($Arguments -join ' ')"
    try {
        [void]$process.Start()
        $entry.pid = $process.Id
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit($Milliseconds)) {
            $entry.timedOut = $true
            $process.Kill($true) # Windows/.NET child-tree termination, not POSIX groups.
            $entry.treeTermination = 'Process.Kill(entireProcessTree=true); descendant exit not independently proven'
            if (-not $process.WaitForExit(5000)) { throw "Child $Name survived tree termination." }
        }
        $entry.exitCode = $process.ExitCode
        $entry.exitConfirmed = $true
        $entry.stdout = $stdout.GetAwaiter().GetResult()
        $entry.stderr = $stderr.GetAwaiter().GetResult()
        Write-Host $entry.stdout
        Write-Host $entry.stderr
    } catch {
        $entry.error = $_.ToString()
        Write-Host $entry.error
        if ($entry.pid -and -not $process.HasExited) {
            $process.Kill($true)
            $entry.treeTermination = 'Process.Kill(entireProcessTree=true); descendant exit not independently proven'
            $entry.exitConfirmed = $process.WaitForExit(5000)
        }
    } finally {
        $commands.Add($entry)
        $entry | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath (Join-Path $evidence "$Name.json")
        $process.Dispose()
    }
    return $entry
}

try {
    New-Item -ItemType Directory -Path $evidence | Out-Null
    $env:WINDOWS_PROOF_EVIDENCE = $evidence
    Add-Content -LiteralPath $env:GITHUB_ENV -Value "WINDOWS_PROOF_EVIDENCE=$evidence"
    New-Item -ItemType Directory -Path $owned | Out-Null
    Set-Content -LiteralPath (Join-Path $owned 'owner.txt') -Value $token
    Start-Transcript -LiteralPath (Join-Path $evidence 'transcript.txt') | Out-Null
    $transcriptStarted = $true
    if ($env:SOURCE_SHA -notmatch '^[0-9a-f]{40}$') { throw 'Exact CI checkout source SHA required.' }
    $os = Get-CimInstance Win32_OperatingSystem
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = [Security.Principal.WindowsPrincipal]::new($identity)
    $admin = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
    $result.host = @{ caption = $os.Caption; version = $os.Version; build = $os.BuildNumber;
        architecture = $os.OSArchitecture; imageOS = $env:ImageOS; imageVersion = $env:ImageVersion;
        runnerEnvironment = $env:RUNNER_ENVIRONMENT; powershell = $PSVersionTable.PSVersion.ToString() }
    $result.identity = @{ name = $identity.Name; sid = $identity.User.Value;
        administrator = $admin; ordinaryUserVerified = $false }
    $result.host | ConvertTo-Json
    $result.identity | ConvertTo-Json
    $sourcePaths = @('tools/windows-workspace-proof/run.ps1', 'tools/windows-workspace-proof/feature.ps1',
        'tools/windows-workspace-proof/api-probe.cpp', '.github/workflows/windows-workspace-proof.yml',
        'tools/windows-workspace-proof/provider.cpp', 'tools/windows-workspace-proof/backend.ps1',
        'tools/windows-workspace-proof/user-run.ps1', 'tools/windows-workspace-proof/checks.mjs',
        'tools/workspace-probe/probe.mjs', 'tools/workspace-probe/atomic-writer.mjs')
    foreach ($path in $sourcePaths) {
        $result.sources[$path] = (Get-FileHash -Algorithm SHA256 -LiteralPath $path).Hash.ToLowerInvariant()
    }
    [void](Invoke-Bounded 'identity' (Join-Path $env:SystemRoot 'System32/whoami.exe') @('/all'))
    # Hosted Unix utilities are development verification tools. No source-control executable or metadata is used.
    $unix = 'C:\Program Files\Git\usr\bin'
    if (Test-Path -LiteralPath $unix -PathType Container) { $env:PATH = "$unix;$env:PATH" }
    $utilities = [ordered]@{}
    foreach ($name in @('cat', 'cp', 'mv', 'rm', 'cmp', 'sh', 'bash', 'cc', 'gcc', 'node', 'bun')) {
        $application = Get-Command "$name.exe" -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($application) {
            $utilities[$name] = @{ path = $application.Source;
                sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $application.Source).Hash.ToLowerInvariant() }
            [void](Invoke-Bounded "utility-$name" $application.Source @('--version'))
        } else { $utilities[$name] = @{ path = $null; verdict = 'INCONCLUSIVE'; reason = 'executable absent from selected PATH' } }
    }
    $utilities | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $evidence 'utilities.json')
    if ([int]$os.BuildNumber -lt 17763) {
        $result.blockers += 'OS predates documented ProjFS minimum; feature setup refused.'
    } else {
        $pwsh = (Get-Process -Id $PID).Path
        $featureRun = Invoke-Bounded 'feature-command' $pwsh @('-NoProfile', '-File', (Join-Path $PSScriptRoot 'feature.ps1')) 60000
        $featurePath = Join-Path $evidence 'feature.json'
        if (Test-Path -LiteralPath $featurePath) {
            $result.feature = Get-Content -Raw -LiteralPath $featurePath | ConvertFrom-Json
            if (-not $result.feature.ready) { $result.blockers += "Client-ProjFS unavailable: $($result.feature.error)" }
        } else { $result.blockers += 'Feature child did not produce a result; inspect feature-command.json.' }
        if ($featureRun.timedOut) { $result.blockers += 'Feature command timed out; no reboot or retry attempted.' }
    }
    $vswhere = 'C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe'
    if (Test-Path -LiteralPath $vswhere) {
        $vs = Invoke-Bounded 'compiler-discovery' $vswhere @('-latest', '-products', '*', '-requires', 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64', '-property', 'installationPath')
        $installation = $vs.stdout.Trim()
        $dev = Join-Path $installation 'Common7/Tools/VsDevCmd.bat'
        if (Test-Path -LiteralPath $dev) {
            $exe = Join-Path $owned 'api-probe.exe'
            $obj = Join-Path $owned 'api-probe.obj'
            $source = Join-Path $PSScriptRoot 'api-probe.cpp'
            $batch = Join-Path $owned 'build.cmd'
            @("@echo off", "call `"$dev`" -no_logo -arch=x64", 'if errorlevel 1 exit /b 2',
                "cl /nologo /Bv /W4 /WX /EHsc /D_WIN32_WINNT=0x0A00 `"$source`" /Fe:`"$exe`" /Fo:`"$obj`" ProjectedFSLib.lib") |
                Set-Content -LiteralPath $batch -Encoding ascii
            $compile = Invoke-Bounded 'compiler' (Join-Path $env:SystemRoot 'System32/cmd.exe') @('/d', '/c', $batch) 60000
            $result.compilerReady = $compile.exitCode -eq 0 -and -not $compile.timedOut
            if ($result.compilerReady) {
                $api = Invoke-Bounded 'api' $exe @()
                $result.apiReady = $api.exitCode -eq 0 -and -not $api.timedOut
                if (-not $result.apiReady) { $result.blockers += 'System ProjFS DLL or required exports unavailable; inspect api.json.' }
            } else { $result.blockers += 'SDK/compiler/link check failed; inspect compiler.json.' }
        } else { $result.blockers += 'Visual Studio native compiler development environment missing.' }
    } else { $result.blockers += 'Native compiler discovery unavailable.' }
    $result.available = $result.blockers.Count -eq 0
    if ($result.available) { . (Join-Path $PSScriptRoot 'backend.ps1') }
} catch {
    $result.blockers += $_.ToString()
    $_ | Format-List * -Force
} finally {
    $cleanup = [ordered]@{ ownedRoot = $owned; providerStarted = $result.providerStarted; rootsProjected = $result.rootsCreated;
        removed = $false; error = $null; children = @($commands | ForEach-Object { @{ pid = $_.pid; exitCode = $_.exitCode; timedOut = $_.timedOut } }) }
    try {
        if ([IO.Path]::GetDirectoryName($owned) -ne $tempParent -or
            (Get-Content -Raw -LiteralPath (Join-Path $owned 'owner.txt')).Trim() -ne $token) { throw 'Owned cleanup boundary mismatch.' }
        if ($result.providerStarted -and -not $result.runtime.results.stoppedProviders) { throw 'Provider stop not confirmed; refusing root removal.' }
        if ($result.runtime -and -not $result.runtime.accountRemoved) { throw 'Disposable account cleanup not confirmed.' }
        # ProjFS roots and symlinks are owned reparse entries. Remove only after all providers stopped.
        foreach ($name in @('a', 'b', 'c', 'd')) {
            $root = Join-Path $owned $name
            if (Test-Path -LiteralPath $root) {
                & (Join-Path $env:SystemRoot 'System32/cmd.exe') /d /c "rmdir /s /q `"$root`""
                if (Test-Path -LiteralPath $root) { throw "Owned projected root $name removal failed." }
            }
        }
        $inside = Join-Path $owned 'base/inside-link'
        if (Test-Path -LiteralPath $inside) { Remove-Item -LiteralPath $inside -Force }
        if ((Get-Item -LiteralPath $owned).Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Owned root became a reparse point.' }
        foreach ($item in Get-ChildItem -LiteralPath $owned -Recurse -Force) {
            if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Refusing cleanup with reparse entries.' }
        }
        Remove-Item -LiteralPath $owned -Recurse -Force
        $cleanup.removed = -not (Test-Path -LiteralPath $owned)
    } catch { $cleanup.error = $_.ToString() }
    $cleanup | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath (Join-Path $evidence 'cleanup.json')
    $result.cleanupVerified = $cleanup.removed -and -not $cleanup.error -and
        @($commands | Where-Object { ($_.pid -and -not $_.exitConfirmed) -or $_.treeTermination -ne 'not needed' }).Count -eq 0
    $result.scopes = [ordered]@{}
    foreach ($scope in @('ordinary-path-common-probe', 'read-write-atomic-save-delete', 'traversal-symlinks-watching',
        'projected-compiler', 'cross-root-isolation', 'sparse-delta-clone', 'provider-kill-restart',
        'cache-private-state-cost', 'base-immutability', 'non-admin-provider', 'product-Gate2', 'GA')) {
        $result.scopes[$scope] = @{ verdict = 'INCONCLUSIVE'; reason = 'See separate common probe and native phase evidence; no whole-backend acceptance.' }
    }
    $result | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath (Join-Path $evidence 'summary.json')
    $commands | ConvertTo-Json -Depth 7 | Set-Content -LiteralPath (Join-Path $evidence 'commands.json')
    Add-Content -LiteralPath $env:GITHUB_STEP_SUMMARY -Value "Windows backend: INCONCLUSIVE. Availability=$($result.available). ProviderStarted=$($result.providerStarted). Inspect separate raw common and native evidence. No product Gate 2 acceptance."
    if ($transcriptStarted) { Stop-Transcript | Out-Null }
}
if (-not $result.cleanupVerified) { throw 'Owned cleanup failed; inspect artifact.' }
