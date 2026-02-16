CREATE TABLE IF NOT EXISTS webauthn_users (
    id           TEXT PRIMARY KEY,
    username     TEXT NOT NULL UNIQUE,
    display_name TEXT NOT NULL,
    created_at   INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS webauthn_credentials (
    id              INTEGER PRIMARY KEY,
    user_id         TEXT NOT NULL REFERENCES webauthn_users(id),
    credential_json TEXT NOT NULL,
    name            TEXT,
    created_at      INTEGER NOT NULL,
    last_used_at    INTEGER
);
CREATE INDEX IF NOT EXISTS idx_creds_user ON webauthn_credentials(user_id);

CREATE TABLE IF NOT EXISTS sessions (
    token      TEXT PRIMARY KEY,
    user_id    TEXT NOT NULL REFERENCES webauthn_users(id),
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_sessions_exp ON sessions(expires_at);
