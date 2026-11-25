//! Interface translation: the `t` function every template can call, whether
//! or not the `trovato_locale` plugin is enabled.
//!
//! Before this existed the only way to translate a template string was the
//! `trans` filter, which [`crate::theme::ThemeEngine`] registered only when
//! `trovato_locale` was enabled, so a template that used it failed to render
//! on every site without the plugin, and no template did. Every admin string
//! was hard coded English.
//!
//! `t` is registered on every engine. With no translation for the string, or
//! no locale service at all, it returns the source string with its
//! placeholders filled, so a template can use it unconditionally.
//!
//! ```text
//! {{ t(s="Save configuration") }}
//! {{ t(s="Delete the {label} stage?", label=stage.label) }}
//! {{ t(s="{count} item", plural="{count} items", count=total) }}
//! {{ t(s="Default", ctx="text format") }}
//! ```
//!
//! - **Placeholders** are named, `{name}`, and filled from the call's other
//!   arguments. A sentence is always one string with placeholders, never
//!   fragments joined together, because word order differs between languages.
//! - **Plurals** take `plural` and `count`. The source language is English,
//!   so the source has two forms; a translation has one per CLDR plural
//!   category of its language ([`plural_category`]), stored as described on
//!   [`plural_source_key`]. `count` is also the `{count}` placeholder.
//! - **Context** (`ctx`) separates the same English word used for different
//!   things ("Default" stage, "Default" format), as gettext's `msgctxt` does.
//!
//! The language is the one the response is rendered in, set for the
//! duration of a render by [`scope`]: a Tera function cannot see the template
//! context, so it cannot read `active_language` itself.
//!
//! # Pseudo locales
//!
//! With `TROVATO_PSEUDO_LOCALES=1`, two extra languages can be chosen as the
//! admin language: [`PSEUDO_ACCENTED`] (every letter accented and the string
//! about 30% longer, in brackets, to find clipped and hard coded text) and
//! [`PSEUDO_RTL`] (the same, right to left, to prove the layout mirrors). They
//! need no translation files.
//!
//! # Marker mode
//!
//! With `TROVATO_I18N_MARKERS=1` (tests only), every string `t` returns is
//! wrapped in [`MARK_T_OPEN`]…[`MARK_T_CLOSE`], and every value a template
//! prints (Tera's autoescape, see [`marker_escape`]) and every placeholder
//! value in [`MARK_DATA_OPEN`]…[`MARK_DATA_CLOSE`]. A rendered page in which
//! any visible text is outside both is text that did not pass through `t`:
//! [`untranslated_text`] finds it. The markers are private use characters, so
//! they cannot occur in real content.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use crate::services::locale::LocaleService;

/// The accented, 30% longer pseudo locale (left to right).
pub const PSEUDO_ACCENTED: &str = "en-XA";
/// The right to left pseudo locale.
pub const PSEUDO_RTL: &str = "ar-XB";

/// Opens a string that passed through `t` (marker mode only).
pub const MARK_T_OPEN: char = '\u{E000}';
/// Closes a string that passed through `t`.
pub const MARK_T_CLOSE: char = '\u{E001}';
/// Opens a printed value: data, not interface text.
pub const MARK_DATA_OPEN: char = '\u{E002}';
/// Closes a printed value.
pub const MARK_DATA_CLOSE: char = '\u{E003}';

/// Whether marker mode is on: `TROVATO_I18N_MARKERS=1`, read once.
pub fn markers_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| env_flag("TROVATO_I18N_MARKERS"))
}

/// Whether the pseudo locales may be chosen: `TROVATO_PSEUDO_LOCALES=1`.
pub fn pseudo_locales_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| env_flag("TROVATO_PSEUDO_LOCALES"))
}

fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|v| matches!(v.trim(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

/// Whether `lang` is one of the pseudo locales.
pub fn is_pseudo(lang: &str) -> bool {
    lang == PSEUDO_ACCENTED || lang == PSEUDO_RTL
}

thread_local! {
    static RENDER_LANG: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Run `f` (a template render) with `lang` as the language `t` translates
/// into. Rendering is synchronous, so a thread local set around it is seen by
/// every `t` call the render makes and by no other render.
pub fn scope<R>(lang: &str, f: impl FnOnce() -> R) -> R {
    struct Reset(Option<String>);
    impl Drop for Reset {
        fn drop(&mut self) {
            let previous = self.0.take();
            RENDER_LANG.with(|l| *l.borrow_mut() = previous);
        }
    }
    let previous = RENDER_LANG.with(|l| l.borrow_mut().replace(lang.to_string()));
    let _reset = Reset(previous);
    f()
}

/// The language of the render in progress, if [`scope`] set one.
pub fn current_language() -> Option<String> {
    RENDER_LANG.with(|l| l.borrow().clone())
}

/// What a `t` call asks for.
#[derive(Debug, Default, Clone)]
pub struct Message<'a> {
    /// The English source, with `{name}` placeholders.
    pub source: &'a str,
    /// The English plural form, when the string has one.
    pub plural: Option<&'a str>,
    /// The number that picks the plural form.
    pub count: Option<i64>,
    /// Disambiguating context, `""` for none.
    pub context: &'a str,
}

impl<'a> Message<'a> {
    pub fn new(source: &'a str) -> Self {
        Self {
            source,
            ..Self::default()
        }
    }
}

/// What joins a plural string's forms in the catalogue: the ASCII unit
/// separator. Not the NUL a compiled gettext catalogue uses, because
/// PostgreSQL `text` cannot hold a NUL and the forms live in
/// `locale_string`.
pub const PLURAL_SEPARATOR: char = '\u{1F}';

/// The key a plural string is stored under in `locale_string.source`: the
/// singular and the plural joined by [`PLURAL_SEPARATOR`]. Its `translation`
/// is the language's forms, one per CLDR category the language uses, in the
/// order zero, one, two, few, many, other, joined the same way.
pub fn plural_source_key(singular: &str, plural: &str) -> String {
    format!("{singular}{PLURAL_SEPARATOR}{plural}")
}

/// Translate `message` into `lang` and fill its placeholders from `args`.
///
/// With no translation, the source (or its plural, by English rules) is
/// used. Pseudo locales are generated rather than looked up. In marker mode
/// the result is wrapped in the translation markers and each filled value in
/// the data markers.
pub fn translate(
    locale: Option<&LocaleService>,
    lang: &str,
    message: &Message<'_>,
    args: &[(String, String)],
) -> String {
    let count = message.count;
    let source_form = match (message.plural, count) {
        (Some(plural), Some(n)) if plural_category("en", n) != PluralCategory::One => plural,
        _ => message.source,
    };

    let translated: Option<String> = if is_pseudo(lang) {
        None
    } else {
        locale.and_then(|service| match message.plural {
            Some(plural) => {
                let key = plural_source_key(message.source, plural);
                let forms = service.lookup_translation(&key, message.context, lang)?;
                let forms: Vec<&str> = forms.split(PLURAL_SEPARATOR).collect();
                let categories = plural_categories(lang);
                let wanted = plural_category(lang, count.unwrap_or(0));
                categories
                    .iter()
                    .position(|c| *c == wanted)
                    .and_then(|i| forms.get(i))
                    .or_else(|| forms.last())
                    .map(|s| (*s).to_string())
            }
            None => service.lookup_translation(message.source, message.context, lang),
        })
    };

    let template = translated.unwrap_or_else(|| source_form.to_string());
    let template = if is_pseudo(lang) {
        pseudo(&template, lang == PSEUDO_RTL)
    } else {
        template
    };

    let mut values: Vec<(String, String)> = args.to_vec();
    if let Some(n) = count
        && !values.iter().any(|(k, _)| k == "count")
    {
        values.push(("count".to_string(), n.to_string()));
    }
    let filled = fill(&template, &values, markers_enabled());

    if markers_enabled() {
        format!("{MARK_T_OPEN}{filled}{MARK_T_CLOSE}")
    } else {
        filled
    }
}

/// Replace each `{name}` in `template` with its value; a name with no value
/// is left as written, so a missing argument shows rather than vanishes.
fn fill(template: &str, values: &[(String, String)], mark: bool) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let close = after.find('}');
        let name = close.map(|c| &after[..c]);
        match name.filter(|n| is_placeholder_name(n)) {
            Some(name) => {
                match values.iter().find(|(k, _)| k == name) {
                    Some((_, value)) if mark => {
                        out.push(MARK_DATA_OPEN);
                        out.push_str(value);
                        out.push(MARK_DATA_CLOSE);
                    }
                    Some((_, value)) => out.push_str(value),
                    None => {
                        out.push('{');
                        out.push_str(name);
                        out.push('}');
                    }
                }
                rest = &after[name.len() + 1..];
            }
            None => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn is_placeholder_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// The pseudo translation of `text`: every ASCII letter outside a
/// placeholder replaced by an accented one, about 30% added, in brackets.
/// The right to left one is wrapped in right-to-left marks; the page's
/// `dir="rtl"` does the rest.
fn pseudo(text: &str, rtl: bool) -> String {
    let mut out = String::with_capacity(text.len() * 2);
    let mut in_placeholder = false;
    let mut letters = 0usize;
    for c in text.chars() {
        match c {
            '{' => {
                in_placeholder = true;
                out.push(c);
            }
            '}' => {
                in_placeholder = false;
                out.push(c);
            }
            _ if in_placeholder => out.push(c),
            _ => {
                if c.is_alphabetic() {
                    letters += 1;
                }
                out.push(accent(c));
            }
        }
    }
    let padding = (letters * 3).div_ceil(10);
    if padding > 0 {
        out.push(' ');
        out.extend(std::iter::repeat_n('·', padding));
    }
    if rtl {
        format!("\u{200F}[{out}]\u{200F}")
    } else {
        format!("[{out}]")
    }
}

fn accent(c: char) -> char {
    match c {
        'a' => 'à',
        'b' => 'ƀ',
        'c' => 'ç',
        'd' => 'ð',
        'e' => 'é',
        'f' => 'ƒ',
        'g' => 'ĝ',
        'h' => 'ĥ',
        'i' => 'î',
        'j' => 'ĵ',
        'k' => 'ķ',
        'l' => 'ļ',
        'm' => 'ɱ',
        'n' => 'ñ',
        'o' => 'ö',
        'p' => 'þ',
        'q' => 'ǫ',
        'r' => 'ŕ',
        's' => 'š',
        't' => 'ţ',
        'u' => 'û',
        'v' => 'ṽ',
        'w' => 'ŵ',
        'x' => 'ẋ',
        'y' => 'ý',
        'z' => 'ž',
        'A' => 'Å',
        'B' => 'Ɓ',
        'C' => 'Ç',
        'D' => 'Ð',
        'E' => 'É',
        'F' => 'Ƒ',
        'G' => 'Ĝ',
        'H' => 'Ĥ',
        'I' => 'Î',
        'J' => 'Ĵ',
        'K' => 'Ķ',
        'L' => 'Ļ',
        'M' => 'Ṁ',
        'N' => 'Ñ',
        'O' => 'Ö',
        'P' => 'Þ',
        'Q' => 'Ǫ',
        'R' => 'Ŕ',
        'S' => 'Š',
        'T' => 'Ţ',
        'U' => 'Û',
        'V' => 'Ṽ',
        'W' => 'Ŵ',
        'X' => 'Ẋ',
        'Y' => 'Ý',
        'Z' => 'Ž',
        other => other,
    }
}

/// A CLDR plural category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluralCategory {
    Zero,
    One,
    Two,
    Few,
    Many,
    Other,
}

use PluralCategory::{Few, Many, One, Other, Two, Zero};

/// The categories `lang` uses for whole numbers, in storage order.
pub fn plural_categories(lang: &str) -> &'static [PluralCategory] {
    match primary(lang) {
        "ja" | "zh" | "ko" | "th" | "vi" | "id" | "ms" | "lo" | "my" | "km" => &[Other],
        "fr" | "pt" | "es" | "it" | "ca" => &[One, Many, Other],
        "ru" | "uk" | "be" | "pl" | "lt" => &[One, Few, Many, Other],
        "cs" | "sk" | "hr" | "sr" | "bs" | "ro" => &[One, Few, Other],
        "ar" => &[Zero, One, Two, Few, Many, Other],
        "he" => &[One, Two, Other],
        "lv" => &[Zero, One, Other],
        "sl" => &[One, Two, Few, Other],
        "ga" => &[One, Two, Few, Many, Other],
        "cy" => &[Zero, One, Two, Few, Many, Other],
        _ => &[One, Other],
    }
}

fn primary(lang: &str) -> &str {
    lang.split(['-', '_']).next().unwrap_or(lang)
}

/// The CLDR cardinal plural category of the whole number `n` in `lang`.
///
/// Whole numbers only: the admin counts things. The rules are CLDR 44's
/// integer cases for the languages listed in [`plural_categories`]; every
/// other language is treated as English (one for 1, other otherwise).
pub fn plural_category(lang: &str, n: i64) -> PluralCategory {
    let i = n.unsigned_abs();
    let m10 = i % 10;
    let m100 = i % 100;
    let millions = i != 0 && i.is_multiple_of(1_000_000);
    match primary(lang) {
        "ja" | "zh" | "ko" | "th" | "vi" | "id" | "ms" | "lo" | "my" | "km" => Other,
        "fr" | "pt" => match i {
            0 | 1 => One,
            _ if millions => Many,
            _ => Other,
        },
        "es" | "it" | "ca" => match i {
            1 => One,
            _ if millions => Many,
            _ => Other,
        },
        "ru" | "uk" | "be" => {
            if m10 == 1 && m100 != 11 {
                One
            } else if (2..=4).contains(&m10) && !(12..=14).contains(&m100) {
                Few
            } else {
                Many
            }
        }
        "pl" => {
            if i == 1 {
                One
            } else if (2..=4).contains(&m10) && !(12..=14).contains(&m100) {
                Few
            } else {
                Many
            }
        }
        "lt" => {
            if m10 == 1 && !(11..=19).contains(&m100) {
                One
            } else if (2..=9).contains(&m10) && !(11..=19).contains(&m100) {
                Few
            } else {
                Other
            }
        }
        "cs" | "sk" => match i {
            1 => One,
            2..=4 => Few,
            _ => Other,
        },
        "hr" | "sr" | "bs" => {
            if m10 == 1 && m100 != 11 {
                One
            } else if (2..=4).contains(&m10) && !(12..=14).contains(&m100) {
                Few
            } else {
                Other
            }
        }
        "ro" => {
            if i == 1 {
                One
            } else if i == 0 || (2..=19).contains(&m100) {
                Few
            } else {
                Other
            }
        }
        "ar" => match i {
            0 => Zero,
            1 => One,
            2 => Two,
            _ if (3..=10).contains(&m100) => Few,
            _ if (11..=99).contains(&m100) => Many,
            _ => Other,
        },
        "he" => match i {
            1 => One,
            2 => Two,
            _ => Other,
        },
        "lv" => {
            if m10 == 0 || (11..=19).contains(&m100) {
                Zero
            } else if m10 == 1 && m100 != 11 {
                One
            } else {
                Other
            }
        }
        "sl" => match m100 {
            1 => One,
            2 => Two,
            3 | 4 => Few,
            _ => Other,
        },
        "ga" => match i {
            1 => One,
            2 => Two,
            3..=6 => Few,
            7..=10 => Many,
            _ => Other,
        },
        "cy" => match i {
            0 => Zero,
            1 => One,
            2 => Two,
            3 => Few,
            6 => Many,
            _ => Other,
        },
        _ => {
            if i == 1 {
                One
            } else {
                Other
            }
        }
    }
}

/// The `t` Tera function, holding the locale service when there is one.
pub struct TranslateFunction {
    locale: Option<Arc<LocaleService>>,
}

impl TranslateFunction {
    pub fn new(locale: Option<Arc<LocaleService>>) -> Self {
        Self { locale }
    }
}

/// The arguments `t` reads itself; every other one is a placeholder.
const RESERVED_ARGS: &[&str] = &["s", "plural", "count", "ctx", "lang"];

impl tera::Function for TranslateFunction {
    fn call(&self, args: &HashMap<String, tera::Value>) -> tera::Result<tera::Value> {
        let source = match args.get("s") {
            Some(tera::Value::String(s)) => s.as_str(),
            Some(other) => {
                return Err(tera::Error::msg(format!(
                    "t: `s` must be a string, got {other}"
                )));
            }
            None => return Err(tera::Error::msg("t: the source string `s` is required")),
        };
        let plural = args.get("plural").and_then(|v| v.as_str());
        let count = args.get("count").and_then(|v| {
            v.as_i64()
                .or_else(|| v.as_u64().map(|u| i64::try_from(u).unwrap_or(i64::MAX)))
                .or_else(|| v.as_f64().map(|f| f as i64))
                .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
        });
        let context = args.get("ctx").and_then(|v| v.as_str()).unwrap_or("");
        let lang = args
            .get("lang")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(current_language)
            .unwrap_or_else(|| "en".to_string());

        let mut values: Vec<(String, String)> = args
            .iter()
            .filter(|(k, _)| !RESERVED_ARGS.contains(&k.as_str()))
            .map(|(k, v)| {
                let text = match v {
                    tera::Value::String(s) => s.clone(),
                    tera::Value::Null => String::new(),
                    other => other.to_string(),
                };
                (k.clone(), text)
            })
            .collect();
        values.sort();

        let message = Message {
            source,
            plural,
            count,
            context,
        };
        Ok(tera::Value::String(translate(
            self.locale.as_deref(),
            &lang,
            &message,
            &values,
        )))
    }

    fn is_safe(&self) -> bool {
        false
    }
}

/// Tera's HTML escape, with every printed value wrapped in the data markers.
/// Installed as the escape function in marker mode only.
pub fn marker_escape(input: &str) -> String {
    let escaped = tera::escape_html(input);
    if escaped.is_empty() {
        return escaped;
    }
    format!("{MARK_DATA_OPEN}{escaped}{MARK_DATA_CLOSE}")
}

/// Remove every marked segment from `text`, innermost first, and return
/// what is left if it contains a letter: text that reached the page without
/// passing through `t` or being printed from data. `None` when the text is
/// clean.
pub fn untranslated_text(text: &str) -> Option<String> {
    let mut current: Vec<char> = text.chars().collect();
    loop {
        let mut changed = false;
        let mut out: Vec<char> = Vec::with_capacity(current.len());
        let mut i = 0;
        while i < current.len() {
            let c = current[i];
            if c == MARK_T_OPEN || c == MARK_DATA_OPEN {
                let close = if c == MARK_T_OPEN {
                    MARK_T_CLOSE
                } else {
                    MARK_DATA_CLOSE
                };
                // Innermost: no other opener before the matching closer.
                let end = current[i + 1..]
                    .iter()
                    .position(|&d| d == close || d == MARK_T_OPEN || d == MARK_DATA_OPEN)
                    .map(|p| p + i + 1);
                if let Some(end) = end
                    && current[end] == close
                {
                    i = end + 1;
                    changed = true;
                    continue;
                }
            }
            out.push(c);
            i += 1;
        }
        current = out;
        if !changed {
            break;
        }
    }
    let left: String = current
        .into_iter()
        .filter(|c| {
            !matches!(
                *c,
                MARK_T_OPEN | MARK_T_CLOSE | MARK_DATA_OPEN | MARK_DATA_CLOSE
            )
        })
        .collect();
    left.chars()
        .any(char::is_alphabetic)
        .then(|| left.trim().to_string())
}

/// Strip every marker from `text`, for code that has to read the plain
/// string back out of a marked render (the shell's title copy).
pub fn strip_markers(text: &str) -> String {
    text.chars()
        .filter(|c| {
            !matches!(
                *c,
                MARK_T_OPEN | MARK_T_CLOSE | MARK_DATA_OPEN | MARK_DATA_CLOSE
            )
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn args(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn with_no_locale_the_source_is_returned_with_its_placeholders_filled() {
        let out = translate(
            None,
            "de",
            &Message::new("Delete the {label} stage?"),
            &args(&[("label", "Review")]),
        );
        assert_eq!(out, "Delete the Review stage?");
    }

    #[test]
    fn an_unknown_placeholder_is_left_as_written_and_braces_survive() {
        let out = translate(
            None,
            "en",
            &Message::new("{a} and {b} {not a name}"),
            &args(&[("a", "x")]),
        );
        assert_eq!(out, "x and {b} {not a name}");
    }

    #[test]
    fn english_plural_forms_follow_count() {
        let m = |n| Message {
            source: "{count} item",
            plural: Some("{count} items"),
            count: Some(n),
            context: "",
        };
        assert_eq!(translate(None, "en", &m(1), &[]), "1 item");
        assert_eq!(translate(None, "en", &m(0), &[]), "0 items");
        assert_eq!(translate(None, "en", &m(21), &[]), "21 items");
    }

    #[test]
    fn cldr_categories_for_representative_languages() {
        let cases: &[(&str, i64, PluralCategory)] = &[
            ("en", 1, One),
            ("en", 2, Other),
            ("fr", 0, One),
            ("fr", 1, One),
            ("fr", 2, Other),
            ("fr", 1_000_000, Many),
            ("ru", 1, One),
            ("ru", 21, One),
            ("ru", 11, Many),
            ("ru", 3, Few),
            ("ru", 13, Many),
            ("ru", 5, Many),
            ("pl", 1, One),
            ("pl", 22, Few),
            ("pl", 21, Many),
            ("ar", 0, Zero),
            ("ar", 2, Two),
            ("ar", 7, Few),
            ("ar", 15, Many),
            ("ar", 100, Other),
            ("ar-EG", 103, Few),
            ("ja", 1, Other),
            ("he", 2, Two),
            ("cy", 6, Many),
            ("lv", 10, Zero),
            ("sl", 102, Two),
        ];
        for (lang, n, want) in cases {
            assert_eq!(plural_category(lang, *n), *want, "{lang} {n}");
            assert!(
                plural_categories(lang).contains(want),
                "{lang} lists {want:?}"
            );
        }
    }

    #[test]
    fn the_accented_pseudo_locale_is_longer_bracketed_and_keeps_placeholders() {
        let out = translate(
            None,
            PSEUDO_ACCENTED,
            &Message::new("Edit {name}"),
            &args(&[("name", "About us")]),
        );
        assert!(out.starts_with("[Éðîţ About us"), "{out}");
        assert!(out.ends_with(']'));
        let source_letters = "Edit".chars().count();
        assert!(out.matches('·').count() >= (source_letters * 3).div_ceil(10));
    }

    #[test]
    fn the_rtl_pseudo_locale_is_wrapped_in_right_to_left_marks() {
        let out = translate(None, PSEUDO_RTL, &Message::new("Save"), &[]);
        assert!(
            out.starts_with('\u{200F}') && out.ends_with('\u{200F}'),
            "{out}"
        );
        assert_eq!(
            crate::middleware::language::text_direction_for_language(PSEUDO_RTL),
            "rtl"
        );
        assert_eq!(
            crate::middleware::language::text_direction_for_language(PSEUDO_ACCENTED),
            "ltr"
        );
    }

    #[test]
    fn untranslated_text_finds_only_unmarked_letters() {
        let t = |s: &str| format!("{MARK_T_OPEN}{s}{MARK_T_CLOSE}");
        let d = |s: &str| format!("{MARK_DATA_OPEN}{s}{MARK_DATA_CLOSE}");
        assert_eq!(untranslated_text(&t("Save")), None);
        assert_eq!(untranslated_text(&d("About us")), None);
        assert_eq!(untranslated_text(&format!("{} · 3 ›", t("Edit"))), None);
        assert_eq!(
            untranslated_text(&d(&t(&format!("Edit {}", d("x"))))),
            None,
            "nested markers"
        );
        assert_eq!(
            untranslated_text(&format!("{} Hard coded", t("Edit"))),
            Some("Hard coded".to_string())
        );
    }

    #[test]
    fn the_tera_function_reads_placeholders_plural_and_context() {
        let mut tera = tera::Tera::default();
        tera.register_function("t", TranslateFunction::new(None));
        tera.add_raw_template(
            "x.html",
            r#"{{ t(s="{count} file", plural="{count} files", count=n) }}|{{ t(s="Hi {who}", who=name) }}|{{ t(s="Default", ctx="stage") }}"#,
        )
        .unwrap();
        let mut ctx = tera::Context::new();
        ctx.insert("n", &3);
        ctx.insert("name", "Ada <b>");
        let out = scope("en", || tera.render("x.html", &ctx)).unwrap();
        assert_eq!(out, "3 files|Hi Ada &lt;b&gt;|Default");
    }

    #[test]
    fn scope_restores_the_previous_language() {
        assert_eq!(current_language(), None);
        scope("fr", || {
            assert_eq!(current_language().as_deref(), Some("fr"));
            scope("de", || {
                assert_eq!(current_language().as_deref(), Some("de"));
            });
            assert_eq!(current_language().as_deref(), Some("fr"));
        });
        assert_eq!(current_language(), None);
    }
}

/// A message built in Rust and shown later (a flash message after a
/// redirect), kept as its source string and placeholder values so it is
/// translated when it is shown, in the language of the page that shows it.
///
/// Before this, flash messages were English sentences assembled with
/// `format!`, some of them HTML, which no translation could reach. Store one
/// in the session as is; the admin shell renders it with `t_message`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UiMessage {
    /// The English source, with `{name}` placeholders.
    pub s: String,
    /// The English plural, when the message counts something.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plural: Option<String>,
    /// The number that picks the plural form; also `{count}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<i64>,
    /// Placeholder values: data, never translated.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub args: std::collections::BTreeMap<String, String>,
    /// A link shown after the message: an address on this site and its
    /// (source, translatable) text, which may use the same placeholders.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<(String, String)>,
    /// `Some("error")` for a message saying something failed or nothing was
    /// done: the shell shows it as an error (an alert, "Error:"), not as the
    /// "Done:" status a plain message gets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

impl UiMessage {
    pub fn new(source: impl Into<String>) -> Self {
        Self {
            s: source.into(),
            ..Self::default()
        }
    }

    /// Set a placeholder value.
    pub fn arg(mut self, name: &str, value: impl Into<String>) -> Self {
        self.args.insert(name.to_string(), value.into());
        self
    }

    /// Make it a plural message counting `count`.
    pub fn plural(mut self, plural: impl Into<String>, count: i64) -> Self {
        self.plural = Some(plural.into());
        self.count = Some(count);
        self
    }

    /// Mark the message as a failure: shown as an error, not "Done:".
    pub fn error(mut self) -> Self {
        self.kind = Some("error".to_string());
        self
    }

    /// Add a link after the message.
    pub fn link(mut self, href: impl Into<String>, text: impl Into<String>) -> Self {
        self.link = Some((href.into(), text.into()));
        self
    }

    /// The message's text in `lang`, unescaped.
    pub fn text(&self, locale: Option<&LocaleService>, lang: &str) -> String {
        let args: Vec<(String, String)> = self.args.clone().into_iter().collect();
        translate(
            locale,
            lang,
            &Message {
                source: &self.s,
                plural: self.plural.as_deref(),
                count: self.count,
                context: "",
            },
            &args,
        )
    }

    /// The message as HTML: its escaped text, then its link if the address is
    /// a path on this site.
    pub fn html(&self, locale: Option<&LocaleService>, lang: &str) -> String {
        let mut out = tera::escape_html(&self.text(locale, lang));
        if let Some((href, text)) = &self.link
            && href.starts_with('/')
            && !href.starts_with("//")
            && !href.contains(['"', '\'', '<', '>', ' ', '\\'])
        {
            let args: Vec<(String, String)> = self.args.clone().into_iter().collect();
            let label = translate(locale, lang, &Message::new(text), &args);
            out.push_str(&format!(
                " <a href=\"{}\">{}</a>",
                tera::escape_html(href),
                tera::escape_html(&label)
            ));
        }
        out
    }
}

/// The `t_message` Tera function: `{{ t_message(m=flash) }}` renders a
/// [`UiMessage`] (or a plain source string) translated, as safe HTML.
pub struct TranslateMessageFunction {
    locale: Option<Arc<LocaleService>>,
}

impl TranslateMessageFunction {
    pub fn new(locale: Option<Arc<LocaleService>>) -> Self {
        Self { locale }
    }
}

impl tera::Function for TranslateMessageFunction {
    fn call(&self, args: &HashMap<String, tera::Value>) -> tera::Result<tera::Value> {
        let lang = current_language().unwrap_or_else(|| "en".to_string());
        let message = match args.get("m") {
            Some(tera::Value::String(s)) => UiMessage::new(s.clone()),
            Some(tera::Value::Null) | None => return Ok(tera::Value::String(String::new())),
            Some(other) => serde_json::from_value::<UiMessage>(other.clone())
                .map_err(|e| tera::Error::msg(format!("t_message: not a message: {e}")))?,
        };
        Ok(tera::Value::String(
            message.html(self.locale.as_deref(), &lang),
        ))
    }

    /// Escaped by [`UiMessage::html`] itself.
    fn is_safe(&self) -> bool {
        true
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod message_tests {
    use super::*;

    #[test]
    fn a_message_is_translated_and_escaped_with_its_link() {
        let m = UiMessage::new("{type} {title} has been created.")
            .arg("type", "Blog post")
            .arg("title", "<b>Hi</b>")
            .link("/admin/content/1/edit", "Edit {title}");
        assert_eq!(
            m.html(None, "en"),
            "Blog post &lt;b&gt;Hi&lt;&#x2F;b&gt; has been created. <a href=\"&#x2F;admin&#x2F;content&#x2F;1&#x2F;edit\">Edit &lt;b&gt;Hi&lt;&#x2F;b&gt;</a>"
        );
        let plural =
            UiMessage::new("{count} item published.").plural("{count} items published.", 2);
        assert_eq!(plural.text(None, "en"), "2 items published.");
    }

    #[test]
    fn a_link_off_the_site_is_dropped() {
        let m = UiMessage::new("Saved.").link("//evil.example/x", "Go");
        assert_eq!(m.html(None, "en"), "Saved.");
        let m = UiMessage::new("Saved.").link("javascript:alert(1)", "Go");
        assert_eq!(m.html(None, "en"), "Saved.");
    }

    #[test]
    fn a_stored_message_round_trips_through_the_session_shape() {
        let m = UiMessage::new("{count} item deleted.").plural("{count} items deleted.", 3);
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(serde_json::from_value::<UiMessage>(v).unwrap(), m);
    }
}

/// A value printed into HTML built in Rust: escaped, and in marker mode
/// wrapped as data, exactly as a template prints a value.
pub fn data_html(value: &str) -> String {
    if markers_enabled() {
        marker_escape(value)
    } else {
        tera::escape_html(value)
    }
}

/// Interface text for HTML built in Rust: translated into `lang`, then
/// escaped. Marked as translated in marker mode, as `t` marks it.
pub fn ui_html(
    locale: Option<&LocaleService>,
    lang: &str,
    source: &str,
    args: &[(&str, &str)],
) -> String {
    let args: Vec<(String, String)> = args
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    tera::escape_html(&translate(locale, lang, &Message::new(source), &args))
}
