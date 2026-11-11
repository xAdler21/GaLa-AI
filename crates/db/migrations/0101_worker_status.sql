-- Lebenszeichen der Worker für das Admin-Panel (Dokumenten-Index).
-- Jeder Worker schreibt etwa alle 30 Sekunden seine Zeile (UPSERT über den Namen).

CREATE TABLE worker_status (
    -- GALA_WORKER_NAME, sonst der Hostname (im Container: die Container-ID)
    name        text PRIMARY KEY,
    -- cpu | vision
    role        text NOT NULL,
    -- Nur Rolle vision: Gerät, auf dem die OCR tatsächlich läuft (cpu, cuda:0)
    ocr_device  text,
    -- Commit, aus dem das Image gebaut wurde (GALA_BUILD_REV), falls bekannt
    build_rev   text,
    started_at  timestamptz NOT NULL,
    last_seen   timestamptz NOT NULL DEFAULT now()
);
