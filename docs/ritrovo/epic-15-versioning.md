# Epic 15 (F): Versioning & Audit Completeness

**Tutorial Parts Affected:** 4 (editorial engine — revisions)
**Trovato Phase Dependency:** Phase 3 (Content Model, Revisions) — already complete
**BMAD Epic:** 45
**Status:** Not started
**Estimated Effort:** 2–3 weeks
**Dependencies:** None (independent)
**Blocks:** Epic H (17) — `ai_generated` flag needed for AI audit trail

---

## Narrative

*Every revision tells two stories. The human story is the `log` field: "Fixed typos in the speaker bio." The machine story is what actually changed: field X went from value A to value B, 47 words were added to the description, the heading structure was reorganized. Today Trovato only tells the human story. This epic adds the machine story.*

Trovato's revision system is functional. Each save creates a new `item_revision` row with a complete JSONB snapshot of the item's fields. The `log` field captures human-entered change notes. Revisions are append-only by application convention (no UPDATE or DELETE on individual revisions — only CASCADE from item delete). Restoring to a previous version creates a new revision (not a destructive overwrite). This is solid.

What's missing:

1. **No auto-generated change summary.** When a revision is created, the kernel could diff the previous revision's `fields` JSONB against the new one and store a structured summary: which fields changed, old/new values for scalars, word count deltas for text, block additions/removals for block fields. This makes revision history useful *without* requiring editors to write detailed log messages (most won't).

2. **No `ai_generated` flag.** When AI creates or modifies content (via field rules in Epic 3's `tap_item_presave`), there's no structural way to know. An `ai_generated` boolean on revisions lets the editorial workflow distinguish AI work from human work — without encoding policy into the kernel. The kernel marks the revision; plugins decide what to do with the mark.

3. **No DB-level immutability.** Revisions are append-only by convention, but nothing prevents a buggy migration or direct SQL from updating an existing revision. A trigger or policy could enforce this.

**Before this epic:** Revisions store complete snapshots with human-entered log messages. No auto-generated diff, no AI attribution, no DB-level immutability guarantee.

**After this epic:** Each revision carries a structured `change_summary` JSONB documenting exactly what changed. AI-originated revisions are flagged. Revision immutability is enforced at the database level.

---

## Kernel Minimality Check

| Change | Why not a plugin? |
|---|---|
| `change_summary` column + auto-generation | Revision creation is kernel — happens inside the item save transaction |
| `ai_generated` flag | Schema on kernel-owned table — plugins can't add columns to `item_revision` |
| DB-level immutability trigger | Database constraint — must be kernel-managed |
| Verification of restore-creates-new-revision | Kernel behavior verification |

All changes are to the core content model. The *display* of change summaries (diff UI, blame view, timeline) is plugin territory.

---

## BMAD Stories

### Story 45.1: Auto-Generated Change Summary on Revisions

**As a** content editor reviewing revision history,
**I want** each revision to include a structured summary of what changed,
**So that** I can understand what was modified without comparing full JSONB snapshots.

**Acceptance criteria:**

- [ ] Migration adds `change_summary` column (JSONB, NULLABLE) to `item_revision` table
- [ ] On item save, kernel diffs previous revision's `fields` against new revision's `fields` and stores structured summary
- [ ] Summary format (JSONB):
  ```json
  {
    "fields_added": ["new_field"],
    "fields_removed": ["old_field"],
    "fields_changed": {
      "title": { "old": "Draft Title", "new": "Final Title" },
      "description": { "word_count_delta": 47 },
      "body": { "blocks_added": 2, "blocks_removed": 0, "blocks_changed": 1 }
    }
  }
  ```
- [ ] Scalar fields: store old and new values (truncated to 200 chars for text)
- [ ] Text fields: store word count delta (not full old/new — too large)
- [ ] Block fields: store block counts (added, removed, changed) — not full block content
- [ ] Category/reference fields: store added/removed reference IDs
- [ ] First revision of a new item: `change_summary` is NULL (no previous revision to diff against)
- [ ] Change summary serialized to plugins via `Item` SDK type (read-only)
- [ ] At least 3 integration tests: scalar change, text change with word count, block change

**Implementation notes:**
- Modify `crates/kernel/src/content/` item save path — after creating revision, diff against previous
- Diffing JSONB: iterate keys, compare values. Use `serde_json::Value` comparison.
- Word count: split on whitespace, count tokens. Not perfect for CJK but sufficient for a delta.
- Block diffing: compare block arrays by index. Added = new blocks beyond previous length. Removed = previous blocks beyond new length. Changed = blocks at same index with different content.
- Store the summary on the revision, not computed on read. Computing on read would be O(revisions) for history display.

---

### Story 45.2: AI-Generated Revision Flag

**As a** editorial workflow plugin developer,
**I want** revisions flagged when AI created or modified the content,
**So that** I can implement "AI content needs review" policies without building AI detection into my plugin.

**Acceptance criteria:**

- [ ] Migration adds `ai_generated` column (BOOLEAN, DEFAULT FALSE) to `item_revision` table
- [ ] When a revision is created via `ai_request()` host function (specifically, when the `tap_item_presave` or `tap_item_insert`/`tap_item_update` tap call chain includes an `ai_request()` call), `ai_generated` is set to TRUE
- [ ] Mechanism: the `ai_request()` host function sets a flag in the request context (`RequestState`). The item save path checks this flag when creating the revision.
- [ ] Manual saves (no AI involvement in the tap chain) get `ai_generated = FALSE`
- [ ] Saves that are partially AI-assisted (human edits + AI field enrichment in presave) get `ai_generated = TRUE` (conservative — any AI involvement flags it)
- [ ] `ai_generated` serialized to plugins via revision data
- [ ] Admin revision history shows a visual indicator for AI-generated revisions (e.g., small icon or label)
- [ ] At least 2 integration tests: manual save (false), save with AI field rule (true)

**Implementation notes:**
- Modify `crates/kernel/src/host/ai.rs` — set `request_state.set("ai_request_made", true)` after successful `ai_request()` call
- Modify `crates/kernel/src/content/` item save path — check request state flag when creating revision
- Migration: `ALTER TABLE item_revision ADD COLUMN ai_generated BOOLEAN DEFAULT FALSE`
- This flag is the kernel's only AI policy infrastructure. What to *do* with AI-generated revisions is plugin territory.

---

### Story 45.3: Revision Immutability Enforcement

**As a** platform maintaining audit trail integrity,
**I want** database-level enforcement preventing modification of existing revisions,
**So that** the revision history is tamper-proof (not just by convention).

**Acceptance criteria:**

- [ ] PostgreSQL trigger on `item_revision` that raises an exception on UPDATE: `RAISE EXCEPTION 'item_revision rows are immutable — updates are not permitted'`
- [ ] DELETE still allowed (needed for CASCADE from item delete and data retention)
- [ ] INSERT still allowed (needed for creating new revisions)
- [ ] Trigger is a migration — applies to all environments
- [ ] At least 1 integration test: attempt UPDATE on revision, verify it fails with expected error
- [ ] Documentation note: if a migration needs to fix bad data in revisions, it must temporarily disable the trigger (`ALTER TABLE item_revision DISABLE TRIGGER revision_immutability`) with explicit comment explaining why

**Implementation notes:**
- Migration creates trigger:
  ```sql
  CREATE OR REPLACE FUNCTION prevent_revision_update()
  RETURNS TRIGGER AS $$
  BEGIN
    RAISE EXCEPTION 'item_revision rows are immutable';
  END;
  $$ LANGUAGE plpgsql;

  CREATE TRIGGER revision_immutability
  BEFORE UPDATE ON item_revision
  FOR EACH ROW
  EXECUTE FUNCTION prevent_revision_update();
  ```
- Simple and effective. No application code change needed — the trigger enforces the invariant.

---

### Story 45.4: Revision Integrity Verification

**As a** kernel maintainer,
**I want** verified guarantees about revision behavior,
**So that** the tutorial and documentation can confidently state these properties.

**Acceptance criteria:**

- [ ] Verified: "Restore to version X" creates a new revision with the restored content (not a destructive overwrite of the current revision)
- [ ] Verified: No code path allows deleting an individual revision (only CASCADE from item delete, or bulk cleanup by retention plugins)
- [ ] Verified: Revision `created` timestamp is set by the kernel at INSERT time (not editable by plugins or API callers)
- [ ] Verified: Revision `author_id` is set to the authenticated user at INSERT time (not spoofable by plugins)
- [ ] If any of these invariants are violated in the current code, fix them
- [ ] Document these guarantees in `docs/design/Design-Content-Model.md` under a "Revision Guarantees" section
- [ ] At least 2 integration tests: restore creates new revision, individual revision deletion not possible

**Implementation notes:**
- Audit `crates/kernel/src/content/` — find all paths that write to `item_revision`
- Audit `crates/kernel/src/routes/` — find any endpoint that could delete a single revision
- This is primarily a verification story. If bugs are found, fix them; if the behavior is correct, write the tests and documentation.

---

## Plugin SDK Changes

None directly. The `change_summary` and `ai_generated` fields are added to the revision data that plugins receive, but these are additive fields — existing plugins ignore them via `#[serde(default)]`.

---

## Design Doc Updates (shipped with this epic)

| Doc | Changes |
|---|---|
| `docs/design/Design-Content-Model.md` | Add "Revision Guarantees" section documenting immutability, restore-creates-new, no individual deletion. Add `change_summary` and `ai_generated` to revision schema documentation. Document the change summary JSONB format. |

---

## Tutorial Impact

| Tutorial Part | Sections Affected | Nature of Change |
|---|---|---|
| `part-04-editorial-engine.md` | Revisions section | Add mention of auto-generated change summary. Show example of revision history with change summaries. Note `ai_generated` flag exists (will be demonstrated in AI epic). |

---

## Recipe Impact

Recipe for Part 4 needs updates matching tutorial changes. Run `docs/tutorial/recipes/sync-check.sh` and update hashes.

---

## Screenshot Impact

| Part | Screenshots | Reason |
|---|---|---|
| Part 4 | Revision history screenshot | Change summary and AI indicator now visible in revision list |

---

## Config Fixture Impact

None. Revision schema changes don't affect config fixtures.

---

## Migration Notes

**Database migrations:**
1. `YYYYMMDD000001_add_revision_change_summary.sql` — ADD `change_summary` JSONB to `item_revision`
2. `YYYYMMDD000002_add_revision_ai_generated.sql` — ADD `ai_generated` BOOLEAN DEFAULT FALSE to `item_revision`. CREATE immutability trigger.

**Breaking changes:** None. All columns are nullable or have defaults. The immutability trigger prevents UPDATE on revisions — if any code path currently UPDATEs revisions, it will break (and should — that's a bug).

**Upgrade path:** Run migrations. Existing revisions get `change_summary = NULL` and `ai_generated = FALSE`. The immutability trigger applies immediately. If a custom migration needs to fix revision data, it must explicitly disable the trigger.

---

## What's Deferred

- **Diff/compare UI** (side-by-side revision comparison) — Plugin. The kernel stores `change_summary`; a plugin renders the diff viewer.
- **Blame/annotation view** (which editor changed which paragraph) — Plugin. Requires paragraph-level tracking beyond change_summary.
- **Collaboration/conflict resolution** (multiple editors, merge conflicts) — Future epic. Current model is "last save wins."
- **Revision pruning** (keep only last N revisions) — Plugin, using `retention_days` from Epic D or revision count limits.
- **Change summary for non-JSONB fields** (title, status) — Could be added to change_summary. Deferred because these fields are visible in the revision list already.
- **Cryptographic revision chain** (hash-linked revisions for tamper evidence) — Future. Immutability trigger provides simpler guarantee.

---

## Related

- [Design-Content-Model.md](../design/Design-Content-Model.md) — Item and revision schema
- [Epic D (13): Privacy Infrastructure](epic-13-privacy.md) — `retention_days` on revisions (coordinated schema changes)
- [Epic H (17): External Interface Infrastructure](epic-17-external.md) — Depends on `ai_generated` flag for AI audit trail
- [Epic 3: AI as a Building Block](epic-03.md) — `tap_item_presave` and field rules that produce AI-generated content
