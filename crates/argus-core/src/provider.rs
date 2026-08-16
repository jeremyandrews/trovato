//! A scripted [`LlmProvider`] for tests plus the lenient-JSON recovery the real
//! decide stage relies on.
//!
//! Real models return sloppy JSON: fenced in ```` ```json ```` blocks, with a
//! sentence of preamble, or with trailing commentary after the closing brace.
//! [`extract_json_object`] recovers the first balanced `{...}` object from such
//! output so [`crate::decide`] can parse a score without a strict-JSON contract
//! it cannot enforce on a third-party model.

use std::cell::RefCell;

use crate::error::{CoreError, CoreResult};
use crate::ports::{ChatRequest, ChatResponse, EmbedResponse, LlmProvider, Usage};

/// Recover the first balanced top-level JSON object substring from `raw`.
///
/// Strips Markdown code fences and any prose before/after the object, then
/// scans for the first `{` and returns through its matching `}`, respecting
/// string literals and escapes so a `}` inside a quoted value does not close
/// the object early. Returns `None` if no balanced object is present.
#[must_use]
pub fn extract_json_object(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let start = raw.find('{')?;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(raw[start..=i].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// Parse a lenient JSON object out of model output.
///
/// Convenience wrapper over [`extract_json_object`] + `serde_json`.
///
/// # Errors
///
/// Returns [`CoreError::DecisionParse`] if no balanced object is present or the
/// recovered substring is not valid JSON.
pub fn parse_lenient_object(raw: &str) -> CoreResult<serde_json::Value> {
    let obj = extract_json_object(raw).ok_or_else(|| {
        CoreError::DecisionParse(format!("no JSON object in model output: {raw:?}"))
    })?;
    serde_json::from_str(&obj)
        .map_err(|e| CoreError::DecisionParse(format!("invalid JSON object {obj:?}: {e}")))
}

/// One scripted turn for [`MockProvider`].
#[derive(Debug, Clone)]
pub enum Scripted {
    /// Return this chat content (usage is synthesized from length).
    Chat(String),
    /// Fail the call with a transient provider error.
    ChatError(String),
    /// Return this embedding vector.
    Embed(Vec<f32>),
}

/// A deterministic, scripted [`LlmProvider`] for unit tests.
///
/// Hands out scripted responses in order and records how many times each method
/// was called and the last chat request seen, so tests can assert "exactly one
/// AI call per job" (M1-7) and inspect the prompt the decide stage built.
#[derive(Debug, Default)]
pub struct MockProvider {
    script: RefCell<std::collections::VecDeque<Scripted>>,
    /// Number of `chat` calls made.
    chat_calls: RefCell<u32>,
    /// Number of `embed` calls made.
    embed_calls: RefCell<u32>,
    /// The most recent chat request, for prompt-formatting assertions.
    last_chat: RefCell<Option<ChatRequest>>,
    /// Per-chat cost the mock reports on every [`ChatResponse::cost_estimate`],
    /// standing in for the host's pricing lookup (G-COST-OPAQUE). `None` mimics an
    /// unpriced model.
    chat_cost: Option<f64>,
}

impl MockProvider {
    /// A provider that will hand out `script` responses in order.
    #[must_use]
    pub fn new(script: Vec<Scripted>) -> Self {
        Self {
            script: RefCell::new(script.into()),
            chat_calls: RefCell::new(0),
            embed_calls: RefCell::new(0),
            last_chat: RefCell::new(None),
            chat_cost: None,
        }
    }

    /// A provider that returns `content` for its next (and only expected) chat.
    #[must_use]
    pub fn chat_once(content: impl Into<String>) -> Self {
        Self::new(vec![Scripted::Chat(content.into())])
    }

    /// Report `cost` as the per-chat `cost_estimate` (G-COST-OPAQUE): the mock's
    /// stand-in for the host's pricing lookup, so a test can prove cost threads
    /// from the response through `decide` into the `DecideReport`.
    #[must_use]
    pub fn with_chat_cost(mut self, cost: f64) -> Self {
        self.chat_cost = Some(cost);
        self
    }

    /// How many `chat` calls have been made.
    #[must_use]
    pub fn chat_calls(&self) -> u32 {
        *self.chat_calls.borrow()
    }

    /// How many `embed` calls have been made.
    #[must_use]
    pub fn embed_calls(&self) -> u32 {
        *self.embed_calls.borrow()
    }

    /// The most recent chat request, if any.
    #[must_use]
    pub fn last_chat(&self) -> Option<ChatRequest> {
        self.last_chat.borrow().clone()
    }
}

/// Synthesize plausible token usage from text length (≈4 chars/token).
fn synth_usage(prompt: &str, completion: &str) -> Usage {
    let p = (prompt.len() / 4) as u32;
    let c = (completion.len() / 4) as u32;
    Usage {
        prompt_tokens: p,
        completion_tokens: c,
        total_tokens: p + c,
    }
}

impl LlmProvider for MockProvider {
    fn chat(&self, req: &ChatRequest) -> CoreResult<ChatResponse> {
        *self.chat_calls.borrow_mut() += 1;
        *self.last_chat.borrow_mut() = Some(req.clone());
        let next = self.script.borrow_mut().pop_front();
        match next {
            Some(Scripted::Chat(content)) => {
                let prompt = format!("{}{}", req.system.as_deref().unwrap_or_default(), req.user);
                let usage = synth_usage(&prompt, &content);
                Ok(ChatResponse {
                    content,
                    model: req.model.clone().unwrap_or_else(|| "mock".to_string()),
                    usage,
                    cost_estimate: self.chat_cost,
                })
            }
            Some(Scripted::ChatError(msg)) => Err(CoreError::Provider(msg)),
            Some(Scripted::Embed(_)) | None => Err(CoreError::Provider(
                "mock: no scripted chat response".to_string(),
            )),
        }
    }

    fn embed(&self, input: &str, model: Option<&str>) -> CoreResult<EmbedResponse> {
        *self.embed_calls.borrow_mut() += 1;
        let next = self.script.borrow_mut().pop_front();
        match next {
            Some(Scripted::Embed(vector)) => Ok(EmbedResponse {
                vector,
                model: model.unwrap_or("mock-embed").to_string(),
                usage: synth_usage(input, ""),
            }),
            _ => Err(CoreError::Provider(
                "mock: no scripted embed response".to_string(),
            )),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn extracts_bare_object() {
        assert_eq!(
            extract_json_object(r#"{"score": 80}"#).as_deref(),
            Some(r#"{"score": 80}"#)
        );
    }

    #[test]
    fn strips_fences_and_prose() {
        let raw = "Here is my answer:\n```json\n{\"score\": 42, \"reason\": \"meh\"}\n```\nHope that helps!";
        let obj = extract_json_object(raw).unwrap();
        let v: serde_json::Value = serde_json::from_str(&obj).unwrap();
        assert_eq!(v["score"], 42);
    }

    #[test]
    fn respects_braces_inside_strings() {
        let raw = r#"prefix {"reason": "a } inside", "score": 5} trailing"#;
        let obj = extract_json_object(raw).unwrap();
        let v: serde_json::Value = serde_json::from_str(&obj).unwrap();
        assert_eq!(v["score"], 5);
        assert_eq!(v["reason"], "a } inside");
    }

    #[test]
    fn none_when_no_object() {
        assert!(extract_json_object("no json here").is_none());
        assert!(parse_lenient_object("no json here").is_err());
    }

    #[test]
    fn mock_records_calls_and_last_request() {
        let p = MockProvider::chat_once(r#"{"score": 90}"#);
        assert_eq!(p.chat_calls(), 0);
        let req = ChatRequest {
            model: Some("cheap".to_string()),
            system: Some("sys".to_string()),
            user: "u".to_string(),
            max_tokens: Some(64),
        };
        let resp = p.chat(&req).unwrap();
        assert_eq!(resp.content, r#"{"score": 90}"#);
        assert_eq!(resp.model, "cheap");
        assert!(resp.usage.total_tokens > 0);
        assert_eq!(p.chat_calls(), 1);
        assert_eq!(p.last_chat().unwrap().user, "u");
    }

    #[test]
    fn mock_chat_error_is_transient() {
        let p = MockProvider::new(vec![Scripted::ChatError("boom".to_string())]);
        let err = p
            .chat(&ChatRequest {
                model: None,
                system: None,
                user: "u".to_string(),
                max_tokens: None,
            })
            .unwrap_err();
        assert!(err.is_transient());
    }
}
