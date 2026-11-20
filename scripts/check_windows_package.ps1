param(
    [Parameter(Mandatory)]
    [string] $PackageDirectory
)

$ErrorActionPreference = 'Stop'
$package = (Resolve-Path -LiteralPath $PackageDirectory).ProviderPath
$manifest = Join-Path $package 'SHA256SUMS.txt'
$hashes = @{}
foreach ($line in Get-Content -LiteralPath $manifest) {
    if ($line -notmatch '^(?<hash>[0-9a-f]{64})  (?<file>[^\\/]+)$') {
        throw "Invalid SHA256SUMS.txt entry: $line"
    }
    if ($hashes.ContainsKey($Matches.file)) { throw "Duplicate checksum for $($Matches.file)" }
    $hashes[$Matches.file] = $Matches.hash
}

$portableNames = @($hashes.Keys | Where-Object { $_ -like 'DismNext-v*-win64-portable.exe' })
if ($portableNames.Count -ne 1 -or
    $portableNames[0] -cnotmatch '^DismNext-v(?<version>\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.+-]+)?)-win64-portable\.exe$') {
    throw 'Expected one versioned portable EXE in the manifest.'
}
$version = $Matches.version
$portableName = $portableNames[0]
$cliName = "dism-cli-v$version-win64.exe"
$bundleName = "DismNext-v$version-win64-bundle.zip"
if ($hashes.Count -ne 3) { throw 'Expected exactly three payload checksums.' }
foreach ($name in @($portableName, $cliName, $bundleName)) {
    $hash = Get-FileHash -LiteralPath (Join-Path $package $name) -Algorithm SHA256
    if ($hash.Hash -ne $hashes[$name]) { throw "SHA-256 mismatch for $name." }
}

$archive = [IO.Compression.ZipFile]::OpenRead((Join-Path $package $bundleName))
try {
    $expected = @('DismNext.exe', 'dism-cli.exe', 'README.md', 'mcp_config.json')
    if ($null -ne (Compare-Object $expected @($archive.Entries.FullName) -CaseSensitive)) {
        throw 'The bundle contents differ from the expected release files.'
    }
    $executables = @{ 'DismNext.exe' = $portableName; 'dism-cli.exe' = $cliName }
    foreach ($name in $executables.Keys) {
        $stream = $archive.GetEntry($name).Open()
        try {
            $hash = Get-FileHash -InputStream $stream -Algorithm SHA256
            if ($hash.Hash -ne $hashes[$executables[$name]]) {
                throw "Bundle $name differs from the standalone executable."
            }
        } finally {
            $stream.Dispose()
        }
    }
    $reader = [IO.StreamReader]::new($archive.GetEntry('mcp_config.json').Open())
    try {
        $server = ($reader.ReadToEnd() | ConvertFrom-Json).mcpServers.dismnext
        if ($server.command -cne 'C:/Tools/DismNext/dism-cli.exe' -or ($server.args -join ' ') -cne 'mcp') {
            throw 'Unexpected MCP configuration in bundle.'
        }
    } finally {
        $reader.Dispose()
    }
} finally {
    $archive.Dispose()
}

$portable = Join-Path $package $portableName
$desktopVersion = & $portable --version
if ($LASTEXITCODE -ne 0 -or $desktopVersion -cne "dism-app $version") {
    throw 'The portable EXE version differs from its release filename.'
}
& (Join-Path $PSScriptRoot 'check_portable_ui.ps1') -Executable $portable -VerifyCleanup

$scratch = Join-Path ([IO.Path]::GetTempPath()) "dismnext-release-check-$([Guid]::NewGuid())"
$cli = Join-Path $scratch $cliName
$inputDir = Join-Path $scratch 'release-smoke-files'
$inputFile = Join-Path $inputDir 'keep.txt'
$data = Join-Path $scratch 'data'
$snapshots = Join-Path $data 'snapshots'
$customRules = Join-Path $data 'custom_rules.json'
$snapshot = Join-Path $snapshots 'release-smoke-snapshot.json'
$fixtures = Join-Path $PSScriptRoot 'fixtures/v2.0.0'
$fallbackInstall = Join-Path $scratch 'blocked-portable'
$fallbackCli = Join-Path $fallbackInstall $cliName
$blockedData = Join-Path $fallbackInstall 'data'
$profile = Join-Path $scratch 'profile'
$profileData = Join-Path $profile 'DismNext'
$profileSnapshots = Join-Path $profileData 'snapshots'
$profileSnapshot = Join-Path $profileSnapshots 'release-smoke-snapshot.json'
try {
    [IO.Directory]::CreateDirectory($snapshots) | Out-Null
    [IO.Directory]::CreateDirectory($inputDir) | Out-Null
    Copy-Item -LiteralPath (Join-Path $package $cliName) -Destination $cli
    Copy-Item -LiteralPath (Join-Path $fixtures 'custom_rules.json') -Destination $customRules
    Copy-Item -LiteralPath (Join-Path $fixtures 'release-smoke-snapshot.json') -Destination $snapshot
    [IO.File]::WriteAllText($inputFile, 'Release smoke test file')

    $originalLocalAppData = $env:LOCALAPPDATA
    Push-Location -LiteralPath $scratch
    try {
        $env:LOCALAPPDATA = $profile
        $cliVersion = & $cli --version
        if ($LASTEXITCODE -ne 0 -or $cliVersion -cne "dism-cli $version") {
            throw 'The CLI version differs from its release filename.'
        }
        $overview = & $cli info --json | ConvertFrom-Json
        if ($LASTEXITCODE -ne 0 -or !$overview.catalog.base_version -or $overview.catalog.cleanup_rules -lt 1) {
            throw 'The standalone CLI could not load the embedded catalog.'
        }
        $preview = & $cli preview custom-release-smoke --json | ConvertFrom-Json
        if ($LASTEXITCODE -ne 0 -or $preview.id -cne 'custom-release-smoke' -or $preview.cleanable_count -ne 1) {
            throw 'Could not preview the v2.0.0 custom rule.'
        }
        $history = @(& $cli snapshot list --json | ConvertFrom-Json)
        if ($LASTEXITCODE -ne 0 -or $history.Count -ne 1 -or $history[0].id -cne 'release-smoke-snapshot' -or
            $history[0].records[0].previous.vtype -cne 'REG_DWORD') {
            throw 'Could not read the v2.0.0 registry snapshot.'
        }
        $messages = @(
            '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"release-smoke","version":"1"}}}',
            '{"jsonrpc":"2.0","method":"notifications/initialized"}',
            '{"jsonrpc":"2.0","id":2,"method":"tools/list"}'
        )
        $replies = @($messages | & $cli mcp | ForEach-Object { $_ | ConvertFrom-Json })
        if ($LASTEXITCODE -ne 0) { throw 'The MCP process exited with an error.' }
        $init = $replies | Where-Object id -eq 1
        $tools = $replies | Where-Object id -eq 2
        if ($init.result.protocolVersion -cne '2025-03-26' -or
            'dismnext_system_overview' -cnotin $tools.result.tools.name) {
            throw 'The packaged CLI failed the MCP initialize or tools/list check.'
        }
    } finally {
        $env:LOCALAPPDATA = $originalLocalAppData
        Pop-Location
    }

    [IO.Directory]::CreateDirectory($fallbackInstall) | Out-Null
    [IO.Directory]::CreateDirectory($profileSnapshots) | Out-Null
    Copy-Item -LiteralPath $cli -Destination $fallbackCli
    Copy-Item -LiteralPath $snapshot -Destination $profileSnapshot
    [IO.File]::WriteAllText($blockedData, 'Portable data directory unavailable.')
    Push-Location -LiteralPath $fallbackInstall
    try {
        $env:LOCALAPPDATA = $profile
        $history = @(& $fallbackCli snapshot list --json | ConvertFrom-Json)
        if ($LASTEXITCODE -ne 0 -or $history.Count -ne 1 -or $history[0].id -cne 'release-smoke-snapshot') {
            throw 'The standalone CLI did not fall back to the user data directory.'
        }
    } finally {
        $env:LOCALAPPDATA = $originalLocalAppData
        Pop-Location
    }
    Write-Output "Verified DismNext v$version release files, CLI, MCP, and v2.0.0 data."
} finally {
    foreach ($file in @($cli, $inputFile, $customRules, $snapshot, $fallbackCli, $blockedData, $profileSnapshot)) {
        if ([IO.File]::Exists($file)) { [IO.File]::Delete($file) }
    }
    foreach ($dir in @($inputDir, $snapshots, $data, $fallbackInstall, $profileSnapshots, $profileData, $profile, $scratch)) {
        if ([IO.Directory]::Exists($dir)) { [IO.Directory]::Delete($dir, $false) }
    }
}
