`trovato import` now imports comments, URL aliases and redirects as well as
items and revisions, each keeping its id and its dates, so a site migration no
longer writes `comment`, `url_alias` or `redirect` with SQL.

Every create path for the three (`CommentService`, `UrlAliasService`,
`Redirect::create`) stamps a new id and the current time, and none can join a
caller's transaction, so a port kept its threads, addresses and old paths by
writing the tables itself. The importer checks each record as the forms do: a
comment's body, subject length, status, a text format the site has, and a
parent on the same item; an alias's paths normalised as the alias form stores
them, and an address no other alias holds in that language; a redirect's
source canonicalised and its destination and status code refused as
`Redirect::create` refuses them. The database triggers set a comment's depth,
the item's comment count and its search vector, as for a comment posted on the
site. Each type upserts by id like items do, so a rerun of the same file
reports 0 changes; a stored comment is not moved to another item or parent.
