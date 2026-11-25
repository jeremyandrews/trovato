-- B218: queue the static search index's items from the database itself, in the
-- same transaction as the write (docs/design/incremental-search-index.md, part
-- 3 of 3). Expand only: three triggers and their functions on existing tables.
--
-- A write to an item, a comment or a URL alias queues the item whose page it
-- changes, atomically with the write: a write that commits is queued, whoever
-- made it (a kernel service, a plugin's raw SQL, a bulk statement), and a
-- write that rolls back queues nothing. Queuing after the commit, from the
-- service, left a window in which a crash or a failed insert lost the change
-- until the next full build.
--
-- The reason recorded is the transaction's `trovato.search_index_reason`
-- setting when one is set (workspace publishing sets `stage_publish`), else
-- what was written: item, comment or url_alias.

-- Queue one item: the same upsert as `crate::search::index_queue::enqueue`,
-- the last write before the drain winning and taking a new `seq`.
CREATE FUNCTION search_index_queue_put(p_item UUID, p_language TEXT, p_op SMALLINT, p_reason TEXT)
RETURNS VOID LANGUAGE sql AS $$
    INSERT INTO search_index_queue (item_id, language, op, reason)
    VALUES (
        p_item, p_language, p_op,
        COALESCE(NULLIF(current_setting('trovato.search_index_reason', true), ''), p_reason)
    )
    ON CONFLICT (item_id) DO UPDATE SET
        language = EXCLUDED.language, op = EXCLUDED.op, reason = EXCLUDED.reason,
        seq = EXCLUDED.seq, enqueued_at = EXCLUDED.enqueued_at;
$$;

-- An item is in the public index's reach when it is published (status 1) on
-- the live stage. A write that leaves it out of reach before and after
-- (a draft, a workspace copy) changes nothing the public can search.
CREATE FUNCTION search_index_queue_item() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    -- `comment_count` and `search_vector` are bookkeeping the comment table's
    -- own triggers rewrite for every comment write, published or not, and
    -- neither is in the page. The comment trigger below decides for comments;
    -- a database search reindex leaves the static index alone.
    IF TG_OP = 'UPDATE'
       AND to_jsonb(OLD) - 'comment_count' - 'search_vector'
           = to_jsonb(NEW) - 'comment_count' - 'search_vector' THEN
        RETURN NEW;
    END IF;
    IF TG_OP = 'DELETE' THEN
        PERFORM search_index_queue_put(OLD.id, OLD.language, 2::smallint, 'item');
        RETURN OLD;
    END IF;
    PERFORM search_index_queue_put(NEW.id, NEW.language, 1::smallint, 'item');
    RETURN NEW;
END;
$$;

CREATE TRIGGER item_search_index_queue_insert
    AFTER INSERT ON item FOR EACH ROW
    WHEN (NEW.status = 1 AND NEW.stage_id = '0193a5a0-0000-7000-8000-000000000001'::uuid)
    EXECUTE FUNCTION search_index_queue_item();

CREATE TRIGGER item_search_index_queue_update
    AFTER UPDATE ON item FOR EACH ROW
    WHEN (((OLD.status = 1 AND OLD.stage_id = '0193a5a0-0000-7000-8000-000000000001'::uuid)
           OR (NEW.status = 1 AND NEW.stage_id = '0193a5a0-0000-7000-8000-000000000001'::uuid))
          AND OLD.* IS DISTINCT FROM NEW.*)
    EXECUTE FUNCTION search_index_queue_item();

CREATE TRIGGER item_search_index_queue_delete
    AFTER DELETE ON item FOR EACH ROW
    WHEN (OLD.status = 1 AND OLD.stage_id = '0193a5a0-0000-7000-8000-000000000001'::uuid)
    EXECUTE FUNCTION search_index_queue_item();

-- A comment is on its item's page when it is published (status 1). The item
-- is queued as an upsert: the drain decides from the item's own state whether
-- it is searchable at all.
CREATE FUNCTION search_index_queue_comment() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP IN ('UPDATE', 'DELETE') AND OLD.status = 1 THEN
        PERFORM search_index_queue_put(OLD.item_id, NULL, 1::smallint, 'comment');
    END IF;
    IF TG_OP IN ('INSERT', 'UPDATE') AND NEW.status = 1
       AND (TG_OP = 'INSERT' OR OLD.item_id IS DISTINCT FROM NEW.item_id OR OLD.status <> 1) THEN
        PERFORM search_index_queue_put(NEW.item_id, NULL, 1::smallint, 'comment');
    END IF;
    RETURN NULL;
END;
$$;

CREATE TRIGGER comment_search_index_queue_insert
    AFTER INSERT ON comment FOR EACH ROW
    WHEN (NEW.status = 1)
    EXECUTE FUNCTION search_index_queue_comment();

CREATE TRIGGER comment_search_index_queue_update
    AFTER UPDATE ON comment FOR EACH ROW
    WHEN ((OLD.status = 1 OR NEW.status = 1) AND OLD.* IS DISTINCT FROM NEW.*)
    EXECUTE FUNCTION search_index_queue_comment();

CREATE TRIGGER comment_search_index_queue_delete
    AFTER DELETE ON comment FOR EACH ROW
    WHEN (OLD.status = 1)
    EXECUTE FUNCTION search_index_queue_comment();

-- A URL alias is the result card's link, so a write to one queues the item its
-- source names (`/item/<uuid>`), before and after. Other sources are ignored.
CREATE FUNCTION search_index_queue_url_alias() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    item_source CONSTANT TEXT :=
        '^/item/[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$';
BEGIN
    IF TG_OP IN ('UPDATE', 'DELETE') AND OLD.source ~ item_source THEN
        PERFORM search_index_queue_put(substring(OLD.source FROM 7)::uuid, NULL, 1::smallint, 'url_alias');
    END IF;
    IF TG_OP IN ('INSERT', 'UPDATE') AND NEW.source ~ item_source
       AND (TG_OP = 'INSERT' OR OLD.source IS DISTINCT FROM NEW.source) THEN
        PERFORM search_index_queue_put(substring(NEW.source FROM 7)::uuid, NULL, 1::smallint, 'url_alias');
    END IF;
    RETURN NULL;
END;
$$;

CREATE TRIGGER url_alias_search_index_queue
    AFTER INSERT OR UPDATE OR DELETE ON url_alias FOR EACH ROW
    EXECUTE FUNCTION search_index_queue_url_alias();
