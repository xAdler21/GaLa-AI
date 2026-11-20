# GaLa-AI Desktop-App

Tauri 2 (Rust-Hülle) mit Leptos-Oberfläche (CSR, WASM). Die App ist die einzige Nutzeroberfläche für Kalkulatoren. Sie spricht über die Rust-Seite (`src-tauri`) mit dem Coordinator (`/api/v1`, siehe `CLAUDE.md` im Repo-Wurzelverzeichnis). Die Oberfläche selbst macht keine HTTP-Aufrufe.

`apps/desktop` ist ein **eigener** Cargo-Workspace (Mitglieder `src-tauri` und `ui`).

## Voraussetzungen

- Windows 10/11 mit **WebView2** (bei Windows 11 vorhanden)
- **Visual Studio Build Tools** (Workload „Desktop development with C++“)
- Rust (rustup) und das WASM-Ziel:

  ```powershell
  rustup target add wasm32-unknown-unknown
  cargo install trunk tauri-cli --locked
  ```

Das Repo liegt auf einem Netzlaufwerk. Das Build-Verzeichnis muss deshalb lokal liegen:

```powershell
$env:CARGO_TARGET_DIR = "$env:LOCALAPPDATA\cargo-target\gala-desktop"
```

## Entwickeln

```powershell
cd apps\desktop\src-tauri
cargo tauri dev
```

`cargo tauri dev` startet `trunk serve` (Port 1420) und das App-Fenster mit Hot-Reload.

Prüfen:

```powershell
cd apps\desktop
cargo check -p ui --target wasm32-unknown-unknown
cargo clippy -p ui --target wasm32-unknown-unknown -- -D warnings
cargo clippy -p gala-ai-desktop -- -D warnings
```

## Signierschlüssel für den Auto-Updater

Updates werden signiert; die App prüft die Signatur mit dem öffentlichen Schlüssel aus `tauri.conf.json`.

- Der private Schlüssel liegt auf dem Build-PC unter `%USERPROFILE%\.tauri\gala-updater.key` (erzeugt am 20.11.2026 mit `cargo tauri signer generate`, **ohne Passwort**, wie bei Plintus). Er gehört nicht ins Repo. Eine Sicherung an einem zweiten Ort ist Pflicht: geht er verloren, kann keine installierte App mehr aktualisiert werden.
- Der öffentliche Schlüssel (`gala-updater.key.pub`) steht in `src-tauri/tauri.conf.json` unter `plugins.updater.pubkey`.
- Ein anderer Schlüsselpfad geht über `$env:GALA_UPDATER_KEY`.

## Installer bauen

```powershell
.\apps\desktop\build-release.ps1 -Version 0.2.0
```

Das Skript setzt die Version in `apps/desktop/Cargo.toml`, baut die Oberfläche (`trunk build --release`) und den NSIS-Installer und signiert ihn. Ergebnis (unter `%LOCALAPPDATA%\cargo-target\gala-desktop\release\bundle\nsis\`):

- `GaLa-AI_<version>_x64-setup.exe`
- `GaLa-AI_<version>_x64-setup.exe.sig`

Die Version steht in `apps/desktop/Cargo.toml` (`[workspace.package] version`). Für jedes Release erhöhen.

## Release veröffentlichen

Im Admin-Panel des Coordinators unter „Desktop-Releases“ hochladen (oder per SSH mit `coordinator release add`, siehe docs/BETRIEB.md 2.7):

1. Version und Release-Notizen eintragen.
2. Die Datei **`…-setup.exe`** auswählen.
3. Den **Inhalt** (Text) der Datei **`…-setup.exe.sig`** in das Signaturfeld einfügen, nicht die Datei selbst.
4. „Veröffentlichen“ klicken. Erst dann sehen die Apps das Update.

Die Apps fragen beim Start und über „Nach Updates suchen“ (Einstellungen) `GET {server}/api/v1/updates/{target}/{arch}/{current_version}` ab. Mit `min_client_version` im Health-Endpunkt kann der Server ein Update erzwingen.

## Aufbau

| Pfad | Inhalt |
|---|---|
| `src-tauri/src/main.rs` | Commands: `app_info`, `test_connection`, `install_server_ca`, `save_server_url`, `health`, `login`, `logout`, `me`, `chat` |
| `src-tauri/src/net.rs` | HTTP zum Coordinator (reqwest, Windows-Zertifikatsspeicher), Server-URL und Token im Store |
| `src-tauri/src/updates.rs` | Commands `check_update`, `install_update` (Event `update-progress`) |
| `ui/src/state.rs` | Zustand, Health-Schleife (30 s), Start-Ablauf |
| `ui/src/screens.rs` | Einrichtung, Anmeldung, Wartungssperre, Pflicht-Update |
| `ui/src/shell.rs`, `chat.rs`, `updates.rs` | Hauptansicht, Chat, Update-Anzeige |
| `ui/src/tauri.rs` | Typisierte Aufrufe der Commands |

Die Server-Adresse (Standard `https://gala-coordinator`) und der Anmelde-Token liegen im Tauri-Store `settings.json` im App-Datenverzeichnis.

Die interne CA des Servers muss im Windows-Zertifikatsspeicher liegen. Meldet Schannel „Stammzertifikat nicht vertrauenswürdig“ (Fehlercode `untrusted_cert`), bietet die App „Server-Zertifikat installieren“ an: Sie lädt `/ca.crt` (ohne Kettenprüfung) und ruft `certutil -user -addstore Root` auf. Windows fragt dabei mit dem SHA-1-Fingerabdruck nach; derselbe steht auf der Startseite des Coordinators. Es wird nur der Speicher des angemeldeten Nutzers geändert, Adminrechte sind nicht nötig.

## Offene Punkte

- Angebote sind ein Platzhalter („kommt mit M1“).
