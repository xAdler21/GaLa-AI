# GaLa-AI – Hinweise für Claude Code

Lokales KI-System für einen GaLaBau-Betrieb:
1. **Angebotsautomatisierung:** LVs einlesen (PDF, Scan, GAEB, Excel), Preise aus Alt-LVs vorschlagen, Material bei Lieferanten anfragen, Angebot als X84/PDF/Excel.
2. **Dokumenten-Chatbot:** indexiert das Netzlaufwerk (überwiegend gescanntes Papier) und beantwortet Fragen mit Quellen.

Fachliche Architektur, Preis-Engine und Meilensteine stehen in [docs/ARCHITEKTUR.md](docs/ARCHITEKTUR.md). Betrieb und VMs stehen in [docs/BETRIEB.md](docs/BETRIEB.md). Diese Datei enthält die Verträge zwischen den Teilen.

## Clients

- **Desktop-App** (`apps/desktop`, Tauri 2 + Leptos CSR) mit NSIS-Installer und signiertem Auto-Updater. Das ist die einzige Nutzeroberfläche. Es gibt **keine** Web-App für Endnutzer.
- **Web-Panel** auf dem Coordinator (server-gerendert, askama + HTMX):
  - öffentliche Startseite mit Download des Installers und des CA-Zertifikats
  - Admin-Login mit Server-Update, Wartungsmodus, Desktop-Releases, dem Schalter für den Claude-Zugang und dem Zustand des Dokumenten-Index (Worker, Warteschlange)

## VMs

| Host | GPU | Läuft dort |
|---|---|---|
| `gala-coordinator` | keine | Postgres 17 + pgvector, `coordinator` (axum), Caddy (TLS), `worker --role cpu`, Datei-Store `/srv/gala/data` (per NFS an gala-ocr) |
| `gala-ocr` | keine | `worker --role vision`: OCR (oar-ocr) und Embeddings (BGE-M3), beides auf der CPU |
| `gala-llm` | RTX 3070 + GTX 1070 (16 GB, Layer-Split, Treiber 580) | `llama-server` (OpenAI-kompatibel) auf `:8080` |

Das Netzlaufwerk (NAS) ist auf gala-coordinator und gala-ocr read-only unter `/mnt/archiv` eingebunden.

Die Prozesse kommunizieren **nur über Postgres, den Datei-Store und HTTP zu llama-server**. Es gibt keinen Message-Broker.

### Ports

| Dienst | Adresse | Zugriff |
|---|---|---|
| Caddy (TLS, interne CA) | `gala-coordinator:443` | LAN/VPN |
| Coordinator, öffentlich | `127.0.0.1:8080` | nur über Caddy |
| Coordinator, intern (`/internal/*`) | `0.0.0.0:8081` | Firewall: nur die drei VMs |
| Postgres | `gala-coordinator:5432` | Firewall: nur gala-ocr |
| llama-server | `gala-llm:8080` | Firewall: nur gala-coordinator und gala-ocr |

## Verträge

### Desktop-API (`/api/v1`)
Die DTOs stehen in `crates/api-types/src/client.rs` und sind die einzige Quelle der Wahrheit.

- **Routen:**
  - `GET /health`
  - `POST /auth/login`, `POST /auth/logout`
  - `GET /me`
  - `POST /chat`
  - `GET /lvs`, `POST /lvs` (Import, Datei als Base64), `GET /lvs/{id}`, `DELETE /lvs/{id}`
  - `PUT /lvs/{id}/prices` (EP setzen, mit `revision`; veraltet → 409 `conflict`)
  - `GET /lvs/{id}/export/{x84|d84|xlsx}` (JSON mit Base64; X84 nur aus X8x, D84 nur aus D8x)
  - `GET /lvs/{id}/suggestions` (frühere EPs zu ähnlichen Kurztexten aus der Preis-Historie)
  - `GET /updates/{target}/{arch}/{current_version}`
- **Auth:** `Authorization: Bearer <token>`. In der DB liegt nur der SHA-256-Hash des Tokens.
- **Wartungsmodus:** Alles außer `health`, `auth/*` und `updates/*` antwortet mit 503 und `ApiError { code: "maintenance" }`.
- **Netzwerk:** Die App macht alle HTTP-Aufrufe im Rust-Teil (reqwest, Windows-Zertifikatsspeicher). Der Coordinator braucht deshalb kein CORS.

### Desktop-Updates (Tauri-Updater, dynamisches Format)
- **Updater-Abfrage:** `GET /api/v1/updates/{target}/{arch}/{current_version}`.
  - `204`, wenn kein neueres veröffentlichtes Release existiert.
  - Sonst `200` mit `{version, notes, pub_date, url, signature}`.
- **Releases:** Im Admin-Panel werden Version, Notizen, `*-setup.exe` und der Inhalt der `.sig` hochgeladen, oder per SSH mit `coordinator release add --version --installer --signature [--notes] [--publish]` (im Container, Dateien unter `$GALA_DATA_DIR`). Gebaut wird mit `apps/desktop/build-release.ps1`, signiert mit `%USERPROFILE%/.tauri/gala-updater.key` (ohne Passwort, nicht im Repo).
  - Ablage: `$GALA_DATA_DIR/releases/<version>/`.
  - Erst nach „Veröffentlichen“ sichtbar.
- **Downloads:**
  - `GET /download` liefert den neuesten veröffentlichten Installer.
  - `GET /releases/{version}/{file}` liefert eine bestimmte Datei.

### Einstellungen (Tabelle `setting`, Key → jsonb)
- `maintenance` = `{"enabled": bool, "message": string|null, "since": rfc3339|null}`
- `claude_access` = `{"enabled": bool, "changed_at": rfc3339|null, "changed_by": string|null}`. Standard ist `false`.
- `bidder_name` = string: Bieter in Satz 04 der D84. Fehlt er, bleibt der Satz 04 der D83 stehen.

Jede Änderung landet in `audit_log`.

### Claude-Zugang auf den VMs
Jede VM hat den Linux-Nutzer `claude` mit sudo (NOPASSWD). Den Schlüssel liefert sshd nur aus, wenn der Schalter im Admin-Panel an ist:
- **Schalter abfragen:** `GET http://gala-coordinator:8081/internal/claude-access` liefert `{"enabled": bool, "changed_at": ...}`.
- **Schlüssel ausliefern:** sshd ruft per `AuthorizedKeysCommand` `/usr/local/lib/gala/claude-keys %u` auf (als `nobody`). Das Skript gibt `/etc/gala/claude_authorized_keys` nur aus, wenn `enabled` true ist.
- **Coordinator nicht erreichbar:** Standard ist fail-closed. Das lässt sich pro VM in `/etc/gala/claude-access.conf` umstellen (`FAIL_MODE=open`).
- **Sitzungen beenden:** Der Timer `gala-claude-reaper.timer` (alle 30 s) beendet laufende `claude`-Sitzungen, sobald der Schalter aus ist.
- **Admin-Zugang:** Der eigene Admin-Zugang des Betreibers ist davon nie betroffen.

### Server-Update aus dem Admin-Panel
- **Anstoßen:** Der Coordinator (läuft nicht als root) schreibt `$GALA_STATE_DIR/update-request.json`:
  `{"requested_by", "requested_at", "ref", "maintenance": bool}`.
- **Ausführen:** `gala-update.path` (systemd) startet `gala-update.service`. Der Dienst führt `deploy/coordinator/update.sh` als root aus und löscht die Anfrage. Das Skript lädt die Images des Commits (siehe „Images“) und wartet dabei, falls die CI sie noch baut.
- **Status:** Das Skript schreibt `$GALA_STATE_DIR/update-status.json`:
  `{"state": "idle|running|succeeded|failed", "ref", "started_at", "finished_at", "message", "log_tail"}`.
- **Wartungsmodus beenden:** `POST http://gala-coordinator:8081/internal/maintenance` mit Header `X-Gala-Internal-Token: $GALA_INTERNAL_TOKEN` und Body `{"enabled": false}`.

### Images
- **Bauen:** `.github/workflows/images.yml` baut bei jedem Push auf `main` `coordinator`, `worker-cpu`, `worker-vision` und `bench`, testet sie und legt sie als `ghcr.io/<besitzer>/<repo>/<name>:<commit-sha>` ab (zusätzlich die Tags `main` und die Versionsnummer `<major>.<minor>.<lauf>`, z. B. `0.1.12`: major.minor aus `[workspace.package]` in `Cargo.toml`, lauf = Nummer des Workflow-Laufs). Sind alle Images oben, setzt der Workflow das Git-Tag `v0.1.12`. Im Admin-Panel wird per Versionsnummer (oder `main`) aktualisiert; angezeigt werden nur Versionsnummern, keine Commit-Hashes.
- **Bereitstellen:** `deploy/common/images.sh <coordinator|ocr> <commit-sha>` lädt die Images des Commits (oder baut lokal) und schreibt `/etc/gala/image.env` mit `GALA_IMAGE_REPO`, `GALA_IMAGE_TAG`, `GALA_VERSION`. `dc.sh` reicht die Datei an compose weiter; ohne sie gelten `gala/<name>:local`.
- **Einstellungen** in `/etc/gala/hosts.env`: `GALA_IMAGE_SOURCE` = `registry` (Standard) oder `build`, `GALA_IMAGE_REGISTRY`, `GALA_IMAGE_WAIT_MIN`.
- `update.sh` und `install.sh` bauen nicht selbst, sondern rufen `images.sh` auf.

### Worker-Status (Tabelle `worker_status`)
- Jeder Worker schreibt alle 30 s per UPSERT über `name`: `role` (`cpu`|`vision`), `ocr_device` (nur vision, im Betrieb `cpu`), `build_rev`, `started_at`, `last_seen`.
- Das Admin-Panel wertet einen Worker nach 90 s ohne Meldung als ausgefallen (`GET /admin/index/status`, HTMX-Fragment).
- OCR läuft im Betrieb auf der CPU (gala-ocr hat keine Grafikkarte, die Images enthalten kein CUDA). `GALA_OCR_DEVICE=cuda` geht nur mit dem Feature `worker/cuda`; lädt der CUDA-Provider dann nicht, beendet sich der Worker mit Fehler (kein stiller Rückfall auf die CPU).

### Umgebungsvariablen
| Variable | Wer | Bedeutung |
|---|---|---|
| `DATABASE_URL` | coordinator, worker | Postgres |
| `GALA_BIND` / `GALA_INTERNAL_BIND` | coordinator | Standard `127.0.0.1:8080` / `0.0.0.0:8081` |
| `GALA_PUBLIC_URL` | coordinator | z. B. `https://gala-coordinator`, für Download- und Update-URLs |
| `GALA_DATA_DIR` | coordinator, worker | Datei-Store (Releases, Seitenbilder, Exporte) |
| `GALA_STATE_DIR` | coordinator, update.sh | Update-Anfrage und -Status |
| `GALA_INTERNAL_TOKEN` | coordinator, update.sh | Schutz für schreibende `/internal`-Routen |
| `GALA_ARCHIV_ROOT` | worker | Wurzel des Netzlaufwerks, Standard `/mnt/archiv` |
| `GALA_LLM_URL` | coordinator, worker | Standard `http://gala-llm:8080` |
| `GALA_EMBED_CACHE_DIR` | coordinator (Feature `embed`), worker (vision) | Modell-Cache für BGE-M3 |
| `PDFIUM_DIR` | worker | Ordner mit libpdfium, falls nicht systemweit installiert |
| `GALA_OCR_DEVICE` | worker (vision) | `cpu` (Standard, in `deploy/ocr/compose.yml` fest gesetzt). `cuda` nur mit Feature `worker/cuda` |
| `GALA_WORKER_NAME` | worker | Name in `index_task.worker` und `worker_status`, Standard Hostname. Die compose-Dateien setzen feste Namen |
| `GALA_VERSION` | coordinator, worker | Versionsnummer des Images, z. B. `0.1.12` (setzt die CI per Build-Argument), erscheint im Admin-Panel. Lokale Builds: `<Cargo-Version>-lokal` |

### Cargo-Features
- `coordinator/embed`: Dokumenten-Chatbot (Frage-Embeddings per fastembed). Ohne das Feature antwortet `POST /api/v1/chat` mit 501. Im Docker-Image ist es standardmäßig an (`GALA_COORDINATOR_FEATURES`).
- `worker/vision`: OCR und Embeddings (CPU) für die Rolle `vision`. Ohne das Feature beendet sich `--role vision` mit einer Meldung.
- `worker/cuda`, `bench/cuda`: OCR auf einer GPU. Im Betrieb ungenutzt und nicht in der CI (das Build-Skript von ort-sys rc.13 bricht damit unter Linux ab).

### Chat-Fehlercodes
- `not_implemented` (501): Chatbot auf dem Server nicht aktiviert.
- `llm_unavailable` (503): llama-server nicht erreichbar.

### LV-Fehlercodes
- `invalid_file` (422): Import-Datei ist keine lesbare GAEB-Datei.
- `export_failed` (422): Abgabe ließ sich aus dem Original nicht erzeugen.
- `conflict` (409): Das LV wurde seit dem Laden von jemand anderem gespeichert.

### Preis-Historie (Tabelle `price_ref`)
- Der Worker (Rolle `cpu`) schreibt beim Extrahieren einer GAEB-Datei der Phase 84–86 deren Einheitspreise (Positionen mit Kurztext und EP > 0). Fehlen einer D84 die Texte, kommen sie aus einer Ausschreibung (D8x/X8x) im selben Ordner oder eine Ebene darüber, Zuordnung über die OZ.
- PDF-Angebote (Dateiname mit „Angebot“, ohne Lieferantenordner): sobald das Dokument fertig indiziert ist (auch OCR-Seiten), liest `ingest::pdf_prices` den AVA-Druck aus den Chunks (OZ + Kurztext, Mengenzeile `Menge Einheit EP [GP]`, GP muss zu Menge × EP passen). Nachholen für schon indizierte PDFs: `worker --backfill-pdf-prices`.
- Vorschläge: Trigramm-Ähnlichkeit (`pg_trgm`, ab 0,3) des Kurztexts, gleiche Einheit, je Position höchstens 5; gleicher Kurztext zum gleichen Preis zählt einmal.

### Migrationen
`crates/db/migrations/`, Nummernkreise siehe `crates/db/src/lib.rs`.

## Konventionen

- **Sprache:** Kommentare, Doku und UI-Texte auf Deutsch, Bezeichner wie im bestehenden Code.
- **Rust:**
  - Edition 2024, `[lints] workspace = true`, `unsafe_code` verboten.
  - `cargo fmt` und `cargo clippy --workspace --all-targets -- -D warnings` müssen sauber sein.
- **sqlx:** nur Laufzeit-Queries (`sqlx::query`/`query_as`), keine `query!`-Makros. `sqlx::migrate!` ist erlaubt.
- **Abhängigkeiten:** Gemeinsam genutzte Abhängigkeiten gehören in `[workspace.dependencies]`.
- **Fehler:** `thiserror` in Libraries, `anyhow` in Apps.
- **Desktop-App:** `apps/desktop` ist ein **eigener** Cargo-Workspace (Tauri und trunk) und bindet `api-types` per Pfad ein.

## Bauen unter Windows

Das Repo liegt auf einem Netzlaufwerk. Das Build-Verzeichnis muss deshalb lokal liegen:

```powershell
$env:CARGO_TARGET_DIR = "$env:LOCALAPPDATA\cargo-target\gala"
cargo test --workspace
```

`git` ist in der PowerShell der Claude-Code-Sitzung nicht im PATH.
