The admin sidebar listed every administration screen to anyone admitted to
`/admin`, so a delegated role (say, `edit any content` with `access
administration pages`) saw a column of links that answered 403: the list was
hand written in `page--admin.html`, and `render_admin_template` never knew who
was looking. It now takes the viewer, and the sidebar is built from one table
in `routes::admin_nav` naming each screen and the permission its page asks
for, so the viewer sees only what they may open (BL-117).
