-- 005: Quota governor + scaling hardening.
--
-- Adds durable daily-quota accounting (survives restarts, shared across
-- workers/instances), per-user data-stability tracking for an adaptive
-- re-check cadence, and the index the is-active probe needs once
-- role_assignments holds millions of rows.

-- Durable per-day Steam Web API quota ledger. One row per UTC day per scope:
-- 'main' for the plugin's own Web API key, 'pub:<hash>' for each configured
-- Steamworks publisher key (partner keys have their own independent daily
-- allowance). In-memory counters would reset to 0 on restart and over-spend;
-- the governor reloads today's value on boot. BIGINT so a raised quota can
-- never overflow.
CREATE TABLE IF NOT EXISTS api_quota_usage (
    quota_date   DATE NOT NULL,
    scope        TEXT NOT NULL DEFAULT 'main',
    used_units   BIGINT NOT NULL DEFAULT 0,
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (quota_date, scope)
);

-- Adaptive cadence: how many consecutive refreshes produced the SAME
-- eval-relevant data (hash below). Steam libraries/levels/groups are very
-- stable for most users, so a high streak earns an exponentially longer
-- interval (bounded), concentrating scarce quota on churn. Reset to 0
-- whenever the data changes so a refund/unlock/new purchase is caught on the
-- normal cadence again.
ALTER TABLE user_cache ADD COLUMN IF NOT EXISTS stable_streak INTEGER NOT NULL DEFAULT 0;

-- SHA-256 over the eval-relevant snapshot written by the last successful
-- refresh; '' means "no baseline yet" and counts as changed.
ALTER TABLE user_cache ADD COLUMN IF NOT EXISTS data_hash TEXT NOT NULL DEFAULT '';

-- The EXISTS(role_assignments WHERE discord_id = ...) "is this user active"
-- probe runs on every refresh. role_assignments' PK is
-- (guild_id, role_id, discord_id), so a discord_id-only lookup was a scan.
CREATE INDEX IF NOT EXISTS idx_role_assignments_discord ON role_assignments (discord_id);
