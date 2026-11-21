//! The `query` host interface: a plugin runs a Gather definition.
//!
//! `run(definition-json, options-json)` takes a Gather `QueryDefinition`, the
//! JSON a site owner's gather is stored as, and returns the `GatherResult` of
//! running it through [`GatherService::execute_for_plugin`](crate::gather::GatherService::execute_for_plugin), the pipeline every
//! gather goes through. `docs/design/plugin-query-surface.md` is the design;
//! this module is its plugin rules.
//!
//! # Two kinds of base
//!
//! - **Content** (`item`, or a record type named by `record_type`) is always
//!   read for the request's viewer, read stages and language, which the plugin
//!   cannot set. `stage_aware = false` is refused. A background call needs
//!   `item_background`, exactly as `item-api` does, and then reads as the
//!   background principal.
//! - **A plugin's own table** (in its allowlist and not a content table) has no
//!   viewer semantics: no access predicate, no stage, no overlay, no `url`.
//!
//! Content is always the base: a content base may join the kernel's join
//! tables and the plugin's own tables; an own-table base may join only the
//! plugin's own tables, never `item` or `item_translation`, because the access
//! pass decides base rows only.
//!
//! # Refusals
//!
//! A table outside the allowlist is [`host_errors::ERR_TABLE_NOT_DECLARED`];
//! a bound exceeded is [`host_errors::ERR_QUERY_BOUND`]; anything else the
//! rules or the validator refuse is [`host_errors::ERR_QUERY_INVALID`], with
//! the reason in the host log.

use std::collections::HashMap;

use anyhow::Result;
use serde::Deserialize;
use tracing::warn;
use trovato_sdk::host_errors;
use wasmtime::Linker;

use super::trace::TracedLinker;
use super::{read_string_from_memory, write_string_to_memory};
use crate::content::reentry;
use crate::gather::{
    FilterOperator, GATHER_JOIN_TABLES, MAX_GROUP_DEPTH, MAX_ITEMS_PER_PAGE, PluginScope,
    QueryContext, QueryDefinition,
};
use crate::plugin::{DbPolicy, PluginState, WasmtimeExt};
use crate::tap::UserContext;

/// Rows per call when the options name none.
const DEFAULT_PER_PAGE: u32 = 20;

/// Filters a plugin definition may carry.
pub const MAX_FILTERS: usize = 32;

/// Sorts a plugin definition may carry.
pub const MAX_SORTS: usize = 8;

/// Relationships a plugin definition may carry: the validator's join cap.
pub const MAX_RELATIONSHIPS: usize = 3;

/// `group_by` columns, and aggregates, a plugin definition may carry.
pub const MAX_AGGREGATES: usize = 8;

/// Tables that hold content: never joinable from a plugin's own base.
const CONTENT_TABLES: &[&str] = &["item", "item_translation"];

/// `options-json`: every key optional.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunOptions {
    page: Option<u32>,
    per_page: Option<u32>,
    #[serde(default)]
    url_args: HashMap<String, String>,
}

/// Whether a definition's base is content.
fn is_content_base(definition: &QueryDefinition) -> bool {
    definition.record_type.is_some() || definition.base_table == "item"
}

/// Refuse a table outside the allowlist, logging which rule refused it.
fn allowed(policy: &DbPolicy, table: &str) -> Result<(), i32> {
    policy.check_table(table).map_err(|msg| {
        warn!(denied = %msg, "plugin query rejected: table outside allowlist");
        host_errors::ERR_TABLE_NOT_DECLARED
    })
}

fn invalid(reason: &str) -> i32 {
    warn!(reason, "plugin query rejected");
    host_errors::ERR_QUERY_INVALID
}

fn bound(reason: &str) -> i32 {
    warn!(reason, "plugin query rejected: bound exceeded");
    host_errors::ERR_QUERY_BOUND
}

/// Apply the plugin rules to a submitted definition: the bounds, the table
/// policy, what may join what. Returns the scope the service runs it in.
///
/// An own-table base has `stage_aware` cleared here, because the table has no
/// `stage_id` and the plugin should not have to say so.
pub fn plan(
    definition: &mut QueryDefinition,
    per_page: u32,
    policy: &DbPolicy,
) -> Result<PluginScope, i32> {
    if per_page > MAX_ITEMS_PER_PAGE {
        return Err(bound("per_page"));
    }
    // Every filter counts: grouped ones and `having` too.
    let filters = definition.all_filters().len() + definition.aggregation.having.len();
    if filters > MAX_FILTERS {
        return Err(bound("filters"));
    }
    if definition.group_depth() > MAX_GROUP_DEPTH {
        return Err(bound("filter group nesting"));
    }
    if definition.aggregation.group_by.len() > MAX_AGGREGATES
        || definition.aggregation.aggregates.len() > MAX_AGGREGATES
    {
        return Err(bound("group_by or aggregates"));
    }
    if definition.sorts.len() > MAX_SORTS {
        return Err(bound("sorts"));
    }
    if definition.relationships.len() > MAX_RELATIONSHIPS {
        return Err(bound("relationships"));
    }
    if !definition.includes.is_empty() {
        return Err(invalid("includes are not accepted from a plugin"));
    }
    if definition
        .all_filters()
        .iter()
        .any(|f| f.operator == FilterOperator::SemanticSimilarity)
    {
        return Err(invalid("semantic_similarity is not accepted from a plugin"));
    }

    let content = is_content_base(definition);
    let mut scope = PluginScope {
        own_base: !content,
        own_tables: Vec::new(),
    };
    if content {
        if !definition.stage_aware {
            return Err(invalid(
                "a content definition cannot set stage_aware = false",
            ));
        }
    } else {
        allowed(policy, &definition.base_table)?;
        definition.stage_aware = false;
    }

    for rel in &definition.relationships {
        let target = rel.target_table.as_str();
        if !content && CONTENT_TABLES.contains(&target) {
            return Err(invalid("an own-table base cannot join content"));
        }
        if content && GATHER_JOIN_TABLES.contains(&target) {
            continue;
        }
        allowed(policy, target)?;
        scope.own_tables.push(target.to_string());
    }
    Ok(scope)
}

/// The viewer a content read is made for, or the refusal.
fn viewer_for(state: &PluginState, content: bool) -> Result<UserContext, i32> {
    let user = &state.request.user;
    if content && reentry::inside_access_decision() {
        return Err(host_errors::ERR_ITEM_ACCESS_DENIED);
    }
    if content && user.is_background() && !state.item_background {
        return Err(host_errors::ERR_ITEM_BACKGROUND_DENIED);
    }
    Ok(user.clone())
}

/// Run one call: parse, plan, execute, serialize.
async fn run(
    state: &PluginState,
    definition_json: &str,
    options_json: &str,
) -> Result<String, i32> {
    let mut definition: QueryDefinition =
        serde_json::from_str(definition_json).map_err(|_| host_errors::ERR_PARAM_DESERIALIZE)?;
    let options: RunOptions = if options_json.trim().is_empty() {
        RunOptions::default()
    } else {
        serde_json::from_str(options_json).map_err(|_| host_errors::ERR_PARAM_DESERIALIZE)?
    };
    let per_page = options.per_page.unwrap_or(DEFAULT_PER_PAGE).max(1);
    let page = options.page.unwrap_or(1).max(1);

    let scope = plan(&mut definition, per_page, &state.db_policy)?;
    let viewer = viewer_for(state, !scope.own_base)?;

    let Some(services) = state.request.services() else {
        return Err(host_errors::ERR_NO_SERVICES);
    };
    let Some(gather) = services.gather_service() else {
        return Err(host_errors::ERR_NO_SERVICES);
    };

    let context = QueryContext {
        current_user_id: viewer.authenticated.then_some(viewer.id),
        viewer: Some(viewer),
        url_args: options.url_args,
        language: state.request.language().map(str::to_string),
    };
    let result = gather
        .execute_for_plugin(
            &definition,
            page,
            per_page,
            state.request.read_stages(),
            &context,
            &scope,
        )
        .await
        .map_err(|e| {
            let message = format!("{e:#}");
            if message.contains("Query validation failed")
                || message.contains("not a content table")
            {
                invalid(&message)
            } else {
                warn!(error = %message, "plugin query failed");
                host_errors::ERR_SQL_FAILED
            }
        })?;
    serde_json::to_string(&result).map_err(|_| host_errors::ERR_SERIALIZE_FAILED)
}

/// Register the `query` host interface.
pub fn register_query_functions(linker: &mut Linker<PluginState>) -> Result<()> {
    // run(definition_json, options_json, out) -> i32 (bytes written or error)
    linker
        .func_wrap_async_traced(
            "trovato:kernel/query",
            "run",
            |mut caller: wasmtime::Caller<'_, PluginState>,
             (def_ptr, def_len, opt_ptr, opt_len, out_ptr, out_max_len): (
                i32,
                i32,
                i32,
                i32,
                i32,
                i32,
            )| {
                Box::new(async move {
                    let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                        return host_errors::ERR_MEMORY_MISSING;
                    };
                    let Ok(definition) =
                        read_string_from_memory(&memory, &caller, def_ptr, def_len)
                    else {
                        return host_errors::ERR_PARAM1_READ;
                    };
                    let Ok(options) = read_string_from_memory(&memory, &caller, opt_ptr, opt_len)
                    else {
                        return host_errors::ERR_PARAM2_OR_OUTPUT;
                    };
                    match run(caller.data(), &definition, &options).await {
                        Ok(json) => write_string_to_memory(
                            &memory,
                            &mut caller,
                            out_ptr,
                            out_max_len,
                            &json,
                        )
                        .unwrap_or(host_errors::ERR_PARAM3_READ),
                        Err(code) => code,
                    }
                })
            },
        )
        .into_anyhow()?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn def(json: serde_json::Value) -> QueryDefinition {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn register_query_succeeds() {
        let engine = wasmtime::Engine::new(&wasmtime::Config::new()).unwrap();
        let mut linker: Linker<PluginState> = Linker::new(&engine);
        assert!(register_query_functions(&mut linker).is_ok());
    }

    #[test]
    fn an_own_table_base_has_its_stage_predicate_cleared() {
        let policy = DbPolicy::from_parts("p", ["votes".to_string()], false);
        let mut d = def(serde_json::json!({"base_table": "votes"}));
        let scope = plan(&mut d, 10, &policy).unwrap();
        assert!(scope.own_base);
        assert!(!d.stage_aware);
    }

    #[test]
    fn a_content_base_joins_kernel_tables_without_a_declaration() {
        let policy = DbPolicy::default();
        let mut d = def(serde_json::json!({
            "base_table": "item",
            "relationships": [{"name": "t", "target_table": "category_tag", "join_type": "left",
                               "local_field": "id", "foreign_field": "id"}]
        }));
        let scope = plan(&mut d, 10, &policy).unwrap();
        assert!(!scope.own_base);
        assert!(scope.own_tables.is_empty());
    }
}
