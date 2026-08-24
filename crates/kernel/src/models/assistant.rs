//! Conversations and proposals: the AI Assistant's two tables.
//!
//! A [`Conversation`] is one person configuring one thing. Its `transcript` is a
//! JSONB array of [`TranscriptEntry`], written whole on every change, because it
//! is only ever read whole: the page renders all of it and the turn loop feeds
//! all of it to the model.
//!
//! A [`Proposal`] is a write the model asked for and the person has not applied
//! yet. It is a row rather than another transcript entry because it has a
//! lifecycle the transcript does not — proposed, then applied or discarded or
//! failed, once, by the owner, in a separate request.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

// =============================================================================
// Transcript
// =============================================================================

/// One entry in a conversation's transcript.
///
/// Tagged by `kind` on the wire, so the page and the model builder can both
/// match on it and a new kind can be added without rewriting stored rows.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TranscriptEntry {
    /// Something the person typed.
    User {
        /// What they said.
        text: String,
        /// Epoch seconds.
        ts: i64,
    },
    /// Something the model said.
    Assistant {
        /// What it said.
        text: String,
        /// Epoch seconds.
        ts: i64,
    },
    /// A tool the model called.
    ToolCall {
        /// Provider-assigned id, matching the [`TranscriptEntry::ToolResult`].
        call_id: String,
        /// The tool's name.
        tool: String,
        /// The arguments the model supplied.
        arguments: serde_json::Value,
        /// Epoch seconds.
        ts: i64,
    },
    /// What a tool call produced.
    ToolResult {
        /// The id of the call this answers.
        call_id: String,
        /// The tool's name.
        tool: String,
        /// Whether the call succeeded.
        ok: bool,
        /// One sentence for the person, when the plugin gave one.
        summary: Option<String>,
        /// What the model reads.
        content: String,
        /// Epoch seconds.
        ts: i64,
    },
    /// A write the model asked for, waiting on the person.
    ///
    /// Carries the arguments because rebuilding the next request's message list
    /// means reconstructing the model's own tool call from this entry: without
    /// them the assistant turn that produced the proposal could not be replayed.
    Proposal {
        /// The `ai_proposal` row's id.
        proposal_id: String,
        /// The model's call id for the tool call this stands in for.
        call_id: String,
        /// The tool's name.
        tool: String,
        /// The arguments the model supplied.
        arguments: serde_json::Value,
        /// The plugin's one-sentence description of what applying would do.
        description: String,
        /// How much care applying deserves.
        risk: String,
        /// Epoch seconds.
        ts: i64,
    },
    /// Something the kernel is telling both the person and the model.
    ///
    /// Reaches the model as a user message prefixed `[Trovato] `, which is how
    /// it learns that a proposal was applied or discarded, that history was
    /// dropped, or that a limit was reached.
    Note {
        /// The message.
        text: String,
        /// Epoch seconds.
        ts: i64,
    },
}

impl TranscriptEntry {
    /// The epoch second this entry was written.
    pub fn ts(&self) -> i64 {
        match self {
            Self::User { ts, .. }
            | Self::Assistant { ts, .. }
            | Self::ToolCall { ts, .. }
            | Self::ToolResult { ts, .. }
            | Self::Proposal { ts, .. }
            | Self::Note { ts, .. } => *ts,
        }
    }

    /// Whether this entry starts a new exchange, for history bounding.
    pub fn starts_exchange(&self) -> bool {
        matches!(self, Self::User { .. })
    }
}

/// The current epoch second.
pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

// =============================================================================
// Conversation
// =============================================================================

/// Conversation status: `open` or `closed`.
pub const STATUS_OPEN: &str = "open";
/// A conversation that Start over replaced.
pub const STATUS_CLOSED: &str = "closed";

/// One person configuring one thing.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Conversation {
    /// Conversation id, in the URL of every API call about it.
    pub id: Uuid,
    /// The person it belongs to. Nobody else can read or write it.
    pub user_id: Uuid,
    /// The plugin that owns the scope.
    pub plugin: String,
    /// The scope name.
    pub scope: String,
    /// The scope id, when the scope takes one.
    pub scope_id: Option<String>,
    /// Page title, from the plugin's context.
    pub title: String,
    /// [`STATUS_OPEN`] or [`STATUS_CLOSED`].
    pub status: String,
    /// The plugin's description of the thing, as of when this opened.
    pub snapshot: String,
    /// Links for the header, as declared by the plugin.
    pub links: serde_json::Value,
    /// The transcript, a JSON array of [`TranscriptEntry`].
    pub transcript: serde_json::Value,
    /// How many messages the person has sent.
    pub message_count: i32,
    /// Total tokens this conversation has cost.
    pub tokens_used: i64,
    /// Epoch seconds.
    pub created: i64,
    /// Epoch seconds of the last change.
    pub changed: i64,
}

impl Conversation {
    /// Decode the transcript.
    ///
    /// An entry that does not decode is skipped rather than failing the read: a
    /// conversation from a future kernel with a kind this one does not know is
    /// still worth showing, minus the entries it cannot render.
    pub fn entries(&self) -> Vec<TranscriptEntry> {
        self.transcript
            .as_array()
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|entry| serde_json::from_value(entry.clone()).ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The header links.
    pub fn link_list(&self) -> Vec<trovato_sdk::types::AssistantLink> {
        serde_json::from_value(self.links.clone()).unwrap_or_default()
    }

    /// Whether this conversation is still open.
    pub fn is_open(&self) -> bool {
        self.status == STATUS_OPEN
    }

    /// Insert a new conversation.
    pub async fn create(pool: &PgPool, conversation: &Conversation) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO ai_conversation
                (id, user_id, plugin, scope, scope_id, title, status, snapshot, links,
                 transcript, message_count, tokens_used, created, changed)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
            "#,
        )
        .bind(conversation.id)
        .bind(conversation.user_id)
        .bind(&conversation.plugin)
        .bind(&conversation.scope)
        .bind(&conversation.scope_id)
        .bind(&conversation.title)
        .bind(&conversation.status)
        .bind(&conversation.snapshot)
        .bind(&conversation.links)
        .bind(&conversation.transcript)
        .bind(conversation.message_count)
        .bind(conversation.tokens_used)
        .bind(conversation.created)
        .bind(conversation.changed)
        .execute(pool)
        .await
        .context("failed to create conversation")?;
        Ok(())
    }

    /// The caller's open conversation for this scope and id, if there is one.
    pub async fn find_open(
        pool: &PgPool,
        user_id: Uuid,
        scope: &str,
        scope_id: Option<&str>,
    ) -> Result<Option<Self>> {
        sqlx::query_as::<_, Self>(
            r#"
            SELECT * FROM ai_conversation
            WHERE user_id = $1 AND scope = $2 AND COALESCE(scope_id, '') = COALESCE($3, '')
              AND status = 'open'
            "#,
        )
        .bind(user_id)
        .bind(scope)
        .bind(scope_id)
        .fetch_optional(pool)
        .await
        .context("failed to look up an open conversation")
    }

    /// One conversation by id, whoever owns it.
    pub async fn find_by_id(pool: &PgPool, id: Uuid) -> Result<Option<Self>> {
        sqlx::query_as::<_, Self>("SELECT * FROM ai_conversation WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await
            .context("failed to load conversation")
    }

    /// Write the transcript and the counters back.
    ///
    /// Called at the end of a turn **and after every tool execution**, so a crash
    /// mid-turn loses at most one model call rather than the whole exchange.
    pub async fn save_transcript(
        pool: &PgPool,
        id: Uuid,
        entries: &[TranscriptEntry],
        message_count: i32,
        tokens_used: i64,
    ) -> Result<()> {
        let transcript = serde_json::to_value(entries).context("failed to encode transcript")?;
        sqlx::query(
            r#"
            UPDATE ai_conversation
            SET transcript = $2, message_count = $3, tokens_used = $4, changed = $5
            WHERE id = $1
            "#,
        )
        .bind(id)
        .bind(transcript)
        .bind(message_count)
        .bind(tokens_used)
        .bind(now())
        .execute(pool)
        .await
        .context("failed to save transcript")?;
        Ok(())
    }

    /// Close a conversation. Start over is the only thing that does this.
    pub async fn close(pool: &PgPool, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE ai_conversation SET status = 'closed', changed = $2 WHERE id = $1")
            .bind(id)
            .bind(now())
            .execute(pool)
            .await
            .context("failed to close conversation")?;
        Ok(())
    }
}

// =============================================================================
// Proposal
// =============================================================================

/// A write the model asked for, waiting on the person.
pub const PROPOSAL_PROPOSED: &str = "proposed";
/// A proposal the person applied, and the plugin carried out.
pub const PROPOSAL_APPLIED: &str = "applied";
/// A proposal the person threw away.
pub const PROPOSAL_DISCARDED: &str = "discarded";
/// A proposal the person applied and the plugin refused or could not do.
pub const PROPOSAL_FAILED: &str = "failed";

/// One proposed write.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Proposal {
    /// Proposal id.
    pub id: Uuid,
    /// The conversation it belongs to.
    pub conversation_id: Uuid,
    /// The person who may apply it.
    pub user_id: Uuid,
    /// The scope, denormalized so the apply path needs one read.
    pub scope: String,
    /// The scope id, when the scope takes one.
    pub scope_id: Option<String>,
    /// The write tool that would be called.
    pub tool: String,
    /// The arguments it would be called with.
    pub arguments: serde_json::Value,
    /// The plugin's one-sentence description of the change.
    pub description: String,
    /// `low`, `normal` or `high`.
    pub risk: String,
    /// [`PROPOSAL_PROPOSED`], [`PROPOSAL_APPLIED`], [`PROPOSAL_DISCARDED`] or
    /// [`PROPOSAL_FAILED`].
    pub status: String,
    /// What happened when it was applied.
    pub result: Option<String>,
    /// The model that proposed it.
    pub model: String,
    /// Epoch seconds.
    pub created: i64,
    /// Epoch seconds it was applied or discarded.
    pub resolved: Option<i64>,
    /// Who resolved it.
    pub resolved_by: Option<Uuid>,
}

impl Proposal {
    /// Insert a proposal.
    pub async fn create(pool: &PgPool, proposal: &Proposal) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO ai_proposal
                (id, conversation_id, user_id, scope, scope_id, tool, arguments,
                 description, risk, status, result, model, created, resolved, resolved_by)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)
            "#,
        )
        .bind(proposal.id)
        .bind(proposal.conversation_id)
        .bind(proposal.user_id)
        .bind(&proposal.scope)
        .bind(&proposal.scope_id)
        .bind(&proposal.tool)
        .bind(&proposal.arguments)
        .bind(&proposal.description)
        .bind(&proposal.risk)
        .bind(&proposal.status)
        .bind(&proposal.result)
        .bind(&proposal.model)
        .bind(proposal.created)
        .bind(proposal.resolved)
        .bind(proposal.resolved_by)
        .execute(pool)
        .await
        .context("failed to create proposal")?;
        Ok(())
    }

    /// One proposal by id.
    pub async fn find_by_id(pool: &PgPool, id: Uuid) -> Result<Option<Self>> {
        sqlx::query_as::<_, Self>("SELECT * FROM ai_proposal WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await
            .context("failed to load proposal")
    }

    /// Every proposal in a conversation, oldest first.
    pub async fn for_conversation(pool: &PgPool, conversation_id: Uuid) -> Result<Vec<Self>> {
        sqlx::query_as::<_, Self>(
            "SELECT * FROM ai_proposal WHERE conversation_id = $1 ORDER BY created",
        )
        .bind(conversation_id)
        .fetch_all(pool)
        .await
        .context("failed to load proposals")
    }

    /// Move a proposal out of `proposed`, and say whether this call is what did
    /// it.
    ///
    /// The `status = 'proposed'` predicate is the concurrency control: two
    /// simultaneous applies both find the row, one updates it, and the other
    /// gets `false` and reports a conflict instead of executing the write twice.
    pub async fn resolve(
        pool: &PgPool,
        id: Uuid,
        status: &str,
        result: Option<&str>,
        resolved_by: Uuid,
    ) -> Result<bool> {
        let affected = sqlx::query(
            r#"
            UPDATE ai_proposal
            SET status = $2, result = $3, resolved = $4, resolved_by = $5
            WHERE id = $1 AND status = 'proposed'
            "#,
        )
        .bind(id)
        .bind(status)
        .bind(result)
        .bind(now())
        .bind(resolved_by)
        .execute(pool)
        .await
        .context("failed to resolve proposal")?
        .rows_affected();
        Ok(affected == 1)
    }

    /// Discard every still-proposed proposal in a conversation, and say how
    /// many. Start over does this: a proposal whose conversation is gone can
    /// never be applied, so leaving it `proposed` would be a lie.
    pub async fn discard_open(pool: &PgPool, conversation_id: Uuid, by: Uuid) -> Result<u64> {
        let affected = sqlx::query(
            r#"
            UPDATE ai_proposal
            SET status = 'discarded', resolved = $2, resolved_by = $3
            WHERE conversation_id = $1 AND status = 'proposed'
            "#,
        )
        .bind(conversation_id)
        .bind(now())
        .bind(by)
        .execute(pool)
        .await
        .context("failed to discard open proposals")?
        .rows_affected();
        Ok(affected)
    }
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn transcript_entries_are_tagged_by_kind() {
        let entries = vec![
            TranscriptEntry::User {
                text: "hi".into(),
                ts: 1,
            },
            TranscriptEntry::ToolCall {
                call_id: "c1".into(),
                tool: "read_widget".into(),
                arguments: serde_json::json!({}),
                ts: 2,
            },
            TranscriptEntry::Proposal {
                proposal_id: "p1".into(),
                call_id: "c2".into(),
                tool: "set_widget_color".into(),
                arguments: serde_json::json!({"color": "teal"}),
                description: "Set widget color to teal".into(),
                risk: "normal".into(),
                ts: 3,
            },
        ];
        let json = serde_json::to_string(&entries).unwrap();
        assert!(json.contains(r#""kind":"user""#), "{json}");
        assert!(json.contains(r#""kind":"tool_call""#), "{json}");
        assert!(json.contains(r#""kind":"proposal""#), "{json}");

        let back: Vec<TranscriptEntry> = serde_json::from_str(&json).unwrap();
        assert_eq!(back.len(), 3);
        assert!(back[0].starts_exchange());
        assert!(!back[1].starts_exchange());
        assert_eq!(back[2].ts(), 3);
    }

    #[test]
    fn a_proposal_entry_keeps_the_arguments_it_would_be_applied_with() {
        // Not decoration: the next request's message list rebuilds the model's
        // own tool call out of this entry, and cannot without them.
        let json = r#"{"kind":"proposal","proposal_id":"p","call_id":"c","tool":"t",
                       "arguments":{"color":"teal"},"description":"d","risk":"low","ts":1}"#;
        let entry: TranscriptEntry = serde_json::from_str(json).unwrap();
        match entry {
            TranscriptEntry::Proposal { arguments, .. } => {
                assert_eq!(arguments["color"], "teal");
            }
            other => panic!("expected a proposal entry, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_entry_kind_is_skipped_rather_than_failing_the_whole_transcript() {
        let conversation = Conversation {
            id: Uuid::nil(),
            user_id: Uuid::nil(),
            plugin: "p".into(),
            scope: "s".into(),
            scope_id: None,
            title: "t".into(),
            status: STATUS_OPEN.into(),
            snapshot: String::new(),
            links: serde_json::json!([]),
            transcript: serde_json::json!([
                {"kind": "user", "text": "hi", "ts": 1},
                {"kind": "from_the_future", "whatever": true},
                {"kind": "assistant", "text": "hello", "ts": 2}
            ]),
            message_count: 1,
            tokens_used: 0,
            created: 0,
            changed: 0,
        };
        let entries = conversation.entries();
        assert_eq!(entries.len(), 2);
        assert!(conversation.is_open());
    }
}
