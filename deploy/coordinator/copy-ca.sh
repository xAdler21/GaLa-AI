#!/usr/bin/env bash
# Kopiert die Root-CA von Caddy nach $GALA_DATA_DIR/ca.crt, damit der
# Coordinator sie auf der Startseite zum Download anbieten kann.
# Idempotent; wartet bis zu 90 s, bis Caddy die CA beim ersten Start erzeugt hat.
set -euo pipefail

CADDY_DATA=${CADDY_DATA:-/var/lib/gala/caddy}
DATA_DIR=${GALA_DATA_DIR:-/srv/gala/data}
SRC="$CADDY_DATA/caddy/pki/authorities/local/root.crt"
DST="$DATA_DIR/ca.crt"

for _ in $(seq 1 45); do
  [ -s "$SRC" ] && break
  sleep 2
done
if [ ! -s "$SRC" ]; then
  echo "[gala] FEHLER: Root-CA nicht gefunden: $SRC (läuft der Caddy-Container?)" >&2
  exit 1
fi

if cmp -s "$SRC" "$DST" 2>/dev/null; then
  echo "[gala] ca.crt ist aktuell."
else
  install -m 0644 -o gala -g gala "$SRC" "$DST"
  echo "[gala] Root-CA nach $DST kopiert."
fi
