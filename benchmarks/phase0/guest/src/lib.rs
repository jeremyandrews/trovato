//! Minimal test plugin for Phase 0 WASM benchmarks.
//!
//! This plugin exercises both data access modes:
//! - `tap_item_view`: handle-based (receives i32 handle, calls host functions)
//! - `tap_item_view_full`: full-serialization (receives JSON, parses, returns JSON)
//!
//! Compile with: `cargo build --target wasm32-wasip1 -p phase0-guest`

#![no_std]

extern crate alloc;

use alloc::string::String;
use core::slice;

// =============================================================================
// Memory Management
// =============================================================================

/// Global allocator for no_std WASM
#[global_allocator]
static ALLOC: wee_alloc::WeeAlloc = wee_alloc::WeeAlloc::INIT;

/// Allocate memory for the host to write into.
#[unsafe(no_mangle)]
pub extern "C" fn alloc(size: i32) -> i32 {
    let layout = core::alloc::Layout::from_size_align(size as usize, 1).unwrap();
    unsafe { alloc::alloc::alloc(layout) as i32 }
}

/// Free memory allocated by `alloc`.
#[unsafe(no_mangle)]
pub extern "C" fn dealloc(ptr: i32, size: i32) {
    let layout = core::alloc::Layout::from_size_align(size as usize, 1).unwrap();
    unsafe { alloc::alloc::dealloc(ptr as *mut u8, layout) }
}

// =============================================================================
// Host Function Imports (Handle-Based API)
// =============================================================================

#[link(wasm_import_module = "trovato:kernel/item-api")]
unsafe extern "C" {
    /// Get the title of an item. Returns length written to buf.
    #[link_name = "get_title"]
    fn host_get_title(handle: i32, buf_ptr: i32, buf_len: i32) -> i32;

    /// Get a string field value. Returns length written to buf, or -1 if not found.
    #[link_name = "get_field_string"]
    fn host_get_field_string(
        handle: i32,
        field_ptr: i32,
        field_len: i32,
        buf_ptr: i32,
        buf_len: i32,
    ) -> i32;

    /// Set a string field value.
    #[link_name = "set_field_string"]
    fn host_set_field_string(
        handle: i32,
        field_ptr: i32,
        field_len: i32,
        value_ptr: i32,
        value_len: i32,
    );
}

// =============================================================================
// Helper Functions
// =============================================================================

/// Static buffer for reading field values (4KB should be plenty for benchmarks).
/// Using raw pointer to avoid Rust 2024's static_mut_refs restrictions.
static mut READ_BUF: [u8; 4096] = [0u8; 4096];

/// Get the read buffer pointer and length.
/// SAFETY: This is single-threaded WASM, no data races possible.
#[inline]
fn read_buf_ptr_len() -> (i32, i32) {
    let ptr = unsafe { core::ptr::addr_of_mut!(READ_BUF) } as *mut u8;
    (ptr as i32, 4096)
}

/// Read from the read buffer as a slice.
/// SAFETY: This is single-threaded WASM, no data races possible.
#[inline]
unsafe fn read_buf_slice(len: usize) -> &'static [u8] {
    let ptr = core::ptr::addr_of!(READ_BUF) as *const u8;
    unsafe { slice::from_raw_parts(ptr, len) }
}

/// Read title using host function.
fn read_title(handle: i32) -> String {
    let (buf_ptr, buf_len) = read_buf_ptr_len();
    let len = unsafe { host_get_title(handle, buf_ptr, buf_len) };
    if len > 0 {
        unsafe { String::from_utf8_lossy(read_buf_slice(len as usize)).into_owned() }
    } else {
        String::new()
    }
}

/// Read a string field using host function.
fn read_field(handle: i32, field_name: &str) -> Option<String> {
    let (buf_ptr, buf_len) = read_buf_ptr_len();
    unsafe {
        let len = host_get_field_string(
            handle,
            field_name.as_ptr() as i32,
            field_name.len() as i32,
            buf_ptr,
            buf_len,
        );
        if len >= 0 {
            Some(String::from_utf8_lossy(read_buf_slice(len as usize)).into_owned())
        } else {
            None
        }
    }
}

/// Write a string field using host function.
fn write_field(handle: i32, field_name: &str, value: &str) {
    unsafe {
        host_set_field_string(
            handle,
            field_name.as_ptr() as i32,
            field_name.len() as i32,
            value.as_ptr() as i32,
            value.len() as i32,
        );
    }
}

// =============================================================================
// Tap Exports
// =============================================================================

/// Handle-based tap_item_view.
///
/// Reads 3 fields via host functions, writes 1 computed field,
/// returns a RenderElement JSON string.
#[unsafe(no_mangle)]
pub extern "C" fn tap_item_view(handle: i32) -> i64 {
    // Read 3 fields (the benchmark workload)
    let title = read_title(handle);
    let body = read_field(handle, "field_body").unwrap_or_default();
    let _summary = read_field(handle, "field_summary").unwrap_or_default();

    // Write 1 computed field
    let computed = alloc::format!("Processed: {}", title);
    write_field(handle, "field_computed", &computed);

    // Build RenderElement JSON (minimal, no serde)
    let json = alloc::format!(
        r#"{{"type":"container","children":[{{"type":"heading","level":1,"text":"{}"}},{{"type":"markup","value":"{}"}}]}}"#,
        escape_json(&title),
        escape_json(&body.chars().take(100).collect::<String>())
    );

    // Return pointer and length packed into i64
    let ptr = json.as_ptr() as i64;
    let len = json.len() as i64;
    core::mem::forget(json); // Don't deallocate - host will read it
    (ptr << 32) | len
}

/// Full-serialization tap_item_view_full.
///
/// Receives full item JSON, parses it (simulated), modifies a field,
/// returns a RenderElement JSON string.
#[unsafe(no_mangle)]
pub extern "C" fn tap_item_view_full(json_ptr: i32, json_len: i32) -> i64 {
    // Read the input JSON
    let json_str = unsafe {
        let json_bytes = slice::from_raw_parts(json_ptr as *const u8, json_len as usize);
        core::str::from_utf8(json_bytes).unwrap_or("")
    };

    // Simulate parsing: extract title (find "title":" pattern)
    let title = extract_json_string(json_str, "title").unwrap_or_default();
    let body = extract_json_string(json_str, "field_body").unwrap_or_default();
    let _summary = extract_json_string(json_str, "field_summary").unwrap_or_default();

    // Build RenderElement JSON with computed value
    let json = alloc::format!(
        r#"{{"type":"container","children":[{{"type":"heading","level":1,"text":"{}"}},{{"type":"markup","value":"{}"}},{{"field_computed":"Processed: {}"}}]}}"#,
        escape_json(&title),
        escape_json(&body.chars().take(100).collect::<String>()),
        escape_json(&title)
    );

    // Return pointer and length packed into i64
    let ptr = json.as_ptr() as i64;
    let len = json.len() as i64;
    core::mem::forget(json);
    (ptr << 32) | len
}

/// Minimal JSON string extraction (no serde dependency).
fn extract_json_string(json: &str, key: &str) -> Option<String> {
    let pattern = alloc::format!(r#""{}":"#, key);
    let start = json.find(&pattern)?;
    let after_key = &json[start + pattern.len()..];

    // Handle nested objects like {"value": "..."}
    if after_key.starts_with('{') {
        // Look for "value" inside
        let value_pattern = r#""value":""#;
        let value_start = after_key.find(value_pattern)?;
        let after_value = &after_key[value_start + value_pattern.len()..];
        let end = after_value.find('"')?;
        return Some(after_value[..end].into());
    }

    // Direct string value
    if after_key.starts_with('"') {
        let content = &after_key[1..];
        let end = content.find('"')?;
        return Some(content[..end].into());
    }

    None
}

/// Escape special characters for JSON strings.
fn escape_json(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => result.push_str(r#"\""#),
            '\\' => result.push_str(r#"\\"#),
            '\n' => result.push_str(r#"\n"#),
            '\r' => result.push_str(r#"\r"#),
            '\t' => result.push_str(r#"\t"#),
            _ => result.push(c),
        }
    }
    result
}

// =============================================================================
// Panic Handler (required for no_std)
// =============================================================================

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
