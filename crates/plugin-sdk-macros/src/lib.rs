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
        // Extract the first parameter type
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
            // Read input JSON from memory
            let input_json = unsafe {
                let slice = core::slice::from_raw_parts(ptr as *const u8, len as usize);
                core::str::from_utf8_unchecked(slice)
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

    let expanded = quote! {
        #inner_fn

        #[doc(hidden)]
        #[unsafe(no_mangle)]
        #fn_vis extern "C" fn #fn_name(ptr: i32, len: i32) -> i64 {
            // Helper to write output and return ptr<<32|len
            fn write_output(s: &str) -> i64 {
                static mut OUTPUT_BUFFER: [u8; 65536] = [0u8; 65536];
                let bytes = s.as_bytes();
                let write_len = bytes.len().min(65536);
                unsafe {
                    OUTPUT_BUFFER[..write_len].copy_from_slice(&bytes[..write_len]);
                    let ptr = OUTPUT_BUFFER.as_ptr() as i64;
                    (ptr << 32) | (write_len as i64)
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
    };

    TokenStream::from(expanded)
}

/// Marks a function as a tap that returns a Result.
///
/// Similar to `#[plugin_tap]` but handles Result<T, E> return types,
/// encoding errors in the response.
#[proc_macro_attribute]
pub fn plugin_tap_result(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input_fn = parse_macro_input!(item as ItemFn);

    let fn_name = &input_fn.sig.ident;
    let inner_fn_name = format_ident!("__inner_{}", fn_name);

    let fn_vis = &input_fn.vis;
    let fn_block = &input_fn.block;
    let fn_output = &input_fn.sig.output;
    let fn_inputs = &input_fn.sig.inputs;

    let has_input = !fn_inputs.is_empty();

    let (inner_fn, wrapper_body) = if has_input {
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
            let input_json = unsafe {
                let slice = core::slice::from_raw_parts(ptr as *const u8, len as usize);
                core::str::from_utf8_unchecked(slice)
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

    let expanded = quote! {
        #inner_fn

        #[doc(hidden)]
        #[unsafe(no_mangle)]
        #fn_vis extern "C" fn #fn_name(ptr: i32, len: i32) -> i64 {
            fn write_output(s: &str, is_error: bool) -> i64 {
                static mut OUTPUT_BUFFER: [u8; 65536] = [0u8; 65536];
                let bytes = s.as_bytes();
                let write_len = bytes.len().min(65536);
                unsafe {
                    OUTPUT_BUFFER[..write_len].copy_from_slice(&bytes[..write_len]);
                    let ptr = OUTPUT_BUFFER.as_ptr() as i64;
                    let len = if is_error { -(write_len as i64) } else { write_len as i64 };
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
    };

    TokenStream::from(expanded)
}
