-- Leistungsverzeichnisse (M1): importierte Ausschreibung, Original für den Export, bepreistes LV.

CREATE TABLE lv (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    name        text NOT NULL,
    -- Quellformat, z. B. 'X83', 'D83'; bestimmt, welche Abgabe (X84/D84) möglich ist.
    format      text NOT NULL,
    file_name   text NOT NULL,
    -- Originaldatei unverändert; X84/D84 werden daraus erzeugt. GAEB-Dateien sind klein.
    original    bytea NOT NULL,
    -- lv_model::Lv als JSON.
    lv          jsonb NOT NULL,
    warnings    jsonb NOT NULL DEFAULT '[]',
    -- Kennzahlen für die Liste, beim Speichern aus dem LV berechnet.
    positions   integer NOT NULL DEFAULT 0,
    priced      integer NOT NULL DEFAULT 0,
    total       text NOT NULL DEFAULT '0',
    -- Optimistisches Sperren: Speichern nur mit der gelesenen Revision.
    revision    integer NOT NULL DEFAULT 1,
    created_by  uuid REFERENCES app_user (id) ON DELETE SET NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX lv_updated_idx ON lv (updated_at DESC);
