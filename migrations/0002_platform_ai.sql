-- Platform administration (SaaS owner) + AI agent metering.

ALTER TABLE users ADD COLUMN platform_admin INTEGER NOT NULL DEFAULT 0;
-- Whoever created the very first account owns the platform.
UPDATE users SET platform_admin = 1
 WHERE id = (SELECT id FROM users ORDER BY created_at, rowid LIMIT 1);

ALTER TABLE organizations ADD COLUMN plan TEXT NOT NULL DEFAULT 'trial';
ALTER TABLE organizations ADD COLUMN max_screens INTEGER;               -- NULL = unlimited
ALTER TABLE organizations ADD COLUMN ai_enabled INTEGER NOT NULL DEFAULT 1;
ALTER TABLE organizations ADD COLUMN ai_monthly_limit INTEGER NOT NULL DEFAULT 50;
ALTER TABLE organizations ADD COLUMN suspended INTEGER NOT NULL DEFAULT 0;
ALTER TABLE organizations ADD COLUMN notes TEXT NOT NULL DEFAULT '';

-- Where a layout/playlist came from: 'manual' or 'ai'.
ALTER TABLE layouts ADD COLUMN origin TEXT NOT NULL DEFAULT 'manual';
ALTER TABLE playlists ADD COLUMN origin TEXT NOT NULL DEFAULT 'manual';

CREATE TABLE ai_usage (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    org_id         TEXT NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    user_id        TEXT,
    kind           TEXT NOT NULL,          -- 'create' | 'revise'
    prompt         TEXT NOT NULL,
    layout_id      TEXT,
    input_tokens   INTEGER NOT NULL DEFAULT 0,
    output_tokens  INTEGER NOT NULL DEFAULT 0,
    ok             INTEGER NOT NULL DEFAULT 1,
    error          TEXT,
    created_at     TEXT NOT NULL
);
CREATE INDEX idx_ai_usage_org_time ON ai_usage(org_id, created_at);
