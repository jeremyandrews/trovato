//! Per-user reader state: reactions, read state, topic subscriptions (M3).
//!
//! This is the pure half — what a reaction *is*, which reactions exclude each
//! other, and what applying one to a user's existing set produces. The storage
//! ports are in [`crate::ports`] and the host implementations in the plugin.
//!
//! # What is and is not reachable in 1.0
//!
//! Read state is written for real: `tap_item_view` fires on the story page with
//! the authenticated user and a live services handle, so "this reader has seen
//! this story" is recordable at view time.
//!
//! Reactions and subscriptions are **not writable by a reader** under the frozen
//! contract, because no kernel surface lets an authenticated user write a
//! plugin-owned table: a plugin cannot serve a route (`tap_menu` carries no
//! callback the kernel honours), and the form/AJAX path is admin-only,
//! service-less, and unreachable. The types and storage here are complete and
//! tested so that the moment a write surface exists the logic does not have to
//! be invented; the gap is `G-NO-PLUGIN-HTTP` in `M3-FRICTION.md`, and
//! `M3-DESIGN.md` Decision 5 argues why shipping this as a placeholder beats
//! modelling an upvote as a revisioned Item.

use crate::error::{CoreError, CoreResult};

/// A reaction a reader can register against a story.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Reaction {
    /// Endorse the story.
    Upvote,
    /// Object to the story.
    Downvote,
    /// Save the story for later.
    Bookmark,
    /// Report the story for moderator attention.
    Flag,
}

impl Reaction {
    /// The stored discriminator, and the wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Upvote => "upvote",
            Self::Downvote => "downvote",
            Self::Bookmark => "bookmark",
            Self::Flag => "flag",
        }
    }

    /// Every reaction, in a stable order (used by the storage tests and by any
    /// future surface that has to enumerate them).
    pub fn all() -> [Self; 4] {
        [Self::Upvote, Self::Downvote, Self::Bookmark, Self::Flag]
    }

    /// Parse a stored or submitted discriminator.
    ///
    /// # Errors
    ///
    /// [`CoreError::Invalid`] for anything not in [`Reaction::all`]. The error is
    /// permanent, not transient: an unknown reaction name will not become known
    /// on a retry.
    pub fn parse(raw: &str) -> CoreResult<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "upvote" => Ok(Self::Upvote),
            "downvote" => Ok(Self::Downvote),
            "bookmark" => Ok(Self::Bookmark),
            "flag" => Ok(Self::Flag),
            other => Err(CoreError::Invalid(format!("unknown reaction {other:?}"))),
        }
    }

    /// The reaction this one displaces, if any.
    ///
    /// A story cannot be simultaneously endorsed and objected to, so applying
    /// one of that pair clears the other. Bookmarking and flagging are
    /// orthogonal to both and to each other.
    pub fn displaces(self) -> Option<Self> {
        match self {
            Self::Upvote => Some(Self::Downvote),
            Self::Downvote => Some(Self::Upvote),
            Self::Bookmark | Self::Flag => None,
        }
    }
}

/// What applying a reaction to a reader's current set should do to storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReactionChange {
    /// The reaction to insert, or `None` when the action was a toggle-off.
    pub insert: Option<Reaction>,
    /// Reactions to remove: the displaced opposite, and/or the toggled-off one.
    pub remove: Vec<Reaction>,
}

/// Decide what one reader tapping `reaction` should do, given what they have
/// already registered on that story.
///
/// Tapping a reaction the reader already holds removes it (the universal
/// toggle-off users expect from an upvote button); tapping a fresh one inserts
/// it and clears whatever it [`displaces`](Reaction::displaces).
///
/// Pure and total: `current` may contain anything, including a contradictory
/// pair left by an older version, and the result still converges on a legal set.
pub fn apply_reaction(current: &[Reaction], reaction: Reaction) -> ReactionChange {
    if current.contains(&reaction) {
        return ReactionChange {
            insert: None,
            remove: vec![reaction],
        };
    }
    let mut remove = Vec::new();
    if let Some(opposite) = reaction.displaces()
        && current.contains(&opposite)
    {
        remove.push(opposite);
    }
    ReactionChange {
        insert: Some(reaction),
        remove,
    }
}

/// A reader's view record for one story.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadState {
    /// When this reader first opened the story (unix seconds).
    pub first_seen_at: i64,
    /// When they last opened it (unix seconds).
    pub last_seen_at: i64,
    /// How many times they have opened it.
    pub view_count: i64,
}

impl ReadState {
    /// The record a first view produces.
    pub fn first_view(now: i64) -> Self {
        Self {
            first_seen_at: now,
            last_seen_at: now,
            view_count: 1,
        }
    }

    /// Fold another view into an existing record.
    ///
    /// `first_seen_at` never moves and `last_seen_at` never goes backwards, so
    /// an out-of-order replay — which at-least-once delivery makes possible —
    /// cannot corrupt the record.
    pub fn record_view(self, now: i64) -> Self {
        Self {
            first_seen_at: self.first_seen_at.min(now),
            last_seen_at: self.last_seen_at.max(now),
            view_count: self.view_count.saturating_add(1),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn every_reaction_round_trips_through_its_wire_name() {
        for r in Reaction::all() {
            assert_eq!(Reaction::parse(r.as_str()).expect("parses"), r);
        }
    }

    #[test]
    fn parsing_is_case_and_whitespace_tolerant() {
        assert_eq!(
            Reaction::parse(" UpVote ").expect("parses"),
            Reaction::Upvote
        );
    }

    #[test]
    fn an_unknown_reaction_is_a_permanent_error() {
        let err = Reaction::parse("shrug").expect_err("rejects");
        assert!(!err.is_transient(), "retrying will not make it known");
    }

    #[test]
    fn upvote_and_downvote_displace_each_other() {
        assert_eq!(Reaction::Upvote.displaces(), Some(Reaction::Downvote));
        assert_eq!(Reaction::Downvote.displaces(), Some(Reaction::Upvote));
        assert_eq!(Reaction::Bookmark.displaces(), None);
        assert_eq!(Reaction::Flag.displaces(), None);
    }

    #[test]
    fn a_fresh_reaction_is_inserted() {
        let change = apply_reaction(&[], Reaction::Upvote);
        assert_eq!(change.insert, Some(Reaction::Upvote));
        assert!(change.remove.is_empty());
    }

    #[test]
    fn tapping_a_held_reaction_toggles_it_off() {
        let change = apply_reaction(&[Reaction::Bookmark], Reaction::Bookmark);
        assert_eq!(change.insert, None);
        assert_eq!(change.remove, vec![Reaction::Bookmark]);
    }

    #[test]
    fn upvoting_a_downvoted_story_clears_the_downvote() {
        let change = apply_reaction(&[Reaction::Downvote], Reaction::Upvote);
        assert_eq!(change.insert, Some(Reaction::Upvote));
        assert_eq!(change.remove, vec![Reaction::Downvote]);
    }

    #[test]
    fn bookmarking_leaves_an_existing_vote_alone() {
        let change = apply_reaction(&[Reaction::Upvote], Reaction::Bookmark);
        assert_eq!(change.insert, Some(Reaction::Bookmark));
        assert!(change.remove.is_empty());
    }

    #[test]
    fn a_contradictory_stored_set_still_converges() {
        // Not reachable through this function, but a row pair could survive a
        // partial write (G-DB-NO-TX): applying Upvote must still leave a legal set.
        let change = apply_reaction(&[Reaction::Upvote, Reaction::Downvote], Reaction::Downvote);
        assert_eq!(change.insert, None);
        assert_eq!(change.remove, vec![Reaction::Downvote]);
    }

    #[test]
    fn a_first_view_stamps_both_ends() {
        let state = ReadState::first_view(1_000);
        assert_eq!(state.first_seen_at, 1_000);
        assert_eq!(state.last_seen_at, 1_000);
        assert_eq!(state.view_count, 1);
    }

    #[test]
    fn a_later_view_advances_only_the_last_seen_stamp() {
        let state = ReadState::first_view(1_000).record_view(2_000);
        assert_eq!(state.first_seen_at, 1_000);
        assert_eq!(state.last_seen_at, 2_000);
        assert_eq!(state.view_count, 2);
    }

    #[test]
    fn an_out_of_order_replay_cannot_move_the_stamps_backwards() {
        let state = ReadState::first_view(2_000).record_view(1_000);
        assert_eq!(state.first_seen_at, 1_000, "an earlier view is the first");
        assert_eq!(state.last_seen_at, 2_000, "last_seen never regresses");
    }
}
