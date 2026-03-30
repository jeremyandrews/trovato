# Story 44.3: Static Asset Content Hashing

Status: ready-for-dev

## Story

As a **site operator deploying updates**,
I want static assets served with content-hashed filenames,
so that browsers can cache assets aggressively while still receiving updates immediately after deploys.

## Acceptance Criteria

1. At startup, scan the `static/` directory and compute a SHA-256 hash of each file's contents
2. Build an asset manifest mapping original paths to hashed filenames (e.g., `theme.css` -> `theme.a1b2c3d4.css`)
3. Register a Tera template function `{{ asset_url("css/theme.css") }}` that returns the hashed path
4. Hashed asset paths are served with `Cache-Control: public, max-age=31536000, immutable`
5. Non-hashed (original) paths still work as a fallback with standard caching headers
6. `base.html` template updated to use `asset_url()` for CSS and JS references
7. The manifest is regenerated on every startup (no persistent cache file needed)
8. At least 1 integration test verifying hashed URL generation and cache headers

## Tasks / Subtasks

- [ ] At startup, walk `static/` directory and compute SHA-256 for each file (AC: #1)
- [ ] Build in-memory manifest mapping `original_path -> hashed_path` using first 8 hex chars of hash (AC: #2)
- [ ] Create symlinks or serve hashed filenames via route handler that strips the hash and serves the original file (AC: #2)
- [ ] Register `asset_url` as a Tera global function that looks up the manifest (AC: #3)
- [ ] Set `Cache-Control: public, max-age=31536000, immutable` on responses matching hashed paths (AC: #4)
- [ ] Ensure non-hashed paths still resolve to the same files with default cache headers (AC: #5)
- [ ] Update `base.html` to use `{{ asset_url("css/theme.css") }}` instead of hardcoded paths (AC: #6)
- [ ] Ensure manifest is rebuilt from scratch on each startup (AC: #7)
- [ ] Write integration test: request asset via `asset_url` output, verify correct content and `Cache-Control` header (AC: #8)

## Dev Notes

### Architecture

The asset manifest is an `Arc<HashMap<String, String>>` stored in `AppState` and populated during startup. The `asset_url` Tera function receives the `AppState` reference (or a clone of the manifest) and performs a lookup. If the path is not found in the manifest (e.g., dynamically added files), it falls back to the original path. The hashed filename format is `{stem}.{hash8}.{ext}` where `hash8` is the first 8 characters of the hex-encoded SHA-256 digest.

The static file handler recognizes hashed filenames by pattern, strips the hash segment, and serves the underlying file. This avoids duplicating files on disk.

### Testing

- Create a temporary static file, start the app, call `asset_url` and verify the returned path contains a hash segment.
- Request the hashed URL and verify `Cache-Control: public, max-age=31536000, immutable`.
- Request the original URL and verify it still serves the file (fallback).

### References

- `crates/kernel/src/routes/static_files.rs` — existing static file serving
- `templates/base.html` — base template with CSS/JS references
- Tera custom function registration in theme engine setup
