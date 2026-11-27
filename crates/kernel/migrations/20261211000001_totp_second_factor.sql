-- B304: TOTP second factor, hashed recovery codes, a role's second-factor
-- requirement, and the password's last-changed time. Expand only: three new
-- tables, two new nullable/defaulted columns, nothing dropped or renamed.

-- One row per enrolled account. `confirmed_at` NULL means the enrolment was
-- started (the setup page showed a QR code) but never confirmed with a right
-- code, and such a row never counts as "has TOTP" anywhere in the kernel.
-- `secret` is the AES-256-GCM-sealed TOTP secret (see `secret_box`), never the
-- plain secret. `last_used_step` is the RFC 6238 time-step of the last
-- accepted code, so the same code (or an earlier step) can never be replayed.
CREATE TABLE user_totp (
    user_id UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    secret BYTEA NOT NULL,
    confirmed_at TIMESTAMPTZ,
    last_used_step BIGINT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Ten single-use recovery codes per enrolment. `code_hash` is an HMAC-SHA256
-- of the code under a key derived from `ENCRYPTION_KEY`, never the code
-- itself. `used_at` is set the first (and only) time a code verifies.
CREATE TABLE user_recovery_codes (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    code_hash BYTEA NOT NULL,
    used_at TIMESTAMPTZ
);

CREATE INDEX idx_user_recovery_codes_user_id ON user_recovery_codes (user_id);

-- A role whose holders must have a second factor (a confirmed TOTP enrolment
-- or a passkey) before they may use the site past enrolment itself. Default
-- false, so every existing role is unaffected until an administrator turns it
-- on.
ALTER TABLE roles ADD COLUMN requires_second_factor BOOLEAN NOT NULL DEFAULT false;

-- When the account's password was last set. NULL for every row that exists
-- before this migration (no backfill, per UPGRADING.md "Expand, then
-- contract"); the password-age policy treats a NULL as "changed when the
-- maximum-age setting was turned on", not as "never", so turning the setting
-- on does not retroactively expire every existing password.
ALTER TABLE users ADD COLUMN pass_changed TIMESTAMPTZ;
