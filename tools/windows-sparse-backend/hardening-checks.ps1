# Real hosted Windows regressions. Runs only inside the ordinary-user job.
if ($admin -or -not $IsWindows -or $env:CAPTURE_PROOF_MODE -ne 'hardened') { throw 'Hosted ordinary-user regression required.' }
$control = Join-Path $Owned 'hardening-control.exe'
$hardening = [ordered]@{ verdict = 'NOT VERIFIED'; sourceSHA = $env:SOURCE_SHA; identity = $summary.identity; commands = @(); checks = @(); error = $null }
function HAssert([bool]$Condition, [string]$Message) { if (-not $Condition) { throw $Message }; $hardening.checks += $Message }
function HCommand([string]$Label, [string]$File, [string[]]$Arguments) {
    $i = [Diagnostics.ProcessStartInfo]::new($sparse)
    $i.UseShellExecute = $false; $i.RedirectStandardOutput = $true; $i.RedirectStandardError = $true
    foreach ($a in @('bounded', '10000', $File) + $Arguments) { $i.ArgumentList.Add($a) }
    $p = [Diagnostics.Process]::new(); $p.StartInfo = $i
    try {
        [void]$p.Start(); $o = $p.StandardOutput.ReadToEndAsync(); $e = $p.StandardError.ReadToEndAsync()
        if (-not $p.WaitForExit(16000)) { $p.Kill($true); [void]$p.WaitForExit(5000); throw 'Regression command exceeded job bound.' }
        $r = @{ label = $Label; executable = $File; argv = $Arguments; pid = $p.Id; exitCode = $p.ExitCode;
            stdout = $o.GetAwaiter().GetResult(); stderr = $e.GetAwaiter().GetResult() }
        $hardening.commands += $r
        HAssert ($r.stderr -match 'activeAfterTermination=0') "$Label child job empty"
        return $r
    } finally { $p.Dispose() }
}
function HCapture([string]$Name, [string]$Label) {
    $out = Join-Path $Owned "staging/fixed-$Label"
    Admin-Control 'seal' $Name
    try { return HCommand $Label $sparse @('capture', $native, (Join-Path $Owned $Name), $out) }
    finally { Admin-Control 'unseal' $Name }
}
function HRefused([string]$Name, [string]$Label, [string]$Reason) {
    $r = HCapture $Name $Label
    HAssert ($r.exitCode -ne 0 -and $r.stderr -match $Reason) "$Label refused for expected reason"
    HAssert (-not (Test-Path -LiteralPath (Join-Path $Owned "staging/fixed-$Label"))) "$Label selected no snapshot"
    HAssert (-not (Test-Path -LiteralPath (Join-Path $Owned "staging/fixed-$Label.pending"))) "$Label left no staging"
}
try {
    foreach ($case in @(@('zero', 4096, 0), @('resident', 16384, 4096))) {
        $name = "hpartial-$($case[0])"; $root = Join-Path $Owned $name
        Start-Provider $name 'cfapi' $native; Stop-Provider $name
        $r = HCommand "create-$name" $control @('placeholder', $root, 'partial.bin', "$($case[1])", 'bytes.bin')
        HAssert ($r.exitCode -eq 0 -and ($r.stdout | ConvertFrom-Json).processed -eq 1) "$name placeholder exists"
        Start-Provider $name 'cfapi' $native
        $r = HCommand "move-$name" $sparse @('move', (Join-Path $root 'partial.bin'), (Join-Path $root 'moved.bin'))
        HAssert ($r.exitCode -eq 0) "$name renamed"
        if ($case[2]) { [void](HCommand "hydrate-$name" $control @('hydrate', (Join-Path $root 'moved.bin'), "$($case[2])")) }
        Stop-Provider $name
        $r = HCommand "state-$name" $sparse @('audit', $root)
        $entry = ($r.stdout | ConvertFrom-Json).entries | Where-Object path -EQ 'moved.bin'
        HAssert ($entry -and ($entry.placeholderState -band 16) -and -not ($entry.placeholderState -band 8) -and $entry.logicalBytes -eq $case[1]) "$name partial state established"
        $r = HCommand "read-$name" $control @('read', (Join-Path $root 'moved.bin'))
        HAssert ($r.exitCode -ne 0 -and ($r.stdout | ConvertFrom-Json).readError -gt 0) "$name real read failure established"
        HRefused $name $name 'uncertain partial'
    }
    $root = Join-Path $Owned 'hjunction'; $outside = Join-Path $Owned 'outside-fixed'
    New-Item -ItemType Directory -Path $outside | Out-Null
    Set-Content -LiteralPath (Join-Path $outside 'sentinel.txt') -Value 'owned outside fixture'
    $before = (Get-FileHash -LiteralPath (Join-Path $outside 'sentinel.txt')).Hash
    Start-Provider 'hjunction' 'cfapi' $native; Stop-Provider 'hjunction'
    $junction = Join-Path $root 'outside'
    try {
        New-Item -ItemType Junction -Path $junction -Target $outside | Out-Null
        HRefused 'hjunction' 'junction' 'unsupported reparse'
        $r = HCommand 'audit-junction' $sparse @('audit', $root)
        HAssert ($r.exitCode -ne 0 -and $r.stderr -match 'unsupported reparse' -and $r.stdout -notmatch 'sentinel') 'audit refuses junction before outside entry'
    } finally { [void](HCommand 'unlink-junction' $control @('unlink-directory', $junction)) }
    HAssert ((Get-FileHash -LiteralPath (Join-Path $outside 'sentinel.txt')).Hash -eq $before) 'outside fixture unchanged'
    New-Item -ItemType SymbolicLink -Path (Join-Path $root 'escape-link') -Target '..\outside-fixed\sentinel.txt' | Out-Null
    HRefused 'hjunction' 'outside-link' 'path alias'
    Remove-Item -LiteralPath (Join-Path $root 'escape-link')

    Start-Provider 'hmarker' 'cfapi' $native (Join-Path $frozen 'snap'); Stop-Provider 'hmarker'
    Set-Content -LiteralPath (Join-Path $Owned 'hmarker.registered') -Value 'snapshot=-'
    HRefused 'hmarker' 'snapshot-mode' 'base-served'
    $binding = Join-Path $frozen 'bindings/hmarker.txt'
    $r = HCommand 'binding-self-grant' (Join-Path $env:SystemRoot 'System32/icacls.exe') @($binding, '/grant', "*$($identity.User.Value):F")
    HAssert ($r.exitCode -ne 0) 'actual SID cannot grant itself config writes'
    $r = HCommand 'binding-owner' (Join-Path $env:SystemRoot 'System32/icacls.exe') @($binding)
    HAssert ($r.exitCode -eq 0) 'protected binding ACL readback recorded'

    Start-Provider 'hactive' 'cfapi' $native
    $r = HCapture 'hactive' 'provider-active'
    HAssert ($r.exitCode -ne 0 -and $r.stderr -match 'is active') 'active provider capture refused'
    Stop-Provider 'hactive'
    $r = HCommand 'unsealed-capture' $sparse @('capture', $native, (Join-Path $Owned 'hactive'), (Join-Path $Owned 'staging/unsealed'))
    HAssert ($r.exitCode -ne 0 -and $r.stderr -match 'quiescent') 'unprotected active-edit mode refused'
    Set-Content -LiteralPath (Join-Path $Owned 'hactive/new.txt') -Value 'bounded private change'
    $r = HCapture 'hactive' 'retry'
    HAssert ($r.exitCode -eq 0) 'quiescent capture succeeds after refusal'
    $manifest = Join-Path $Owned 'staging/fixed-retry/manifest.tsv'
    $before = (Get-FileHash -LiteralPath $manifest).Hash
    $r = HCapture 'hactive' 'retry'
    HAssert ($r.exitCode -ne 0 -and $r.stderr -match 'already exists') 'retry cannot replace selected snapshot'
    HAssert ((Get-FileHash -LiteralPath $manifest).Hash -eq $before) 'previous snapshot unchanged after refusal'
    [IO.File]::WriteAllBytes((Join-Path $Owned 'hactive/oversize.bin'), [byte[]]::new(262145))
    HRefused 'hactive' 'oversize' 'bounded file metadata'
    Remove-Item -LiteralPath (Join-Path $Owned 'hactive/oversize.bin')

    Start-Provider 'hbudget' 'cfapi' $native; Stop-Provider 'hbudget'
    $budgetRoot = Join-Path $Owned 'hbudget'
    foreach ($n in 1..129) { [IO.File]::WriteAllText((Join-Path $budgetRoot "extra-$n"), '') }
    $r = HCommand 'entry-budget' $sparse @('audit', $budgetRoot)
    HAssert ($r.exitCode -ne 0 -and $r.stderr -match 'walk bound') 'entry budget refuses bounded walk'
    foreach ($n in 1..129) { Remove-Item -LiteralPath (Join-Path $budgetRoot "extra-$n") }
    $deep = $budgetRoot
    foreach ($n in 1..17) { $deep = Join-Path $deep 'x'; [IO.Directory]::CreateDirectory($deep) | Out-Null }
    $r = HCommand 'depth-budget' $sparse @('audit', $budgetRoot)
    HAssert ($r.exitCode -ne 0 -and $r.stderr -match 'path bound') 'path depth budget refuses bounded walk'

    $payload = [Text.Encoding]::UTF8.GetBytes('private snapshot payload')
    $digest = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($payload)).ToLowerInvariant()
    $rows = [ordered]@{
        escape = "F`tREADME.txt`t..\outside`n"
        absolute = "F`tC:\outside`t$digest`n"
        ads = "F`tREADME.txt:stream`t$digest`n"
        missing = "F`tREADME.txt`t$('f' * 64)`n"
        mismatch = "F`tREADME.txt`t$('0' * 64)`n"
        duplicate = "F`tREADME.txt`t$digest`nF`treadme.txt`t$digest`n"
        prefix = "F`tx`t$digest`nF`tx\y`t$digest`n"
        malformed = "F`tREADME.txt`t$digest`textra`n"
        base = "B`tREADME.txt`tmissing.txt`n"
        link = "L`tinside-link`t..\outside-fixed\sentinel.txt`n"
        truncated = "F`tREADME.txt`t$digest`n"
    }
    foreach ($label in $rows.Keys) {
        $snapshot = Join-Path $Owned "staging/invalid-$label"
        [IO.Directory]::CreateDirectory((Join-Path $snapshot 'blobs')) | Out-Null
        $blobContent = $payload
        if ($label -eq 'truncated') { $blobContent = [byte[]]::new(0) }
        [IO.File]::WriteAllBytes((Join-Path $snapshot "blobs/$digest"), $blobContent)
        [IO.File]::WriteAllBytes((Join-Path $snapshot "blobs/$('0' * 64)"), $payload)
        [IO.File]::WriteAllText((Join-Path $snapshot 'manifest.tsv'), $rows[$label], [Text.UTF8Encoding]::new($false))
        $name = "hmanifest-$label"; $root = Join-Path $Owned $name
        [IO.Directory]::CreateDirectory($root) | Out-Null
        Admin-Control 'bind' $name $native $snapshot
        $stop = Join-Path $Owned "$name.stop"; $ready = Join-Path $Owned "$name.ready"
        [IO.File]::WriteAllText($stop, 'stop')
        $r = HCommand "manifest-$label" $sparse @('serve', $native, $snapshot, $root, $stop, $ready)
        HAssert ($r.exitCode -ne 0 -and -not (Test-Path -LiteralPath $ready)) "manifest $label refused before exposure"
    }
    $snapshot = Join-Path $Owned 'staging/late-tamper'
    [IO.Directory]::CreateDirectory((Join-Path $snapshot 'blobs')) | Out-Null
    [IO.File]::WriteAllBytes((Join-Path $snapshot "blobs/$digest"), $payload)
    [IO.File]::WriteAllText((Join-Path $snapshot 'manifest.tsv'), "F`tREADME.txt`t$digest`n", [Text.UTF8Encoding]::new($false))
    Start-Provider 'htamper' 'cfapi' $native $snapshot
    try {
        [IO.File]::WriteAllBytes((Join-Path $snapshot "blobs/$digest"), [byte[]]::new($payload.Length))
        $r = HCommand 'tamper-before-hydration' $control @('read', (Join-Path $Owned 'htamper/README.txt'))
        HAssert ($r.exitCode -ne 0) 'served blob rechecked before hydration'
    } finally { Stop-Provider 'htamper' }
    $root = Join-Path $Owned 'hwriter'; Start-Provider 'hwriter' 'cfapi' $native; Stop-Provider 'hwriter'
    [IO.File]::WriteAllText((Join-Path $root 'active.txt'), 'closed')
    $ready = Join-Path $Owned 'writer.ready'
    $writer = Start-Process -FilePath $sparse -ArgumentList @('hold', ('"' + (Join-Path $root 'active.txt') + '"'), 'overwrite', 'open', ('"' + $ready + '"')) -PassThru
    try {
        Wait-File $ready
        HAssert (-not $writer.HasExited) 'actual writer holds handle during capture attempt'
        $refused = $false
        try { [void](HCapture 'hwriter' 'writer-active') } catch { $refused = $true }
        HAssert $refused 'open writer prevents quiescent capture'
    } finally {
        if (-not $writer.HasExited) { $writer.Kill(); [void]$writer.WaitForExit(5000) }
        Admin-Control 'unseal' 'hwriter'
    }
    $r = HCapture 'hwriter' 'writer-retry'
    HAssert ($r.exitCode -eq 0) 'capture retries after killed writer handle closes'
} catch { $hardening.error = $_.ToString() }
finally {
    $summary.phases['hardening-checks'] = if ($hardening.error) { 3 } else { 0 }
    if (-not $hardening.error) { $hardening.verdict = 'VERIFIED' }
    $hardening | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath (Join-Path $Evidence 'hardening-checks.json')
}
