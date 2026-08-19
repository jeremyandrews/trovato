-- A book is an ordered tree of items.
--
-- One row per page. The item is the page: a book adds ordering and hierarchy to
-- items that already exist, rather than introducing a second kind of content. Any
-- item type can be a page, which is what makes this useful for a documentation site
-- whose pages are ordinary content.
--
-- Plugin-owned, declared in `[capabilities] db_tables`. Nothing here touches a
-- kernel table: adding a `book_id` column to `item` would make every site carry a
-- column for a feature most of them do not use, and would put a plugin's schema
-- inside the kernel's.
CREATE TABLE IF NOT EXISTS book_page (
    -- The item that is this page. Primary key: an item belongs to at most one book,
    -- which is what makes "the next page" answerable at all.
    item_id UUID PRIMARY KEY,

    -- The item that is the book's root. A root page has book_id = item_id, so a book
    -- is identified by its own first page and needs no separate entity.
    book_id UUID NOT NULL,

    -- The parent page, or NULL for the root. Constrained to the same book by the
    -- plugin, which is also where cycles are rejected: a self-referential foreign
    -- key permits a cycle, so the check cannot live here.
    parent_item_id UUID,

    -- Sort weight among siblings. Ties break on the item's title, so the order is
    -- total and stable rather than whatever the database returns.
    weight INTEGER NOT NULL DEFAULT 0
);

-- The two reads this table exists for: every page of one book, in order, and one
-- page's children.
CREATE INDEX IF NOT EXISTS idx_book_page_book ON book_page (book_id, weight);
CREATE INDEX IF NOT EXISTS idx_book_page_parent ON book_page (parent_item_id);
