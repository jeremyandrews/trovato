-- Each person's administration preferences: colour theme, admin language,
-- and which sidebar groups they collapsed (B199).
--
-- A table of its own rather than a key in `users.data`, so that saving a user
-- from the account form, which writes `data` whole, can never drop them, and
-- so the shell reads one small row per admin page instead of the account.
-- Removed with the account. Expand-only: the previous release never reads it.
CREATE TABLE IF NOT EXISTS admin_preference (
    user_id UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    prefs JSONB NOT NULL DEFAULT '{}'::jsonb,
    changed TIMESTAMPTZ NOT NULL DEFAULT now()
);
