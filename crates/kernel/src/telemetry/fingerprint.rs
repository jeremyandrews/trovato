//! A statement's fingerprint: what it is, never what it was asked about.
//!
//! `select:item:3fa2c1d0` is the verb, the first table and eight hex digits of
//! the SHA-256 of the normalised SQL. Normalising:
//!
//! - drops comments and squeezes whitespace;
//! - lowercases everything outside a quoted identifier;
//! - replaces every string and number literal with `?`, so a value `format!`
//!   built into the SQL text never survives into a span or a test;
//! - turns `$1`, `$2` ... into `$?` and collapses a comma-separated run of
//!   them (or of `?`) into one, so an `IN` list does not change its
//!   statement's fingerprint with its length.
//!
//! Bound values never reach here at all: sqlx's statement event carries the
//! SQL text and nothing it was bound to.

use sha2::{Digest, Sha256};

/// The normalised form of `sql`: see the module documentation.
pub fn normalize(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let bytes = sql.as_bytes();
    let mut i = 0;
    // Whether the last thing pushed was whitespace (or nothing yet), so runs
    // of whitespace and comments collapse to one space.
    let mut pending_space = false;
    // Set by an `E` prefix: only an escape string treats a backslash as an
    // escape.
    let mut escape_string = false;

    while i < bytes.len() {
        let c = bytes[i];
        match c {
            // Line comment.
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                pending_space = true;
            }
            // Block comment.
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
                pending_space = true;
            }
            c if c.is_ascii_whitespace() => {
                pending_space = true;
                i += 1;
            }
            // A string literal, with '' as an escaped quote. An `E` prefix
            // (escape string) is consumed with it.
            b'\'' => {
                i += 1;
                while i < bytes.len() {
                    if escape_string && bytes[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == b'\'' {
                        if bytes.get(i + 1) == Some(&b'\'') {
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                escape_string = false;
                push_token(&mut out, &mut pending_space, "?");
            }
            // A quoted identifier keeps its case and its content.
            b'"' => {
                let start = i;
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += 1;
                }
                i = (i + 1).min(bytes.len());
                push_token(&mut out, &mut pending_space, &sql[start..i]);
            }
            // A placeholder.
            b'$' if bytes.get(i + 1).is_some_and(u8::is_ascii_digit) => {
                i += 1;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                push_token(&mut out, &mut pending_space, "$?");
            }
            // A dollar-quoted string: $$...$$ or $tag$...$tag$.
            b'$' => {
                let tag_end = sql[i + 1..].find('$').map(|p| i + 1 + p);
                match tag_end {
                    Some(end)
                        if sql[i + 1..end]
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'_') =>
                    {
                        let tag = &sql[i..=end];
                        let body_start = end + 1;
                        let close = sql[body_start..]
                            .find(tag)
                            .map_or(bytes.len(), |p| body_start + p + tag.len());
                        i = close;
                        push_token(&mut out, &mut pending_space, "?");
                    }
                    _ => {
                        push_token(&mut out, &mut pending_space, "$");
                        i += 1;
                    }
                }
            }
            // A number that is not part of an identifier.
            c if c.is_ascii_digit() && !last_is_word(&out, pending_space) => {
                while i < bytes.len()
                    && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'.')
                {
                    i += 1;
                }
                push_token(&mut out, &mut pending_space, "?");
            }
            c if c.is_ascii_alphanumeric() || c == b'_' => {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                // `E'...'`: the prefix of an escape string belongs to it.
                if (i - start == 1)
                    && (bytes[start] == b'e' || bytes[start] == b'E')
                    && bytes.get(i) == Some(&b'\'')
                {
                    escape_string = true;
                    continue;
                }
                push_token(
                    &mut out,
                    &mut pending_space,
                    &sql[start..i].to_ascii_lowercase(),
                );
            }
            _ => {
                // Punctuation; non-ASCII bytes outside a literal are copied
                // through a whole character at a time.
                let ch_len = utf8_len(c);
                let end = (i + ch_len).min(bytes.len());
                push_token(&mut out, &mut pending_space, &sql[i..end]);
                i = end;
            }
        }
    }
    collapse_lists(&out)
}

/// The fingerprint of `sql`: `<verb>:<first table>:<8 hex>`.
pub fn fingerprint(sql: &str) -> String {
    let normalized = normalize(sql);
    fingerprint_normalized(&normalized)
}

/// The fingerprint of SQL that [`normalize`] has already produced.
pub fn fingerprint_normalized(normalized: &str) -> String {
    let digest = Sha256::digest(normalized.as_bytes());
    let verb = normalized
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .find(|w| !w.is_empty())
        .unwrap_or("-");
    let table = first_table(normalized).unwrap_or("-");
    format!(
        "{verb}:{table}:{:02x}{:02x}{:02x}{:02x}",
        digest[0], digest[1], digest[2], digest[3]
    )
}

/// The first table a normalised statement names: the word after the first
/// `from`, `into`, `update` or `join`, without quotes or schema.
fn first_table(normalized: &str) -> Option<&str> {
    let mut words = normalized
        .split(|c: char| c.is_ascii_whitespace() || c == ',' || c == '(' || c == ')')
        .filter(|w| !w.is_empty());
    while let Some(word) = words.next() {
        if matches!(word, "from" | "into" | "update" | "join") {
            let next = words.next()?;
            if next == "only" || next == "lateral" {
                continue;
            }
            let bare = next.rsplit('.').next().unwrap_or(next).trim_matches('"');
            if bare == "?" || bare == "$?" || bare.is_empty() {
                return None;
            }
            return Some(bare);
        }
    }
    None
}

fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

/// Whether the output ends in a word character, so a digit continues it
/// (`table1`) rather than starting a number.
fn last_is_word(out: &str, pending_space: bool) -> bool {
    !pending_space
        && out
            .as_bytes()
            .last()
            .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
}

fn push_token(out: &mut String, pending_space: &mut bool, token: &str) {
    if *pending_space && !out.is_empty() {
        out.push(' ');
    }
    *pending_space = false;
    out.push_str(token);
}

/// Collapse `$?, $?, $?` (and `?, ?`) into one placeholder.
fn collapse_lists(normalized: &str) -> String {
    let mut out = String::with_capacity(normalized.len());
    let mut rest = normalized;
    while !rest.is_empty() {
        let (token, after) = if let Some(after) = rest.strip_prefix("$?") {
            ("$?", after)
        } else if let Some(after) = rest.strip_prefix('?') {
            ("?", after)
        } else {
            let ch_len = rest.chars().next().map_or(1, char::len_utf8);
            out.push_str(&rest[..ch_len]);
            rest = &rest[ch_len..];
            continue;
        };
        out.push_str(token);
        rest = after;
        // Swallow every following `, $?` / `, ?`.
        loop {
            let trimmed = rest.trim_start_matches(' ');
            let Some(after_comma) = trimmed.strip_prefix(',') else {
                break;
            };
            let after_comma = after_comma.trim_start_matches(' ');
            if let Some(after) = after_comma.strip_prefix("$?") {
                rest = after;
            } else if let Some(after) = after_comma.strip_prefix('?') {
                rest = after;
            } else {
                break;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_never_survive() {
        let n = normalize(
            "SELECT * FROM users WHERE mail = 'jane@example.com' AND id = 42 AND pass = E'p\\'w'",
        );
        assert_eq!(
            n,
            "select * from users where mail = ? and id = ? and pass = ?"
        );
        assert!(!n.contains("jane"));
        // PG16 hex and underscore numbers leave no digits behind.
        assert_eq!(normalize("SELECT 0xDEAD, 1_000 FROM t"), "select ? from t");
    }

    #[test]
    fn placeholder_lists_collapse() {
        assert_eq!(
            normalize("SELECT id FROM item WHERE id IN ($1, $2, $3) LIMIT $4"),
            "select id from item where id in ($?) limit $?"
        );
        assert_eq!(
            fingerprint("SELECT id FROM item WHERE id IN ($1, $2)"),
            fingerprint("SELECT id FROM item WHERE id IN ($1,$2,$3,$4)"),
        );
    }

    #[test]
    fn whitespace_case_and_comments_do_not_matter() {
        assert_eq!(
            fingerprint("select  id\n  from item -- note\n where x = $1"),
            fingerprint("SELECT id /* c */ FROM item WHERE x = $1"),
        );
    }

    #[test]
    fn identifiers_with_digits_are_kept() {
        assert_eq!(normalize("SELECT col1 FROM t2"), "select col1 from t2");
    }

    #[test]
    fn dollar_quoted_strings_are_literals() {
        assert_eq!(
            normalize("SELECT $$secret$$, $tag$x$tag$ FROM t"),
            "select ? from t"
        );
    }

    #[test]
    fn the_fingerprint_names_verb_and_table() {
        let f = fingerprint("SELECT i.id FROM public.\"item\" i JOIN users u ON u.id = i.uid");
        assert!(f.starts_with("select:item:"), "{f}");
        assert!(
            fingerprint("INSERT INTO item_revision (id) VALUES ($1)")
                .starts_with("insert:item_revision:")
        );
        assert!(fingerprint("UPDATE item SET title = $1").starts_with("update:item:"));
        assert!(fingerprint("SELECT 1").starts_with("select:-:"));
        assert_eq!(fingerprint("SELECT 1").len(), "select:-:".len() + 8);
    }
}
