#!/usr/bin/env bash
# Beschränkt die von Docker veröffentlichten Ports auf die erlaubten Quellen.
#
# Hintergrund: Docker trägt seine Portweiterleitungen direkt in iptables ein
# (nat + FORWARD) und umgeht dadurch die INPUT-Regeln von ufw. Ein
# `ufw allow from X to any port 5432` schützt einen veröffentlichten
# Container-Port deshalb NICHT. Die einzige dafür vorgesehene Stelle ist die
# Kette DOCKER-USER. Dieses Skript füllt dort eine eigene Kette GALA-FW.
#
# Installiert nach /usr/local/lib/gala/docker-firewall.sh und von
# gala-docker-firewall.service nach jedem (Neu-)Start von Docker ausgeführt.
# Rolle steht in /etc/gala/role, IPs in /etc/gala/hosts.env.
set -euo pipefail

ROLE=$(cat /etc/gala/role)
set -a
# shellcheck source=/dev/null
. /etc/gala/hosts.env
set +a

CHAIN=GALA-FW

iptables -nL DOCKER-USER >/dev/null 2>&1 \
  || { echo "DOCKER-USER-Kette fehlt (läuft Docker?)" >&2; exit 1; }

if iptables -nL "$CHAIN" >/dev/null 2>&1; then
  iptables -F "$CHAIN"
else
  iptables -N "$CHAIN"
fi

# Bereits aufgebaute Verbindungen und Verkehr aus Container-/Loopback-Netzen
# nicht anfassen.
iptables -A "$CHAIN" -m conntrack --ctstate RELATED,ESTABLISHED -j RETURN
iptables -A "$CHAIN" -s 127.0.0.0/8 -j RETURN
iptables -A "$CHAIN" -s 172.16.0.0/12 -j RETURN

# protect <port> <quelle>...: nur die genannten Quellen dürfen auf den
# (vor der DNAT-Umsetzung) angesprochenen Port, alle anderen werden verworfen.
protect() {
  local port=$1 src
  shift
  for src in "$@"; do
    # VMs, die es noch nicht gibt ("none"), bekommen keine Freigabe.
    [ "$src" != none ] || continue
    iptables -A "$CHAIN" -p tcp -m conntrack --ctorigdstport "$port" --ctdir ORIGINAL -s "$src" -j RETURN
  done
  iptables -A "$CHAIN" -p tcp -m conntrack --ctorigdstport "$port" --ctdir ORIGINAL -j DROP
}

case "$ROLE" in
  coordinator)
    protect 5432 "$GALA_OCR_IP"
    protect 8081 "$GALA_COORDINATOR_IP" "$GALA_OCR_IP" "$GALA_LLM_IP"
    protect 443 "$GALA_LAN_CIDR"
    ;;
  llm)
    protect 8080 "$GALA_COORDINATOR_IP" "$GALA_OCR_IP"
    ;;
  ocr)
    : # gala-ocr veröffentlicht keine Ports
    ;;
  *)
    echo "Unbekannte Rolle: $ROLE" >&2
    exit 1
    ;;
esac

# Kette (einmalig) an den Anfang von DOCKER-USER hängen
iptables -C DOCKER-USER -j "$CHAIN" 2>/dev/null || iptables -I DOCKER-USER 1 -j "$CHAIN"
echo "GALA-FW für Rolle '$ROLE' gesetzt."
