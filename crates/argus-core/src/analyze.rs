//! The analyze stage's pure logic (M2): build the deep-analysis prompt, run one
//! model call, and parse a structured analysis out of whatever the model
//! actually returned.
//!
//! One AI call per queue job, the same rule the decide stage follows and for
//! the same reason: the background epoch budget is a trap for multi-call loops,
//! so stage granularity is one call.
//!
//! # Parsing posture
//!
//! Every prose field is optional and defaults to empty. Only two things make a
//! response unusable: no recoverable JSON object at all, or an object with no
//! summary and no entities (a response that carried no analysis). Everything
//! else — a missing field, a string where an array belongs, an entity given as
//! a bare string, an unrecognized entity type, a truncated tail — is absorbed.
//! A model that produces sloppy-but-present output must not cost the article.

use crate::entity::{EntityType, ExtractedEntity};
use crate::error::{CoreError, CoreResult};
use crate::ports::{ChatRequest, LlmProvider};
use crate::provider::parse_lenient_object;

/// Words of article body handed to the analyze model. Larger than the decide
/// stage's window (which only has to judge relevance): analysis quality falls
/// off sharply when the model cannot see the whole argument.
pub const ANALYZE_BODY_WORDS: usize = 2000;

/// Generation cap for the analyze call. Five prose fields plus an entity list.
const ANALYZE_MAX_TOKENS: u32 = 1400;

/// Most entities accepted from one response. A model that returns hundreds has
/// misunderstood the task, and each one costs an upsert and a link row.
pub const MAX_ENTITIES_PER_ARTICLE: usize = 32;

/// The system prompt. Names the exact keys and forbids prose outside the JSON,
/// which is what makes the lenient recovery in [`crate::provider`] a safety net
/// rather than the primary mechanism.
const ANALYZE_SYSTEM: &str = "You are a news analyst. Read the article and respond with ONLY a \
JSON object with these keys: \"summary\" (2-3 sentences of what happened), \
\"critical_analysis\" (what the piece argues and how well it supports it), \
\"fallacy_analysis\" (specific reasoning flaws, or \"none identified\"), \
\"source_analysis\" (who is quoted, what they are positioned to gain, what is \
missing), and \"entities\" (an array of objects {\"name\": string, \"type\": one of \
person, company, place, event, technology}). Do not include any text outside the \
JSON object.";

/// A parsed analyze response.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Analysis {
    /// Two-to-three sentence factual summary.
    pub summary: String,
    /// What the piece argues and how well it argues it.
    pub critical_analysis: String,
    /// Named reasoning flaws.
    pub fallacy_analysis: String,
    /// Who is quoted and what is missing.
    pub source_analysis: String,
    /// Entities named in the article, capped at [`MAX_ENTITIES_PER_ARTICLE`].
    pub entities: Vec<ExtractedEntity>,
}

/// Take the first [`ANALYZE_BODY_WORDS`] whitespace-delimited words of `body`.
#[must_use]
pub fn first_words(body: &str) -> String {
    body.split_whitespace()
        .take(ANALYZE_BODY_WORDS)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Build the analyze [`ChatRequest`] for one article.
///
/// `model` routes the call; analyze uses the strong model where decide uses the
/// cheap one, which is the whole reason per-stage routing exists.
#[must_use]
pub fn build_request(model: Option<String>, title: &str, body: &str) -> ChatRequest {
    let user = format!(
        "Article title: {title}\n\nArticle body:\n{}",
        first_words(body)
    );
    ChatRequest {
        model,
        system: Some(ANALYZE_SYSTEM.to_string()),
        user,
        max_tokens: Some(ANALYZE_MAX_TOKENS),
    }
}

/// Read a JSON value as prose, tolerating the shapes models substitute for a
/// string: a bare string, a list of strings, or a nested `{"text": ...}`.
fn as_prose(v: Option<&serde_json::Value>) -> String {
    match v {
        Some(serde_json::Value::String(s)) => s.trim().to_string(),
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter_map(|i| i.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" "),
        Some(serde_json::Value::Object(map)) => map
            .get("text")
            .or_else(|| map.get("value"))
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .trim()
            .to_string(),
        Some(serde_json::Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

/// Read one entry of the `entities` array, accepting an object or a bare
/// string. An entry with no usable name is dropped, not defaulted.
fn as_entity(v: &serde_json::Value) -> Option<ExtractedEntity> {
    match v {
        serde_json::Value::String(s) if !s.trim().is_empty() => Some(ExtractedEntity {
            name: s.trim().to_string(),
            entity_type: EntityType::Other,
        }),
        serde_json::Value::Object(map) => {
            let name = map
                .get("name")
                .or_else(|| map.get("entity"))
                .or_else(|| map.get("text"))
                .and_then(|x| x.as_str())?
                .trim();
            if name.is_empty() {
                return None;
            }
            let entity_type = map
                .get("type")
                .or_else(|| map.get("entity_type"))
                .or_else(|| map.get("kind"))
                .and_then(|x| x.as_str())
                .map_or(EntityType::Other, EntityType::parse);
            Some(ExtractedEntity {
                name: name.to_string(),
                entity_type,
            })
        }
        _ => None,
    }
}

/// Parse a model response into an [`Analysis`], tolerating sloppy output.
///
/// # Errors
///
/// [`CoreError::AnalysisParse`] when no balanced JSON object can be recovered,
/// or when the recovered object carries neither a summary nor any entity —
/// which means the call produced no analysis, however well-formed it looks.
/// Both are permanent: retrying the same prompt against the same model is not
/// expected to help, so the article is flagged rather than requeued.
pub fn parse_analysis(raw: &str) -> CoreResult<Analysis> {
    let obj = parse_lenient_object(raw)
        .map_err(|e| CoreError::AnalysisParse(format!("no analysis object: {e}")))?;

    let entities: Vec<ExtractedEntity> = obj
        .get("entities")
        .and_then(|e| e.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(as_entity)
                .take(MAX_ENTITIES_PER_ARTICLE)
                .collect()
        })
        .unwrap_or_default();

    let analysis = Analysis {
        summary: as_prose(obj.get("summary")),
        critical_analysis: as_prose(
            obj.get("critical_analysis")
                .or_else(|| obj.get("criticalAnalysis")),
        ),
        fallacy_analysis: as_prose(
            obj.get("fallacy_analysis")
                .or_else(|| obj.get("fallacyAnalysis")),
        ),
        source_analysis: as_prose(
            obj.get("source_analysis")
                .or_else(|| obj.get("sourceAnalysis")),
        ),
        entities,
    };

    if analysis.summary.is_empty() && analysis.entities.is_empty() {
        return Err(CoreError::AnalysisParse(format!(
            "analysis object carried neither summary nor entities: {obj}"
        )));
    }
    Ok(analysis)
}

/// The outcome of one analyze call.
#[derive(Debug, Clone, PartialEq)]
pub struct AnalyzeOutcome {
    /// The parsed analysis.
    pub analysis: Analysis,
    /// The model's raw response, kept verbatim so the `analysis` column can
    /// hold what the model actually said. When a later prompt or parser change
    /// looks wrong, the raw text is the only way to tell which of the two
    /// moved.
    pub raw: String,
    /// Host-reported dollar cost of the call; `None` when unpriced.
    pub cost_estimate: Option<f64>,
}

/// Run one analyze call.
///
/// Makes **exactly one** `chat` call. A provider failure propagates as a
/// transient [`CoreError::Provider`] (the worker retries); an unparseable
/// response propagates as a permanent [`CoreError::AnalysisParse`] (the worker
/// records the article as failed without retrying).
///
/// # Errors
///
/// Propagates provider (transient) and parse (permanent) errors; callers use
/// [`CoreError::is_transient`] to pick retry versus record-and-stop.
pub fn analyze<P: LlmProvider + ?Sized>(
    provider: &P,
    model: Option<String>,
    title: &str,
    body: &str,
) -> CoreResult<AnalyzeOutcome> {
    let req = build_request(model, title, body);
    let resp = provider.chat(&req)?;
    let analysis = parse_analysis(&resp.content)?;
    Ok(AnalyzeOutcome {
        analysis,
        raw: resp.content,
        cost_estimate: resp.cost_estimate,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::provider::{MockProvider, Scripted};

    const CLEAN: &str = r#"{
        "summary": "A chip maker reported record revenue.",
        "critical_analysis": "The piece leans on company guidance.",
        "fallacy_analysis": "Appeal to authority in paragraph four.",
        "source_analysis": "Only the CFO is quoted.",
        "entities": [
            {"name": "Nvidia", "type": "company"},
            {"name": "Jensen Huang", "type": "person"}
        ]
    }"#;

    #[test]
    fn parses_a_clean_response() {
        let a = parse_analysis(CLEAN).unwrap();
        assert_eq!(a.summary, "A chip maker reported record revenue.");
        assert_eq!(a.critical_analysis, "The piece leans on company guidance.");
        assert_eq!(a.fallacy_analysis, "Appeal to authority in paragraph four.");
        assert_eq!(a.source_analysis, "Only the CFO is quoted.");
        assert_eq!(a.entities.len(), 2);
        assert_eq!(a.entities[0].entity_type, EntityType::Company);
        assert_eq!(a.entities[1].entity_type, EntityType::Person);
    }

    #[test]
    fn parses_fenced_output_with_preamble_and_trailing_prose() {
        let raw = format!("Sure, here you go:\n```json\n{CLEAN}\n```\nLet me know!");
        let a = parse_analysis(&raw).unwrap();
        assert_eq!(a.entities.len(), 2);
    }

    #[test]
    fn accepts_camel_case_keys() {
        let raw = r#"{"summary": "s", "criticalAnalysis": "c", "fallacyAnalysis": "f",
                      "sourceAnalysis": "so", "entities": []}"#;
        let a = parse_analysis(raw).unwrap();
        assert_eq!(a.critical_analysis, "c");
        assert_eq!(a.fallacy_analysis, "f");
        assert_eq!(a.source_analysis, "so");
    }

    #[test]
    fn accepts_prose_delivered_as_a_list_or_object() {
        let raw = r#"{"summary": ["First sentence.", "Second sentence."],
                      "critical_analysis": {"text": "wrapped"},
                      "entities": []}"#;
        let a = parse_analysis(raw).unwrap();
        assert_eq!(a.summary, "First sentence. Second sentence.");
        assert_eq!(a.critical_analysis, "wrapped");
    }

    #[test]
    fn accepts_entities_as_bare_strings_and_alternate_keys() {
        let raw = r#"{"summary": "s", "entities": ["Nvidia", {"entity": "Intel", "kind": "org"},
                      {"name": "  ", "type": "company"}, 42, null]}"#;
        let a = parse_analysis(raw).unwrap();
        assert_eq!(a.entities.len(), 2, "blank and non-entity entries dropped");
        assert_eq!(a.entities[0].name, "Nvidia");
        assert_eq!(a.entities[0].entity_type, EntityType::Other);
        assert_eq!(a.entities[1].name, "Intel");
        assert_eq!(a.entities[1].entity_type, EntityType::Company);
    }

    #[test]
    fn missing_prose_fields_default_to_empty() {
        let a = parse_analysis(r#"{"summary": "just a summary"}"#).unwrap();
        assert_eq!(a.summary, "just a summary");
        assert!(a.critical_analysis.is_empty());
        assert!(a.fallacy_analysis.is_empty());
        assert!(a.source_analysis.is_empty());
        assert!(a.entities.is_empty());
    }

    #[test]
    fn entities_only_is_enough() {
        let a = parse_analysis(r#"{"entities": [{"name": "Intel", "type": "company"}]}"#).unwrap();
        assert!(a.summary.is_empty());
        assert_eq!(a.entities.len(), 1);
    }

    #[test]
    fn entity_list_is_capped() {
        let many: Vec<String> = (0..100).map(|i| format!("\"Entity {i}\"")).collect();
        let raw = format!(r#"{{"summary": "s", "entities": [{}]}}"#, many.join(","));
        let a = parse_analysis(&raw).unwrap();
        assert_eq!(a.entities.len(), MAX_ENTITIES_PER_ARTICLE);
    }

    #[test]
    fn entities_as_a_string_instead_of_an_array_is_ignored_not_fatal() {
        let a = parse_analysis(r#"{"summary": "s", "entities": "Nvidia, Intel"}"#).unwrap();
        assert!(a.entities.is_empty());
        assert_eq!(a.summary, "s");
    }

    #[test]
    fn no_json_object_is_permanent() {
        let err = parse_analysis("I read the article and it seemed fine.").unwrap_err();
        assert!(!err.is_transient());
        assert!(matches!(err, CoreError::AnalysisParse(_)));
    }

    #[test]
    fn empty_analysis_object_is_permanent() {
        let err = parse_analysis(r#"{"critical_analysis": "", "entities": []}"#).unwrap_err();
        assert!(!err.is_transient());
    }

    #[test]
    fn truncated_output_is_permanent_not_a_panic() {
        // A generation cut mid-object leaves no balanced braces.
        let err = parse_analysis(r#"{"summary": "it started well", "entities": [{"name": "Nv"#)
            .unwrap_err();
        assert!(matches!(err, CoreError::AnalysisParse(_)));
    }

    #[test]
    fn body_is_capped_at_the_word_limit() {
        let body = (0..3000)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(
            first_words(&body).split_whitespace().count(),
            ANALYZE_BODY_WORDS
        );
    }

    #[test]
    fn request_carries_the_system_prompt_and_routes_the_model() {
        let req = build_request(Some("strong".into()), "T", "b");
        assert_eq!(req.model.as_deref(), Some("strong"));
        assert!(req.system.unwrap().contains("critical_analysis"));
        assert!(req.user.contains("Article title: T"));
    }

    #[test]
    fn analyze_makes_exactly_one_call_and_threads_cost_and_raw() {
        let mock = MockProvider::chat_once(CLEAN).with_chat_cost(0.004);
        let out = analyze(&mock, Some("strong".into()), "T", "body").unwrap();
        assert_eq!(out.analysis.entities.len(), 2);
        assert_eq!(out.cost_estimate, Some(0.004));
        assert_eq!(out.raw, CLEAN, "the model's own words are kept verbatim");
        assert_eq!(mock.chat_calls(), 1, "exactly one AI call per analyze");
    }

    #[test]
    fn analyze_provider_error_is_transient_one_call() {
        let mock = MockProvider::new(vec![Scripted::ChatError("503".into())]);
        let err = analyze(&mock, None, "T", "b").unwrap_err();
        assert!(err.is_transient());
        assert_eq!(mock.chat_calls(), 1);
    }

    #[test]
    fn analyze_unparseable_is_permanent_one_call() {
        let mock = MockProvider::chat_once("no json at all");
        let err = analyze(&mock, None, "T", "b").unwrap_err();
        assert!(!err.is_transient());
        assert_eq!(mock.chat_calls(), 1);
    }
}
