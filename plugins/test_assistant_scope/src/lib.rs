//! 0.102 e2e fixture: a plugin that can be **configured by conversation**.
//!
//! The smallest thing that exercises all three assistant taps end to end. It
//! owns one piece of state — a widget's colour, in a plugin variable — and
//! offers the three shapes of tool that matter:
//!
//! - `read_widget`, a read the kernel executes as soon as the model calls it;
//! - `set_widget_color`, a write the kernel turns into a proposal, so the
//!   variable must still be unchanged after the model asks for it and only
//!   change once a person applies;
//! - `fail_loudly`, a read that fails on purpose, because a tool saying no has
//!   to reach the model as a result rather than killing the turn.
//!
//! It also declares a **deliberately invalid** second scope, so the registry's
//! drop-and-warn path is exercised by a real plugin rather than only by a unit
//! test: a site with one bad scope declaration must still boot, and must still
//! register the good scope beside it.

use trovato_sdk::host;
use trovato_sdk::prelude::*;

/// Permission the scope is gated on. An administrator passes without it.
const PERM: &str = "configure test widget";

/// The plugin variable holding the widget's colour.
const COLOR_VAR: &str = "color";

/// What the widget's colour is before anyone sets one.
const UNSET: &str = "unset";

/// The widget's current colour.
fn current_color() -> String {
    host::variables_get(COLOR_VAR, UNSET).unwrap_or_else(|_| UNSET.to_string())
}

/// Declare what can be configured by conversation.
#[plugin_tap]
pub fn tap_assistant_scopes() -> Vec<AssistantScope> {
    vec![
        AssistantScope::new("test_widget", "Test widget", PERM, AssistantIdKind::String)
            .description("Configure a test widget")
            .prompt("You configure a test widget.")
            .suggestions(["What colour is the widget?", "Make it teal"])
            .tool(AssistantTool::read(
                "read_widget",
                "Read the widget's current colour.",
            ))
            .tool(
                AssistantTool::write(
                    "set_widget_color",
                    "Set the widget's colour.",
                    AssistantRisk::Normal,
                )
                .parameters(serde_json::json!({
                    "type": "object",
                    "required": ["color"],
                    "properties": {"color": {"type": "string"}}
                })),
            )
            .tool(AssistantTool::read(
                "fail_loudly",
                "Always fails, on purpose.",
            )),
        // An item scope, so the kernel's automatic launcher on an item page has
        // something to attach to. `conference` is a content type the kernel's
        // own test fixture seeds, so this needs no other plugin enabled.
        AssistantScope::new(
            "test_conference",
            "Test conference",
            PERM,
            AssistantIdKind::Item,
        )
        .description("Configure a conference")
        .item_types(["conference"])
        .prompt("You configure a conference.")
        .tool(AssistantTool::read(
            "read_widget",
            "Read the widget's current colour.",
        )),
        // Invalid on purpose: a tool name with a space in it. The registry must
        // drop this scope, keep the one above, and record why.
        AssistantScope::new(
            "broken_widget",
            "Broken widget",
            PERM,
            AssistantIdKind::String,
        )
        .prompt("This scope should never be registered.")
        .tool(AssistantTool::read("read widget", "Invalid tool name.")),
    ]
}

/// Describe the widget being configured.
#[plugin_tap]
pub fn tap_assistant_context(request: AssistantContextRequest) -> AssistantContext {
    let id = request.scope_id.unwrap_or_default();
    let color = current_color();
    if request.scope == "test_conference" {
        return AssistantContext::new(
            format!("Conference {id}"),
            format!("Conference {id} exists. The widget colour is {color}."),
        )
        .link("View conference", format!("/item/{id}"));
    }
    AssistantContext::new(
        format!("Widget {id}"),
        format!("Widget {id} has color {color}."),
    )
    .link("View widget", format!("/widget/{id}"))
}

/// Answer one tool call.
#[plugin_tap]
pub fn tap_assistant_tool(call: AssistantToolCall) -> AssistantToolResult {
    // The belt over the kernel's braces: the kernel already checked the scope's
    // permission before opening the conversation, and this checks again at the
    // moment of the call, because that is where the change happens.
    if !host::current_user_has_permission(PERM) {
        return AssistantToolResult::failed("You do not have permission to configure the widget.");
    }

    match call.tool.as_str() {
        "read_widget" => {
            AssistantToolResult::data(serde_json::json!({"color": current_color()}).to_string())
        }
        "fail_loudly" => AssistantToolResult::failed("as requested"),
        "set_widget_color" => set_widget_color(&call),
        other => AssistantToolResult::failed(format!("no such tool: {other}")),
    }
}

/// The one write: describe, then (once applied) carry out.
fn set_widget_color(call: &AssistantToolCall) -> AssistantToolResult {
    let Some(color) = call.arguments.get("color").and_then(|v| v.as_str()) else {
        return AssistantToolResult::failed("set_widget_color needs a `color` string");
    };

    match call.mode {
        // Describe changes nothing. That is the whole contract: what the person
        // reads on the proposal card is produced without the change happening.
        AssistantToolMode::Describe => AssistantToolResult::ok(
            format!("Would set the widget colour to {color}."),
            format!("Set widget color to {color}"),
        ),
        AssistantToolMode::Execute => match host::variables_set(COLOR_VAR, color) {
            Ok(()) => AssistantToolResult::ok(
                format!("The widget colour is now {color}."),
                format!("Widget color is now {color}"),
            ),
            Err(code) => AssistantToolResult::failed(format!(
                "could not write the widget colour (host error {code})"
            )),
        },
    }
}
