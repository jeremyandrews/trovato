# Epic 10: Production Ready

**Tutorial Part:** 8
**Trovato Phase Dependency:** Phase 6 (Files, Search, Cron, Hardening)
**BMAD Epic:** 39
**Status:** Complete (all features implemented)

---

## Narrative

*Parts 1-7 built the features. Part 8 proves they work at scale. Two-tier caching keeps pages fast, batch operations handle bulk work, S3 stores files durably, Prometheus monitors everything, and a comprehensive test suite makes it safe to change anything.*

---

## BMAD Stories

### Story 39.1: Two-Tier Caching with Tag-Based Invalidation

**As a** site operator serving thousands of concurrent visitors,
**I want** a two-tier cache (in-process + Redis) with tag-based invalidation,
**So that** pages load fast and content changes are reflected immediately.

**Acceptance criteria:**

- [x] Moka L1 cache with 60s TTL, 10K entry capacity
- [x] Redis L2 cache with 5min TTL, shared across instances
- [x] Tag-based invalidation via Redis Lua scripts
- [x] Stage-scoped cache keys (live vs. non-live stages)
- [x] Configurable TTLs per service via environment variables

### Story 39.2: Batch Operations Service

**As a** site administrator managing 5,000+ items,
**I want** batch operations for bulk content management,
**So that** I can reindex, regenerate aliases, or publish in bulk without timeout.

**Acceptance criteria:**

- [x] Batch operation types: reindex, pathauto regenerate, stage publish
- [x] Progress tracking with percentage and item count
- [x] Status lifecycle: pending → processing → completed/failed

### Story 39.3: S3-Compatible File Storage

**As a** site operator deploying to cloud infrastructure,
**I want** S3-compatible file storage,
**So that** files are durable, CDN-ready, and don't depend on local disk.

**Acceptance criteria:**

- [x] S3FileStorage implementation with AWS SDK
- [x] LocalFileStorage for development
- [x] Configurable via FILE_STORAGE, S3_BUCKET, S3_REGION env vars
- [x] Same security rules (MIME allowlist, magic byte validation, filename sanitization) for both backends
- [x] Tenant-scoped URIs for multi-tenant deployments

### Story 39.4: Cron with Distributed Locking

**As a** site operator running multiple server instances,
**I want** cron with distributed locking,
**So that** scheduled tasks run exactly once even with multiple instances.

**Acceptance criteria:**

- [x] Redis distributed lock with heartbeat
- [x] `tap_cron` dispatch to all plugins
- [x] Queue worker processing during cron runs
- [x] Kernel maintenance tasks (temp file cleanup, session pruning)
- [x] External trigger via HTTP POST with cron key

### Story 39.5: Prometheus Metrics & Health Check

**As a** DevOps engineer monitoring the site,
**I want** Prometheus metrics and a health check endpoint,
**So that** I can set up alerts and load balancer probes.

**Acceptance criteria:**

- [x] `/metrics` endpoint in Prometheus exposition format
- [x] `/health` endpoint verifying Postgres and Redis connectivity
- [x] HTTP request duration and count metrics
- [x] Rate limiting with per-endpoint Redis sliding window counters

### Story 39.6: Comprehensive Test Suite

**As a** developer making changes to the kernel,
**I want** a comprehensive test suite,
**So that** I can refactor with confidence.

**Acceptance criteria:**

- [x] 727+ unit tests covering all kernel modules
- [x] 325+ integration tests across 15 test files
- [x] Plugin tests via native `__inner_*` functions
- [x] Load testing tool for performance verification
- [x] CI pipeline running all checks on every push

### Story 39.7: Configuration Import/Export

**As a** site builder deploying across environments,
**I want** complete configuration import/export,
**So that** I can version-control my site configuration and replicate it.

**Acceptance criteria:**

- [x] Export all 13 entity types to YAML
- [x] Import with dependency ordering (types before content)
- [x] Upsert semantics (create or update)
- [x] Dry-run mode for validation before import

---

## What's Deferred

- CDN URL rewriting (plugin territory)
- Database replication / read replicas
- Distributed tracing (OpenTelemetry)
- Response compression (reverse proxy territory)
- HTTP/2 (reverse proxy territory)
