# GaLa-AI – Hinweise für Claude Code

Lokales KI-System für einen GaLaBau-Betrieb:
1. **Angebotsautomatisierung:** LVs einlesen (PDF, Scan, GAEB, Excel), Preise aus Alt-LVs vorschlagen, Material bei Lieferanten anfragen, Angebot als X84/PDF/Excel.
2. **Dokumenten-Chatbot:** indexiert das Netzlaufwerk (überwiegend gescanntes Papier) und beantwortet Fragen mit Quellen.

Fachliche Architektur, Preis-Engine und Meilensteine stehen in [docs/ARCHITEKTUR.md](docs/ARCHITEKTUR.md). Betrieb und VMs stehen in [docs/BETRIEB.md](docs/BETRIEB.md). Diese Datei enthält die Verträge zwischen den Teilen.

## Clients

- **Desktop-App** (`apps/desktop`, Tauri 2 + Leptos CSR) mit NSIS-Installer und signiertem Auto-Updater. Das ist die einzige Nutzeroberfläche. Es gibt **keine** Web-App für Endnutzer.
- **Web-Panel** auf dem Coordinator (server-gerendert, askama + HTMX):
  - öffentliche Startseite mit Download des Installers und des CA-Zertifikats
  - Admin-Login mit Server-Update, Wartungsmodus, Desktop-Releases und dem Schalter für den Claude-Zugang

## VMs

| Host | GPU | Läuft dort |
|---|---|---|
| `gala-coordinator` | keine | Postgres 17 + pgvector, `coordinator` (axum), Caddy (TLS), `worker --role cpu`, Datei-Store `/srv/gala/data` (per NFS an gala-ocr) |
| `gala-ocr` | GTX 950 (2 GB, Maxwell sm_52) | `worker --role vision`: OCR (oar-ocr) und Embeddings (BGE-M3) |
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
  - `GET /updates/{target}/{arch}/{current_version}`
- **Auth:** `Authorization: Bearer <token>`. In der DB liegt nur der SHA-256-Hash des Tokens.
- **Wartungsmodus:** Alles außer `health`, `auth/*` und `updates/*` antwortet mit 503 und `ApiError { code: "maintenance" }`.
- **Netzwerk:** Die App macht alle HTTP-Aufrufe im Rust-Teil (reqwest, Windows-Zertifikatsspeicher). Der Coordinator braucht deshalb kein CORS.

### Desktop-Updates (Tauri-Updater, dynamisches Format)
- **Updater-Abfrage:** `GET /api/v1/updates/{target}/{arch}/{current_version}`.
  - `204`, wenn kein neueres veröffentlichtes Release existiert.
  - Sonst `200` mit `{version, notes, pub_date, url, signature}`.
- **Releases:** Im Admin-Panel werden Version, Notizen, `*-setup.exe` und der Inhalt der `.sig` hochgeladen.
  - Ablage: `$GALA_DATA_DIR/releases/<version>/`.
  - Erst nach „Veröffentlichen“ sichtbar.
- **Downloads:**
  - `GET /download` liefert den neuesten veröffentlichten Installer.
  - `GET /releases/{version}/{file}` liefert eine bestimmte Datei.

### Einstellungen (Tabelle `setting`, Key → jsonb)
- `maintenance` = `{"enabled": bool, "message": string|null, "since": rfc3339|null}`
- `claude_access` = `{"enabled": bool, "changed_at": rfc3339|null, "changed_by": string|null}`. Standard ist `false`.

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
- **Ausführen:** `gala-update.path` (systemd) startet `gala-update.service`. Der Dienst führt `deploy/coordinator/update.sh` als root aus und löscht die Anfrage.
- **Status:** Das Skript schreibt `$GALA_STATE_DIR/update-status.json`:
  `{"state": "idle|running|succeeded|failed", "ref", "started_at", "finished_at", "message", "log_tail"}`.
- **Wartungsmodus beenden:** `POST http://gala-coordinator:8081/internal/maintenance` mit Header `X-Gala-Internal-Token: $GALA_INTERNAL_TOKEN` und Body `{"enabled": false}`.

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

### Cargo-Features
- `coordinator/embed`: Dokumenten-Chatbot (Frage-Embeddings per fastembed). Ohne das Feature antwortet `POST /api/v1/chat` mit 501. Im Docker-Image ist es standardmäßig an (`GALA_COORDINATOR_FEATURES`).
- `worker/vision`: OCR (CUDA) und Embeddings für die Rolle `vision`. Ohne das Feature beendet sich `--role vision` mit einer Meldung.

### Chat-Fehlercodes
- `not_implemented` (501): Chatbot auf dem Server nicht aktiviert.
- `llm_unavailable` (503): llama-server nicht erreichbar.

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
