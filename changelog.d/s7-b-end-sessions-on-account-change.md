Changing, resetting or recovering a password, and blocking an account, now end
the account's other sessions, and a block also deletes its API tokens. A
password change only rotated the caller's own session id, and a reset by
emailed token, an account recovery and a password set by an administrator ended
nothing, so a session opened under the old password, possibly by the person the
change was meant to lock out, kept working. A block left the user's sessions and
API tokens in place. The admin edit path said cross-user invalidation needed a
user to session index that did not exist, but `SessionRegistry` has provided one
since multi-device session management landed. One kernel function,
`session_registry::end_user_sessions`, now ends a user's indexed sessions, audits
each one as revoked by the user or by an administrator, and logs rather than
fails when Redis is unreachable. A self-service password change keeps the
caller's own device, as does an administrator setting their own password. A
reset, a recovery (before its new session is established), a password an
administrator sets for someone else and a block keep none. The command line
`user reset-password` ends the sessions through the same index in Redis and
prints how many it ended, or says plainly that it could not reach Redis and
ended none. Separately, a blocked user kept a working identity on every page
built from `get_user_context` (item, gather, search, file, comment, batch and
the JSON API among them), because that helper loaded the session user without
checking that the account was active. It now deletes the session and answers
as anonymous, as `require_login` already did. An API token whose owner is not
active is refused with the same 401 an unknown token gets, decided in the token
lookup itself, so the MCP server refuses it too.
