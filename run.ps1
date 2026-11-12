# ReadChinese (Tauri prototype)
#   .\run.ps1          -> dev run (debug build, console logs)
#   .\run.ps1 build    -> release build + NSIS installer
# Uses the Rust Tauri CLI (cargo install tauri-cli): no node_modules, which Google Drive tends to corrupt.
# Build output goes to %LOCALAPPDATA%\ReadChinese-tauri-target so Google Drive does not sync gigabytes.
param([ValidateSet('dev', 'build')][string]$Mode = 'dev')
$ErrorActionPreference = 'Stop'
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
$env:CARGO_TARGET_DIR = Join-Path $env:LOCALAPPDATA 'ReadChinese-tauri-target'
Set-Location $PSScriptRoot
cargo tauri --version *> $null
if ($LASTEXITCODE -ne 0) {
  Write-Host 'Installing the Tauri CLI (cargo install tauri-cli, one time, a few minutes)...'
  cargo install tauri-cli --version '^2' --locked
  if ($LASTEXITCODE -ne 0) { throw 'Could not install tauri-cli' }
}
cargo tauri $Mode
if ($LASTEXITCODE -ne 0) { throw "cargo tauri $Mode failed (exit $LASTEXITCODE)" }
if ($Mode -eq 'build') {
  Write-Host "Installer: $env:CARGO_TARGET_DIR\release\bundle\nsis" -ForegroundColor Green
}
