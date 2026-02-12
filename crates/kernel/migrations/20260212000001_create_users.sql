-- Create users table
-- User 1 bypass is replaced with is_admin boolean
-- Anonymous user is represented by uuid_nil() (00000000-0000-0000-0000-000000000000)

CREATE TABLE users (
    -- UUIDv7 for time-sortable IDs
    id UUID PRIMARY KEY,

    -- Username (unique, used for login)
    name VARCHAR(255) NOT NULL UNIQUE,

    -- Password hash (Argon2id)
    pass VARCHAR(255) NOT NULL,

    -- Email address
    mail VARCHAR(255) NOT NULL,

    -- Admin flag (replaces Drupal's User 1 check)
    is_admin BOOLEAN NOT NULL DEFAULT FALSE,

    -- Account timestamps
    created TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    access TIMESTAMPTZ,  -- Last access time
    login TIMESTAMPTZ,   -- Last login time

    -- Account status (0 = blocked, 1 = active)
    status SMALLINT NOT NULL DEFAULT 1,

    -- User preferences
    timezone VARCHAR(64) DEFAULT 'UTC',
    language VARCHAR(12) DEFAULT 'en',

    -- Arbitrary user data (profile fields, preferences, etc.)
    data JSONB NOT NULL DEFAULT '{}'::jsonb
);

-- Index for email lookups (password reset, etc.)
CREATE INDEX idx_users_mail ON users(mail);

-- Index for status filtering
CREATE INDEX idx_users_status ON users(status);

-- Insert the anonymous user (nil UUID)
-- This represents unauthenticated requests
INSERT INTO users (id, name, pass, mail, is_admin, status)
VALUES (
    '00000000-0000-0000-0000-000000000000',
    'anonymous',
    '',  -- No password for anonymous
    '',  -- No email for anonymous
    FALSE,
    1
);
