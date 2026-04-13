//! Scolta AI search integration plugin for Trovato.
//!
//! Provides AI-powered search enhancement: query expansion, result
//! summarization, and multi-turn follow-up conversations. Uses the
//! kernel's `ai_request` host function for LLM calls and `variables_get`
//! for site configuration.

use trovato_sdk::host;
use trovato_sdk::prelude::*;

/// Register the Scolta search permission.
#[plugin_tap]
pub fn tap_perm() -> Vec<PermissionDefinition> {
    vec![PermissionDefinition::new(
        "use scolta search",
        "Use AI-powered search features (query expansion, summarization, follow-up)",
    )]
}

/// Register Scolta API routes.
#[plugin_tap]
pub fn tap_menu() -> Vec<MenuDefinition> {
    vec![
        MenuDefinition::new("/api/scolta/v1/expand-query", "Scolta: Expand Query")
            .callback("scolta_expand_query"),
        MenuDefinition::new("/api/scolta/v1/summarize", "Scolta: Summarize Results")
            .callback("scolta_summarize"),
        MenuDefinition::new("/api/scolta/v1/follow-up", "Scolta: Follow-up")
            .callback("scolta_follow_up"),
        MenuDefinition::new("/api/scolta/v1/health", "Scolta: Health Check")
            .callback("scolta_health"),
    ]
}

/// Expand a search query into related search terms using AI.
///
/// Called by the `scolta_expand_query` route callback.
///
/// Calls the AI provider to suggest alternative and related search terms
/// based on the original query and site context. Returns a JSON array of
/// suggested terms.
#[allow(dead_code)] // called by plugin route callback at runtime
fn expand_query(query: &str, site_description: &str) -> Result<String, i32> {
    let prompt = format!(
        "Given a search query on a website described as: \"{site_description}\"\n\n\
         Original query: \"{query}\"\n\n\
         Suggest 5-8 related search terms or phrases that would help the user \
         find relevant content. Return ONLY a JSON array of strings, no explanation.\n\n\
         Example: [\"term one\", \"term two\", \"term three\"]"
    );

    let request = AiRequest {
        operation: AiOperationType::Chat,
        provider_id: None,
        model: None,
        messages: vec![
            AiMessage::system(
                "You are a search query expansion assistant. Respond with only \
                 valid JSON arrays of strings. No markdown, no explanation.",
            ),
            AiMessage::user(&prompt),
        ],
        input: None,
        options: AiRequestOptions {
            max_tokens: Some(200),
            ..AiRequestOptions::default()
        },
    };

    let response = host::ai_request(&request)?;
    Ok(response.content)
}

/// Summarize search results using AI.
///
/// Called by the `scolta_summarize` route callback.
///
/// Takes a JSON string of search results and the original query, then
/// produces a concise natural-language summary highlighting the most
/// relevant findings.
#[allow(dead_code)] // called by plugin route callback at runtime
fn summarize_results(results_json: &str, query: &str) -> Result<String, i32> {
    let prompt = format!(
        "The user searched for: \"{query}\"\n\n\
         Here are the search results:\n{results_json}\n\n\
         Provide a concise 2-3 sentence summary of the most relevant findings. \
         Highlight key themes and suggest which results are most pertinent to the query."
    );

    let request = AiRequest {
        operation: AiOperationType::Chat,
        provider_id: None,
        model: None,
        messages: vec![
            AiMessage::system(
                "You are a search results summarizer. Be concise and helpful. \
                 Focus on relevance to the user's query.",
            ),
            AiMessage::user(&prompt),
        ],
        input: None,
        options: AiRequestOptions {
            max_tokens: Some(300),
            ..AiRequestOptions::default()
        },
    };

    let response = host::ai_request(&request)?;
    Ok(response.content)
}

/// Multi-turn AI follow-up on search results.
///
/// Called by the `scolta_follow_up` route callback.
///
/// Takes a conversation history (JSON array of messages), a new question,
/// and search context to continue an interactive search dialogue.
#[allow(dead_code)] // called by plugin route callback at runtime
fn follow_up(conversation_json: &str, question: &str, context: &str) -> Result<String, i32> {
    let mut messages = vec![AiMessage::system(
        "You are a helpful search assistant. Use the provided context to answer \
         follow-up questions about search results. Be concise and cite specific \
         results when possible.",
    )];

    // Parse conversation history and add as prior messages
    if let Ok(history) = serde_json::from_str::<Vec<serde_json::Value>>(conversation_json) {
        for msg in &history {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("user");
            let content = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
            match role {
                "assistant" => messages.push(AiMessage::assistant(content)),
                _ => messages.push(AiMessage::user(content)),
            }
        }
    }

    // Add the new question with context
    let prompt = format!("Context from search results:\n{context}\n\nQuestion: {question}");
    messages.push(AiMessage::user(&prompt));

    let request = AiRequest {
        operation: AiOperationType::Chat,
        provider_id: None,
        model: None,
        messages,
        input: None,
        options: AiRequestOptions {
            max_tokens: Some(500),
            ..AiRequestOptions::default()
        },
    };

    let response = host::ai_request(&request)?;
    Ok(response.content)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn perm_returns_scolta_permission() {
        let perms = __inner_tap_perm();
        assert_eq!(perms.len(), 1);
        assert_eq!(perms[0].name, "use scolta search");
    }

    #[test]
    fn menu_returns_four_routes() {
        let menus = __inner_tap_menu();
        assert_eq!(menus.len(), 4);

        let paths: Vec<&str> = menus.iter().map(|m| m.path.as_str()).collect();
        assert!(paths.contains(&"/api/scolta/v1/expand-query"));
        assert!(paths.contains(&"/api/scolta/v1/summarize"));
        assert!(paths.contains(&"/api/scolta/v1/follow-up"));
        assert!(paths.contains(&"/api/scolta/v1/health"));
    }

    #[test]
    fn expand_query_calls_ai() {
        // Stub ai_request returns mock response
        let result = expand_query("rust conferences", "A tech conference directory");
        assert!(result.is_ok());
    }

    #[test]
    fn summarize_results_calls_ai() {
        let results = r#"[{"title": "RustConf 2026", "url": "/conf/rustconf"}]"#;
        let result = summarize_results(results, "rust");
        assert!(result.is_ok());
    }

    #[test]
    fn follow_up_handles_empty_conversation() {
        let result = follow_up("[]", "Tell me more", "Some context");
        assert!(result.is_ok());
    }

    #[test]
    fn follow_up_handles_conversation_history() {
        let history = r#"[
            {"role": "user", "content": "What about Rust?"},
            {"role": "assistant", "content": "Rust is a systems language."}
        ]"#;
        let result = follow_up(history, "Any conferences?", "RustConf 2026");
        assert!(result.is_ok());
    }
}
