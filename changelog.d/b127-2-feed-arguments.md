One gather feed declaration can now serve a feed per author and per tag. A feed
path may carry one argument (`/blog/author/{user}/feed.xml`,
`/tag/{term}/rss.xml`), and `paths` lists more addresses for the same feed, so
the stock blog gather serves `/blog/feed.xml`, `/blog/author/<name>/feed.xml`
and `/blog/tag/<slug>/feed.xml`, each with its `.atom` sibling, from one
declaration.

A feed path was refused if it contained `{`, because the feed handler had no
way to fill an argument, so the only way to get a per-author feed was a second
gather per author, which is what the jibnotes, rudere and KernelTrap ports did.
The argument is now bound by the gather's own `display.routes` mapping with
the same segment name, resolved by the code the gather's page route uses: that
loop moved out of `handle_gather_route` into
`gather_routes::resolve_route_params`, which both call. It gained one mode,
`user: true`, which resolves an account name to the active account's id. An
argument that names nothing answers 404 rather than an empty feed: an unknown
slug or account name fails its lookup, and a passed-through value that matches
nothing the viewer can see is treated the same way. The page reached with an
argument (`/blog/author/<name>`) advertises the feed with the same argument in
its autodiscovery links; argument feeds are not added to every page's head.

`trovato_blog` gets migration `002_author_and_tag_feeds.sql`: two contextual
filters on `blog_listing` (author and tag, each a no-op when absent, so `/blog`
is unchanged), the routes `/blog/author/{user}` and `/blog/tag/{tag}`, and the
feed declaration. Every feed still runs the query as the requesting viewer.
