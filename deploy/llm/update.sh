#!/usr/bin/env bash
# Update von gala-llm (Aufrufweg wie bei deploy/ocr/update.sh).
#   per SSH:  sudo -n /bin/bash /opt/gala/deploy/llm/update.sh --ssh  (Commit in SSH_ORIGINAL_COMMAND)
#   von Hand: update.sh <ref>
set -Eeuo pipefail

main() {
  local REPO=${GALA_REPO:-/opt/gala}
  local ref

  if [ "${1:-}" = "--ssh" ]; then
    ref=${SSH_ORIGINAL_COMMAND:-}
    [[ $ref =~ ^[0-9a-f]{7,40}$ ]] || { echo "Ungültiger Commit: '$ref'" >&2; return 2; }
  else
    ref=${1:?Aufruf: update.sh <ref>}
    [[ $ref =~ ^[A-Za-z0-9][A-Za-z0-9._/-]*$ && $ref != *..* ]] || { echo "Ungültiger ref" >&2; return 2; }
  fi
  [ "$(id -u)" -eq 0 ] || { echo "Muss als root laufen" >&2; return 1; }

  exec 9>/run/gala-update-llm.lock
  flock -w 600 9

  g() { git -c safe.directory="$REPO" -C "$REPO" "$@"; }

  echo "[gala-llm] git fetch"
  g fetch --tags --prune --force origin
  local commit
  commit=$(g rev-parse --verify --quiet "${ref}^{commit}" \
    || g rev-parse --verify --quiet "origin/${ref}^{commit}") \
    || { echo "Ref/Commit nicht gefunden: $ref" >&2; return 1; }
  g checkout --force --detach "$commit"
  echo "[gala-llm] jetzt auf $(g log -1 --format='%h %s' "$commit")"

  echo "[gala-llm] docker compose pull + up"
  "$REPO/deploy/common/dc.sh" llm pull
  "$REPO/deploy/common/dc.sh" llm up -d --remove-orphans

  # Modell laden dauert; /health antwortet 503, bis es fertig ist.
  local i
  for i in $(seq 1 60); do
    if curl -fsS --max-time 3 http://127.0.0.1:8080/health >/dev/null 2>&1; then
      echo "[gala-llm] llama-server ist bereit."
      return 0
    fi
    sleep 5
  done
  echo "[gala-llm] FEHLER: llama-server wurde nicht bereit (docker logs gala-llama)" >&2
  return 1
}

main "$@"
exit $?
