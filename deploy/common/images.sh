#!/usr/bin/env bash
# Stellt die Images einer Rolle für einen Commit bereit und merkt sich, welche
# gelten. Danach genügt `dc.sh <rolle> up -d`.
#
#   deploy/common/images.sh <coordinator|ocr> <commit-sha>
#
# Zwei Quellen (GALA_IMAGE_SOURCE in /etc/gala/hosts.env):
#   registry  (Standard) Die Images baut GitHub Actions bei jedem Push auf main
#             (.github/workflows/images.yml) und legt sie in die Registry. Die VM
#             lädt sie nur herunter: kein Rust-Build, kein Build-Cache auf der VM.
#   build     Die VM baut selbst aus dem Checkout (wie früher). Nötig ohne
#             Internet-Zugang zur Registry oder mit abweichenden Build-Features
#             (z. B. GALA_COORDINATOR_FEATURES leer auf einer CPU ohne AVX).
#
# Weitere Einstellungen in /etc/gala/hosts.env:
#   GALA_IMAGE_REGISTRY   Standard: ghcr.io/<besitzer>/<repo> des git-Remotes origin
#   GALA_IMAGE_WAIT_MIN   So lange auf Images warten, die die CI noch baut (Standard 30)
#
# Ergebnis: /etc/gala/image.env (GALA_IMAGE_REPO, GALA_IMAGE_TAG, GALA_BUILD_REV);
# dc.sh reicht die Datei an docker compose weiter. Sie wird erst geschrieben, wenn
# die Images wirklich da sind.
set -euo pipefail

role=${1:?Aufruf: images.sh <coordinator|ocr> <commit-sha>}
commit=${2:?Aufruf: images.sh <coordinator|ocr> <commit-sha>}
[[ $commit =~ ^[0-9a-f]{40}$ ]] || { echo "[gala] images.sh braucht die volle Commit-SHA, nicht '$commit'." >&2; exit 2; }

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
case "$role" in
  coordinator) services=(coordinator worker-cpu) ;;
  ocr) services=(worker-vision) ;;
  *) echo "[gala] images.sh: unbekannte Rolle '$role' (gala-llm nutzt ein fertiges Image)." >&2; exit 2 ;;
esac

# Wert aus /etc/gala/hosts.env (KEY=VALUE ohne Anführungszeichen), sonst Standard.
setting() { # <KEY> <standard>
  local val
  val=$(sed -n "s/^$1=//p" /etc/gala/hosts.env 2>/dev/null | tail -n1 | tr -d '"'"'"'[:space:]')
  printf '%s\n' "${val:-$2}"
}

# ghcr.io/<besitzer>/<repo> in Kleinbuchstaben, abgeleitet vom GitHub-Remote.
default_registry() {
  local url path
  url=$(git -c safe.directory="$repo" -C "$repo" remote get-url origin 2>/dev/null || true)
  case "$url" in
    *github.com[:/]*)
      path=${url#*github.com}
      path=${path#[:/]}
      path=${path%.git}
      path=${path%/}
      printf 'ghcr.io/%s\n' "${path,,}"
      ;;
    *) printf '\n' ;;
  esac
}

source_mode=$(setting GALA_IMAGE_SOURCE registry)
wait_min=$(setting GALA_IMAGE_WAIT_MIN 30)
registry=$(setting GALA_IMAGE_REGISTRY "$(default_registry)")

write_image_env() { # <repo> <tag>
  local tmp=/etc/gala/.image.env.$$
  printf '# Von deploy/common/images.sh geschrieben (Stand: %s). Nicht von Hand ändern.\nGALA_IMAGE_REPO=%s\nGALA_IMAGE_TAG=%s\nGALA_BUILD_REV=%s\n' \
    "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$1" "$2" "${commit:0:7}" >"$tmp"
  chmod 0644 "$tmp"
  mv -f "$tmp" /etc/gala/image.env
}

export GALA_BUILD_REV=${commit:0:7}

case "$source_mode" in
  build)
    echo "[gala] Images lokal bauen (GALA_IMAGE_SOURCE=build)"
    export GALA_IMAGE_REPO=gala GALA_IMAGE_TAG=local
    "$repo/deploy/common/dc.sh" "$role" build "${services[@]}"
    write_image_env gala local
    ;;
  registry)
    [ -n "$registry" ] || {
      echo "[gala] FEHLER: Registry unbekannt (origin ist kein GitHub-Remote). GALA_IMAGE_REGISTRY in /etc/gala/hosts.env setzen oder GALA_IMAGE_SOURCE=build." >&2
      exit 1
    }
    export GALA_IMAGE_REPO=$registry GALA_IMAGE_TAG=$commit
    echo "[gala] Images laden: $registry/{${services[*]}}:${commit:0:12}"
    deadline=$(($(date +%s) + wait_min * 60))
    while :; do
      if out=$("$repo/deploy/common/dc.sh" "$role" pull --quiet "${services[@]}" 2>&1); then
        break
      fi
      printf '%s\n' "$out" | tail -n 3
      if [ "$(date +%s)" -ge "$deadline" ]; then
        cat >&2 <<EOF
[gala] FEHLER: Images für ${commit:0:12} sind nach $wait_min Minuten nicht ladbar.
[gala] Prüfen:
[gala]   - Ist der Lauf "Images" in GitHub Actions für diesen Commit fertig und grün?
[gala]   - Sind die Pakete unter $registry öffentlich? Sonst auf dieser VM einmal
[gala]     als root: docker login ghcr.io   (Token mit Recht read:packages)
[gala]   - Kein Zugang zur Registry gewollt: GALA_IMAGE_SOURCE=build in /etc/gala/hosts.env
EOF
        exit 1
      fi
      echo "[gala] Noch nicht ladbar (baut die CI noch?). Neuer Versuch in 30 s."
      sleep 30
    done
    write_image_env "$registry" "$commit"
    ;;
  *)
    echo "[gala] FEHLER: GALA_IMAGE_SOURCE='$source_mode' (erlaubt: registry, build)." >&2
    exit 1
    ;;
esac
echo "[gala] Images bereit ($(sed -n 's/^GALA_IMAGE_REPO=//p' /etc/gala/image.env)/…:$(sed -n 's/^GALA_IMAGE_TAG=//p' /etc/gala/image.env | cut -c1-12))."
