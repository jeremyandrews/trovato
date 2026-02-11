//! Stub host functions for Phase 0 benchmarks.
//!
//! These simulate the Kernel-side host functions that plugins call
//! across the WASM boundary. For benchmarking purposes they return
//! canned data without hitting a real database.

use std::collections::HashMap;

/// Simulated host state for a single request.
pub struct StubHostState {
    /// Canned item data, keyed by handle.
    pub items: HashMap<i32, serde_json::Value>,
    /// Canned permission results.
    pub permissions: HashMap<String, bool>,
    /// Canned variables.
    pub variables: HashMap<String, String>,
    /// Log output buffer.
    pub log_buffer: Vec<String>,
}

impl StubHostState {
    pub fn new() -> Self {
        Self {
            items: HashMap::new(),
            permissions: HashMap::new(),
            variables: HashMap::new(),
            log_buffer: Vec::new(),
        }
    }

    /// Load a fixture item at the given handle index.
    pub fn load_item(&mut self, handle: i32, item: serde_json::Value) {
        self.items.insert(handle, item);
    }

    /// Stub: get a field value from the item at the given handle.
    pub fn get_field_string(&self, handle: i32, field_name: &str) -> Option<String> {
        self.items.get(&handle)
            .and_then(|item| item.get("fields"))
            .and_then(|fields| fields.get(field_name))
            .and_then(|field| {
                // Handle both {"value": "..."} and direct string
                field.get("value")
                    .and_then(|v| v.as_str())
                    .or_else(|| field.as_str())
                    .map(|s| s.to_string())
            })
    }

    /// Stub: get the title from the item at the given handle.
    pub fn get_title(&self, handle: i32) -> Option<String> {
        self.items.get(&handle)
            .and_then(|item| item.get("title"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }

    /// Stub: set a field value on the item at the given handle.
    pub fn set_field_string(&mut self, handle: i32, field_name: &str, value: &str) {
        if let Some(item) = self.items.get_mut(&handle) {
            if let Some(fields) = item.get_mut("fields") {
                fields[field_name] = serde_json::json!({ "value": value });
            }
        }
    }

    /// Stub: check if the current user has a permission.
    pub fn user_has_permission(&self, permission: &str) -> bool {
        self.permissions.get(permission).copied().unwrap_or(false)
    }

    /// Stub: get a variable value.
    pub fn get_variable(&self, name: &str, default: &str) -> String {
        self.variables.get(name).cloned().unwrap_or_else(|| default.to_string())
    }

    /// Stub: log a message.
    pub fn log_message(&mut self, level: &str, plugin: &str, message: &str) {
        self.log_buffer.push(format!("[{level}] {plugin}: {message}"));
    }

    /// Stub: simulate a database query (returns canned JSON).
    pub fn db_query(&self, _query_json: &str) -> Result<String, String> {
        Ok(serde_json::json!([]).to_string())
    }
}
