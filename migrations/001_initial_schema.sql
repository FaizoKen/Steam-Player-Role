-- Role links (standard)
CREATE TABLE IF NOT EXISTS role_links (
    id              BIGSERIAL PRIMARY KEY,
    guild_id        TEXT NOT NULL,
    role_id         TEXT NOT NULL,
    api_token       TEXT NOT NULL,
    conditions      JSONB NOT NULL DEFAULT '[]',
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (guild_id, role_id)
);

-- Linked accounts: discord_id ↔ steam_id
CREATE TABLE IF NOT EXISTS linked_accounts (
    id              BIGSERIAL PRIMARY KEY,
    discord_id      TEXT NOT NULL UNIQUE,
    steam_id        TEXT NOT NULL UNIQUE,
    steam_name      TEXT,
    linked_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Role assignments (local mirror)
CREATE TABLE IF NOT EXISTS role_assignments (
    guild_id        TEXT NOT NULL,
    role_id         TEXT NOT NULL,
    discord_id      TEXT NOT NULL,
    assigned_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (guild_id, role_id, discord_id),
    FOREIGN KEY (guild_id, role_id) REFERENCES role_links (guild_id, role_id) ON DELETE CASCADE
);

-- Verification sessions (OpenID return state)
CREATE TABLE IF NOT EXISTS verification_sessions (
    id              BIGSERIAL PRIMARY KEY,
    discord_id      TEXT NOT NULL,
    steam_id        TEXT NOT NULL DEFAULT '',
    nonce           TEXT NOT NULL,
    expires_at      TIMESTAMPTZ NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_verification_discord ON verification_sessions (discord_id);
CREATE INDEX IF NOT EXISTS idx_verification_nonce ON verification_sessions (nonce);

-- Guild settings (per-guild, shared across role links)
CREATE TABLE IF NOT EXISTS guild_settings (
    guild_id        TEXT PRIMARY KEY,
    view_permission TEXT NOT NULL DEFAULT 'members'
                    CHECK (view_permission IN ('members', 'managers')),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
