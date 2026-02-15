# Netgrasp Integration Validation (Story 20.4)

**Date**: 2026-02-15
**Status**: Complete
**Gate**: Trovato handles a Netgrasp-style network monitoring application using only existing infrastructure — no custom endpoints, no schema changes, no new migrations.

## What Was Validated

### Content Model

Six content types registered at startup using `ContentTypeRegistry::create()`:

| Type | Label | Key Fields |
|------|-------|------------|
| `ng_device` | Device | mac, display_name, hostname, vendor, device_type, os_family, state, last_ip, current_ap, owner_id, hidden, notify, baseline |
| `ng_person` | Person | name, notes, notification_prefs |
| `ng_event` | Event | device_id, event_type, timestamp, details |
| `ng_presence` | Presence Session | device_id, start_time, end_time |
| `ng_ip_history` | IP History | device_id, ip_address, first_seen, last_seen |
| `ng_location` | Location History | device_id, location, start_time, end_time |

All field definitions stored in `settings` JSONB. No new database tables or migrations required — the existing `item` table with JSONB `fields` column handles everything.

### Gather Queries

Two queries registered with exposed filters on JSONB fields:

**`ng_device_list`** — Device dashboard:
- Filters: status=1 (published), plus 3 exposed filters (state, device_type, owner_id) using `Contains` operator
- Sort: display_name ASC
- Display: Table format, 50 per page, pager enabled
- URL alias: `/devices` → `/gather/ng_device_list`

**`ng_event_log`** — Event log:
- Filters: status=1, plus 2 exposed time-range filters (timestamp GreaterOrEqual/LessOrEqual)
- Sort: timestamp DESC
- Display: Table format, 100 per page, pager enabled
- URL alias: `/events` → `/gather/ng_event_log`

The `Contains` filter with an empty default string produces `LIKE '%%'` which matches all rows, acting as a no-op when no filter value is provided. This lets exposed filters work as optional refinements without breaking the base query.

### Auth Roles

Two roles with per-type permissions:

**`network_admin`**: `access content` + create/edit/delete for all 6 `ng_*` types (19 permissions total).

**`ng_viewer`**: `access content` only (read-only dashboard access).

All role creation is idempotent — guarded by `Role::find_by_name()` check, and `Role::add_permission()` uses `ON CONFLICT DO NOTHING`.

### Dashboard Templates

Two Tera templates using the theme suggestion system:

- `templates/gather/query--ng_device_list.html` — Device table with Name, MAC, State, Type, Last IP columns. State column uses CSS class for color-coding (online=green, offline=gray, unknown=amber).
- `templates/gather/query--ng_event_log.html` — Event table with Time, Type, Device, Details columns.

Templates follow the established `query--{id}.html` naming convention and are automatically discovered by the Gather route handler.

### Performance Notes

The Netgrasp use case involves moderate write volume (device scans every few minutes, ~100 events/min at peak). PostgreSQL handles this easily with the existing JSONB + GIN index approach. A Raspberry Pi 4 with 4GB RAM can run PostgreSQL + Trovato comfortably for a home network monitoring setup.

For high-traffic deployments, expression indexes on frequently filtered JSONB fields (e.g., `fields->>'state'`, `fields->>'device_type'`) would improve query performance.

## What This Proves

1. **Content model flexibility**: The `item` table + JSONB fields pattern handles domain-specific data (network devices, events, presence) without schema changes.
2. **Query engine expressiveness**: Gather queries with exposed filters on JSONB fields work correctly for dashboard-style listings.
3. **Auth granularity**: Per-type permissions (`create ng_device content`, `edit any ng_event content`) provide fine-grained access control.
4. **Template system**: Theme suggestion templates render domain-specific dashboards without any route changes.
5. **URL aliases**: Clean URLs (`/devices`, `/events`) work via the existing alias system.

## Files Changed

| File | Action |
|------|--------|
| `crates/kernel/src/state.rs` | Added `register_netgrasp_validation()` helper |
| `templates/gather/query--ng_device_list.html` | New — device dashboard template |
| `templates/gather/query--ng_event_log.html` | New — event log template |
| `docs/netgrasp-validation.md` | New — this document |
