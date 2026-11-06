-- Public website: demo requests (leads) and platform-wide settings.

CREATE TABLE leads (
    id            TEXT PRIMARY KEY,
    name          TEXT NOT NULL,
    email         TEXT NOT NULL,
    organization  TEXT NOT NULL DEFAULT '',
    screens       TEXT NOT NULL DEFAULT '',
    message       TEXT NOT NULL DEFAULT '',
    lang          TEXT NOT NULL DEFAULT 'en',
    status        TEXT NOT NULL DEFAULT 'new' CHECK (status IN ('new','contacted','won','lost')),
    ip            TEXT,
    created_at    TEXT NOT NULL
);
CREATE INDEX idx_leads_created ON leads(created_at);

CREATE TABLE platform_settings (
    key    TEXT PRIMARY KEY,
    value  TEXT NOT NULL
);
