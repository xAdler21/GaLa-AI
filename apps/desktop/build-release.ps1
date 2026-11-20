# Baut den signierten Desktop-Installer (NSIS) für ein Release.
#
#   .\apps\desktop\build-release.ps1            # Version aus apps\desktop\Cargo.toml
#   .\apps\desktop\build-release.ps1 -Version 0.2.0   # Version vorher dort setzen
#
# Signierschlüssel: $env:GALA_UPDATER_KEY oder %USERPROFILE%\.tauri\gala-updater.key
# (ohne Passwort, nicht im Repo; Sicherung siehe apps\desktop\README.md).
# Ergebnis: GaLa-AI_<version>_x64-setup.exe und .sig unter
# %LOCALAPPDATA%\cargo-target\gala-desktop\release\bundle\nsis\
param([string]$Version)
$ErrorActionPreference = 'Stop'

$root = $PSScriptRoot
$cargoToml = Join-Path $root 'Cargo.toml'
if ($Version) {
    if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw "Version muss x.y.z sein, nicht '$Version'" }
    $text = [IO.File]::ReadAllText($cargoToml)
    $new = [regex]::Replace($text, '(?m)^version = "[^"]*"', "version = `"$Version`"", 1)
    [IO.File]::WriteAllText($cargoToml, $new, (New-Object Text.UTF8Encoding $false))
}
$Version = ([regex]::Match([IO.File]::ReadAllText($cargoToml), '(?m)^version = "([^"]*)"')).Groups[1].Value
Write-Host "Desktop-Version $Version"

$key = if ($env:GALA_UPDATER_KEY) { $env:GALA_UPDATER_KEY } else { Join-Path $env:USERPROFILE '.tauri\gala-updater.key' }
if (-not (Test-Path $key)) { throw "Signierschlüssel fehlt: $key" }
$env:TAURI_SIGNING_PRIVATE_KEY = [IO.File]::ReadAllText($key)
$env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = ''
# Das Repo liegt auf einem Netzlaufwerk: Build-Verzeichnis lokal
$env:CARGO_TARGET_DIR = Join-Path $env:LOCALAPPDATA 'cargo-target\gala-desktop'

Push-Location (Join-Path $root 'src-tauri')
try {
    # PowerShell 5.1 wertet cargos Fortschritt auf stderr sonst als Fehler; maßgeblich ist der Exit-Code.
    $ErrorActionPreference = 'Continue'
    cargo tauri build --bundles nsis
    $ErrorActionPreference = 'Stop'
    if ($LASTEXITCODE -ne 0) { throw "cargo tauri build ist fehlgeschlagen ($LASTEXITCODE)" }
} finally {
    Pop-Location
}

$dir = Join-Path $env:CARGO_TARGET_DIR 'release\bundle\nsis'
$exe = Join-Path $dir "GaLa-AI_${Version}_x64-setup.exe"
foreach ($f in $exe, "$exe.sig") {
    if (-not (Test-Path $f)) { throw "Fehlt: $f" }
}
Write-Host "Fertig:"
Write-Host "  $exe"
Write-Host "  $exe.sig"
