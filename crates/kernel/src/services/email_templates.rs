//! Email template rendering.
//!
//! Renders Tera templates from `templates/email/` with site-specific context.
//! Supports HTML + plain text multipart emails. Falls back to plain text
//! if the HTML template is missing.

use anyhow::{Context, Result};

/// Render an email template to HTML and plain text.
///
/// Returns `(html_body, text_body)`. If the HTML template
/// doesn't exist, `html_body` is `None`.
pub fn render(
    tera: &tera::Tera,
    template_name: &str,
    context: &tera::Context,
) -> Result<(Option<String>, String)> {
    // Render plain text (required)
    let txt_template = format!("email/{template_name}.txt");
    let text = tera
        .render(&txt_template, context)
        .with_context(|| format!("failed to render email template {txt_template}"))?;

    // Render HTML (optional)
    let html_template = format!("email/{template_name}.html");
    let html = tera.render(&html_template, context).ok();

    Ok((html, text))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn test_tera() -> tera::Tera {
        // CARGO_MANIFEST_DIR points to crates/kernel/; templates live at
        // the workspace root's templates/ directory.
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let root = manifest.join("../../templates/**/*");
        let pattern = root.to_str().expect("template glob path");
        tera::Tera::new(pattern).expect("failed to load test templates")
    }

    #[test]
    fn render_registration_verify_text() {
        let tera = test_tera();
        let mut ctx = tera::Context::new();
        ctx.insert("site_name", "Test Site");
        ctx.insert("action_url", "https://example.com/verify/abc123");
        ctx.insert("subject", "Verify");

        let (html, text) = render(&tera, "registration_verify", &ctx).unwrap();
        assert!(text.contains("Test Site"));
        assert!(text.contains("https://example.com/verify/abc123"));
        assert!(html.is_some());
        assert!(html.unwrap().contains("Verify Email"));
    }

    #[test]
    fn render_password_reset_text() {
        let tera = test_tera();
        let mut ctx = tera::Context::new();
        ctx.insert("site_name", "Test Site");
        ctx.insert("action_url", "https://example.com/reset/xyz");
        ctx.insert("subject", "Reset");

        let (html, text) = render(&tera, "password_reset", &ctx).unwrap();
        assert!(text.contains("password reset"));
        assert!(text.contains("https://example.com/reset/xyz"));
        assert!(html.is_some());
    }

    #[test]
    fn render_comment_notification() {
        let tera = test_tera();
        let mut ctx = tera::Context::new();
        ctx.insert("site_name", "Test Site");
        ctx.insert("commenter_name", "Alice");
        ctx.insert("content_title", "My Post");
        ctx.insert("comment_text", "Great article!");
        ctx.insert("action_url", "https://example.com/item/123");
        ctx.insert("subject", "New Comment");

        let (html, text) = render(&tera, "comment_notification", &ctx).unwrap();
        assert!(text.contains("Alice"));
        assert!(text.contains("Great article!"));
        assert!(html.is_some());
    }

    #[test]
    fn render_missing_template_fails() {
        let tera = test_tera();
        let ctx = tera::Context::new();
        let result = render(&tera, "nonexistent_template_xyz", &ctx);
        assert!(result.is_err());
    }

    #[test]
    fn render_admin_new_user() {
        let tera = test_tera();
        let mut ctx = tera::Context::new();
        ctx.insert("site_name", "Test Site");
        ctx.insert("username", "newuser");
        ctx.insert("user_email", "new@example.com");
        ctx.insert("action_url", "https://example.com/admin/users");
        ctx.insert("subject", "New Registration");

        let (html, text) = render(&tera, "admin_new_user", &ctx).unwrap();
        assert!(text.contains("newuser"));
        assert!(text.contains("new@example.com"));
        assert!(html.is_some());
    }
}
