# Story 47.1: Route Metadata Annotations

Status: ready-for-dev

## Story

As a **plugin developer building API documentation tools**,
I want kernel routes annotated with structured metadata,
so that I can programmatically discover available endpoints and their capabilities.

## Acceptance Criteria

1. `RouteMetadata` struct defined with fields: `method` (HTTP method), `path` (route pattern), `summary` (human-readable description), `parameters` (list of path/query params), `response_type` (content type), `tags` (Vec of category strings), `deprecated` (bool)
2. All kernel API routes are annotated with `RouteMetadata`
3. Metadata is accessible via a `RouteRegistry` stored in `AppState`
4. `GET /api/v1/routes` endpoint returns all registered metadata as JSON
5. Admin API routes are annotated with `tags: ["admin"]`
6. Plugins can register route metadata via a `register_route_metadata` host function
7. At least 2 integration tests: route listing endpoint, plugin metadata registration

## Tasks / Subtasks

- [ ] Define `RouteMetadata` struct with all specified fields (AC: #1)
- [ ] Define `RouteRegistry` struct with methods to register and list metadata (AC: #3)
- [ ] Add `RouteRegistry` to `AppState` (AC: #3)
- [ ] Annotate all kernel API routes (~20 routes in `api_v1.rs`) with `RouteMetadata` (AC: #2)
- [ ] Tag admin API routes with `tags: ["admin"]` (AC: #5)
- [ ] Implement `GET /api/v1/routes` handler that serializes the registry to JSON (AC: #4)
- [ ] Implement `register_route_metadata` WASM host function for plugin route registration (AC: #6)
- [ ] Write integration test: `GET /api/v1/routes` returns expected metadata entries (AC: #7)
- [ ] Write integration test: plugin-registered metadata appears in route listing (AC: #7)

## Dev Notes

### Architecture

The `RouteRegistry` is populated at startup during route construction. Each route handler's metadata is registered alongside the route itself. The registry is read-only after startup (no locking needed for reads).

The `RouteMetadata` struct lives in the kernel crate (not plugin-sdk) since it describes kernel infrastructure. Plugins interact with it via host functions that accept serialized metadata.

The `/api/v1/routes` endpoint is itself registered in the metadata, providing self-documenting API discovery.

### Security

- The `/api/v1/routes` endpoint should respect permissions. Consider restricting to authenticated users or making it configurable.
- Plugin-registered metadata is validated: path patterns must start with the plugin's route prefix.

### Testing

- Hit `GET /api/v1/routes`, verify the response contains entries for known routes (e.g., `/api/v1/items`, `/api/v1/routes` itself).
- Register metadata via the host function in a test plugin, verify it appears in the route listing.

### References

- `crates/kernel/src/routes/api_v1.rs` -- existing API routes to annotate
- `crates/kernel/src/state.rs` -- `AppState` for registry storage
- `crates/kernel/src/host/` -- WASM host function implementations
