-- Optimistic-locking version for the iframe role-config page. Bumped on
-- every save; a save carrying a stale version is rejected with 409 so two
-- dashboard tabs can't silently clobber each other.
ALTER TABLE role_links
    ADD COLUMN IF NOT EXISTS config_version INT NOT NULL DEFAULT 1;
