#!/usr/bin/env bash
# Installiert den schaltbaren SSH-Zugang für den Nutzer "claude" (pro VM).
#   sudo /opt/gala/deploy/common/claude-access/install.sh
#   sudo /opt/gala/deploy/common/claude-access/install.sh --uninstall
#
# Voraussetzung: Der Nutzer "claude" existiert (bootstrap-claude.sh) und hat
# seine Schlüssel in ~claude/.ssh/authorized_keys. Diese werden nach
# /etc/gala/claude_authorized_keys kopiert (root:root 0644); danach liefert
# sshd sie nur noch über claude-keys aus, wenn der Admin-Schalter an ist.
#
# Betrifft ausschließlich den Nutzer claude. Andere SSH-Zugänge bleiben unberührt.
# Die sshd-Konfiguration wird mit `sshd -t` geprüft, BEVOR sshd neu geladen wird.
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
LIB_DIR=/usr/local/lib/gala
ETC_DIR=/etc/gala
DROPIN=/etc/ssh/sshd_config.d/50-gala-claude.conf
KEYS_DST=$ETC_DIR/claude_authorized_keys
CONF=$ETC_DIR/claude-access.conf

log() { printf '[gala] %s\n' "$*"; }
warn() { printf '[gala] WARNUNG: %s\n' "$*" >&2; }
die() {
  printf '[gala] FEHLER: %s\n' "$*" >&2
  exit 1
}

[ "$(id -u)" -eq 0 ] || die "Bitte als root ausführen (sudo)."

reload_sshd() {
  if ! sshd -t; then
    return 1
  fi
  if systemctl list-unit-files ssh.service >/dev/null 2>&1 && systemctl cat ssh.service >/dev/null 2>&1; then
    systemctl reload ssh
  else
    systemctl reload sshd
  fi
}

# --- Deinstallation ---------------------------------------------------------------
if [ "${1:-}" = "--uninstall" ]; then
  log "Deinstallation des schaltbaren claude-Zugangs"
  systemctl disable --now gala-claude-reaper.timer 2>/dev/null || true
  rm -f /etc/systemd/system/gala-claude-reaper.timer /etc/systemd/system/gala-claude-reaper.service
  systemctl daemon-reload
  rm -f "$DROPIN"
  if reload_sshd; then
    log "sshd neu geladen."
  else
    die "sshd -t meldet Fehler nach dem Entfernen; bitte /etc/ssh/sshd_config.d prüfen (sshd NICHT neu geladen)."
  fi
  rm -f "$LIB_DIR/claude-keys" "$LIB_DIR/claude-reaper" "$KEYS_DST"
  warn "Der Nutzer claude ist wieder über ~claude/.ssh/authorized_keys erreichbar (sofern vorhanden), ohne Admin-Schalter."
  warn "$CONF wurde behalten (FAIL_MODE); bei Bedarf von Hand löschen."
  exit 0
fi

# --- Installation -------------------------------------------------------------------
id claude >/dev/null 2>&1 || die "Nutzer 'claude' existiert nicht. Zuerst bootstrap-claude.sh ausführen."
command -v curl >/dev/null 2>&1 || die "curl fehlt (apt install curl)."
command -v jq >/dev/null 2>&1 || warn "jq fehlt: claude-keys nutzt dann den grep-Fallback (apt install jq empfohlen)."
command -v sshd >/dev/null 2>&1 || [ -x /usr/sbin/sshd ] || die "sshd nicht gefunden (openssh-server installieren)."
PATH=$PATH:/usr/sbin

# 1. Schlüssel übernehmen
claude_home=$(getent passwd claude | cut -d: -f6)
src_keys="$claude_home/.ssh/authorized_keys"
install -d -m 0755 -o root -g root "$ETC_DIR" "$LIB_DIR"
if [ -s "$src_keys" ]; then
  install -m 0644 -o root -g root "$src_keys" "$KEYS_DST"
  log "Schlüssel von $src_keys nach $KEYS_DST kopiert."
elif [ -s "$KEYS_DST" ]; then
  log "$src_keys fehlt/leer, vorhandene $KEYS_DST bleibt unverändert."
else
  die "Keine Schlüssel gefunden ($src_keys). Erst bootstrap-claude.sh ausführen."
fi
chown root:root "$KEYS_DST"
chmod 0644 "$KEYS_DST"

# 2. Skripte (root-eigen, in root-eigenen Verzeichnissen: sshd verlangt das)
install -m 0755 -o root -g root "$SCRIPT_DIR/claude-keys" "$LIB_DIR/claude-keys"
install -m 0755 -o root -g root "$SCRIPT_DIR/claude-reaper" "$LIB_DIR/claude-reaper"
if [ ! -f "$CONF" ]; then
  printf '# FAIL_MODE: Verhalten, wenn gala-coordinator nicht erreichbar ist\n#   closed = kein Zugang (Standard), open = Zugang erlaubt\nFAIL_MODE=closed\n' >"$CONF"
  chmod 0644 "$CONF"
fi

# 3. sshd-Drop-in; bei ungültiger Konfiguration sofort zurückrollen
install -d -m 0755 /etc/ssh/sshd_config.d
had_dropin=0
[ -f "$DROPIN" ] && { cp -a "$DROPIN" "$DROPIN.bak"; had_dropin=1; }
install -m 0644 -o root -g root "$SCRIPT_DIR/50-gala-claude.conf" "$DROPIN"
if ! sshd -t; then
  warn "sshd -t meldet Fehler: Änderung wird zurückgenommen, sshd bleibt unverändert."
  if [ "$had_dropin" -eq 1 ]; then mv -f "$DROPIN.bak" "$DROPIN"; else rm -f "$DROPIN"; fi
  die "sshd-Konfiguration ungültig (prüfe, ob /etc/ssh/sshd_config die Datei sshd_config.d/*.conf per Include einbindet)."
fi
rm -f "$DROPIN.bak"
# Kontrolle: greift der Drop-in für claude (und nur dafür)?
cfg_claude=$(sshd -T -C user=claude,host=localhost,addr=127.0.0.1 2>/dev/null || true)
cfg_root=$(sshd -T -C user=root,host=localhost,addr=127.0.0.1 2>/dev/null || true)
if grep -qi '^authorizedkeyscommand .*claude-keys' <<<"$cfg_claude"; then
  log "sshd-Check: für 'claude' gilt AuthorizedKeysCommand."
else
  warn "sshd -T zeigt keinen AuthorizedKeysCommand für claude: wird die Datei per Include geladen?"
fi
if grep -qi '^authorizedkeyscommand .*claude-keys' <<<"$cfg_root"; then
  warn "AuthorizedKeysCommand gilt auch für andere Nutzer! Match-Block in $DROPIN prüfen."
fi
reload_sshd || die "sshd konnte nicht neu geladen werden."

# 4. Reaper-Timer
install -m 0644 -o root -g root "$SCRIPT_DIR/gala-claude-reaper.service" /etc/systemd/system/gala-claude-reaper.service
install -m 0644 -o root -g root "$SCRIPT_DIR/gala-claude-reaper.timer" /etc/systemd/system/gala-claude-reaper.timer
systemctl daemon-reload
systemctl enable --now gala-claude-reaper.timer

if ! getent hosts gala-coordinator >/dev/null 2>&1; then
  warn "gala-coordinator ist nicht auflösbar: claude-keys findet den Coordinator nicht (/etc/hosts bzw. deploy/<vm>/install.sh)."
fi

cat <<EOF

[gala] Schaltbarer claude-Zugang ist aktiv.

  WICHTIG: Ab jetzt ist der SSH-Login von 'claude' NUR möglich, solange der
  Schalter "Claude-Zugang" im Admin-Panel an ist (Standard: AUS). Laufende
  claude-Sitzungen werden bei ausgeschaltetem Schalter innerhalb von ca. 30 s
  beendet - auch die Sitzung, aus der dieses Skript evtl. gestartet wurde.

  Der Admin-Zugang des Betreibers (eigener Nutzer) ist nicht betroffen.
  Verhalten bei nicht erreichbarem Coordinator: $CONF (FAIL_MODE=closed|open).
  Entfernen: $0 --uninstall
EOF
