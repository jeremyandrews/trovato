//! Admin routes for user, role, and permission management.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use serde::Deserialize;
use tower_sessions::Session;

use crate::form::csrf::generate_csrf_token;
use crate::i18n::UiMessage;
use crate::models::role::KERNEL_PERMISSIONS;
use crate::models::role::well_known::{ANONYMOUS_ROLE_ID, AUTHENTICATED_ROLE_ID};
use crate::models::user::ANONYMOUS_USER_ID;
use crate::models::{CreateUser, UpdateUser, User};
use crate::routes::admin_form::FieldError;
use crate::state::AppState;

use super::admin_list::{ListQuery, PER_PAGE};
use super::helpers::{
    CsrfOnlyForm, admin_user_context, build_local_tasks, render_admin_template, render_error,
    render_forbidden, render_not_found, render_server_error, require_csrf, require_permission,
    validate_password,
};
use super::user_delete::blocks_last_admin;

/// User form data.
#[derive(Debug, Deserialize)]
struct UserFormData {
    #[serde(rename = "_token")]
    token: String,
    #[serde(rename = "_form_build_id")]
    form_build_id: String,
    name: String,
    mail: String,
    password: Option<String>,
    is_admin: Option<String>,
    status: Option<String>,
    /// "Notify user of new account" on the add form: mail a welcome with a
    /// one-time sign in link.
    #[serde(default)]
    notify: Option<String>,
    /// Role membership, as `role_{role_id}` keys.
    ///
    /// Flattened into a map rather than collected into a `Vec<Uuid>` because
    /// `axum::Form` deserializes with `serde_urlencoded`, which cannot gather
    /// repeated same-name keys into a sequence. Every multi-checkbox group in
    /// this codebase encodes identity in the key for that reason, and reads it
    /// back by reconstructing the key from the server's own list rather than by
    /// trusting what the browser sent.
    #[serde(flatten)]
    roles: std::collections::HashMap<String, String>,
}

/// One role in the listing.
///
/// Not the `Role` row: the counts are what the screen is for, and a template that
/// has to ask for them one at a time is a template doing queries.
#[derive(Debug, serde::Serialize)]
struct RoleRow {
    id: uuid::Uuid,
    name: String,
    /// How many users hold the role. Deleting it removes these assignments.
    members: i64,
    /// How many permissions it grants.
    permissions: usize,
    /// Whether this is one of the two roles the kernel depends on by id.
    built_in: bool,
}

/// Role form data.
#[derive(Debug, Deserialize)]
struct RoleFormData {
    #[serde(rename = "_token")]
    token: String,
    #[serde(rename = "_form_build_id")]
    form_build_id: String,
    name: String,
    /// "Requires a second factor" checkbox (B304): present means checked.
    #[serde(default)]
    requires_second_factor: Option<String>,
}

/// Permission form data (for permission matrix).
#[derive(Debug, Deserialize)]
struct PermissionFormData {
    #[serde(rename = "_token")]
    token: String,
    #[serde(rename = "_form_build_id")]
    form_build_id: String,
    #[serde(flatten)]
    permissions: std::collections::HashMap<String, String>,
}

// =============================================================================
// Delegation
// =============================================================================

/// What an actor may hand out, take away, or act on through these screens.
///
/// `administer users` is a grantable permission, and everything these screens
/// write is made of permissions: a role carries them, an account holds them, the
/// grid assigns them. So one rule covers all of it: a non-superuser may act only
/// within the permissions they already hold, and can hand out what they have
/// and no more. A superuser is unrestricted, as they are everywhere else.
///
/// The rule is applied to a role's membership ([`apply_role_membership`]), to a
/// role's deletion ([`delete_role`]), to each box on the permission grid
/// ([`save_permissions`]), and to an account ([`may_act_on`]). It lives here, in
/// [`Authority::covers`], so the four cannot drift apart.
enum Authority {
    /// A superuser.
    Unrestricted,
    /// Anyone else: exactly the permissions they hold.
    Holds(std::collections::HashSet<String>),
}

impl Authority {
    /// The actor's authority, loaded once per request.
    ///
    /// An error is returned rather than an empty set so that every caller has
    /// to decide, visibly, to refuse.
    async fn of(state: &AppState, actor: &User) -> anyhow::Result<Self> {
        if actor.is_admin {
            return Ok(Self::Unrestricted);
        }
        Ok(Self::Holds(
            state.permissions().user_permissions(actor).await?,
        ))
    }

    /// Whether every one of `permissions` is within this authority.
    fn covers<'a>(&self, permissions: impl IntoIterator<Item = &'a String>) -> bool {
        match self {
            Self::Unrestricted => true,
            Self::Holds(held) => permissions.into_iter().all(|p| held.contains(p)),
        }
    }
}

/// Whether `actor` may edit, reset, block or delete `target`.
///
/// The [`Authority`] rule applied to a person instead of a role: a superuser may
/// act on anyone; nobody else may act on a superuser, whose authority is not a
/// set of permissions at all; otherwise the actor must hold every permission the
/// target holds. Without this, `administer users` reached every account on the
/// site through the password and email fields, superusers included.
///
/// Fails closed: a permission set that cannot be loaded is a refusal.
async fn may_act_on(state: &AppState, actor: &User, target: &User) -> bool {
    if actor.is_admin {
        return true;
    }
    if target.is_admin {
        return false;
    }

    let authority = match Authority::of(state, actor).await {
        Ok(authority) => authority,
        Err(e) => {
            tracing::error!(error = %e, actor = %actor.id, "failed to load the actor's permissions");
            return false;
        }
    };
    match state.permissions().user_permissions(target).await {
        Ok(target_permissions) => authority.covers(&target_permissions),
        Err(e) => {
            tracing::error!(error = %e, target = %target.id, "failed to load the target's permissions");
            false
        }
    }
}

/// Whether an edit takes an active superuser out of that role, by clearing the
/// flag or by blocking the account.
///
/// Only the edit's effect on the superuser population is decided here; whether
/// that leaves the site with none is [`blocks_last_admin`], the same rule self
/// service account deletion uses.
fn demotes_active_superuser(
    was_admin: bool,
    was_status: i16,
    is_admin_after: bool,
    status_after: i16,
) -> bool {
    was_admin && was_status == 1 && (!is_admin_after || status_after != 1)
}

/// Whether the site can afford to lose one active superuser.
///
/// Fails closed: a count that cannot be read refuses.
async fn may_lose_a_superuser(state: &AppState) -> bool {
    match state.users().active_admin_count().await {
        Ok(count) => !blocks_last_admin(true, count),
        Err(e) => {
            tracing::error!(error = %e, "failed to count active superusers");
            false
        }
    }
}

// =============================================================================
// User Management
// =============================================================================

/// Session key for the warning the user list shows after an administrator
/// creates an account an access rule blocks.
const ACCESS_RULE_WARNING_KEY: &str = "admin_user_access_rule_warning";

/// The people listing's sortable columns: key and SQL.
const USER_SORTS: &[(&str, &str)] = &[
    ("name", "lower(name)"),
    ("email", "lower(mail)"),
    ("status", "status"),
];

/// List users, a page at a time, filtered by name or email address.
///
/// GET /admin/people
async fn list_users(
    State(state): State<AppState>,
    session: Session,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "administer users").await {
        return redirect;
    }

    let query = ListQuery::from_params(&params);
    let sort = query.sort(USER_SORTS, ("name", false));
    let like = query.like();

    let (users, total) = match state
        .users()
        .admin_page(like.as_deref(), &sort.order_by(), PER_PAGE, query.offset())
        .await
    {
        Ok(page) => page,
        Err(e) => {
            tracing::error!(error = %e, "failed to list users");
            return render_server_error("Failed to load users.");
        }
    };

    let csrf_token = generate_csrf_token(&session).await;
    let warning: Option<String> = session.remove(ACCESS_RULE_WARNING_KEY).await.ok().flatten();

    let mut context = tera::Context::new();
    context.insert("warning", &warning);
    context.insert("users", &users);
    context.insert("list", &query.view(total, users.len(), Some(&sort)));
    context.insert("csrf_token", &csrf_token);
    context.insert("path", "/admin/people");

    render_admin_template(
        &state,
        &crate::routes::helpers::get_user_context(&session, &state).await,
        "admin/users.html",
        context,
    )
    .await
}

/// Show add user form.
///
/// GET /admin/people/add
async fn add_user_form(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "administer users").await {
        return redirect;
    }

    let csrf_token = generate_csrf_token(&session).await;
    let form_build_id = uuid::Uuid::new_v4().to_string();

    let mut context = tera::Context::new();
    context.insert("action", "/admin/people/add");
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert("editing", &false);
    context.insert("values", &serde_json::json!({"roles": []}));
    context.insert("roles", &state.roles().list().await.unwrap_or_default());
    context.insert("path", "/admin/people/add");

    render_admin_template(
        &state,
        &crate::routes::helpers::get_user_context(&session, &state).await,
        "admin/user-form.html",
        context,
    )
    .await
}

/// Everything wrong with a new account, in the words the "Add user" form uses:
/// empty when it may be created.
///
/// The one set of rules for an account an administrator creates, on this form
/// and with `trovato user create`. A name and an email address are required,
/// and neither may belong to another account (compared case-insensitively). A
/// `password` of `None` is an account created with no password at all
/// (`--no-password`); `Some` is checked by [`validate_password`].
pub(crate) async fn new_account_problems(
    state: &AppState,
    name: &str,
    mail: &str,
    password: Option<&str>,
) -> Vec<FieldError> {
    let mut errors = Vec::new();

    if name.trim().is_empty() {
        errors.push(FieldError::on("name", "Username is required."));
    }

    if mail.trim().is_empty() {
        errors.push(FieldError::on("mail", "Email is required."));
    }

    if let Some(password) = password
        && let Err(msg) = validate_password(state.db(), password).await
    {
        errors.push(FieldError::on("password", msg));
    }

    // Check if username already exists. An empty value is already refused
    // above, and would otherwise match the anonymous account's empty mail.
    if !name.trim().is_empty()
        && let Ok(Some(_)) = state.users().find_by_name(name).await
    {
        errors.push(FieldError::on(
            "name",
            UiMessage::new("Username '{name}' is already taken.").arg("name", name),
        ));
    }

    // Check if email already exists
    if !mail.trim().is_empty()
        && let Ok(Some(_)) = state.users().find_by_mail(mail).await
    {
        errors.push(FieldError::on(
            "mail",
            UiMessage::new("Email '{mail}' is already in use.").arg("mail", mail),
        ));
    }

    errors
}

/// What to tell the administrator who just created the account `name` when an
/// access rule (B108) blocks its name or address, or `None`.
///
/// Access rules warn here rather than refuse: an administrator may knowingly
/// create an account the public registration form would turn away.
pub(crate) async fn access_rule_warning(
    state: &AppState,
    name: &str,
    mail: &str,
) -> Option<String> {
    let blocked = crate::services::access_rule::blocked_types(
        state.db(),
        Some(name.trim()),
        Some(mail.trim()),
        None,
    )
    .await;
    if blocked.is_empty() {
        return None;
    }
    let types: Vec<&str> = blocked
        .iter()
        .map(|t| match t {
            crate::services::access_rule::RuleType::Username => "username",
            crate::services::access_rule::RuleType::Email => "email address",
            crate::services::access_rule::RuleType::Host => "address",
        })
        .collect();
    Some(format!(
        "User {} was created, but an access rule blocks its {}: it could not have registered itself, and cannot sign in until the rule is changed.",
        name.trim(),
        types.join(" and "),
    ))
}

/// Handle add user form submission.
///
/// POST /admin/people/add
async fn add_user_submit(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<UserFormData>,
) -> Response {
    let current_user = match require_permission(&state, &session, "administer users").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    // Verify CSRF token
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    let password = form.password.as_deref().unwrap_or("");
    let errors = new_account_problems(&state, &form.name, &form.mail, Some(password)).await;

    if !errors.is_empty() {
        let csrf_token = generate_csrf_token(&session).await;
        let form_build_id = uuid::Uuid::new_v4().to_string();

        let mut context = tera::Context::new();
        context.insert("action", "/admin/people/add");
        context.insert("csrf_token", &csrf_token);
        context.insert("form_build_id", &form_build_id);
        context.insert("editing", &false);
        context.insert("errors", &errors);
        let all_roles = state.roles().list().await.unwrap_or_default();
        context.insert(
            "values",
            &serde_json::json!({
                "name": form.name,
                "mail": form.mail,
                "is_admin": form.is_admin.is_some(),
                "status": form.status.is_some(),
                "notify": form.notify.is_some(),
                "roles": submitted_role_ids(&all_roles, &form.roles),
            }),
        );
        context.insert("roles", &all_roles);
        context.insert("path", "/admin/people/add");

        return render_admin_template(
            &state,
            &crate::routes::helpers::get_user_context(&session, &state).await,
            "admin/user-form.html",
            context,
        )
        .await;
    }

    // Create the user
    let input = CreateUser {
        name: form.name.clone(),
        password: password.to_string(),
        mail: form.mail.clone(),
        // Only a superuser may create another superuser. The route gate is
        // `administer users`, which is a grantable permission; the superuser
        // flag is what that permission is granted *by*, so letting it be set
        // from here would make `administer users` a self-escalation to
        // superuser. Checked on the field rather than the route so that
        // managing ordinary users stays delegable.
        is_admin: form.is_admin.is_some() && current_user.is_admin,
    };

    let user_ctx = admin_user_context(&state, &current_user).await;
    match state.users().create(input, &user_ctx, "admin").await {
        Ok(user) => {
            apply_role_membership(&state, &current_user, user.id, &form.roles).await;
            tracing::info!(name = %form.name, "user created");
            if form.notify.is_some() {
                // After the answer, as every kernel mail is: the request does
                // not wait on the mail server.
                tokio::spawn(send_welcome_mail(state.clone(), user.clone()));
            }
            if let Some(message) = access_rule_warning(&state, &form.name, &form.mail).await {
                let _ = session.insert(ACCESS_RULE_WARNING_KEY, message).await;
            }
            Redirect::to("/admin/people").into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to create user");
            render_server_error("Failed to create user.")
        }
    }
}

/// Mail `user` the welcome an administrator's "Notify user of new account"
/// asks for: who made the account, and a one-time link to sign in and set a
/// password (a password reset token, which is what that link is).
///
/// Queued, the link made when it is sent. Without a mail transport the link
/// is only logged at debug level, as the password reset's is.
async fn send_welcome_mail(state: AppState, user: User) {
    let Some(email_service) = state.email() else {
        // No mail transport: make the link and log it, as the password reset
        // does, so a site without SMTP can still hand it over.
        match crate::models::PasswordResetToken::create(state.db(), user.id).await {
            Ok((_, token)) => tracing::debug!(
                user_id = %user.id,
                reset_url = format!("/user/password-reset/{token}"),
                "welcome mail not sent: no mail transport is configured"
            ),
            Err(e) => tracing::error!(
                error = %e,
                user_id = %user.id,
                "welcome mail: failed to create the sign in link"
            ),
        }
        return;
    };
    let site_name = crate::models::SiteConfig::site_name(state.db())
        .await
        .unwrap_or_default();
    let subject = format!("An administrator created an account for you at {site_name}");
    let action_url = format!(
        "{}/user/password-reset/{}",
        email_service.site_url(),
        crate::services::email::SECRET_MARKER
    );
    let mut context = tera::Context::new();
    context.insert("site_name", &site_name);
    context.insert("username", &user.name);
    context.insert("action_url", &action_url);
    context.insert("subject", &subject);
    let (html, text) = match crate::services::email_templates::render(
        state.theme().tera(),
        "admin_created_account",
        &context,
    ) {
        Ok(parts) => parts,
        Err(e) => {
            tracing::error!(error = %e, "failed to render the welcome email");
            return;
        }
    };
    // The link is made when the mail is sent, as the password reset's is.
    let message = crate::services::email::NewMail::account(
        "admin_created_account",
        &user.mail,
        &subject,
        &text,
    )
    .html(html)
    .secret(crate::services::email::MailSecret::PasswordReset { user_id: user.id })
    .dedupe(format!("admin-created-account:{}", user.id));
    match email_service.queue_now(message).await {
        Ok(_) => tracing::info!(user_id = %user.id, "welcome mail queued"),
        Err(e) => {
            tracing::error!(error = %e, user_id = %user.id, "failed to queue the welcome mail");
        }
    }
}

/// Show edit user form.
///
/// GET /admin/people/{id}/edit
async fn edit_user_form(
    State(state): State<AppState>,
    session: Session,
    Path(user_id): Path<uuid::Uuid>,
) -> Response {
    let current_user = match require_permission(&state, &session, "administer users").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    let Some(target_user) = state.users().find_by_id(user_id).await.ok().flatten() else {
        return render_not_found();
    };

    // Not shown a form they could not submit.
    if !may_act_on(&state, &current_user, &target_user).await {
        return render_forbidden("You may not edit an account holding permissions you do not.");
    }

    let csrf_token = generate_csrf_token(&session).await;
    let form_build_id = uuid::Uuid::new_v4().to_string();

    let mut context = tera::Context::new();
    context.insert("action", &format!("/admin/people/{user_id}/edit"));
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert("editing", &true);
    context.insert("user_id", &user_id.to_string());
    let held: Vec<String> = state
        .roles()
        .get_user_roles(user_id)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|r| r.id.to_string())
        .collect();
    context.insert(
        "values",
        &serde_json::json!({
            "name": target_user.name,
            "mail": target_user.mail,
            "is_admin": target_user.is_admin,
            "status": target_user.status == 1,
            "roles": held,
        }),
    );
    context.insert("roles", &state.roles().list().await.unwrap_or_default());
    context.insert("path", &format!("/admin/people/{user_id}/edit"));
    context.insert(
        "has_totp",
        &crate::services::second_factor::has_confirmed_totp(&state, user_id).await,
    );
    context.insert(
        "second_factor_csrf_token",
        &generate_csrf_token(&session).await,
    );

    // Local task tabs for user edit pages (hardcoded + plugin-registered)
    let current_path = format!("/admin/people/{user_id}/edit");
    context.insert(
        "local_tasks",
        &build_local_tasks(
            &state,
            "/admin/people/:id",
            &current_path,
            Some(&user_id.to_string()),
            vec![serde_json::json!({"title": "Edit", "path": &current_path, "active": true})],
        ),
    );

    render_admin_template(
        &state,
        &crate::routes::helpers::get_user_context(&session, &state).await,
        "admin/user-form.html",
        context,
    )
    .await
}

/// Handle edit user form submission.
///
/// POST /admin/people/{id}/edit
async fn edit_user_submit(
    State(state): State<AppState>,
    session: Session,
    Path(user_id): Path<uuid::Uuid>,
    Form(form): Form<UserFormData>,
) -> Response {
    let current_user = match require_permission(&state, &session, "administer users").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    // Verify CSRF token
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    let Some(existing_user) = state.users().find_by_id(user_id).await.ok().flatten() else {
        return render_not_found();
    };

    // Before anything is validated or written: nothing about an account the
    // actor may not act on changes, roles and password included.
    if !may_act_on(&state, &current_user, &existing_user).await {
        tracing::warn!(
            actor = %current_user.id,
            target = %user_id,
            "refused an account edit beyond the actor's own permissions"
        );
        return render_forbidden("You may not edit an account holding permissions you do not.");
    }

    // Only a superuser may change the superuser flag, in either direction:
    // granting it would make `administer users` a self-escalation, and
    // revoking it would let a delegated user administrator lock the real
    // superusers out. For anyone else the stored value is preserved
    // regardless of what the form submitted.
    let is_admin_after = if current_user.is_admin {
        form.is_admin.is_some()
    } else {
        existing_user.is_admin
    };
    let status_after: i16 = if form.status.is_some() { 1 } else { 0 };

    // A superuser may demote or block another, or themselves, but not the last
    // one: a site with no active superuser cannot be administered back into
    // having one.
    if demotes_active_superuser(
        existing_user.is_admin,
        existing_user.status,
        is_admin_after,
        status_after,
    ) && !may_lose_a_superuser(&state).await
    {
        return render_forbidden("Cannot demote or block the last active superuser.");
    }

    // Validate
    let mut errors = Vec::new();

    if form.name.trim().is_empty() {
        errors.push(FieldError::on("name", "Username is required."));
    }

    if form.mail.trim().is_empty() {
        errors.push(FieldError::on("mail", "Email is required."));
    }

    // Check if new username is taken by someone else
    if form.name != existing_user.name
        && let Ok(Some(_)) = state.users().find_by_name(&form.name).await
    {
        errors.push(FieldError::on(
            "name",
            UiMessage::new("Username '{name}' is already taken.").arg("name", &form.name),
        ));
    }

    // Check if new email is taken by someone else
    if form.mail != existing_user.mail
        && let Ok(Some(_)) = state.users().find_by_mail(&form.mail).await
    {
        errors.push(FieldError::on(
            "mail",
            UiMessage::new("Email '{mail}' is already in use.").arg("mail", &form.mail),
        ));
    }

    // Validate password if provided
    if let Some(ref password) = form.password
        && !password.is_empty()
        && let Err(msg) = validate_password(state.db(), password).await
    {
        errors.push(FieldError::on("password", msg));
    }

    if !errors.is_empty() {
        let csrf_token = generate_csrf_token(&session).await;
        let form_build_id = uuid::Uuid::new_v4().to_string();

        let mut context = tera::Context::new();
        context.insert("action", &format!("/admin/people/{user_id}/edit"));
        context.insert("csrf_token", &csrf_token);
        context.insert("form_build_id", &form_build_id);
        context.insert("editing", &true);
        context.insert("user_id", &user_id.to_string());
        context.insert("errors", &errors);
        let all_roles = state.roles().list().await.unwrap_or_default();
        context.insert(
            "values",
            &serde_json::json!({
                "name": form.name,
                "mail": form.mail,
                "is_admin": form.is_admin.is_some(),
                "status": form.status.is_some(),
                "roles": submitted_role_ids(&all_roles, &form.roles),
            }),
        );
        context.insert("roles", &all_roles);
        let current_path = format!("/admin/people/{user_id}/edit");
        context.insert("path", &current_path);
        context.insert(
            "local_tasks",
            &build_local_tasks(
                &state,
                "/admin/people/:id",
                &current_path,
                Some(&user_id.to_string()),
                vec![serde_json::json!({"title": "Edit", "path": &current_path, "active": true})],
            ),
        );

        return render_admin_template(
            &state,
            &crate::routes::helpers::get_user_context(&session, &state).await,
            "admin/user-form.html",
            context,
        )
        .await;
    }

    // Update the user
    let input = UpdateUser {
        name: Some(form.name.clone()),
        mail: Some(form.mail.clone()),
        is_admin: Some(is_admin_after),
        status: Some(status_after),
        timezone: None,
        language: None,
        data: None,
    };

    let user_ctx = admin_user_context(&state, &current_user).await;
    match state.users().update(user_id, input, &user_ctx).await {
        Ok(_) => {
            apply_role_membership(&state, &current_user, user_id, &form.roles).await;

            // Update password if provided
            let password_set = match form.password {
                Some(ref password) if !password.is_empty() => {
                    if let Err(e) = state
                        .users()
                        .update_password(user_id, password, &user_ctx, "admin")
                        .await
                    {
                        tracing::error!(error = %e, "failed to update user password");
                        return render_server_error("Failed to update password.");
                    }
                    true
                }
                _ => false,
            };

            let admin_user_id = current_user.id;
            let own_device = session
                .get::<uuid::Uuid>(crate::services::session_registry::SESSION_DEVICE_ID)
                .await
                .ok()
                .flatten();
            if existing_user.is_active() && status_after == 0 {
                // Blocked: nothing the account held may keep working. Every
                // session ends and every API token is deleted.
                crate::services::session_registry::end_user_sessions(
                    &state,
                    user_id,
                    None,
                    Some(admin_user_id),
                    "account_blocked",
                )
                .await;
                match crate::services::api_token::revoke_all_for_user(
                    &state,
                    user_id,
                    Some(admin_user_id),
                    "account_blocked",
                )
                .await
                {
                    Ok(n) => tracing::info!(
                        user_id = %user_id,
                        admin = %admin_user_id,
                        tokens = n,
                        "blocked account's API tokens deleted"
                    ),
                    Err(e) => tracing::error!(
                        error = %e,
                        user_id = %user_id,
                        "failed to delete a blocked account's API tokens"
                    ),
                }
            } else if password_set {
                // A password set by an administrator ends the account's
                // sessions. When the administrator set their own, the device
                // they did it from is kept, as a self-service change keeps it.
                let keep = if user_id == admin_user_id {
                    own_device
                } else {
                    None
                };
                crate::services::session_registry::end_user_sessions(
                    &state,
                    user_id,
                    keep,
                    Some(admin_user_id),
                    "password_set_by_admin",
                )
                .await;
            }

            // An administrator who changed their own account rotates their
            // session id, so a copy of the pre-change cookie stops working.
            if user_id == admin_user_id
                && let Err(e) = session.cycle_id().await
            {
                tracing::warn!(error = %e, "failed to rotate session after self-privilege change");
            }

            tracing::info!(user_id = %user_id, "user updated");
            Redirect::to("/admin/people").into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to update user");
            render_server_error("Failed to update user.")
        }
    }
}

/// Reset a user's second factor: delete their TOTP enrolment and recovery
/// codes, end every session of theirs, and record it (B304).
///
/// POST /admin/people/{id}/second-factor/reset
async fn reset_second_factor(
    State(state): State<AppState>,
    session: Session,
    Path(user_id): Path<uuid::Uuid>,
    Form(form): Form<CsrfOnlyForm>,
) -> Response {
    let current_user = match require_permission(&state, &session, "administer users").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    let Some(_target_user) = state.users().find_by_id(user_id).await.ok().flatten() else {
        return render_not_found();
    };

    if let Err(e) = crate::services::totp::remove_enrolment(state.db(), user_id).await {
        tracing::error!(error = %e, user_id = %user_id, "failed to reset a user's second factor");
        return render_server_error("Failed to reset the second factor.");
    }
    crate::services::session_registry::end_user_sessions(
        &state,
        user_id,
        None,
        Some(current_user.id),
        "second_factor_reset",
    )
    .await;
    state
        .audit_trail()
        .record(
            "second_factor_reset",
            "user",
            user_id,
            Some(current_user.id),
            serde_json::json!({}),
        )
        .await;
    state
        .security_audit()
        .emit(
            crate::audit::SecurityEvent::new(crate::audit::SecurityEventKind::TotpResetByAdmin)
                .user(user_id)
                .actor(current_user.id),
        )
        .await;

    Redirect::to(&format!("/admin/people/{user_id}/edit")).into_response()
}

/// Delete a user.
///
/// POST /admin/people/{id}/delete
async fn delete_user(
    State(state): State<AppState>,
    session: Session,
    Path(user_id): Path<uuid::Uuid>,
    Form(form): Form<CsrfOnlyForm>,
) -> Response {
    let current_user = match require_permission(&state, &session, "administer users").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    // Prevent deleting anonymous user
    if user_id == ANONYMOUS_USER_ID {
        return render_error("Cannot delete the anonymous user.");
    }

    // Prevent deleting yourself
    if user_id == current_user.id {
        return render_error("Cannot delete your own account.");
    }

    let Some(target_user) = state.users().find_by_id(user_id).await.ok().flatten() else {
        return render_not_found();
    };

    if !may_act_on(&state, &current_user, &target_user).await {
        tracing::warn!(
            actor = %current_user.id,
            target = %user_id,
            "refused an account deletion beyond the actor's own permissions"
        );
        return render_forbidden("You may not delete an account holding permissions you do not.");
    }

    // Unreachable today: only a superuser gets this far with a superuser
    // target, and it cannot be themselves, so at least two exist. Checked
    // anyway so the guard does not rest on that reasoning.
    if demotes_active_superuser(target_user.is_admin, target_user.status, false, 0)
        && !may_lose_a_superuser(&state).await
    {
        return render_forbidden("Cannot delete the last active superuser.");
    }

    let user_ctx = admin_user_context(&state, &current_user).await;
    // Every session of the deleted account ends inside `delete`, as it does
    // for a self-service deletion (`routes::user_delete`).
    match state
        .users()
        .delete(user_id, &user_ctx, state.session_registry())
        .await
    {
        Ok(true) => {
            tracing::info!(user_id = %user_id, "user deleted");
            Redirect::to("/admin/people").into_response()
        }
        Ok(false) => render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to delete user");
            render_server_error("Failed to delete user.")
        }
    }
}

// =============================================================================
// Account cancellation
// =============================================================================

/// How an account is cancelled: Drupal's four cancellation methods, each a
/// function a site used (B156). Deleting outright was the only one there was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelMethod {
    /// Block the account and keep its content as it is.
    Block,
    /// Block the account and unpublish its items and comments.
    BlockUnpublish,
    /// Delete the account and give its content to the anonymous author.
    Reassign,
    /// Delete the account and its items and comments.
    Delete,
}

impl CancelMethod {
    /// Every method, in the order the form offers them.
    pub const ALL: [Self; 4] = [
        Self::Block,
        Self::BlockUnpublish,
        Self::Reassign,
        Self::Delete,
    ];

    /// The form value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Block => "block",
            Self::BlockUnpublish => "block_unpublish",
            Self::Reassign => "reassign",
            Self::Delete => "delete",
        }
    }

    /// The method a form value names.
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.as_str() == value)
    }

    /// What the form says the method does.
    fn label(self) -> &'static str {
        match self {
            Self::Block => "Disable the account and keep its content.",
            Self::BlockUnpublish => "Disable the account and unpublish its content.",
            Self::Reassign => {
                "Delete the account and make its content belong to the Anonymous user."
            }
            Self::Delete => "Delete the account and its content.",
        }
    }
}

/// The posted cancel form.
#[derive(Debug, Deserialize)]
struct CancelForm {
    #[serde(rename = "_token")]
    token: String,
    method: String,
}

/// Who may cancel `target`, answered as the refusal when they may not: the
/// same rule the edit and delete routes apply.
async fn cancel_refusal(
    state: &AppState,
    session: &Session,
    target_id: uuid::Uuid,
) -> Result<(User, User), Response> {
    let actor = require_permission(state, session, "administer users").await?;
    if target_id == ANONYMOUS_USER_ID {
        return Err(render_not_found());
    }
    let Some(target) = state.users().find_by_id(target_id).await.ok().flatten() else {
        return Err(render_not_found());
    };
    if !may_act_on(state, &actor, &target).await {
        return Err(render_forbidden(
            "You may not cancel an account holding permissions you do not.",
        ));
    }
    Ok((actor, target))
}

/// GET /user/{id}/cancel: the confirmation, with the cancellation methods.
///
/// One's own account is cancelled through `/user/delete`, which asks for the
/// password again; this page sends its own owner there.
async fn cancel_form(
    State(state): State<AppState>,
    session: Session,
    Path(user_id): Path<uuid::Uuid>,
) -> Response {
    if let Some(me) = super::helpers::session_user(&state, &session).await
        && me.id == user_id
    {
        return Redirect::to("/user/delete").into_response();
    }
    let (actor, target) = match cancel_refusal(&state, &session, user_id).await {
        Ok(pair) => pair,
        Err(refusal) => return refusal,
    };
    render_cancel_form(&state, &session, &actor, &target, None).await
}

async fn render_cancel_form(
    state: &AppState,
    session: &Session,
    actor: &User,
    target: &User,
    error: Option<&str>,
) -> Response {
    let methods: Vec<serde_json::Value> = CancelMethod::ALL
        .iter()
        .map(|m| serde_json::json!({ "value": m.as_str(), "label": m.label() }))
        .collect();
    let mut context = tera::Context::new();
    context.insert(
        "account",
        &serde_json::json!({ "id": target.id, "name": target.name }),
    );
    context.insert("methods", &methods);
    context.insert("csrf_token", &generate_csrf_token(session).await);
    context.insert("error", &error);
    // Placed under People, where the user edit form that links here lives.
    context.insert("path", "/admin/people");
    render_admin_template(
        state,
        &super::helpers::user_context_for(state, actor).await,
        "admin/user-cancel.html",
        context,
    )
    .await
}

/// POST /user/{id}/cancel: cancel the account by the chosen method.
async fn cancel_submit(
    State(state): State<AppState>,
    session: Session,
    Path(user_id): Path<uuid::Uuid>,
    Form(form): Form<CancelForm>,
) -> Response {
    let (actor, target) = match cancel_refusal(&state, &session, user_id).await {
        Ok(pair) => pair,
        Err(refusal) => return refusal,
    };
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }
    if actor.id == target.id {
        return render_error("Cancel your own account from your account page.");
    }
    let Some(method) = CancelMethod::parse(form.method.trim()) else {
        return render_cancel_form(
            &state,
            &session,
            &actor,
            &target,
            Some("Choose how to cancel the account."),
        )
        .await;
    };
    if demotes_active_superuser(target.is_admin, target.status, false, 0)
        && !may_lose_a_superuser(&state).await
    {
        return render_forbidden("Cannot cancel the last active superuser.");
    }

    let acting = admin_user_context(&state, &actor).await;
    match cancel_account(&state, &target, method, &acting).await {
        Ok(()) => {
            let message = match method {
                CancelMethod::Block | CancelMethod::BlockUnpublish => {
                    format!("{} has been disabled.", target.name)
                }
                CancelMethod::Reassign | CancelMethod::Delete => {
                    format!("{} has been deleted.", target.name)
                }
            };
            let _ = session.insert(ACCESS_RULE_WARNING_KEY, message).await;
            Redirect::to("/admin/people").into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, user_id = %target.id, method = method.as_str(), "account cancellation failed");
            render_server_error("The account could not be cancelled.")
        }
    }
}

/// Cancel `target` by `method`, through the user, item and comment services
/// so every write keeps its taps, audit rows and side effects. Every session
/// of the account ends whichever method is used.
pub async fn cancel_account(
    state: &AppState,
    target: &User,
    method: CancelMethod,
    acting: &crate::tap::UserContext,
) -> anyhow::Result<()> {
    match method {
        CancelMethod::Block | CancelMethod::BlockUnpublish => {
            state
                .users()
                .update(
                    target.id,
                    UpdateUser {
                        status: Some(0),
                        ..Default::default()
                    },
                    acting,
                )
                .await?;
            if method == CancelMethod::BlockUnpublish {
                for item in crate::models::Item::list_by_author(state.db(), target.id).await? {
                    if item.status != 0 {
                        let unpublish = crate::models::UpdateItem {
                            author_id: None,
                            created: None,
                            stage_id: None,
                            title: None,
                            status: Some(0),
                            fields: None,
                            promote: None,
                            sticky: None,
                            log: Some(format!("Account {} cancelled", target.name)),
                        };
                        state.items().update(item.id, unpublish, acting).await?;
                    }
                }
                for comment in crate::models::Comment::list_by_author(state.db(), target.id).await?
                {
                    if comment.status != 0 {
                        let unpublish = crate::models::UpdateComment {
                            body: None,
                            body_format: None,
                            status: Some(0),
                            subject: None,
                        };
                        state
                            .comments()
                            .update(comment.id, unpublish, acting)
                            .await?;
                    }
                }
            }
            // A blocked account keeps its row, so its sessions are ended here;
            // the deleting methods end them inside `UserService::delete`.
            if let Err(e) = state
                .session_registry()
                .revoke_all_except(target.id, None)
                .await
            {
                tracing::error!(error = %e, user_id = %target.id, "failed to end a cancelled account's sessions");
            }
        }
        CancelMethod::Reassign | CancelMethod::Delete => {
            if method == CancelMethod::Delete {
                for comment in crate::models::Comment::list_by_author(state.db(), target.id).await?
                {
                    state.comments().delete(comment.id, acting).await?;
                }
                for item in crate::models::Item::list_by_author(state.db(), target.id).await? {
                    state.items().delete(item.id, acting).await?;
                }
            }
            // `delete` ends the account's sessions itself.
            state
                .users()
                .delete(target.id, acting, state.session_registry())
                .await?;
        }
    }
    Ok(())
}

// =============================================================================
// Role Management
// =============================================================================

/// List all roles.
///
/// GET /admin/people/roles
async fn list_roles(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "administer users").await {
        return redirect;
    }

    let roles = match state.roles().list().await {
        Ok(roles) => roles,
        Err(e) => {
            tracing::error!(error = %e, "failed to list roles");
            return render_server_error("Failed to load roles.");
        }
    };

    // Members and permissions per role. Deleting a role removes its assignments
    // (`user_roles.role_id` is ON DELETE CASCADE), so the count belongs on the
    // screen that offers the delete rather than in the surprise afterwards.
    let mut rows = Vec::with_capacity(roles.len());
    for role in roles {
        let members = state.roles().member_count(role.id).await.unwrap_or(0);
        let permissions = state
            .roles()
            .get_permissions(role.id)
            .await
            .map(|p| p.len())
            .unwrap_or(0);
        let built_in = role.id == ANONYMOUS_ROLE_ID || role.id == AUTHENTICATED_ROLE_ID;
        rows.push(RoleRow {
            id: role.id,
            name: role.name,
            members,
            permissions,
            built_in,
        });
    }

    let csrf_token = generate_csrf_token(&session).await;

    let mut context = tera::Context::new();
    context.insert("roles", &rows);
    context.insert("anonymous_role_id", &ANONYMOUS_ROLE_ID.to_string());
    context.insert("authenticated_role_id", &AUTHENTICATED_ROLE_ID.to_string());
    context.insert("csrf_token", &csrf_token);
    context.insert("path", "/admin/people/roles");

    render_admin_template(
        &state,
        &crate::routes::helpers::get_user_context(&session, &state).await,
        "admin/roles.html",
        context,
    )
    .await
}

/// Show add role form.
///
/// GET /admin/people/roles/add
async fn add_role_form(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "administer users").await {
        return redirect;
    }

    let csrf_token = generate_csrf_token(&session).await;
    let form_build_id = uuid::Uuid::new_v4().to_string();

    let mut context = tera::Context::new();
    context.insert("action", "/admin/people/roles/add");
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert("editing", &false);
    context.insert(
        "values",
        &serde_json::json!({ "requires_second_factor": false }),
    );
    context.insert("path", "/admin/people/roles/add");

    render_admin_template(
        &state,
        &crate::routes::helpers::get_user_context(&session, &state).await,
        "admin/role-form.html",
        context,
    )
    .await
}

/// Handle add role form submission.
///
/// POST /admin/people/roles/add
async fn add_role_submit(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<RoleFormData>,
) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "administer users").await {
        return redirect;
    }

    // Verify CSRF token
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    // Validate
    let mut errors = Vec::new();

    if form.name.trim().is_empty() {
        errors.push(FieldError::on("name", "Role name is required."));
    }

    // Check if role name already exists
    if let Ok(Some(_)) = state.roles().find_by_name(&form.name).await {
        errors.push(FieldError::on(
            "name",
            UiMessage::new("A role named '{name}' already exists.").arg("name", &form.name),
        ));
    }

    if !errors.is_empty() {
        let csrf_token = generate_csrf_token(&session).await;
        let form_build_id = uuid::Uuid::new_v4().to_string();

        let mut context = tera::Context::new();
        context.insert("action", "/admin/people/roles/add");
        context.insert("csrf_token", &csrf_token);
        context.insert("form_build_id", &form_build_id);
        context.insert("editing", &false);
        context.insert("errors", &errors);
        context.insert(
            "values",
            &serde_json::json!({
                "name": form.name,
                "requires_second_factor": form.requires_second_factor.is_some(),
            }),
        );
        context.insert("path", "/admin/people/roles/add");

        return render_admin_template(
            &state,
            &crate::routes::helpers::get_user_context(&session, &state).await,
            "admin/role-form.html",
            context,
        )
        .await;
    }

    match state.roles().create(&form.name).await {
        Ok(role) => {
            if let Err(e) = crate::models::Role::set_requires_second_factor(
                state.db(),
                role.id,
                form.requires_second_factor.is_some(),
            )
            .await
            {
                tracing::error!(error = %e, "failed to save the role's second-factor requirement");
            }
            tracing::info!(name = %form.name, "role created");
            Redirect::to("/admin/people/roles").into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to create role");
            render_server_error("Failed to create role.")
        }
    }
}

/// Show edit role form.
///
/// GET /admin/people/roles/{id}/edit
async fn edit_role_form(
    State(state): State<AppState>,
    session: Session,
    Path(role_id): Path<uuid::Uuid>,
) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "administer users").await {
        return redirect;
    }

    let Some(role) = state.roles().find_by_id(role_id).await.ok().flatten() else {
        return render_not_found();
    };

    let permissions = state
        .roles()
        .get_permissions(role_id)
        .await
        .unwrap_or_default();

    let csrf_token = generate_csrf_token(&session).await;
    let form_build_id = uuid::Uuid::new_v4().to_string();

    let mut context = tera::Context::new();
    context.insert("action", &format!("/admin/people/roles/{role_id}/edit"));
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert("editing", &true);
    context.insert("role_id", &role_id.to_string());
    context.insert("role_permissions", &permissions);
    context.insert(
        "values",
        &serde_json::json!({
            "name": role.name,
            "requires_second_factor": role.requires_second_factor,
        }),
    );
    context.insert("path", &format!("/admin/people/roles/{role_id}/edit"));

    render_admin_template(
        &state,
        &crate::routes::helpers::get_user_context(&session, &state).await,
        "admin/role-form.html",
        context,
    )
    .await
}

/// Handle edit role form submission.
///
/// POST /admin/people/roles/{id}/edit
async fn edit_role_submit(
    State(state): State<AppState>,
    session: Session,
    Path(role_id): Path<uuid::Uuid>,
    Form(form): Form<RoleFormData>,
) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "administer users").await {
        return redirect;
    }

    // Verify CSRF token
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    let Some(existing_role) = state.roles().find_by_id(role_id).await.ok().flatten() else {
        return render_not_found();
    };

    // Validate
    let mut errors = Vec::new();

    if form.name.trim().is_empty() {
        errors.push(FieldError::on("name", "Role name is required."));
    }

    // Check if new name is taken by someone else
    if form.name != existing_role.name
        && let Ok(Some(_)) = state.roles().find_by_name(&form.name).await
    {
        errors.push(FieldError::on(
            "name",
            UiMessage::new("A role named '{name}' already exists.").arg("name", &form.name),
        ));
    }

    if !errors.is_empty() {
        let permissions = state
            .roles()
            .get_permissions(role_id)
            .await
            .unwrap_or_default();
        let csrf_token = generate_csrf_token(&session).await;
        let form_build_id = uuid::Uuid::new_v4().to_string();

        let mut context = tera::Context::new();
        context.insert("action", &format!("/admin/people/roles/{role_id}/edit"));
        context.insert("csrf_token", &csrf_token);
        context.insert("form_build_id", &form_build_id);
        context.insert("editing", &true);
        context.insert("role_id", &role_id.to_string());
        context.insert("role_permissions", &permissions);
        context.insert("errors", &errors);
        context.insert(
            "values",
            &serde_json::json!({
                "name": form.name,
                "requires_second_factor": form.requires_second_factor.is_some(),
            }),
        );
        context.insert("path", &format!("/admin/people/roles/{role_id}/edit"));

        return render_admin_template(
            &state,
            &crate::routes::helpers::get_user_context(&session, &state).await,
            "admin/role-form.html",
            context,
        )
        .await;
    }

    match state.roles().update(role_id, &form.name).await {
        Ok(_) => {
            if let Err(e) = crate::models::Role::set_requires_second_factor(
                state.db(),
                role_id,
                form.requires_second_factor.is_some(),
            )
            .await
            {
                tracing::error!(error = %e, "failed to save the role's second-factor requirement");
            }
            tracing::info!(role_id = %role_id, "role updated");
            Redirect::to("/admin/people/roles").into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to update role");
            render_server_error("Failed to update role.")
        }
    }
}

/// Delete a role.
///
/// POST /admin/people/roles/{id}/delete
async fn delete_role(
    State(state): State<AppState>,
    session: Session,
    Path(role_id): Path<uuid::Uuid>,
    Form(form): Form<CsrfOnlyForm>,
) -> Response {
    let current_user = match require_permission(&state, &session, "administer users").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    // Prevent deleting built-in roles
    if role_id == ANONYMOUS_ROLE_ID || role_id == AUTHENTICATED_ROLE_ID {
        return render_error("Cannot delete built-in roles.");
    }

    // Deleting a role takes its permissions from every member, which is a
    // revocation, so it is held to the rule every other revocation is.
    if !current_user.is_admin {
        let covered = match (
            Authority::of(&state, &current_user).await,
            state.roles().get_permissions(role_id).await,
        ) {
            (Ok(authority), Ok(role_permissions)) => authority.covers(&role_permissions),
            (Err(e), _) | (_, Err(e)) => {
                tracing::error!(error = %e, role_id = %role_id, "failed to load permissions for a role deletion");
                false
            }
        };
        if !covered {
            tracing::warn!(
                actor = %current_user.id,
                role_id = %role_id,
                "refused a role deletion carrying permissions the actor does not hold"
            );
            return render_forbidden(
                "You may not delete a role carrying permissions you do not hold.",
            );
        }
    }

    // Recorded before the delete, because the cascade takes the rows with it and
    // afterwards there is nothing left to count.
    let members = state.roles().member_count(role_id).await.unwrap_or(0);

    match state.roles().delete(role_id).await {
        Ok(true) => {
            tracing::info!(
                role_id = %role_id,
                members_unassigned = members,
                "role deleted; its assignments went with it"
            );
            Redirect::to("/admin/people/roles").into_response()
        }
        Ok(false) => render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to delete role");
            render_server_error("Failed to delete role.")
        }
    }
}

/// The role ids a submitted form ticked, for re-rendering after a validation
/// error so the operator does not lose their selection.
fn submitted_role_ids(
    roles: &[crate::models::Role],
    submitted: &std::collections::HashMap<String, String>,
) -> Vec<String> {
    roles
        .iter()
        .filter(|role| submitted.contains_key(&format!("role_{}", role.id)))
        .map(|role| role.id.to_string())
        .collect()
}

/// Apply the role checkboxes a user form submitted.
///
/// The authoritative iteration is over the roles the server knows, not over
/// whatever keys the browser sent, so an unknown `role_*` key names nothing and
/// does nothing.
///
/// # The escalation guard
///
/// `administer users` is a grantable permission, and roles carry permissions.
/// Without a guard, a delegated user administrator could assign themselves a
/// role holding permissions well beyond their own — `administer site` and the
/// structure screens, say — by way of the screen they were given to manage
/// usernames. (Since BL-33 that is an escalation of permissions and not a
/// promotion to site administrator: the superuser column is not grantable here
/// at all, and `administer site` is an ordinary permission.) So a non-superuser
/// may only grant or revoke a role whose permissions they already hold
/// themselves; see [`Authority`].
///
/// Roles the actor may not touch are left exactly as they are on the target,
/// granted or not, rather than being silently dropped. If the actor's own
/// permissions cannot be loaded, nothing is changed.
async fn apply_role_membership(
    state: &AppState,
    actor: &User,
    target_id: uuid::Uuid,
    submitted: &std::collections::HashMap<String, String>,
) {
    let roles = match state.roles().list().await {
        Ok(roles) => roles,
        Err(e) => {
            tracing::error!(error = %e, "failed to list roles while saving a user");
            return;
        }
    };

    let held: std::collections::HashSet<uuid::Uuid> = state
        .roles()
        .get_user_roles(target_id)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|r| r.id)
        .collect();

    // What the actor may delegate. Loaded once; a superuser skips the query.
    let authority = match Authority::of(state, actor).await {
        Ok(authority) => authority,
        Err(e) => {
            tracing::error!(error = %e, actor = %actor.id, "failed to load the actor's permissions while saving a user");
            return;
        }
    };

    for role in &roles {
        let wanted = submitted.contains_key(&format!("role_{}", role.id));
        let currently = held.contains(&role.id);
        if wanted == currently {
            continue;
        }

        if !actor.is_admin {
            // Fails closed: a role whose permissions cannot be read is one the
            // actor cannot be shown to hold.
            let may_delegate = match state.roles().get_permissions(role.id).await {
                Ok(role_permissions) => authority.covers(&role_permissions),
                Err(_) => false,
            };
            if !may_delegate {
                tracing::warn!(
                    actor = %actor.id,
                    target = %target_id,
                    role = %role.name,
                    "refused a role change carrying permissions the actor does not hold"
                );
                continue;
            }
        }

        let result = if wanted {
            state.roles().assign_to_user(target_id, role.id).await
        } else {
            state.roles().remove_from_user(target_id, role.id).await
        };
        if let Err(e) = result {
            tracing::error!(
                error = %e,
                target = %target_id,
                role = %role.name,
                "failed to change role membership"
            );
        }
    }
}

/// One row of the permission grid.
///
/// The grid used to iterate over bare strings, which left it no way to say who
/// owns a permission or what it does. Now that a plugin's permissions appear
/// beside the kernel's, "who declared this" is the difference between a grid an
/// administrator can reason about and a flat list of strings from nowhere.
#[derive(Debug, serde::Serialize)]
struct GridPermission {
    /// The permission string itself.
    name: String,
    /// Who declared it: `Kernel`, or the plugin's name.
    provider: String,
    /// Human-readable description; empty for kernel permissions, which have
    /// never carried one.
    description: String,
}

/// Every permission the grid renders: the kernel's, then each plugin's.
///
/// This is also the exact set the save is allowed to revoke, which is why both
/// sides go through one function. A permission missing from here is one the
/// screen cannot show, cannot grant, and must not take away.
fn grid_permissions(state: &AppState) -> Vec<GridPermission> {
    let mut rows: Vec<GridPermission> = KERNEL_PERMISSIONS
        .iter()
        .map(|name| GridPermission {
            name: (*name).to_string(),
            provider: "Kernel".to_string(),
            description: String::new(),
        })
        .collect();

    // `use <format>` for each stored text format the kernel's list does not
    // already name (B108): a site's own formats are granted here too.
    for format in crate::content::text_format::all().iter() {
        if let Some(permission) = format.permission()
            && !rows.iter().any(|r| r.name == permission)
        {
            rows.push(GridPermission {
                name: permission,
                provider: "Text formats".to_string(),
                description: format!("Use the {} text format.", format.name),
            });
        }
    }

    for declared in state.plugin_permissions().permissions() {
        rows.push(GridPermission {
            name: declared.name.clone(),
            provider: declared.plugin.clone(),
            description: declared.description.clone(),
        });
    }

    rows
}

/// Show permission matrix.
///
/// GET /admin/people/permissions
async fn permissions_matrix(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "administer users").await {
        return redirect;
    }

    let roles = match state.roles().list().await {
        Ok(roles) => roles,
        Err(e) => {
            tracing::error!(error = %e, "failed to list roles");
            return render_error("Failed to load roles.");
        }
    };

    // Get permissions for each role
    let mut role_permissions: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for role in &roles {
        let perms = state
            .roles()
            .get_permissions(role.id)
            .await
            .unwrap_or_default();
        role_permissions.insert(role.id.to_string(), perms);
    }

    let csrf_token = generate_csrf_token(&session).await;
    let form_build_id = uuid::Uuid::new_v4().to_string();

    let mut context = tera::Context::new();
    context.insert("roles", &roles);
    context.insert("role_permissions", &role_permissions);
    context.insert("available_permissions", &grid_permissions(&state));
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert("path", "/admin/people/permissions");

    render_admin_template(
        &state,
        &crate::routes::helpers::get_user_context(&session, &state).await,
        "admin/permissions.html",
        context,
    )
    .await
}

/// Save permission matrix.
///
/// POST /admin/people/permissions
async fn save_permissions(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<PermissionFormData>,
) -> Response {
    let current_user = match require_permission(&state, &session, "administer users").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    // Verify CSRF token
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    let roles = match state.roles().list().await {
        Ok(roles) => roles,
        Err(e) => {
            tracing::error!(error = %e, "failed to list roles");
            return render_server_error("Failed to load roles.");
        }
    };

    // The form states which permissions it rendered, as `permname_{i}`, and
    // checks them as `perm_{i}_{role_id}`.
    //
    // The index matters. The old encoding put the permission name in the
    // checkbox key with spaces replaced by underscores, which is not
    // reversible: `create argus_feed content` and a hypothetical `create argus
    // feed content` produce the same key. That was harmless while the grid
    // rendered only the kernel's 21 names and stopped being harmless the moment
    // plugin permissions appeared, since plugin content permissions routinely
    // contain underscores. The name now travels as a value, where nothing has
    // to be escaped out of it.
    //
    // Reading the rendered set from the form rather than recomputing it is
    // deliberate too: it is the set the administrator actually saw, so it stays
    // correct even if a plugin was enabled or disabled between the render and
    // the save.
    let mut rendered: Vec<(usize, String)> = Vec::new();
    for index in 0usize.. {
        let Some(name) = form.permissions.get(&format!("permname_{index}")) else {
            break;
        };
        rendered.push((index, name.clone()));
    }

    // Two filters on what the form claims it rendered, both of which leave a
    // dropped permission exactly as it was on every role.
    //
    // A name the grid does not know is dropped for everyone, superusers
    // included: this screen can only have rendered names from
    // `grid_permissions`, so anything else was typed into the request, and it
    // is not a permission anything checks.
    //
    // A permission the actor does not hold is dropped for a non-superuser, so
    // it is neither granted nor revoked on any role: the grid is the
    // `apply_role_membership` rule applied one box at a time.
    let known: std::collections::HashSet<String> = grid_permissions(&state)
        .into_iter()
        .map(|p| p.name)
        .collect();
    let authority = match Authority::of(&state, &current_user).await {
        Ok(authority) => authority,
        Err(e) => {
            tracing::error!(error = %e, actor = %current_user.id, "failed to load the actor's permissions; the grid was not saved");
            return Redirect::to("/admin/people/permissions").into_response();
        }
    };
    let mut unknown = Vec::new();
    let mut refused = Vec::new();
    rendered.retain(|(_, name)| {
        if !known.contains(name) {
            unknown.push(name.clone());
            false
        } else if !authority.covers(std::iter::once(name)) {
            refused.push(name.clone());
            false
        } else {
            true
        }
    });
    if !unknown.is_empty() {
        tracing::warn!(
            actor = %current_user.id,
            permissions = ?unknown,
            "ignored permission names the grid does not render"
        );
    }
    if !refused.is_empty() {
        tracing::warn!(
            actor = %current_user.id,
            permissions = ?refused,
            "refused grid changes to permissions the actor does not hold"
        );
    }

    let rendered_names: std::collections::HashSet<String> =
        rendered.iter().map(|(_, name)| name.clone()).collect();

    for role in &roles {
        let desired: Vec<String> = rendered
            .iter()
            .filter(|(index, _)| {
                form.permissions
                    .contains_key(&format!("perm_{index}_{}", role.id))
            })
            .map(|(_, name)| name.clone())
            .collect();

        // Scoped to what the form rendered: a permission this screen never
        // showed is left exactly as it was, granted or not. Replacing the whole
        // set here is what deleted 29 plugin grants across every role on a
        // 0.102.0 site.
        if let Err(e) = state
            .roles()
            .save_permissions_within(role.id, &desired, &rendered_names)
            .await
        {
            tracing::error!(error = %e, role_id = %role.id, "failed to save permissions");
        }
    }

    tracing::info!(rendered = rendered_names.len(), "permissions updated");
    Redirect::to("/admin/people/permissions").into_response()
}

/// Build the router for admin user, role, and permission routes.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/people", get(list_users))
        .route(
            "/admin/people/add",
            get(add_user_form).post(add_user_submit),
        )
        .route(
            "/admin/people/{id}/edit",
            get(edit_user_form).post(edit_user_submit),
        )
        .route("/admin/people/{id}/delete", post(delete_user))
        .route(
            "/admin/people/{id}/second-factor/reset",
            post(reset_second_factor),
        )
        .route("/user/{id}/cancel", get(cancel_form).post(cancel_submit))
        .route("/admin/people/roles", get(list_roles))
        .route(
            "/admin/people/roles/add",
            get(add_role_form).post(add_role_submit),
        )
        .route(
            "/admin/people/roles/{id}/edit",
            get(edit_role_form).post(edit_role_submit),
        )
        .route("/admin/people/roles/{id}/delete", post(delete_role))
        .route(
            "/admin/people/permissions",
            get(permissions_matrix).post(save_permissions),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(names: &[&str]) -> Authority {
        Authority::Holds(names.iter().map(|n| (*n).to_string()).collect())
    }

    fn set(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_string()).collect()
    }

    #[test]
    fn a_superuser_covers_anything() {
        assert!(Authority::Unrestricted.covers(&set(&["administer site", "anything"])));
    }

    #[test]
    fn a_delegate_covers_only_what_they_hold() {
        let authority = held(&["administer users", "access content"]);
        assert!(authority.covers(&set(&["access content"])));
        assert!(authority.covers(&set(&["administer users", "access content"])));
        assert!(!authority.covers(&set(&["access content", "administer site"])));
    }

    #[test]
    fn nothing_is_covered_by_anyone() {
        assert!(held(&[]).covers(&set(&[])));
    }

    #[test]
    fn clearing_the_flag_or_blocking_demotes_an_active_superuser() {
        assert!(demotes_active_superuser(true, 1, false, 1), "flag cleared");
        assert!(demotes_active_superuser(true, 1, true, 0), "blocked");
        assert!(demotes_active_superuser(true, 1, false, 0), "both");
    }

    #[test]
    fn keeping_the_flag_and_the_status_demotes_nobody() {
        assert!(!demotes_active_superuser(true, 1, true, 1));
    }

    #[test]
    fn someone_who_is_not_an_active_superuser_cannot_be_demoted_from_it() {
        assert!(
            !demotes_active_superuser(false, 1, false, 0),
            "not a superuser"
        );
        assert!(
            !demotes_active_superuser(true, 0, false, 0),
            "a blocked superuser is not counted as active"
        );
        assert!(
            !demotes_active_superuser(false, 1, true, 1),
            "promotion is not demotion"
        );
    }

    /// The two halves together, as `edit_user_submit` combines them.
    #[test]
    fn the_last_active_superuser_cannot_be_demoted() {
        let refused =
            |count| demotes_active_superuser(true, 1, false, 1) && blocks_last_admin(true, count);
        assert!(refused(1), "the last one");
        assert!(refused(0), "a count that has already gone wrong");
        assert!(!refused(2), "one of two may go");
    }
}
