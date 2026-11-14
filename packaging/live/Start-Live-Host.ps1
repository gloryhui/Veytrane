$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$mtx = Join-Path $PSScriptRoot 'mediamtx.exe'
$gateway = Join-Path $repo 'target/x86_64-pc-windows-msvc/release/live-gateway.exe'
$data = Join-Path $repo 'live-private/gateway'
if (!(Test-Path $mtx)) { throw 'Extract official MediaMTX v1.21.2 mediamtx.exe into packaging/live first.' }
if (!(Test-Path $gateway)) { throw 'Run Build-Live.ps1 first.' }
if (!(Test-Path (Join-Path $data 'gateway.json'))) { throw 'Run Init-Live.ps1 first.' }
# Keep ownership of this child only; never stop an unrelated MediaMTX instance.
$process = Start-Process -FilePath $mtx -ArgumentList @('mediamtx.yml') -WorkingDirectory $PSScriptRoot -NoNewWindow -PassThru
try {
    Start-Sleep -Milliseconds 800
    if ($process.HasExited) { throw 'MediaMTX exited. Check its configuration and port conflicts.' }
    Write-Host 'OBS: Service WHIP, URL http://127.0.0.1:18891/live/whip, Bearer Token empty.'
    & $gateway --data-dir $data
    if ($LASTEXITCODE -ne 0) { throw 'Live gateway stopped with an error.' }
} finally {
    if (!$process.HasExited) { Stop-Process -Id $process.Id }
}
