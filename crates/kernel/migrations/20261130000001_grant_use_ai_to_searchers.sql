-- Grant `use ai` to every role that can already search, so no site loses AI
-- search on upgrade. Forward-only; no rollback.
--
-- The AI search endpoints (`/api/v1/search/expand`, `/summarize` and
-- `/followup`) had no permission check of their own: every visitor who could
-- reach them could spend the site's provider tokens. They now ask for
-- `use ai`, as every other AI route does. A role that could search yesterday
-- got expansion, summaries and follow-ups with it, so that is the selection
-- here; in a default install that is the anonymous and the authenticated role.
--
-- `use ai` alone sends nothing anywhere else: the chatbot, the writing
-- assistant and the AI Assistant also ask for `use ai chat` or
-- `use ai assistant`, and a plugin's `ai-request` for the operation's own
-- permission, none of which this grants.
--
-- A one-time correction, not a rule: a role granted `search content` from now
-- on gets exactly that. `ON CONFLICT DO NOTHING` so the migration is safe to
-- re-run.
INSERT INTO role_permissions (role_id, permission)
SELECT DISTINCT role_id, 'use ai'
FROM role_permissions
WHERE permission = 'search content'
ON CONFLICT DO NOTHING;
