-- Replace the case-sensitive UNIQUE index on users.mail with a
-- case-insensitive version to prevent duplicate registrations via
-- email case variants (e.g. Admin@Example.com vs admin@example.com).
DROP INDEX IF EXISTS idx_users_mail_unique;
CREATE UNIQUE INDEX idx_users_mail_unique ON users(LOWER(mail)) WHERE mail != '';
