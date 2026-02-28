//! Proc macros for Trovato plugin SDK.
//!
//! Provides `#[plugin_tap]` attribute macro that generates WASM export
//! wrappers with JSON serialization/deserialization.

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{FnArg, ItemFn, PatType, parse_macro_input};

/// Marks a function as a tap implementation.
///
/// Generates a WASM export wrapper that:
/// 1. Reads JSON input from WASM memory (ptr, len)
/// 2. Deserializes to the function's input type (if any)
/// 3. Calls the user's function
/// 4. Serializes the result to JSON
/// 5. Returns ptr<<32|len encoding
///
/// # Example
///
/// ```ignore
/// #[plugin_tap]
/// fn tap_item_info() -> Vec<ContentTypeDefinition> {
///     vec![ContentTypeDefinition { ... }]
/// }
/// ```
#[proc_macro_attribute]
pub fn plugin_tap(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input_fn = parse_macro_input!(item as ItemFn);
    expand_tap(input_fn).into()
}

/// Marks a function as a tap that returns a Result.
///
/// Similar to `#[plugin_tap]` but handles Result<T, E> return types,
/// encoding errors in the response.
#[proc_macro_attribute]
pub fn plugin_tap_result(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input_fn = parse_macro_input!(item as ItemFn);
    expand_tap_result(input_fn).into()
}

fn expand_tap(input_fn: ItemFn) -> proc_macro2::TokenStream {
    let fn_name = &input_fn.sig.ident;
    let inner_fn_name = format_ident!("__inner_{}", fn_name);

    let fn_vis = &input_fn.vis;
    let fn_block = &input_fn.block;
    let fn_output = &input_fn.sig.output;
    let fn_inputs = &input_fn.sig.inputs;

    // Determine if the function takes an input parameter
    let has_input = !fn_inputs.is_empty();

    // Generate different code based on whether there's an input parameter
    let (inner_fn, wrapper_body) = if has_input {
        // Infallible: has_input is true only when fn_inputs is non-empty
        #[allow(clippy::unwrap_used)]
        let first_param = fn_inputs.first().unwrap();
        let FnArg::Typed(PatType {
            pat: param_name,
            ty: param_type,
            ..
        }) = first_param
        else {
            panic!("plugin_tap functions cannot have self parameters");
        };

        let inner = quote! {
            #[inline]
            fn #inner_fn_name(#param_name: #param_type) #fn_output #fn_block
        };

        let wrapper = quote! {
            // Validate host-provided pointer and length before unsafe access
            if ptr < 0 || len < 0 {
                trovato_sdk::host::log("error", "sdk", "negative ptr or len from host");
                return write_output("{\"error\": \"invalid input pointer\"}");
            }

            let input_json = {
                // SAFETY: ptr and len validated non-negative above. The host provides
                // a (ptr, len) pair into WASM linear memory, which remains stable for
                // the duration of this exported function call.
                let slice = unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) };
                match core::str::from_utf8(slice) {
                    Ok(s) => s,
                    Err(_) => {
                        trovato_sdk::host::log("error", "sdk", "invalid UTF-8 input from host");
                        return write_output("{\"error\": \"invalid UTF-8 input\"}");
                    }
                }
            };

            // Deserialize input
            let input: #param_type = match trovato_sdk::serde_json::from_str(input_json) {
                Ok(v) => v,
                Err(e) => {
                    let error = format!("{{\"error\": \"deserialize: {}\"}}", e);
                    return write_output(&error);
                }
            };

            // Call inner function with input
            let result = #inner_fn_name(input);
        };

        (inner, wrapper)
    } else {
        let inner = quote! {
            #[inline]
            fn #inner_fn_name() #fn_output #fn_block
        };

        let wrapper = quote! {
            // No input needed, just call the function
            let _ = (ptr, len); // Silence unused warnings
            let result = #inner_fn_name();
        };

        (inner, wrapper)
    };

    quote! {
        #inner_fn

        #[doc(hidden)]
        #[unsafe(no_mangle)]
        #fn_vis extern "C" fn #fn_name(ptr: i32, len: i32) -> i64 {
            // Helper to write output and return ptr<<32|len.
            // Errors (UTF-8, deserialization, overflow) are returned through the
            // normal encoding — the host detects them by parsing for {"error": ...}.
            fn write_output(s: &str) -> i64 {
                static mut OUTPUT_BUFFER: [u8; 65536] = [0u8; 65536];
                let bytes = s.as_bytes();
                if bytes.len() > 65536 {
                    let error = b"{\"error\": \"output exceeds 64KB buffer\"}";
                    // SAFETY: WASM is single-threaded; no concurrent access.
                    // The host reads this buffer synchronously before the next call.
                    unsafe {
                        OUTPUT_BUFFER[..error.len()].copy_from_slice(error);
                        let ptr = OUTPUT_BUFFER.as_ptr() as i64;
                        return (ptr << 32) | (error.len() as i64);
                    }
                }
                // SAFETY: WASM is single-threaded; no concurrent access.
                // The host reads this buffer synchronously before the next call.
                unsafe {
                    OUTPUT_BUFFER[..bytes.len()].copy_from_slice(bytes);
                    let ptr = OUTPUT_BUFFER.as_ptr() as i64;
                    (ptr << 32) | (bytes.len() as i64)
                }
            }

            #wrapper_body

            // Serialize result to JSON
            let output = match trovato_sdk::serde_json::to_string(&result) {
                Ok(json) => json,
                Err(e) => format!("{{\"error\": \"serialize: {}\"}}", e),
            };

            write_output(&output)
        }
    }
}

fn expand_tap_result(input_fn: ItemFn) -> proc_macro2::TokenStream {
    let fn_name = &input_fn.sig.ident;
    let inner_fn_name = format_ident!("__inner_{}", fn_name);

    let fn_vis = &input_fn.vis;
    let fn_block = &input_fn.block;
    let fn_output = &input_fn.sig.output;
    let fn_inputs = &input_fn.sig.inputs;

    let has_input = !fn_inputs.is_empty();

    let (inner_fn, wrapper_body) = if has_input {
        // Infallible: has_input is true only when fn_inputs is non-empty
        #[allow(clippy::unwrap_used)]
        let first_param = fn_inputs.first().unwrap();
        let FnArg::Typed(PatType {
            pat: param_name,
            ty: param_type,
            ..
        }) = first_param
        else {
            panic!("plugin_tap functions cannot have self parameters");
        };

        let inner = quote! {
            #[inline]
            fn #inner_fn_name(#param_name: #param_type) #fn_output #fn_block
        };

        let wrapper = quote! {
            // Validate host-provided pointer and length before unsafe access
            if ptr < 0 || len < 0 {
                trovato_sdk::host::log("error", "sdk", "negative ptr or len from host");
                return write_output("{\"error\": \"invalid input pointer\"}", true);
            }

            let input_json = {
                // SAFETY: ptr and len validated non-negative above. The host provides
                // a (ptr, len) pair into WASM linear memory, which remains stable for
                // the duration of this exported function call.
                let slice = unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) };
                match core::str::from_utf8(slice) {
                    Ok(s) => s,
                    Err(_) => {
                        trovato_sdk::host::log("error", "sdk", "invalid UTF-8 input from host");
                        return write_output("{\"error\": \"invalid UTF-8 input\"}", true);
                    }
                }
            };

            let input: #param_type = match trovato_sdk::serde_json::from_str(input_json) {
                Ok(v) => v,
                Err(e) => {
                    let error = format!("{{\"error\": \"deserialize: {}\"}}", e);
                    return write_output(&error, true);
                }
            };

            let result = #inner_fn_name(input);
        };

        (inner, wrapper)
    } else {
        let inner = quote! {
            #[inline]
            fn #inner_fn_name() #fn_output #fn_block
        };

        let wrapper = quote! {
            let _ = (ptr, len);
            let result = #inner_fn_name();
        };

        (inner, wrapper)
    };

    quote! {
        #inner_fn

        #[doc(hidden)]
        #[unsafe(no_mangle)]
        #fn_vis extern "C" fn #fn_name(ptr: i32, len: i32) -> i64 {
            // Errors are signaled via negative length encoding (is_error = true).
            fn write_output(s: &str, is_error: bool) -> i64 {
                static mut OUTPUT_BUFFER: [u8; 65536] = [0u8; 65536];
                let bytes = s.as_bytes();
                if bytes.len() > 65536 {
                    let error = b"{\"error\": \"output exceeds 64KB buffer\"}";
                    // SAFETY: WASM is single-threaded; no concurrent access.
                    // The host reads this buffer synchronously before the next call.
                    unsafe {
                        OUTPUT_BUFFER[..error.len()].copy_from_slice(error);
                        let ptr = OUTPUT_BUFFER.as_ptr() as i64;
                        let len = -(error.len() as i64);
                        return (ptr << 32) | (len & 0xFFFFFFFF);
                    }
                }
                // SAFETY: WASM is single-threaded; no concurrent access.
                // The host reads this buffer synchronously before the next call.
                unsafe {
                    OUTPUT_BUFFER[..bytes.len()].copy_from_slice(bytes);
                    let ptr = OUTPUT_BUFFER.as_ptr() as i64;
                    let len = if is_error { -(bytes.len() as i64) } else { bytes.len() as i64 };
                    (ptr << 32) | (len & 0xFFFFFFFF)
                }
            }

            #wrapper_body

            match result {
                Ok(value) => {
                    let output = match trovato_sdk::serde_json::to_string(&value) {
                        Ok(json) => json,
                        Err(e) => return write_output(&format!("{{\"error\": \"serialize: {}\"}}", e), true),
                    };
                    write_output(&output, false)
                }
                Err(e) => {
                    let error = format!("{{\"error\": \"{}\"}}", e);
                    write_output(&error, true)
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn expand_tap_str(input: &str) -> String {
        let input_fn: ItemFn = syn::parse_str(input).expect("valid Rust function");
        expand_tap(input_fn).to_string()
    }

    fn expand_tap_result_str(input: &str) -> String {
        let input_fn: ItemFn = syn::parse_str(input).expect("valid Rust function");
        expand_tap_result(input_fn).to_string()
    }

    #[test]
    fn tap_generates_safe_utf8_validation() {
        let output = expand_tap_str("fn my_tap(input: String) -> String { input }");
        assert!(output.contains("from_utf8"), "should use safe from_utf8");
        assert!(
            !output.contains("from_utf8_unchecked"),
            "should not use unsafe from_utf8_unchecked"
        );
        assert!(
            output.contains("invalid UTF-8 input"),
            "should return error on invalid UTF-8"
        );
    }

    #[test]
    fn tap_generates_ptr_len_validation() {
        let output = expand_tap_str("fn my_tap(input: String) -> String { input }");
        assert!(
            output.contains("ptr < 0"),
            "should validate ptr is non-negative"
        );
        assert!(
            output.contains("len < 0"),
            "should validate len is non-negative"
        );
    }

    #[test]
    fn tap_generates_overflow_check() {
        let output = expand_tap_str("fn my_tap(input: String) -> String { input }");
        assert!(
            output.contains("output exceeds 64KB"),
            "should check for buffer overflow"
        );
    }

    #[test]
    fn tap_result_generates_safe_utf8_validation() {
        let output = expand_tap_result_str(
            "fn my_tap(input: String) -> Result<String, String> { Ok(input) }",
        );
        assert!(output.contains("from_utf8"), "should use safe from_utf8");
        assert!(
            !output.contains("from_utf8_unchecked"),
            "should not use unsafe from_utf8_unchecked"
        );
        assert!(
            output.contains("invalid UTF-8 input"),
            "should return error on invalid UTF-8"
        );
    }

    #[test]
    fn tap_result_generates_ptr_len_validation() {
        let output = expand_tap_result_str(
            "fn my_tap(input: String) -> Result<String, String> { Ok(input) }",
        );
        assert!(
            output.contains("ptr < 0"),
            "should validate ptr is non-negative"
        );
    }

    #[test]
    fn tap_no_input_skips_input_validation() {
        let output = expand_tap_str("fn my_tap() -> String { String::new() }");
        assert!(
            !output.contains("from_utf8"),
            "no-input tap should not validate UTF-8"
        );
        assert!(
            !output.contains("ptr < 0"),
            "no-input tap should not validate ptr"
        );
    }
}
