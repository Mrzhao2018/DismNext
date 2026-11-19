param(
    [string] $BuildDirectory = (Join-Path $PSScriptRoot '../target/release'),
    [string] $OutputDirectory
)

$ErrorActionPreference = 'Stop'
$build = (Resolve-Path -LiteralPath $BuildDirectory).ProviderPath
$desktop = Join-Path $build 'dism-app.exe'
$cli = Join-Path $build 'dism-cli.exe'
if (!(Test-Path -LiteralPath $desktop -PathType Leaf) -or !(Test-Path -LiteralPath $cli -PathType Leaf)) {
    throw "Build the release desktop and CLI binaries before packaging: $build"
}

$cliVersion = & $cli --version
if ($LASTEXITCODE -ne 0 -or $cliVersion -cnotmatch '^dism-cli (?<version>\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.+-]+)?)$') {
    throw 'Could not read the release version from dism-cli.exe.'
}
$version = $Matches.version
$desktopVersion = & $desktop --version
if ($LASTEXITCODE -ne 0 -or $desktopVersion -cne "dism-app $version") {
    throw "Desktop and CLI version mismatch: '$desktopVersion' and '$cliVersion'."
}

if (!$OutputDirectory) {
    $OutputDirectory = Join-Path $PSScriptRoot "../target/windows-release/v$version"
}
$outputPath = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutputDirectory)
$output = [IO.Directory]::CreateDirectory($outputPath).FullName
$staging = Join-Path $output ".dismnext-package-$([Guid]::NewGuid())"
$portableName = "DismNext-v$version-win64-portable.exe"
$cliName = "dism-cli-v$version-win64.exe"
$bundleName = "DismNext-v$version-win64-bundle.zip"
$manifestName = 'SHA256SUMS.txt'
$assetNames = @($portableName, $cliName, $bundleName)
$bundleInputs = @('DismNext.exe', 'dism-cli.exe', 'README.md', 'mcp_config.json')

try {
    [IO.Directory]::CreateDirectory($staging) | Out-Null
    Copy-Item -LiteralPath $desktop -Destination (Join-Path $staging $portableName)
    Copy-Item -LiteralPath $cli -Destination (Join-Path $staging $cliName)

    Copy-Item -LiteralPath (Join-Path $staging $portableName) -Destination (Join-Path $staging 'DismNext.exe')
    Copy-Item -LiteralPath (Join-Path $staging $cliName) -Destination (Join-Path $staging 'dism-cli.exe')
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot '../README.md') -Destination (Join-Path $staging 'README.md')
    $config = @{
        mcpServers = @{
            dismnext = @{
                command = 'C:/Tools/DismNext/dism-cli.exe'
                args = @('mcp')
            }
        }
    } | ConvertTo-Json -Depth 4
    [IO.File]::WriteAllText((Join-Path $staging 'mcp_config.json'), $config)
    $paths = $bundleInputs | ForEach-Object { Join-Path $staging $_ }
    Compress-Archive -LiteralPath $paths -DestinationPath (Join-Path $staging $bundleName)

    $hashes = foreach ($name in $assetNames) {
        $hash = Get-FileHash -LiteralPath (Join-Path $staging $name) -Algorithm SHA256
        '{0}  {1}' -f $hash.Hash.ToLowerInvariant(), $name
    }
    [IO.File]::WriteAllLines((Join-Path $staging $manifestName), [string[]] $hashes)
    foreach ($name in $assetNames + $manifestName) {
        Move-Item -Force -LiteralPath (Join-Path $staging $name) -Destination (Join-Path $output $name)
    }
    Write-Output "Packaged DismNext v$version in $output."
} finally {
    foreach ($name in $bundleInputs + $assetNames + $manifestName) {
        $file = Join-Path $staging $name
        if ([IO.File]::Exists($file)) { [IO.File]::Delete($file) }
    }
    if ([IO.Directory]::Exists($staging)) {
        [IO.Directory]::Delete($staging, $false)
    }
}
