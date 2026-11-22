Logging out now removes the device from the account's session list. Logout
ended the session with `Session::flush()`, which deletes the session record from
Redis but not the device's entry in the per-user session index,
`user_sessions:<uid>`. The session tracking middleware, which owns every other
write to that index, could not clean up after the handler either, because a
flushed session no longer names a user. So the logged out device stayed on
`/user/sessions` for every other device of the account until the index expired.
Logout now reads the device id before flushing and removes the entry through
`SessionRegistry::revoke`, the same removal a revocation from another device
uses. A session that never reached the index logs out exactly as before.

The same gap existed on two other paths that end a session. A blocked account's
session, ended on its next request by `session_user` or `get_user_context`, was
flushed the same way and left its entry behind; it now leaves the index too. An
administrator deleting an account through `/admin/people/{id}/delete` ended none
of that account's sessions at all, so its devices stayed listed and its index
went on being refreshed; the deletion now revokes every session of the account,
as self-service deletion already did. The work happens only when a session
ends: ordinary page views make no extra Redis call.
