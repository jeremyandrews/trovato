-- Built-in account-recovery paths (FR-7c, Story 4.6, design §4.5).
--
-- The kernel ships two recovery methods, and they ride through the SAME frozen
-- `tap_account_recovery` contract as any plugin-provided method: same JSON
-- schema, same owner-scoped fail-closed fold, same kernel-owned flow. There is
-- deliberately no privileged second recovery codepath to audit.
--
-- Both tables store only a SHA-256 hash of the secret, mirroring the
-- `password_reset_tokens` precedent. A database read must not yield anything
-- that can be replayed into an account.

-- Pre-generated recovery codes: the offline path. A user prints or saves these
-- and can get back in with no email and no second device.
CREATE TABLE IF NOT EXISTS recovery_codes (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id    UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,

    -- SHA-256 of the code. The plaintext is shown once at generation and is
    -- never recoverable afterwards, by design.
    code_hash  TEXT NOT NULL,

    -- Single-use. A consumed code stays as a row so "which code was used, and
    -- when" remains answerable, but can never be redeemed twice.
    used_at    TIMESTAMPTZ,

    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),

    -- The same code must not be redeemable against two accounts.
    UNIQUE (code_hash)
);

-- "How many unused codes does this account have?" drives both the >=1-path
-- invariant and the `describe` availability answer, so it is the hot query.
CREATE INDEX IF NOT EXISTS idx_recovery_codes_user
    ON recovery_codes (user_id) WHERE used_at IS NULL;

-- Per-flow email challenges: the emailed one-time code.
--
-- Scoped to a single kernel-owned flow nonce, so a code issued for one recovery
-- attempt cannot be replayed into another — the challenge is bound to the flow,
-- not merely to the account.
CREATE TABLE IF NOT EXISTS recovery_email_challenges (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id    UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,

    -- The kernel-owned flow nonce this challenge belongs to.
    flow_id    TEXT NOT NULL,

    -- SHA-256 of the emitted code; the plaintext exists only in the email.
    code_hash  TEXT NOT NULL,

    -- The kernel enforces its own TTL on top of any plugin advisory.
    expires_at TIMESTAMPTZ NOT NULL,

    -- Single-use, like the codes above.
    used_at    TIMESTAMPTZ,

    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),

    -- One live challenge per flow.
    UNIQUE (flow_id)
);

-- Verification looks a challenge up by its flow; expiry sweeps scan by time.
CREATE INDEX IF NOT EXISTS idx_recovery_email_challenges_expires
    ON recovery_email_challenges (expires_at);
