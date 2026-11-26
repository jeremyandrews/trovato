//! Owner routes for owned vocabularies (B108).
//!
//! For every owner type a plugin declares (`[[vocabulary_owners]]`), the
//! kernel serves, under the declared path `P` (e.g.
//! `/group/{owner}/categories`):
//!
//! | Route | |
//! |---|---|
//! | `GET P` | the owner's vocabularies |
//! | `GET P/{vocabulary}` | the vocabulary's term list |
//! | `GET`, `POST P/{vocabulary}/add` | the add-term form |
//! | `GET`, `POST P/{vocabulary}/terms/{term}/edit` | the edit-term form |
//! | `POST P/{vocabulary}/terms/{term}/delete` | delete a term |
//!
//! The term list and term form are the kernel's own (`admin_taxonomy`), with
//! this route's URLs. Every request is admitted by
//! `content::owned_vocabulary::check_access`: an anonymous user is sent to
//! sign in, a user the owner's check refuses gets 403, an owner the plugin
//! does not know is 404, and so is a vocabulary or term of another owner.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Form, Router};
use tower_sessions::Session;

use crate::content::owned_vocabulary::{self, OwnerAccess, RegisteredOwner};
use crate::models::{Category, Tag};
use crate::state::AppState;

use super::admin_taxonomy::{self, TermForm, TermUrls};
use super::helpers::{
    CsrfOnlyForm, render_admin_template, render_forbidden, render_not_found, require_login,
};

/// Routes for every owner the loaded plugins declare.
pub fn router(state: &AppState) -> Router<AppState> {
    let mut router = Router::new();
    for owner in owned_vocabulary::owners_of(state.plugin_runtime()) {
        let owner = Arc::new(owner);
        let base = owner.decl.path.clone();
        tracing::debug!(
            plugin = %owner.plugin,
            owner_type = %owner.decl.owner_type,
            path = %base,
            "registered owned vocabulary routes"
        );
        let o = Arc::clone(&owner);
        router = router.route(
            &base,
            get(move |state, session, path| index(o.clone(), state, session, path)),
        );
        let o = Arc::clone(&owner);
        router = router.route(
            &format!("{base}/{{vocabulary}}"),
            get(move |state, session, path, query| list(o.clone(), state, session, path, query)),
        );
        let (o, p) = (Arc::clone(&owner), Arc::clone(&owner));
        router = router.route(
            &format!("{base}/{{vocabulary}}/add"),
            get(move |state, session, path| add_form(o.clone(), state, session, path)).post(
                move |state, session, path, form| add_submit(p.clone(), state, session, path, form),
            ),
        );
        let (o, p) = (Arc::clone(&owner), Arc::clone(&owner));
        router = router.route(
            &format!("{base}/{{vocabulary}}/terms/{{term}}/edit"),
            get(move |state, session, path| edit_form(o.clone(), state, session, path)).post(
                move |state, session, path, form| {
                    edit_submit(p.clone(), state, session, path, form)
                },
            ),
        );
        let o = Arc::clone(&owner);
        router = router.route(
            &format!("{base}/{{vocabulary}}/terms/{{term}}/delete"),
            post(move |state, session, path, form| delete(o.clone(), state, session, path, form)),
        );
    }
    router
}

type Params = Path<HashMap<String, String>>;

/// Admit the request: the owner id, or the refusal.
async fn admit(
    owner: &RegisteredOwner,
    state: &AppState,
    session: &Session,
    owner_key: &str,
) -> Result<String, Response> {
    if !state.is_plugin_enabled(&owner.plugin) {
        return Err(render_not_found());
    }
    require_login(state, session).await?;
    let user = super::helpers::get_user_context(session, state).await;
    match owned_vocabulary::check_access(state, owner, owner_key, &user).await {
        OwnerAccess::Granted { owner_id } => Ok(owner_id),
        OwnerAccess::Denied => Err(render_forbidden(
            "You do not have permission to manage these categories.",
        )),
        OwnerAccess::NotFound => Err(render_not_found()),
    }
}

/// The owner's vocabulary `id`, or 404.
async fn vocabulary_of(
    owner: &RegisteredOwner,
    state: &AppState,
    owner_id: &str,
    id: &str,
) -> Result<Category, Response> {
    match state.categories().get_category(id).await {
        Ok(Some(category))
            if category.owner() == Some((owner.decl.owner_type.as_str(), owner_id)) =>
        {
            Ok(category)
        }
        _ => Err(render_not_found()),
    }
}

/// The vocabulary's term `id`, or 404.
async fn term_of(state: &AppState, category: &Category, id: &str) -> Result<Tag, Response> {
    let Ok(id) = uuid::Uuid::parse_str(id) else {
        return Err(render_not_found());
    };
    match state.categories().get_tag(id).await {
        Ok(Some(tag)) if tag.category_id == category.id => Ok(tag),
        _ => Err(render_not_found()),
    }
}

fn urls(owner: &RegisteredOwner, owner_key: &str, vocabulary: &str) -> TermUrls {
    let base = owner.base_url(owner_key);
    TermUrls {
        index: base.clone(),
        index_label: "Categories".to_string(),
        list: format!("{base}/{vocabulary}"),
        add: format!("{base}/{vocabulary}/add"),
        term_base: format!("{base}/{vocabulary}/terms"),
    }
}

/// Everything a handler under a vocabulary needs: the owner id and the
/// vocabulary, or the refusal.
async fn admit_vocabulary(
    owner: &RegisteredOwner,
    state: &AppState,
    session: &Session,
    params: &HashMap<String, String>,
) -> Result<(Category, TermUrls), Response> {
    let owner_key = params.get("owner").map(String::as_str).unwrap_or("");
    let vocabulary = params.get("vocabulary").map(String::as_str).unwrap_or("");
    let owner_id = admit(owner, state, session, owner_key).await?;
    let category = vocabulary_of(owner, state, &owner_id, vocabulary).await?;
    let urls = urls(owner, owner_key, &category.id);
    Ok((category, urls))
}

/// GET P
async fn index(
    owner: Arc<RegisteredOwner>,
    State(state): State<AppState>,
    session: Session,
    Path(params): Params,
) -> Response {
    let owner_key = params.get("owner").map(String::as_str).unwrap_or("");
    let owner_id = match admit(&owner, &state, &session, owner_key).await {
        Ok(id) => id,
        Err(refusal) => return refusal,
    };
    let vocabularies =
        match Category::list_owned(state.db(), &owner.decl.owner_type, &owner_id).await {
            Ok(list) => list,
            Err(e) => {
                tracing::error!(error = %e, "failed to list owned vocabularies");
                return super::helpers::render_server_error("Failed to load categories.");
            }
        };
    let mut counts: HashMap<String, i64> = HashMap::new();
    for v in &vocabularies {
        let count = state.categories().count_tags(&v.id).await.unwrap_or(0);
        counts.insert(v.id.clone(), count);
    }
    let base = owner.base_url(owner_key);
    let mut context = tera::Context::new();
    context.insert("vocabularies", &vocabularies);
    context.insert("tag_counts", &counts);
    context.insert("owner_label", owner.label());
    context.insert("owner_key", owner_key);
    context.insert("base", &base);
    context.insert("path", &base);
    render_admin_template(
        &state,
        &super::helpers::get_user_context(&session, &state).await,
        "admin/owned-vocabularies.html",
        context,
    )
    .await
}

/// GET P/{vocabulary}
async fn list(
    owner: Arc<RegisteredOwner>,
    State(state): State<AppState>,
    session: Session,
    Path(params): Params,
    axum::extract::Query(query): axum::extract::Query<HashMap<String, String>>,
) -> Response {
    match admit_vocabulary(&owner, &state, &session, &params).await {
        Ok((category, urls)) => {
            admin_taxonomy::term_list(&state, &session, &category, &urls, &query).await
        }
        Err(refusal) => refusal,
    }
}

/// GET P/{vocabulary}/add
async fn add_form(
    owner: Arc<RegisteredOwner>,
    State(state): State<AppState>,
    session: Session,
    Path(params): Params,
) -> Response {
    match admit_vocabulary(&owner, &state, &session, &params).await {
        Ok((category, urls)) => {
            admin_taxonomy::term_add_form(&state, &session, &category, &urls).await
        }
        Err(refusal) => refusal,
    }
}

/// POST P/{vocabulary}/add
async fn add_submit(
    owner: Arc<RegisteredOwner>,
    State(state): State<AppState>,
    session: Session,
    Path(params): Params,
    Form(form): Form<TermForm>,
) -> Response {
    match admit_vocabulary(&owner, &state, &session, &params).await {
        Ok((category, urls)) => {
            admin_taxonomy::term_add_submit(&state, &session, &category, form, &urls).await
        }
        Err(refusal) => refusal,
    }
}

/// GET P/{vocabulary}/terms/{term}/edit
async fn edit_form(
    owner: Arc<RegisteredOwner>,
    State(state): State<AppState>,
    session: Session,
    Path(params): Params,
) -> Response {
    let (category, urls) = match admit_vocabulary(&owner, &state, &session, &params).await {
        Ok(found) => found,
        Err(refusal) => return refusal,
    };
    let term = params.get("term").map(String::as_str).unwrap_or("");
    match term_of(&state, &category, term).await {
        Ok(tag) => admin_taxonomy::term_edit_form(&state, &session, &category, &tag, &urls).await,
        Err(refusal) => refusal,
    }
}

/// POST P/{vocabulary}/terms/{term}/edit
async fn edit_submit(
    owner: Arc<RegisteredOwner>,
    State(state): State<AppState>,
    session: Session,
    Path(params): Params,
    Form(form): Form<TermForm>,
) -> Response {
    let (category, urls) = match admit_vocabulary(&owner, &state, &session, &params).await {
        Ok(found) => found,
        Err(refusal) => return refusal,
    };
    let term = params.get("term").map(String::as_str).unwrap_or("");
    match term_of(&state, &category, term).await {
        Ok(tag) => {
            admin_taxonomy::term_edit_submit(&state, &session, &category, &tag, form, &urls).await
        }
        Err(refusal) => refusal,
    }
}

/// POST P/{vocabulary}/terms/{term}/delete
async fn delete(
    owner: Arc<RegisteredOwner>,
    State(state): State<AppState>,
    session: Session,
    Path(params): Params,
    Form(form): Form<CsrfOnlyForm>,
) -> Response {
    let (category, urls) = match admit_vocabulary(&owner, &state, &session, &params).await {
        Ok(found) => found,
        Err(refusal) => return refusal,
    };
    let term = params.get("term").map(String::as_str).unwrap_or("");
    match term_of(&state, &category, term).await {
        Ok(tag) => admin_taxonomy::term_delete(&state, &session, &tag, &form.token, &urls).await,
        Err(refusal) => refusal,
    }
}
