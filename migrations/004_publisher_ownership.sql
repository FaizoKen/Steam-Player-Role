-- Optional Steamworks Web API publisher key per role link. When set,
-- OwnsGame conditions are verified via the partner CheckAppOwnership
-- endpoint instead of the public library.
ALTER TABLE role_links ADD COLUMN IF NOT EXISTS publisher_key TEXT;

-- CheckAppOwnership results (per user per app), refreshed by the
-- background worker alongside user_cache
CREATE TABLE IF NOT EXISTS app_ownership_cache (
    steam_id        TEXT NOT NULL,
    app_id          TEXT NOT NULL,
    owns_app        BOOLEAN NOT NULL DEFAULT FALSE,
    permanent       BOOLEAN NOT NULL DEFAULT FALSE,
    owner_steam_id  TEXT NOT NULL DEFAULT '',
    fetched_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (steam_id, app_id)
);
