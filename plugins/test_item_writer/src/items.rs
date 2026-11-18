//! The four `item-api` calls, bound by hand.
//!
//! The SDK binds none of them yet (BL-19), so this fixture carries the same
//! hand-written imports Argus and Netgrasp do. The signatures are the ones the
//! SDK bindings will have, so switching to them is a one-line change here.

use serde_json::Value;
use trovato_sdk::types::Item;

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "trovato:kernel/item-api")]
unsafe extern "C" {
    #[link_name = "get-item"]
    fn __get_item(id_ptr: i32, id_len: i32, out_ptr: i32, out_max_len: i32) -> i32;
    #[link_name = "save-item"]
    fn __save_item(item_ptr: i32, item_len: i32, out_ptr: i32, out_max_len: i32) -> i32;
    #[link_name = "delete-item"]
    fn __delete_item(id_ptr: i32, id_len: i32) -> i32;
    #[link_name = "query-items"]
    fn __query_items(query_ptr: i32, query_len: i32, out_ptr: i32, out_max_len: i32) -> i32;
}

#[cfg(target_arch = "wasm32")]
fn call_out(f: impl FnOnce(i32, i32) -> i32) -> Result<String, i32> {
    let mut buf = vec![0u8; 256 * 1024];
    let n = f(buf.as_mut_ptr() as i32, buf.len() as i32);
    if n < 0 {
        return Err(n);
    }
    buf.truncate(n as usize);
    String::from_utf8(buf).map_err(|_| -101)
}

#[cfg(target_arch = "wasm32")]
pub fn get_item(id: &str) -> Result<Option<Item>, i32> {
    let out = call_out(|p, l| unsafe { __get_item(id.as_ptr() as i32, id.len() as i32, p, l) })?;
    serde_json::from_str(&out).map_err(|_| -102)
}

#[cfg(target_arch = "wasm32")]
pub fn save_item(item: &Value) -> Result<Option<Item>, i32> {
    let json = item.to_string();
    let out =
        call_out(|p, l| unsafe { __save_item(json.as_ptr() as i32, json.len() as i32, p, l) })?;
    serde_json::from_str(&out).map_err(|_| -102)
}

#[cfg(target_arch = "wasm32")]
pub fn delete_item(id: &str) -> Result<(), i32> {
    let code = unsafe { __delete_item(id.as_ptr() as i32, id.len() as i32) };
    if code < 0 { Err(code) } else { Ok(()) }
}

#[cfg(target_arch = "wasm32")]
pub fn query_items(query: &Value) -> Result<Vec<Item>, i32> {
    let json = query.to_string();
    let out =
        call_out(|p, l| unsafe { __query_items(json.as_ptr() as i32, json.len() as i32, p, l) })?;
    serde_json::from_str(&out).map_err(|_| -102)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn get_item(_id: &str) -> Result<Option<Item>, i32> {
    Err(-1)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn save_item(_item: &Value) -> Result<Option<Item>, i32> {
    Err(-1)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn delete_item(_id: &str) -> Result<(), i32> {
    Err(-1)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn query_items(_query: &Value) -> Result<Vec<Item>, i32> {
    Err(-1)
}
