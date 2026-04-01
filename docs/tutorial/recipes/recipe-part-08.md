# Recipe: Part 8 — Production Ready

> **Synced with:** `docs/tutorial/part-08-production-ready.md`
> **Sync hash:** 694d1ee9
> **Last verified:** 2026-04-01
>
> Run `docs/tutorial/recipes/sync-check.sh` before starting to verify this recipe matches the current tutorial.

---

## Prerequisites

- Parts 1-7 must be completed.
- Server running with Postgres + Redis.

---

## Step 1: Verify Two-Tier Cache

`[CLI]` Verify cache is operational:

```bash
# Check Prometheus metrics include cache stats
curl -s http://localhost:3000/metrics | grep -c "cache"
# Expect: > 0

# Verify cache config env vars are read
grep "CACHE_TTL" .env || echo "Using defaults (60s global, 300s for users/items/categories)"
```

**Verify:** Metrics endpoint returns cache-related counters.

---

## Step 2: Verify Batch Operations

`[CLI]` Verify batch service is available:

```bash
# Trigger a search reindex (batch operation)
# Login first
rm -f /tmp/trovato-cookies.txt
LOGIN_PAGE=$(curl -s -c /tmp/trovato-cookies.txt http://localhost:3000/user/login)
CSRF=$(echo "$LOGIN_PAGE" | grep -oE 'name="_token" value="[a-f0-9]+"' | grep -oE '[a-f0-9]{64}')
curl -s -b /tmp/trovato-cookies.txt -c /tmp/trovato-cookies.txt \
  -X POST http://localhost:3000/user/login \
  -d "username=admin&password=trovato-admin1&_token=$CSRF" \
  -o /dev/null -w "%{http_code}"
# Expect: 303

# Reindex conferences
CSRF=$(curl -s -b /tmp/trovato-cookies.txt -c /tmp/trovato-cookies.txt \
  http://localhost:3000/admin | grep -oE 'csrf-token" content="[a-f0-9]+"' | grep -oE '[a-f0-9]{64}')
curl -s -b /tmp/trovato-cookies.txt -c /tmp/trovato-cookies.txt \
  -X POST http://localhost:3000/admin/structure/types/conference/search/reindex \
  --data-urlencode "_token=$CSRF" -o /dev/null -w "%{http_code}"
# Expect: 303
```

---

## Step 3: Verify File Storage Security

`[CLI]` Verify upload security:

```bash
# Health check (confirms Postgres + Redis)
curl -s http://localhost:3000/health | jq .

# Verify directory traversal blocked
curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/files/../../../etc/passwd
# Expect: 404

# Verify non-existent file returns 404
curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/files/nonexistent.jpg
# Expect: 404
```

---

## Step 4: Verify Cron & Queues

`[CLI]`

```bash
# Trigger cron
curl -s -X POST http://localhost:3000/cron/default-cron-key | jq '.status'
# Expect: "completed"

# Verify queue is drained
docker exec trovato-redis-1 redis-cli LLEN queue:ritrovo_import
# Expect: 0
```

---

## Step 5: Verify Observability

`[CLI]`

```bash
# Prometheus metrics
curl -s http://localhost:3000/metrics | head -20

# Health check
curl -s http://localhost:3000/health
# Expect: {"status":"healthy","postgres":true,"redis":true}

# Security headers present
curl -s -I http://localhost:3000/ | grep -E "content-security-policy|x-frame-options|x-content-type"
# Expect: all three headers present

# Rate limiting headers (after multiple rapid requests)
for i in $(seq 1 10); do curl -s -o /dev/null -w "%{http_code} " http://localhost:3000/api/v1/conferences; done
echo ""
# Expect: all 200 (well within 100/min limit)
```

---

## Step 6: Run Tests

`[CLI]`

```bash
# Unit tests
cargo test -p trovato-kernel --lib
# Expect: 727+ passed

# Plugin tests
cargo test -p ritrovo_importer
cargo test -p ritrovo_cfp
cargo test -p ritrovo_access

# All tests (requires running Postgres + Redis)
cargo test --all -- --test-threads=1

# Clippy (zero warnings)
cargo clippy --all-targets -- -D warnings
```

---

## Step 7: Verify Config Export/Import

`[CLI]`

```bash
# Export current config
cargo run --release --bin trovato -- config export /tmp/trovato-export/

# Count exported entities
ls /tmp/trovato-export/*.yml | wc -l
# Expect: > 50 files

# Dry-run import (verify round-trip)
cargo run --release --bin trovato -- config import /tmp/trovato-export/ --dry-run
```

---

## Completion Checklist

- [ ] Two-tier cache operational (Moka L1 + Redis L2)
- [ ] Cache TTLs configurable via environment variables
- [ ] Batch operations work (search reindex tested)
- [ ] File storage security verified (traversal blocked, MIME validated)
- [ ] Cron runs successfully with distributed locking
- [ ] Queue system drains work items
- [ ] Prometheus metrics endpoint operational
- [ ] Health check returns healthy status
- [ ] Security headers present on all responses
- [ ] Rate limiting functional
- [ ] Unit tests pass (727+)
- [ ] Integration tests pass
- [ ] Plugin tests pass
- [ ] Config export produces valid YAML
- [ ] Config import round-trips correctly
