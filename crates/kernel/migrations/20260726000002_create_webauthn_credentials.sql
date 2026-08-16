-- WebAuthn/passkey credentials (FR-7a, design §2.1, D-35).
--
-- Storage shape per D-35: the library-serialized `Passkey` blob is the single
-- source of truth for the COSE public key, attestation, and counter — that
-- representation belongs to `webauthn-rs` and re-deriving it into hand-rolled
-- columns would be fragile and version-coupled. Everything else here is a
-- denormalized lookup/display convenience for the management UI and the D-37
-- regression check, refreshed from the blob on every successful authentication.
--
-- Credentials live PARALLEL to `users.pass` (D-33): an account is
-- `{ pass?, credentials: 0..N }`. Nothing here removes or weakens the password;
-- "passwordless" is an opt-in end state guarded by the ≥1-active-recovery-path
-- invariant at removal time, not a migration performed here.
--
-- FK/cascade mirrors the `password_reset_tokens` precedent.

CREATE TABLE IF NOT EXISTS webauthn_credentials (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id         UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,

    -- Raw credential id (the authenticator handle), base64url as the
    -- authenticator/browser presents it. UNIQUE across the whole table: one
    -- authenticator credential belongs to exactly one account, and the
    -- uniqueness violation is how a re-registration of an existing passkey is
    -- detected.
    credential_id   TEXT NOT NULL,

    -- The authoritative `webauthn-rs` serialized `Passkey`. Everything the
    -- library needs to verify an assertion is in here.
    passkey_json    JSONB NOT NULL,

    -- Denormalized signature counter for the D-37 regression check and display.
    -- Many platform passkeys always report 0; 0 means "counter unsupported",
    -- which the regression check treats as exempt rather than as a regression.
    sign_count      BIGINT NOT NULL DEFAULT 0,

    -- usb | nfc | ble | internal | hybrid, as reported at registration.
    transports      TEXT[] NOT NULL DEFAULT '{}',

    -- Authenticator model. Nullable: some authenticators report all zeroes, and
    -- privacy-preserving platform authenticators may report nothing useful.
    aaguid          UUID,

    -- Backup (multi-device credential) flags from the authenticator data.
    backup_eligible BOOLEAN NOT NULL DEFAULT false,
    backup_state    BOOLEAN NOT NULL DEFAULT false,

    -- User-assigned label for the credential list (FR-7b).
    device_name     TEXT,

    -- D-37: set when an authentication presented a regressed signature counter.
    -- The credential is FLAGGED, never auto-revoked (a false positive on
    -- auto-revoke is a self-inflicted lockout). A flagged credential still
    -- exists and is shown to the user; the authentication that tripped it was
    -- rejected.
    flagged_at      TIMESTAMPTZ,
    flag_reason     TEXT,

    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at    TIMESTAMPTZ,

    UNIQUE (credential_id)
);

-- "List this user's passkeys" is the dominant query (login allow-list,
-- management page, and the ≥1-path invariant check at removal time).
CREATE INDEX IF NOT EXISTS idx_webauthn_credentials_user
    ON webauthn_credentials (user_id);
