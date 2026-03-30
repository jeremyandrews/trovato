# Epic 17 (H): External Interface Infrastructure (API + AI)

**Tutorial Parts Affected:** None directly (infrastructure for API consumers and AI plugins)
**Trovato Phase Dependency:** Phase 6 (API, AI Core) — already complete
**BMAD Epic:** 47
**Status:** Not started
**Estimated Effort:** 3–4 weeks
**Dependencies:** Epic F (15) — needs `ai_generated` flag on `item_revision`
**Blocks:** None

---

## Narrative

*This epic combines two domains that are both about external interfaces: the REST API (how external systems consume Trovato) and AI integration (how Trovato consumes external AI systems). After correctly pushing plugin work out, each domain's kernel changes are small enough that combining them prevents two half-sized epics.*

**API-First:** Trovato already has REST API routes (`/api/v1/`), OAuth2 with JWT/PKCE, and Gather queries that produce structured data. What's missing is the infrastructure that makes the API *evolvable*: route metadata annotations that tools can read (for OpenAPI generation by a plugin), API versioning infrastructure (so v2 can exist alongside v1 without breaking clients), and deprecation headers (per RFC 8594) that warn clients when endpoints are sunsetting.

The API numbering convention already exists — routes are in `api_v1.rs`. But there's no kernel mechanism to route between versions, inject deprecation warnings, or expose route metadata for documentation generation. Adding this infrastructure now (a middleware and a route attribute) is cheap. Adding it after plugins depend on unversioned routes is a breaking-change nightmare.

**Ethical AI:** The `ai_request()` host function works — plugins call it, the kernel handles keys, rate limits, and token budgets. What's missing is *observability and governability*: no audit trail of AI request metadata (which model, how many tokens, how long it took), no `tap_ai_request` for plugins to intercept or modify AI calls, and no per-feature configuration toggles (can't disable image generation while keeping chat enabled).

The `tap_ai_request` hook is the key infrastructure piece. It's the AI equivalent of `tap_item_access` — it lets plugins implement policies (prompt sanitization, content logging, "needs review" flagging, bias detection) without the kernel encoding any specific policy. The kernel provides the interception point; plugins implement the policy.

**Before this epic:** Working REST API and AI host function with no evolvability infrastructure and no AI observability.

**After this epic:** Route metadata for documentation generation. API versioning with deprecation headers. AI request metadata logging. Plugin interception of AI calls via tap. Per-AI-feature configuration.

---

## Kernel Minimality Check

| Change | Why not a plugin? |
|---|---|
| Route metadata annotations | Routing is kernel — plugins can't annotate kernel routes |
| API versioning middleware | Version routing is kernel infrastructure |
| Deprecation headers | Response headers are kernel middleware |
| AI metadata audit trail | `ai_request()` is a kernel host function — logging happens at the host boundary |
| `tap_ai_request` hook | Tap infrastructure is kernel |
| Per-AI-feature config | AI host function dispatch is kernel |

Plugin territory (NOT this epic): Webhook implementation (table, delivery, retry, HMAC signing), OAuth2 admin UI, OpenAPI spec generation, GraphQL, AI content logging via tap, "needs review" workflow, prompt validation, bias detection.

---

## BMAD Stories

### Story 47.1: Route Metadata Annotations

**As a** plugin developer building API documentation tools,
**I want** kernel routes annotated with structured metadata,
**So that** I can generate OpenAPI specs, API explorers, or client SDKs from the route definitions.

**Acceptance criteria:**

- [ ] Route metadata struct: `RouteMetadata { method: Method, path: String, summary: String, parameters: Vec<ParamMeta>, response_type: String, tags: Vec<String>, deprecated: bool }`
- [ ] All kernel API routes (`/api/v1/*`) annotated with `RouteMetadata`
- [ ] Metadata accessible via `AppState` — a `RouteRegistry` service that returns all registered routes with their metadata
- [ ] `GET /api/v1/routes` endpoint that returns all API route metadata as JSON (self-describing API)
- [ ] Admin API routes (`/admin/api/*`) also annotated but marked with `tags: ["admin"]`
- [ ] Plugin routes can register metadata via a host function: `register_route_metadata(metadata)`
- [ ] At least 2 integration tests: list routes, verify metadata for a known route

**Implementation notes:**
- Add `RouteMetadata` struct to kernel (not SDK — it's kernel-side)
- Route metadata is *descriptive*, not *prescriptive* — it doesn't affect routing behavior, only describes routes for documentation
- The `/api/v1/routes` endpoint is the simplest possible API explorer — an OpenAPI plugin reads this and produces a proper spec
- Annotating routes is manual work but well-scoped — there are ~20 API routes in `api_v1.rs`

---

### Story 47.2: API Versioning Infrastructure

**As a** platform maintaining backward compatibility,
**I want** API versioning infrastructure,
**So that** I can introduce `/api/v2/` routes alongside `/api/v1/` without breaking existing clients.

**Acceptance criteria:**

- [ ] Version routing middleware: requests to `/api/v{N}/...` routed to the appropriate version's router
- [ ] Version routers are separate Axum routers composed into the main app (already the pattern with `api_v1.rs` — formalize it)
- [ ] `Sunset` header (RFC 8594) injected on deprecated API versions: `Sunset: Sat, 01 Jan 2028 00:00:00 GMT`
- [ ] `Deprecation` header injected on deprecated endpoints: `Deprecation: true`
- [ ] `Link` header pointing to the successor endpoint: `Link: </api/v2/items>; rel="successor-version"`
- [ ] Deprecation config: per-route or per-version sunset dates stored in a `DEPRECATED_API_ROUTES` config (YAML or code constant)
- [ ] No v2 routes created yet — this is infrastructure for when they're needed
- [ ] API responses include `X-API-Version: 1` header (so clients know which version served the response)
- [ ] At least 2 integration tests: version header present, deprecation headers on a test deprecated route

**Implementation notes:**
- Add version routing in `crates/kernel/src/routes/mod.rs` — the v1 router is already separate; formalize the pattern
- Deprecation middleware checks route against config and injects headers
- This is forward-looking infrastructure — no routes are actually deprecated yet. The mechanism is in place for when v2 routes are introduced.

---

### Story 47.3: Verify Gather-as-JSON-API

**As a** API consumer,
**I want** Gather queries available as JSON endpoints,
**So that** I can use the same query definitions for both HTML pages and API responses.

**Acceptance criteria:**

- [ ] Gather queries with `display.routes` configured can be accessed with `Accept: application/json` header to get JSON instead of HTML
- [ ] JSON response format: `{ "items": [...], "pager": { "current_page": 1, "total_pages": 10, "total_items": 250 }, "query": { "name": "upcoming_conferences" } }`
- [ ] Content negotiation: `Accept: text/html` → rendered page, `Accept: application/json` → JSON response
- [ ] JSON response includes the same items that the HTML page would show (same filters, same sort, same pagination)
- [ ] If this already works, document it and add tests. If not, implement it.
- [ ] At least 2 integration tests: JSON response for a Gather query, verify item count matches HTML version

**Implementation notes:**
- Check `crates/kernel/src/routes/gather_routes.rs` — does it already support content negotiation?
- If not, add `Accept` header check and branch to JSON serialization
- The Gather query engine already produces structured data — rendering to JSON instead of HTML is a serialization format change, not a logic change

---

### Story 47.4: AI Request Metadata Audit Trail

**As a** site operator monitoring AI usage,
**I want** metadata about every AI request logged,
**So that** I can track costs, detect anomalies, and satisfy audit requirements.

**Acceptance criteria:**

- [ ] Every `ai_request()` host function call logs metadata to the `ai_usage_log` table (already exists):
  - `model` (which AI model was called)
  - `operation_type` (Chat, Embedding, etc.)
  - `input_tokens` and `output_tokens` (already logged ✅ — verify)
  - `latency_ms` (wall-clock time from request to response)
  - `user_id` (who initiated the request)
  - `plugin_name` (which plugin made the call)
  - `finish_reason` (complete, truncated, error)
  - `status` (success, error, timeout)
- [ ] Migration adds missing columns to `ai_usage_log` if needed: `latency_ms`, `plugin_name`, `finish_reason`, `status`
- [ ] Metadata logging is *always on* — not configurable (this is audit infrastructure, not optional)
- [ ] Actual prompt/response content is NOT logged (privacy-sensitive, potentially massive — content logging is plugin territory via `tap_ai_request`)
- [ ] `GET /admin/reports/ai-usage` endpoint shows aggregate stats (total tokens, cost estimate, requests per plugin)
- [ ] At least 2 integration tests: verify metadata logged on successful AI call, verify metadata logged on failed AI call

**Implementation notes:**
- Modify `crates/kernel/src/host/ai.rs` — add metadata fields to logging
- Check `ai_usage_log` table schema (migration `20260226000003_create_ai_usage_log.sql`) — add missing columns if needed
- The admin report endpoint is a simple aggregate query, not a feature-rich dashboard

---

### Story 47.5: AI Request Interception Tap

**As a** plugin developer implementing AI governance policies,
**I want** a `tap_ai_request` hook,
**So that** I can intercept, modify, or block AI calls for policy enforcement.

**Acceptance criteria:**

- [ ] New `tap_ai_request` tap added to the tap registry
- [ ] Tap fires *before* the AI request is sent to the provider
- [ ] Tap signature: `tap_ai_request(request: &mut AiRequest, context: &AiRequestContext) -> AiRequestDecision`
- [ ] `AiRequestContext`: `user_id`, `plugin_name`, `operation_type`, `item_id` (if content-related), `field_name` (if field rule)
- [ ] `AiRequestDecision`: `Allow` (proceed as-is), `AllowModified` (proceed with modifications the tap made to the request), `Deny(reason: String)` (block the request, return error to calling plugin)
- [ ] Multiple plugins can implement the tap — first `Deny` wins (deny-wins aggregation, consistent with `tap_item_access`)
- [ ] Denied requests logged in `ai_usage_log` with `status = "denied"` and `deny_reason`
- [ ] At least 3 integration tests: allow passthrough, allow with modification, deny

**Implementation notes:**
- Add `tap_ai_request` to `crates/kernel/src/tap/` registry
- Add `AiRequestContext`, `AiRequestDecision` types to `crates/plugin-sdk/src/types.rs`
- Fire the tap in `crates/kernel/src/host/ai.rs` before sending to provider
- Plugin use cases (not implemented here): prompt sanitization (strip PII from prompts), content logging (log full prompts to audit_log), "needs review" flagging (set a flag when AI generates content), bias detection (scan AI responses)

---

### Story 47.6: Per-AI-Feature Configuration

**As a** site operator,
**I want** granular control over which AI operation types are enabled,
**So that** I can enable chat but disable image generation, or use different providers for different operations.

**Acceptance criteria:**

- [ ] Site config gains `ai_features` section:
  ```json
  {
    "chat": { "enabled": true, "provider": "anthropic", "model": "claude-sonnet-4-20250514" },
    "embedding": { "enabled": true, "provider": "openai", "model": "text-embedding-3-small" },
    "image_generation": { "enabled": false },
    "speech_to_text": { "enabled": false },
    "text_to_speech": { "enabled": false },
    "moderation": { "enabled": true, "provider": "openai" }
  }
  ```
- [ ] `ai_request()` checks feature config before dispatching — disabled operations return an error to the calling plugin
- [ ] Per-feature provider override: "use Anthropic for chat, OpenAI for embeddings"
- [ ] Per-feature model override: "use claude-sonnet for chat, but claude-haiku for moderation"
- [ ] Default: all operations enabled with the global provider/model config (backward compatible)
- [ ] Admin UI: `/admin/config/ai` page with per-operation toggles, provider, and model selection
- [ ] At least 2 integration tests: disabled operation returns error, per-operation provider selection

**Implementation notes:**
- Modify `crates/kernel/src/host/ai.rs` — check feature config before dispatch
- Add `ai_features` to site config schema
- Admin UI page in `crates/kernel/src/routes/admin.rs` (or appropriate admin module)
- The existing AI provider config becomes the *default*; per-feature config overrides it

---

## Plugin SDK Changes

| Change | File | Breaking? | Affected Plugins |
|---|---|---|---|
| `AiRequestContext` type | `crates/plugin-sdk/src/types.rs` | No (new type) | None — plugins implement `tap_ai_request` to use it |
| `AiRequestDecision` type | `crates/plugin-sdk/src/types.rs` | No (new type) | Same |
| `RouteMetadata` registration function | `crates/plugin-sdk/src/` | No (new function) | Plugins that want their routes documented call it |

**Migration guide:** No action required. All changes are additive. Existing AI-calling plugins continue to work — `tap_ai_request` only fires if a plugin implements it.

---

## Design Doc Updates (shipped with this epic)

| Doc | Changes |
|---|---|
| `docs/design/Design-Web-Layer.md` | Add "API Versioning" section: version routing, deprecation headers, route metadata. |
| `docs/design/Design-Plugin-SDK.md` | Add `tap_ai_request` to tap reference. Add `AiRequestContext`/`AiRequestDecision` types. Add route metadata registration. |
| `docs/design/ai-integration.md` | Add "AI Governance Infrastructure" section: metadata logging, tap_ai_request, per-feature config. Update the existing AI architecture diagram. |

---

## Tutorial Impact

| Tutorial Part | Sections Affected | Nature of Change |
|---|---|---|
| None | — | API versioning and AI governance are infrastructure invisible to the tutorial narrative. The tutorial uses API endpoints but doesn't discuss versioning or deprecation. |

**Note:** Epic 3 (AI as a Building Block) covers the tutorial treatment of AI features. This epic provides infrastructure that Epic 3's plugin implementations depend on.

---

## Recipe Impact

None. External interface infrastructure doesn't affect recipe command sequences.

---

## Screenshot Impact

| Part | Screenshots | Reason |
|---|---|---|
| None directly | Admin AI config page is new, but it's not part of the current tutorial | Future tutorial (AI tutorial) would need screenshots |

---

## Config Fixture Impact

`docs/tutorial/config/` may benefit from an `ai.yml` fixture showing per-feature AI configuration, but this is optional for the current tutorial scope.

---

## Migration Notes

**Database migrations:**
1. `YYYYMMDD000001_extend_ai_usage_log.sql` — ADD `latency_ms`, `plugin_name`, `finish_reason`, `status`, `deny_reason` to `ai_usage_log` (if not already present)

**Breaking changes:** None. All changes are additive. Existing `ai_request()` calls continue to work — `tap_ai_request` only fires if a plugin implements it; per-feature config defaults to "all enabled."

**Upgrade path:** Run migration. No configuration change required. Existing behavior unchanged.

---

## What's Deferred

- **Webhook plugin implementation** (delivery table, retry logic, exponential backoff, dead-letter queue, HMAC signing via kernel crypto from Epic C) — Plugin. This is the most significant deferred plugin work. The kernel provides `tap_item_*` hooks and `crypto_hmac_sha256`; the plugin does everything else.
- **OAuth2 admin UI** (client registration, listing, secret rotation, consent screen, token introspection RFC 7662) — Plugin. The kernel provides JWT/PKCE infrastructure; the plugin provides management UI.
- **OpenAPI spec generation** — Plugin. Reads kernel route metadata (from Story 47.1) and produces OpenAPI JSON/YAML.
- **GraphQL** — Plugin. Uses Gather queries as resolvers.
- **AI content logging** (full prompt/response content) — Plugin via `tap_ai_request`. Content is potentially massive and privacy-sensitive; logging policy is not kernel's job.
- **"Needs review" workflow for AI content** — Plugin. Reads `ai_generated` flag (from Epic F) and creates review tasks.
- **Prompt validation/sanitization** — Plugin via `tap_ai_request`. What constitutes a "valid" or "safe" prompt is policy.
- **Bias detection** — Plugin via `tap_ai_request`. Bias definitions are domain-specific policy.
- **Transparency labels** ("This content was AI-generated") — Plugin/theme. The kernel provides the `ai_generated` flag; display is theme territory.
- **Human approval workflow UI** — Plugin. The kernel provides the `ai_generated` flag and stage system; workflow UI is plugin territory.

---

## Related

- [Design-Web-Layer.md](../design/Design-Web-Layer.md) — HTTP routing and API
- [Design-Plugin-SDK.md](../design/Design-Plugin-SDK.md) — Host functions and taps
- [ai-integration.md](../design/ai-integration.md) — AI architecture
- [Epic F (15): Versioning & Audit](epic-15-versioning.md) — `ai_generated` flag dependency
- [Epic C (12): Security Hardening](epic-12-security.md) — Crypto host functions for webhook HMAC signing
- [Epic 3: AI as a Building Block](epic-03.md) — AI field rules and chatbot that depend on this infrastructure
