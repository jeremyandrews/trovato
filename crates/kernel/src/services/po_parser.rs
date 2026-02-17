//! Gettext .po file parser.
//!
//! Parses .po format files into (source, translation, context) tuples
//! for bulk import into the locale_string table.

/// A parsed .po entry.
#[derive(Debug, Clone)]
pub struct PoEntry {
    pub source: String,
    pub translation: String,
    pub context: String,
}

/// Parse a .po file contents into entries.
///
/// Handles multiline strings, msgctxt, msgid, and msgstr directives.
pub fn parse_po(content: &str) -> Vec<PoEntry> {
    let mut entries = Vec::new();
    let mut context = String::new();
    let mut msgid = String::new();
    let mut msgstr = String::new();
    let mut current_field: Option<&str> = None;

    for line in content.lines() {
        let line = line.trim();

        // Skip comments
        if line.starts_with('#') {
            continue;
        }

        // Empty line marks end of entry
        if line.is_empty() {
            if !msgid.is_empty() && !msgstr.is_empty() {
                entries.push(PoEntry {
                    source: msgid.clone(),
                    translation: msgstr.clone(),
                    context: context.clone(),
                });
            }
            context.clear();
            msgid.clear();
            msgstr.clear();
            current_field = None;
            continue;
        }

        if let Some(rest) = line.strip_prefix("msgctxt ") {
            context = unquote(rest);
            current_field = Some("msgctxt");
        } else if let Some(rest) = line.strip_prefix("msgid ") {
            msgid = unquote(rest);
            current_field = Some("msgid");
        } else if let Some(rest) = line.strip_prefix("msgstr ") {
            msgstr = unquote(rest);
            current_field = Some("msgstr");
        } else if line.starts_with('"') {
            // Continuation line
            let continued = unquote(line);
            match current_field {
                Some("msgctxt") => context.push_str(&continued),
                Some("msgid") => msgid.push_str(&continued),
                Some("msgstr") => msgstr.push_str(&continued),
                _ => {}
            }
        }
    }

    // Handle last entry (file may not end with empty line)
    if !msgid.is_empty() && !msgstr.is_empty() {
        entries.push(PoEntry {
            source: msgid,
            translation: msgstr,
            context,
        });
    }

    entries
}

/// Remove surrounding quotes and unescape basic sequences.
///
/// Uses a single-pass character-by-character parser to avoid ordering bugs
/// where chained `.replace()` calls could double-unescape sequences like `\\n`.
fn unquote(s: &str) -> String {
    let s = s.trim();
    let s = s.strip_prefix('"').unwrap_or(s);
    let s = s.strip_suffix('"').unwrap_or(s);

    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => result.push('\n'),
                Some('t') => result.push('\t'),
                Some('"') => result.push('"'),
                Some('\\') => result.push('\\'),
                Some(other) => {
                    result.push('\\');
                    result.push(other);
                }
                None => result.push('\\'),
            }
        } else {
            result.push(c);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_po() {
        let content = r#"
msgid "Hello"
msgstr "Bonjour"

msgid "Goodbye"
msgstr "Au revoir"
"#;
        let entries = parse_po(content);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].source, "Hello");
        assert_eq!(entries[0].translation, "Bonjour");
        assert_eq!(entries[1].source, "Goodbye");
        assert_eq!(entries[1].translation, "Au revoir");
    }

    #[test]
    fn parse_with_context() {
        let content = r#"
msgctxt "menu"
msgid "File"
msgstr "Fichier"
"#;
        let entries = parse_po(content);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].context, "menu");
        assert_eq!(entries[0].source, "File");
    }

    #[test]
    fn parse_multiline() {
        let content = r#"
msgid ""
"Hello "
"World"
msgstr ""
"Bonjour "
"Monde"
"#;
        let entries = parse_po(content);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].source, "Hello World");
        assert_eq!(entries[0].translation, "Bonjour Monde");
    }

    #[test]
    fn skip_empty_msgstr() {
        let content = r#"
msgid "Untranslated"
msgstr ""

msgid "Translated"
msgstr "Traduit"
"#;
        let entries = parse_po(content);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].source, "Translated");
    }

    #[test]
    fn unescape_sequences() {
        let content = r#"
msgid "Line 1\nLine 2"
msgstr "Ligne 1\nLigne 2"
"#;
        let entries = parse_po(content);
        assert_eq!(entries[0].source, "Line 1\nLine 2");
        assert_eq!(entries[0].translation, "Ligne 1\nLigne 2");
    }

    #[test]
    fn unescape_double_backslash_then_n() {
        // "\\n" in a .po file means literal backslash + n, not a newline.
        // The single-pass parser handles this correctly.
        let result = unquote(r#""line\\nend""#);
        assert_eq!(result, "line\\nend");

        // Contrast with a simple \n which is a newline
        let result2 = unquote(r#""line\nend""#);
        assert_eq!(result2, "line\nend");

        // Double backslash alone
        let result3 = unquote(r#""path\\to\\file""#);
        assert_eq!(result3, "path\\to\\file");
    }

    #[test]
    fn skip_comments() {
        let content = r#"
# This is a comment
#. Translator comment
#: src/main.rs:42
msgid "Hello"
msgstr "Bonjour"
"#;
        let entries = parse_po(content);
        assert_eq!(entries.len(), 1);
    }
}
