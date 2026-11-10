# Erzeugt je VM ein SSH-Schlüsselpaar für den Nutzer `claude` im Schlüsselordner auf dem NAS
# und gibt die Befehle aus, die auf der jeweiligen VM einzufügen sind.
#
#   powershell -ExecutionPolicy Bypass -File deploy\common\bootstrap-claude.ps1
#
# Schlüsselnamen folgen der Konvention im NAS-Ordner: claude_<host>_vm.
# ssh-keygen kann nicht direkt auf die SMB-Freigabe schreiben, deshalb wird lokal erzeugt und kopiert.
# Die privaten Schlüssel haben keine Passphrase, damit Claude Code sie ohne Rückfrage nutzen kann.
# Sie gehören nie ins Repo.
param(
    [string] $KeyDir = 'V:\Programming\#VM-Keys',
    [string[]] $Hosts = @('gala-llm', 'gala-ocr', 'gala-coordinator')
)
$ErrorActionPreference = 'Stop'

New-Item -ItemType Directory -Force $KeyDir | Out-Null
$script = Get-Content -Raw -Encoding UTF8 (Join-Path $PSScriptRoot 'bootstrap-claude.sh')
$script = $script -replace "`r`n", "`n"

foreach ($h in $Hosts) {
    $name = "claude_${h}_vm"
    $key = Join-Path $KeyDir $name
    if (-not (Test-Path -LiteralPath $key)) {
        $tmp = Join-Path $env:TEMP "gala-keygen-$([guid]::NewGuid())"
        New-Item -ItemType Directory $tmp | Out-Null
        # PowerShell 5.1 übergibt '""' als leere Passphrase.
        & ssh-keygen.exe -q -t ed25519 -N '""' -C "claude@$h" -f (Join-Path $tmp $name)
        Copy-Item -LiteralPath (Join-Path $tmp $name), (Join-Path $tmp "$name.pub") -Destination $KeyDir
        Remove-Item -Recurse -Force $tmp
    }
    $pub = (Get-Content -LiteralPath "$key.pub" -Raw).Trim()
    Write-Host ""
    Write-Host "===== Auf $h als Admin einfuegen =====" -ForegroundColor Cyan
    Write-Host "sudo hostnamectl set-hostname $h"
    Write-Host "sudo bash -s -- '$pub' <<'GALA_EOF'"
    Write-Host $script.TrimEnd()
    Write-Host "GALA_EOF"
}
