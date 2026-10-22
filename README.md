# AngebotsAutomatisierung

Automatisiert die Angebotserstellung auf GaLaBau-Ausschreibungen:
- LV einlesen (PDF, Scan, GAEB, Excel)
- Preise aus alten LVs inflationsbereinigt vorschlagen
- Material bei Lieferanten anfragen und Preise mit Preisgarantie cachen
- Angebot als GAEB X84, PDF oder Excel ausgeben

Alle KI-Funktionen laufen lokal (Xeon-Server mit RTX 3070). Architektur, Entscheidungen und Meilensteine stehen in [docs/ARCHITEKTUR.md](docs/ARCHITEKTUR.md).

## Aufbau

| Pfad | Inhalt |
|---|---|
| `crates/doc-ir` | Zwischenrepräsentation: Tokens mit Bbox, Konfidenz und Herkunft, Zeilenbildung |
| `crates/lv-model` | LV-Domänenmodell: Titel/Positionen, OZ, Einheiten, deutsche Zahlen, Review-Felder (WASM-tauglich) |
| `crates/api-types` | Gemeinsame API- und Live-Event-Typen für Server und Client (WASM-tauglich) |
| `crates/pdf` | PDFium-Anbindung: Textebene → Wort-Tokens, Seiten-Triage (Textebene oder OCR?), Rendern |
| `crates/ocr` | `OcrEngine`-Trait und PaddleOCR über `oar-ocr` (CPU/CUDA) |
| `crates/gaeb` | GAEB DA XML 3.x lesen (X81–X86, tolerant, ISO-8859-1) und X84-Angebotsabgabe aus der Original-X83 schreiben |
| `crates/export` | Bepreistes LV als Excel mit Formeln, Titelsummen und MwSt. |
| `apps/bench` | M0-Benchmark für Textebene und OCR an echten LVs |

## Entwicklung unter Windows

Voraussetzungen: `rustup` und die Visual Studio Build Tools mit C++-Workload.

```powershell
winget install Rustlang.Rustup
winget install Microsoft.VisualStudio.2022.BuildTools --override "--wait --quiet --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
```

**Das Repo liegt auf einem Netzlaufwerk.** Von dort lassen sich keine Build-Skripte ausführen, und Kompilieren über SMB ist langsam. Deshalb das Build-Verzeichnis auf die lokale Platte legen:

```powershell
$env:CARGO_TARGET_DIR = "$env:LOCALAPPDATA\cargo-target\angebots"
cargo test --workspace
```

Dauerhaft geht das mit `setx CARGO_TARGET_DIR "%LOCALAPPDATA%\cargo-target\angebots"`.

### PDFium

PDFium wird zur Laufzeit geladen und nicht eingecheckt:
1. `pdfium-win-x64.tgz` (bzw. `pdfium-linux-x64.tgz`) von [bblanchon/pdfium-binaries](https://github.com/bblanchon/pdfium-binaries/releases) herunterladen.
2. Nach `vendor/pdfium/` entpacken. Das Tool findet dann `vendor/pdfium/bin/pdfium.dll` bzw. `vendor/pdfium/lib/libpdfium.so` automatisch.
3. Alternativ `PDFIUM_DIR` setzen.

## M0-Benchmark

Misst an echten LVs, bevor die Import-Pipeline darauf aufbaut. Beispiel-LVs gehören nach `corpus/private/`; der Ordner wird nicht eingecheckt.

```powershell
# Welche Seiten haben eine brauchbare Textebene, welche brauchen OCR?
cargo run --release -p bench -- text corpus/private

# OCR-Presets vergleichen. Die Modelle lädt oar-ocr beim ersten Lauf nach ~/.oar.
cargo run --release -p bench -- ocr corpus/private --preset v5-latin --preset v6-small --json bench-out/ocr.json --dump-dir bench-out/texte
```

So misst `bench ocr` die Zeichenfehlerrate:
- **Digitale Seiten:** Die Textebene dient automatisch als Referenz. Das ist optimistisch, weil die Seite sauber gerendert ist.
- **Scans:** Hier braucht es Ground-Truth-Dateien neben dem PDF, `<name>.p<Seite>.txt` (Seite 1-basiert). Bei Bildern heißt die Datei `<name>.txt`.

Auf dem Linux-Server mit GPU:

```bash
docker compose --profile bench build bench
docker compose --profile bench run --rm bench ocr /data --preset v5-latin --preset v6-small --device cuda --json /out/ocr.json
```
