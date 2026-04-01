# Story 38.5: REST API Endpoints

Status: done

## Story

As an **API consumer**,
I want versioned REST API endpoints with pagination and filtering,
so that I can programmatically access conference data for external applications and integrations.

## Acceptance Criteria

1. Versioned API at `/api/v1/` with `x-api-version: 1` response header
2. List conferences endpoint with pagination (`page`, `per_page`) and filters (`topic`, `country`, `online`, `lang`, `stage`, `q`)
3. Single conference endpoint returning full item data
4. List/single speaker endpoints
5. Topic listing and topic-filtered conference listing endpoints
6. Search endpoint using gather query infrastructure
7. Subscribe/unsubscribe endpoints for authenticated users
8. User data export endpoint at `/api/v1/user/export`
9. Paginated list responses use `{ data, total, page, per_page }` envelope
10. Single-resource responses use `{ data }` envelope
11. Error responses use `{ error, status }` envelope
12. `per_page` clamped to 1-100 range

## Tasks / Subtasks

- [x] Create v1 API router with versioning middleware (AC: #1)
- [x] Implement list_conferences with pagination and gather query integration (AC: #2, #9, #12)
- [x] Implement get_conference for single item retrieval (AC: #3, #10)
- [x] Implement list_speakers and get_speaker endpoints (AC: #4)
- [x] Implement list_topics and list_topic_conferences endpoints (AC: #5)
- [x] Implement search endpoint using gather queries (AC: #6)
- [x] Implement subscribe/unsubscribe endpoints (AC: #7)
- [x] Implement user data export endpoint (AC: #8)
- [x] Define ListEnvelope, DataEnvelope, ErrorEnvelope response types (AC: #9, #10, #11)
- [x] Add per_page clamping to 1-100 range (AC: #12)

## Dev Notes

### Architecture

The v1 API (`routes/api_v1.rs`, 758 lines) is a self-contained router with:

- **Versioning**: `inject_api_version` middleware adds `x-api-version: 1` to all responses. Applied as a router-level layer.
- **Envelope types**: Three response envelopes (`ListEnvelope<T>`, `DataEnvelope<T>`, `ErrorEnvelope`) standardize all API responses.
- **Gather integration**: List endpoints use the gather query infrastructure (`QueryContext` with `FilterValue` parameters) for consistent filtering and sorting. Constants define gather query IDs: `QUERY_UPCOMING_CONFERENCES`, `QUERY_CONFERENCES_BY_TOPIC`, `QUERY_SPEAKERS`.
- **Language awareness**: List endpoints accept `lang` parameter and read `ResolvedLanguage` from request extensions for multilingual content delivery.

Routes registered:
- GET `/api/v1/conferences` -- list with filters
- GET `/api/v1/conferences/{id}` -- single conference
- GET `/api/v1/topics` -- category listing
- GET `/api/v1/topics/{id}/conferences` -- conferences by topic
- GET `/api/v1/search` -- full-text search
- GET `/api/v1/speakers` -- speaker listing
- GET `/api/v1/speakers/{id}` -- single speaker
- POST/DELETE `/api/v1/conferences/{id}/subscribe` -- subscription management
- GET `/api/v1/user/export` -- GDPR user data export

### Testing

- API endpoints tested via integration tests
- Pagination boundary tested (page < 1, per_page clamping)
- Envelope format verified in response assertions

### References

- `crates/kernel/src/routes/api_v1.rs` (758 lines) -- Complete v1 API implementation
