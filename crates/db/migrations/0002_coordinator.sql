-- Coordinator: Nutzer, Sitzungen, Einstellungen, Releases, Audit-Log.

CREATE TABLE app_user (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    username    text NOT NULL UNIQUE,
    pw_hash     text NOT NULL,
    role        text NOT NULL CHECK (role IN ('admin', 'kalkulator')),
    disabled    boolean NOT NULL DEFAULT false,
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE session (
    token_hash   bytea PRIMARY KEY,
    user_id      uuid NOT NULL REFERENCES app_user (id) ON DELETE CASCADE,
    kind         text NOT NULL CHECK (kind IN ('desktop', 'web')),
    device_name  text,
    created_at   timestamptz NOT NULL DEFAULT now(),
    expires_at   timestamptz NOT NULL,
    last_seen    timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX session_user_idx ON session (user_id);
CREATE INDEX session_expires_idx ON session (expires_at);

CREATE TABLE setting (
    key         text PRIMARY KEY,
    value       jsonb NOT NULL,
    updated_at  timestamptz NOT NULL DEFAULT now(),
    updated_by  text
);

CREATE TABLE release (
    id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    version       text NOT NULL UNIQUE,
    platform      text NOT NULL DEFAULT 'windows-x86_64',
    file_name     text NOT NULL,
    signature     text NOT NULL,
    notes         text NOT NULL DEFAULT '',
    published     boolean NOT NULL DEFAULT false,
    created_at    timestamptz NOT NULL DEFAULT now(),
    published_at  timestamptz,
    created_by    text
);

CREATE TABLE audit_log (
    id       bigserial PRIMARY KEY,
    at       timestamptz NOT NULL DEFAULT now(),
    user_id  uuid,
    action   text NOT NULL,
    detail   jsonb NOT NULL DEFAULT '{}'::jsonb
);
CREATE INDEX audit_log_at_idx ON audit_log (at DESC);
