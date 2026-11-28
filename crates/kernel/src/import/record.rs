//! The records of an import file, as they are read from it.

use anyhow::Result;
use serde::{Deserialize, Deserializer};
use uuid::Uuid;

/// Every record type, in the order the report lists them.
pub const RECORD_TYPES: &[&str] = &[
    "user", "file", "item", "revision", "comment", "alias", "redirect", "tile", "delete",
];

/// The kinds a `delete` record may name: every kind with an id of its own
/// that an import can also create or change. Not `revision` (immutable, never
/// deleted on its own) and not `redirect` (out of scope for this record).
const DELETE_KINDS: &[&str] = &["item", "comment", "alias", "tile", "file", "user"];

/// Check `kind` against [`DELETE_KINDS`], and return the matching literal (so
/// a report bucket key is always one of the fixed strings [`RECORD_TYPES`]
/// lists, not a copy of the file's own string).
pub(crate) fn delete_kind(kind: &str) -> Result<&'static str> {
    DELETE_KINDS
        .iter()
        .copied()
        .find(|k| *k == kind)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "'{kind}' is not a kind a delete record can name ({})",
                DELETE_KINDS.join(", ")
            )
        })
}

/// One line of the file, by its `type`.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub(crate) enum Record {
    Item(ItemRecord),
    Revision(RevisionRecord),
    Comment(CommentRecord),
    Alias(AliasRecord),
    Redirect(RedirectRecord),
    User(UserRecord),
    Tile(TileRecord),
    File(FileRecord),
    Delete(DeleteRecord),
}

impl Record {
    /// The `type` the line gave.
    pub(crate) fn type_name(&self) -> &'static str {
        match self {
            Self::Item(_) => "item",
            Self::Revision(_) => "revision",
            Self::Comment(_) => "comment",
            Self::Alias(_) => "alias",
            Self::Redirect(_) => "redirect",
            Self::User(_) => "user",
            Self::Tile(_) => "tile",
            Self::File(_) => "file",
            Self::Delete(_) => "delete",
        }
    }

    /// The id this line names: the record's own id, or for a `delete` record,
    /// the id of the record it names.
    pub(crate) fn id(&self) -> Uuid {
        match self {
            Self::Item(r) => r.id,
            Self::Revision(r) => r.id,
            Self::Comment(r) => r.id,
            Self::Alias(r) => r.id,
            Self::Redirect(r) => r.id,
            Self::User(r) => r.id,
            Self::Tile(r) => r.id,
            Self::File(r) => r.id,
            Self::Delete(r) => r.id,
        }
    }

    /// The report bucket this line counts against: the kind it names, for
    /// every type, so a `delete` is counted against the same bucket its
    /// matching create or change would be (`report.of("item")`, not a
    /// separate `"delete"` bucket) — one line per kind in the printed
    /// summary, each with its own `deleted` count.
    pub(crate) fn bucket(&self) -> Result<&'static str> {
        match self {
            Self::Delete(r) => delete_kind(&r.kind),
            other => Ok(other.type_name()),
        }
    }
}

/// A `delete` record: removes a stored record of `kind`, by the id its
/// create used. An id that names nothing stored is unchanged, so a rerun is
/// idempotent (`docs/design/import.md`, "Delete").
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeleteRecord {
    pub id: Uuid,
    pub kind: String,
}

/// An item as it is now: the item row and its current revision.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ItemRecord {
    pub id: Uuid,
    pub item_type: String,
    pub title: String,
    pub author_id: Uuid,
    #[serde(deserialize_with = "timestamp")]
    pub created: i64,
    #[serde(deserialize_with = "timestamp")]
    pub changed: i64,
    #[serde(default)]
    pub status: Option<i16>,
    #[serde(default)]
    pub promote: Option<i16>,
    #[serde(default)]
    pub sticky: Option<i16>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub fields: Option<serde_json::Value>,
    /// The id of the revision a create or a change writes.
    #[serde(default)]
    pub revision_id: Option<Uuid>,
    /// Who saved that revision; the item's author when absent.
    #[serde(default)]
    pub revision_author_id: Option<Uuid>,
    /// That revision's log message.
    #[serde(default)]
    pub log: Option<String>,
}

/// A past revision of an item.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RevisionRecord {
    pub id: Uuid,
    pub item_id: Uuid,
    pub author_id: Uuid,
    pub title: String,
    #[serde(deserialize_with = "timestamp")]
    pub created: i64,
    #[serde(default)]
    pub status: Option<i16>,
    #[serde(default)]
    pub fields: Option<serde_json::Value>,
    #[serde(default)]
    pub log: Option<String>,
}

/// A comment on an item.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CommentRecord {
    pub id: Uuid,
    pub item_id: Uuid,
    #[serde(default)]
    pub parent_id: Option<Uuid>,
    pub author_id: Uuid,
    #[serde(default)]
    pub subject: Option<String>,
    pub body: String,
    #[serde(default)]
    pub body_format: Option<String>,
    #[serde(default)]
    pub status: Option<i16>,
    #[serde(deserialize_with = "timestamp")]
    pub created: i64,
    #[serde(deserialize_with = "timestamp")]
    pub changed: i64,
}

/// A URL alias.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AliasRecord {
    pub id: Uuid,
    pub source: String,
    pub alias: String,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(deserialize_with = "timestamp")]
    pub created: i64,
    #[serde(deserialize_with = "timestamp")]
    pub changed: i64,
}

/// A redirect from an old path.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RedirectRecord {
    pub id: Uuid,
    pub source: String,
    pub destination: String,
    #[serde(default)]
    pub status_code: Option<i16>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(deserialize_with = "timestamp")]
    pub created: i64,
}

/// An account. The password, if any, only as a hash.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UserRecord {
    pub id: Uuid,
    pub name: String,
    pub mail: String,
    #[serde(default)]
    pub status: Option<i16>,
    #[serde(deserialize_with = "timestamp")]
    pub created: i64,
    #[serde(default)]
    pub password_hash: Option<String>,
    #[serde(default)]
    pub roles: Vec<String>,
}

/// A tile.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TileRecord {
    pub id: Uuid,
    pub machine_name: String,
    pub label: String,
    pub region: String,
    #[serde(default)]
    pub tile_type: Option<String>,
    #[serde(default)]
    pub config: Option<serde_json::Value>,
    #[serde(default)]
    pub visibility: Option<serde_json::Value>,
    #[serde(default)]
    pub weight: Option<i32>,
    #[serde(default)]
    pub status: Option<i16>,
    #[serde(deserialize_with = "timestamp")]
    pub created: i64,
    #[serde(deserialize_with = "timestamp")]
    pub changed: i64,
}

/// A file already copied into the files directory, registered under the id,
/// owner and date the record gives.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileRecord {
    pub id: Uuid,
    /// Who the file belongs to; a user id, stored or on an earlier line.
    pub owner_id: Uuid,
    /// Where the bytes already sit, relative to the files directory
    /// (`UPLOADS_DIR`). No absolute path, no `..`, no symlink leading outside it.
    pub path: String,
    /// The name shown to users. Its extension must agree with `mime_type`.
    pub filename: String,
    /// The type the bytes are checked against, as an upload is.
    pub mime_type: String,
    #[serde(deserialize_with = "timestamp")]
    pub created: i64,
}

/// A date as the file may give it: Unix seconds, or an RFC 3339 string.
fn timestamp<'de, D: Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Seconds(i64),
        Text(String),
    }
    match Raw::deserialize(deserializer)? {
        Raw::Seconds(s) => Ok(s),
        Raw::Text(t) => chrono::DateTime::parse_from_rfc3339(&t)
            .map(|d| d.timestamp())
            .map_err(|e| {
                serde::de::Error::custom(format!(
                    "'{t}' is not a date (Unix seconds or RFC 3339): {e}"
                ))
            }),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn a_date_is_seconds_or_rfc_3339() {
        let a: Record = serde_json::from_str(
            r#"{"type":"revision","id":"0193a5a0-0000-7000-8000-000000000001","item_id":"0193a5a0-0000-7000-8000-000000000002","author_id":"0193a5a0-0000-7000-8000-000000000003","title":"t","created":"2019-04-01T10:00:00Z"}"#,
        )
        .unwrap();
        let Record::Revision(r) = a else {
            panic!("a revision")
        };
        assert_eq!(r.created, 1_554_112_800);

        let b: Record = serde_json::from_str(
            r#"{"type":"revision","id":"0193a5a0-0000-7000-8000-000000000001","item_id":"0193a5a0-0000-7000-8000-000000000002","author_id":"0193a5a0-0000-7000-8000-000000000003","title":"t","created":1554112800}"#,
        )
        .unwrap();
        let Record::Revision(r) = b else {
            panic!("a revision")
        };
        assert_eq!(r.created, 1_554_112_800);
    }

    #[test]
    fn an_unknown_key_or_type_is_refused() {
        let misspelt = serde_json::from_str::<Record>(
            r#"{"type":"revision","id":"0193a5a0-0000-7000-8000-000000000001","item_id":"0193a5a0-0000-7000-8000-000000000002","author_id":"0193a5a0-0000-7000-8000-000000000003","title":"t","created":1,"chnaged":2}"#,
        )
        .unwrap_err();
        assert!(misspelt.to_string().contains("chnaged"), "{misspelt}");

        let unknown = serde_json::from_str::<Record>(r#"{"type":"page","id":"x"}"#).unwrap_err();
        assert!(unknown.to_string().contains("page"), "{unknown}");

        let bad_date = serde_json::from_str::<Record>(
            r#"{"type":"revision","id":"0193a5a0-0000-7000-8000-000000000001","item_id":"0193a5a0-0000-7000-8000-000000000002","author_id":"0193a5a0-0000-7000-8000-000000000003","title":"t","created":"last tuesday"}"#,
        )
        .unwrap_err();
        assert!(bad_date.to_string().contains("last tuesday"), "{bad_date}");
    }
}
