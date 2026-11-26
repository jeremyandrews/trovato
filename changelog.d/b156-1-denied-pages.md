A visitor who is not signed in and asks for a page that needs a sign in is now
answered `403` "Access denied", with the login form on the page, and signing in
from that form lands them on the page they asked for. It used to be a `303` to
the bare `/user/login`: the cause was that the HTML half of the refusal rule
was a redirect rather than a page, and the login form read no destination, so
the page that was asked for was lost on the way, and a crawler or an old link
was told `200` for a page nobody may see. The refusal is now the `401` every
guard already produced, which the one error-page layer shows a browser as the
themed access-denied page; a script is still told `401`. `/user/login` reads
`?destination=` (as the comment form's "Log in to post a comment" link has
always written it) and carries it through a failed attempt, and follows it only
to a local path, never to another host. The assistant and the gather pages,
which wrote their own redirect, refuse the same way.

Closed registration answers `403` where it answered `404`, on the form, its
submit and the JSON route, so an old "Create new account" link is told the page
is closed rather than that it never existed, and a visitor who already has an
account is offered the login form there.

An author holding the new `view own unpublished content` permission (Drupal's
permission of the same name, granted by no migration) reads their own
unpublished items. Without it their own draft answered them `404` while they
could still open its edit form. Everyone else still gets `404` for an
unpublished item, so a refusal never confirms that one exists.
