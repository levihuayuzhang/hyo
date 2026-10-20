# Development-only cargo wrapper for this machine.
# The Windows Schannel stack cannot create TLS credentials in this session, so
# cargo is pointed at tools/crates_proxy.py (Python/OpenSSL) instead of
# crates.io directly. Normal machines can just run `cargo`.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot

try {
    Invoke-WebRequest -UseBasicParsing -TimeoutSec 2 http://127.0.0.1:8765/health | Out-Null
} catch {
    Write-Host 'starting local crates proxy...'
    Start-Process -FilePath 'python' `
        -ArgumentList (Join-Path $root 'tools\crates_proxy.py') `
        -WorkingDirectory $root -WindowStyle Hidden
    Start-Sleep -Milliseconds 800
}

$configPath = Join-Path $root '.cache\cargo-local.toml'
if (-not (Test-Path $configPath)) {
    New-Item -ItemType Directory -Force (Split-Path $configPath) | Out-Null
    @'
[source.crates-io]
replace-with = "local"

[source.local]
registry = "sparse+http://127.0.0.1:8765/"
'@ | Set-Content -Path $configPath -Encoding utf8
}

$env:CARGO_HOME = Join-Path $root '.cache\cargo-home'
New-Item -ItemType Directory -Force $env:CARGO_HOME | Out-Null

Push-Location $root
try {
    & cargo --config $configPath @args
    exit $LASTEXITCODE
} finally {
    Pop-Location
}
