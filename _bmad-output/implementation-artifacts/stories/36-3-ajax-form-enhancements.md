# Story 36.3: AJAX Form Enhancements

Status: done

## Story

As a **content editor**,
I want forms to support AJAX interactions like add-another and conditional fields,
so that I can manage multi-value fields and dynamic form behavior without full page reloads.

## Acceptance Criteria

1. `trovato.js` provides a client-side AJAX framework that executes server-returned commands
2. AJAX command types include: replace, append, prepend, remove, alert, redirect, add_class, remove_class, set_attribute, focus, scroll_to, invoke_callback
3. `AjaxRequest`/`AjaxResponse` types serialize command arrays between client and server
4. "Add another item" pattern increments field count and appends new field HTML via AJAX
5. "Remove item" pattern decrements field count and removes field HTML via AJAX
6. Form state is cached in `form_state_cache` DB table keyed by `form_build_id`
7. `tap_form_ajax` dispatches custom AJAX triggers to plugins
8. Event delegation handles `[data-ajax-trigger]` elements without per-element binding
9. Screen reader announcements via `aria-live` region for AJAX updates

## Tasks / Subtasks

- [x] Implement trovato.js AJAX framework with command execution (AC: #1, #2)
- [x] Define AjaxRequest, AjaxResponse, AjaxCommand types in `form/ajax.rs` (AC: #3)
- [x] Implement handle_add_item for multi-value "add another" pattern (AC: #4)
- [x] Implement handle_remove_item for multi-value removal (AC: #5)
- [x] Implement form state save/load/cleanup against form_state_cache table (AC: #6)
- [x] Wire tap_form_ajax dispatching for custom plugin triggers (AC: #7)
- [x] Add event delegation for data-ajax-trigger attributes (AC: #8)
- [x] Add Trovato.announce() for accessible AJAX notifications (AC: #9)

## Dev Notes

### Architecture

The AJAX system uses a command pattern: the server returns a `Vec<AjaxCommand>` which the client executes sequentially. This keeps all logic server-side while the client is a thin command executor.

- **Client** (`static/js/trovato.js`, 189 lines): `Trovato.ajax.submit()` POSTs to `/system/ajax` with `form_build_id`, `trigger`, and serialized form values. `executeCommands()` processes the response array. Event delegation on `[data-ajax-trigger]` handles clicks without per-element binding.
- **Server** (`form/ajax.rs`, 257 lines): `AjaxResponse` builder with fluent methods (`replace()`, `append()`, `remove()`, etc.). 11 command types cover DOM manipulation, navigation, and callback invocation.
- **State** (`form/service.rs`): `FormState` persists in `form_state_cache` table with `form_build_id` as key. Includes `current_step` for multi-step forms and `extra` map for arbitrary state. Cleanup removes entries older than 24 hours.

`Trovato.updateFieldDelta` is invoked after add/remove to update client-side field counts. `Trovato.resetAddFieldForm` clears the add-field form after successful AJAX submission.

### Testing

- AJAX command serialization tested via serde round-trip in ajax.rs
- Form state persistence tested through FormService integration
- Client-side behavior tested manually via browser

### References

- `static/js/trovato.js` (189 lines) -- Client-side AJAX framework
- `crates/kernel/src/form/ajax.rs` (257 lines) -- AjaxRequest/AjaxResponse/AjaxCommand types
- `crates/kernel/src/form/service.rs` -- ajax_callback, handle_add_item, handle_remove_item, state persistence
