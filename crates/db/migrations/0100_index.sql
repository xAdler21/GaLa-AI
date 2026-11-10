-- Dokumenten-Index für den Chatbot: Dokumente, Chunks (Volltext + Vektor), Index-Aufgaben.
-- Die Erweiterung `vector` legt 0001_init.sql an.

CREATE TABLE document (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    -- Pfad relativ zu GALA_ARCHIV_ROOT, immer mit '/' als Trenner.
    rel_path    text NOT NULL UNIQUE,
    size        bigint NOT NULL,
    mtime       timestamptz NOT NULL,
    sha256      text NOT NULL,
    -- pdf | xlsx | gaeb | text
    kind        text NOT NULL,
    -- new -> extracting -> indexed | failed
    status      text NOT NULL DEFAULT 'new'
                CHECK (status IN ('new', 'extracting', 'indexed', 'failed')),
    pages       int,
    error       text,
    indexed_at  timestamptz,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX document_status_idx ON document (status);

CREATE TABLE chunk (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    document_id uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE,
    -- 1-basiert; NULL bei Dateien ohne Seiten
    page        int,
    -- Reihenfolge innerhalb des Dokuments
    ord         int NOT NULL,
    text        text NOT NULL,
    emb         vector(1024),
    tsv         tsvector GENERATED ALWAYS AS (to_tsvector('german', text)) STORED,
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX chunk_document_idx ON chunk (document_id, ord);
CREATE INDEX chunk_emb_hnsw_idx ON chunk USING hnsw (emb vector_cosine_ops);
CREATE INDEX chunk_tsv_gin_idx ON chunk USING gin (tsv);

-- Bewusst kleine Aufgabenwarteschlange (FOR UPDATE SKIP LOCKED, Lease + Heartbeat, Retry mit
-- Höchstzahl an Versuchen). Sie ist der Vorläufer des geplanten `jobs`-Crates und wird dort
-- aufgehen.
CREATE TABLE index_task (
    id          bigserial PRIMARY KEY,
    document_id uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE,
    kind        text NOT NULL CHECK (kind IN ('extract', 'ocr_page', 'embed')),
    -- 1-basiert, nur für ocr_page
    page        int,
    -- höher = zuerst
    priority    int NOT NULL DEFAULT 0,
    status      text NOT NULL DEFAULT 'pending'
                CHECK (status IN ('pending', 'running', 'done', 'failed')),
    attempts    int NOT NULL DEFAULT 0,
    lease_until timestamptz,
    worker      text,
    error       text,
    not_before  timestamptz NOT NULL DEFAULT now(),
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX index_task_claim_idx ON index_task (kind, priority DESC, id)
    WHERE status IN ('pending', 'running');
CREATE INDEX index_task_document_idx ON index_task (document_id);
