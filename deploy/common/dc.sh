#!/usr/bin/env bash
# docker compose für eine Rolle mit den richtigen Umgebungsdateien.
#   deploy/common/dc.sh <coordinator|ocr|llm> <compose-Argumente...>
# Beispiel: deploy/common/dc.sh coordinator logs -f coordinator
set -euo pipefail

role=${1:?Aufruf: dc.sh <coordinator|ocr|llm> <compose-Argumente...>}
shift
case "$role" in
  coordinator | ocr | llm) ;;
  *) echo "Unbekannte Rolle: $role" >&2; exit 2 ;;
esac

# VMs, die es noch nicht gibt, stehen in hosts.env als "none". Compose braucht
# für extra_hosts aber eine IP: dann zeigt der Name auf eine nicht belegte
# Loopback-Adresse, Verbindungen scheitern sofort statt zu hängen.
# Variablen der Shell haben in Compose Vorrang vor --env-file.
for v in GALA_OCR_IP GALA_LLM_IP; do
  val=$(sed -n "s/^${v}=//p" /etc/gala/hosts.env | tail -n1 | tr -d '"'"'"'[:space:]')
  if [ "$val" = none ]; then
    export "$v=127.0.0.2"
  fi
done

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
exec docker compose -p gala \
  --env-file /etc/gala/hosts.env --env-file "/etc/gala/$role.env" \
  -f "$repo/deploy/$role/compose.yml" "$@"
