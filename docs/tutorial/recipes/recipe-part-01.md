# Recipe: Part 1 — Hello, Trovato

> **Synced with:** `docs/tutorial/part-01-hello-trovato.md`
> **Sync hash:** 5f3bfd1d
> **Last verified:** 2026-03-05
>
> Run `docs/tutorial/recipes/sync-check.sh` before starting to verify this recipe matches the current tutorial.

---

## Prerequisites

Before starting, check `TOOLS.md` for any previously recorded environment details. If `TOOLS.md` is empty or missing sections, fill them in as you go through this recipe.

---

## Step 1: Install Trovato

### 1.1 Check Prerequisites

`[CLI]` Verify required tools are installed. Record versions in `TOOLS.md -> Prerequisites`.

```
rustc --version        # need 1.85+
psql --version         # need 16+
redis-cli ping         # should print PONG
```

**Verify:** All three commands succeed. If any fail, stop and resolve before continuing.

### 1.2 Start Services

`[CLI]` Check `TOOLS.md -> Server` for existing start commands. If not recorded, use one of:

**Option A: Docker Compose**
```
docker compose up -d
```

**Option B: Local PostgreSQL + Redis**
```
psql -c "CREATE USER trovato WITH PASSWORD 'trovato';"
psql -c "CREATE DATABASE trovato OWNER trovato;"
```
Ensure Redis is running locally.

Record whichever approach worked in `TOOLS.md -> Server`.

### 1.3 Build and Start the Server

`[CLI]` Check `TOOLS.md -> Build` for the build command. If not recorded:

```
cargo build --release
```

Check `TOOLS.md -> Server` for the start command. If not recorded:

```
cp .env.example .env   # only on first run
cargo run --release --bin trovato
```

Record the working start command in `TOOLS.md -> Server`.

**Verify:** Server starts without errors. Check logs for migration and plugin discovery output.

**Troubleshooting:**
- "role trovato does not exist" — local PostgreSQL is intercepting the Docker port. Stop local PostgreSQL or use Option B.
- To reset: `docker compose down -v && docker compose up -d`, then restart the server.

### 1.4 Run the Installer

`[UI-ONLY]` The web installer runs on first startup only.

1. Open `$BASE_URL` (check `TOOLS.md -> Server` for base URL; default `http://localhost:3000`).
2. You will be redirected to the installer. Walk through all four steps:
   - **Welcome** — confirms DB and Redis connections.
   - **Create Admin Account** — set username, email, password (min 12 chars). **Record credentials in `TOOLS.md -> Admin UI`.**
   - **Site Configuration** — set site name, slogan, contact email.
   - **Complete** — follow link to site.

### 1.5 Verify Health Check

`[CLI]` Check `TOOLS.md -> API` for the health check URL. If not recorded:

```
curl http://localhost:3000/health
```

**Verify:** Response is `{"status":"healthy","postgres":true,"redis":true}`.

Record the health check URL in `TOOLS.md -> API`.

---

## Step 2: Create the Conference Item Type

### 2.0 Choose Your Path

There are two ways to create the conference type:

- **Shortcut (config import)** — skip to Step 2.S below.
- **Manual (admin UI)** — follow Steps 2.1 through 2.4.

### 2.S Config Import Shortcut

`[CLI]` Import the pre-built conference type:

```
cargo run --release --bin trovato -- config import docs/tutorial/config --dry-run
cargo run --release --bin trovato -- config import docs/tutorial/config
```

Record the config import command in `TOOLS.md -> Config`.

**Verify:** After import, wait up to 60 seconds (cache TTL), then:
```
curl $BASE_URL/api/content-types | jq '.[] | select(. == "conference")'
```
Should output `"conference"`. Skip to Step 3 if using this path.

### 2.1 Create the Type

`[UI-ONLY]` Navigate to `$ADMIN_URL/structure/types/add` (see `TOOLS.md -> Admin UI` for base admin URL; default `http://localhost:3000/admin`).

Fill in the form:

| Field | Value |
|---|---|
| Name | Conference |
| Machine name | conference |
| Description | A tech conference or meetup event |
| Title field label | Conference Name |

Click **Save content type**.

**Verify:** Redirected to content types list. "Conference" appears alongside "Basic Page".

### 2.2 Add Fields

`[UI-ONLY]` Navigate to `$ADMIN_URL/structure/types/conference/fields`.

Add each field one at a time using the "Add a new field" form. For each row: fill in Label, verify/edit Machine name, select Type, click **Add field**.

| Label | Machine name | Type |
|---|---|---|
| Website URL | `field_url` | Text (plain) |
| Start Date | `field_start_date` | Date |
| End Date | `field_end_date` | Date |
| City | `field_city` | Text (plain) |
| Country | `field_country` | Text (plain) |
| Online | `field_online` | Boolean |
| CFP URL | `field_cfp_url` | Text (plain) |
| CFP End Date | `field_cfp_end_date` | Date |
| Description | `field_description` | Text (long) |
| Language | `field_language` | Text (plain) |
| Source ID | `field_source_id` | Text (plain) |
| Editor Notes | `field_editor_notes` | Text (long) |

**Important:** For "Website URL", the auto-generated machine name will be `field_website_url`. Edit it to `field_url` before clicking **Add field**.

### 2.3 Make Date Fields Required

`[UI-ONLY]` For **Start Date** and **End Date**: click the **Edit** link next to each field, check the **Required** checkbox, and save.

### 2.4 Verify the Type

`[CLI]`
```
curl $BASE_URL/api/content-types | jq '.[] | select(. == "conference")'
```

**Verify:** Output is `"conference"`.

Record the content-types API endpoint in `TOOLS.md -> API` if not already there.

---

## Step 3: Create Your First Conference

### 3.0 Overview

This step creates three conferences by hand via the admin UI. All three are `[UI-ONLY]`.

### 3.1 Conference 1: RustConf 2026

`[UI-ONLY]` Navigate to `$ADMIN_URL/content/add/conference`.

| Field | Value |
|---|---|
| Conference Name | RustConf 2026 |
| Website URL | https://rustconf.com |
| Start Date | 2026-09-09 |
| End Date | 2026-09-11 |
| City | Portland |
| Country | United States |
| CFP URL | https://rustconf.com/cfp |
| CFP End Date | 2026-06-15 |
| Description | The official Rust conference, featuring talks on the latest Rust developments. |
| Published | (checked) |

Click **Create content**.

**Verify:** Redirected to `$ADMIN_URL/content`. "RustConf 2026" appears in the list.

### 3.2 Conference 2: EuroRust 2026

`[UI-ONLY]` Navigate to `$ADMIN_URL/content/add/conference`.

| Field | Value |
|---|---|
| Conference Name | EuroRust 2026 |
| Website URL | https://eurorust.eu |
| Start Date | 2026-10-15 |
| End Date | 2026-10-16 |
| City | Paris |
| Country | France |
| Description | Europe's premier Rust conference, bringing together Rustaceans from across the continent. |
| Published | (checked) |

Click **Create content**.

### 3.3 Conference 3: WasmCon Online 2026

`[UI-ONLY]` Navigate to `$ADMIN_URL/content/add/conference`.

| Field | Value |
|---|---|
| Conference Name | WasmCon Online 2026 |
| Website URL | https://wasmcon.dev |
| Start Date | 2026-07-22 |
| End Date | 2026-07-23 |
| Online | (checked) |
| Description | A virtual conference dedicated to WebAssembly, covering toolchains, runtimes, and the component model. |
| Published | (checked) |

Leave City and Country blank (online event). Click **Create content**.

### 3.4 Verify All Three Conferences

`[CLI]` Query the database to confirm. Check `TOOLS.md -> Database` for the connection string. If not recorded, default is:

```
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT id, title FROM item WHERE type = 'conference' ORDER BY created;"
```

**Verify:** Three rows returned: RustConf 2026, EuroRust 2026, WasmCon Online 2026.

Record the psql command in `TOOLS.md -> Database` if not already there.

`[CLI]` Also verify via API — pick any UUID from the query above:
```
curl $BASE_URL/api/item/<UUID> | jq .title
```

**Verify:** Returns the conference title.

---

## Step 4: Build Your First Gather

### 4.1 Create the Gather Query

`[UI-ONLY]` Navigate to `$ADMIN_URL/gather/create`.

| Field | Value |
|---|---|
| Query ID | upcoming_conferences |
| Label | Upcoming Conferences |
| Description | Published conferences sorted by start date |
| Base Table | item |

**Add two filters** (click **Add filter** twice):

Filter 1:
- Field: `type`
- Operator: `equals`
- Value: `conference`

Filter 2:
- Field: `status`
- Operator: `equals`
- Value: `1`

**Add a sort:**
- Field: `fields.field_start_date`
- Direction: `asc`

**Display settings:**
- Format: `list`
- Items per page: `25`
- Empty text: `No conferences found.`

Click **Save**.

### 4.2 Create the URL Alias

`[UI-ONLY]` Navigate to `$ADMIN_URL/structure/aliases/add`.

| Field | Value |
|---|---|
| Source path | /gather/upcoming_conferences |
| Alias | /conferences |

Click **Save**.

### 4.3 Verify the Gather

`[CLI]`
```
curl -s -o /dev/null -w "%{http_code}" $BASE_URL/conferences
```

**Verify:** Returns `200`.

`[CLI]` Also verify via API:
```
curl $BASE_URL/api/query/upcoming_conferences/execute | jq '.total'
```

**Verify:** Returns `3` (the three conferences created in Step 3).

Record the gather API endpoint pattern in `TOOLS.md -> API`.

`[UI-ONLY]` Visit `$BASE_URL/conferences` in a browser. Confirm three conference cards appear sorted by start date: WasmCon Online (July), RustConf (September), EuroRust (October).

---

## Step 5: Human-Friendly URLs

### 5.0 Choose Your Path

- **Option A: Admin UI** — follow Step 5.1.
- **Option B: Config Import** — follow Step 5.S, then Step 5.2.

### 5.1 Option A: Configure Pathauto via Admin UI

`[UI-ONLY]` Navigate to `$ADMIN_URL/config/pathauto`.

For the **Conference** row, enter: `conferences/[title]`

Click **Save configuration**.

Click **Regenerate aliases** next to Conference.

**Verify:** The regeneration report shows 3 aliases created.

### 5.S Option B: Config Import

`[CLI]`
```
cargo run --release --bin trovato -- config import docs/tutorial/config
```

Then navigate to `$ADMIN_URL/config/pathauto` and click **Regenerate aliases** next to Conference. `[UI-ONLY]` — the regenerate endpoint requires a CSRF-protected form POST.

### 5.2 Verify Aliases

`[CLI]`
```
curl -s -o /dev/null -w "%{http_code}" $BASE_URL/conferences/rustconf-2026
curl -s -o /dev/null -w "%{http_code}" $BASE_URL/conferences/eurorust-2026
curl -s -o /dev/null -w "%{http_code}" $BASE_URL/conferences/wasmcon-online-2026
```

**Verify:** All three return `200`.

`[CLI]` Also verify in the database:
```
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT source, alias FROM url_alias WHERE alias LIKE '/conferences/%' ORDER BY alias;"
```

**Verify:** Three rows with aliases `/conferences/eurorust-2026`, `/conferences/rustconf-2026`, `/conferences/wasmcon-online-2026`.

---

## Completion Checklist

After completing all steps, verify the full Part 1 outcome:

- [ ] Server running and healthy (`curl $BASE_URL/health`)
- [ ] `conference` content type exists (`curl $BASE_URL/api/content-types`)
- [ ] Three conferences in the database
- [ ] Gather listing at `/conferences` shows three conferences sorted by start date
- [ ] URL aliases work: `/conferences/rustconf-2026`, `/conferences/eurorust-2026`, `/conferences/wasmcon-online-2026`
- [ ] All discoveries recorded in `TOOLS.md`
