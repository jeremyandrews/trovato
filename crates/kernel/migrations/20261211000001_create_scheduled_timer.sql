-- B278: precise timers beside cron (docs/timers.md).
--
-- One row per timer, named per owner: `kernel` or a plugin's machine name.
-- Registering the same (owner, name) again replaces the row, which is what
-- makes registration idempotent. `run_at` is when the next attempt is due, to
-- the second; `slot_at` is the schedule slot that attempt belongs to, which a
-- retry leaves alone so a repeating timer keeps its cadence. A claimed row is
-- `running` with a `claim_token` and a lease (`claimed_until`); a row whose
-- lease ran out is claimed again by any server.
CREATE TABLE scheduled_timer (
    id               BIGSERIAL    PRIMARY KEY,
    owner            TEXT         NOT NULL,
    name             TEXT         NOT NULL,
    run_at           TIMESTAMPTZ  NOT NULL,
    slot_at          TIMESTAMPTZ  NOT NULL,
    repeat_secs      INTEGER      NULL CHECK (repeat_secs IS NULL OR repeat_secs > 0),
    payload          JSONB        NOT NULL DEFAULT '{}'::jsonb,
    status           TEXT         NOT NULL DEFAULT 'scheduled'
                     CHECK (status IN ('scheduled', 'paused', 'running', 'failed')),
    attempts         INTEGER      NOT NULL DEFAULT 0,
    claim_token      UUID         NULL,
    claimed_by       TEXT         NULL,
    claimed_until    TIMESTAMPTZ  NULL,
    last_fired_at    TIMESTAMPTZ  NULL,
    last_lateness_ms BIGINT       NULL,
    last_error       TEXT         NULL,
    created          TIMESTAMPTZ  NOT NULL DEFAULT now(),
    changed          TIMESTAMPTZ  NOT NULL DEFAULT now(),
    CONSTRAINT scheduled_timer_owner_name UNIQUE (owner, name)
);

-- What the scheduler reads: the earliest scheduled timer, and the due ones in
-- order. Only scheduled rows, so paused and failed timers cost it nothing.
CREATE INDEX scheduled_timer_due_idx ON scheduled_timer (run_at)
    WHERE status = 'scheduled';

-- Claims whose server stopped before finishing, found by lease.
CREATE INDEX scheduled_timer_lease_idx ON scheduled_timer (claimed_until)
    WHERE status = 'running';
