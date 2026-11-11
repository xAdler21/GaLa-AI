#!/usr/bin/env bash
# Server-Update (läuft als root, gestartet von gala-update.service, sobald der
# Coordinator $GALA_STATE_DIR/update-request.json geschrieben hat).
#
# Ablauf (Vertrag siehe CLAUDE.md, "Server-Update aus dem Admin-Panel"):
#   1. Anfrage lesen (jq) und löschen, Status "running" schreiben
#   2. git fetch + checkout des Refs in /opt/gala
#   3. gala-coordinator: docker compose build + up -d, Health abwarten, CA kopieren
#   4. gala-ocr per SSH (Nutzer gala-deploy, Forced Command) aktualisieren
#   5. gala-llm nur, wenn sich deploy/llm geändert hat
#   6. Wartungsmodus beenden, falls er für dieses Update angefordert wurde
#   Fehler: Status "failed" mit Meldung und log_tail; der Wartungsmodus bleibt an.
#
# Das Skript kann sich beim git checkout selbst ersetzen. Deshalb steht alles in
# main(), und die letzte Zeile ruft main auf und beendet das Skript sofort
# (bash hat dann die komplette Funktion schon gelesen).
set -Eeuo pipefail

main() {
  local REPO=${GALA_REPO:-/opt/gala}
  local ENV_FILE=/etc/gala/coordinator.env
  local LOG=/var/log/gala/update.log
  local DEPLOY_KEY=/etc/gala/deploy_key
  local KNOWN_HOSTS=/etc/gala/deploy_known_hosts

  install -d -m 0755 /var/log/gala
  exec 9>/run/gala-update.lock
  if ! flock -n 9; then
    echo "Ein Update läuft bereits." >&2
    return 0
  fi

  # Umgebung des Coordinators (GALA_STATE_DIR, GALA_INTERNAL_TOKEN, ...)
  [ -r "$ENV_FILE" ] || { echo "$ENV_FILE fehlt" >&2; return 1; }
  set -a
  # shellcheck source=/dev/null
  . "$ENV_FILE"
  set +a
  local STATE_DIR=${GALA_STATE_DIR:-/var/lib/gala/state}
  local REQ="$STATE_DIR/update-request.json"
  local STATUS="$STATE_DIR/update-status.json"
  local INTERNAL_URL=${GALA_INTERNAL_URL:-http://127.0.0.1:8081}

  [ -f "$REQ" ] || return 0 # nichts zu tun

  # --- Zustand für Status und Fehlerbehandlung -------------------------------
  local REF="" STARTED STEP="Anfrage lesen" WANT_MAINT_OFF=false
  STARTED=$(date -u +%Y-%m-%dT%H:%M:%SZ)

  write_status() { # <state> <message> [finished_at]
    local state=$1 message=$2 finished=${3:-} log_tail="" tmp
    if [ -f "$LOG" ]; then
      log_tail=$(tail -n 80 "$LOG" | tail -c 12000 || true)
    fi
    tmp="$STATE_DIR/.update-status.$$.tmp"
    jq -n --arg state "$state" --arg ref "$REF" --arg started "$STARTED" \
      --arg finished "$finished" --arg message "$message" --arg log_tail "$log_tail" \
      '{state: $state, ref: $ref, started_at: $started,
        finished_at: (if $finished == "" then null else $finished end),
        message: $message, log_tail: $log_tail}' >"$tmp"
    chmod 0644 "$tmp"
    mv -f "$tmp" "$STATUS" # atomar: der Coordinator sieht nie eine halbe Datei
  }

  fail() { # <meldung>
    trap - ERR
    echo "FEHLER: $1" >&2
    write_status failed "$1" "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    exit 1
  }
  trap 'fail "Fehlgeschlagen bei Schritt: $STEP (Zeile $LINENO)"' ERR

  step() {
    STEP=$1
    echo
    echo "=== $(date -u +%H:%M:%S) $STEP ==="
  }

  # --- Anfrage lesen und verbrauchen -----------------------------------------
  local req
  req=$(cat "$REQ")
  rm -f "$REQ"
  [ -f "$LOG" ] && mv -f "$LOG" "$LOG.1"
  exec >"$LOG" 2>&1

  REF=$(jq -r '.ref // empty' <<<"$req")
  REF=${REF:-${GALA_UPDATE_DEFAULT_REF:-main}}
  if [ "$(jq -r '.maintenance // false' <<<"$req")" = "true" ]; then
    WANT_MAINT_OFF=true
  fi
  write_status running "Update läuft"

  echo "Update angefordert von: $(jq -r '.requested_by // "?"' <<<"$req"), ref: $REF, Wartungsmodus: $WANT_MAINT_OFF"
  [[ $REF =~ ^[A-Za-z0-9][A-Za-z0-9._/-]*$ && $REF != *..* ]] \
    || fail "Ungültiger ref: $REF"

  g() { git -c safe.directory="$REPO" -C "$REPO" "$@"; }
  dc() { "$REPO/deploy/common/dc.sh" "$@"; }

  # --- 1. Code holen ----------------------------------------------------------
  step "git fetch"
  local old commit cand
  old=$(g rev-parse --verify --quiet HEAD || echo none)
  g fetch --tags --prune --force origin

  step "Ref $REF auflösen und auschecken"
  commit=""
  for cand in "origin/$REF" "refs/tags/$REF" "$REF"; do
    if commit=$(g rev-parse --verify --quiet "${cand}^{commit}"); then
      break
    fi
    commit=""
  done
  [ -n "$commit" ] || fail "Ref nicht gefunden: $REF"
  g checkout --force --detach "$commit"
  echo "Jetzt auf $(g log -1 --format='%h %s' "$commit")"

  local llm_changed=0
  if [ "$old" = none ] || ! g diff --quiet "$old" "$commit" -- deploy/llm; then
    llm_changed=1
  fi

  # --- 2. Units aktualisieren, falls geändert ----------------------------------
  step "systemd-Units abgleichen"
  local unit units_changed=0
  for unit in gala-update.path gala-update.service gala-backup.service gala-backup.timer; do
    if ! cmp -s "$REPO/deploy/coordinator/$unit" "/etc/systemd/system/$unit"; then
      install -m 0644 -o root -g root "$REPO/deploy/coordinator/$unit" "/etc/systemd/system/$unit"
      units_changed=1
    fi
  done
  if [ "$units_changed" -eq 1 ]; then
    systemctl daemon-reload
  fi

  # --- 3. gala-coordinator ------------------------------------------------------
  step "Plattenplatz prüfen"
  /bin/bash "$REPO/deploy/common/disk.sh" check "${GALA_BUILD_MIN_FREE_GB:-8}" \
    || fail "Zu wenig Plattenplatz für den Build (siehe log_tail)"

  step "docker compose build (coordinator-VM)"
  dc coordinator build

  step "docker compose up -d (coordinator-VM)"
  dc coordinator up -d --remove-orphans

  step "Health-Check des Coordinators"
  local ok=0
  for _ in $(seq 1 60); do
    if curl -fsS --max-time 3 http://127.0.0.1:8080/api/v1/health >/dev/null 2>&1; then
      ok=1
      break
    fi
    sleep 2
  done
  [ "$ok" -eq 1 ] || fail "Coordinator wurde nach dem Update nicht gesund (siehe log_tail)"

  step "Caddy-Root-CA bereitstellen"
  GALA_DATA_DIR=/srv/gala/data "$REPO/deploy/coordinator/copy-ca.sh"

  step "Aufräumen (verwaiste Images, fstrim)"
  /bin/bash "$REPO/deploy/common/disk.sh" cleanup || true

  # --- 4./5. weitere VMs ----------------------------------------------------------
  ssh_update() { # <host>
    [ -r "$DEPLOY_KEY" ] || fail "Deploy-Schlüssel $DEPLOY_KEY fehlt (install.sh erneut ausführen)"
    ssh -i "$DEPLOY_KEY" -o BatchMode=yes -o IdentitiesOnly=yes -o ConnectTimeout=15 \
      -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile="$KNOWN_HOSTS" \
      "gala-deploy@$1" "$commit"
  }

  # VMs, die in hosts.env als "none" stehen, gibt es noch nicht.
  local ocr_ip llm_ip
  ocr_ip=$(sed -n 's/^GALA_OCR_IP=//p' /etc/gala/hosts.env | tail -n1 | tr -d '"'"'"'[:space:]')
  llm_ip=$(sed -n 's/^GALA_LLM_IP=//p' /etc/gala/hosts.env | tail -n1 | tr -d '"'"'"'[:space:]')

  if [ "$ocr_ip" = none ]; then
    echo "gala-ocr: in hosts.env als 'none' eingetragen, übersprungen."
  else
    step "gala-ocr aktualisieren"
    ssh_update gala-ocr
  fi

  if [ "$llm_ip" = none ]; then
    echo "gala-llm: in hosts.env als 'none' eingetragen, übersprungen."
  elif [ "$llm_changed" -eq 1 ]; then
    step "gala-llm aktualisieren (deploy/llm hat sich geändert)"
    ssh_update gala-llm
  else
    echo "gala-llm: deploy/llm unverändert, übersprungen."
  fi

  # --- 6. Wartungsmodus beenden -----------------------------------------------------
  if [ "$WANT_MAINT_OFF" = true ]; then
    step "Wartungsmodus beenden"
    curl -fsS --max-time 10 -X POST \
      -H "X-Gala-Internal-Token: ${GALA_INTERNAL_TOKEN:?GALA_INTERNAL_TOKEN fehlt}" \
      -H 'Content-Type: application/json' -d '{"enabled": false}' \
      "$INTERNAL_URL/internal/maintenance"
    echo
  fi

  step "Fertig"
  trap - ERR
  write_status succeeded "Update auf $(g rev-parse --short "$commit") abgeschlossen" \
    "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
}

main "$@"
exit $?
