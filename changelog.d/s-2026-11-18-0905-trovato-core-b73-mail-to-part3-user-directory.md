A plugin can look up public facts about the site's users by id: `user-api`
gains `load-users` (SDK: `host::load_users`), gated by a new manifest capability,
`user_directory`. **API change:** a new host function in an existing interface
and a new `[capabilities]` boolean, defaulting to false; no existing signature
changed (BL-105, G-NO-USER-DIRECTORY).

The root cause was that `user-api` could only answer about the user a request
was for, so a plugin mailing a digest to its subscribers, or listing who did
what, had no way to learn a name. The only way round it was the `users` table
itself, which is now on the protected floor and stays there. `load-users` takes
up to 100 ids and answers, in one query that selects only these columns, each
existing user's id, name, status (`"active"` or `"blocked"`), timezone,
language and creation time; never an address, a password hash or `data`.
Declaring `user-api` alone does not grant it: the plugin must also declare
`user_directory = true`, so reading about people it is not serving is visible
in its manifest. New error codes `ERR_USER_DIRECTORY_DENIED` (-73),
`ERR_USER_IDS_INVALID` (-74) and `ERR_USER_IDS_TOO_MANY` (-75).
