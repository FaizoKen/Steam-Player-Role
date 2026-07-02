-- Whether GetOwnedGames returned the games key (false = the user's
-- "game details" privacy setting hides their library). Defaults TRUE so
-- existing users aren't warned before their next refresh.
ALTER TABLE user_cache ADD COLUMN IF NOT EXISTS library_visible BOOLEAN NOT NULL DEFAULT TRUE;

-- Rate-limits the verify page's manual "Re-check" button
ALTER TABLE user_cache ADD COLUMN IF NOT EXISTS recheck_requested_at TIMESTAMPTZ;
