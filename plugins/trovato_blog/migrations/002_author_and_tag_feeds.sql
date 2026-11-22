-- The blog listing per author and per tag, as pages and as feeds (B127).
--
-- Two contextual filters, each a no-op when its argument is absent, so /blog
-- itself still lists every post:
--   author_id          equals  url_arg("author")
--   fields.field_tags  has_tag url_arg("tag")
--
-- Two gather routes bind those arguments from the path: /blog/author/{user}
-- resolves an account name to its id, /blog/tag/{tag} resolves a slug in the
-- `tags` vocabulary to its term id. An unknown name or slug is a 404.
--
-- One feed declaration serves the whole blog, each author and each tag, in RSS
-- and Atom: /blog/feed.xml (.atom), /blog/author/{user}/feed.xml (.atom) and
-- /blog/tag/{tag}/feed.xml (.atom). The argument feeds bind {user} and {tag}
-- through the same routes, and each route's page advertises its own feed.
--
-- Forward-only; jsonb_set/concatenation keeps whatever else a site changed in
-- the definition and display since 001.

UPDATE gather_query
SET definition = jsonb_set(
        definition,
        '{filters}',
        COALESCE(definition->'filters', '[]'::jsonb)
        || '[
            {"field": "author_id", "operator": "equals", "value": {"url_arg": "author"}, "exposed": false, "exposed_label": null},
            {"field": "fields.field_tags", "operator": "has_tag", "value": {"url_arg": "tag"}, "exposed": false, "exposed_label": null}
        ]'::jsonb
    ),
    display = display || '{
        "routes": [
            {"path": "/blog/author/{user}", "params": [{"segment": "user", "param": "author", "user": true}]},
            {"path": "/blog/tag/{tag}", "params": [{"segment": "tag", "param": "tag", "tag_category": "tags"}]}
        ],
        "feed": {
            "path": "/blog/feed.xml",
            "paths": ["/blog/author/{user}/feed.xml", "/blog/tag/{tag}/feed.xml"],
            "title": "Blog",
            "items": 20
        }
    }'::jsonb,
    changed = EXTRACT(EPOCH FROM NOW())::bigint
WHERE query_id = 'blog_listing'
  AND NOT (definition->'filters' @> '[{"value": {"url_arg": "author"}}]'::jsonb);
