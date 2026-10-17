# Bounded Windows sparse backend attempt. Disposable GitHub-hosted Windows only.
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted' -or
    $env:RUNNER_OS -ne 'Windows' -or -not $IsWindows) {
    throw 'Refusing outside disposable GitHub-hosted Windows. No host changes made.'
}
$token = [guid]::NewGuid().ToString('N')
$tempParent = [IO.Path]::GetFullPath($env:RUNNER_TEMP)
$owned = Join-Path $tempParent "javelin-sparse-proof-$token"
$evidence = Join-Path $tempParent "javelin-sparse-evidence-$token"
$adminResults = Join-Path $owned 'admin-results'
$frozen = Join-Path $owned 'frozen'
$username = "jvs$($token.Substring(0, 12))"
$commands = [Collections.Generic.List[object]]::new()
$result = [ordered]@{ schema = 1; sourceSHA = $env:SOURCE_SHA; runId = $env:GITHUB_RUN_ID; runAttempt = $env:GITHUB_RUN_ATTEMPT;
    blockers = @(); sources = [ordered]@{}; account = $username; accountCreated = $false; accountRemoved = $false; freeze = $null }
$transcriptStarted = $false

function Invoke-Bounded([string]$Name, [string]$File, [string[]]$Arguments, [int]$Milliseconds = 30000, [string]$Directory = '') {
    $info = [Diagnostics.ProcessStartInfo]::new($File)
    if ($Directory) { $info.WorkingDirectory = $Directory }
    $info.UseShellExecute = $false; $info.RedirectStandardOutput = $true; $info.RedirectStandardError = $true
    foreach ($argument in $Arguments) { $info.ArgumentList.Add($argument) }
    $entry = [ordered]@{ name = $Name; executable = $File; argv = $Arguments; timeoutMs = $Milliseconds; exitCode = $null; timedOut = $false; error = $null }
    $process = [Diagnostics.Process]::new(); $process.StartInfo = $info
    try {
        [void]$process.Start(); $entry.pid = $process.Id
        $stdout = $process.StandardOutput.ReadToEndAsync(); $stderr = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit($Milliseconds)) { $entry.timedOut = $true; $process.Kill($true); [void]$process.WaitForExit(5000) }
        $entry.exitCode = $process.ExitCode
        $entry.stdout = $stdout.GetAwaiter().GetResult(); $entry.stderr = $stderr.GetAwaiter().GetResult()
    } catch { $entry.error = $_.ToString() }
    finally {
        $commands.Add($entry)
        $entry | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath (Join-Path $evidence "$Name.json")
        Write-Host "COMMAND $Name exit=$($entry.exitCode) timedOut=$($entry.timedOut)"
        $process.Dispose()
    }
    return $entry
}
function Get-Tree([string]$Root) {
    @(Get-ChildItem -LiteralPath $Root -Recurse -Force | Sort-Object FullName | ForEach-Object {
        $relative = [IO.Path]::GetRelativePath($Root, $_.FullName)
        if ($_.PSIsContainer) { @{ path = $relative; kind = 'directory' } }
        else { @{ path = $relative; kind = 'file'; bytes = $_.Length; sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $_.FullName).Hash.ToLowerInvariant() } }
    })
}

try {
    New-Item -ItemType Directory -Path $evidence | Out-Null
    Add-Content -LiteralPath $env:GITHUB_ENV -Value "WINDOWS_SPARSE_EVIDENCE=$evidence"
    New-Item -ItemType Directory -Path $owned, $adminResults | Out-Null
    Set-Content -LiteralPath (Join-Path $owned 'owner.txt') -Value $token
    Start-Transcript -LiteralPath (Join-Path $evidence 'transcript.txt') | Out-Null; $transcriptStarted = $true
    if ($env:SOURCE_SHA -notmatch '^[0-9a-f]{40}$') { throw 'Exact CI checkout source SHA required.' }
    $os = Get-CimInstance Win32_OperatingSystem
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $result.host = @{ caption = $os.Caption; version = $os.Version; build = $os.BuildNumber; imageOS = $env:ImageOS; imageVersion = $env:ImageVersion;
        setupIdentity = $identity.Name; setupAdministrator = ([Security.Principal.WindowsPrincipal]::new($identity)).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator) }
    $here = 'tools/windows-sparse-backend'
    foreach ($path in @("$here/run.ps1", "$here/user-run.ps1", "$here/checks.mjs", "$here/sparse.cpp", "$here/projfs-provider.cpp",
        "$here/hardening-repro.ps1", "$here/hardening-control.cpp", "$here/hardening-checks.ps1",
        '.github/workflows/windows-sparse-backend.yml', 'tools/workspace-probe/probe.mjs', 'tools/workspace-probe/atomic-writer.mjs')) {
        $result.sources[$path] = (Get-FileHash -Algorithm SHA256 -LiteralPath $path).Hash.ToLowerInvariant()
    }
    # Read-only platform facts first; the only setup change allowed is starting an inbox filter if it is stopped.
    $system = Join-Path $env:SystemRoot 'System32'
    [void](Invoke-Bounded 'cldflt-query' (Join-Path $system 'sc.exe') @('query', 'cldflt'))
    [void](Invoke-Bounded 'cldflt-config' (Join-Path $system 'sc.exe') @('qc', 'cldflt'))
    [void](Invoke-Bounded 'filters' (Join-Path $system 'fltMC.exe') @('filters'))
    [void](Invoke-Bounded 'developer-mode' (Join-Path $system 'reg.exe') @('query', 'HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\AppModelUnlock'))
    $result.projfsFeature = (Get-WindowsOptionalFeature -Online -FeatureName Client-ProjFS).State.ToString()
    $cldflt = Get-Service -Name cldflt -ErrorAction SilentlyContinue
    $result.cldflt = @{ present = [bool]$cldflt; status = "$($cldflt.Status)"; startType = "$($cldflt.StartType)" }
    if ($cldflt -and $cldflt.Status -ne 'Running') {
        $start = Invoke-Bounded 'cldflt-start' (Join-Path $system 'sc.exe') @('start', 'cldflt')
        $result.cldflt.startAttempt = $start.exitCode
        $result.cldflt.statusAfter = "$((Get-Service -Name cldflt).Status)"
    }
    if (-not $cldflt) { $result.blockers += 'Cloud Files filter service cldflt absent.' }
    Get-ChildItem env: | ForEach-Object { "$($_.Name) length=$($_.Value.Length)" } | Set-Content -LiteralPath (Join-Path $evidence 'environment-names.txt')
    $kits = 'C:\Program Files (x86)\Windows Kits\10\Include'
    $header = Get-ChildItem -LiteralPath $kits -Recurse -Filter cfapi.h -ErrorAction SilentlyContinue | Sort-Object FullName | Select-Object -Last 1
    if ($header) {
        $result.sdk = @{ cfapi = $header.FullName; sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $header.FullName).Hash.ToLowerInvariant() }
        Select-String -LiteralPath $header.FullName -Pattern 'CF_POPULATION_POLICY_|CF_HYDRATION_POLICY_(FULL|PARTIAL)|CF_INSYNC_POLICY_TRACK_ALL|RequestKey|CF_PLACEHOLDER_STATE_|CF_CALLBACK_RENAME_FLAG|CfRegisterSyncRoot|CF_REGISTER_FLAG_' |
            ForEach-Object { "$($_.LineNumber): $($_.Line)" } | Set-Content -LiteralPath (Join-Path $evidence 'sdk-cfapi-declarations.txt')
    } else { $result.blockers += 'Windows SDK cfapi.h absent.' }
    $vswhere = 'C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe'
    $installation = (Invoke-Bounded 'compiler-discovery' $vswhere @('-latest', '-products', '*', '-requires', 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64', '-property', 'installationPath')).stdout.Trim()
    $dev = Join-Path $installation 'Common7/Tools/VsDevCmd.bat'
    # The pinned PR #9 provider keeps its original flags; the new provider targets Server 2022 (build 20348) headers.
    foreach ($build in @(@('sparse', 'CldApi.lib Bcrypt.lib Advapi32.lib', '/DUNICODE /D_UNICODE /DNTDDI_VERSION=0x0A00000A'),
        @('hardening-control', 'CldApi.lib Bcrypt.lib Advapi32.lib', '/DUNICODE /D_UNICODE /DNTDDI_VERSION=0x0A00000A'),
        @('projfs-provider', 'ProjectedFSLib.lib Ole32.lib', '/DNTDDI_VERSION=0x0A000008'))) {
        $flags = "/nologo /Bv /std:c++20 /W4 /WX /EHsc /D_WIN32_WINNT=0x0A00 $($build[2])"
        $batch = Join-Path $owned "build-$($build[0]).cmd"
        $source = Join-Path $PSScriptRoot "$($build[0]).cpp"
        # Command echo stays on so a failing batch line is visible in raw evidence.
        @("call `"$dev`" -no_logo -arch=x64", 'if errorlevel 1 exit /b 2', 'cd',
            "cl $flags `"$source`" /Fe:`"$(Join-Path $owned "$($build[0]).exe")`" /Fo:`"$(Join-Path $owned "$($build[0]).obj")`" $($build[1])") |
            Set-Content -LiteralPath $batch -Encoding ascii
        Copy-Item -LiteralPath $batch -Destination (Join-Path $evidence "build-$($build[0]).cmd.txt")
        $cmd = Join-Path $system 'cmd.exe'
        [void](Invoke-Bounded "cmd-echo-$($build[0])" $cmd @('/d', '/c', 'echo', 'cmd-ok'))
        [void](Invoke-Bounded "cmd-echo-owned-$($build[0])" $cmd @('/d', '/c', 'echo', 'cmd-ok') 30000 $owned)
        $operator = & $cmd /d /c echo cmd-ok 2>&1
        Set-Content -LiteralPath (Join-Path $evidence "cmd-operator-$($build[0]).txt") -Value "exit=$LASTEXITCODE cwd=$([Environment]::CurrentDirectory) pwd=$PWD`n$operator"
        $compile = Invoke-Bounded "compile-$($build[0])" $cmd @('/d', '/c', $batch) 90000 $owned
        if ($compile.exitCode -ne 0) { throw "Native $($build[0]) compile failed; this is a source failure, not a platform verdict." }
        $result.sources["binary/$($build[0]).exe"] = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $owned "$($build[0]).exe")).Hash.ToLowerInvariant()
    }
    if ($result.blockers.Count) { throw "Prerequisite blocked: $($result.blockers -join '; ')" }
    if ($env:CAPTURE_COMPILE_ONLY -eq 'true') {
        $result.compileOnly = $true
        return
    }
    # Fixtures live in an administrator-owned container. The proof user receives read/execute only.
    $tools = Join-Path $owned 'user-tools'
    New-Item -ItemType Directory -Path $tools, $frozen, (Join-Path $owned 'user-results'), (Join-Path $owned 'staging') | Out-Null
    Copy-Item -LiteralPath (Get-Command bun.exe -CommandType Application).Source -Destination (Join-Path $tools 'bun.exe')
    foreach ($name in @('probe.mjs', 'atomic-writer.mjs')) { Copy-Item -LiteralPath "tools/workspace-probe/$name" -Destination (Join-Path $tools $name) }
    foreach ($name in @('checks.mjs', 'user-run.ps1', 'hardening-repro.ps1', 'hardening-checks.ps1')) { Copy-Item -LiteralPath "$here/$name" -Destination (Join-Path $tools $name) }
    $seed = Invoke-Bounded 'fixture-seed-native' (Join-Path $tools 'bun.exe') @((Join-Path $tools 'checks.mjs'), $owned, $adminResults, 'seed')
    if ($seed.exitCode -ne 0) { throw 'Native fixtures could not be seeded.' }
    New-Item -ItemType Directory -Path (Join-Path $frozen 'base-common') | Out-Null
    $common = Invoke-Bounded 'fixture-seed-common' (Join-Path $tools 'bun.exe') @((Join-Path $tools 'probe.mjs'), '--seed', (Join-Path $frozen 'base-common'))
    if ($common.exitCode -ne 0) { throw 'Unchanged common fixture could not be seeded.' }
    $password = ConvertTo-SecureString ('Aa1!' + [guid]::NewGuid().ToString('N') + [guid]::NewGuid().ToString('N')) -AsPlainText -Force
    New-LocalUser -Name $username -Password $password -Description 'Disposable Javelin sparse proof user' | Out-Null
    $result.accountCreated = $true
    Add-LocalGroupMember -SID 'S-1-5-32-545' -Member $username
    $credential = [PSCredential]::new("$env:COMPUTERNAME\$username", $password)
    foreach ($acl in @(
        # The user may add entries here and fully control what it creates, but cannot delete or rename
        # administrator children such as the frozen container (no delete-child on this folder).
        @('owned-user-acl', @($owned, '/grant', "${username}:(RX,WD,AD)", "${username}:(OI)(CI)(IO)F")),
        @('frozen-owner', @($frozen, '/setowner', '*S-1-5-32-544', '/T', '/C', '/L')),
        # Protect the container only; children inherit. Explicit (OI)(CI) grants applied to files leave them with empty DACLs.
        @('frozen-acl', @($frozen, '/inheritance:r', '/grant:r', "${username}:(OI)(CI)RX", '*S-1-5-32-544:(OI)(CI)F', '*S-1-5-18:(OI)(CI)F', '/C', '/L')),
        @('frozen-children-inherit', @((Join-Path $frozen '*'), '/reset', '/T', '/C', '/L')))) {
        $set = Invoke-Bounded $acl[0] (Join-Path $system 'icacls.exe') $acl[1]
        if ($set.exitCode -ne 0) { throw "ACL step $($acl[0]) failed." }
    }
    [void](Invoke-Bounded 'frozen-acl-readback' (Join-Path $system 'icacls.exe') @($frozen, '/T'))
    New-Item -ItemType Directory -Path (Join-Path $frozen 'bindings') | Out-Null
    [void](Invoke-Bounded 'bindings-owner' (Join-Path $system 'icacls.exe') @((Join-Path $frozen 'bindings'), '/setowner', '*S-1-5-32-544'))
    $result.baseTrees = @{ native = Get-Tree (Join-Path $frozen 'base-native'); common = Get-Tree (Join-Path $frozen 'base-common') }
    $pwsh = (Get-Process -Id $PID).Path
    $argv = @('bounded', '190000', $pwsh, '-NoProfile', '-File', (Join-Path $tools 'user-run.ps1'), '-Owned', $owned, '-Username', $username)
    $child = Start-Process -FilePath (Join-Path $owned 'hardening-control.exe') -ArgumentList ($argv | ForEach-Object { '"' + $_ + '"' }) -Credential $credential `
        -WorkingDirectory $owned -RedirectStandardOutput (Join-Path $evidence 'ordinary-user.stdout.txt') -RedirectStandardError (Join-Path $evidence 'ordinary-user.stderr.txt') -PassThru
    $deadline = [DateTime]::UtcNow.AddSeconds(205)
    while (-not $child.HasExited -and [DateTime]::UtcNow -lt $deadline) {
        foreach ($controlRequest in @(Get-ChildItem -LiteralPath $owned -Filter 'control-*.request.json' -File)) {
            $reply = [ordered]@{ ok = $false; error = $null }
            try {
                $requestData = Get-Content -Raw -LiteralPath $controlRequest.FullName | ConvertFrom-Json
                if ($requestData.name -notmatch '^(a|b|d|e|cr|pa|pb|pc|h[a-z0-9-]+)$') { throw 'Unsupported owned root.' }
                $targetRoot = Join-Path $owned $requestData.name
                if ($requestData.operation -eq 'bind') {
                    if ($requestData.base -notin @((Join-Path $frozen 'base-native'), (Join-Path $frozen 'base-rename'), (Join-Path $frozen 'base-common'))) { throw 'Unsupported base.' }
                    if ($requestData.snapshot -ne '-' -and $requestData.snapshot -ne (Join-Path $frozen 'snap') -and
                        -not ([IO.Path]::GetFullPath($requestData.snapshot)).StartsWith((Join-Path $owned 'staging') + '\', [StringComparison]::OrdinalIgnoreCase)) { throw 'Unsupported snapshot.' }
                    $r = Invoke-Bounded "bind-$($requestData.name)" (Join-Path $owned 'sparse.exe') @('bind', $requestData.base, $requestData.snapshot, $targetRoot)
                    if ($r.exitCode -ne 0) { throw 'Protected root binding failed.' }
                    $r = Invoke-Bounded "binding-owner-$($requestData.name)" (Join-Path $system 'icacls.exe') @((Join-Path $frozen "bindings/$($requestData.name).txt"), '/setowner', '*S-1-5-32-544')
                    if ($r.exitCode -ne 0) { throw 'Protected binding owner failed.' }
                } elseif ($requestData.operation -in @('seal', 'unseal')) {
                    $sid = (Get-LocalUser -Name $username).SID.Value
                    $r = Invoke-Bounded "$($requestData.operation)-$($requestData.name)" (Join-Path $owned 'sparse.exe') @('access', $targetRoot, $sid, $requestData.operation)
                    if ($r.exitCode -ne 0) { throw 'Quiescent root ACL step failed.' }
                } else { throw 'Unsupported fixture operation.' }
                $reply.ok = $true
            } catch { $reply.error = $_.ToString() }
            $reply | ConvertTo-Json | Set-Content -LiteralPath ($controlRequest.FullName + '.done')
            Remove-Item -LiteralPath $controlRequest.FullName
        }
        $request = Join-Path $owned 'freeze.request'
        if ((Test-Path -LiteralPath $request) -and -not $result.freeze) {
            # Freeze: an administrator copy becomes administrator-owned, read-only and not writable by the proof user.
            $target = Join-Path $frozen 'snap'
            Copy-Item -LiteralPath (Join-Path $owned 'staging/snap') -Destination $target -Recurse
            [void](Invoke-Bounded 'snapshot-owner' (Join-Path $system 'icacls.exe') @($target, '/setowner', '*S-1-5-32-544', '/T', '/C', '/L'))
            [void](Invoke-Bounded 'snapshot-acl' (Join-Path $system 'icacls.exe') @($target, '/inheritance:r', '/grant:r', "${username}:(OI)(CI)RX", '*S-1-5-32-544:(OI)(CI)F', '*S-1-5-18:(OI)(CI)F', '/C', '/L'))
            [void](Invoke-Bounded 'snapshot-children-inherit' (Join-Path $system 'icacls.exe') @((Join-Path $target '*'), '/reset', '/T', '/C', '/L'))
            [void](Invoke-Bounded 'snapshot-readonly' (Join-Path $system 'attrib.exe') @('+R', (Join-Path $target '*'), '/S'))
            [void](Invoke-Bounded 'snapshot-acl-readback' (Join-Path $system 'icacls.exe') @($target, '/T'))
            $result.freeze = @{ tree = Get-Tree $target; at = [DateTime]::UtcNow.ToString('o') }
            # checks.mjs compares its own tree format, so it records that independently from the user side.
            $userTree = Invoke-Bounded 'snapshot-tree' (Join-Path $tools 'bun.exe') @('-e', "const fs=require('fs/promises'),p=require('path'),c=require('crypto');async function t(b){const r=[];async function v(n){const q=p.join(b,n),s=await fs.lstat(q);if(s.isDirectory()){r.push([n,'directory']);for(const x of (await fs.readdir(q)).sort())await v(p.join(n,x))}else r.push([n,'file',s.size,c.createHash('sha256').update(await fs.readFile(q)).digest('hex')])}await v('');return r}t(process.argv[1]).then(x=>console.log(JSON.stringify({tree:x})))", $target)
            Set-Content -LiteralPath (Join-Path $owned 'freeze.json') -Value $userTree.stdout
            Set-Content -LiteralPath (Join-Path $owned 'freeze.done') -Value 'done'
        }
        Start-Sleep -Milliseconds 200
    }
    if (-not $child.HasExited) { $child.Kill($true); [void]$child.WaitForExit(5000); throw 'Ordinary-user wrapper exceeded its independent budget.' }
    $result.userWrapperExit = $child.ExitCode
    Copy-Item -Path (Join-Path $owned 'user-results/*') -Destination $evidence
    Copy-Item -Path (Join-Path $adminResults '*') -Destination $evidence
    $reproductionPath = Join-Path $evidence 'hardening-checks.json'
    if (-not (Test-Path -LiteralPath $reproductionPath) -or
        (Get-Content -Raw -LiteralPath $reproductionPath | ConvertFrom-Json).verdict -ne 'VERIFIED') {
        $result.blockers += 'Capture hardening regressions did not pass.'
    }
    $userSummary = Get-Content -Raw -LiteralPath (Join-Path $evidence 'user-summary.json') | ConvertFrom-Json
    if ($userSummary.errors.Count -or -not $userSummary.providersStopped) { $result.blockers += 'User fixture errors or live providers.' }
    foreach ($phase in $userSummary.phases.PSObject.Properties) {
        $expected = if ($phase.Name -eq 'common-probe') { 1 } else { 0 }
        if ($phase.Value -ne $expected) { $result.blockers += "Unexpected phase result $($phase.Name): $($phase.Value)" }
    }
    if ($result.freeze) { $result.snapshotUnchangedAtEnd = (ConvertTo-Json (Get-Tree (Join-Path $frozen 'snap')) -Depth 4) -eq (ConvertTo-Json $result.freeze.tree -Depth 4) }
    $result.baseUnchangedAtEnd = (ConvertTo-Json (Get-Tree (Join-Path $frozen 'base-native')) -Depth 4) -eq (ConvertTo-Json $result.baseTrees.native -Depth 4)
} catch {
    $result.blockers += $_.ToString()
    $_ | Format-List * -Force
} finally {
    if ($result.accountCreated) {
        try { Remove-LocalUser -Name $username; $result.accountRemoved = -not (Get-LocalUser -Name $username -ErrorAction SilentlyContinue) }
        catch { $result.blockers += "Account cleanup failed: $_" }
    } else { $result.accountRemoved = $true }
    $cleanup = [ordered]@{ ownedRoot = $owned; removed = $false; error = $null; roots = [ordered]@{} }
    try {
        if ([IO.Path]::GetDirectoryName($owned) -ne $tempParent -or
            (Get-Content -Raw -LiteralPath (Join-Path $owned 'owner.txt')).Trim() -ne $token) { throw 'Owned cleanup boundary mismatch.' }
        $rootNames = @('a', 'b', 'd', 'e', 'cr', 'pa', 'pb', 'pc', 'pr')
        $rootNames += @(Get-ChildItem -LiteralPath $owned -Directory | Where-Object Name -Match '^h[a-z0-9-]+$' | ForEach-Object Name)
        foreach ($name in $rootNames) {
            $root = Join-Path $owned $name
            if (Test-Path -LiteralPath $root) {
                [void](Invoke-Bounded "cleanup-owner-$name" (Join-Path $system 'icacls.exe') @($root, '/setowner', '*S-1-5-32-544', '/T', '/C', '/L'))
                [void](Invoke-Bounded "cleanup-acl-$name" (Join-Path $system 'icacls.exe') @($root, '/grant', '*S-1-5-32-544:(OI)(CI)F', '/T', '/C', '/L'))
                [void](Invoke-Bounded "cleanup-attributes-$name" (Join-Path $system 'attrib.exe') @('-R', (Join-Path $root '*'), '/S', '/D', '/L'))
                $unregister = & (Join-Path $owned 'sparse.exe') unregister $root
                & (Join-Path $system 'cmd.exe') /d /c "rmdir /s /q `"$root`""
                $cleanup.roots[$name] = @{ unregister = "$unregister"; removed = -not (Test-Path -LiteralPath $root) }
                if (Test-Path -LiteralPath $root) { throw "Owned root $name removal failed." }
            }
        }
        if (Test-Path -LiteralPath $frozen) {
            & "$env:SystemRoot\System32\icacls.exe" $frozen /reset /T /C /L /Q | Out-Null
            & "$env:SystemRoot\System32\attrib.exe" -R (Join-Path $frozen '*') /S /D /L | Out-Null
        }
        foreach ($base in @('base-native', 'base-rename', 'base-common')) {
            $link = Join-Path $frozen "$base/inside-link"
            if (Test-Path -LiteralPath $link) { Remove-Item -LiteralPath $link -Force }
        }
        foreach ($item in Get-ChildItem -LiteralPath $owned -Recurse -Force) {
            if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw "Refusing cleanup with reparse entry $($item.FullName)." }
        }
        Remove-Item -LiteralPath $owned -Recurse -Force
        $cleanup.removed = -not (Test-Path -LiteralPath $owned)
    } catch { $cleanup.error = $_.ToString() }
    $cleanup | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath (Join-Path $evidence 'cleanup.json')
    $result.cleanupVerified = $cleanup.removed -and -not $cleanup.error -and $result.accountRemoved
    $result | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath (Join-Path $evidence 'summary.json')
    $commands | ConvertTo-Json -Depth 7 | Set-Content -LiteralPath (Join-Path $evidence 'commands.json')
    Add-Content -LiteralPath $env:GITHUB_STEP_SUMMARY -Value 'Windows sparse backend attempt finished. Workflow success means collection completed; inspect raw evidence for verdicts.'
    if ($transcriptStarted) { Stop-Transcript | Out-Null }
}
if (-not $result.cleanupVerified) { throw 'Owned cleanup failed; inspect artifact.' }
if ($result.blockers.Count) { throw 'Proof collection encountered a blocker; inspect raw artifact.' }
