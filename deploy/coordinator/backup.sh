#!/usr/bin/env bash
# Tägliches Backup (gala-backup.timer): pg_dump der Datenbank und optional
# ein rsync von /srv/gala/data. Alte Dumps werden nach GALA_BACKUP_KEEP_DAYS gelöscht.
set -euo pipefail

set -a
# shellcheck source=/dev/null
. /etc/gala/coordinator.env
set +a

REPO=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
BACKUP_DIR=${GALA_BACKUP_DIR:-/srv/gala/backup}
KEEP_DAYS=${GALA_BACKUP_KEEP_DAYS:-14}
DB_DIR="$BACKUP_DIR/db"
TS=$(date +%Y%m%d-%H%M%S)
OUT="$DB_DIR/gala-$TS.dump"

install -d -m 0750 "$DB_DIR"

echo "[gala] pg_dump nach $OUT"
# Custom-Format (-Fc): komprimiert, mit pg_restore auch teilweise einspielbar.
# Erst in .tmp schreiben, damit nie ein halber Dump wie ein fertiger aussieht.
"$REPO/deploy/common/dc.sh" coordinator exec -T postgres \
  pg_dump -U "${POSTGRES_USER:-gala}" -d "${POSTGRES_DB:-gala}" -Fc >"$OUT.tmp"
mv "$OUT.tmp" "$OUT"
chmod 0640 "$OUT"

echo "[gala] Dumps älter als $KEEP_DAYS Tage löschen"
find "$DB_DIR" -maxdepth 1 -name 'gala-*.dump' -mtime +"$KEEP_DAYS" -print -delete

if [ -n "${GALA_BACKUP_RSYNC_TARGET:-}" ]; then
  echo "[gala] rsync /srv/gala/data -> $GALA_BACKUP_RSYNC_TARGET"
  # Ohne --delete: versehentlich gelöschte Dateien bleiben im Backup erhalten.
  rsync -a /srv/gala/data/ "${GALA_BACKUP_RSYNC_TARGET%/}/data/"
fi
echo "[gala] Backup fertig."
