//! Preview before saving (B158): the per-type setting.
//!
//! A content type's `preview_mode` setting says whether its item forms offer a
//! Preview button (`optional`, the default, as Drupal's), offer none
//! (`disabled`), or must be previewed before they can be saved (`required`).
//! The forms read it in `routes::item_form`; a preview renders the submission
//! as the item page would and saves nothing.

/// The type setting that holds the mode.
pub const SETTING: &str = "preview_mode";

/// The name of the button that asks for a preview rather than a save.
pub const BUTTON: &str = "preview";

/// The form state key that says the form was built for a submission that
/// was previewed, which a `required` type needs before it saves.
pub const PREVIEWED: &str = "previewed";

/// Whether, and how, a type's item forms preview.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PreviewMode {
    /// No Preview button.
    Disabled,
    /// A Preview button beside Save.
    #[default]
    Optional,
    /// A Preview button only, until the submission has been previewed.
    Required,
}

impl PreviewMode {
    /// Read a stored or submitted value; anything else is the default.
    pub fn parse(value: Option<&str>) -> Self {
        match value {
            Some("disabled") => Self::Disabled,
            Some("required") => Self::Required,
            _ => Self::Optional,
        }
    }

    /// The mode a type's settings hold.
    pub fn of(settings: Option<&serde_json::Value>) -> Self {
        Self::parse(
            settings
                .and_then(|s| s.get(SETTING))
                .and_then(serde_json::Value::as_str),
        )
    }

    /// The stored value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Optional => "optional",
            Self::Required => "required",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_or_unknown_mode_is_optional() {
        assert_eq!(PreviewMode::of(None), PreviewMode::Optional);
        assert_eq!(
            PreviewMode::of(Some(&serde_json::json!({}))),
            PreviewMode::Optional
        );
        assert_eq!(PreviewMode::parse(Some("bogus")), PreviewMode::Optional);
        for mode in [
            PreviewMode::Disabled,
            PreviewMode::Optional,
            PreviewMode::Required,
        ] {
            assert_eq!(PreviewMode::parse(Some(mode.as_str())), mode);
        }
    }
}
