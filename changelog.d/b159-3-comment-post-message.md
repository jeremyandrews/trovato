A reader who posts a comment with JavaScript on is told it was posted. The
form's script (`static/js/comment-post.js`) posts JSON and, for a published
comment, reloaded the page; the item page says "Your comment was posted." only
for `?comment=posted`, which a reload does not add, so the message showed only
to readers without JavaScript, whose form post is redirected with it. On a
paged thread the reload also left the writer on the page they were reading,
not the one their comment went to. The script now goes to the comment's own
address, `/comment/{id}?comment=posted`, a new route that redirects to the item
on the page of the thread that holds the comment, at the comment, passing on
`posted`, `pending` or `updated` and nothing else. The address answers 404 for
an id that is not a comment and for a comment the viewer may not see, the
rule the comment API already applies. A comment held for review is still
reported in place, without leaving the page.
