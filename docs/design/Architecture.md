# Trovato: Architecture Components

## Stack

| Layer | Technology | Purpose |
|-------|-----------|---------|
| HTTP | Axum + Tokio | Async web framework, Tower middleware |
| Database | PostgreSQL + SQLx | Content, users, config, search vectors |
| Cache | Redis + moka | L1 in-process (moka), L2 shared (Redis) |
| Sessions | Redis (tower-sessions) | HttpOnly, Secure, SameSite=Strict |
| Plugins | Wasmtime (WASM) | Sandboxed, runtime-loaded, pooled per-request |
| Query Builder | SeaQuery | Type-safe SQL generation (Gather engine) |
| Templates | Tera | Strict templating with layered resolution |
| Auth | Argon2id | Password hashing |
| Files | Local / S3 | Pluggable FileStorage trait |
| Observability | tracing + Prometheus | Structured logs, metrics on /metrics |

## Core Subsystems

1. **Web Layer** (Axum): Static routes + dynamic route handler via MenuRegistry
2. **Sessions/Auth**: Redis-backed, stage-aware sessions
3. **Plugin System** (WASM): Pooled instantiation, WIT v2 interface, structured DB API
4. **Tap System**: Explicit registration in .info.toml, weight-based ordering
5. **Render Tree**: JSON Render Elements -> alter taps -> sanitize -> Tera templates
6. **Record System**: Items with JSONB fields, content types, field validation
7. **Stages**: Content staging with revision tracking, stage-aware loading
8. **Categories**: Vocabularies, terms, hierarchy (DAG), recursive CTEs
9. **Gather Engine**: SeaQuery SQL builder, filters, relationships, pager, cache
10. **Form API**: Declarative JSON forms, tap_form_alter, CSRF, multi-step, AJAX
11. **File Storage**: Upload validation, temp cleanup, pluggable backends
12. **Cron/Queues**: Distributed locking (Redis), heartbeat pattern, queue workers
13. **Search**: PostgreSQL tsvector, configurable field weights, pluggable backend trait
14. **Caching**: Tag-based invalidation via Lua scripts, two-tier (moka + Redis)
15. **Observability**: Gander profiling middleware, Prometheus metrics, health check

## Key Design Rules

1. **Kernel holds no persistent state.** All state in Postgres or Redis. Horizontal scaling without session affinity.
2. **Plugins are untrusted.** WASM boundary enforces isolation. Structured DB API prevents injection.
3. **No opaque HTML from plugins.** JSON Render Elements only. Kernel handles sanitization.

## Database Schema (20+ tables)

Core: users, sessions, item, item_revision, item_type, field_config, stage, stage_association, category_vocabulary, category_term, category_term_hierarchy, permission, role, role_permission, menu_router, system (plugins), variable, cache, queue, search_dataset, search_field_config, file_managed, form_state_cache
