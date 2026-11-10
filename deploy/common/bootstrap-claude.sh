#!/usr/bin/env bash
# Legt auf einer VM den Nutzer `claude` mit sudo (ohne Passwort) und SSH-Schlüssel an.
#
# Aufruf als root bzw. mit sudo, der öffentliche Schlüssel als erstes Argument:
#   sudo bash bootstrap-claude.sh 'ssh-ed25519 AAAA... claude@gala-llm'
#
# Idempotent: Mehrfaches Ausführen ersetzt nur den Schlüssel.
# Danach gilt der statische Schlüssel. Ab deploy/common/claude-access/install.sh
# entscheidet der Schalter im Admin-Panel, ob der Schlüssel akzeptiert wird.
set -euo pipefail

PUBKEY="${1:?Öffentlichen Schlüssel als erstes Argument übergeben}"
case "$PUBKEY" in
  ssh-ed25519\ *|ssh-rsa\ *|ecdsa-sha2-*) ;;
  *) echo "Das sieht nicht nach einem öffentlichen SSH-Schlüssel aus." >&2; exit 1 ;;
esac

if ! id claude >/dev/null 2>&1; then
  useradd --create-home --shell /bin/bash claude
  echo "Nutzer claude angelegt."
fi

# Kein Passwort-Login; Zugang nur per Schlüssel.
passwd --lock claude >/dev/null

# sudo ohne Passwort, vorher syntaktisch prüfen.
tmp="$(mktemp)"
echo "claude ALL=(ALL:ALL) NOPASSWD: ALL" > "$tmp"
visudo --check --file "$tmp" >/dev/null
install -m 0440 -o root -g root "$tmp" /etc/sudoers.d/90-claude
rm -f "$tmp"

# Docker ohne sudo, falls Docker schon installiert ist.
if getent group docker >/dev/null; then
  usermod --append --groups docker claude
fi

install -d -m 0700 -o claude -g claude /home/claude/.ssh
printf '%s\n' "$PUBKEY" > /home/claude/.ssh/authorized_keys
chown claude:claude /home/claude/.ssh/authorized_keys
chmod 0600 /home/claude/.ssh/authorized_keys

echo "Fertig: claude@$(hostname) mit sudo und Schlüssel ${PUBKEY##* }."
