# Story 31.9: AI Admin UI & Usage Dashboard

Status: done

## Story

As a **site administrator**,
I want a unified admin interface for managing AI providers, budgets, chat settings, and viewing usage,
so that I can configure and monitor all AI features from a single set of admin pages.

## Acceptance Criteria

1. **AC1: AI Providers Admin** — `/admin/system/ai-providers` lists all configured providers with label, protocol, enabled status, and operation count. Links to add/edit forms. Protected by `configure ai` permission.

2. **AC2: AI Provider Form** — `/admin/system/ai-providers/add` and `/admin/system/ai-providers/{id}/edit` provide a form for provider configuration: label, protocol selector, base URL, API key env var, rate limit RPM, enabled toggle, and repeatable operation-to-model pairs. Includes "Test Connection" button.

3. **AC3: AI Budgets Dashboard** — `/admin/system/ai-budgets` shows token usage across all providers for the current period. Displays per-provider totals, per-role breakdown, and top users. Protected by `view ai usage` permission.

4. **AC4: Per-User Budget Detail** — `/admin/system/ai-budgets/{user_id}` shows a single user's token usage with per-provider breakdown and allows setting per-user budget overrides. Protected by `configure ai` permission.

5. **AC5: AI Chat Configuration** — `/admin/system/ai-chat` allows configuring chatbot settings: system prompt, RAG toggle, RAG max results, RAG min score, max history turns, rate limit per hour, max tokens, temperature. Protected by `configure ai` permission.

6. **AC6: AI Features Configuration** — `/admin/config/ai/features` allows enabling/disabling individual AI operation types and setting per-operation provider/model overrides. Protected by `configure ai` permission.

7. **AC7: Admin Sidebar Links** — All AI admin pages linked in the System section of `page--admin.html`: AI Providers, AI Budgets, AI Chat. Each link respects its required permission.

8. **AC8: Template Consistency** — All AI admin templates extend `page--admin.html`, use form macros from `templates/admin/macros/`, include CSRF tokens, and follow established admin UI patterns.

## Dev Notes

### Key Implementation Details

Six admin templates serve the AI admin UI:
- `templates/admin/ai-providers.html` — Provider list with add/edit/delete actions
- `templates/admin/ai-provider-form.html` — Provider add/edit form with operation-model pairs
- `templates/admin/ai-budgets.html` — Usage dashboard with period selector
- `templates/admin/ai-budget-user.html` — Per-user usage detail with override controls
- `templates/admin/ai-chat.html` — Chat configuration form
- `templates/admin/ai-features.html` — Per-operation enable/disable with provider/model overrides

Three route files serve admin requests:
- `crates/kernel/src/routes/admin_ai_provider.rs` (699 lines) — Provider CRUD, defaults, connection test
- `crates/kernel/src/routes/admin_ai_budget.rs` (451 lines) — Budget dashboard, per-user detail, override save
- `crates/kernel/src/routes/admin_ai_chat.rs` (135 lines) — Chat config GET/POST

### Permission Mapping

| Route | Permission |
|-------|-----------|
| `/admin/system/ai-providers` (all) | `configure ai` |
| `/admin/system/ai-budgets` (dashboard) | `view ai usage` |
| `/admin/system/ai-budgets/{user_id}` (detail + save) | `configure ai` |
| `/admin/system/ai-chat` | `configure ai` |
| `/admin/config/ai/features` | `configure ai` |

### Files

**Templates:**
- `templates/admin/ai-providers.html`
- `templates/admin/ai-provider-form.html`
- `templates/admin/ai-budgets.html`
- `templates/admin/ai-budget-user.html`
- `templates/admin/ai-chat.html`
- `templates/admin/ai-features.html`

**Route Handlers:**
- `crates/kernel/src/routes/admin_ai_provider.rs` (699 lines)
- `crates/kernel/src/routes/admin_ai_budget.rs` (451 lines)
- `crates/kernel/src/routes/admin_ai_chat.rs` (135 lines)

**Modified:**
- `templates/page--admin.html` — Sidebar links for AI Providers, AI Budgets, AI Chat

## Dev Agent Record

### Agent Model Used

Claude Opus 4.6

### Completion Notes List

- Admin UI built incrementally across Stories 31.1, 31.3, and 31.7 as each feature was implemented
- All templates use `{% extends "page--admin.html" %}` and `{% import "admin/macros/form.html" as form %}`
- CSRF tokens included in all POST forms via `{{ form::csrf(csrf_token=csrf_token) }}` macro
- Tera `{% import %}` must be at top level (not inside `{% block %}`) per Tera limitation
- Macro calls use keyword syntax: `form::csrf(csrf_token=csrf_token)`, not positional

### File List

- `templates/admin/ai-providers.html`
- `templates/admin/ai-provider-form.html`
- `templates/admin/ai-budgets.html`
- `templates/admin/ai-budget-user.html`
- `templates/admin/ai-chat.html`
- `templates/admin/ai-features.html`
- `crates/kernel/src/routes/admin_ai_provider.rs` (699 lines)
- `crates/kernel/src/routes/admin_ai_budget.rs` (451 lines)
- `crates/kernel/src/routes/admin_ai_chat.rs` (135 lines)
