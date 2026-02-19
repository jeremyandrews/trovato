//! Form service for building, processing, and AJAX handling.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;
use tower_sessions::Session;
use tracing::{debug, warn};

use crate::tap::{RequestState, TapDispatcher};
use crate::theme::ThemeEngine;

use super::ajax::{AjaxRequest, AjaxResponse};
use super::csrf::{generate_csrf_token, verify_csrf_token};
use super::types::{Form, FormElement};

/// Form service for managing forms.
pub struct FormService {
    pool: PgPool,
    dispatcher: Arc<TapDispatcher>,
    theme: Arc<ThemeEngine>,
}

impl FormService {
    /// Create a new form service.
    pub fn new(pool: PgPool, dispatcher: Arc<TapDispatcher>, theme: Arc<ThemeEngine>) -> Self {
        Self {
            pool,
            dispatcher,
            theme,
        }
    }

    /// Build a form by ID.
    ///
    /// This calls the form builder and then invokes `tap_form_alter` for plugins
    /// to modify the form.
    pub async fn build(
        &self,
        form_id: &str,
        session: &Session,
        state: &RequestState,
    ) -> Result<Form> {
        // Generate CSRF token
        let token = generate_csrf_token(session).await?;

        // Create base form
        let mut form = Form::new(form_id);
        form.token = token;

        // Invoke tap_form_alter for plugins to modify the form
        let form_json = serde_json::to_string(&form)?;
        let results = self
            .dispatcher
            .dispatch("tap_form_alter", &form_json, state.clone())
            .await;

        // Apply alterations from plugins
        for result in results {
            if result.output.is_empty() || result.output == "{}" {
                continue;
            }

            match serde_json::from_str::<Form>(&result.output) {
                Ok(altered_form) => {
                    debug!(
                        plugin = %result.plugin_name,
                        form_id = %form_id,
                        "form altered by plugin"
                    );
                    form = altered_form;
                }
                Err(e) => {
                    warn!(
                        plugin = %result.plugin_name,
                        error = %e,
                        "failed to parse form alteration"
                    );
                }
            }
        }

        Ok(form)
    }

    /// Process a form submission.
    pub async fn process(
        &self,
        form_id: &str,
        values: &HashMap<String, Value>,
        session: &Session,
        state: &RequestState,
    ) -> Result<FormResult> {
        // Verify CSRF token
        let csrf_token = values.get("_token").and_then(|v| v.as_str()).unwrap_or("");

        if !verify_csrf_token(session, csrf_token).await? {
            return Ok(FormResult::ValidationFailed(vec![ValidationError {
                field: None,
                message: "Invalid or expired form token. Please try again.".to_string(),
            }]));
        }

        // Run built-in validation
        let mut errors = self.validate_form(form_id, values, state).await?;

        // Run tap_form_validate for plugin validation
        let validate_input = serde_json::json!({
            "form_id": form_id,
            "values": values,
        });

        let results = self
            .dispatcher
            .dispatch(
                "tap_form_validate",
                &serde_json::to_string(&validate_input)?,
                state.clone(),
            )
            .await;

        for result in results {
            if result.output.is_empty() || result.output == "{}" {
                continue;
            }

            if let Ok(tap_errors) = serde_json::from_str::<TapValidationResult>(&result.output) {
                for error in tap_errors.errors {
                    errors.push(ValidationError {
                        field: None,
                        message: error,
                    });
                }
            }
        }

        // If there are errors, return them
        if !errors.is_empty() {
            return Ok(FormResult::ValidationFailed(errors));
        }

        // Call tap_form_submit for side effects
        let submit_input = serde_json::json!({
            "form_id": form_id,
            "values": values,
        });

        self.dispatcher
            .dispatch(
                "tap_form_submit",
                &serde_json::to_string(&submit_input)?,
                state.clone(),
            )
            .await;

        // Default to success redirect
        Ok(FormResult::Success)
    }

    /// Handle an AJAX callback.
    pub async fn ajax_callback(
        &self,
        request: &AjaxRequest,
        _session: &Session,
        state: &RequestState,
    ) -> Result<AjaxResponse> {
        // Load form state
        let form_state = self.load_state(&request.form_build_id).await?;

        let Some(mut form_state) = form_state else {
            return Ok(AjaxResponse::new().alert("Form session expired. Please reload the page."));
        };

        // Update form state with current values
        form_state
            .values
            .extend(request.values.iter().map(|(k, v)| (k.clone(), v.clone())));

        // Handle the trigger
        let response = self
            .handle_ajax_trigger(&request.trigger, &mut form_state, state)
            .await?;

        // Save updated state
        self.save_state(&request.form_build_id, &form_state).await?;

        Ok(response)
    }

    /// Handle an AJAX trigger.
    async fn handle_ajax_trigger(
        &self,
        trigger: &str,
        state: &mut FormState,
        request_state: &RequestState,
    ) -> Result<AjaxResponse> {
        // Check for built-in triggers
        if trigger.starts_with("add_") {
            // "Add another item" pattern
            let field_name = trigger.strip_prefix("add_").unwrap_or(trigger);
            return self.handle_add_item(field_name, state).await;
        }

        if trigger.starts_with("remove_") {
            let parts: Vec<&str> = trigger
                .strip_prefix("remove_")
                .unwrap_or(trigger)
                .split('_')
                .collect();
            if parts.len() >= 2 {
                let field_name = parts[0];
                let index: usize = parts[1].parse().unwrap_or(0);
                return self.handle_remove_item(field_name, index, state).await;
            }
        }

        // Default: invoke tap for custom handlers
        let callback_input = serde_json::json!({
            "trigger": trigger,
            "form_id": state.form_id,
            "values": state.values,
        });

        let results = self
            .dispatcher
            .dispatch(
                "tap_form_ajax",
                &serde_json::to_string(&callback_input)?,
                request_state.clone(),
            )
            .await;

        // Combine responses
        let mut response = AjaxResponse::new();
        for result in results {
            if result.output.is_empty() || result.output == "{}" {
                continue;
            }

            if let Ok(ajax_response) = serde_json::from_str::<AjaxResponse>(&result.output) {
                response.commands.extend(ajax_response.commands);
            }
        }

        Ok(response)
    }

    /// Handle "Add another item" for multi-value fields.
    async fn handle_add_item(
        &self,
        field_name: &str,
        state: &mut FormState,
    ) -> Result<AjaxResponse> {
        // Increment item count
        let count_key = format!("{field_name}_count");
        let current_count = state
            .extra
            .get(&count_key)
            .and_then(|v| v.as_u64())
            .unwrap_or(1) as usize;

        let new_count = current_count + 1;
        state
            .extra
            .insert(count_key, Value::Number(new_count.into()));

        // Generate HTML for new item
        let new_item_html = self.render_multi_value_item(field_name, new_count - 1)?;

        Ok(AjaxResponse::new()
            .append(format!("#{field_name}-wrapper"), new_item_html)
            .invoke(
                "Trovato.updateFieldDelta",
                serde_json::json!({
                    "field": field_name,
                    "count": new_count
                }),
            ))
    }

    /// Handle "Remove item" for multi-value fields.
    async fn handle_remove_item(
        &self,
        field_name: &str,
        index: usize,
        state: &mut FormState,
    ) -> Result<AjaxResponse> {
        // Decrement item count
        let count_key = format!("{field_name}_count");
        let current_count = state
            .extra
            .get(&count_key)
            .and_then(|v| v.as_u64())
            .unwrap_or(1) as usize;

        if current_count > 0 {
            state
                .extra
                .insert(count_key, Value::Number((current_count - 1).into()));
        }

        Ok(AjaxResponse::new().remove(format!("#{field_name}-{index}")))
    }

    /// Render a multi-value field item.
    fn render_multi_value_item(&self, field_name: &str, index: usize) -> Result<String> {
        let element = FormElement::textfield().title(format!("Item {}", index + 1));

        let mut context = tera::Context::new();
        context.insert("name", &format!("{field_name}[{index}]"));
        context.insert("element", &element);
        context.insert("index", &index);
        context.insert("field_name", field_name);

        self.theme
            .tera()
            .render("form/multi-value-item.html", &context)
            .or_else(|_| {
                // Fallback to inline rendering
                Ok(format!(
                    r#"<div id="{field_name}-{index}" class="multi-value-item">
                        <input type="text" name="{field_name}[{index}]" class="form-text" />
                        <button type="button" class="remove-item" data-ajax-trigger="remove_{field_name}_{index}">Remove</button>
                    </div>"#
                ))
            })
    }

    /// Validate form values.
    async fn validate_form(
        &self,
        _form_id: &str,
        _values: &HashMap<String, Value>,
        _state: &RequestState,
    ) -> Result<Vec<ValidationError>> {
        // TODO: Load form definition and validate required fields
        Ok(Vec::new())
    }

    /// Save form state for AJAX/multi-step forms.
    pub async fn save_state(&self, form_build_id: &str, state: &FormState) -> Result<()> {
        let state_json = serde_json::to_value(state)?;
        let now = chrono::Utc::now().timestamp();

        sqlx::query(
            r#"
            INSERT INTO form_state_cache (form_build_id, form_id, state, created, updated)
            VALUES ($1, $2, $3, $4, $4)
            ON CONFLICT (form_build_id) DO UPDATE
            SET state = EXCLUDED.state, updated = EXCLUDED.updated
            "#,
        )
        .bind(form_build_id)
        .bind(&state.form_id)
        .bind(&state_json)
        .bind(now)
        .execute(&self.pool)
        .await
        .context("failed to save form state")?;

        Ok(())
    }

    /// Load form state.
    pub async fn load_state(&self, form_build_id: &str) -> Result<Option<FormState>> {
        let row: Option<(Value,)> =
            sqlx::query_as("SELECT state FROM form_state_cache WHERE form_build_id = $1")
                .bind(form_build_id)
                .fetch_optional(&self.pool)
                .await
                .context("failed to load form state")?;

        let Some((state_json,)) = row else {
            return Ok(None);
        };

        let state: FormState =
            serde_json::from_value(state_json).context("failed to deserialize form state")?;

        Ok(Some(state))
    }

    /// Clean up expired form states.
    pub async fn cleanup_expired(&self) -> Result<u64> {
        let cutoff = chrono::Utc::now().timestamp() - 6 * 3600; // 6 hours

        let result = sqlx::query("DELETE FROM form_state_cache WHERE updated < $1")
            .bind(cutoff)
            .execute(&self.pool)
            .await
            .context("failed to clean up expired form states")?;

        Ok(result.rows_affected())
    }
}

impl std::fmt::Debug for FormService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FormService").finish()
    }
}

/// Form state stored for AJAX/multi-step forms.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormState {
    /// Form ID.
    pub form_id: String,

    /// Form build ID.
    pub form_build_id: String,

    /// Current form values.
    pub values: HashMap<String, Value>,

    /// Current step (for multi-step forms).
    #[serde(default)]
    pub step: usize,

    /// Extra state data (e.g., item counts for multi-value fields).
    #[serde(default)]
    pub extra: HashMap<String, Value>,
}

impl FormState {
    /// Create a new form state.
    pub fn new(form_id: impl Into<String>, form_build_id: impl Into<String>) -> Self {
        Self {
            form_id: form_id.into(),
            form_build_id: form_build_id.into(),
            values: HashMap::new(),
            step: 0,
            extra: HashMap::new(),
        }
    }
}

/// Result of form processing.
#[derive(Debug)]
pub enum FormResult {
    /// Form processed successfully.
    Success,

    /// Redirect to a URL.
    Redirect(String),

    /// Re-display form with errors.
    ValidationFailed(Vec<ValidationError>),

    /// Rebuild form (e.g., after AJAX update).
    Rebuild(Box<Form>),

    /// AJAX response.
    Ajax(AjaxResponse),
}

/// Validation error.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationError {
    /// Field name (None for form-level errors).
    pub field: Option<String>,

    /// Error message.
    pub message: String,
}

impl ValidationError {
    /// Create a field-level error.
    pub fn field(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: Some(name.into()),
            message: message.into(),
        }
    }

    /// Create a form-level error.
    pub fn form(message: impl Into<String>) -> Self {
        Self {
            field: None,
            message: message.into(),
        }
    }
}

/// Response from tap_form_validate.
#[derive(Debug, Deserialize)]
struct TapValidationResult {
    #[serde(default)]
    errors: Vec<String>,
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_form_state_new() {
        let state = FormState::new("test_form", "build-123");
        assert_eq!(state.form_id, "test_form");
        assert_eq!(state.form_build_id, "build-123");
        assert_eq!(state.step, 0);
    }

    #[test]
    fn test_validation_error() {
        let field_error = ValidationError::field("email", "Invalid email");
        assert_eq!(field_error.field, Some("email".to_string()));

        let form_error = ValidationError::form("Form expired");
        assert!(form_error.field.is_none());
    }

    #[test]
    fn test_form_state_serialization() {
        let mut state = FormState::new("test", "build-1");
        state
            .values
            .insert("name".to_string(), Value::String("John".to_string()));
        state.step = 2;

        let json = serde_json::to_string(&state).unwrap();
        let parsed: FormState = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.form_id, "test");
        assert_eq!(parsed.step, 2);
        assert_eq!(
            parsed.values.get("name"),
            Some(&Value::String("John".to_string()))
        );
    }
}
