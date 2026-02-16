ALTER TABLE webauthn_users ADD COLUMN is_admin INTEGER NOT NULL DEFAULT 0;

-- Upgrade path: mark first existing user as admin
UPDATE webauthn_users SET is_admin = 1 WHERE rowid = (
    SELECT rowid FROM webauthn_users ORDER BY created_at ASC LIMIT 1
);

CREATE TABLE IF NOT EXISTS invites (
    token       TEXT PRIMARY KEY,
    created_by  TEXT NOT NULL REFERENCES webauthn_users(id),
    created_at  INTEGER NOT NULL,
    expires_at  INTEGER NOT NULL,
    used_by     TEXT REFERENCES webauthn_users(id),
    used_at     INTEGER
);
