-- Preis-Historie (M3): Einheitspreise aus GAEB-Abgaben im Archiv (Phase 84–86). Der Worker
-- schreibt sie beim Extrahieren eines GAEB-Dokuments; Vorschläge über Trigramm-Ähnlichkeit
-- des Kurztexts.

CREATE EXTENSION IF NOT EXISTS pg_trgm;

CREATE TABLE price_ref (
    id           bigserial PRIMARY KEY,
    document_id  uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE,
    phase        smallint NOT NULL,
    lv_name      text NOT NULL,
    oz           text NOT NULL,
    kurztext     text NOT NULL,
    langtext     text NOT NULL DEFAULT '',
    menge        numeric,
    einheit      text,
    ep           numeric NOT NULL,
    art          text NOT NULL DEFAULT 'normal',
    created_at   timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX price_ref_document_idx ON price_ref (document_id);
CREATE INDEX price_ref_kurztext_trgm_idx ON price_ref USING gin (kurztext gin_trgm_ops);

-- Bereits indizierte GAEB-Dateien noch einmal extrahieren: so kommen ihre Preise in die
-- Historie, und GAEB-90-Dateien werden jetzt strukturiert statt als Rohtext gelesen.
INSERT INTO index_task (document_id, kind, priority)
SELECT id, 'extract', 5 FROM document WHERE kind = 'gaeb';
