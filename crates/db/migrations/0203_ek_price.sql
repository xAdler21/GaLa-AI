-- Einkaufspreise aus Lieferantenangeboten und Auftragsbestätigungen im Archiv. Der Coordinator
-- liest die Belege im Hintergrund mit dem LLM aus (`ek_scan` merkt sich, welche Fassung eines
-- Dokuments schon gelesen ist; ändert sich die Datei, wird sie neu gelesen).

CREATE TABLE ek_scan (
    document_id  uuid PRIMARY KEY REFERENCES document (id) ON DELETE CASCADE,
    sha256       text NOT NULL,
    prices       integer NOT NULL DEFAULT 0,
    error        text,
    scanned_at   timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE ek_price (
    id           bigserial PRIMARY KEY,
    document_id  uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE,
    lieferant    text NOT NULL,
    belegdatum   date,
    bezeichnung  text NOT NULL,
    menge        numeric,
    einheit      text,
    ep           numeric NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX ek_price_document_idx ON ek_price (document_id);
CREATE INDEX ek_price_bezeichnung_trgm_idx ON ek_price USING gin (bezeichnung gin_trgm_ops);
