#!/usr/bin/env bash
# Einrichtung von gala-coordinator (Ubuntu Server 24.04, Docker + compose-Plugin).
# Als root aus dem Repo-Checkout /opt/gala starten:
#   sudo /opt/gala/deploy/coordinator/install.sh [--no-start]
#
# Idempotent: kann nach Änderungen jederzeit erneut laufen.
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
GALA_REPO=$(cd "$SCRIPT_DIR/../.." && pwd)
# shellcheck source=../common/lib.sh
. "$SCRIPT_DIR/../common/lib.sh"

START=1
for arg in "$@"; do
  case "$arg" in
    --no-start) START=0 ;;
    *) die "Unbekannte Option: $arg" ;;
  esac
done

require_root
[ "$GALA_REPO" = /opt/gala ] \
  || warn "Das Repo liegt in $GALA_REPO, die systemd-Units erwarten /opt/gala."
load_hosts
require_docker

log "Pakete installieren"
apt_install jq curl git openssl rsync openssh-client nfs-kernel-server cifs-utils ufw

log "Nutzer, Verzeichnisse"
ensure_user_gala
install -d -m 0755 "$GALA_ETC" "$GALA_LIB" /var/log/gala /mnt/archiv
install -d -m 0750 -o gala -g gala /srv/gala/data
install -d -m 0755 -o gala -g gala /var/lib/gala /var/lib/gala/state
install -d -m 0755 /var/lib/gala/caddy
install -d -m 0750 /srv/gala/backup
printf 'coordinator\n' >"$GALA_ETC/role"

log "Konfiguration"
if [ ! -f "$GALA_ETC/coordinator.env" ]; then
  install -m 0640 -o root -g root "$SCRIPT_DIR/coordinator.env.example" "$GALA_ETC/coordinator.env"
fi
fill_secret "$GALA_ETC/coordinator.env" POSTGRES_PASSWORD
fill_secret "$GALA_ETC/coordinator.env" GALA_INTERNAL_TOKEN
chmod 0640 "$GALA_ETC/coordinator.env"

write_hosts_block

install -d -m 0755 /etc/exports.d
if vm_present ocr; then
  log "NFS-Export von /srv/gala/data an gala-ocr"
  printf '/srv/gala/data %s(rw,sync,no_subtree_check,root_squash)\n' "$GALA_OCR_IP" \
    >/etc/exports.d/gala.exports
  systemctl enable --now nfs-server >/dev/null
  exportfs -ra
else
  log "gala-ocr fehlt noch: kein NFS-Export (install.sh nach dem Eintragen der IP erneut ausführen)."
  rm -f /etc/exports.d/gala.exports
  if systemctl is-active --quiet nfs-server; then exportfs -ra; fi
fi

log "NAS einbinden (read-only)"
setup_cifs_archiv
docker_after_mounts mnt-archiv.mount

log "Firewall (ufw + DOCKER-USER)"
ufw_base
for net in $(lan_cidrs); do
  ufw_allow_from "$net" 443 'caddy'
done
# 5432 und 8081 sind Docker-Ports: ufw greift dort nicht, siehe docker-firewall.sh.
# Die ufw-Regeln dokumentieren die Absicht und gelten, falls ein Dienst nicht in Docker läuft.
if vm_present ocr; then
  ufw_allow_from "$GALA_OCR_IP" 2049 'nfs fuer gala-ocr'
  ufw_allow_from "$GALA_OCR_IP" 5432 'postgres fuer gala-ocr'
fi
for ip in "$GALA_COORDINATOR_IP" "$GALA_OCR_IP" "$GALA_LLM_IP"; do
  [ "$ip" != none ] || continue
  ufw_allow_from "$ip" 8081 'coordinator intern'
done
ufw_enable
install_docker_firewall coordinator

log "Deploy-Schlüssel für Updates von gala-ocr / gala-llm"
if [ ! -f "$GALA_ETC/deploy_key" ]; then
  ssh-keygen -q -t ed25519 -N '' -C "gala-deploy@gala-coordinator" -f "$GALA_ETC/deploy_key"
fi
chmod 0600 "$GALA_ETC/deploy_key"
chmod 0644 "$GALA_ETC/deploy_key.pub"

log "systemd-Units"
for unit in gala-update.path gala-update.service gala-backup.service gala-backup.timer; do
  install -m 0644 -o root -g root "$SCRIPT_DIR/$unit" "/etc/systemd/system/$unit"
done
systemctl daemon-reload
systemctl enable --now gala-update.path gala-backup.timer

if [ "$START" -eq 1 ]; then
  log "Images bereitstellen und Stack starten"
  /bin/bash "$GALA_REPO/deploy/common/images.sh" coordinator \
    "$(git -c safe.directory="$GALA_REPO" -C "$GALA_REPO" rev-parse HEAD)"
  dc coordinator up -d
  "$SCRIPT_DIR/copy-ca.sh"
else
  log "--no-start: Stack nicht gestartet."
fi

cat <<EOF

[gala] gala-coordinator ist eingerichtet.

Noch zu tun:
  1. Öffentlichen Deploy-Schlüssel auf gala-ocr und gala-llm hinterlegen:
       scp $GALA_ETC/deploy_key.pub <admin>@gala-ocr:/tmp/gala-deploy.pub
       (dort: install.sh --deploy-pubkey /tmp/gala-deploy.pub, siehe docs/BETRIEB.md)
  2. Zugangsdaten für das NAS in $GALA_ETC/cifs-archiv.cred eintragen, dann: mount /mnt/archiv
  3. Admin-Konto anlegen und erstes Desktop-Release hochladen (docs/BETRIEB.md).
EOF
