# Recipe: Part 1 — Hello, Trovato

> **Synced with:** `docs/tutorial/part-01-hello-trovato.md`
> **Sync hash:** 215ebc7d
> **Last verified:** 2026-03-06
>
> Run `docs/tutorial/recipes/sync-check.sh` before starting to verify this recipe matches the current tutorial.

---

## Prerequisites

Before starting, check `TOOLS.md` for any previously recorded environment details. If `TOOLS.md` is empty or missing sections, fill them in as you go through this recipe.

---

## Step 1: Install Trovato

### 1.1 Check Prerequisites

`[CLI]` Verify required tools are installed. Record versions in `TOOLS.md -> Prerequisites`.

```bash
rustc --version                              # need 1.85+
$(brew --prefix libpq)/bin/psql --version    # need 16+ (psql is NOT on default PATH)
docker compose version                       # Docker Compose for services
```

**Note:** `psql` and `redis-cli` are typically not on PATH. PostgreSQL is accessed via `$(brew --prefix libpq)/bin/psql`. Redis runs inside Docker and can be pinged with `docker exec trovato-redis-1 redis-cli ping`.

**Verify:** All commands succeed. Record versions and paths in `TOOLS.md -> Prerequisites`.

### 1.2 Start Services

`[CLI]` Check `TOOLS.md -> Server` for existing start commands. If not recorded:

```bash
docker compose up -d
```

**Verify:** `docker compose ps` shows both `postgres` and `redis` containers as healthy.

Record in `TOOLS.md -> Server`.

### 1.3 Build and Start the Server

`[CLI]` Check `TOOLS.md -> Build` and `TOOLS.md -> Server` for commands. If not recorded:

```bash
ls .env || cp .env.example .env    # only needed on first run
cargo run --release --bin trovato   # run in background for agent use
```

Wait a few seconds after starting, then check health:

```bash
curl -s http://localhost:3000/health
```

**Verify:** Returns `{"status":"healthy","postgres":true,"redis":true}`.

Record start command and health URL in `TOOLS.md`.

**Troubleshooting:**
- "role trovato does not exist" — local PostgreSQL intercepting Docker port. Stop local PG or use local setup.
- To reset to clean slate: `docker compose down -v && docker compose up -d`, kill any running trovato process, then restart the server.

### 1.4 Run the Installer

`[CLI]` The installer has no CSRF protection and can be driven entirely with curl.

First, confirm the server redirects to the installer:

```bash
curl -s -o /dev/null -w "%{http_code} %{redirect_url}" http://localhost:3000/
# Expect: 303 http://localhost:3000/install
```

If it returns 200 instead, the installer has already run — skip to Step 1.5.

**Create the admin account:**

```bash
curl -s -X POST http://localhost:3000/install/admin \
  -d "username=admin&email=admin@example.com&password=trovato-admin1&password_confirm=trovato-admin1" \
  -o /dev/null -w "%{http_code}"
# Expect: 303
```

**Configure the site:**

```bash
curl -s -X POST http://localhost:3000/install/site \
  -d "site_name=Ritrovo&site_slogan=Tech+Conference+Aggregator&site_mail=admin@example.com" \
  -o /dev/null -w "%{http_code}"
# Expect: 303
```

**Record credentials in `TOOLS.md -> Admin UI`:** username `admin`, password `trovato-admin1`.

### 1.5 Verify Health Check

`[CLI]`

```bash
curl -s http://localhost:3000/health
```

**Verify:** Returns `{"status":"healthy","postgres":true,"redis":true}`.

Also confirm the root URL no longer redirects to the installer:

```bash
curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/
# Expect: 200
```

---

## Step 2: Create the Conference Item Type

### 2.0 Use Config Import

`[CLI]` The config import shortcut is the fastest agent-friendly path. It creates the conference type with all 12 fields and the pathauto pattern in one command.

```bash
cargo run --release --bin trovato -- config import docs/tutorial/config --dry-run
# Expect: "Would import 2 config entities" (item_type: 1, variable: 1)

cargo run --release --bin trovato -- config import docs/tutorial/config
# Expect: "Imported 2 config entities"
```

Record the config import command in `TOOLS.md -> Config`.

### 2.1 Verify the Type

`[CLI]` The imported config may take up to 60 seconds to appear due to cache TTL. Poll until it shows:

```bash
sleep 5
curl -s http://localhost:3000/api/content-types | jq '.[] | select(. == "conference")'
# Expect: "conference"
```

If empty, wait a few more seconds and retry.

---

## Step 3: Create Three Conferences

### 3.0 Log In and Get a Session

`[CLI]` All item creation requires an authenticated session with CSRF tokens. Log in once, then reuse the cookie jar.

```bash
rm -f /tmp/trovato-cookies.txt
LOGIN_PAGE=$(curl -s -c /tmp/trovato-cookies.txt http://localhost:3000/user/login)
CSRF=$(echo "$LOGIN_PAGE" | grep -oE 'name="_token" value="[a-f0-9]+"' | grep -oE '[a-f0-9]{64}')
curl -s -b /tmp/trovato-cookies.txt -c /tmp/trovato-cookies.txt \
  -X POST http://localhost:3000/user/login \
  -d "username=admin&password=trovato-admin1&_token=$CSRF" \
  -o /dev/null -w "%{http_code}"
# Expect: 303
```

Record the login flow in `TOOLS.md -> Admin UI` if not already there.

### 3.1 Helper: Fetch Fresh CSRF Token

CSRF tokens are **single-use**. Before each item creation, fetch a fresh one:

```bash
CSRF=$(curl -s -b /tmp/trovato-cookies.txt -c /tmp/trovato-cookies.txt \
  http://localhost:3000/admin | grep -oE 'csrf-token" content="[a-f0-9]+"' | grep -oE '[a-f0-9]{64}')
```

### 3.2 Conference 1: RustConf 2026

`[CLI]` Items are created via `POST /item/add/{type}` with a JSON body and `X-CSRF-Token` header.

```bash
# Fetch fresh CSRF
CSRF=$(curl -s -b /tmp/trovato-cookies.txt -c /tmp/trovato-cookies.txt \
  http://localhost:3000/admin | grep -oE 'csrf-token" content="[a-f0-9]+"' | grep -oE '[a-f0-9]{64}')

curl -s -b /tmp/trovato-cookies.txt -c /tmp/trovato-cookies.txt \
  -X POST http://localhost:3000/item/add/conference \
  -H "Content-Type: application/json" \
  -H "X-CSRF-Token: $CSRF" \
  -d '{
    "title": "RustConf 2026",
    "status": 1,
    "fields": {
      "field_url": "https://rustconf.com",
      "field_start_date": "2026-09-09",
      "field_end_date": "2026-09-11",
      "field_city": "Portland",
      "field_country": "United States",
      "field_cfp_url": "https://rustconf.com/cfp",
      "field_cfp_end_date": "2026-06-15",
      "field_description": "The official Rust conference, featuring talks on the latest Rust developments."
    }
  }'
# Expect: JSON with id, title, item_type, status
```

### 3.3 Conference 2: EuroRust 2026

`[CLI]`

```bash
CSRF=$(curl -s -b /tmp/trovato-cookies.txt -c /tmp/trovato-cookies.txt \
  http://localhost:3000/admin | grep -oE 'csrf-token" content="[a-f0-9]+"' | grep -oE '[a-f0-9]{64}')

curl -s -b /tmp/trovato-cookies.txt -c /tmp/trovato-cookies.txt \
  -X POST http://localhost:3000/item/add/conference \
  -H "Content-Type: application/json" \
  -H "X-CSRF-Token: $CSRF" \
  -d '{
    "title": "EuroRust 2026",
    "status": 1,
    "fields": {
      "field_url": "https://eurorust.eu",
      "field_start_date": "2026-10-15",
      "field_end_date": "2026-10-16",
      "field_city": "Paris",
      "field_country": "France",
      "field_description": "Europe'\''s premier Rust conference, bringing together Rustaceans from across the continent."
    }
  }'
```

### 3.4 Conference 3: WasmCon Online 2026

`[CLI]`

```bash
CSRF=$(curl -s -b /tmp/trovato-cookies.txt -c /tmp/trovato-cookies.txt \
  http://localhost:3000/admin | grep -oE 'csrf-token" content="[a-f0-9]+"' | grep -oE '[a-f0-9]{64}')

curl -s -b /tmp/trovato-cookies.txt -c /tmp/trovato-cookies.txt \
  -X POST http://localhost:3000/item/add/conference \
  -H "Content-Type: application/json" \
  -H "X-CSRF-Token: $CSRF" \
  -d '{
    "title": "WasmCon Online 2026",
    "status": 1,
    "fields": {
      "field_url": "https://wasmcon.dev",
      "field_start_date": "2026-07-22",
      "field_end_date": "2026-07-23",
      "field_online": "1",
      "field_description": "A virtual conference dedicated to WebAssembly, covering toolchains, runtimes, and the component model."
    }
  }'
```

### 3.5 Verify All Three Conferences

`[CLI]` Check `TOOLS.md -> Database` for the psql command.

```bash
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT id, title FROM item WHERE type = 'conference' ORDER BY created;"
```

**Verify:** Three rows: RustConf 2026, EuroRust 2026, WasmCon Online 2026.

---

## Step 4: Build Your First Gather

### 4.1 Create the Gather Query

`[CLI]` The gather form is an admin POST with `_token`, `definition_json`, and `display_json` fields.

```bash
CSRF=$(curl -s -b /tmp/trovato-cookies.txt -c /tmp/trovato-cookies.txt \
  http://localhost:3000/admin | grep -oE 'csrf-token" content="[a-f0-9]+"' | grep -oE '[a-f0-9]{64}')

DEFINITION='{"base_table":"item","item_type":"conference","filters":[{"field":"type","operator":"equals","value":"conference"},{"field":"status","operator":"equals","value":1}],"sorts":[{"field":"fields.field_start_date","direction":"asc"}],"stage_aware":true}'

DISPLAY='{"format":"list","items_per_page":25,"pager":{"enabled":true,"style":"full","show_count":true},"empty_text":"No conferences found."}'

curl -s -b /tmp/trovato-cookies.txt -c /tmp/trovato-cookies.txt \
  -X POST http://localhost:3000/admin/gather/create \
  --data-urlencode "_token=$CSRF" \
  --data-urlencode "_form_build_id=manual-1" \
  --data-urlencode "query_id=upcoming_conferences" \
  --data-urlencode "label=Upcoming Conferences" \
  --data-urlencode "description=Published conferences sorted by start date" \
  --data-urlencode "definition_json=$DEFINITION" \
  --data-urlencode "display_json=$DISPLAY" \
  -o /dev/null -w "%{http_code}"
# Expect: 303 (redirect to /admin/gather)
```

### 4.2 Create the URL Alias

`[CLI]`

```bash
CSRF=$(curl -s -b /tmp/trovato-cookies.txt -c /tmp/trovato-cookies.txt \
  http://localhost:3000/admin | grep -oE 'csrf-token" content="[a-f0-9]+"' | grep -oE '[a-f0-9]{64}')

curl -s -b /tmp/trovato-cookies.txt -c /tmp/trovato-cookies.txt \
  -X POST http://localhost:3000/admin/structure/aliases/add \
  --data-urlencode "_token=$CSRF" \
  --data-urlencode "source=/gather/upcoming_conferences" \
  --data-urlencode "alias=/conferences" \
  -o /dev/null -w "%{http_code}"
# Expect: 303
```

### 4.3 Verify the Gather

`[CLI]`

```bash
# The /conferences URL works (serves the gather via the alias we just created)
curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/conferences
# Expect: 200

# The tutorial gather also works at its direct URL
curl -s http://localhost:3000/api/query/upcoming_conferences/execute | jq '.total'
# Expect: 3
```

**Verify:** The conference card template renders correctly — each conference shows its title, dates, location, and description (not a raw timestamp):

```bash
# Verify card template is used (conf-card class present)
curl -s http://localhost:3000/conferences | grep -c 'class="conf-card__title"'
# Expect: 3 (one per conference)

# Verify conference descriptions appear (not raw timestamps)
curl -s http://localhost:3000/conferences | grep -o 'conf-card__desc">[^<]*' | head -3
# Expect: three lines with conference descriptions, e.g.:
#   conf-card__desc">A virtual conference dedicated to WebAssembly...
#   conf-card__desc">The official Rust conference...
#   conf-card__desc">Europe&#x27;s premier Rust conference...

# Verify date ranges appear (dates are on their own line inside the span)
curl -s http://localhost:3000/conferences | grep -oE '[0-9]{4}-[0-9]{2}-[0-9]{2} &ndash; [0-9]{4}-[0-9]{2}-[0-9]{2}' | head -3
# Expect: three date ranges, e.g.:
#   2026-07-22 &ndash; 2026-07-23
#   2026-09-09 &ndash; 2026-09-11
#   2026-10-15 &ndash; 2026-10-16
```

---

## Step 5: Human-Friendly URLs

### 5.0 Pathauto Pattern

The pathauto pattern (`conferences/[title]`) was already imported in Step 2 via `config import docs/tutorial/config` (the `variable.pathauto_patterns.yml` file). No additional configuration needed.

### 5.1 Regenerate Aliases

`[CLI]` The regenerate endpoint is a CSRF-protected admin form POST:

```bash
CSRF=$(curl -s -b /tmp/trovato-cookies.txt -c /tmp/trovato-cookies.txt \
  http://localhost:3000/admin | grep -oE 'csrf-token" content="[a-f0-9]+"' | grep -oE '[a-f0-9]{64}')

curl -s -b /tmp/trovato-cookies.txt -c /tmp/trovato-cookies.txt \
  -X POST http://localhost:3000/admin/config/pathauto/regenerate \
  --data-urlencode "_token=$CSRF" \
  --data-urlencode "item_type=conference" \
  -o /dev/null -w "%{http_code}"
# Expect: 303
```

### 5.2 Verify Aliases

`[CLI]`

```bash
curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/conferences/rustconf-2026
# Expect: 200
curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/conferences/eurorust-2026
# Expect: 200
curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/conferences/wasmcon-online-2026
# Expect: 200
```

Also verify in the database:

```bash
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT source, alias FROM url_alias WHERE alias LIKE '/conferences/%' ORDER BY alias;"
```

**Verify:** Three rows with aliases `/conferences/eurorust-2026`, `/conferences/rustconf-2026`, `/conferences/wasmcon-online-2026`.

---

## Completion Checklist

```bash
echo "=== Part 1 Completion Checklist ==="
echo -n "1. Server healthy: "; curl -s http://localhost:3000/health | jq -r '.status'
echo -n "2. Conference type: "; curl -s http://localhost:3000/api/content-types | jq -r '.[] | select(. == "conference")'
echo -n "3. Conference count: "; $(brew --prefix libpq)/bin/psql -t postgres://trovato:trovato@localhost:5432/trovato -c "SELECT COUNT(*) FROM item WHERE type = 'conference';" | tr -d ' '
echo -n "4. /conferences: "; curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/conferences
echo -n "5. Aliases: "
curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/conferences/rustconf-2026
echo -n " "
curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/conferences/eurorust-2026
echo -n " "
curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/conferences/wasmcon-online-2026
echo ""
```

Expected output:
```
1. Server healthy: healthy
2. Conference type: conference
3. Conference count: 3
4. /conferences: 200
5. Aliases: 200 200 200
```

All discoveries should be recorded in `TOOLS.md`.
