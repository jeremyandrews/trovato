//! User host functions for WASM plugins.
//!
//! Provides access to current user information and permission checks.

use anyhow::Result;
use wasmtime::Linker;

use super::{read_string_from_memory, write_string_to_memory};
use crate::plugin::PluginState;

/// Register user host functions.
pub fn register_user_functions(linker: &mut Linker<PluginState>) -> Result<()> {
    // current_user_id() -> string
    linker.func_wrap(
        "trovato:kernel/user-api",
        "current-user-id",
        |mut caller: wasmtime::Caller<'_, PluginState>, out_ptr: i32, out_max_len: i32| -> i32 {
            let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                return 0;
            };

            let user_id = caller.data().request.user_id_string();

            write_string_to_memory(&memory, &mut caller, out_ptr, out_max_len, &user_id)
                .unwrap_or(0)
        },
    )?;

    // current_user_has_permission(permission) -> bool
    linker.func_wrap(
        "trovato:kernel/user-api",
        "current-user-has-permission",
        |mut caller: wasmtime::Caller<'_, PluginState>, perm_ptr: i32, perm_len: i32| -> i32 {
            let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                return 0;
            };

            let Ok(permission) = read_string_from_memory(&memory, &caller, perm_ptr, perm_len)
            else {
                return 0;
            };

            let has_perm = caller.data().request.user.has_permission(&permission);
            if has_perm { 1 } else { 0 }
        },
    )?;

    Ok(())
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use wasmtime::Engine;

    #[test]
    fn register_user_succeeds() {
        let config = wasmtime::Config::new();
        let engine = Engine::new(&config).unwrap();
        let mut linker: Linker<PluginState> = Linker::new(&engine);

        let result = register_user_functions(&mut linker);
        assert!(result.is_ok());
    }
}
