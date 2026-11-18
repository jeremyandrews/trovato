//! `push-to`, declared by hand: the SDK has no binding for it yet.

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "trovato:kernel/queue")]
unsafe extern "C" {
    #[link_name = "push-to"]
    fn __push_to(
        owner_ptr: i32,
        owner_len: i32,
        queue_ptr: i32,
        queue_len: i32,
        payload_ptr: i32,
        payload_len: i32,
    ) -> i32;
}

/// Push `payload` to `owner`'s `queue`; `Err` carries the host's code.
#[cfg(target_arch = "wasm32")]
pub fn push_to(owner: &str, queue: &str, payload: &serde_json::Value) -> Result<(), i32> {
    let payload = payload.to_string();
    let code = unsafe {
        __push_to(
            owner.as_ptr() as i32,
            owner.len() as i32,
            queue.as_ptr() as i32,
            queue.len() as i32,
            payload.as_ptr() as i32,
            payload.len() as i32,
        )
    };
    if code < 0 { Err(code) } else { Ok(()) }
}

/// Native builds have no host.
#[cfg(not(target_arch = "wasm32"))]
pub fn push_to(_owner: &str, _queue: &str, _payload: &serde_json::Value) -> Result<(), i32> {
    Ok(())
}
