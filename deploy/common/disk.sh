#!/usr/bin/env bash
# Plattenplatz der VM: vor einem Build prüfen, danach aufräumen.
#   deploy/common/disk.sh check <GB>   bricht ab, wenn unter /var/lib/docker weniger frei ist
#   deploy/common/disk.sh cleanup      verwaiste Images löschen, freie Blöcke an den Host melden
#   deploy/common/disk.sh report       Belegung anzeigen
#
# Hintergrund: Die VM-Platten liegen auf dem Hypervisor meist dünn provisioniert
# (thin). Jeder Build schreibt neue Blöcke; gelöschte Daten bekommt der Host erst
# mit fstrim zurück, und nur, wenn an der VM-Platte "Discard" eingeschaltet ist
# (siehe docs/BETRIEB.md, Speicher-Planung). Ohne das läuft der Host voll, obwohl
# in der VM noch Platz frei ist.
set -euo pipefail

docker_root() {
  local dir
  dir=$(docker info --format '{{.DockerRootDir}}' 2>/dev/null || true)
  [ -d "$dir" ] || dir=/var/lib/docker
  [ -d "$dir" ] || dir=/
  printf '%s\n' "$dir"
}

free_gb() { # <verzeichnis>
  df --output=avail -BG "$1" | tail -n1 | tr -dc '0-9'
}

cmd=${1:-report}
case "$cmd" in
  check)
    need=${2:?Aufruf: disk.sh check <GB>}
    dir=$(docker_root)
    free=$(free_gb "$dir")
    if [ "$free" -lt "$need" ]; then
      echo "[gala] FEHLER: nur ${free} GB frei unter $dir, gebraucht werden ${need} GB." >&2
      echo "[gala] Aufräumen: deploy/common/disk.sh cleanup; alten Build-Cache löschen: docker builder prune" >&2
      exit 1
    fi
    echo "[gala] Plattenplatz: ${free} GB frei unter $dir (Minimum ${need} GB)."
    ;;
  cleanup)
    # Nur verwaiste Images (ohne Tag, von keinem Container benutzt): jeder Build
    # hinterlässt das vorherige Image. Der Build-Cache bleibt, sonst kompiliert
    # der nächste Build alles neu.
    docker image prune -f | tail -n1 || true
    # Freie Blöcke an den Hypervisor zurückgeben (wirkt nur mit Discard an der VM-Platte)
    if command -v fstrim >/dev/null 2>&1; then
      fstrim -av 2>&1 | sed 's/^/[gala] fstrim: /' || true
    fi
    ;;
  report)
    df -h "$(docker_root)" | sed 's/^/[gala] /'
    docker system df 2>/dev/null | sed 's/^/[gala] /' || true
    ;;
  *)
    echo "Aufruf: disk.sh <check GB|cleanup|report>" >&2
    exit 2
    ;;
esac
