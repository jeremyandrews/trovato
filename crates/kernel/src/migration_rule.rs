//! The expand-then-contract rule for migrations, checked mechanically.
//!
//! Every migration must leave the previous release working, because the
//! previous release is still serving while the migration runs and is what a
//! rollback redeploys (`docs/design/upgrades-and-probes.md` §3, `UPGRADING.md`).
//!
//! [`check`] refuses the statements that can break code which still reads the
//! old shape: dropping or renaming, retyping a column, `SET NOT NULL`, a
//! `NOT NULL` column without a default, a new constraint, `TRUNCATE`, and a
//! plain `CREATE INDEX` on a table the file did not create (it blocks writes
//! for the whole build; build it `CONCURRENTLY` in a `-- no-transaction` file).
//!
//! A refused statement is allowed in a file that names the release whose code
//! stopped using the object:
//!
//! ```sql
//! -- trovato:contract 0.108.0: item.legacy_flag unread since 0.108.0
//! ```
//!
//! A file named `<version>_incompatible_<desc>.sql` declares that it breaks the
//! previous release. It is not checked; older binaries refuse to become ready
//! on a schema holding it, and its release needs planned downtime.
//!
//! This is a keyword scan, not a parser: comments, string literals and
//! dollar-quoted bodies are blanked first, then each statement is matched. It
//! only has to be right about the dozen shapes above.

/// The comment that allows a contract statement.
pub const CONTRACT_MARKER: &str = "-- trovato:contract ";

/// One refused statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// The statement, normalized (upper case, single spaces).
    pub statement: String,
    /// Why it is refused.
    pub reason: &'static str,
}

/// Whether a migration file name declares it incompatible with the previous
/// release: `<digits>_incompatible_<desc>.sql`.
pub fn is_incompatible_name(file_name: &str) -> bool {
    let base = file_name.rsplit('/').next().unwrap_or(file_name);
    base.split_once('_').is_some_and(|(version, rest)| {
        !version.is_empty()
            && version.bytes().all(|b| b.is_ascii_digit())
            && rest.starts_with("incompatible_")
    })
}

/// Whether a recorded migration description (sqlx stores the file name's
/// description with `_` as spaces) declares it incompatible.
pub fn is_incompatible_description(description: &str) -> bool {
    description.starts_with("incompatible ")
}

/// Check one migration's SQL against the rule.
///
/// `workspace_version` is the release being built; a contract marker must name
/// an older release, the one whose code stopped using the object. The checker
/// cannot see tags, so "older" is all it can hold a marker to.
pub fn check(sql: &str, workspace_version: &str) -> Result<(), Vec<Violation>> {
    if let Some(marker) = contract_marker(sql) {
        return match marker_is_valid(marker, workspace_version) {
            true => Ok(()),
            false => Err(vec![Violation {
                statement: marker.trim().to_string(),
                reason: "a contract marker must read `-- trovato:contract <x.y.z>: <reason>`, \
                         naming a release older than this one",
            }]),
        };
    }

    let statements: Vec<String> = blank_comments_and_literals(sql)
        .split(';')
        .map(normalize)
        .filter(|s| !s.is_empty())
        .collect();
    let created: Vec<String> = statements.iter().filter_map(|s| created_table(s)).collect();

    let violations: Vec<Violation> = statements
        .iter()
        .filter_map(|s| {
            refusal(s, &created).map(|reason| Violation {
                statement: s.clone(),
                reason,
            })
        })
        .collect();
    if violations.is_empty() {
        Ok(())
    } else {
        Err(violations)
    }
}

/// The marker line, if the file has one.
fn contract_marker(sql: &str) -> Option<&str> {
    sql.lines()
        .map(str::trim_start)
        .find_map(|l| l.strip_prefix(CONTRACT_MARKER))
}

fn marker_is_valid(marker: &str, workspace_version: &str) -> bool {
    let Some((version, reason)) = marker.split_once(':') else {
        return false;
    };
    match (
        parse_version(version.trim()),
        parse_version(workspace_version),
    ) {
        (Some(named), Some(current)) => named < current && !reason.trim().is_empty(),
        _ => false,
    }
}

fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let mut parts = v.split('.').map(|p| p.parse::<u64>().ok());
    let version = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(version)
}

/// Why `statement` is refused, if it is.
fn refusal(s: &str, created: &[String]) -> Option<&'static str> {
    // Every kind of DROP but an index's, so a kind nobody listed is refused.
    if s.starts_with("DROP ") && !s.starts_with("DROP INDEX ") {
        return Some("drops an object the previous release may still use");
    }
    if s.starts_with("TRUNCATE ") {
        return Some("deletes rows the previous release may still read");
    }
    if s.starts_with("ALTER ") && s.contains(" RENAME ") {
        return Some("renames an object the previous release reads by its old name");
    }
    // A table this file creates has no previous release reading it.
    if let Some(table) = altered_table(s)
        && !created.contains(&table)
    {
        if s.contains(" DROP COLUMN ") {
            return Some("drops a column the previous release may still read");
        }
        if alters_column_type(s) {
            return Some("retypes a column the previous release reads");
        }
        if s.contains(" SET NOT NULL") {
            return Some("makes a column NOT NULL that the previous release may leave empty");
        }
        if s.contains(" DROP DEFAULT") {
            return Some("removes a default the previous release's inserts may rely on");
        }
        for action in alter_actions(s) {
            if let Some(reason) = add_refusal(&action) {
                return Some(reason);
            }
        }
    }
    if let Some(table) = plain_index_table(s)
        && !created.contains(&table)
    {
        return Some(
            "builds an index on an existing table without CONCURRENTLY, which blocks \
             its writes for the whole build (kernel: CREATE INDEX CONCURRENTLY in a \
             `-- no-transaction` file; a plugin migration always runs in a \
             transaction, so it needs a contract marker)",
        );
    }
    None
}

/// `ALTER TABLE [IF EXISTS] [ONLY] name ...` → `name`.
fn altered_table(s: &str) -> Option<String> {
    let rest = s.strip_prefix("ALTER TABLE ")?;
    let rest = rest.strip_prefix("IF EXISTS ").unwrap_or(rest);
    let rest = rest.strip_prefix("ONLY ").unwrap_or(rest);
    rest.split(' ').next().map(table_name)
}

fn alters_column_type(s: &str) -> bool {
    s.match_indices("ALTER COLUMN ").any(|(i, m)| {
        let rest = &s[i + m.len()..];
        let mut words = rest.split(' ').skip(1);
        match words.next() {
            Some("TYPE") => true,
            Some("SET") => matches!((words.next(), words.next()), (Some("DATA"), Some("TYPE"))),
            _ => false,
        }
    })
}

/// The comma-separated actions of an `ALTER TABLE`, split at the top level.
fn alter_actions(s: &str) -> Vec<String> {
    let mut actions = Vec::new();
    let mut depth = 0i32;
    let mut current = String::new();
    for c in s.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                actions.push(std::mem::take(&mut current));
                continue;
            }
            _ => {}
        }
        current.push(c);
    }
    actions.push(current);
    // The first action carries the `ALTER TABLE name` prefix; drop it.
    actions
        .into_iter()
        .map(|a| {
            let a = a.trim().to_string();
            match a.find(" ADD ") {
                Some(i) if a.starts_with("ALTER TABLE ") => a[i + 1..].to_string(),
                _ => a,
            }
        })
        .collect()
}

fn add_refusal(action: &str) -> Option<&'static str> {
    let rest = action.strip_prefix("ADD ")?;
    const CONSTRAINTS: [&str; 6] = [
        "CONSTRAINT ",
        "PRIMARY KEY",
        "UNIQUE",
        "FOREIGN KEY",
        "CHECK",
        "EXCLUDE",
    ];
    if CONSTRAINTS.iter().any(|c| rest.starts_with(c)) {
        return Some("adds a constraint the previous release's writes may not meet");
    }
    (rest.contains("NOT NULL") && !rest.contains(" DEFAULT ")).then_some(
        "adds a NOT NULL column without a DEFAULT, which the previous release's inserts leave empty",
    )
}

/// `CREATE TABLE [IF NOT EXISTS] name` → `name`.
fn created_table(s: &str) -> Option<String> {
    let rest = s
        .strip_prefix("CREATE TABLE ")
        .or_else(|| s.strip_prefix("CREATE UNLOGGED TABLE "))?;
    let rest = rest.strip_prefix("IF NOT EXISTS ").unwrap_or(rest);
    rest.split([' ', '(']).next().map(table_name)
}

/// The table of a `CREATE INDEX` built without `CONCURRENTLY`.
fn plain_index_table(s: &str) -> Option<String> {
    let rest = s
        .strip_prefix("CREATE INDEX ")
        .or_else(|| s.strip_prefix("CREATE UNIQUE INDEX "))?;
    if rest.starts_with("CONCURRENTLY ") {
        return None;
    }
    let (_, after_on) = rest
        .split_once(" ON ")
        .or_else(|| rest.strip_prefix("ON ").map(|r| ("", r)))?;
    let after_on = after_on.strip_prefix("ONLY ").unwrap_or(after_on);
    after_on.split([' ', '(']).next().map(table_name)
}

fn table_name(raw: &str) -> String {
    let name = raw.rsplit('.').next().unwrap_or(raw);
    name.trim_matches('"').to_ascii_uppercase()
}

fn normalize(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_uppercase()
}

/// Replace comments, string literals and dollar-quoted bodies with spaces
/// (quoted identifiers are kept), so a keyword inside one is not read as a
/// statement.
fn blank_comments_and_literals(sql: &str) -> String {
    let bytes = sql.as_bytes();
    let mut out = String::with_capacity(sql.len());
    let mut i = 0;
    while i < bytes.len() {
        let rest = &sql[i..];
        if rest.starts_with("--") {
            let end = rest.find('\n').map_or(bytes.len(), |n| i + n);
            out.push(' ');
            i = end;
        } else if rest.starts_with("/*") {
            let end = rest.find("*/").map_or(bytes.len(), |n| i + n + 2);
            out.push(' ');
            i = end;
        } else if rest.starts_with('\'') {
            // '' is an escaped quote inside a literal.
            let mut j = i + 1;
            while j < bytes.len() {
                if bytes[j] == b'\'' {
                    if bytes.get(j + 1) == Some(&b'\'') {
                        j += 2;
                        continue;
                    }
                    break;
                }
                j += 1;
            }
            out.push_str(" '' ");
            i = (j + 1).min(bytes.len());
        } else if let Some(tag) = dollar_tag(rest) {
            let body_start = i + tag.len();
            let end = sql[body_start..]
                .find(tag)
                .map_or(bytes.len(), |n| body_start + n + tag.len());
            out.push_str(" '' ");
            i = end;
        } else {
            let c = rest.chars().next().unwrap_or(' ');
            out.push(c);
            i += c.len_utf8();
        }
    }
    out
}

/// `$tag$` or `$$` at the start of `s`.
fn dollar_tag(s: &str) -> Option<&str> {
    let rest = s.strip_prefix('$')?;
    let end = rest.find('$')?;
    let tag = &rest[..end];
    tag.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_')
        .then(|| &s[..end + 2])
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    const V: &str = "0.107.0";

    fn refused(sql: &str) -> bool {
        check(sql, V).is_err()
    }

    #[test]
    fn drop_column_without_the_marker_fails_and_with_it_passes() {
        let sql = "ALTER TABLE item DROP COLUMN legacy_flag;";
        let err = check(sql, V).unwrap_err();
        assert_eq!(err.len(), 1);
        assert!(err[0].reason.contains("drops a column"), "{err:?}");

        let marked = "-- trovato:contract 0.106.0: item.legacy_flag unread since 0.106.0\n\
                      ALTER TABLE item DROP COLUMN legacy_flag;";
        assert_eq!(check(marked, V), Ok(()));
    }

    #[test]
    fn a_marker_must_name_an_existing_release_and_a_reason() {
        let sql = "ALTER TABLE item DROP COLUMN legacy_flag;";
        for marker in [
            "-- trovato:contract 0.108.0: not released yet",
            "-- trovato:contract 0.107.0: the release being built",
            "-- trovato:contract 0.106.0:",
            "-- trovato:contract 0.106: no patch number",
            "-- trovato:contract soon: no version",
        ] {
            assert!(refused(&format!("{marker}\n{sql}")), "{marker}");
        }
        assert!(!refused(&format!(
            "-- trovato:contract 0.106.3: an older release\n{sql}"
        )));
    }

    #[test]
    fn contract_statements_are_refused() {
        for sql in [
            "DROP TABLE old_thing;",
            "drop view old_view;",
            "DROP FUNCTION f();",
            "DROP TYPE mood;",
            "ALTER TABLE item RENAME COLUMN a TO b;",
            "ALTER TABLE item RENAME TO items;",
            "ALTER TABLE item ALTER COLUMN title TYPE TEXT;",
            "ALTER TABLE item ALTER COLUMN title SET DATA TYPE TEXT;",
            "ALTER TABLE item ALTER COLUMN title SET NOT NULL;",
            "ALTER TABLE item ADD COLUMN weight INTEGER NOT NULL;",
            "ALTER TABLE item ADD weight INTEGER NOT NULL;",
            "ALTER TABLE item ADD COLUMN a TEXT, ADD COLUMN b INT NOT NULL;",
            "ALTER TABLE item ADD CONSTRAINT title_len CHECK (length(title) < 9);",
            "ALTER TABLE item ADD UNIQUE (title);",
            "ALTER VIEW v RENAME TO w;",
            "ALTER TYPE mood RENAME VALUE 'sad' TO 'blue';",
            "DROP SEQUENCE item_seq;",
            "DROP TRIGGER t ON item;",
            "ALTER TABLE item ALTER COLUMN weight DROP DEFAULT;",
            "ALTER TABLE IF EXISTS ONLY item DROP COLUMN legacy;",
            "TRUNCATE search_analytics;",
            "CREATE INDEX idx_item_title ON item (title);",
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_item_slug ON public.item (slug);",
        ] {
            assert!(refused(sql), "should be refused: {sql}");
        }
    }

    #[test]
    fn expand_statements_pass() {
        for sql in [
            "CREATE TABLE thing (id UUID PRIMARY KEY, name TEXT NOT NULL);",
            "CREATE TABLE IF NOT EXISTS thing (id UUID PRIMARY KEY);\n\
             CREATE INDEX idx_thing_id ON thing (id);",
            "ALTER TABLE item ADD COLUMN summary TEXT;",
            "ALTER TABLE item ADD COLUMN IF NOT EXISTS weight INTEGER NOT NULL DEFAULT 0;",
            "ALTER TABLE item ADD COLUMN owner UUID REFERENCES users(id);",
            "ALTER TABLE item ALTER COLUMN summary DROP NOT NULL;",
            "ALTER TABLE item ALTER COLUMN summary SET DEFAULT '';",
            "-- no-transaction\nCREATE INDEX CONCURRENTLY idx_item_title ON item (title);",
            "UPDATE item SET summary = '' WHERE summary IS NULL;",
            "INSERT INTO role_permissions (role_id, permission) VALUES ('x', 'drop table y');",
            "DROP INDEX IF EXISTS idx_unused;",
            "ALTER TABLE item DROP CONSTRAINT IF EXISTS old_check;",
            // A table this file creates: nothing older reads it.
            "CREATE TABLE thing (id INT, name TEXT);\n\
             ALTER TABLE thing ALTER COLUMN name SET NOT NULL;\n\
             ALTER TABLE thing ADD CONSTRAINT thing_pk PRIMARY KEY (id);",
        ] {
            assert_eq!(check(sql, V), Ok(()), "should pass: {sql}");
        }
    }

    #[test]
    fn keywords_inside_comments_literals_and_bodies_are_ignored() {
        for sql in [
            "-- DROP TABLE x;\nCREATE TABLE y (id INT);",
            "/* ALTER TABLE item DROP COLUMN a; */ SELECT 1;",
            "INSERT INTO t (v) VALUES ('it''s; DROP TABLE x');",
            "CREATE FUNCTION f() RETURNS void AS $$ BEGIN DROP TABLE tmp; END $$ LANGUAGE plpgsql;",
            "CREATE FUNCTION g() RETURNS void AS $body$ TRUNCATE x; $body$ LANGUAGE plpgsql;",
        ] {
            assert_eq!(check(sql, V), Ok(()), "should pass: {sql}");
        }
    }

    #[test]
    fn incompatible_names() {
        assert!(is_incompatible_name(
            "20261201000001_incompatible_drop_x.sql"
        ));
        assert!(is_incompatible_name("migrations/002_incompatible_x.sql"));
        assert!(!is_incompatible_name(
            "20261201000001_fix_incompatible_types.sql"
        ));
        assert!(!is_incompatible_name("incompatible_x.sql"));
        assert!(is_incompatible_description("incompatible drop x"));
        assert!(!is_incompatible_description("fix incompatible types"));
    }

    /// Kernel migrations newer than this were written under the rule; older
    /// ones are history and are not re-litigated.
    const KERNEL_CUTOFF: u64 = 20261127160002;

    /// How many kernel migrations are at or below the cutoff. Pinned, so a file
    /// given an old timestamp (a long-lived branch, a hand-picked name) cannot
    /// slip under the cutoff unchecked; sqlx would apply it all the same.
    const KERNEL_HISTORY: usize = 87;

    /// The rule over the tree: every kernel migration after the cutoff and
    /// every plugin migration. This is the check
    /// `scripts/pre-commit-check.sh` and CI run.
    #[test]
    fn every_new_migration_in_the_tree_follows_the_rule() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let version = env!("CARGO_PKG_VERSION");
        let mut failures = Vec::new();
        let mut history = 0;

        let mut check_file = |path: &std::path::Path, label: String| {
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if is_incompatible_name(&name) {
                return;
            }
            let sql = std::fs::read_to_string(path).unwrap();
            if let Err(violations) = check(&sql, version) {
                for v in violations {
                    failures.push(format!("{label}: {} — {}", v.reason, v.statement));
                }
            }
        };

        for entry in std::fs::read_dir(root.join("crates/kernel/migrations")).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            let Some(version) = name.split('_').next().and_then(|v| v.parse::<u64>().ok()) else {
                continue;
            };
            if !name.ends_with(".sql") {
                continue;
            }
            if version > KERNEL_CUTOFF {
                check_file(&path, format!("crates/kernel/migrations/{name}"));
            } else {
                history += 1;
            }
        }

        for plugin in std::fs::read_dir(root.join("plugins")).unwrap() {
            let dir = plugin.unwrap().path().join("migrations");
            let Ok(files) = std::fs::read_dir(&dir) else {
                continue;
            };
            for file in files {
                let path = file.unwrap().path();
                if path.extension().is_none_or(|e| e != "sql") {
                    continue;
                }
                let label = path
                    .strip_prefix(&root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .to_string();
                check_file(&path, label);
            }
        }

        assert_eq!(
            history, KERNEL_HISTORY,
            "a kernel migration was added at or below KERNEL_CUTOFF; give it a current timestamp"
        );
        assert!(
            failures.is_empty(),
            "migrations that break the previous release (docs/design/upgrades-and-probes.md §3); \
             mark a deliberate contract step with `-- trovato:contract <release>: <reason>`:\n{}",
            failures.join("\n")
        );
    }
}
