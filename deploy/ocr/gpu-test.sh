#!/usr/bin/env bash
# GPU-Test auf gala-ocr: erkennt ein Test-PDF einmal auf der CPU und einmal auf
# der GPU und zeigt, ob die Grafikkarte wirklich rechnet.
#
#   sudo /bin/bash /opt/gala/deploy/ocr/gpu-test.sh [--apply] [--keep] [<pdf|ordner>...]
#
#   --apply   Ergebnis übernehmen: GALA_OCR_DEVICE in /etc/gala/ocr.env auf cuda
#             (GPU bestanden) oder cpu setzen und den Worker neu starten.
#   --keep    Bench-Image und Testdateien danach nicht löschen.
#   <pfade>   Eigene PDFs/Scans statt des erzeugten Test-PDFs (z. B. echte LVs).
#
# Voraussetzung: das Worker-Image ist da (update.sh bzw. deploy/common/images.sh).
# Das Bench-Image kommt aus derselben Quelle: aus der Registry, oder es wird hier
# gebaut, wenn die VM ihre Images selbst baut (GALA_IMAGE_SOURCE=build).
# Der laufende Worker wird für die Dauer des Tests angehalten (2 GB VRAM reichen
# nicht für zwei Prozesse) und danach wieder gestartet.
#
# "Bestanden" heißt: der CUDA-Provider lädt ohne Fehler, der Prozess belegt
# während des Laufs Grafikspeicher, und die Erkennung liefert Text.
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck disable=SC2034 # wird von lib.sh gelesen
GALA_REPO=$(cd "$SCRIPT_DIR/../.." && pwd)
# shellcheck source=../common/lib.sh
. "$SCRIPT_DIR/../common/lib.sh"

APPLY=0
KEEP=0
INPUTS=()
while [ $# -gt 0 ]; do
  case "$1" in
    --apply) APPLY=1 ;;
    --keep) KEEP=1 ;;
    -*) die "Unbekannte Option: $1" ;;
    *) INPUTS+=("$(readlink -f "$1")") ;;
  esac
  shift
done

require_root
require_docker
command -v nvidia-smi >/dev/null 2>&1 || die "nvidia-smi fehlt: NVIDIA-Treiber nicht installiert."

# Welche Images gelten, steht in /etc/gala/image.env (deploy/common/images.sh).
IMAGE_REPO=gala
IMAGE_TAG=local
if [ -r "$GALA_ETC/image.env" ]; then
  IMAGE_REPO=$(sed -n 's/^GALA_IMAGE_REPO=//p' "$GALA_ETC/image.env" | tail -n1)
  IMAGE_TAG=$(sed -n 's/^GALA_IMAGE_TAG=//p' "$GALA_ETC/image.env" | tail -n1)
fi
WORKER_IMAGE=${IMAGE_REPO:-gala}/worker-vision:${IMAGE_TAG:-local}
BENCH_IMAGE=${IMAGE_REPO:-gala}/bench:${IMAGE_TAG:-local}
WORK=$(mktemp -d /var/tmp/gala-gpu-test.XXXXXX)
WORKER_WAS_RUNNING=0

cleanup() {
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
install -d -m 0755 "$WORK/in" "$WORK/out"
chown "$GALA_UID:$GALA_UID" "$WORK/out"
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

# run_bench <gerät>: Ausgabe nach $WORK/out/<gerät>.log; bei cuda nebenher den
# höchsten vom Prozess belegten Grafikspeicher mitschreiben.
run_bench() {
  local device=$1 name="gala-gpu-test-$1" peak=0 used
  docker rm -f "$name" >/dev/null 2>&1 || true
  docker run -d --name "$name" --gpus all \
    -e RUST_LOG=info,ort=warn,oar_ocr=warn,oar_ocr_core=warn \
    "${MOUNTS[@]}" -v gala_oar-models:/var/cache/gala/oar \
    "$BENCH_IMAGE" ocr "${ARGS[@]}" --device "$device" --batch 2 >/dev/null
  while [ "$(docker inspect -f '{{.State.Running}}' "$name" 2>/dev/null)" = true ]; do
    if [ "$device" = cuda ]; then
      used=$(nvidia-smi --query-compute-apps=used_memory --format=csv,noheader,nounits 2>/dev/null \
        | sort -n | tail -n1 | tr -dc '0-9')
      if [ -n "$used" ] && [ "$used" -gt "$peak" ]; then peak=$used; fi
    fi
    sleep 1
  done
  docker logs "$name" >"$WORK/out/$device.log" 2>&1 || true
  printf '%s\n' "$(docker inspect -f '{{.State.ExitCode}}' "$name")" >"$WORK/out/$device.rc"
  printf '%s\n' "$peak" >"$WORK/out/$device.vram"
  docker rm -f "$name" >/dev/null 2>&1 || true
}

# ms_per_page <gerät>: "OCR Ø 123 ms/Seite" aus der Zusammenfassung
ms_per_page() {
  sed -n 's/.*OCR Ø \([0-9][0-9]*\) ms\/Seite.*/\1/p' "$WORK/out/$1.log" | tail -n1
}
pages() {
  sed -n 's/^Seiten: \([0-9][0-9]*\),.*/\1/p' "$WORK/out/$1.log" | tail -n1
}

log "Lauf 1/2: CPU"
run_bench cpu
log "Lauf 2/2: GPU"
run_bench cuda

echo
for d in cpu cuda; do
  echo "----- $d -----"
  grep -v -e '^$' "$WORK/out/$d.log" | tail -n 8 | cut -c1-200
done
echo

CPU_MS=$(ms_per_page cpu)
GPU_MS=$(ms_per_page cuda)
GPU_PAGES=$(pages cuda)
GPU_RC=$(cat "$WORK/out/cuda.rc")
GPU_VRAM=$(cat "$WORK/out/cuda.vram")

FAIL=""
[ "$GPU_RC" = 0 ] || FAIL="bench ist auf der GPU mit Code $GPU_RC abgebrochen"
# ort meldet einen nicht ladbaren Provider als ERROR und rechnet dann still auf der CPU.
# (Nicht nach "CUDAExecutionProvider" allein suchen: das steht auch in harmlosen Warnungen.)
GPU_ERR=$(grep -m1 -e 'attempting to register' -e 'Failed to load library' -e 'no kernel image' \
  "$WORK/out/cuda.log" | cut -c1-240 || true)
if [ -z "$FAIL" ] && [ -n "$GPU_ERR" ]; then
  FAIL="der CUDA-Provider meldet einen Fehler: $GPU_ERR"
fi
[ -n "$FAIL" ] || [ "${GPU_VRAM:-0}" -gt 0 ] || FAIL="der Prozess hat keinen Grafikspeicher belegt (rechnet auf der CPU)"
[ -n "$FAIL" ] || [ "${GPU_PAGES:-0}" -gt 0 ] || FAIL="keine Seite erkannt"

echo "===== Ergebnis ====="
echo "CPU: ${CPU_MS:-?} ms/Seite"
echo "GPU: ${GPU_MS:-?} ms/Seite, ${GPU_VRAM:-0} MiB Grafikspeicher belegt"
if [ -z "$FAIL" ]; then
  echo "GPU-OCR: BESTANDEN"
  DEVICE=cuda
else
  echo "GPU-OCR: NICHT BESTANDEN – $FAIL"
  echo "Vollständige Ausgabe mit --keep: $WORK/out/cuda.log"
  DEVICE=cpu
fi

if [ "$APPLY" -eq 1 ]; then
  env_file="$GALA_ETC/ocr.env"
  [ -f "$env_file" ] || die "$env_file fehlt (install.sh ausführen)."
  if grep -q '^GALA_OCR_DEVICE=' "$env_file"; then
    sed -i "s/^GALA_OCR_DEVICE=.*/GALA_OCR_DEVICE=$DEVICE/" "$env_file"
  else
    printf 'GALA_OCR_DEVICE=%s\n' "$DEVICE" >>"$env_file"
  fi
  log "GALA_OCR_DEVICE=$DEVICE in $env_file gesetzt, Worker wird neu erstellt"
  WORKER_WAS_RUNNING=0
  dc ocr up -d
else
  echo "Nichts geändert. Mit --apply wird GALA_OCR_DEVICE=$DEVICE gesetzt und der Worker neu gestartet."
fi

[ -z "$FAIL" ]
