-- One-time tokens for password resets and invitations (stored hashed).
CREATE TABLE password_tokens (
    token_hash  TEXT PRIMARY KEY,
    user_id     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    purpose     TEXT NOT NULL CHECK (purpose IN ('reset','invite')),
    expires_at  TEXT NOT NULL,
    used_at     TEXT,
    created_at  TEXT NOT NULL
);
CREATE INDEX idx_password_tokens_user ON password_tokens(user_id);
