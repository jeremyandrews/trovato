-- Epic 2 e2e callee fixture: one migration-owned table so the derived WASM-2
-- db policy has a real allowlist. The integration test asserts that `callee_owned`
-- passes `DbPolicy::check_table` while an undeclared table is rejected with the
-- exact frozen `table-not-declared` message.
CREATE TABLE IF NOT EXISTS callee_owned (id bigint primary key);
