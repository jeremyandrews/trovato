# Recipe: Part 9 — AI & Intelligent Search

> **Synced with:** `docs/tutorial/part-09-ai-and-search.md`
> **Sync hash:** 312c1824
> **Last verified:** 2026-04-05 (AI provider, AI Assist, Scolta search, Pagefind index)
>
> Run `docs/tutorial/recipes/sync-check.sh` before starting to verify this recipe matches the current tutorial.

---

## Prerequisites

- Parts 1-2 must be completed (conferences imported, search config applied).
- An AI API key (Anthropic or OpenAI) must be available.
- Check `TOOLS.md` for server start commands and database connection.

---

## Step 1: Enable trovato_ai

`[CLI]`

```bash
cargo run --release --bin trovato -- plugin enable trovato_ai
# Restart server
```

**Verify:** `curl -s http://localhost:3000/health` returns healthy. Plugin list at `/admin/plugins` shows `trovato_ai` as Enabled.

## Step 2: Configure AI Provider

`[CLI]` Set the API key as an environment variable:

```bash
export ANTHROPIC_API_KEY=sk-ant-...
# Add to .env for persistence
```

`[UI]` Navigate to `/admin/system/ai-providers`:
1. Click "Add provider"
2. Fill: Label=Anthropic, Protocol=Anthropic, Base URL=`https://api.anthropic.com/v1`, API Key Env=`ANTHROPIC_API_KEY`, Chat model=`claude-sonnet-4-20250514`
3. Save
4. Set Chat default to Anthropic in "Default Providers"

**Verify:** Test connection shows green checkmark.

## Step 3: Test AI Assist

`[CLI]`

```bash
# Log in
rm -f /tmp/trovato-cookies.txt
LOGIN_PAGE=$(curl -s -c /tmp/trovato-cookies.txt http://localhost:3000/user/login)
CSRF=$(echo "$LOGIN_PAGE" | grep -oE 'name="_token" value="[a-f0-9]+"' | grep -oE '[a-f0-9]{64}')
curl -s -b /tmp/trovato-cookies.txt -c /tmp/trovato-cookies.txt \
  -X POST http://localhost:3000/user/login \
  -d "username=admin&password=trovato-admin1&_token=$CSRF" -o /dev/null

# Get CSRF for API
CSRF=$(curl -s -b /tmp/trovato-cookies.txt -c /tmp/trovato-cookies.txt \
  http://localhost:3000/admin | grep -oE 'csrf-token" content="[a-f0-9]+"' | grep -oE '[a-f0-9]{64}')

# Test AI assist
curl -s -b /tmp/trovato-cookies.txt -c /tmp/trovato-cookies.txt \
  -X POST http://localhost:3000/api/v1/ai/assist \
  -H "Content-Type: application/json" \
  -H "X-CSRF-Token: $CSRF" \
  -d '{"text": "A conference about Rust programming.", "operation": "expand"}'
```

**Verify:** Returns JSON with `result` (expanded text) and `tokens_used` > 0.

## Step 4: Enable Search Indexing

`[CLI]`

```bash
cargo run --release --bin trovato -- plugin enable trovato_search
# Restart server, then:
curl -s -X POST http://localhost:3000/cron/default-cron-key | jq '.status'
# Expect: "completed"
```

**Verify:** Pagefind index exists:

```bash
ls static/pagefind/pagefind-entry.json
# Should exist
```

## Step 5: Verify Search

`[CLI]`

```bash
curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/search?q=rust
# Expect: 200

# Verify search returns results
curl -s http://localhost:3000/api/search?q=rust | jq '.total'
# Expect: > 0
```

`[BROWSER]` Visit `/search?q=rust` — should show search results with the full site layout.

---

## Completion Checklist

```bash
echo "=== Part 9 Completion Checklist ==="
echo -n "1. trovato_ai enabled: "; curl -s http://localhost:3000/admin/plugins 2>/dev/null | grep -c "trovato_ai" || echo "check manually"
echo -n "2. AI assist works: "; curl -s -b /tmp/trovato-cookies.txt -X POST http://localhost:3000/api/v1/ai/assist -H "Content-Type: application/json" -H "X-CSRF-Token: $CSRF" -d '{"text":"test","operation":"rewrite"}' 2>/dev/null | python3 -c "import sys,json; print('yes' if json.load(sys.stdin).get('result') else 'no')" 2>/dev/null || echo "no"
echo -n "3. Search works: "; curl -s http://localhost:3000/api/search?q=rust 2>/dev/null | python3 -c "import sys,json; d=json.load(sys.stdin); print(f'{d[\"total\"]} results')" 2>/dev/null || echo "check"
echo -n "4. Pagefind index: "; ls static/pagefind/pagefind-entry.json 2>/dev/null && echo "built" || echo "not built"
```
