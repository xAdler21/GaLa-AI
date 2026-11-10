# GaLa-AI – Betrieb

Diese Anleitung beschreibt Aufbau, Einrichtung und laufenden Betrieb der drei Server-VMs. Die Verträge zwischen den Teilen (Ports, Variablen, Update-Mechanismus) stehen in [CLAUDE.md](../CLAUDE.md), die fachliche Architektur in [ARCHITEKTUR.md](ARCHITEKTUR.md). Alle Betriebsdateien liegen unter `deploy/`.

## 1. Überblick

Zielsystem: Ubuntu Server 24.04 LTS mit Docker Engine und compose-Plugin. Das Repo liegt auf **jeder** VM unter `/opt/gala`. Konfiguration liegt unter `/etc/gala/`, nie im Repo.

| VM | GPU | Dienste | Verzeichnisse und Mounts |
|---|---|---|---|
| `gala-coordinator` | keine | Postgres 17 + pgvector, `coordinator`, `worker --role cpu`, Caddy (TLS) | `/srv/gala/data` (Datei-Store, per NFS an gala-ocr), `/var/lib/gala/state` (Update-Anfrage/-Status), `/var/lib/gala/caddy`, `/srv/gala/backup`, `/mnt/archiv` (NAS, ro) |
| `gala-ocr` | GTX 950, 2 GB | `worker --role vision` (OCR, Embeddings) | `/srv/gala/data` (NFS von gala-coordinator), `/mnt/archiv` (NAS, ro) |
| `gala-llm` | RTX 3070 + GTX 1070 | `llama-server` auf `:8080` | `/srv/gala/models` (GGUF-Dateien) |

Ports und Firewall (identisch zu CLAUDE.md):

| Dienst | Adresse | Erreichbar für |
|---|---|---|
| Caddy | `gala-coordinator:443` | LAN/VPN |
| Coordinator öffentlich | `127.0.0.1:8080` | nur Caddy (im Docker-Netz) und lokal |
| Coordinator intern | `0.0.0.0:8081` | gala-coordinator, gala-ocr, gala-llm |
| Postgres | `gala-coordinator:5432` | gala-ocr |
| NFS | `gala-coordinator:2049` | gala-ocr |
| llama-server | `gala-llm:8080` | gala-coordinator, gala-ocr |

**Firewall-Falle.** Docker trägt veröffentlichte Ports direkt in iptables ein und umgeht damit die INPUT-Regeln von `ufw`. Eine reine `ufw`-Regel schützt Postgres, `:8081` und `:8080` (llama) deshalb nicht. Die `install.sh`-Skripte setzen darum zusätzlich den Dienst `gala-docker-firewall.service`, der in der Kette `DOCKER-USER` die erlaubten Quellen je Port festlegt (`deploy/common/docker-firewall.sh`). Prüfen mit `sudo iptables -S GALA-FW`.

### Wichtige Dateien

| Datei | Zweck |
|---|---|
| `deploy/hosts.env.example` | IPs der VMs, LAN-Netz, NAS-Pfad → `/etc/gala/hosts.env` |
| `deploy/<vm>/compose.yml`, `*.env.example` | compose-Stack und Konfigurationsvorlage je VM |
| `deploy/<vm>/install.sh` | Einrichtung der VM (idempotent) |
| `deploy/coordinator/update.sh` | Server-Update (siehe Abschnitt 5) |
| `deploy/common/dc.sh <vm> …` | `docker compose` mit den richtigen env-Dateien |
| `deploy/common/claude-access/` | schaltbarer SSH-Zugang für `claude` |

Alles, was Compose braucht, wird über `deploy/common/dc.sh` aufgerufen, zum Beispiel:

```bash
/opt/gala/deploy/common/dc.sh coordinator ps
/opt/gala/deploy/common/dc.sh coordinator logs -f coordinator
/opt/gala/deploy/common/dc.sh llm logs -f llama
```

## 2. Ersteinrichtung

Reihenfolge: VMs vorbereiten → Bootstrap → `install.sh` je VM (coordinator zuerst) → Claude-Zugang → Admin → erstes Desktop-Release.

### 2.1 Voraussetzungen je VM

1. Ubuntu Server 24.04 oder neuer (gala-coordinator läuft auf 26.04), feste IP, Hostname passend (`gala-coordinator`, `gala-ocr`, `gala-llm`), eigener Admin-Nutzer mit sudo und SSH-Schlüssel.
2. **CPU mit AVX/AVX2 in der VM.** Die ONNX Runtime (BGE-M3 im Coordinator mit Feature `embed`, OCR/Embeddings auf gala-ocr) stürzt sonst beim Start mit SIGILL ab (Container-Exit-Code 132). In Proxmox den CPU-Typ auf `host` (oder `x86-64-v3`) stellen; der Standardtyp (`kvm64`/„QEMU Virtual CPU“) hat kein AVX. Prüfen: `grep -cw avx2 /proc/cpuinfo`. Ohne AVX den Coordinator mit `GALA_COORDINATOR_FEATURES=` (leer) in `/etc/gala/coordinator.env` bauen.
3. Docker Engine, compose-Plugin und buildx. Unter Ubuntu reichen die Distributionspakete: `sudo apt install docker.io docker-compose-v2 docker-buildx`.
4. GPU-VMs: NVIDIA-Treiber und NVIDIA Container Toolkit (siehe Abschnitt 7 und 8).
5. Repo nach `/opt/gala` klonen. Die VMs brauchen dafür Lesezugriff auf das Git-Repo (z. B. Deploy-Key mit Nur-Lese-Recht oder Token in der Remote-URL):
   ```bash
   sudo git clone <repo-url> /opt/gala
   ```
   Der Checkout gehört root; das Update-System läuft als root.

### 2.2 Bootstrap des Nutzers `claude`

Auf jeder VM zuerst `deploy/common/bootstrap-claude.sh` ausführen (legt den Nutzer `claude` mit sudo NOPASSWD und dessen SSH-Schlüssel an). Das ist Voraussetzung für Abschnitt 2.5.

### 2.3 Netzwerkdaten

Auf jeder VM:

```bash
sudo install -d -m 0755 /etc/gala
sudo install -m 0644 /opt/gala/deploy/hosts.env.example /etc/gala/hosts.env
sudoedit /etc/gala/hosts.env     # IPs, LAN-CIDR, NAS-Freigabe eintragen
```

Die Datei muss auf allen drei VMs identisch sein. Platzhalter (`192.0.2.x`, `NAS-IP`) lehnt `install.sh` ab.

**Aufbau in Etappen (nur gala-coordinator):** Gibt es gala-ocr oder gala-llm noch nicht, dort `GALA_OCR_IP=none` bzw. `GALA_LLM_IP=none` eintragen. Dann gilt:
- `install.sh` richtet keinen NFS-Export ein und setzt für diese VMs keine Firewall-Freigaben.
- `update.sh` überspringt sie.
- In den Containern zeigen die Namen auf eine unbelegte Loopback-Adresse. Der Chatbot antwortet dann mit „Sprachmodell nicht erreichbar“, und OCR/Embeddings bleiben in der Warteschlange.

Sobald eine VM steht, ihre IP eintragen und `sudo /opt/gala/deploy/coordinator/install.sh` erneut ausführen. Danach `install.sh` auf der neuen VM wie unten beschrieben.

### 2.4 `install.sh` je VM

**gala-coordinator** (zuerst):

```bash
sudo /opt/gala/deploy/coordinator/install.sh
```

Das Skript legt den Nutzer `gala` (UID 10001), die Verzeichnisse und `/etc/gala/coordinator.env` an (Postgres-Passwort und `GALA_INTERNAL_TOKEN` zufällig), exportiert `/srv/gala/data` per NFS an gala-ocr, bindet das NAS read-only ein, setzt ufw und `DOCKER-USER`, erzeugt den Deploy-Schlüssel `/etc/gala/deploy_key`, installiert die systemd-Units (`gala-update.path`, `gala-backup.timer`) und startet den Stack. Danach liegt die Caddy-Root-CA als `/srv/gala/data/ca.crt`, damit sie der Coordinator ausliefern kann.

NAS-Zugangsdaten eintragen (Vorlage wird angelegt, Rechte 0600):

```bash
sudoedit /etc/gala/cifs-archiv.cred      # username=…, password=…, domain=…
sudo mount /mnt/archiv && ls /mnt/archiv
```

**gala-ocr:**

```bash
scp gala-coordinator:/etc/gala/deploy_key.pub /tmp/gala-deploy.pub   # oder per Hand kopieren
sudo /opt/gala/deploy/ocr/install.sh --deploy-pubkey /tmp/gala-deploy.pub
sudoedit /etc/gala/ocr.env      # POSTGRES_PASSWORD = Wert aus coordinator.env
sudo /opt/gala/deploy/common/dc.sh ocr up -d --build
```

**gala-llm:**

```bash
sudo /opt/gala/deploy/llm/install.sh --deploy-pubkey /tmp/gala-deploy.pub
# GGUF-Modell nach /srv/gala/models legen, LLM_MODEL in /etc/gala/llm.env anpassen
sudo /opt/gala/deploy/common/dc.sh llm up -d
```

**Deploy-Schlüssel (Update per SSH).** Der Coordinator aktualisiert die anderen VMs per SSH als Nutzer `gala-deploy`. Dessen `authorized_keys`-Eintrag ist doppelt eingeschränkt: `restrict` (kein Shell, kein Forwarding, kein PTY), `from=<IP des Coordinators>` und ein Forced Command `sudo -n /bin/bash /opt/gala/deploy/<vm>/update.sh --ssh`. Der gewünschte Commit kommt als `SSH_ORIGINAL_COMMAND` und wird dort strikt als Hex-SHA geprüft. Per sudoers darf `gala-deploy` genau diesen einen Befehl als root ausführen. Einrichtung: `install.sh --deploy-pubkey <datei>` (siehe oben); der Schlüssel liegt auf dem Coordinator in `/etc/gala/deploy_key` (Passphrase-los, nur root lesbar). Beim ersten Update wird der Host-Key der Ziel-VM per `StrictHostKeyChecking=accept-new` in `/etc/gala/deploy_known_hosts` gemerkt.

### 2.5 Claude-Zugang installieren

Auf **jeder** VM, nachdem `claude` per Bootstrap existiert und gala-coordinator läuft:

```bash
sudo /opt/gala/deploy/common/claude-access/install.sh
```

Wichtig: Ab jetzt kann sich `claude` nur anmelden, wenn der Schalter im Admin-Panel an ist (Standard: aus). Das Skript nicht aus einer `claude`-Sitzung starten, die Sitzung würde nach ca. 30 s beendet. Entfernen mit `--uninstall`. Details in Abschnitt 4.

### 2.6 Admin-Konto

Das Admin-Konto wird über die CLI des Coordinators angelegt. Das Passwort wird interaktiv abgefragt oder aus `GALA_ADMIN_PASSWORD` gelesen:

```bash
sudo /opt/gala/deploy/common/dc.sh coordinator exec coordinator coordinator admin create --username <name>
# weitere Nutzer: --role kalkulator; Passwort ändern: admin set-password --username <name>
```

Danach Anmeldung im Admin-Panel unter `https://gala-coordinator/admin`. Ohne DNS-Eintrag geht auch die IP (`https://10.10.0.235/admin`); dann `GALA_PUBLIC_URL` in `coordinator.env` auf die IP setzen, damit Download- und Update-URLs stimmen.

### 2.7 Erstes Desktop-Release

1. Desktop-Installer (`*-setup.exe`) und die `.sig` auf einem Entwickler-PC bauen (Tauri, signiert).
2. Im Admin-Panel unter „Desktop-Releases“ hochladen: Version, Notizen, `*-setup.exe`, Inhalt der `.sig`, dann „Veröffentlichen“.
3. Auf der Startseite `https://gala-coordinator/` lassen sich Installer und CA-Zertifikat herunterladen. Das CA-Zertifikat (`ca.crt`) muss auf den Windows-Clients in „Vertrauenswürdige Stammzertifizierungsstellen“ installiert werden, sonst vertraut die App dem Server nicht.

## 3. Admin-Panel: Schalter im Hintergrund

| Schalter | Gespeichert in | Wirkung |
|---|---|---|
| Wartungsmodus | `setting.maintenance` | Desktop-API antwortet außer `health`, `auth/*`, `updates/*` mit 503 `maintenance`. |
| Claude-Zugang | `setting.claude_access` | Nur `GET /internal/claude-access` liefert den Wert; die VMs fragen ihn ab (Abschnitt 4). |
| Server-Update | Datei `update-request.json` | Löst `gala-update.path` aus (Abschnitt 5). |

Jede Änderung steht in `audit_log`. Die internen Routen (`:8081`) sind nur für die drei VMs erreichbar und werden von Caddy nie nach außen geroutet (`/internal/*` → 404). Schreibende interne Routen verlangen zusätzlich den Header `X-Gala-Internal-Token`.

## 4. Claude-Zugang im Detail

- Der Nutzer `claude` hat auf jeder VM sudo NOPASSWD. Sein Schlüssel wird **nicht** aus `~claude/.ssh/authorized_keys` geliefert (`AuthorizedKeysFile none`), sondern über den `AuthorizedKeysCommand` `/usr/local/lib/gala/claude-keys %u` (läuft als `nobody`).
- Das Skript fragt `GET http://gala-coordinator:8081/internal/claude-access` (3 s Timeout). Bei `{"enabled": true}` gibt es `/etc/gala/claude_authorized_keys` aus, sonst nichts. Für jeden anderen Nutzer als `claude` ist die Ausgabe immer leer.
- **Coordinator nicht erreichbar oder unbrauchbare Antwort:** Standard ist fail-closed. Pro VM umstellbar in `/etc/gala/claude-access.conf` (`FAIL_MODE=open`).
- `gala-claude-reaper.timer` (alle 30 s) beendet per `loginctl terminate-user claude` und `pkill -KILL -u claude` alle Sitzungen, solange der Zugang nicht erlaubt ist (gleiche Regel wie beim Login, inkl. `FAIL_MODE`).
- Die sshd-Konfiguration (`/etc/ssh/sshd_config.d/50-gala-claude.conf`) gilt nur im Block `Match User claude`. Der Admin-Zugang des Betreibers und alle anderen Nutzer bleiben unberührt. `install.sh` prüft mit `sshd -t`, bevor sshd neu geladen wird, und rollt bei einem Fehler zurück.
- Grenze des Mechanismus: Der Schalter steuert den **Login**. Ein bereits eingeloggter `claude` hat wegen sudo Root-Rechte und könnte den Mechanismus prinzipiell ändern. Deshalb ist der Reaper nur ein Komfort- und Hygienemechanismus, keine Sicherheitsgrenze gegen einen böswilligen Nutzer mit Root-Rechten.

Prüfen (auf der VM):

```bash
sudo -u nobody /usr/local/lib/gala/claude-keys claude    # leer = Zugang aus
curl -s http://gala-coordinator:8081/internal/claude-access
systemctl list-timers gala-claude-reaper.timer
```

## 5. Server-Update

```
Admin-Panel "Update" ─► Coordinator schreibt $GALA_STATE_DIR/update-request.json
                        (vorher ggf. Wartungsmodus an)
gala-update.path ─► gala-update.service ─► deploy/coordinator/update.sh (root)
```

Ablauf von `update.sh`:

1. Anfrage lesen (`jq`), löschen, `update-status.json` auf `running` setzen (atomar per tmp + `mv`).
2. `git fetch` und `git checkout --detach` des Refs (Branch, Tag oder SHA) in `/opt/gala`.
3. gala-coordinator: `docker compose build`, `up -d`, Health-Check auf `http://127.0.0.1:8080/api/v1/health`, danach CA nach `ca.crt` kopieren.
4. gala-ocr: per SSH (`gala-deploy`, Forced Command) mit der aufgelösten Commit-SHA: fetch, checkout, build, up.
5. gala-llm: nur wenn sich `deploy/llm/` zwischen altem und neuem Commit geändert hat: pull, up, Health abwarten.
6. War „Wartungsmodus“ angefordert, setzt das Skript `POST http://127.0.0.1:8081/internal/maintenance` mit `{"enabled": false}` und `X-Gala-Internal-Token`.
7. Status `succeeded` mit Meldung und `log_tail`.

**Fehler:** `update-status.json` zeigt `failed` mit Meldung und den letzten Logzeilen, der Wartungsmodus **bleibt an**. Nach der Ursachenanalyse das Update erneut auslösen. Das vollständige Log: `/var/log/gala/update.log` (vorheriges: `update.log.1`).

Hinweise:
- Das Skript ersetzt sich beim Checkout selbst; es ist deshalb komplett in eine Funktion gekapselt.
- Änderungen an den systemd-Units des Coordinators übernimmt `update.sh` automatisch. Bei Änderungen an `install.sh`-Teilen (Firewall, Mounts, ufw) das Skript einmal von Hand erneut ausführen.
- Das Skript braucht die Executable-Bits nicht (Units und sudoers rufen `/bin/bash` auf).
- Manuell: `sudo /opt/gala/deploy/ocr/update.sh <ref>` bzw. `…/llm/update.sh <ref>` auf der jeweiligen VM.

## 6. Backups

| Was | Wie | Wohin |
|---|---|---|
| Postgres | `gala-backup.timer`, täglich ca. 02:30: `pg_dump -Fc` | `/srv/gala/backup/db/gala-<datum>.dump`, Aufbewahrung `GALA_BACKUP_KEEP_DAYS` (14) |
| Datei-Store `/srv/gala/data` | optional im selben Lauf per `rsync` (ohne `--delete`), wenn `GALA_BACKUP_RSYNC_TARGET` gesetzt ist | z. B. eingebundenes NAS-Backup-Ziel oder `user@host:/pfad` |
| Konfiguration | `/etc/gala/` (enthält Passwörter und Deploy-Schlüssel) | an einem sicheren Ort ablegen |

Der Backup-Ordner liegt standardmäßig auf derselben VM-Platte. Er schützt vor Fehlbedienung, **nicht** vor Plattenausfall. Deshalb einen externen Pfad als `GALA_BACKUP_RSYNC_TARGET` setzen und `/srv/gala/backup` ebenfalls dorthin kopieren (VM-Snapshots des Hypervisors ergänzen das).

```bash
systemctl list-timers gala-backup.timer
sudo systemctl start gala-backup.service && journalctl -u gala-backup.service -n 20
# Wiederherstellen (Datenbank vorher leer/neu):
cat /srv/gala/backup/db/gala-XXXX.dump | /opt/gala/deploy/common/dc.sh coordinator exec -T postgres \
  pg_restore -U gala -d gala --clean --if-exists
```

Das NAS-Archiv selbst wird hier nicht gesichert (nur gelesen).

## 7. Speicher-Planung

Plan: ca. **200–300 GB SSD** für gala-coordinator.

| Posten | Größenordnung |
|---|---|
| Postgres (Texte, Embeddings, Preisdaten) | einige 10 GB, wächst mit dem Archiv |
| Gerenderte Seitenbilder (`/srv/gala/data`) | der größte Posten; bei 100.000 Seiten à 0,3–1 MB entsprechend 30–100 GB |
| Releases, Exporte | wenige GB |
| Docker-Images und Build-Cache | 10–20 GB |
| Backups (14 Dumps) | wenige GB bis einige 10 GB |

Ein Platzcheck: `df -h /srv/gala /var/lib/docker`, `docker system df`. Alte Build-Caches: `docker builder prune`. gala-ocr und gala-llm brauchen kaum Platz (Images, Modelle: Qwen 14B Q4_K_M ca. 9 GB, OCR-Modelle einige GB).

## 8. Hardware-Hinweise und Fehlersuche

### 8.1 GPU in der VM

1. Auf dem Hypervisor muss die GPU per PCI-Passthrough an die VM durchgereicht sein (IOMMU/VT-d an, GPU in einer eigenen IOMMU-Gruppe, bei Proxmox/KVM zusätzlich die Audio-Funktion der Karte mit durchreichen). Bei zwei Karten (gala-llm) beide durchreichen.
2. In der VM: `lspci | grep -i nvidia` muss die Karten zeigen, `nvidia-smi` Treiber, Karten und freien VRAM.
3. Docker: `docker run --rm --gpus all nvidia/cuda:12.8.1-base-ubuntu24.04 nvidia-smi`. Schlägt das fehl, fehlt das NVIDIA Container Toolkit oder die Runtime-Konfiguration:
   ```bash
   sudo nvidia-ctk runtime configure --runtime=docker && sudo systemctl restart docker
   ```
4. Typische Ursachen bei „keine GPU“: Passthrough fehlt, Secure Boot blockiert das Treibermodul (MOK einrichten oder Secure Boot aus), Open-Kernel-Modul passt nicht zu alten Karten (Pascal/Maxwell: proprietäres Modul nutzen, Paket `nvidia-driver-580` statt `-open`).

### 8.2 gala-llm: Treiber und Layer-Split

- **Treiber 580:** Der Branch 580 ist der letzte, der Pascal (GTX 1070) unterstützt. Keine neueren Branches installieren, sonst fällt die 1070 aus (`sudo apt-mark hold nvidia-driver-580` hilft gegen versehentliche Upgrades).
- **GPU-Reihenfolge:** `CUDA_DEVICE_ORDER=PCI_BUS_ID` ist im Compose gesetzt. `--main-gpu 0` meint dann die Karte mit der niedrigsten Bus-ID. Prüfen: `nvidia-smi --query-gpu=index,pci.bus_id,name --format=csv`. Steht dort die GTX 1070 an Index 0, `LLM_TENSOR_SPLIT` und `--main-gpu` anpassen oder die Passthrough-Reihenfolge ändern.
- **Split prüfen:** `docker logs gala-llama 2>&1 | grep -Ei 'cuda|offload|buffer|split'` muss beide Karten und die verteilten Layer zeigen; `nvidia-smi` zeigt VRAM-Belegung auf beiden Karten. Ist eine Karte fast leer, den `LLM_TENSOR_SPLIT` ändern (z. B. `3,2`: mehr auf die 3070).
- **VRAM:** `LLM_CTX` ist der Gesamt-Kontext, geteilt durch die 2 Slots (`--parallel 2`). Bei „out of memory“: `LLM_CTX` senken, kleinere Quantisierung oder anderes Modell wählen.
- **Image pinnen:** `LLAMA_IMAGE_TAG` in `/etc/gala/llm.env` auf einen konkreten Build setzen (`server-cuda-b<nummer>`), damit ein Update nicht unbemerkt eine neue llama.cpp-Version zieht. Für ein bewusstes Upgrade den Tag ändern und `update.sh` laufen lassen (ändert sich nur die Env-Datei, die nicht im Repo liegt, `docker compose pull && up -d` von Hand ausführen).
- Test: `curl http://gala-llm:8080/health` von gala-coordinator aus; von einer anderen IP aus muss die Verbindung scheitern.

### 8.3 gala-ocr: GTX-950-Risiko (Maxwell, sm_52)

Die GTX 950 (2 GB) ist die schwächste Stelle der Planung:

- **CUDA-Version:** Maxwell (sm_52) wird nur bis **CUDA 12.x** unterstützt, **CUDA 13 hat Maxwell entfernt**. Auf der VM muss deshalb ein Treiberzweig installiert sein, der die GTX 950 noch unterstützt (Branch 580 oder älter; darüber wird Maxwell nicht mehr berücksichtigt), und im Container CUDA 12.x (Standard: `nvidia/cuda:12.8.1-cudnn-runtime-ubuntu24.04`, wie `docker/bench.Dockerfile`).
- **ONNX Runtime:** Die von `ort` geladene CUDA-Build von ONNX Runtime muss Kernel für **sm_52** enthalten. Das ist nicht bei jeder Version der Fall. Zu prüfen im M0-Benchmark auf der echten Karte (Fehlerbild: `no kernel image is available for execution on the device` oder stilles Zurückfallen auf CPU).
- **VRAM:** 2 GB sind knapp für OCR **und** BGE-M3. Bei „out of memory“ kleinere Batches oder die Vision-Rolle ohne GPU nutzen.
- **Fallback CPU-OCR:** In `/etc/gala/ocr.env` `GALA_WORKER_FEATURES=` (leer) und `GALA_CUDA_RUNTIME_IMAGE=debian:bookworm-slim` setzen, die GPU-Reservierung in `deploy/ocr/compose.yml` (Block `deploy.resources.reservations`) entfernen und neu bauen: `dc.sh ocr up -d --build`. Das ist langsamer, aber ohne CUDA-Abhängigkeit.

### 8.4 Allgemeine Checks

| Symptom | Prüfen |
|---|---|
| Desktop-App meldet Zertifikatsfehler | `ca.crt` auf dem Client installiert? Name in `GALA_HOSTNAME`/IP stimmt mit der Adresse überein? Nach Änderung von Name/IP Caddy neu starten (neues Zertifikat), CA bleibt gleich. |
| `ca.crt` fehlt auf der Startseite | `sudo /opt/gala/deploy/coordinator/copy-ca.sh`; läuft Caddy (`dc.sh coordinator logs caddy`)? |
| gala-ocr erreicht Postgres nicht | `nc -vz gala-coordinator 5432` von gala-ocr; `sudo iptables -S GALA-FW` auf dem Coordinator; Passwort in `ocr.env` gleich wie in `coordinator.env`? |
| NFS-Mount hängt/fehlt | `showmount -e gala-coordinator` (ggf. mit `nfs-common`), `sudo exportfs -v` auf dem Coordinator, ufw 2049 von gala-ocr; Dateirechte: Nutzer `gala` hat auf beiden VMs UID 10001. |
| NAS leer im Container | `mountpoint /mnt/archiv` auf dem Host; der Mount muss vor dem Container-Start bestehen (Docker wartet per Drop-in auf `mnt-archiv.mount`). Nach NAS-Ausfall Container neu starten. |
| Update bleibt auf `failed` | `update-status.json` (`log_tail`), `/var/log/gala/update.log`, `journalctl -u gala-update.service`. |
| Update startet nicht | `systemctl status gala-update.path`; Schreibrechte des Coordinators auf `/var/lib/gala/state` (Besitzer `gala`). |
| `claude`-Login geht nicht | Schalter im Admin-Panel an? `curl http://gala-coordinator:8081/internal/claude-access` auf der VM; `FAIL_MODE`; `journalctl -u ssh -n 50`. |
