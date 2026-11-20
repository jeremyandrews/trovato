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

    /// Finish building `form`: mint its CSRF token, let plugins alter it, and
    /// save `form_state` with the inputs the altered form renders.
    ///
    /// `tap_form_alter` runs once per implementing plugin in weight order, each
    /// receiving the previous plugin's output, so two plugins altering one form
    /// compose. An empty or unparsable answer leaves the form as it was. A
    /// plugin cannot change the form's id, build id or token.
    ///
    /// The rendered input list ([`Form::rendered_inputs`]) is taken after
    /// alteration and kept server side in `form_state`; the submission builder
    /// writes those inputs and nothing else.
    pub async fn build(
        &self,
        mut form: Form,
        session: &Session,
        state: &RequestState,
        mut form_state: FormState,
    ) -> Result<Form> {
        form.token = generate_csrf_token(session).await;
        let (form_id, build_id, token) = (
            form.form_id.clone(),
            form.form_build_id.clone(),
            form.token.clone(),
        );

        for handler in self.dispatcher.registry().get_handlers("tap_form_alter") {
            let plugin = handler.plugin.info.name.clone();
            let input = serde_json::to_string(&form)?;
            let Some(result) = self
                .dispatcher
                .dispatch_to_plugin("tap_form_alter", &input, &plugin, state.clone())
                .await
            else {
                continue;
            };
            if result.output.is_empty() || result.output == "{}" {
                continue;
            }
            match serde_json::from_str::<Form>(&result.output) {
                Ok(altered) => {
                    debug!(plugin = %plugin, form_id = %form_id, "form altered by plugin");
                    form = altered;
                    form.form_id = form_id.clone();
                    form.form_build_id = build_id.clone();
                    form.token = token.clone();
                }
                Err(e) => {
                    warn!(plugin = %plugin, error = %e, "failed to parse form alteration");
                }
            }
        }

        form_state.form_id = form_id;
        form_state.form_build_id = build_id.clone();
        form_state.rendered_fields = form.rendered_inputs();
        if let Err(e) = self.save_state(&build_id, &form_state).await {
            // The submission then answers "expired" rather than guessing.
            warn!(error = %e, "failed to save form state");
        }

        Ok(form)
    }

    /// Check a submission of the form saved under `form_build_id`.
    ///
    /// Verifies the CSRF token, recalls the form state (built for this user),
    /// runs the built-in checks `validate` performs (for an item form, the
    /// submission builder), then `tap_form_validate`. Returns the state and
    /// the value `validate` produced, or every error at once.
    ///
    /// `tap_form_submit` is not dispatched here: the caller saves first and
    /// then calls [`Self::submitted`], so a plugin never hears of a submission
    /// the save refused.
    pub async fn process<T>(
        &self,
        form_build_id: &str,
        posted: &crate::content::submission::PostedForm,
        session: &Session,
        state: &RequestState,
        validate: impl FnOnce(&FormState) -> std::result::Result<T, Vec<ValidationError>>,
    ) -> std::result::Result<(FormState, T), ProcessError> {
        let csrf_token = posted.get("_token").unwrap_or("");
        if !verify_csrf_token(session, csrf_token)
            .await
            .unwrap_or(false)
        {
            return Err(ProcessError::Csrf);
        }

        let Some(form_state) = self.recall(form_build_id, state.user.id).await else {
            return Err(ProcessError::Invalid(vec![ValidationError::form(
                FORM_EXPIRED,
            )]));
        };

        let built = validate(&form_state);
        let mut errors = match &built {
            Ok(_) => Vec::new(),
            Err(errors) => errors.clone(),
        };

        let validate_input = serde_json::json!({
            "form_id": form_state.form_id,
            "values": posted_values(posted, &form_state.rendered_fields),
        });
        let input = serde_json::to_string(&validate_input).unwrap_or_default();
        for result in self
            .dispatcher
            .dispatch("tap_form_validate", &input, state.clone())
            .await
        {
            if result.output.is_empty() || result.output == "{}" {
                continue;
            }
            match serde_json::from_str::<TapValidationResult>(&result.output) {
                Ok(tap) => errors.extend(tap.errors.into_iter().map(TapError::into_error)),
                Err(e) => warn!(
                    plugin = %result.plugin_name,
                    error = %e,
                    "failed to parse tap_form_validate result"
                ),
            }
        }

        match built {
            Ok(value) if errors.is_empty() => Ok((form_state, value)),
            _ => Err(ProcessError::Invalid(errors)),
        }
    }

    /// Tell plugins a submission was saved: `tap_form_submit`, once.
    ///
    /// The input is `{form_id, values, item_id}`: the posted values of the
    /// inputs the form rendered, and the item the save created or updated, if
    /// any.
    pub async fn submitted(
        &self,
        form_state: &FormState,
        posted: &crate::content::submission::PostedForm,
        item_id: Option<uuid::Uuid>,
        state: &RequestState,
    ) {
        let input = serde_json::json!({
            "form_id": form_state.form_id,
            "values": posted_values(posted, &form_state.rendered_fields),
            "item_id": item_id,
        });
        self.dispatcher
            .dispatch(
                "tap_form_submit",
                &serde_json::to_string(&input).unwrap_or_default(),
                state.clone(),
            )
            .await;
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

    /// The state saved under `form_build_id`, if it was built for `user_id`.
    ///
    /// A missing, expired or unreadable state and one built for another user
    /// are the same answer: `None`, and the caller treats the form as expired.
    pub async fn recall(&self, form_build_id: &str, user_id: uuid::Uuid) -> Option<FormState> {
        if form_build_id.is_empty() {
            return None;
        }
        match self.load_state(form_build_id).await {
            Ok(Some(state)) if state.user_id == Some(user_id) => Some(state),
            Ok(_) => None,
            Err(e) => {
                warn!(error = %e, "failed to load form state");
                None
            }
        }
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

    /// The user the form was built for. A submission or AJAX request from
    /// anyone else finds no state.
    #[serde(default)]
    pub user_id: Option<uuid::Uuid>,

    /// The item an edit form edits; `None` on an add form.
    #[serde(default)]
    pub item_id: Option<uuid::Uuid>,

    /// The content type an item form is for.
    #[serde(default)]
    pub item_type: Option<String>,

    /// The fields the built form rendered as inputs, after alteration and
    /// field access. The submission builder writes these and only these; it
    /// is kept here, server side, so a request cannot widen it.
    #[serde(default)]
    pub rendered_fields: Vec<String>,
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
            user_id: None,
            item_id: None,
            item_type: None,
            rendered_fields: Vec::new(),
        }
    }
}

/// The error a submission gets when its form state is missing, expired or
/// another user's.
pub const FORM_EXPIRED: &str = "This form has expired. Reload the page and submit again.";

/// Why [`FormService::process`] refused a submission.
#[derive(Debug)]
pub enum ProcessError {
    /// The CSRF token was missing, used or wrong: a forged or replayed
    /// request, answered 403 rather than shown on the form.
    Csrf,
    /// Errors to show on the form: an expired state, built-in validation or a
    /// `tap_form_validate` refusal.
    Invalid(Vec<ValidationError>),
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
    errors: Vec<TapError>,
}

/// One error from `tap_form_validate`: a message for the whole form, or
/// `{field, message}` for one field, which the form shows beside it.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum TapError {
    Form(String),
    Field { field: String, message: String },
}

impl TapError {
    fn into_error(self) -> ValidationError {
        match self {
            Self::Form(message) => ValidationError::form(message),
            Self::Field { field, message } => ValidationError::field(field, message),
        }
    }
}

/// The posted values as plugins receive them: each name once, a repeated or
/// `name[i]` name as an array in delta order.
///
/// Only the inputs the form rendered are passed (`rendered`, from
/// [`Form::rendered_inputs`]), so form machinery (`_token`, `_form_build_id`),
/// anything a client added to the body, and password inputs, which that list
/// never holds, do not reach a plugin.
fn posted_values(
    posted: &crate::content::submission::PostedForm,
    rendered: &[String],
) -> serde_json::Map<String, Value> {
    let mut out = serde_json::Map::new();
    for (name, _) in &posted.0 {
        let base = name.split('[').next().unwrap_or(name);
        if out.contains_key(base) || !rendered.iter().any(|r| r == base) {
            continue;
        }
        let values = posted.values_of(base);
        let value = if name.contains('[') || values.len() > 1 {
            Value::Array(
                values
                    .into_iter()
                    .map(|v| Value::String(v.to_string()))
                    .collect(),
            )
        } else {
            Value::String(values.first().copied().unwrap_or_default().to_string())
        };
        out.insert(base.to_string(), value);
    }
    out
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn plugins_see_only_the_rendered_inputs() {
        use crate::content::submission::PostedForm;
        use crate::form::{Form, FormElement};

        let form = Form::new("f")
            .element("name", FormElement::textfield())
            .element("current_password", FormElement::password())
            .element("note", FormElement::markup("<p>x</p>"));
        let rendered = form.rendered_inputs();
        assert_eq!(rendered, vec!["name".to_string()]);

        let posted = PostedForm(vec![
            ("_token".into(), "t".into()),
            ("name".into(), "Ada".into()),
            ("current_password".into(), "hunter2".into()),
            ("smuggled".into(), "x".into()),
        ]);
        let values = posted_values(&posted, &rendered);
        assert_eq!(values.len(), 1, "{values:?}");
        assert_eq!(values["name"], "Ada");
    }

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
