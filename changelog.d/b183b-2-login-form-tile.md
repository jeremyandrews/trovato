A site can put the login form itself in a tile. The new `user_login` tile type
(the tile form's "User login") draws the login page's own form, `user_login`,
for a visitor who is not signed in, posting to `/user/login`. The one handler
there checks it, shows the login page with its error when a sign in fails, and
on success returns to the page the tile was on. The tile is left off for a
signed-in viewer and on the login page itself.

A sidebar login form was not possible before because the page context issued a
CSRF token only to a signed-in viewer, for the Logout form, and the tile
renderer had no session to issue one into. That left a site like jibnotes with
a "User login" tile of links. `TileRenderContext` now carries the request's
session, and the login tile builds its form through the forms pipeline, which
issues the token. The token, and with it the visitor's session cookie, is
created only on a page where the tile is drawn: an anonymous page without it
still starts no session.
