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
| `deploy/common/prepare-vm.sh [--gpu]` | frische VM vorbereiten: Docker, bei GPU-VMs Treiber 580 und Container Toolkit (Abschnitt 2.1) |
| `deploy/common/images.sh <vm> <commit>` | Images eines Commits aus der Registry laden oder lokal bauen (Abschnitt 5.1) |
| `.github/workflows/images.yml` | baut, testet und veröffentlicht die Images bei jedem Push auf `main` |
| `deploy/common/dc.sh <vm> …` | `docker compose` mit den richtigen env-Dateien |
| `deploy/common/disk.sh <check GB\|cleanup\|report>` | Plattenplatz prüfen, aufräumen, anzeigen (Abschnitt 7) |
| `deploy/ocr/gpu-test.sh` | prüft, ob die OCR auf der GPU rechnet (Abschnitt 8.3) |
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
2. **CPU mit AVX/AVX2 in der VM.** Die ONNX Runtime (BGE-M3 im Coordinator mit Feature `embed`, OCR/Embeddings auf gala-ocr) stürzt sonst beim Start mit SIGILL ab (Container-Exit-Code 132). In Proxmox den CPU-Typ auf `host` (oder `x86-64-v3`) stellen; der Standardtyp (`kvm64`/„QEMU Virtual CPU“) hat kein AVX. Prüfen: `grep -cw avx2 /proc/cpuinfo`. Ohne AVX den Coordinator mit `GALA_COORDINATOR_FEATURES=` (leer) in `/etc/gala/coordinator.env` selbst bauen; dazu `GALA_IMAGE_SOURCE=build` in `/etc/gala/hosts.env` (das Image aus der Registry enthält `embed`).
3. Repo nach `/opt/gala` klonen. Die VMs brauchen dafür Lesezugriff auf das Git-Repo (z. B. Deploy-Key mit Nur-Lese-Recht oder Token in der Remote-URL):
   ```bash
   sudo apt-get install -y git
   sudo git clone <repo-url> /opt/gala
   ```
   Der Checkout gehört root; das Update-System läuft als root.
4. VM vorbereiten:
   ```bash
   sudo /bin/bash /opt/gala/deploy/common/prepare-vm.sh         # gala-coordinator
   sudo /bin/bash /opt/gala/deploy/common/prepare-vm.sh --gpu   # gala-ocr, gala-llm
   ```
   Das Skript installiert Docker (Distributionspakete `docker.io`, `docker-compose-v2`, `docker-buildx`) und mit `--gpu` den NVIDIA-Treiber aus Branch 580 (proprietäres Modul, vorgebaut passend zum Kernel) sowie das NVIDIA Container Toolkit aus der Paketquelle von NVIDIA. Es warnt bei fehlendem AVX2, Secure Boot und fehlendem Passthrough. Nach der Treiber-Installation ist ein Neustart nötig; danach `nvidia-smi` prüfen (Abschnitt 8.1). Das Skript vergrößert keine Platten (Abschnitt 7).

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
sudo /opt/gala/deploy/ocr/install.sh                      # erneut: lädt das Image, startet den Worker
sudo /bin/bash /opt/gala/deploy/ocr/gpu-test.sh --apply   # rechnet die GPU? setzt GALA_OCR_DEVICE
```

Das Worker-Image kommt fertig aus der Registry (Abschnitt 5.1) und ist mit CUDA-Laufzeit einige GB groß. Vorher Abschnitt 7 lesen (Platz auf dem Host). Bis `gpu-test.sh --apply` gelaufen ist, steht `GALA_OCR_DEVICE` auf `cuda`: lädt der CUDA-Provider nicht, startet der Worker nicht (Abschnitt 8.3).

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

**Dokumenten-Index.** Der Abschnitt zeigt, ob die Verarbeitung läuft (aktualisiert sich alle 15 Sekunden):
- **Worker:** Jeder Worker schreibt alle 30 Sekunden ein Lebenszeichen in die Tabelle `worker_status` (Name, Rolle, OCR-Gerät, Commit des Images). Nach 90 Sekunden ohne Meldung steht dort „keine Meldung“. „OCR-Gerät“ ist das Gerät, auf dem die Texterkennung tatsächlich läuft (`cuda:0` oder `cpu`).
- **Warteschlange:** wartende, laufende und endgültig gescheiterte Aufgaben je Art (Text extrahieren, OCR, Embeddings).
- **Warnung:** Warten Aufgaben, aber kein Worker der zuständigen Rolle meldet sich, steht das als Hinweis darüber (z. B. gala-ocr aus oder Worker startet nicht).

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
3. gala-coordinator: Plattenplatz prüfen, Images des Commits bereitstellen (`images.sh`, Abschnitt 5.1), `up -d`, Health-Check auf `http://127.0.0.1:8080/api/v1/health`, CA nach `ca.crt` kopieren, aufräumen (`disk.sh cleanup`).
4. gala-ocr: per SSH (`gala-deploy`, Forced Command) mit der aufgelösten Commit-SHA: fetch, checkout, Image bereitstellen, up, aufräumen.
5. gala-llm: nur wenn sich `deploy/llm/` zwischen altem und neuem Commit geändert hat: pull, up, Health abwarten.
6. War „Wartungsmodus“ angefordert, setzt das Skript `POST http://127.0.0.1:8081/internal/maintenance` mit `{"enabled": false}` und `X-Gala-Internal-Token`.
7. Status `succeeded` mit Meldung und `log_tail`.

**Fehler:** `update-status.json` zeigt `failed` mit Meldung und den letzten Logzeilen, der Wartungsmodus **bleibt an**. Nach der Ursachenanalyse das Update erneut auslösen. Das vollständige Log: `/var/log/gala/update.log` (vorheriges: `update.log.1`).

Hinweise:
- Das Skript ersetzt sich beim Checkout selbst; es ist deshalb komplett in eine Funktion gekapselt.
- Änderungen an den systemd-Units des Coordinators übernimmt `update.sh` automatisch. Bei Änderungen an `install.sh`-Teilen (Firewall, Mounts, ufw) das Skript einmal von Hand erneut ausführen.
- Das Skript braucht die Executable-Bits nicht (Units und sudoers rufen `/bin/bash` auf).
- Manuell: `sudo /opt/gala/deploy/ocr/update.sh <ref>` bzw. `…/llm/update.sh <ref>` auf der jeweiligen VM.

### 5.1 Images: von GitHub Actions gebaut, auf den VMs nur geladen

Die Images von Coordinator und Workern baut der Workflow `.github/workflows/images.yml` bei jedem Push auf `main` und legt sie in die GitHub-Registry:

| Image | Inhalt |
|---|---|
| `ghcr.io/<besitzer>/<repo>/coordinator:<commit>` | Coordinator mit Feature `embed` |
| `…/worker-cpu:<commit>` | Worker, Rolle `cpu` |
| `…/worker-vision:<commit>` | Worker mit Feature `vision`, CUDA-12-Laufzeit |
| `…/bench:<commit>` | Benchmark für `gpu-test.sh`, setzt auf `worker-vision` auf |

Vor dem Hochladen prüft der Workflow jedes Image: das Binary startet; im Vision-Image ist der CUDA-12-Provider vollständig; das Bench-Image erkennt ein Test-PDF auf der CPU (lädt dabei die OCR-Modelle); `--device cuda` bricht ohne GPU ab. Schlägt eine Prüfung fehl, gibt es für den Commit kein Image und das Update bleibt beim alten Stand.

Auf den VMs stellt `deploy/common/images.sh <vm> <commit>` die Images bereit und schreibt `/etc/gala/image.env` (welches Image gilt). `update.sh` und `install.sh` rufen es auf. Einstellungen in `/etc/gala/hosts.env`:

| Variable | Bedeutung |
|---|---|
| `GALA_IMAGE_SOURCE` | `registry` (Standard): Images laden. `build`: die VM baut selbst aus `/opt/gala` (viel Platz und Zeit; nötig ohne Zugang zur Registry oder mit abweichenden Features, z. B. Coordinator ohne `embed`). |
| `GALA_IMAGE_REGISTRY` | Standard `ghcr.io/<besitzer>/<repo>`, abgeleitet vom git-Remote `origin` |
| `GALA_IMAGE_WAIT_MIN` | So lange wartet ein Update auf Images, die die CI noch baut (Standard 30). Ein Lauf dauert etwa 15 Minuten. |

Ablauf einer Änderung: Commit nach `main` pushen → Workflow „Images“ läuft → im Admin-Panel „Update starten“. Wer das Update sofort nach dem Push startet, sieht im Log „Noch nicht ladbar (baut die CI noch?)“, bis die Images da sind.

**Einmalig nach dem ersten Lauf:** Neue Pakete in der GitHub-Registry sind zunächst privat. Entweder jedes der vier Pakete auf GitHub öffentlich stellen (Paket → Package settings → Change visibility), oder auf jeder VM einmal als root anmelden: `sudo docker login ghcr.io` mit einem Token, das nur `read:packages` darf. Ohne das scheitert das Laden mit `denied`/`unauthorized`.

Hinweis: gala-llm nutzt weiter das fertige llama.cpp-Image und ist davon nicht betroffen.

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

gala-ocr braucht rund **25 GB** (Betriebssystem und NVIDIA-Treiber ca. 10 GB, das Worker-Image in zwei Ständen während eines Updates ca. 10 GB, Reserve), gala-llm vor allem Platz für die Modelle (Qwen 14B Q4_K_M ca. 9 GB). Die Zahlen für gala-ocr sind geschätzt; `disk.sh report` zeigt die echte Belegung. Baut eine VM ihre Images selbst (`GALA_IMAGE_SOURCE=build`), kommen Rust-Image und Cargo-Cache dazu, grob 10 GB mehr.

Ein Platzcheck: `sudo /bin/bash /opt/gala/deploy/common/disk.sh report` (entspricht `df -h /var/lib/docker` und `docker system df`). Die Update-Skripte brechen ab, bevor sie Images laden oder bauen, wenn weniger als 8 GB frei sind (`GALA_BUILD_MIN_FREE_GB`), und löschen danach das vorherige Image (`disk.sh cleanup`). Ein alter Build-Cache aus der Zeit lokaler Builds lässt sich von Hand löschen: `docker builder prune -a`.

**Thin Provisioning auf dem Hypervisor.** Liegen die VM-Platten dünn provisioniert (Proxmox: LVM-thin, ZFS, qcow2), belegt die VM auf dem Host jeden Block, den sie je beschrieben hat. Gelöschte Dateien geben den Platz erst mit `fstrim` zurück, und nur, wenn an der VM-Platte **Discard** eingeschaltet ist (Proxmox: Hardware → Festplatte → „Discard“, dazu „SSD-Emulation“). Ohne Discard läuft der Host voll, obwohl die VMs intern noch Platz zeigen; `df` in der VM sieht das nicht. Deshalb:
- Discard an allen VM-Platten einschalten. `disk.sh cleanup` ruft `fstrim -av` auf, Ubuntu zusätzlich wöchentlich (`fstrim.timer`).
- Die Summe der VM-Platten nicht größer wählen als der freie Platz des Hosts, oder den Füllstand des Hosts überwachen (`pvesm status`, `lvs`).
- Eine VM-Platte oder ein LV nur vergrößern, wenn der Host den Platz wirklich hat. Am 10.11.2026 lief der Host voll, nachdem auf gala-ocr das Root-LV von 24 auf 48 GB vergrößert und mehrfach gebaut wurde.

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
- **ONNX Runtime und CUDA 12:** `ort` 2.0.0-rc.13 lädt beim Build eine vorgebaute ONNX Runtime 1.28.0, deren CUDA-Provider es nur für **CUDA 13** gibt (`ort-sys`: „we only ship 13 for now“). CUDA 13 kennt Maxwell nicht mehr. `docker/worker.Dockerfile` ersetzt deshalb den CUDA-Provider (`libonnxruntime_providers_cuda.so`, `…_shared.so`) durch den aus dem offiziellen Microsoft-Paket `onnxruntime-linux-x64-gpu_cuda12-1.28.0.tgz` (mit SHA-256 geprüft). Die Versionen müssen zusammenpassen: bei einem Update von `ort` die Build-Argumente `ORT_CUDA12_URL` und `ORT_CUDA12_SHA256` mitziehen.
- **Stand 11.11.2026:** Mit dem CUDA-12-Provider lädt der Worker auf der GTX 950 (`Successfully registered CUDAExecutionProvider`, cuDNN 9.8, VRAM belegt). Ob auch die Inferenz läuft, zeigt erst `gpu-test.sh` (unten); dieser Lauf steht noch aus. Mögliches Fehlerbild: `no kernel image is available for execution on the device`.
- **GPU-Test:** `sudo /bin/bash /opt/gala/deploy/ocr/gpu-test.sh` erkennt ein Test-PDF auf CPU und GPU, zeigt ms/Seite und den belegten Grafikspeicher und urteilt „BESTANDEN“ oder „NICHT BESTANDEN“. Mit `--apply` trägt es das Ergebnis als `GALA_OCR_DEVICE` in `/etc/gala/ocr.env` ein und startet den Worker neu. Mit eigenen Pfaden (`gpu-test.sh /mnt/archiv/…`) misst es echte Scans. Der Worker wird für den Test kurz angehalten.
- **Kein stiller Rückfall auf die CPU:** Mit `GALA_OCR_DEVICE=cuda` prüft der Worker beim Start, ob der CUDA-Provider lädt. Wenn nicht, beendet er sich mit `CUDA-Provider für cuda:0 nicht ladbar: …` (z. B. `libcublasLt.so.13: cannot open shared object file` bei falscher CUDA-Version, oder fehlender Treiber/Passthrough). Der Container startet dann in einer Schleife neu, und im Admin-Panel fehlt der Worker bzw. steht auf „keine Meldung“. Entweder die Ursache beheben oder bewusst `GALA_OCR_DEVICE=cpu` setzen. Das tatsächlich genutzte Gerät steht im Admin-Panel unter „Dokumenten-Index“; zusätzlich zeigt `nvidia-smi` bei laufendem Worker den Prozess mit belegtem VRAM.
- **VRAM:** 2 GB sind knapp. BGE-M3 läuft im Worker auf der CPU, die GPU gehört der OCR. Bei „out of memory“ kleinere Batches oder CPU-OCR nutzen.
- **CPU-OCR:** In `/etc/gala/ocr.env` `GALA_OCR_DEVICE=cpu` setzen und `dc.sh ocr up -d`. Das Feature `vision` bleibt an (ohne es beendet sich `--role vision`), Image und GPU-Reservierung können bleiben. Das ist langsamer, aber ohne CUDA-Abhängigkeit.

### 8.4 Allgemeine Checks

| Symptom | Prüfen |
|---|---|
| Desktop-App meldet Zertifikatsfehler | `ca.crt` auf dem Client installiert? Name in `GALA_HOSTNAME`/IP stimmt mit der Adresse überein? Nach Änderung von Name/IP Caddy neu starten (neues Zertifikat), CA bleibt gleich. |
| `ca.crt` fehlt auf der Startseite | `sudo /opt/gala/deploy/coordinator/copy-ca.sh`; läuft Caddy (`dc.sh coordinator logs caddy`)? |
| gala-ocr erreicht Postgres nicht | `nc -vz gala-coordinator 5432` von gala-ocr; `sudo iptables -S GALA-FW` auf dem Coordinator; Passwort in `ocr.env` gleich wie in `coordinator.env`? |
| NFS-Mount hängt/fehlt | `showmount -e gala-coordinator` (ggf. mit `nfs-common`), `sudo exportfs -v` auf dem Coordinator, ufw 2049 von gala-ocr; Dateirechte: Nutzer `gala` hat auf beiden VMs UID 10001. |
| NAS leer im Container | `mountpoint /mnt/archiv` auf dem Host; der Mount muss vor dem Container-Start bestehen (Docker wartet per Drop-in auf `mnt-archiv.mount`). Nach NAS-Ausfall Container neu starten. |
| Update wartet lange bei „Noch nicht ladbar“ | Läuft der Workflow „Images“ für den Commit noch, oder ist er rot (GitHub → Actions)? Pakete privat und VM nicht angemeldet (Abschnitt 5.1)? Notfalls `GALA_IMAGE_SOURCE=build`. |
| Aufgaben bleiben in der Warteschlange | Admin-Panel → Dokumenten-Index: Meldet sich ein Worker der Rolle? Auf der VM `docker logs gala-worker-vision` bzw. `gala-worker-cpu`. Bei `CUDA-Provider … nicht ladbar` siehe Abschnitt 8.3. |
| Update bleibt auf `failed` | `update-status.json` (`log_tail`), `/var/log/gala/update.log`, `journalctl -u gala-update.service`. |
| Update startet nicht | `systemctl status gala-update.path`; Schreibrechte des Coordinators auf `/var/lib/gala/state` (Besitzer `gala`). |
| `claude`-Login geht nicht | Schalter im Admin-Panel an? `curl http://gala-coordinator:8081/internal/claude-access` auf der VM; `FAIL_MODE`; `journalctl -u ssh -n 50`. |
