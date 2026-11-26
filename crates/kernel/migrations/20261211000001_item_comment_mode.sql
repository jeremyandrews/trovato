-- One item's own override of its content type's comment setting (B239).
--
-- `item.comment_mode` is nullable: NULL is `inherit`, meaning "use the
-- type's own `comment_mode` setting" (B159), the only value any existing row
-- holds, so no site's behaviour changes. A non-null value is one of the
-- type's own three: `open`, `closed` (existing comments shown, no new ones
-- taken) or `hidden` (no comments shown, none taken), and wins over the
-- type's setting for this item alone.
--
-- Not revisioned, the way `promote` and `sticky` are not: it is an
-- administrative flag an editor sets beside the item, not part of what the
-- item says.
ALTER TABLE item ADD COLUMN comment_mode TEXT
    CHECK (comment_mode IN ('open', 'closed', 'hidden') OR comment_mode IS NULL);

-- Teach the search vector function the same rule `render_thread` already
-- follows for a type's own `hidden` setting: an item whose *own* override is
-- hidden is searched without its comments, on top of the per-type `comments`
-- switch this function already reads. An item left at `inherit` (every
-- existing item) is unaffected.
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
    switches JSONB;
    item_comment_mode TEXT;
BEGIN
    SELECT settings->'search' INTO switches FROM item_type WHERE type = p_type;
    IF jsonb_typeof(switches) = 'object' AND switches->'include' = 'false'::jsonb THEN
        RETURN ''::tsvector;
    END IF;

    SELECT comment_mode INTO item_comment_mode FROM item WHERE id = p_id;

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

    IF jsonb_typeof(switches) = 'object' AND switches->'comments' = 'false'::jsonb THEN
        RETURN vector;
    END IF;

    IF item_comment_mode = 'hidden' THEN
        RETURN vector;
    END IF;

    WITH RECURSIVE visible AS (
        SELECT id, COALESCE(subject || ' ', '') || body AS body FROM comment
        WHERE item_id = p_id AND parent_id IS NULL AND status = 1
        UNION ALL
        SELECT c.id, COALESCE(c.subject || ' ', '') || c.body FROM comment c
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
