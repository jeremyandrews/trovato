-- Kernel-internal append-only security audit stream (Epic 4 / the audit-events
-- direction, DECIDED 2026-07-17).
--
-- This is NOT the plugin-provided `trovato_audit_log` content-CRUD audit
-- (`services/audit.rs`, gated on that plugin being enabled). This table is
-- kernel infrastructure: always present, always written, one append-only
-- stream, structured events, one bounded retention policy. Authentication,
-- session lifecycle, credential lifecycle, and account recovery all emit here
-- and nowhere else, so incident response has a single place to look.
--
-- 1.0 posture: kernel-internal ONLY. There is deliberately no plugin-facing
-- audit host interface, which keeps this whole subsystem off the PF-5 frozen
-- contract path (a post-1.0 additive host interface stays possible via opt-in
-- `host_interfaces`).
--
-- Never store raw credential-adjacent material. Session identifiers are stored
-- as a stable SHA-256 hex digest (`subject_hash`), never in the clear, so the
-- durable record cannot be replayed into a live session.

CREATE TABLE IF NOT EXISTS security_audit_log (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),

    -- The structured event kind, e.g. `passkey.registered`, `session.revoked_by_admin`,
    -- `recovery.fold_escalation_attempt`. Dotted `<domain>.<event>` namespace;
    -- the exhaustive list lives in `crate::audit::SecurityEventKind`.
    kind         TEXT NOT NULL,

    -- The account the event is about, when there is one. NULL for events that
    -- precede identification (e.g. a generic recovery initiation for an address
    -- that matches no account). ON DELETE SET NULL: deleting a user must not
    -- silently erase the audit trail of what happened to that account.
    user_id      UUID REFERENCES users(id) ON DELETE SET NULL,

    -- The actor who caused the event, when different from `user_id` (an admin
    -- revoking another user's session). NULL when the subject acted on itself.
    actor_id     UUID REFERENCES users(id) ON DELETE SET NULL,

    -- A stable SHA-256 hex digest of whatever opaque identifier the event is
    -- about — a session id (D-36 rider: NEVER the raw id), a credential id, a
    -- recovery flow nonce. Hex, so it is greppable across events without ever
    -- being usable as the identifier itself.
    subject_hash TEXT,

    -- Whether the audited operation succeeded. Failures are the interesting
    -- half of an auth audit trail, so they are first-class rows, not omissions.
    outcome      TEXT NOT NULL DEFAULT 'success'
                 CHECK (outcome IN ('success', 'failure')),

    -- Validated client IP (see `AuditService::sanitize_ip` precedent) and the
    -- raw User-Agent, both truncated at the write site.
    ip_address   TEXT,
    user_agent   TEXT,

    -- Event-specific structured detail. MUST NOT carry secrets, raw tokens,
    -- raw session ids, or password/credential material.
    details      JSONB NOT NULL DEFAULT '{}'::jsonb,

    -- Unix seconds, matching the `audit_log` / `item_embed_status` convention.
    created      BIGINT NOT NULL DEFAULT EXTRACT(EPOCH FROM now())::BIGINT
);

-- Retention pruning deletes by age; incident response reads recent-first.
CREATE INDEX IF NOT EXISTS idx_security_audit_log_created
    ON security_audit_log (created DESC);

-- "What happened to this account?" is the dominant investigative query.
CREATE INDEX IF NOT EXISTS idx_security_audit_log_user
    ON security_audit_log (user_id, created DESC);

-- "Show me every counter regression / every escalation attempt" — alerting.
CREATE INDEX IF NOT EXISTS idx_security_audit_log_kind
    ON security_audit_log (kind, created DESC);
