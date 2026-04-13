# T2: Integrations — ARCHIVED

Prompt executed 2026-04-13. All 5 plugins committed as `da48f04`.

## What was built

| Plugin | Commit | Status |
|--------|--------|--------|
| trovato_scolta | da48f04 | WASM compiles, not runtime-tested |
| trovato_captcha | da48f04 | WASM compiles, needs Turnstile keys |
| tag1_hubspot | da48f04 | WASM compiles, needs HubSpot OAuth |
| trovato_feeds | da48f04 | WASM compiles, needs Gather queries |
| trovato_series | da48f04 | WASM compiles, needs content with series fields |

## Runtime dependencies not yet configured

- Scolta: AI provider must be configured in admin, Pagefind index must be built
- CAPTCHA: TURNSTILE_SITE_KEY + TURNSTILE_SECRET_KEY env vars
- HubSpot: HUBSPOT_CLIENT_ID + HUBSPOT_CLIENT_SECRET + HUBSPOT_REFRESH_TOKEN
- Feeds: Gather queries for "insights" and "planet-drupal" must exist
- Series: Blog posts with field_series_title populated

## Integration test checklist (from prompt)

- [ ] Search: query expansion + summarization + follow-up
- [ ] CAPTCHA: Turnstile widget + server validation
- [ ] Contact form → HubSpot
- [ ] RSS feeds: /rss/insights.xml, /rss/planet-drupal.xml
- [ ] Series pager: prev/next navigation on multi-part posts
