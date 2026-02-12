# Trovato

A content management system built in Rust, reimagining Drupal 6's mental model with modern foundations.

## What It Is

Trovato takes the core ideas that made Drupal 6 powerful—nodes, fields, views, hooks—and rebuilds them with:

- **Axum + Tokio** for async HTTP
- **PostgreSQL + JSONB** for flexible field storage without join complexity
- **WebAssembly plugins** running in per-request sandboxes via Wasmtime
- **Content staging** built into the schema from day one

## Security Model

Plugins are untrusted code. They run in WASM sandboxes, return JSON render trees (not raw HTML), and access data through a structured API. The kernel sanitizes all output. This isn't optional—the WASM boundary enforces isolation whether plugin authors intend it or not.

## Scaling

No persistent state in the binary. PostgreSQL and Redis handle everything. Horizontal scaling works out of the box.

---

## Progress

### Phase 0: WASM Architecture Validation
Benchmarked WASM plugin performance on ARM and x86-64. Validated that full-serialization (passing complete JSON to plugins) outperforms handle-based field access by 1.2-1.6x. Confirmed pooling allocator scales to 2000+ concurrent requests with sub-millisecond p95 latency.

### Phase 1: Skeleton
Built the HTTP server foundation with Axum, PostgreSQL via SQLx, and Redis sessions. Implemented user authentication (Argon2id), role-based permissions, account lockout, password reset, and stage switching.

---

*This project is being developed with AI assistance.*
