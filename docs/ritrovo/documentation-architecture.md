# Documentation Architecture: Dual-Track Tutorial with Tested Assertions

**Status:** Draft proposal
**Related:** [Ritrovo Overview](overview.md)

---

## The Two Tracks

Ritrovo's tutorial tells two stories from the same codebase. Both are accurate. Both are tested. A reader picks their depth.

### Track 1: The User Story (existing tutorial plan)

This is the tutorial as already designed in Epics 1-8. It teaches a developer how to build a real site with Trovato: define content types, write plugins, configure layouts, build forms. It answers "what do I do?" and "what happens when I do it?"

The reader follows along, types commands, and sees results. They never need to understand internal implementation to succeed.

### Track 2: Under the Hood (new)

Each tutorial step gets an optional companion section that explains the internals. It answers "how does this actually work?" and "why was it built this way?"

This track is for readers who want to understand Trovato's architecture, contribute to core, or make informed decisions about extending it. It covers generated SQL, WASM host function boundaries, cache tag propagation, render tree construction, and similar implementation details.

**Under the Hood is always optional.** Skipping it loses nothing from the user story. Reading it adds depth without disrupting the narrative flow.

---

## Presentation

### In Source (mdbook markdown)

Tutorial chapters live in `docs/tutorial/` in the Trovato repo. Each chapter is a markdown file corresponding to an epic (Part 1 through Part 8).

Within each chapter, tutorial steps are the primary content. Under the Hood sections appear at the end of each step, inside HTML `<details>` blocks:

```markdown
### Step 2: Define the Conference Item Type

Create the `conference` Item Type definition...

[... tutorial content, code blocks, expected output ...]

<details>
<summary><strong>Under the Hood:</strong> JSONB Storage Layout</summary>

When you define fields on an Item Type, Trovato stores them as JSONB columns
in PostgreSQL. Here's what the raw row looks like for the conference you just
created:

...sql
SELECT id, jsonb_pretty(fields) FROM items WHERE item_type = 'conference';
...

The JSONB approach means...

[... explanation of design decisions, tradeoffs, alternatives considered ...]

</details>
```

### In Rendered Output (mdbook)

The `<details>` element renders as a collapsible section with a disclosure triangle. Closed by default. The reader sees "Under the Hood: JSONB Storage Layout" as a clickable header. The tutorial flow is uninterrupted.

No special mdbook plugins required. This is standard HTML that mdbook passes through.

---

## Under the Hood: What Each Part Covers

This maps technical topics to the tutorial parts where they naturally belong. Not every step needs an Under the Hood section -- only where the internals are interesting or non-obvious.

### Part 1: Hello, Trovato

| Step | Under the Hood Topic |
|---|---|
| Install & Scaffold | Project structure: what each generated file does, how the kernel boots |
| Define Item Type | JSONB storage layout, field type system internals, migration SQL |
| Create Content | How Items get IDs (UUIDv7 with embedded timestamps), timestamp handling, the `items` table schema |
| First Gather | Generated SQL from a Gather definition, query planner output, how the Gather engine resolves field references |

### Part 2: Real Data, Real Site

| Step | Under the Hood Topic |
|---|---|
| First Plugin | WASM compilation pipeline, host function ABI, the sandbox boundary (what plugins can/cannot access and why) |
| Import Logic | How `http_request()` crosses the WASM boundary, queue storage schema, dedup algorithm |
| Topic Taxonomy | Recursive CTE queries for hierarchical terms, the `ltree` or adjacency-list implementation |
| Advanced Gathers | How exposed filters compose into SQL WHERE clauses, contextual filter resolution, `InCategory` descendant query |
| Full-Text Search | `tsvector` column management, GIN index structure, `ts_rank` weighting math, search index update triggers |

### Part 3: Look & Feel

| Step | Under the Hood Topic |
|---|---|
| Render Tree | RenderElement JSON structure, how core and plugins contribute elements, the tree merge algorithm |
| Templates | Tera template resolution chain, how the render tree maps to template variables, template caching |
| File Uploads | Temp file lifecycle, the `files` table schema, reference counting for shared files |
| Speakers | RecordReference implementation (foreign keys vs JSONB references), reverse reference resolution, Left join generation |
| Slots & Tiles | Slot/Tile configuration storage, Tile visibility evaluation order, render pipeline for composing Slots into a page |

### Part 4: The Editorial Engine

| Step | Under the Hood Topic |
|---|---|
| Users & Auth | Session storage, password hashing (argon2), "users are Items" implementation (how user records share the Items table) |
| Access Control | Grant/Deny/Neutral aggregation algorithm, how multiple plugins contribute access decisions, the access check hot path |
| Stages | CTE-based stage filtering (the SQL that makes stage-aware queries work), stage transition validation |
| Revisions | Revision storage model, how revert creates a new revision pointing to old data, the draft-while-live data structure, cross-stage field update merging |

### Part 5: Forms & User Input

| Step | Under the Hood Topic |
|---|---|
| Form API | Form definition to RenderElement conversion, the form processing pipeline (build > validate > submit), form cache table schema |
| Multi-Step Forms | Form state serialization, step transition mechanics, how file uploads are tracked across steps |
| WYSIWYG | HTML sanitization pipeline, allowed-tag configuration, how `filtered_html` differs from `plain` in storage |
| AJAX | AJAX callback routing, partial form rebuild mechanics, how form state is maintained during AJAX round-trips |
| CFP Plugin | `tap_item_view` injection point in the render pipeline, cross-plugin communication via shared queues |

### Part 6: Community

| Step | Under the Hood Topic |
|---|---|
| Comments | Self-referencing RecordReference for threading, comment tree query (recursive CTE), comment count denormalization |
| Subscriptions | Subscription storage design choices (join table vs JSONB), subscription lookup performance |
| Notification Plugin | Cross-plugin communication via shared queues, queue processing internals, digest aggregation algorithm |
| Integration | Event propagation across three plugins, how the kernel dispatches tap calls, ordering guarantees |

### Part 7: Going Global

| Step | Under the Hood Topic |
|---|---|
| i18n Architecture | JSONB parallel field set storage for translations, how translatable fields are declared vs stored |
| Translation Plugin | Language detection internals, translation status state machine |
| Routing | URL alias resolution, language prefix routing implementation, redirect chain, hreflang tag generation |
| REST API | How Gather definitions become API endpoints (the thin JSON serializer layer), Tower middleware stack, rate limiter implementation (token bucket vs sliding window) |

### Part 8: Production Ready

| Step | Under the Hood Topic |
|---|---|
| Caching | Cache tag data structure, L1 (moka) eviction policy, L2 (Redis) serialization format, invalidation propagation, the tag > key mapping |
| Gander Profiling | How Gander instruments the request pipeline, timing collection points, the profiling data structure |
| Batch Operations | Batch job scheduling, Redis-backed progress tracking, how batch invalidation differs from per-item invalidation |
| S3 Storage | Storage backend trait, signed URL generation for private files, image derivative pipeline |

---

## Enforcement: Tested Documentation

The core rule: **if the tutorial says it, a test proves it. If the test breaks, the docs must be updated before the PR merges.**

### How It Works

#### 1. Standard integration tests validate tutorial claims

Tutorial tests are standard Rust integration tests in `crates/kernel/tests/tutorial_test.rs`. They use the same shared `TestApp` infrastructure as the rest of the test suite -- `shared_app()`, `run_test()`, `#[test]` macros, and direct database access.

Each test is named after the tutorial part and step it validates:

```
test_part01_step02_conference_type_in_api
test_part01_step02_conference_has_17_fields
test_part01_step03_conferences_exist
test_part01_step04_gather_returns_conferences
```

Tests make HTTP requests to API endpoints, query the database directly, and verify that the behavior matches what the tutorial prose describes. For example, if the tutorial says "the Gather displays the configured empty text: 'No conferences found.'", a test verifies the gather query definition has that exact `empty_text` value.

#### 2. No custom extraction tooling

Unlike `skeptic` or `rustdoc` test extraction, tutorial tests are plain Rust code in a `.rs` file. This means:

- **Full IDE support** -- autocomplete, type checking, go-to-definition, refactoring
- **Clear error messages** -- compilation and assertion failures point at real source lines
- **Standard debugging** -- set breakpoints, step through code, inspect variables
- **No build script** -- no custom markdown parser, no code generation, no fragile extraction pipeline

#### 3. CI enforcement

Two checks run on every Trovato PR:

**Check 1: Tutorial tests pass**

```bash
cargo test --test tutorial_test
```

If a core change breaks behavior that the tutorial documents, this fails. The fix requires updating both the code and the tutorial test.

**Check 2: Coverage check**

A naming convention makes coverage verification trivial. A script parses tutorial markdown for `## Step` headers and verifies each step has at least one corresponding `test_partNN_stepNN_*` function:

```bash
# scripts/check-tutorial-coverage.sh
# Parses markdown for ## Step N headers
# Checks tutorial_test.rs for matching test_partNN_stepNN_ functions
# Exits non-zero if any step lacks coverage
```

This is a simple grep script, not a framework.

#### 4. The developer workflow

When a Trovato core change breaks a tutorial test:

1. `cargo test --test tutorial_test` fails in CI
2. The failure message identifies the test function and assertion
3. Each test function has a comment referencing the tutorial file and step it validates
4. The developer updates both the tutorial prose and the test to match the new behavior

When adding a new tutorial step:

1. Write the tutorial markdown
2. Add test functions in `tutorial_test.rs` named `test_partNN_stepNN_*`
3. Each test should have a comment linking back to the tutorial section it validates
4. `cargo test --test tutorial_test` verifies everything
5. Coverage check confirms no steps were left untested

---

## What This Does NOT Do

This system is deliberately simple. It does not:

- **Generate documentation from tests.** The tutorial is hand-written prose. Tests validate it; they don't produce it.
- **Require custom tooling.** It's standard `cargo test` with a naming convention. No build scripts, no markdown parsers, no code generators.
- **Test prose accuracy.** If the tutorial says "this is fast" but doesn't quantify it, no test catches that. Tests validate behavior, not adjectives.
- **Version documentation separately from code.** Tutorial markdown lives in the same repo as Trovato core. They ship together, branch together, and break together. That's the point.

---

## Implementation Plan

This is built incrementally alongside the tutorial, not as a separate project.

### Phase 1: Part 1 tests (Epic 1)

- Create `crates/kernel/tests/tutorial_test.rs` using existing `shared_app()` infrastructure
- Write tests for all four Part 1 steps (install, define type, create content, first gather)
- Verify tests pass with `cargo test --test tutorial_test`

### Phase 2: Iterate (Epics 2-8)

- Each epic adds test functions for its tutorial steps
- Tests use HTTP requests to API endpoints plus direct database queries
- All tests share the same `TestApp` instance for efficiency

### Phase 3: Coverage script (Epic 2+)

- Write `scripts/check-tutorial-coverage.sh` to verify naming convention coverage
- Add to CI pipeline

### Phase 4: Polish (Epic 15)

- Coverage audit: every step has tests
- Tutorial prose review and editing
- Appendix materials

---

## Related

- [Ritrovo Overview](overview.md)
