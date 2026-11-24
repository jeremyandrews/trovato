# Performance tests

Trovato's performance tests count work instead of timing it. For each page
they check, they load the page on a small generated site and compare what the
request did with a number committed to the repository:

- database statements, and the fingerprint of each one;
- cache lookups, the lookups that missed, and the values written to Redis;
- plugin calls, and the host functions those plugins called;
- gather queries run and templates rendered.

Counts are the same on every machine and every run, so the tests run on every
pull request and fail on the commit that adds a query, not three releases
later when somebody looks at a graph. Timings belong to the benchmark
(see the timing benchmark in the BG14 design), which is run by hand and never
on a pull request.

The tests live in `crates/kernel/tests/performance_test.rs`. The harness is
`crates/kernel/tests/common/perf.rs`. The expected numbers are in
`crates/kernel/tests/fixtures/performance_counts.toml`.

## Writing a performance test

### What the harness gives you

`common::perf::shared_site()` returns a `PerfSite`: a whole site with a
database of its own. It is built once per test binary:

1. A scratch database is created and migrated.
2. The kernel starts with a plugin directory holding only the modules in
   `SITE_PLUGINS` (blog and search, which CI builds), so the counts do not
   depend on which other plugins a machine has built. `trovato_comments` is
   enabled without its module, which turns on the kernel's comment threads.
   Its module only declares permissions and a menu entry.
3. `trovato generate` fills it with a small fixed site: 40 posts, 120 comments
   (three on each post, by three different members), 6 tags (`topic-1` to
   `topic-6`), 7 members (`gen_user_1` to `gen_user_7`) and an administrator
   (`gen_admin`). The seed is fixed, so every id, title
   and date is the same on every run.
4. A member and the administrator sign in.
5. The site uses Redis database 13, which no other test uses, so emptying it
   touches no other test. Two performance runs at once against the same Redis
   would disturb each other, so run one at a time.

`site.visit(page, visitor, caches, path)` loads `path` and returns a `Visit`
with what the request did:

- `visitor` is `Visitor::Anonymous`, `Visitor::User` or `Visitor::Admin`.
- `caches` is `Caches::Cold`, which first clears every in-process cache and
  every Redis key except the sessions, the session index and the cache
  generation counters (`cache:gen`), or `Caches::Warm`, which makes the same
  request once just before.

Visits run one at a time, even when the tests in the binary run in parallel,
and each waits for the cache generation watcher to settle first, so nothing
another test does lands inside the measured request.

Each visit also checks that the visitor is who it says: a member's page shows
the logout button and an anonymous visitor's does not. `measure` in the test
file also checks that the page shows what it should, such as the post's
comments or the one search result, so a page that quietly broke cannot
record small numbers.

`common::perf::check(&visit)` compares the visit with the expectations file
under the key `page.visitor.caches` (for example `item.anonymous.warm`) and
fails with the difference.

### Adding a page

Add a test to `performance_test.rs`:

```rust
#[test]
fn tag_cloud() {
    run_test(async {
        let site = shared_site();
        measure(site, "tag_cloud", &[Visitor::Anonymous], "/tags", &["topic-1"]).await;
    });
}
```

`measure` visits the page as each visitor, cold and warm. Then record its
numbers:

```sh
TROVATO_PERF_RECORD=1 cargo test -p trovato-kernel --test performance_test -- --test-threads=1
```

Record mode writes what the pages do now into `performance_counts.toml`. It
keeps the tables of tests it did not run, so a filtered run records one page.
Remove a page's tables by hand when you remove its test.
Read the diff before you commit it. A new page should have new tables and
nothing else should change.

If the page needs content the generator does not make, add it to the generator
(`crates/kernel/src/generate.rs`) rather than inserting rows in the test, so
the benchmark site has it too. Expect the numbers of other pages to move when
you do, and say why in the commit.

### When a test fails

A failure names the page, the visitor and the cache state, and lists what
changed:

```
item.anonymous.warm does different work than tests/fixtures/performance_counts.toml expects:
  queries: expected 13, got 14
  select:users:3f2a9c1e: expected 0x, got 1x
      select * from users where id = $?
```

Each statement is listed by its fingerprint and its normalised text. The
fingerprint is the verb, the first table, and eight hex digits of a hash of the
normalised text. The normalised text is the statement with comments dropped,
every literal and parameter replaced by `?` or `$?`, and lists collapsed. The
expectations file holds the same two for every statement a page runs, so
`git diff` on it shows which statements came or went. To see where in the code
a statement runs, search the code for a distinctive part of its text.

Then do one of these:

- **The extra work is a mistake.** Fix the code. The usual causes are a query
  inside a loop, a cache lookup moved after the query it should save, and a
  plugin tap that now runs on every item.
- **The work is intended.** Run record mode and commit the changed numbers in
  the same commit as the code that changed them. The reviewer then sees
  `queries = 13` become `queries = 14` next to the statement and the reason.

Never raise a number to make the test pass without knowing what the new work
is.

### Keeping the counts deterministic

The counts are exact, so anything that changes between runs breaks them. The
harness already handles the known causes:

- **Which plugins run.** Fixed by `SITE_PLUGINS`.
- **What is in the database.** A fixed seed, and a database of its own.
- **What is in the caches.** Cold visits empty them. Warm visits fill them with
  the same request first.
- **Signed-in sessions.** A cold visit keeps the sessions and the session
  index. Without its index entry a registered session counts as revoked and
  the visitor is silently anonymous. A session's first request registers it
  and writes an audit row, so the harness makes that request before measuring
  anything.
- **Background cache invalidation.** A write bumps a cache generation in Redis,
  and a watcher clears local caches up to 250 ms later. The harness waits that
  out before every measured request.
- **Other tests.** Visits take turns.

When you add a test, keep the request itself deterministic too. Do not put the
current time or a random value in the path, and do not depend on the order of
results that have no `ORDER BY`. If a count still changes between two runs,
run the test twice with record mode and diff the two files to see which
statement comes and goes.

### What the counts leave out

- **Work on other tasks.** A cache generation bump or a search analytics flush
  that a request spawns runs on another task and is not counted.
- **The front page lookup.** `/` is rewritten to the configured front page
  before routing, outside the timing layer. On a cold start the lookup reads
  `site_config` once, and that read is not in `front.*.cold`.
- **Plugins on published content.** A published item on the live stage is
  shown to anyone with "access content" without asking plugins, so these
  pages make no plugin calls. A page that starts calling a plugin shows up as
  a change in `plugin_calls`.

### Proving the harness bites

`one_more_query_on_the_item_page_fails_its_test` sends the item page request
with an `x-perf-extra-query` header. The test router runs one extra
`SELECT 1` inside the request when it sees that header. The test checks that
the counts go up by one, that the difference names the `SELECT 1` fingerprint,
and that `check` fails. If it ever passes without the failure, the harness has
stopped counting.
