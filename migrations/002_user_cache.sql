-- Base user data cache (profile + bans + game library summary)
CREATE TABLE IF NOT EXISTS user_cache (
    steam_id            TEXT PRIMARY KEY,
    profile_data        JSONB NOT NULL DEFAULT '{}',
    ban_data            JSONB NOT NULL DEFAULT '{}',
    owned_games         JSONB NOT NULL DEFAULT '[]',
    groups              JSONB NOT NULL DEFAULT '[]',
    -- Denormalized for SQL-side filtering
    steam_level         INTEGER DEFAULT 0,
    account_created     TIMESTAMPTZ,
    total_games_owned   INTEGER DEFAULT 0,
    is_vac_banned       BOOLEAN DEFAULT FALSE,
    is_game_banned      BOOLEAN DEFAULT FALSE,
    country_code        TEXT DEFAULT '',
    -- Refresh tracking
    fetched_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    next_fetch_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    fetch_failures      INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_user_cache_next_fetch ON user_cache (next_fetch_at ASC);

-- Game-specific achievement cache (per-user per-game)
CREATE TABLE IF NOT EXISTS game_achievement_cache (
    steam_id        TEXT NOT NULL,
    app_id          TEXT NOT NULL,
    achievements    JSONB NOT NULL DEFAULT '[]',
    total_count     INTEGER NOT NULL DEFAULT 0,
    unlocked_count  INTEGER NOT NULL DEFAULT 0,
    fetched_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (steam_id, app_id)
);
