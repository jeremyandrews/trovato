-- Replace the case-sensitive UNIQUE constraint on users.name with a
-- case-insensitive unique index to prevent lookalike usernames (e.g. Admin vs admin).
ALTER TABLE users DROP CONSTRAINT users_name_key;
CREATE UNIQUE INDEX idx_users_name_unique ON users(LOWER(name));
