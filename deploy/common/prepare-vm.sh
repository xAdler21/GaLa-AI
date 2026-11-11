#!/usr/bin/env bash
# Bereitet eine frische Ubuntu-VM für GaLa-AI vor: Docker, bei GPU-VMs zusätzlich
# NVIDIA-Treiber (Branch 580) und NVIDIA Container Toolkit. Danach folgt das
# install.sh der jeweiligen Rolle.
#
#   sudo apt-get install -y git
#   sudo git clone <repo-url> /opt/gala
#   sudo /bin/bash /opt/gala/deploy/common/prepare-vm.sh [--gpu]
#
#   --gpu   für gala-ocr und gala-llm
#
# Idempotent. Vergrößert KEINE Platten oder Volumes: das entscheidet der Betreiber
# mit Blick auf den freien Platz des Hypervisors (docs/BETRIEB.md, Speicher-Planung).
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh
. "$SCRIPT_DIR/lib.sh"

# Letzter Treiberzweig für Maxwell (GTX 950) und Pascal (GTX 1070).
NVIDIA_BRANCH=580
GPU=0
for arg in "$@"; do
  case "$arg" in
    --gpu) GPU=1 ;;
    *) die "Unbekannte Option: $arg" ;;
  esac
done

require_root
REBOOT=0

log "Prüfungen"
# Die ONNX Runtime (Coordinator mit Feature embed, Worker vision) braucht AVX2.
if ! grep -qw avx2 /proc/cpuinfo; then
  warn "Die CPU der VM hat kein AVX2. In Proxmox den CPU-Typ auf 'host' oder 'x86-64-v3' stellen (docs/BETRIEB.md 2.1)."
fi
free_gb=$(df --output=avail -BG / | tail -n1 | tr -dc '0-9')
log "Frei auf /: ${free_gb} GB"
if [ "$GPU" -eq 1 ] && [ "$free_gb" -lt 20 ]; then
  warn "Weniger als 20 GB frei: für Treiber, Images und Modelle wird es eng (docs/BETRIEB.md, Speicher-Planung)."
fi

log "Docker"
apt_install ca-certificates curl gnupg jq git docker.io docker-compose-v2 docker-buildx
systemctl enable --now docker >/dev/null
if id claude >/dev/null 2>&1; then
  usermod --append --groups docker claude
fi

if [ "$GPU" -eq 1 ]; then
  if command -v mokutil >/dev/null 2>&1 && mokutil --sb-state 2>/dev/null | grep -qi enabled; then
    warn "Secure Boot ist an: das NVIDIA-Modul lädt nur mit eingerichtetem MOK (oder Secure Boot ausschalten)."
  fi
  if command -v lspci >/dev/null 2>&1 && ! lspci | grep -qi nvidia; then
    warn "lspci zeigt keine NVIDIA-Karte: PCI-Passthrough der VM prüfen (docs/BETRIEB.md 8.1)."
  fi

  log "NVIDIA-Treiber (Branch $NVIDIA_BRANCH, proprietäres Modul)"
  # Bevorzugt die vorgebauten Module passend zum Kernel; sie kommen mit jedem
  # Kernel-Update mit. Sonst DKMS (baut das Modul auf der VM).
  if apt-cache show "nvidia-headless-no-dkms-$NVIDIA_BRANCH" >/dev/null 2>&1 \
    && apt-cache show "linux-modules-nvidia-$NVIDIA_BRANCH-generic" >/dev/null 2>&1; then
    apt_install "nvidia-headless-no-dkms-$NVIDIA_BRANCH" "linux-modules-nvidia-$NVIDIA_BRANCH-generic" \
      "nvidia-utils-$NVIDIA_BRANCH"
  else
    apt_install "nvidia-headless-$NVIDIA_BRANCH" linux-headers-generic "nvidia-utils-$NVIDIA_BRANCH"
  fi
  # Bewusst kein "apt-mark hold": Kernel-Modul und nvidia-utils müssen dieselbe
  # Version haben und werden gemeinsam aktualisiert. Nur keinen neueren Zweig
  # (590+) installieren, der kennt diese Karten nicht mehr.

  log "NVIDIA Container Toolkit (Paketquelle von NVIDIA)"
  keyring=/usr/share/keyrings/nvidia-container-toolkit-keyring.gpg
  if [ ! -s "$keyring" ]; then
    curl -fsSL https://nvidia.github.io/libnvidia-container/gpgkey | gpg --dearmor -o "$keyring"
  fi
  curl -fsSL https://nvidia.github.io/libnvidia-container/stable/deb/nvidia-container-toolkit.list \
    | sed "s#deb https://#deb [signed-by=$keyring] https://#g" \
      >/etc/apt/sources.list.d/nvidia-container-toolkit.list
  _apt_updated=0
  apt_install nvidia-container-toolkit
  nvidia-ctk runtime configure --runtime=docker >/dev/null
  systemctl restart docker

  # Der Treiber sperrt nouveau erst nach einem Neustart aus.
  if lsmod | grep -q '^nouveau' || ! nvidia-smi >/dev/null 2>&1; then
    REBOOT=1
  else
    nvidia-smi --query-gpu=index,name,memory.total,driver_version --format=csv,noheader
  fi
fi

log "VM vorbereitet."
if [ "$REBOOT" -eq 1 ]; then
  warn "Neustart nötig, damit der NVIDIA-Treiber lädt: sudo reboot"
  warn "Danach prüfen: nvidia-smi   und   docker run --rm --gpus all nvidia/cuda:12.8.1-base-ubuntu24.04 nvidia-smi"
fi
log "Weiter mit /etc/gala/hosts.env und deploy/<rolle>/install.sh (docs/BETRIEB.md 2.3, 2.4)."
