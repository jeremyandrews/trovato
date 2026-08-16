-- Add UNIQUE constraint on users.mail (excluding the anonymous user's empty email).
CREATE UNIQUE INDEX IF NOT EXISTS idx_users_mail_unique ON users(mail) WHERE mail != '';

-- Upgrade token_hash index to UNIQUE for defense-in-depth.
DROP INDEX IF EXISTS idx_email_verification_tokens_hash;
CREATE UNIQUE INDEX idx_email_verification_tokens_hash ON email_verification_tokens(token_hash);

-- Drop the DEFAULT on purpose column now that all rows have been backfilled.
ALTER TABLE email_verification_tokens ALTER COLUMN purpose DROP DEFAULT;
