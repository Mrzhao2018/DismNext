param(
    [Parameter(Mandatory)]
    [string] $Executable,
    [switch] $VerifyCleanup
)

$ErrorActionPreference = 'Stop'

function Invoke-CleanupPost([string] $Uri, [hashtable] $Body, [int] $ExpectedStatus = 200) {
    $response = Invoke-RestMethod -Uri $Uri -Method Post -TimeoutSec 10 `
        -Headers $sessionHeaders -ContentType 'application/json' `
        -Body (ConvertTo-Json -InputObject $Body -Compress) -SkipHttpErrorCheck -StatusCodeVariable status
    if ($status -ne $ExpectedStatus) {
        throw "$Uri returned $status (expected $ExpectedStatus)."
    }
    return $response
}

function Test-CleanupReview([string] $BaseUrl, [string] $Scratch, [hashtable] $Files) {
    $api = "$BaseUrl/api/cleanup"
    $rules = (Invoke-RestMethod -Uri "$api/rules" -Headers $sessionHeaders -TimeoutSec 10).items.rule
    $manualIds = @('clean-003', 'clean-005', 'clean-006', 'clean-007', 'clean-014', 'clean-043', 'clean-051', 'clean-094')
    foreach ($id in $manualIds) {
        $rule = $rules | Where-Object id -CEQ $id
        if (!$rule) {
            # A rule can be absent from the list when its app or directory is missing.
            $rule = Invoke-RestMethod -Uri "$api/preview?id=$id" -Headers $sessionHeaders -TimeoutSec 10
        }
        if ($rule.id -cne $id -or $null -eq $rule.level -or $rule.level -gt 1) {
            throw "Built-in rule $id must remain outside automatic cleanup presets."
        }
    }

    $ruleId = 'custom-release-smoke'
    $selection = @{ rule_ids = @($ruleId) }
    $preview = Invoke-RestMethod -Uri "$api/preview?id=$ruleId" -Headers $sessionHeaders -TimeoutSec 10
    $initialPaths = @($Files.ready, $Files.replace, $Files.lock, $Files.move)
    $previewPaths = @($preview.preview_files | ForEach-Object { [IO.Path]::GetFullPath($_.path, $Scratch) })
    if ($preview.id -cne $ruleId -or $preview.source -cne 'custom' -or
        $preview.cleanable_count -ne $initialPaths.Count -or
        $null -ne (Compare-Object $initialPaths $previewPaths)) {
        throw 'Cleanup test rule must select exactly the four isolated fixture files.'
    }

    $null = Invoke-CleanupPost "$api/execute" @{ rule_ids = @($ruleId); dry_run = $false } 400
    $cancelled = Invoke-CleanupPost "$api/review" $selection
    if (!$cancelled.plan_id) { throw 'The desktop did not create a cleanup plan to cancel.' }
    $cancelledPlan = @{ plan_id = $cancelled.plan_id }
    $discarded = Invoke-CleanupPost "$api/discard-review" $cancelledPlan
    if (!$discarded.ok) { throw 'The desktop did not acknowledge cleanup cancellation.' }
    $null = Invoke-CleanupPost "$api/confirm" $cancelledPlan 409
    foreach ($path in $initialPaths) {
        if ([IO.File]::ReadAllText($path) -cne 'original') {
            throw 'An unconfirmed cleanup modified a fixture file.'
        }
    }

    $prepared = Invoke-CleanupPost "$api/review" $selection
    if (!$prepared.plan_id -or !$prepared.report.dry_run -or
        $prepared.report.total_deleted_files -ne $initialPaths.Count) {
        throw 'The cleanup plan did not review exactly the original four files.'
    }
    $plan = @{ plan_id = $prepared.plan_id }
    [IO.File]::Move($Files.replace, $Files.originalReplacement)
    [IO.File]::Move($Files.move, $Files.originalMoved)
    [IO.File]::WriteAllText($Files.replace, 'replaced')
    [IO.File]::WriteAllText($Files.add, 'new')
    $lock = [IO.File]::Open($Files.lock, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::None)
    try {
        $report = (Invoke-CleanupPost "$api/confirm" $plan).report
    } finally {
        $lock.Dispose()
    }
    $outcome = $report.outcomes | Where-Object id -CEQ $ruleId
    if ($report.dry_run -ne $false -or @($report.outcomes).Count -ne 1 -or
        $report.total_deleted_files -ne 1 -or $report.total_skipped_files -ne 3 -or
        $outcome.actions.permanent_delete.count -ne 1 -or $outcome.failed_files -ne 0 -or
        $outcome.skipped_locked_files -ne 1 -or $outcome.review.skipped_changed -ne 1 -or
        $outcome.review.skipped_missing -ne 1) {
        throw "Unexpected cleanup result: $(ConvertTo-Json -InputObject $report -Depth 8 -Compress)"
    }
    if ([IO.File]::Exists($Files.ready) -or [IO.File]::Exists($Files.move) -or
        [IO.File]::ReadAllText($Files.replace) -cne 'replaced' -or
        [IO.File]::ReadAllText($Files.add) -cne 'new' -or
        [IO.File]::ReadAllText($Files.lock) -cne 'original' -or
        [IO.File]::ReadAllText($Files.originalReplacement) -cne 'original' -or
        [IO.File]::ReadAllText($Files.originalMoved) -cne 'original') {
        throw 'Cleanup did not preserve the expected file contents.'
    }

    [IO.File]::WriteAllText($Files.ready, 'created after cleanup')
    $null = Invoke-CleanupPost "$api/confirm" $plan 409
    if ([IO.File]::ReadAllText($Files.ready) -cne 'created after cleanup') {
        throw 'Confirming a consumed cleanup plan changed a file.'
    }
    Write-Output 'Packaged desktop verified cleanup scope, cancellation, single use, and manual-only rule levels.'
}

$source = (Resolve-Path -LiteralPath $Executable).ProviderPath
$scratch = Join-Path ([IO.Path]::GetTempPath()) "dismnext-portable-$([Guid]::NewGuid())"
$portable = Join-Path $scratch 'dism-app.exe'
$inputDir = Join-Path $scratch 'release-smoke-files'
$dataDir = Join-Path $scratch 'data'
$customRules = Join-Path $dataDir 'custom_rules.json'
$catalogDir = Join-Path $scratch 'catalog'
$implicitCatalog = Join-Path $catalogDir 'rules.json'
$files = @{
    ready = Join-Path $inputDir 'ready.txt'
    replace = Join-Path $inputDir 'replace.txt'
    lock = Join-Path $inputDir 'lock.txt'
    move = Join-Path $inputDir 'move.txt'
    add = Join-Path $inputDir 'added.txt'
    originalReplacement = Join-Path $scratch 'original-replacement.txt'
    originalMoved = Join-Path $scratch 'original-moved.txt'
}
$process = $null
$listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
$listener.Start()
try {
    $port = $listener.LocalEndpoint.Port
} finally {
    $listener.Stop()
}

try {
    [IO.Directory]::CreateDirectory($scratch) | Out-Null
    [IO.Directory]::CreateDirectory($catalogDir) | Out-Null
    [IO.File]::WriteAllText($implicitCatalog, 'An implicit catalog must never be read.')
    if ($VerifyCleanup) {
        [IO.Directory]::CreateDirectory($inputDir) | Out-Null
        [IO.Directory]::CreateDirectory($dataDir) | Out-Null
        Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'fixtures/v2.0.0/custom_rules.json') -Destination $customRules
        foreach ($file in @($files.ready, $files.replace, $files.lock, $files.move)) {
            [IO.File]::WriteAllText($file, 'original')
        }
    }
    Copy-Item -LiteralPath $source -Destination $portable
    $start = [Diagnostics.ProcessStartInfo]::new($portable)
    $start.WorkingDirectory = $scratch
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    foreach ($argument in @('--no-window', '--port', $port)) { $start.ArgumentList.Add([string]$argument) }
    $process = [Diagnostics.Process]::Start($start)
    $startupLine = $process.StandardOutput.ReadLineAsync()
    $baseUrl = "http://127.0.0.1:$port"
    $index = $null
    $sessionHeaders = $null
    $deadline = [DateTime]::UtcNow.AddSeconds(20)
    while ([DateTime]::UtcNow -lt $deadline) {
        if ($process.HasExited) { throw "Portable desktop host exited with code $($process.ExitCode)." }
        if (!$sessionHeaders -and $startupLine.IsCompleted) {
            $line = $startupLine.GetAwaiter().GetResult()
            if ($null -eq $line) { throw 'The portable host closed stdout before announcing a session.' }
            if ($line -match 'DismNext session URL: http://127\.0\.0\.1:\d+/#token=([0-9a-f]{64})') {
                $sessionHeaders = @{ 'X-DismNext-Token' = $Matches[1] }
            } else {
                $startupLine = $process.StandardOutput.ReadLineAsync()
            }
        }
        if (!$sessionHeaders) {
            Start-Sleep -Milliseconds 100
            continue
        }
        try {
            $index = Invoke-WebRequest -Uri "$baseUrl/" -TimeoutSec 2
            break
        } catch {
            Start-Sleep -Milliseconds 250
        }
    }

    if (!$index -or $index.Content -notmatch '<title>[^<]*DismNext' -or
        $index.Content -notmatch '<script\b[^>]*\bsrc="(?<entry>/assets/[^"<>]+\.js)"') {
        throw 'The portable EXE did not serve its embedded DismNext HTML and JavaScript entry.'
    }
    $entry = Invoke-WebRequest -Uri "$baseUrl$($Matches.entry)" -TimeoutSec 5
    if ($entry.Headers['Content-Type'] -notmatch 'javascript' -or $entry.RawContentLength -le 0) {
        throw 'The portable EXE did not serve the embedded JavaScript asset.'
    }
    foreach ($headers in @(
        @{},
        @{ 'X-DismNext-Token' = ('0' * 64) },
        @{ 'X-DismNext-Token' = $sessionHeaders['X-DismNext-Token']; Origin = 'http://127.0.0.1:5173' }
    )) {
        $denied = Invoke-WebRequest -Uri "$baseUrl/api/sysinfo" -Headers $headers -TimeoutSec 5 -SkipHttpErrorCheck
        if ($denied.StatusCode -ne 403) { throw 'The portable API accepted an untrusted request.' }
    }
    $info = Invoke-RestMethod -Uri "$baseUrl/api/sysinfo" -Headers $sessionHeaders -TimeoutSec 5
    if (!$info.catalog.base_version -or $info.catalog.cleanup_rules_total -lt 1) {
        throw 'The portable EXE could not load the embedded rule catalog.'
    }
    $oversized = Invoke-WebRequest -Uri "$baseUrl/api/lockcheck/inspect" -Method Post `
        -Headers $sessionHeaders -ContentType 'application/json' -Body (' ' * (4MB + 1)) `
        -TimeoutSec 10 -SkipHttpErrorCheck
    if ($oversized.StatusCode -ne 413) { throw 'The portable API did not enforce the request body limit.' }
    Write-Output 'Standalone EXE served embedded assets and enforced API session authentication.'
    $job = Invoke-CleanupPost "$baseUrl/api/maintenance/start" @{ kind = 'rescue'; action_id = 'smoke-invalid-action' }
    $deadline = [DateTime]::UtcNow.AddSeconds(10)
    do {
        $status = Invoke-RestMethod -Uri "$baseUrl/api/maintenance/status?id=$($job.id)" -Headers $sessionHeaders -TimeoutSec 5
        if ($status.state -ne 'running') { break }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    if ($status.state -ne 'failed' -or $status.error -notmatch 'Unknown rescue action') {
        throw 'The portable host did not report the maintenance worker failure.'
    }
    Write-Output 'Packaged desktop reported a maintenance worker failure through the job API.'
    if ($VerifyCleanup) {
        Test-CleanupReview $baseUrl $scratch $files
    }
} finally {
    if ($process) {
        if (!$process.HasExited) { Stop-Process -InputObject $process }
        $process.WaitForExit()
        $process.Dispose()
    }
    # Windows utilities started during desktop pre-warm can briefly hold the working directory.
    $cleanupDeadline = [DateTime]::UtcNow.AddSeconds(15)
    while ($true) {
        try {
            foreach ($file in @($portable, $implicitCatalog, $customRules) + @($files.Values)) {
                if ([IO.File]::Exists($file)) { [IO.File]::Delete($file) }
            }
            foreach ($dir in @($inputDir, $dataDir, $catalogDir, $scratch)) {
                if ([IO.Directory]::Exists($dir)) { [IO.Directory]::Delete($dir, $false) }
            }
            break
        } catch {
            if ([DateTime]::UtcNow -ge $cleanupDeadline) { throw }
            Start-Sleep -Milliseconds 250
        }
    }
}
