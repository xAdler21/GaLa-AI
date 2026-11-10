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

1. Schlüsselpaar erzeugen, an einem sicheren Ort **außerhalb des Repos**, den Sie selbst wählen (nicht im Repo und nicht im NAS-Ordner `#VM-Keys`):

   ```powershell
   cargo tauri signer generate -w "<Pfad>\gala-updater.key"
   ```

   Das Passwort sichern Sie getrennt (Passwort-Manager). Geht der private Schlüssel verloren, kann keine bestehende Installation mehr aktualisiert werden.
2. Der **öffentliche** Schlüssel (Inhalt von `gala-updater.key.pub`) kommt in `src-tauri/tauri.conf.json` unter `plugins.updater.pubkey`. Dort steht bisher der Platzhalter `REPLACE_WITH_TAURI_SIGNING_PUBLIC_KEY`. Ersetzen Sie ihn durch den kompletten Text der `.pub`-Datei und bauen Sie danach neu.
3. Der private Schlüssel kommt nur über Umgebungsvariablen in den Build, nie in eine Datei im Repo:

   ```powershell
   $env:TAURI_SIGNING_PRIVATE_KEY = Get-Content "<Pfad>\gala-updater.key" -Raw
   $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = "<Passwort>"
   ```

## Installer bauen

```powershell
cd apps\desktop\src-tauri
cargo tauri build
```

Das baut zuerst die Oberfläche (`trunk build --release`) und dann den NSIS-Installer. Ergebnis (unter `$env:CARGO_TARGET_DIR\release\bundle\nsis\`):

- `GaLa-AI_<version>_x64-setup.exe`
- `GaLa-AI_<version>_x64-setup.exe.sig`

Die Version steht in `apps/desktop/Cargo.toml` (`[workspace.package] version`). Für jedes Release erhöhen.

## Release veröffentlichen

Im Admin-Panel des Coordinators unter „Desktop-Releases“ hochladen:

1. Version und Release-Notizen eintragen.
2. Die Datei **`…-setup.exe`** auswählen.
3. Den **Inhalt** (Text) der Datei **`…-setup.exe.sig`** in das Signaturfeld einfügen, nicht die Datei selbst.
4. „Veröffentlichen“ klicken. Erst dann sehen die Apps das Update.

Die Apps fragen beim Start und über „Nach Updates suchen“ (Einstellungen) `GET {server}/api/v1/updates/{target}/{arch}/{current_version}` ab. Mit `min_client_version` im Health-Endpunkt kann der Server ein Update erzwingen.

## Aufbau

| Pfad | Inhalt |
|---|---|
| `src-tauri/src/main.rs` | Commands: `app_info`, `test_connection`, `save_server_url`, `health`, `login`, `logout`, `me`, `chat` |
| `src-tauri/src/net.rs` | HTTP zum Coordinator (reqwest, Windows-Zertifikatsspeicher), Server-URL und Token im Store |
| `src-tauri/src/updates.rs` | Commands `check_update`, `install_update` (Event `update-progress`) |
| `ui/src/state.rs` | Zustand, Health-Schleife (30 s), Start-Ablauf |
| `ui/src/screens.rs` | Einrichtung, Anmeldung, Wartungssperre, Pflicht-Update |
| `ui/src/shell.rs`, `chat.rs`, `updates.rs` | Hauptansicht, Chat, Update-Anzeige |
| `ui/src/tauri.rs` | Typisierte Aufrufe der Commands |

Die Server-Adresse (Standard `https://gala-coordinator`) und der Anmelde-Token liegen im Tauri-Store `settings.json` im App-Datenverzeichnis. Die interne CA des Servers muss im Windows-Zertifikatsspeicher installiert sein (Download auf der Startseite des Coordinators).

## Offene Punkte

- Der Platzhalter für den Updater-Schlüssel muss vor dem ersten Release ersetzt werden.
- Angebote sind ein Platzhalter („kommt mit M1“).
