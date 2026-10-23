param([string]$OutputDirectory = '')
$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
if (!$OutputDirectory) { $OutputDirectory = Join-Path $repo ('release/live/viewer-' + (Get-Date -Format 'yyyyMMdd-HHmmss')) }
if (Test-Path $OutputDirectory) { throw 'Output directory already exists. Choose a new path.' }
$binary = Join-Path $repo 'target/x86_64-pc-windows-msvc/release/live-viewer.exe'
$profile = Join-Path $repo 'live-private/gateway/live-viewer.json'
if (!(Test-Path $binary) -or !(Test-Path $profile)) { throw 'Build and initialize the host first.' }
New-Item -ItemType Directory -Path $OutputDirectory | Out-Null
Copy-Item $binary (Join-Path $OutputDirectory 'Live-Viewer.exe')
Copy-Item $profile (Join-Path $OutputDirectory 'live-viewer.json')
Copy-Item (Join-Path $PSScriptRoot 'Start-Live-Viewer.bat') $OutputDirectory
Copy-Item (Join-Path $repo 'LICENSE') (Join-Path $OutputDirectory 'LICENSE.txt')
$instructions = @'
双击 Start-Live-Viewer.bat，连接后自动打开浏览器。
请保留观看程序窗口；关闭窗口后停止连接。画面默认静音，点击“开启声音”。
不需要 OBS、MediaMTX、Rust 或管理员权限，不需要登录。
主播未开播时网页会自动等待；断线后自动重试。
live-viewer.json 是观看权限文件，请只转发给希望一起观看的朋友。
本机端口：18889（网页）、18189（媒体）。如果被占用，程序会报错。
'@
Set-Content -LiteralPath (Join-Path $OutputDirectory '使用说明.txt') -Value $instructions -Encoding UTF8
$archive = $OutputDirectory + '.zip'
Compress-Archive -LiteralPath $OutputDirectory -DestinationPath $archive
Write-Host ('Viewer ZIP: ' + $archive)
Get-FileHash -Algorithm SHA256 $archive
