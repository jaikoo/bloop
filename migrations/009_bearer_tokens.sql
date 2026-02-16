-- Bearer tokens for API access (scoped, hashed)
CREATE TABLE bearer_tokens (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    token_hash  TEXT NOT NULL UNIQUE,
    token_prefix TEXT NOT NULL,
    project_id  TEXT NOT NULL REFERENCES projects(id),
    created_by  TEXT NOT NULL REFERENCES webauthn_users(id),
    scopes      TEXT NOT NULL,    -- JSON array: ["errors:read","alerts:read"]
    created_at  INTEGER NOT NULL,
    expires_at  INTEGER,          -- NULL = no expiry
    last_used_at INTEGER,
    revoked_at  INTEGER           -- soft delete
);
CREATE INDEX idx_bearer_token_hash ON bearer_tokens(token_hash) WHERE revoked_at IS NULL;
CREATE INDEX idx_bearer_project ON bearer_tokens(project_id);
