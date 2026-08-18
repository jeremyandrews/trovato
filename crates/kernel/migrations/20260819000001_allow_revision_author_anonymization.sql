-- Permit exactly one update to an immutable revision: anonymizing its authorship
-- when the author's account is deleted.
--
-- WHY THIS EXISTS
--
-- `item_revision` rows are immutable by trigger (Story 45.3): a revision is a
-- snapshot of an item at a point in time, and a snapshot that can be edited is not
-- a history. `author_id` is `NOT NULL REFERENCES users(id)` with no `ON DELETE`
-- action, so the trigger and the foreign key together made an account that had ever
-- saved an item **undeletable** — by its holder and by an administrator alike, which
-- is what self-service account deletion ran into.
--
-- Three ways out, and only one of them is right:
--
--   * Delete the author's revisions. Wrong: they are the history of items that
--     remain visible, and other people's record of how that content changed.
--   * Refuse to delete such an account. Wrong: that is most accounts, and a right
--     of erasure that applies only to people who never wrote anything is not one.
--   * Narrow the invariant. Correct, and this is it.
--
-- THE INVARIANT, RESTATED
--
-- Before: "a revision never changes."
-- After:  "a revision's *content* never changes; its authorship may be anonymized
--          when the author's account is deleted."
--
-- That is the tension a right of erasure creates with append-only history, and the
-- resolution has to be deliberate rather than a disabled trigger in a script.
--
-- HOW THE NARROWING IS ENFORCED
--
-- The permitted update is compared as a whole row: `to_jsonb(NEW) - 'author_id'`
-- must equal `to_jsonb(OLD) - 'author_id'`. Written that way rather than as a list
-- of column comparisons on purpose — a column added to this table in future is
-- covered automatically, where a hand-written list would silently start permitting
-- changes to it. The new author must be the anonymous sentinel, and the old author
-- must be somebody else, so this cannot be used to reassign a revision from one
-- real person to another.

CREATE OR REPLACE FUNCTION prevent_item_revision_update()
RETURNS TRIGGER AS $$
BEGIN
    IF NEW.author_id = '00000000-0000-0000-0000-000000000000'::uuid
       AND OLD.author_id <> NEW.author_id
       AND (to_jsonb(NEW) - 'author_id') = (to_jsonb(OLD) - 'author_id')
    THEN
        RETURN NEW;
    END IF;

    RAISE EXCEPTION 'item_revision rows are immutable — the only permitted update is anonymizing author_id when the author''s account is deleted';
END;
$$ LANGUAGE plpgsql;
