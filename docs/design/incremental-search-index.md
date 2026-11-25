# Incremental search index

Status: built. Part 2 (B217) is the codec and the native full build through
the ledger, with no `pagefind` binary. Part 3 (B218) is the change queue, the
incremental updater, compaction and reader recovery. "Part 2 as built" and
"Part 3 as built" say where each differs from the design below, which is kept
as written.

## The problem

One comment costs a rebuild of the whole site's index. `request_rebuild()`
(`crates/kernel/src/cron/pagefind.rs`) sets
`pagefind_index_status.rebuild_requested`. It is called by the item service
(`request_search_rebuild`), the comment service, and the content type admin.
Cron's `maybe_rebuild_index` then runs `build_index_inner`, which goes through
these steps:

1. It writes every public item as `<id>.html` into a temporary directory that
   sits beside `PAGEFIND_OUTPUT_DIR`.
2. It runs `pagefind --site <tmp> --output-path <tmp>/_pagefind`, with a 120 s
   timeout.
3. It renames the old index out of the way, renames the new one in, and deletes
   the old one.

Pagefind has no incremental path. The crate's only public API is
`pagefind::api::PagefindIndex`, with `add_html_file`, `add_custom_record`,
`build_indexes`, `write_files` and `get_files`. Every write rebuilds every page
in memory, and the API cannot load an existing index. Tokenizing, HTML parsing
(`lol_html`) and the `minicbor` index structs live in private modules. Of
Pagefind's parts, only `pagefind_stem` is published as a separate crate.

## What today costs

### The corpus

This was measured on the largest content set available on this machine. I
excluded the live and production-check sites by rule, and the 20 million row
synthetic benchmark database, because it is not real content and is far past
the CLI's 120 s timeout. The set used is the `trovato_test` database of the
`bg14` run, copied with `pg_dump` into a private container so nothing it
belongs to was touched:

* 2,454 items, 1,270 published, 1,237 of them searchable: published, live
  stage, and visible to the anonymous role through the access seam.
* 481 comments, 265 published. 223 of them are visible on searchable items, and
  124 items carry at least one.
* 29 content types. The largest groups are 431 `conference` items and 145
  `page` items; the rest are test fixtures.

It is a long-lived test database, not a production site. Its pages are short:
the median fragment is 430 bytes and the largest is 892 bytes.

### Timings

These come from `pagefind-harness measure --runs 3`. That subcommand builds the
full `AppState` with the same plugins cron loads, then repeats `build_index`
step for step. Export is `indexable_documents` plus writing the files. CLI is
the `pagefind` 1.3.0 run. Swap is the two renames plus deleting the backup and
the temporary directory. The machine is a Mac Studio running a release build.

| run | items | export | CLI | swap and cleanup | total |
|---|---|---|---|---|---|
| 1 | 1,237 | 105 ms | 1,637 ms | 79 ms | 1,821 ms |
| 2 | 1,237 | 127 ms | 1,625 ms | 111 ms | 1,864 ms |
| 3 | 1,237 | 112 ms | 1,945 ms | 128 ms | 2,185 ms |

The CLI is 88 to 89% of the total. On this corpus a full rebuild is about two
seconds. That is not slow in absolute terms, but it is paid for every single
write, it grows with the site rather than with the change, and it rewrites every
file.

### Index size and files

| kind | files | bytes |
|---|---|---|
| `.pf_fragment` | 1,237 | 262,398 |
| `.pf_index` | 2 | 49,385 |
| `.pf_meta` | 1 | 7,217 |
| `.js` | 4 | 169,709 |
| `.css` | 2 | 21,822 |
| `.pagefind` (WASM) | 1 | 66,928 |
| `.json` (entry) | 1 | 108 |
| total | 1,248 | 577,567 |

### The language the index is built as

The index is built as `unknown`. The generated pages carry no `lang` on
`<html>`, so Pagefind files every page under the `unknown` language. That
language has no stemmer and no language-specific WASM. The evidence is in the
output:

* the meta file is `pagefind.unknown_d060c09b4a40407.pf_meta`;
* the WASM is `wasm.unknown.pagefind`;
* `pagefind-entry.json` is
  `{"version":"1.3.0","languages":{"unknown":{"hash":"unknown_d060c09b4a40407","wasm":null,"page_count":1237}}}`.

So today "running" does not match "run", even though every item in the database
has `language = 'en'`.

## The tokenizer proof

The question part 2 depends on is this: given one page alone, does Pagefind's
own crate produce exactly what a full CLI build produces for that page? If it
does, a per item term cache can be filled by the stock tokenizer. Then only the
index assembly (the numbering, the postings and the files) has to be native,
and the tokenizing never has to be reimplemented.

### The harness

The harness is `crates/pagefind-harness` (`trovato-pagefind-harness`). It is
`publish = false`, and nothing in the kernel depends on it. It depends on
`pagefind = "=1.3.0"`, which is exactly the release the Dockerfile downloads
(`pagefind-v1.3.0-*-unknown-linux-musl`). The two move together.

It is its own Cargo workspace, with its own `Cargo.lock`, listed in the root
workspace's `exclude`, so it is not a member. The reason is the `pagefind`
crate itself. Version 1.3.0 depends unconditionally on `actix-web` for
`pagefind --serve`, which the harness never calls. Through that it pulls in two
advisories that the repository's pinned audit fails on:

* RUSTSEC-2026-0443, in `actix-files` 0.6.10; the fix is in 0.7.4;
* RUSTSEC-2026-0258, in `h2` 0.3.27; the fix is in 0.4.16.

Neither fix fits inside pagefind 1.3.0's version ranges. As a member, the
harness would have put 117 crates and both advisories into the root lockfile
the product ships and audits from. Passing the audit would then have meant the
first two entries in `.cargo/audit.toml`'s empty ignore list. Kept apart, the
root lockfile is unchanged, the kernel's dependency graph never contains
either crate (`cargo tree -p trovato-kernel -i actix-files` finds nothing),
and the harness is built and tested by its own CI job (see below).

**Part 2 inherits this.** A production codec that depends on the `pagefind`
crate at runtime would ship `actix-web` and both advisories in the kernel.
Part 2 should take the tokenizer by **vendoring `fossick`** (its three files
plus the `fragments` types, with `lol_html`, `pagefind_stem` and
`unicode-segmentation`) at the pinned version, and gate it with this harness,
rather than depend on the crate.

It works in four parts:

* **Decode.** It gunzips each file, strips the `pagefind_dcd` signature and CBOR
  decodes the result. It uses `minicbor` 0.19, the same minor as Pagefind 1.3.0,
  with `Decode` structs that mirror Pagefind's `MetaIndex`, `MetaPage`,
  `MetaChunk`, `MetaFilter`, `MetaSort`, `WordIndex`, `PackedWord` and
  `PackedPage` field for field and index for index. Fragments are read as JSON.
  Positions and weights are decoded the way `pagefind_web/src/index.rs` decodes
  them.
* **(a)** It runs the CLI over the export directory with exactly
  `build_index_inner`'s arguments.
* **(b)** For each page alone, it runs `PagefindIndex::new(None)`, then
  `add_html_file(Some("<id>.html"), None, html)`, then `get_files`, and decodes
  the result. It requires exactly one page out.
* **(c)** It decodes the CLI's output and matches pages by URL; the URL is the
  fragment's `url`, which the CLI derives from the same `<id>.html` path. For
  every page it asserts that the following are identical: the set of words,
  every word's raw `locs` (positions with their inline weight changes), the
  `MetaPage` word count, the whole fragment JSON (url, content, word_count,
  filters, meta, anchors), and the fragment's file name.

It also runs two further checks:

* **The crate's own full build.** It adds every page to one `PagefindIndex` in
  sorted order and compares by URL. This needs no CLI.
* **Bytes in CLI order.** It recovers the CLI's page order from its page
  numbers, feeds the same pages through the crate in that order, and compares
  every file of the two bundles byte for byte.

### Corpora and results

| corpus | pages | postings | positions | mismatches vs CLI | index files byte identical |
|---|---|---|---|---|---|
| full export of the corpus above | 1,237 | 10,533 | 11,926 | **0** | yes, 1,247 of 1,247 |
| `corpus/`: 60 of those, sampled and committed | 60 | 557 | 625 | **0** | yes |
| stress, from `stress.rs` | 40 | 3,160 | 118,673 | **0** | yes, 7 word chunks |

The committed `corpus/` holds 60 items exported by
`pagefind-harness export --sample 60`, using the kernel's own
`indexable_documents` and `render_fragment` and the `<id>.html` naming. A
quarter of them are drawn from items with visible comments (15 items, 24
comments); the rest are sampled systematically, so types appear in proportion.

Because the real pages are short and almost all ASCII, I added the stress
corpus. It is deterministic, and it is rendered through the kernel's
`render_fragment` from constructed `IndexDocument`s. It has long text (up to
2,700 words a page), which crosses the 20,000 location chunk cut six times. It
also covers:

* hyphens, apostrophes, dots, slashes, `&`, and HTML entities;
* non-breaking spaces;
* accented Latin, Greek, Cyrillic, CJK and Hangul text;
* emoji and numbers;
* very long words;
* all four field weights, comments, and every meta field.

**Result: the proof holds.** Per page tokenizing through the public crate API
is identical to a full CLI build on every page of every corpus. That covers
words, positions, weights, word count, meta, the fragment, and the fragment
file name. So vendoring `fossick` is not needed to get parity.

In the CLI's page order, the crate writes the CLI's index files byte for byte
(meta, word chunks, fragments, entry). The only file that differs is
`wasm.unknown.pagefind`, which is the browser runtime, not index data:

* The macOS release binary embeds a WASM compiled on a macOS runner. Its panic
  paths read `/Users/runner/...`.
* The crate's copy, and the Linux musl binary's copy, were compiled on a Linux
  runner (`/home/runner/...`).

The Linux binary that production runs embeds a WASM that decompresses to exactly
the crate's (SHA-1 `8a9a988e…`). Only the gzip header's timestamp differs. Its
index files are identical to the macOS CLI's on the same corpus. The harness
reports runtime files separately and requires only the index files to match.

Each page costs about 0.85 ms through the crate API on the short pages, and
about 5 ms on the stress pages. Most of that is `get_files`, which minifies
`pagefind.js` again for every page. Part 3 can skip the minify by vendoring
`fossick` (see [Version bumps](#version-bumps)), but at these numbers it does
not need to.

### Rerunning it

```sh
# The integration test: the crate comparisons always, the CLI ones when present.
PAGEFIND_BIN=/path/to/pagefind-1.3.0 \
  cargo test --manifest-path crates/pagefind-harness/Cargo.toml --test tokenizer_parity -- --nocapture

# Over any export. Run from the repository root, against a copy of a site
# (DATABASE_URL, REDIS_URL and PLUGINS_DIR as for the server). Start-up
# migrations are switched off, but the server state still starts as `serve`
# would. The output directory must be new or empty.
cargo run --release --manifest-path crates/pagefind-harness/Cargo.toml -- export --out /tmp/site
PAGEFIND_BIN=/path/to/pagefind-1.3.0 \
  cargo run --release --manifest-path crates/pagefind-harness/Cargo.toml -- parity --site /tmp/site

# The CLI rebuild cost part 1 measured:
PAGEFIND_BIN=... cargo run --release --manifest-path crates/pagefind-harness/Cargo.toml -- \
  measure --runs 3 --output /tmp/pagefind-out

# The native full build, as cron now runs it. It writes the ledger and term
# cache of the database it is pointed at, so point it at a copy. --cold empties
# the term cache before every run.
cargo run --release --manifest-path crates/pagefind-harness/Cargo.toml -- \
  measure-native --runs 3 --output /tmp/pagefind-native [--cold]

# Time to searchable after one title edit and one comment, through the item
# and comment services and the cron updater, against a full build (part 3).
# It writes items and comments: point it at a copy.
cargo run --release --manifest-path crates/pagefind-harness/Cargo.toml -- \
  measure-update --runs 3 --output /tmp/pagefind-update
```

**CI runs all of these tests, the CLI comparisons included.** The harness is not
a root workspace member, so no other job builds it. The `Pagefind Harness` job
in `ci.yml` does it on its own manifest and lockfile:

* it downloads the pinned `pagefind` 1.3.0 Linux release and sets
  `PAGEFIND_BIN`;
* it runs `fmt`, `clippy -D warnings` and `cargo test --locked`;
* it fails if any CLI comparison printed `SKIPPED`.

Without a CLI (on a laptop, say), the crate-only comparisons on `corpus/` and
the stress set, the decoder and export unit tests and the corpus check still
run. The two CLI comparisons then print `SKIPPED: no pagefind CLI ...` and
pass.

## What Pagefind 1.3.0 actually writes

Part 2 must reproduce these details. Several of them differ from what later
releases do.

* **Page numbers are positions in `MetaIndex.pages`, in build order. At 1.3.0
  they are written plainly, not delta encoded.** Delta encoded postings,
  `meta_locs` and diacritic `additional_variants` all arrive in later releases
  (1.6.0 has all three). This is the first reason the codec is pinned to one
  release.
* **`locs`** is the list of word positions on the page, sorted so the positions
  at weight 25 (Pagefind's 1.0) come first. A change of weight is written inline
  as `-(weight) - 1` before the positions it applies to. Weights are
  `data-pagefind-weight × 25`, so 10.0 is 250.
* **Word chunks.** Words are sorted by byte order and cut into a new chunk
  whenever the running sum over postings of `locs.len() + 1` reaches 20,000.
  Each chunk's `from` and `to` are then shortened to the shortest prefix that
  still tells them apart from the neighbouring boundary (`get_prefixes`).
  `pagefind_web` loads **one** chunk per query term: the first whose
  `from <= term <= to`. If no chunk matches, it retries with both sides trimmed
  to the term's length.
* **File names.** A file is named `<lang>_` plus the first `lang.len() + 8`
  characters of its SHA-1, so `unknown_` plus 7 characters. The SHA-1 "hex" is
  written as `format!("{:x?}", byte)` per byte, which is **not zero padded**: a
  byte `0x05` is written `5`, not `05`. The native codec must copy this exactly
  to produce byte-identical names. Short-hash collisions are resolved by
  extending the name, in build order. With stable page numbers, "build order"
  becomes ledger order, which keeps the outcome deterministic.
* **Fragments.** Each fragment is
  `serde_json::to_string(PageFragmentData{url, content, word_count, filters, meta, anchors})`.
  Two byte-identical fragments share one file.
* **Compression.** Every `.pf_*` file is gzipped at `Compression::best()` with
  the 12 byte `pagefind_dcd` signature inside the stream. The fixed-name runtime
  files, the WASM and `pagefind-entry.json` are written uncompressed. The WASM
  file is already gzip.
* **`pagefind-entry.json`** holds `{version, languages: {lang: {hash, wasm, page_count}}}`.
  `page_count` is the number of page slots, and the JS uses it only to pick the
  default language.

## The design

### Page ledger

```sql
CREATE TABLE search_page (
    language     text    NOT NULL,          -- the Pagefind language key
    page_number  integer NOT NULL,          -- position in MetaIndex.pages, for life
    item_id      uuid    NULL UNIQUE,       -- NULL: a tombstone
    freed_at     timestamptz NULL,          -- when it became a tombstone
    PRIMARY KEY (language, page_number)
);
```

* Each searchable item gets a page number in its language's index when it is
  first indexed, and keeps it for life. Edits never move it.
* When an item is deleted, unpublished, switched out of search, or loses
  anonymous visibility, its slot becomes a **tombstone**: `item_id` is set to
  NULL and `freed_at` is set. In the meta file, a tombstone is
  `MetaPage { hash: <tombstone fragment>, word_count: 0 }`, and no word posts to
  it. One shared tombstone fragment per language, with an empty url, content and
  meta, means no meta file ever points at a missing file.
* **The free list** is the set of tombstones. The next new item takes the
  **lowest** free page number in its language, so reuse is deterministic. Only
  when the free list is empty does a new slot go on the end.
* **An item that changes language** is a delete in the old language's index and
  an upsert in the new one.
* **The full build reads the same ledger.** It numbers pages by
  `search_page.page_number` and writes tombstones where the ledger has them. So
  a full build and an incremental build of the same state number every page
  identically, which is what gate 1 relies on. Only compaction renumbers.

### Term cache

```sql
CREATE TABLE search_page_terms (
    item_id       uuid    PRIMARY KEY,
    language      text    NOT NULL,
    content_hash  bytea   NOT NULL,   -- SHA-256 of the rendered fragment HTML
    word_count    integer NOT NULL,
    fragment      text    NOT NULL,   -- the fragment JSON, verbatim
    words         bytea   NOT NULL    -- CBOR map: word -> locs (i32 array, as encoded)
);
```

* The key input is the HTML `render_fragment` produces, which is exactly what
  the tokenizer reads. If the hash is unchanged, there is nothing to do; this is
  common, because a comment edit on an item that does not show comments in
  search leaves its fragment unchanged.
* To update an item, read its cached `words` and remove its page from each of
  those words' postings. Tokenize the new HTML with `PagefindIndex` (one page,
  proven identical above), add the new postings, and store the new row. The
  cache is the only way to find stale postings without decoding every chunk.
* Words and `locs` are stored in their encoded form, so the codec copies them in
  without reinterpreting anything.

### Frozen word chunk ranges

```sql
CREATE TABLE search_chunk (
    language    text    NOT NULL,
    ordinal     integer NOT NULL,     -- chunk order
    from_word   text    NOT NULL,     -- as in MetaChunk, prefixes included
    to_word     text    NOT NULL,
    locations   integer NOT NULL,     -- running count, the 20,000 rule's measure
    hash        text    NOT NULL,     -- current file name stem
    PRIMARY KEY (language, ordinal)
);
```

* The ranges come from the last full build, cut by Pagefind's own 20,000
  location rule and prefix shortening, and stay frozen until the next full
  build.
* **A word goes to the chunk whose `[from_word, to_word]` holds it**, found by
  binary search over `ordinal`.
* **A word in a gap** (after chunk *i*'s `to_word` and before chunk *i + 1*'s
  `from_word`) goes to the **nearer neighbour**. Nearer means the longer common
  prefix with that neighbour's boundary, and a tie goes to the lower chunk. That
  chunk's boundary is widened to the word, and the boundary pair is
  re-shortened with Pagefind's `get_prefixes` so the ranges stay disjoint and
  `pagefind_web`'s single-chunk lookup still finds the word. A word before the
  first chunk widens the first chunk's `from_word`, and a word after the last
  widens the last chunk's `to_word`.
* **Only touched chunks are re-encoded.** A touched chunk is one that gained or
  lost a posting. It gets a new content hash and therefore a new file name. An
  untouched chunk keeps its file and its name.
* **A chunk that grows past twice the cut (40,000 locations) is split** at the
  word where the running count crosses 20,000, using the same rule. A chunk that
  empties is dropped, and its range is merged into the neighbour that gap
  assignment would have chosen. Both are rare, and both stay inside the frozen
  model: they change the plan by one chunk and are recorded in `search_chunk`.
* With no filters and no sorts (true today; see below), a typical comment
  rewrites one fragment, the one to three chunks holding that item's words, the
  meta file and the entry file. A full rebuild rewrites every file.

### Write order

All writes go into the live directory. Every file except
`pagefind-entry.json` is content addressed and never overwritten:

1. Write the new fragments and the new word chunk files (and filter files, if
   filters ever exist). Each is written to a temporary name in the same
   directory and then renamed.
2. Write the new `pagefind.<lang>_<hash>.pf_meta`, the same way.
3. Write `pagefind-entry.json` to a temporary name and rename it over the old
   one. This is the single atomic pointer flip. Before it, readers see the old
   meta, whose files are all still present. After it, readers see the new meta,
   whose files were all written first.
4. Delete superseded files by age, not by generation count. Record when each
   file stopped being referenced by the current meta (in the state tables, at
   the flip). Delete it once it has gone unreferenced for the **retention
   window**, 24 hours, however many generations have been published since.

At the moment of each flip, the published meta never names a missing file:
everything it names was written in steps 1 and 2. A reader is a different
matter. `pagefind_web` reads the meta once, at `init()`, and keeps asking for
the chunk and fragment names that meta lists until the page reloads. Counting
generations cannot bound that, because a busy site publishes many in an hour.
The age-based window can, but only for readers younger than the window. A tab
left open for longer than 24 hours can still ask for a deleted chunk.

No retention policy can rule that out, because nothing on the server knows how
long a tab has been open. So the guarantee has two parts:

* **Retention.** Any reader that loaded its meta within the last 24 hours finds
  every file that meta names.
* **Recovery.** An older reader that hits a missing file reloads the index:
  `scolta.js` catches a failed search, calls `pagefind.destroy()` and
  `pagefind.init()` again, which fetch the current entry and meta, and repeats
  the search once. That is a small change to `scolta.js` in part 3; the browser
  bundle itself is untouched.

Part 3 shipped the recovery in `scolta.js` (see "Part 3 as built" below).
`pagefind-entry.json` is fetched as `pagefind-entry.json?ts=<now>` (in
`coupled_search.ts`), so browser caching of it does not delay the flip.

### Compaction

* **Trigger.** Compaction runs when tombstones are more than **5% of page
  slots**, measured **after free list reuse**: when cron has drained a batch,
  `tombstones / slots` per language is above 0.05.
* **What it does.** It is a full rebuild through the ledger that renumbers
  densely: live items keep their relative order, tombstones are dropped, the
  free list empties, the chunk plan is re-cut by the 20,000 rule, and every file
  is rewritten. It is logged at `info` with the measured ratio.

#### What tombstones do to ranking

There are three effects at Pagefind 1.3.0. Throughout, *t* is the fraction of
page slots that are tombstones, *N* is the number of slots, and *L = N(1 − t)*
is the number of live pages.

1. **Average page length.** `metadata.rs` sets
   `average_page_length = Σ word_count / pages.len()`, and the count includes
   tombstones. Tombstones add 0 to the sum, so the average shrinks by the factor
   *(1 − t)*. Every page's `document_length / average_page_length` grows by
   *1 / (1 − t)*. BM25's length normalisation (`b = 0.75`, `k1 = 1.4`, which are
   Pagefind's defaults; Trovato sets no ranking options) then lowers every
   term's TF part. Longer pages and pages with fewer occurrences lose most.
2. **IDF.** `search.rs` uses `total_pages = self.pages.len()`, which also counts
   tombstones. In 1.3.0 this is computed inline in `calculate_bm25_word_score`;
   later releases call it `calculate_idf`. Pagefind's IDF is
   `ln((N − n + 0.5)/(n + 0.5) + 1)`, which simplifies to
   `ln(N + 1) − ln(n + 0.5)`, so tombstones add the **same constant**,
   about −ln(1 − t), to every term's IDF. In a one-term query that is a common
   factor and changes no order. In a multi-term query it raises common terms
   relative to rare ones.
3. **All-pages and negated filter results.** The "all pages" set (`search.rs`)
   is used only when no term was given (`search(null, …)`, a filter-only
   listing). It is every page index from 0 up to `pages.len()`, so tombstones
   are returned. A `not`/`none` filter complements its set against the same
   range (`filter.rs`), and it returns tombstones too. **Trovato uses neither
   today.** `scolta.js` never calls `search(null)`. Its warm-up
   `pagefind.search("")` returns an empty result in JS before it reaches the
   WASM, because an empty term that is not `null` stops there. No item carries a
   `data-pagefind-filter`, so the only filter `scolta.js` can send (`site`,
   from a `site` meta no export writes) never appears. A query with terms
   intersects with pages that have words, and a tombstone has none.

The size of the first two effects:

| t | average length × | length ratio × | IDF shift | IDF, rare term (n = 10) | IDF, common term (n = 500) | TF, f=1 at average length | TF, f=1 at 2× average | TF, f=4 at average length | TF, f=4 at ½ average |
|---|---|---|---|---|---|---|---|---|---|
| 1% | 0.990 | 1.010 | +0.010 | +0.21% | +1.11% | −0.44% | −0.61% | −0.20% | −0.11% |
| 5% | 0.950 | 1.053 | +0.051 | +1.07% | +5.66% | −2.25% | −3.10% | −1.01% | −0.56% |
| 10% | 0.900 | 1.111 | +0.105 | +2.21% | +11.62% | −4.64% | −6.33% | −2.11% | −1.18% |

Notes on the table:

* The IDF columns use *L* = 1,237, this corpus. The shift in nats does not
  depend on *L*.
* *f* is the weighted term frequency, where 1.0 is one occurrence at weight 1.
* The TF columns are the BM25 TF factor relative to the same page with no
  tombstones.

At 1% every effect is about 1% or less: at most 0.6% on TF, and 1.1% on a
common term's IDF (0.2% on a rare one). That is too small to reorder any but
near-tied results. At 5% they reach
about 3% on TF and about 6% on a common term's IDF. That is enough to swap two
results that were within a few percent of each other, but it will not reorder a
page list wholesale. That is why 5% is the trigger. At 10% a long page loses
over 6% on TF and common terms gain almost 12% on IDF.

Gate 3 is run on a compacted index and an uncompacted one. Run at 5%, it
measures how many results actually move.

### The change queue

It replaces the rebuild flag.

```sql
CREATE TABLE search_index_queue (
    id          bigserial PRIMARY KEY,
    item_id     uuid      NOT NULL,
    op          smallint  NOT NULL,    -- 1 upsert, 2 delete
    reason      text      NOT NULL,    -- 'item_update', 'comment', 'type_settings', ...
    enqueued_at timestamptz NOT NULL DEFAULT now()
);

-- One row: what is published, for the single-writer check below.
CREATE TABLE search_index_state (
    id               boolean PRIMARY KEY DEFAULT true CHECK (id),
    generation       bigint  NOT NULL,   -- bumped by every publish
    pagefind_version text    NOT NULL,   -- the pinned version that wrote it
    entry_meta       jsonb   NOT NULL    -- language -> meta file named in the entry
);
```

* **Enqueuing.** Callers enqueue item ids instead of calling
  `request_rebuild()`:
  * the item service enqueues on publish, edit, unpublish and delete;
  * the comment service enqueues the comment's item;
  * the content type admin enqueues every item of the type, as one
    `INSERT … SELECT`.

  An upsert of an item that is no longer searchable is a delete. The updater
  decides searchability with the same `indexable_documents` seam, restricted to
  the queued ids.
* **Draining.** There is **one publisher for the whole index at a time**. It
  is not one per language, because `pagefind-entry.json` lists every language and is shared
  between them. Each drain
  runs in a single transaction that does the following, in order:
  1. Takes `pg_advisory_xact_lock(hashtext('search_index'))`
     before reading anything. A second drainer, on this server or another,
     blocks there until the first commits or rolls back. It then reads the
     state the first one published and builds on it, rather than on the
     generation both started from.
  2. Reads `search_index_state.generation` (one row for the index), and the batch:
     queue rows, coalesced by item, last op wins.
  3. Renders, tokenizes and applies the batch, and follows the write order
     above.
  4. Updates the ledger, term cache, chunk rows and state with
     `UPDATE search_index_state SET generation = generation + 1 … WHERE
     generation = $read`.
  5. Deletes the queue rows it applied, and commits.

  The `WHERE generation = $read` is a second guard, independent of the lock:
  if anything else published in between, the update touches no row. The drain
  then rolls back, leaving its queue rows in place, and the batch is retried
  from the new state. The files it already wrote are content addressed and
  merely unreferenced, so retention removes them.

  The entry file is renamed in step 3, under the lock and before the commit.
  If the commit then fails, the entry may name a meta the database does not
  record. The next drain sees the mismatch (the meta named in the entry is not
  the state row's) and takes a loud full rebuild. If the process crashes, the
  rows are still there and the next run redoes them.

  `FOR UPDATE SKIP LOCKED` is not used. With one publisher for the index there
  is nothing to skip, and concurrent drainers each publishing a partial entry
  is exactly what the lock prevents.
* **The full rebuild stays as a fallback.** It is the full build through the
  ledger, and it is **taken loudly**: a `warn` log line naming the reason
  whenever a precondition fails. The preconditions are:
  * no ledger yet, or the ledger and the files on disk disagree (the meta file
    named in the entry is missing, or its hash is not what the state row
    records);
  * the Pagefind version recorded in the state row differs from the pinned
    one;
  * the batch's language set includes a language with no index yet;
  * a chunk file the updater needs to rewrite fails to decode;
  * the compaction threshold is passed;
  * an operator asks for one (`trovato search rebuild`, part 3).
* **Plugin writes and scheduled publishing** go through the same enqueue path.
  That is folded into part 3.

### Language

* Set `<html lang="…">` in `render_fragment` from `item.language`, falling back
  to the site default when it is empty or `und`. Pagefind then stems:
  `pagefind_stem` covers the WASM languages it ships (en, de, fr, es, … 28 in
  all), and the language-specific WASM is used in the browser.
* There is one index per language, as Pagefind builds them: one ledger, chunk
  plan and meta file per language key, and one entry file listing all of them.
  The browser loads the index for the page's own `lang`.
* Pagefind merges pages without a `lang` into the site's most common language.
  The native build does the same through the fallback, so no `unknown` index
  appears next to real ones.
* **This changes results.** "running" will match "run". So it ships behind the
  part 2 full build, with gate 3's query set rerun on stemmed queries, and not
  as a change to the CLI path on its own.

### Filters and sorts

There are none today. If either is added later, filter files must be rewritten
whenever a page's filter values change and must leave tombstones out, and
`MetaSort` lists must leave tombstones out. Both join gate 1.

## Compatibility gates

Parts 2 and 3 must pass all three gates on every change to the codec, and again
on every Pagefind bump:

1. **Incremental equals full, byte for byte, through the same code.** Start from
   a full build of state *S*, apply a sequence of changes through the updater
   (upserts, deletes, free list reuse, gap words, a chunk split), and compare
   the resulting bundle with a full build of the resulting state through the
   ledger. Every index file must be identical, same names and same bytes.
2. **Decoded equality with the CLI on the same corpus.** Build a dense ledger
   (no tombstones), decode both bundles with this harness's decoder, and remap
   page numbers by URL. Every page's words, locations, weights, word count,
   meta and fragment must be identical. This is the comparison `parity.rs`
   already does; part 2 swaps its candidate from "the crate, one page at a time"
   to "the native build".
3. **Same results in the browser.** Load the stock `pagefind.js` and WASM from
   the pinned release in headless Node against each index. Run a fixed query
   set, which must include single terms, multi-term queries, prefixes,
   punctuation, a non-ASCII term, a term only in comments, and a term only in a
   title. The result URLs must come back in the same order from the native
   index and from the CLI's. Run it on a dense native index, which must be
   exactly equal, and on a native index at 5% tombstones, where the number of
   moved results is reported and must stay under a stated bound.

## Version bumps

* Pin exactly, in the places that move together: `pagefind = "=X.Y.Z"` in
  `crates/pagefind-harness/Cargo.toml`, `PAGEFIND_VERSION` and the vendored
  `fossick` and runtime files in `crates/search-index`, and the CLI release URL
  the CI jobs download for the comparisons. The state row records the version; a
  mismatch forces a loud full rebuild.
* Rerun all three gates on the new version before merging the bump. The 1.3.0
  to 1.6.0 changes listed above (delta encoded page numbers, `meta_locs`,
  diacritic variants, configurable chunk size) each break the codec, so a bump
  is a codec change, not a dependency update.
* Copy the browser bundle (`pagefind.js`, `pagefind-*.js/css` and the
  `wasm.*.pagefind` files) from the **same** release. Take it from the crate's
  `vendor/` directory or the Linux release binary; those two are the same WASM.
  Never mix it with another release's index.
* `/static/pagefind/*` is served with `max-age=86400`. The fixed-name runtime
  files can be stale in a browser for up to a day after a bump, while the
  content-addressed index files are new. Part 2 should serve the fixed-name
  runtime files with a short or `no-cache` lifetime, or version their path.
* If a future release makes per page output differ from the full build (none
  does at 1.3.0), vendor Pagefind's `fossick` module (`fossick/mod.rs`,
  `parser.rs`, `splitting.rs` and the `fragments` types) at the pinned version
  instead of calling the public API. Vendoring would also remove the repeated
  `pagefind.js` minify that dominates the per page cost.

## Part 2 as built

* **No `pagefind` binary.** The workspace crate `trovato-search-index`
  (`crates/search-index`) holds the codec (`codec.rs`, minicbor 0.19 with the
  same derive layout as Pagefind's structs, gzip with the `pagefind_dcd`
  signature, SHA-1 file names), the assembler (`assemble.rs`, Pagefind's
  `build_indexes` per language), the browser bundle (`bundle.rs`), the
  publisher (`publish.rs`) and the decoder the harness used to own
  (`decode.rs`). It does not depend on the `pagefind` crate: Pagefind 1.3.0's
  `fossick` module and `fragments` types are vendored in `src/vendor`, under
  their MIT licence (`LICENSE-PAGEFIND`), made synchronous and cut to what an
  in-memory page needs; every change is marked `TROVATO:`. The runtime files
  and the 29 `wasm.*.pagefind` files are copied from the pinned crate into
  `assets/pagefind-1.3.0/` by `pagefind-harness bundle`, and a harness test
  holds them equal to the crate's.
* **Ledger, term cache and state** are the tables above, in migration
  `20261202000001_search_index_ledger.sql`. `search_index_state` is one row:
  `generation`, `pagefind_version` and the published `entry_meta`. A fourth
  table, `search_index_file`, records when each content addressed file stopped
  being referenced, for the retention window.
* **The full build** (`crates/kernel/src/cron/search_index.rs`, called by the
  cron rebuild in place of the CLI): read the published generation, export the
  indexable items, render every item's fragment, hash it, reuse the cached
  terms when the hash matches and the state row's version is 1.3.0, and
  tokenize the rest. Then, in one transaction holding
  `pg_advisory_xact_lock(hashtext('search_index'))`: check that the generation
  is still the one read before the export, and if not, change nothing and ask
  for another rebuild (another build published a newer export, or an older
  one that the next build replaces). Tombstone the slots of items no longer
  indexable, give each new item the lowest tombstone in its language (new
  items taken in id order) or the next slot, store the terms, encode, stage
  the bundle in a directory beside `PAGEFIND_OUTPUT_DIR`, bump the generation,
  publish, sweep, commit. A language left with no live page loses its ledger
  rows and its index. An empty site publishes an empty index in the default
  language.
* **File names against the live directory.** Names carry seven hash
  characters, and a published file stays on disk for 24 hours after it stops
  being referenced, so a new file can share a name with an older file of
  different content. Publishing never overwrites a content addressed file, so
  the build names its files against the live directory: a name the directory
  holds with different bytes is lengthened, by Pagefind's own extension rule,
  and `publish` refuses a bundle that would still reuse one. With no such
  clash, which is almost always, the names are exactly Pagefind's.
* **Publishing is not transactional with the database.** Once
  `pagefind-entry.json` is renamed in, a failure to record the publish
  (sweep, commit, a crash) leaves a complete live index whose ledger,
  generation and `entry_meta` were rolled back. The build reports that as a
  rebuild to request, and the next full build re-derives everything and
  brings the two back into agreement. Part 3's updater must not start from a
  ledger whose `entry_meta` differs from the live entry; it should run a full
  build first.
* **Language** comes from `item.language`, falling back to the default row of
  `language` (then `en`) when it is empty or `und`, and is written as
  `<html lang>`. Every page has one, so the "no lang merges into the most
  common language" fallback is never needed by the kernel; the harness's native
  build implements it for CLI comparisons of language-less exports.
* **Publishing** follows the write order above, from the staging directory:
  data files, then metas, then the fixed-name runtime files that changed, then
  `pagefind-entry.json` by rename. It refuses before touching the live
  directory if the staged entry is missing, does not parse, or names a missing
  meta, so a build that wrote nothing leaves the published index alone. A file
  that cannot be renamed in (another filesystem) is copied to a temporary name
  in the live directory and renamed. The fixed-name files are served with
  `Cache-Control: no-cache`; the content addressed ones keep a day.
* **Gate 1** is a full build twice (and with the term cache emptied), byte for
  byte, plus a rebuild after deletes and an insert; the updater half arrives
  with part 3. **Gate 2** is as written. **Gate 3** compares results tie-aware:
  `pagefind_web` orders equal scores by page number, and the CLI numbers pages
  by its file walk while the ledger numbers them by first indexing, so equal
  scores may come back in a different order with identical scores. Within a
  run of equal scores the URL sets must match; across runs the order must. At
  5% tombstones the bound is 10% of result positions moved outside their tie
  group, and every query's result set must be unchanged. A query may fail only if it is
  on the gate's list of `pagefind_web` failures (German `zürich`, below) and
  fails on both indexes, and at least 20 queries must return results.
* **Gates in CI.** `crates/kernel/tests/search_index_gates_test.rs` runs all
  three over a 60 item fixture in English and German. Gates 2 and 3 skip
  without a CLI or Node; the Test job installs the pinned CLI and sets
  `TROVATO_REQUIRE_PAGEFIND_CLI`, which turns a skip into a failure.

## Part 3 as built

Part 3 replaces "every change asks for a full rebuild" with a change queue and
an updater that rewrites only what a change touched.

### Measured

These numbers come from the same corpus as part 1: a `pg_dump` copy of bg14's
`trovato_test` in a private container, 1,237 searchable pages, with this
branch's migrations applied. The tool is `pagefind-harness measure-update
--runs 3`, a release build on this machine. Each change is a real write
through the item or comment service, so it is queued as on a site. Then one
cron run of the index (`run_index`, as `maybe_rebuild_index` calls it) applies
it. Finally the published index is decoded to check that the new word finds
the item.

| change | write | cron update | time to searchable | pages changed | files written | chunks rewritten |
|---|---|---|---|---|---|---|
| title edit | 17 ms | 21 ms | 45 ms | 1 | 5 | 2 |
| title edit | 13 ms | 22 ms | 41 ms | 1 | 5 | 2 |
| title edit | 13 ms | 20 ms | 40 ms | 1 | 5 | 2 |
| comment | 13 ms | 21 ms | 40 ms | 1 | 5 | 2 |
| comment | 6 ms | 21 ms | 34 ms | 1 | 5 | 2 |
| comment | 7 ms | 20 ms | 34 ms | 1 | 5 | 2 |
| title edit, then a full build (the old path) | 12 ms | 248 ms | 260 ms | 1,237 | 9 | all |
| title edit, then a full build (the old path) | 10 ms | 254 ms | 264 ms | 1,237 | 11 | all |
| title edit, then a full build (the old path) | 12 ms | 266 ms | 277 ms | 1,237 | 12 | all |

How to read the table:

* **Time to searchable** runs from the start of the write to the end of the
  cron run that publishes it. Cron itself is triggered from outside, so on a
  site you add the wait for the next cron run on top.
* **Files written per update: 5.** They are the item's fragment, the two word
  chunks its old and new words are in, the language's meta file, and
  `pagefind-entry.json`. Before part 2, the CLI rebuild rewrote all 1,248 files
  and took 1.8 to 2.2 s. The part 2 full build is content addressed too, so it
  writes only the files whose bytes changed, but it still exports, renders,
  hashes and re-encodes all 1,237 pages and every chunk. Because it re-cuts the
  chunk plan, it also changes more chunk files than the update does.
* **A full build on its own** (`measure-update`'s first table) took 248 to
  320 ms. The first one into an empty directory writes 1,241 files.

### What was built

* **The queue.** It lives in `search_index_queue`, in migration
  `20261203000001_search_index_queue.sql`.
  * There is one row per item, keyed by `item_id`, with the item's `language`,
    an `op` (1 upsert, 2 delete), a `reason` and a `seq` from a sequence.
  * A second write to an item before the drain replaces its row and its `seq`.
    Coalescing therefore happens at enqueue time (last write wins), not at
    drain time.
  * A drain deletes exactly the `(item_id, seq)` pairs it read. A write queued
    while a drain runs stays for the next run.
  * Enqueuing is `crate::search::index_queue`: `enqueue`, `enqueue_type` (one
    `INSERT … SELECT` for a whole type) and `enqueue_stage_publish`.
* **The writers.** Each queues the items it changed, with a reason:
  * **The item service** (`item`), on create, edit, publish, unpublish, a move
    off Live, and delete (op 2).
  * **The comment service** (`comment`), queuing the comment's item.
  * **The content type search switches** (`type_settings`), **the search field
    weights** (`field_config`) and **the reindex button** (`reindex`). Each
    queues every item of the type that is published on Live or holds a page.
  * **Workspace publishing** (`stage_publish`), inside the publish's own
    transaction and before anything leaves the stage. It queues every item on
    the stage, every item the publish deletes, and every item whose alias
    moves.
  * **URL alias writes** (`url_alias`), because the alias is the result's link.
    They queue the item the alias names (`/item/<id>`), the old and the new
    one on an update.
  * **Plugins** (`plugin:<name>`), through the new `db` host function
    `search-index-enqueue` (`host::search_index_enqueue` in the SDK). The rule
    for plugin authors is in
    [plugin-development.md](../plugin-development.md#the-static-search-index-queue-what-you-change).
  * **Scheduled publishing** now finds what is due with a raw read and writes
    each change through `host::save_item`. It is therefore an item service
    write: it is queued, a revision is recorded, the write taps fire and the
    item cache drops the item. It used to write `item.status` with raw SQL,
    which nothing saw.
  * **`trovato_search`'s `tap_cron` no longer polls `MAX(changed)`.** With
    every writer queuing, that poll would have turned each edit back into a
    full rebuild. It now reports the queue length.
* **Full rebuild requests** are kept for what really is site wide:
  `trovato search rebuild` (new), `trovato generate`, and config import.
* **The updater** is `trovato_search_index::update::update_language`, which
  works on one language's meta, chunk plan and changed slots.
  * It reads the live meta and only the chunks a change touches.
  * It removes each changed page's old postings (from the term cache) and adds
    its new ones.
  * It routes a word in a gap to the nearer neighbour, with the tie going to
    the lower chunk. A word before the first chunk widens the first; one after
    the last widens the last.
  * It splits a chunk past 40,000 locations and drops a chunk that empties.
  * It re-encodes only the touched chunks, and writes a new meta.
  * New file names are chosen against the live directory and the files the
    same update already produced, by Pagefind's extension rule.
* **The drain** is `cron/search_update.rs`.
  * Each cron run drains up to 50 batches of 200 queued items, oldest first.
  * Each batch exports just its items through the same seam as a full build
    (`indexable_documents_for`, the access decision included), renders and
    hashes them, and tokenizes only the fragments whose hash the term cache
    lacks.
  * Then, in one transaction holding
    `pg_advisory_xact_lock(hashtext('search_index'))`, it checks the
    preconditions below and frees or reuses page slots (lowest tombstone
    first, as the full build does). It stages the changed files, records the
    ledger, terms, chunk plan and generation, deletes the drained rows,
    publishes (data files, then metas, then the entry by rename), sweeps and
    commits.
  * The cron Redis lock already keeps two cron runs on one site apart; the
    advisory lock and the generation check cover everything else.
* **The full build is taken, logged at `warn` with its reason,** in these
  cases:
  * there is no state row or no entry file;
  * the live entry names a meta other than the state row's `entry_meta` (a
    crash between the flip and the commit);
  * the recorded Pagefind version differs from the pinned one;
  * a meta or chunk the update needs does not read, or a meta's slot count
    differs from the ledger's;
  * a cached term set is missing;
  * a batch puts a page in a language with no index yet, or leaves a language
    with no page;
  * more than 1,000 items are queued and that is more than a quarter of the
    index's slots, where a full build is cheaper.
* **Compaction** is a site setting, `index_compaction_percent` in
  `SearchSettings`, edited at `/admin/config/search`. It defaults to 5 and
  takes 0.5 to 50.
  * After draining, a language whose tombstones are above it is compacted:
    one full build from an empty ledger, logged at `info` with the measured
    ratio.
* **Reader recovery** is in `scolta.js`, so a tab older than the 24 hour
  window now recovers.
  * A search that fails on a missing chunk or fragment (Pagefind throws when
    the 404 body does not decompress) calls `pagefind.destroy()` and
    `pagefind.init()` once and runs the search again.
  * Searches that fail at the same time share one reload.

### Where this differs from the design above

* **The queue is one row per item, not one row per write.** It also stores the
  item's language and a `seq`. Coalescing by item is what the drain wanted
  anyway, and the `seq` keeps a write that lands during a drain from being
  deleted with the batch.
* **`search_chunk` stores each chunk's real first and last word, not the
  meta's prefixes.** Routing needs the real words. The prefixes are recomputed
  from adjacent pairs with `get_prefixes` whenever a meta is encoded, so the
  meta file is what Pagefind writes.
* **Compaction renumbers in (language, item id) order.** The design said live
  items keep their relative order. Renumbering in a fixed order makes a
  compacted index byte for byte a first build of the same content, which gate
  1 checks.
* **A batch that empties a language takes a full build** (which removes that
  language from the entry), as well as one that adds a language.
* **Gate 1 compares decoded indexes before compaction, and bytes only after
  it.** The design asked for byte equality between the updater's output and a
  full build of the same state. That cannot hold while the updater keeps the
  chunk ranges frozen and a full build re-cuts them: the same postings land in
  differently bounded chunks, so the file bytes and names differ. The
  comparison that matters to a reader is the decoded one: every slot's
  fragment and word count, and every word's postings. Byte equality is checked
  where it can hold, after compaction against a first build from an empty
  ledger. The B218 brief asked for gate 1 in exactly this form.
* **The B218 brief asked for a full rebuild on a content type settings
  change.** This follows the design instead and queues every item of the
  type. The backlog rule above still turns a very large type into a full
  build.

### The proofs

* **Gate 1, incremental** (`search_index_gates_test.rs`,
  `gate_1_incremental_updates_equal_a_full_build_of_the_same_state`). It
  starts from a full build of the 60 page English and German fixture.
  * 60 batches apply 250 random changes through the queue and the updater:
    inserts, long pages, title and body edits, deletes, language moves, and
    comments added and removed.
  * After every 6th batch and the last, the published index decodes equal to a
    full build of the same state through a copy of the same ledger. No batch
    takes a fallback, and a chunk splits along the way.
  * Compaction at 0% then gives an index byte for byte equal to a first build
    of the final state from an empty ledger, with an equal term cache.
  * The crate's own `random_updates_decode_equal_to_a_full_build` runs 150
    batches the same way without a database.
* **Gate 3, incremental** (`gate_3_pagefind_js_finds_each_change_right_after_its_update`).
  The stock `pagefind.js` in Node, after each kind of change (a title edit, a
  comment added and removed, a delete, a slot reused, a language move), finds
  the change and returns the same results as a full rebuild of the same
  state.
* **Readers never see a meta pointing at a missing file**
  (`crates/search-index/tests/incremental.rs`).
  * One test stops a publish between every pair of writes and checks, at each
    point, that the entry's metas and every file they name exist.
  * Another runs the publish in a child process and `SIGKILL`s it at a random
    moment, 30 times. Each time the directory holds either the old index or
    the new one, complete.
* **Concurrency.** `two_runners_at_once_apply_every_queued_item_exactly_once`
  runs two drains at once. A live entry the database did not record takes a
  full build.
* **One test per writer** (`search_index_writers_test.rs`). Each write goes
  through the real service, the test asserts the queue row and its reason,
  then moves the row into a private index and checks the search after one
  update. The paths covered:
  * the item service (create, title edit, unpublish, delete);
  * the comment service (create, edit, delete);
  * field weights for a whole type;
  * URL alias create, update and delete;
  * workspace publishing;
  * scheduled publish and unpublish through the real cron and plugin. A
    scheduled unpublish takes the item out of the results;
  * a plugin's raw write, which is stale without `search_index_enqueue` and
    found with it.

  The type search switches screen is covered by `search_switches_test.rs`.

## Found and not fixed here

* **`pagefind_web` 1.3.0 panics on some non-ASCII queries.** When no chunk
  matches a term exactly, `try_request_indexes` (`pagefind_web/src/lib.rs`,
  lines 232-236) cuts the term and the chunk bounds at a byte length, which
  panics inside a multi-byte character. The query `zürich` against the German
  fixture index hits it, with the CLI's index as well as the native one, and
  the search throws `unreachable` in the browser. Fixing it means a patched
  `pagefind_web`, which is a different WASM from the pinned release.
* **Site-wide changes to what anonymous visitors may see queue nothing.** A
  change to the anonymous role's permissions, an access plugin enabled or
  disabled, or a new default language can change which items are searchable,
  or under which language, without writing any item. They never triggered a
  rebuild before either, since the `MAX(changed)` poll did not see them. After
  one, run `trovato search rebuild`.
* **A plugin that writes with raw SQL and does not call
  `search_index_enqueue` is stale until the next full build.** The rule is
  documented, and the SDK and WIT carry it, but nothing detects a write that
  breaks it.
* **Time to searchable includes the wait for the next cron run,** which is
  triggered from outside the server. The update itself is about 20 ms on the
  measured corpus.

Fixed in part 2, from the list part 1 left here:

* The rename swap that deleted the old index at once is gone: files are
  published into the live directory and kept 24 hours after they stop being
  referenced.
* The fixed-name runtime files are served `no-cache`.
* The "npm install -g pagefind" warning is gone with the CLI.
* A build that writes nothing can no longer swap in nothing: `publish` checks
  the staged entry and metas before it touches the live directory, and a test
  holds that.
