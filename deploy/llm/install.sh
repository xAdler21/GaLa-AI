#!/usr/bin/env bash
# Einrichtung von gala-llm (Ubuntu Server 24.04, Docker, NVIDIA-Treiber und
# NVIDIA Container Toolkit bereits installiert).
#   sudo /opt/gala/deploy/llm/install.sh [--deploy-pubkey <datei>] [--no-start]
#
# Treiber: Branch 580 ist der letzte, der Pascal (GTX 1070) unterstützt.
# Ubuntu: sudo ubuntu-drivers install nvidia:580   (bzw. Paket nvidia-driver-580-server)
# Keine neueren Branches (590+) installieren, sonst fällt die 1070 aus.
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
GALA_REPO=$(cd "$SCRIPT_DIR/../.." && pwd)
# shellcheck source=../common/lib.sh
. "$SCRIPT_DIR/../common/lib.sh"

START=1
PUBKEY=""
while [ $# -gt 0 ]; do
  case "$1" in
    --no-start) START=0 ;;
    --deploy-pubkey)
      PUBKEY=${2:?--deploy-pubkey braucht einen Dateipfad}
      shift
      ;;
    *) die "Unbekannte Option: $1" ;;
  esac
  shift
done

require_root
load_hosts
require_docker

if command -v nvidia-smi >/dev/null 2>&1; then
  gpus=$(nvidia-smi -L | wc -l)
  [ "$gpus" -eq 2 ] || warn "nvidia-smi sieht $gpus GPU(s), erwartet werden 2 (RTX 3070 + GTX 1070). VM-Passthrough prüfen."
  nvidia-smi --query-gpu=index,pci.bus_id,name,driver_version --format=csv,noheader || true
  drv=$(nvidia-smi --query-gpu=driver_version --format=csv,noheader | head -n1 | cut -d. -f1 || true)
  [ "${drv:-0}" -le 580 ] || warn "Treiber $drv: die GTX 1070 (Pascal) braucht Branch 580 oder älter."
else
  warn "nvidia-smi fehlt: NVIDIA-Treiber (Branch 580) nicht installiert."
fi
docker_info=$(docker info 2>/dev/null || true)
grep -qi nvidia <<<"$docker_info" \
  || warn "Docker kennt keine NVIDIA-Runtime (nvidia-ctk runtime configure --runtime=docker)."

log "Pakete installieren"
apt_install jq curl git ufw

log "Verzeichnisse und Konfiguration"
install -d -m 0755 "$GALA_ETC" "$GALA_LIB" /srv/gala/models
printf 'llm\n' >"$GALA_ETC/role"
if [ ! -f "$GALA_ETC/llm.env" ]; then
  install -m 0644 "$SCRIPT_DIR/llm.env.example" "$GALA_ETC/llm.env"
  warn "$GALA_ETC/llm.env angelegt: LLM_MODEL anpassen, GGUF-Datei nach /srv/gala/models legen."
fi
write_hosts_block

log "Firewall: 8080 nur von gala-coordinator und gala-ocr"
ufw_base
ufw_allow_from "$GALA_COORDINATOR_IP" 8080 'llama-server (coordinator)'
ufw_allow_from "$GALA_OCR_IP" 8080 'llama-server (ocr)'
ufw_enable
# Docker umgeht ufw für veröffentlichte Ports: DOCKER-USER-Regel zusätzlich nötig
install_docker_firewall llm

if [ -n "$PUBKEY" ]; then
  setup_deploy_user llm "$PUBKEY"
else
  id gala-deploy >/dev/null 2>&1 \
    || warn "Kein Deploy-Schlüssel angegeben: Server-Updates erreichen diese VM erst nach 'install.sh --deploy-pubkey <datei>'."
fi

if [ "$START" -eq 1 ]; then
  # shellcheck source=/dev/null
  model=$(. "$GALA_ETC/llm.env" && printf '%s' "${LLM_MODEL:-}")
  if [ -f "/srv/gala/models/$model" ]; then
    log "llama-server starten"
    dc llm up -d
  else
    warn "Modell /srv/gala/models/$model fehlt: Stack nicht gestartet. Danach: /opt/gala/deploy/common/dc.sh llm up -d"
  fi
fi
log "gala-llm eingerichtet. Prüfen: curl http://127.0.0.1:8080/health ; docker logs gala-llama"
