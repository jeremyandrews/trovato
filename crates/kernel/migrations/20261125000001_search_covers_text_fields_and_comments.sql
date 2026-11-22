-- Search covers every text field and every published comment (B106).
--
-- The vector held the title and the fields named in `search_field_config`
-- only, so a type nobody had configured was findable by title alone, and no
-- comment was ever findable. It now holds:
--
--   * the title, at A, as before;
--   * every text field (`Text`, `TextLong`) the type declares in
--     `item_type.settings.fields`, every configured field, and the long text
--     field under either of its names (BL-21), each at its configured weight,
--     D when it has none. The configured weights stay weights; they are no
--     longer the list of what is indexed;
--   * the body of every comment an anonymous visitor sees on the item, at D:
--     published, and under a published parent all the way up, which is the rule
--     the item page's thread follows.
--
-- Markup is stripped before `to_tsvector`, so `<p>` is not a token, and the
-- comment vector is kept current by a trigger on `comment` rather than by
-- whoever writes a comment remembering to touch the item.

CREATE OR REPLACE FUNCTION item_search_vector(
    p_id UUID,
    p_type TEXT,
    p_title TEXT,
    p_fields JSONB
) RETURNS tsvector AS $$
DECLARE
    field RECORD;
    vector tsvector;
    field_value TEXT;
    comment_text TEXT;
BEGIN
    vector := setweight(to_tsvector('english', COALESCE(p_title, '')), 'A');

    FOR field IN
        WITH declared AS (
            SELECT def->>'field_name' AS field_name
            FROM item_type it,
                 jsonb_array_elements(
                     CASE WHEN jsonb_typeof(it.settings->'fields') = 'array'
                          THEN it.settings->'fields' ELSE '[]'::jsonb END
                 ) AS def
            WHERE it.type = p_type
              AND (def->'field_type' = '"TextLong"'::jsonb
                   OR (jsonb_typeof(def->'field_type') = 'object'
                       AND def->'field_type' ? 'Text'))
        ),
        names AS (
            SELECT field_name FROM declared
            UNION
            SELECT field_name FROM search_field_config WHERE bundle = p_type
            UNION
            SELECT unnest(ARRAY['field_body', 'body'])
        )
        SELECT n.field_name, COALESCE(c.weight, 'D') AS weight
        FROM names n
        LEFT JOIN search_field_config c
            ON c.bundle = p_type AND c.field_name = n.field_name
        WHERE n.field_name IS NOT NULL
    LOOP
        field_value := COALESCE(
            p_fields->field.field_name->>'value',
            CASE WHEN jsonb_typeof(p_fields->field.field_name) = 'string'
                 THEN p_fields->>field.field_name END
        );
        IF p_fields->field.field_name->>'summary' IS NOT NULL THEN
            field_value := COALESCE(field_value, '') || ' '
                || (p_fields->field.field_name->>'summary');
        END IF;

        IF field_value IS NOT NULL AND field_value <> '' THEN
            vector := vector || setweight(
                to_tsvector('english', regexp_replace(field_value, '<[^>]*>', ' ', 'g')),
                field.weight::"char"
            );
        END IF;
    END LOOP;

    WITH RECURSIVE visible AS (
        SELECT id, body FROM comment
        WHERE item_id = p_id AND parent_id IS NULL AND status = 1
        UNION ALL
        SELECT c.id, c.body FROM comment c
        JOIN visible v ON c.parent_id = v.id
        WHERE c.status = 1
    )
    SELECT string_agg(body, ' ') INTO comment_text FROM visible;

    IF comment_text IS NOT NULL AND comment_text <> '' THEN
        vector := vector || setweight(
            to_tsvector('english', regexp_replace(comment_text, '<[^>]*>', ' ', 'g')),
            'D'
        );
    END IF;

    RETURN vector;
END;
$$ LANGUAGE plpgsql STABLE;

CREATE OR REPLACE FUNCTION item_search_update() RETURNS trigger AS $$
BEGIN
    NEW.search_vector := item_search_vector(NEW.id, NEW.type, NEW.title, NEW.fields);
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

-- A comment written, edited, (un)published, moved or deleted recomputes its
-- item's vector. `search_vector` is not one of the columns `trg_item_search`
-- fires on, so this update does not run that trigger a second time.
CREATE OR REPLACE FUNCTION comment_search_update() RETURNS trigger AS $$
BEGIN
    IF TG_OP IN ('UPDATE', 'DELETE') THEN
        UPDATE item
        SET search_vector = item_search_vector(id, type, title, fields)
        WHERE id = OLD.item_id;
    END IF;
    IF TG_OP IN ('INSERT', 'UPDATE')
       AND (TG_OP = 'INSERT' OR NEW.item_id IS DISTINCT FROM OLD.item_id) THEN
        UPDATE item
        SET search_vector = item_search_vector(id, type, title, fields)
        WHERE id = NEW.item_id;
    END IF;
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trg_comment_search ON comment;
CREATE TRIGGER trg_comment_search
    AFTER INSERT OR DELETE OR UPDATE OF body, status, parent_id, item_id ON comment
    FOR EACH ROW
    EXECUTE FUNCTION comment_search_update();

-- Backfill: every existing item, with its fields and its comments.
UPDATE item SET search_vector = item_search_vector(id, type, title, fields);
