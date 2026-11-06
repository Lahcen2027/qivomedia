-- Qivomedia core schema (SQLite; mirrors the Postgres schema planned for SaaS scale)

CREATE TABLE organizations (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    created_at  TEXT NOT NULL
);

CREATE TABLE users (
    id             TEXT PRIMARY KEY,
    org_id         TEXT NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    email          TEXT NOT NULL UNIQUE,
    name           TEXT NOT NULL,
    password_hash  TEXT NOT NULL,
    role           TEXT NOT NULL CHECK (role IN ('owner','admin','editor','viewer')),
    created_at     TEXT NOT NULL
);

CREATE TABLE sessions (
    token_hash  TEXT PRIMARY KEY,
    user_id     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    expires_at  TEXT NOT NULL
);

CREATE TABLE media (
    id           TEXT PRIMARY KEY,
    org_id       TEXT NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    kind         TEXT NOT NULL,
    mime         TEXT NOT NULL,
    size         INTEGER NOT NULL,
    sha256       TEXT NOT NULL,
    file_name    TEXT NOT NULL,
    duration_ms  INTEGER,
    tags         TEXT NOT NULL DEFAULT '[]',
    created_at   TEXT NOT NULL
);
CREATE INDEX idx_media_org ON media(org_id);

CREATE TABLE playlists (
    id          TEXT PRIMARY KEY,
    org_id      TEXT NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    items       TEXT NOT NULL DEFAULT '[]',   -- JSON array of items
    shuffle     INTEGER NOT NULL DEFAULT 0,
    updated_at  TEXT NOT NULL
);
CREATE INDEX idx_playlists_org ON playlists(org_id);

CREATE TABLE layouts (
    id          TEXT PRIMARY KEY,
    org_id      TEXT NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    width       INTEGER NOT NULL DEFAULT 1920,
    height      INTEGER NOT NULL DEFAULT 1080,
    background  TEXT NOT NULL DEFAULT '#000000',
    regions     TEXT NOT NULL DEFAULT '[]',   -- JSON array of regions
    updated_at  TEXT NOT NULL
);
CREATE INDEX idx_layouts_org ON layouts(org_id);

CREATE TABLE display_groups (
    id      TEXT PRIMARY KEY,
    org_id  TEXT NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    name    TEXT NOT NULL
);

CREATE TABLE displays (
    id                 TEXT PRIMARY KEY,
    org_id             TEXT REFERENCES organizations(id) ON DELETE CASCADE,
    name               TEXT NOT NULL,
    device_key_hash    TEXT NOT NULL UNIQUE,
    pairing_code       TEXT UNIQUE,
    status             TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','active','disabled')),
    group_id           TEXT REFERENCES display_groups(id) ON DELETE SET NULL,
    default_layout_id  TEXT REFERENCES layouts(id) ON DELETE SET NULL,
    player_info        TEXT NOT NULL DEFAULT '{}',
    last_seen          TEXT,
    created_at         TEXT NOT NULL
);
CREATE INDEX idx_displays_org ON displays(org_id);

CREATE TABLE schedules (
    id           TEXT PRIMARY KEY,
    org_id       TEXT NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    layout_id    TEXT NOT NULL REFERENCES layouts(id) ON DELETE CASCADE,
    target_type  TEXT NOT NULL CHECK (target_type IN ('all','group','display')),
    target_id    TEXT,
    starts_at    TEXT,            -- display-local 'YYYY-MM-DDTHH:MM', NULL = no start bound
    ends_at      TEXT,            -- display-local, NULL = open-ended
    days_mask    INTEGER NOT NULL DEFAULT 127,  -- bit0=Mon .. bit6=Sun
    time_from    TEXT,            -- 'HH:MM' daily window, NULL = all day
    time_to      TEXT,
    priority     INTEGER NOT NULL DEFAULT 0,
    enabled      INTEGER NOT NULL DEFAULT 1,
    created_at   TEXT NOT NULL
);
CREATE INDEX idx_schedules_org ON schedules(org_id);

CREATE TABLE triggers (
    id           TEXT PRIMARY KEY,
    org_id       TEXT NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    hook_key     TEXT NOT NULL UNIQUE,
    layout_id    TEXT NOT NULL REFERENCES layouts(id) ON DELETE CASCADE,
    duration_s   INTEGER NOT NULL DEFAULT 30,
    target_type  TEXT NOT NULL CHECK (target_type IN ('all','group','display')),
    target_id    TEXT,
    last_fired   TEXT,
    created_at   TEXT NOT NULL
);

CREATE TABLE proof_of_play (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    org_id       TEXT NOT NULL,
    display_id   TEXT NOT NULL,
    layout_id    TEXT,
    media_id     TEXT,
    item_type    TEXT NOT NULL,
    started_at   TEXT NOT NULL,
    duration_ms  INTEGER NOT NULL
);
CREATE INDEX idx_pop_org_time ON proof_of_play(org_id, started_at);

CREATE TABLE audit_log (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    org_id      TEXT NOT NULL,
    user_id     TEXT,
    action      TEXT NOT NULL,
    detail      TEXT NOT NULL DEFAULT '',
    created_at  TEXT NOT NULL
);
CREATE INDEX idx_audit_org ON audit_log(org_id, created_at);
