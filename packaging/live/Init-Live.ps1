param(
    [Parameter(Mandatory=$true)][string]$SignalServer,
    [string]$RelayServer = ''
)
$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$gateway = Join-Path $repo 'target/x86_64-pc-windows-msvc/release/live-gateway.exe'
$data = Join-Path $repo 'live-private/gateway'
if (!(Test-Path $gateway)) { throw 'Run packaging/live/Build-Live.ps1 first.' }
$arguments = @('--data-dir', $data, '--init', '--signal-server', $SignalServer)
if ($RelayServer) { $arguments += @('--relay-server', $RelayServer) }
& $gateway @arguments
if ($LASTEXITCODE -ne 0) { throw 'Initialization failed. Existing identities must be preserved.' }
Write-Host 'Viewer profile generated in live-private/gateway/live-viewer.json. Do not commit it.'
