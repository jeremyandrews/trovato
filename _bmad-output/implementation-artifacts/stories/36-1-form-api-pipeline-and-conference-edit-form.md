# Story 36.1: Form API Pipeline & Conference Edit Form

Status: done

## Story

As a **content editor**,
I want a form API that can build, validate, and submit content forms,
so that I can create and edit conferences through a structured, plugin-extensible form system.

## Acceptance Criteria

1. Form API defines `Form`, `FormElement`, and `ElementType` types for declarative form construction
2. `FormService::build()` generates a form by ID with CSRF token and invokes `tap_form_alter` for plugin modifications
3. `FormService::process()` verifies CSRF, runs built-in validation, dispatches `tap_form_validate` and `tap_form_submit`
4. `FormBuilder` auto-generates add/edit forms from `ContentTypeDefinition` field definitions
5. Edit form pre-populates field values from an existing `Item`
6. Format selectors respect user permissions via `with_permitted_formats()`
7. CSRF tokens use session-based generation and verification (`form/csrf.rs`)
8. Forms render through Tera templates with all standard HTML input types (text, textarea, select, checkbox, number, date, email, url, hidden)
9. `FormResult` returns either `Success` or `ValidationFailed` with per-field error messages

## Tasks / Subtasks

- [x] Define Form, FormElement, ElementType types in `form/types.rs` (AC: #1)
- [x] Implement FormService with build/process/ajax_callback methods in `form/service.rs` (AC: #2, #3)
- [x] Implement CSRF token generation and verification in `form/csrf.rs` (AC: #7)
- [x] Implement FormBuilder for auto-generated content type forms in `content/form.rs` (AC: #4, #5, #6)
- [x] Build add form with title, dynamic fields, and status checkbox (AC: #4, #8)
- [x] Build edit form with pre-populated values from existing Item (AC: #5)
- [x] Wire tap_form_alter, tap_form_validate, tap_form_submit dispatching (AC: #2, #3)
- [x] Return ValidationError list on validation failure (AC: #9)

## Dev Notes

### Architecture

The form system has two layers:
- **Form API** (`form/` module): Declarative form types, service orchestration, CSRF, and AJAX handling. `FormService` coordinates the build/validate/submit pipeline with tap integration.
- **Content Form Builder** (`content/form.rs`): Auto-generates HTML forms from `ContentTypeDefinition` field schemas. Handles all standard field types (Text, TextLong, Integer, Float, Boolean, Date, EntityReference, Blocks, etc.) with format-aware textarea widgets.

`FormElement` uses a builder pattern with fluent API. `Form` uses `BTreeMap` for deterministic element ordering. The `form_build_id` (UUID) tracks form instances for AJAX state correlation.

### Testing

- FormBuilder tested indirectly via admin content routes in integration tests
- CSRF flow tested in registration and profile update integration tests
- FormService validation tested through form submission handlers

### References

- `crates/kernel/src/form/types.rs` (588 lines) -- Form, FormElement, ElementType definitions
- `crates/kernel/src/form/service.rs` (535 lines) -- FormService build/process/ajax
- `crates/kernel/src/form/csrf.rs` -- CSRF token generation/verification
- `crates/kernel/src/form/mod.rs` -- Module exports
- `crates/kernel/src/content/form.rs` (609 lines) -- FormBuilder auto-generation
