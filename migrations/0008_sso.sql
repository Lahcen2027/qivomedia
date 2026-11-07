-- Single sign-on (OpenID Connect): Microsoft Entra ID, Google Workspace or any OIDC provider.
CREATE TABLE sso_providers (
    id                TEXT PRIMARY KEY,
    org_id            TEXT NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    kind              TEXT NOT NULL CHECK (kind IN ('microsoft', 'google', 'oidc')),
    name              TEXT NOT NULL,                  -- button label, e.g. "Université Mohammed V"
    issuer            TEXT NOT NULL,                  -- canonical issuer from the discovery document
    client_id         TEXT NOT NULL,
    client_secret_enc TEXT NOT NULL,                  -- AES-256-GCM, see crypto.rs
    domains           TEXT NOT NULL DEFAULT '',       -- allowed email domains, comma-separated
    auto_create       INTEGER NOT NULL DEFAULT 1,     -- create accounts on first sign-in
    default_role      TEXT NOT NULL DEFAULT 'viewer' CHECK (default_role IN ('admin', 'editor', 'viewer')),
    group_roles       TEXT NOT NULL DEFAULT '[]',     -- JSON [{"group": "...", "role": "editor"}]
    enabled           INTEGER NOT NULL DEFAULT 1,
    created_at        TEXT NOT NULL
);
CREATE INDEX sso_providers_org ON sso_providers(org_id);

-- Which provider account belongs to which Qivomedia user (matched by the provider's stable `sub`).
CREATE TABLE user_identities (
    provider_id   TEXT NOT NULL REFERENCES sso_providers(id) ON DELETE CASCADE,
    subject       TEXT NOT NULL,
    user_id       TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    email         TEXT,
    created_at    TEXT NOT NULL,
    last_login_at TEXT,
    PRIMARY KEY (provider_id, subject)
);
CREATE INDEX user_identities_user ON user_identities(user_id);

-- Sign-ins in progress (10 minutes, single use).
CREATE TABLE sso_states (
    state_hash   TEXT PRIMARY KEY,
    provider_id  TEXT NOT NULL REFERENCES sso_providers(id) ON DELETE CASCADE,
    nonce        TEXT NOT NULL,
    verifier     TEXT NOT NULL,
    redirect_uri TEXT NOT NULL,
    created_at   TEXT NOT NULL
);

ALTER TABLE organizations ADD COLUMN sso_required INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sessions ADD COLUMN sso INTEGER NOT NULL DEFAULT 0;
