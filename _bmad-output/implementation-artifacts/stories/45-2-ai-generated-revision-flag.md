# Story 45.2: AI-Generated Revision Flag

Status: ready-for-dev

## Story

As an **editorial workflow plugin developer**,
I want revisions flagged when AI created or modified the content,
so that editorial review workflows can distinguish human-authored from AI-assisted content.

## Acceptance Criteria

1. Migration adds `ai_generated BOOLEAN DEFAULT FALSE` column to `item_revision`
2. When a revision is created via the `ai_request()` call chain, `ai_generated` is set to TRUE
3. Mechanism: `ai_request()` sets a flag in `RequestState`; the item save path checks this flag
4. Manual saves (admin UI, direct API) set `ai_generated` to FALSE
5. Partially AI-assisted saves (e.g., human edits after AI generation within the same request) set `ai_generated` to TRUE (conservative approach)
6. `ai_generated` is serialized to plugins as part of revision data
7. Admin revision history UI shows a visual indicator for AI-generated revisions
8. At least 2 integration tests covering AI-generated and manual revision scenarios

## Tasks / Subtasks

- [ ] Write migration adding `ai_generated BOOLEAN DEFAULT FALSE` to `item_revision` (AC: #1)
- [ ] Add `ai_generated` flag to `RequestState` (or equivalent per-request context), default FALSE (AC: #3)
- [ ] Update `ai_request()` in `crates/kernel/src/host/ai.rs` to set the flag in request state (AC: #2, #3)
- [ ] Update item save path to read the flag from request state and persist to `item_revision.ai_generated` (AC: #2)
- [ ] Ensure manual saves (no AI involvement) default to FALSE (AC: #4)
- [ ] Document conservative policy: any AI involvement in the request marks the revision (AC: #5)
- [ ] Serialize `ai_generated` in plugin-facing revision data (AC: #6)
- [ ] Add visual indicator (icon or badge) in admin revision history template for AI-generated revisions (AC: #7)
- [ ] Write integration test: save via AI request path, verify `ai_generated = TRUE` (AC: #8)
- [ ] Write integration test: save via manual path, verify `ai_generated = FALSE` (AC: #8)

## Dev Notes

### Architecture

The `RequestState` (or a task-local equivalent) carries an `ai_assisted: bool` flag. When `ai_request()` is called by a plugin (via the WASM host function), it sets this flag to TRUE before making the AI API call. The content save path reads this flag at revision creation time. This is intentionally conservative: if any AI call was made during the request that produces a revision, that revision is marked as AI-generated. This avoids complex provenance tracking while still providing useful editorial signal.

The flag is per-request, not per-field. A future enhancement could track which specific fields were AI-modified, but that is out of scope for this story.

### Security

The `ai_generated` flag is set by kernel infrastructure, not by the plugin or user. Plugins cannot directly set this flag — it is derived from whether `ai_request()` was invoked during the request lifecycle. This prevents plugins from falsely marking content as human-authored.

### Testing

- **AI path test**: Simulate a plugin calling `ai_request()`, then saving an item in the same request context. Verify the resulting revision has `ai_generated = TRUE`.
- **Manual path test**: Save an item through the standard admin save endpoint with no AI involvement. Verify `ai_generated = FALSE`.

### References

- `crates/kernel/src/host/ai.rs` — AI request host function
- `crates/kernel/src/content/` — item save path and revision creation
- Admin revision history template
