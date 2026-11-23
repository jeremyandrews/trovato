//! B108 fixture: a site plugin whose **clubs own vocabularies**.
//!
//! The smallest thing that uses the whole owned-vocabulary surface:
//!
//! - the manifest declares the `club` owner type and its route,
//!   `/club/{owner}/categories`;
//! - `POST /tov/clubs/:slug` creates a club and its first vocabulary through
//!   `vocabulary::create-owned`;
//! - `GET /tov/clubs/:slug/vocabularies` lists them through `list-owned`;
//! - `tap_vocabulary_owner_access` resolves a club slug to its id and grants
//!   the club's managers (`tov_manager`).

use trovato_sdk::host;
use trovato_sdk::prelude::*;
use trovato_sdk::types::{
    ApiRequest, ApiResponse, MenuRoute, OwnedVocabularyRequest, VocabularyOwnerAccess,
    VocabularyOwnerAccessRequest,
};

/// May create clubs through the fixture's API.
const PERM_CREATE: &str = "create test clubs";
/// Administers every club's categories (declared in the manifest).
const PERM_ALL_CLUBS: &str = "administer every club's categories";

#[plugin_tap]
pub fn tap_perm() -> Vec<PermissionDefinition> {
    vec![
        PermissionDefinition::new(PERM_CREATE, "Create clubs through the test API"),
        PermissionDefinition::new(PERM_ALL_CLUBS, "Administer the categories of every club"),
    ]
}

#[plugin_tap]
pub fn tap_menu() -> Vec<MenuRoute> {
    vec![
        MenuRoute::api("POST", "/tov/clubs/:slug", "create_club")
            .title("Create a club")
            .permission(PERM_CREATE),
        MenuRoute::api("GET", "/tov/clubs/:slug/vocabularies", "list_vocabularies")
            .title("A club's vocabularies")
            .permission(PERM_CREATE),
    ]
}

#[plugin_tap]
pub fn tap_api(request: ApiRequest) -> ApiResponse {
    match request.callback.as_str() {
        "create_club" => create_club(&request),
        "list_vocabularies" => list_vocabularies(&request),
        other => ApiResponse::error(404, &format!("no such callback: {other}")),
    }
}

/// Which club a route's `{owner}` names, and whether this user manages it.
///
/// For the read operation, `view`, the key is the club's id rather than its
/// slug, and a club's managers may read its vocabularies; nobody else is
/// granted, so the kernel hides them from everyone else (B189).
#[plugin_tap]
pub fn tap_vocabulary_owner_access(request: VocabularyOwnerAccessRequest) -> VocabularyOwnerAccess {
    if request.operation == "view" {
        let club_id = request.owner_key;
        return if request.authenticated && manages(&club_id, &request.user_id) {
            VocabularyOwnerAccess::grant(club_id)
        } else {
            VocabularyOwnerAccess::neutral(club_id)
        };
    }
    let Some(club_id) = club_id_of(&request.owner_key) else {
        return VocabularyOwnerAccess::not_found();
    };
    if request.authenticated && manages(&club_id, &request.user_id) {
        VocabularyOwnerAccess::grant(club_id)
    } else {
        // Not a manager: the declared permission still admits.
        VocabularyOwnerAccess::neutral(club_id)
    }
}

fn first_row(json: &str) -> Option<serde_json::Value> {
    serde_json::from_str::<Vec<serde_json::Value>>(json)
        .ok()?
        .into_iter()
        .next()
}

fn club_id_of(slug: &str) -> Option<String> {
    let rows = host::query_raw(
        "SELECT id FROM tov_club WHERE slug = $1",
        &[serde_json::json!(slug)],
    )
    .ok()?;
    first_row(&rows)?.get("id")?.as_str().map(str::to_string)
}

fn manages(club_id: &str, user_id: &str) -> bool {
    host::query_raw(
        "SELECT 1 AS yes FROM tov_manager WHERE club_id = $1 AND user_id = $2::uuid",
        &[serde_json::json!(club_id), serde_json::json!(user_id)],
    )
    .ok()
    .and_then(|rows| first_row(&rows))
    .is_some()
}

/// `POST /tov/clubs/:slug` with `{"id", "vocabulary", "label", "owner_type"?}`:
/// the club and one vocabulary it owns. `owner_type` overrides `club`, so a
/// test can ask for an owner type the plugin does not declare.
fn create_club(request: &ApiRequest) -> ApiResponse {
    let Some(slug) = request.params.get("slug") else {
        return ApiResponse::error(400, "missing slug");
    };
    let body: serde_json::Value = match request.json() {
        Ok(v) => v,
        Err(e) => return ApiResponse::error(400, &format!("body is not JSON: {e}")),
    };
    let field = |name: &str| body.get(name).and_then(|v| v.as_str()).unwrap_or_default();
    let (club_id, vocabulary, label) = (field("id"), field("vocabulary"), field("label"));
    let owner_type = body
        .get("owner_type")
        .and_then(|v| v.as_str())
        .unwrap_or("club");

    if let Err(code) = host::execute_raw(
        "INSERT INTO tov_club (slug, id, name) VALUES ($1, $2, $1) ON CONFLICT (slug) DO NOTHING",
        &[serde_json::json!(slug), serde_json::json!(club_id)],
    ) {
        return ApiResponse::error(500, &format!("club write failed with code {code}"));
    }

    let request = OwnedVocabularyRequest::new(owner_type, club_id, vocabulary, label)
        .description("Created by the test fixture")
        .hierarchy(1);
    match host::create_owned_vocabulary(&request) {
        Ok(created) => ApiResponse::json(&created)
            .unwrap_or_else(|_| ApiResponse::error(500, "serialize failed")),
        Err(code) => {
            ApiResponse::with_status(422, serde_json::json!({ "error_code": code }).to_string())
        }
    }
}

/// `GET /tov/clubs/:slug/vocabularies`: the club's vocabularies.
fn list_vocabularies(request: &ApiRequest) -> ApiResponse {
    let Some(club_id) = request.params.get("slug").and_then(|s| club_id_of(s)) else {
        return ApiResponse::error(404, "no such club");
    };
    match host::list_owned_vocabularies("club", &club_id) {
        Ok(list) => {
            ApiResponse::json(&list).unwrap_or_else(|_| ApiResponse::error(500, "serialize failed"))
        }
        Err(code) => ApiResponse::error(500, &format!("list failed with code {code}")),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn the_menu_declares_two_api_routes() {
        let routes = __inner_tap_menu();
        assert_eq!(routes.len(), 2);
    }

    #[test]
    fn a_missing_slug_is_a_400() {
        let request = ApiRequest::new("create_club", "POST", "/tov/clubs/", "u", true);
        assert_eq!(create_club(&request).status, 400);
    }
}
