A tile can now be shown to anonymous visitors only, or to signed-in users
only, without a site having to hide it in a template. The login tile is the
case this closes: a tile's `visibility` JSON takes a new `audience` key
(`"anonymous"` or `"authenticated"`), decided from whether the request is
actually signed in rather than from the role list `roles` matches against.

`roles` could not express "anonymous only" because every viewer, signed in or
not, carries the `anonymous user` role in the list `routes::helpers` builds
for tile matching — a list built only for tiles, separate from (and
inconsistent with) the role names `PermissionService` actually grants
permissions by, which never put `anonymous user` on a signed-in user.
`{"roles": ["anonymous user"]}` therefore matched everyone, and a site
(rudere) worked around it with a per-theme template check the kernel should
have offered instead. `audience` is decided straight from the viewer's
signed-in state rather than from that list, so it needs no change to it and
carries no risk to anything the list already does.

The tile admin form gains a "Show to: Everyone / Anonymous visitors only /
Signed-in users only" field, saved into `visibility` alongside any `paths` or
`roles` rule a config import already set, and shown again on edit. Config
import and export round-trip `audience` the same way they already did every
other `visibility` key.
