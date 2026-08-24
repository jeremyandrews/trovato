-- AI Assistant: conversations and the proposals they produce (0.102).
--
-- A conversation is one person configuring one thing. The (user, scope,
-- scope_id) triple is what identifies it, and the partial unique index below is
-- what makes "open the assistant for this device" idempotent: reopening finds
-- the conversation already there instead of starting a second one beside it.
--
-- A proposal is a write the model asked for and the person has not applied. It
-- is a row rather than a transcript entry because it has a lifecycle the
-- transcript does not: it is applied or discarded by a separate request, by the
-- owner, once.
--
-- Timestamps are epoch seconds in BIGINT, as everywhere else in this schema.

CREATE TABLE ai_conversation (
    id             UUID PRIMARY KEY,
    user_id        UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    plugin         TEXT NOT NULL,
    scope          TEXT NOT NULL,
    scope_id       TEXT,
    title          TEXT NOT NULL,
    status         TEXT NOT NULL DEFAULT 'open',
    snapshot       TEXT NOT NULL,
    links          JSONB NOT NULL DEFAULT '[]',
    transcript     JSONB NOT NULL DEFAULT '[]',
    message_count  INTEGER NOT NULL DEFAULT 0,
    tokens_used    BIGINT NOT NULL DEFAULT 0,
    created        BIGINT NOT NULL,
    changed        BIGINT NOT NULL
);

-- One open conversation per person per thing. COALESCE because a NULL scope_id
-- is a value here ("the site-wide scope"), and NULL never equals NULL in an
-- index, so without it a `None`-kind scope could open unboundedly many.
CREATE UNIQUE INDEX ai_conversation_open_uq
    ON ai_conversation (user_id, scope, COALESCE(scope_id, '')) WHERE status = 'open';

CREATE INDEX ai_conversation_user_idx ON ai_conversation (user_id, changed DESC);

CREATE TABLE ai_proposal (
    id               UUID PRIMARY KEY,
    conversation_id  UUID NOT NULL REFERENCES ai_conversation(id) ON DELETE CASCADE,
    user_id          UUID NOT NULL,
    scope            TEXT NOT NULL,
    scope_id         TEXT,
    tool             TEXT NOT NULL,
    arguments        JSONB NOT NULL,
    description      TEXT NOT NULL,
    risk             TEXT NOT NULL,
    status           TEXT NOT NULL DEFAULT 'proposed',
    result           TEXT,
    model            TEXT NOT NULL,
    created          BIGINT NOT NULL,
    resolved         BIGINT,
    resolved_by      UUID
);

CREATE INDEX ai_proposal_conversation_idx ON ai_proposal (conversation_id, created);
