Performance tests now catch a page that starts doing more work. `trovato
generate` writes a deterministic site of any size (members, an administrator,
tags, blog posts each with a revision, and comments) with set-based SQL, so
Postgres does the work. It writes only into an empty database and has no default
password.

The new `performance_test` builds a small generated site on a scratch database
with a fixed plugin set. It loads the front page, an item page with comments, a
tag page, the feed, search, the login page and the admin content list, as each
kind of visitor, with cold and warm caches. It compares the exact number of
statements, cache lookups, misses and writes, plugin calls, host calls, gather
runs and renders with `crates/kernel/tests/fixtures/performance_counts.toml`,
statement by statement, each named by its fingerprint and normalised text. Each
visit also checks that the visitor is signed in or not as it should be and that
the page shows its real content. The counts are the same on every machine and
every run, so they run on every pull request, and a change in a page's work
shows in review as a diff of that file.
`docs/performance-tests.md` explains how to add a page and what to do when one
fails. The request profile now keeps each statement's normalised text next to
its fingerprint, and a quoted identifier left unterminated no longer carries
the rest of the statement into it.
