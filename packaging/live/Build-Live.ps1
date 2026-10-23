$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
Push-Location $repo
try {
    . (Join-Path $repo 'packaging/windows-x64/desktop-build-env.ps1')
    $vswhere = Find-P2PVsWhere
    if (!$vswhere) { throw 'Install Visual Studio 2022 C++ Build Tools and Windows SDK first.' }
    Initialize-P2PVisualStudioEnvironment -VsWherePath $vswhere | Out-Null
    rustup target add x86_64-pc-windows-msvc
    if ($LASTEXITCODE -ne 0) { throw 'Could not install the Windows MSVC Rust target.' }
    cargo build --locked --release --target x86_64-pc-windows-msvc --features live --bin live-gateway --bin live-viewer
    if ($LASTEXITCODE -ne 0) { throw 'Cargo build failed.' }
    Write-Host 'Built target/x86_64-pc-windows-msvc/release/live-gateway.exe and live-viewer.exe'
} finally { Pop-Location }
