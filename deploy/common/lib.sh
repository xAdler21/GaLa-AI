#!/usr/bin/env bash
# Gemeinsame Hilfsfunktionen für die install.sh-/update.sh-Skripte.
# Wird mit `source` eingebunden und nicht direkt ausgeführt.
# shellcheck shell=bash

GALA_ETC=/etc/gala
GALA_LIB=/usr/local/lib/gala
GALA_REPO=${GALA_REPO:-/opt/gala}
GALA_UID=10001

log() { printf '[gala] %s\n' "$*"; }
warn() { printf '[gala] WARNUNG: %s\n' "$*" >&2; }
die() {
  printf '[gala] FEHLER: %s\n' "$*" >&2
  exit 1
}

require_root() {
  [ "$(id -u)" -eq 0 ] || die "Bitte als root ausführen (sudo)."
}

_apt_updated=0
apt_install() {
  if [ "$_apt_updated" -eq 0 ]; then
    DEBIAN_FRONTEND=noninteractive apt-get update -qq
    _apt_updated=1
  fi
  DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends "$@"
}

# Docker Engine und das compose-Plugin müssen schon installiert sein.
require_docker() {
  command -v docker >/dev/null 2>&1 \
    || die "Docker Engine fehlt. Installation: https://docs.docker.com/engine/install/ubuntu/"
  docker compose version >/dev/null 2>&1 \
    || die "Das docker-compose-Plugin fehlt (Paket docker-compose-plugin)."
}

# Liest /etc/gala/hosts.env. Fehlt die Datei, wird die Vorlage dorthin kopiert
# und das Skript bricht ab, damit der Betreiber sie ausfüllen kann.
load_hosts() {
  local template="$GALA_REPO/deploy/hosts.env.example"
  if [ ! -r "$GALA_ETC/hosts.env" ]; then
    install -d -m 0755 "$GALA_ETC"
    if [ -r "$template" ]; then
      install -m 0644 "$template" "$GALA_ETC/hosts.env"
    fi
    die "$GALA_ETC/hosts.env fehlt. Vorlage wurde angelegt: bitte IPs, LAN-Netz und NAS-Pfad eintragen und das Skript erneut starten."
  fi
  set -a
  # shellcheck source=/dev/null
  . "$GALA_ETC/hosts.env"
  set +a
  local v val
  for v in GALA_COORDINATOR_IP GALA_OCR_IP GALA_LLM_IP GALA_LAN_CIDR GALA_NAS_SHARE; do
    val=${!v:-}
    [ -n "$val" ] || die "$v ist in $GALA_ETC/hosts.env nicht gesetzt."
    case "$val" in
      192.0.2.* | *NAS-IP*) die "$v in $GALA_ETC/hosts.env ist noch der Platzhalter ($val)." ;;
    esac
  done
  # gala-ocr und gala-llm dürfen noch fehlen (Aufbau in Etappen): Wert "none".
  for v in GALA_COORDINATOR_IP GALA_LAN_CIDR GALA_NAS_SHARE; do
    [ "${!v}" != none ] || die "$v darf nicht 'none' sein."
  done
  vm_present ocr || warn "gala-ocr ist als 'none' eingetragen: OCR/Embeddings laufen noch nicht."
  vm_present llm || warn "gala-llm ist als 'none' eingetragen: der Chatbot antwortet noch nicht."
  GALA_NAS_SMB_VERS=${GALA_NAS_SMB_VERS:-3.0}
}

# vm_present <ocr|llm>: false, wenn die VM in hosts.env als "none" steht.
vm_present() {
  case "$1" in
    ocr) [ "${GALA_OCR_IP:-none}" != none ] ;;
    llm) [ "${GALA_LLM_IP:-none}" != none ] ;;
    *) return 0 ;;
  esac
}

# Trägt gala-coordinator/gala-ocr/gala-llm in /etc/hosts ein (verwalteter Block).
# VMs mit "none" werden ausgelassen.
write_hosts_block() {
  local tmp
  tmp=$(mktemp)
  sed '/^# BEGIN gala-ai$/,/^# END gala-ai$/d' /etc/hosts >"$tmp"
  {
    echo '# BEGIN gala-ai'
    echo "$GALA_COORDINATOR_IP gala-coordinator"
    if vm_present ocr; then echo "$GALA_OCR_IP gala-ocr"; fi
    if vm_present llm; then echo "$GALA_LLM_IP gala-llm"; fi
    echo '# END gala-ai'
  } >>"$tmp"
  # cat statt mv: Rechte und Inode von /etc/hosts bleiben erhalten
  cat "$tmp" >/etc/hosts
  rm -f "$tmp"
  log "/etc/hosts: gala-ai-Block aktualisiert."
}

# Systemnutzer/-gruppe gala mit fester UID/GID. Dieselbe ID steckt in den
# Container-Images, damit Dateien auf Host, NFS und im Container zusammenpassen.
ensure_user_gala() {
  getent group gala >/dev/null || groupadd --system --gid "$GALA_UID" gala
  getent passwd gala >/dev/null \
    || useradd --system --uid "$GALA_UID" --gid gala --home-dir /var/lib/gala \
      --no-create-home --shell /usr/sbin/nologin gala
}

# Eine Zeile in /etc/fstab setzen; vorhandene Zeile mit demselben Mountpunkt
# wird ersetzt (idempotent).
set_fstab_line() {
  local mountpoint=$1 line=$2 tmp
  tmp=$(mktemp)
  awk -v mp="$mountpoint" '/^[[:space:]]*#/ || $2 != mp' /etc/fstab >"$tmp"
  printf '%s\n' "$line" >>"$tmp"
  cat "$tmp" >/etc/fstab
  rm -f "$tmp"
  systemctl daemon-reload
}

# Docker soll erst nach den Daten-Mounts starten (Bind-Mounts in Container).
docker_after_mounts() {
  install -d -m 0755 /etc/systemd/system/docker.service.d
  printf '[Unit]\nAfter=%s\n' "$*" >/etc/systemd/system/docker.service.d/10-gala-mounts.conf
  systemctl daemon-reload
}

# NAS (SMB) read-only nach /mnt/archiv. Zugangsdaten in /etc/gala/cifs-archiv.cred (0600).
setup_cifs_archiv() {
  apt_install cifs-utils
  install -d -m 0755 /mnt/archiv
  local cred="$GALA_ETC/cifs-archiv.cred"
  if [ ! -f "$cred" ]; then
    (
      umask 077
      printf 'username=CHANGE_ME\npassword=CHANGE_ME\ndomain=\n' >"$cred"
    )
    warn "$cred wurde als Vorlage angelegt: bitte NAS-Zugangsdaten eintragen."
  fi
  chown root:root "$cred"
  chmod 0600 "$cred"
  # uid/gid = gala: der Container-Nutzer darf lesen, sonst niemand außer root
  set_fstab_line /mnt/archiv \
    "$GALA_NAS_SHARE /mnt/archiv cifs ro,credentials=$cred,uid=$GALA_UID,gid=$GALA_UID,file_mode=0440,dir_mode=0550,iocharset=utf8,vers=$GALA_NAS_SMB_VERS,_netdev,nofail,x-systemd.mount-timeout=30 0 0"
  if mountpoint -q /mnt/archiv; then
    log "/mnt/archiv ist bereits eingebunden."
  elif grep -q CHANGE_ME "$cred"; then
    warn "NAS nicht eingebunden: Zugangsdaten in $cred fehlen noch (danach: mount /mnt/archiv)."
  elif mount /mnt/archiv; then
    log "NAS unter /mnt/archiv (read-only) eingebunden."
  else
    warn "mount /mnt/archiv ist fehlgeschlagen (Freigabe, Zugangsdaten, SMB-Version prüfen)."
  fi
}

# ufw-Grundkonfiguration: eingehend alles gesperrt, SSH nur aus dem LAN.
ufw_base() {
  apt_install ufw
  ufw default deny incoming >/dev/null
  ufw default allow outgoing >/dev/null
  ufw allow from "$GALA_LAN_CIDR" to any port 22 proto tcp comment 'gala: ssh' >/dev/null
}

ufw_allow_from() { # <quelle> <port> <kommentar>
  ufw allow from "$1" to any port "$2" proto tcp comment "gala: $3" >/dev/null
}

ufw_enable() {
  ufw --force enable >/dev/null
  log "ufw aktiv:"
  ufw status numbered || true
}

# Docker umgeht ufw für veröffentlichte Container-Ports. Deshalb zusätzlich
# eine DOCKER-USER-Kette (siehe docker-firewall.sh), als systemd-Dienst.
install_docker_firewall() { # <rolle>
  local role=$1
  install -d -m 0755 -o root -g root "$GALA_LIB" "$GALA_ETC"
  install -m 0755 -o root -g root "$GALA_REPO/deploy/common/docker-firewall.sh" "$GALA_LIB/docker-firewall.sh"
  install -m 0644 -o root -g root "$GALA_REPO/deploy/common/gala-docker-firewall.service" \
    /etc/systemd/system/gala-docker-firewall.service
  printf '%s\n' "$role" >"$GALA_ETC/role"
  systemctl daemon-reload
  systemctl enable gala-docker-firewall.service >/dev/null
  systemctl restart gala-docker-firewall.service
  log "Docker-Firewall (DOCKER-USER) für Rolle '$role' aktiv."
}

# Nutzer gala-deploy: nur per SSH-Schlüssel des Coordinators erreichbar und
# darf ausschließlich deploy/<rolle>/update.sh --ssh (als root via sudo) starten.
setup_deploy_user() { # <rolle> <pubkey-datei>
  local role=$1 pubfile=$2 pub
  [ -r "$pubfile" ] || die "Öffentlicher Deploy-Schlüssel nicht lesbar: $pubfile"
  pub=$(head -n1 "$pubfile")
  case "$pub" in
    ssh-ed25519\ * | ssh-rsa\ * | ecdsa-sha2-*) ;;
    *) die "$pubfile enthält keinen gültigen öffentlichen SSH-Schlüssel." ;;
  esac
  id gala-deploy >/dev/null 2>&1 || useradd --create-home --shell /bin/bash gala-deploy
  passwd -l gala-deploy >/dev/null 2>&1 || true
  install -d -m 0700 -o gala-deploy -g gala-deploy /home/gala-deploy/.ssh
  # /bin/bash explizit: unabhängig vom Executable-Bit in git
  printf 'restrict,from="%s",command="sudo -n /bin/bash %s/deploy/%s/update.sh --ssh" %s\n' \
    "$GALA_COORDINATOR_IP" "$GALA_REPO" "$role" "$pub" \
    >/home/gala-deploy/.ssh/authorized_keys
  chown gala-deploy:gala-deploy /home/gala-deploy/.ssh/authorized_keys
  chmod 0600 /home/gala-deploy/.ssh/authorized_keys
  local sudoers=/etc/sudoers.d/gala-deploy tmp
  tmp=$(mktemp)
  {
    echo '# von GaLa-AI install.sh verwaltet'
    echo 'Defaults:gala-deploy env_keep += "SSH_ORIGINAL_COMMAND"'
    echo "gala-deploy ALL=(root) NOPASSWD: /bin/bash $GALA_REPO/deploy/$role/update.sh --ssh"
  } >"$tmp"
  visudo -cf "$tmp" >/dev/null || die "sudoers-Datei ungültig, abgebrochen."
  install -m 0440 -o root -g root "$tmp" "$sudoers"
  rm -f "$tmp"
  log "Nutzer gala-deploy eingerichtet (Forced Command: deploy/$role/update.sh --ssh)."
}

# docker compose mit den Umgebungsdateien der Rolle (siehe dc.sh, dort auch
# die Behandlung von "none").
dc() { # <rolle> <compose-args...>
  "$GALA_REPO/deploy/common/dc.sh" "$@"
}

# Setzt KEY= (leer) in einer env-Datei auf einen Zufallswert.
fill_secret() { # <datei> <KEY>
  local file=$1 key=$2
  if grep -q "^${key}=$" "$file"; then
    sed -i "s|^${key}=\$|${key}=$(openssl rand -hex 24)|" "$file"
    log "$key in $file zufällig gesetzt."
  fi
}
