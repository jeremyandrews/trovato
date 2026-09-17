//! Database models.

pub mod api_token;
pub mod assistant;
pub mod category;
pub mod comment;
pub mod email_verification;
pub mod item;
pub mod item_type;
pub mod language;
pub mod menu_link;
pub mod password_reset;
pub mod role;
pub mod site_config;
pub mod stage;
pub mod subscription;
pub mod tenant;
pub mod tile;
pub mod url_alias;
pub mod user;
pub mod webauthn_credential;

pub use assistant::{
    Conversation, PROPOSAL_APPLIED, PROPOSAL_DISCARDED, PROPOSAL_FAILED, PROPOSAL_PROPOSED,
    Proposal, STATUS_CLOSED, STATUS_OPEN, TranscriptEntry,
};
pub use category::{
    Category, CreateCategory, CreateTag, Tag, TagHierarchy, TagTreeNode, TagWithDepth,
    UpdateCategory, UpdateTag,
};
pub use comment::{Comment, CommentStatus, CreateComment, UpdateComment};
pub use email_verification::EmailVerificationToken;
pub use item::{CreateItem, Item, ItemRevision, UpdateItem};
pub use item_type::{CreateItemType, ItemType};
pub use language::{CreateLanguage, Language};
pub use menu_link::{CreateMenuLink, MenuLink, UpdateMenuLink};
pub use password_reset::PasswordResetToken;
pub use role::Role;
pub use site_config::{RegistrationMode, SiteConfig};
pub use stage::{CreateStage, Stage, StageReferences};
pub use subscription::Subscription;
pub use tenant::{DEFAULT_TENANT_ID, Tenant, TenantContext};
pub use url_alias::{CreateUrlAlias, UpdateUrlAlias, UrlAlias};
pub use user::{CreateUser, UpdateUser, User};
pub use webauthn_credential::WebauthnCredential;

/// The current Unix time in seconds.
///
/// The serde default for the `created` and `changed` timestamps of config
/// entities. A config file is the interface for everything without a screen, and
/// a hand-written file has no meaningful timestamp to give: without a default,
/// omitting one failed the file, and because import validates the whole set
/// first, failed every other file with it. Storage never overwrites an existing
/// row's `created` on re-import, so the default only ever dates a new row.
pub(crate) fn unix_now() -> i64 {
    chrono::Utc::now().timestamp()
}
