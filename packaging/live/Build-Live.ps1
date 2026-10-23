$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$previousEncodedRustFlags = [Environment]::GetEnvironmentVariable('CARGO_ENCODED_RUSTFLAGS', 'Process')
Push-Location $repo
try {
    . (Join-Path $repo 'packaging/windows-x64/desktop-build-env.ps1')
    $vswhere = Find-P2PVsWhere
    if (!$vswhere) { throw 'Install Visual Studio 2022 C++ Build Tools and Windows SDK first.' }
    Initialize-P2PVisualStudioEnvironment -VsWherePath $vswhere | Out-Null
    rustup target add x86_64-pc-windows-msvc
    if ($LASTEXITCODE -ne 0) { throw 'Could not install the Windows MSVC Rust target.' }
    # Friends must not need a separate Visual C++ runtime installation. Apply
    # crt-static to every target dependency, while retaining caller flags and
    # restoring the caller environment even when Cargo fails.
    $flags = @()
    if ($previousEncodedRustFlags) {
        $flags = @($previousEncodedRustFlags -split [char]31)
    } else {
        $callerFlags = $env:RUSTFLAGS
        if (!$callerFlags) { $callerFlags = $env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS }
        if ($callerFlags) { $flags = @($callerFlags -split '\s+' | Where-Object { $_ }) }
    }
    $env:CARGO_ENCODED_RUSTFLAGS = ($flags + @('-C', 'target-feature=+crt-static')) -join [char]31
    cargo build --locked --release --target x86_64-pc-windows-msvc --features live --bin live-gateway --bin live-viewer
    if ($LASTEXITCODE -ne 0) { throw 'Cargo build failed.' }
    Write-Host 'Built target/x86_64-pc-windows-msvc/release/live-gateway.exe and live-viewer.exe'
} finally {
    [Environment]::SetEnvironmentVariable('CARGO_ENCODED_RUSTFLAGS', $previousEncodedRustFlags, 'Process')
    Pop-Location
}
