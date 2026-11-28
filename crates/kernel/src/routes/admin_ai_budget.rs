//! Admin routes for AI token budget management.
//!
//! Both forms here, the budget configuration and a person's overrides, go
//! through `FormService` (B83c). Their grids of limits are drawn by a
//! template into a markup element of the form, so the table stays a table.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use tower_sessions::Session;
use uuid::Uuid;

use crate::content::submission::PostedForm;
use crate::form::{FormElement, ValidationError};
use crate::models::User;
use crate::services::ai_token_budget::{BudgetAction, BudgetConfig, BudgetPeriod};
use crate::state::AppState;

use super::admin_form;
use super::admin_nav::AdminText;
use super::helpers::{
    render_admin_template, render_error, render_not_found, render_server_error, require_permission,
    user_context_for,
};

/// Session key for flash messages on the budget page.
const FLASH_KEY: &str = "ai_budget_flash";

/// The budget page: usage and the configuration form.
const BASE: &str = "/admin/system/ai-budgets";

/// The budget configuration form.
const CONFIG_FORM_ID: &str = "ai_budget_config_form";

/// A person's override form.
const OVERRIDE_FORM_ID: &str = "ai_budget_override_form";

// =============================================================================
// Forms
// =============================================================================

/// A template's HTML in the page's language, for a markup element of a
/// form: the grid of limits on either page. `None` (logged) when the
/// template fails to render.
fn grid_html(
    state: &AppState,
    text: &AdminText,
    template: &str,
    context: &tera::Context,
) -> Option<String> {
    text.scope(|| state.theme().tera().render(template, context))
        .map_err(|e| tracing::error!(error = %e, template, "failed to render a budget grid"))
        .ok()
}

/// A Cancel link back to the budget page, for beside a submit button.
fn cancel_link(text: &AdminText) -> String {
    format!(
        r#"<a href="{BASE}" class="button button--secondary">{}</a>"#,
        text.html("Cancel", &[])
    )
}

/// The budget configuration form: the period, what happens at the limit, and
/// the grid of per-provider, per-role limits (`limits`, already rendered).
fn config_form(text: &AdminText, period: &str, action: &str, limits: String) -> crate::form::Form {
    crate::form::Form::new(CONFIG_FORM_ID)
        .action(BASE)
        .element(
            "period",
            FormElement::select(vec![
                ("daily".to_string(), "Daily".to_string()),
                ("weekly".to_string(), "Weekly".to_string()),
                ("monthly".to_string(), "Monthly".to_string()),
            ])
            .title("Budget Period")
            .default_value(period)
            .required()
            .weight(0),
        )
        .element(
            "action_on_limit",
            FormElement::select(vec![
                ("deny".to_string(), "Deny request".to_string()),
                ("warn".to_string(), "Allow with warning".to_string()),
            ])
            .title("Action When Limit Exceeded")
            .default_value(action)
            .required()
            .weight(1),
        )
        .element("limits", FormElement::markup(limits).weight(2))
        .element(
            "effect",
            super::admin_ai_chat::effect(
                text,
                "how much each role may spend on each AI provider in a period, and whether a \
                 request over the limit is refused or allowed with a warning, from the next \
                 request on.",
            )
            .weight(9),
        )
        .element(
            "op",
            FormElement::submit("Save configuration")
                .suffix(cancel_link(text))
                .weight(10),
        )
}

/// A person's override form: the grid of per-provider limits (`grid`,
/// already rendered).
fn override_form(text: &AdminText, user_id: Uuid, grid: String) -> crate::form::Form {
    crate::form::Form::new(OVERRIDE_FORM_ID)
        .action(format!("{BASE}/user/{user_id}"))
        .element("overrides", FormElement::markup(grid).weight(0))
        .element(
            "op",
            FormElement::submit("Save overrides")
                .suffix(cancel_link(text))
                .weight(10),
        )
}

/// The period and the action at the limit a configuration form posted, or
/// the problems with them.
fn read_config(posted: &PostedForm) -> Result<(BudgetPeriod, BudgetAction), Vec<ValidationError>> {
    let mut errors = Vec::new();
    let period = match posted.get("period").unwrap_or_default() {
        "daily" => Some(BudgetPeriod::Daily),
        "weekly" => Some(BudgetPeriod::Weekly),
        "monthly" => Some(BudgetPeriod::Monthly),
        _ => {
            errors.push(ValidationError::field("period", "Invalid budget period."));
            None
        }
    };
    let action = match posted.get("action_on_limit").unwrap_or_default() {
        "deny" => Some(BudgetAction::Deny),
        "warn" => Some(BudgetAction::Warn),
        _ => {
            errors.push(ValidationError::field(
                "action_on_limit",
                "Invalid action on limit.",
            ));
            None
        }
    };
    match (period, action) {
        (Some(period), Some(action)) => Ok((period, action)),
        _ => Err(errors),
    }
}

// =============================================================================
// Router
// =============================================================================

/// Build the budget admin routes.
pub fn router() -> Router<AppState> {
    Router::new()
        .route(BASE, get(budget_dashboard))
        .route(BASE, post(save_budget_config))
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
    let user = match require_permission(&state, &session, "view ai usage").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    render_dashboard(&state, &session, &user, None, &[]).await
}

/// Render the budget dashboard. `posted` is a configuration submission to
/// show again with its `errors`; `None` shows the stored configuration.
async fn render_dashboard(
    state: &AppState,
    session: &Session,
    user: &User,
    posted: Option<&PostedForm>,
    errors: &[ValidationError],
) -> Response {
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

    // The defaults grid for the template: provider_id -> role_name -> limit,
    // or what was posted when the form is shown again.
    let defaults_grid: serde_json::Value = match posted {
        Some(posted) => {
            let mut grid = serde_json::Map::new();
            for provider in &providers {
                let mut row = serde_json::Map::new();
                for role in &role_entries {
                    let role = role["name"].as_str().unwrap_or_default();
                    let value = posted
                        .get(&format!("limit_{}_{role}", provider.id))
                        .unwrap_or_default();
                    if !value.is_empty() {
                        row.insert(role.to_string(), value.into());
                    }
                }
                grid.insert(provider.id.clone(), row.into());
            }
            grid.into()
        }
        None => serde_json::to_value(&config.defaults).unwrap_or_else(|_| serde_json::json!({})),
    };

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

    let stored_period = config.period.to_string();
    let stored_action = serde_json::to_value(config.action_on_limit)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "deny".to_string());
    let (period, action) = match posted {
        Some(posted) => (
            posted.get("period").unwrap_or_default().to_string(),
            posted
                .get("action_on_limit")
                .unwrap_or_default()
                .to_string(),
        ),
        None => (stored_period, stored_action),
    };

    let viewer = user_context_for(state, user).await;
    let text = AdminText::for_page(state, &viewer, BASE).await;
    let mut grid = tera::Context::new();
    grid.insert("providers", &provider_entries);
    grid.insert("roles", &role_entries);
    grid.insert("defaults_grid", &defaults_grid);
    let Some(limits) = grid_html(state, &text, "admin/ai-budget-limits.html", &grid) else {
        return render_server_error("The form could not be displayed.");
    };
    let form = config_form(&text, &period, &action, limits);
    let form_html = match admin_form::render(state, session, user, form, errors).await {
        Ok(html) => html,
        Err(response) => return response,
    };

    let mut context = tera::Context::new();
    context.insert("period_label", config.period.label());
    context.insert("provider_usage", &provider_usage);
    context.insert("user_usage", &user_usage);
    context.insert("form", &form_html);
    context.insert("flash", &flash);
    context.insert("path", BASE);

    render_admin_template(state, &viewer, "admin/ai-budgets.html", context).await
}

/// Save budget configuration.
///
/// POST /admin/system/ai-budgets
async fn save_budget_config(
    State(state): State<AppState>,
    session: Session,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, "configure ai").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    let parsed = read_config(&posted);
    let errors = parsed.as_ref().err().cloned().unwrap_or_default();
    let form_state =
        match admin_form::check(&state, &session, &user, &posted, CONFIG_FORM_ID, errors).await {
            Ok(form_state) => form_state,
            Err(Ok(errors)) => {
                return render_dashboard(&state, &session, &user, Some(&posted), &errors).await;
            }
            Err(Err(response)) => return response,
        };
    let Ok((period, action_on_limit)) = parsed else {
        return render_not_found();
    };

    // Parse per-provider per-role limits from flat form fields
    // Fields are named: limit_{provider_id}_{role_name}
    let mut defaults: std::collections::HashMap<String, std::collections::HashMap<String, u64>> =
        std::collections::HashMap::new();

    for (key, value) in &posted.0 {
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

    // Preserve any per-plugin background budgets (P11c / D-42/D-44): this
    // role-budget form has no plugin fields, so carry the existing token and
    // currency maps forward rather than wiping them on save.
    let existing = state.ai_budgets().get_config().await.ok().flatten();
    let plugin_defaults = existing
        .as_ref()
        .map(|c| c.plugin_defaults.clone())
        .unwrap_or_default();
    let plugin_cost_defaults = existing.map(|c| c.plugin_cost_defaults).unwrap_or_default();

    let config = BudgetConfig {
        period,
        action_on_limit,
        defaults,
        plugin_defaults,
        plugin_cost_defaults,
    };

    if let Err(e) = state.ai_budgets().save_config(&config).await {
        tracing::error!(error = %e, "failed to save budget config");
        return render_server_error("Failed to save budget configuration.");
    }
    admin_form::submitted(&state, &user, &form_state, &posted).await;

    let _ = session
        .insert(FLASH_KEY, "Budget configuration saved.")
        .await;
    Redirect::to(BASE).into_response()
}

/// Per-user budget detail and override form.
///
/// GET /admin/system/ai-budgets/user/{id}
async fn user_budget_detail(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<String>,
) -> Response {
    let admin = match require_permission(&state, &session, "configure ai").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

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

    let flash: Option<String> = session.get(FLASH_KEY).await.ok().flatten();
    if flash.is_some() {
        let _ = session.remove::<String>(FLASH_KEY).await;
    }

    let path = format!("{BASE}/user/{user_id}");
    let viewer = user_context_for(&state, &admin).await;
    let text = AdminText::for_page(&state, &viewer, &path).await;
    let mut grid = tera::Context::new();
    grid.insert("user_name", &user.name);
    grid.insert("provider_usage", &user_provider_usage);
    let Some(grid) = grid_html(&state, &text, "admin/ai-budget-overrides.html", &grid) else {
        return render_server_error("The form could not be displayed.");
    };
    let form = override_form(&text, user_id, grid);
    let form_html = match admin_form::render(&state, &session, &admin, form, &[]).await {
        Ok(html) => html,
        Err(response) => return response,
    };

    let mut context = tera::Context::new();
    context.insert("user_id", &user_id.to_string());
    context.insert("user_name", &user.name);
    context.insert("period_label", config.period.label());
    context.insert("provider_usage", &user_provider_usage);
    context.insert("form", &form_html);
    context.insert("flash", &flash);
    context.insert("path", &path);

    render_admin_template(&state, &viewer, "admin/ai-budget-user.html", context).await
}

/// Save per-user budget overrides.
///
/// POST /admin/system/ai-budgets/user/{id}
async fn save_user_override(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<String>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let admin = match require_permission(&state, &session, "configure ai").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    let form_state = match admin_form::check(
        &state,
        &session,
        &admin,
        &posted,
        OVERRIDE_FORM_ID,
        Vec::new(),
    )
    .await
    {
        Ok(form_state) => form_state,
        Err(Ok(_)) => {
            return render_error("The form has expired. Reload the page and try again.");
        }
        Err(Err(response)) => return response,
    };

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
    for (key, value) in &posted.0 {
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
    admin_form::submitted(&state, &admin, &form_state, &posted).await;

    let _ = session
        .insert(FLASH_KEY, "User budget overrides saved.")
        .await;
    Redirect::to(&format!("{BASE}/user/{user_id}")).into_response()
}
