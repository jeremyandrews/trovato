# Story 42.7: Crypto Host Functions for Plugins

Status: ready-for-dev

## Story

As a plugin developer implementing HMAC signing or token generation,
I want crypto primitives available as host functions,
so that I can perform cryptographic operations without bundling crypto libraries into my WASM module.

## Acceptance Criteria

1. `crypto_hmac_sha256(key, message)` host function returns hex-encoded HMAC-SHA256 digest
2. `crypto_sha256(data)` host function returns hex-encoded SHA-256 hash
3. `crypto_random_bytes(len)` host function returns `len` cryptographically random bytes
4. `crypto_constant_time_eq(a, b)` host function returns boolean result of constant-time comparison
5. All functions documented in the Plugin SDK with usage examples
6. Implementation uses `ring` or `hmac`/`sha2` crate (not hand-rolled crypto)
7. Efficient WASM byte array handling (linear memory read/write, not serialization overhead)
8. At least 2 integration tests covering HMAC and random byte generation

## Tasks / Subtasks

- [ ] Create `crates/kernel/src/host/crypto.rs` with all four host functions (AC: #1, #2, #3, #4)
- [ ] Implement `crypto_hmac_sha256` using `ring` or `hmac` crate (AC: #1, #6)
- [ ] Implement `crypto_sha256` using `ring` or `sha2` crate (AC: #2, #6)
- [ ] Implement `crypto_random_bytes` using `ring::rand` or `getrandom` (AC: #3, #6)
- [ ] Implement `crypto_constant_time_eq` using `ring::constant_time` or `subtle` crate (AC: #4, #6)
- [ ] Register all four functions in the WASM host function registry (AC: #1, #2, #3, #4)
- [ ] Implement efficient WASM linear memory byte passing (AC: #7)
- [ ] Add error constants to `crates/plugin-sdk/src/host_errors.rs` for crypto failures (AC: #1, #2, #3, #4)
- [ ] Add SDK documentation with usage examples for each function (AC: #5)
- [ ] Write integration test: HMAC-SHA256 produces correct digest for known input (AC: #1, #8)
- [ ] Write integration test: random bytes returns correct length and is non-deterministic (AC: #3, #8)
- [ ] Write integration test: constant-time eq returns correct results (AC: #4)

## Dev Notes

### Architecture

Host functions follow the existing pattern in `crates/kernel/src/host/`. Each function reads input from WASM linear memory, performs the crypto operation on the host side, and writes the result back. For byte arrays, use the established `(ptr, len)` pair convention. For `crypto_random_bytes`, enforce a maximum length (e.g., 1024 bytes) to prevent resource abuse.

The `ring` crate is preferred if already in the dependency tree; otherwise `hmac` + `sha2` + `subtle` are lighter alternatives. Check existing `Cargo.toml` dependencies before adding new crates.

### Security

- Crypto operations run on the host, not in the WASM sandbox. This is intentional -- WASM plugins should not bundle their own crypto (larger modules, potential for weak implementations).
- `crypto_constant_time_eq` must use a constant-time comparison to prevent timing side-channels. Do not use `==` on byte slices.
- `crypto_random_bytes` must use a CSPRNG (not `rand::thread_rng()`). `ring::rand::SystemRandom` or `getrandom` crate are appropriate.
- Maximum length on `crypto_random_bytes` prevents a plugin from requesting gigabytes of random data.
- Error constants follow the pattern in `crates/plugin-sdk/src/host_errors.rs` per CLAUDE.md rules.

### Testing

- HMAC test: compute `HMAC-SHA256("key", "message")` and compare against a known-good value from a reference implementation (e.g., RFC 4231 test vectors).
- Random bytes test: request 32 bytes, verify length is 32, request again, verify the two results differ (probabilistic but effectively certain for 32 bytes).
- Constant-time eq test: equal inputs return true, differing inputs return false, different-length inputs return false.

### References

- `crates/kernel/src/host/` -- existing host function modules
- `crates/plugin-sdk/src/host_errors.rs` -- error constants
- `docs/ritrovo/epic-12-security.md` -- Epic 42 source
