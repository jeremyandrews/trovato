# Story 36.4: Multi-Step Conference Submission Form

Status: done

## Story

As a **content editor**,
I want conference submission forms to support multi-step navigation,
so that complex forms can be broken into manageable steps with state preserved between them.

## Acceptance Criteria

1. `FormState` includes a `current_step` field for tracking multi-step progress
2. Form state is persisted to `form_state_cache` table between steps
3. `form_state_cache` table stores form_build_id, form_id, serialized state, created/updated timestamps
4. Form state values accumulate across steps without loss
5. Stale form state is cleaned up (entries older than 24 hours)
6. Step navigation works via AJAX callbacks that update `current_step`

## Tasks / Subtasks

- [x] Add current_step field to FormState struct (AC: #1)
- [x] Create form_state_cache table migration (AC: #3)
- [x] Implement save_state() with upsert to form_state_cache (AC: #2)
- [x] Implement load_state() to restore form state by form_build_id (AC: #2, #4)
- [x] Implement cleanup_stale_states() to remove entries older than 24h (AC: #5)
- [x] Wire step transitions through AJAX trigger handling (AC: #6)

## Dev Notes

### Architecture

Multi-step support is built on the same `FormState` and `form_state_cache` infrastructure used for AJAX:

- `FormState` carries `form_id`, `form_build_id`, `values` (accumulated across steps), `current_step` (integer), and `extra` (arbitrary metadata).
- `save_state()` uses `INSERT ... ON CONFLICT DO UPDATE` to upsert state keyed on `form_build_id`.
- `load_state()` deserializes the stored JSON back into `FormState`.
- `cleanup_stale_states()` deletes rows where `updated < now - 24h`.

Step transitions are just AJAX triggers that modify `current_step` and re-render the appropriate form section. The accumulated `values` map preserves all previously entered data.

### Testing

- FormState serialization round-trip tested with unit tests in service.rs
- State persistence tested via save/load cycle
- Cleanup threshold tested with stale timestamp comparison

### References

- `crates/kernel/src/form/service.rs` (535 lines) -- FormState, save_state, load_state, cleanup_stale_states
