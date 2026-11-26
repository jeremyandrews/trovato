//! Counts `<img>` elements with no text alternative in a fragment of HTML
//! (B251), with the same `lol_html` parser [`crate::tokenize()`] runs over a
//! page: a real HTML tokenizer, not a regular expression over markup.

use lol_html::html_content::Element;
use lol_html::{HtmlRewriter, Settings, element};

struct DiscardSink;

impl lol_html::OutputSink for DiscardSink {
    fn handle_chunk(&mut self, _: &[u8]) {}
}

/// Whether an `<img>` carrying this `alt` attribute (`None` when the
/// attribute is absent) has no text alternative.
///
/// `alt=""` is a deliberate decorative marker, set on purpose to tell a
/// screen reader to skip the image, and is not a finding. An attribute that
/// is only whitespace is not that marker, just an empty one, and is.
fn is_missing(alt: Option<&str>) -> bool {
    match alt {
        None => true,
        Some(alt) => !alt.is_empty() && alt.trim().is_empty(),
    }
}

/// Counts the `<img>` elements in `html` with no text alternative: no `alt`
/// attribute at all, or one that is only whitespace.
pub fn count_images_missing_alt(html: &str) -> usize {
    let count = std::rc::Rc::new(std::cell::Cell::new(0usize));
    let counted = std::rc::Rc::clone(&count);

    let mut rewriter = HtmlRewriter::new(
        Settings {
            element_content_handlers: vec![element!("img", move |el: &mut Element| {
                if is_missing(el.get_attribute("alt").as_deref()) {
                    counted.set(counted.get() + 1);
                }
                Ok(())
            })],
            ..Settings::default()
        },
        DiscardSink,
    );

    let _ = rewriter.write(html.as_bytes());
    drop(rewriter);

    count.get()
}

#[cfg(test)]
mod tests {
    use super::count_images_missing_alt;

    #[test]
    fn an_img_with_no_alt_attribute_is_counted() {
        assert_eq!(count_images_missing_alt(r#"<p><img src="x"></p>"#), 1);
    }

    #[test]
    fn an_img_with_an_empty_alt_is_a_deliberate_marker_and_is_not_counted() {
        assert_eq!(count_images_missing_alt(r#"<img src="x" alt="">"#), 0);
    }

    #[test]
    fn an_img_with_a_whitespace_only_alt_is_counted() {
        assert_eq!(count_images_missing_alt(r#"<img src="x" alt="   ">"#), 1);
    }

    #[test]
    fn an_img_with_real_alt_text_is_not_counted() {
        assert_eq!(count_images_missing_alt(r#"<img src="x" alt="A cat">"#), 0);
    }

    #[test]
    fn more_than_one_img_without_alt_is_summed() {
        let html = r#"<div><img src="a"><p><img src="b" alt="ok"></p><img src="c" alt="  "></div>"#;
        assert_eq!(count_images_missing_alt(html), 2);
    }

    #[test]
    fn html_with_no_images_counts_zero() {
        assert_eq!(count_images_missing_alt("<p>No images here.</p>"), 0);
    }
}
