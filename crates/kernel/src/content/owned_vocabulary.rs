//! Owned vocabularies (B108).
//!
//! A vocabulary (a `category` row) normally belongs to the site: one list of
//! terms, administered by whoever holds `administer categories`. A site
//! plugin that models groups, clubs or projects can give each of them their
//! own vocabularies instead, administered by that owner's managers:
//!
//! 1. The plugin declares the kind of owner in its manifest
//!    (`[[vocabulary_owners]]`, [`VocabularyOwnerDecl`]): the owner type, the
//!    route the owner's term administration is served under, and optionally a
//!    permission whose holders administer every owner's terms.
//! 2. It creates a vocabulary for one owner through the `vocabulary` host
//!    interface (`create-owned`), which stores the owner on the row
//!    (`category.owner_type`, `category.owner_id`). A site-wide vocabulary
//!    has neither, which is what every vocabulary created before B108 is.
//! 3. It answers `tap_vocabulary_owner_access` ([`AccessRequest`] in,
//!    [`AccessAnswer`] out): which owner the route's `{owner}` segment names,
//!    and whether this user may administer its terms.
//!
//! The kernel then serves, under the declared path, the owner's vocabulary
//! list and the same term list and term form `/admin/structure/categories`
//! uses (`routes::owned_vocabulary`), and refuses an item reference field
//! limited to the item's owner (a field setting, [`OwnerLimit`]) a term from
//! any other vocabulary ([`TermsNotOwned`]).

use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::Category;
use crate::plugin::VocabularyOwnerDecl;
use crate::tap::UserContext;

/// The tap a plugin answers to resolve an owner and decide access.
pub const ACCESS_TAP: &str = "tap_vocabulary_owner_access";

/// The operation [`AccessRequest::operation`] names: everything the owner
/// route does (list the vocabularies, list, add, edit and delete terms).
pub const ADMINISTER_TERMS: &str = "administer terms";

/// The path segment the owner fills in an owner route.
pub const OWNER_SEGMENT: &str = "{owner}";

/// First path segments the kernel serves itself, which an owner route may
/// not take: axum refuses two routes on one path, and an owner route must not
/// shadow a kernel page.
pub const RESERVED_PREFIXES: &[&str] = &[
    "admin",
    "api",
    "static",
    "user",
    "users",
    "item",
    "node",
    "install",
    "system",
    "cron",
    "file",
    "files",
    "category",
    "categories",
    "tag",
    "tags",
    "gather",
    "rss",
    "feed",
    "sitemap.xml",
    "robots.txt",
    "health",
    "metrics",
    "login",
    "logout",
    "register",
    "search",
    "comment",
    "comments",
    "media",
    "assistant",
    "chat",
    "oauth",
    "webauthn",
    "password",
    "account",
    "plugins",
    "themes",
    "batch",
    "form",
    "ajax",
];

/// Whether `name` may be an owner type: `[a-z][a-z0-9_]*`, at most 64.
pub fn is_owner_type(name: &str) -> bool {
    let mut chars = name.chars();
    name.len() <= 64
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Whether `owner_type` and `owner_id` may be stored together: both absent
/// (a site-wide vocabulary), or a valid owner type with a non-empty id of at
/// most 255 characters.
pub fn check_owner(owner_type: Option<&str>, owner_id: Option<&str>) -> Result<(), String> {
    match (owner_type, owner_id) {
        (None, None) => Ok(()),
        (Some(owner_type), Some(owner_id)) => {
            if !is_owner_type(owner_type) {
                return Err(format!(
                    "owner type '{owner_type}' must be lowercase letters, digits and \
                     underscores, starting with a letter"
                ));
            }
            if owner_id.trim().is_empty() || owner_id.len() > 255 {
                return Err("owner id must be 1 to 255 characters".to_string());
            }
            Ok(())
        }
        _ => Err("owner_type and owner_id must be set together".to_string()),
    }
}

/// Check a manifest declaration: the owner type, the route shape and the
/// permission name.
pub fn check_declaration(decl: &VocabularyOwnerDecl) -> Result<(), String> {
    if !is_owner_type(&decl.owner_type) {
        return Err(
            "owner_type must be lowercase letters, digits and underscores, starting with a \
             letter, at most 64 characters"
                .to_string(),
        );
    }
    check_path(&decl.path)?;
    if let Some(permission) = &decl.permission
        && permission.trim().is_empty()
    {
        return Err("permission, when given, must not be empty".to_string());
    }
    Ok(())
}

/// An owner route is `/<prefix>/{owner}/<name>`: three segments, the middle
/// one the owner, the outer two `[a-z0-9_-]+`, and a prefix the kernel does
/// not serve.
pub fn check_path(path: &str) -> Result<(), String> {
    let segments: Vec<&str> = path.strip_prefix('/').unwrap_or("").split('/').collect();
    let plain = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    };
    match segments.as_slice() {
        [prefix, owner, name] if *owner == OWNER_SEGMENT && plain(prefix) && plain(name) => {
            if RESERVED_PREFIXES.contains(prefix) {
                Err(format!(
                    "path '{path}' begins with /{prefix}, which the kernel serves"
                ))
            } else {
                Ok(())
            }
        }
        _ => Err(format!(
            "path '{path}' must be /<prefix>/{OWNER_SEGMENT}/<name>, e.g. \
             /group/{OWNER_SEGMENT}/categories"
        )),
    }
}

/// One declared kind of owner, with the plugin that declared it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredOwner {
    /// The declaring plugin.
    pub plugin: String,
    /// What it declared.
    pub decl: VocabularyOwnerDecl,
}

impl RegisteredOwner {
    /// The owner route with the owner filled in.
    pub fn base_url(&self, owner_key: &str) -> String {
        self.decl.path.replace(OWNER_SEGMENT, owner_key)
    }

    /// What the screens call one owner.
    pub fn label(&self) -> &str {
        if self.decl.label.trim().is_empty() {
            &self.decl.owner_type
        } else {
            &self.decl.label
        }
    }
}

/// Every declared owner, from `(plugin, declarations)` pairs. Plugins are
/// taken in name order; a second plugin declaring an owner type or a path
/// already taken is skipped with a warning, so the answer does not depend on
/// load order and no two routes collide.
pub fn registered_owners<'a>(
    sources: impl IntoIterator<Item = (&'a str, &'a [VocabularyOwnerDecl])>,
) -> Vec<RegisteredOwner> {
    let mut sources: Vec<(&str, &[VocabularyOwnerDecl])> = sources.into_iter().collect();
    sources.sort_by(|a, b| a.0.cmp(b.0));
    let mut types = HashSet::new();
    let mut paths = HashSet::new();
    let mut owners = Vec::new();
    for (plugin, decls) in sources {
        for decl in decls {
            if check_declaration(decl).is_err() {
                continue;
            }
            if !types.insert(decl.owner_type.clone()) {
                tracing::warn!(
                    plugin,
                    owner_type = %decl.owner_type,
                    "skipping a vocabulary owner type another plugin already declared"
                );
                continue;
            }
            if !paths.insert(decl.path.clone()) {
                tracing::warn!(
                    plugin,
                    path = %decl.path,
                    "skipping a vocabulary owner route another plugin already declared"
                );
                continue;
            }
            owners.push(RegisteredOwner {
                plugin: plugin.to_string(),
                decl: decl.clone(),
            });
        }
    }
    owners
}

/// The owners the loaded plugins declare.
pub fn owners_of(runtime: &crate::plugin::PluginRuntime) -> Vec<RegisteredOwner> {
    let plugins = runtime.plugins();
    registered_owners(
        plugins
            .iter()
            .map(|(name, plugin)| (name.as_str(), plugin.info.vocabulary_owners.as_slice())),
    )
}

/// Why [`create`] refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateRefused {
    /// The machine name is not `[a-z0-9_]+` or is too long.
    BadId,
    /// The label is empty.
    BadLabel,
    /// The owner is not a valid owner (see [`check_owner`]).
    BadOwner(String),
    /// A vocabulary with this machine name exists.
    Exists,
}

impl std::fmt::Display for CreateRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadId => write!(
                f,
                "the vocabulary id must be lowercase letters, digits and underscores"
            ),
            Self::BadLabel => write!(f, "the vocabulary needs a label"),
            Self::BadOwner(problem) => write!(f, "{problem}"),
            Self::Exists => write!(f, "a vocabulary with this id already exists"),
        }
    }
}

impl std::error::Error for CreateRefused {}

/// What a plugin asks for to create an owned vocabulary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateOwned {
    pub owner_type: String,
    pub owner_id: String,
    /// The vocabulary's machine name, unique across the site.
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
    /// 0 flat, 1 single parent, 2 multiple parents.
    #[serde(default)]
    pub hierarchy: Option<i16>,
    #[serde(default)]
    pub weight: Option<i16>,
}

/// Create a vocabulary owned by `input.owner_type` / `input.owner_id`.
pub async fn create(pool: &PgPool, input: CreateOwned) -> Result<Category> {
    if !crate::routes::helpers::is_valid_machine_name(&input.id) || input.id.len() > 32 {
        return Err(CreateRefused::BadId.into());
    }
    if input.label.trim().is_empty() {
        return Err(CreateRefused::BadLabel.into());
    }
    check_owner(Some(&input.owner_type), Some(&input.owner_id)).map_err(CreateRefused::BadOwner)?;
    if Category::exists(pool, &input.id).await? {
        return Err(CreateRefused::Exists.into());
    }
    let hierarchy = input.hierarchy.map(|h| h.clamp(0, 2));
    let mut tx = pool.begin().await.context("failed to begin")?;
    sqlx::query(
        "INSERT INTO category (id, label, description, hierarchy, weight, owner_type, owner_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(&input.id)
    .bind(input.label.trim())
    .bind(&input.description)
    .bind(hierarchy.unwrap_or(0))
    .bind(input.weight.unwrap_or(0))
    .bind(&input.owner_type)
    .bind(&input.owner_id)
    .execute(&mut *tx)
    .await
    .context("failed to create owned category")?;
    tx.commit().await.context("failed to commit")?;
    crate::cache::generation::bump(crate::cache::generation::CacheKind::Categories);
    Category::find_by_id(pool, &input.id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("failed to fetch created category"))
}

/// What the kernel sends `tap_vocabulary_owner_access`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessRequest {
    pub owner_type: String,
    /// The `{owner}` path segment as requested.
    pub owner_key: String,
    /// [`ADMINISTER_TERMS`].
    pub operation: String,
    pub user_id: Uuid,
    pub authenticated: bool,
}

/// What a plugin answers `tap_vocabulary_owner_access` with.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AccessAnswer {
    /// The owner id `owner_key` names, or `None` if there is no such owner.
    #[serde(default)]
    pub owner_id: Option<String>,
    /// `grant`, `deny` or `neutral` (the default).
    #[serde(default)]
    pub access: String,
}

/// The kernel's decision for one request to an owner route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnerAccess {
    /// The user may administer this owner's terms.
    Granted { owner_id: String },
    /// The owner exists and the user may not.
    Denied,
    /// No owner by that key.
    NotFound,
}

/// Combine the plugin's answer with the user: an anonymous user is always
/// refused; an unknown owner is not found; a plugin `deny` refuses anyone but
/// a superuser; otherwise a superuser, a plugin `grant` or the declared
/// permission admits.
pub fn decide(
    owner: &RegisteredOwner,
    user: &UserContext,
    answer: Option<AccessAnswer>,
    owner_key: &str,
) -> OwnerAccess {
    if !user.authenticated {
        return OwnerAccess::Denied;
    }
    // A plugin that does not answer the tap treats the key as the id.
    let answer = answer.unwrap_or_else(|| AccessAnswer {
        owner_id: Some(owner_key.to_string()),
        access: String::new(),
    });
    let Some(owner_id) = answer.owner_id.filter(|id| !id.is_empty()) else {
        return OwnerAccess::NotFound;
    };
    let granted = match answer.access.as_str() {
        "deny" => user.is_admin(),
        "grant" => true,
        _ => {
            user.is_admin()
                || owner
                    .decl
                    .permission
                    .as_deref()
                    .is_some_and(|p| user.has_permission(p))
        }
    };
    if granted {
        OwnerAccess::Granted { owner_id }
    } else {
        OwnerAccess::Denied
    }
}

/// Ask the declaring plugin about `owner_key` for `user`, and decide.
pub async fn check_access(
    state: &crate::state::AppState,
    owner: &RegisteredOwner,
    owner_key: &str,
    user: &UserContext,
) -> OwnerAccess {
    if !user.authenticated {
        return OwnerAccess::Denied;
    }
    let request = AccessRequest {
        owner_type: owner.decl.owner_type.clone(),
        owner_key: owner_key.to_string(),
        operation: ADMINISTER_TERMS.to_string(),
        user_id: user.id,
        authenticated: user.authenticated,
    };
    let Ok(payload) = serde_json::to_string(&request) else {
        return OwnerAccess::Denied;
    };
    let tap_state = crate::tap::RequestState::new(user.clone(), state.tap_services().clone());
    let answer = match state
        .tap_dispatcher()
        .dispatch_to_plugin_outcome(ACCESS_TAP, &payload, &owner.plugin, tap_state)
        .await
    {
        crate::tap::DispatchOutcome::Ok(result) => {
            match serde_json::from_str::<AccessAnswer>(&result.output) {
                Ok(answer) => Some(answer),
                Err(e) => {
                    tracing::warn!(
                        plugin = %owner.plugin,
                        error = %e,
                        "tap_vocabulary_owner_access returned an unparseable answer"
                    );
                    return OwnerAccess::Denied;
                }
            }
        }
        crate::tap::DispatchOutcome::NoHandler => None,
        _ => {
            tracing::warn!(plugin = %owner.plugin, "tap_vocabulary_owner_access failed");
            return OwnerAccess::Denied;
        }
    };
    decide(owner, user, answer, owner_key)
}

/// An item reference field limited to vocabularies its item's owner owns,
/// from the field's `settings.vocabulary_owner`:
///
/// ```json
/// {"vocabulary_owner": {"owner_type": "group", "owner_field": "field_group"}}
/// ```
///
/// The item's owner is the value of `owner_field` on the same item (an id, or
/// a reference `{"target_id": …}`). A term the field holds must belong to a
/// vocabulary with that owner type and owner id; an item with no owner may
/// hold no term in the field.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct OwnerLimit {
    pub owner_type: String,
    pub owner_field: String,
}

/// The limit on `field`, if it has one.
pub fn limit_of(settings: &serde_json::Value) -> Option<OwnerLimit> {
    serde_json::from_value(settings.get("vocabulary_owner")?.clone()).ok()
}

/// The single id a field value holds: a string, or an object's `target_id`
/// or `id`.
fn id_of(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        serde_json::Value::Object(map) => map
            .get("target_id")
            .or_else(|| map.get("id"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.trim().to_string()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Every id a field value holds, single or multiple.
fn ids_of(value: &serde_json::Value) -> Vec<String> {
    match value {
        serde_json::Value::Array(values) => values.iter().filter_map(id_of).collect(),
        other => id_of(other).into_iter().collect(),
    }
}

/// A write refused because a limited reference field holds a term from a
/// vocabulary the item's owner does not own.
///
/// Returned inside `anyhow::Error` and found with `downcast_ref`, like
/// `PageBuilderInvalid`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TermsNotOwned(
    /// `(item field name, message)` pairs.
    pub Vec<(String, String)>,
);

impl std::fmt::Display for TermsNotOwned {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let parts: Vec<String> = self
            .0
            .iter()
            .map(|(field, message)| format!("{field}: {message}"))
            .collect();
        write!(f, "{}", parts.join("; "))
    }
}

impl std::error::Error for TermsNotOwned {}

/// Check every limited reference field the write changes.
///
/// `fields` is what the write stores; `existing` is the stored item, if any.
/// A field whose value and owner did not change is not checked, so a term
/// moved out of an owner's vocabulary does not make the item unsaveable.
pub async fn check_item_terms(
    pool: &PgPool,
    definitions: &[trovato_sdk::types::FieldDefinition],
    fields: Option<&serde_json::Value>,
    existing: Option<&serde_json::Value>,
) -> Result<()> {
    let Some(fields) = fields.and_then(|f| f.as_object()) else {
        return Ok(());
    };
    let limited: Vec<(&trovato_sdk::types::FieldDefinition, OwnerLimit)> = definitions
        .iter()
        .filter_map(|d| limit_of(&d.settings).map(|l| (d, l)))
        .collect();
    if limited.is_empty() {
        return Ok(());
    }

    let mut wanted: Vec<(String, OwnerLimit, Option<String>, Vec<String>)> = Vec::new();
    for (def, limit) in limited {
        let value = fields.get(&def.field_name);
        let owner_value = fields
            .get(&limit.owner_field)
            .or_else(|| existing.and_then(|e| e.get(&limit.owner_field)));
        let unchanged = existing.is_some_and(|e| {
            e.get(&def.field_name) == value && e.get(&limit.owner_field) == owner_value
        });
        if unchanged {
            continue;
        }
        let terms = value.map(ids_of).unwrap_or_default();
        if terms.is_empty() {
            continue;
        }
        let owner = owner_value.and_then(id_of);
        wanted.push((def.field_name.clone(), limit, owner, terms));
    }
    if wanted.is_empty() {
        return Ok(());
    }

    let ids: Vec<Uuid> = wanted
        .iter()
        .flat_map(|(_, _, _, terms)| terms.iter().filter_map(|t| Uuid::parse_str(t).ok()))
        .collect();
    let rows: Vec<(Uuid, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT t.id, c.owner_type, c.owner_id FROM category_tag t \
         JOIN category c ON c.id = t.category_id WHERE t.id = ANY($1)",
    )
    .bind(&ids)
    .fetch_all(pool)
    .await
    .context("failed to load term owners")?;
    let owners: HashMap<Uuid, (Option<String>, Option<String>)> = rows
        .into_iter()
        .map(|(id, owner_type, owner_id)| (id, (owner_type, owner_id)))
        .collect();

    let mut refused = Vec::new();
    for (field, limit, owner, terms) in wanted {
        let Some(owner) = owner else {
            refused.push((
                field,
                format!(
                    "this item has no {} yet, so it cannot use that {}'s categories",
                    limit.owner_type, limit.owner_type
                ),
            ));
            continue;
        };
        let foreign = terms.iter().any(|term| {
            let Ok(id) = Uuid::parse_str(term) else {
                return true;
            };
            match owners.get(&id) {
                Some((Some(t), Some(o))) => *t != limit.owner_type || *o != owner,
                _ => true,
            }
        });
        if foreign {
            refused.push((
                field,
                format!(
                    "only categories that belong to this item's {} may be used here",
                    limit.owner_type
                ),
            ));
        }
    }
    if refused.is_empty() {
        Ok(())
    } else {
        Err(TermsNotOwned(refused).into())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn decl(owner_type: &str, path: &str, permission: Option<&str>) -> VocabularyOwnerDecl {
        VocabularyOwnerDecl {
            owner_type: owner_type.to_string(),
            label: String::new(),
            path: path.to_string(),
            permission: permission.map(str::to_string),
        }
    }

    #[test]
    fn declarations_are_checked() {
        assert!(check_declaration(&decl("group", "/group/{owner}/categories", None)).is_ok());
        assert!(check_declaration(&decl("Group", "/group/{owner}/categories", None)).is_err());
        assert!(check_declaration(&decl("group", "/admin/{owner}/categories", None)).is_err());
        assert!(check_declaration(&decl("group", "/group/{owner}", None)).is_err());
        assert!(check_declaration(&decl("group", "/group/x/categories", None)).is_err());
        assert!(check_declaration(&decl("group", "/group/{owner}/categories", Some(" "))).is_err());
    }

    #[test]
    fn owners_are_checked_together() {
        assert!(check_owner(None, None).is_ok());
        assert!(check_owner(Some("group"), Some("42")).is_ok());
        assert!(check_owner(Some("group"), None).is_err());
        assert!(check_owner(None, Some("42")).is_err());
        assert!(check_owner(Some("group"), Some(" ")).is_err());
        assert!(check_owner(Some("9group"), Some("42")).is_err());
    }

    #[test]
    fn the_first_plugin_by_name_keeps_a_type_or_path() {
        let a = [decl("group", "/group/{owner}/categories", None)];
        let b = [
            decl("group", "/team/{owner}/categories", None),
            decl("team", "/group/{owner}/categories", None),
            decl("club", "/club/{owner}/categories", None),
        ];
        let owners = registered_owners([("b_plugin", &b[..]), ("a_plugin", &a[..])]);
        let got: Vec<(&str, &str)> = owners
            .iter()
            .map(|o| (o.plugin.as_str(), o.decl.owner_type.as_str()))
            .collect();
        assert_eq!(got, vec![("a_plugin", "group"), ("b_plugin", "club")]);
        assert_eq!(owners[1].base_url("rowers"), "/club/rowers/categories");
    }

    #[test]
    fn access_combines_the_plugin_answer_and_the_user() {
        let owner = RegisteredOwner {
            plugin: "p".into(),
            decl: decl(
                "group",
                "/group/{owner}/categories",
                Some("administer group terms"),
            ),
        };
        let anon = UserContext::anonymous();
        let plain = UserContext::authenticated(Uuid::now_v7(), vec![]);
        let holder =
            UserContext::authenticated(Uuid::now_v7(), vec!["administer group terms".into()]);
        let answer = |id: Option<&str>, access: &str| {
            Some(AccessAnswer {
                owner_id: id.map(str::to_string),
                access: access.to_string(),
            })
        };
        let granted = OwnerAccess::Granted {
            owner_id: "g1".into(),
        };
        assert_eq!(
            decide(&owner, &anon, answer(Some("g1"), "grant"), "a"),
            OwnerAccess::Denied
        );
        assert_eq!(
            decide(&owner, &plain, answer(Some("g1"), "neutral"), "a"),
            OwnerAccess::Denied
        );
        assert_eq!(
            decide(&owner, &plain, answer(Some("g1"), "grant"), "a"),
            granted
        );
        assert_eq!(
            decide(&owner, &holder, answer(Some("g1"), ""), "a"),
            granted
        );
        assert_eq!(
            decide(&owner, &holder, answer(Some("g1"), "deny"), "a"),
            OwnerAccess::Denied
        );
        assert_eq!(
            decide(&owner, &plain, answer(None, "grant"), "a"),
            OwnerAccess::NotFound
        );
        assert_eq!(
            decide(&owner, &holder, None, "g1"),
            granted,
            "no tap: the key is the id"
        );
    }

    #[test]
    fn a_limit_is_read_from_settings_and_ids_from_values() {
        let settings = serde_json::json!({
            "vocabulary_owner": {"owner_type": "group", "owner_field": "field_group"}
        });
        assert_eq!(
            limit_of(&settings),
            Some(OwnerLimit {
                owner_type: "group".into(),
                owner_field: "field_group".into()
            })
        );
        assert_eq!(limit_of(&serde_json::json!({})), None);
        assert_eq!(
            ids_of(&serde_json::json!(["a", {"target_id": "b"}, "", null])),
            vec!["a".to_string(), "b".to_string()]
        );
        assert_eq!(id_of(&serde_json::json!(7)), Some("7".to_string()));
    }
}
