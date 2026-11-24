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
//! for the whole build; build it `CONCURRENTLY`, alone in a `-- no-transaction`
//! kernel file, and not `UNIQUE`, which is a new constraint).
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

/// Where a migration runs, which decides what it may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// A kernel migration, run by sqlx: a first line `-- no-transaction`
    /// runs it outside a transaction.
    Kernel,
    /// A plugin migration: always inside a transaction.
    Plugin,
}

/// Check one migration's SQL against the rule.
///
/// `workspace_version` is the release being built; a contract marker must name
/// an older release, the one whose code stopped using the object. The checker
/// cannot see tags, so "older" is all it can hold a marker to.
pub fn check(sql: &str, workspace_version: &str, source: Source) -> Result<(), Vec<Violation>> {
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
    let no_transaction = source == Source::Kernel && sql.starts_with("-- no-transaction");

    let violations: Vec<Violation> = statements
        .iter()
        .filter_map(|s| {
            refusal(s, &created)
                .or_else(|| concurrent_index_refusal(s, statements.len(), no_transaction))
                .map(|reason| Violation {
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

/// The marker, if the file has one in its leading comment block (the lines
/// before the first statement), so a marker-shaped line inside a string or a
/// later comment blesses nothing.
fn contract_marker(sql: &str) -> Option<&str> {
    sql.lines()
        .map(str::trim_start)
        .take_while(|l| l.is_empty() || l.starts_with("--"))
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
    if let Some((table, actions)) = altered_table(s)
        && !created.contains(&table)
    {
        return actions.iter().find_map(|a| action_refusal(a));
    }
    if let Some(table) = plain_index_table(s)
        && !created.contains(&table)
    {
        return Some(if s.starts_with("CREATE UNIQUE ") {
            "builds a unique index on an existing table: a new uniqueness rule the \
                 previous release's writes may break, so it needs a contract marker"
        } else {
            "builds an index on an existing table without CONCURRENTLY, which blocks \
                 its writes for the whole build (kernel: CREATE INDEX CONCURRENTLY alone \
                 in a `-- no-transaction` file; a plugin migration always runs in a \
                 transaction, so it needs a contract marker)"
        });
    }
    None
}

/// `CONCURRENTLY` cannot run in a transaction block, and Postgres runs a
/// string of several statements (which is how sqlx sends a `-- no-transaction`
/// file) as one. So a concurrent build or drop must be the only statement of a
/// `-- no-transaction` kernel file. A failed or cancelled build leaves an
/// INVALID index under its name; without `IF NOT EXISTS`, the retry fails on it
/// loudly (recovery: `DROP INDEX CONCURRENTLY <name>`, then rerun) instead of
/// recording the migration as done over a broken index.
fn concurrent_index_refusal(
    s: &str,
    statements_in_file: usize,
    no_transaction: bool,
) -> Option<&'static str> {
    if !s.contains(" CONCURRENTLY ") || !(s.starts_with("CREATE ") || s.starts_with("DROP ")) {
        return None;
    }
    if s.starts_with("CREATE UNIQUE ") {
        return Some(
            "adds a uniqueness rule to an existing table, which the previous release's \
             writes may break, as ADD UNIQUE would",
        );
    }
    if !no_transaction {
        return Some(
            "uses CONCURRENTLY, which cannot run in a transaction: only a kernel \
             migration whose first line is `-- no-transaction` can",
        );
    }
    if statements_in_file != 1 {
        return Some(
            "uses CONCURRENTLY in a file with other statements: Postgres runs them as one \
             transaction block, where CONCURRENTLY fails; give it a file of its own",
        );
    }
    if s.starts_with("CREATE INDEX CONCURRENTLY IF NOT EXISTS ") {
        return Some(
            "builds an index CONCURRENTLY IF NOT EXISTS, which would keep the INVALID index \
             a failed build leaves and record the migration as done",
        );
    }
    None
}

/// `ALTER TABLE [IF EXISTS] [ONLY] name action, action ...` → the table and
/// its actions, split at top-level commas.
fn altered_table(s: &str) -> Option<(String, Vec<String>)> {
    let rest = s.strip_prefix("ALTER TABLE ")?;
    let rest = rest.strip_prefix("IF EXISTS ").unwrap_or(rest);
    let rest = rest.strip_prefix("ONLY ").unwrap_or(rest);
    let (name, actions) = rest.split_once(' ').unwrap_or((rest, ""));
    Some((table_name(name), split_top_level(actions)))
}

fn split_top_level(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut current = String::new();
    for c in s.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(std::mem::take(&mut current).trim().to_string());
                continue;
            }
            _ => {}
        }
        current.push(c);
    }
    parts.push(current.trim().to_string());
    parts
}

/// Why one `ALTER TABLE` action on an existing table is refused, if it is.
/// `COLUMN` is optional in Postgres (`DROP c`, `ALTER c TYPE t`), so actions
/// are read by position, not by looking for `DROP COLUMN`.
fn action_refusal(action: &str) -> Option<&'static str> {
    let words: Vec<&str> = action.split(' ').collect();
    match words.as_slice() {
        // Dropping a constraint only relaxes what the previous release meets.
        ["DROP", "CONSTRAINT", ..] => None,
        ["DROP", ..] => Some("drops a column the previous release may still read"),
        ["ADD", ..] => add_refusal(action),
        ["ALTER", rest @ ..] => {
            let rest = match rest {
                ["COLUMN", rest @ ..] => rest,
                rest => rest,
            };
            match rest.get(1..).unwrap_or_default() {
                ["TYPE", ..] | ["SET", "DATA", "TYPE", ..] => {
                    Some("retypes a column the previous release reads")
                }
                ["SET", "NOT", "NULL", ..] => {
                    Some("makes a column NOT NULL that the previous release may leave empty")
                }
                ["DROP", "DEFAULT", ..] => {
                    Some("removes a default the previous release's inserts may rely on")
                }
                _ => None,
            }
        }
        _ => None,
    }
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
        check(sql, V, Source::Kernel).is_err()
    }

    #[test]
    fn drop_column_without_the_marker_fails_and_with_it_passes() {
        let sql = "ALTER TABLE item DROP COLUMN legacy_flag;";
        let err = check(sql, V, Source::Kernel).unwrap_err();
        assert_eq!(err.len(), 1);
        assert!(err[0].reason.contains("drops a column"), "{err:?}");

        let marked = "-- trovato:contract 0.106.0: item.legacy_flag unread since 0.106.0\n\
                      ALTER TABLE item DROP COLUMN legacy_flag;";
        assert_eq!(check(marked, V, Source::Kernel), Ok(()));
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
            // COLUMN is optional in Postgres.
            "ALTER TABLE item DROP legacy;",
            "ALTER TABLE item DROP IF EXISTS legacy;",
            "ALTER TABLE item ALTER title TYPE TEXT;",
            "ALTER TABLE item ALTER title SET DATA TYPE TEXT;",
            "ALTER TABLE item ALTER title SET NOT NULL;",
            "ALTER TABLE item ADD COLUMN a TEXT, DROP b;",
            "DROP SCHEMA old CASCADE;",
            "ALTER TABLE item ADD PRIMARY KEY (id);",
            "ALTER TABLE item ADD FOREIGN KEY (owner) REFERENCES users(id);",
            "ALTER TABLE item ADD EXCLUDE USING gist (span WITH &&);",
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
            "CREATE UNLOGGED TABLE scratch (id INT);\nCREATE INDEX idx_scratch ON scratch (id);",
            "UPDATE item SET summary = '' WHERE summary IS NULL;",
            "INSERT INTO role_permissions (role_id, permission) VALUES ('x', 'drop table y');",
            "DROP INDEX IF EXISTS idx_unused;",
            "ALTER TABLE item DROP CONSTRAINT IF EXISTS old_check;",
            // A table this file creates: nothing older reads it.
            "CREATE TABLE thing (id INT, name TEXT);\n\
             ALTER TABLE thing ALTER COLUMN name SET NOT NULL;\n\
             ALTER TABLE thing ADD CONSTRAINT thing_pk PRIMARY KEY (id);",
        ] {
            assert_eq!(check(sql, V, Source::Kernel), Ok(()), "should pass: {sql}");
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
            assert_eq!(check(sql, V, Source::Kernel), Ok(()), "should pass: {sql}");
        }
    }

    #[test]
    fn a_concurrent_index_needs_a_no_transaction_file_of_its_own() {
        let build = "CREATE INDEX CONCURRENTLY idx_item_title ON item (title);";
        let drop = "DROP INDEX CONCURRENTLY IF EXISTS idx_item_title;";
        // Inside a transaction it cannot run at all.
        assert!(refused(build));
        assert!(refused(drop));
        // With any other statement Postgres runs the file as one transaction
        // block, where CONCURRENTLY fails.
        assert!(refused(&format!("-- no-transaction\n{drop}\n{build}")));
        assert!(refused(&format!(
            "-- no-transaction\n{build}\nUPDATE item SET title = title;"
        )));
        // IF NOT EXISTS would keep the INVALID index a failed build leaves.
        assert!(refused(
            "-- no-transaction\n\
             CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_item_title ON item (title);"
        ));
        // Alone in its file it runs, the build and the drop alike.
        assert!(!refused(&format!("-- no-transaction\n{build}")));
        assert!(!refused(&format!("-- no-transaction\n-- why\n{build}\n")));
        assert!(!refused(&format!("-- no-transaction\n{drop}")));
        // A unique one is a new constraint on existing rows and writes.
        assert!(refused(
            "-- no-transaction\nCREATE UNIQUE INDEX CONCURRENTLY u ON item (slug);"
        ));
        // A plugin migration always runs in a transaction.
        assert!(check(&format!("-- no-transaction\n{build}"), V, Source::Plugin).is_err());
    }

    #[test]
    fn the_marker_counts_only_in_the_leading_comments() {
        let sql = "ALTER TABLE item DROP COLUMN legacy_flag;";
        let marker = "-- trovato:contract 0.106.0: unread since 0.106.0";
        assert!(!refused(&format!("-- Drop the flag.\n\n{marker}\n{sql}")));
        // After the first statement, in a comment or a string, it blesses nothing.
        assert!(refused(&format!("{sql}\n{marker}")));
        assert!(refused(&format!(
            "INSERT INTO note (body) VALUES ('\n{marker}\n');\n{sql}"
        )));
        assert!(refused(&format!("/*\n{marker}\n*/\n{sql}")));
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

        let mut check_file = |path: &std::path::Path, label: String, source: Source| {
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if is_incompatible_name(&name) {
                return;
            }
            let sql = std::fs::read_to_string(path).unwrap();
            if let Err(violations) = check(&sql, version, source) {
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
                check_file(
                    &path,
                    format!("crates/kernel/migrations/{name}"),
                    Source::Kernel,
                );
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
                check_file(&path, label, Source::Plugin);
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
