-- Emergency alerts: take over screens above every schedule until cleared.

CREATE TABLE alerts (
    id           TEXT PRIMARY KEY,
    org_id       TEXT NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    kind         TEXT NOT NULL,              -- fire | evacuation | lockdown | weather | custom
    color        TEXT NOT NULL,              -- red | amber | blue
    lines        TEXT NOT NULL,              -- JSON [{lang, title, message}]
    target_type  TEXT NOT NULL CHECK (target_type IN ('all','group','display')),
    target_id    TEXT,
    test         INTEGER NOT NULL DEFAULT 0, -- drill: screens show "TEST / EXERCICE"
    sound        INTEGER NOT NULL DEFAULT 1,
    source       TEXT NOT NULL,              -- dashboard | webhook | cap
    external_id  TEXT,                       -- CAP identifier, to match a later Cancel
    started_by   TEXT,
    started_at   TEXT NOT NULL,
    cleared_at   TEXT,
    cleared_by   TEXT
);
CREATE INDEX idx_alerts_org_active ON alerts(org_id, cleared_at);

ALTER TABLE organizations ADD COLUMN alert_hook_key TEXT;
CREATE UNIQUE INDEX idx_org_alert_hook ON organizations(alert_hook_key);
