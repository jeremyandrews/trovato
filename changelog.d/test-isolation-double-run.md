The test suite can be run twice in a row against one database, from a database
that was never migrated, and CI enforces both. Neither held before, and neither
was visible to any CI job: every job started from a database nobody had run the
suite against, and migrated it first.

Four defects were behind it, and three of them share a shape. Tests created
roles and did not remove them, about a hundred a run, which matters because
`/admin/people/permissions` renders every permission for every role on one page:
the page the permission-grid tests read under a 4 MB body cap outgrew the cap at
around five hundred roles, and the failure then arrived as a body-length error
in tests that had nothing to do with roles. Teardown that did exist sat at the
bottom of a test body, which is the line a failing assertion skips, so the runs
that left state behind were exactly the runs that had already gone wrong — one
AI budget test left a per-user override and its usage rows, against a user whose
name was fixed and whose id therefore survived into the next run, and three
files created a scratch database per test and dropped it on their last line,
leaving a migrated database on the server for every red run. And the harness
seeded the `language` table before `AppState` ran the migrations, so `createdb`
followed by `cargo test` failed at the first target that needed the shared app.

The fixes are at those causes rather than at the symptoms. `common::defer_cleanup`
registers teardown where the state is created and runs it from `run_test`,
including while a failing assert unwinds; `common::create_test_role` is now the
one way a test makes a role and removes it again, replacing eight hand-written
copies of the same insert; `common::ensure_database_migrated` runs the
migrations before anything in the fixtures reads a table;
`trovato_test_utils::ScratchDb` drops its database in `Drop`. The permission
grid's 4 MB cap is unchanged, because raising it only moves the cliff.

One smaller thing came with it. Nine test files had `redis://127.0.0.1:6379`
written out while the rest read `REDIS_URL`, so pointing the suite at another
Redis — a second checkout, a non-default port — moved some of it and not the
others, and whatever was left behind failed in whatever way a foreign Redis
happened to produce. They all go through `trovato_test_utils::env::redis_url()`
now.
