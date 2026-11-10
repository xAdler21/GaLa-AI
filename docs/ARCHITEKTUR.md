# GaLa-AI – Architektur & Umsetzungsplan

> GaLa-AI vereint zwei Vorhaben: die Angebotsautomatisierung (dieses Dokument, ab „Kontext“) und den Dokumenten-Chatbot über das Netzlaufwerk (Abschnitt D).
>
> Seit November 2026 ist die Hardware auf drei VMs verteilt (Abschnitt „VM-Aufteilung“). Wo ältere Abschnitte von „der 3070“ als einziger GPU sprechen, gilt die VM-Aufteilung. Die Verträge zwischen den Teilen stehen in [CLAUDE.md](../CLAUDE.md).

## Kontext

Angebote auf GaLaBau-Ausschreibungen zu erstellen ist heute zäh und manuell: LV lesen (oft als PDF, alte LVs nur auf Papier), jede Position bepreisen, Material bei Herstellern/Händlern anfragen, Preise nachhalten, Angebot abgeben. Ziel ist ein System, das diesen Prozess weitgehend automatisiert und dem Nutzer dabei jederzeit live zeigt, was es gerade tut.

Es soll:
- LVs aus beliebigen Quellen einlesen (Worst Case: gescannte PDFs per OCR),
- Preise aus ~10 Jahren alter LVs inflationsbereinigt vorschlagen,
- Materialanfragen erzeugen und Lieferantenpreise bis zum Ende ihrer Preisgarantie cachen,
- das Angebot als GAEB X84, PDF oder Excel ausgeben.

Alle KI-Funktionen laufen lokal (8-Core-Xeon, DDR4, RTX 3070 mit 8 GB).

Stand:
- **M0 begonnen:** Workspace, Kern-Crates (`doc-ir`, `lv-model`, `api-types`, `pdf`, `ocr`) und das Benchmark-Tool `apps/bench` existieren.
- **M1 begonnen:** GAEB-Import/X84-Export (`gaeb`) und Excel-Export (`export`) stehen.
- **Offen in M1:**
  - GAEB 90 (D83)
  - Datenbank, Job-Framework, Server, Leptos-UI

### Geklärte Entscheidungen
| Thema | Entscheidung |
|---|---|
| Eingangsformate | Alles: PDF mit Textebene, gescannte PDFs (der gesamte Altbestand), GAEB (X83/X84, GAEB 90 D83/P83), Excel. OCR ist Kernfeature. |
| Ausgabe | GAEB X84, PDF-Angebot und Excel, pro Angebot wählbar |
| Materialanfragen | Alle drei Modi bauen (nur Dokumente / Mail nach Freigabe / vollautomatisch), umschaltbar per Feature-Flag |
| Kalkulation | Beide Modi: EP-basiert (ähnliche alte EPs × Index) **und** Aufgliederung (Lohn/Material/Gerät/Fremdleistung/Zuschläge) |
| Clients | **Nur Desktop-App** (Tauri v2 + Leptos) mit Installer und Auto-Updater. Keine Web-App für Endnutzer (Entscheidung des Nutzers, November 2026). Der Coordinator liefert ein kleines Web-Panel für Download und Administration. |
| Server | Linux, nur LAN/VPN, 1–5 Nutzer, Altbestand > 500 LVs; drei VMs (LLM, OCR, Coordinator) |
| Dokumenten-Chatbot | Teil von GaLa-AI: indexiert das Netzlaufwerk (überwiegend Papier → OCR) und beantwortet Fragen mit Quellen |
| Claude-Zugang | Nutzer `claude` mit sudo auf allen VMs; im Admin-Panel per Schalter ein- und ausschaltbar |

### Zur Sprachwahl Rust/WASM
Rust ist hier eine gute Wahl, allerdings aus anderen Gründen als reiner Rechengeschwindigkeit:
- **Rechenzeit:** Der eigentliche Rechenaufwand steckt in der GPU-Inferenz (OCR, LLM). Die läuft in C++/CUDA, egal aus welcher Sprache sie aufgerufen wird.
- **Robustheit:** Rust liefert eine robuste, ressourcenschonende Orchestrierung ohne GC-Pausen.
- **Gemeinsame Typen:** Server und Leptos-Client teilen sich dieselben API-Typen.
- **Kein Python nötig:** Das ist 2026 möglich, weil
  - `oar-ocr` PaddleOCR nativ über ONNX Runtime/CUDA ausführt und
  - das LLM per `llama-server` angebunden wird.

**Fallback:** Liefert `oar-ocr` in den Benchmarks (M0) nicht die nötige Qualität, kommt ein Python-OCR-Sidecar (PaddleOCR/docling) hinter dasselbe `OcrEngine`-Trait. Der Rest des Systems bleibt davon unberührt.

**WASM im Frontend** bringt bei Tabellen mit tausenden Zeilen keinen messbaren Vorteil gegenüber JS: Entscheidend ist Virtualisierung, und `leptos-struct-table` hat virtuelles Scrolling. Der Vorteil ist die einheitliche Sprache. Der Nachteil sind weniger fertige UI-Komponenten. Das ist ein akzeptierter Trade-off.

---

## Tech-Stack (Stand Oktober 2026)

| Bereich | Wahl | Begründung |
|---|---|---|
| API-Server | axum 0.8, tokio | Standard, WebSocket/SSE eingebaut |
| DB | PostgreSQL 17 + pgvector, sqlx 0.9 | Relationale Daten und Vektorsuche in einer DB; SQL-lastige Preisabfragen |
| Job-Queue | Eigene Postgres-Queue (`FOR UPDATE SKIP LOCKED`, ~600 Zeilen) | apalis/underway können keine GPU-Phasen-Affinität und Prioritäts-Aging. Kein zusätzlicher Broker, crash-sicher. |
| Live-Events | Postgres `LISTEN/NOTIFY` → tokio `broadcast` → **SSE** | Fortschritt fließt nur in eine Richtung; automatischer Reconnect mit Replay, funktioniert auch im Tauri-Webview |
| PDF | `pdfium-render` (MIT/Apache) | Text mit Zeichen-Bounding-Boxes und Rendering in 300 dpi. **Nicht** MuPDF (AGPL). |
| OCR | `oar-ocr` 0.10: PP-OCRv6 Latin, PP-DocLayout, SLANeXt (Tabellen), ort + CUDA | Apache-2.0, nativ in Rust |
| OCR-Fallback | PaddleOCR-VL-1.6 / GLM-OCR (~0,9B) über `oar-ocr-vl` | Für schwierige Seiten und Handschrift (handschriftliche EPs in alten LVs) |
| LLM | `llama-server` (llama.cpp) mit Qwen3.5-9B Q4_K_M, Thinking aus, JSON-Schema-Output | Passt in 8 GB. Ministral 3 8B wird parallel evaluiert. |
| Embeddings | `fastembed` mit BGE-M3 (dense + sparse) und `bge-reranker-v2-m3` | Beste kommerziell nutzbare deutsche Retrieval-Qualität in dieser Größe |
| GAEB | Eigener Crate: toleranter quick-xml-Reader; X84-Writer aus XSD-generierten Typen (`xsd-parser`); Parser für GAEB 90 | Kein reifer Rust-Crate vorhanden. `pyGAEB` dient nur in CI als Referenz-Validator. |
| PDF-Ausgabe | `typst` als Library | Briefkopf- und Angebotsvorlagen als editierbare Templates |
| Excel | `calamine` (Lesen), `rust_xlsxwriter` (Schreiben) | Reif und schnell |
| Mail | `lettre` (SMTP), `async-imap` | Anfragen versenden, Antworten einlesen |
| Preisindizes | Destatis GENESIS REST API, mit DBnomics als Fallback | Quartalsindex „Landschaftsbauarbeiten“ (`BAUPRIL01`, Cube `61261BV004`), dazu Erzeugerpreisindizes für Material |
| Frontend | Leptos 0.8 (CSR) mit `leptos-struct-table`; Tauri 2 als Desktop-Hülle mit Updater | Entscheidung des Nutzers |
| Auth | Lokale Accounts mit argon2 und Session-Cookies; Rollen admin/kalkulator | LAN/VPN, 1–5 Nutzer |
| Deployment | docker compose auf Linux (NVIDIA Container Toolkit) | Container: postgres, server, worker, llama-server |

---

## VM-Aufteilung (ab November 2026)

```
                       ┌──────────────── gala-coordinator (keine GPU) ────────────────┐
 Desktop-App (Tauri) ──┼─► Caddy :443 ─► coordinator (axum)                            │
 Web-Panel (Browser) ──┘                   REST /api/v1 · Web-Panel · Update-Server    │
                       │                 worker --role cpu (Scan, Text, Chunking)      │
                       │                 Postgres 17 + pgvector · Datei-Store (NFS)    │
                       └───────────┬───────────────────────────────┬──────────────────┘
                                   │ Postgres, NFS                 │ HTTP (OpenAI-API)
              ┌────────────────────▼───────────┐       ┌───────────▼──────────────────────┐
              │ gala-ocr: GTX 950 (2 GB)       │       │ gala-llm: RTX 3070 + GTX 1070    │
              │ worker --role vision           │──────►│ llama-server, 16 GB Layer-Split  │
              │ OCR (oar-ocr), Embeddings      │       │ 12–14B-Modell, Treiber 580       │
              └────────────────────────────────┘       └──────────────────────────────────┘
               Netzlaufwerk (NAS) read-only unter /mnt/archiv auf gala-coordinator und gala-ocr
```

Was sich gegenüber dem Einzelserver-Entwurf weiter unten ändert:
- **GPU-Arbiter:** Der Phasenwechsel zwischen VISION und LLM auf einer 8-GB-Karte entfällt, weil OCR und LLM eigene GPUs haben. Der Arbiter schrumpft zu einer Ressourcen-Zuordnung: `gpu_vision`-Tasks laufen auf gala-ocr, `gpu_llm`-Aufrufe gehen per HTTP an gala-llm.
- **LLM:** 16 GB über zwei Karten erlauben 12–14B-Modelle statt 7–9B. Der Layer-Split braucht kaum PCIe-Bandbreite; die langsamere 1070 kostet etwas Tempo (geschätzt 20–30 Token/s).
- **OCR-GPU:** Die GTX 950 hat nur 2 GB und Maxwell (sm_52). OCR und Embedding passen hinein. Das VLM für Handschrift (~0,9B) passt nicht; es läuft bei Bedarf auf gala-llm oder auf der CPU. Ob ONNX Runtime/CUDA die 950 noch unterstützt, klärt M0. Fallback ist CPU-OCR.
- **Clients:** Es gibt nur die Desktop-App. Das Web-Panel des Coordinators dient nur dem Download und der Administration.

## Systemarchitektur

```
 Web-Client (Leptos/WASM) ─┐                     ┌─ worker --role gpu  (genau 1×, besitzt die 3070)
 Desktop (Tauri + Leptos) ─┼─ REST + SSE ─► api ─┤    VISION-Phase: oar-ocr (PP-OCRv6, Layout, SLANeXt, VLM)
                           │   (axum, kein CUDA) │    LLM-Phase:    startet/stoppt llama-server als Kindprozess
                           │                     ├─ worker --role cpu  (~4×, je eigenes PDFium)
                           │                     │    Render, Struktur-Parser, Embeddings, Preise, SMTP/IMAP, Indizes
                           └──────────── PostgreSQL + pgvector (Jobs, Events, Daten)  +  Datei-Store (sha256)
```

Die Prozesse kommunizieren **nur über Postgres und den Datei-Store**. Das bringt drei Vorteile:
- Ein CUDA-OOM oder Segfault im GPU-Worker reißt die API nicht mit.
- PDFium ist nicht thread-safe; mehrere Prozesse umgehen das.
- Jeder Teil startet unabhängig neu (systemd/compose restart).

### Cargo-Workspace
```
crates/
  api-types      (wasm)  API-DTOs + Event-Enum, geteilt von Server und Client
  lv-model       (wasm)  LV-Baum: Titel/Position/Vorbemerkung, Einheiten, Positionsarten
  pricing-core   (wasm)  Aggregation/Index-Mathematik → Client rechnet beim Ab-/Anwählen sofort neu
  doc-ir                 Token/Line/Doc-Zwischenrepräsentation mit Bbox, Konfidenz, Herkunft
  lv-parse               Seiten-Triage, Kopf/Fuß-Erkennung, Spaltenmodell, Segmentierung, Zahl-/Einheiten-Parser
  gaeb                   DA-XML-3.x-Reader (tolerant), GAEB-90-Parser, X84-Writer
  pdf                    pdfium-render-Wrapper (Text+Bbox, 300-dpi-Render)
  ocr                    OcrEngine-Trait; oar-ocr-Implementierung (+ optional Python-Sidecar)
  llm                    llama-server-Client (JSON-Schema-Output) + Prozess-Supervisor
  retrieval              fastembed (BGE-M3 dense/sparse), Reranker, Hybrid-Suche
  pricing                Engine: Kandidaten → pricing-core, Aufgliederungsmodus, Kalibrierung
  materials              Material-Extraktion, Normalisierung, Bündelung, Anfragen, Preis-Cache
  indices                GENESIS-/DBnomics-Client, Verkettung, Prognose
  mail                   lettre/async-imap, Referenz-Matching
  export                 typst-PDF, xlsx, X84
  db                     sqlx-Queries + Migrationen
  jobs                   Queue, DAG, Leases, GPU-Arbiter, Events
apps/  server · worker · web (Leptos) · desktop (Tauri v2)
xtask/ eval, golden-update, bench          corpus/ (git-lfs, Golden-Files)
```

---

## A. LV-Einlesen und Strukturerkennung

**Eine Zwischenrepräsentation für alle Quellen.** Textebene, OCR, VLM und Tabellenzellen werden alle zu `Token { text, bbox, conf, src, page }`. Daraus entsteht ein seitenübergreifender Zeilenstrom.

Jedes Feld wird als `FieldValue<T> { value, raw, conf, prov: Vec<TokenRef>, flags }` gespeichert. Damit kann das UI zu jedem Wert die Fundstelle als Bbox auf dem Seitenbild zeigen.

**Pipeline**
1. **Seiten-Triage (pro Seite).** Die Textebene wird benutzt, wenn
   - genug Zeichen vorhanden sind,
   - der Anteil an Garbage-Glyphen niedrig ist (kaputte ToUnicode-Maps sind häufig) und
   - die Wörterbuch-Trefferquote stimmt.

   Sonst wird die Seite mit 300 dpi gerendert, geradegerückt (Deskew) und per OCR gelesen. Gemischte PDFs funktionieren dadurch.
2. **Kopf-/Fußzeilen.** Wiederkehrende Zeilen in den oberen/unteren ~12 % (auf ≥ 40 % der Seiten, Ziffern normalisiert) werden entfernt. „Übertrag“, „Summe Titel“ und „Seite # von #“ bleiben als Prüfsummen erhalten.
3. **Spaltenmodell pro Dokument.**
   - OZ-Anker per Regex; die OZ-Maske (z. B. 2.2.4) per Mehrheitsentscheid.
   - Die rechten Kanten der Zahlen werden geclustert (KDE). Daraus ergeben sich die Spalten Menge/Einheit/EP/GP. EP und GP werden über die Prüfung `Menge × EP ≈ GP` zugeordnet.
   - Überschriften wie Pos/OZ/Menge/ME/EP/GP dienen als Hinweis (Prior).
   - **Layout-Fingerprint je AVA-Software:** Bestätigte Spaltentemplates werden gecacht, sodass jedes neue Layout nur einmal gelernt wird.
4. **Segmentierung als Zustandsautomat** über den Zeilenstrom. Langtexte über Seitenumbrüche hinweg sind damit automatisch gelöst.
   - Der Automat erkennt Titel, Position, Vorbemerkung und Kurz-/Langtext.
   - Trennstriche am Zeilenende werden nur entfernt, wenn das zusammengefügte Wort im Lexikon steht.
   - Der Zustand ist per serde checkpointbar. Dadurch kann das Parsen inkrementell laufen, während OCR noch Seiten liefert.
5. **Parsing.**
   - Das Zahlenformat (`1.234,56`) wird per Dokument-Mehrheit bestimmt.
   - OCR-Korrekturen (O→0, l→1) gelten nur in Zahlenspalten.
   - Einheiten werden normalisiert (qm/m2/m² → M2, cbm → M3, St/Stk → STK, lfm → M, psch, t, h). Der Rohtext bleibt erhalten.
6. **Positionsarten.** Bedarfs-/Eventualposition, Alternativ/Wahl mit Bezug „zu Pos. X“, Zulage und Stundenlohn werden per Schlüsselwort erkannt.
7. **Konfidenz und Review.**
   - Berechnung: `conf = min(Token-conf) × Regel-Marge × Checks`.
   - Checks: GP = Menge×EP, Summe Positionen = Titelsumme, OZ aufsteigend und maskenkonform, Einheit bekannt.
   - `needs_review` wird gesetzt, wenn die Schwelle unterschritten wird. Für EP und Menge ist sie streng, für den Langtext locker.
8. **Handschriftliche EPs** (Altbestand). Die Zellen werden anhand des Spaltenmodells ausgeschnitten und vom VLM mit einem Prompt gelesen, der nur Ziffern zulässt. Stimmt zusätzlich `Menge × EP = GP`, gilt der Wert als hoch-konfident.

**Das LLM kommt nur zum Einsatz, wenn die Regeln nicht reichen:** bei Freitext-LVs ohne Spaltenmodell, unklaren Kurz-/Langtext-Grenzen, dem Bezug einer Alternativposition und der Attribut-Extraktion.

Das LLM bekommt nummerierte Zeilen und **antwortet nur mit Zeilenindizes**. Es tippt keinen Text ab und kann daher keine Werte halluzinieren:

```json
{"blocks":[{"type":"titel|position|vorbemerkung|other","oz":"…","lines":[a,b],"kurztext_lines":[…],
  "menge_line":n,"einheit_line":n,"kind":"normal|bedarf|alternativ|zulage|stundenlohn","grund_oz":"…"}]}
```

LLM-Felder sind auf conf 0,8 gedeckelt.

**GAEB und Excel landen im selben `Lv`-Modell.**
- **DA XML 3.x:**
  - `BoQCtgy` → Titel, `Item` → Position.
  - `OutlineText` → Kurztext, `DetailTxt` → Langtext.
  - `BoQBkdn` → OZ-Maske.
  - conf = 1,0.
- **GAEB 90 (D83/P83):** tabellengetriebener Parser für die Satzarten.
- **Excel:** calamine plus derselbe Spalten-Klassifikator.

---

## B. Preis-Engine

**Datenmodell (Kern)**
```
hist_position(id, lv_id, offer_date, base_quarter, oz, kurz, lang, unit_canon, unit_dim, qty, ep,
              kind, trade_class, chapter_path, attrs jsonb, quality, reviewed,
              emb vector(1024), sparse sparsevec)                       -- HNSW-Indizes
price_index(series, quarter, value, base_year, source, fetched_at)      -- + verkettete View
cost_share(trade_class, component[lohn|material|geraet|fremd], series, share)
suggestion(id, position_id, mode, ep, p20, p80, conf, method_ver, params)
suggestion_ref(suggestion_id, hist_id, score, weight, ep_raw, f_index, f_qty, ep_adj, excluded_reason)
override(position_id, suggestion_id, ep_final, reason, ref_toggles, user_id, at)
breakdown(position_id, minutes, wage, equip, sub, zuschlaege)
breakdown_material(position_id, material_id, qty_per_unit, waste, supplier_price_id, price, indexed)
bid_outcome(lv_id, status, our_total, winner_total, rank)
```

**Retrieval (hybrid)**
1. Die Anfrage besteht aus dem Kurztext plus den ersten ~200 Tokens des Langtexts.
2. Harte Filter:
   - Die Einheitendimension muss kompatibel sein. M3↔T geht nur mit Dichte-Attribut und wird markiert.
   - Stundenlohn wird nur mit Stundenlohn verglichen.
3. Je Top-100 aus Dense-Suche (BGE-M3) und Sparse-Suche (BGE-M3) werden per RRF zusammengeführt; davon bleiben die Top 40.
4. Reranking mit `bge-reranker-v2-m3`.
5. Score: `s = a·rerank + b·attr_match + c·kapitel_sim + d·aktualität − e·|log Mengenverhältnis|`.
6. **Attribute** kommen aus Regex-Regeln plus LLM: Maße, Dicke, Material, Farbe, DIN/ZTV, Körnung 0/32, Betonklasse C20/25, Pflanzqualität (Hei 3xv, StU 16-18). Ein Widerspruch bei einem kritischen Attribut, etwa 8 cm statt 10 cm Pflaster, gibt einen starken Abzug. Derselbe Extraktor liefert auch den Materialbedarf.

**Aggregation (EP-Modus)**
- Indexfaktor: `f_index = I(Q_jetzt) / I(Q_alt)` mit `BAUPRIL01`. Sobald Kostenanteile bekannt sind, wird stattdessen ein Mischindex `Σ s_k · I_k(jetzt)/I_k(alt)` über Lohn, Material und Gerät verwendet.
- Mengeneffekt: `f_qty = (q_neu/q_alt)^β`. β wird je Gewerkeklasse robust geschätzt (Theil–Sen) und auf [0,85; 1,2] begrenzt.
- Bereinigter Preis: `ep_adj = ep · f_index · f_qty`.
- Ausreißer: robuster z-Score auf log(ep_adj) mit Median und MAD. Bei |z| > 3 wird der Kandidat ausgeschlossen, mit Begründung.
- Gewichte: `w = softmax(s/τ) · 0,5^(Alter/3 J.) · Qualität · Ergebnis(gewonnen/verloren)`.
- **Vorschlag = gewichteter Median**, Spanne = P20–P80.
- Die Konfidenz ergibt sich aus n_eff, dem Top-Score und der IQR. Bei n_eff < 1,5 zeigt das UI „keine belastbare Referenz“ und bietet den Aufgliederungsmodus an.
- **Index-Mathematik:**
  - Basisquartal ist das Angebotsdatum des alten LVs.
  - Zielquartal ist das letzte veröffentlichte Quartal.
  - Die Fortschreibung auf die Ausführungsmitte ist optional und wird als eigener Faktor ausgewiesen.

**Aufgliederungsmodus:** `EP = (L + M + G + F) · (1 + Zuschläge)`
- **L** = Minuten × aktueller Mittellohn aus den Einstellungen.
- **M** = Σ Menge/Einheit · (1 + Verlust) · Preis.
  - Gibt es einen **gültigen Lieferantenpreis aus dem Cache**, wird dieser verwendet.
  - Sonst der letzte Preis × Material-Erzeugerpreisindex, markiert als „indexiert“.
- **G** und **F**: historische Werte × Index, oder ein aktuelles Nachunternehmerangebot.
- Bestätigte Aufgliederungen liefern die Kostenanteile s_k für den Mischindex im EP-Modus. Darüber sind die beiden Modi verzahnt.

**Lernen**
- Nach der Abgabe werden die finalen EPs zu neuen `hist_position`-Zeilen mit Qualität 1,0.
- Referenzen, die der Nutzer abwählt, werden zu Hard Negatives.
- Kalibrierfaktor je Gewerkeklasse: EWMA von log(final/vorgeschlagen), zu 1 hin gestaucht.
- Die Gewichte (a…e, τ) werden per Grid-Search gegen den **Backtest** neu eingestellt.
- Gewonnen/verloren wirkt nur als Gewicht. Ein „Marktfaktor“ aus Submissionsergebnissen wird separat angezeigt, nie stillschweigend angewendet.

**Erklärung im UI.** Zu jedem Vorschlag sieht der Nutzer:
- EP, Spannenbalken und Konfidenz-Badge.
- Eine Referenztabelle mit:
  - Projekt, Datum, OZ, Kurztext mit hervorgehobenen Unterschieden
  - Menge und alter EP
  - Indexfaktor ausgeschrieben, z. B. „Q2/2019→Q2/2026: 152,1/118,4 = 1,285“
  - f_qty, bereinigter EP und Gewicht in %
- Ausgeschlossene Referenzen mit Begründung.
- Ein Klick öffnet den Ausschnitt der Originalseite.

`pricing-core` läuft als WASM im Client. Ab- und Anwählen von Referenzen rechnet sofort neu, ohne Server-Roundtrip.

---

## C. Jobs, GPU-Scheduling und Live-Transparenz

**Queue-Tabellen**
```
job(id, kind, lv_id, priority, status, cancel_requested, created_by, at)
task(id, job_id, stage, resource[cpu|pdf|gpu_vision|gpu_llm|net], page, pos_id, status,
     priority, attempts, lease_until, worker, not_before, pipeline_ver, error)
task_dep(task_id, depends_on)        job_event(seq bigserial, job_id, ts, body jsonb)
```
- Dequeue per `FOR UPDATE SKIP LOCKED`, gefiltert nach den Ressourcen, die der Worker bedienen kann. Workers verlängern ihren Lease per Heartbeat.
- **Crash-Sicherheit pro Seite:**
  - Jedes Stufenergebnis wird idempotent gespeichert, mit Schlüssel `(doc, page, stage, pipeline_ver)`.
  - Abgelaufene Leases gehen zurück in die Queue, fertige Seiten laufen nie doppelt.
  - Ändert sich nur der Parser, wird OCR wiederverwendet. Das Neu-Strukturieren des Altbestands kostet dann keine GPU-Zeit.
- **DAG je Dokument:**
  1. triage
  2. render
  3. ocr
  4. vlm_fix
  5. structure (inkrementell)
  6. llm_resolve/attrs
  7. embed
  8. price
  9. material_extract

**GPU-Arbiter.** Es gibt zwei Phasen, weil die 8 GB VRAM nicht für alles gleichzeitig reichen:

| Phase | Inhalt | VRAM |
|---|---|---|
| VISION | OCR, Layout, Tabellen, VLM gemeinsam geladen | ~3–4 GB |
| LLM | llama-server | ~6,2 GB + KV-Cache |

- Der Arbiter bleibt in der aktuellen Phase, solange dort Arbeit wartet.
- Er wechselt, wenn höher priorisierte Arbeit der anderen Phase zu lange wartet. Grenzen: 5 s für interaktive Jobs, 10 min für den Backfill. Jede Phase läuft mindestens 60 s.
- Ablauf eines Wechsels:
  1. ort-Sessions freigeben.
  2. Freien VRAM per NVML prüfen.
  3. llama-server starten und `/health` abwarten.

  Das ist deterministischer als der Router-Modus.
- Priorität: Ein interaktives Angebot geht immer vor den Backfill. Die Priorität des Backfills altert hoch, damit er nicht verhungert.
- Ein Task, der den GPU-Worker zweimal abstürzen lässt, wird als fehlgeschlagen markiert und geht ins manuelle Review.
- **Backfill-Schätzung:** 500+ LVs × ~40 Seiten ≈ 20.000 Seiten × 1–2 s ≈ 6–11 GPU-Stunden. Das läuft nachts/am Wochenende; die Zahlen werden in M0 gemessen.

**Live-Events („immer zeigen, was es macht“)**
- Der Worker schreibt `job_event` und ruft `pg_notify` in derselben Transaktion auf.
- Die API hält einen PgListener, verteilt per tokio `broadcast` und streamt per SSE an `/jobs/{id}/events`. `Last-Event-ID` sorgt für lückenloses Replay nach einem Reconnect.
- Zähler werden auf ≤ 4 Hz gebündelt.
- Event-Typen (in `api-types`, geteilt mit dem Client):
  ```rust
  enum EventBody { StageProgress{stage,done,total,eta_s}, PageState{page,state,src,mean_conf},
    PositionsUpserted{items,provisional}, PriceSuggested{pos,ep,conf},
    GpuPhase{phase,waiting_for}, Warning{code,page,msg}, JobFinished{status,stats} }
  ```
- Das UI zeigt:
  - einen Stufen-Stepper
  - einen Seiten-Thumbnail-Streifen, eingefärbt nach Zustand (Textebene/OCR/VLM/fertig/Fehler)
  - eine virtuelle Positionstabelle, in der Zeilen **live erscheinen**: vorläufig grau, „needs review“ orange
  - auf Klick die Seite mit Bbox-Overlay
  - GPU-Wartezeiten im Klartext, z. B. „wartet auf GPU: Backfill läuft, Wechsel in ~8 s“
- **Abbruch:** Flag setzen plus NOTIFY. Laufende Tasks prüfen zwischen Seiten bzw. Requests einen CancellationToken. Teilergebnisse bleiben erhalten, der Job ist fortsetzbar.

---

## Materialanfragen und Preis-Cache

1. **Extraktion:** Das LLM zieht aus jeder Position die benötigten Materialien heraus, z. B. „Betonpflaster 20×10×8 grau, DIN EN 1338“. Format ist ein JSON-Schema mit diesen Feldern:
   - `kategorie`, `bezeichnung`
   - `attribute` (Maße, Farbe, Festigkeit, Norm)
   - `menge_je_einheit`
   - `verlust_pct`

   Die Bedarfsmenge entsteht aus der Positionsmenge mal Umrechnungsfaktor, z. B. m² Pflaster → m² Steine plus Verschnitt, oder m³ Schotter → t über die Dichte. Die Umrechnungsfaktoren pflegt man in Stammdaten.
2. **Normalisierung:** Jedes extrahierte Material wird einem kanonischen Eintrag in `material_items` zugeordnet. Dafür werden Attribute und Embedding abgeglichen. Unsichere Zuordnungen gehen in eine Review-Liste.
3. **Cache-Check vor jeder Anfrage:** Gibt es einen bestätigten Preis, dessen `gueltig_bis` die **Bindefrist bzw. den Ausführungszeitraum** des Angebots abdeckt?
   - Ja → keine Anfrage nötig.
   - Läuft die Garantie vor der Zuschlagsfrist ab → Warnung im UI.
4. **Bündelung:** Die offenen Materialien werden je Lieferant zu einer Anfrage gebündelt. Die Zuordnung läuft über die Lieferanten-Stammdaten (Kategorien, Region, Kontakt). Die Anfrage entsteht als PDF (typst) und als Excel. Der Betreff enthält eine eindeutige Referenz `[ANF-2026-0042]`.
5. **Versandmodus** (Feature-Flag `inquiry_mode`):
   - `documents_only`: Download, der Nutzer versendet selbst.
   - `approve_then_send`: Mail-Entwurf im UI, Versand per Klick.
   - `auto_send`: Versand ohne Freigabe. Abgesichert durch Limit pro Tag und eine Whitelist bekannter Lieferanten.
6. **Rücklauf:** Ein IMAP-Poll-Job (alle N Minuten) ordnet Antworten über die Referenz zu.
   - Mailtext und PDF-/Excel-Anhänge laufen durch dieselbe Import-Pipeline (Textebene/OCR → LLM-Extraktion).
   - Ergebnis sind Preiszeilen mit `gueltig_von/bis`, Mindestmengen, Fracht und Staffelpreisen.
   - Der Nutzer bestätigt die Preise, danach landen sie in `supplier_prices`.
   - Ohne IMAP lädt der Nutzer die Antwort manuell hoch.
7. **Auswertung:** Pro Material wird automatisch der günstigste gültige Preis gewählt (überschreibbar). Er fließt direkt in den Aufgliederungsmodus der Preis-Engine.

## Preisindizes

- Quartalsjob über die GENESIS REST API (`genesisWS/rest/2020`, Token im Header). Fallback ist DBnomics (offen, ohne Auth).
- Tabelle `price_index(series, quarter, value, source, fetched_at)`. Basisjahr-Umstellungen werden über Verkettung normalisiert.
- Serien:
  - `BAUPRIL01` Landschaftsbauarbeiten für Lohn/Gesamt-EP
  - Erzeugerpreisindizes für Kies/Sand/Naturstein (GP19-08121), Betonwaren und Baumschulware; die genauen Codes werden in M0 verifiziert
  - Tariflohnindex GaLaBau, optional
- Für das aktuelle, noch nicht veröffentlichte Quartal wird linear aus den letzten vier Quartalen fortgeschrieben, sichtbar markiert als „Prognose“. Die Werte lassen sich manuell überschreiben.

## Export

- **GAEB X84 (DA XML 3.3):**
  - Basiert auf dem importierten X83, damit OZ-Schema, IDs und Struktur 1:1 erhalten bleiben.
  - Wurde das LV per PDF/OCR importiert, wird ein X84 aus der rekonstruierten Struktur erzeugt, mit Hinweis, dass die Plattform es eventuell nicht akzeptiert. In diesem Fall ist PDF/Excel die primäre Abgabe.
  - CI-Validierung gegen XSD und `pyGAEB`. Die XSDs werden beim Build heruntergeladen und nicht ins Repo committet, weil die Lizenz ungeklärt ist.
- **PDF:** typst-Templates für das Angebotsschreiben und das bepreiste LV mit Briefkopf, Summen je Titel, Nachlass und MwSt.
- **Excel:** bepreistes LV mit Formeln, optional mit Aufgliederungsspalten (Lohn/Material/Gerät).

## D. Dokumenten-Chatbot

Beantwortet Fragen zum Inhalt des Netzlaufwerks, z. B. „Welche Pflastersteine hatten wir 2019 beim Kita-Projekt?“.

1. **Scan:** `worker --role cpu` läuft periodisch über `/mnt/archiv`.
   - Neue, geänderte und gelöschte Dateien erkennt er über Größe, mtime und sha256.
   - Unterstützt: PDF, Excel, GAEB, Text.
2. **Text gewinnen:** Für PDFs mit Textebene liefert sie die Seiten-Triage aus Abschnitt A. Alle anderen Seiten werden zu `ocr_page`-Aufgaben für gala-ocr. Der Altbestand ist überwiegend Papier; das OCR-Backlog ist daher der erste große Job.
3. **Chunking:** seitenbewusst und absatzbasiert, ~2.500 Zeichen mit einem Absatz Überlappung.
4. **Embeddings:** BGE-M3 (1024 Dimensionen) auf gala-ocr. Speicherung in `chunk.emb` (pgvector, HNSW) plus deutscher Volltextindex (`tsvector`).
5. **Antwort:**
   - Hybride Suche: dense + Volltext, zusammengeführt per RRF, Top 8.
   - Das LLM auf gala-llm antwortet nur aus den nummerierten Quellen und zitiert sie als [1], [2].
   - Die App zeigt Datei, Seite und Ausschnitt.

**Tipps für die Digitalisierung:**
- 300 dpi in Graustufen scannen.
- Einen Einzugsscanner verwenden.
- Die erkannten Texte zurück in die PDFs schreiben (durchsuchbare PDFs), damit auch die Windows-Suche sie findet.

Der Engpass ist das Scannen, nicht die Rechenleistung.

## Deployment und Betrieb

Details stehen in [BETRIEB.md](BETRIEB.md). Kurz:
- Drei VMs, je eine `deploy/<vm>/compose.yml`:
  - `gala-coordinator`: postgres, coordinator, worker --role cpu, caddy
  - `gala-ocr`: worker --role vision
  - `gala-llm`: llama-server
- Das Web-Panel des Coordinators bietet:
  - Download des Installers
  - Admin-Login
  - Server-Update, Wartungsmodus, Desktop-Releases
  - den Schalter für den Claude-Zugang
- Dateien (Original-PDFs, gerenderte Seiten, Exporte) liegen content-addressed (sha256) in einem Volume, die Metadaten in Postgres.
- Backups: `pg_dump` täglich plus Dateivolume.
- Tauri-Desktop-Client mit signiertem Updater. Er fragt den Coordinator (`/api/v1/updates/...`); Releases werden im Admin-Panel hochgeladen und veröffentlicht.
- Einstellungen und Feature-Flags liegen in der Tabelle `settings` und sind im Admin-UI umschaltbar.

---

## Meilensteine

Jeder Meilenstein ist für sich durchgängig nutzbar.

| # | Inhalt | Nutzbar danach |
|---|---|---|
| **M0** | **Benchmark-Spike, ca. 1 Woche.** 20–50 echte LV-Seiten (digital, Scan, handschriftlich) laufen durch PP-OCRv6 und PaddleOCR-VL/GLM-OCR auf der 3070. Gemessen werden deutsche Genauigkeit, Seiten/s und VRAM. Außerdem werden Qwen3.5-9B und Ministral 3 8B für Extraktions-Prompts verglichen und die GENESIS-Serien verifiziert. Gleichzeitig entstehen das Workspace-Gerüst, docker compose und CI. | Belastbare Stack-Entscheidung, bevor darauf gebaut wird |
| **M1** | GAEB-X83/D83-Import → LV-Tabelle (Leptos) → EP manuell → Export als X84/Excel. Dazu Job-Framework, SSE-Events und Login. | Erste echte Angebote per GAEB |
| **M2** | PDF-Import mit Textebene: Regel-Parser, Live-Vorschau, Review-UI mit Bbox-Overlay. Golden-Corpus aus PDF+X83-Paaren. Tauri-Desktop-Hülle. PDF-Angebot (typst). | Digitale PDF-LVs, alle drei Ausgabeformate |
| **M3** | Scan-OCR im GPU-Worker, VLM für Handschrift, **Backfill des Altbestands** mit Preis-Review-Queue (Ausschnitt + Wert, Bestätigung per Tastatur, sortiert nach Preiswirkung). Destatis-Index-Sync. | Historische Datenbasis steht |
| **M4** | Preis-Engine im EP-Modus: Hybrid-Retrieval, Index, Mengeneffekt, Erklär-UI, Overrides, Backtest-Gate. | **Automatische Preisvorschläge** |
| **M5** | LLM-Phase und GPU-Arbiter vollständig: Struktur-Fallback, Attribut-/Material-Extraktion, Lieferanten-Stammdaten, Anfrage-Dokumente (`documents_only`). | Materialbedarf und Anfragen aus dem LV |
| **M6** | SMTP-Versand nach Freigabe, IMAP-Rücklauf, Angebots-Parsing, Preis-Cache mit Gültigkeit → **Aufgliederungsmodus**. | Geschlossener Anfrage-Kreislauf |
| **M7** | `auto_send` hinter Feature-Flag, Kalibrierung, Lernen aus Gewonnen/Verloren, Layout-Template-Cache, Feinschliff der Performance. | Vollautomatik |

**Vom Nutzer benötigt (für M0):**
- 20–50 Beispiel-LVs, möglichst gemischt und gern mit passendem X83
- Zugang zum Server
- ein GENESIS-Account-Token (kostenlos)
- später Zugangsdaten für den Mailserver (SMTP/IMAP) bzw. ein Test-Postfach
- Lieferantenliste und Mittellohn/Zuschläge

## Hauptrisiken

| Risiko | Gegenmaßnahme |
|---|---|
| OCR-/Handschriftfehler im Altbestand verfälschen die Preise | Qualitätsgewichte, Check GP = Menge×EP, schnelle Preis-Review-Queue, M0-Benchmark vor dem Backfill |
| Gleicher Text, andere Spezifikation (8 cm vs. 10 cm) | Abzüge bei kritischen Attributen, Hard Negatives aus Nutzerfeedback, Backtest-Gate |
| VRAM-Thrashing oder CUDA-Instabilität auf 8 GB | Phasen-Arbiter mit Mindestverweildauer, NVML-Check, Prozessisolation, automatischer Neustart |
| Unzählige AVA-Layouts | Layout-Fingerprints mit Template-Cache, LLM-Fallback; den Auftraggeber wenn möglich nach der GAEB-Datei fragen |
| Index-Daten: API-Änderungen, Umbasierung, Veröffentlichungsverzug | Lokaler Cache, Verkettung, DBnomics-Fallback, manuelle Eingabe, konfigurierbares Mapping |
| Bus-Faktor bei `oar-ocr` (im Kern 1 Maintainer) | Kapselung hinter dem `OcrEngine`-Trait; Python-Sidecar als vorbereiteter Fallback |
| Lizenz der GAEB-XSDs unklar | XSDs nicht committen, beim Build laden; vor einer Veröffentlichung beim GAEB klären |

## Verifikation

- **Golden-Corpus:** 20–50 echte LVs, anonymisiert, mit von Hand erfasster Soll-Struktur als JSON. Sie decken ab:
  - digitale PDFs
  - gescannte PDFs, inklusive schlechter Scans und handschriftlicher EPs
  - GAEB D83/X83
  - Excel

  `cargo test -p lv-structure --test golden` misst pro Datei:
  - Positions-Recall/Precision
  - OZ-Genauigkeit
  - Mengen- und EP-Genauigkeit
  - Anteil der „needs review“-Felder

  CI schlägt bei einer Regression fehl.
- **GAEB:** Roundtrip-Test X83 → Modell → X84; der Export wird gegen das XSD validiert und mit `pyGAEB` (CI-Hilfsjob) gegengelesen.
- **Preis-Engine:** Backtest ohne Datenleck. Für jedes historische LV werden alle Positionen aus Daten vorhergesagt, die zeitlich *davor* liegen. Gemessen werden Median-APE, Trefferquote innerhalb ±15 % und die Abdeckung (Anteil Positionen mit Vorschlag). Das ist zugleich die Kennzahl für Verbesserungen.
- **Performance-Benchmarks (`cargo bench` / Benchmark-Skript):**
  - Seiten pro Sekunde für Textebene und OCR auf der 3070
  - Zeit vom Upload bis zum vollständig bepreisten LV (100 Positionen)
  - VRAM-Peak beim Modellwechsel
- **End-to-End:** Mit `docker compose up` startet das System. Ein Test-LV wird in Web-Client und Tauri-Client hochgeladen. Dabei muss Folgendes funktionieren:
  - Der Live-Fortschritt erscheint je Seite und Position.
  - Preise werden vorgeschlagen und erklärt.
  - Eine Materialanfrage geht im Modus `approve_then_send` gegen einen lokalen Test-Mailserver (z. B. MailHog/Mailpit).
  - Eine Antwort wird per IMAP importiert.
  - X84, PDF und Excel werden exportiert.
