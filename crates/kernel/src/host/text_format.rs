//! Text format host function: filter text through a text format.
//!
//! A plugin that renders HTML a user wrote (a comment, a profile field, a
//! submission) had no way to filter it: the kernel's `FilterPipeline` was
//! reachable from kernel code only, so a plugin either escaped everything or
//! returned the HTML as written. `filter-text` runs the same pipeline the
//! kernel renders a field with, for a format the user the plugin is acting for
//! may use, under the same rule the item forms apply.

use anyhow::Result;
use wasmtime::Linker;

use super::trace::HostCallGuard;
use super::{read_string_from_memory, write_string_to_memory};
use crate::content::FilterPipeline;
use crate::content::submission::permitted_text_formats;
use crate::plugin::{PluginState, WasmtimeExt};
use crate::tap::UserContext;
use trovato_sdk::host_errors;

/// Filter `text` through `format` for `user`, or the error code a plugin gets.
///
/// The format must be stored (`content::text_format`) ([`host_errors::ERR_TEXT_FORMAT_UNKNOWN`]) and be one
/// `user` may write ([`host_errors::ERR_TEXT_FORMAT_NOT_PERMITTED`]); the
/// check is [`permitted_text_formats`], the one the item forms use.
pub fn filter_for(user: &UserContext, text: &str, format: &str) -> Result<String, i32> {
    if crate::content::text_format::get(format).is_none() {
        return Err(host_errors::ERR_TEXT_FORMAT_UNKNOWN);
    }
    if !permitted_text_formats(user).iter().any(|f| f == format) {
        return Err(host_errors::ERR_TEXT_FORMAT_NOT_PERMITTED);
    }
    Ok(FilterPipeline::for_format(format).process(text))
}

/// Register the text format host function.
pub fn register_text_format_functions(linker: &mut Linker<PluginState>) -> Result<()> {
    // filter-text(text, format) -> result<string, string>, as bytes written or
    // a negative error code.
    linker
        .func_wrap(
            "trovato:kernel/text-format",
            "filter-text",
            |mut caller: wasmtime::Caller<'_, PluginState>,
             text_ptr: i32,
             text_len: i32,
             format_ptr: i32,
             format_len: i32,
             out_ptr: i32,
             out_max_len: i32|
             -> i32 {
                let _trace =
                    HostCallGuard::new(&caller, "trovato:kernel/text-format", "filter-text");
                let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                    return host_errors::ERR_MEMORY_MISSING;
                };
                let Ok(text) = read_string_from_memory(&memory, &caller, text_ptr, text_len) else {
                    return host_errors::ERR_PARAM1_READ;
                };
                let Ok(format) = read_string_from_memory(&memory, &caller, format_ptr, format_len)
                else {
                    return host_errors::ERR_PARAM2_OR_OUTPUT;
                };

                let filtered = match filter_for(&caller.data().request.user, &text, &format) {
                    Ok(filtered) => filtered,
                    Err(code) => return code,
                };
                // Refuse rather than truncate: a cut could end inside a tag.
                if usize::try_from(out_max_len).is_ok_and(|max| filtered.len() > max) {
                    return host_errors::ERR_TEXT_FORMAT_TOO_LARGE;
                }
                write_string_to_memory(&memory, &mut caller, out_ptr, out_max_len, &filtered)
                    .unwrap_or(host_errors::ERR_PARAM2_OR_OUTPUT)
            },
        )
        .into_anyhow()?;

    Ok(())
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn user(permissions: &[&str]) -> UserContext {
        UserContext::authenticated(
            Uuid::now_v7(),
            permissions.iter().map(|p| p.to_string()).collect(),
        )
    }

    #[test]
    fn filtered_html_drops_a_script() {
        let out = filter_for(
            &user(&["use filtered_html"]),
            "<p>Hi</p><script>alert(1)</script>",
            "filtered_html",
        )
        .unwrap();
        assert!(out.contains("<p>Hi</p>"), "{out}");
        assert!(!out.contains("<script"), "{out}");
    }

    #[test]
    fn full_html_needs_its_permission() {
        assert_eq!(
            filter_for(&user(&["use filtered_html"]), "<b>x</b>", "full_html"),
            Err(host_errors::ERR_TEXT_FORMAT_NOT_PERMITTED)
        );
        assert_eq!(
            filter_for(&user(&["use full_html"]), "<b>x</b>", "full_html").unwrap(),
            "<b>x</b>"
        );
    }

    #[test]
    fn plain_text_is_always_allowed_and_escapes() {
        let out = filter_for(&user(&[]), "<b>x</b>", "plain_text").unwrap();
        assert!(out.contains("&lt;b&gt;"), "{out}");
    }

    #[test]
    fn an_unknown_format_is_refused() {
        assert_eq!(
            filter_for(&user(&["use full_html"]), "x", "markdown"),
            Err(host_errors::ERR_TEXT_FORMAT_UNKNOWN)
        );
    }

    #[test]
    fn register_text_format_succeeds() {
        let engine = wasmtime::Engine::new(&wasmtime::Config::new()).unwrap();
        let mut linker: Linker<PluginState> = Linker::new(&engine);
        assert!(register_text_format_functions(&mut linker).is_ok());
    }
}
