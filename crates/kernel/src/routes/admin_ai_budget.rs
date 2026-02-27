//! Admin routes for AI token budget management.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use serde::Deserialize;
use tower_sessions::Session;
use uuid::Uuid;

use crate::form::csrf::generate_csrf_token;
use crate::services::ai_token_budget::{BudgetAction, BudgetConfig, BudgetPeriod};
use crate::state::AppState;

use super::helpers::{
    render_admin_template, render_error, render_not_found, render_server_error, require_csrf,
    require_permission,
};

/// Session key for flash messages on the budget page.
const FLASH_KEY: &str = "ai_budget_flash";

// =============================================================================
// Form data
// =============================================================================

/// Budget configuration form.
#[derive(Debug, Deserialize)]
struct BudgetConfigForm {
    #[serde(rename = "_token")]
    token: String,
    #[serde(rename = "_form_build_id")]
    form_build_id: String,
    period: String,
    action_on_limit: String,
    /// Flat fields: `limit_{provider_id}_{role_name}` = token limit string.
    #[serde(flatten)]
    extra: std::collections::HashMap<String, String>,
}

/// Per-user override form.
#[derive(Debug, Deserialize)]
struct UserOverrideForm {
    #[serde(rename = "_token")]
    token: String,
    #[serde(rename = "_form_build_id")]
    form_build_id: String,
    /// Flat fields: `override_{provider_id}` = token limit string.
    #[serde(flatten)]
    extra: std::collections::HashMap<String, String>,
}

// =============================================================================
// Router
// =============================================================================

/// Build the budget admin routes.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/system/ai-budgets", get(budget_dashboard))
        .route("/admin/system/ai-budgets", post(save_budget_config))
        .route(
            "/admin/system/ai-budgets/user/{id}",
            get(user_budget_detail),
        )
        .route(
            "/admin/system/ai-budgets/user/{id}",
            post(save_user_override),
        )
}

// =============================================================================
// Handlers
// =============================================================================

/// Budget dashboard: usage stats + config form.
///
/// GET /admin/system/ai-budgets
async fn budget_dashboard(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "view ai usage").await {
        return redirect;
    }

    let budget_svc = state.ai_budgets();
    let config = match budget_svc.get_config().await {
        Ok(Some(c)) => c,
        Ok(None) => BudgetConfig::default(),
        Err(e) => {
            tracing::error!(error = %e, "failed to load budget config");
            return render_server_error("Failed to load budget configuration.");
        }
    };

    let since = config.period.period_start();

    // Load providers, roles, and usage data in parallel-safe order
    let providers = match state.ai_providers().list_providers().await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "failed to list providers for budget dashboard");
            return render_server_error("Failed to load providers.");
        }
    };

    let roles = match state.roles().list().await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "failed to list roles for budget dashboard");
            return render_server_error("Failed to load roles.");
        }
    };

    let usage_by_provider = match budget_svc.usage_by_provider(since).await {
        Ok(u) => u,
        Err(e) => {
            tracing::warn!(error = %e, "failed to load provider usage");
            Vec::new()
        }
    };

    let top_users = match budget_svc.usage_by_user(since, 20).await {
        Ok(u) => u,
        Err(e) => {
            tracing::warn!(error = %e, "failed to load user usage");
            Vec::new()
        }
    };

    let csrf_token = generate_csrf_token(&session).await;
    let form_build_id = uuid::Uuid::new_v4().to_string();

    // Read and clear flash
    let flash: Option<String> = session.get(FLASH_KEY).await.ok().flatten();
    if flash.is_some() {
        let _ = session.remove::<String>(FLASH_KEY).await;
    }

    // Build provider entries for the template
    let provider_entries: Vec<serde_json::Value> = providers
        .iter()
        .map(|p| {
            serde_json::json!({
                "id": p.id,
                "label": p.label,
            })
        })
        .collect();

    // Build role entries
    let role_entries: Vec<serde_json::Value> = {
        let mut rv: Vec<serde_json::Value> = roles
            .iter()
            .map(|r| {
                serde_json::json!({
                    "name": r.name,
                })
            })
            .collect();
        // Add "authenticated" pseudo-role
        rv.push(serde_json::json!({ "name": "authenticated" }));
        rv
    };

    // Build the defaults grid for the template: provider_id -> role_name -> limit
    let defaults_grid: serde_json::Value =
        serde_json::to_value(&config.defaults).unwrap_or_else(|_| serde_json::json!({}));

    // Usage by provider with labels
    let provider_usage: Vec<serde_json::Value> = usage_by_provider
        .iter()
        .map(|u| {
            let label = providers
                .iter()
                .find(|p| p.id == u.provider_id)
                .map(|p| p.label.as_str())
                .unwrap_or(&u.provider_id);
            serde_json::json!({
                "provider_id": u.provider_id,
                "label": label,
                "total_tokens": u.total_tokens,
                "request_count": u.request_count,
            })
        })
        .collect();

    // Top users
    let user_usage: Vec<serde_json::Value> = top_users
        .iter()
        .map(|u| {
            let provider_label = providers
                .iter()
                .find(|p| p.id == u.provider_id)
                .map(|p| p.label.as_str())
                .unwrap_or(&u.provider_id);
            serde_json::json!({
                "user_id": u.user_id,
                "user_name": u.user_name,
                "provider_id": u.provider_id,
                "provider_label": provider_label,
                "total_tokens": u.total_tokens,
                "request_count": u.request_count,
            })
        })
        .collect();

    let mut context = tera::Context::new();
    context.insert("config_period", &config.period.to_string());
    context.insert(
        "config_action",
        &serde_json::to_value(config.action_on_limit)
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_else(|| "deny".to_string()),
    );
    context.insert("period_label", config.period.label());
    context.insert("providers", &provider_entries);
    context.insert("roles", &role_entries);
    context.insert("defaults_grid", &defaults_grid);
    context.insert("provider_usage", &provider_usage);
    context.insert("user_usage", &user_usage);
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert("flash", &flash);
    context.insert("path", "/admin/system/ai-budgets");

    render_admin_template(&state, "admin/ai-budgets.html", context).await
}

/// Save budget configuration.
///
/// POST /admin/system/ai-budgets
async fn save_budget_config(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<BudgetConfigForm>,
) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "configure ai").await {
        return redirect;
    }
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    let period = match form.period.as_str() {
        "daily" => BudgetPeriod::Daily,
        "weekly" => BudgetPeriod::Weekly,
        "monthly" => BudgetPeriod::Monthly,
        _ => return render_error("Invalid budget period."),
    };

    let action_on_limit = match form.action_on_limit.as_str() {
        "deny" => BudgetAction::Deny,
        "warn" => BudgetAction::Warn,
        _ => return render_error("Invalid action on limit."),
    };

    // Parse per-provider per-role limits from flat form fields
    // Fields are named: limit_{provider_id}_{role_name}
    let mut defaults: std::collections::HashMap<String, std::collections::HashMap<String, u64>> =
        std::collections::HashMap::new();

    for (key, value) in &form.extra {
        if let Some(rest) = key.strip_prefix("limit_") {
            // Split on first underscore after provider_id
            // Provider IDs are UUIDs (36 chars), role names may contain underscores
            if rest.len() > 37 {
                let provider_id = &rest[..36];
                let role_name = &rest[37..];
                if !value.trim().is_empty()
                    && let Ok(limit) = value.trim().parse::<u64>()
                {
                    defaults
                        .entry(provider_id.to_string())
                        .or_default()
                        .insert(role_name.to_string(), limit);
                }
            }
        }
    }

    let config = BudgetConfig {
        period,
        action_on_limit,
        defaults,
    };

    if let Err(e) = state.ai_budgets().save_config(&config).await {
        tracing::error!(error = %e, "failed to save budget config");
        return render_server_error("Failed to save budget configuration.");
    }

    let _ = session
        .insert(FLASH_KEY, "Budget configuration saved.")
        .await;
    Redirect::to("/admin/system/ai-budgets").into_response()
}

/// Per-user budget detail and override form.
///
/// GET /admin/system/ai-budgets/user/{id}
async fn user_budget_detail(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<String>,
) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "configure ai").await {
        return redirect;
    }

    let Ok(user_id) = Uuid::parse_str(&id) else {
        return render_not_found();
    };

    // Load user info
    let user = match state.users().find_by_id(user_id).await {
        Ok(Some(u)) => u,
        Ok(None) => return render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to load user");
            return render_server_error("Failed to load user.");
        }
    };

    let budget_svc = state.ai_budgets();
    let config = budget_svc
        .get_config()
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    let since = config.period.period_start();

    let providers = match state.ai_providers().list_providers().await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "failed to list providers");
            return render_server_error("Failed to load providers.");
        }
    };

    let overrides = match budget_svc.get_all_user_overrides(state.db(), user_id).await {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!(error = %e, "failed to load user overrides");
            std::collections::HashMap::new()
        }
    };

    // Per-provider usage for this user
    let mut user_provider_usage: Vec<serde_json::Value> = Vec::new();
    for provider in &providers {
        let used = budget_svc
            .get_usage_for_period(state.db(), user_id, &provider.id, since)
            .await
            .unwrap_or(0);

        let override_limit = overrides.get(&provider.id).copied();

        user_provider_usage.push(serde_json::json!({
            "provider_id": provider.id,
            "label": provider.label,
            "used": used,
            "override": override_limit,
        }));
    }

    let csrf_token = generate_csrf_token(&session).await;
    let form_build_id = uuid::Uuid::new_v4().to_string();

    let flash: Option<String> = session.get(FLASH_KEY).await.ok().flatten();
    if flash.is_some() {
        let _ = session.remove::<String>(FLASH_KEY).await;
    }

    let mut context = tera::Context::new();
    context.insert("user_id", &user_id.to_string());
    context.insert("user_name", &user.name);
    context.insert("period_label", config.period.label());
    context.insert("provider_usage", &user_provider_usage);
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert("flash", &flash);
    context.insert("path", &format!("/admin/system/ai-budgets/user/{user_id}"));

    render_admin_template(&state, "admin/ai-budget-user.html", context).await
}

/// Save per-user budget overrides.
///
/// POST /admin/system/ai-budgets/user/{id}
async fn save_user_override(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<String>,
    Form(form): Form<UserOverrideForm>,
) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "configure ai").await {
        return redirect;
    }
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    let Ok(user_id) = Uuid::parse_str(&id) else {
        return render_not_found();
    };

    // Verify user exists
    match state.users().find_by_id(user_id).await {
        Ok(Some(_)) => {}
        Ok(None) => return render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to load user");
            return render_server_error("Failed to load user.");
        }
    }

    let budget_svc = state.ai_budgets();

    // Parse overrides from form: override_{provider_id} = limit or empty
    for (key, value) in &form.extra {
        if let Some(provider_id) = key.strip_prefix("override_") {
            if provider_id.is_empty() {
                continue;
            }
            let trimmed = value.trim();
            if trimmed.is_empty() {
                // Remove override
                if let Err(e) = budget_svc
                    .remove_user_override(state.db(), user_id, provider_id)
                    .await
                {
                    tracing::error!(error = %e, provider = %provider_id, "failed to remove user override");
                    return render_server_error("Failed to save user override.");
                }
            } else if let Ok(limit) = trimmed.parse::<u64>()
                && let Err(e) = budget_svc
                    .set_user_override(state.db(), user_id, provider_id, limit)
                    .await
            {
                tracing::error!(error = %e, provider = %provider_id, "failed to set user override");
                return render_server_error("Failed to save user override.");
            }
        }
    }

    let _ = session
        .insert(FLASH_KEY, "User budget overrides saved.")
        .await;
    Redirect::to(&format!("/admin/system/ai-budgets/user/{user_id}")).into_response()
}
