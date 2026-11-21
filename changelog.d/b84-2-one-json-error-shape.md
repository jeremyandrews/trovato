Every JSON error a kernel route writes is now one shape, the `AppError` body:
`code`, `message` and `request_id` (and `details` for a validation failure).
Each route keeps the status it answered with before.

There were five shapes. `AppError` was the canonical one, but the comment,
category, gather and item handlers answered with the deprecated `JsonError`
(`{"error"}`) at about seventy sites; the recovery, sessions, account deletion
and passkey routes each carried a private copy of it; `/api/v1` answered
`{"error","status"}`; the rate limiter wrote `{"error","retry_after"}` by hand;
and the CSRF check and the API token middleware wrote raw `{"error"}`
literals. The content-lock routes, the plugin gates, the bearer-token
middleware, the OAuth endpoints and the image style routes answered in plain
text. A script had to know which route it had called to read the reason, and
`static/js/chat-widget.js`, which read `err.error`, showed "Unknown error" for
every `AppError` failure.

`JsonError` and the local copies are gone; `AppError::with_status` keeps a
route's own status where no named constructor fits, deriving `code` from it.
`require_csrf_header` returns an `AppError`. The rate-limit `429` is an
`AppError::RateLimited`, still with `Retry-After`. The passkey, session and
recovery JSON endpoints, which used the HTML login guard and so redirected an
anonymous script to the login form, use the JSON guards and answer `401`.

A plugin's API route errors are `AppError` too, with an `error` key carrying
the same string as `message`, so plugin clients that read `error` keep working
until 2.0. The block editor image upload keeps Editor.js's `{"success": 0}`.

The kernel's scripts (`chat-widget.js`, `ai-assist.js`, `api-tokens.js`,
`file-upload.js`, `gather-builder.js`, `passkey-login.js`, and the recovery,
passkeys and sessions pages) read `message`.

`tests/error_shape_test.rs` asks one failing request of each router and
requires the `AppError` body; a unit test refuses a `JsonError` or a
`json!({"error": ...})` body anywhere under `routes/` or `middleware/`.

Contract: JSON API clients read `message` (and `code`) instead of `error`; see
`UPGRADING.md`. Plugin route errors are additive (`error` kept).
