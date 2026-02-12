-- Password reset tokens table

CREATE TABLE password_reset_tokens (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token_hash VARCHAR(255) NOT NULL,  -- SHA-256 hash of the token
    expires_at TIMESTAMPTZ NOT NULL,
    used_at TIMESTAMPTZ,  -- NULL until token is used
    created TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Index for token lookup
CREATE INDEX idx_password_reset_tokens_hash ON password_reset_tokens(token_hash);

-- Index for cleanup of expired tokens
CREATE INDEX idx_password_reset_tokens_expires ON password_reset_tokens(expires_at);
