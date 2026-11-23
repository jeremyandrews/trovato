The block editor's image upload (`POST /api/v1/block-editor/upload`, and its
`/api/block-editor/upload` alias) is counted in the `uploads` rate bucket, 10 a
minute, like `/file/upload`. Before this it fell through to the generic `api`
bucket at 100 a minute, so a client could write ten times as many files through
the editor as through the upload form. The admin "send test email" action
(`POST /admin/config/site/test-email`) is counted in the `mail` bucket instead
of the generic `forms` one.

A new test, `every_sensitive_route_is_in_its_named_bucket`, lists every kernel
route that writes files, sends mail or calls an AI provider with the bucket
that bounds it. It checks each one: the route exists, and the middleware puts
it in that bucket, or its handler checks the bucket itself and the middleware
does not count it there twice. `POST /api/v1/ai/assist` is listed as still in
`api`, pending the AI rate buckets.
