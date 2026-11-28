-- trovato:contract 0.105.0: 0.105.0 writes only draft, published and archived and never relies on this check refusing other values
--
-- Configurable moderation workflows (B301): the Editorial workflow's states
-- and transitions move from the compiled-in constants of
-- `content::moderation` to one site configuration variable,
-- `moderation.workflows`, read like `workflow.editorial` is
-- (`docs/design/moderation-workflows.md`). A content type's items move
-- through the workflow its `moderation_workflow` setting names, defaulting to
-- this one, `editorial`, when the setting is absent — so a site that never
-- adds a workflow of its own sees no change.
--
-- The check this file drops let the database refuse any `moderation_state`
-- but `draft`, `published` and `archived`; the kernel now decides which
-- states exist, so a site that adds a workflow with its own state ids needs
-- the column free to hold them. 0.105.0 (seeded by this migration, and every
-- release since) writes only those three strings when no other workflow is
-- configured, so dropping the check changes nothing it relies on.

ALTER TABLE item_revision DROP CONSTRAINT IF EXISTS item_revision_moderation_state_check;

INSERT INTO site_config (key, value, updated)
VALUES (
    'moderation.workflows',
    '{"workflows":[{"id":"editorial","label":"Editorial","default_state":"draft","states":[{"id":"draft","label":"Draft","published":false,"default_revision":false},{"id":"published","label":"Published","published":true,"default_revision":true},{"id":"archived","label":"Archived","published":false,"default_revision":true}],"transitions":[{"id":"create_new_draft","label":"Create New Draft","from":["draft","published"],"to":"draft"},{"id":"publish","label":"Publish","from":["draft","published"],"to":"published"},{"id":"archive","label":"Archive","from":["published"],"to":"archived"},{"id":"archived_draft","label":"Restore to Draft","from":["archived"],"to":"draft"},{"id":"archived_published","label":"Restore","from":["archived"],"to":"published"}]}]}'::jsonb,
    NOW()
)
ON CONFLICT (key) DO NOTHING;
