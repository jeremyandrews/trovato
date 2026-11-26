Creating an account now writes a security event. `SecurityEventKind` had no
kind for it, so public registration (the form and the JSON endpoint), the
admin "Add user" form, the installer and `trovato user create` left the
security audit stream with no record that an account had appeared at all —
only the content audit log's generic `create`/`user` row, and that only when
the optional `trovato_audit_log` plugin is enabled. `account.created` is now
emitted once, from the shared `UserService::create*` functions every one of
those paths calls, naming which path it was, whether the account starts
active, and who did it.

The same audit swept up two more account actions with no event of their own:
changing a username or an e-mail address wrote nothing beyond that same
content-audit row, so `user.name_changed` and `user.mail_changed` are new.
And an administrator deleting or cancelling someone else's account wrote no
`account.deleted` either — only self-service deletion did, because the event
lived at that route instead of in `UserService::delete`, the function both
paths call. It has moved there, so both are recorded the same way now.
