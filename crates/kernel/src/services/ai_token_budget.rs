//! Token budget tracking and enforcement for AI operations.
//!
//! Tracks per-request token usage in the `ai_usage_log` table and enforces
//! configurable budgets per-provider, per-role, and per-user. Budget
//! configuration is stored in the `site_config` table; per-user overrides
//! live in the user's `data` JSONB column.
//!
//! ## Budget Resolution Order
//!
//! 1. Per-user override (`users.data["ai_budget_overrides"][provider_id]`)
//! 2. Per-role default — highest limit among user's roles wins (most permissive)
//! 3. No config → unlimited

use std::collections::HashMap;

use anyhow::{Context, Result};
use chrono::Datelike;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::SiteConfig;

/// Site config key for budget configuration.
const CONFIG_KEY: &str = "ai_token_budgets";
/// Site config key for the model→price table (P11c / D-44).
const CONFIG_KEY_PRICING: &str = "ai_pricing";

/// Price of a single model, per 1,000 tokens (P11c / D-44).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPrice {
    /// Price per 1,000 prompt (input) tokens, in [`Self::currency`].
    pub input_per_1k: f64,
    /// Price per 1,000 completion (output) tokens, in [`Self::currency`].
    pub output_per_1k: f64,
    /// Currency code (e.g. `"USD"`). Informational — the kernel does not convert
    /// between currencies; a currency budget must be denominated in the same
    /// currency as the models it meters.
    #[serde(default = "default_currency")]
    pub currency: String,
}

fn default_currency() -> String {
    "USD".to_string()
}

/// Config-owned model→price table (P11c / D-44), stored in `site_config` under
/// `ai_pricing`. No prices are hard-coded; an unpriced model degrades gracefully
/// to tokens-only accounting (no invented cost).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AiPricingConfig {
    /// Prices keyed by model identifier (e.g. `"gpt-4o"`).
    #[serde(default)]
    pub models: HashMap<String, ModelPrice>,
}

impl AiPricingConfig {
    /// Estimate the cost of a call from its model and token counts, or `None`
    /// when the model is not priced (tokens-only, never an invented cost).
    pub fn cost_for(&self, model: &str, prompt_tokens: i64, completion_tokens: i64) -> Option<f64> {
        let price = self.models.get(model)?;
        let input = (prompt_tokens.max(0) as f64) / 1000.0 * price.input_per_1k;
        let output = (completion_tokens.max(0) as f64) / 1000.0 * price.output_per_1k;
        Some(input + output)
    }
}

/// Result of a per-plugin currency (cost) budget check (P11c / D-44).
#[derive(Debug, Clone)]
pub struct CostBudgetCheckResult {
    /// Whether the request is allowed.
    pub allowed: bool,
    /// The resolved currency limit (`0.0` = unlimited / unconfigured).
    pub limit: f64,
    /// Cost already spent in the current period.
    pub used: f64,
    /// The action to take if the budget is exceeded.
    pub action: BudgetAction,
}

// =============================================================================
// Data types
// =============================================================================

/// Budget period — how often usage resets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetPeriod {
    /// Resets every day at 00:00 UTC.
    Daily,
    /// Resets every Monday at 00:00 UTC.
    Weekly,
    /// Resets on the 1st of each month at 00:00 UTC.
    Monthly,
}

impl BudgetPeriod {
    /// Unix timestamp (seconds) of the start of the current period.
    ///
    /// # Panics
    ///
    /// Panics if chrono cannot construct midnight (00:00:00) or day-1 of the
    /// current month — these are infallible for valid UTC dates.
    #[allow(clippy::expect_used)]
    pub fn period_start(self) -> i64 {
        let now = chrono::Utc::now();
        match self {
            Self::Daily => now
                .date_naive()
                .and_hms_opt(0, 0, 0)
                .expect("midnight is always valid") // Infallible: 0,0,0
                .and_utc()
                .timestamp(),
            Self::Weekly => {
                let days_since_monday = now.weekday().num_days_from_monday();
                (now - chrono::Duration::days(i64::from(days_since_monday)))
                    .date_naive()
                    .and_hms_opt(0, 0, 0)
                    .expect("midnight is always valid") // Infallible: 0,0,0
                    .and_utc()
                    .timestamp()
            }
            Self::Monthly => now
                .date_naive()
                .with_day(1)
                .expect("day 1 is always valid") // Infallible: day 1 exists in every month
                .and_hms_opt(0, 0, 0)
                .expect("midnight is always valid") // Infallible: 0,0,0
                .and_utc()
                .timestamp(),
        }
    }

    /// Human-readable label for the current period.
    pub fn label(self) -> &'static str {
        match self {
            Self::Daily => "Today",
            Self::Weekly => "This week",
            Self::Monthly => "This month",
        }
    }
}

impl std::fmt::Display for BudgetPeriod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Daily => write!(f, "daily"),
            Self::Weekly => write!(f, "weekly"),
            Self::Monthly => write!(f, "monthly"),
        }
    }
}

/// Action to take when a budget limit is reached.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetAction {
    /// Reject the request with `ERR_AI_BUDGET_EXCEEDED`.
    #[default]
    Deny,
    /// Allow but log a warning.
    Warn,
    /// Queue for later execution (not implemented — treated as Deny).
    Queue,
}

/// Site-wide budget configuration stored in `site_config`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetConfig {
    /// How often usage resets.
    pub period: BudgetPeriod,
    /// What happens when the limit is exceeded.
    pub action_on_limit: BudgetAction,
    /// Per-provider, per-role token limits.
    /// Structure: `provider_id → role_name → token_limit` (0 = unlimited).
    pub defaults: HashMap<String, HashMap<String, u64>>,
    /// Per-provider, per-plugin token limits for **background** AI calls
    /// (P11c / D-42). Structure: `provider_id → plugin_name → token_limit`
    /// (0 = unlimited). A background call has no human role, so a per-plugin cap
    /// is the honest unit; it is metered against the plugin's background usage
    /// (`user_id IS NULL` rows attributed by `plugin_name`), kept separate from
    /// the per-user/role [`Self::defaults`] above. Absent entry ⇒ unlimited.
    #[serde(default)]
    pub plugin_defaults: HashMap<String, HashMap<String, u64>>,
    /// Optional per-provider, per-plugin **currency** budgets for background AI
    /// calls (P11c / D-44). Structure: `provider_id → plugin_name → cost_limit`
    /// (`0.0` = unlimited). Metered against the summed `cost_estimate` of the
    /// plugin's background usage; enforced alongside the token cap in
    /// [`Self::plugin_defaults`]. Absent entry ⇒ no currency cap.
    #[serde(default)]
    pub plugin_cost_defaults: HashMap<String, HashMap<String, f64>>,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            period: BudgetPeriod::Monthly,
            action_on_limit: BudgetAction::Deny,
            defaults: HashMap::new(),
            plugin_defaults: HashMap::new(),
            plugin_cost_defaults: HashMap::new(),
        }
    }
}

/// Result of a budget check.
#[derive(Debug, Clone)]
pub struct BudgetCheckResult {
    /// Whether the request is allowed.
    pub allowed: bool,
    /// Tokens remaining in the budget (`None` if unlimited).
    pub remaining: Option<u64>,
    /// The resolved limit (0 = unlimited).
    pub limit: u64,
    /// Tokens already used in the current period.
    pub used: u64,
    /// The action to take if the budget is exceeded.
    pub action: BudgetAction,
}

/// A single usage log entry to persist.
pub struct UsageLogEntry {
    /// User who triggered the request (None for anonymous).
    pub user_id: Option<Uuid>,
    /// Plugin that made the `ai_request()` call.
    pub plugin_name: String,
    /// Provider configuration ID.
    pub provider_id: String,
    /// Operation type (e.g. "Chat", "Embedding").
    pub operation: String,
    /// Model that was used.
    pub model: String,
    /// Prompt tokens from provider response.
    pub prompt_tokens: i32,
    /// Completion tokens from provider response.
    pub completion_tokens: i32,
    /// Total tokens from provider response.
    pub total_tokens: i32,
    /// Round-trip latency in milliseconds.
    pub latency_ms: i64,
    /// Estimated cost of the call (P11c / D-44), derived from the model and token
    /// counts via the `ai_pricing` table. `None` for an unpriced model —
    /// tokens-only, never an invented cost.
    pub cost_estimate: Option<f64>,
}

/// Summary of token usage by provider.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderUsageSummary {
    /// Provider configuration ID.
    pub provider_id: String,
    /// Total tokens used in the period.
    pub total_tokens: i64,
    /// Number of requests in the period.
    pub request_count: i64,
}

/// Summary of token usage by user.
#[derive(Debug, Clone, Serialize)]
pub struct UserUsageSummary {
    /// User ID.
    pub user_id: Uuid,
    /// Username.
    pub user_name: String,
    /// Provider configuration ID.
    pub provider_id: String,
    /// Total tokens used in the period.
    pub total_tokens: i64,
    /// Number of requests in the period.
    pub request_count: i64,
}

// =============================================================================
// Service
// =============================================================================

/// Token budget tracking and enforcement service.
pub struct AiTokenBudgetService {
    db: PgPool,
}

impl AiTokenBudgetService {
    /// Create a new budget service.
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }

    // -------------------------------------------------------------------------
    // Config CRUD
    // -------------------------------------------------------------------------

    /// Load budget configuration from site config.
    ///
    /// Returns `None` if no budget configuration has been saved.
    pub async fn get_config(&self) -> Result<Option<BudgetConfig>> {
        let value = SiteConfig::get(&self.db, CONFIG_KEY).await?;
        match value {
            Some(v) => {
                let config: BudgetConfig =
                    serde_json::from_value(v).context("failed to deserialize budget config")?;
                Ok(Some(config))
            }
            None => Ok(None),
        }
    }

    /// Save budget configuration to site config.
    pub async fn save_config(&self, config: &BudgetConfig) -> Result<()> {
        let value = serde_json::to_value(config).context("failed to serialize budget config")?;
        SiteConfig::set(&self.db, CONFIG_KEY, value).await
    }

    /// Load the model→price table (P11c / D-44) from site config, or an empty
    /// table when unset (every model unpriced ⇒ tokens-only).
    pub async fn get_pricing_config(&self) -> Result<AiPricingConfig> {
        let value = SiteConfig::get(&self.db, CONFIG_KEY_PRICING)
            .await
            .context("failed to read ai_pricing config")?;
        match value {
            Some(v) => serde_json::from_value(v).context("failed to parse ai_pricing config"),
            None => Ok(AiPricingConfig::default()),
        }
    }

    /// Persist the model→price table (P11c / D-44) to site config.
    pub async fn save_pricing_config(&self, config: &AiPricingConfig) -> Result<()> {
        let value = serde_json::to_value(config).context("failed to serialize ai_pricing")?;
        SiteConfig::set(&self.db, CONFIG_KEY_PRICING, value).await
    }

    /// Estimate the cost of a call (P11c / D-44) from its model and token counts,
    /// using the current pricing table. `None` when the model is unpriced or the
    /// pricing table cannot be read — tokens-only, never an invented cost.
    pub async fn estimate_cost(
        &self,
        model: &str,
        prompt_tokens: i64,
        completion_tokens: i64,
    ) -> Option<f64> {
        let pricing = self.get_pricing_config().await.ok()?;
        pricing.cost_for(model, prompt_tokens, completion_tokens)
    }

    // -------------------------------------------------------------------------
    // Per-user overrides (stored in users.data JSONB)
    // -------------------------------------------------------------------------

    /// Get a per-user budget override for a specific provider.
    ///
    /// Returns `None` if no override is set.
    pub async fn get_user_override(
        &self,
        pool: &PgPool,
        user_id: Uuid,
        provider_id: &str,
    ) -> Result<Option<u64>> {
        let data: Option<serde_json::Value> =
            sqlx::query_scalar("SELECT data FROM users WHERE id = $1")
                .bind(user_id)
                .fetch_optional(pool)
                .await
                .context("failed to read user data")?;

        Ok(data.and_then(|d| d.get("ai_budget_overrides")?.get(provider_id)?.as_u64()))
    }

    /// Get all per-user budget overrides.
    pub async fn get_all_user_overrides(
        &self,
        pool: &PgPool,
        user_id: Uuid,
    ) -> Result<HashMap<String, u64>> {
        let data: Option<serde_json::Value> =
            sqlx::query_scalar("SELECT data FROM users WHERE id = $1")
                .bind(user_id)
                .fetch_optional(pool)
                .await
                .context("failed to read user data")?;

        let overrides = data
            .and_then(|d| d.get("ai_budget_overrides").cloned())
            .and_then(|v| serde_json::from_value::<HashMap<String, u64>>(v).ok())
            .unwrap_or_default();

        Ok(overrides)
    }

    /// Set a per-user budget override for a specific provider.
    ///
    /// Pass `limit = 0` for unlimited. Uses atomic JSONB operations to avoid
    /// read-modify-write races.
    pub async fn set_user_override(
        &self,
        pool: &PgPool,
        user_id: Uuid,
        provider_id: &str,
        limit: u64,
    ) -> Result<()> {
        // Atomic: ensure ai_budget_overrides key exists, then set the provider limit.
        sqlx::query(
            r#"
            UPDATE users
            SET data = jsonb_set(
                jsonb_set(
                    COALESCE(data, '{}'::jsonb),
                    '{ai_budget_overrides}',
                    COALESCE(data->'ai_budget_overrides', '{}'::jsonb)
                ),
                ARRAY['ai_budget_overrides', $2],
                to_jsonb($3::bigint)
            )
            WHERE id = $1
            "#,
        )
        .bind(user_id)
        .bind(provider_id)
        .bind(limit as i64)
        .execute(pool)
        .await
        .context("failed to save user budget override")?;

        Ok(())
    }

    /// Remove a per-user budget override for a specific provider.
    ///
    /// Uses atomic JSONB operation to avoid read-modify-write races.
    pub async fn remove_user_override(
        &self,
        pool: &PgPool,
        user_id: Uuid,
        provider_id: &str,
    ) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE users
            SET data = data #- ARRAY['ai_budget_overrides', $2]
            WHERE id = $1 AND data->'ai_budget_overrides' ? $2
            "#,
        )
        .bind(user_id)
        .bind(provider_id)
        .execute(pool)
        .await
        .context("failed to remove user budget override")?;

        Ok(())
    }

    // -------------------------------------------------------------------------
    // Usage logging
    // -------------------------------------------------------------------------

    /// Record a usage entry in `ai_usage_log`.
    pub async fn record_usage(&self, pool: &PgPool, entry: UsageLogEntry) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO ai_usage_log
                (user_id, plugin_name, provider_id, operation, model,
                 prompt_tokens, completion_tokens, total_tokens, latency_ms,
                 cost_estimate)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
            "#,
        )
        .bind(entry.user_id)
        .bind(&entry.plugin_name)
        .bind(&entry.provider_id)
        .bind(&entry.operation)
        .bind(&entry.model)
        .bind(entry.prompt_tokens)
        .bind(entry.completion_tokens)
        .bind(entry.total_tokens)
        .bind(entry.latency_ms)
        .bind(entry.cost_estimate)
        .execute(pool)
        .await
        .context("failed to insert ai_usage_log entry")?;

        Ok(())
    }

    /// Get total tokens used by a user for a provider since a given timestamp.
    pub async fn get_usage_for_period(
        &self,
        pool: &PgPool,
        user_id: Uuid,
        provider_id: &str,
        since: i64,
    ) -> Result<u64> {
        let total: Option<i64> = sqlx::query_scalar(
            r#"
            SELECT COALESCE(SUM(total_tokens), 0)
            FROM ai_usage_log
            WHERE user_id = $1 AND provider_id = $2 AND created >= $3
            "#,
        )
        .bind(user_id)
        .bind(provider_id)
        .bind(since)
        .fetch_one(pool)
        .await
        .context("failed to query usage for period")?;

        Ok(total.unwrap_or(0).max(0) as u64)
    }

    /// Total tokens used by a plugin's **background** calls for a provider since a
    /// given timestamp (P11c / D-42).
    ///
    /// Meters only `user_id IS NULL` rows — the background-principal attribution
    /// (`plugin_name` set, no human user) — so a per-plugin background budget
    /// never conflates with the per-user budgets metered by
    /// [`Self::get_usage_for_period`].
    pub async fn get_plugin_usage_for_period(
        &self,
        pool: &PgPool,
        plugin_name: &str,
        provider_id: &str,
        since: i64,
    ) -> Result<u64> {
        let total: Option<i64> = sqlx::query_scalar(
            r#"
            SELECT COALESCE(SUM(total_tokens), 0)
            FROM ai_usage_log
            WHERE plugin_name = $1 AND provider_id = $2 AND created >= $3
              AND user_id IS NULL
            "#,
        )
        .bind(plugin_name)
        .bind(provider_id)
        .bind(since)
        .fetch_one(pool)
        .await
        .context("failed to query plugin usage for period")?;

        Ok(total.unwrap_or(0).max(0) as u64)
    }

    /// Total estimated cost of a plugin's **background** calls for a provider
    /// since a given timestamp (P11c / D-44). Sums `cost_estimate` over the
    /// plugin's `user_id IS NULL` rows; unpriced rows (NULL cost) contribute 0.
    pub async fn get_plugin_cost_for_period(
        &self,
        pool: &PgPool,
        plugin_name: &str,
        provider_id: &str,
        since: i64,
    ) -> Result<f64> {
        let total: Option<f64> = sqlx::query_scalar(
            r#"
            SELECT COALESCE(SUM(cost_estimate), 0)
            FROM ai_usage_log
            WHERE plugin_name = $1 AND provider_id = $2 AND created >= $3
              AND user_id IS NULL
            "#,
        )
        .bind(plugin_name)
        .bind(provider_id)
        .bind(since)
        .fetch_one(pool)
        .await
        .context("failed to query plugin cost for period")?;

        Ok(total.unwrap_or(0.0).max(0.0))
    }

    // -------------------------------------------------------------------------
    // Budget enforcement
    // -------------------------------------------------------------------------

    /// Check whether a user's token budget allows a request.
    ///
    /// Resolves the effective limit using the budget resolution order:
    /// 1. Per-user override
    /// 2. Per-role default (highest among user's roles)
    /// 3. No config → unlimited
    pub async fn check_budget(
        &self,
        pool: &PgPool,
        user_id: Uuid,
        provider_id: &str,
    ) -> Result<BudgetCheckResult> {
        let Some(config) = self.get_config().await? else {
            return Ok(BudgetCheckResult {
                allowed: true,
                remaining: None,
                limit: 0,
                used: 0,
                action: BudgetAction::Deny,
            });
        };

        // Anonymous users get no budget (unlimited — budget applies to authenticated)
        if user_id.is_nil() {
            return Ok(BudgetCheckResult {
                allowed: true,
                remaining: None,
                limit: 0,
                used: 0,
                action: config.action_on_limit,
            });
        }

        // 1. Check per-user override
        let user_override = self.get_user_override(pool, user_id, provider_id).await?;

        let limit = if let Some(ovr) = user_override {
            ovr
        } else {
            // 2. Resolve per-role default — highest limit wins
            self.resolve_role_budget(pool, user_id, provider_id, &config)
                .await?
        };

        // 0 = unlimited
        if limit == 0 {
            return Ok(BudgetCheckResult {
                allowed: true,
                remaining: None,
                limit: 0,
                used: 0,
                action: config.action_on_limit,
            });
        }

        // 3. Query current usage
        let since = config.period.period_start();
        let used = self
            .get_usage_for_period(pool, user_id, provider_id, since)
            .await?;

        if used >= limit {
            Ok(BudgetCheckResult {
                allowed: false,
                remaining: Some(0),
                limit,
                used,
                action: config.action_on_limit,
            })
        } else {
            Ok(BudgetCheckResult {
                allowed: true,
                remaining: Some(limit - used),
                limit,
                used,
                action: config.action_on_limit,
            })
        }
    }

    /// Check whether a plugin's per-plugin **background** token budget allows a
    /// call (P11c / D-42).
    ///
    /// The background principal has no human role, so enforcement keys on
    /// `plugin_name` via [`BudgetConfig::plugin_defaults`] and meters the plugin's
    /// background usage (`user_id IS NULL`). Resolution:
    /// 1. No budget config, or no `plugin_defaults` entry for this
    ///    provider/plugin, or a configured limit of `0` → unlimited (allowed).
    /// 2. Otherwise allowed while background usage in the current period is below
    ///    the limit; on reaching it, the configured [`BudgetConfig::action_on_limit`]
    ///    applies (as for per-user budgets).
    pub async fn check_plugin_budget(
        &self,
        pool: &PgPool,
        plugin_name: &str,
        provider_id: &str,
    ) -> Result<BudgetCheckResult> {
        let Some(config) = self.get_config().await? else {
            return Ok(BudgetCheckResult {
                allowed: true,
                remaining: None,
                limit: 0,
                used: 0,
                action: BudgetAction::Deny,
            });
        };

        let limit = config
            .plugin_defaults
            .get(provider_id)
            .and_then(|per_plugin| per_plugin.get(plugin_name))
            .copied()
            .unwrap_or(0);

        // 0 = unlimited (no per-plugin cap configured for this provider/plugin).
        if limit == 0 {
            return Ok(BudgetCheckResult {
                allowed: true,
                remaining: None,
                limit: 0,
                used: 0,
                action: config.action_on_limit,
            });
        }

        let since = config.period.period_start();
        let used = self
            .get_plugin_usage_for_period(pool, plugin_name, provider_id, since)
            .await?;

        if used >= limit {
            Ok(BudgetCheckResult {
                allowed: false,
                remaining: Some(0),
                limit,
                used,
                action: config.action_on_limit,
            })
        } else {
            Ok(BudgetCheckResult {
                allowed: true,
                remaining: Some(limit - used),
                limit,
                used,
                action: config.action_on_limit,
            })
        }
    }

    /// Check whether a plugin's per-plugin **currency** budget allows a
    /// background call (P11c / D-44).
    ///
    /// Keys on `plugin_name` via [`BudgetConfig::plugin_cost_defaults`] and meters
    /// the plugin's background cost (`user_id IS NULL` rows). No config, no entry,
    /// or a `0.0` limit ⇒ unlimited (allowed). Enforced by the host alongside the
    /// token cap, so a plugin can be capped by tokens, by currency, or both.
    pub async fn check_plugin_cost_budget(
        &self,
        pool: &PgPool,
        plugin_name: &str,
        provider_id: &str,
    ) -> Result<CostBudgetCheckResult> {
        let Some(config) = self.get_config().await? else {
            return Ok(CostBudgetCheckResult {
                allowed: true,
                limit: 0.0,
                used: 0.0,
                action: BudgetAction::Deny,
            });
        };

        let limit = config
            .plugin_cost_defaults
            .get(provider_id)
            .and_then(|per_plugin| per_plugin.get(plugin_name))
            .copied()
            .unwrap_or(0.0);

        // 0.0 = no currency cap configured for this provider/plugin.
        if limit <= 0.0 {
            return Ok(CostBudgetCheckResult {
                allowed: true,
                limit: 0.0,
                used: 0.0,
                action: config.action_on_limit,
            });
        }

        let since = config.period.period_start();
        let used = self
            .get_plugin_cost_for_period(pool, plugin_name, provider_id, since)
            .await?;

        Ok(CostBudgetCheckResult {
            allowed: used < limit,
            limit,
            used,
            action: config.action_on_limit,
        })
    }

    /// Resolve the per-role budget for a user. Returns the highest limit
    /// among all the user's roles (most permissive wins). If any role has
    /// limit 0, returns 0 (unlimited).
    async fn resolve_role_budget(
        &self,
        pool: &PgPool,
        user_id: Uuid,
        provider_id: &str,
        config: &BudgetConfig,
    ) -> Result<u64> {
        let Some(provider_defaults) = config.defaults.get(provider_id) else {
            return Ok(0); // No defaults for this provider → unlimited
        };

        // Load user's role names.
        // Uses Role model directly: AiTokenBudgetService has no RoleService
        // reference, and this is a read-only lookup with no cache to invalidate.
        let roles = crate::models::Role::get_user_roles(pool, user_id).await?;

        let mut highest_limit: Option<u64> = None;
        for role in &roles {
            if let Some(&role_limit) = provider_defaults.get(&role.name) {
                if role_limit == 0 {
                    return Ok(0); // Any role with 0 → unlimited
                }
                highest_limit =
                    Some(highest_limit.map_or(role_limit, |current| current.max(role_limit)));
            }
        }

        // Also check "authenticated" pseudo-role for any logged-in user
        if let Some(&auth_limit) = provider_defaults.get("authenticated") {
            if auth_limit == 0 {
                return Ok(0);
            }
            highest_limit =
                Some(highest_limit.map_or(auth_limit, |current| current.max(auth_limit)));
        }

        // If no matching role found → no budget configured → unlimited
        Ok(highest_limit.unwrap_or(0))
    }

    // -------------------------------------------------------------------------
    // Dashboard queries
    // -------------------------------------------------------------------------

    /// Get usage summaries grouped by provider for the current period.
    pub async fn usage_by_provider(&self, since: i64) -> Result<Vec<ProviderUsageSummary>> {
        let rows = sqlx::query_as::<_, (String, i64, i64)>(
            r#"
            SELECT provider_id,
                   COALESCE(SUM(total_tokens), 0) AS total_tokens,
                   COUNT(*) AS request_count
            FROM ai_usage_log
            WHERE created >= $1
            GROUP BY provider_id
            ORDER BY total_tokens DESC
            "#,
        )
        .bind(since)
        .fetch_all(&self.db)
        .await
        .context("failed to query usage by provider")?;

        Ok(rows
            .into_iter()
            .map(
                |(provider_id, total_tokens, request_count)| ProviderUsageSummary {
                    provider_id,
                    total_tokens,
                    request_count,
                },
            )
            .collect())
    }

    /// Get top users by token usage for the current period.
    pub async fn usage_by_user(&self, since: i64, limit: i64) -> Result<Vec<UserUsageSummary>> {
        let rows = sqlx::query_as::<_, (Uuid, String, String, i64, i64)>(
            r#"
            SELECT l.user_id,
                   COALESCE(u.name, 'anonymous') AS user_name,
                   l.provider_id,
                   COALESCE(SUM(l.total_tokens), 0) AS total_tokens,
                   COUNT(*) AS request_count
            FROM ai_usage_log l
            LEFT JOIN users u ON l.user_id = u.id
            WHERE l.created >= $1 AND l.user_id IS NOT NULL
            GROUP BY l.user_id, u.name, l.provider_id
            ORDER BY total_tokens DESC
            LIMIT $2
            "#,
        )
        .bind(since)
        .bind(limit)
        .fetch_all(&self.db)
        .await
        .context("failed to query usage by user")?;

        Ok(rows
            .into_iter()
            .map(
                |(user_id, user_name, provider_id, total_tokens, request_count)| UserUsageSummary {
                    user_id,
                    user_name,
                    provider_id,
                    total_tokens,
                    request_count,
                },
            )
            .collect())
    }
}

impl std::fmt::Debug for AiTokenBudgetService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AiTokenBudgetService")
            .field("db", &"PgPool")
            .finish()
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn budget_period_daily_start() {
        let start = BudgetPeriod::Daily.period_start();
        let now = chrono::Utc::now().timestamp();
        // Start should be today's midnight — between 0 and 86400 seconds ago
        assert!(start <= now);
        assert!(now - start < 86_400);
    }

    #[test]
    fn budget_period_weekly_start() {
        let start = BudgetPeriod::Weekly.period_start();
        let now = chrono::Utc::now().timestamp();
        // Start should be this Monday's midnight — between 0 and 7*86400 seconds ago
        assert!(start <= now);
        assert!(now - start < 7 * 86_400);

        // Verify it's a Monday
        let dt = chrono::DateTime::from_timestamp(start, 0).unwrap();
        assert_eq!(dt.weekday(), chrono::Weekday::Mon);
    }

    #[test]
    fn budget_period_monthly_start() {
        let start = BudgetPeriod::Monthly.period_start();
        let now = chrono::Utc::now().timestamp();
        // Start should be the 1st of this month — between 0 and 31*86400 seconds ago
        assert!(start <= now);
        assert!(now - start < 31 * 86_400);

        // Verify it's the 1st
        let dt = chrono::DateTime::from_timestamp(start, 0).unwrap();
        assert_eq!(dt.day(), 1);
    }

    #[test]
    fn budget_config_default() {
        let config = BudgetConfig::default();
        assert_eq!(config.period, BudgetPeriod::Monthly);
        assert_eq!(config.action_on_limit, BudgetAction::Deny);
        assert!(config.defaults.is_empty());
        assert!(config.plugin_defaults.is_empty());
        assert!(config.plugin_cost_defaults.is_empty());
    }

    // ---- P11c / D-44 pricing / cost accounting ----

    #[test]
    fn pricing_cost_math_per_row() {
        let mut models = HashMap::new();
        models.insert(
            "gpt-4o".to_string(),
            ModelPrice {
                input_per_1k: 2.5,
                output_per_1k: 10.0,
                currency: "USD".to_string(),
            },
        );
        let pricing = AiPricingConfig { models };
        // 1000 input @ $2.5/1k + 500 output @ $10/1k = 2.5 + 5.0 = 7.5
        let cost = pricing.cost_for("gpt-4o", 1000, 500).unwrap();
        assert!((cost - 7.5).abs() < 1e-9, "cost was {cost}");
        // Zero tokens ⇒ zero cost (still priced, not None).
        assert_eq!(pricing.cost_for("gpt-4o", 0, 0), Some(0.0));
        // Negative token counts are floored at 0.
        assert_eq!(pricing.cost_for("gpt-4o", -5, -5), Some(0.0));
    }

    #[test]
    fn pricing_unpriced_model_degrades_to_none() {
        let pricing = AiPricingConfig::default();
        assert_eq!(
            pricing.cost_for("some-unpriced-model", 1000, 1000),
            None,
            "an unpriced model must yield no cost, never an invented one"
        );
        // A populated table still returns None for a model it does not list.
        let mut models = HashMap::new();
        models.insert(
            "gpt-4o".to_string(),
            ModelPrice {
                input_per_1k: 1.0,
                output_per_1k: 1.0,
                currency: "USD".to_string(),
            },
        );
        let pricing = AiPricingConfig { models };
        assert_eq!(pricing.cost_for("claude-opus-4", 1000, 1000), None);
    }

    #[test]
    fn model_price_currency_defaults_to_usd() {
        // Currency is optional in the config; absent ⇒ USD.
        let price: ModelPrice =
            serde_json::from_str(r#"{"input_per_1k": 1.0, "output_per_1k": 2.0}"#).unwrap();
        assert_eq!(price.currency, "USD");
    }

    #[test]
    fn budget_config_serde_roundtrip() {
        let mut defaults = HashMap::new();
        let mut role_limits = HashMap::new();
        role_limits.insert("authenticated".to_string(), 10_000u64);
        role_limits.insert("editor".to_string(), 50_000u64);
        role_limits.insert("admin".to_string(), 0u64);
        defaults.insert("openai-main".to_string(), role_limits);

        let config = BudgetConfig {
            period: BudgetPeriod::Weekly,
            action_on_limit: BudgetAction::Warn,
            defaults,
            plugin_defaults: HashMap::new(),
            plugin_cost_defaults: HashMap::new(),
        };

        let json = serde_json::to_string(&config).unwrap();
        let parsed: BudgetConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.period, BudgetPeriod::Weekly);
        assert_eq!(parsed.action_on_limit, BudgetAction::Warn);
        assert_eq!(parsed.defaults["openai-main"]["authenticated"], 10_000);
        assert_eq!(parsed.defaults["openai-main"]["admin"], 0);
    }

    #[test]
    fn budget_action_serde() {
        assert_eq!(
            serde_json::to_string(&BudgetAction::Deny).unwrap(),
            "\"deny\""
        );
        assert_eq!(
            serde_json::to_string(&BudgetAction::Warn).unwrap(),
            "\"warn\""
        );
        assert_eq!(
            serde_json::to_string(&BudgetAction::Queue).unwrap(),
            "\"queue\""
        );
    }

    #[test]
    fn budget_period_serde() {
        assert_eq!(
            serde_json::to_string(&BudgetPeriod::Daily).unwrap(),
            "\"daily\""
        );
        assert_eq!(
            serde_json::to_string(&BudgetPeriod::Weekly).unwrap(),
            "\"weekly\""
        );
        assert_eq!(
            serde_json::to_string(&BudgetPeriod::Monthly).unwrap(),
            "\"monthly\""
        );
    }

    use chrono::Datelike;

    #[test]
    fn budget_period_display() {
        assert_eq!(BudgetPeriod::Daily.to_string(), "daily");
        assert_eq!(BudgetPeriod::Weekly.to_string(), "weekly");
        assert_eq!(BudgetPeriod::Monthly.to_string(), "monthly");
    }
}
