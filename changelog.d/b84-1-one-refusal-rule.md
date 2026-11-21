A refusal now depends on who is asking and what they asked for, not on which
handler answered. A JSON caller who is nobody gets `401`; a signed-in JSON
caller who may not do the thing gets `403`. A browser that is nobody is sent to
`/user/login` with a `303`; a signed-in one gets `403`.

Before this the same caller got a different answer from route to route. The
JSON permission helpers answered `403` to an anonymous caller, because they were
built on the HTML guard and threw away which of its two refusals it had made.
Sixteen handlers read the session's user id themselves instead of calling a
guard, some without the check that the account was still active, so a blocked
user's session could still create API tokens, upload files and hold content
locks until it expired. The translation screens refused with a `400` "error"
page, the content-lock routes with plain text, and the block editor with a bare
`403` for someone not signed in.

`require_login`, `require_permission` and `require_admin` now share one core,
`session_user`, which is the only place a handler learns who is asking and the
only place a blocked account's session is ended. `require_login_json` is new,
`require_permission_json` and `require_admin_json` return the `User` and answer
`401` or `403` as `AppError`, and `refusal_for` gives the same answer to a
handler that already holds the viewer (the item create, edit and delete routes,
and the gather access gate). The token, file, lock, comment, export, media,
chat, assist, mail preference, OAuth consent, account deletion, translation and
`/system/ajax` routes all go through them.

A session naming a user who has been deleted is now the anonymous visitor, with
the anonymous role's permissions; it used to be a visitor with no permissions
at all, so a published page that anyone could read answered that session `403`.

The `admin_ai_features` router, `/admin/config/ai/features`, is deleted along
with its template and its line in `docs/admin-permissions.md`. It was never
mounted, and nothing in the kernel or any plugin reads the `ai_features` config
it would have saved, so mounting it would have shipped a settings page with no
effect. A unit test now requires every `routes::*::router()` to be reachable
from `main.rs`.

Contract: an API client that told "not signed in" from "not allowed" by status
should read `401` for the first and `403` for the second; the ad hoc gather
endpoint, the dead-letter queue and embed admin endpoints, `/system/ajax`, the
block editor endpoints and `/api/lock/break` answered `403` to an anonymous
caller and now answer `401`.
