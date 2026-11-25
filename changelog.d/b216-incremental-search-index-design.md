The design for a native, incremental, Pagefind compatible search indexer is
written down in `docs/design/incremental-search-index.md`, together with the
evidence it rests on. Nothing about search changes in this release: the index is
still rebuilt whole by the `pagefind` CLI after every change, exactly as before.

The full rebuild was measured on the largest content set available locally
(1,237 searchable items, 223 visible comments). The design covers a page
ledger in Postgres, which gives each item a page number for its whole life and
reuses a deleted item's slot. It also covers a per item term cache keyed by
content hash, word chunk ranges frozen at the last full build, a write order
under which a reader never sees a meta file pointing at a missing chunk,
compaction once tombstones pass 5% of page slots, and a change queue to replace
the rebuild flag. It sets out three compatibility gates that the production
indexer has to pass.

A new dev tool, `crates/pagefind-harness` (`trovato-pagefind-harness`, not
published, not a kernel dependency), depends on the `pagefind` crate pinned to
`=1.3.0`, the release the Dockerfile downloads. It decodes a built index
(gunzip, the `pagefind_dcd` signature, CBOR into structs that mirror Pagefind's
`MetaIndex` and `WordIndex`), and it proves that the crate, given one page at a
time, produces the same words, positions, weights, word count, meta and
fragment for every page as a full CLI build. Its `tokenizer_parity` test runs
that proof over 60 exported items it carries in `corpus/`. The comparison
against the crate's own full build always runs. The comparison against the CLI
runs when `PAGEFIND_BIN` or `pagefind` on `PATH` is present, and skips with a
message otherwise.

The tool is its own Cargo workspace with its own lockfile, excluded from the
root one, so CI neither builds nor runs it. The `pagefind` crate depends on
`actix-web` for `pagefind --serve`, and through it on two advisories that have
no fix inside pagefind 1.3.0's version ranges: RUSTSEC-2026-0443
(`actix-files` 0.6) and RUSTSEC-2026-0258 (`h2` 0.3). Kept apart, neither
reaches the root lockfile the product ships and audits from, and the audit
ignore list stays empty.

`appearance_test`'s `a_theme_that_does_not_parse_cannot_be_activated` failed
intermittently in CI, on `main` as well, with the active theme `stock` where it
expected `lakeside`. An earlier theme test's switch bumps the themes cache
generation from a background task, and this server's watcher answers the bump
by reloading the stored choice, which cleanup had already deleted. When the
reload landed after the test had set Lakeside, it replaced it. The test now
waits for pending generations to settle before it sets the theme.
