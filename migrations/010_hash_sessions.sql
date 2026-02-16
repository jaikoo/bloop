-- Migration 010: Clean up plaintext session tokens.
-- Session tokens are now stored as SHA-256 hashes (64-char hex).
-- Existing plaintext tokens (43-char base64) will never validate
-- against the new hashed lookup, so delete them as cleanup.
DELETE FROM sessions WHERE length(token) < 64;
