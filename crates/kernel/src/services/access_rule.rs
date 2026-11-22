//! Access rules (B108): block registrations and sign ins by username, email
//! address or client address.
//!
//! A rule is a type (`username`, `email` or `host`), a pattern in which `%`
//! stands for any run of characters, and a status, `allow` or `deny`. A value
//! is blocked when a deny rule of its type matches it and no allow rule of
//! that type does: allow overrides deny, so a site can deny a whole domain and
//! still admit one address in it. An empty table blocks nothing.
//!
//! Checked at registration (username, email, client address), at sign in
//! (username, client address), and on admin user creation, where a match is a
//! warning rather than a refusal: an administrator may knowingly create an
//! account the public form would turn away.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

/// What a rule matches against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleType {
    Username,
    Email,
    Host,
}

impl RuleType {
    /// Every type, in display order.
    pub const ALL: [RuleType; 3] = [RuleType::Username, RuleType::Email, RuleType::Host];

    /// The stored name.
    pub fn as_str(self) -> &'static str {
        match self {
            RuleType::Username => "username",
            RuleType::Email => "email",
            RuleType::Host => "host",
        }
    }

    /// The name an administrator reads.
    pub fn label(self) -> &'static str {
        match self {
            RuleType::Username => "Username",
            RuleType::Email => "Email",
            RuleType::Host => "Host",
        }
    }

    /// Parse a stored name.
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.as_str() == value)
    }
}

/// Whether a matching rule admits or turns away.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleStatus {
    Allow,
    Deny,
}

impl RuleStatus {
    /// The stored name.
    pub fn as_str(self) -> &'static str {
        match self {
            RuleStatus::Allow => "allow",
            RuleStatus::Deny => "deny",
        }
    }

    /// Parse a stored name.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "allow" => Some(RuleStatus::Allow),
            "deny" => Some(RuleStatus::Deny),
            _ => None,
        }
    }
}

/// One stored rule.
#[derive(Debug, Clone, Serialize)]
pub struct AccessRule {
    pub id: Uuid,
    pub rule_type: RuleType,
    pub pattern: String,
    pub status: RuleStatus,
    /// Unix seconds.
    pub created: i64,
}

#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    rule_type: String,
    pattern: String,
    status: String,
    created: i64,
}

impl Row {
    fn into_rule(self) -> Option<AccessRule> {
        Some(AccessRule {
            id: self.id,
            rule_type: RuleType::parse(&self.rule_type)?,
            pattern: self.pattern,
            status: RuleStatus::parse(&self.status)?,
            created: self.created,
        })
    }
}

/// Whether `value` matches `pattern`: case-insensitive, the whole value, `%`
/// matching any run of characters (including none). No other character is
/// special.
pub fn pattern_matches(pattern: &str, value: &str) -> bool {
    let pattern: Vec<char> = pattern.to_lowercase().chars().collect();
    let value: Vec<char> = value.to_lowercase().chars().collect();
    // Greedy wildcard match with backtracking to the last `%`.
    let (mut p, mut v) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut star_v = 0usize;
    while v < value.len() {
        if p < pattern.len() && pattern[p] == '%' {
            star = Some(p);
            star_v = v;
            p += 1;
        } else if p < pattern.len() && pattern[p] == value[v] {
            p += 1;
            v += 1;
        } else if let Some(s) = star {
            p = s + 1;
            star_v += 1;
            v = star_v;
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|c| *c == '%')
}

/// Whether `rules` block `value` for `rule_type`: a deny rule of that type
/// matches and no allow rule of that type does.
pub fn blocks(rules: &[AccessRule], rule_type: RuleType, value: &str) -> bool {
    let mut denied = false;
    for rule in rules.iter().filter(|r| r.rule_type == rule_type) {
        if pattern_matches(&rule.pattern, value) {
            match rule.status {
                RuleStatus::Allow => return false,
                RuleStatus::Deny => denied = true,
            }
        }
    }
    denied
}

/// Every rule, by type, then deny before allow, then pattern.
pub async fn list(pool: &PgPool) -> Result<Vec<AccessRule>> {
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id, rule_type, pattern, status, created FROM access_rule \
         ORDER BY rule_type, status DESC, pattern",
    )
    .fetch_all(pool)
    .await
    .context("failed to list access rules")?;
    Ok(rows.into_iter().filter_map(Row::into_rule).collect())
}

/// The rules of one type.
pub async fn list_of_type(pool: &PgPool, rule_type: RuleType) -> Result<Vec<AccessRule>> {
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id, rule_type, pattern, status, created FROM access_rule WHERE rule_type = $1",
    )
    .bind(rule_type.as_str())
    .fetch_all(pool)
    .await
    .context("failed to load access rules")?;
    Ok(rows.into_iter().filter_map(Row::into_rule).collect())
}

/// One rule by id.
pub async fn find(pool: &PgPool, id: Uuid) -> Result<Option<AccessRule>> {
    let row: Option<Row> = sqlx::query_as(
        "SELECT id, rule_type, pattern, status, created FROM access_rule WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .context("failed to load access rule")?;
    Ok(row.and_then(Row::into_rule))
}

/// Add a rule; returns its id.
pub async fn create(
    pool: &PgPool,
    rule_type: RuleType,
    pattern: &str,
    status: RuleStatus,
) -> Result<Uuid> {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO access_rule (id, rule_type, pattern, status, created) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(rule_type.as_str())
    .bind(pattern)
    .bind(status.as_str())
    .bind(chrono::Utc::now().timestamp())
    .execute(pool)
    .await
    .context("failed to create access rule")?;
    Ok(id)
}

/// Change a rule; `false` when there is no such rule.
pub async fn update(
    pool: &PgPool,
    id: Uuid,
    rule_type: RuleType,
    pattern: &str,
    status: RuleStatus,
) -> Result<bool> {
    let done = sqlx::query(
        "UPDATE access_rule SET rule_type = $2, pattern = $3, status = $4 WHERE id = $1",
    )
    .bind(id)
    .bind(rule_type.as_str())
    .bind(pattern)
    .bind(status.as_str())
    .execute(pool)
    .await
    .context("failed to update access rule")?;
    Ok(done.rows_affected() > 0)
}

/// Remove a rule; `false` when there is no such rule.
pub async fn delete(pool: &PgPool, id: Uuid) -> Result<bool> {
    let done = sqlx::query("DELETE FROM access_rule WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await
        .context("failed to delete access rule")?;
    Ok(done.rows_affected() > 0)
}

/// Whether the stored rules block `value` for `rule_type`.
///
/// A database error blocks nothing and is logged: a broken rules table must
/// not lock every user out of the site.
pub async fn is_blocked(pool: &PgPool, rule_type: RuleType, value: &str) -> bool {
    match list_of_type(pool, rule_type).await {
        Ok(rules) => blocks(&rules, rule_type, value),
        Err(e) => {
            tracing::warn!(error = %e, "failed to check access rules; allowing");
            false
        }
    }
}

/// What a registration or account would be refused for: the types whose
/// value is blocked, in the order username, email, host. `None` values are
/// not checked.
pub async fn blocked_types(
    pool: &PgPool,
    username: Option<&str>,
    email: Option<&str>,
    host: Option<&str>,
) -> Vec<RuleType> {
    let mut found = Vec::new();
    for (rule_type, value) in [
        (RuleType::Username, username),
        (RuleType::Email, email),
        (RuleType::Host, host),
    ] {
        if let Some(value) = value
            && is_blocked(pool, rule_type, value).await
        {
            found.push(rule_type);
        }
    }
    found
}

/// The message a refused visitor reads for a blocked type.
pub fn refusal_message(rule_type: RuleType) -> &'static str {
    match rule_type {
        RuleType::Username => "This username is not allowed on this site.",
        RuleType::Email => "This email address is not allowed on this site.",
        RuleType::Host => "Registration from your address is not allowed on this site.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(rule_type: RuleType, pattern: &str, status: RuleStatus) -> AccessRule {
        AccessRule {
            id: Uuid::nil(),
            rule_type,
            pattern: pattern.to_string(),
            status,
            created: 0,
        }
    }

    #[test]
    fn a_pattern_matches_the_whole_value_with_percent_as_any_run() {
        assert!(pattern_matches("spam%", "spammer"));
        assert!(pattern_matches("spam%", "spam"));
        assert!(pattern_matches("%@example.com", "bob@EXAMPLE.com"));
        assert!(pattern_matches("%bad%", "a-bad-name"));
        assert!(pattern_matches("192.168.%.1", "192.168.40.1"));
        assert!(!pattern_matches("spam", "spammer"), "no wildcard, exact");
        assert!(!pattern_matches("%@example.com", "bob@example.com.evil"));
        assert!(!pattern_matches("a_c", "abc"), "underscore is literal");
        assert!(pattern_matches("a_c", "A_C"));
        assert!(pattern_matches("%", ""));
        assert!(!pattern_matches("x%", ""));
        assert!(pattern_matches("%a%b%", "xxaxxbxx"));
    }

    #[test]
    fn each_type_is_checked_only_against_its_own_rules() {
        let rules = vec![
            rule(RuleType::Username, "spam%", RuleStatus::Deny),
            rule(RuleType::Email, "%@spam.test", RuleStatus::Deny),
            rule(RuleType::Host, "10.0.%", RuleStatus::Deny),
        ];
        assert!(blocks(&rules, RuleType::Username, "spambot"));
        assert!(!blocks(&rules, RuleType::Email, "spambot"));
        assert!(blocks(&rules, RuleType::Email, "x@spam.test"));
        assert!(!blocks(&rules, RuleType::Username, "x@spam.test"));
        assert!(blocks(&rules, RuleType::Host, "10.0.3.4"));
        assert!(!blocks(&rules, RuleType::Host, "10.1.3.4"));
        assert!(
            !blocks(&[], RuleType::Host, "10.0.3.4"),
            "no rules, no block"
        );
    }

    #[test]
    fn allow_overrides_deny_of_the_same_type_only() {
        let rules = vec![
            rule(RuleType::Email, "%@example.com", RuleStatus::Deny),
            rule(RuleType::Email, "editor@example.com", RuleStatus::Allow),
            rule(RuleType::Username, "editor%", RuleStatus::Allow),
        ];
        assert!(blocks(&rules, RuleType::Email, "writer@example.com"));
        assert!(!blocks(&rules, RuleType::Email, "editor@example.com"));
        // An allow rule of another type does not lift an email deny.
        assert!(blocks(&rules, RuleType::Email, "editor2@example.com"));
        // An allow rule alone blocks nothing.
        assert!(!blocks(&rules, RuleType::Username, "someone"));
    }
}
