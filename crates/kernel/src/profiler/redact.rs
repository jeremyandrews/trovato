//! SQL literal redaction for request profiles.

/// `sql` with every literal value replaced by `?` and whitespace collapsed.
///
/// A literal is a quoted string (`'…'`, `E'…'`, a dollar-quoted `$tag$…$tag$`)
/// or a number standing on its own. Identifiers, keywords, `"quoted"` names
/// and bind parameters (`$1`) are kept, so the statement still reads as the
/// query it was. The kernel binds its values rather than writing them into
/// statement text, so most statements lose nothing; this is for the ones
/// that do not, and for a plugin's.
pub fn redact_sql(sql: &str) -> String {
    let chars: Vec<char> = sql.chars().collect();
    let mut out = String::with_capacity(sql.len());
    let mut i = 0;
    while let Some(&c) = chars.get(i) {
        let prev = i.checked_sub(1).and_then(|p| chars.get(p)).copied();
        let ident_before = prev.is_some_and(|p| p.is_alphanumeric() || p == '_');
        match c {
            '\'' => {
                i = skip_quoted(&chars, i + 1);
                // `E'…'`: the `E` is part of the literal.
                if prev.is_some_and(|p| p == 'E' || p == 'e') && ends_with_lone_e(&out) {
                    out.pop();
                }
                out.push('?');
            }
            '"' => {
                let end = chars
                    .iter()
                    .skip(i + 1)
                    .position(|&q| q == '"')
                    .map_or(chars.len(), |p| i + 1 + p + 1);
                out.extend(chars.get(i..end).unwrap_or_default());
                i = end;
            }
            '$' if !ident_before => {
                let digits = count_while(&chars, i + 1, |d| d.is_ascii_digit());
                if digits > 0 {
                    // A bind parameter.
                    out.extend(chars.get(i..i + 1 + digits).unwrap_or_default());
                    i += 1 + digits;
                } else if let Some(end) = dollar_quoted_end(&chars, i) {
                    out.push('?');
                    i = end;
                } else {
                    out.push(c);
                    i += 1;
                }
            }
            d if d.is_ascii_digit() && !ident_before => {
                i += count_while(&chars, i, |n| n.is_ascii_digit() || n == '.');
                // An exponent: `1e10`, `2.5E-3`.
                if chars.get(i).is_some_and(|e| *e == 'e' || *e == 'E') {
                    let sign =
                        usize::from(chars.get(i + 1).is_some_and(|s| *s == '-' || *s == '+'));
                    let exp = count_while(&chars, i + 1 + sign, |n| n.is_ascii_digit());
                    if exp > 0 {
                        i += 1 + sign + exp;
                    }
                }
                out.push('?');
            }
            w if w.is_whitespace() => {
                if !out.is_empty() && !out.ends_with(' ') {
                    out.push(' ');
                }
                i += 1;
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    out.trim_end().to_string()
}

/// The index just past the `'` closing a string whose body starts at `i`; a
/// doubled `''` is a quote inside it.
fn skip_quoted(chars: &[char], mut i: usize) -> usize {
    while let Some(&c) = chars.get(i) {
        if c == '\'' {
            if chars.get(i + 1) == Some(&'\'') {
                i += 2;
                continue;
            }
            return i + 1;
        }
        i += 1;
    }
    i
}

/// Whether `out` ends in an `E` that is a word of its own (the `E` of
/// `E'…'`, not the last letter of an identifier).
fn ends_with_lone_e(out: &str) -> bool {
    let mut rev = out.chars().rev();
    matches!(rev.next(), Some('E' | 'e'))
        && rev
            .next()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '_'))
}

/// How many chars from `i` satisfy `pred`.
fn count_while(chars: &[char], i: usize, pred: impl Fn(char) -> bool) -> usize {
    chars.iter().skip(i).take_while(|c| pred(**c)).count()
}

/// The index just past a dollar-quoted string opening at `i` (`$$…$$` or
/// `$tag$…$tag$`), or `None` if there is none.
fn dollar_quoted_end(chars: &[char], i: usize) -> Option<usize> {
    let tag_len = count_while(chars, i + 1, |c| c.is_alphanumeric() || c == '_');
    if chars.get(i + 1 + tag_len) != Some(&'$') {
        return None;
    }
    let tag: Vec<char> = chars.get(i..i + 2 + tag_len)?.to_vec();
    let body = i + tag.len();
    (body..=chars.len().saturating_sub(tag.len()))
        .find(|&j| chars.get(j..j + tag.len()) == Some(tag.as_slice()))
        .map(|j| j + tag.len())
        .or(Some(chars.len()))
}

#[cfg(test)]
mod tests {
    use super::redact_sql;

    #[test]
    fn literals_become_placeholders() {
        assert_eq!(
            redact_sql("SELECT 'secret', 42, 3.14, 1e10 FROM t WHERE a = 'it''s'"),
            "SELECT ?, ?, ?, ? FROM t WHERE a = ?"
        );
        assert_eq!(
            redact_sql("SELECT E'x\\ny', $$body$$, $q$z$q$"),
            "SELECT ?, ?, ?"
        );
    }

    #[test]
    fn identifiers_binds_and_names_are_kept() {
        assert_eq!(
            redact_sql("SELECT \"col 1\", t2.v FROM item_v2 WHERE id = $1 AND x = $12"),
            "SELECT \"col 1\", t2.v FROM item_v2 WHERE id = $1 AND x = $12"
        );
    }

    #[test]
    fn whitespace_is_collapsed() {
        assert_eq!(redact_sql("\n\n  SELECT\n   1\n"), "SELECT ?");
    }

    #[test]
    fn an_unterminated_literal_is_still_redacted() {
        assert_eq!(redact_sql("SELECT 'never closed"), "SELECT ?");
    }
}
