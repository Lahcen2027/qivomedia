-- Security hardening: richer sessions, two-factor authentication, org 2FA policy.

ALTER TABLE sessions ADD COLUMN id TEXT;
ALTER TABLE sessions ADD COLUMN created_at TEXT;
ALTER TABLE sessions ADD COLUMN last_used_at TEXT;
ALTER TABLE sessions ADD COLUMN ip TEXT;
ALTER TABLE sessions ADD COLUMN user_agent TEXT;
ALTER TABLE sessions ADD COLUMN mfa INTEGER NOT NULL DEFAULT 0;
UPDATE sessions SET id = substr(token_hash, 1, 16),
                    created_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'),
                    last_used_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now');
CREATE UNIQUE INDEX idx_sessions_id ON sessions(id);
CREATE INDEX idx_sessions_user ON sessions(user_id);

ALTER TABLE users ADD COLUMN mfa_enabled INTEGER NOT NULL DEFAULT 0;
ALTER TABLE users ADD COLUMN mfa_secret TEXT;
ALTER TABLE users ADD COLUMN mfa_pending_secret TEXT;
ALTER TABLE users ADD COLUMN mfa_recovery TEXT NOT NULL DEFAULT '[]';   -- JSON array of SHA-256 hashes
ALTER TABLE users ADD COLUMN mfa_last_step INTEGER NOT NULL DEFAULT 0;  -- replay protection

ALTER TABLE organizations ADD COLUMN require_mfa INTEGER NOT NULL DEFAULT 0;
