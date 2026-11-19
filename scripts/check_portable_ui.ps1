param(
    [Parameter(Mandatory)]
    [string] $Executable
)

$ErrorActionPreference = 'Stop'
$source = (Resolve-Path -LiteralPath $Executable).ProviderPath
$scratch = Join-Path ([IO.Path]::GetTempPath()) "dismnext-portable-$([Guid]::NewGuid())"
$portable = Join-Path $scratch 'dism-app.exe'
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
    Copy-Item -LiteralPath $source -Destination $portable
    $process = Start-Process -FilePath $portable -WorkingDirectory $scratch `
        -ArgumentList @('--no-window', '--port', $port) -PassThru -WindowStyle Hidden
    $baseUrl = "http://127.0.0.1:$port"
    $index = $null
    $deadline = [DateTime]::UtcNow.AddSeconds(20)
    while ([DateTime]::UtcNow -lt $deadline) {
        if ($process.HasExited) { throw "Portable desktop host exited with code $($process.ExitCode)." }
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
    $info = Invoke-RestMethod -Uri "$baseUrl/api/sysinfo" -TimeoutSec 5
    if (!$info.catalog.base_version -or $info.catalog.cleanup_rules_total -lt 1) {
        throw 'The portable EXE could not load the embedded rule catalog.'
    }
    Write-Output 'Standalone EXE served the embedded UI, JavaScript, and catalog.'
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
            if ([IO.File]::Exists($portable)) { [IO.File]::Delete($portable) }
            if ([IO.Directory]::Exists($scratch)) { [IO.Directory]::Delete($scratch, $false) }
            break
        } catch {
            if ([DateTime]::UtcNow -ge $cleanupDeadline) { throw }
            Start-Sleep -Milliseconds 250
        }
    }
}
