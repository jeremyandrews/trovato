#![allow(clippy::unwrap_used, clippy::expect_used)]
//! A call is priced on the model it asked for, not the one the provider served.
//!
//! Pricing used to read `ai_response.model`, the model string parsed out of the
//! provider's own response. Anthropic resolves an alias server side, so a
//! request for `claude-haiku-4-5` comes back as `claude-haiku-4-5-20251001`
//! and matched no row in a pricing table an operator keys by the name they
//! configured. Every call logged unpriced: `ai_usage_log.cost_estimate` NULL,
//! the currency cap summing NULL rows as zero and so enforcing nothing, and the
//! spend reading zero while real money was being spent. Reading zero is worse
//! than reading nothing, because zero looks like an answer.
//!
//! This drives the **real** host AI path: the real `test_ai_background` wasm
//! calls the real `ai-request` host function against a provider configured to
//! point at a local server that answers the way Anthropic does, and the
//! assertions are on the row the kernel wrote to `ai_usage_log`.
//!
//! Build the wasm first:
//!
//! ```text
//! cargo build -p test_ai_background --target wasm32-wasip1 --release \
//!   && cp target/wasm32-wasip1/release/test_ai_background.wasm plugins/test_ai_background/
//! ```

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Json;
use axum::routing::post;
use sqlx::PgPool;

use trovato_kernel::models::SiteConfig;
use trovato_kernel::plugin::{PluginConfig, PluginRuntime};
use trovato_kernel::services::ai_provider::{
    AiDefaults, AiOperationType, AiProviderConfig, AiProviderService, OperationModel,
    ProviderProtocol,
};
use trovato_kernel::services::ai_token_budget::{
    AiPricingConfig, AiTokenBudgetService, ModelPrice, UsageLogEntry,
};
use trovato_kernel::tap::{RequestServices, RequestState, TapDispatcher, TapRegistry, UserContext};

const FIXTURE: &str = "test_ai_background";

/// What the operator configures, and therefore what the pricing table is keyed
/// by.
const REQUESTED_MODEL: &str = "claude-haiku-4-5";

/// What Anthropic answers with once it has resolved the alias server side.
const SERVED_MODEL: &str = "claude-haiku-4-5-20251001";

const PROMPT_TOKENS: u32 = 2_000;
const COMPLETION_TOKENS: u32 = 1_000;

/// Prices per 1,000 tokens, chosen so the expected cost is exact in binary.
const INPUT_PER_1K: f64 = 0.25;
const OUTPUT_PER_1K: f64 = 1.25;

fn plugins_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("plugins")
}

fn dispatcher_with_fixture() -> Arc<TapDispatcher> {
    let mut runtime = PluginRuntime::new(&PluginConfig::default()).expect("create runtime");
    runtime
        .load_plugin(&plugins_dir().join(FIXTURE))
        .unwrap_or_else(|e| {
            panic!(
                "failed to load fixture '{FIXTURE}': {e:#}\n\
                 build it first: cargo build -p {FIXTURE} --target wasm32-wasip1 --release \
                 && cp target/wasm32-wasip1/release/{FIXTURE}.wasm plugins/{FIXTURE}/"
            )
        });
    let runtime = Arc::new(runtime);
    let registry = Arc::new(TapRegistry::from_plugins(&runtime));
    Arc::new(TapDispatcher::new(runtime, registry))
}

/// A local stand-in for the Anthropic Messages API that answers with a model
/// string the caller did not ask for, the way alias resolution does.
async fn serve_anthropic_answering_with(served_model: &'static str) -> String {
    let app = axum::Router::new().route(
        "/messages",
        post(move || async move {
            Json(serde_json::json!({
                "id": "msg_test",
                "type": "message",
                "role": "assistant",
                "model": served_model,
                "stop_reason": "end_turn",
                "content": [{ "type": "text", "text": "pong" }],
                "usage": {
                    "input_tokens": PROMPT_TOKENS,
                    "output_tokens": COMPLETION_TOKENS,
                },
            }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

/// Point the Chat operation at `base_url`, asking for [`REQUESTED_MODEL`].
async fn configure_provider(providers: &AiProviderService, provider_id: &str, base_url: String) {
    providers
        .save_provider(AiProviderConfig {
            id: provider_id.to_string(),
            label: "Pricing test provider".to_string(),
            protocol: ProviderProtocol::Anthropic,
            base_url,
            // Deliberately an environment variable nothing sets: the request
            // builder simply omits the key header, and the local server does
            // not check one.
            api_key_env: "TROVATO_AI_COST_TEST_KEY_UNSET".to_string(),
            models: vec![OperationModel {
                operation: AiOperationType::Chat,
                model: REQUESTED_MODEL.to_string(),
            }],
            rate_limit_rpm: 0,
            enabled: true,
        })
        .await
        .expect("save provider");

    let mut defaults = AiDefaults::default();
    defaults
        .defaults
        .insert(AiOperationType::Chat, provider_id.to_string());
    providers
        .save_defaults(defaults)
        .await
        .expect("save defaults");
}

async fn configure_pricing(budgets: &AiTokenBudgetService) {
    let mut pricing = AiPricingConfig::default();
    pricing.models.insert(
        REQUESTED_MODEL.to_string(),
        ModelPrice {
            input_per_1k: INPUT_PER_1K,
            output_per_1k: OUTPUT_PER_1K,
            currency: "USD".to_string(),
        },
    );
    budgets
        .save_pricing_config(&pricing)
        .await
        .expect("save pricing");
}

/// The two site-wide keys this file overwrites, so they can be put back.
///
/// `ai_providers` and `ai_defaults` are single site-wide rows, so pointing the
/// Chat operation at a local server is necessarily a global edit. Leaving a
/// provider behind that points at a port nothing is listening on any more is
/// how one test target breaks the next one on a shared database.
const SITE_KEYS: [&str; 2] = ["ai_providers", "ai_defaults"];

async fn snapshot_site_keys(db: &PgPool) -> Vec<(&'static str, Option<serde_json::Value>)> {
    let mut saved = Vec::new();
    for key in SITE_KEYS {
        saved.push((
            key,
            SiteConfig::get(db, key).await.expect("read site config"),
        ));
    }
    saved
}

async fn restore_site_keys(db: &PgPool, saved: Vec<(&'static str, Option<serde_json::Value>)>) {
    for (key, value) in saved {
        match value {
            Some(v) => SiteConfig::set(db, key, v)
                .await
                .expect("restore site config"),
            None => {
                sqlx::query("DELETE FROM site_config WHERE key = $1")
                    .bind(key)
                    .execute(db)
                    .await
                    .expect("clear site config");
            }
        }
    }
}

/// The newest usage row this fixture wrote: `(model, cost_estimate)`.
async fn newest_usage_row(db: &PgPool) -> (String, Option<f64>) {
    sqlx::query_as(
        "SELECT model, cost_estimate FROM ai_usage_log \
         WHERE plugin_name = $1 ORDER BY created DESC, id DESC LIMIT 1",
    )
    .bind(FIXTURE)
    .fetch_one(db)
    .await
    .expect("the host must have written a usage row")
}

/// **The finding, closed.** A provider that resolves an alias server side still
/// produces a priced call.
#[test]
fn an_aliased_model_is_priced_from_the_model_that_was_requested() {
    common::run_test(async {
        let app = common::shared_app().await;
        let db = app.db.clone();

        // Start from a clean slate for this fixture's rows.
        sqlx::query("DELETE FROM ai_usage_log WHERE plugin_name = $1")
            .bind(FIXTURE)
            .execute(&db)
            .await
            .unwrap();

        let saved = snapshot_site_keys(&db).await;

        let providers = Arc::new(AiProviderService::new(db.clone()));
        let budgets = Arc::new(AiTokenBudgetService::new(db.clone()));
        let base_url = serve_anthropic_answering_with(SERVED_MODEL).await;
        configure_provider(&providers, "ai-cost-test", base_url).await;
        configure_pricing(&budgets).await;

        let dispatcher = dispatcher_with_fixture();
        let services = RequestServices::for_background(
            db.clone(),
            Some(providers),
            Some(budgets),
            reqwest::Client::new(),
        )
        .with_plugin_runtime(dispatcher.runtime().clone());
        let state = RequestState::new(UserContext::background(), services);

        let result = dispatcher
            .dispatch_to_plugin("tap_cron", "{}", FIXTURE, state)
            .await
            .expect("fixture implements tap_cron");

        let output: serde_json::Value =
            serde_json::from_str(&result.output).expect("fixture output is JSON");
        assert!(
            output.get("ok").is_some(),
            "the provider must have answered; got {output}"
        );

        let (model, cost) = newest_usage_row(&db).await;
        restore_site_keys(&db, saved).await;

        // The resolved string stays in the log, where it says what actually ran.
        assert_eq!(
            model, SERVED_MODEL,
            "the log must keep the model the provider reported serving"
        );

        // And the call is priced, from the name the operator configured.
        let expected = (f64::from(PROMPT_TOKENS) / 1000.0) * INPUT_PER_1K
            + (f64::from(COMPLETION_TOKENS) / 1000.0) * OUTPUT_PER_1K;
        let cost = cost.expect(
            "an aliased model must still be priced; a NULL cost here is the call reading as free",
        );
        assert!(
            (cost - expected).abs() < 1e-9,
            "expected {expected}, got {cost}"
        );
    });
}

/// **The other half.** An unpriced call is not allowed to read as zero spend.
///
/// A NULL `cost_estimate` sums as zero, so a period of unpriced calls and a
/// period of no calls produced the same figure, and a currency cap checked
/// against it enforced nothing without saying so.
#[test]
fn unpriced_calls_do_not_read_as_zero_spend() {
    common::run_test(async {
        let app = common::shared_app().await;
        let db = app.db.clone();
        let plugin = "ai_cost_unpriced_probe";
        let provider = "ai-cost-unpriced-provider";

        sqlx::query("DELETE FROM ai_usage_log WHERE plugin_name = $1")
            .bind(plugin)
            .execute(&db)
            .await
            .unwrap();

        let budgets = AiTokenBudgetService::new(db.clone());

        // Three background calls the kernel could not price.
        for _ in 0..3 {
            budgets
                .record_usage(
                    &db,
                    UsageLogEntry {
                        user_id: None,
                        plugin_name: plugin.to_string(),
                        provider_id: provider.to_string(),
                        operation: "Chat".to_string(),
                        model: SERVED_MODEL.to_string(),
                        prompt_tokens: PROMPT_TOKENS as i32,
                        completion_tokens: COMPLETION_TOKENS as i32,
                        total_tokens: (PROMPT_TOKENS + COMPLETION_TOKENS) as i32,
                        latency_ms: 1,
                        cost_estimate: None,
                    },
                )
                .await
                .expect("record usage");
        }

        let since = 0;
        let spend = budgets
            .get_plugin_cost_for_period(&db, plugin, provider, since)
            .await
            .expect("cost for period");
        let unpriced = budgets
            .get_plugin_unpriced_calls_for_period(&db, plugin, provider, since)
            .await
            .expect("unpriced count");

        // The spend figure itself is still zero — the kernel does not know what
        // those calls cost and will not invent a number. What it must not do is
        // present that zero as the whole story.
        assert_eq!(spend, 0.0);
        assert_eq!(
            unpriced, 3,
            "three calls the kernel could not price must be countable; otherwise a cap checked \
             against a spend of 0.0 cannot tell 'nothing was spent' from 'nothing was measured'"
        );

        sqlx::query("DELETE FROM ai_usage_log WHERE plugin_name = $1")
            .bind(plugin)
            .execute(&db)
            .await
            .unwrap();
    });
}
