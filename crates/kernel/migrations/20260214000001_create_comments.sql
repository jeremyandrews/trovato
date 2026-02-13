-- Comment table for threaded discussions on content items
CREATE TABLE comment (
    -- Unique identifier (UUIDv7)
    id UUID PRIMARY KEY,

    -- Parent item this comment belongs to
    item_id UUID NOT NULL REFERENCES item(id) ON DELETE CASCADE,

    -- Parent comment for threading (NULL for top-level comments)
    parent_id UUID REFERENCES comment(id) ON DELETE CASCADE,

    -- Author user ID
    author_id UUID NOT NULL REFERENCES users(id),

    -- Comment body (supports HTML with text format filtering)
    body TEXT NOT NULL,

    -- Text format for the body (plain_text, filtered_html, full_html)
    body_format VARCHAR(50) NOT NULL DEFAULT 'filtered_html',

    -- Publication status (0 = unpublished/pending, 1 = published)
    status SMALLINT NOT NULL DEFAULT 1,

    -- Unix timestamp when created
    created BIGINT NOT NULL,

    -- Unix timestamp when last changed
    changed BIGINT NOT NULL,

    -- Thread depth for display (auto-calculated)
    depth SMALLINT NOT NULL DEFAULT 0
);

-- Index for loading comments by item
CREATE INDEX idx_comment_item ON comment(item_id);

-- Index for loading replies to a comment
CREATE INDEX idx_comment_parent ON comment(parent_id) WHERE parent_id IS NOT NULL;

-- Index for comment moderation (by status)
CREATE INDEX idx_comment_status ON comment(status);

-- Index for user's comments
CREATE INDEX idx_comment_author ON comment(author_id);

-- Add comment_count column to item table for denormalized count
ALTER TABLE item ADD COLUMN comment_count INTEGER NOT NULL DEFAULT 0;

-- Function to update comment count on item
CREATE OR REPLACE FUNCTION update_item_comment_count()
RETURNS TRIGGER AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        UPDATE item SET comment_count = comment_count + 1 WHERE id = NEW.item_id;
    ELSIF TG_OP = 'DELETE' THEN
        UPDATE item SET comment_count = comment_count - 1 WHERE id = OLD.item_id;
    END IF;
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

-- Trigger to maintain comment count
CREATE TRIGGER trigger_update_comment_count
AFTER INSERT OR DELETE ON comment
FOR EACH ROW EXECUTE FUNCTION update_item_comment_count();

-- Function to set comment depth based on parent
CREATE OR REPLACE FUNCTION set_comment_depth()
RETURNS TRIGGER AS $$
BEGIN
    IF NEW.parent_id IS NULL THEN
        NEW.depth := 0;
    ELSE
        SELECT depth + 1 INTO NEW.depth FROM comment WHERE id = NEW.parent_id;
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

-- Trigger to auto-set depth on insert
CREATE TRIGGER trigger_set_comment_depth
BEFORE INSERT ON comment
FOR EACH ROW EXECUTE FUNCTION set_comment_depth();
