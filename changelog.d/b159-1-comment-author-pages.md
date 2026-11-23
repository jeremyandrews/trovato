Comments have author pages: `/comment/reply/{item}` and
`/comment/reply/{item}/{comment}` are the reply form (the second with the
comment being answered shown above it), `/comment/{id}/edit` edits a comment's
text, and `/comment/{id}/delete` is a confirmation page whose form POSTs with
the CSRF token and deletes the comment and its replies. Each comment on the
item page links Reply, and Edit and Delete where the viewer may use them, so
replying no longer needs JavaScript and a member can correct or withdraw what
they wrote. Before, the only edit and delete controls were on the
administrator's moderation screen. Editing your own comment is `edit own
comments`; deleting it is the new `delete own comments`, which no role holds
until it is granted, so a site's members still cannot delete unless it says
so. A visitor who is not signed in is sent to the login form and brought back
to the reply page; a member without `post comments` gets 403.

The comment API's PUT and DELETE refused a moderator who held `administer
comments` but was not the site administrator: the access rule asked for `edit
any comment` and `delete any comment`, two permissions nothing declares and so
no role could be given from the permissions page. Editing or deleting someone
else's comment is now `administer comments`, the permission the moderation
screens already require; the two old names are still honoured where a site
granted them by hand. A comment also counts as the viewer's own only when the
viewer is signed in, so an anonymous visitor does not own an imported
anonymous comment that shares the nil author id.
