-- Index on expires_at for efficient expired-token cleanup queries.
CREATE INDEX idx_email_verification_tokens_expires
    ON email_verification_tokens(expires_at);
