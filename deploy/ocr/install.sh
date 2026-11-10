#!/usr/bin/env bash
# Einrichtung von gala-ocr (Ubuntu Server 24.04, Docker, NVIDIA-Treiber und
# NVIDIA Container Toolkit bereits installiert).
#   sudo /opt/gala/deploy/ocr/install.sh [--deploy-pubkey <datei>] [--no-start]
#
# --deploy-pubkey: öffentlicher Schlüssel von gala-coordinator
#                  (/etc/gala/deploy_key.pub dort), erlaubt dessen Update-Aufrufe.
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

command -v nvidia-smi >/dev/null 2>&1 \
  || warn "nvidia-smi fehlt: NVIDIA-Treiber nicht installiert (siehe docs/BETRIEB.md)."
docker_info=$(docker info 2>/dev/null || true)
grep -qi nvidia <<<"$docker_info" \
  || warn "Docker kennt keine NVIDIA-Runtime (NVIDIA Container Toolkit: nvidia-ctk runtime configure --runtime=docker)."

log "Pakete installieren"
apt_install jq curl git openssl nfs-common cifs-utils ufw

log "Nutzer, Verzeichnisse"
ensure_user_gala
install -d -m 0755 "$GALA_ETC" "$GALA_LIB" /mnt/archiv /srv/gala/data
printf 'ocr\n' >"$GALA_ETC/role"

log "Konfiguration"
if [ ! -f "$GALA_ETC/ocr.env" ]; then
  install -m 0640 -o root -g root "$SCRIPT_DIR/ocr.env.example" "$GALA_ETC/ocr.env"
  warn "$GALA_ETC/ocr.env angelegt: POSTGRES_PASSWORD (aus coordinator.env) eintragen."
fi
chmod 0640 "$GALA_ETC/ocr.env"
write_hosts_block

log "NFS (Datei-Store) und NAS einbinden"
set_fstab_line /srv/gala/data \
  "gala-coordinator:/srv/gala/data /srv/gala/data nfs4 rw,vers=4.2,hard,noatime,_netdev,nofail,x-systemd.mount-timeout=30 0 0"
if ! mountpoint -q /srv/gala/data; then
  mount /srv/gala/data || warn "NFS-Mount fehlgeschlagen: läuft der Export auf gala-coordinator (install.sh dort)?"
fi
setup_cifs_archiv
docker_after_mounts mnt-archiv.mount srv-gala-data.mount

log "Firewall"
ufw_base
ufw_enable
install_docker_firewall ocr

if [ -n "$PUBKEY" ]; then
  setup_deploy_user ocr "$PUBKEY"
else
  id gala-deploy >/dev/null 2>&1 \
    || warn "Kein Deploy-Schlüssel angegeben: Server-Updates aus dem Admin-Panel erreichen diese VM erst nach 'install.sh --deploy-pubkey <datei>'."
fi

if [ "$START" -eq 1 ]; then
  if grep -q '^POSTGRES_PASSWORD=$' "$GALA_ETC/ocr.env"; then
    warn "POSTGRES_PASSWORD in $GALA_ETC/ocr.env ist leer: Stack nicht gestartet."
  elif ! mountpoint -q /srv/gala/data; then
    warn "/srv/gala/data ist nicht eingebunden: Stack nicht gestartet."
  else
    log "Worker bauen und starten (erster Build dauert einige Minuten)"
    dc ocr up -d --build
  fi
fi
log "gala-ocr eingerichtet. Prüfen: nvidia-smi; docker logs gala-worker-vision"
