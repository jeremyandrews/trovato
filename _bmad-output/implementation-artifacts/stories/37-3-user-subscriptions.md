# Story 37.3: User Subscriptions

Status: done

## Story

As an **authenticated user**,
I want to subscribe to conferences I am interested in,
so that I can receive notifications about new comments and updates on those conferences.

## Acceptance Criteria

1. Subscription model with user_id, item_id, and created timestamp
2. `subscribe()` creates a subscription (no-op if already subscribed, via ON CONFLICT DO NOTHING)
3. `unsubscribe()` removes a subscription and returns whether one existed
4. `is_subscribed()` checks subscription status for a user/item pair
5. `list_subscribers()` returns all subscriber user IDs for an item
6. `list_user_subscriptions()` returns all subscriptions for a user, most recent first
7. REST API endpoints at `/api/v1/conferences/{id}/subscribe` (POST to subscribe, DELETE to unsubscribe)

## Tasks / Subtasks

- [x] Define Subscription model with user_id, item_id, created fields (AC: #1)
- [x] Implement Subscription::subscribe() with ON CONFLICT DO NOTHING (AC: #2)
- [x] Implement Subscription::unsubscribe() returning bool (AC: #3)
- [x] Implement Subscription::is_subscribed() existence check (AC: #4)
- [x] Implement Subscription::list_subscribers() ordered by created (AC: #5)
- [x] Implement Subscription::list_user_subscriptions() ordered by created DESC (AC: #6)
- [x] Add subscribe/unsubscribe endpoints to v1 API router (AC: #7)
- [x] Create user_subscriptions table migration

## Dev Notes

### Architecture

The Subscription model (`models/subscription.rs`, 92 lines) is a thin wrapper around the `user_subscriptions` table. All methods are static async functions taking `&PgPool`. The idempotent `subscribe()` uses `ON CONFLICT DO NOTHING` so callers do not need to check existing state.

REST endpoints are registered in the v1 API router (`routes/api_v1.rs`):
- POST `/api/v1/conferences/{id}/subscribe` -- requires authenticated session
- DELETE `/api/v1/conferences/{id}/subscribe` -- requires authenticated session

The subscription model is intentionally generic (user_id + item_id) rather than conference-specific, enabling future subscription to any item type.

### Testing

- Subscription CRUD tested via model-level integration tests
- Subscribe/unsubscribe API endpoints tested via REST API calls

### References

- `crates/kernel/src/models/subscription.rs` (92 lines) -- Subscription model with full CRUD
- `crates/kernel/src/routes/api_v1.rs` -- subscribe/unsubscribe API endpoints
