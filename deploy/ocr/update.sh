#!/usr/bin/env bash
# Update von gala-ocr. Zwei Wege:
#   - per SSH vom Coordinator: gala-deploy ruft über den Forced Command
#       sudo -n /bin/bash /opt/gala/deploy/ocr/update.sh --ssh
#     auf; der Ziel-Commit (volle oder gekürzte SHA) steht in SSH_ORIGINAL_COMMAND.
#   - von Hand als root: update.sh <ref>
# Ausgabe geht an den Aufrufer zurück (landet im log_tail des Coordinators).
#
# Wie bei coordinator/update.sh: alles in main(), damit git checkout das Skript
# ohne Folgen ersetzen darf.
set -Eeuo pipefail

main() {
  local REPO=${GALA_REPO:-/opt/gala}
  local ref

  if [ "${1:-}" = "--ssh" ]; then
    ref=${SSH_ORIGINAL_COMMAND:-}
    # Über SSH wird ausschließlich eine Commit-SHA akzeptiert
    [[ $ref =~ ^[0-9a-f]{7,40}$ ]] || { echo "Ungültiger Commit: '$ref'" >&2; return 2; }
  else
    ref=${1:?Aufruf: update.sh <ref>}
    [[ $ref =~ ^[A-Za-z0-9][A-Za-z0-9._/-]*$ && $ref != *..* ]] || { echo "Ungültiger ref" >&2; return 2; }
  fi
  [ "$(id -u)" -eq 0 ] || { echo "Muss als root laufen" >&2; return 1; }

  exec 9>/run/gala-update-ocr.lock
  flock -w 600 9

  g() { git -c safe.directory="$REPO" -C "$REPO" "$@"; }

  echo "[gala-ocr] git fetch"
  g fetch --tags --prune --force origin
  local commit
  commit=$(g rev-parse --verify --quiet "${ref}^{commit}" \
    || g rev-parse --verify --quiet "origin/${ref}^{commit}") \
    || { echo "Ref/Commit nicht gefunden: $ref" >&2; return 1; }
  g checkout --force --detach "$commit"
  echo "[gala-ocr] jetzt auf $(g log -1 --format='%h %s' "$commit")"

  echo "[gala-ocr] docker compose build + up"
  "$REPO/deploy/common/dc.sh" ocr build
  "$REPO/deploy/common/dc.sh" ocr up -d --remove-orphans

  # Läuft der Worker nach ein paar Sekunden noch?
  sleep 8
  if [ -z "$("$REPO/deploy/common/dc.sh" ocr ps --status running --services)" ]; then
    echo "[gala-ocr] FEHLER: worker-vision läuft nicht (docker logs gala-worker-vision)" >&2
    return 1
  fi
  echo "[gala-ocr] Update fertig."
}

main "$@"
exit $?
