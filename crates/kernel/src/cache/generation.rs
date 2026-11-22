//! Cache generations: how one server tells the others to drop a cache.
//!
//! Each in-process cache that holds data from Postgres is a [`CacheKind`]. A
//! write clears its own server's cache at once and then calls [`bump`], which
//! adds one to that kind's counter in the Redis hash `cache:gen`. Every server
//! (one or many) polls the hash every 250 ms and clears a kind whose counter
//! differs from the value it last saw. A difference, not only an increase: a
//! restarted Redis starts the hash again from nothing.
//!
//! No messages, no payload, no origin: a bump clears the whole kind, including
//! on the server that wrote. When Redis cannot be reached the poller logs once
//! and keeps trying, and the caches' own TTLs bound how stale they get.

use std::collections::HashMap;
use std::future::Future;
use std::sync::OnceLock;
use std::time::Duration;

use redis::AsyncCommands;
use tracing::{info, warn};

/// The Redis hash holding one counter per kind.
const GENERATION_KEY: &str = "cache:gen";

/// How often each server reads the counters.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// The Redis this process bumps, set once when the server's state is built.
static REDIS: OnceLock<redis::Client> = OnceLock::new();

/// One in-process cache another server's write can make stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheKind {
    /// Each user's permissions and role names (`PermissionService`).
    Permissions,
    /// Loaded user records (`UserService`).
    Users,
    /// Loaded items (`ItemService`).
    Items,
    /// `tap_field_access` answers (`ItemService`).
    FieldAccess,
    /// Content type definitions (`ContentTypeRegistry`).
    ContentTypes,
    /// Categories and their tag lists (`CategoryService`).
    Categories,
    /// Gather query definitions and distinct values (`GatherService`).
    Gather,
    /// Redirect lookups (`RedirectCache`).
    Redirects,
    /// Interface string translations (`LocaleService`).
    Locale,
    /// Config menus by stage and language (`menu::navigation`).
    Menus,
    /// Stored text formats (`content::text_format`).
    TextFormats,
    /// The default and admin themes (`theme::registry`).
    Themes,
}

impl CacheKind {
    /// Every kind, for a writer that may have touched any of them.
    pub const ALL: [CacheKind; 12] = [
        CacheKind::Permissions,
        CacheKind::Users,
        CacheKind::Items,
        CacheKind::FieldAccess,
        CacheKind::ContentTypes,
        CacheKind::Categories,
        CacheKind::Gather,
        CacheKind::Redirects,
        CacheKind::Locale,
        CacheKind::Menus,
        CacheKind::TextFormats,
        CacheKind::Themes,
    ];

    /// The field name in the Redis hash.
    pub fn as_str(self) -> &'static str {
        match self {
            CacheKind::Permissions => "permissions",
            CacheKind::Users => "users",
            CacheKind::Items => "items",
            CacheKind::FieldAccess => "field_access",
            CacheKind::ContentTypes => "content_types",
            CacheKind::Categories => "categories",
            CacheKind::Gather => "gather",
            CacheKind::Redirects => "redirects",
            CacheKind::Locale => "locale",
            CacheKind::Menus => "menus",
            CacheKind::TextFormats => "text_formats",
            CacheKind::Themes => "themes",
        }
    }

    /// The kind whose cache holds rows of `table`, if any.
    pub fn for_table(table: &str) -> Option<CacheKind> {
        let table = table.trim_matches('"').rsplit('.').next().unwrap_or(table);
        Some(match table {
            "users" => CacheKind::Users,
            "roles" | "role_permissions" | "user_roles" => CacheKind::Permissions,
            "item" | "item_revision" | "item_translation" => CacheKind::Items,
            "item_type" => CacheKind::ContentTypes,
            "category" | "category_tag" | "category_tag_hierarchy" => CacheKind::Categories,
            "gather_query" => CacheKind::Gather,
            "redirect" => CacheKind::Redirects,
            "locale_string" => CacheKind::Locale,
            "menu_link" | "url_alias" => CacheKind::Menus,
            "text_format" => CacheKind::TextFormats,
            _ => return None,
        })
    }
}

/// Use `redis` for bumps from this process. The first call wins; a test
/// process that builds several states keeps the first one's Redis.
pub fn init(redis: &redis::Client) {
    let _ = REDIS.set(redis.clone());
}

/// Tell every server, this one included, to clear `kind`.
///
/// Call it after the write has committed. It does not wait: the increment is
/// sent from a background task, so it may be called from synchronous code. A
/// process that never called [`init`] (a unit test) bumps nothing.
pub fn bump(kind: CacheKind) {
    bump_all(&[kind]);
}

/// [`bump`] for several kinds at once.
pub fn bump_all(kinds: &[CacheKind]) {
    let Some(redis) = REDIS.get().cloned() else {
        return;
    };
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let kinds = kinds.to_vec();
    handle.spawn(async move {
        if let Err(e) = increment(&redis, &kinds).await {
            warn!(error = %e, "failed to bump cache generations; other servers rely on TTLs");
        }
    });
}

/// Bump `kinds` and wait for Redis to answer: for a command line process,
/// which exits as soon as it is done.
pub async fn bump_now(redis: &redis::Client, kinds: &[CacheKind]) -> anyhow::Result<()> {
    increment(redis, kinds).await
}

async fn increment(redis: &redis::Client, kinds: &[CacheKind]) -> anyhow::Result<()> {
    let mut conn = redis.get_multiplexed_async_connection().await?;
    for kind in kinds {
        conn.hincr::<_, _, _, i64>(GENERATION_KEY, kind.as_str(), 1)
            .await?;
    }
    Ok(())
}

/// Poll the counters forever, calling `clear` for each kind that changed.
///
/// The first successful read only records what it saw.
pub fn spawn_watcher<F, Fut>(redis: redis::Client, clear: F)
where
    F: Fn(CacheKind) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send,
{
    tokio::spawn(async move {
        let mut seen: Option<HashMap<String, i64>> = None;
        let mut conn = None;
        let mut failing = false;
        let mut tick = tokio::time::interval(POLL_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let read = match conn.as_mut() {
                Some(c) => read(c).await,
                None => match redis.get_multiplexed_async_connection().await {
                    Ok(mut c) => {
                        let r = read(&mut c).await;
                        conn = Some(c);
                        r
                    }
                    Err(e) => Err(e),
                },
            };
            let current = match read {
                Ok(current) => current,
                Err(e) => {
                    conn = None;
                    if !failing {
                        warn!(error = %e, "cannot read cache generations; caches fall back to their TTLs");
                        failing = true;
                    }
                    continue;
                }
            };
            if failing {
                info!("cache generations readable again");
                failing = false;
            }
            if let Some(previous) = &seen {
                for kind in CacheKind::ALL {
                    if current.get(kind.as_str()) != previous.get(kind.as_str()) {
                        clear(kind).await;
                    }
                }
            }
            seen = Some(current);
        }
    });
}

async fn read(
    conn: &mut redis::aio::MultiplexedConnection,
) -> redis::RedisResult<HashMap<String, i64>> {
    conn.hgetall(GENERATION_KEY).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_has_its_own_field() {
        let names: std::collections::HashSet<_> =
            CacheKind::ALL.iter().map(|k| k.as_str()).collect();
        assert_eq!(names.len(), CacheKind::ALL.len());
    }

    #[test]
    fn tables_map_to_their_cache() {
        assert_eq!(CacheKind::for_table("item"), Some(CacheKind::Items));
        assert_eq!(
            CacheKind::for_table("public.redirect"),
            Some(CacheKind::Redirects)
        );
        assert_eq!(
            CacheKind::for_table("\"locale_string\""),
            Some(CacheKind::Locale)
        );
        assert_eq!(CacheKind::for_table("comment"), None);
    }
}
