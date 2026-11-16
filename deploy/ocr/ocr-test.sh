#!/usr/bin/env bash
# OCR-Test auf gala-ocr: erkennt ein Test-PDF mit dem Bench-Image und zeigt, ob die
# Texterkennung läuft und wie lange eine Seite dauert.
#
#   sudo /bin/bash /opt/gala/deploy/ocr/ocr-test.sh [--keep] [<pdf|ordner>...]
#
#   --keep    Bench-Image und Testdateien danach nicht löschen.
#   <pfade>   Eigene PDFs/Scans statt des erzeugten Test-PDFs (z. B. echte LVs).
#
# Voraussetzung: das Worker-Image ist da (update.sh bzw. deploy/common/images.sh).
# Das Bench-Image kommt aus derselben Quelle: aus der Registry, oder es wird hier
# gebaut, wenn die VM ihre Images selbst baut (GALA_IMAGE_SOURCE=build).
# Der laufende Worker wird für die Dauer des Tests angehalten (sonst teilen sich
# beide die Kerne und die Zeit stimmt nicht) und danach wieder gestartet.
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck disable=SC2034 # wird von lib.sh gelesen
GALA_REPO=$(cd "$SCRIPT_DIR/../.." && pwd)
# shellcheck source=../common/lib.sh
. "$SCRIPT_DIR/../common/lib.sh"

KEEP=0
INPUTS=()
while [ $# -gt 0 ]; do
  case "$1" in
    --keep) KEEP=1 ;;
    -*) die "Unbekannte Option: $1" ;;
    *) INPUTS+=("$(readlink -f "$1")") ;;
  esac
  shift
done

require_root
require_docker

# Welche Images gelten, steht in /etc/gala/image.env (deploy/common/images.sh).
IMAGE_REPO=gala
IMAGE_TAG=local
if [ -r "$GALA_ETC/image.env" ]; then
  IMAGE_REPO=$(sed -n 's/^GALA_IMAGE_REPO=//p' "$GALA_ETC/image.env" | tail -n1)
  IMAGE_TAG=$(sed -n 's/^GALA_IMAGE_TAG=//p' "$GALA_ETC/image.env" | tail -n1)
fi
WORKER_IMAGE=${IMAGE_REPO:-gala}/worker-vision:${IMAGE_TAG:-local}
BENCH_IMAGE=${IMAGE_REPO:-gala}/bench:${IMAGE_TAG:-local}
WORK=$(mktemp -d /var/tmp/gala-ocr-test.XXXXXX)
NAME=gala-ocr-test
WORKER_WAS_RUNNING=0

cleanup() {
  docker rm -f "$NAME" >/dev/null 2>&1 || true
  if [ "$WORKER_WAS_RUNNING" -eq 1 ]; then
    dc ocr start worker-vision >/dev/null 2>&1 || warn "Worker ließ sich nicht wieder starten (dc.sh ocr up -d)."
  fi
  if [ "$KEEP" -eq 0 ]; then
    rm -rf "$WORK"
    docker image rm "$BENCH_IMAGE" >/dev/null 2>&1 || true
  else
    log "Behalten: $WORK, Image $BENCH_IMAGE"
  fi
}
trap cleanup EXIT

docker image inspect "$WORKER_IMAGE" >/dev/null 2>&1 \
  || die "Worker-Image $WORKER_IMAGE fehlt. Zuerst: sudo /bin/bash $GALA_REPO/deploy/ocr/update.sh <ref>"

if [ "${IMAGE_REPO:-gala}" = gala ]; then
  # Lokal gebaute Images: auch das Bench-Image hier bauen (dünne Schicht auf dem Worker-Image)
  /bin/bash "$GALA_REPO/deploy/common/disk.sh" check "${GALA_BUILD_MIN_FREE_GB:-4}"
  log "Bench-Image bauen (setzt auf dem Worker-Image auf)"
  docker build -q -f "$GALA_REPO/docker/bench.Dockerfile" \
    --build-arg "WORKER_IMAGE=$WORKER_IMAGE" -t "$BENCH_IMAGE" "$GALA_REPO" >/dev/null
else
  log "Bench-Image laden ($BENCH_IMAGE)"
  docker pull -q "$BENCH_IMAGE" >/dev/null \
    || die "Bench-Image nicht ladbar: $BENCH_IMAGE (baut die CI es für diesen Commit? siehe deploy/common/images.sh)"
fi

# Eingaben: eigene Pfade oder das erzeugte Test-PDF
install -d -m 0755 "$WORK/in"
MOUNTS=()
ARGS=()
if [ ${#INPUTS[@]} -eq 0 ]; then
  command -v python3 >/dev/null 2>&1 || die "python3 fehlt (für das Test-PDF). Alternativ eigene PDFs angeben."
  python3 "$SCRIPT_DIR/mk-test-pdf.py" "$WORK/in/test-lv.pdf"
  chmod 0644 "$WORK/in/test-lv.pdf"
  MOUNTS+=(-v "$WORK/in:/in:ro")
  ARGS+=(/in)
else
  i=0
  for p in "${INPUTS[@]}"; do
    [ -e "$p" ] || die "Nicht gefunden: $p"
    MOUNTS+=(-v "$p:/in/$i/$(basename "$p"):ro")
    ARGS+=("/in/$i")
    i=$((i + 1))
  done
fi

if [ -n "$(dc ocr ps --status running --services 2>/dev/null)" ]; then
  log "Worker für den Test anhalten"
  dc ocr stop worker-vision >/dev/null
  WORKER_WAS_RUNNING=1
fi

log "OCR läuft ($(nproc) Kerne)"
RC=0
docker run --name "$NAME" \
  -e RUST_LOG=info,ort=warn,oar_ocr=warn,oar_ocr_core=warn \
  "${MOUNTS[@]}" -v gala_oar-models:/var/cache/gala/oar \
  "$BENCH_IMAGE" ocr "${ARGS[@]}" --device cpu >"$WORK/ocr.log" 2>&1 || RC=$?

echo
grep -v -e '^$' "$WORK/ocr.log" | tail -n 12 | cut -c1-200
echo

MS=$(sed -n 's/.*OCR Ø \([0-9][0-9]*\) ms\/Seite.*/\1/p' "$WORK/ocr.log" | tail -n1)
PAGES=$(sed -n 's/^Seiten: \([0-9][0-9]*\),.*/\1/p' "$WORK/ocr.log" | tail -n1)

FAIL=""
[ "$RC" -eq 0 ] || FAIL="bench ist mit Code $RC abgebrochen"
[ -n "$FAIL" ] || [ "${PAGES:-0}" -gt 0 ] || FAIL="keine Seite erkannt"

echo "===== Ergebnis ====="
if [ -z "$FAIL" ]; then
  echo "OCR: BESTANDEN – $PAGES Seiten, ${MS:-?} ms/Seite"
  if [ -n "$MS" ] && [ "$MS" -gt 0 ]; then
    echo "Hochrechnung: rund $((3600000 / MS)) Seiten pro Stunde"
  fi
else
  echo "OCR: NICHT BESTANDEN – $FAIL"
  echo "Vollständige Ausgabe mit --keep: $WORK/ocr.log"
fi

[ -z "$FAIL" ]
