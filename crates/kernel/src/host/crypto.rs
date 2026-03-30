//! Cryptographic host functions for WASM plugins.
//!
//! Provides HMAC-SHA256, SHA-256 hashing, secure random bytes, and
//! constant-time comparison. Plugins use these instead of bundling
//! their own crypto libraries in WASM (which would be large, slow,
//! and potentially insecure).

use anyhow::Result;
use wasmtime::Linker;

use super::{read_string_from_memory, write_string_to_memory};
use crate::plugin::PluginState;

/// Register cryptographic host functions.
pub fn register_crypto_functions(linker: &mut Linker<PluginState>) -> Result<()> {
    // sha256(data_ptr, data_len, out_ptr, out_max_len) -> i32
    // Returns hex-encoded SHA-256 hash (64 chars), or -1 on error.
    linker.func_wrap(
        "trovato:kernel/crypto-api",
        "sha256",
        |mut caller: wasmtime::Caller<'_, PluginState>,
         data_ptr: i32,
         data_len: i32,
         out_ptr: i32,
         out_max_len: i32|
         -> i32 {
            let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                return -1;
            };
            let Ok(data) = read_string_from_memory(&memory, &caller, data_ptr, data_len) else {
                return -1;
            };

            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(data.as_bytes());
            let hash = hasher.finalize();
            let hex = hex::encode(hash);

            write_string_to_memory(&memory, &mut caller, out_ptr, out_max_len, &hex).unwrap_or(-1)
        },
    )?;

    // hmac_sha256(key_ptr, key_len, msg_ptr, msg_len, out_ptr, out_max_len) -> i32
    // Returns hex-encoded HMAC-SHA256 (64 chars), or -1 on error.
    linker.func_wrap(
        "trovato:kernel/crypto-api",
        "hmac_sha256",
        |mut caller: wasmtime::Caller<'_, PluginState>,
         key_ptr: i32,
         key_len: i32,
         msg_ptr: i32,
         msg_len: i32,
         out_ptr: i32,
         out_max_len: i32|
         -> i32 {
            let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                return -1;
            };
            let Ok(key) = read_string_from_memory(&memory, &caller, key_ptr, key_len) else {
                return -1;
            };
            let Ok(msg) = read_string_from_memory(&memory, &caller, msg_ptr, msg_len) else {
                return -1;
            };

            use hmac::{Hmac, Mac};
            use sha2::Sha256;
            type HmacSha256 = Hmac<Sha256>;

            let Ok(mut mac) = HmacSha256::new_from_slice(key.as_bytes()) else {
                return -1;
            };
            mac.update(msg.as_bytes());
            let result = mac.finalize();
            let hex = hex::encode(result.into_bytes());

            write_string_to_memory(&memory, &mut caller, out_ptr, out_max_len, &hex).unwrap_or(-1)
        },
    )?;

    // random_bytes(len, out_ptr, out_max_len) -> i32
    // Returns hex-encoded random bytes, or -1 on error.
    linker.func_wrap(
        "trovato:kernel/crypto-api",
        "random_bytes",
        |mut caller: wasmtime::Caller<'_, PluginState>,
         len: i32,
         out_ptr: i32,
         out_max_len: i32|
         -> i32 {
            if len <= 0 || len > 256 {
                return -1;
            }
            let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                return -1;
            };

            use rand::RngCore;
            let mut buf = vec![0u8; len as usize];
            rand::thread_rng().fill_bytes(&mut buf);
            let hex = hex::encode(&buf);

            write_string_to_memory(&memory, &mut caller, out_ptr, out_max_len, &hex).unwrap_or(-1)
        },
    )?;

    // constant_time_eq(a_ptr, a_len, b_ptr, b_len) -> i32
    // Returns 1 if equal, 0 if not. Constant-time to prevent timing attacks.
    linker.func_wrap(
        "trovato:kernel/crypto-api",
        "constant_time_eq",
        |mut caller: wasmtime::Caller<'_, PluginState>,
         a_ptr: i32,
         a_len: i32,
         b_ptr: i32,
         b_len: i32|
         -> i32 {
            let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                return 0;
            };
            let Ok(a) = read_string_from_memory(&memory, &caller, a_ptr, a_len) else {
                return 0;
            };
            let Ok(b) = read_string_from_memory(&memory, &caller, b_ptr, b_len) else {
                return 0;
            };

            use subtle::ConstantTimeEq;
            let equal = a.as_bytes().ct_eq(b.as_bytes());
            if bool::from(equal) { 1 } else { 0 }
        },
    )?;

    Ok(())
}
