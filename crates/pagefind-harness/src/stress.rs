//! A stress corpus, rendered through the kernel's own `render_fragment`.
//!
//! The real corpus (`corpus/`) comes from the largest content set available
//! locally, which is a long lived test database: its pages are short (the
//! largest is under 900 bytes) and nearly all ASCII. This corpus covers what that
//! one cannot. It has long text, so the whole set crosses Pagefind's 20,000
//! location chunk cut several times. It has punctuation Pagefind strips or
//! splits on (hyphens, apostrophes, dots, slashes, `&`), HTML entities,
//! non-breaking spaces, accented Latin, Cyrillic, Greek, CJK, emoji, numbers,
//! very long words, repeated words at several weights, comments, and every meta
//! field. It is deterministic: no randomness, the same pages every run.

use trovato_kernel::cron::{IndexDocument, IndexedField, render_fragment};
use uuid::Uuid;

use crate::build::SourcePage;

const WORDS: &[&str] = &[
    "conference",
    "rust",
    "kernel",
    "search",
    "index",
    "postgres",
    "plugin",
    "wasm",
    "stemming",
    "running",
    "runner",
    "ran",
    "runs",
    "café",
    "naïve",
    "résumé",
    "straße",
    "Zürich",
    "München",
    "København",
    "Ελληνικά",
    "русский",
    "текст",
    "東京",
    "日本語のテキスト",
    "中文分词",
    "한국어",
    "🦀",
    "emoji🎉party",
    "e-mail",
    "co-operate",
    "don't",
    "rock'n'roll",
    "U.S.A.",
    "v1.3.0",
    "3.14159",
    "2026-11-25",
    "AT&amp;T",
    "a/b",
    "foo_bar",
    "CamelCaseWord",
    "ALLCAPS",
    "x",
    "I",
    "a",
    "supercalifragilisticexpialidocious",
    "pneumonoultramicroscopicsilicovolcanoconiosis",
    "#hashtag",
    "@mention",
    "100%",
    "$42",
    "«quoted»",
    "„Anführung“",
    "—dash—",
    "semi;colon",
    "colon:here",
    "(paren)",
    "[bracket]",
    "{brace}",
    "question?",
    "exclaim!",
    "tab\tseparated",
    "nbsp&nbsp;joined",
    "&lt;tag&gt;",
    "&quot;q&quot;",
];

/// Deterministic text of `n` words, starting at `seed` in the word list.
fn text(seed: usize, n: usize) -> String {
    (0..n)
        .map(|i| WORDS[(seed * 7 + i * i + i * 3) % WORDS.len()])
        .collect::<Vec<_>>()
        .join(" ")
}

/// The stress documents.
pub fn documents() -> Vec<IndexDocument> {
    (0..40usize)
        .map(|i| {
            let id = Uuid::from_u128(0x0193_0000_0000_7000_8000_0000_0000_0000 + i as u128);
            let fields = [
                (10.0, "field_summary"),
                (4.0, "field_body"),
                (2.0, "field_city"),
                (1.0, "field_notes"),
            ]
            .iter()
            .enumerate()
            .filter(|(f, _)| (i + f) % 3 != 0 || *f == 1)
            .map(|(f, (weight, name))| IndexedField {
                name: (*name).to_string(),
                weight: *weight,
                text: text(
                    i + f,
                    if *name == "field_body" {
                        1200 + i * 37
                    } else {
                        20 + i
                    },
                ),
            })
            .collect();
            IndexDocument {
                id,
                item_type: if i % 2 == 0 {
                    "conference".into()
                } else {
                    "page".into()
                },
                title: format!("Stress {i}: {}", text(i, 6)),
                // Rendered with `<html lang="en">`, as the native build renders
                // every page now, so the proof covers the English stemmer too.
                language: "en".into(),
                url: if i % 4 == 0 {
                    format!("/item/{id}")
                } else {
                    format!("/stress/{i}-{}", WORDS[i % 10])
                },
                fields,
                comments: (0..(i % 5)).map(|c| text(i * 11 + c, 15 + c * 9)).collect(),
                description: if i % 3 == 0 {
                    String::new()
                } else {
                    text(i, 25)
                },
                location: if i % 2 == 0 {
                    "Zürich, Switzerland".into()
                } else {
                    String::new()
                },
                event_dates: if i % 5 == 0 {
                    "2026-11-25 – 2026-11-27".into()
                } else {
                    String::new()
                },
                created_display: format!("2026-11-{:02}", 1 + i % 28),
            }
        })
        .collect()
}

/// The stress pages, as `build_index_inner` would write them.
pub fn pages() -> Vec<SourcePage> {
    let mut pages: Vec<SourcePage> = documents()
        .iter()
        .map(|d| SourcePage {
            file_name: format!("{}.html", d.id),
            html: render_fragment(d),
        })
        .collect();
    pages.sort_by(|a, b| a.file_name.cmp(&b.file_name));
    pages
}

/// Write the stress pages into a directory, for the CLI.
pub fn write(dir: &std::path::Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir)?;
    for page in pages() {
        std::fs::write(dir.join(&page.file_name), page.html)?;
    }
    Ok(())
}
