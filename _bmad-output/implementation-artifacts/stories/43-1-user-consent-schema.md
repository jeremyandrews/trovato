# Story 43.1: User Consent Schema

Status: ready-for-dev

## Story

As a **GDPR compliance plugin developer**,
I want consent metadata stored on the user record,
so that I can track when users consented and to which privacy policy version.

## Acceptance Criteria

1. Migration adds columns to `users` table: `consent_given` (BOOLEAN, DEFAULT FALSE), `consent_date` (TIMESTAMPTZ, NULLABLE), `consent_version` (VARCHAR(64), NULLABLE), `data_retention_days` (INTEGER, NULLABLE)
2. Existing users get `consent_given = false`, `consent_date = NULL`, `consent_version = NULL`, `data_retention_days = NULL`
3. `User` model struct in kernel includes consent fields
4. User admin form (`/admin/people/{id}/edit`) displays consent fields as read-only info (not editable by admins -- consent is user-initiated)
5. `User` serialization to plugins includes consent fields (plugins can read but not directly write -- consent changes go through a kernel service method)
6. Kernel `UserService` gains `record_consent(user_id, version)` method that sets `consent_given = true`, `consent_date = NOW()`, `consent_version = version`
7. Kernel `UserService` gains `withdraw_consent(user_id)` method that sets `consent_given = false` (preserves `consent_date` and `consent_version` for audit trail)
8. At least 2 integration tests: record consent, withdraw consent

## Tasks / Subtasks

- [ ] Write migration SQL adding consent columns to `users` table (AC: #1, #2)
- [ ] Add consent fields to `User` model struct with serde attributes (AC: #3)
- [ ] Update `User` query methods to select and populate consent fields (AC: #3)
- [ ] Add `record_consent(user_id, version)` method to `UserService` (AC: #6)
- [ ] Add `withdraw_consent(user_id)` method to `UserService` (AC: #7)
- [ ] Update user admin edit form template to display consent fields as read-only (AC: #4)
- [ ] Verify `User` serialization to plugins includes consent fields (AC: #5)
- [ ] Write integration test: record consent sets fields correctly (AC: #8)
- [ ] Write integration test: withdraw consent preserves audit trail (AC: #8)

## Dev Notes

### Architecture

- Migration: `ALTER TABLE users ADD COLUMN consent_given BOOLEAN DEFAULT FALSE, ADD COLUMN consent_date TIMESTAMPTZ, ADD COLUMN consent_version VARCHAR(64), ADD COLUMN data_retention_days INTEGER`
- `DEFAULT FALSE` on `consent_given` means existing rows automatically get `false` -- no backfill needed
- Consent *collection* (the UI that asks for consent) is plugin territory -- the kernel only stores the result
- `data_retention_days` is per-user (how long to retain a specific user's data after deletion request), distinct from per-item retention in Story 43.4

### Security

- Consent changes must go through `UserService` methods, not direct SQL -- enforces audit trail
- `withdraw_consent` preserves `consent_date` and `consent_version` so the system retains proof that consent was once given (GDPR audit requirement)
- Admin form shows consent as read-only -- admins cannot fabricate consent on behalf of users

### Testing

- Integration tests in `crates/kernel/tests/` using shared test infrastructure (`run_test`, `shared_app`)
- Test `record_consent`: create user, call `record_consent`, verify all four fields
- Test `withdraw_consent`: record consent first, then withdraw, verify `consent_given = false` while date/version preserved

### References

- [Source: docs/ritrovo/epic-13-privacy.md -- Story 43.1]
- [Source: crates/kernel/src/models/user.rs -- User model]
- [Source: crates/kernel/src/services/user.rs -- UserService]
