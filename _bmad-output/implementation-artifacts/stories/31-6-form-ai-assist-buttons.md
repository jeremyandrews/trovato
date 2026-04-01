# Story 31.6: Form AI Assist Buttons

Status: ready-for-dev

## Story

As a **content editor**,
I want AI assist buttons next to text fields in content editing forms,
so that I can rewrite, expand, shorten, or translate content inline without leaving the editor.

## Acceptance Criteria

1. **AC1: tap_form_alter Integration** — `tap_form_alter` is listed in `KNOWN_TAPS` and wired into the form building pipeline in `form/service.rs`. When a content editing form is built, the kernel dispatches `tap_form_alter` with the form JSON, allowing plugins to inject additional elements.

2. **AC2: AI Assist Button Injection** — The `trovato_ai` plugin's `tap_form_alter` handler identifies text fields (text, textarea, formatted text) in the form and injects an "AI Assist" button element next to each. Buttons are only injected for users with the `use ai chat` permission.

3. **AC3: Operation Menu** — Clicking an AI Assist button opens a popover/dropdown with operation choices: Rewrite, Expand, Shorten, Translate, Adjust Tone. Each operation sends the current field value to the AI for transformation.

4. **AC4: AI Operation Endpoint** — A new JSON endpoint (e.g., `POST /api/v1/ai/assist`) accepts `{ "operation": "rewrite", "text": "...", "options": { "language": "es" } }` and returns `{ "result": "..." }`. Requires `use ai` + `use ai chat` permissions. Uses `AiProviderService` directly (kernel-side, not WASM host function).

5. **AC5: Client-Side JavaScript** — Vanilla JS handles: button click -> popover display, operation selection -> fetch to assist endpoint, response -> field value replacement with undo capability (store previous value). No external JS framework required.

6. **AC6: Permission Gating** — AI Assist buttons are only visible to users with `use ai chat` permission. The assist endpoint enforces the same permission check server-side.

7. **AC7: Rate Limiting** — The assist endpoint shares the chat rate limiter or has its own per-user rate limit to prevent abuse.

8. **AC8: Integration Tests** — Tests verify: (a) `tap_form_alter` fires during form building; (b) assist endpoint returns transformed text; (c) user without permission gets 403 on assist endpoint; (d) rate limit returns 429 after exceeding limit.

## Tasks / Subtasks

- [x] Task 1: Add `tap_form_alter` to KNOWN_TAPS (AC: #1)
  - [x] 1.1 `"tap_form_alter"` already in `KNOWN_TAPS` (line 101 of `info_parser.rs`)
- [x] Task 2: Wire `tap_form_alter` into form service (AC: #1)
  - [x] 2.1 `FormService::build_form()` already dispatches `tap_form_alter` (lines 39-58 of `form/service.rs`)
- [x] Task 3: Scaffold `tap_form_alter` in trovato_ai plugin (AC: #2)
  - [x] 3.1 `tap_form_alter` handler exists in `plugins/trovato_ai/src/lib.rs` (returns form unchanged)
- [ ] Task 4: Implement button injection in tap_form_alter (AC: #2)
  - [ ] 4.1 Parse form JSON to identify text-type fields
  - [ ] 4.2 Inject AI Assist button elements after each text field
  - [ ] 4.3 Check user permission before injection
- [ ] Task 5: Create AI assist endpoint (AC: #4, #6, #7)
  - [ ] 5.1 Create `POST /api/v1/ai/assist` route handler
  - [ ] 5.2 Implement operation-to-prompt mapping (rewrite, expand, shorten, translate, tone)
  - [ ] 5.3 Add permission and rate limit checks
- [ ] Task 6: Client-side JavaScript (AC: #3, #5)
  - [ ] 6.1 Implement button click -> popover with operation choices
  - [ ] 6.2 Implement fetch to assist endpoint + field value replacement
  - [ ] 6.3 Add undo capability (store previous value)
- [ ] Task 7: Integration tests (AC: #8)

## Dev Notes

### Current State

The infrastructure for form alteration exists:
- `tap_form_alter` is in `KNOWN_TAPS` (line 101 of `info_parser.rs`)
- `FormService::build_form()` dispatches `tap_form_alter` (lines 39-58 of `form/service.rs`)
- `trovato_ai` plugin has a scaffold `tap_form_alter` that passes through the form JSON unchanged (line 90-99 of `plugins/trovato_ai/src/lib.rs`)

What remains:
- Actual button injection logic in the trovato_ai plugin's `tap_form_alter`
- AI assist endpoint (`POST /api/v1/ai/assist`) with operation routing
- Client-side JavaScript for button interaction, popover, and field value replacement
- Integration tests for the assist endpoint

### Operation-to-Prompt Mapping (Planned)

| Operation | System Prompt |
|-----------|--------------|
| Rewrite | "Rewrite the following text to improve clarity and readability..." |
| Expand | "Expand the following text with more detail..." |
| Shorten | "Condense the following text to be more concise..." |
| Translate | "Translate the following text to {language}..." |
| Adjust Tone | "Rewrite the following text in a {tone} tone..." |

### Key Files

- `crates/kernel/src/plugin/info_parser.rs` — KNOWN_TAPS (tap_form_alter at line 101)
- `crates/kernel/src/form/service.rs` — tap_form_alter dispatch (lines 39-58)
- `plugins/trovato_ai/src/lib.rs` — tap_form_alter scaffold (lines 89-99)
- `crates/kernel/src/routes/api_chat.rs` — Reference for permission/rate limit patterns
