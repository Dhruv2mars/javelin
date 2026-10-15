# Dot-sourced by the ordinary proof user, against unchanged inherited sparse.exe.
if ($env:CAPTURE_PROOF_MODE -ne 'inherited-control' -or $admin -or
    $env:RUNNER_ENVIRONMENT -ne 'github-hosted' -or -not $IsWindows) { throw 'Hosted ordinary-user control required.' }
$control = Join-Path $Owned 'hardening-control.exe'
$repro = [ordered]@{ schema = 1; mode = $env:CAPTURE_PROOF_MODE; identity = $summary.identity;
    sourceSHA = $env:SOURCE_SHA; commands = @(); cases = [ordered]@{}; errors = @(); preconditionFailures = @(); verdict = 'INCONCLUSIVE' }
function Repro-Command([string]$Label, [string]$File, [string[]]$Arguments, [int]$Budget = 8000) {
    $info = [Diagnostics.ProcessStartInfo]::new($sparse)
    $info.UseShellExecute = $false; $info.RedirectStandardOutput = $true; $info.RedirectStandardError = $true
    foreach ($a in @('bounded', "$Budget", $File) + $Arguments) { $info.ArgumentList.Add($a) }
    $p = [Diagnostics.Process]::new(); $p.StartInfo = $info
    $row = [ordered]@{ label = $Label; executable = $File; argv = $Arguments; budgetMs = $Budget }
    try {
        [void]$p.Start(); $row.pid = $p.Id
        $o = $p.StandardOutput.ReadToEndAsync(); $e = $p.StandardError.ReadToEndAsync()
        if (-not $p.WaitForExit($Budget + 6000)) { $p.Kill($true); [void]$p.WaitForExit(5000); throw 'Wrapper exceeded bound.' }
        $row.exitCode = $p.ExitCode; $row.stdout = $o.GetAwaiter().GetResult(); $row.stderr = $e.GetAwaiter().GetResult()
    } catch { $row.error = $_.ToString() }
    finally { $p.Dispose(); $repro.commands += $row }
    return $row
}
function Repro-Capture([string]$Label, [string]$Root) {
    $out = Join-Path $Owned "staging/repro-$Label"
    $r = Repro-Command "capture-$Label" $sparse @('capture', $native, $Root, $out)
    $manifest = Join-Path $out 'manifest.tsv'
    $records = if (Test-Path -LiteralPath $manifest) { Get-Content -Raw -LiteralPath $manifest } else { $null }
    $blobs = if (Test-Path -LiteralPath (Join-Path $out 'blobs')) {
        @(Get-ChildItem -LiteralPath (Join-Path $out 'blobs') -File | ForEach-Object {
            @{ name = $_.Name; bytes = $_.Length; digest = (Get-FileHash -LiteralPath $_.FullName).Hash.ToLowerInvariant() }
        })
    } else { @() }
    return @{ command = $r; manifestExists = (Test-Path -LiteralPath $manifest); manifest = $records; blobs = @($blobs) }
}
try {
    # Both cases retain a real base identity while declaring another logical length.
    foreach ($case in @(@('zero', 4096, 0), @('short', 16384, 4096))) {
        $name = "hpartial-$($case[0])"; $r = Join-Path $Owned $name
        try {
            Start-Provider $name 'cfapi' $native
            Stop-Provider $name
            $create = Repro-Command "placeholder-$($case[0])" $control @('placeholder', $r, 'partial.bin', "$($case[1])", 'bytes.bin')
            if ($create.exitCode -ne 0) {
                $repro.preconditionFailures += "partial-$($case[0]): placeholder creation failed"
                $repro.cases["partial-$($case[0])"] = @{ verdict = 'NOT_ESTABLISHED'; create = $create }
                continue
            }
            $created = Repro-Command "inspect-created-$($case[0])" $control @('inspect', (Join-Path $r 'partial.bin'), 'bytes.bin')
            $metadata = $created.stdout | ConvertFrom-Json
            if ($created.exitCode -ne 0 -or -not $metadata.identityMatches -or $metadata.logicalBytes -ne $case[1]) {
                throw 'Created placeholder identity or size was not established.'
            }
            Start-Provider $name 'cfapi' $native
            $move = Repro-Command "rename-$($case[0])" $sparse @('move', (Join-Path $r 'partial.bin'), (Join-Path $r 'moved-partial.bin'))
            if ($move.exitCode -ne 0) { throw 'Required placeholder rename failed.' }
            $hydrate = if ($case[2]) { Repro-Command "hydrate-$($case[0])" $control @('hydrate', (Join-Path $r 'moved-partial.bin'), "$($case[2])") } else { $null }
            Stop-Provider $name
            $inspected = Repro-Command "inspect-moved-$($case[0])" $control @('inspect', (Join-Path $r 'moved-partial.bin'), 'bytes.bin')
            $state = Repro-Command "state-$($case[0])" $sparse @('audit', $r)
            if ($state.exitCode -ne 0) { throw 'Cannot establish placeholder state.' }
            $entry = ($state.stdout | ConvertFrom-Json).entries | Where-Object path -EQ 'moved-partial.bin'
            if (-not $entry -or -not ($entry.placeholderState -band 16) -or
                ($entry.placeholderState -band 8) -or $entry.logicalBytes -eq 12288) {
                $repro.preconditionFailures += "partial-$($case[0]): required renamed partial state missing"
                $repro.cases["partial-$($case[0])"] = @{ verdict = 'NOT_ESTABLISHED'; create = $create; move = $move; state = $state }
                continue
            }
            $read = Repro-Command "read-$($case[0])" $control @('read', (Join-Path $r 'moved-partial.bin'))
            $stream = Repro-Command "stream-$($case[0])" $control @('stream', (Join-Path $r 'moved-partial.bin'))
            $capture = Repro-Capture "partial-$($case[0])" $r
            $readResult = $read.stdout | ConvertFrom-Json
            $streamResult = $stream.stdout | ConvertFrom-Json
            $prefixMatchesCase = if ($case[0] -eq 'zero') { $readResult.readBytes -eq 0 } else { $readResult.readBytes -gt 0 }
            $failedRead = $readResult.readError -gt 0 -and $readResult.readBytes -lt $entry.logicalBytes -and $prefixMatchesCase
            $acceptedPrefix = $streamResult.opened -and $streamResult.bytes -eq $readResult.readBytes -and
                $capture.command.exitCode -eq 0 -and $capture.manifestExists -and
                $capture.manifest -match '(?m)^F\tmoved-partial\.bin\t[0-9a-f]{64}$'
            $established = $failedRead -and $acceptedPrefix
            $repro.cases["partial-$($case[0])"] = @{ reproductionEstablished = $established; create = $create;
                created = $created; inspected = $inspected; move = $move; hydrate = $hydrate; state = $state; read = $read; stream = $stream; capture = $capture }
            if (-not $established) { $repro.preconditionFailures += "partial-$($case[0]): intended failed-read prefix acceptance not observed" }
        } catch { $repro.errors += "partial-$($case[0]): $_"; $repro.preconditionFailures += "partial-$($case[0]): setup or observation failed" }
        finally { if ($providers.ContainsKey($name)) { Stop-Provider $name } }
    }
    # Only the disposable sibling fixture can be the target of this junction.
    $outside = Join-Path $Owned 'outside-control'
    New-Item -ItemType Directory -Path $outside | Out-Null
    $sentinel = Join-Path $outside 'sentinel.txt'
    Set-Content -LiteralPath $sentinel -Value 'outside-owned-control-sentinel'
    $before = (Get-FileHash -LiteralPath $sentinel).Hash
    $r = Join-Path $Owned 'hjunction'; $junction = Join-Path $r 'outside'
    try {
        Start-Provider 'hjunction' 'cfapi' $native; Stop-Provider 'hjunction'
        New-Item -ItemType Junction -Path $junction -Target $outside | Out-Null
        $a = Repro-Command 'junction-audit' $sparse @('audit', $r)
        $c = Repro-Capture 'junction' $r
        $repro.cases.junction = @{ audit = $a; capture = $c; sentinelDigest = $before.ToLowerInvariant();
            outsideUnchanged = (Get-FileHash -LiteralPath $sentinel).Hash -eq $before }
    } finally {
        if (Test-Path -LiteralPath $junction) {
            $remove = Repro-Command 'remove-junction-without-traversal' $control @('unlink-directory', $junction)
            if ($remove.exitCode -ne 0) { throw 'Junction removal failed; refuse recursive cleanup.' }
        }
        if ($providers.ContainsKey('hjunction')) { Stop-Provider 'hjunction' }
    }
    # An unhydrated snapshot root wrongly resolves its in-sync identities against base after a marker edit.
    try {
        Start-Provider 'hmarker' 'cfapi' $native (Join-Path $frozen 'snap'); Stop-Provider 'hmarker'
        Set-Content -LiteralPath (Join-Path $Owned 'hmarker.registered') -Value 'snapshot=-' -NoNewline
        $repro.cases.marker = Repro-Capture 'marker' (Join-Path $Owned 'hmarker')
    } finally { if ($providers.ContainsKey('hmarker')) { Stop-Provider 'hmarker' } }
    # Validate source loading before any hydration. These snapshots are writable copies, never the frozen one.
    $cases = [ordered]@{
        outsideBlob = "F`tREADME.txt`t..\outside-blob`n"
        digestMismatch = "F`tREADME.txt`t$('0' * 64)`n"
        duplicate = "F`tREADME.txt`t$('0' * 64)`nF`tREADME.txt`t$('0' * 64)`n"
        prefix = "F`tx`t$('0' * 64)`nF`tx\y`t$('0' * 64)`n"
        outsideLink = "L`tinside-link`t..\outside-control\sentinel.txt`n"
    }
    foreach ($label in $cases.Keys) {
        $snapshot = Join-Path $Owned "staging/manifest-$label"
        New-Item -ItemType Directory -Path (Join-Path $snapshot 'blobs') -Force | Out-Null
        Set-Content -LiteralPath (Join-Path $snapshot 'outside-blob') -Value 'outside blob directory'
        Set-Content -LiteralPath (Join-Path $snapshot "blobs/$('0' * 64)") -Value 'digest mismatch'
        [IO.File]::WriteAllText((Join-Path $snapshot 'manifest.tsv'), $cases[$label], [Text.UTF8Encoding]::new($false))
        $name = "hmanifest-$($label.ToLowerInvariant())"; $accepted = $false
        try { Start-Provider $name 'cfapi' $native $snapshot; $accepted = $true }
        catch { $repro.errors += "manifest-${label}: $_" }
        finally { if ($providers.ContainsKey($name)) { Stop-Provider $name } }
        $repro.cases["manifest-$label"] = @{ loaded = $accepted; manifest = $cases[$label] }
    }
} catch { $repro.errors += $_.ToString() }
finally {
    # The workflow must not turn a successful capture of an empty control into reproduction evidence.
    if ($repro.preconditionFailures.Count -or $repro.errors.Count) {
        $summary.phases['hardening-reproduction'] = 3
    } else {
        $summary.phases['hardening-reproduction'] = 0
        $repro.verdict = 'COLLECTED'
    }
    $repro | ConvertTo-Json -Depth 14 | Set-Content -LiteralPath (Join-Path $Evidence 'hardening-repro.json')
}
